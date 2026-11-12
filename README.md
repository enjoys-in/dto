# DTO Transformation Engine (Rust + WebAssembly)

High-performance, multi-threaded Data Transfer Object (DTO) transformation engine written in **Rust** with **WebAssembly** bindings for **Go**, **Python**, **JavaScript/TypeScript**, and **Java**.

Transform arbitrary JSON data structures (single objects or arrays of objects) into target DTO shapes using **Dot-Notation Path Navigation** (`profile.address.city`, `items[0].id`, `roles[*].name`) across three transformation models:
1. **Recipes ("Cooking Recipes")**: Ergonomic mappings with `source` → `target`, function chaining, output constraints, and required field validation.
2. **Enterprise Medallion Pipelines**: Multi-target ELT pipelines projecting Bronze documents into multiple Silver tables with `unpack: "array_explode"`, parent header field broadcasting, and `dedup_strategy: "latest"` by business keys.
3. **JSON Schema**: Standard JSON Schema extended with `x-from` and `x-transform` annotations.

---

## Architecture Overview

```
                        ┌─────────────────────────────────────────────────────────┐
                        │                       Input Data                        │
                        │        Single Object  OR  Array of Objects [ {..} ]      │
                        └────────────────────────────┬────────────────────────────┘
                                                     │
                                                     ▼
┌─────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    dto_core (Rust Engine)                                       │
│                                                                                                 │
│   ┌───────────────────────────┐  ┌──────────────────────────┐  ┌────────────────────────────┐   │
│   │   Dot-Notation Parser     │  │   Schema / Recipe Model  │  │  Multi-Threaded Parallel   │   │
│   │   (ast, tokenizer, res)   │  │ (mappings, constraints)  │  │   Batch Engine (Rayon)     │   │
│   │                           │  │                          │  │                            │   │
│   │   - Nested dot navigation │  │ - target (primary) / to  │  │ - Non-blocking data parallelism│
│   │   - Wildcard array access │  │ - cast (primary)/transform│ - Header/Line explode       │
│   │   - Negative array indices│  │ - Medallion pipelines    │  │ - Business key dedup       │
│   └───────────────────────────┘  └──────────────────────────┘  └────────────────────────────┘   │
└────────────────────────────────────────────────┬────────────────────────────────────────────────┘
                                                 │
                                                 ▼
┌─────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    dto_wasm (WebAssembly)                                       │
│                        Length-Prefixed, Thread-Safe Memory Exchange Protocol                    │
└────────────────┬───────────────────────┬────────────────────────┬──────────────────────┬────────┘
                 │                       │                        │                      │
                 ▼                       ▼                        ▼                      ▼
         ┌───────────────┐       ┌───────────────┐        ┌───────────────┐      ┌───────────────┐
         │      Go       │       │    Python     │        │    JS / TS    │      │     Java      │
         │   (Wazero)    │       │  (Wasmtime)   │        │   (Node/Web)  │      │   (Chicory)   │
         │   Zero CGO    │       │  PyPI Package │        │  Native Wasm  │      │    Pure JVM   │
         └───────────────┘       └───────────────┘        └───────────────┘      └───────────────┘
```

---

## 1. Recipe Format (The "Cooking Recipe")

Recipes define clean `source` $\rightarrow$ `target` field mappings:

```json
{
  "name": "UserSummaryDTO",
  "version": "1.0.0",
  "mappings": [
    { "source": "user.profile.full_name",    "target": "name" },
    { "source": "user.address.city",         "target": "city" },
    { "source": "user.contact.emails[0]",    "target": "primaryEmail" },
    { "source": "user.tags[*]",              "target": "skills", "transform": "uppercase" },
    { "source": "user.age",                  "target": "age",    "cast": "decimal" },
    { "source": "v1.0",                      "target": "version", "literal": true }
  ],
  "constraints": {
    "name": { "type": "string", "min_length": 1, "max_length": 255 },
    "age":  { "type": "number", "min": 0, "max": 150 }
  },
  "required": ["name", "primaryEmail"]
}
```

> **Note on Naming**: `target` is the primary field name (with `to` supported as a backwards-compatible alias). `cast` is the primary type converter (with `transform` supported as an alias).

