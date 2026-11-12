"""
WebAssembly-powered DTO data transformation engine for Python.
"""

from __future__ import annotations

import json
import weakref
from collections import OrderedDict
from pathlib import Path
from typing import Any, Callable, Optional, Union

import wasmtime

DEFAULT_WASM_PATH = Path(__file__).parent / "dto.wasm"

_STATUS_OK = 0

#: Compiled schemas kept by :meth:`DTOEngine.transform` (least recently used are freed first).
SCHEMA_CACHE_SIZE = 64

JsonInput = Union[dict, list, str]


def _to_json(value: JsonInput) -> str:
    return value if isinstance(value, str) else json.dumps(value)


class DTOEngine:
    """DTO Transformation Engine.

    Executes Rust-compiled WebAssembly for data mapping using Recipes,
    JSON Schema or Medallion pipelines.

    - :meth:`transform` caches compiled recipes (up to 64, LRU), so repeated
      calls with the same recipe do not re-parse it.
    - :meth:`compile` returns a :class:`CompiledTransform` you control; close it
      when done (it is also freed automatically when garbage-collected).

    WebAssembly memory grows to the largest input processed and never shrinks;
    to release it, drop the engine and create a new one. If the WebAssembly
    instance traps, the engine refuses further calls. Not thread-safe: use one
    engine per thread.
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
        self._engine_count = exports["dto_engine_count"]

        self._cache: "OrderedDict[str, int]" = OrderedDict()
        self._broken: Optional[BaseException] = None

    def compile(self, schema: JsonInput) -> "CompiledTransform":
        """Compile a Recipe, JSON Schema or pipeline once.

        Raises:
            RuntimeError: on configuration errors (unknown functions, bad paths, ...).
        """
        return CompiledTransform(self, self._compile(_to_json(schema)))

    def transform(self, schema: JsonInput, data: JsonInput, raw_json: bool = False) -> Any:
        """Transform input data according to a schema or recipe.

        The compiled recipe is cached for later calls.

        Args:
            schema: Recipe or JSON Schema (dict or JSON string).
            data: Input object or array of objects (dict, list, or JSON string).
            raw_json: If True, returns output as a JSON string instead of parsed Python object.

        Returns:
            Transformed Python dict/list, or raw JSON string if raw_json=True.

        Raises:
            RuntimeError: if the schema is invalid or any record fails.
        """
        text = self._transform_handle(self._cached_handle(_to_json(schema)), data)
        return text if raw_json else json.loads(text)

    @property
    def live_handles(self) -> int:
        """Number of compiled recipes alive inside WebAssembly (cache + compile())."""
        return self._call(lambda: self._engine_count(self._store))

    @property
    def memory_bytes(self) -> int:
        """Current WebAssembly memory size in bytes (grows, never shrinks)."""
        return self._memory.data_len(self._store)

    # ── internals ────────────────────────────────────────────────────────

    def _cached_handle(self, schema_json: str) -> int:
        handle = self._cache.get(schema_json)
        if handle is not None:
            self._cache.move_to_end(schema_json)
            return handle
        handle = self._compile(schema_json)
        self._cache[schema_json] = handle
        if len(self._cache) > SCHEMA_CACHE_SIZE:
            _, oldest = self._cache.popitem(last=False)
            self._free_handle(oldest)
        return handle

    def _compile(self, schema_json: str) -> int:
        text = self._with_bytes(
            schema_json.encode("utf-8"),
            lambda ptr, size: self._read_status(
                self._call(lambda: self._engine_new(self._store, ptr, size))
            ),
        )
        return int(text)

    def _transform_handle(self, handle: int, data: JsonInput) -> str:
        return self._with_bytes(
            _to_json(data).encode("utf-8"),
            lambda ptr, size: self._read_status(
                self._call(lambda: self._engine_transform(self._store, handle, ptr, size))
            ),
        )

    def _free_handle(self, handle: int) -> None:
        """Free a handle; never raises (used by close() and the finalizer)."""
        if self._broken is not None:
            return
        try:
            self._call(lambda: self._engine_free(self._store, handle))
        except Exception:  # noqa: BLE001 - the instance trapped; nothing left to free
            pass

    def _call(self, fn: Callable[[], Any]) -> Any:
        """Run a WebAssembly call, marking the engine unusable if it traps."""
        if self._broken is not None:
            raise RuntimeError(
                "DTO transform error: the WebAssembly instance trapped earlier "
                f"({self._broken}); create a new DTOEngine"
            )
        try:
            return fn()
        except wasmtime.Trap as trap:
            self._broken = trap
            self._cache.clear()
            raise RuntimeError(f"DTO transform error: WebAssembly trap: {trap}") from trap

    def _with_bytes(self, data: bytes, fn: Callable[[int, int], Any]) -> Any:
        ptr = self._call(lambda: self._alloc(self._store, len(data)))
        try:
            self._memory.write(self._store, data, start=ptr)
            return fn(ptr, len(data))
        finally:
            if self._broken is None:
                self._dealloc(self._store, ptr, len(data))

    def _read_status(self, res_ptr: int) -> str:
        """Read and free a ``[u32 len][u8 status][payload]`` result buffer."""
        header = self._read_memory(res_ptr, 5)
        size = int.from_bytes(header[:4], byteorder="little", signed=False)
        status = header[4]
        text = self._read_memory(res_ptr + 5, size).decode("utf-8")
        self._call(lambda: self._dealloc(self._store, res_ptr, size + 5))
        if status != _STATUS_OK:
            raise RuntimeError(f"DTO transform error: {text}")
        return text

    def _read_memory(self, ptr: int, size: int) -> bytes:
        return bytes(self._memory.read(self._store, start=ptr, stop=ptr + size))


class CompiledTransform:
    """A recipe / schema / pipeline compiled once by :meth:`DTOEngine.compile`.

    Use as a context manager or call :meth:`close` when done. If it is
    garbage-collected without being closed, its handle is freed automatically.
    """

    def __init__(self, engine: DTOEngine, handle: int) -> None:
        self._engine = engine
        self._handle: Optional[int] = handle
        self._finalizer = weakref.finalize(self, engine._free_handle, handle)

    def transform(self, data: JsonInput, raw_json: bool = False) -> Any:
        if self._handle is None:
            raise RuntimeError("DTO transform error: this CompiledTransform has been closed")
        text = self._engine._transform_handle(self._handle, data)
        return text if raw_json else json.loads(text)

    def close(self) -> None:
        """Release the compiled engine inside WebAssembly. Safe to call twice."""
        if self._handle is not None:
            self._handle = None
            self._finalizer()  # runs the free exactly once

    def __enter__(self) -> "CompiledTransform":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()
