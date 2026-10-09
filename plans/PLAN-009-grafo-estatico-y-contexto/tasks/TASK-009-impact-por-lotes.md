# TASK-009 — `impact_analysis` por lotes, con tope, confianza y marcas

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-008
- **Estado:** `done`

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

- [x] **Paso 1 — tests que fallan.** (a) un `Store` de test con contador de consultas: `impact` a
      profundidad 3 hace ≤ 2 × 3 + constante consultas, sin importar el tamaño de la frontera;
      (b) con 1 000 callers sintéticos y `max_nodes = 200` → 200 + `omitted.count = 800`; (c) tests
      y externos excluidos por defecto y presentes con `include_*`; (d) orden por `confidence`,
      luego `rank` (NULL hasta TASK-010: cae al nombre), luego nombre.
- [x] **Paso 2 — BFS por niveles.** Frontera como tabla temporal o lista de ids en `IN (…)` por
      lotes; una consulta por nivel y dirección; tope antes del siguiente nivel.
- [x] **Paso 3 — salida.** Por nodo: `symbol`, `sym`, `file`, `line`, `depth`, `confidence`, `via`;
      `test`/`external` solo cuando son `true`. `resolved_symbols` y `branch_fallback` como hoy.
- [x] **Paso 4 — parámetros** en MCP/API/CLI con defaults de DD-11 (Q-2 del master).
- [x] **Paso 5 — medir** (backend-b y DevCtxEngine; backend-a pendiente de OK, ver Resultado) p50/p95 de `get`, `map`, `actualizar`, `OfficeService.actualizar` en
      backend-a (serve caliente) con el arnés.

## Criterios de aceptación

- [x] Tests (a)-(d) verdes; tests existentes de `impact` (`graph.rs` tests, `state.rs` tests) verdes
      o adaptados solo en lo aditivo.
- [ ] backend-a: `impact("get")` y `impact("map")` p95 ≤ 300 ms. **Pendiente** (sin OK para
      indexar backend-a); en backend-b y DevCtxEngine p95 ≤ 68 ms en todos los casos.
- [x] La salida JSON de 0.9.0 sigue siendo un subconjunto de la nueva (campos viejos con el mismo
      significado).

## Riesgos

- Un `IN (…)` con miles de ids puede ser más lento que una tabla temporal: medir ambas en la copia
  de backend-a y elegir con números.
