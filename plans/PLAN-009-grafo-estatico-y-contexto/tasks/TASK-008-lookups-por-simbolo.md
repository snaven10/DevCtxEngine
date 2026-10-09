# TASK-008 — Lookups por símbolo: `read_symbol`, anclaje, `get_references`, `external`, sugerencias, vista web

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-005
- **Estado:** `done`

---

## Objetivo

Que todas las lecturas que hoy buscan por nombre o sufijo sobre `vectors.symbol`/`graph_edges`
pasen a la tabla `symbols` y a `edges` por id, **conservando sus contratos JSON** (`anchored`,
`suggestions`, `external`, `called_from`, `next_step`, `resolved_symbols`, `branch_fallback`,
`omitted`), y cerrar los pendientes 1 y 2 de PLAN-008 y el hallazgo B2 de su campo. En ramas sin
reindexar, el camino viejo con el aviso de índice viejo. Implementa DD-10 y la parte de lectura de
DD-19.

## Contexto verificado

- `Store::symbol_definitions` (`crates/devctx-store/src/store.rs:865-930`): exacto → `ends_with`
  `.`/`::` → regex sobre `grouped`. Usado por `do_read_symbol` (`crates/devctx-mcp/src/state.rs:5546-5570`)
  y el CLI (`crates/devctx-cli/src/main.rs:2191`).
- `Store::symbol_matches` (`store.rs:995-1032`): el anclaje de `anchor_identifiers`
  (`crates/devctx-search/src/lib.rs:664-704`), con `rank_definitions` (l.715-728), `ANCHOR_MAX` 3
  (l.360), `ANCHOR_FETCH` 48 (l.365), `ANCHOR_TOKENS` 3 (l.369).
- `is_anchored_definition` (`state.rs:675-682`) marca `anchored` por sufijo.
- `Store::symbol_suggestions` (`store.rs:940-987`), `not_found_hints` (`state.rs:5577-5620`),
  `Store::external_call_sites` (`crates/devctx-store/src/graph.rs:227-258`, fallback por último
  segmento en l.254-257), `Store::resolve_symbol` (`graph.rs:183-210`), `Store::find_references`
  (`graph.rs:304-332`), `do_references` (`state.rs:6091-6125`).
- `split_file_symbol` (`state.rs:5622-5631`): acepta cualquier "extensión" alfanumérica de ≤ 5
  caracteres → `Foo.Bar::baz` se lee como archivo `Foo.Bar` (pendiente 1 de PLAN-008).
- Vista web: `do_graph` (`state.rs:1873+`) usa `graph_edges` y `graph_defined_symbols`
  (`graph.rs:153-160`, "definido" = aparece como source).
- Selección de grupo: `Srv::defines` hace `GET /symbol/<name>?limit=1` (`state.rs:5122-5128`), que
  termina en `symbol_definitions`.
- Hallazgos de campo de PLAN-008 TASK-016: B2 (`external` falla con calificados), B3 (sugerencias
  ruidosas para externos), B4 (`memories_by_symbol("links.rs::memories_by_symbol")` cae a
  `inference`).
- TUI: `crates/devctx-tui/src/lib.rs:230` llama `impact_analysis` (no cambia de firma acá).

## Archivos

- **Modificar:** `crates/devctx-store/src/store.rs` (`symbol_definitions`, `symbol_matches`,
  `symbol_suggestions` sobre `symbols` + chunks por rango, DD-4), `crates/devctx-store/src/graph.rs`
  (`resolve_symbol`, `find_references`, `external_call_sites`, `graph_defined_symbols`, `graph_edges`
  de la vista web sobre `edges`; camino viejo si la rama no tiene filas en `symbols`),
  `crates/devctx-mcp/src/state.rs` (`is_anchored_definition` por id, `not_found_hints`,
  `split_file_symbol`, `do_references` con `confidence`, `do_graph`), `crates/devctx-search/src/lib.rs`
  (los hits anclados llevan su id), `crates/devctx-core/src/types.rs` si `VectorMetadata` necesita
  transportar `symbol_id` en memoria (no en el schema de `vectors`).

