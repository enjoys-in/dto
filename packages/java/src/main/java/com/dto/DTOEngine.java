package com.dto;

import com.dylibso.chicory.runtime.ExportFunction;
import com.dylibso.chicory.runtime.Instance;
import com.dylibso.chicory.runtime.Memory;
import com.dylibso.chicory.wasm.Parser;
import com.dylibso.chicory.wasm.WasmModule;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.lang.ref.Cleaner;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * DTO data transformation engine for Java powered by WebAssembly.
 * Uses Chicory for pure Java execution without any JNI native binaries.
 *
 * <ul>
 *   <li>{@link #transform(String, String)} caches compiled recipes (up to 64, least
 *       recently used are freed first), so repeated calls do not re-parse them.</li>
 *   <li>{@link #compile(String)} returns a {@link CompiledTransform} you control; close
 *       it when done (it is also freed automatically after garbage collection).</li>
 * </ul>
 *
 * <p>All methods are synchronized on the engine. WebAssembly memory grows to the
 * largest input processed and never shrinks; to release it, drop the engine and
 * create a new one. If the WebAssembly instance traps, the engine refuses further calls.
 */
public class DTOEngine implements AutoCloseable {
    static final Cleaner CLEANER = Cleaner.create();

    private static final int STATUS_OK = 0;
    private static final int SCHEMA_CACHE_SIZE = 64;

    private final Memory memory;
    private final ExportFunction alloc;
    private final ExportFunction dealloc;
    private final ExportFunction engineNew;
    private final ExportFunction engineTransform;
    private final ExportFunction engineFree;
    private final ExportFunction engineCount;

    /** Schema JSON to handle, in access order (eldest = least recently used). */
    private final LinkedHashMap<String, Long> cache = new LinkedHashMap<>(16, 0.75f, true);
    /** Set when a WebAssembly call trapped; the instance must not be used again. */
    private RuntimeException broken;

    /**
     * Creates a new DTOEngine using the bundled dto.wasm binary.
     */
    public DTOEngine() {
        this(loadDefaultWasm());
    }

    /**
     * Creates a new DTOEngine from custom WebAssembly bytes.
     */
    public DTOEngine(byte[] wasmBytes) {
        WasmModule module = Parser.parse(wasmBytes);
        // One instance for the engine's lifetime: compiled handles live in its memory.
        Instance instance = Instance.builder(module).build();
        this.memory = instance.memory();
        this.alloc = instance.export("dto_alloc");
        this.dealloc = instance.export("dto_dealloc");
        this.engineNew = instance.export("dto_engine_new");
        this.engineTransform = instance.export("dto_engine_transform");
        this.engineFree = instance.export("dto_engine_free");
        this.engineCount = instance.export("dto_engine_count");
    }

    private static byte[] loadDefaultWasm() {
        try (InputStream is = DTOEngine.class.getResourceAsStream("/dto.wasm")) {
            if (is == null) {
                throw new IllegalStateException("dto.wasm not found in jar resources");
            }
            ByteArrayOutputStream buffer = new ByteArrayOutputStream();
            byte[] data = new byte[8192];
            int nRead;
            while ((nRead = is.read(data, 0, data.length)) != -1) {
                buffer.write(data, 0, nRead);
            }
            return buffer.toByteArray();
        } catch (IOException e) {
            throw new RuntimeException("Failed to load dto.wasm resource", e);
        }
    }

    /**
     * Compiles a Recipe, JSON Schema or pipeline once.
     *
     * @param schemaJson Recipe / JSON Schema / pipeline (UTF-8 JSON string).
     * @return a compiled transform; close it when done.
     * @throws DTOException on configuration errors (unknown functions, bad paths, ...).
     */
    public synchronized CompiledTransform compile(String schemaJson) {
        return new CompiledTransform(this, compileHandle(schemaJson));
    }

    /**
     * Transforms input JSON data according to the given Recipe or JSON Schema.
     * The compiled recipe is cached for later calls.
     *
     * @param schemaJson Recipe or JSON Schema (UTF-8 JSON string).
     * @param dataJson   Input object or array of objects (UTF-8 JSON string).
     * @return Transformed JSON string.
     * @throws DTOException if the schema is invalid or any record fails.
     */
    public synchronized String transform(String schemaJson, String dataJson) {
        return transformWithHandle(cachedHandle(schemaJson), dataJson);
    }

    /** Number of compiled recipes alive inside WebAssembly (cache + compile()). */
    public synchronized int liveHandles() {
        return (int) call(engineCount)[0];
    }

    private long cachedHandle(String schemaJson) {
        Long hit = cache.get(schemaJson); // access order: get() marks it most recently used
        if (hit != null) {
            return hit;
        }
        long handle = compileHandle(schemaJson);
        cache.put(schemaJson, handle);
        if (cache.size() > SCHEMA_CACHE_SIZE) {
            Map.Entry<String, Long> eldest = cache.entrySet().iterator().next();
            long evicted = eldest.getValue();
            cache.remove(eldest.getKey());
            freeHandle(evicted);
        }
        return handle;
    }

    private long compileHandle(String schemaJson) {
        byte[] bytes = schemaJson.getBytes(StandardCharsets.UTF_8);
        int ptr = write(bytes);
        try {
            int resPtr = (int) call(engineNew, ptr, bytes.length)[0];
            return Long.parseLong(readStatus(resPtr));
        } finally {
            release(ptr, bytes.length);
        }
    }

    synchronized String transformWithHandle(long handle, String dataJson) {
        byte[] bytes = dataJson.getBytes(StandardCharsets.UTF_8);
        int ptr = write(bytes);
        try {
            int resPtr = (int) call(engineTransform, handle, ptr, bytes.length)[0];
            return readStatus(resPtr);
        } finally {
            release(ptr, bytes.length);
        }
    }

    /** Frees a handle; never throws (used by close() and the Cleaner thread). */
    synchronized void freeHandle(long handle) {
        if (broken != null) {
            return;
        }
        try {
            call(engineFree, handle);
        } catch (DTOException ignored) {
            // The instance trapped; nothing left to free.
        }
    }

    /** Runs a WebAssembly call, marking the engine unusable if it traps. */
    private long[] call(ExportFunction fn, long... args) {
        if (broken != null) {
            throw new DTOException("the WebAssembly instance trapped earlier ("
                + broken.getMessage() + "); create a new DTOEngine");
        }
        try {
            return fn.apply(args);
        } catch (RuntimeException e) {
            broken = e;
            cache.clear();
            throw new DTOException("WebAssembly trap: " + e.getMessage());
        }
    }

    /** Copies bytes into WASM memory; the caller must release them. */
    private int write(byte[] bytes) {
        int ptr = (int) call(alloc, bytes.length)[0];
        memory.write(ptr, bytes);
        return ptr;
    }

    private void release(int ptr, int length) {
        if (broken == null) {
            call(dealloc, ptr, length);
        }
    }

    /** Reads and frees a [u32 len][u8 status][payload] result buffer. */
    private String readStatus(int resPtr) {
        byte[] header = memory.readBytes(resPtr, 5);
        int len = ByteBuffer.wrap(header, 0, 4).order(ByteOrder.LITTLE_ENDIAN).getInt();
        int status = header[4];
        String text = new String(memory.readBytes(resPtr + 5, len), StandardCharsets.UTF_8);
        call(dealloc, resPtr, len + 5);
        if (status != STATUS_OK) {
            throw new DTOException(text);
        }
        return text;
    }

    @Override
    public void close() {
        // The WASM instance is garbage-collected with the engine.
    }
}
