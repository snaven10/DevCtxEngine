package com.example.app.resource;

import static org.junit.jupiter.api.Assertions.*;

import java.util.List;
import lombok.Data;

public class Reports {
    @Data
    static class Row {
        private String label;
    }

    void check(Row row, List<Object> all) {
        assertEquals("a", row.getLabel());
        all.forEach(x -> x.toString());
        "a".equals(row.getLabel());
    }
}
