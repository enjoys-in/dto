package dto

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync"
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

func TestCompileOnceTransformMany(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer engine.Close(ctx)

	compiled, err := engine.Compile([]byte(`{"name":"T","mappings":[{"source":"user.name","target":"name"}]}`))
	if err != nil {
		t.Fatalf("Compile: %v", err)
	}
	for _, name := range []string{"Ada", "Linus"} {
		out, err := compiled.Transform([]byte(`{"user":{"name":"` + name + `"}}`))
		if err != nil {
			t.Fatalf("Transform: %v", err)
		}
		if string(out) != `{"name":"`+name+`"}` {
			t.Fatalf("unexpected output %s", out)
		}
	}
	if err := compiled.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := compiled.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if _, err := compiled.Transform([]byte(`{}`)); !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
}

func TestErrorsAndOutputsNamedError(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer engine.Close(ctx)

	// A real output with a single "error" field used to be mistaken for a failure.
	out, err := engine.Transform([]byte(`{"name":"T","mappings":[{"source":"msg","target":"error"}]}`), []byte(`{"msg":"disk full"}`))
	if err != nil || string(out) != `{"error":"disk full"}` {
		t.Fatalf("got %s, %v", out, err)
	}

	_, err = engine.Transform([]byte(`{"name":"T","mappings":[{"source":"id","target":"id"}],"required":["id"]}`), []byte(`[{"id":1},{}]`))
	if err == nil || !strings.Contains(err.Error(), "item 1: required field 'id'") {
		t.Fatalf("expected item 1 error, got %v", err)
	}

	if _, err := engine.Compile([]byte(`{"name":"T","mappings":[{"source":"a","target":"b","cast":"nope"}]}`)); err == nil || !strings.Contains(err.Error(), "nope") {
		t.Fatalf("expected compile error, got %v", err)
	}
}

func TestConcurrentTransformsUseSeparateInstances(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer engine.Close(ctx)

	compiled, err := engine.Compile([]byte(`{"name":"T","mappings":[{"source":"n","target":"n"}]}`))
	if err != nil {
		t.Fatalf("Compile: %v", err)
	}
	defer compiled.Close()

	var wg sync.WaitGroup
	errs := make(chan error, 64)
	for i := 0; i < 32; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			want := fmt.Sprintf(`{"n":%d}`, i)
			out, err := engine.Transform([]byte(`{"name":"T","mappings":[{"source":"n","target":"n"}]}`), []byte(want))
			if err != nil || string(out) != want {
				errs <- fmt.Errorf("Transform %d: %s, %v", i, out, err)
			}
			out, err = compiled.Transform([]byte(want))
			if err != nil || string(out) != want {
				errs <- fmt.Errorf("Compiled %d: %s, %v", i, out, err)
			}
		}(i)
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Error(err)
	}
}

func bigArray(n int) []byte {
	var b strings.Builder
	b.WriteString("[")
	for i := 0; i < n; i++ {
		if i > 0 {
			b.WriteString(",")
		}
		fmt.Fprintf(&b, `{"id":%d,"name":"user-%d","pad":"%s"}`, i, i, strings.Repeat("x", 40))
	}
	b.WriteString("]")
	return []byte(b.String())
}

// Regression: sync.Pool dropped instances on GC while wazero kept them alive,
// leaking ~75 MB per GC cycle for this workload.
func TestPoolDoesNotLeakMemoryAcrossGC(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer engine.Close(ctx)

	recipe := []byte(`{"name":"T","mappings":[{"source":"id","target":"id"},{"source":"name | uppercase","target":"name"}]}`)
	data := bigArray(50000)
	var m runtime.MemStats
	var first uint64
	for i := 0; i < 6; i++ {
		if _, err := engine.Transform(recipe, data); err != nil {
			t.Fatalf("Transform: %v", err)
		}
		runtime.GC()
		runtime.GC()
		runtime.ReadMemStats(&m)
		if i == 0 {
			first = m.HeapInuse
		}
	}
	if grown := int64(m.HeapInuse) - int64(first); grown > 40<<20 {
		t.Fatalf("heap grew by %d MB across GC cycles", grown>>20)
	}
}

func TestSchemaCacheReusesAndBoundsHandles(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer engine.Close(ctx)

	inst, err := engine.acquire()
	if err != nil {
		t.Fatalf("acquire: %v", err)
	}
	defer engine.release(inst)

	schema := func(i int) []byte {
		return []byte(fmt.Sprintf(`{"name":"T","mappings":[{"source":"a","target":"f%d"}]}`, i))
	}
	h1, _ := inst.cachedHandle(ctx, schema(0))
	h2, _ := inst.cachedHandle(ctx, schema(0))
	if h1 != h2 {
		t.Fatalf("cache miss for identical schema: %d vs %d", h1, h2)
	}
	for i := 0; i < schemaCacheSize+20; i++ {
		if _, err := inst.cachedHandle(ctx, schema(i)); err != nil {
			t.Fatalf("cachedHandle: %v", err)
		}
	}
	if n := inst.liveHandles(ctx); n != schemaCacheSize {
		t.Fatalf("expected %d live handles, got %d", schemaCacheSize, n)
	}
	// Failed compiles are not cached and leave nothing behind.
	if _, err := inst.cachedHandle(ctx, []byte(`{"name":"T","mappings":[{"source":"a","target":"b","cast":"nope"}]}`)); err == nil {
		t.Fatal("expected compile error")
	}
	if n := inst.liveHandles(ctx); n != schemaCacheSize {
		t.Fatalf("failed compile leaked a handle: %d", n)
	}
}

func TestBrokenInstancesAreReplaced(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer engine.Close(ctx)

	inst, _ := engine.acquire()
	inst.broken = true
	engine.release(inst)
	next, _ := engine.acquire()
	defer engine.release(next)
	if next == inst {
		t.Fatal("a broken instance was handed out again")
	}
	if len(engine.slots) != 1 {
		t.Fatalf("expected 1 live instance, got %d", len(engine.slots))
	}
}

func TestUseAfterEngineClose(t *testing.T) {
	ctx := context.Background()
	engine, err := New(ctx)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	recipe := []byte(`{"name":"T","mappings":[{"source":"a","target":"b"}]}`)
	compiled, err := engine.Compile(recipe)
	if err != nil {
		t.Fatalf("Compile: %v", err)
	}
	if err := engine.Close(ctx); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if _, err := engine.Transform(recipe, []byte(`{}`)); !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
	if _, err := compiled.Transform([]byte(`{}`)); !errors.Is(err, ErrClosed) {
		t.Fatalf("expected ErrClosed, got %v", err)
	}
	_ = compiled.Close()
}
