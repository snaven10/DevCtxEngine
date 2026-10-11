# TASK-011 — Tool `repo_map` con PageRank personalizado bajo presupuesto

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-010
- **Estado:** `done`

---

## Objetivo

Una tool nueva `repo_map` que, con ~1k tokens, le da al agente un mapa del repo: las firmas de los
símbolos más relevantes para su foco (archivos, símbolos o una consulta), agrupadas por archivo y
ordenadas por PageRank personalizado, sin cuerpos. Es la orientación inicial barata que hoy no
existe (Aider: archivo correcto en el 70.3 % de SWE-bench Lite atribuido al repo-map). Implementa
DD-12 (parte personalizada) y DD-14.

## Contexto verificado

- Patrón para agregar una tool (verificado en las existentes): función `do_*` en
  `crates/devctx-mcp/src/state.rs` → método en `Backend` con brazo local y remoto
  (`crates/devctx-mcp/src/backend.rs`, p. ej. `read_symbol` l.640, `build_context` l.648) → ruta en
  `crates/devctx-api/src/lib.rs` (`router`, l.43-77) → `#[tool(...)]` en
  `crates/devctx-mcp/src/lib.rs` (p. ej. `build_context`, l.1247-1251) → subcomando opcional en
  `crates/devctx-cli/src/main.rs` (enum `Command`, l.52+).
- `identifier_tokens` para sacar identificadores de una consulta (`crates/devctx-search/src/lib.rs:600-641`).
- Estimación de tokens del producto: chars/4 (`CHARS_PER_TOKEN`, `state.rs:946`;
  `devctx_chunk::estimate_tokens`, `crates/devctx-chunk/src/chunk.rs:77-79`).
- Contrato de salida textual con cierre `[devctx] …` como `compose_context`
  (`state.rs:4295-4301`, `close_context`).
- Caché por proceso: `AppState` ya cachea modelos con liberación por inactividad
  (`release_idle_models`, `state.rs:350`).
- Resolución de proyecto y rama: `graph_branch` (`state.rs:884`) con `branch_fallback`; parámetro
  `project` como en las tools de PLAN-008 TASK-010.

## Archivos

- **Crear:** `crates/devctx-mcp/src/repo_map.rs` (selección y render; puro y testeable),
  `crates/devctx-index/src/pagerank.rs` (ya existe de TASK-010: agregar personalización).
- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_repo_map`, caché de adyacencia por
  `(repo, branch, generación del índice)` invalidada al terminar un `index`),
  `crates/devctx-mcp/src/backend.rs`, `crates/devctx-mcp/src/lib.rs`, `crates/devctx-api/src/lib.rs`
  (`GET /repo-map`), `crates/devctx-cli/src/main.rs` (`devctx map [--budget N] [--focus …]`),
  `crates/devctx-cli/src/help_map.rs` si lista comandos.

## Pasos

- [x] **Paso 1 — tests que fallan.** (a) render nunca supera `budget × 1.15` tokens (propiedad sobre
      grafos aleatorios chicos); (b) con `focus_files = [X]`, los símbolos que X usa aparecen antes
      que los globalmente centrales; (c) sin foco = orden de `rank` global; (d) tests y externos
      nunca aparecen; (e) jerarquía por `parent_id` en el render; (f) rama v1 → error que pide
      `devctx index --full`.
- [x] **Paso 2 — personalización** (DD-14): `focus_symbols` e identificadores de `query` × 10,
      `focus_files` × 50 como referenciadores, prefijo `_` × 0.1, nombres definidos en > 5 archivos
      × 0.1; misma matriz ponderada de DD-12.
- [x] **Paso 3 — selección** por búsqueda binaria de cuántas firmas entran (tolerancia 15 %);
      archivos por rank agregado, dentro por línea.
- [x] **Paso 4 — caché de adyacencia** en `AppState`; se suelta con la política de inactividad y se
      invalida con cada `index` terminado.
- [x] **Paso 5 — exposición** MCP/API/CLI; descripción del tool clara ("para orientarse; para leer
      código usá `read_symbol`/`read_file`").
- [x] **Paso 6 — medir** con el arnés: casos con foco → archivo esperado presente en el mapa; tokens;
      latencia p95 en backend-a (fría y caliente); memoria de la caché.

## Criterios de aceptación

- [x] Tests (a)-(f) verdes.
- [x] Arnés: archivo esperado presente en ≥ 70 % de los casos con foco; tokens ≤ 1024 × 1.15 con el
      default.
- [x] p95 ≤ 500 ms caliente en backend-a; primera carga < 1 s; memoria de la caché anotada.

## Riesgos

- El mapa sin foco en un repo grande puede no decir nada útil: se documenta que rinde con foco
  (archivos abiertos, símbolos de la tarea) y el agente lo pide así.
- Firmas largas (Java con anotaciones): tope de 200 caracteres (DD-17) y anotaciones fuera.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. Tests (a)-(f) verdes; arnés: archivo esperado en el mapa en **7/7**
  casos con foco por identificador (casi tautológico, revisión M2) y en **16/17 con foco por
  archivo vecino** (la medida de DD-14, abajo en la revisión), tokens ≤ 1024 siempre (máx. 996); backend-a: p95
  caliente 34 ms, primera carga 132 ms, caché ~7,4 MB. Antes, los follow-ups de la segunda
  revisión de TASK-013 (MINOR 1, NITs y la re-medición del MINOR 2, que encontró dos fallos más).
- **Resumen:** tool nueva `repo_map` (MCP, `GET /repo-map`, `devctx map`, `Backend` local y
  remoto): con ~1k tokens, las firmas (sin cuerpos) de los símbolos más relevantes para el foco
  (`focus_files`, `focus_symbols`, `query`), agrupadas por archivo, miembros bajo sus contenedores,
  ordenadas por PageRank personalizado sobre la matriz de DD-12 (solo `live_edges`); nunca tests
  ni externos; cierre `[devctx]` con lo que el foco encontró y cuántos símbolos no entraron.
  Detalle y diferencias en DD-12 y DD-14 ("Implementado en TASK-011").
- **Commits** (sobre `b5ddae9`, sin push):
  1. `f31a047` fix(traverse): segunda revisión de TASK-013 (MINOR 1 y NITs).
  2. `535e7a8` fix(traverse): presupuesto respetado con muchas aristas chicas y aristas solo de la
     página (hallado al re-medir, MINOR 2).
  3. `d16e171` fix(parse): pánico de `TypeText::parse` con un `>` antes del primer `<` (hallado al
     indexar el frontend para el arnés: el index entero se caía).
  4. `a2c659a` feat(index): `RankMatrix` (global y personalizado) y `RankEdge` sin cadenas.
  5. `ed5d31f` feat(repo_map): la tool.
  6. docs EN/ES, CHANGELOG, design (DD-12, DD-14, DD-16), master y Resultados de TASK-011/013.
- **Números** (sandbox `mktemp -d -p /var/tmp`; copias de solo lectura con `tar` sin `.devctx`/
  `target`/`node_modules`, `find` vacío antes de indexar; índices con el embedder sin modelo del
  arnés `repo_map_bench` —el grafo es real, los vectores no importan al mapa—; binario de test
  `--release`; carga de la máquina 4-8):
  - *Arnés* (casos de `scripts/graph-eval` y los privados por `$DEVCTX_EVAL_CASES`; la consulta del
    caso es el `query` del mapa, `budget` 1024; "con foco" = la consulta aporta un identificador que
    nombra un símbolo — el mapa dice `personalized to the focus`):

    | Repo | casos | con foco: archivo esperado en el mapa | sin foco (mapa global): archivo presente | tokens (máx.) |
    |---|---|---|---|---|
    | DevCtxEngine | 10 | **3/3** | 3/7 | 992 |
    | backend-a | 8 | **2/2** | 0/6 | 996 |
    | backend-b | 2 | — (0) | 2/2 | 891 |
    | frontend | 6 | **2/2** | 0/4 | 936 |
    | **Total** | 26 | **7/7 = 100 %** (meta ≥ 70 %) | 5/19 | **≤ 1024** (meta 1177) |

    Los casos con foco son los de identificador (la consulta es el nombre): el mapa lista su
    definición, así que el criterio mide sobre todo que la personalización y la selección no la
    pierdan. Los de comportamiento en lenguaje natural no tienen identificador y caen al mapa
    global, que acierta por azar (5/19): el riesgo de la spec se confirma y la tool lo dice
    (`query: no identifier in it`) y pide un foco. legacy-migration no se indexó (no estaba
    autorizado para esta task).
  - *Latencia y memoria* (40 corridas calientes por caso, intercaladas, más el mapa sin foco):

    | Repo | símbolos / pares | primera carga (leer + armar / con el mapa) | caliente p50 / p95 | caché estimada / ΔRSS |
    |---|---|---|---|---|
    | backend-a | 22 801 / 28 942 | 104 / 132 ms | 20,9 / 34,3 ms | 7,4 MB / +7,6 MB |
    | frontend | 18 944 / 18 055 | 69 / 95 ms | 19,2 / 26,7 ms | 5,3 MB / +2,0 MB |
    | DevCtxEngine | 6 778 / 10 478 | 46 / 74 ms | 12,0 / 16,5 ms | 1,7 MB / +12,4 MB* |
    | backend-b | 3 000 / 3 758 | 16 / 35 ms | 9,6 / 14,3 ms | 0,9 MB / +1,4 MB |

    \*ΔRSS de proceso, ruidoso (el alocador). **Criterios: p95 ≤ 500 ms caliente (DD-21; DD-12
    pedía < 200 ms): cumplido; primera carga < 1 s: cumplido.** Una sola carga por corrida
    (`loads: 1`): la caché se reusó.
  - *`RankEdge` sin cadenas* (`rank_edges_bench`, backend-a, 20 corridas): 31 093 filas,
    p50 30,1 → 20,3 ms; 80 → 32 bytes por fila.
  - *Ejemplo* (DevCtxEngine, `--query search_ranked`): primer archivo `crates/devctx-search/src/
    lib.rs` con `RankOptions`, `search_ranked`, `identifier_tokens`, `reciprocal_rank_fusion`…, luego
    `devctx-core/src/types.rs` (`SearchResult`, `SearchFilter`), los traits del embedder y del
    reranker y `Store`; 45 de 4 267 símbolos en 9 archivos, 934 tokens (en las docs). Sin foco
    encabezan `Result`, `Text.pick`, `Store`, `ProjectConfig`, `AppState` (el riesgo 3 de TASK-010:
    los tipos muy referenciados encabezan el global; con foco no molestan).
- **Hallazgos de §2:** H7 (el grafo no participa del contexto) avanza: hay una vista del repo por
  grafo. Sin cambios en H1-H6. Nuevo: pánico del parser TS (commit 3).
- **Archivos tocados:** `crates/devctx-index/src/{pagerank.rs, lib.rs}`,
  `crates/devctx-store/src/{symbols.rs, lib.rs, traverse.rs}`,
  `crates/devctx-parse/src/resolve/scope.rs`, `crates/devctx-mcp/src/{repo_map.rs (nuevo),
  state/repo_map.rs (nuevo), state.rs, state/traverse.rs, state/lookup.rs (helper de test),
  backend.rs, lib.rs}`, `crates/devctx-api/src/lib.rs`, `crates/devctx-cli/src/{main.rs,
  remote.rs, help_map.rs}`, `crates/devctx-cli/tests/mcp_tools.rs` (un test nuevo), CHANGELOG, docs
  EN/ES (`symbol-graph`/`grafo-de-simbolos`, `mcp-integration`/`integracion-mcp`), design y master.
  Públicos nuevos: `devctx_index::{RankMatrix, PageRankRun}`; `devctx_store::{MapSymbol, RankKind,
  RankConfidence}`, `Store::map_symbols`; `devctx_mcp::repo_map::{MapGraph, RepoMapQuery, render,
  tokens, query_identifiers, DEFAULT_REPO_MAP_BUDGET, MIN_REPO_MAP_BUDGET}`,
  `devctx_mcp::state::{do_repo_map, repo_map_at, REPO_MAP_NEEDS_REINDEX}`,
  `devctx_mcp::backend::{Backend::repo_map, REPO_MAP_OLD_SERVER, repo_map_path}`. Cambiados:
  `RankEdge.{kind, confidence}` (enums en lugar de `String`/`Option<String>`). Sin cambios de
  schema, config, `languages/*.json`, `EXTRACTOR_VERSION` ni `LINK_VERSION`.
- **Tests** (`TMPDIR=<sandbox>/tmp cargo test --workspace --locked`; gate completo —`fmt --check`,
  `clippy -D warnings` con y sin `--features gpu`— antes de cada commit de código): 1 146 en
  `f31a047`, 1 153 en `a2c659a` (con los commits 2 y 3 debajo), 1 173 en `ed5d31f`; 0 fallan,
  ningún flake. En `f31a047` la primera corrida falló en los doc-tests por edits míos en curso
  (compilan el código del árbol): se repitieron sobre el árbol del commit (0 fallos).
  - *(a)-(f):* (a) `the_map_never_exceeds_the_budget` (300 grafos aleatorios, presupuestos dentro y
    fuera de rango, foco encontrado y no) y `the_search_fills_the_budget_within_the_tolerance`; (b)
    `what_a_focus_file_uses_comes_before_the_central_code`,
    `a_focus_symbol_ranks_first_and_a_missing_one_is_said`, `a_map_of_a_real_index`; (c)
    `without_focus_the_global_rank_decides`; (d) `test_code_never_appears` (archivo de test y código
    `#[cfg(test)]` nombrados por el foco), `a_map_of_a_real_index` (índice real: test, `mod tests`
    de Rust, `jakarta`); (e) `members_are_drawn_under_their_containers`; (f)
    `an_old_index_asks_for_a_full_reindex` (extractor viejo y rama sin filas).
  - *Otros:* `private_and_generic_names_are_dampened`, `the_map_is_deterministic`,
    `signatures_are_cleaned`, `the_query_round_trips_through_its_parameters`,
    `the_adjacency_is_cached_until_an_index_or_idleness`, `another_index_generation_reloads`,
    `a_discarded_call_lends_nothing`, `repo_map_on_an_old_server_says_to_restart_it` (MCP y CLI),
    `repo_map_takes_its_parameters_from_the_query` (API), `repo_map_refuses_a_budget_of_zero` (MCP
    por stdio); pagerank: `a_uniform_personalization_is_the_global_rank`,
    `the_focus_and_what_it_uses_outrank_the_global_hub`, `a_destination_multiplier_reweighs_the_split`,
    `personalized_ranks_are_deterministic`.
  - *Fallaban antes:* los de la tool no compilaban (API nueva, escritos contra la spec);
    `signatures_are_cleaned` falló con la primera versión (`@interface` se borraba); los de los
    follow-ups, arriba y en el Resultado de TASK-013.
  - *Verificados por mutación* (revertir la regla, `touch`, correr; los 19 fallan): presupuesto
    ignorado, tolerancia 90 %, sin personalización, archivos de foco listados, tests listados, orden
    global invertido, sin contenedores, empates solo por id, sin ×0,1 de `_`, sin ×0,1 de nombres
    comunes, símbolo nombrado sin masa, caché nunca reusada, sin soltar al terminar un `index`, sin
    generación, sin soltar por inactividad, leer `edges` en vez de `live_edges`, índice viejo
    respondido, `p` ignorado y multiplicador ignorado en `RankMatrix`. Tres escapaban al principio
    (los dos de ×0,1 y `live_edges`: el desempate o el orden por archivo los tapaba); se rehicieron
    los fixtures hasta que fallaron.
- **Contrato (lección 2):** ningún assert de search, impact, traverse ni `mcp_tools` cambió (a
  `mcp_tools` y a traverse solo se agregaron tests). Cambió el literal de `RankEdge` en el test de
  TASK-010 `the_rank_graph_reads_live_edges_only` (mismo contenido, tipos nuevos) y el helper `e()`
  de los tests de pagerank. `status.memory` suma `repo_map` (aditivo).
- **Contrato** (aditivo): tool `repo_map` (texto); `GET /repo-map?budget&focus_files&focus_symbols&query`;
  `devctx map`; `status.memory.repo_map: {loaded, repo?, branch?, symbols?, edges?,
  estimated_bytes?, load_ms?, loaded_at?, idle_secs?}`; `release_idle_models` nombra `repo map
  graph`. En traverse: `omitted_for_budget.edges` puede aparecer donde antes salía todo.
- **Desviaciones:**
  1. ×50 de `focus_files` como masa de personalización, no como multiplicador de aristas (se
     cancelaría en el reparto por fuente); los archivos de foco no se listan.
  2. Una consulta de una sola palabra cuenta como identificador (sin eso `Cached`, `dispatch` no
     tenían foco).
  3. La búsqueda nunca supera el presupuesto (la tolerancia del 15 % solo corta la búsqueda); mínimo
     256 tokens, máximo `DEVCTX_MAX_OUTPUT_TOKENS`; `budget: 0` es un error.
  4. Sin foco se usa `symbols.rank` (no se recalcula); el `rank` nulo de un pase cortado se dice.
  5. Commits fuera de alcance, hallados midiendo: el presupuesto y las aristas de `traverse`
     (commit 2) y el pánico del parser TS (commit 3).
  6. "Casos con foco" del arnés = los de identificador; la muestra es chica (7).
- **Lo que NO se verificó:** macOS/Windows (solo CI); el mapa con un modelo real (no cambia: el
  mapa no lee vectores); legacy-migration; el mapa vía un serve real (`devctx map` contra el
  serve) — probado por la API, el backend remoto con 404/500 y el MCP por stdio; el CLI local
  `repo_map_at` no tiene test propio (comparte `map_on` con la tool).
- **Riesgos abiertos / siguiente:** (1) Sin foco, en un repo grande el mapa dice poco (5/19
  presencias): la descripción pide foco; TASK-012/015 pueden sugerir pasar los archivos abiertos.
  (2) `traverse` sobre un nombre de miles de definiciones con dispatch masivo tarda 2-4 s (Resultado
  de TASK-013): TASK-016. (3) Medir el arnés con foco por archivo (`focus_files` = un archivo
  vecino) da una medida más exigente que la de identificador; queda para TASK-016.
- **Revisión (APROBADA CON CAMBIOS, sin BLOCKER), resuelta en commits encima de `7c1f4b6`:**
  - *Patología de traverse (bug de TASK-017)* (`3d84718`): la causa medida (instrumentación
    temporal, ya quitada) no eran solo las filas clonadas: `override_equivalents` tardaba 1,2 s de
    1,3 s, porque hacía join de cada método de la frontera con todos los métodos del mismo nombre de
    todos los tipos del grafo de supertipos (1 001 × 1 001 filas con cadenas) y filtraba por alcance
    en memoria. Ahora el alcance se calcula antes (por tipo, una vez) y la consulta solo lee los
    pares (tipo del origen, tipo alcanzado). Además, lo pedido: a lo sumo 16 orígenes por
    equivalente (`DISPATCH_MAX_ORIGINS_PER_EQUIVALENT`, por id de origen; el resto en
    `omitted_by_limit.dispatch.origins_capped` + `max_origins`) y un presupuesto de 8 192 filas de
    dispatch por llamada (`DISPATCH_MAX_ROWS`; `rows_capped` + `max_rows`, y en traverse
    `total_is_lower_bound`), con las filas del nivel ordenadas antes para que el corte sea
    determinista. Tests que **fallaban antes**: `an_equivalent_shared_by_many_origins_is_read_once_per_capped_origin`
    (120 filas de dispatch con 40 implementaciones, tope 64),
    `the_equivalents_query_reads_only_reachable_types` (con la consulta vieja: 1 600 filas leídas
    por 40), `capped_dispatch_origins_and_rows_are_said` (JSON, sin campos);
    `dispatch_rows_have_a_budget_per_call` y `dispatch_origins_are_capped_per_equivalent` se
    escribieron con la API (verificados **por mutación**: sin `truncate` o sin presupuesto fallan).
    Tests de TASK-017 y de contrato verdes sin tocarlos. **Sintético** (3 000 `get`, 1 000
    implementan `IGet`, 300 llamadores; `--release`, 40 corridas × 2 rondas, carga 4-5): traverse
    `get` `calls` `in` p95 2,2 s → **93-104 ms**, `both` 3,5 s → **115-126 ms**, `IGet.get` 34-40 ms;
    impact_analysis (tool, `impact_bench`) `get` p95 **98 ms**, `IGet.get` **66 ms** (antes: el arnés
    no terminó una pasada en 18 min). backend-a: mismos nodos y aristas, p95 ≤ 62 ms (antes ≤ 107).
  - *M1* (`4746186`): masa de 50 por archivo de foco repartida entre sus símbolos y 10 por término
    repartida entre sus definiciones. **Fallaban antes**:
    `a_named_symbol_keeps_its_weight_next_to_a_large_focus_file` (participación del símbolo
    nombrado 0,05 %; y la de uno junto a un término de 300 definiciones) y
    `focus_files_weigh_the_same_whatever_their_size` (0,453 contra 0,0045); por mutación, volver a
    la masa por símbolo (de archivo o de definición) hace fallar cada uno.
  - *M2* (`0a500ea`, arnés): foco = el **vecino más fuerte**, el archivo que más usa el símbolo esperado (llamadas,
    instanciaciones, referencias, imports; no el archivo esperado ni tests), sin query; ¿aparece el
    archivo esperado? DevCtxEngine 3/3 (puestos 1, 3, 8 de 18-32 archivos), backend-a 6/7 (puestos 1,
    1, 1, 1, 4, 7, 7; el que falta no entra en 22 archivos), backend-b 1/1, frontend 6/6 (puestos 1,
    2, 2, 4, 7 y 29 de 30): **16/17 = 94 %** (meta ≥ 70 %). 3 casos sin vecino (nadie usa el
    símbolo fuera de su archivo) no cuentan. Tokens ≤ 1 024. Los de identificador siguen 7/7.
  - *m1* (`686eea2`): golden bit a bit de `global()`: dos grafos de 6 nodos con los `to_bits` del
    `pagerank_run` de `b5ddae9` (compilado aparte en el sandbox) y un FNV de los 40 ranks del grafo
    de `the_ranks_are_deterministic`. Por mutación (reasociar la suma o el término base) falla el
    hash de 40 nodos; los de 6 nodos solos no lo detectaban.
  - *m3* (`9b0ae7c`): lock con `PoisonError::into_inner` y la carga fuera del lock con doble
    chequeo. `a_poisoned_cache_still_answers` **fallaba antes** (PoisonError).
  - *m5* (`4746186`): el test (a) exige `≤ effective`; pasó (el contrato ya se cumplía).
  - *m2* (`9b0ae7c`): contador `index_generation` en `index_meta`, que sube todo `index` que
    termina; la generación de la caché lo incluye. `a_run_in_the_same_second_is_another_generation`
    (verificado por mutación: sin el contador en la clave y sin la suba, falla).
  - *m6* (`9b0ae7c`): `RepoMapQuery.label` (también `label` en `GET /repo-map`) lleva el
    `[devctx] map of <proyecto>` del MCP dentro del presupuesto (`the_label_counts_in_the_budget`,
    por mutación).
  - *m4*: documentado (un slot; LRU si TASK-016 lo ve).
  - *NITs:* `TypeText::parse` de un tipo función da `None` (`c20e57d`, test que **fallaba antes**);
    variable sin usar del test (e) quitada; comas en rutas y `│`/`⋮` documentados como límites.
  - *Gate:* fmt, clippy con y sin `--features gpu` y `cargo test --workspace --locked
    --no-fail-fast` antes de cada commit de código: 1 178, 1 180, 1 183, 1 184 y 1 184 pasan.
    Flakes de carga (load 16-18 por trabajo ajeno), verdes al repetir:
    `procown::a_checkpoint_that_ends_inside_the_term_wait_still_earns_the_margin`,
    `remote::the_autospawn_mark_is_read_from_the_environment`,
    `serve_lifecycle::an_idle_server_exits_after_its_window`.
- **Segunda revisión (APROBADA CON UN CAMBIO), resuelta encima de `72366d3`:**
  - *MAJOR* (`0715d62`): el tope de orígenes por equivalente se quedaba con los 16 de id más bajo; si
    llegaban por un `implements` `low` y el que quedaba afuera por uno `high`, todas las filas salían
    `low` y, con `medium`, los llamadores caían en `below_confidence`. Ahora se ordena por cadena
    descendente y luego por origen. `the_origins_kept_are_the_surest` (20 orígenes `low`, uno `high`
    de id más alto) **fallaba antes**: 0 nodos, `below_confidence: 3`.
  - *m1* (`2e8143d`): impact pone `total_is_lower_bound: true` con `rows_capped`;
    `capped_dispatch_origins_and_rows_are_said` amplía (**fallaba antes**) y fija que con solo
    `origins_capped` no aparece.
  - *m3* (`79895fe`): un contador de drops leído antes de la carga; si un `index` termina (bien,
    mal o cancelado) mientras se lee, el grafo se responde y no se guarda.
    `a_graph_read_while_an_index_ended_is_not_kept` **fallaba antes** (quedaba guardado).
  - *Gate* por commit (fmt, clippy con y sin `gpu`, `cargo test --workspace --locked
    --no-fail-fast`): 1 185, 1 185 y 1 186 pasan, 0 fallan.
  - **Pendientes para TASK-016 (anotados, no hechos):** (m4) el arnés M2 usa el vecino más fuerte:
    medir también un vecino mediano o aleatorio y uno a 2 saltos; (m2) en downstream los pares
    `owners × reach` de la consulta de equivalentes pueden llegar a cientos de miles (una interfaz
    con muchas implementaciones en la frontera); (NIT) un serve viejo ignora `label`;
    `has_top_level_arrow` puede bajar a profundidad negativa con un `)` o `>` sobrante; empates
    exactos en el borde de las 8 192 filas (el orden fija cuáles, pero filas iguales en clave se
    cortan por posición); **reindexar el frontend del arnés antes de TASK-016** (se indexó con las
    bases basura de `TypeText` previas a `c20e57d`).

