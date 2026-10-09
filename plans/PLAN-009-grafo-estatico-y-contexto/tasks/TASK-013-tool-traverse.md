# TASK-013 — Tool `traverse`

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-009
- **Estado:** `pending`

---

## Objetivo

Una tool nueva `traverse` para que el agente recorra el grafo por su cuenta (multi-hop): desde un
símbolo, por los tipos de arista que elija (`calls`, `instantiates`, `imports`, `inherits`,
`implements`, `contains`, `references`), en la dirección y profundidad que pida, con límite,
paginación y conteo de omitidos. Inspirada en TraverseGraph de LocAgent. Implementa DD-16.

## Contexto verificado

- La expansión por niveles sobre ids la deja TASK-009 en `crates/devctx-store/src/graph.rs`.
- Resolución de un nombre a candidatos: `Store::resolve_symbol` sobre `symbols` (TASK-008) y
  `split_file_symbol` corregido (TASK-008) para `archivo::símbolo`.
- Contrato de paginación y omitidos de PLAN-008 TASK-010: `devctx_core::hits`, `Page`,
  `set_budget_omitted`, `next_offset` (ver `do_search_routes`, `crates/devctx-mcp/src/state.rs:6133+`).
- Patrón de alta de tool: `state.rs` → `backend.rs` → `devctx-api/src/lib.rs` → `#[tool]` en
  `devctx-mcp/src/lib.rs` → CLI (ver TASK-011).

## Archivos

- **Modificar:** `crates/devctx-store/src/graph.rs` (`traverse(seeds, kinds, direction, depth,
  filtros, limit, offset)` reusando la expansión de TASK-009), `crates/devctx-mcp/src/state.rs`
  (`do_traverse`), `crates/devctx-mcp/src/backend.rs`, `crates/devctx-mcp/src/lib.rs` (`TraverseReq`
  + `#[tool]`), `crates/devctx-api/src/lib.rs` (`POST /traverse`), `crates/devctx-cli/src/main.rs`
  (`devctx traverse <símbolo> [--kinds …] [--dir in|out|both] [--depth N]`).

## Pasos

- [ ] **Paso 1 — tests que fallan.** Grafo de fixture: (a) `direction=in` sobre `calls` profundidad
      2 = callers y callers de callers; (b) `contains` desde una clase = sus métodos; (c)
      `inherits`+`implements` hacia arriba desde una clase = su jerarquía; (d) nombre ambiguo →
      `candidates` y recorrido desde todos, o desde uno con `sym`; (e) `limit`/`offset`/`next_offset`
      y `omitted`; (f) `depth > 4` rechazado con mensaje; (g) rama v1 → error que pide
      `devctx index --full`.
- [ ] **Paso 2 — implementación** sobre la expansión por niveles (una consulta por nivel y
      dirección), filtros `min_confidence`/`include_tests`/`include_external` con los defaults de
      DD-11.
- [ ] **Paso 3 — salida** `{root, candidates?, nodes, edges, total, next_offset?, omitted?,
      branch_fallback?}` con `sym` en hex; presupuesto de tokens del producto
      (`DEVCTX_MAX_OUTPUT_TOKENS`) con `fit_json_array`.
- [ ] **Paso 4 — exposición** MCP/API/CLI y descripción del tool con un ejemplo por tipo de arista.
- [ ] **Paso 5 — medir** p95 de profundidad 1 y 2 en backend-a.

## Criterios de aceptación

- [ ] Tests (a)-(g) verdes.
- [ ] p95 ≤ 200 ms a profundidad 2 en backend-a (serve caliente).
- [ ] La descripción del tool menciona cuándo usar `traverse` vs `impact_analysis` vs
      `get_references`.

## Riesgos

- `references` e `imports` pueden explotar en hubs (un DTO usado en 300 archivos): `limit` y
  `omitted` lo contienen; documentarlo en la descripción.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
