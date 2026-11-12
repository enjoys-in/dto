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
