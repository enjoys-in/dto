import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const STATUS_OK = 0;

/** Compiled schemas kept by `transform()` (least recently used are freed first). */
const SCHEMA_CACHE_SIZE = 64;

const toJson = (value) => (typeof value === 'string' ? value : JSON.stringify(value));

/** Frees compiled handles whose CompiledTransform was garbage-collected without free(). */
const finalizer =
  typeof FinalizationRegistry === 'function'
    ? new FinalizationRegistry(({ engine, handle }) => engine._freeHandle(handle))
    : null;

/**
 * DTO data transformation engine powered by WebAssembly.
 *
 * - `transform(recipe, data)` caches compiled recipes (up to 64, LRU), so
 *   repeated calls with the same recipe do not re-parse it.
 * - `compile(recipe)` returns a handle you control; call `.free()` when done
 *   (it is also freed automatically when garbage-collected).
 *
 * WebAssembly memory grows to the largest input processed and never shrinks;
 * to release it, drop the engine and create a new one with `DTOEngine.init()`.
 * If the WebAssembly instance traps, the engine refuses further calls.
 */
export class DTOEngine {
  constructor(wasmInstance) {
    this.instance = wasmInstance;
    this.exports = wasmInstance.exports;
    this.memory = this.exports.memory;
    /** @type {Map<string, number>} schema JSON -> handle, in LRU order */
    this._cache = new Map();
    /** @type {Error | null} set when the instance trapped */
    this._broken = null;
  }

  /**
   * Initializes the engine with the WebAssembly binary.
   *
   * @param {string | Buffer | Uint8Array | ArrayBuffer | Response} [wasmSource] - Optional custom path or buffer.
   * @returns {Promise<DTOEngine>}
   */
  static async init(wasmSource) {
    let bytes;
    if (!wasmSource) {
      const defaultPath = path.resolve(__dirname, '../dto.wasm');
      bytes = fs.readFileSync(defaultPath);
    } else if (typeof wasmSource === 'string') {
      bytes = fs.readFileSync(wasmSource);
    } else if (wasmSource instanceof Uint8Array || wasmSource instanceof ArrayBuffer) {
      bytes = wasmSource;
    } else if (typeof Response !== 'undefined' && wasmSource instanceof Response) {
      bytes = await wasmSource.arrayBuffer();
    } else {
      bytes = wasmSource;
    }

    const { instance } = await WebAssembly.instantiate(bytes, {});
    return new DTOEngine(instance);
  }

  /**
   * Compiles a Recipe, JSON Schema or pipeline once. Configuration errors
   * (unknown functions, bad paths, ...) are thrown here.
   *
   * @param {object | string} schema - Recipe / JSON Schema / pipeline (object or JSON string).
   * @returns {CompiledTransform} Call `.free()` when done.
   */
  compile(schema) {
    return new CompiledTransform(this, this._compile(toJson(schema)));
  }

  /**
   * Transforms input JSON data according to the given Recipe or JSON Schema.
   * The compiled recipe is cached for later calls.
   *
   * @param {object | Array | string} schema - Recipe or JSON Schema (object or JSON string).
   * @param {object | Array | string} data - Input object or array of objects.
   * @returns {any} Transformed JavaScript object or array.
   */
  transform(schema, data) {
    return JSON.parse(this._transformHandle(this._cachedHandle(toJson(schema)), data));
  }

  /** Number of compiled recipes alive inside WebAssembly (cache + compile()). */
  get liveHandles() {
    return this._call(() => this.exports.dto_engine_count());
  }

  /** Current WebAssembly memory size in bytes (grows, never shrinks). */
  get memoryBytes() {
    return this.memory.buffer.byteLength;
  }

  // ── internals ──────────────────────────────────────────────────────