## Pasos

- [x] **Paso 1 — tests que fallan.** (a) `split_file_symbol("Foo.Bar::baz")` → no es archivo; con
      `links.rs::memories_by_symbol` sí (pendiente 1, B4); (b) `Foo::new` inexistente con un `new`
      hoja definido en el repo → **no** `external` (pendiente 2); (c) `serde_json::from_str` y
      `Panache.withTransaction` → `external: true` con `called_from` (B2); (d) `with_context`
      externo → sin sugerencias de near-miss (B3, regla ya existente que debe sobrevivir); (e)
      Python: `get_references("helper")` con dos llamadas desde el mismo método → dos referencias;
      (f) `read_symbol` de un arrow-const TS → definición.
- [x] **Paso 2 — lecturas nuevas** por `symbols`/`edges` con las mismas firmas públicas y el mismo
      orden de salida (archivo, línea). Chunks del símbolo por rango (DD-4).
- [x] **Paso 3 — contratos.** Los tests existentes de anclaje, sugerencias, `external`,
      `resolved_symbols` y `branch_fallback` se corren **sin modificar**; campos nuevos aditivos:
      `sym` (16 hex), `confidence` en referencias, `kind` del símbolo.
- [x] **Paso 4 — camino viejo.** Si la rama no tiene filas en `symbols` (índice v1), cada lectura usa
      la implementación actual y la respuesta ya trae `extractor_stale`/warning (mecanismo de
      PLAN-008; test `an_index_without_extractor_record_warns_in_every_graph_answer` sigue verde).
- [x] **Paso 5 — vista web** (`do_graph`) sobre `edges`, con `external`/`from_test` reales en vez de
      la heurística "nunca es source".
- [x] **Paso 6 — medir** con el arnés: anclaje (definición primero, 8 consultas de PLAN-008),
      `read_symbol`/`get_references` en los casos de campo, latencia de `read_symbol` (p95).

## Criterios de aceptación

- [x] Tests (a)-(f) verdes; tests de contrato de PLAN-008 verdes sin tocarlos.
- [x] Anclaje: definición primero en ≥ 7/8 de las consultas de PLAN-008 TASK-016 (igual o mejor).
- [x] `read_symbol("tokenInterceptor")` en frontend reindexado → definición en
      el archivo del interceptor de la librería de auth (o en fixture equivalente si el
      reindex de FrontEnd no está aprobado todavía; se dice).
- [x] Rama v1 sin reindexar: respuestas iguales a 0.9.0 + aviso.

## Riesgos

