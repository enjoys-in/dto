//! # dto_wasm
//!
//! WebAssembly bindings for the DTO transformation engine.
//!
//! Exposes:
//! 1. **High-level wasm-bindgen API** for JavaScript / TypeScript (Node.js, Bun, Deno, Web).
//! 2. **Low-level C-ABI memory exports** for Go (wazero), Python (wasmtime), and Java (chicory/wasmtime).

use dto_core::DtoEngine;

// ─── High-Level wasm-bindgen API (JavaScript / TypeScript) ───

#[cfg(feature = "js")]
use wasm_bindgen::prelude::*;

/// Transform JSON data using either a Recipe or a JSON Schema.
///
/// - `schema`: JSON string of either a Recipe or JSON Schema with `x-from` annotations.
/// - `data`: JSON string of either a single object or an array of objects.
///
/// Returns the transformed JSON string.
#[cfg(feature = "js")]
#[wasm_bindgen]
pub fn transform(schema: &str, data: &str) -> Result<String, JsError> {
    let engine = DtoEngine::from_json(schema)
        .map_err(|e| JsError::new(&format!("Schema/Recipe error: {e}")))?;

    let input: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| JsError::new(&format!("Invalid input JSON: {e}")))?;

    let output = engine.transform(&input)
        .map_err(|e| JsError::new(&format!("Transform error: {e}")))?;

    serde_json::to_string(&output)
        .map_err(|e| JsError::new(&format!("Serialization error: {e}")))
}

/// Transform JavaScript objects directly (zero manual JSON.stringify in JS).
#[cfg(feature = "js")]
#[wasm_bindgen(js_name = "transformObject")]
pub fn transform_object(schema: JsValue, data: JsValue) -> Result<JsValue, JsError> {
    let schema_val: serde_json::Value = serde_wasm_bindgen_compat_from_js(schema)?;
    let data_val: serde_json::Value = serde_wasm_bindgen_compat_from_js(data)?;

    let engine = DtoEngine::from_json_schema_value(&schema_val)
        .or_else(|_| {
            let s = serde_json::to_string(&schema_val).unwrap_or_default();
            DtoEngine::from_json(&s)
        })
        .map_err(|e| JsError::new(&format!("Schema/Recipe error: {e}")))?;

    let output = engine.transform(&data_val)
        .map_err(|e| JsError::new(&format!("Transform error: {e}")))?;

    let json_str = serde_json::to_string(&output)
        .map_err(|e| JsError::new(&format!("Serialization error: {e}")))?;

    js_sys::JSON::parse(&json_str)
        .map_err(|_| JsError::new("Failed to parse transformed JSON into JS object"))
}

#[cfg(feature = "js")]
fn serde_wasm_bindgen_compat_from_js(val: JsValue) -> Result<serde_json::Value, JsError> {
    let stringified = js_sys::JSON::stringify(&val)
        .map_err(|_| JsError::new("Failed to stringify JavaScript object"))?;
    let s: String = stringified.as_string()
        .ok_or_else(|| JsError::new("JSON.stringify returned non-string"))?;
    serde_json::from_str(&s)
        .map_err(|e| JsError::new(&format!("Invalid JSON: {e}")))
}

/// Returns the version of the DTO engine.
#[cfg(feature = "js")]
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ─── Low-Level C-ABI exports (for Go / Python / Java WASM runtimes) ───