- Cambiar el default (sin tests) cambia lo que ve un agente acostumbrado: queda en las notas del
  release y en la descripción del tool.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`, con el p95 de backend-a **pendiente**: indexar la copia completa de
  backend-a necesita el OK del usuario, que no llegó durante la task (ajuste de alcance del
  orquestador). Se desarrolló y midió con backend-b y DevCtxEngine; no se copió ni indexó
  backend-a, y la base de backend-a que dejó TASK-005 en su sandbox no se abrió (schema y
  `LINK_VERSION` viejos).
- **Resumen:** en una rama con grafo de símbolos vigente, `impact_analysis` recorre `live_edges`
  **por id y por niveles**: una consulta por profundidad y dirección, sea cual sea el tamaño de la
  frontera; el tope (`max_nodes`, 200 por dirección) se aplica antes de leer el nivel siguiente;
  dentro de cada nivel, orden por `confidence`, `rank` (NULL hasta TASK-010: cae al nombre) y
  nombre. Por defecto sigue solo llamadas `high`/`medium` y deja afuera tests y externos; todo lo
  que queda afuera se **cuenta** (`below_confidence`, `excluded`, `omitted` + `omitted_by_limit`)
  y no se recorre. `min_confidence`, `include_tests`, `include_external` y `max_nodes` en la tool,
  `GET /impact`, `devctx impact` y las docs EN/ES. Una rama vieja (otro extractor, grafo fuera de
  paso), sin filas en `symbols` o con error de lectura contesta con el recorrido de 0.9.0 sobre
  `graph_edges` (sin filtros ni tope: la misma respuesta que 0.9.0) más su `warning`. Implementa
  DD-11.
- **Primer commit (MINOR de la revisión de TASK-008):** en un `impl Trait for T` con `T` de
  afuera, el método propio del `impl` también queda `medium` (un método inherente de `T` le gana:
  `self.len()` en `impl Counted for String` llama a `String::len`). Fixture nueva en
  `link_rust` (`an_impl_method_for_an_outside_type_is_no_surer_than_medium`, **fallaba antes**:
  daba `high`); el assert de `u32.bow` en `self_in_an_impl_of_a_repo_trait_reaches_the_trait` (test
  nuestro, no de contrato) pasa a `medium`. **`LINK_VERSION` 20.**
- **Diseño:**
  - *Store* (`crates/devctx-store/src/impact.rs`, nuevo): `Store::impact_graph(repo, branch, name,
    file, &ImpactOptions) -> SymbolImpact`. La semilla es `lookup_symbols` (la misma expansión de
    `read_symbol`/`get_references`; `resolved` = sus nombres calificados). `walk` recorre un lado:
    por nivel, una llamada a `impact_level` (una sentencia, `IN (ids…)` literal), la mejor arista
    por nodo, filtros (confianza → externo → test; lo filtrado se cuenta y no se recorre), orden,
    tope. Un nombre **sin definición** toma como semilla sus sitios externos (regla de
    `lookup_external`, como `get_references`): su upstream son los llamadores de esos sitios.
    `Store::impact_analysis` (0.9.0) queda como camino viejo.
  - *MCP* (`crates/devctx-mcp/src/state/impact.rs`, nuevo): `ImpactQuery` (pública: la usan API,
    CLI y `Backend`), `impact_on` elige el camino con el `Reader` de TASK-008 y arma la respuesta
    con un solo constructor para los dos caminos (mitad del presupuesto por dirección como 0.9.0,
    `resolved_symbols` con `merged_declarations`). Sin definición: `external` + `called_from` +
    `next_step` o `suggestions` (`not_found_hints` de `read_symbol`). `do_impact_with`,
    `impact_at` (CLI local, con `branch_fallback` y avisos), `Backend::impact(symbol, depth, &q)`
    (remoto: query string; si pidió filtros y la respuesta no trae `confidence`, `warning` de
    filtros no aplicados, como `get_references`).
  - *CLI*: `devctx impact --min-confidence --include-tests --include-external --max-nodes`; imprime
    confianza, marcas y archivo:línea, y en stderr lo que quedó afuera. *TUI*: la vista local usa
    el grafo nuevo (defaults) cuando la rama está vigente y tiene filas; si no, 0.9.0.
- **Números** (sandbox `mktemp -d -p /var/tmp`, 222 MB; copias sin `.devctx`, verificado con
  `find`; índices con el pipeline real y el embedder sin modelo de `lookup_bench` —parse,
  `symbols`/`edges` y link pass reales, vectores constantes— desde un arnés `#[ignore]` nuevo,
  `state::impact::tests::impact_bench`, binario `--release`; cada llamada de la tool abre el store
  como el serve; 40 corridas por caso, dos rondas intercaladas; carga 2-11; "camino viejo" = el
  mismo índice sellado por otro extractor). p50 / p95 en ms, peor de las dos rondas:

  | Caso | tool (defaults) | nodos (omitidos por tope) | camino viejo (0.9.0) | nodos 0.9.0 |
  |---|---|---|---|---|
  | DevCtxEngine `get` (7 definiciones) | 32,4 / 43,5 | 40 | 745 / 805 | 1 224 |
  | DevCtxEngine `map` (sin definición: externo) | 27,2 / 36,9 | 200 (+12) | 977 / 1 059 | 1 451 |
  | DevCtxEngine `new` (29 definiciones) | 57,1 / 61,1 | 252 (+56) | 1 423 / 1 550 | 1 853 |
  | DevCtxEngine `open` | 55,8 / 62,5 | 253 (+16) | 389 / 425 | 767 |
  | DevCtxEngine `Store.open` | 48,3 / 57,7 | 109 | 86 / 100 | 190 |
  | DevCtxEngine `open_in_memory` | 40,0 / 43,3 | 215 (+7) | 146 / 165 | 164 |
  | DevCtxEngine `run` | 61,9 / 68,0 | 387 (+93) | 528 / 599 | 855 |
  | backend-b `get` | 15,9 / 18,8 | 5 | 199 / 232 | 257 |
  | backend-b `map` (externo) | 32,0 / 35,3 | 138 | 184 / 204 | 211 |
  | backend-b, método de actualización (bare, 15 definiciones; el `actualizar` del plan) | 25,2 / 30,0 | 208 (+253) | 87 / 98 | 78 |
  | backend-b, el de un servicio, calificado (el `OfficeService.actualizar` del plan) | 24,8 / 29,3 | 20 | 32 / 38 | 33 |
  | backend-b, helper de conversión muy llamado | 25,7 / 31,2 | 33 | 26 / 32 | 10 |
  | backend-b, constructor de un DTO muy usado | 23,5 / 25,2 | 9 (94 tests contados) | 10 / 12 | 0 |
  | backend-b `toJson` | 24,0 / 28,3 | 64 | 70 / 81 | 67 |

  **Criterio (p95 ≤ 300 ms en `get`/`map`):** cumplido en backend-b y DevCtxEngine (p95 máximo de
  todos los casos: 68 ms); **backend-a pendiente**. Contra el camino viejo: 10-25× más rápido donde
  0.9.0 recorría mucho (`get`, `map`, `new`); donde 0.9.0 encontraba poco (un nombre calificado con
  pocos llamadores, un constructor sin aristas por nombre) el nuevo cuesta ~10-15 ms más (el piso
  de abrir el store, `lookup_symbols` y 4-6 consultas por id con joins a `symbols`).

  **Frontera `IN (…)` contra tabla temporal** (y un parámetro `UBIGINT[]` con `unnest`), el
  recorrido solo con el store abierto, p50 / p95, por defecto y sin filtros ni tope (frontera más
  grande: 951 nodos, `map` de DevCtxEngine):

  | | `IN` literal | lista `UBIGINT[]` | tabla temporal |
  |---|---|---|---|
  | DevCtxEngine, defaults, rango de p95 de los 7 casos | 16,1-59,5 | 19,9-56,4 | 21,0-88,9 |
  | DevCtxEngine, sin filtros ni tope | 29,9-57,7 | 39,7-64,6 | 55,2-102,5 |
  | backend-b, defaults | 12,9-23,4 | 20,4-28,4 | 25,4-41,9 |
  | backend-b, sin filtros ni tope | 17,7-28,5 | 22,2-30,4 | 31,0-44,5 |

  Se eligió **`IN` literal** (ids enteros escritos en la sentencia: nada que escapar; una sentencia
  por nivel): es el más rápido o empata en todos los casos; la tabla temporal es la más lenta
  (vaciar + appender + join: tres sentencias por nivel) en todos. Las tres quedan en
  `FrontierSql` (oculto) para volver a medir con fronteras de miles en backend-a.
  **No medido en esta task:** backend-a (sin OK), relevancia del arnés de TASK-001 (no cambia: la
  tool no usa vectores), costo de indexado, tamaño y RAM (esta task no cambia la escritura; el
  bump de `LINK_VERSION` relinkea una vez).