- El orden de `symbol_matches` decide qué se ancla: mantener `rank_definitions` como único juez.
- `grouped` deja de ser necesario para encontrar funciones chicas (DD-4); no borrar la tercera
  pasada del camino viejo.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`.
- **Resumen:** en una rama con grafo de símbolos vigente (sin `extractor_stale` y con filas en
  `symbols`), `read_symbol`, `get_references`, el anclaje de `search`, `external`/`suggestions` y la
  vista web leen la tabla `symbols` y las aristas resueltas **por id**, siempre a través de
  `live_edges` (las llamadas descartadas no llegan a ninguna respuesta). Los contratos JSON solo
  crecen: `sym` (16 hex), `kind`, `qualified`, `confidence`, `via`, `below_confidence`. Una rama vieja
  (otro extractor, grafo fuera de paso) o sin filas en `symbols` contesta con los lectores de 0.9.0
  sobre `graph_edges`/`vectors` y lo dice en `warning`. Cierra los pendientes 1 y 2 de PLAN-008 y
  B2/B3/B4 de su campo.
- **Diseño:**
  - *Store* (`crates/devctx-store/src/lookup.rs`, nuevo): `lookup_symbols` resuelve un nombre pelado
    por `name`; uno calificado (`.` o `::`, que se pliegan a `.`) por `qualified` exacto, por sufijo
    (`Inner.m` de `Outer.Inner.m`) o con el paquete adelante (`com.x.Card.charge`), y si nada de eso,
    por una ruta más larga que termina en un `Tipo.miembro` del repo (`devctx_store::Store::open`);
    `archivo::nombre` restringe al archivo. Orden `(archivo, línea)`, con campos e `impl` al final.
    `symbol_code_chunks` liga un símbolo a sus chunks por rango (DD-4): el chunk `class` de un
    contenedor, los `function`/`block` con su nombre (una función grande son varios bloques) o el
    `grouped` que lo lista o lleva su encabezado `# … > nombre`; un campo o una constante sin chunk
    contestan con su firma. `lookup_external` cuenta solo filas `calls`/`instantiates` con
    `external` del link pass cuyo destino es el nombre o termina en `.nombre` (`::` y `.` igual), y
    cae al último segmento solo si ninguna definición de la rama tiene ese nombre (pendiente 2).
    `lookup_references` lista una fila por ocurrencia de las relaciones pedidas (por defecto
    `calls`/`instantiates`, revisión) hacia los ids, o los sitios externos de un nombre sin definición, más las filas sin decidir del mismo
    nombre (para marcarlas). `lookup_graph_view` arma la vista desde `live_edges`.
  - *Search*: `search_anchored(.., AnchorLookup::{Names, Symbols})` devuelve además los chunks
    anclados con su id; sobre el grafo compite **un chunk por definición** por un lugar fijado
    (`rank_definitions` sigue siendo el único juez, `ANCHOR_MAX` 3 y `ANCHOR_TOKENS` 3 sin cambio);
    un identificador que el grafo no define (texto crudo, Kotlin) se sigue buscando por nombre.
    `search_ranked` = `Names`, igual que 0.9.0.
  - *MCP* (`crates/devctx-mcp/src/state/lookup.rs`, nuevo, hijo de `state`): `Reader::of(choice,
    store)` decide el camino; `read_symbol`, `references` (con `MinConfidence`), `graph_view`.
    `do_read_symbol`, `do_references_with`, `search_items` y `do_graph` lo usan; el CLI local
    (`devctx symbol` sin serve) también, con `branch_fallback` y aviso de índice viejo.
  - *Confianza visible* (requisito 4): `get_references` muestra por defecto `high` y `medium`, cada
    referencia con su `confidence` (la `medium` queda marcada por el campo); `low` y las filas sin
    decidir solo con **`min_confidence: "low"`** (MCP, `GET /references/<s>?min_confidence=low`),
    marcadas `undecided: true`; lo que el default dejó afuera se cuenta en
    `below_confidence: {count, min_confidence, hint}`, nunca vacío en silencio. Documentado en la
    descripción de la tool y en `docs/03-core-concepts/symbol-graph.md` (EN) /
    `docs/es/03-conceptos-fundamentales/grafo-de-simbolos.md` (ES).
