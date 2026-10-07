package com.example.app.entity;

import io.quarkus.hibernate.reactive.panache.PanacheEntity;
import io.smallrye.mutiny.Uni;

public class Office extends PanacheEntity {
    public String code;
    public Office parent;

    public static Uni<Office> findByCode(String c) {
        return find("code", c).firstResult();
    }

    public static String normalize(String c) {
        return c.trim();
    }

    public void rename(String c) {
        this.code = c;
    }
}
