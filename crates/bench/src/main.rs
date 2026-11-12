//! # dto_bench
//!
//! Benchmarks and synthetic test-data generators for the DTO engine. This
//! crate is not published and is not part of the `dto` CLI, so the shipped
//! binary contains no dummy payloads.
//!
//! ```bash
//! # Engine-only throughput (in-memory, or batched with --chunk-size)
//! cargo run --release -p dto_bench -- engine --count 250000 --threads 6 --chunk-size 5000
//!
//! # Write synthetic input files for end-to-end `dto transform` runs
//! cargo run --release -p dto_bench -- generate --kind narrow --count 300000 -o narrow.json
//!
//! # Compare the memory estimate used by --max-memory with real memory use
//! cargo run --release -p dto_bench -- calibrate
//! ```

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use dto_core::DtoEngine;
use serde_json::{json, Value};

#[derive(Parser, Debug)]
#[command(name = "dto-bench", about = "Benchmarks and synthetic data for the DTO engine")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Benchmark the in-memory transform step with timing, memory, and CPU metrics
    Engine {
        /// Number of records to generate and transform
        #[arg(short, long, default_value_t = 100_000)]
        count: usize,

        /// Path to custom schema/recipe file (uses a built-in recipe if omitted)
        #[arg(short, long)]
        schema: Option<PathBuf>,

        /// Number of benchmark runs to average
        #[arg(long, default_value_t = 3)]
        runs: usize,

        /// Number of warmup runs before timing
        #[arg(long, default_value_t = 1)]
        warmup: usize,

        /// Number of Rayon worker threads (defaults to the logical CPU count)
        #[arg(short, long)]
        threads: Option<usize>,

        /// Records per batch. If omitted, all records are held in memory at once.
        #[arg(long)]
        chunk_size: Option<usize>,

        /// Payload size per record in KB (0.3 = small records; >= 1 uses 6.4 KB enterprise records)
        #[arg(long, default_value_t = 0.3)]
        payload_kb: f64,
    },

    /// Write a JSON array of synthetic records for end-to-end runs
    Generate {
        /// Record shape
        #[arg(long, value_enum)]
        kind: Kind,

        /// Number of records
        #[arg(short, long)]
        count: usize,

        /// Output file
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Compare `estimate_heap_bytes` (used by --max-memory) with measured memory
    Calibrate {
        /// Records per sample
        #[arg(short, long, default_value_t = 50_000)]
        count: usize,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Kind {
    /// ~0.3 KB user-profile records (matches `recipes/narrow.json`)
    Narrow,
    /// ~3.9 KB purchase orders with 8 line items and 30 attributes (matches `recipes/wide.json`)
    Wide,
    /// ~6.4 KB enterprise procurement records (the `engine --payload-kb 6.4` payload)
    Enterprise,
}

fn main() {
    let result = match Cli::parse().command {
        Commands::Engine { count, schema, runs, warmup, threads, chunk_size, payload_kb } => {
            run_benchmark(count, schema, runs, warmup, threads, chunk_size, payload_kb)
        }
        Commands::Generate { kind, count, output } => generate_file(kind, count, &output),
        Commands::Calibrate { count } => {
            calibrate(count);
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

/// Stream `count` synthetic records of `kind` into `output` as one JSON array.
fn generate_file(kind: Kind, count: usize, output: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = BufWriter::with_capacity(1 << 20, fs::File::create(output)?);
    out.write_all(b"[")?;
    for i in 0..count {
        if i > 0 {
            out.write_all(b",")?;
        }
        let record = match kind {
            Kind::Narrow => narrow_record(i),
            Kind::Wide => wide_record(i),
            Kind::Enterprise => generate_enterprise_payload(i),
        };
        serde_json::to_writer(&mut out, &record)?;
    }
    out.write_all(b"]")?;
    out.flush()?;
    let size = fs::metadata(output)?.len();
    eprintln!(
        "wrote {count} {kind:?} records to {} ({:.1} MB, {:.2} KB/record)",
        output.display(),
        size as f64 / 1048576.0,
        size as f64 / count.max(1) as f64 / 1024.0
    );
    Ok(())
}

/// ~0.3 KB record used by the end-to-end runs (`recipes/narrow.json`).
fn narrow_record(i: usize) -> Value {
    json!({
        "id": format!("USR-{i}"),
        "profile": {
            "first_name": "  Alexander  ",
            "last_name": "  Hamilton  ",
            "address": { "city": "New York", "state": "NY", "zip": "10001" }
        },
        "contact": { "emails": [format!("alex_{i}@example.com"), "h@t.gov"], "phone": 18005550199u64 },
        "account": { "score": "98.50", "tier": "platinum" },
        "roles": [{ "name": "finance" }, { "name": "admin" }]
    })
}

/// ~3.9 KB purchase order with 8 line items, 30 attributes and a long note (`recipes/wide.json`).
fn wide_record(i: usize) -> Value {
    let lines: Vec<Value> = (0..8)
        .map(|k| {
            json!({
                "sku": format!("SKU-{k}-{i}"),
                "title": "Industrial Bearing 608-2RS precision component",
                "qty": k + 1,
                "price": 10.5 + k as f64,
                "tax": { "gst": 18, "cess": 0, "amount": 1.89 * (k + 1) as f64 },
                "notes": format!("Handle with care; deliver to dock {k}")
            })
        })
        .collect();
    let mut record = json!({
        "id": format!("PO-{i}"),
        "supplier": {
            "id": format!("SUP-{}", i % 50),
            "name": format!("Supplier Corp {}", i % 50),
            "address": { "line1": "12 Industrial Estate Road", "city": "Detroit", "state": "MI", "zip": "48201" }
        },
        "lines": lines,
        "notes": "x".repeat(1500)
    });
    let obj = record.as_object_mut().expect("record is an object");
    for f in 0..30 {
        obj.insert(format!("attr_{f}"), json!(format!("value-{f}-{i}")));
    }
    record
}

/// Print measured working-set growth next to `estimate_heap_bytes` for each record kind.
fn calibrate(count: usize) {
    let kinds: [(&str, fn(usize) -> Value); 3] = [
        ("narrow (0.3 KB JSON)", narrow_record),
        ("wide (3.9 KB JSON)", wide_record),
        ("enterprise (6.4 KB JSON)", generate_enterprise_payload),
    ];
    for (label, make) in kinds {
        let (before, _) = sys_metrics::get_memory_info();
        let data: Vec<Value> = (0..count).map(make).collect();
        let (after, _) = sys_metrics::get_memory_info();
        let estimated: usize = data.iter().map(dto_core::estimate_heap_bytes).sum();
        let measured = after.saturating_sub(before);
        if measured == 0 {
            println!("{label}: memory measurement not available on this platform; estimated {:.1} MB", estimated as f64 / 1048576.0);
        } else {
            println!(
                "{label}: measured {:.1} MB, estimated {:.1} MB ({:.0}%), ~{} B/record in memory",
                measured as f64 / 1048576.0,
                estimated as f64 / 1048576.0,
                100.0 * estimated as f64 / measured as f64,
                estimated / count.max(1)
            );
        }
        drop(data);
    }
}

fn run_benchmark(
    count: usize,
    schema_path: Option<PathBuf>,
    runs: usize,
    warmup: usize,
    threads: Option<usize>,
    chunk_size: Option<usize>,
    payload_kb: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    // Configure thread pool if requested
    if let Some(t) = threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build_global()
            .ok();
    }

    let num_threads = rayon::current_num_threads();

    println!("================================================================================");
    println!("               DTO ENGINE 100K+ RECORD ARRAY BENCHMARK                         ");
    println!("================================================================================");
    println!(" Hardware / Environment:");
    println!("   • CPU Threads / Workers : {num_threads} parallel Rayon threads");
    println!("   • Target Record Count   : {count} objects");
    println!("   • Payload Size / Record : {:.1} KB (approx)", payload_kb);
    if let Some(cs) = chunk_size {
        println!("   • Execution Strategy    : Micro-Batch Streaming (chunk size: {} items)", cs);
    } else {
        println!("   • Execution Strategy    : In-Memory Batch (All {} records simultaneously)", count);
    }
    println!("   • Benchmark Iterations  : {runs} runs (after {warmup} warmup)");

    let (initial_ws, _) = sys_metrics::get_memory_info();

    // 1. Prepare Recipe/Schema
    let is_enterprise = payload_kb >= 1.0;
    let (schema_json, schema_name) = match schema_path {
        Some(p) => {
            let content = fs::read_to_string(&p)?;
            (content, p.to_string_lossy().to_string())
        }
        None => {
            if is_enterprise {
                (enterprise_bench_recipe(), "Enterprise Procurement Recipe (18 mappings)".to_string())
            } else {
                (default_bench_recipe(), "Built-in Realistic Recipe (9 mappings)".to_string())
            }
        }
    };

    println!("\n Schema / Recipe: {schema_name}");
    let engine = DtoEngine::from_json(&schema_json)?;
    let is_medallion = schema_json.contains("\"targets\"");

    // 2. Prepare Execution (In-Memory Batch vs Streaming)
    let mut durations: Vec<Duration> = Vec::with_capacity(runs);
    let mut cpu_factors: Vec<f64> = Vec::with_capacity(runs);
    let mut peak_memories: Vec<usize> = Vec::with_capacity(runs);

    match chunk_size {
        None => {
            // In-Memory Mode: generate all records into memory at once
            print!("\n ⏳ Generating {count} realistic JSON records in memory... ");
            io::Write::flush(&mut io::stdout())?;
            let gen_start = Instant::now();
            let dataset = generate_benchmark_batch(0, count, is_medallion, payload_kb);
            let gen_duration = gen_start.elapsed();
            let (dataset_ws, _) = sys_metrics::get_memory_info();
            let dataset_mem_mb = (dataset_ws.saturating_sub(initial_ws)) as f64 / (1024.0 * 1024.0);
            println!("Done in {:.2?} (Input RAM: ~{:.2} MB)", gen_duration, dataset_mem_mb);

            // Warmup
            if warmup > 0 {
                print!(" 🔥 Running {warmup} warmup iteration(s)... ");
                io::Write::flush(&mut io::stdout())?;
                for _ in 0..warmup {
                    let _ = engine.transform(&dataset)?;
                }
                println!("Ready.");
            }

            // Benchmark Runs
            println!("\n 📊 Execution Results (In-Memory Mode):");
            println!(" ┌──────┬──────────────┬──────────────┬──────────────┬──────────────┬──────────────┐");
            println!(" │ Run  │ Elapsed Time │  Throughput  │ Avg Latency  │ CPU Utilized │ Peak RSS RAM │");
            println!(" ├──────┼──────────────┼──────────────┼──────────────┼──────────────┼──────────────┤");

            for r in 1..=runs {
                let (cpu_kernel_start, cpu_user_start) = sys_metrics::get_cpu_times();
                let start = Instant::now();

                let output = engine.transform(&dataset)?;

                let elapsed = start.elapsed();
                let (cpu_kernel_end, cpu_user_end) = sys_metrics::get_cpu_times();
                let (_, peak_ws) = sys_metrics::get_memory_info();

                let processed_count = match output {
                    Value::Array(ref arr) => arr.len(),
                    Value::Object(ref map) => {
                        map.values().filter_map(|v| v.as_array().map(|a| a.len())).max().unwrap_or(count)
                    }
                    _ => count,
                };

                let total_cpu_time = (cpu_kernel_end.saturating_sub(cpu_kernel_start))
                    + (cpu_user_end.saturating_sub(cpu_user_start));
                let cpu_factor = if elapsed.as_secs_f64() > 0.0 {
                    total_cpu_time.as_secs_f64() / elapsed.as_secs_f64()
                } else {
                    1.0
                };

                let throughput = (processed_count as f64) / elapsed.as_secs_f64();
                let avg_latency_us = (elapsed.as_nanos() as f64 / processed_count as f64) / 1_000.0;
                let peak_mb = (peak_ws as f64) / (1024.0 * 1024.0);

                println!(
                    " │ #{:<3} │ {:>10.2?} │ {:>8.0} rec/s │ {:>8.2} µs/r │ {:>9.2}x core│ {:>9.2} MB │",
                    r, elapsed, throughput, avg_latency_us, cpu_factor, peak_mb
                );

                durations.push(elapsed);
                cpu_factors.push(cpu_factor);
                peak_memories.push(peak_ws);
            }
            println!(" └──────┴──────────────┴──────────────┴──────────────┴──────────────┴──────────────┘");
        }
        Some(cs) => {
            // Streaming Micro-Batch Mode: process in micro-batches with bounded RAM
            let cs = cs.max(1);
            println!("\n 🌊 Streaming Pipeline Active: micro-batches of {} items processed & released.", cs);

            // Warmup
            if warmup > 0 {
                print!(" 🔥 Running {warmup} warmup iteration(s)... ");
                io::Write::flush(&mut io::stdout())?;
                for _ in 0..warmup {
                    let sample_chunk = generate_benchmark_batch(0, cs.min(count), is_medallion, payload_kb);
                    let _ = engine.transform(&sample_chunk)?;
                }
                println!("Ready.");
            }

            // Benchmark Runs
            println!("\n 📊 Execution Results (Streaming Micro-Batch Mode):");
            println!(" ┌──────┬──────────────┬──────────────┬──────────────┬──────────────┬──────────────┐");
            println!(" │ Run  │ Elapsed Time │  Throughput  │ Avg Latency  │ CPU Utilized │ Peak RSS RAM │");
            println!(" ├──────┼──────────────┼──────────────┼──────────────┼──────────────┼──────────────┤");

            for r in 1..=runs {
                let (cpu_kernel_start, cpu_user_start) = sys_metrics::get_cpu_times();
                let start = Instant::now();

                let mut processed = 0;
                let mut max_run_ws = 0;
                let mut transform_elapsed = Duration::ZERO;

                while processed < count {
                    let batch_size = (count - processed).min(cs);
                    let chunk = generate_benchmark_batch(processed, batch_size, is_medallion, payload_kb);
                    let t_start = Instant::now();
                    let output = engine.transform(&chunk)?;
                    transform_elapsed += t_start.elapsed();
                    let (cur_ws, _) = sys_metrics::get_memory_info();
                    if cur_ws > max_run_ws {
                        max_run_ws = cur_ws;
                    }
                    drop(chunk);
                    drop(output);
                    processed += batch_size;
                }

                let total_elapsed = start.elapsed();
                let (cpu_kernel_end, cpu_user_end) = sys_metrics::get_cpu_times();
                let (_, peak_ws) = sys_metrics::get_memory_info();
                let actual_peak_ws = peak_ws.max(max_run_ws);

                let total_cpu_time = (cpu_kernel_end.saturating_sub(cpu_kernel_start))
                    + (cpu_user_end.saturating_sub(cpu_user_start));
                let cpu_factor = if transform_elapsed.as_secs_f64() > 0.0 {
                    total_cpu_time.as_secs_f64() / total_elapsed.as_secs_f64()
                } else {
                    1.0
                };

                let throughput = (count as f64) / transform_elapsed.as_secs_f64();
                let avg_latency_us = (transform_elapsed.as_nanos() as f64 / count as f64) / 1_000.0;
                let peak_mb = (actual_peak_ws as f64) / (1024.0 * 1024.0);

                println!(
                    " │ #{:<3} │ {:>10.2?} │ {:>8.0} rec/s │ {:>8.2} µs/r │ {:>9.2}x core│ {:>9.2} MB │",
                    r, transform_elapsed, throughput, avg_latency_us, cpu_factor, peak_mb
                );

                durations.push(transform_elapsed);
                cpu_factors.push(cpu_factor);
                peak_memories.push(actual_peak_ws);
            }
            println!(" └──────┴──────────────┴──────────────┴──────────────┴──────────────┴──────────────┘");
        }
    }

    // Summary Statistics
    let total_secs: f64 = durations.iter().map(|d| d.as_secs_f64()).sum();
    let avg_sec = total_secs / runs as f64;
    let min_dur = durations.iter().min().unwrap();
    let max_dur = durations.iter().max().unwrap();
    let avg_throughput = (count as f64) / avg_sec;
    let avg_latency = (avg_sec * 1_000_000.0) / (count as f64);
    let avg_cpu_factor: f64 = cpu_factors.iter().sum::<f64>() / runs as f64;
    let max_peak_mb = (*peak_memories.iter().max().unwrap() as f64) / (1024.0 * 1024.0);

    println!("\n 🏆 Summary for {count} records across {num_threads} threads:");
    println!("   • Fastest Run           : {:.2?}", min_dur);
    println!("   • Slowest Run           : {:.2?}", max_dur);
    println!("   • Average Elapsed Time  : {:.2?}", Duration::from_secs_f64(avg_sec));
    println!("   • Average Throughput    : {:.0} records / second", avg_throughput);
    println!("   • Average Item Latency  : {:.3} microseconds per item", avg_latency);
    println!("   • Parallel CPU Scaling  : {:.2}x logical core utilization", avg_cpu_factor);
    println!("   • Peak Working Set RSS  : {:.2} MB", max_peak_mb);
    println!("================================================================================\n");

    Ok(())
}

fn default_bench_recipe() -> String {
    r#"{
      "name": "BenchmarkDTO",
      "version": "1.0.0",
      "mappings": [
        { "source": "id", "target": "userId" },
        { "source": "profile.first_name | trim | lowercase", "target": "firstName" },
        { "source": "profile.last_name | trim | lowercase", "target": "lastName" },
        { "source": "profile.address.city", "target": "city" },
        { "source": "contact.emails[0]", "target": "primaryEmail" },
        { "source": "contact.phone | to_string | trim", "target": "phone" },
        { "source": "account.score", "target": "scoreNum", "cast": "decimal" },
        { "source": "account.tier", "target": "tierUpper", "transform": "uppercase" },
        { "source": "roles[*].name", "target": "roleNames" }
      ],
      "required": ["userId", "firstName", "primaryEmail"]
    }"#.to_string()
}

fn enterprise_bench_recipe() -> String {
    r#"{
      "name": "EnterpriseProcurementDTO",
      "version": "1.0.0",
      "mappings": [
        { "source": "header.document_id", "target": "documentId" },
        { "source": "header.tenant_id", "target": "tenantId" },
        { "source": "customer.company_name | trim | uppercase", "target": "customerName" },
        { "source": "customer.tax_identifier", "target": "customerTaxId" },
        { "source": "customer.billing_address.city", "target": "billingCity" },
        { "source": "customer.billing_address.state", "target": "billingState" },
        { "source": "supplier.legal_name | trim", "target": "supplierName" },
        { "source": "supplier.vendor_code", "target": "vendorCode" },
        { "source": "supplier.payment_terms", "target": "paymentTerms" },
        { "source": "logistics.tracking_number", "target": "trackingNumber" },
        { "source": "logistics.delivery_hub", "target": "destinationHub" },
        { "source": "financials.subtotal_amount", "target": "subtotal", "cast": "decimal" },
        { "source": "financials.grand_total", "target": "grandTotal", "cast": "decimal" },
        { "source": "financials.cost_center", "target": "costCenter" },
        { "source": "line_items[*].product_code", "target": "productCodes" },
        { "source": "line_items[*].extended_price", "target": "linePrices" },
        { "source": "audit.request_id", "target": "requestId" },
        { "source": "audit.approval_chain[0].approver", "target": "initialApprover" }
      ],
      "required": ["documentId", "tenantId", "customerName", "grandTotal"]
    }"#.to_string()
}

fn generate_benchmark_batch(start_idx: usize, count: usize, is_medallion: bool, payload_kb: f64) -> Value {
    if is_medallion {
        let mut txs = Vec::with_capacity(count);
        for i in start_idx..(start_idx + count) {
            txs.push(json!({
                "purchaseOrderId": format!("PO-2026-{:07}", i / 5),
                "productCartId": format!("LINE-{:07}", i),
                "supplierId": format!("SUP-{:03}", i % 50),
                "supplierName": format!("Supplier Corp {:03}", i % 50),
                "sellerBillingAddressCity": "Detroit",
                "sellerBillingAddressState": "MI",
                "outletId": format!("SITE-{:02}", i % 10),
                "outletCity": "New York",
                "outletState": "NY",
                "customerId": "ACME-CORP",
                "purchaseOrderDateIST": "2026-11-12T14:30:00+05:30",
                "totalAmount": 1250.0,
                "deliveryCharges": 50.0,
                "deliveryChargesTax": 9.0,
                "poDiscountAmount": 25.0,
                "additionalChargesAmount": 15.0,
                "taxAmount": 190.0,
                "subTotalAmount": 1000.0,
                "poDiscountType": "LOYALTY",
                "additionalChargesType": "STANDARD",
                "pickupAddressCity": "Detroit",
                "pickupAddressState": "MI",
                "buyerBillingAddressCity": "New York",
                "buyerBillingAddressState": "NY",
                "productTitle": "Industrial Bearing 608-2RS",
                "quantity": 100.0,
                "price": 10.0,
                "lineSubTotal": 1000.0,
                "lineTaxAmount": 180.0,
                "lineTotalAmount": 1180.0,
                "discountType": "BULK_100",
                "productPackagingType": "BOX",
                "itemDiscount": 20.0,
                "lineGstPercentage": 18.0,
                "productGstPercentage": 18.0,
                "productCess": 0.0,
                "unitPriceWithoutTax": 10.0,
                "unitPriceWithTax": 11.8,
                "isCapex": false,
                "isKitchenItem": false
            }));
        }

        json!({
            "item_id": "PART-889900",
            "revision_number": "R3",
            "reference": {
                "name": "Precision Ball Bearing 608-2RS",
                "description": "High precision deep groove ball bearing",
                "uom": "EA",
                "category": "BEARINGS",
                "hsn": "84821010",
                "brand": "SKF Industrial",
                "price": 12.50,
                "subcategory": "Deep Groove",
                "sku_product_code": "SKU-BRG-608",
                "price_uom": "PER_UNIT",
                "mrp": 18.00,
                "tax": 2.25
            },
            "transactions": txs
        })
    } else if payload_kb >= 1.0 {
        use rayon::prelude::*;
        let items: Vec<Value> = (start_idx..(start_idx + count))
            .into_par_iter()
            .map(generate_enterprise_payload)
            .collect();
        Value::Array(items)
    } else {
        use rayon::prelude::*;
        let items: Vec<Value> = (start_idx..(start_idx + count))
            .into_par_iter()
            .map(|i| {
                json!({
                    "id": format!("USR-{i:07}"),
                    "profile": {
                        "first_name": "  Alexander  ",
                        "last_name": "  Hamilton  ",
                        "address": {
                            "city": "New York",
                            "state": "NY",
                            "zip": "10001"
                        }
                    },
                    "contact": {
                        "emails": [
                            format!("alex_{i}@example.com"),
                            "hamilton@treasury.gov"
                        ],
                        "phone": 18005550199u64
                    },
                    "account": {
                        "score": "98.50",
                        "tier": "platinum",
                        "is_active": true
                    },
                    "roles": [
                        { "name": "finance" },
                        { "name": "government" },
                        { "name": "admin" }
                    ]
                })
            })
            .collect();
        Value::Array(items)
    }
}

fn generate_enterprise_payload(i: usize) -> Value {
    let mut line_items = Vec::with_capacity(4);
    for line in 1..=4 {
        line_items.push(json!({
            "line_number": line,
            "product_code": format!("PROD-COMP-{:04}-PRECISION-SPEC", line * 17),
            "item_description": "High-Precision Silicon Nitride Hybrid Ceramic Sealed Bearing with Reinforced Cage Assembly",
            "commodity_code": "HSN-8482.10.50.90",
            "manufacturer_part_no": format!("MFG-SKF-7008-CE-P4A-DGA-{:03}", line),
            "quantity": 250.0 + (line as f64 * 10.0),
            "unit_of_measure": "PIECE",
            "unit_price": 62.50 + (line as f64 * 1.25),
            "discount_rate": 0.05,
            "net_unit_price": 59.375,
            "extended_price": 14843.75,
            "tax_rate": 0.18,
            "tax_amount": 2671.875,
            "total_price": 17515.625,
            "notes": "Inspection cert EN 10204 3.1 required on dock arrival",
            "dimensions": {
                "length_mm": 68.0,
                "width_mm": 68.0,
                "height_mm": 15.0,
                "weight_grams": 190.0
            },
            "custom_attributes": {
                "tolerance_class": "ABEC-7 / ISO P4",
                "lubricant_spec": "Aeroshell Grease 22 Synthetic",
                "radial_clearance": "C3 Normal Range Clearance",
                "cage_material": "PEEK High-Temp Polymer"
            }
        }));
    }

    json!({
        "header": {
            "document_id": format!("DOC-2026-GLOBAL-{:07}", i),
            "tenant_id": "TENANT-GLOBAL-ENTERPRISE-AEROSPACE-01",
            "source_system": "SAP-S4HANA-ERP-ENTERPRISE-PROD",
            "event_type": "PROCUREMENT_PURCHASE_ORDER_SUBMISSION",
            "status": "APPROVED_FOR_DISPATCH",
            "created_at": "2026-11-12T14:30:00+05:30",
            "updated_at": "2026-11-12T15:45:00+05:30",
            "version": "2.4.1",
            "checksum": "sha256-e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "flags": ["is_production", "priority_high", "audit_required", "auto_reconcile", "export_controlled"]
        },
        "customer": {
            "customer_id": "CUST-99281742-NORTH-AMERICA",
            "company_name": "Acme Industrial Technologies Corporation North America Commercial Division",
            "legal_entity": "Acme Holdings Incorporated Delaware Corporate Entity",
            "tax_identifier": "US-EIN-12-3456789-FED-01",
            "account_manager": "Sarah Jenkins, VP Strategic Accounts and Supply Chain Logistics",
            "contact_email": "purchasing-group-americas@acme-industrial-technologies.com",
            "contact_phone": "+1 (800) 555-0199 ext 4421",
            "billing_address": {
                "line1": "100 Innovation Way, Suite 400, Technology Center Park",
                "line2": "Building B, Financial Operations and Commercial Remittance Department",
                "city": "San Francisco",
                "state": "CA",
                "postal_code": "94105-1234",
                "country": "United States of America"
            },
            "credit_rating": "AAA_PRIME_ENTERPRISE",
            "credit_limit": 5000000.00
        },
        "supplier": {
            "supplier_id": "SUP-88219034-EUROPE-CENTRAL",
            "legal_name": "Precision Global Aerospace & Manufacturing Components GmbH Germany",
            "gstin_tax_no": "DE-VAT-987654321098-AMTSGERICHT",
            "vendor_code": "VEND-NORTH-771-TIER1",
            "remittance_email": "invoices-europe-central@precision-components-manufacturing.de",
            "bank_name": "Deutsche Bank Commercial Operations Frankfurt am Main",
            "iban": "DE89370400440532013000",
            "bic_swift": "DBEUMM21XXX",
            "currency": "USD",
            "payment_terms": "NET_60_DAYS_WITH_2_PERCENT_EARLY_PAYMENT_DISCOUNT"
        },
        "logistics": {
            "shipping_method": "AIR_EXPRESS_PRIORITY_INTERNATIONAL_FREIGHT",
            "carrier_code": "FEDEX_CUSTOM_CRITICAL_AIR",
            "tracking_number": format!("TRK-9902837461928374-{:07}", i),
            "pickup_hub": "Frankfurt Air Logistics Cargo City South Hub 4 Facility",
            "delivery_hub": "San Francisco International Airport Cargo Terminal 7",
            "estimated_dispatch": "2026-11-15T08:00:00Z",
            "estimated_delivery": "2026-11-18T18:00:00Z",
            "special_instructions": "Maintain temperature control 15C-25C; pallet max 1.5m; hazmat cert attached."
        },
        "financials": {
            "subtotal_amount": 185450.75,
            "discount_total": 5500.00,
            "freight_charges": 3200.50,
            "insurance_amount": 850.25,
            "customs_duty_amount": 4120.00,
            "tax_subtotal": 16840.50,
            "roundoff_adjustment": 0.00,
            "grand_total": 204962.00,
            "cost_center": "CC-AERO-ENGINEERING-DIVISION-402",
            "gl_account": "GL-5002910-CAPEX-EQUIPMENT-RESERVE"
        },
        "audit": {
            "source_node": "us-west-prod-api-gateway-instance-07.internal.cloud",
            "request_id": format!("req-20261112-{:08}-1234-5678-90ab-cdef12345678", i),
            "client_ip": "198.51.100.42",
            "user_agent": "ERP-Integration-Daemon/4.1.0 (Linux x86_64; OpenSSL 3.2.0; libcurl 8.5.0)",
            "approval_chain": [
                {"approver": "step1_mgr@acme.com", "role": "DEPT_MANAGER", "status": "APPROVED", "timestamp": "2026-11-12T14:40:00Z"},
                {"approver": "step2_fin@acme.com", "role": "FINANCE_DIRECTOR", "status": "APPROVED", "timestamp": "2026-11-12T15:10:00Z"},
                {"approver": "step3_vp@acme.com", "role": "VP_OPERATIONS", "status": "APPROVED", "timestamp": "2026-11-12T15:40:00Z"}
            ]
        },
        "line_items": line_items
    })
}

#[cfg(windows)]
mod sys_metrics {
    use std::time::Duration;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCounters {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }

    #[repr(C)]
    struct FileTime {
        dw_low_date_time: u32,
        dw_high_date_time: u32,
    }

    #[link(name = "psapi")]
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessMemoryInfo(
            process: isize,
            ppsmps: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
        fn GetProcessTimes(
            h_process: isize,
            lp_creation_time: *mut FileTime,
            lp_exit_time: *mut FileTime,
            lp_kernel_time: *mut FileTime,
            lp_user_time: *mut FileTime,
        ) -> i32;
    }

    pub fn get_memory_info() -> (usize, usize) {
        unsafe {
            let mut pmc = std::mem::zeroed::<ProcessMemoryCounters>();
            pmc.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
            let handle = GetCurrentProcess();
            if GetProcessMemoryInfo(handle, &mut pmc, pmc.cb) != 0 {
                (pmc.WorkingSetSize, pmc.PeakWorkingSetSize)
            } else {
                (0, 0)
            }
        }
    }

    pub fn get_cpu_times() -> (Duration, Duration) {
        unsafe {
            let mut create = std::mem::zeroed();
            let mut exit = std::mem::zeroed();
            let mut kernel = std::mem::zeroed();
            let mut user = std::mem::zeroed();
            if GetProcessTimes(GetCurrentProcess(), &mut create, &mut exit, &mut kernel, &mut user) != 0 {
                let k_100ns = ((kernel.dw_high_date_time as u64) << 32) | (kernel.dw_low_date_time as u64);
                let u_100ns = ((user.dw_high_date_time as u64) << 32) | (user.dw_low_date_time as u64);
                (
                    Duration::from_nanos(k_100ns * 100),
                    Duration::from_nanos(u_100ns * 100),
                )
            } else {
                (Duration::ZERO, Duration::ZERO)
            }
        }
    }
}

/// Non-Windows fallback (previously missing, so the CLI did not compile on
/// Linux/macOS). Memory comes from `/proc/self/status` on Linux; other
/// platforms report zeros. CPU times are not measured here.
#[cfg(not(windows))]
mod sys_metrics {
    use std::time::Duration;

    /// (current RSS, peak RSS) in bytes.
    pub fn get_memory_info() -> (usize, usize) {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let kb = |key: &str| {
            status
                .lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
                * 1024
        };
        (kb("VmRSS:"), kb("VmHWM:"))
    }

    pub fn get_cpu_times() -> (Duration, Duration) {
        (Duration::ZERO, Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enterprise_payload_size() {
        let val = generate_enterprise_payload(0);
        let s = serde_json::to_string(&val).unwrap();
        println!("Enterprise payload size: {} bytes ({:.2} KB)", s.len(), s.len() as f64 / 1024.0);
        // Ensure size is approximately 6.4 KB (+/- 10%)
        let kb = s.len() as f64 / 1024.0;
        assert!(kb >= 5.5 && kb <= 7.0, "Expected ~6.4 KB, got {:.2} KB", kb);
    }

    #[test]
    fn generated_kinds_have_the_documented_sizes() {
        let kb = |v: Value| serde_json::to_string(&v).unwrap().len() as f64 / 1024.0;
        assert!((0.2..0.4).contains(&kb(narrow_record(1))));
        assert!((3.5..4.5).contains(&kb(wide_record(1))));
    }
}