- **Números** (sandbox `mktemp -d -p /var/tmp`; índices hechos con el pipeline real y un
  embedder sin modelo —parse, `symbols`/`edges` y link pass reales, vectores constantes— por un arnés
  `#[ignore]` nuevo, `state::lookup::tests::lookup_bench`, binario `--release`; "camino viejo" = el
  mismo índice sellado por otro extractor, o sea los lectores de 0.9.0 sobre las mismas filas; 30
  corridas por caso, serve caliente equivalente (cada llamada abre el store como el serve); máquina
  con carga 5-12; snapshots: DevCtxEngine en 3b2dfd4 (438 archivos), el **subconjunto de
  sublibrerías de frontend de TASK-006** (163 archivos, sin `.devctx`, verificado con `find`; el
  frontend entero no se indexó: requiere OK del usuario) y legacy-migration):

  | Caso de §6 | línea base (TASK-001 / 0.9.0) | camino viejo (mismo índice) | **TASK-008** |
  |---|---|---|---|
  | `read_symbol` del interceptor arrow-const de la librería de auth (en este snapshot se llama distinto que `tokenInterceptor`, que es su nombre en un commit posterior; mismo archivo, misma forma) | `[]` y `external: true` (falso) | definición (vía el chunk `grouped`, sin `sym`) | **definición en el archivo del interceptor**, con `sym`/`kind`/`qualified` |
  | `get_references` de un método del servicio de auth (llamado desde el `inject()` del interceptor y desde guards) | — | 5 referencias (pares colapsados) | **7**, todas `high`; las 3 llamadas desde el interceptor salen como 3 |
  | Ocurrencias repetidas (Python, legacy-migration): helper-a llamado 5 y 3 veces desde dos funciones; helper-b 4 veces desde una; helper-c 2 veces desde una | 1 por par (0.9.0) | 2 / 1 / 1 | **8 / 4 / 2** |
  | Ídem en Rust (DevCtxEngine, `Store.graph_out_of_step` llamado 2 veces desde un método) | 1 | 1 | **2** |
  | `serde_json::from_str` / `tokio::spawn` (DevCtxEngine) | `external` + `called_from` 60 / sin llamadas | `external`, 63 / 5 | **`external`, 72 / 5** |
  | `with_context` | `external` sin near-miss | `external`, 26, sin sugerencias | **`external`, 30, sin sugerencias** |
  | `Foo::new` inexistente | `{definitions: [], suggestions: []}` | ídem | **no externo, `suggestions`: `App.new`, `Cached.new`, …** |
  | `Foo.Bar::baz` / `links.rs::memories_by_symbol` | se leía como archivo `Foo.Bar` / sin definición | no es archivo / **sin definición** | no es archivo / **la definición de `links.rs`** |
  | Anclaje, definición primero (8 consultas de identificador de DevCtxEngine) | 7/8 (PLAN-008 TASK-016) | 6/8 | **7/8** |

  Anclaje: la lista de 8 consultas de PLAN-008 TASK-016 no quedó escrita; se reconstruyeron 8 de
  DevCtxEngine con su archivo esperado **antes de medir** (sandbox, `cases/anchor-devctx.*`):
  `memories_by_symbol` (esperado `links.rs`), `Store::open`, `extractor_stale`, `RepoIndex`,
  `rank_definitions`, `do_read_symbol`, `LangResolver`, `graph_out_of_step`. El fallo es el mismo de
  TASK-016: `memories_by_symbol` tiene cuatro definiciones y `rank_definitions` (orden por ruta) fija
  primero el handler de `devctx-api`. El camino viejo además no ancla `Store::open` (no hay
  `vectors.symbol` que termine en `.open`; el grafo sí tiene `Store.open`), y fija dos chunks de la
  misma definición donde el nuevo fija una por definición. Con vectores constantes el anclaje es lo
  único que se mide (los recuperadores no ordenan nada).

  Latencia de `read_symbol` (p50 / p95 en ms, 12 casos de DevCtxEngine y 3 de frontend): **nuevo
  10,7-35,6 / 13,2-42,0** (p95 máximo 42 ms, en un nombre que no existe: `Foo.Bar::baz`, que corre
  búsqueda, externos y sugerencias); camino viejo 7,7-45,7 / 9,3-53,0. Con definición, el nuevo cuesta
  ~5-10 ms más que el viejo (una consulta a `symbols` más una de chunks por definición); en un fallo
  es más barato (no hay regex sobre `grouped` ni `ends_with` sobre `vectors`). `get_references` p95:
  nuevo 14,6-31,2, viejo 6,8-19,7.
  **No medido en esta task:** relevancia del arnés de TASK-001 (Hit@5/MRR/brief: necesita el modelo
  real y reindexar; las lecturas por símbolo no usan vectores, el anclaje se midió arriba), costo de
  indexado, tamaño y RAM (esta task no cambia la escritura).
- **Hallazgos de §2:** H9 confirmado y cerrado (pendientes 1 y 2). Nuevo: el camino viejo, sobre un
  índice 0.10, sigue listando la llamada descartada (`graph_edges` la conserva): el test
  `discarded_calls_reach_no_answer` lo fija.
