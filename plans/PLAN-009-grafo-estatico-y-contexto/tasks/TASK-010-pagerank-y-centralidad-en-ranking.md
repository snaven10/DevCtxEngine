# TASK-010 — PageRank global al indexar y centralidad como señal en `search` híbrido

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-006, TASK-007, TASK-008
- **Estado:** `done`

---

## Objetivo

Que cada símbolo tenga un PageRank global y un in-degree calculados al final del link pass, y que
`search` en modo híbrido (y por lo tanto `build_context`) use la centralidad como **señal adicional**
—una tercera lista ponderada de la RRF— junto a las penalizaciones y el anclaje, sin tocar el modo
vectorial. Implementa DD-12 (parte global) y DD-13.

**Compuerta:** no arranca hasta que las filas de "grafo limpio" de §6 del master se cumplan con
TASK-005…008 (basura 0, externos/tests marcados, precisión de gold edges ≥ meta).

## Contexto verificado

- `search_ranked` (`crates/devctx-search/src/lib.rs:269-357`): `Hybrid` = RRF de vector + keyword
  (`reciprocal_rank_fusion(&[vector, keyword], RRF_K)`, l.301-309) → filtros duros → `apply_penalty`
  (por posición, l.175-197; `demoted_order` l.219-239) → `dedup_hits` (l.751-779) → rerank → si no es
  `Vector`, `anchor_identifiers` (l.351-356).
- RRF genérica: `devctx_core::rank::fuse_by_rank`, `RRF_K = 60` (`crates/devctx-core/src/rank.rs`).
- `SearchResult.raw_score` (`crates/devctx-core/src/types.rs:63-79`): lo usa la selección de miembro
  de grupo (`member_scores`, `crates/devctx-mcp/src/state.rs:4557-4572`) y no debe incluir la
  centralidad.
- `build_context` usa `SearchMode::Hybrid` con 30 hits (`state.rs:4137-4146`).
- Tests de penalización y anclaje en `crates/devctx-search/src/lib.rs` (p. ej.
  `anchoring_is_bounded_in_tokens_and_in_share_of_the_answer`, l.1617).

## Archivos

- **Crear:** `crates/devctx-index/src/pagerank.rs` (iteración de potencia propia, DD-12; sin
  dependencias nuevas).
- **Modificar:** `crates/devctx-index/src/link.rs` (calcular y escribir `symbols.rank`/`in_degree`
  al final del link pass), `crates/devctx-store/src/symbols.rs` (lectura de `rank` para un lote de
  chunks: símbolo más interno por rango, DD-4), `crates/devctx-search/src/lib.rs` (lista de
  centralidad en `Hybrid`), `crates/devctx-core/src/config.rs` (`search.centrality_weight`, default
  0.3; 0 apaga).

## Pasos

- [x] **Paso 1 — tests que fallan.** (a) PageRank en un grafo de 6 nodos con ranking conocido;
      determinismo entre corridas; nodos colgantes; (b) pesos: una arista `from_test` pesa 0.1 y una
      `low` 0.2; `sqrt(n)` de multiplicidad; (c) `Hybrid` con dos candidatos empatados en RRF → gana
      el de mayor `rank`; (d) `Vector` devuelve exactamente lo mismo con y sin `rank`; (e) un hit
      anclado sigue primero aunque su `rank` sea 0; (f) `raw_score` no cambia con la centralidad.
- [x] **Paso 2 — PageRank global** en el link pass: damping 0.85, tol 1e-6, máx. 50 iteraciones;
      pesos por kind/confianza/multiplicidad/test de DD-12; `in_degree` = llamadas entrantes no-test
      `≥ medium`.
- [x] **Paso 3 — señal en la RRF.** Ordenar el pool de candidatos por `rank` del símbolo más interno
      de cada hit y sumar `w / (RRF_K + pos + 1)`; solo candidatos ya traídos por algún retriever.
- [x] **Paso 4 — calibrar `w`** con el arnés de TASK-001 (0, 0.15, 0.3, 0.5): elegir el que maximiza
      la relevancia de `build_context` sin bajar Hit@5/MRR híbrido más de 2 % relativo. Si ningún
      valor mejora, default 0 y se documenta (la infraestructura queda para repo_map/ego-graph).
