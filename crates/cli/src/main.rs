//! # dto_cli
//!
//! Command-line interface for the DTO transformation engine.
//! Supports both Recipes and JSON Schema, single and batch parallel processing,
//! streaming with fixed or memory-capped batches. Benchmarks live in the
//! separate `dto_bench` crate so this binary contains no synthetic data.

use std::fmt;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use dto_core::{estimate_heap_bytes, parse_size, AutoChunker, DtoEngine, StreamStats};
use serde::de::{Error as _, SeqAccess, Visitor};
use serde::Deserializer as _;
use serde_json::Value;

#[derive(Parser, Debug)]
#[command(
    name = "dto",
    version,
    about = "High-performance DTO data transformation engine",
    long_about = "Transform JSON data using JSON Schema (with x-from) or Recipes (source->target mappings).\nSupports batch parallel processing, non-blocking execution, and dot-notation paths."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Transform JSON data using a schema or recipe
    Transform {
        /// Path to the schema or recipe file (JSON)
        #[arg(short, long)]
        schema: PathBuf,

        /// Path to the input JSON file (reads from stdin if omitted)
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Pretty-print the output JSON
        #[arg(short, long, default_value_t = false)]
        pretty: bool,

        /// Disable constraint validation (validation is enabled by default)
        #[arg(long, default_value_t = false)]
        no_validate: bool,

        /// Fixed batching: transform N records at a time, reading the input incrementally (e.g. 5000)
        #[arg(long, conflicts_with = "max_memory")]
        chunk_size: Option<usize>,

        /// Auto batching: cap the memory held for records (inputs + outputs) per batch,
        /// e.g. 64MB or 1GB. Batch sizes adapt to row width. Process memory adds ~10-20 MB.
        #[arg(long, value_parser = parse_size)]
        max_memory: Option<usize>,

        /// Print batch statistics (records, batches, batch sizes) to stderr
        #[arg(long, default_value_t = false)]
        stats: bool,
    },

    /// Validate the syntax and structure of a schema or recipe file
    Validate {
        /// Path to the schema or recipe file (JSON)
        #[arg(short, long)]
        schema: PathBuf,
    },

    /// List all available built-in transform functions
    Functions,
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

/// Records per chunk when streaming an array without `--chunk-size` / `--max-memory`.
const DEFAULT_CHUNK: usize = 2000;
const IO_BUFFER: usize = 1 << 20;

/// How a streamed array is split into batches.
enum Batching {
    /// Always `n` records per batch (`--chunk-size`, and the default 2000).
    Fixed(usize),
    /// Batches sized by estimated memory (`--max-memory`).
    Auto(AutoChunker),
}

