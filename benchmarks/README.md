# Benchmarks

All numbers below were measured on one machine on 2026-11-13. Re-run them on yours with:

```bash
pwsh benchmarks/run_all.ps1          # sections 2-6, several minutes
pwsh benchmarks/run_all.ps1 -Quick   # smaller sizes, about a minute
```

| | |
|---|---|
| CPU | Intel Core i5-10400 @ 2.90 GHz: **6 cores / 12 threads** |
| OS | Windows 11 Pro |
| Toolchains | Rust 1.99.1, Node.js 25.3.0, Go 1.25.6, Python 3.14.3 (wasmtime) |

To simulate smaller machines, the thread count is limited: `--threads N` for the engine benchmark, or `RAYON_NUM_THREADS=N` for `dto transform`. On a real 2- or 4-core machine, clock speed and cache size also differ. Individual runs vary by about ±10–15%.

There are two kinds of measurement:

- **Engine only** (`dto-bench engine`) times just the transform step. The input is already parsed and the output is not written. It shows what the Rust engine can do inside your own program.
- **End to end** (`dto transform` on a file) includes reading, parsing, transforming and writing JSON. It shows what CLI users get.

Synthetic data comes from the separate `dto_bench` crate (`crates/bench`), so the `dto` binary contains no test payloads. The recipes are in [`recipes/`](recipes/).

---

## 1. Before and after the fixes

| What | Before | After |
|---|---|---|
| Engine, 100k records, 12 threads | 538k records/s | **1.49M records/s** |
| Engine, 100k records, 1 thread | 100k records/s | **273k records/s** |
| Engine, 6.4 KB records, 2,000 per batch | 205k records/s | **413k records/s** |
| `dto transform`, 95 MB / 300k records | 3.6 s, **2.0 GB** peak | 3.0 s, **112 MB** peak |
| `dto transform --chunk-size 2000`, same file | 9.2 s, **1.6 GB** peak (input not streamed) | 3.5 s, **23 MB** peak |
| JS SDK, 100k records in one call | 68k records/s | ~94–125k records/s |
| JS SDK, 20k one-record calls | 26k records/s | 73k (`transform`, cached) / 103k (`compile`) records/s |
| Go SDK heap after 8 large transforms with GC between them | 80 → 604 MB (leak) | flat at 80 MB |

The engine got faster because paths and transform chains are now compiled once per batch instead of once per field per record. CLI memory dropped because input is now streamed.

## 2. Engine thread scaling

Records held in memory at once, 9-field recipe, 0.3 KB records:

| Records | 1 thread | 2 | 4 | 6 | 12 | Peak memory |
|---|---|---|---|---|---|---|
| 10k | 255k/s | 575k/s | 1.03M/s | 1.55M/s | 1.63M/s | 76 MB |
| 50k | 244k/s | 435k/s | 698k/s | 1.05M/s | 1.56M/s | 356 MB |
| 100k | 255k/s | 487k/s | 718k/s | 908k/s | 1.38M/s | 706 MB |
| 250k | 275k/s | 472k/s | 882k/s | 1.06M/s | 1.54M/s | 1.76 GB |

- **6 threads (all physical cores)** are 3.6–4.3× faster than 1 thread; for 250k records, 908 ms becomes 235 ms.
- **12 threads (hyperthreading)** reach 5.4–6.4×.
- **10k rows take only a few milliseconds**, so those figures are noisy.
- **Memory depends on record count, not threads**, because this mode holds every record at once (see section 4).

**6.4 KB enterprise records** (100k, 2,000 per batch, about 75 MB peak in every case):

| 1 thread | 2 | 4 | 6 | 12 |
|---|---|---|---|---|
| ~94k/s | ~143k/s | ~255k/s | 342k/s | 392k/s |

Large records scale less, because more of the time goes into memory allocation. The 1-, 2- and 4-thread figures are the averages of repeated runs; one early 2-thread run measured 90k/s.

## 3. Why 250k records take 1.76 GB

A parsed record is much larger than its JSON text, because every object, array and string is a separate memory allocation. `dto-bench calibrate` measures this:

| Record | JSON size | In memory | `estimate_heap_bytes` vs measured |
|---|---|---|---|
| narrow | 0.3 KB | ~5.9 KB | 111% |
| wide | 3.9 KB | ~23.8 KB | 106% |
| enterprise | 6.4 KB | ~35.5 KB | 113% |

250k narrow records therefore take ~1.44 GB for the inputs, plus ~0.3 GB for the outputs.

