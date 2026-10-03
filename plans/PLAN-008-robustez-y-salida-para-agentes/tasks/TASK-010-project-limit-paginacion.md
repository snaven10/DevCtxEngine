# TASK-010 — `project`, `limit`, paginación y conteo de omitidos en las tools

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-004
- **Estado:** `pending`

---

## Objetivo

Que ninguna tool devuelva por defecto miles de tokens que el agente no pidió, que toda tool que
recorta diga cuánto dejó afuera y cómo pedir más, y que las tools de memoria por código acepten
`project` (PLAN-008 P1).

## Contexto verificado

- Structs de parámetros en `crates/devctx-mcp/src/lib.rs`: `PlanStatusReq` (`:192`, tiene `project`,
  `plan`), `MemoriesBySymbolReq` (`:252`, sin `project`), `MemoriesByFileReq` (`:263`, sin
  `project`), `SearchRoutesReq` (`:312`, tiene `project`, sin `limit`), `BuildContextReq` (`:232`,
  lo cubre TASK-013).
- Medido en campo: `search_routes` sin `path` → 50 rutas ~6k tokens; `plan_status` sin filtro en
  revfa → 130 planes ~11k tokens; `memories_by_*` ~2.5k tokens/memoria.
- Recortes existentes que ya informan: `omitted_for_budget` en `do_search` (`state.rs:430-439`),
  `do_references` (`state.rs:3417-3420`), `linked_response` (`state.rs:3355-3357`).
  `routes_to_json` (`state.rs:3449`) no recorta ni informa.
- Rutas HTTP del serve a extender: `crates/devctx-api/src/lib.rs` (cada tool tiene su ruta remota).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/backend.rs`,
  `crates/devctx-mcp/src/state.rs`, `crates/devctx-api/src/lib.rs`

## Pasos

- [ ] **Paso 1 — `memories_by_symbol`/`memories_by_file`.** `project` (resuelto como en las demás
      tools con hint) y `limit` (default 5). Contenido recortado a N caracteres (default 600) con
      `content_truncated: true` y `id` para pedir el completo (`recall`/`memory_context`); parámetro
      `full: bool` para desactivar el recorte.
- [ ] **Paso 2 — `search_routes`.** `limit` (default 20) + `offset`; respuesta `{routes, total,
      next_offset?}`.
- [ ] **Paso 3 — `plan_status`.** `active_only: bool` (planes con tasks no resueltas), `limit`
      (default 25 en listado) + `offset`; `total`, `next_offset?`. Default sin `active_only` para no
      romper el hook de sesión.
- [ ] **Paso 4 — Regla uniforme.** Toda tool que recorta (por límite o por presupuesto) incluye
      `omitted: {count, reason: "limit"|"budget", next_offset?}`. Mantener `omitted_for_budget` como
      alias un release (contrato aditivo).
- [ ] **Paso 5.** Rutas HTTP del serve aceptan los mismos parámetros (query string o body).

## Criterios de aceptación

- [ ] Tests por tool: default respeta el límite; `offset` pagina sin repetir ni saltear; `total` es
      correcto; `omitted.count` = total − devueltos.
- [ ] `memories_by_file` con `project` de otro repo del grupo responde desde ese repo.
- [ ] Medición en el Resultado: tokens (bytes/4) de `search_routes` sin `path` y de `plan_status` sin
      filtro sobre un fixture de 130 planes, antes/después.
- [ ] `plan_status_cli` (presupuesto del hook) sigue verde.

## Riesgos

Más parámetros en el schema de las tools = más tokens de descripción en cada sesión: descripciones
de una línea.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
