# TASK-012 — Ego-graph en `build_context` y evaluación de la selección de grupo

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-009, TASK-010
- **Estado:** `pending`

---

## Objetivo

Que `build_context`, además del código que la búsqueda encontró, traiga lo que lo rodea en el grafo
—callers, callees e implementaciones directas de los mejores hits— como **firmas** y con un tope de
presupuesto, para cubrir el código relacionado que no comparte vocabulario con la pregunta
(RepoGraph: +2-2.7 pts en SWE-bench Lite). Y medir si la selección de miembro de grupo gana algo con
el grafo antes de tocarla. Implementa DD-15 y DD-18.

## Contexto verificado

- `do_build_context` (`crates/devctx-mcp/src/state.rs:4114-4171`): memorias (`recall_fused`, 5) →
  `search_items` híbrido con 30 hits → `compose_context`.
- `compose_context` (`state.rs:4205-4303`, puro y testeable): sección de memorias con tope
  `CTX_MEMORY_SHARE_PCT = 35` (l.4175), sección `## Code` con tope por chunk
  (`CTX_CODE_CAP_DIV`, l.4182), "Recorded against this code" para los primeros 5 archivos, y
  `[devctx] omitted: N item(s)…` (l.4295-4300) + `close_context` (l.5493).
- Request MCP: `BuildContextReq` (`crates/devctx-mcp/src/lib.rs:256`); CLI `devctx context`
  (`--max-tokens`, `--no-memories`).
- Selección de grupo: `pick_group_member_within` (`state.rs:5381-5452`), `score_member`
  (`state.rs:5317`), desempate por `defines` (`/symbol/<name>`, `state.rs:5122-5128`),
  `PICK_IDENT_TOKENS = 2` (l.4526), `PICK_MARGIN = 0.03` (l.4524), `member_scores` sobre
  `raw_score` (l.4557-4572). Calibrado en campo 7/7 (PLAN-008 TASK-016).
- Tests existentes de `compose_context` (p. ej. `build_context_skips_a_chunk_that_does_not_fit_and_keeps_going`,
  `state.rs:7633`) y de selección de grupo (`group_pick_*`, `state.rs:7878+`).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_build_context`: vecinos de los primeros 5
  hits de código por lotes con la expansión de TASK-009; `compose_context`: sección
  `## Related (signatures)`), `crates/devctx-mcp/src/lib.rs` (`BuildContextReq`: `related: bool`,
  `graph_hops: u8`), `crates/devctx-mcp/src/backend.rs` y `crates/devctx-api/src/lib.rs` (pasar los
  parámetros), `crates/devctx-cli/src/main.rs` (`--no-related`, `--hops`).

## Pasos

- [ ] **Paso 1 — tests que fallan** (sobre `compose_context`, puro): (a) la sección de firmas nunca
      supera 20 % del presupuesto; (b) va después de `## Code` y antes de "Recorded against this
      code"; (c) lo que no entra suma al `omitted` existente; (d) nunca un cuerpo, solo firma +
      relación + `archivo:línea`; (e) con `related: false` la salida es idéntica a la de 0.9.0.
- [ ] **Paso 2 — vecinos.** Para los primeros 5 hits de código con símbolo (DD-4): `calls`/
      `instantiates` en ambas direcciones e `inherits`/`implements` hacia arriba, `≥ medium`, sin
      tests ni externos, por `rank`, hasta 4 por hit, sin repetir lo que ya está en `## Code`. Una
      consulta por lote, no por hit.
- [ ] **Paso 3 — parámetros** `related` (default según Q-4) y `graph_hops` (1, máx. 2).
- [ ] **Paso 4 — medir** con el arnés: relevancia del brief (archivo/símbolo esperado presente) con y
      sin `related`, tokens usados, latencia p50. Decidir el default con esos números.
- [ ] **Paso 5 — selección de grupo (DD-18).** Variante experimental: bonus por `rank` del símbolo
      que el miembro define. Correr los 7 casos de PLAN-008 TASK-016 con y sin la variante. Solo se
      adopta si mejora sin romper ninguno; si no, se deja documentado en el Resultado y no se cambia
      la fórmula.

## Criterios de aceptación

- [ ] Tests (a)-(e) verdes; tests existentes de `compose_context` y `group_pick_*` verdes sin
      modificar.
- [ ] Arnés: relevancia de `build_context` ≥ base 0.9.0 + 10 pp (criterio global de §6), o el
      default queda en `related: false` con la tabla que lo justifica.
- [ ] `build_context` p50 ≤ +150 ms sobre la base.
- [ ] Tabla de los 7 casos de grupo con y sin variante en el Resultado.

## Riesgos

- Las firmas desplazan código útil del presupuesto: tope de 20 % y solo después de `## Code`.
- Vecinos de baja calidad en lenguajes poco resueltos (JS sin tipos): el filtro `≥ medium` los deja
  afuera.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