/// Allocate a buffer of `size` bytes in linear memory.
/// The caller must free this memory using `dto_dealloc`.
#[no_mangle]
pub extern "C" fn dto_alloc(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a buffer of `size` bytes previously allocated by `dto_alloc`.
#[no_mangle]
pub extern "C" fn dto_dealloc(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Transform data via raw memory exchange.
///
/// # Arguments:
/// - `schema_ptr`, `schema_len`: Pointer and length of the schema/recipe UTF-8 JSON bytes.
/// - `data_ptr`, `data_len`: Pointer and length of the input data UTF-8 JSON bytes.
///
/// # Return Value:
/// Returns a pointer to a length-prefixed buffer:
/// - **Bytes 0..4**: Length of the JSON payload as a 32-bit unsigned integer (little-endian `u32`).
/// - **Bytes 4..4+len**: The UTF-8 JSON payload (the transformed output or an error `{"error":"..."}`).
///
/// The caller reads the 4-byte length `L`, extracts the `L` bytes of payload starting at offset 4,
/// and then frees the entire buffer by calling `dto_dealloc(ptr, L + 4)`.
/// This protocol is 100% thread-safe and reentrant.
#[no_mangle]
pub extern "C" fn dto_transform(
    schema_ptr: *const u8,
    schema_len: usize,
    data_ptr: *const u8,
    data_len: usize,
) -> *mut u8 {
    let result = std::panic::catch_unwind(|| {
        if schema_ptr.is_null() || data_ptr.is_null() {
            return Err("null pointer passed to dto_transform".to_string());
        }

        let schema_bytes = unsafe { std::slice::from_raw_parts(schema_ptr, schema_len) };
        let data_bytes = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };

        let schema_str = std::str::from_utf8(schema_bytes)
            .map_err(|e| format!("schema is not valid UTF-8: {e}"))?;
        let data_str = std::str::from_utf8(data_bytes)
            .map_err(|e| format!("data is not valid UTF-8: {e}"))?;

        let engine = DtoEngine::from_json(schema_str)
            .map_err(|e| format!("schema error: {e}"))?;

        let input: serde_json::Value = serde_json::from_str(data_str)
            .map_err(|e| format!("input JSON parse error: {e}"))?;

        let output = engine.transform(&input)
            .map_err(|e| format!("transformation error: {e}"))?;

        serde_json::to_string(&output)
            .map_err(|e| format!("output serialization error: {e}"))
    });

    let result_str = match result {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => serde_json::json!({ "error": e }).to_string(),
        Err(_) => serde_json::json!({ "error": "dto_transform panicked" }).to_string(),
    };

    let bytes = result_str.as_bytes();
    let payload_len = bytes.len() as u32;
    let total_len = 4 + bytes.len();
    let ptr = dto_alloc(total_len);

    unsafe {
        let len_bytes = payload_len.to_le_bytes();
        std::ptr::copy_nonoverlapping(len_bytes.as_ptr(), ptr, 4);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

// ─── Compile-once engine handles (status-byte protocol) ───
//
// `dto_transform` re-parses the recipe on every call and reports errors as an
// `{"error": ...}` payload, which is indistinguishable from a real output that
// has a single `error` field. The functions below fix both:
//
// 1. `dto_engine_new(schema_ptr, schema_len)` compiles a Recipe / JSON Schema /
//    pipeline once and returns a handle.
// 2. `dto_engine_transform(handle, data_ptr, data_len)` transforms with it.
// 3. `dto_engine_free(handle)` releases it (returns 1 if it existed, else 0).
//
// Result buffers from (1) and (2):
// - bytes 0..4: payload length `L` (u32, little-endian)
// - byte 4:     status — 0 = ok, 1 = error
// - bytes 5..5+L: UTF-8 payload — the handle in decimal (1), the output JSON
//   (2), or an error message.
// Free them with `dto_dealloc(ptr, L + 5)`.
//
// Handles are never reused, so a stale handle gives an error instead of
// silently using another engine. Each WASM instance has its own handles.

use std::cell::RefCell;
use std::collections::HashMap;

const STATUS_OK: u8 = 0;
const STATUS_ERR: u8 = 1;

#[derive(Default)]
struct Registry {
    last_handle: u32,
    engines: HashMap<u32, DtoEngine>,
}

thread_local! {
    static ENGINES: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Borrow `len` bytes at `ptr` as UTF-8.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (as written by the host after `dto_alloc`).
unsafe fn read_utf8<'a>(ptr: *const u8, len: usize, what: &str) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Err(format!("null {what} pointer"));
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    std::str::from_utf8(bytes).map_err(|e| format!("{what} is not valid UTF-8: {e}"))
}

/// Write a status-prefixed result buffer.
fn status_buffer(result: Result<String, String>) -> *mut u8 {
    let (status, payload) = match result {
        Ok(text) => (STATUS_OK, text),
        Err(message) => (STATUS_ERR, message),
    };
    let bytes = payload.as_bytes();
    let ptr = dto_alloc(5 + bytes.len());
    unsafe {
        std::ptr::copy_nonoverlapping((bytes.len() as u32).to_le_bytes().as_ptr(), ptr, 4);
        *ptr.add(4) = status;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(5), bytes.len());
    }
    ptr
}

fn guarded(f: impl FnOnce() -> Result<String, String>) -> *mut u8 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("dto engine panicked".to_string()));
    status_buffer(result)
}