Storing JSON objects in a compact map instead (serde_json's `preserve_order`) was tried and rejected. Input memory fell 22% (1.44 → 1.12 GB), but peak memory barely changed (1.76 → 1.73 GB) and throughput dropped 38% (1.52M → 0.95M/s).

## 4. Batch size vs memory

250k records, 12 threads:

| Records per batch | Speed | Peak memory |
|---|---|---|
| all at once | 1.54M/s | 1,763 MB |
| 500 | 1.04M/s | 12 MB |
| 1,000 | 1.26M/s | 16 MB |
| 2,000 | 1.27M/s | 25 MB |
| **5,000** | **1.37M/s** | **45 MB** |
| 10,000 | 1.27M/s | 80 MB |
| 50,000 | 1.39M/s | 362 MB |
| **1,000,000 records**, 5,000 per batch | 1.27–1.31M/s (0.73–0.87 s per run, 0.79 s on average) | **46 MB** |

- **5,000 per batch** keeps 89% of the speed with 97% less memory.
- **Memory depends on batch size, not total records:** one million records also fit in 46 MB.
- **Very small batches are slower**, because each batch has a fixed cost to spread over the threads.
- **On fewer threads**, with 5,000 per batch: 1.07M/s on 6 and 480k/s on 2, both at about 42 MB.

## 5. End to end: `dto transform` on files

Default mode (records streamed 2,000 at a time). Best of 3 runs:

| File | 1 thread | 2 | 4 | 6 | 12 | Peak memory |
|---|---|---|---|---|---|---|
| 10k records (3 MB) | 84k/s (0.12 s) | 89k/s | 94k/s | 97k/s | 83k/s | 22 MB |
| 100k records (31 MB) | 92k/s (1.1 s) | 94k/s | 101k/s | 97k/s | 101k/s | 49 MB |
| 300k records (95 MB) | 73k/s (4.1 s) | 85k/s | 106k/s | 116k/s (2.6 s) | 115k/s | 110 MB |

More cores barely help here, because reading and writing JSON run on one thread. With 12 threads on the 300k file, the transform step takes about 0.1 s of the 2.6 s total. On small files, process start-up dominates.

## 6. Batching modes: fixed row count vs memory cap

Whole-process peak memory, 12 threads:

| Mode | 300k narrow rows (0.3 KB) | 50k wide rows (3.9 KB) |
|---|---|---|
| `--chunk-size 5000` | 41 MB (5,000 rows/batch) | 121 MB (5,000 rows/batch) |
| `--max-memory 16MB` | 24 MB (~2,300 rows/batch) | 24 MB (~640 rows/batch) |
| `--max-memory 64MB` | 68 MB (~8,800 rows/batch) | 68 MB (~2,400 rows/batch) |
| `--max-memory 256MB` | 242 MB (~30,000 rows/batch) | 231 MB (~8,300 rows/batch) |

- **A fixed row count uses 3× more memory on wide rows** than on narrow ones.
- **A memory cap gives the same peak for both widths**; it simply puts fewer rows in each batch when rows are wide.
- **Speed was about the same in all modes** (~4.0–4.4 s for either file).
- **The extra ~8 MB** above small caps is the program itself.

## 7. SDKs (JavaScript)

100k records, same 9-field mapping (`node packages/js/bench/compare_plain_js.mjs 100000`):

| Implementation | Speed |
|---|---|
| Hand-written `Array.map` | ~2.5–3.3M/s |
| Generic ~25-line JS recipe interpreter | ~390–545k/s |
| WASM SDK `transform()` | ~92–125k/s |

All three produce identical output. The SDK pays for copying JSON text into WebAssembly and back, and the WASM build runs on one thread. Its value is identical behaviour in Go, Python, JS and Java, not raw speed.

One large batch vs many small calls (`node packages/js/bench/compile_vs_oneshot.mjs`):

| | `transform(recipe, data)` | `compile(recipe)` then `.transform(data)` |
|---|---|---|
| 100k records, one call | ~108–124k/s | ~113–128k/s |
| 20k calls × 1 record | ~73–81k/s | ~86–103k/s |

The Go memory-leak check is a regular test: `go test -run TestPoolDoesNotLeakMemoryAcrossGC` in `packages/go`.

## Reproducing individual numbers

```bash
cargo build --release -p dto_cli -p dto_bench

# Engine only
target/release/dto-bench engine --count 250000 --threads 6
target/release/dto-bench engine --count 1000000 --chunk-size 5000
target/release/dto-bench engine --count 100000 --payload-kb 6.4 --chunk-size 2000
target/release/dto-bench calibrate

# End to end
target/release/dto-bench generate --kind narrow --count 300000 -o narrow.json
target/release/dto-bench generate --kind wide --count 50000 -o wide.json
RAYON_NUM_THREADS=6 target/release/dto transform -s benchmarks/recipes/narrow.json -i narrow.json > out.json
target/release/dto transform -s benchmarks/recipes/wide.json -i wide.json --max-memory 64MB --stats > out.json
```
