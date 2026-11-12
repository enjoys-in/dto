import pytest
from dto import DTOEngine


def test_recipe_transform():
    engine = DTOEngine()

    recipe = {
        "name": "UserSummary",
        "mappings": [
            {"source": "user.name", "target": "name"},
            {"source": "user.location.city", "target": "city"},
            {"source": "user.tags[*]", "target": "tags"},
        ],
        "required": ["name"],
    }

    data = {
        "user": {
            "name": "Pythonista",
            "location": {"city": "Amsterdam"},
            "tags": ["python", "rust", "wasm"],
        }
    }

    result = engine.transform(recipe, data)
    assert result["name"] == "Pythonista"
    assert result["city"] == "Amsterdam"
    assert result["tags"] == ["python", "rust", "wasm"]


def test_batch_transform():
    engine = DTOEngine()

    recipe = {
        "name": "Batch",
        "mappings": [
            {"source": "id", "target": "user_id"},
            {"source": "role", "target": "role", "transform": "uppercase"},
        ],
    }

    data = [
        {"id": "p1", "role": "admin"},
        {"id": "p2", "role": "developer"},
    ]

    result = engine.transform(recipe, data)
    assert len(result) == 2
    assert result[0] == {"user_id": "p1", "role": "ADMIN"}
    assert result[1] == {"user_id": "p2", "role": "DEVELOPER"}


def test_medallion_pipeline():
    engine = DTOEngine()

    schema = {
        "targets": [
            {
                "table": "silver_part",
                "grain": "header",
                "mappings": [
                    {"source": "item_id", "target": "part_number", "cast": "string"},
                    {"source": "price", "target": "standard_cost", "cast": "decimal"},
                ],
            },
            {
                "table": "silver_orders",
                "grain": "line",
                "business_key": ["po_id"],
                "dedup_strategy": "latest",
                "mappings": [
                    {"source": "orders[*].id", "target": "po_id", "unpack": "array_explode"},
                    {"source": "item_id", "target": "part_number"},
                    {"source": "orders[*].qty", "target": "quantity", "cast": "decimal", "unpack": "array_explode"},
                ],
            },
        ],
    }

    data = {
        "item_id": "PART-100",
        "price": 45.5,
        "orders": [
            {"id": "PO-1", "qty": 10},
            {"id": "PO-2", "qty": 25},
        ],
    }

    result = engine.transform(schema, data)
    assert len(result["silver_part"]) == 1
    assert result["silver_part"][0]["part_number"] == "PART-100"
    assert len(result["silver_orders"]) == 2
    assert result["silver_orders"][0]["part_number"] == "PART-100"
    assert result["silver_orders"][0]["quantity"] == 10.0
    assert result["silver_orders"][1]["quantity"] == 25.0


def test_compile_once_transform_many():
    engine = DTOEngine()
    with engine.compile({"name": "T", "mappings": [{"source": "user.name", "target": "name"}]}) as compiled:
        assert compiled.transform({"user": {"name": "Ada"}}) == {"name": "Ada"}
        assert compiled.transform([{"user": {"name": "A"}}, {"user": {"name": "B"}}]) == [{"name": "A"}, {"name": "B"}]
        assert compiled.transform('{"user":{"name":"Z"}}', raw_json=True) == '{"name":"Z"}'
    with pytest.raises(RuntimeError, match="has been closed"):
        compiled.transform({})


def test_errors_raise_and_outputs_named_error_do_not():
    engine = DTOEngine()
    # A real output with a single "error" field used to be mistaken for a failure.
    recipe = {"name": "T", "mappings": [{"source": "msg", "target": "error"}]}
    assert engine.transform(recipe, {"msg": "disk full"}) == {"error": "disk full"}

    strict = {"name": "T", "mappings": [{"source": "id", "target": "id"}], "required": ["id"]}
    with pytest.raises(RuntimeError, match="item 1: required field 'id'"):
        engine.transform(strict, [{"id": 1}, {}])
    with pytest.raises(RuntimeError, match="nope"):
        engine.compile({"name": "T", "mappings": [{"source": "a", "target": "b", "cast": "nope"}]})


def test_schema_cache_reuses_and_stays_bounded():
    engine = DTOEngine()
    recipe = {"name": "T", "mappings": [{"source": "a", "target": "b"}]}
    engine.transform(recipe, {"a": 1})
    engine.transform(recipe, {"a": 2})
    assert engine.live_handles == 1
    for i in range(100):
        engine.transform({"name": "T", "mappings": [{"source": "a", "target": f"f{i}"}]}, {"a": i})
    assert engine.live_handles == 64
    with pytest.raises(RuntimeError):
        engine.transform({"name": "T", "mappings": [{"source": "a", "target": "b", "cast": "nope"}]}, {})
    assert engine.live_handles == 64


def test_forgotten_compiled_handles_are_freed_by_gc():
    import gc

    engine = DTOEngine()
    recipe = {"name": "T", "mappings": [{"source": "a", "target": "b"}]}
    engine.compile(recipe).close()
    assert engine.live_handles == 0
    for _ in range(50):
        engine.compile(recipe)  # never closed
    gc.collect()
    assert engine.live_handles == 0


def test_trapped_instance_is_not_reused():
    import wasmtime

    engine = DTOEngine()
    recipe = {"name": "T", "mappings": [{"source": "a", "target": "b"}]}

    def trap(*_args):
        raise wasmtime.Trap("unreachable")

    real = engine._engine_transform
    engine._engine_transform = trap
    with pytest.raises(RuntimeError, match="WebAssembly trap"):
        engine.transform(recipe, {"a": 1})
    engine._engine_transform = real
    with pytest.raises(RuntimeError, match="trapped earlier"):
        engine.transform(recipe, {"a": 1})