- **Archivos tocados:** `crates/devctx-store/src/{lookup.rs (nuevo), lib.rs, store.rs, symbols.rs}`,
  `crates/devctx-search/src/lib.rs`, `crates/devctx-index/src/lib.rs` (reexporta
  `lang_for_extension`/`raw_text_language`), `crates/devctx-mcp/src/{state.rs, state/lookup.rs
  (nuevo), lib.rs, backend.rs}`, `crates/devctx-api/src/lib.rs`, `crates/devctx-cli/src/main.rs`,
  `CHANGELOG.md`, docs EN/ES (`symbol-graph`, `mcp-integration`, `search`), el design (DD-8, DD-10,
  DD-19) y el master. Públicos nuevos: `Store::{has_symbol_graph, lookup_symbols,
  symbol_code_chunks, lookup_definitions, lookup_anchors, lookup_suggestions, lookup_same_name,
  defines_name, lookup_external, lookup_references, lookup_graph_view, qualified_names}`,
  `devctx_store::{SymbolDefinition, SymbolReference, ExternalSites, ViewEdge, MinConfidence,
  sym_hex}`, `devctx_search::{search_anchored, AnchorLookup, Anchored}`,
  `devctx_mcp::state::{do_references_with, split_file_symbol (ahora pub)}`,
  `Backend::references(symbol, min_confidence)`. Sin cambios de schema, de `languages/*.json` ni de
  config.
- **Tests** (`TMPDIR=/var/tmp cargo test --workspace --locked`, verde; `fmt --check`; `clippy
  --workspace --all-targets --locked -D warnings`, también con `--features gpu`): en
  `state::lookup::tests`, sobre un repo de cinco lenguajes indexado de verdad con `do_index --full`:
  `a_dotted_type_before_double_colon_is_not_a_file` (a) y `symbol_lookups_over_the_symbol_graph`
  (b-f) **fallaban contra el código previo** (corridos antes de cablear: `Foo.Bar::baz` daba archivo;
  `Foo::new` daba `external: true`; dos `helper()` desde `Runner.run` daban una referencia; sin
  `sym`; (c), (d) y (f) ya pasaban por el camino viejo y se fijan para el nuevo).
  `discarded_calls_reach_no_answer` (requisito 1), `references_show_high_and_medium_unless_asked_for_low`
  (requisito 4), `a_stale_index_takes_the_old_path_and_says_so` (requisito 3; también una rama vigente
  sin filas en `symbols`), `the_graph_view_marks_externals_and_tests_from_the_link_pass` (paso 5) y
  `anchoring_and_branch_fallback_over_the_symbol_graph` se escribieron con el fix: cada uno falla al
  mutar su regla (leer `edges` en vez de `live_edges`; sin filtro de confianza; sin mirar
  `extractor_stale`; la vista vieja; anclaje por nombre), y la guarda del último segmento de
  `lookup_external` falla en (b) si se quita (con una `PathBuf::new` externa en el fixture). En
  `devctx-store`: `min_confidence_admits_by_rank`, `sym_is_sixteen_hex_digits`,
  `grouped_names_drop_the_overflow_count`. **Sin tocar ningún assert existente**: los de
  `mcp_tools`, anclaje (`anchored_marks_only_the_identifiers_anchoring_looked_up` y los de
  `devctx-search`), sugerencias, `external` (`read_symbol_not_found_suggests_and_marks_external`,
  `qualified_externals_are_found_and_get_no_noisy_suggestions`,
  `a_grouped_small_function_is_found_not_external`), `branch_fallback`, `resolved_symbols` y
  `an_index_without_extractor_record_warns_in_every_graph_answer` siguen verdes: corren por el camino
  viejo (sin filas en `symbols`) y sus respuestas no cambiaron.
