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
 * High-performance DTO data transformation engine for Java powered by WebAssembly.
 * Uses Chicory for pure Java, zero-dependency execution without any CGO/JNI native binaries.
 */
public class DTOEngine implements AutoCloseable {
    private final WasmModule module;

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
        this.module = Parser.parse(wasmBytes);
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
     * Transforms input JSON data according to the given Recipe or JSON Schema.
     *
     * @param schemaJson Recipe or JSON Schema (UTF-8 JSON string).
     * @param dataJson   Input object or array of objects (UTF-8 JSON string).
     * @return Transformed JSON string.
     */
    public synchronized String transform(String schemaJson, String dataJson) {
        Instance instance = Instance.builder(module).build();
        Memory memory = instance.memory();

        ExportFunction alloc = instance.export("dto_alloc");
        ExportFunction dealloc = instance.export("dto_dealloc");
        ExportFunction transform = instance.export("dto_transform");

        byte[] schemaBytes = schemaJson.getBytes(StandardCharsets.UTF_8);
        byte[] dataBytes = dataJson.getBytes(StandardCharsets.UTF_8);

        int schemaPtr = (int) alloc.apply(schemaBytes.length)[0];
        memory.write(schemaPtr, schemaBytes);

        int dataPtr = (int) alloc.apply(dataBytes.length)[0];
        memory.write(dataPtr, dataBytes);

        try {
            int resPtr = (int) transform.apply(schemaPtr, schemaBytes.length, dataPtr, dataBytes.length)[0];

            // Read 4-byte little-endian length prefix
            byte[] lenBytes = memory.readBytes(resPtr, 4);
            int payloadLen = ByteBuffer.wrap(lenBytes).order(ByteOrder.LITTLE_ENDIAN).getInt();

            // Read payload
            byte[] payloadBytes = memory.readBytes(resPtr + 4, payloadLen);
            String resultStr = new String(payloadBytes, StandardCharsets.UTF_8);

            // Free result memory in WASM
            dealloc.apply(resPtr, payloadLen + 4);

            return resultStr;
        } finally {
            dealloc.apply(schemaPtr, schemaBytes.length);
            dealloc.apply(dataPtr, dataBytes.length);
        }
    }

    @Override
    public void close() {
        // Resources cleanup
    }
}
