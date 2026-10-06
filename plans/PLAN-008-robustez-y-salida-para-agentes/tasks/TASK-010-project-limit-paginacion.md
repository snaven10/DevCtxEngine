# TASK-010 — `project`, `limit`, paginación y conteo de omitidos en las tools

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-004
- **Estado:** `done`

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
  acme → 130 planes ~11k tokens; `memories_by_*` ~2.5k tokens/memoria.
- Recortes existentes que ya informan: `omitted_for_budget` en `do_search` (`state.rs:430-439`),
  `do_references` (`state.rs:3417-3420`), `linked_response` (`state.rs:3355-3357`).
  `routes_to_json` (`state.rs:3449`) no recorta ni informa.
- Rutas HTTP del serve a extender: `crates/devctx-api/src/lib.rs` (cada tool tiene su ruta remota).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/backend.rs`,
  `crates/devctx-mcp/src/state.rs`, `crates/devctx-api/src/lib.rs`

## Pasos

- [x] **Paso 1 — `memories_by_symbol`/`memories_by_file`.** `project` (resuelto como en las demás
      tools con hint) y `limit` (default 5). Contenido recortado a N caracteres (default 600) con
      `content_truncated: true` y `id` para pedir el completo (`recall`/`memory_context`); parámetro
      `full: bool` para desactivar el recorte.
- [x] **Paso 2 — `search_routes`.** `limit` (default 20) + `offset`; respuesta `{routes, total,
      next_offset?}`.
- [x] **Paso 3 — `plan_status`.** `active_only: bool` (planes con tasks no resueltas), `limit`
      (default 25 en listado) + `offset`; `total`, `next_offset?`. Default sin `active_only` para no
      romper el hook de sesión.
- [x] **Paso 4 — Regla uniforme.** Toda tool que recorta (por límite o por presupuesto) incluye
      `omitted: {count, reason: "limit"|"budget", next_offset?}`. Mantener `omitted_for_budget` como
      alias un release (contrato aditivo).
- [x] **Paso 5.** Rutas HTTP del serve aceptan los mismos parámetros (query string o body).

## Criterios de aceptación

- [x] Tests por tool: default respeta el límite; `offset` pagina sin repetir ni saltear; `total` es
      correcto; `omitted.count` = total − devueltos.
- [x] `memories_by_file` con `project` de otro repo del grupo responde desde ese repo.
- [x] Medición en el Resultado: tokens (bytes/4) de `search_routes` sin `path` y de `plan_status` sin
      filtro sobre un fixture de 130 planes, antes/después.
- [x] `plan_status_cli` (presupuesto del hook) sigue verde.

## Riesgos

Más parámetros en el schema de las tools = más tokens de descripción en cada sesión: descripciones
de una línea.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes: `search_routes` sin `path` devolvía todas las rutas sin tope ni conteo
   (arreglo pelado salvo fallback/budget); `plan_status` sin filtro, los 130 planes; `memories_by_*`
   10 memorias con contenido completo y sin `project`. Después: cubierto por
   `search_routes_pages_through_the_tool`, `plan_status_pages_and_filters_the_listing`,
   `memories_by_file_takes_project_limit_offset_and_full` (e2e MCP) y los unitarios de §5.
3. **Causa raíz:** no había paginación en el contrato de las tools; cada una decidía su forma de recorte
   (array pelado / objeto con `omitted_for_budget` solo bajo presupuesto). Confirmado §Contexto.
4. **Archivos y símbolos.** `devctx-mcp/src/state.rs`: `Page{limit,offset}` (`new`, `window`),
   `MemoriesOpts{page,full}`, `PlanListOpts{active_only,page}`, `omitted_note`, `set_budget_omitted`,
   `trim_memory_content`, `linked_answer`, `plan_status_page_in`, `DEFAULT_{ROUTES,PLANS,MEMORIES}_LIMIT`,
   `MEMORY_CONTENT_CHARS`; firmas nuevas `do_search_routes(.., page)`, `do_memories_by_symbol/file(.., opts)`,
   `do_plan_status(.., opts)`, `plan_status_budgeted(.., opts)`, `routes_to_json(.., page, default_limit)`.
   `backend.rs` (mismos parámetros; querystring a serves remotos; `ensure_object` envuelve el arreglo de un
   serve < 0.9), `lib.rs` (MCP: `project`/`limit`/`offset`/`full`/`active_only`), `devctx-api/src/lib.rs`
   (rutas HTTP: query `limit`, `offset`, `full`, `active_only`), `devctx-core/src/hits.rs` (contrato +
   `total`/`next_offset`/`omitted_for_budget`), `devctx-store/src/routes.rs` (orden determinista
   `path, method, file, line`), CLI (`render_json`, `omitted_line`, `print_remote_search`, `cmd_routes` remoto,
   `remote.rs`) y `index.html`.
5. **Tests.** Nuevos: unitarios `search_routes_default_page_says_what_it_left_out`,
   `search_routes_pages_without_repeating_or_skipping`, `search_routes_limit_and_an_offset_past_the_end`,
   `routes_for_handler_shape_has_total_and_no_default_cut`, `omitted_adds_limit_and_budget_cuts`,
   `page_window_clamps`, `plan_status_list_is_one_page_of_25_with_a_total`,
   `plan_status_active_only_filters_before_paging`, `linked_memories_default_to_five_trimmed_to_600`,
   `linked_memories_full_keeps_content_and_short_ones_are_not_flagged`, `linked_memories_page_without_repeating`
   (devctx-mcp); `the_unified_object_carries_paging_and_omitted`, `omitted_and_its_legacy_alias_are_both_kept`,
   `old_shapes_still_read` (devctx-core); `json_is_an_object_with_results`,
   `omitted_line_names_count_reason_and_next_page` (cli bin); e2e `search_is_always_an_object_even_without_notes`,
   `search_routes_pages_through_the_tool` (search_shapes_cli), `plan_status_pages_and_filters_the_listing`,
   `memories_by_file_takes_project_limit_offset_and_full` (mcp_tools). Los e2e y unitarios usan parámetros/
   formas que el padre no tiene: fallan allí (no compilan o no cumplen la forma). Actualizados por cambio de
   contrato (no se debilitó ninguna aserción): `routes_to_json_stays_a_bare_array_when_everything_fits` →
   `..._is_an_object_even_when_everything_fits`, `the_indexed_current_branch_adds_no_field` (ya no exige `[`).
   Comandos: `cargo test --workspace` (partido: `--exclude devctx-cli`; `-p devctx-cli --bins`, `--test
   mcp_binding`, y el resto de tests de integración), con `TMPDIR=/var/tmp
   DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache`: todo verde. `plan_status_cli` verde.
   `cargo fmt --check` y `clippy --workspace --all-targets` (con y sin `--features gpu`): 0 warnings.
6. **Contrato JSON** (documentado en `devctx-core/src/hits.rs`). `search`: `{results, omitted?,
   omitted_for_budget?, branch_fallback?}` siempre objeto. `search_routes`: `{routes, total, next_offset?,
   omitted?, omitted_for_budget?, branch_fallback?, warning?}`, página de 20, orden `path, method`.
   `routes_for_handler`: igual, sin paginar. `plan_status` (listado): `{plans, active, total, next_offset?,
   omitted?}`, 25 por página; `offset` cuenta desde el plan más nuevo y dentro de la página van por id;
   `active_only` filtra antes de paginar y `total` cuenta lo filtrado; el detalle (`plan`) no cambia salvo
   `omitted: {count, reason: "budget", blocked, warnings}`. `memories_by_symbol/file`: `{subject, memories,
   total, matched_by, next_offset?, omitted?, ...}`, 5 por defecto, `content` a 600 caracteres con
   `content_truncated: true` y `content_chars` (el `id` queda para pedir el completo), `full: true` lo
   desactiva, `project` resuelto como en las demás tools (`resolved_project`). `omitted = {count, reason:
   "limit"|"budget", next_offset?}` (suma límite + presupuesto; `reason` es `budget` si el presupuesto recortó
   algo); `omitted_for_budget` queda como alias un release. `omitted` también en `impact_analysis`,
   `get_references`, `search_project`, búsqueda de grupo, `recall*` y `memory_context` (solo cuando recortan).
   Lectores: `search_hits` acepta arreglo pelado y objeto (`results`/`routes`/`hits`), y expone `total`,
   `next_offset`, `omitted`, `omitted_for_budget`.
7. **No verificado.** Contra `~/acme` real (TASK-016); un serve < 0.9 con MCP nuevo solo por el envoltorio
   `ensure_object` (sin test e2e); TUI sin cambios de código (ya leía por `search_hits`); `total` de memorias
   cuenta hasta 200 vínculos (tope de escaneo). La búsqueda de grupo y `search_project` siguen usando la clave
   `hits` (no `results`). Docs EN/ES = TASK-015.
8. **Números** (tokens = bytes/4, JSON con sangría; fixture sintético, no acme): `search_routes` sin `path`
   sobre 50 rutas: 3553 → 1449 tokens (20 rutas + `omitted`). `plan_status` sin filtro sobre 130 planes
   (títulos cortos): 3901 → 796 tokens (25 planes); con los títulos reales de acme (~11k medido en campo)
   la proporción es ~80% menos. `memories_by_*`: de ~2.5k tokens/memoria a ≤ ~150 (600 caracteres).
   **Desviaciones:** el cursor es `next_offset` (como pide la task), no `next_cursor`; `search` no pagina (top-k
   por `limit`), solo informa `omitted` por presupuesto; el listado MCP/HTTP de `plan_status` ahora pagina
   (el CLI y el hook, no: `plan_status_value_in` sigue completo).

### Fixup E (review)

- **UNIFY:** `search` en grupo (`do_search_group`) y `search_project` (`do_search_project`) responden `results`
  (antes `hits`); nunca ambas claves. `devctx_core::search_hits` sigue aceptando `hits` (servers viejos); doc del
  contrato en `devctx-core/src/hits.rs` actualizado; consumidor e2e `search_shapes_cli` lee `results` y
  verifica que no exista `hits`.
- **M1:** `fit_plan_status_budget` solo escribe `omitted` si el presupuesto recortó algo, y lo fusiona con el
  `omitted` de página (suma `count`, conserva `next_offset`, `reason: budget`). Test
  `the_plan_budget_pass_keeps_the_limit_note`.
- **M2:** `memories_by_*` agrega `total_capped: true` cuando algún escaneo (junction, fallback local o central)
  llegó al tope de 200. **M3:** un fallback de texto vacío es `matched_by: "text-inference"` (no `junction`)
  — `matched_by_text`. Test `linked_answers_flag_caps_and_name_the_match_honestly`.
- **M9:** la búsqueda y el recall de grupo ya no emiten `omitted_for_budget` por un corte de `limit`; solo
  `omitted{count, reason: "limit"}` (`note_limit_cut`). Test `a_limit_cut_is_not_reported_as_a_budget_cut`.
- **Nits:** el doc de percent-encoding pegado a `query_string` pasó a `urlencode` (documentada) en backend.rs.