- **Contrato JSON** (todo aditivo):
  - `read_symbol`: cada definición agrega `sym`, `kind`, `qualified`; `warning` cuando contesta el
    camino viejo en una rama vigente sin filas en `symbols`.
  - `get_references`: cada referencia agrega `confidence`, `via` y, si aplica, `sym`, `external`,
    `test`, `undecided`; el objeto agrega `below_confidence`; parámetro nuevo `min_confidence`.
  - `search` (keyword/hybrid): un resultado `anchored` agrega `sym`.
  - vista web (`/graph`): aristas con `confidence` y, si aplica, `external`/`test`; nodo `external`
    por la marca del link pass.
- **Desviaciones:**
  1. `get_references` por defecto `min_confidence = medium`, no `low` como decía DD-8: lo pide la
     revisora (requisito 4); lo de abajo se cuenta en `below_confidence`. DD-8 actualizado.
  2. Las lecturas nuevas son **funciones nuevas** (`lookup_*`) y no un reemplazo en sitio de
     `symbol_definitions`/`resolve_symbol`/…: esas siguen siendo el camino viejo y las usa
     `impact_analysis` (sobre `graph_edges`) hasta TASK-009. DD-10 actualizado.
  3. ~~`get_references` lista todas las relaciones menos `contains`~~ — **no aceptada en la
     revisión**: por defecto `calls` e `instantiates` (ver abajo), las demás con `kinds`.
  4. `read_symbol` sigue contestando un entry por chunk (como 0.9.0), con el `limit` sobre
     definiciones; campos e `impl` van después del resto (no solo `(archivo, línea)`).
  5. `split_file_symbol` acepta `archivo::nombre` con `/` o con una extensión de lenguaje conocida
     (parseable o de texto crudo, en minúscula); no consulta si el archivo existe en el índice
     (DD-10 lo admitía como alternativa): alcanza para B4 y el pendiente 1.
  6. Una rama vigente **sin filas en `symbols`** (repo sin lenguajes parseables) también avisa
     (`warning` propio), además del caso `extractor_stale` de DD-19. DD-19 actualizado.
  7. La medición de §6 usa un arnés propio (`lookup_bench`, `#[ignore]`) con embedder sin modelo, no
     `scripts/graph-eval/run.sh` (que necesita el modelo real); la lista de 8 consultas de anclaje se
     reconstruyó (la original no quedó escrita).
- **Riesgos abiertos / siguiente:** (1) `read_symbol` con definición cuesta ~5-10 ms más que 0.9.0
  (p95 ≤ 42 ms medido); si molesta, `symbol_code_chunks` puede pedir los chunks de todas las
  definiciones en una consulta. (2) `rank_definitions` por ruta sigue fijando el handler de
  `devctx-api` antes que la implementación de `memories_by_symbol` (el 1/8 del anclaje); un juez por
  `rank` (PageRank, TASK-010) es el candidato. (3) No medido sobre frontend entero ni backend-a/b
  (sin OK de reindex), ni macOS/Windows (solo CI). (4) `impact_analysis` y la TUI siguen leyendo
  `graph_edges` (TASK-009). (5) El sandbox de medición es un `mktemp -d` local, fuera del repo.
