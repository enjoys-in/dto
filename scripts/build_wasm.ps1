# One-click build script to compile WebAssembly and distribute to all SDK packages (Go, Python, JS, Java)
$ErrorActionPreference = "Stop"

Write-Host "🔨 Compiling crates/wasm to WebAssembly (wasm32-unknown-unknown)..." -ForegroundColor Cyan
cargo build -p dto_wasm --target wasm32-unknown-unknown --release

$wasmFile = "target/wasm32-unknown-unknown/release/dto_wasm.wasm"

if (!(Test-Path $wasmFile)) {
    Write-Error "WASM file not found at $wasmFile"
    exit 1
}

$fileSize = (Get-Item $wasmFile).Length
Write-Host "✅ Built WASM successfully ($fileSize bytes)" -ForegroundColor Green

Write-Host "📦 Distributing to SDK packages..." -ForegroundColor Cyan

# Go
Copy-Item $wasmFile packages/go/dto.wasm -Force
Write-Host "  -> packages/go/dto.wasm" -ForegroundColor Gray

# Python
Copy-Item $wasmFile packages/python/dto/dto.wasm -Force
Write-Host "  -> packages/python/dto/dto.wasm" -ForegroundColor Gray

# JavaScript / TypeScript
Copy-Item $wasmFile packages/js/dto.wasm -Force
Write-Host "  -> packages/js/dto.wasm" -ForegroundColor Gray

# Java
Copy-Item $wasmFile packages/java/src/main/resources/dto.wasm -Force
Write-Host "  -> packages/java/src/main/resources/dto.wasm" -ForegroundColor Gray

Write-Host "🎉 All packages synced with the latest WebAssembly binary!" -ForegroundColor Green
