#!/usr/bin/env bash
set -euo pipefail

echo "🔨 Compiling crates/wasm to WebAssembly (wasm32-unknown-unknown)..."
cargo build -p dto_wasm --target wasm32-unknown-unknown --release

WASM_FILE="target/wasm32-unknown-unknown/release/dto_wasm.wasm"

if [ ! -f "$WASM_FILE" ]; then
    echo "Error: WASM file not found at $WASM_FILE" >&2
    exit 1
fi

FILE_SIZE=$(wc -c < "$WASM_FILE" | tr -d ' ')
echo "✅ Built WASM successfully ($FILE_SIZE bytes)"

echo "📦 Distributing to SDK packages..."

# Go
cp -f "$WASM_FILE" packages/go/dto.wasm
echo "  -> packages/go/dto.wasm"

# Python
cp -f "$WASM_FILE" packages/python/dto/dto.wasm
echo "  -> packages/python/dto/dto.wasm"

# JavaScript / TypeScript
cp -f "$WASM_FILE" packages/js/dto.wasm
echo "  -> packages/js/dto.wasm"

# Java
cp -f "$WASM_FILE" packages/java/src/main/resources/dto.wasm
echo "  -> packages/java/src/main/resources/dto.wasm"

echo "🎉 All packages synced with the latest WebAssembly binary!"
