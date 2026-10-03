# TASK-006 — Grafo honesto: fallback de rama como `search` y avisos explícitos

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

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

- [ ] **Paso 1.** `fn graph_branch(state, store) -> BranchChoice { repo, branch, fallback_from:
      Option<String>, indexed: bool }` con la regla de `search_branch` (sin el caso "sin filtro": el
      grafo necesita una rama; si ninguna tiene filas, `indexed = false`). Usar "la última rama
      indexada" (`index_state.indexed_at` más reciente) cuando la default tampoco tiene filas.
- [ ] **Paso 2.** Las cinco tools usan `graph_branch`. Si `fallback_from` es `Some`, agregar
      `"branch_fallback": {"current": X, "used": Y, "why": "X is not indexed"}`. Si `indexed = false`,
      error explícito: `"<repo> has no index for any branch; run devctx index"`.
- [ ] **Paso 3.** `search` también reporta `branch_fallback` cuando `search_branch` no usó la actual
      (hoy es silencioso en la dirección contraria).
- [ ] **Paso 4.** `do_index_status` con `indexed: false` agrega `indexed_branches: [...]` y un `hint`.

## Criterios de aceptación

- [ ] Test unitario/integración: store con filas solo en `main`, checkout en `feat/x` →
      `read_symbol` encuentra la definición y trae `branch_fallback.used = "main"`; ídem
      `get_references`, `search_routes`, `impact_analysis`.
- [ ] Store vacío → error explícito, no `[]`.
- [ ] Rama actual indexada → salida idéntica a hoy (sin campo nuevo).

## Riesgos

Responder con otra rama puede mostrar código que ya cambió en la actual: por eso el campo es
obligatorio cuando hay fallback.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