- **Hallazgos de §2:** el "1 883 upstream" de `get` en 0.9.0 era, en su mayor parte, la mezcla por
  nombre de todos los `.get(` de librerías (en DevCtxEngine: 1 224 nodos en 0.9.0 contra 82 sin
  filtros ni tope en el grafo nuevo). El grafo nuevo, como `get_references`, usa las definiciones
  del repo cuando las hay y los sitios externos solo cuando no.
- **Archivos tocados:** `crates/devctx-parse/src/resolve/link.rs`, `crates/devctx-parse/tests/
  {link_rust.rs, fixtures/rust/link/crates/app/src/review.rs}`, `crates/devctx-index/src/{link.rs,
  lib.rs}`, `crates/devctx-store/src/{impact.rs (nuevo), lib.rs, lookup.rs, graph.rs}`,
  `crates/devctx-mcp/src/{state.rs, state/impact.rs (nuevo), state/lookup.rs, lib.rs, backend.rs}`,
  `crates/devctx-api/src/lib.rs`, `crates/devctx-cli/src/{main.rs, remote.rs}`,
  `crates/devctx-tui/src/lib.rs`, `CHANGELOG.md`, docs EN/ES (`symbol-graph`, `mcp-integration`),
  el design (DD-11) y el master. Públicos nuevos: `Store::{impact_graph, impact_graph_with
  (oculto), impact_level (oculto), impact_top_called (oculto)}`, `devctx_store::{ImpactOptions,
  ImpactNode, ImpactSide, SymbolImpact, Direction, FrontierSql, LevelRow, DEFAULT_IMPACT_DEPTH,
  DEFAULT_IMPACT_MAX_NODES}`, `devctx_mcp::state::{ImpactQuery, do_impact_with, impact_at}`.
  Cambiado: `Backend::impact(symbol, depth, &ImpactQuery)` y `RemoteClient::impact` del CLI (un
  parámetro más). `do_impact(state, symbol, depth)` conserva su firma (defaults). Sin cambios de
  schema, de `languages/*.json` ni de config.
