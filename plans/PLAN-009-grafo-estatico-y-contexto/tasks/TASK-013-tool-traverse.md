# TASK-013 — Tool `traverse`

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-009
- **Estado:** `done`

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

- [x] **Paso 1 — tests que fallan.** Grafo de fixture: (a) `direction=in` sobre `calls` profundidad
      2 = callers y callers de callers; (b) `contains` desde una clase = sus métodos; (c)
      `inherits`+`implements` hacia arriba desde una clase = su jerarquía; (d) nombre ambiguo →
      `candidates` y recorrido desde todos, o desde uno con `sym`; (e) `limit`/`offset`/`next_offset`
      y `omitted`; (f) `depth > 4` rechazado con mensaje; (g) rama v1 → error que pide
      `devctx index --full`.
- [x] **Paso 2 — implementación** sobre la expansión por niveles (una consulta por nivel y
      dirección), filtros `min_confidence`/`include_tests`/`include_external` con los defaults de
      DD-11.
- [x] **Paso 3 — salida** `{root, candidates?, nodes, edges, total, next_offset?, omitted?,
      branch_fallback?}` con `sym` en hex; presupuesto de tokens del producto
      (`DEVCTX_MAX_OUTPUT_TOKENS`) con `fit_json_array`.
- [x] **Paso 4 — exposición** MCP/API/CLI y descripción del tool con un ejemplo por tipo de arista.
- [x] **Paso 5 — medir** p95 de profundidad 1 y 2 en backend-a.

## Criterios de aceptación

- [x] Tests (a)-(g) verdes.
- [x] p95 ≤ 200 ms a profundidad 2 en backend-a (serve caliente).
- [x] La descripción del tool menciona cuándo usar `traverse` vs `impact_analysis` vs
      `get_references`.

## Riesgos

- `references` e `imports` pueden explotar en hubs (un DTO usado en 300 archivos): `limit` y
  `omitted` lo contienen; documentarlo en la descripción.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. Tests (a)-(g) verdes; p95 a profundidad 2 en backend-a ≤ 47,5 ms
  (presupuesto 200); la descripción dice cuándo usar `traverse`, `impact_analysis` o
  `get_references`, con un ejemplo por relación. Antes, los pendientes de la revisión de TASK-017.
