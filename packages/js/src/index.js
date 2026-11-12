import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/**
 * High-performance DTO data transformation engine powered by WebAssembly.
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
   * Transforms input JSON data according to the given Recipe or JSON Schema.
   *
   * @param {object | Array | string} schema - Recipe or JSON Schema (object or JSON string).
   * @param {object | Array | string} data - Input object or array of objects.
   * @returns {any} Transformed JavaScript object or array.
   */
  transform(schema, data) {
    const schemaStr = typeof schema === 'string' ? schema : JSON.stringify(schema);
    const dataStr = typeof data === 'string' ? data : JSON.stringify(data);

    const encoder = new TextEncoder();
    const decoder = new TextDecoder();

    const schemaBytes = encoder.encode(schemaStr);
    const dataBytes = encoder.encode(dataStr);

    const schemaPtr = this.exports.dto_alloc(schemaBytes.length);
    new Uint8Array(this.memory.buffer, schemaPtr, schemaBytes.length).set(schemaBytes);

    const dataPtr = this.exports.dto_alloc(dataBytes.length);
    new Uint8Array(this.memory.buffer, dataPtr, dataBytes.length).set(dataBytes);

    try {
      const resPtr = this.exports.dto_transform(
        schemaPtr,
        schemaBytes.length,
        dataPtr,
        dataBytes.length
      );

      const view = new DataView(this.memory.buffer);
      const payloadLen = view.getUint32(resPtr, true);

      const payloadBytes = new Uint8Array(this.memory.buffer, resPtr + 4, payloadLen);
      const resultStr = decoder.decode(payloadBytes);

      this.exports.dto_dealloc(resPtr, payloadLen + 4);

      const parsed = JSON.parse(resultStr);
      if (
        parsed &&
        typeof parsed === 'object' &&
        parsed.error &&
        Object.keys(parsed).length === 1
      ) {
        throw new Error(`DTO transform error: ${parsed.error}`);
      }

      return parsed;
    } finally {
      this.exports.dto_dealloc(schemaPtr, schemaBytes.length);
      this.exports.dto_dealloc(dataPtr, dataBytes.length);
    }
  }
}
