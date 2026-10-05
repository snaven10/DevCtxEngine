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

### Fixup (review)

- **Regresión:** `search` devuelve `{results, branch_fallback}` en toda búsqueda respondida desde la rama por defecto (y, desde TASK-007, rutas devuelven `{routes, warning|branch_fallback}`). Los consumidores internos asumían array: `search_one` (fan-out de grupo) inyectaba el wrapper como un hit sin `file`; `do_search_project` devolvía vacío en silencio; la tabla de `devctx search` imprimía "No results."; la TUI (F1 ruteada) y la UI web (`index.html`) también perdían los hits.
- **Arreglo:** un único lector, `devctx_core::search_hits(&Value) -> SearchHits { hits, branch_fallback, omitted, warning }` (acepta array pelado u objeto con `results`/`routes`/`hits`; un wrapper nunca se vuelve hit). Usado en `search_one`, `do_search_project`, `print_remote_search`, TUI y la UI web. El grupo propaga las notas por miembro en `member_notes`; `search_project` conserva `branch_fallback`/`warning`; la tabla imprime la línea de nota/aviso. Las formas de respuesta no cambian (unificación = TASK-010).
- **Tests:** 7 unitarios del helper (todas las formas); integración `search_shapes_cli.rs`: grupo con un miembro indexado en `main` y checkout en `feat` devuelve hits reales con `file` y reporta su fallback; tabla con serve en fallback imprime filas.

### Fixup (review) — M-9, M-11, I-3

- **M-9:** `search_branch` usaba otra regla que `pick_graph_branch`: sin filas en la rama actual ni en la por defecto buscaba SIN filtro de rama (mezclando ramas) y sin `branch_fallback`, contradiciendo el hint de `index_status`. Ahora `search_branch` reutiliza `pick_graph_branch` (vía `pick_branch`). `branch_fallback` se agregó además a `do_graph` (objeto ya existente, con `warning` si aplica), `memories_by_symbol` (consulta por la rama elegida y agrega el campo al objeto) y `do_build_context` (línea `[devctx] branch_fallback: ...` en la prosa). `devctx routes` local usa `devctx_mcp::state::graph_target` y avisa por stderr. Formas array/objeto sin cambios; `search_hits` sigue leyendo todo.
- **M-11:** `do_search_project` conserva `omitted_for_budget` del hijo (si además recorta, suma los conteos); `do_build_context` ya no pierde `branch_fallback`.
- **I-3:** `graph_branch`, el chequeo de `extractor_stale` y `prune_untracked_branches` (preexistente) usaban `state.root`; ahora usan `AppState::repo_path()` = toplevel de git, igual que pipeline e `index_status`. Tests (devctx-mcp): `a_project_in_a_repo_subdirectory_keys_the_store_by_the_git_toplevel`, `a_symlinked_project_path_keys_the_store_by_the_real_toplevel`, `search_and_the_graph_tools_pick_the_same_branch_and_say_so`.
- **No cubierto con test:** `memories_by_symbol`/`build_context`/`devctx routes` con fallback (tocan central o el embedder); la regla es la misma función.

### Fixup B2 (review)

- **`report_index`:** leía `index_totals` bajo la ruta canónica del registro; el store archiva por toplevel de git, así que en un subdirectorio de monorepo (o con `\\?\` en Windows) `projects list` mostraba 0/0/0. Ahora `totals_for_root` usa `repo_key(root)`; la ruta canónica queda solo como clave del registro. Test: `report_totals_are_read_under_the_git_toplevel`.
- **Latencia:** `AppState` cachea el toplevel (`OnceLock`, un spawn la primera vez) y `repo_branch` lee solo la rama (`GitRepo::branch`, sin el `rev-parse HEAD`). Spawns de git por `pick_branch`: 4 -> 1 (medido con `GIT_TRACE`: 8 `rev-parse` para 2 llamadas antes; 2 después, más el toplevel único). `has_branch_rows` usa `SELECT EXISTS` (antes `count(*)`).
- **Extracciones testeables:** `symbol_branch` (`memories_by_symbol_queries_the_fallback_branch`), `fallback_note`/`close_context` (`build_context_fallback_note`: con 0 resultados el aviso de rama va DESPUÉS de "nothing indexed matched"), `merge_omitted` (conserva `items` del hijo y suma conteos). `memories_by_symbol` omite `branch_fallback` cuando el match es `text-inference` (no depende de la rama).
- **Rama indexada pero vacía:** `branch_fallback.why` dice "is indexed but empty" si hay registro sin filas (`an_indexed_but_empty_current_branch_is_described_as_such`); `index_status` agrega `empty: true` y su hint. El hint de "no indexada" nombra la rama que se usará.
- **`devctx routes` local:** `graph_target` devuelve también `extractor_stale` y la CLI imprime el aviso por stderr.
- Test del symlink reescrito: `project.path` es un symlink al toplevel mismo (sin `sub/`).

### Fixup D2 (review) — B2 y N3

- B2: la pista de "indexed but empty" ya no pisa la de extractor viejo cuando aplican ambas: se concatenan
  (`stale. Also: empty`). Test: `a_stale_and_empty_index_reports_both_hints`.
- N3: en un repo sin ningún índice `index_status` decía "branch X is not indexed; ... answer from branch X" (misma
  rama a ambos lados) con `indexed_branches` vacío. `no_record_hint` separa los casos: sin registros ->
  "nothing indexed yet; run `devctx index`"; filas sin registro completo en la rama actual -> "has rows but no
  completed index record (an interrupted run?)"; fallback real -> el texto de siempre. Test:
  `a_repository_never_indexed_gets_its_own_message`.

### Fixup D2b (review final) — m-3

- `no_record_hint` era inconsistente (filas de un primer índice interrumpido decían "nothing indexed
  yet"; sin filas pero con registros de otras ramas decía "nothing is indexed for this repository").
  Ahora decide por lo que search va a hacer (`chosen`): filas de la rama actual sin registro →
  "has rows but no completed index record" (con o sin otros registros); fallback → nombra la rama;
  sin filas y sin registros → "nothing indexed yet"; sin filas pero con registros de otras ramas → lo
  dice y las nombra. Test: `each_no_record_situation_gets_its_own_message`.