- **Revisión (REQUEST CHANGES, sin BLOCKER), resuelta en commits encima de 6199ea4:**
  - *M1 (contrato vacuo en el camino nuevo):* los asserts de
    `read_symbol_not_found_suggests_and_marks_external`,
    `qualified_externals_are_found_and_get_no_noisy_suggestions` y
    `a_grouped_small_function_is_found_not_external` pasaron, **sin cambiar una línea**, a
    helpers `check_*` que el test original sigue llamando sobre su índice sembrado (camino viejo) y
    que tres tests nuevos llaman sobre un índice real (`*_over_the_symbol_graph`, que además
    verifican que el lector es el grafo de símbolos): clase y método Java, dos aserciones externas
    con import estático, un typo; Rust con `serde_json`, `tokio` y `anyhow` declarados más Java
    con Panache; Python con funciones chicas agrupadas y un `get_a` importado de un paquete
    declarado. Un externo **no lleva `suggestions`**, como 0.9.0 (antes del fix
    `read_symbol("from_str")` daba `["serde_json::from_str", …]`; el test indexado lo exige para
    `from_str`, `spawn` y `serde_json::from_str`). El presupuesto de `get_references` es el mismo
    constructor en los dos caminos: sin paginado (no hay `next_offset`), una referencia que no
    entra en su parte se queda con `file`/`line` y su `source` se corta marcado, así que `omitted`
    no aparece — como en 0.9.0 (`references_report_what_the_budget_cut_on_both_paths`).
  - *M2:* `search` en `keyword`/`hybrid` sobre una rama vieja (o sin filas, o con error de lectura)
    trae `warning`: su anclaje fue por nombre (`a_search_anchored_by_name_warns`).
  - *M3:* el anclaje y `read_symbol` leen el código de todas las definiciones en **una** consulta
    (`Store::code_chunks_of`, `symbols` ⋈ `vectors` por archivo y rango; antes una por definición,
    hasta ~48 por identificador) y el search elige la rama una sola vez. Medido en la copia de
    **DevCtxEngine** del sandbox (438 archivos; backend-a no está en el sandbox), embedder sin
    modelo, binario `--release`, 40 corridas, dos rondas intercaladas antes/después/camino viejo,
    carga 3-9, 6 consultas (5 de identificador y 1 de comportamiento) por tool:

    | | antes (6199ea4) p50 / p95 máx | después p50 / p95 máx | camino viejo (por nombre) |
    |---|---|---|---|
    | `search --hybrid` (media de p50) | 27,0 y 25,2 / 43,2 ms | **23,1 y 22,4 / 32,3 ms** | 17,7 y 19,7 / 34,6 ms |
    | `build_context` 4096 tokens | 27,6 y 26,8 / 43,7 ms | **26,1 y 25,9 / 34,9 ms** | 20,7 y 20,8 / 32,1 ms |

    Sobre el camino viejo el nuevo cuesta +3 a +5 ms de p50, muy dentro de DD-21 (≤ +150 ms). Una
    primera medición no intercalada daba 48 → 24 ms, pero la consulta de comportamiento (que no
    ancla) también bajaba, así que era la carga de la máquina; por eso las rondas intercaladas.
  - *Desviación 3 no aceptada:* `get_references` lista por defecto `calls` e `instantiates` (la
    semántica de 0.9.0 era "llamadas"; `new Foo()` llama al constructor de `Foo`). Las demás
    relaciones van con el parámetro opt-in **`kinds`** (`references`, `imports`, `inherits`,
    `implements` o `all`; MCP y `GET /references?kinds=`), documentado en la tool y en las docs EN/ES
    (`references_default_to_calls_and_take_kinds`). DD-10 y CHANGELOG actualizados.
  - *m1:* la descartada del test se marca además `external` (como haría un lector de `edges`) y
    sigue sin llegar a nada. `memories_by_symbol("Foo.Bar::baz")` se prueba hasta la parte local
    de la tool (`symbol_query` + `text_fallback_local`: el miembro `baz` sin archivo, `inference`);
    la tool entera consulta el daemon central, que un test no debe levantar.
  - *m2:* un `archivo::nombre` sin resultado nunca es externo (`a_file_qualified_miss_is_never_external`).
  - *m3:* las filas sin decidir se cuentan con un `count(*)` aparte
    (`Store::count_undecided_references`) y solo se listan con `low`; el conteo y el listado se
    restringen al `dst_name` escrito igual que el nombre pedido (un `Left.flush` no cuenta un
    `flush` pelado).
  - *m4:* un error al leer `symbols` es `Reader::Legacy(Legacy::Error)` con su propio `warning`
    ("read error, not a reason to reindex"), no "sin filas, reindexá"
    (`a_failed_symbol_graph_read_is_not_a_reindex_hint`).
  - *m5:* `devctx symbol` sin serve usa `state::read_symbol_at`: la misma respuesta que la tool
    (aviso de grafo ausente, `branch_fallback`, sugerencias y externos en un fallo)
    (`the_local_cli_answer_is_the_tool_answer`).
  - *m6 (47b3f3c):* en un `impl Trait for T` con `T` de afuera, `self.m()` se resuelve por el
    trait del `impl` como supertipo (`RepoIndex::impl_trait_member`; también un método por
    defecto), `high`, `inherited`; nunca por nombre. `self_in_an_impl_for_an_outside_type_is_no_guess`
    y su par de Go siguen verdes. **`LINK_VERSION` 18.**
  - *NIT:* la vista omite `confidence` cuando es null; `Backend::Remote` agrega un `warning` si
    pidió `min_confidence`/`kinds` y la respuesta no trae `confidence` (servidor o índice
    anterior); fuera la ruta del sandbox de este Resultado; `@scope/pkg::fn` no es archivo.
  - *Tests que fallaban antes del fix:* `self_in_an_impl_of_a_repo_trait_reaches_the_trait` (daba
    `name_only` `low`) se escribió y corrió primero. Los demás se escribieron junto con el fix y se
    verificaron revirtiéndolo (mutación con `touch`): `qualified_externals_over_the_symbol_graph`
    (externo con las formas calificadas como sugerencias), `references_default_to_calls_and_take_kinds`
    (default de todas las relaciones), `a_file_qualified_miss_is_never_external`,
    `a_failed_symbol_graph_read_is_not_a_reindex_hint`, `a_search_anchored_by_name_warns`,
    `batched_code_chunks_equal_the_per_symbol_lookup` (filas asignadas al primer símbolo; join
    sin el archivo), `discarded_calls_reach_no_answer` (externos leídos de `edges`),
    `a_dotted_type_before_double_colon_is_not_a_file` (`@scope`),
    `references_show_high_and_medium_unless_asked_for_low` (conteo por último segmento) y
    `the_local_cli_answer_is_the_tool_answer`. Los helpers `check_*` sobre el índice real pasaron
    a la primera: el camino nuevo ya cumplía esos asserts salvo las sugerencias de un externo, que
    los asserts de PLAN-008 no miraban para `from_str`.
