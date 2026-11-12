package com.dto;

import com.dylibso.chicory.runtime.ExportFunction;
import com.dylibso.chicory.runtime.Instance;
import com.dylibso.chicory.runtime.Memory;
import com.dylibso.chicory.wasm.Parser;
import com.dylibso.chicory.wasm.WasmModule;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;

/**
 * DTO data transformation engine for Java powered by WebAssembly.
 * Uses Chicory for pure Java execution without any JNI native binaries.
 *
 * <p>Use {@link #compile(String)} once and call {@link CompiledTransform#transform(String)}
 * many times to avoid re-parsing the recipe on every call. {@link #transform(String, String)}
 * is a one-shot convenience. All methods are synchronized on the engine.
 */
public class DTOEngine implements AutoCloseable {
    private static final int STATUS_OK = 0;

    private final Memory memory;
    private final ExportFunction alloc;
    private final ExportFunction dealloc;
    private final ExportFunction engineNew;
    private final ExportFunction engineTransform;
    private final ExportFunction engineFree;

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
        byte[] bytes = schemaJson.getBytes(StandardCharsets.UTF_8);
        int ptr = write(bytes);
        try {
            int resPtr = (int) engineNew.apply(ptr, bytes.length)[0];
            long handle = Long.parseLong(readStatus(resPtr));
            return new CompiledTransform(this, handle);
        } finally {
            dealloc.apply(ptr, bytes.length);
        }
    }

    /**
     * Transforms input JSON data according to the given Recipe or JSON Schema (one-shot).
     *
     * @param schemaJson Recipe or JSON Schema (UTF-8 JSON string).
     * @param dataJson   Input object or array of objects (UTF-8 JSON string).
     * @return Transformed JSON string.
     * @throws DTOException if the schema is invalid or any record fails.
     */
    public synchronized String transform(String schemaJson, String dataJson) {
        try (CompiledTransform compiled = compile(schemaJson)) {
            return compiled.transform(dataJson);
        }
    }

    synchronized String transformWithHandle(long handle, String dataJson) {
        byte[] bytes = dataJson.getBytes(StandardCharsets.UTF_8);
        int ptr = write(bytes);
        try {
            int resPtr = (int) engineTransform.apply(handle, ptr, bytes.length)[0];
            return readStatus(resPtr);
        } finally {
            dealloc.apply(ptr, bytes.length);
        }
    }

    synchronized void freeHandle(long handle) {
        engineFree.apply(handle);
    }

    /** Copies bytes into WASM memory; the caller must dealloc. */
    private int write(byte[] bytes) {
        int ptr = (int) alloc.apply(bytes.length)[0];
        memory.write(ptr, bytes);
        return ptr;
    }

    /** Reads and frees a [u32 len][u8 status][payload] result buffer. */
    private String readStatus(int resPtr) {
        byte[] header = memory.readBytes(resPtr, 5);
        int len = ByteBuffer.wrap(header, 0, 4).order(ByteOrder.LITTLE_ENDIAN).getInt();
        int status = header[4];
        String text = new String(memory.readBytes(resPtr + 5, len), StandardCharsets.UTF_8);
        dealloc.apply(resPtr, len + 5);
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
