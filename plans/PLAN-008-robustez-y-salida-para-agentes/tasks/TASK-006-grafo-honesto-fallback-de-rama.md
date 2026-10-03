# TASK-006 — Grafo honesto: fallback de rama como `search` y avisos explícitos

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Que `read_symbol`, `get_references`, `impact_analysis`, `search_routes` y `routes_for_handler`
elijan la rama con la misma regla que `search`, y que **digan** qué rama usaron cuando no es la
actual o que no hay índice, en vez de `[]` silencioso (PLAN-008 B4).

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:485-506` — `search_branch`: actual si tiene filas → default si
  tiene filas → sin filtro.
- `state.rs:352` — `repo_branch()`; lo usan a secas `do_impact` (`:2894`), `do_read_symbol`
  (`:3120`), `do_references` (`:3396`), `do_search_routes` (`:3426`), `do_routes_for_handler`
  (`:3440`).
- `state.rs:704-728` — `do_index_status`: `{"indexed": false, "branch"}` sin más.
- Store: `symbol_definitions(repo, branch, …)` (`crates/devctx-store/src/store.rs:658`),
  `resolve_symbol`/`find_references`/`impact_analysis` en `graph.rs`, `search_routes`/
  `routes_for_handler` en `routes.rs` — todas exigen una rama concreta.
- Campo: REVFA_FrontEnd en `feat/plan-124-…` (fuera de `indexing.branches`).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs`

## Pasos

- [x] **Paso 1.** `fn graph_branch(state, store) -> BranchChoice { repo, branch, fallback_from:
      Option<String>, indexed: bool }` con la regla de `search_branch` (sin el caso "sin filtro": el
      grafo necesita una rama; si ninguna tiene filas, `indexed = false`). Usar "la última rama
      indexada" (`index_state.indexed_at` más reciente) cuando la default tampoco tiene filas.
- [x] **Paso 2.** Las cinco tools usan `graph_branch`. Si `fallback_from` es `Some`, agregar
      `"branch_fallback": {"current": X, "used": Y, "why": "X is not indexed"}`. Si `indexed = false`,
      error explícito: `"<repo> has no index for any branch; run devctx index"`.
- [x] **Paso 3.** `search` también reporta `branch_fallback` cuando `search_branch` no usó la actual
      (hoy es silencioso en la dirección contraria).
- [x] **Paso 4.** `do_index_status` con `indexed: false` agrega `indexed_branches: [...]` y un `hint`.

## Criterios de aceptación

- [x] Test unitario/integración: store con filas solo en `main`, checkout en `feat/x` →
      `read_symbol` encuentra la definición y trae `branch_fallback.used = "main"`; ídem
      `get_references`, `search_routes`, `impact_analysis`.
- [x] Store vacío → error explícito, no `[]`.
- [x] Rama actual indexada → salida idéntica a hoy (sin campo nuevo).

## Riesgos

Responder con otra rama puede mostrar código que ya cambió en la actual: por eso el campo es
obligatorio cuando hay fallback.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes: repo indexado en `trunk`, checkout en `feat/not-indexed` ->
   `read_symbol` devolvía `definitions: []` sin aviso. Después (`mcp_binding::graph_tools_fall_back_to_the_indexed_branch_and_say_so`):
   `definitions` con resultado y `branch_fallback {current: "feat/not-indexed", used: "trunk", why}`;
   sin índice alguno: error `<repo> has no index for any branch; run devctx index`. Sin repro manual en REVFA_FrontEnd.
3. **Causa raíz:** confirmada (§2 B4): `search_branch` caía a la rama por defecto/sin filtro; las 5 tools de grafo
   usaban `repo_branch()` a secas.
4. **Archivos y símbolos.** `crates/devctx-mcp/src/state.rs`: `search_branch` ahora devuelve `(SearchFilter,
   Option<BranchFallback>)`; nuevos `BranchFallback`, `BranchChoice { repo, branch, fallback, indexed }`
   (`ready()`, `annotate()`), `pick_graph_branch(store, repo, repo_path, current, default)`, `graph_branch(state, store)`;
   `routes_to_json(routes, &BranchChoice)`. Regla: actual con filas -> `default` con filas -> rama de `index_state`
   más reciente (`indexed_at`) con filas -> `indexed=false` (error). `crates/devctx-store/src/state.rs`:
   `Store::branches_by_recency(repo_path) -> Result<Vec<String>>`. `graph_edges` (tool `get_graph`) y las tools de
   memoria siguen con `repo_branch()` (fuera de alcance).
5. **Tests nuevos.** `devctx-mcp` (unit): `a_branch_nobody_indexed_answers_the_graph_from_the_indexed_one`
   (definición, referencias, rutas e impacto desde `main` con checkout en `feat/x`),
   `the_configured_default_wins_over_the_most_recent`, `the_indexed_current_branch_adds_no_field`,
   `an_empty_store_is_an_explicit_error_not_an_empty_answer`, `the_most_recently_indexed_branch_is_the_last_resort`.
   `devctx-cli` `mcp_binding`: `graph_tools_fall_back_to_the_indexed_branch_and_say_so`. Cambiado
   `a_project_hint_selects_the_member`: ahora indexa `beta` antes de llamar `impact_analysis` (un repo sin índice ya
   es error, no objeto); mismo assert de `resolved_project`. Comandos: `cargo test -p devctx-mcp -p devctx-store`
   (mcp 51+4, store 54, 0 fallos); `cargo test -p devctx-cli --test mcp_binding --test mcp_tools --test plan_status_cli`
   (27 / 8 + 1 ignored previo / 9, 0 fallos); `cargo clippy --all-targets` sin warnings; `cargo fmt --all` aplicado.
   Nota: en una corrida con la suite cargada `scope_defaults_follow_the_binding` falló una vez con `timed out reading
   response` (`remember`); pasó en la re-corrida (flakiness de carga ya conocida, no relacionada).
6. **Contrato JSON.** `branch_fallback: {"current", "used", "why"}` agregado cuando se respondió desde otra rama:
   en `read_symbol`, `get_references`, `impact_analysis` (objeto), `search_routes`, `routes_for_handler` y `search`.
   Forma: `search`/`search_routes`/`routes_for_handler` devuelven hoy un array; con fallback (u omitidos por
   presupuesto) pasan a `{"results"|"routes": [...], "branch_fallback": ..., "omitted_for_budget"?: ...}`; con la
   rama actual indexada y sin recorte el array queda idéntico. Error nuevo de las 5 tools de grafo con repo sin
   índice. `index_status` con `indexed: false` agrega `indexed_branches: [...]` (más reciente primero) y `hint`.
   `search` solo reporta fallback cuando cae a la rama por defecto (no cuando va sin filtro).
7. **No verificado:** REVFA_FrontEnd real; el cambio de forma de `search`/rutas en clientes externos; `graph_edges`.
8. **Números:** sin medición de tiempo; `mcp_binding` completo ~200 s.
