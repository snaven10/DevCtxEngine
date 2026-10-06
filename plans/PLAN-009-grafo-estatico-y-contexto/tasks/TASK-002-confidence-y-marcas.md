# TASK-002 — `confidence` y marcas `external`/`test` por arista

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** rust (a definir al detallar)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Cada arista lleva `confidence` (high: receptor tipado o import resuelto; medium: calificado por convención; low: nombre pelado), `external` cuando no hay definición en el repo y `test` cuando sale de un archivo de test (reusar `path_kind` de PLAN-008 TASK-011). Las cadenas fluent sin receptor tipable se descartan en vez de crear aristas. Meta: bajar el 70 % de aristas a externos y el 57 % desde tests a ruido filtrable.

*Stub de Fase 0: contexto verificado, pasos y criterios de aceptación se escriben al detallar
PLAN-009, después de cerrar PLAN-008.*

## Resultado

<!-- SE LLENA AL CERRAR -->