- [x] **Paso 5 — medir** costo del PageRank (ms) en backend-a y latencia de `search --hybrid`.

## Criterios de aceptación

- [x] Tests (a)-(f) verdes; tests de penalización/anclaje/dedup existentes verdes sin modificar.
- [x] Tabla del arnés con los cuatro valores de `w` en el Resultado y el default elegido con su
      razón.
- [x] PageRank global < 1 s en backend-a; `search --hybrid` p50 ≤ +20 ms.

## Riesgos

- Centralidad que favorece utilidades genéricas (`StringUtils`, mappers): por eso `sqrt(n)`, tests
  ×0.1, y la calibración puede dejar `w = 0`.
- Un hit cuyo chunk no está dentro de ningún símbolo (chunk de archivo, docs) no tiene `rank`: va al
  final de la lista de centralidad, sin penalidad extra.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. **Tras la revisión, `search.centrality_weight` = 0 por default
  (opt-in)**; ver "Revisión" al final. La calibración de `w` se hizo con DevCtxEngine y backend-b (12 casos);
  backend-a con modelo quedó **sin indexar** (el permiso de la sesión lo bloqueó): confirmar `w`
  con sus 8 casos queda pendiente, junto con TASK-016.
- **Paso 0 — compuerta de ruido (master §5), verificada contra los Resultados de TASK-005…009:**

  | Fila de §6 ("grafo limpio") | Valor medido | Fuente |
  |---|---|---|
  | Nodos basura (`var.*`, `LOG.*`, expresiones) = 0 | backend-a 0 / 0 / 0, backend-b 0 / 0 / 0 (antes 892 / 1 761 y 502 / 78), DevCtxEngine 0; `graph_edges` 0 / 0; TS (subconjunto de frontend) 0 | TASK-005 (re-medición tras la revisión), TASK-006 |
  | Externos marcados con evidencia (DD-9) | Java: externos por plataforma/import/supertipo, `chain_external` `medium`; TS: solo con manifest o builtin (M2), auditoría 30/30 `external_known` y 20/20 `chain_external`; Rust 78/78 y Python 40/40 aristas auditadas | TASK-005, TASK-006, TASK-007 |
  | Aristas desde tests marcadas `from_test` = `path_kind` Test | 0 diferencias en backend-a, backend-b y DevCtxEngine (la misma regla para todos los lenguajes) | TASK-005 |
  | Gold edges: ≥ 90 % en `high`, ≥ 75 % global, cobertura (no `low`) ≥ 80 % | Java 20/25 = 80 % global, `high` 19/19, cobertura 20/25 = 80 % (los 5 fallos: 3 sin arista, 2 `name_only` `low`); TS 20/20, `high` 20/20, 100 %; Python 26/27 = 96 %, `high` 26/26; Rust 26/26, `high` 22/22, 100 % | TASK-005, TASK-006, TASK-007 (segunda revisión) |

  **Se cumple.** Fuera de la compuerta (no la nombra, pero es fila de §6): "sin decidir ≤ 15 %"
  sigue arriba en Rust (17,5 %, TASK-007); Java 8,1 % / 4,2 %, TS 10,8 %, Python 1,6 %. La
  cobertura Java está justo en la meta.
- **Resumen:** cada link pass que completa calcula el **PageRank global** de los símbolos de la
  rama sobre `live_edges` y lo escribe en `symbols.rank`, con `in_degree`; la búsqueda **híbrida**
  (y con ella `build_context`) lo usa como **tercera lista de la RRF**, ponderada por
  `search.centrality_weight` (default 0,3; **0 tras la revisión**, opt-in), solo sobre los candidatos que trajeron los
  retrievers. `Vector` no cambia, el anclaje sigue ganando y `raw_score` sigue siendo la fusión de
  vector + keyword. Primer commit: el MINOR m-a y los tres NITs de la revisión de TASK-009.
- **Commits** (rama `feat/plan-009-grafo`, sin push): 59fa285 (m-a y NITs), 0fdcad6 (PageRank y
  centralidad), y el de esta documentación.
