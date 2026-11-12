package dto

import (
	"context"
	"encoding/json"
	"testing"
)

func TestRecipeTransform(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("failed to create engine: %v", err)
	}
	defer engine.Close(ctx)

	recipe := []byte(`{
		"name": "UserSummary",
		"mappings": [
			{ "source": "user.profile.name", "target": "name" },
			{ "source": "user.address.city", "target": "city" }
		],
		"required": ["name"]
	}`)

	data := []byte(`{
		"user": {
			"profile": { "name": "Gopher" },
			"address": { "city": "Mountain View" }
		}
	}`)

	out, err := engine.Transform(recipe, data)
	if err != nil {
		t.Fatalf("transform failed: %v", err)
	}

	var res map[string]any
	if err := json.Unmarshal(out, &res); err != nil {
		t.Fatalf("failed to parse output JSON: %v", err)
	}

	if res["name"] != "Gopher" {
		t.Errorf("expected name 'Gopher', got %v", res["name"])
	}
	if res["city"] != "Mountain View" {
		t.Errorf("expected city 'Mountain View', got %v", res["city"])
	}
}

func TestBatchArrayTransform(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("failed to create engine: %v", err)
	}
	defer engine.Close(ctx)

	recipe := []byte(`{
		"name": "Batch",
		"mappings": [
			{ "source": "id", "target": "userId" },
			{ "source": "score", "target": "score" }
		]
	}`)

	data := []byte(`[
		{"id": "u1", "score": 95},
		{"id": "u2", "score": 88}
	]`)

	out, err := engine.Transform(recipe, data)
	if err != nil {
		t.Fatalf("batch transform failed: %v", err)
	}

	var res []map[string]any
	if err := json.Unmarshal(out, &res); err != nil {
		t.Fatalf("failed to parse batch output: %v", err)
	}

	if len(res) != 2 {
		t.Fatalf("expected 2 items, got %d", len(res))
	}
	if res[0]["userId"] != "u1" || res[1]["userId"] != "u2" {
		t.Errorf("unexpected batch results: %v", res)
	}
}

func TestMedallionPipelineMultiTable(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("failed to create engine: %v", err)
	}
	defer engine.Close(ctx)

	schema := []byte(`{
		"targets": [
			{
				"table": "silver_part",
				"grain": "header",
				"mappings": [
					{ "source": "item_id", "target": "part_number", "cast": "string" },
					{ "source": "price", "target": "standard_cost", "cast": "decimal" }
				]
			},
			{
				"table": "silver_orders",
				"grain": "line",
				"business_key": ["po_id"],
				"dedup_strategy": "latest",
				"mappings": [
					{ "source": "orders[*].id", "target": "po_id", "unpack": "array_explode" },
					{ "source": "item_id", "target": "part_number" },
					{ "source": "orders[*].qty", "target": "quantity", "cast": "decimal", "unpack": "array_explode" }
				]
			}
		]
	}`)

	data := []byte(`{
		"item_id": "PART-100",
		"price": 45.5,
		"orders": [
			{ "id": "PO-1", "qty": 10 },
			{ "id": "PO-2", "qty": 25 }
		]
	}`)

	out, err := engine.Transform(schema, data)
	if err != nil {
		t.Fatalf("pipeline transform failed: %v", err)
	}

	var res map[string][]map[string]any
	if err := json.Unmarshal(out, &res); err != nil {
		t.Fatalf("failed to parse pipeline output: %v", err)
	}

	if len(res["silver_part"]) != 1 {
		t.Fatalf("expected 1 silver_part, got %d", len(res["silver_part"]))
	}
	if res["silver_part"][0]["part_number"] != "PART-100" {
		t.Errorf("expected PART-100, got %v", res["silver_part"][0]["part_number"])
	}

	if len(res["silver_orders"]) != 2 {
		t.Fatalf("expected 2 silver_orders, got %d", len(res["silver_orders"]))
	}
	if res["silver_orders"][0]["part_number"] != "PART-100" {
		t.Errorf("expected broadcasted parent part_number 'PART-100', got %v", res["silver_orders"][0]["part_number"])
	}
}