- **Segunda revisión (REQUEST CHANGES acotado), resuelta en commits encima de 6f05206:**
  - *N1:* `kinds` **se suma** a `calls`/`instantiates` (`all` sigue siendo todo); antes los
    reemplazaba, en contra de lo que decían la tool y las docs. `references_default_to_calls_and_take_kinds`
    exige que con `kinds: ["references"]` también vengan las llamadas (falla si `parse_kinds`
    arranca vacío).
  - *N2 (m6 daba `high` equivocado):* en un `impl Trait for T` con `T` de afuera, primero el método
    del propio `impl` (un override de un default: `same_file`, `high`, como antes de 47b3f3c); si
    no, el trait como supertipo con **cap a `medium`** (un método inherente del tipo de afuera
    ganaría y no se puede conocer). Fixtures nuevas: un `len` por defecto en un trait para
    `PathBuf` y un override en `impl Polite for u32`. `self_in_an_impl_of_a_repo_trait_reaches_the_trait`
    ahora espera `medium` para el trait y `same_file` para el override (fallaba: daba `high`, y el
    override se resolvía a la declaración del trait, verificado por mutación).
    `self_in_an_impl_for_an_outside_type_is_no_guess` y su par de Go siguen verdes.
    **`LINK_VERSION` 19.**
  - *n1:* el fixture de `batched_code_chunks_equal_the_per_symbol_lookup` suma un archivo con dos
    `new` de chunk propio; quitar `v.end_line >= s.start_line` ahora lo hace fallar.
  - *n2:* con `archivo::nombre` las filas sin decidir no se cuentan ni se listan (tienen un nombre,
    no un archivo); verificado en `references_show_high_and_medium_unless_asked_for_low`.
  - *n3:* `code_chunks_of` deriva el índice de `s.id` del número de columnas de `COLS`.
  - Gate completo antes de cada commit de código (`fmt --check`, `clippy -D warnings` con y sin
    `--features gpu`, `cargo test --workspace --locked`): 1026 pasan, 0 fallan, en los dos.