- **Números** (sandbox `mktemp -d -p /var/tmp`; copias de solo lectura sin `.devctx`, verificado con
  `find` antes de cada índice):
  - *Calibración de `w`* (arnés de TASK-001, `scripts/graph-eval/run.sh` paso `relevance`, modelo
    real `ml-granite`, serve caliente reiniciado por cada `w`, dos corridas por valor; índices con
    modelo de DevCtxEngine —391 archivos, 6 859 chunks, 1 188 s— y de backend-b —216 archivos,
    3 284 chunks, 694 s—; casos de backend-b fuera del repo):

    | `w` | DevCtxEngine (10) híbrido Hit@5 · Hit@10 · MRR | brief archivo · símbolo | backend-b (2) híbrido Hit@5 · MRR | brief archivo · símbolo | **Total (12)** híbrido Hit@5 · MRR · brief archivo |
    |---|---|---|---|---|---|
    | 0 | 8/10 · 9/10 · 0,662 / 0,661 | 10/10 · 3/4 | 0/2 · 0,127 | 1/2 · 2/2 | 8/12 · 0,573 · 11/12 |
    | 0,15 | 8/10 · 9/10 · 0,661 | 10/10 · 3/4 | 1/2 · 0,238 | 1/2 · 2/2 | 9/12 · 0,591 · 11/12 |
    | **0,3** | 8/10 · 8/10 · 0,664 / 0,659 | 10/10 · 3/4 | 1/2 · 0,583 | **2/2** · 2/2 | **9/12 · 0,650 · 12/12** |
    | 0,5 | 8/10 · 8/10 · 0,663 | 10/10 · 3/4 | 1/2 · 0,583 | 2/2 · 2/2 | 9/12 · 0,650 · 12/12 |

    Vectorial idéntico en los cuatro valores (8/10 · 0,642 y 1/2 · 0,125). Por caso: en backend-b
    los dos casos suben en el híbrido (9 → 6, 7 → 1) y el primero entra al brief desde 0,3; en
    DevCtxEngine solo se mueven los de español 6 (fuera del top 20 → 18-20) y 7 (8 → 9 / 11 / 14
    con 0,15 / 0,3 / 0,5: sale del top 10, nunca estuvo en el top 5). ~~**Elegido `w` = 0,3**~~
    (revisión: **0**, ver abajo) (el default de DD-13): el menor valor que maximiza el brief, sin bajar Hit@5 ni MRR híbrido más de
    2 % en ningún repo (DevCtxEngine 0,662 → 0,659-0,664, el ruido entre corridas de TASK-001).
    0,5 no suma nada y empuja más el caso 7. **Evidencia fina:** el brief mejora por un caso de
    backend-b; confirmar con backend-a (8 casos, pendiente de indexar con modelo) y en TASK-016.
  - *Costo del PageRank* (cálculo + escritura de `rank`/`in_degree`, la línea `rank … ms` del link
    pass): **backend-a 165 ms y 69 ms** (22 876 símbolos, dos corridas de `graph_bench` /
    `impact_bench` con embedder sin modelo, máquina con carga 17-40; pase completo 5,6 s y 3,4 s,
    el resto es resolver y escribir aristas); DevCtxEngine 58 ms (6 365 símbolos); backend-b 21 ms
    (3 000). **< 1 s: cumplido.**
  - *Latencia de `search --hybrid`* (DevCtxEngine con modelo, serve caliente, los 10 casos × 5,
    rondas intercaladas `w` 0 / 0,3 / 0 / 0,3): p50 74 / 74 / 72 / 78 ms, p95 91 / 89 / 95 /
    105 ms. **+0 a +6 ms de p50: cumplido (≤ +20 ms).** Las latencias que imprime el arnés (p50
    138-344 ms) incluyen el arranque del CLI y la carga de la máquina; no sirven para comparar `w`.
  - *Frontera de `impact` (NIT 1)*, una consulta de nivel sobre fronteras sintéticas de backend-a
    (`impact_bench`, 40 corridas, p50 upstream `IN` / `UBIGINT[]`): 256 ids 21,4 / 15,2 ms; 373:
    25,8 / 17,2; 512: 25,7 / 19,0; 768: 26,9 / 18,3; 1 024: 28,2 / 18,6 (DD-11).
  - *Sanidad del ranking* (DevCtxEngine, el top por `rank` sin símbolos de archivo): tipos y
    structs centrales (`Result`, `Store`, `ProjectConfig`, `Lang`, `Memory`, `AppState`,
    `EmbeddingProvider`, `VectorPoint`) y métodos muy llamados (`Text.pick`, `Store.w`,
    `Store.open_in_memory`); los tipos suben por los usos de tipo (`references`, peso 0,5). Una
    función de un `mod tests` en línea de Rust no es `is_test` (su archivo no es de test).
