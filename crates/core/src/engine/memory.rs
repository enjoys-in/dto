//! Memory estimation and memory-capped ("auto") batching.
//!
//! [`estimate_heap_bytes`] approximates how much memory a parsed
//! [`serde_json::Value`] occupies. [`AutoChunker`] uses it to decide when a
//! batch of records should be transformed so that the records held at once
//! (inputs plus their outputs) stay under a memory budget, whatever the
//! width of the rows.

use serde_json::Value;

/// Size of a `serde_json::Value` slot (the enum itself, stored inline in its parent).
const VALUE_SLOT: usize = std::mem::size_of::<Value>();
/// Per-allocation bookkeeping of the system allocator (approximate).
const ALLOC_OVERHEAD: usize = 16;
/// `serde_json::Map` is a `BTreeMap<String, Value>`; std B-tree nodes hold up to 11 entries.
const BTREE_CAPACITY: usize = 11;
const BTREE_LEAF: usize = 16 + BTREE_CAPACITY * (std::mem::size_of::<String>() + VALUE_SLOT);
const BTREE_INTERNAL: usize = BTREE_LEAF + (BTREE_CAPACITY + 1) * std::mem::size_of::<usize>();
/// Transient per-record bookkeeping while a batch is transformed (result vectors).
const PER_RECORD_TRANSIENT: usize = 128;

fn alloc(bytes: usize) -> usize {
    if bytes == 0 {
        0
    } else {
        bytes.div_ceil(16) * 16 + ALLOC_OVERHEAD
    }
}

/// Approximate number of bytes a parsed JSON value occupies in memory,
/// including its own slot. A 0.3 KB JSON record typically costs several KB
/// once parsed, because every object, array and string is a separate allocation.
pub fn estimate_heap_bytes(value: &Value) -> usize {
    VALUE_SLOT + children_bytes(value)
}

fn children_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        Value::String(s) => alloc(s.len()),
        Value::Array(items) => {
            // Vec growth doubles capacity (minimum 4 elements).
            let capacity = if items.is_empty() { 0 } else { items.len().next_power_of_two().max(4) };
            alloc(capacity * VALUE_SLOT) + items.iter().map(children_bytes).sum::<usize>()
        }
        Value::Object(map) => {
            let n = map.len();
            let nodes = if n == 0 {
                0
            } else if n <= BTREE_CAPACITY {
                alloc(BTREE_LEAF)
            } else {
                // Nodes are on average roughly two-thirds full.
                let leaves = n.div_ceil(8);
                leaves * alloc(BTREE_LEAF) + leaves.div_ceil(6) * alloc(BTREE_INTERNAL)
            };
            nodes
                + map
                    .iter()
                    .map(|(k, v)| alloc(k.len()) + children_bytes(v))
                    .sum::<usize>()
        }
    }
}

/// Plans batches so that the records held at once stay under a memory budget.
///
/// Feed it the estimated size of each record as it is read ([`AutoChunker::push`]);
/// when it returns `true`, transform the pending records, then report the
/// input/output sizes ([`AutoChunker::finish_batch`]) so it learns how large
/// outputs are relative to inputs for this recipe.
#[derive(Debug, Clone)]
pub struct AutoChunker {
    budget: usize,
    pending_bytes: usize,
    pending_records: usize,
    /// Learned output size / input size (starts pessimistic at 1.0).
    output_ratio: f64,
    stats: StreamStats,
}

/// What a streamed, auto-chunked run did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamStats {
    /// Records processed.
    pub records: usize,
    /// Batches transformed.
    pub batches: usize,
    /// Smallest batch (records).
    pub min_batch: usize,
    /// Largest batch (records).
    pub max_batch: usize,
    /// Largest estimated memory of one batch (inputs + outputs), in bytes.
    pub max_batch_bytes: usize,
}

