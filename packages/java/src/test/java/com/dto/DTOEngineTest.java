package com.dto;

import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.*;

public class DTOEngineTest {

    @Test
    public void testRecipeTransform() {
        try (DTOEngine engine = new DTOEngine()) {
            String recipe = "{"
                + "\"name\": \"UserSummary\","
                + "\"mappings\": ["
                + "  {\"source\": \"user.name\", \"target\": \"name\"},"
                + "  {\"source\": \"user.city\", \"target\": \"city\"}"
                + "]"
                + "}";

            String data = "{\"user\": {\"name\": \"Java Dev\", \"city\": \"Zurich\"}}";

            String result = engine.transform(recipe, data);
            assertNotNull(result);
            assertTrue(result.contains("\"name\":\"Java Dev\""));
            assertTrue(result.contains("\"city\":\"Zurich\""));
        }
    }

    @Test
    public void testBatchTransform() {
        try (DTOEngine engine = new DTOEngine()) {
            String recipe = "{"
                + "\"name\": \"Batch\","
                + "\"mappings\": ["
                + "  {\"source\": \"id\", \"target\": \"code\"}"
                + "]"
                + "}";

            String data = "[{\"id\": \"100\"}, {\"id\": \"200\"}]";

            String result = engine.transform(recipe, data);
            assertNotNull(result);
            assertTrue(result.contains("\"code\":\"100\""));
            assertTrue(result.contains("\"code\":\"200\""));
        }
    }

    @Test
    public void testMedallionPipeline() {
        try (DTOEngine engine = new DTOEngine()) {
            String schema = "{"
                + "\"targets\": ["
                + "  {"
                + "    \"table\": \"silver_part\","
                + "    \"grain\": \"header\","
                + "    \"mappings\": ["
                + "      {\"source\": \"item_id\", \"target\": \"part_number\", \"cast\": \"string\"}"
                + "    ]"
                + "  }"
                + "]"
                + "}";

            String data = "{\"item_id\": \"ITEM-777\"}";

            String result = engine.transform(schema, data);
            assertNotNull(result);
            assertTrue(result.contains("silver_part"));
            assertTrue(result.contains("\"part_number\":\"ITEM-777\""));
        }
    }
}
