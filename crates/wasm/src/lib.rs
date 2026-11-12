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
}
