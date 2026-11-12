import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const STATUS_OK = 0;

const toJsonBytes = (value) =>
  encoder.encode(typeof value === 'string' ? value : JSON.stringify(value));

/**
 * DTO data transformation engine powered by WebAssembly.
 *
 * Use `compile(recipe)` once and call `.transform(data)` many times to avoid
 * re-parsing the recipe on every call. `transform(recipe, data)` is a one-shot
 * convenience that compiles, transforms and frees.
 */
export class DTOEngine {
  constructor(wasmInstance) {
    this.instance = wasmInstance;
    this.exports = wasmInstance.exports;
    this.memory = this.exports.memory;
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
    const handle = this._withBytes(toJsonBytes(schema), (ptr, len) =>
      this._readStatus(this.exports.dto_engine_new(ptr, len))
    );
    return new CompiledTransform(this, Number(handle));
  }

  /**
   * Transforms input JSON data according to the given Recipe or JSON Schema.
   *
   * @param {object | Array | string} schema - Recipe or JSON Schema (object or JSON string).
   * @param {object | Array | string} data - Input object or array of objects.
   * @returns {any} Transformed JavaScript object or array.
   */
  transform(schema, data) {
    const compiled = this.compile(schema);
    try {
      return compiled.transform(data);
    } finally {
      compiled.free();
    }
  }

  /** @internal Transform with a compiled handle; returns the output JSON text. */
  _transformHandle(handle, data) {
    return this._withBytes(toJsonBytes(data), (ptr, len) =>
      this._readStatus(this.exports.dto_engine_transform(handle, ptr, len))
    );
  }

  /** Copy bytes into WASM memory for the duration of `fn(ptr, len)`. */
  _withBytes(bytes, fn) {
    const ptr = this.exports.dto_alloc(bytes.length);
    // Create the view after allocating: allocation may grow (and detach) memory.
    new Uint8Array(this.memory.buffer, ptr, bytes.length).set(bytes);
    try {
      return fn(ptr, bytes.length);
    } finally {
      this.exports.dto_dealloc(ptr, bytes.length);
    }
  }

  /** Read and free a `[u32 len][u8 status][payload]` result buffer. */
  _readStatus(resPtr) {
    const view = new DataView(this.memory.buffer);
    const len = view.getUint32(resPtr, true);
    const status = view.getUint8(resPtr + 4);
    const text = decoder.decode(new Uint8Array(this.memory.buffer, resPtr + 5, len));
    this.exports.dto_dealloc(resPtr, len + 5);
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
      this.engine.exports.dto_engine_free(this.handle);
      this.handle = null;
    }
  }
}

if (typeof Symbol.dispose === 'symbol') {
  CompiledTransform.prototype[Symbol.dispose] = CompiledTransform.prototype.free;
}
