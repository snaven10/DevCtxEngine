# TASK-001 — Tabla `symbols` con IDs estables y aristas tipadas

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** rust (a definir al detallar)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

---

## Objetivo

Crear la tabla `symbols` (id estable derivado de repo+rama+archivo+nombre calificado+kind, con `parent_id`, firma y rango) y persistir aristas `calls`, `imports`, `inherits` y `contains`, ligando cada arista a la definición destino (`target_symbol_id`, `target_file`) cuando resuelve. Hoy solo hay `calls`, `vectors.symbol` es el nombre pelado y `target_file`/`metadata` quedan vacíos (`graph.rs:82`); los imports se extraen pero solo los usa `chunker.rs:209`. Prever la columna `edge_source = treesitter|scip`.

*Stub de Fase 0: contexto verificado, pasos y criterios de aceptación se escriben al detallar
PLAN-009, después de cerrar PLAN-008.*

## Resultado

<!-- SE LLENA AL CERRAR -->