### Supported Path Expressions
- **Field access**: `user.profile.name`. Unquoted names may contain letters, digits, `_` and `-` (`headers.content-type`)
- **Quoted keys** for any other characters: `user["first name"]`, `meta['@type']`, `data["a.b"]` (`\` escapes a quote)
- **Array indexing**: `orders[0].id`, `items[-1].sku` (negative index counts from end)
- **Wildcard projections**: `roles[*].name`, `tags[*]` (extracts array)
- **Literal values**: `"literal": true` treats `source` as a constant string

### Function Chaining / Pipeline Syntax

You can chain multiple transformations using either **pipe (`|`)**, **comma (`,`)**, or **arrow (`->`)** syntax:

#### Option 1: Inline directly in the `source` path
```json
{
  "mappings": [
    { "source": "user.name | trim | lowercase",  "target": "clean_name" },
    { "source": "user.phone | to_string | trim", "target": "phone_string" },
    { "source": "user.score | trim | to_number", "target": "score_float" }
  ]
}
```

#### Option 2: In the `transform` / `cast` field
```json
{
  "mappings": [
    { "source": "user.name", "target": "clean_name", "transform": "trim | lowercase" },
    { "source": "user.tags[*]", "target": "skills",  "transform": "trim | uppercase" }
  ]
}
```

#### Available Built-in Functions & Casts:
- **String**: `trim`, `lowercase` (or `lower`), `uppercase` (or `upper`), `reverse`
- **Numeric & Casting**: `to_string` (`string`), `to_number` (`number`, `decimal`, `float`, `int`, `integer`), `to_bool` (`bool`, `boolean`), `date`, `timestamp`, `json`
- **Collection / Array**: `first`, `last`, `length`, `keys`, `values`, `flatten`, `unique`, `sort`, `compact`

**Date & time casts.** `date` and `timestamp` accept an ISO-8601 date (`2024-03-15`) or date-time (`T` or space separator, optional seconds, fraction and `Z`/`±HH:MM` offset), or a Unix epoch number (seconds; milliseconds when ≥ 1e11).
- `date` → `YYYY-MM-DD`. The calendar date is taken as written; no time-zone conversion.
- `timestamp` → `YYYY-MM-DDTHH:MM:SS[.fraction][offset]`. A date-only value becomes midnight, an offset is kept as written, and epoch numbers are rendered in UTC with `Z`.
- Anything else (`"not a date"`, `2024-02-30`, `15/03/2024`) is an error. It is never truncated.

### Error Handling
- **Configuration errors fail when the engine is created**: unknown functions, invalid paths, `required` fields with no mapping, duplicate targets, unknown constraint types, unknown pipeline `grain` / `dedup_strategy` / `unpack` values.
- **Array input fails as a whole** if any record fails (missing required field, failed cast, constraint violation, non-object element). The error names the record's position in the input, e.g. `item 1: required field 'id' is missing`. To keep the good records and collect the bad ones, use `DtoEngine::transform_batch` (Rust). Its `BatchResult` has `items`, their original `indices`, `errors` and `validation_errors`.
- **Constraints**: `type` (`string`, `number`, `integer`, `boolean`, `array`, `object`, `null`), `min`/`max`, `min_length`/`max_length` (counted in characters), `allowed`, `pattern`, and an optional `message` that replaces the default error text.
  - `pattern` is a regular expression for string values. Like JSON Schema it can match anywhere in the value, so use `^...$` to match the whole value, e.g. `"pattern": "^PO-\\d{3}$"`. It uses the `regex-lite` syntax: `\d`, `\w` and `\s` match ASCII only, and Unicode classes such as `\p{L}` are not available. An invalid pattern is reported when the recipe loads.

---

## 2. Enterprise Medallion Pipelines (Bronze → Silver Multi-Table ETL)

For enterprise data ingestion, the engine natively executes multi-target relational pipelines:

```json
{
  "sources": [
    {
      "id": "item",
      "bronze_table": "stg_upload"
    }
  ],
  "targets": [
    {
      "table": "silver_part",
      "grain": "header",
      "business_key": ["part_number"],
      "dedup_strategy": "latest",
      "mappings": [
        { "source": "item_id", "target": "part_number", "cast": "string", "kind": "column" },
        { "source": "reference.price", "target": "standard_cost", "cast": "decimal", "kind": "column" }
      ]
    },
    {
      "table": "silver_purchase_order_line",
      "grain": "line",
      "business_key": ["po_id", "extra_po_line_key"],
      "dedup_strategy": "latest",
      "mappings": [
        { "source": "item_id", "target": "part_number", "cast": "string", "kind": "column" },
        { "source": "transactions[*].purchaseOrderId", "target": "po_id", "cast": "string", "unpack": "array_explode" },
        { "source": "transactions[*].quantity", "target": "quantity", "cast": "decimal", "unpack": "array_explode" },
        { "source": "transactions[*]", "target": "flex_attributes", "cast": "json", "unpack": "array_explode" }
      ]
    }
  ]
}
```

### Medallion Engine Capabilities:
1. **Multi-Table Fan-Out**: Automatically routes one incoming Bronze document into multiple distinct Silver tables.
2. **`unpack: "array_explode"`**: Unrolls nested arrays into individual rows while automatically **broadcasting parent fields** (e.g. `item_id`) down to each line item.
3. **Deduplication (`dedup_strategy: "latest"`)**: Groups rows by their `business_key` and keeps one row per key. Set `sequence_by` (alias `order_by`) to an output column to keep the row with the greatest value. Numbers compare numerically and strings lexicographically, so ISO-8601 timestamps sort correctly when they share one offset; ties go to the later row. Without `sequence_by`, the **last row in input order** wins. Rows whose key column is missing or null are never merged. `business_key` and `sequence_by` must name columns that a mapping produces.
4. **Grain**: `grain: "header"` produces one row per document, even when a mapping such as `roles[*].name` yields an array column. `grain: "line"`, or any `unpack: "array_explode"` mapping, produces one row per element of the exploded array. Inline pipes (`transactions[*].qty | to_number`) work on exploded fields.
5. **Column Categorization**: Supports `kind: "column"` and `kind: "extension"`.
6. **Source filters**: a source's `filters` work like an `if` condition on each input record, and `filter_action` decides what to do with records that match:

   ```json
   "sources": [{
     "id": "item",
     "filters": { "source_system": "api_ingest", "meta.entity": "part" },
     "filter_action": "keep"
   }]
   ```

   | `filter_action` | Effect |
   |---|---|
   | `"keep"` (default) | Only matching records are processed; the rest are skipped |
   | `"delete"` | Matching records are skipped; the rest are processed |
   | `"ignore"` | The filter is switched off; every record is processed |

   A record matches when **every** `path: value` pair is equal. Paths use the same dot notation as mappings, numbers compare by value (`1` equals `1.0`), and a `null` value also matches a missing field. A target uses the source named by its `source_id`. If the pipeline has exactly one source, targets without `source_id` use it automatically. With several sources, each target must set `source_id`.

Targets accept only the keys documented here. Unknown keys, including the removed `contract` block, are rejected with an error instead of being silently ignored.

---

## 3. Host SDK Packages

### Go (`packages/go`)
Pure Go, **zero CGO**, powered by [wazero](https://wazero.io/):

```go
package main

import (
    "context"
    "fmt"
    dto "github.com/enjoys/dto-go"
)

func main() {
    ctx := context.Background()
    engine, err := dto.New(ctx)
    if err != nil {
        panic(err)
    }
    defer engine.Close(ctx)

    recipe := []byte(`{
        "name": "UserSummary",
        "mappings": [
            { "source": "profile.name", "target": "name" },
            { "source": "profile.city", "target": "city" }
        ]
    }`)

    data := []byte(`{"profile": {"name": "Gopher", "city": "Berlin"}}`)

    out, err := engine.Transform(recipe, data)
    if err != nil {
        panic(err)
    }
    fmt.Println(string(out))
    // {"city":"Berlin","name":"Gopher"}

    // Compile once, transform many times (the recipe is parsed only once).
    compiled, err := engine.Compile(recipe)
    if err != nil {
        panic(err)
    }
    defer compiled.Close()
    out, err = compiled.Transform([]byte(`{"profile": {"name": "Ferris", "city": "Oslo"}}`))
}
```

### Python (`packages/python`)
Powered by [wasmtime-py](https://github.com/bytecodealliance/wasmtime-py):

```python
from dto import DTOEngine

engine = DTOEngine()

recipe = {
    "name": "UserSummary",
    "mappings": [
        {"source": "user.name", "target": "name"},
        {"source": "user.tags[*]", "target": "tags", "transform": "uppercase"}
    ]
}

data = {
    "user": {
        "name": "Guido",
        "tags": ["python", "wasm"]
    }
}

result = engine.transform(recipe, data)
print(result)
# {'name': 'Guido', 'tags': ['PYTHON', 'WASM']}

# Compile once, transform many times (the recipe is parsed only once).
with engine.compile(recipe) as compiled:
    for record in records:
        print(compiled.transform(record))
```

### JavaScript / TypeScript (`packages/js`)
Runs in Node.js, Bun, Deno, and modern Browsers:

```typescript
import { DTOEngine } from 'dto-wasm';

const engine = await DTOEngine.init();

const recipe = {
  name: 'UserSummary',
  mappings: [
    { source: 'account.name', target: 'name' },
    { source: 'account.city', target: 'city' }
  ]
};

const data = {
  account: { name: 'Alice', city: 'Tokyo' }
};

const result = engine.transform(recipe, data);
console.log(result);
// { name: 'Alice', city: 'Tokyo' }

// Compile once, transform many times (the recipe is parsed only once).
const compiled = engine.compile(recipe);
try {
  for (const record of records) console.log(compiled.transform(record));
} finally {
  compiled.free();
}
```

### Java (`packages/java`)
Zero native dependencies (no JNI DLLs) via [Chicory](https://github.com/dylibso/chicory):

```java
import com.dto.CompiledTransform;
import com.dto.DTOEngine;

public class Main {
    public static void main(String[] args) {
        try (DTOEngine engine = new DTOEngine()) {
            String recipe = "{\"name\":\"T\",\"mappings\":[{\"source\":\"user.name\",\"target\":\"name\"}]}";
            String data = "{\"user\":{\"name\":\"Duke\"}}";

            String result = engine.transform(recipe, data);
            System.out.println(result);
            // {"name":"Duke"}

            // Compile once, transform many times (the recipe is parsed only once).
            try (CompiledTransform compiled = engine.compile(recipe)) {
                System.out.println(compiled.transform("{\"user\":{\"name\":\"Ada\"}}"));
            }
        }
    }
}
```

### Errors and performance in the SDKs
- Every SDK reports failures as an error: Go returns an `error`, Python raises `RuntimeError`, JS throws `Error`, and Java throws `DTOException`. The message includes the failing record's position, e.g. `item 1: required field 'id' is missing`. A successful output that happens to contain only an `error` field is returned normally.
- Each call copies JSON text into WebAssembly and back, and the WASM build runs on a single thread. In the JS SDK, one 100k-record batch runs at ~125k records/s. For 20k one-record calls, `transform()` runs at ~73k records/s (its cache saves re-parsing the recipe, but it still serializes the recipe to look it up) and `compile()` at ~103k. A hand-written JS mapping is faster still, so the SDKs' value is identical behaviour across languages, not raw speed. For maximum throughput, use the Rust crate or the CLI.

### Caching, concurrency and memory in the SDKs
| | Go | Python | JS | Java |
|---|---|---|---|---|
| `transform()` recipe cache | 64 per instance, LRU | 64, LRU | 64, LRU | 64, LRU |
| Concurrency | `Engine` pool: up to GOMAXPROCS instances, extra callers wait; `Compiled` serializes calls | one engine per thread | single-threaded | engine is `synchronized` |
| Forgotten `close()`/`free()` | finalizer releases the instance | `weakref.finalize` frees the handle | `FinalizationRegistry` frees the handle | `Cleaner` frees the handle |
| WebAssembly memory (never shrinks) | instances above 64 MB are closed and replaced after use | drop the engine to release it | drop the engine to release it | drop the engine to release it |
| Instance traps (crash inside WASM) | instance discarded and replaced | engine refuses further calls | engine refuses further calls | engine refuses further calls |

`liveHandles` (JS/Java) / `live_handles` (Python) reports how many compiled recipes exist inside WebAssembly, which the tests use to check for leaks.

---

## 4. Command Line Tool (`dto`)

```bash
# Transform data using a Recipe, Medallion Pipeline, or JSON Schema
cargo run --bin dto -- transform -s recipe.json -i data.json --pretty

# Or pipe through stdin
cat data.json | cargo run --bin dto -- transform -s recipe.json

# Arrays are streamed automatically: records are parsed, transformed and written
# 2,000 at a time, so memory stays near the file size
# (95 MB / 300k-record file: ~3.0 s, ~112 MB peak, versus ~1.9 GB when fully parsed).

# Fixed batching: read the file incrementally and transform N records at a time.
# Memory depends on row width: 5,000 narrow rows ~41 MB, 5,000 wide (4 KB) rows ~121 MB.
cargo run --release --bin dto -- transform -s recipe.json -i big.json --chunk-size 5000

# Auto batching: cap the memory held for records instead of the row count.
# Each row's in-memory size is estimated as it is read, and a batch is
# transformed when its inputs plus outputs would reach the cap, so narrow rows
# get large batches and wide rows small ones. --stats prints what was chosen.
cargo run --release --bin dto -- transform -s recipe.json -i big.json --max-memory 64MB --stats
# dto: 300000 records in 34 batches (records per batch: min 1840, avg 8823, max 9301), largest batch ~64.0 MB estimated

# Pipelines are always processed whole because deduplication needs every record,
# so --chunk-size and --max-memory are rejected for them.
```

Measured peak process memory (12 threads):

| Mode | 300k narrow rows (0.3 KB) | 50k wide rows (3.9 KB) |
|---|---|---|
| `--chunk-size 5000` | 41 MB (5,000 rows/batch) | 121 MB (5,000 rows/batch) |
| `--max-memory 16MB` | 24 MB (~2,300 rows/batch) | 24 MB (~640 rows/batch) |
| `--max-memory 64MB` | 68 MB (~8,800 rows/batch) | 68 MB (~2,400 rows/batch) |
| `--max-memory 256MB` | 242 MB (~30,000 rows/batch) | 231 MB (~8,300 rows/batch) |

The cap covers the records; the process itself adds roughly 8–20 MB. A single row larger than the cap is processed on its own. Very small caps make batches small, which spreads less work across cores.

From Rust, the same two modes are `DtoEngine::transform_stream(items, rows, on_chunk)` and `DtoEngine::transform_stream_auto(items, max_bytes, on_chunk)`. The second returns `StreamStats`. `estimate_heap_bytes(&value)` exposes the per-row estimate, which lands within ~10–15% of measured memory and errs high.

```bash

# Validate syntax and structure of a recipe/schema
cargo run --bin dto -- validate -s recipe.json

# List all available functions and cast keywords
cargo run --bin dto -- functions
```

---

## 5. Performance Benchmarks

Full results, methodology and the script that reproduces them are in **[benchmarks/README.md](benchmarks/README.md)**. They were measured on an Intel i5-10400 (6 cores / 12 threads).

| Scenario | Result |
|---|---|
| Rust engine, 250k records, all held in memory | 275k records/s on 1 thread → 1.06M on 6 → 1.54M on 12; 1.76 GB peak |
| Same, 5,000 records per batch | 1.37M records/s at **45 MB** peak; 1M records in ~0.8 s at 46 MB |
| `dto transform`, 95 MB / 300k-record file | ~2.6–3.0 s, ~110 MB peak (reading and writing JSON dominate, so more cores add little) |
| `dto transform --max-memory 64MB` | ~68 MB peak for both narrow (0.3 KB) and wide (3.9 KB) rows |
| JS SDK (WASM) | ~92–125k records/s |

"Engine" figures time only the transform step on records that are already parsed. Synthetic data and the benchmark runner live in the separate `dto_bench` crate, not in the `dto` binary:

```bash
cargo run --release -p dto_bench -- engine --count 250000 --threads 6 --chunk-size 5000
pwsh benchmarks/run_all.ps1 -Quick
```

---

## 6. Examples Suite

Explore runnable examples in the `examples/` directory:
- [`01_user_profile`](examples/01_user_profile/): Basic dot-notation mapping with constraints and validation.
- [`02_chaining_pipeline`](examples/02_chaining_pipeline/): Multi-stage function chaining (`trim | lowercase`, `to_string | trim`).
- [`03_batch_parallel`](examples/03_batch_parallel/): Multi-threaded Rayon batch parallel execution over record arrays.
- [`04_json_schema`](examples/04_json_schema/): Standard JSON Schema with `x-from` and `x-transform`.
- [`05_medallion_pipeline`](examples/05_medallion_pipeline/): Enterprise Medallion Bronze $\rightarrow$ Silver multi-table fan-out with `unpack: "array_explode"` and deduplication.

---

## 7. Running Tests Across All Environments

```bash
# 1. Rust workspace unit tests (220+ tests)
cargo test --workspace

# 2. Go SDK tests (Wazero pure Go wasm)
cd packages/go && go test -v ./...

# 3. Python SDK tests (wasmtime-py)
cd packages/python && python -m pytest -v

# 4. JavaScript SDK tests (Node.js native test runner)
cd packages/js && node --test
```
