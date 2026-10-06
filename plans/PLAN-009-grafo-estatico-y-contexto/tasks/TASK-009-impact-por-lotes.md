# TASK-009 — `impact_analysis` por lotes, con tope, confianza y marcas

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-008
- **Estado:** `pending`

---

## Objetivo

Que `impact_analysis` haga una consulta por nivel de profundidad sobre ids (no una por nodo), con
tope de expansión aplicado antes de computar el siguiente nivel, orden por confianza y centralidad,
y que por defecto excluya tests y externos sin esconder que existen. Implementa DD-11.

## Contexto verificado

- `Store::impact_analysis` → `bfs` (`crates/devctx-store/src/graph.rs:335-388`): seeds por
  `resolve_symbol`, cola con `neighbours_of` **por nodo** (l.374-375), sin tope.
- `do_impact` (`crates/devctx-mcp/src/state.rs:4031-4083`): mitad del presupuesto por dirección,
  `fit_json_array` (`state.rs:3572`), `resolved_symbols` vía `merged_declarations` (l.4088-4094),
  `omitted` con `set_budget_omitted`.
- Otros llamadores: CLI `devctx impact` (`crates/devctx-cli/src/main.rs:2264-2265`), TUI
  (`crates/devctx-tui/src/lib.rs:230`), API `GET /impact/:symbol` (`crates/devctx-api/src/lib.rs:62`),
  `Backend::impact` (`crates/devctx-mcp/src/backend.rs:597`), MCP `impact_analysis`
  (`crates/devctx-mcp/src/lib.rs:1170`).
- Medido (modelo): `get` 3.9 s / 1 883 upstream; `map` 5.5 s / 2 569 upstream.

## Archivos

- **Modificar:** `crates/devctx-store/src/graph.rs` (BFS por niveles sobre `edges`; el camino viejo
  queda para ramas v1), `crates/devctx-mcp/src/state.rs` (`do_impact`: parámetros y salida),
  `crates/devctx-mcp/src/lib.rs` (request del tool: `include_tests`, `include_external`,
  `min_confidence`, `max_nodes`), `crates/devctx-mcp/src/backend.rs` y `crates/devctx-api/src/lib.rs`
  (pasar los parámetros), `crates/devctx-cli/src/main.rs` (flags equivalentes), `crates/devctx-tui/src/lib.rs`
  (firma si cambia).

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) un `Store` de test con contador de consultas: `impact` a
      profundidad 3 hace ≤ 2 × 3 + constante consultas, sin importar el tamaño de la frontera;
      (b) con 1 000 callers sintéticos y `max_nodes = 200` → 200 + `omitted.count = 800`; (c) tests
      y externos excluidos por defecto y presentes con `include_*`; (d) orden por `confidence`,
      luego `rank` (NULL hasta TASK-010: cae al nombre), luego nombre.
- [ ] **Paso 2 — BFS por niveles.** Frontera como tabla temporal o lista de ids en `IN (…)` por
      lotes; una consulta por nivel y dirección; tope antes del siguiente nivel.
- [ ] **Paso 3 — salida.** Por nodo: `symbol`, `sym`, `file`, `line`, `depth`, `confidence`, `via`;
      `test`/`external` solo cuando son `true`. `resolved_symbols` y `branch_fallback` como hoy.
- [ ] **Paso 4 — parámetros** en MCP/API/CLI con defaults de DD-11 (Q-2 del master).
- [ ] **Paso 5 — medir** p50/p95 de `get`, `map`, `actualizar`, `OfficeService.actualizar` en
      backend-a (serve caliente) con el arnés.

## Criterios de aceptación

- [ ] Tests (a)-(d) verdes; tests existentes de `impact` (`graph.rs` tests, `state.rs` tests) verdes
      o adaptados solo en lo aditivo.
- [ ] backend-a: `impact("get")` y `impact("map")` p95 ≤ 300 ms.
- [ ] La salida JSON de 0.9.0 sigue siendo un subconjunto de la nueva (campos viejos con el mismo
      significado).

## Riesgos

- Un `IN (…)` con miles de ids puede ser más lento que una tabla temporal: medir ambas en la copia
  de backend-a y elegir con números.
- Cambiar el default (sin tests) cambia lo que ve un agente acostumbrado: queda en las notas del
  release y en la descripción del tool.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
