"""
WebAssembly-powered DTO data transformation engine for Python.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Callable, Union

import wasmtime

DEFAULT_WASM_PATH = Path(__file__).parent / "dto.wasm"

_STATUS_OK = 0

JsonInput = Union[dict, list, str]


def _to_json_bytes(value: JsonInput) -> bytes:
    return (value if isinstance(value, str) else json.dumps(value)).encode("utf-8")


class DTOEngine:
    """DTO Transformation Engine.

    Executes Rust-compiled WebAssembly for data mapping using Recipes,
    JSON Schema or Medallion pipelines.

    Use :meth:`compile` once and call ``transform`` on the result many times to
    avoid re-parsing the recipe on every call. :meth:`transform` is a one-shot
    convenience. Not thread-safe: use one engine per thread.
    """

    def __init__(self, wasm_path: Union[str, Path, None] = None) -> None:
        path = Path(wasm_path) if wasm_path else DEFAULT_WASM_PATH
        if not path.exists():
            raise FileNotFoundError(f"WASM module not found at: {path}")

        self._engine = wasmtime.Engine()
        self._module = wasmtime.Module.from_file(self._engine, str(path))
        self._store = wasmtime.Store(self._engine)
        self._linker = wasmtime.Linker(self._engine)

        self._instance = self._linker.instantiate(self._store, self._module)

        exports = self._instance.exports(self._store)
        self._memory = exports["memory"]
        self._alloc = exports["dto_alloc"]
        self._dealloc = exports["dto_dealloc"]
        self._engine_new = exports["dto_engine_new"]
        self._engine_transform = exports["dto_engine_transform"]
        self._engine_free = exports["dto_engine_free"]

    def compile(self, schema: JsonInput) -> "CompiledTransform":
        """Compile a Recipe, JSON Schema or pipeline once.

        Raises:
            RuntimeError: on configuration errors (unknown functions, bad paths, ...).
        """
        handle = self._with_bytes(
            _to_json_bytes(schema),
            lambda ptr, size: self._read_status(self._engine_new(self._store, ptr, size)),
        )
        return CompiledTransform(self, int(handle))

    def transform(self, schema: JsonInput, data: JsonInput, raw_json: bool = False) -> Any:
        """Transform input data according to a schema or recipe (one-shot).

        Args:
            schema: Recipe or JSON Schema (dict or JSON string).
            data: Input object or array of objects (dict, list, or JSON string).
            raw_json: If True, returns output as a JSON string instead of parsed Python object.

        Returns:
            Transformed Python dict/list, or raw JSON string if raw_json=True.

        Raises:
            RuntimeError: if the schema is invalid or any record fails.
        """
        with self.compile(schema) as compiled:
            return compiled.transform(data, raw_json=raw_json)

    # ── internals ────────────────────────────────────────────────────────

    def _transform_handle(self, handle: int, data: JsonInput) -> str:
        return self._with_bytes(
            _to_json_bytes(data),
            lambda ptr, size: self._read_status(
                self._engine_transform(self._store, handle, ptr, size)
            ),
        )

    def _free_handle(self, handle: int) -> None:
        self._engine_free(self._store, handle)

    def _with_bytes(self, data: bytes, fn: Callable[[int, int], Any]) -> Any:
        ptr = self._alloc(self._store, len(data))
        self._memory.write(self._store, data, start=ptr)
        try:
            return fn(ptr, len(data))
        finally:
            self._dealloc(self._store, ptr, len(data))

    def _read_status(self, res_ptr: int) -> str:
        """Read and free a ``[u32 len][u8 status][payload]`` result buffer."""
        header = self._read_memory(res_ptr, 5)
        size = int.from_bytes(header[:4], byteorder="little", signed=False)
        status = header[4]
        text = self._read_memory(res_ptr + 5, size).decode("utf-8")
        self._dealloc(self._store, res_ptr, size + 5)
        if status != _STATUS_OK:
            raise RuntimeError(f"DTO transform error: {text}")
        return text

    def _read_memory(self, ptr: int, size: int) -> bytes:
        return bytes(self._memory.read(self._store, start=ptr, stop=ptr + size))


class CompiledTransform:
    """A recipe / schema / pipeline compiled once by :meth:`DTOEngine.compile`.

    Use as a context manager or call :meth:`close` when done.
    """

    def __init__(self, engine: DTOEngine, handle: int) -> None:
        self._engine = engine
        self._handle: Union[int, None] = handle

    def transform(self, data: JsonInput, raw_json: bool = False) -> Any:
        if self._handle is None:
            raise RuntimeError("DTO transform error: this CompiledTransform has been closed")
        text = self._engine._transform_handle(self._handle, data)
        return text if raw_json else json.loads(text)

    def close(self) -> None:
        """Release the compiled engine inside WebAssembly. Safe to call twice."""
        if self._handle is not None:
            self._engine._free_handle(self._handle)
            self._handle = None

    def __enter__(self) -> "CompiledTransform":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()