/// Load the schema and transform the input.
///
/// - Default, top-level array: the input is read as raw bytes and its records
///   are parsed, transformed and written [`DEFAULT_CHUNK`] at a time, so memory
///   stays near the file size instead of ~20x it for a fully parsed document.
/// - `--chunk-size N`: the input is also *read* incrementally, `N` records per batch.
/// - `--max-memory SIZE`: read incrementally; each batch is closed when its
///   records (inputs + outputs, estimated) would exceed SIZE, so wide rows get
///   small batches and narrow rows large ones.
/// - Pipelines and non-array input are transformed whole (pipeline
///   deduplication needs every record).
fn run_transform(
    schema: &Path,
    input: Option<&Path>,
    pretty: bool,
    no_validate: bool,
    chunk_size: Option<usize>,
    max_memory: Option<usize>,
    show_stats: bool,
) -> Result<(), String> {
    let schema_content = fs::read_to_string(schema)
        .map_err(|e| format!("failed to read schema file {}: {e}", schema.display()))?;
    let mut engine = DtoEngine::from_json(&schema_content)
        .map_err(|e| format!("invalid schema/recipe: {e}"))?;
    if no_validate {
        engine.set_validation(false);
    }
    if (chunk_size.is_some() || max_memory.is_some()) && engine.is_pipeline() {
        return Err("--chunk-size/--max-memory cannot be used with a pipeline: deduplication \
                    needs the whole input. Run without them."
            .into());
    }

    let reader: Box<dyn BufRead> = match input {
        Some(path) => Box::new(io::BufReader::with_capacity(
            IO_BUFFER,
            fs::File::open(path)
                .map_err(|e| format!("failed to read input file {}: {e}", path.display()))?,
        )),
        None => Box::new(io::BufReader::with_capacity(IO_BUFFER, io::stdin())),
    };
    let stdout = io::stdout();
    let mut out = io::BufWriter::with_capacity(IO_BUFFER, stdout.lock());

    let stats = match (chunk_size, max_memory) {
        (_, Some(bytes)) => {
            transform_streaming(&engine, reader, &mut out, Batching::Auto(AutoChunker::new(bytes)), pretty)?
        }
        (Some(n), None) => transform_streaming(&engine, reader, &mut out, Batching::Fixed(n), pretty)?,
        (None, None) => transform_buffered(&engine, reader, &mut out, pretty)?,
    };
    out.flush().map_err(|e| format!("failed to write output: {e}"))?;

    if show_stats {
        match stats {
            Some(s) => {
                let avg = s.records.checked_div(s.batches).unwrap_or(0);
                let mut line = format!(
                    "dto: {} records in {} batches (records per batch: min {}, avg {}, max {})",
                    s.records, s.batches, s.min_batch, avg, s.max_batch
                );
                if s.max_batch_bytes > 0 {
                    line.push_str(&format!(
                        ", largest batch ~{:.1} MB estimated",
                        s.max_batch_bytes as f64 / 1048576.0
                    ));
                }
                eprintln!("{line}");
            }
            None => eprintln!("dto: input transformed in one piece (not an array, or a pipeline)"),
        }
    }
    Ok(())
}

/// Read the whole input as bytes. A top-level array (recipe/schema engines) is
/// then streamed record by record; anything else is transformed whole.
/// Returns batch statistics for streamed arrays.
fn transform_buffered<R: Read, W: Write>(
    engine: &DtoEngine,
    mut reader: R,
    out: &mut W,
    pretty: bool,
) -> Result<Option<StreamStats>, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("failed to read input: {e}"))?;

    let first = bytes.iter().copied().find(|b| !b.is_ascii_whitespace());
    if first == Some(b'[') && !engine.is_pipeline() {
        let mut de = serde_json::Deserializer::from_slice(&bytes);
        return stream_array(engine, &mut de, out, Batching::Fixed(DEFAULT_CHUNK), pretty).map(Some);
    }

    let input: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("input JSON parse error: {e}"))?;
    drop(bytes);
    write_whole(engine, input, out, pretty)?;
    Ok(None)
}

/// Read a top-level JSON array incrementally and stream it in batches.
/// Non-array input falls back to [`transform_buffered`].
///
/// On error the process exits non-zero and the output written so far is incomplete.
fn transform_streaming<R: BufRead, W: Write>(
    engine: &DtoEngine,
    mut reader: R,
    out: &mut W,
    batching: Batching,
    pretty: bool,
) -> Result<Option<StreamStats>, String> {
    match first_non_whitespace(&mut reader).map_err(|e| format!("failed to read input: {e}"))? {
        Some(b'[') => {}
        _ => return transform_buffered(engine, reader, out, pretty),
    }
    let mut de = serde_json::Deserializer::from_reader(reader);
    stream_array(engine, &mut de, out, batching, pretty).map(Some)
}

/// Pull array elements from `de` one at a time, transforming and writing them
/// batch by batch. Returns the batch statistics.
fn stream_array<'de, R: serde_json::de::Read<'de>, W: Write>(
    engine: &DtoEngine,
    de: &mut serde_json::Deserializer<R>,
    out: &mut W,
    batching: Batching,
    pretty: bool,
) -> Result<StreamStats, String> {
    out.write_all(b"[").map_err(|e| format!("failed to write output: {e}"))?;
    let visitor = StreamArray {
        engine,
        out: &mut *out,
        batching: match batching {
            Batching::Fixed(n) => Batching::Fixed(n.max(1)),
            auto => auto,
        },
        pretty,
        written: 0,
        fixed_stats: StreamStats::default(),
    };
    let stats = (&mut *de)
        .deserialize_seq(visitor)
        .map_err(|e| format!("transform failed: {e}"))?;
    de.end().map_err(|e| format!("input JSON parse error: {e}"))?;

    let close: &[u8] = if pretty && stats.records > 0 { b"\n]\n" } else { b"]\n" };
    out.write_all(close).map_err(|e| format!("failed to write output: {e}"))?;
    Ok(stats)
}