- **Tests** (`TMPDIR=<sandbox> cargo test --workspace --locked`, verde; `fmt --check`; `clippy
  --workspace --all-targets --locked -D warnings`, también con `--features gpu`; gate completo
  antes de cada commit de código: 1036 y 1046 pasan, 0 fallan):
  - (a) `a_level_is_one_query_whatever_the_frontier`: 3 niveles de 1 000 llamadores, ≤ 2 × 3
    sentencias de nivel (contador por hilo en `impact_level`; la semilla es una consulta más,
    constante); la tabla temporal, ≤ 3 × 2 × 3.
  - (b) `the_cap_counts_the_rest_and_stops_before_the_next_level` (store: 200 + 800, un solo
    nivel leído; un nivel que llena el tope justo deja contar el siguiente) y
    `the_tool_caps_at_max_nodes_and_counts_the_rest` (tool, índice real de 1 000 funciones Python:
    200 listados, `omitted: {count: 800, reason: "limit"}`, `omitted_by_limit.upstream: {800,
    depth 1}`).
  - (c) `tests_and_externals_are_counted_by_default_and_listed_on_request` (store) e
    `impact_over_the_symbol_graph_marks_and_counts` (tool, Java real: `test`/`external` marcados,
    `excluded`).
  - (d) `a_level_is_ordered_by_confidence_rank_and_name`.
  - Requisito 1: `discarded_calls_never_reach_an_impact_and_low_is_counted` (store) y
    `discarded_calls_reach_no_impact` (tool: la descartada del fixture marcada externa y apuntada
    a una definición, en los dos sentidos, con todos los filtros abiertos).
  - Requisito 3: `a_stale_index_walks_as_0_9_0_and_says_so` (otro extractor y rama sin filas) e
    `impact_branch_fallback_over_the_symbol_graph`.
  - Otros: `a_name_with_no_definition_walks_from_its_external_sites`,
    `a_name_without_definition_says_what_it_is`, `a_node_shown_later_is_not_counted_and_recursion_adds_nothing`,
    `omitted_adds_the_budget_and_the_cap`, `bad_filters_are_errors`,
    `an_impact_without_confidence_says_the_filters_were_not_applied` (backend remoto),
    `impact_takes_its_filters_from_the_query_string` (API),
    `impact_over_the_symbol_graph_keeps_the_0_9_0_answer` (devctx-index).
  - **Contrato (requisito 2):** los asserts de los tests de impact de `graph.rs`
    (`impact_bfs_upstream_and_downstream`, `traversal_does_not_re_expand_the_names_it_walks`,
    `impact_seeds_from_every_form_of_a_bare_name`,
    `resolving_a_bare_name_lists_every_declaration_it_could_mean`,
    `a_qualified_name_does_not_collect_its_homonyms`) pasaron a helpers `check_*` sin cambiar
    ningún valor esperado (en el último, la expresión `store.resolve_symbol(…)`/`get_callers(…)`
    pasó a ser el cierre que recibe el helper); el test original los llama sobre `graph_edges` y
    `the_0_9_0_impact_asserts_hold_over_the_symbol_graph` sobre el grafo nuevo, con los defaults y
    con todos los filtros abiertos (el de `flush` sin decidir solo con `low`: por defecto se
    cuenta). `an_unknown_name_resolves_to_itself_and_finds_nothing` no se porta: su assert es sobre
    `resolve_symbol` (devuelve el nombre mismo; el grafo nuevo devuelve vacío), y en el JSON los
    dos dan lo mismo (sin `resolved_symbols`). El de `state.rs`
    (`a_branch_nobody_indexed_answers_the_graph_from_the_indexed_one`) tiene su par sobre el grafo
    nuevo en `impact_branch_fallback_over_the_symbol_graph`; el de `devctx-index`
    (`graph_edges_stays_readable_by_0_9_0`), en `impact_over_the_symbol_graph_keeps_the_0_9_0_answer`.
    `mcp_tools`, el CLI y la API no tenían tests de impact (solo el nombre de la tool en
    `mcp_binding`). Ningún assert existente se tocó.
  - **Fallaban antes del fix:** `an_impl_method_for_an_outside_type_is_no_surer_than_medium`
    (corrido antes del cambio en `link.rs`). Los de impact se escribieron con el código y se
    verificaron **por mutación** (revertir la regla, `touch`, correr): `live_edges` → `edges`
    (fallan los dos de descartadas); una consulta por nodo (falla (a)); sin cortar tras el tope
    (falla (b)); sin filtro de externos o de tests (falla (c) y el de marcas de la tool); sin
    `rank` o sin confianza en el orden (falla (d)); default `low` (falla el de descartadas/`low`);
    sin `Reader` (falla el de índice viejo); `omitted` sin sumar el tope (fallan el de suma y el de
    tope de la tool); sin semilla externa (fallan los dos de nombre sin definición); nota remota
    desactivada; validación de filtros después de abrir el store (falla el de la API).