/// Compile a Recipe / JSON Schema / pipeline once; see the protocol notes above.
#[no_mangle]
pub extern "C" fn dto_engine_new(schema_ptr: *const u8, schema_len: usize) -> *mut u8 {
    guarded(|| {
        let schema = unsafe { read_utf8(schema_ptr, schema_len, "schema")? };
        let engine = DtoEngine::from_json(schema).map_err(|e| format!("schema error: {e}"))?;
        ENGINES.with(|cell| {
            let mut registry = cell.borrow_mut();
            let handle = registry
                .last_handle
                .checked_add(1)
                .ok_or_else(|| "engine handle space exhausted".to_string())?;
            registry.last_handle = handle;
            registry.engines.insert(handle, engine);
            Ok(handle.to_string())
        })
    })
}

/// Transform data with a compiled engine; see the protocol notes above.
#[no_mangle]
pub extern "C" fn dto_engine_transform(handle: u32, data_ptr: *const u8, data_len: usize) -> *mut u8 {
    guarded(|| {
        let data = unsafe { read_utf8(data_ptr, data_len, "data")? };
        let input: serde_json::Value =
            serde_json::from_str(data).map_err(|e| format!("input JSON parse error: {e}"))?;
        ENGINES.with(|cell| {
            let registry = cell.borrow();
            let engine = registry
                .engines
                .get(&handle)
                .ok_or_else(|| format!("unknown or freed engine handle {handle}"))?;
            let output = engine
                .transform(&input)
                .map_err(|e| format!("transformation error: {e}"))?;
            serde_json::to_string(&output).map_err(|e| format!("output serialization error: {e}"))
        })
    })
}

/// Release a compiled engine. Returns 1 if the handle existed, 0 otherwise.
#[no_mangle]
pub extern "C" fn dto_engine_free(handle: u32) -> u32 {
    ENGINES.with(|cell| u32::from(cell.borrow_mut().engines.remove(&handle).is_some()))
}

