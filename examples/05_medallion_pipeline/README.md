# Medallion Architecture Pipeline (Bronze → Silver Multi-Table ETL)

This example demonstrates an **Enterprise Medallion Architecture pipeline** converting raw ingested Bronze documents (`stg_upload`) into multiple normalized Silver tables.

## Key Capabilities Demonstrated

1. **`target` (Primary Mapping Syntax)**:
   Every mapping explicitly defines `source` and `target`:
   ```json
   {
     "source": "item_id",
     "target": "part_number",
     "cast": "string",
     "kind": "column"
   }
   ```

2. **Multi-Target Routing**:
   A single incoming bronze item document is automatically routed and fanned out into **6 distinct silver tables**:
   - `silver_part` (Part master header)
   - `silver_purchase_order` (PO headers)
   - `silver_purchase_order_line` (Line items)
   - `silver_supplier` (Supplier master)
   - `silver_company_site` (Site / facility master)
   - `silver_supplier_location` (Supplier location master)

3. **Array Explode (`unpack: "array_explode"`)**:
   Unrolls nested transactions (`transactions[*]`) into individual relational rows while automatically **broadcasting parent document fields** (e.g. `item_id`, `revision_number`, `reference.category`) down into each exploded row.

4. **Deduplication Strategy (`dedup_strategy: "latest"`)**:
   Automatically groups by `business_key` (such as `["supplier_id"]` or `["site_id"]`) and keeps the latest record, eliminating duplicate entries across transactions.

5. **Type Casting (`cast`)**:
   - `cast: "date"`: Formats ISO-8601 timestamps to `YYYY-MM-DD` (`2026-11-12`).
   - `cast: "decimal"`: Casts values to floating-point numbers.
   - `cast: "boolean"`: Normalizes boolean flags.
   - `cast: "json"`: Preserves full nested objects for extension columns (e.g., `flex_attributes`).

---

## Running the Example

Using the CLI:
```bash
dto transform -s schema.json -i input.json --pretty
```

Or using Cargo:
```bash
cargo run --bin dto -- transform -s examples/05_medallion_pipeline/schema.json -i examples/05_medallion_pipeline/input.json --pretty
```
