package com.dto;

/**
 * A Recipe, JSON Schema or pipeline compiled once by {@link DTOEngine#compile(String)}.
 * Use it in try-with-resources or call {@link #close()} when done.
 */
public final class CompiledTransform implements AutoCloseable {
    private final DTOEngine engine;
    private long handle;
    private boolean closed;

    CompiledTransform(DTOEngine engine, long handle) {
        this.engine = engine;
        this.handle = handle;
    }

    /**
     * Transforms an object or array of objects.
     *
     * @param dataJson input (UTF-8 JSON string).
     * @return the transformed JSON string.
     * @throws DTOException if any record fails, or if this transform is closed.
     */
    public synchronized String transform(String dataJson) {
        if (closed) {
            throw new DTOException("this CompiledTransform has been closed");
        }
        return engine.transformWithHandle(handle, dataJson);
    }

    /** Releases the compiled engine inside WebAssembly. Safe to call twice. */
    @Override
    public synchronized void close() {
        if (!closed) {
            closed = true;
            engine.freeHandle(handle);
        }
    }
}
