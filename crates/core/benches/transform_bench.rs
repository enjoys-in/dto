use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dto_core::DtoEngine;
use serde_json::json;

fn bench_single_object_recipe(c: &mut Criterion) {
    let recipe = r#"{
        "name": "UserSummary",
        "mappings": [
            { "source": "profile.name", "target": "name" },
            { "source": "profile.address.city", "target": "city" },
            { "source": "contact.emails[0]", "target": "primary_email" },
            { "source": "stats.score", "target": "score" }
        ],
        "required": ["name"]
    }"#;

    let engine = DtoEngine::from_json(recipe).unwrap();

    let input = json!({
        "profile": {
            "name": "Alice Developer",
            "address": { "city": "San Francisco" }
        },
        "contact": {
            "emails": ["alice@example.com", "dev@alice.org"]
        },
        "stats": {
            "score": 98.5
        }
    });

    c.bench_function("single_object_recipe_transform", |b| {
        b.iter(|| engine.transform(black_box(&input)).unwrap())
    });
}

fn bench_single_object_json_schema(c: &mut Criterion) {
    let schema = r#"{
        "type": "object",
        "properties": {
            "name": { "type": "string", "x-from": "profile.name" },
            "city": { "type": "string", "x-from": "profile.address.city" },
            "email": { "type": "string", "x-from": "contact.emails[0]" }
        }
    }"#;

    let engine = DtoEngine::from_json(schema).unwrap();

    let input = json!({
        "profile": {
            "name": "Bob Smith",
            "address": { "city": "London" }
        },
        "contact": {
            "emails": ["bob@example.com"]
        }
    });

    c.bench_function("single_object_json_schema_transform", |b| {
        b.iter(|| engine.transform(black_box(&input)).unwrap())
    });
}

fn bench_parallel_batch_1000(c: &mut Criterion) {
    let recipe = r#"{
        "name": "BatchBench",
        "mappings": [
            { "source": "user_id", "target": "id" },
            { "source": "account.email", "target": "email" },
            { "source": "account.role", "target": "role", "transform": "uppercase" }
        ]
    }"#;

    let engine = DtoEngine::from_json(recipe).unwrap();

    let items: Vec<serde_json::Value> = (0..1_000)
        .map(|i| {
            json!({
                "user_id": format!("usr_{i}"),
                "account": {
                    "email": format!("user{i}@corp.net"),
                    "role": "admin"
                }
            })
        })
        .collect();
    let batch_input = serde_json::Value::Array(items);

    c.bench_function("parallel_batch_1000_items", |b| {
        b.iter(|| engine.transform(black_box(&batch_input)).unwrap())
    });
}

criterion_group!(
    benches,
    bench_single_object_recipe,
    bench_single_object_json_schema,
    bench_parallel_batch_1000
);
criterion_main!(benches);
