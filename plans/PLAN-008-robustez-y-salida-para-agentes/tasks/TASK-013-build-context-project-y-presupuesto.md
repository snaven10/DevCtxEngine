# TASK-013 — `build_context`: `project`, selección en grupo y presupuesto por relevancia

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet; opus si la selección en grupo se complica)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-010, TASK-011, TASK-012
- **Estado:** `done`

---

## Objetivo

Que `build_context` arme contexto del repo correcto y con el código antes que la prosa, sin cortar
en el primer chunk grande ni traer memorias truncadas (PLAN-008 B7).

## Contexto verificado

- `crates/devctx-mcp/src/lib.rs:232-239` — `BuildContextReq { query, max_tokens, include_memories }`,
  sin `project`; tool en `lib.rs:1018`.
- `crates/devctx-mcp/src/state.rs:2972-3093` — `do_build_context`: recall de 5 memorias en todos los
  tiers (`:2996`), `do_search(…, 30, None, SearchMode::Vector, false)` (`:3023`), corte con `break`
  en el primer chunk que no cabe (`:3044`), memorias por archivo de los 5 primeros archivos.
- Truncados: `do_recall_scoped` recorta memorias con "exceeded its share of the output budget"
  (`state.rs:2493`, `:2506`, `:2564`) antes de que `build_context` las vea.
- Grupo: `DevctxServer::backend_for` (`lib.rs:492-512`) devuelve `Binding::Group.default` sin hint.
- Campo: sesión de grupo revfa → tarea de FrontEnd respondida desde `tickets-srv`.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`,
  `crates/devctx-mcp/src/backend.rs`, `crates/devctx-api/src/lib.rs`

## Pasos

- [x] **Paso 1 — `project`.** Parámetro `project` como en las demás tools (acepta un repo distinto del
      bindeado en sesiones de grupo).
- [x] **Paso 2 — Grupo sin `project`.** Correr la búsqueda en cada miembro (reusar la federación de
      `search` en grupo) y elegir el repo con mejor score top-k; nombrarlo en la salida
      (`[devctx] context from <repo> (best match among N members)`). Si el margen entre el primero y
      el segundo es chico, devolver error pidiendo `project` con los candidatos. Nunca default
      silencioso.
- [x] **Paso 3 — Ranking.** Búsqueda `hybrid` con las penalizaciones de TASK-011 y dedup de TASK-012;
      `kind`/`include_tests` pasan derecho.
- [x] **Paso 4 — Presupuesto.** Reemplazar `break` por "saltar y seguir" (un chunk grande no impide
      los siguientes que sí caben); dentro de un chunk de código, recortar comentarios de
      documentación iniciales largos antes que el cuerpo; memorias como título + primeras líneas con
      `id` (sin la marca de truncado por cada una) y una sola línea final con el conteo de omitidos.

## Criterios de aceptación

- [x] Test: grupo de 2 miembros, consulta que solo matchea en el no-default → contexto del correcto,
      nombrado.
- [x] Test: consulta ambigua entre miembros → error que lista candidatos y pide `project`.
- [x] Test: primer hit más grande que el presupuesto → los siguientes que caben aparecen.
- [x] Test: ningún "exceeded its share" en la salida de `build_context`; sí un conteo final.
- [ ] En el Resultado: la consulta de campo (FrontEnd en revfa) antes/después. -> diferido a TASK-016 (medición de campo).

## Riesgos

La selección en grupo cuesta N búsquedas (una por miembro): mismo costo que `search` en grupo hoy;
medirlo y reportarlo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después:** antes, en sesión de grupo `build_context` usaba `Binding::Group.default` (sin `project`), buscaba en `Vector` con `do_search` (que recorta cada fila a 1/30 del presupuesto con "exceeded its share") y cortaba con `break` en el primer chunk que no cabía. Ahora: ver tests; la consulta de campo FrontEnd/revfa queda para TASK-016.
3. **Causa raíz:** confirmada. Además: `do_recall_scoped` y `do_search` presupuestaban por su cuenta (marca "truncated" por ítem) antes de que `build_context` viera los datos; ahora `build_context` toma los datos sin presupuestar (`recall_fused`, `search_items`) y reparte su propio `max_tokens`.
4. **Archivos:** `devctx-mcp/src/{state,lib,backend}.rs`, `devctx-api/src/lib.rs`, `devctx-cli/src/{remote,main}.rs`. Nuevos/cambiados: `do_build_context(state, query, max_tokens, include_memories, &KindSel)`, `Backend::build_context(.., &KindSel)`, `RemoteClient::build_context(.., &KindSel)`, privados `recall_fused`, `search_items`, `group_targets`, `fan_out_search` (extraídos de `do_recall_scoped`/`do_search`/`do_search_group`, sin duplicar), `compose_context`, `memory_brief`, `trim_leading_doc`, `cap_lines`, `member_score`, `choose_member`, pub `pick_group_member(members, query, &KindSel) -> Result<(String, usize), String>`; `DevctxServer::context_backend`.
5. **Tests** (`cargo test -p devctx-mcp --lib -- build_context group_pick`, 6/6 ok): `build_context_skips_a_chunk_that_does_not_fit_and_keeps_going` (en el padre el `break` perdía los chunks siguientes), `build_context_trims_a_long_leading_doc_before_the_body`, `build_context_memories_keep_to_a_share_and_never_stamp_truncation`, `group_pick_chooses_the_member_that_matches`, `group_pick_refuses_a_close_call_and_names_candidates`. En el padre las funciones no existen (no compilan); la conducta que fijan es la descrita en el punto 2. Gate completo: fmt, clippy (también `--features gpu`) y `cargo test --workspace`.
6. **Contrato:** `build_context` (MCP) acepta `project`, `kind`, `include_tests`; `POST /context` acepta `kind`, `include_tests`. Salida prosa: primera línea `[devctx] context from <repo>` (con `(best match among N members)` si eligió por score); cierre `[devctx] omitted: N item(s), reason: budget (T tokens)...` (una sola vez); `[devctx] branch_fallback: ...` se mantiene. Memorias: `[memory] <id> — <título>` + primeras líneas (6 líneas / 500 chars), sin marca de truncado por ítem.
7. **No verificado:** consulta de campo en revfa (TASK-016); el margen de selección `PICK_MARGIN=0.03` sobre media top-3 de coseno es heurístico, sin calibrar en campo; no hay test de integración end-to-end del grupo (la selección se prueba sobre `choose_member`/`member_score`, la fan-out reusa `search_one` ya cubierto).
8. **Parámetros/números:** búsqueda `hybrid` limit 30, rerank off; memorias ≤35% del presupuesto; un chunk ≤ max(presupuesto/3, 600 chars); doc inicial >6 líneas se deja en 3 + "N doc lines trimmed"; selección: top-3 por miembro (vector), margen 0.03. Costo de selección: N búsquedas (una por miembro, lotes de 4) más la de contexto; igual que `search` en grupo; no medido en tiempo.

### Fixup F (review)

- Sin cambios en `build_context`. Efecto indirecto: `search_ranked` ya no multiplica scores por 0.6 sino que
  degrada por posiciones y deja los scores monótonos (`min(propio, anterior)`), así que `member_score` (media
  top-3) deja de castigar ×0.6 al miembro cuyo mejor hit es un doc/test/config; y el pool del reranker se corta a
  `max(pool, 2×limit)` también con filtro duro (TASK-011 Fixup F, I-1). Los tests de `build_context`/`group_pick`
  pasan sin cambios.