impl AutoChunker {
    /// Create a planner for a memory budget in bytes (minimum 64 KB).
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget: budget_bytes.max(64 * 1024),
            pending_bytes: 0,
            pending_records: 0,
            output_ratio: 1.0,
            stats: StreamStats::default(),
        }
    }

    /// The memory budget in bytes.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Account for one more pending record of `record_bytes` (from
    /// [`estimate_heap_bytes`]). Returns `true` when the batch should be
    /// transformed now. A single record larger than the budget forms a batch
    /// on its own.
    pub fn push(&mut self, record_bytes: usize) -> bool {
        self.pending_bytes += record_bytes + PER_RECORD_TRANSIENT;
        self.pending_records += 1;
        let projected = self.pending_bytes as f64 * (1.0 + self.output_ratio);
        projected >= self.budget as f64
    }

    /// Estimated bytes of the records pushed since the last batch.
    pub fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Record that the pending batch was transformed and produced outputs
    /// totalling `output_bytes` (estimated); resets the pending counters.
    pub fn finish_batch(&mut self, output_bytes: usize) {
        if self.pending_records == 0 {
            return;
        }
        let input = self.pending_bytes.max(1) as f64;
        let observed = output_bytes as f64 / input;
        // Smooth, but react quickly when outputs get relatively larger.
        self.output_ratio = if observed > self.output_ratio {
            observed
        } else {
            (self.output_ratio + observed) / 2.0
        }
        .max(0.01);

        let s = &mut self.stats;
        s.records += self.pending_records;
        s.batches += 1;
        s.min_batch = if s.batches == 1 { self.pending_records } else { s.min_batch.min(self.pending_records) };
        s.max_batch = s.max_batch.max(self.pending_records);
        s.max_batch_bytes = s.max_batch_bytes.max(self.pending_bytes + output_bytes);

        self.pending_bytes = 0;
        self.pending_records = 0;
    }

    /// Statistics so far.
    pub fn stats(&self) -> &StreamStats {
        &self.stats
    }
}

/// Parse a human-friendly size such as `64MB`, `1.5GB`, `512KB`, `256M` or `1048576`
/// (binary units: 1 KB = 1024 bytes).
pub fn parse_size(text: &str) -> Result<usize, String> {
    let t = text.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (number, unit) = t.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| format!("invalid size '{text}' (examples: 64MB, 1GB, 512KB)"))?;
    let multiplier = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        other => return Err(format!("invalid size unit '{other}' in '{text}' (use B, KB, MB or GB)")),
    };
    let bytes = value * multiplier;
    if !bytes.is_finite() || bytes < 1.0 {
        return Err(format!("size '{text}' must be positive"));
    }
    Ok(bytes as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimates_grow_with_width() {
        let narrow = json!({ "id": 1, "name": "a" });
        let wide = json!({ "id": 1, "name": "a", "tags": ["x", "y", "z"], "address": { "city": "Paris", "zip": "75001" },
                           "notes": "a much longer free-text field that takes real space in memory" });
        assert!(estimate_heap_bytes(&narrow) < estimate_heap_bytes(&wide));
        assert_eq!(estimate_heap_bytes(&json!(null)), VALUE_SLOT);
        // A small object costs at least one B-tree node.
        assert!(estimate_heap_bytes(&narrow) > BTREE_LEAF);
    }

    #[test]
    fn chunker_closes_batches_at_the_budget() {
        let mut c = AutoChunker::new(100_000);
        let mut pushed = 0;
        while !c.push(1_000) {
            pushed += 1;
        }
        // With the initial 1.0 output ratio, ~half the budget goes to inputs.
        assert!((40..=50).contains(&(pushed + 1)), "batch of {}", pushed + 1);
        c.finish_batch(c.pending_bytes() / 10); // outputs 10% of inputs

        let mut second = 1;
        while !c.push(1_000) {
            second += 1;
        }
        assert!(second > pushed + 1, "learned ratio should allow a larger batch ({second})");
        c.finish_batch(0);
        assert_eq!(c.stats().batches, 2);
        assert_eq!(c.stats().records, pushed + 1 + second);
    }

    #[test]
    fn oversized_record_is_a_batch_of_one() {
        let mut c = AutoChunker::new(64 * 1024);
        assert!(c.push(10 * 1024 * 1024));
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("64MB").unwrap(), 64 << 20);
        assert_eq!(parse_size("1.5gb").unwrap(), 3 << 29);
        assert_eq!(parse_size("512K").unwrap(), 512 << 10);
        assert_eq!(parse_size("2048").unwrap(), 2048);
        assert!(parse_size("ten MB").is_err());
        assert!(parse_size("5PB").is_err());
        assert!(parse_size("0").is_err());
    }
}