- **Contrato JSON** (todo aditivo): cada nodo agrega `confidence`, `via`, y `sym`/`file`/`line`
  si tiene definición, `test`/`external`/`undecided` si son verdaderos; el objeto agrega
  `below_confidence`, `excluded`, `omitted_by_limit`, y para un nombre sin definición
  `external`/`called_from`/`next_step` o `suggestions`; `omitted.count` suma tope y presupuesto
  (`reason: "budget"` si el presupuesto cortó algo). Parámetros nuevos: `min_confidence`,
  `include_tests`, `include_external`, `max_nodes`.
- **Desviaciones:**
  1. p95 de backend-a no medido (sin OK de indexar la copia completa); medido en backend-b y
     DevCtxEngine.
  2. **Semántica de un nombre pelado:** con definiciones en el repo, la semilla son solo esas (0.9.0
     mezclaba por nombre todos los sitios, también los de librerías); sin definiciones, sus sitios
     externos. Es la regla de `get_references` (DD-10); por eso `get` de DevCtxEngine baja de
     1 224 nodos a 40 (82 sin filtros). DD-11 actualizado.
  3. Se siguen `calls` e `instantiates` (DD-11 no lo decía).
  4. Un nivel que llena el tope justo deja leer el siguiente (una consulta) solo para contarlo:
     sin eso, la respuesta se cortaría sin `omitted`.
  5. `file`/`line` son los de la **definición**; un nodo sin definición no los lleva.
  6. El camino viejo no aplica filtros ni tope: es la respuesta de 0.9.0 más el aviso (requisito 3).
  7. Medición con un arnés propio (`impact_bench`) y no con `scripts/graph-eval/run.sh`, como en
     TASK-008.
- **Riesgos abiertos / siguiente:** (1) backend-a: medir cuando haya OK (los casos están en el
  plan: `get`, `map`, `actualizar`, `OfficeService.actualizar`), y re-medir `IN` contra tabla
  temporal con fronteras de miles. (2) `rank` es NULL hasta TASK-010: el orden cae al nombre.
  (3) No se siguen `inherits`/`implements`: cambiar un método de una interfaz no alcanza a quien
  llama a la implementación por la interfaz ni al revés (candidato para `traverse`, TASK-013).
  (4) El cambio de defaults cambia lo que ve un agente: está en la descripción de la tool, el
  CHANGELOG y las docs.