  _cachedHandle(schemaJson) {
    const hit = this._cache.get(schemaJson);
    if (hit !== undefined) {
      this._cache.delete(schemaJson); // move to most-recently-used
      this._cache.set(schemaJson, hit);
      return hit;
    }
    const handle = this._compile(schemaJson);
    this._cache.set(schemaJson, handle);
    if (this._cache.size > SCHEMA_CACHE_SIZE) {
      const [oldestKey, oldestHandle] = this._cache.entries().next().value;
      this._cache.delete(oldestKey);
      this._freeHandle(oldestHandle);
    }
    return handle;
  }

  _compile(schemaJson) {
    const text = this._withBytes(encoder.encode(schemaJson), (ptr, len) =>
      this._readStatus(this._call(() => this.exports.dto_engine_new(ptr, len)))
    );
    return Number(text);
  }

  /** @internal Transform with a compiled handle; returns the output JSON text. */
  _transformHandle(handle, data) {
    return this._withBytes(encoder.encode(toJson(data)), (ptr, len) =>
      this._readStatus(this._call(() => this.exports.dto_engine_transform(handle, ptr, len)))
    );
  }

  /** @internal Free a handle; never throws (used by free() and the finalizer). */
  _freeHandle(handle) {
    if (this._broken) return;
    try {
      this._call(() => this.exports.dto_engine_free(handle));
    } catch {
      // The instance trapped; nothing left to free.
    }
  }

  /** Run a WebAssembly call, marking the engine unusable if it traps. */
  _call(fn) {
    if (this._broken) {
      throw new Error(
        `DTO transform error: the WebAssembly instance trapped earlier (${this._broken.message}); create a new DTOEngine`
      );
    }
    try {
      return fn();
    } catch (err) {
      if (err instanceof WebAssembly.RuntimeError) {
        this._broken = err;
        this._cache.clear();
        throw new Error(`DTO transform error: WebAssembly trap: ${err.message}`, { cause: err });
      }
      throw err;
    }
  }

  /** Copy bytes into WASM memory for the duration of `fn(ptr, len)`. */
  _withBytes(bytes, fn) {
    const ptr = this._call(() => this.exports.dto_alloc(bytes.length));
    try {
      // Create the view after allocating: allocation may grow (and detach) memory.
      new Uint8Array(this.memory.buffer, ptr, bytes.length).set(bytes);
      return fn(ptr, bytes.length);
    } finally {
      if (!this._broken) this.exports.dto_dealloc(ptr, bytes.length);
    }
  }

  /** Read and free a `[u32 len][u8 status][payload]` result buffer. */
  _readStatus(resPtr) {
    const view = new DataView(this.memory.buffer);
    const len = view.getUint32(resPtr, true);
    const status = view.getUint8(resPtr + 4);
    const text = decoder.decode(new Uint8Array(this.memory.buffer, resPtr + 5, len));
    this._call(() => this.exports.dto_dealloc(resPtr, len + 5));
    if (status !== STATUS_OK) {
      throw new Error(`DTO transform error: ${text}`);
    }
    return text;
  }
}

/**
 * A recipe / schema / pipeline compiled once by `DTOEngine.compile()`.
 */
export class CompiledTransform {
  constructor(engine, handle) {
    this.engine = engine;
    this.handle = handle;
    finalizer?.register(this, { engine, handle }, this);
  }

  /**
   * @param {object | Array | string} data - Input object or array of objects.
   * @returns {any} Transformed JavaScript object or array.
   */
  transform(data) {
    return JSON.parse(this.transformRaw(data));
  }

  /**
   * Like `transform` but returns the output JSON text without parsing it.
   * @param {object | Array | string} data
   * @returns {string}
   */
  transformRaw(data) {
    if (this.handle === null) {
      throw new Error('DTO transform error: this CompiledTransform has been freed');
    }
    return this.engine._transformHandle(this.handle, data);
  }

  /** Releases the compiled engine inside WebAssembly. Safe to call twice. */
  free() {
    if (this.handle !== null) {
      finalizer?.unregister(this);
      this.engine._freeHandle(this.handle);
      this.handle = null;
    }
  }
}

if (typeof Symbol.dispose === 'symbol') {
  CompiledTransform.prototype[Symbol.dispose] = CompiledTransform.prototype.free;
}