- **Hallazgos de §2:** H7 (el grafo no participa del contexto) empieza a cerrarse: el brief de
  `build_context` ya ordena por centralidad. Sin cambios en H1-H6.
- **Archivos tocados:** `crates/devctx-index/src/{pagerank.rs (nuevo), link.rs, pipeline.rs,
  lib.rs}`, `crates/devctx-store/src/{symbols.rs, impact.rs, lib.rs}`,
  `crates/devctx-search/src/lib.rs`, `crates/devctx-core/src/config.rs`,
  `crates/devctx-mcp/src/{state.rs, state/impact.rs, backend.rs, lib.rs}`,
  `crates/devctx-cli/src/main.rs`, `crates/devctx-tui/src/lib.rs`, `CHANGELOG.md`, docs EN/ES
  (`search`, `configuration`, `symbol-graph`), el design (DD-11, DD-12, DD-13) y el master.
  Públicos nuevos: `devctx_store::RankEdge`, `Store::{branch_rank_edges, write_branch_ranks,
  chunk_ranks}`, `SymbolImpact.undecided_calls`, `RankOptions.centrality`,
  `devctx_core::config::DEFAULT_CENTRALITY_WEIGHT`, `SearchCfg.centrality_weight`,
  `devctx_mcp::backend::add_unapplied_impact_note`; internos: `pagerank::{pagerank, rank_branch,
  edge_weight, kind_weight, confidence_weight}`, `LinkStats.{ranked, rank_ms}`. Config nueva:
  `search.centrality_weight`. Sin cambios de schema (`rank` e `in_degree` existían desde
  TASK-003) ni de `languages/*.json`. **`LINK_VERSION` 20 → 21.**
- **Cómo se recalcula `rank` en índices existentes:** el `LINK_VERSION` nuevo hace que la próxima
  corrida de `devctx index` (incremental alcanza) relinkee la rama entera y la rankee; no se
  reindexa ni se embebe nada (DevCtxEngine: 1 s; el pase completo de backend-a, unos 3-6 s). Un
  índice anterior no tiene ranks hasta entonces y la híbrida fusiona solo vector + keyword. Cada
  pase que completa recalcula el rank entero (también el incremental: los símbolos reescritos
  vuelven a tener rank).