- **Resumen:** tool nueva `traverse` (MCP, `POST /traverse`, `devctx traverse`, `Backend` local y
  remoto): desde un símbolo (pelado, calificado, `archivo::nombre`, la ruta de un archivo o un `sym`
  hex) por las relaciones pedidas (`kinds`: `calls`, `instantiates`, `imports`, `inherits`,
  `implements`, `contains`, `references`, o `all`; default `calls`), `in`/`out`/`both` (default
  `out`), profundidad 1-4 (más, o 0, se rechaza con un mensaje antes de abrir nada). Es la expansión
  por niveles de TASK-009 (`edge_level`: una sentencia por nivel y dirección, `FrontierSql::Auto`,
  `live_edges`), con los filtros de DD-11 (`min_confidence` `medium`, sin tests ni externos; lo
  dejado afuera se cuenta y no se recorre) y, si sigue `calls`, el dispatch de TASK-017
  (`DispatchIndex`). Salida `{root, candidates?, nodes, edges, total, next_offset?, omitted?,
  partial?, filters, below_confidence?, excluded?, omitted_by_limit?, dispatch?,
  omitted_for_budget?, branch_fallback?}` con `sym` en hex; detalle en DD-16 ("Implementado en
  TASK-013").
- **Commits** (sobre `123dd9d`):
  1. `713bcbc` feat(impact): nota `dispatch: {evaluated: false, reason}` (pedido de la revisora).
  2. `d563b50` test(impact): diamante, ciclo, TS con varios `implements`, blanket impl de Rust y los
     asserts BFS de 0.9.0 con el índice de supertipos leído.
  3. `7cb0bfa` fix(impact): aridad ≤ en TS/JS.
  4. `4c473b4` feat(impact): lo que corta el tope de profundidad 4 se cuenta.
  5. `1e46c9d` test(index): escenario (e) por `impact_graph`.
  6. `eade2a1` feat(store): `Store::traverse`.
  7. `488328e` feat(traverse): tool, API, CLI, backends.
  8. `b1b6a7b` fix(traverse): la regla de `impact` sobre las raíces, y el arnés `traverse_bench`.
  9. docs EN/ES, CHANGELOG, design y este Resultado.
- **Pendientes de TASK-017, cerrados:**
  - *Nota de dispatch no evaluado:* `dispatch: {"evaluated": false, "reason": "dispatch:false" |
    "min_confidence high"}` en `impact_analysis` (y en `traverse` con `calls`), de los filtros solos
    (nada se lee). Test `an_answer_says_when_dispatch_was_not_evaluated` (**fallaba antes**: sin
    campo).
  - *(4) Aridad TS/JS:* con un archivo `.ts/.tsx/.mts/.cts/.js/.jsx/.mjs/.cjs`, el método del subtipo
    solo necesita no tener más obligatorios que los que acepta el del supertipo (`arities_fit`);
    Java, Rust y Python siguen exigiendo que los rangos se crucen. Test
    `a_typescript_implementation_with_fewer_parameters_dispatches` (**fallaba antes**: sin el
    llamador por dispatch) y el unitario `a_typescript_implementation_may_take_fewer_parameters`.
  - *(6) Tope de profundidad:* `DispatchIndex::reach` sigue la jerarquía más allá de
    `DISPATCH_DEPTH` solo para marcar esos tipos; los candidatos override-equivalentes que caen ahí
    (sin tests si se excluyen) se cuentan en `ImpactSide::dispatch_beyond_depth` →
    `omitted_by_limit.dispatch.{count, beyond_depth, max_depth}`, nunca en `omitted`. Sin sentencias
    nuevas (los candidatos ya se leían). Un método más allá del tope que otro nodo alcanza dentro
    del suyo se lista y no se cuenta. Tests `the_dispatch_depth_cap_is_counted` (store) y
    `the_dispatch_depth_cap_is_in_omitted_by_limit` (JSON); **por mutación** (no contar): falla.
  - *(8) Escenario (e):* `dispatch_follows_an_incremental_implements_change` (devctx-index): antes
    del cambio el llamador por `IFirst`, después por `ISecond`, e igual que un índice completo del
    mismo árbol. **No falló antes**: no hay fix, verifica que el dispatch se lee en la consulta.
  - *NITs:* `a_diamond_of_supertypes_lists_each_method_once` (gana la cadena más segura: por
    mutación, quedarse con la primera cadena hace fallar el test), `a_cycle_of_supertypes_ends_and_
    lists_each_method_once`, `dispatch_follows_every_interface_a_typescript_class_implements`,
    `dispatch_follows_a_rust_blanket_impl` (el método del impl genérico se llama `T.write` y despacha
    en los dos sentidos) y `the_0_9_0_bfs_asserts_hold_with_a_supertype_graph`. Pasaron al
    escribirlos: documentan comportamiento que ya era correcto.
- **Números** (sandbox `mktemp -d -p /var/tmp`; copia de backend-a con `tar` sin `.devctx`/`target`/
  `node_modules`, `find` vacío antes del índice; indexada con el embedder sin modelo del arnés
  `traverse_bench` en 102 s; binario de test `--release` del árbol de `b1b6a7b`; 40 corridas por caso
  y profundidad, 2 rondas, cada corrida recorre todos los casos (intercalado); cada llamada abre el
  store como el serve; carga 4,4-11,9). 16 casos: 8 de `calls` `in` (los nombres de TASK-017),
  `calls` `out`, `references` `in` de los dos tipos más referenciados, `imports` `in`, `contains`
  `out` de la clase más grande (331 miembros), `implements,inherits` `in` de la interfaz más
  implementada, `instantiates` `in` y `all` `both` del tipo más referenciado. p50 / p95 en ms, peor
  de las dos rondas:

  | Caso | prof. 1 | prof. 2 | total (página 50) |
  |---|---|---|---|
  | `calls` `in`, 8 casos | 15,4-38,4 / 17,7-44,3 | 15,6-41,7 / 18,9-47,5 | 0-107 |
  | `calls` `out` | 16,6 / 18,3 | 16,5 / 19,6 | 0 |
  | `references` `in`, tipo más referenciado | 15,7 / 18,7 | 16,0 / 18,9 (`partial`) | 105 |
  | `references` `in`, el segundo | 14,3 / 16,7 | 20,2 / 23,5 | 37 |
  | `imports` `in` | 14,3 / 16,5 | 18,7 / 22,1 | 28 |
  | `contains` `out`, clase de 331 miembros | 14,6 / 17,1 | 14,7 / 16,8 (`partial`) | 331 |
  | `implements,inherits` `in` | 13,4 / 14,6 | 26,4 / 31,5 | 7 |
  | `instantiates` `in` | 20,3 / 23,3 | 23,0 / 26,2 | 1 |
  | `all` `both`, tipo más referenciado | 27,8 / 32,7 | 27,7 / 31,4 (`partial`) | 177 |

  **Criterio: cumplido** (peor p95 a profundidad 2: 47,5 ms ≤ 200). El costo crece con la cantidad
  de raíces (los casos más caros son nombres pelados con muchas definiciones) más que con la
  profundidad, porque una página llena no lee niveles más profundos. Una primera corrida con el
  árbol de `488328e` (antes de la regla de raíces) dio lo mismo dentro del ruido (peor p95 49,5).
  **Hub sintético** (300 clases que usan e instancian un DTO, 301 archivos): `references` `in`
  11,3 / 13,6 (prof. 1) y 11,6 / 14,3 (prof. 2, `partial`); `all` `in` 15,0 / 17,6 y 14,7 / 17,2;
  `imports,references` `both` 13,9 / 15,8 y 13,4 / 15,5; cada respuesta 50 nodos y 50 aristas,
  `total` 600 (901 con `all`) y `omitted` con el resto. El test `a_hub_is_paged_and_counted` fija el
  contrato (300 archivos: 50 + `next_offset` 50 + `omitted` 250; la última página sin
  `next_offset`; prof. 2 → `partial`).
- **Hallazgos de §2:** sin cambios. Dos de los ocho nombres de `calls` `in` dan 0 nodos en
  backend-a, verificado en el índice: uno no tiene llamadas resueltas; el otro (21 definiciones)
  solo las tiene desde tests o desde otras definiciones del mismo nombre (sobrecargas que delegan).
  Esto llevó a aplicar la regla de `impact` sobre las raíces (antes de este commit ninguna raíz se
  listaba): de las definiciones de un nombre pelado, una alcanzada desde otra se lista (ese caso da
  ahora 10 nodos). Las llamadas al nombre **sin decidir** (`undecided_calls` de
  impact) no entran a `traverse`, que sigue aristas por id.
- **Archivos tocados:** `crates/devctx-store/src/{dispatch.rs, impact.rs, traverse.rs (nuevo),
  lib.rs}`, `crates/devctx-mcp/src/{state.rs, state/impact.rs, state/traverse.rs (nuevo), lib.rs,
  backend.rs}`, `crates/devctx-api/src/lib.rs`, `crates/devctx-cli/src/{main.rs, remote.rs,
  help_map.rs}`, `crates/devctx-cli/tests/mcp_tools.rs` (un test nuevo, sin tocar asserts),
  `crates/devctx-index/src/lib.rs` (test), `CHANGELOG.md`, docs EN/ES (`symbol-graph`/
  `grafo-de-simbolos`, `mcp-integration`/`integracion-mcp`), el design (DD-11, DD-16) y el master.
  Públicos nuevos: `devctx_store::{Traversal, TraverseDirection, TraverseEdge, TraverseEnd,
  TraverseNode, TraverseOptions, TRAVERSE_KINDS, TRAVERSE_MAX_DEPTH}`, `Store::{traverse,
  traverse_roots}`, `ImpactSide::dispatch_beyond_depth`; `devctx_mcp::state::{TraverseQuery,
  DEFAULT_TRAVERSE_LIMIT, TRAVERSE_NEEDS_REINDEX, do_traverse, traverse_at}`,
  `devctx_mcp::backend::{Backend::traverse, TRAVERSE_OLD_SERVER, traverse_unsupported}`. Internos:
  `Store::edge_level` (la consulta de nivel con las relaciones como parámetro; `impact_level` la
  llama con `calls`/`instantiates`), `dispatch_level(…, kinds)`, `LevelRow::{edge_line,
  node_kind}`, `Level::beyond`, `Equivalent::kind`. Sin cambios de schema, config ni
  `languages/*.json`.
- **Tests** (`TMPDIR=<sandbox> cargo test --workspace --locked`; gate completo antes de cada commit
  de código: `fmt --check`, `clippy -D warnings` con y sin `--features gpu`): 1 105, 1 110, 1 112,
  1 114, 1 115, 1 124, 1 135 y 1 136 pasan, 0 fallan (una corrida fallida por clippy antes del commit
  del store, `cloned_ref_to_slice_refs` en un test, corregida); ningún flake en esta task. Nuevos de `traverse`:
  store `callers_and_their_callers` (a), `a_class_contains_its_members` (b),
  `a_class_climbs_its_hierarchy` (c), `externals_and_undecided_are_counted_and_discarded_never_seen`
  (descartadas fuera: **por mutación**, `edges` en vez de `live_edges` hace fallar el test),
  `both_directions_and_several_relations`, `one_statement_per_level_and_direction`,
  `a_full_page_reads_no_deeper_level` (hub de 300 en el store: un nivel leído, una sentencia),
  `calls_follow_dispatch` (`via: dispatch` en nodo y arista; apagado, `high` u otra relación no leen
  el grafo de supertipos), `a_path_is_its_file_symbol`,
  `a_root_reached_from_another_is_listed_for_a_bare_name` (la regla de `impact` sobre las raíces;
  **por mutación**: no listar ninguna raíz lo hace fallar); tool `callers_and_callers_of_callers` (a,
  con dispatch y su `via`), `an_example_per_relation` (b, c y los ejemplos de la descripción),
  `an_ambiguous_name_lists_candidates_and_sym_picks_one` (d), `a_hub_is_paged_and_counted` (e),
  `bad_parameters_are_refused_with_a_message` (f), `an_old_index_asks_for_a_full_reindex` (g, rama
  de otro extractor y rama sin filas), `dispatch_not_evaluated_is_said_on_calls`,
  `the_budget_names_what_it_drops`; `traverse_on_an_old_server_says_to_restart_it` (backend
  remoto contra un 404), `traverse_takes_its_parameters_from_the_body` (API),
  `traverse_refuses_a_depth_over_four` (MCP por stdio). Los de `traverse` se escribieron contra la
  spec antes del cableado (no compilaban); las reglas se verificaron por mutación donde había una
  alternativa plausible.
- **Contrato (lección 2):** ningún assert existente cambió; los tests de impact, `get_references` y
  `mcp_tools` pasan sin tocarlos (al de `mcp_tools` solo se le agregó un test).
- **Contrato JSON** (aditivo): `impact_analysis` agrega `dispatch: {evaluated: false, reason}` y
  `omitted_by_limit.dispatch.{beyond_depth, max_depth}` (y esos métodos dentro de su `count`); la
  tool `traverse` es nueva.
- **Desviaciones:**
  1. El parámetro se llama `kinds` (string separado por comas o `all`, como `get_references`) y no
     `edge_types` (lista).
  2. `symbol` acepta la ruta de un archivo (su símbolo de archivo): sin eso, `imports` `out` no
     tenía de dónde salir (las importaciones son del archivo).
  3. Paginación con corte de lectura: una página llena no lee niveles más profundos (`partial`), así
     que `total` es exacto solo cuando no hay `partial` (lección "nada se lee si no puede
     mostrarse"; un nivel siempre se lee entero, y las páginas son estables).
  4. Aristas: solo las que alcanzaron a un nodo listado en su profundidad (no las cruzadas hacia
     nodos de niveles anteriores ni hacia las raíces), agrupadas por par y relación con
     `occurrences`.
  5. Índice viejo: error que pide `devctx index --full` (como dice la spec, (g)), no `warning`: no
     hay camino 0.9 del que responder.
  6. No lista llamadores de llamadas sin decidir al nombre (ver hallazgos).
  7. TUI: no aplica — su vista de grafo es la de `impact_analysis`; no se agregó una de `traverse`.
  8. La validación de parámetros corre también en la tool MCP antes de buscar el serve, para que un
     `depth: 9` se responda sin levantar nada.
- **Lo que NO se verificó:** macOS/Windows (solo CI); la salida humana de `devctx traverse` (sin
  test: se probó el JSON por la API y la tool); backend-b, frontend y legacy-migration no medidos
  con `traverse`; costo de indexado, tamaño y RAM no aplican (sin escrituras nuevas).
- **Riesgos abiertos / siguiente:** (1) TASK-015 integra estas docs en la guía de reindex. (2) Si
  hace falta ver los llamadores sin decidir en `traverse`, sería un `min_confidence: low` que los
  liste en el primer nivel como hace `impact` (una sentencia más). (3) `total` con `partial` es una
  cota inferior: un cliente que quiera el total exacto pide `limit` grande o recorre las páginas.