/// Transform an already-parsed input in one go and write the result.
fn write_whole<W: Write>(engine: &DtoEngine, input: Value, out: &mut W, pretty: bool) -> Result<(), String> {
    let output = engine.transform(&input).map_err(|e| format!("transform failed: {e}"))?;
    drop(input);

    let written = if pretty {
        serde_json::to_writer_pretty(&mut *out, &output)
    } else {
        serde_json::to_writer(&mut *out, &output)
    };
    written
        .map_err(io::Error::from)
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|e| format!("failed to write output: {e}"))
}

/// Skip leading whitespace and peek at the first significant byte.
fn first_non_whitespace<R: BufRead>(reader: &mut R) -> io::Result<Option<u8>> {
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(None);
        }
        match buf.iter().position(|b| !b.is_ascii_whitespace()) {
            Some(i) => {
                let byte = buf[i];
                reader.consume(i);
                return Ok(Some(byte));
            }
            None => {
                let n = buf.len();
                reader.consume(n);
            }
        }
    }
}

/// serde visitor that pulls array elements one at a time, transforming and
/// writing them whenever the batching policy closes a batch.
struct StreamArray<'a, W: Write> {
    engine: &'a DtoEngine,
    out: &'a mut W,
    batching: Batching,
    pretty: bool,
    written: usize,
    /// Statistics for fixed batching (auto batching keeps its own).
    fixed_stats: StreamStats,
}

impl<W: Write> StreamArray<'_, W> {
    fn flush_chunk(&mut self, buf: &mut Vec<Value>) -> Result<(), String> {
        if buf.is_empty() {
            return Ok(());
        }
        let count = buf.len();
        let outputs = self
            .engine
            .transform_items(buf, self.written)
            .map_err(|e| e.to_string())?;
        buf.clear(); // inputs are released before the outputs are written

        match &mut self.batching {
            Batching::Auto(chunker) => {
                chunker.finish_batch(outputs.iter().map(estimate_heap_bytes).sum());
            }
            Batching::Fixed(_) => {
                let s = &mut self.fixed_stats;
                s.min_batch = if s.batches == 0 { count } else { s.min_batch.min(count) };
                s.max_batch = s.max_batch.max(count);
                s.records += count;
                s.batches += 1;
            }
        }

        for item in &outputs {
            write_array_item(self.out, item, self.written == 0, self.pretty)
                .map_err(|e| format!("failed to write output: {e}"))?;
            self.written += 1;
        }
        Ok(())
    }

    fn stats(&self) -> StreamStats {
        match &self.batching {
            Batching::Auto(chunker) => chunker.stats().clone(),
            Batching::Fixed(_) => self.fixed_stats.clone(),
        }
    }
}

impl<'de, W: Write> Visitor<'de> for StreamArray<'_, W> {
    type Value = StreamStats;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON array of records")
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<StreamStats, A::Error> {
        let mut buf = Vec::new();
        while let Some(item) = seq.next_element::<Value>()? {
            let flush = match &mut self.batching {
                Batching::Fixed(n) => {
                    buf.push(item);
                    buf.len() >= *n
                }
                Batching::Auto(chunker) => {
                    let bytes = estimate_heap_bytes(&item);
                    buf.push(item);
                    chunker.push(bytes)
                }
            };
            if flush {
                self.flush_chunk(&mut buf).map_err(A::Error::custom)?;
            }
        }
        self.flush_chunk(&mut buf).map_err(A::Error::custom)?;
        Ok(self.stats())
    }
}