- **Tests** (gate completo antes de cada commit de código: `cargo fmt --all -- --check`; `cargo
  clippy --workspace --all-targets --locked -- -D warnings`, con y sin `--features gpu`;
  `TMPDIR=<sandbox>/tmp cargo test --workspace --locked`: 1 055 pasan en 59fa285 y 1 069 en
  0fdcad6, 0 fallan; en 0fdcad6 la primera corrida tuvo un fallo de
  `backend::tests::a_refused_connection_is_retried` —el puerto "cerrado" lo tomó otro test con la
  máquina a carga 40: `Connection reset by peer`—, que pasó al repetir `devctx-mcp` y el
  workspace entero):
  - *Fallaban antes del fix* (corridos primero): `undecided_calls_have_their_own_count_without_tests_or_listed_callers`
    (m-a: `below_confidence` daba 6, unidades mezcladas), `an_unsure_caller_reached_later_by_a_decided_edge_is_walked`
    (NIT 2: faltaba `Top.t`), `a_stale_index_says_the_filters_were_not_applied` (NIT 3: sin
    aviso).
  - *Verificados por mutación* (el fix escrito, revertir la regla, `touch`, correr):
    `pagerank::tests` (con ranks uniformes y pesos sin test ni `sqrt` fallan 4 de 6; el de
    determinismo falla sin el orden de las aristas; `weights_shape_the_ranking` falló contra la
    normalización clásica, que es la que se cambió), `the_rank_graph_reads_live_edges_only`
    (falla leyendo `edges`), `the_link_pass_ranks_the_branch` (falla sin el rank en el link pass),
    `an_index_from_before_ranks_gets_them_on_the_next_run` (falla con `LINK_VERSION` 20),
    `centrality_breaks_a_fusion_tie_and_keeps_the_retriever_score` (falla con el orden de rank
    invertido), `vector_search_ignores_centrality` (falla con la centralidad en `Vector`),
    `an_anchored_hit_stays_first_whatever_its_rank` (falla sin el retriever score del hit
    anclado). Otros: `ranks_are_written_and_read_per_chunk`, `centrality_weight_defaults_and_parses`.
  - *Pasos (a)-(f):* (a) `a_six_node_graph_ranks_as_expected`, `dangling_nodes_are_redistributed`,
    `the_ranks_are_deterministic`, `degenerate_inputs`; (b) `edge_weights_follow_kind_confidence_tests_and_multiplicity`,
    `weights_shape_the_ranking`; (c) y (f) `centrality_breaks_a_fusion_tie_and_keeps_the_retriever_score`;
    (d) `vector_search_ignores_centrality`; (e) y (f) `an_anchored_hit_stays_first_whatever_its_rank`.
  - **Tests existentes de penalización, anclaje y dedup: verdes sin tocar ningún assert.** Se
    actualizaron asserts propios de TASK-009 por m-a (`undecided_calls_to_the_name_are_counted_or_listed_upstream`
    y `an_undecided_call_to_the_name_is_counted_or_listed`: el conteo pasó de `below_confidence`
    a `undecided_calls`, que es el cambio pedido).
- **Contrato JSON:** `impact_analysis` agrega `undecided_calls: {count, hint}`;
  `below_confidence` deja de incluir esas llamadas (cuenta solo símbolos); en el camino viejo,
  `warning` dice que los filtros no se aplicaron. `search`/`build_context`: sin campos nuevos; en
  híbrido cambia el orden (y `score`), `raw_score` no. Config: `search.centrality_weight`.
- **Desviaciones:**
  1. **Normalización del PageRank** (DD-12 actualizado): un nodo reparte `peso / Σ kind ×
     sqrt(n)`; lo que la confianza y el ×0,1 de test le quitan se redistribuye uniforme. Con la
     normalización clásica esos factores se cancelan en un nodo cuyas aristas los comparten todas.
  2. **Hit anclado:** su retriever score pasa a ser el mejor de la respuesta (antes el score más
     alto mostrado): sin eso la centralidad llegaba a la selección de miembro por el hit anclado.
     Sin centralidad, sin rerank y sin un hit degradado arriba es el mismo número.
  3. El MCP usa la centralidad **solo sobre un grafo vigente** (`Reader::SymbolGraph`); el CLI
     local y la TUI, por la config (un índice sin ranks la apaga solo).
  4. `rank` es una probabilidad (suma 1 por rama), sin reescalar. `in_degree` cuenta ocurrencias.
  5. NIT 1: umbral 512 y no menos, aunque la consulta sintética favorece al parámetro también en
     256-373 ids (DD-11).
  6. Calibración con 12 casos (DevCtxEngine + backend-b), no con los 30 de la línea base.
- **Riesgos abiertos / siguiente:** (1) **`w` con evidencia fina**: confirmar con backend-a (8
  casos) y frontend cuando se indexen con modelo; si backend-a no mejora o baja, `w` = 0 es la
  salida (la infraestructura queda para repo_map/ego-graph). (2) El caso en español 7 de
  DevCtxEngine baja en el híbrido con `w` ≥ 0,3 (8 → 11; sigue en el brief). (3) Los tipos muy
  referenciados (`Result`) encabezan el rank: si ensucian el repo-map de TASK-011, bajar el peso
  de `references` o excluir alias de tipos genéricos. (4) Funciones de `mod tests` en línea (Rust)
  no son `is_test`. (5) No verificado en macOS/Windows (solo CI), ni en frontend.
