"""
WebAssembly-powered DTO data transformation engine for Python.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Union

import wasmtime

DEFAULT_WASM_PATH = Path(__file__).parent / "dto.wasm"


class DTOEngine:
    """DTO Transformation Engine.

    Executes Rust-compiled WebAssembly for high-speed, memory-efficient
    data mapping using either Recipes or JSON Schema.
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

        # Get exported functions
        exports = self._instance.exports(self._store)
        self._memory = exports["memory"]
        self._alloc = exports["dto_alloc"]
        self._dealloc = exports["dto_dealloc"]
        self._transform = exports["dto_transform"]

    def transform(
        self,
        schema: Union[dict, list, str],
        data: Union[dict, list, str],
        raw_json: bool = False,
    ) -> Any:
        """Transform input data according to schema or recipe.

        Args:
            schema: Recipe or JSON Schema (dict or JSON string).
            data: Input object or array of objects (dict, list, or JSON string).
            raw_json: If True, returns output as a JSON string instead of parsed Python object.

        Returns:
            Transformed Python dict/list, or raw JSON string if raw_json=True.
        """
        schema_bytes = (
            schema.encode("utf-8") if isinstance(schema, str) else json.dumps(schema).encode("utf-8")
        )
        data_bytes = (
            data.encode("utf-8") if isinstance(data, str) else json.dumps(data).encode("utf-8")
        )

        # Allocate memory for schema
        schema_ptr = self._alloc(self._store, len(schema_bytes))
        self._write_memory(schema_ptr, schema_bytes)

        # Allocate memory for data
        data_ptr = self._alloc(self._store, len(data_bytes))
        self._write_memory(data_ptr, data_bytes)

        try:
            # Execute transform
            res_ptr = self._transform(
                self._store,
                schema_ptr,
                len(schema_bytes),
                data_ptr,
                len(data_bytes),
            )

            # Read 4-byte little-endian length prefix
            len_bytes = self._read_memory(res_ptr, 4)
            payload_len = int.from_bytes(len_bytes, byteorder="little", signed=False)

            # Read payload
            payload_bytes = self._read_memory(res_ptr + 4, payload_len)
            payload_str = payload_bytes.decode("utf-8")

            # Free result memory
            self._dealloc(self._store, res_ptr, payload_len + 4)

            # Parse and check for errors
            result_obj = json.loads(payload_str)
            if isinstance(result_obj, dict) and "error" in result_obj and len(result_obj) == 1:
                raise RuntimeError(f"DTO transform error: {result_obj['error']}")

            return payload_str if raw_json else result_obj

        finally:
            # Free input buffers
            self._dealloc(self._store, schema_ptr, len(schema_bytes))
            self._dealloc(self._store, data_ptr, len(data_bytes))

    def _write_memory(self, ptr: int, data: bytes) -> None:
        self._memory.write(self._store, data, start=ptr)

    def _read_memory(self, ptr: int, size: int) -> bytes:
        return bytes(self._memory.read(self._store, start=ptr, stop=ptr + size))
