/**
 * High-performance DTO data transformation engine powered by WebAssembly.
 */
export class DTOEngine {
  private constructor(wasmInstance: WebAssembly.Instance);

  /**
   * Initializes the engine with the WebAssembly binary.
   */
  static init(
    wasmSource?: string | Buffer | Uint8Array | ArrayBuffer | Response
  ): Promise<DTOEngine>;

  /**
   * Transforms input JSON data according to the given Recipe or JSON Schema.
   *
   * @param schema Recipe or JSON Schema (object or JSON string).
   * @param data Input object or array of objects.
   * @returns Transformed JavaScript object or array.
   */
  transform<T = any>(
    schema: Record<string, any> | string,
    data: Record<string, any> | Array<Record<string, any>> | string
  ): T;
}