- **Revisión (REQUEST CHANGES, sin BLOCKER), resuelta en commits encima de 7264d4b:**
  - *MAJOR 1 (decisión de la revisora):* **default `w` = 0, opt-in en 0.10.0.** 12 de 30 casos sin
    backend-a; el brief +1 está dentro del ruido de ±1; el MRR sube solo por backend-b; Hit@10 de
    DevCtxEngine 9 → 8. La tabla de calibración de arriba queda como evidencia; la subida se
    re-evalúa en TASK-016 (paso 3b nuevo) con los 30 casos y el corte por idioma.
  - *MAJOR 2 — corte por idioma de la calibración* (de los mismos reportes; español 4 casos = 3 de
    DevCtxEngine + 1 de backend-b, inglés 8 = 7 + 1):

    | `w` | es: híbrido Hit@5 · MRR · brief archivo | en: híbrido Hit@5 · MRR · brief archivo |
    |---|---|---|
    | 0 | 1/4 · 0,309 · 3/4 | 7/8 · 0,706 · 8/8 |
    | 0,15 | 1/4 · 0,313 · 3/4 | 8/8 · 0,729 · 8/8 |
    | 0,3 | 1/4 · 0,315-0,327 · 4/4 | 8/8 · 0,813 · 8/8 |
    | 0,5 | 1/4 · 0,324 · 4/4 | 8/8 · 0,813 · 8/8 |

    En español el Hit@10 de DevCtxEngine baja 2/3 → 1/3 desde 0,3 (caso 7: 8 → 11) y el brief
    sube por el caso de backend-b: con 4 casos no se concluye nada. Pendiente explícito de TASK-016
    (y en DD-13): encenderla exige no empeorar el corte en español.
  - *MAJOR 3:* un hit sin rank no recibe bonus (antes quedaba al final de la lista con
    `w / (K + pos + 1)`, una degradación sistemática de docs, config, chunks de archivo y
    memorias). La doc ya no dice "decide casi-empates": con `w` = 0,3 un caso de backend-b pasó
    del puesto 7 al 1.
  - *MAJOR 4:* en Rust, `#[cfg(test)]`, `mod tests` y `#[test]` dentro de un archivo de código son
    test (`FileFacts.test_lines`; símbolos `is_test`, llamadas `from_test`); **`EXTRACTOR_VERSION`
    18**, golden regenerado con un fixture nuevo (`rust/inline_tests.rs`), sin tocar el de
    `cache.rs` (el outline de `facts_rust` lo fija).
  - *MINOR 1:* `local_centrality` (MCP `state`): el CLI local y la TUI aplican la centralidad solo
    con un grafo de símbolos vigente, como el MCP. *MINOR 2:* sin centralidad aplicada, el hit
    anclado tiene el score de siempre (sin `raw_score` propio), también con reranker. *MINOR 4:*
    `SearchCfg::centrality` acota a `[0, 1]` (no finito → 0) y `SearchCfg::warnings` lo avisa.
    *MINOR 3:* documentado qué cambia al encenderla (orden, `score`, `raw_score` presente).
    *MINOR 5:* solo se escriben los ranks que cambiaron. *MINOR 6:* lectura por percentil dentro
    de la rama. *MINOR 7:* el test de `sqrt` compara la razón 2:1 (4:1 sin `sqrt`). *MINOR 8:*
    tope de 100 iteraciones; `LinkStats.{rank_iterations, rank_converged}` y la línea del link
    pass. *MINOR 9:* la lista se arma después de los filtros duros. *MINOR 10:*
    `penalty_and_dedup_hold_under_centrality` con ranks reales. NITs: `three` → `mixed`, assert de
    `below_confidence` sumado en el test de llamadas sin decidir.
  - *No hecho (para TASK-011):* NIT de `RankEdge` con dos `String` por fila (enum o interning): el
    costo medido (69-165 ms en backend-a) no lo pide todavía; lo revisa repo_map, que relee la
    adyacencia.
  - **Commits:** e1e11f3 (MAJOR 4), d60f8e0 (MAJOR 3, MINOR 2, 9, 10), c8a65cc (MAJOR 1, MINOR 1,
    4), 9d40ed8 (MINOR 5-8, NIT) y el de esta documentación (MAJOR 2, MINOR 3, docs).
  - *Tests que fallaban antes del fix* (escritos y corridos primero): `rust_inline_tests_are_test_symbols`
    (`checks.helper` no era test), el golden (`subí EXTRACTOR_VERSION`),
    `centrality_breaks_a_fusion_tie_and_keeps_the_retriever_score` (el hit sin rank tenía 0,5048),
    `without_centrality_an_anchored_hit_keeps_its_old_score` (`raw_score` 0,0328 con reranker y
    `w` 0), `centrality_weight_defaults_and_parses` (0,3) y
    `a_centrality_weight_out_of_range_is_bounded_and_warned_about` (NaN), con stubs de
    `centrality`/`warnings`; `local_centrality_follows_the_symbol_graph_rule` (stub que devolvía el
    peso: 0,4 con un extractor viejo); `chunk_ranks_are_percentiles_within_the_branch`;
    `a_pass_over_the_same_graph_writes_no_rank` (escribía 20) y `a_six_node_graph_ranks_as_expected`
    (convergencia, con stubs); `weights_shape_the_ranking` verificado por mutación (sin `sqrt`
    falla). `penalty_and_dedup_hold_under_centrality` pasó de entrada (cobertura, MINOR 10); MINOR 9
    no tiene un test que lo distinga (un filtro que saca un hit no cambia el orden relativo de los
    que quedan, solo el tamaño del bonus).
  - **Gate** antes de cada commit de código (`fmt --check`, `clippy -D warnings` con y sin
    `--features gpu`, `cargo test --workspace --locked`): 1 070, 1 072, 1 074 y 1 076 pasan, 0
    fallan. En 9d40ed8 la primera corrida tuvo dos fallos de `devctx-cli` ajenos al cambio y de
    carrera con la máquina a carga 18 (`connection_refused_recognises_a_real_ureq_error`: el
    puerto cerrado devolvió `Connection reset`; `the_autospawn_mark_is_read_from_the_environment`:
    `/proc/<pid>/environ` leído antes del `exec`); la corrida repetida del workspace entero dio
    1 076 / 0.
