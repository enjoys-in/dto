type Json = Record<string, any> | Array<any> | string;

/**
 * DTO data transformation engine powered by WebAssembly.
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
   * Compiles a Recipe, JSON Schema or pipeline once so it can transform many
   * inputs without being re-parsed. Throws on configuration errors.
   * Call `free()` on the result when done.
   */
  compile(schema: Record<string, any> | string): CompiledTransform;

  /**
   * Transforms `data` with `schema`. The compiled schema is cached (up to 64,
   * least recently used freed first), so repeated calls do not re-parse it.
   *
   * @param schema Recipe or JSON Schema (object or JSON string).
   * @param data Input object or array of objects.
   * @returns Transformed JavaScript object or array.
   */
  transform<T = any>(schema: Record<string, any> | string, data: Json): T;

  /** Number of compiled recipes alive inside WebAssembly (cache + compile()). */
  readonly liveHandles: number;

  /** Current WebAssembly memory size in bytes (grows, never shrinks). */
  readonly memoryBytes: number;
}

/**
 * A recipe / schema / pipeline compiled once by `DTOEngine.compile()`.
 */
export class CompiledTransform {
  private constructor();

  /** Transforms `data` and returns the parsed result. */
  transform<T = any>(data: Json): T;

  /** Transforms `data` and returns the output JSON text unparsed. */
  transformRaw(data: Json): string;

  /** Releases the compiled engine inside WebAssembly. Safe to call twice. */
  free(): void;
}