fn write_array_item<W: Write>(out: &mut W, item: &Value, first: bool, pretty: bool) -> io::Result<()> {
    if !first {
        out.write_all(b",")?;
    }
    if pretty {
        let text = serde_json::to_string_pretty(item)?;
        out.write_all(b"\n  ")?;
        out.write_all(text.replace('\n', "\n  ").as_bytes())
    } else {
        serde_json::to_writer(&mut *out, item).map_err(io::Error::from)
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Transform {
            schema,
            input,
            pretty,
            no_validate,
            chunk_size,
            max_memory,
            stats,
        } => {
            run_transform(&schema, input.as_deref(), pretty, no_validate, chunk_size, max_memory, stats)?;
        }

        Commands::Validate { schema } => {
            let schema_content = fs::read_to_string(&schema)
                .map_err(|e| format!("Failed to read schema file {:?}: {e}", schema))?;

            match DtoEngine::from_json(&schema_content) {
                Ok(_) => {
                    println!("Valid DTO schema/recipe ({:?})", schema);
                }
                Err(e) => {
                    eprintln!("Invalid schema/recipe: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Functions => {
            println!("Available built-in transform functions:\n");
            println!("  String transforms:");
            println!("    - trim        : Strip leading and trailing whitespace");
            println!("    - lowercase   : Convert string to lowercase (alias: lower)");
            println!("    - uppercase   : Convert string to uppercase (alias: upper)");
            println!("    - reverse     : Reverse characters in string");
            println!("\n  Type casts:");
            println!("    - to_string   : Cast number, boolean, or object to string (alias: string)");
            println!("    - to_number   : Parse string to number, or bool to 1/0");
            println!("                    (aliases: number, decimal, float, int, integer)");
            println!("    - to_bool     : Cast truthy/falsy value to boolean (aliases: bool, boolean)");
            println!("    - date        : ISO-8601 date/date-time or Unix epoch -> YYYY-MM-DD (errors on invalid dates)");
            println!("    - timestamp   : ISO-8601 date/date-time or Unix epoch -> YYYY-MM-DDTHH:MM:SS[.f][offset]");
            println!("    - json        : Pass the value through unchanged (keeps objects/arrays as-is)");
            println!("\n  Array / Object operations:");
            println!("    - first       : Extract first array element");
            println!("    - last        : Extract last array element");
            println!("    - length      : Get length of array, string, or object keys (alias: len)");
            println!("    - keys        : Extract object keys as string array");
            println!("    - values      : Extract object values as array");
            println!("    - flatten     : Flatten nested arrays by one level");
            println!("    - unique      : Deduplicate array items (alias: uniq)");
            println!("    - sort        : Sort array items in ascending order");
            println!("    - compact     : Remove null elements from array");
        }
    }

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RECIPE: &str = r#"{"name":"T","mappings":[
        {"source":"id","target":"id"},{"source":"name | trim | uppercase","target":"name"}],
        "required":["id"]}"#;

    fn stream(input: &str, chunk: usize, pretty: bool) -> Result<(usize, String), String> {
        let engine = DtoEngine::from_json(RECIPE).unwrap();
        let mut out = Vec::new();
        let stats = transform_streaming(&engine, io::Cursor::new(input.as_bytes()), &mut out, Batching::Fixed(chunk), pretty)?;
        let n = stats.map_or(0, |s| s.records);
        Ok((n, String::from_utf8(out).unwrap()))
    }

    /// Reference output: parse everything, transform in one call.
    fn whole(input: &str, pretty: bool) -> String {
        let engine = DtoEngine::from_json(RECIPE).unwrap();
        let mut out = Vec::new();
        write_whole(&engine, serde_json::from_str(input).unwrap(), &mut out, pretty).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Default CLI mode: buffered bytes, streamed records.
    fn buffered(input: &str, pretty: bool) -> Result<(usize, String), String> {
        let engine = DtoEngine::from_json(RECIPE).unwrap();
        let mut out = Vec::new();
        let n = transform_buffered(&engine, io::Cursor::new(input.as_bytes()), &mut out, pretty)?.map_or(0, |s| s.records);
        Ok((n, String::from_utf8(out).unwrap()))
    }

    #[test]
    fn streaming_matches_whole_input_output() {
        let input = serde_json::to_string(
            &(0..25).map(|i| json!({"id": i, "name": format!(" n{i} ")})).collect::<Vec<_>>(),
        )
        .unwrap();
        for pretty in [false, true] {
            let expected = whole(&input, pretty);
            for chunk in [1, 4, 25, 100] {
                let (n, streamed) = stream(&format!("  \n{input}\n"), chunk, pretty).unwrap();
                assert_eq!(n, 25);
                assert_eq!(streamed, expected, "chunk={chunk} pretty={pretty}");
            }
            let (n, default_mode) = buffered(&format!("\n{input}"), pretty).unwrap();
            assert_eq!(n, 25);
            assert_eq!(default_mode, expected, "default mode, pretty={pretty}");
        }
    }

    #[test]
    fn default_mode_reports_position_and_handles_objects_and_pipelines() {
        let err = buffered(r#"[{"id":1},{"nope":2}]"#, false).unwrap_err();
        assert!(err.contains("item 1: required field 'id'"), "got {err}");
        assert_eq!(buffered(r#"{"id":7,"name":"x"}"#, false).unwrap().1, "{\"id\":7,\"name\":\"X\"}\n");

        // Pipelines are transformed whole (dedup across all records).
        let engine = DtoEngine::from_json(
            r#"{"targets":[{"table":"t","business_key":["k"],"dedup_strategy":"latest",
                "mappings":[{"source":"k","target":"k"},{"source":"v","target":"v"}]}]}"#,
        )
        .unwrap();
        let mut out = Vec::new();
        transform_buffered(&engine, io::Cursor::new(&br#"[{"k":1,"v":1},{"k":1,"v":2}]"#[..]), &mut out, false)
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "{\"t\":[{\"k\":1,\"v\":2}]}\n");
    }

    #[test]
    fn streaming_reports_position_in_full_input() {
        let err = stream(r#"[{"id":1},{"id":2},{"id":3},{"nope":4},{"id":5}]"#, 2, false).unwrap_err();
        assert!(err.contains("item 3: required field 'id'"), "got {err}");
    }

    #[test]
    fn streaming_handles_empty_and_non_array_input() {
        assert_eq!(stream("[]", 10, false).unwrap(), (0, "[]\n".to_string()));
        assert_eq!(stream("[ ]", 10, true).unwrap().1, "[]\n");
        // A single object falls back to a whole-input transform.
        let (_, out) = stream(r#" {"id":7,"name":"x"}"#, 10, false).unwrap();
        assert_eq!(out, "{\"id\":7,\"name\":\"X\"}\n");
        assert!(stream(r#"[{"id":1}] trailing"#, 10, false).is_err());
    }

    #[test]
    fn auto_batching_matches_fixed_output_and_adapts_to_row_width() {
        let auto = |input: &str, budget: usize| {
            let engine = DtoEngine::from_json(RECIPE).unwrap();
            let mut out = Vec::new();
            let stats = transform_streaming(
                &engine,
                io::Cursor::new(input.as_bytes()),
                &mut out,
                Batching::Auto(AutoChunker::new(budget)),
                false,
            )
            .unwrap()
            .unwrap();
            (stats, String::from_utf8(out).unwrap())
        };
        let rows = |pad: usize| {
            serde_json::to_string(
                &(0..2000).map(|i| json!({"id": i, "name": " n ", "pad": "x".repeat(pad)})).collect::<Vec<_>>(),
            )
            .unwrap()
        };

        let narrow_input = rows(8);
        let (narrow, out) = auto(&narrow_input, 256 * 1024);
        assert_eq!(out, whole(&narrow_input, false));
        let (wide, _) = auto(&rows(4000), 256 * 1024);
        assert_eq!((narrow.records, wide.records), (2000, 2000));
        assert!(wide.max_batch < narrow.max_batch, "narrow {narrow:?} wide {wide:?}");

        // Errors still name the position in the whole input.
        let engine = DtoEngine::from_json(RECIPE).unwrap();
        let err = transform_streaming(
            &engine,
            io::Cursor::new(&br#"[{"id":1},{"id":2},{"x":3}]"#[..]),
            &mut Vec::new(),
            Batching::Auto(AutoChunker::new(64 * 1024)),
            false,
        )
        .unwrap_err();
        assert!(err.contains("item 2: required field 'id'"), "got {err}");
    }
}