- **Segunda revisión (APROBADA salvo un MAJOR), resuelto en un commit encima de 57d150f:**
  - *MAJOR:* `is_cfg_test` buscaba la subcadena `test` y marcaba como test código de producción
    (`any(test, feature = "x")`, `not(feature = "test-utils")`, `feature = "test-utils"`,
    `attest`). Ahora `cfg_requires_test` parsea el predicado y marca solo si `test` es obligatorio
    (solo o dentro de un `all(…)`); bajo `any`/`not`, nunca. **`EXTRACTOR_VERSION` 19**; el
    fixture de tests en línea suma `#[cfg(any(test, feature = "mock"))] pub fn fake_client()`, que
    el golden muestra sin rango de test.
  - *Test que fallaba antes del fix:* `facts_rust::only_a_required_test_cfg_marks_test_code` (una
    forma por caso: `test`, `all(test, x)`, `all` anidado, `any(test, x)`, `not(test)`,
    `feature = "test-utils"`, `not(feature = "test-utils")`, `attest`, `any` dentro de `all`,
    `not(all(test, …))`, más `#[test]`, `#[tokio::test(flavor = …)]`, `#[rstest]`, comentario tras
    el atributo, `#[inline]` y `#![cfg(test)]`): falló en `any(test, feature = "x")` (marcado
    `[(4, 6)]`).
  - *Falsos negativos arreglados (baratos):* `#[tokio::test(…)]`, `#[rstest]`, comentario entre el
    atributo y el ítem, `#![cfg(test)]` interno. *Quedan (del lado seguro, DD-9):* `#[cfg(test)]
    mod tests;` con el cuerpo en otro archivo, tests generados por macros.
  - *Además:* comentario de `with_centrality` corregido (un hit sin rank no recibe bonus pero queda
    relativamente degradado frente a los rankeados; DD-13 y `search.md` EN/ES lo dicen);
    `penalty_and_dedup_hold_under_centrality` exige ahora un reorden (`b` pasa a `a` con `w` = 1
    y no con `w` = 0). Pendientes anotados en TASK-016 (paso 3b): bonus promedio en empates de
    percentil, latencia de la CTE de percentil con `w` > 0, caché de `local_centrality` en la TUI.

