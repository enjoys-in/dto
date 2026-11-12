package com.dto;

import java.lang.ref.Cleaner;

/**
 * A Recipe, JSON Schema or pipeline compiled once by {@link DTOEngine#compile(String)}.
 * Use it in try-with-resources or call {@link #close()} when done. If it becomes
 * unreachable without being closed, its handle is freed automatically.
 */
public final class CompiledTransform implements AutoCloseable {
    private final DTOEngine engine;
    private final long handle;
    private final Cleaner.Cleanable cleanable;
    private boolean closed;

    CompiledTransform(DTOEngine engine, long handle) {
        this.engine = engine;
        this.handle = handle;
        this.cleanable = DTOEngine.CLEANER.register(this, new FreeHandle(engine, handle));
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
            cleanable.clean(); // runs FreeHandle exactly once
        }
    }

    /** Must not reference the CompiledTransform, or it could never be collected. */
    private static final class FreeHandle implements Runnable {
        private final DTOEngine engine;
        private final long handle;

        FreeHandle(DTOEngine engine, long handle) {
            this.engine = engine;
            this.handle = handle;
        }

        @Override
        public void run() {
            engine.freeHandle(handle);
        }
    }
}