/// Number of live compiled engines in this instance (for leak checks).
#[no_mangle]
pub extern "C" fn dto_engine_count() -> u32 {
    ENGINES.with(|cell| cell.borrow().engines.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_and_free_c_abi_result(ptr: *mut u8) -> (serde_json::Value, usize) {
        unsafe {
            let mut len_bytes = [0u8; 4];
            std::ptr::copy_nonoverlapping(ptr, len_bytes.as_mut_ptr(), 4);
            let payload_len = u32::from_le_bytes(len_bytes) as usize;

            let payload_slice = std::slice::from_raw_parts(ptr.add(4), payload_len);
            let s = std::str::from_utf8(payload_slice).unwrap();
            let val: serde_json::Value = serde_json::from_str(s).unwrap();

            dto_dealloc(ptr, payload_len + 4);
            (val, payload_len)
        }
    }

    #[test]
    fn test_c_abi_roundtrip_recipe() {
        let recipe = r#"{
            "name": "Test",
            "mappings": [
                { "source": "user.name", "target": "name" },
                { "source": "user.city", "target": "city" }
            ]
        }"#;

        let data = r#"{"user":{"name":"Alice","city":"Berlin"}}"#;

        let schema_bytes = recipe.as_bytes();
        let data_bytes = data.as_bytes();

        let ptr = dto_transform(
            schema_bytes.as_ptr(),
            schema_bytes.len(),
            data_bytes.as_ptr(),
            data_bytes.len(),
        );

        let (val, _) = read_and_free_c_abi_result(ptr);
        assert_eq!(val["name"], "Alice");
        assert_eq!(val["city"], "Berlin");
    }

    #[test]
    fn test_c_abi_roundtrip_batch_array() {
        let recipe = r#"{
            "name": "BatchTest",
            "mappings": [
                { "source": "id", "target": "userId" }
            ]
        }"#;

        let data = r#"[{"id":"u1"},{"id":"u2"}]"#;

        let schema_bytes = recipe.as_bytes();
        let data_bytes = data.as_bytes();

        let ptr = dto_transform(
            schema_bytes.as_ptr(),
            schema_bytes.len(),
            data_bytes.as_ptr(),
            data_bytes.len(),
        );

        let (val, _) = read_and_free_c_abi_result(ptr);
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["userId"], "u1");
        assert_eq!(arr[1]["userId"], "u2");
    }

    #[test]
    fn test_c_abi_roundtrip_medallion_pipeline() {
        let pipeline = r#"{
            "targets": [
                {
                    "table": "silver_part",
                    "grain": "header",
                    "mappings": [
                        { "source": "item_id", "target": "part_number", "cast": "string" },
                        { "source": "price", "target": "cost", "cast": "decimal" }
                    ]
                }
            ]
        }"#;

        let data = r#"{"item_id":"PART-99","price":19.99}"#;

        let schema_bytes = pipeline.as_bytes();
        let data_bytes = data.as_bytes();

        let ptr = dto_transform(
            schema_bytes.as_ptr(),
            schema_bytes.len(),
            data_bytes.as_ptr(),
            data_bytes.len(),
        );

        let (val, _) = read_and_free_c_abi_result(ptr);
        assert_eq!(val["silver_part"][0]["part_number"], "PART-99");
        assert_eq!(val["silver_part"][0]["cost"], 19.99);
    }

    #[test]
    fn test_c_abi_error_handling() {
        let recipe = r#"invalid json"#;
        let data = r#"{}"#;

        let ptr = dto_transform(
            recipe.as_bytes().as_ptr(),
            recipe.len(),
            data.as_bytes().as_ptr(),
            data.len(),
        );

        let (val, _) = read_and_free_c_abi_result(ptr);
        assert!(val.get("error").is_some());
    }

    /// Read and free a status-prefixed buffer: (is_ok, payload).
    fn read_status(ptr: *mut u8) -> (bool, String) {
        unsafe {
            let mut len_bytes = [0u8; 4];
            std::ptr::copy_nonoverlapping(ptr, len_bytes.as_mut_ptr(), 4);
            let len = u32::from_le_bytes(len_bytes) as usize;
            let status = *ptr.add(4);
            let payload = std::str::from_utf8(std::slice::from_raw_parts(ptr.add(5), len))
                .unwrap()
                .to_string();
            dto_dealloc(ptr, len + 5);
            (status == STATUS_OK, payload)
        }
    }

    fn new_engine(schema: &str) -> (bool, String) {
        read_status(dto_engine_new(schema.as_ptr(), schema.len()))
    }

    fn run(handle: u32, data: &str) -> (bool, String) {
        read_status(dto_engine_transform(handle, data.as_ptr(), data.len()))
    }

    #[test]
    fn engine_handle_compiles_once_and_transforms_many_times() {
        let (ok, handle) = new_engine(r#"{"name":"T","mappings":[{"source":"user.name","target":"name"}]}"#);
        assert!(ok, "{handle}");
        let handle: u32 = handle.parse().unwrap();

        for name in ["Ada", "Linus"] {
            let (ok, out) = run(handle, &format!(r#"{{"user":{{"name":"{name}"}}}}"#));
            assert!(ok);
            assert_eq!(out, format!(r#"{{"name":"{name}"}}"#));
        }

        assert_eq!(dto_engine_free(handle), 1);
        assert_eq!(dto_engine_free(handle), 0);
        let (ok, err) = run(handle, "{}");
        assert!(!ok);
        assert!(err.contains("unknown or freed engine handle"));
    }

    #[test]
    fn status_byte_separates_errors_from_outputs_named_error() {
        // A real output whose only key is "error" must not be mistaken for a failure.
        let (_, handle) = new_engine(r#"{"name":"T","mappings":[{"source":"msg","target":"error"}]}"#);
        let handle: u32 = handle.parse().unwrap();
        assert_eq!(run(handle, r#"{"msg":"disk full"}"#), (true, r#"{"error":"disk full"}"#.to_string()));

        let (ok, err) = run(handle, "not json");
        assert!(!ok);
        assert!(err.starts_with("input JSON parse error"));
        dto_engine_free(handle);

        let (ok, err) = new_engine(r#"{"name":"T","mappings":[{"source":"a","target":"b","cast":"nope"}]}"#);
        assert!(!ok);
        assert!(err.contains("nope"));
    }

    #[test]
    fn handles_are_not_reused() {
        let schema = r#"{"name":"T","mappings":[{"source":"a","target":"b"}]}"#;
        let first: u32 = new_engine(schema).1.parse().unwrap();
        dto_engine_free(first);
        let second: u32 = new_engine(schema).1.parse().unwrap();
        assert_ne!(first, second);
        dto_engine_free(second);
    }
}
