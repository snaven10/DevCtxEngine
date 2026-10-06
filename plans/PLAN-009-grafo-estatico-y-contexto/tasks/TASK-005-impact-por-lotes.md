# TASK-005 — `impact_analysis` por lotes y con tope

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** rust (a definir al detallar)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-003, TASK-004
- **Estado:** `pending`

---

## Objetivo

Reemplazar el BFS de una consulta SQL por nodo (`graph.rs:300`, `neighbours_of`) por una consulta por nivel sobre IDs de símbolo, con tope de expansión y orden por `confidence`; `external`/`test` excluidos por defecto con opción de incluirlos. Meta: `impact_analysis("map")` de ~5.5 s a < 1 s en backend-a.

*Stub de Fase 0: contexto verificado, pasos y criterios de aceptación se escriben al detallar
PLAN-009, después de cerrar PLAN-008.*

## Resultado

<!-- SE LLENA AL CERRAR -->
