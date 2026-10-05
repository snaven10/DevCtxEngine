# TASK-012 — Dedup de chunks y anclaje por identificador exacto

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-006
- **Estado:** `done`

---

## Objetivo

Que `search` no devuelva el mismo fragmento varias veces, y que una consulta que es (o contiene) un
identificador exacto ponga primero su definición (PLAN-008 B6; evidencia: GrepRAG, +7-15.6 % EM con
boost por identificador y dedup estructural).

## Contexto verificado

- `crates/devctx-search/src/lib.rs:121-122` — la RRF deduplica solo por id de punto.
- `crates/devctx-mcp/src/state.rs:485-506` — `search_branch` puede caer a "sin filtro" y traer la
  misma ruta de varias ramas (hipótesis de los duplicados; TASK-006 cambia la elección de rama).
- Campo: "how are memories linked to symbols" en DevCtxEngine → mismo chunk 3-5×; keyword
  `do_memories_by_symbol` no trae la definición (`state.rs:3147`).
- `crates/devctx-store/src/store.rs:428` — `keyword_search` (BM25); `store.rs:658` —
  `symbol_definitions` (exacto, luego `%.name`/`%::name`).

## Archivos

- **Modificar:** `crates/devctx-search/src/lib.rs`
- **Modificar:** `crates/devctx-mcp/src/state.rs` (si el anclaje usa `symbol_definitions`)

## Pasos

- [x] **Paso 1 — Repro.** Reproducir los duplicados y anotar si difieren en rama, en id o en rango
      (solapados método/clase). Confirmar o corregir la hipótesis en el Resultado.
- [x] **Paso 2 — Dedup.** Tras la fusión: misma `(file, start_line, end_line)` → uno (el de la rama
      elegida); rango contenido en otro ya devuelto del mismo archivo → se descarta el contenido si el
      contenedor ya está, o se reemplaza el contenedor por el más chico si el chico rankea más alto.
- [x] **Paso 3 — Anclaje.** Si la consulta tiene tokens con forma de identificador (snake/camel, o
      `Clase.metodo`), en modo `keyword`/`hybrid` buscar `symbol_definitions` exactas y ponerlas al
      frente (boost fijo en la RRF), marcadas `anchored: true`.

## Criterios de aceptación

- [x] Test: store con el mismo chunk en dos ramas y un método contenido en su clase → 0 duplicados
      `(file,start,end)` y sin el par contenedor/contenido.
- [x] Test: `search("do_memories_by_symbol", mode: keyword)` → primer resultado es la definición.
- [ ] Repro de campo re-medido en el Resultado. → sin números todavía; se mide en TASK-016 (verificación de campo).

## Riesgos

Descartar contenidos puede esconder el método exacto detrás de la clase: la regla prefiere el
fragmento más chico cuando rankea más alto.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después:** sin repro manual de campo (no se tocó el índice ni el serve en marcha); cubierto por
   `the_same_chunk_is_returned_once`, `an_identifier_query_puts_its_definition_first_in_hybrid` y `..._in_keyword`,
   que fallan con el pipeline anterior (comprobado desactivando dedup y anclaje) y pasan ahora.
3. **Causa raíz:** confirmada a medias. La RRF solo deduplicaba por id de punto (`devctx-search/src/lib.rs`), así que el mismo
   rango en otra rama (id distinto) o un método junto a su clase se repetían. No se midió en el repo real cuántos duplicados
   venían de rama vs. rango. El keyword no anclaba porque BM25 premia a quien más menciona el identificador, no a quien lo define.
4. **Archivos:** `crates/devctx-search/src/lib.rs` (nuevos `pub fn dedup_hits(Vec<SearchResult>) -> Vec<SearchResult>`,
   `pub fn identifier_tokens(&str) -> Vec<String>`; `search` deduplica antes de cortar el pool y ancla después),
   `crates/devctx-store/src/store.rs` (`Store::symbol_matches(&SearchFilter, &str, usize)`: exacto, luego `%.name`/`%::name`,
   respeta el filtro, excluye filas de memoria), `crates/devctx-mcp/src/state.rs` (`do_search` marca `anchored`).
   Regla de dedup: se conserva el mejor rankeado entre mismo id, mismo `(repo,file,start,end)` (ramas) y rango contenido en otro
   ya conservado del mismo archivo (en cualquier dirección: gana el que rankeaba más alto). Filas sin archivo (memorias) solo por id.
   Anclaje: en `keyword`/`hybrid`, tokens con forma de identificador (snake, camel/Pascal con mayúscula interna, `A.b`, `a::b`)
   -> hasta 3 definiciones por token al frente con el mejor score, dedup de nuevo y corte a `limit`.
5. **Tests:** `cargo test -p devctx-search` (11 ok) y `cargo test --workspace` (todo verde); nuevos: `the_same_chunk_is_returned_once`,
   `dedup_prefers_the_higher_ranked_of_container_and_contained`, `identifier_tokens_only_take_identifier_shapes`,
   `an_identifier_query_puts_its_definition_first_in_hybrid`, `..._in_keyword`, `a_plain_word_query_is_not_anchored`.
   Gate: fmt --check, clippy --workspace --all-targets (también `--features gpu`) sin warnings.
6. **Contrato JSON:** cada resultado de `search` en keyword/hybrid puede traer `anchored: true`. Sin cambios en `omitted`: el dedup
   ocurre antes del corte por `limit`, así que lo descartado por duplicado no se informa como omitido.
7. **No verificado:** la repro de campo (query "how are memories linked to symbols" y `do_memories_by_symbol` sobre este repo);
   el anclaje con un índice real multi-rama; `anchored` por el camino CLI `--format json` (solo lo marca `do_search` del MCP).
   El test keyword se salta si la extensión FTS no está disponible.
8. **Números:** sin medición de campo.

### Fixup E (review)

- **B1 (bloqueante):** la contención del dedup se comía las funciones detrás del chunk `file` (1..N, solo resumen)
  y los métodos detrás del `class`. Ahora `dedup_hits` (devctx-search/src/lib.rs) solo aplica contención entre
  chunks de *contenido*: `file`/`class`/`doc` (`is_summary`) nunca contienen ni son contenidos; el mismo rango
  solo se colapsa si además coincide `chunk_level`. `identifier_tokens` descarta nombres de archivo
  (`state.rs`, `README.md`: lista `FILE_EXTENSIONS`), versiones (`v0.8.4`) y abreviaturas (`e.g`, `i.e`), así
  "state.rs plan_status_list" ancla la definición y no el resumen del archivo.
  Tests (fallan en 409c0d4): `a_summary_chunk_never_swallows_the_code_inside_it`,
  `naming_the_file_and_the_function_keeps_the_definition_first`, `file_names_and_versions_are_not_identifiers`.
- **I1:** anclaje acotado: máx. 3 tokens (`ANCHOR_TOKENS`), fijados ≤ `max(1, limit/2)`; `Store::symbol_matches`
  y `Store::symbol_definitions` usan `ends_with(symbol, '.' || ?)` / `'::' || ?` en vez de `LIKE '%.name'`
  (`_` era comodín). Tests: `anchoring_is_bounded_in_tokens_and_in_share_of_the_answer`,
  `the_suffix_match_is_literal` (fallan en el padre). Sin índice sobre `vectors.symbol` (con el tope son ≤ 6 scans).
- **I2:** pool = `max(2×limit, 20, pool del reranker)`; con filtro duro además `max(400, 4×limit)`.
  Test `dedup_does_not_shorten_the_answer_below_the_limit` (falla en el padre: devolvía 10 de 20).
- **I3:** criterio de repro de campo destildado (→ TASK-016). Política FTS decidida: los tests *de keyword*
  (`keyword_mode_needs_no_embedder`, `..._first_in_keyword`, store `keyword_search_ranks_by_bm25`) pasan por
  `require_fts`, que **falla** si la extensión FTS no carga; solo con `DEVCTX_TEST_ALLOW_NO_FTS=1` se saltan, y
  avisan `SKIPPED ...`. Los tests híbridos siguen degradando a vector-only (es el comportamiento de producción).
- Gate: fmt --check; clippy --workspace --all-targets (con y sin `--features gpu`) 0 warnings;
  `TMPDIR=/var/tmp DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache cargo test --workspace` verde.

### Fixup F (review)

- **I-2 (anclaje con LIMIT 3 antes del filtro y orden por ruta):** `anchor_identifiers` pide `ANCHOR_FETCH = 48`
  definiciones por identificador y `rank_definitions` las ordena en Rust (Code primero, luego ruta y línea),
  aplica `keeps` y recién entonces corta a `ANCHOR_MAX = 3`. Antes (A) con `include_tests=false` tres copias de test
  que ordenaban antes ocupaban los 3 slots y el filtro las tiraba sin anclar nada; (B) `__mocks__/foo.service.ts`
  ordenaba antes que `src/` y, al no penalizarse lo anclado, el mock ganaba a producción. Tests (fallan con la
  lógica previa): `an_anchored_mock_never_outranks_the_production_definition` (mock + doc homónimos),
  `test_copies_do_not_crowd_the_definition_out_of_anchoring`.
- **M-4 (`identifier_tokens`):** extensiones que también son nombres de miembro (`c h m r go fs ex tf sh pl hs ml
  log env db lock json html css sql conf cfg properties ini`) solo hacen "archivo" si el stem parece nombre de
  archivo: todo en minúsculas o todo en mayúsculas y sin empezar por un receptor conocido (`this`, `self`,
  `process`, `console`, `logger`, `req`, `res`, …). `process.env`, `this.db`, `logger.log`, `App.go`, `res.json`,
  `this.state.db` → identificadores; `main.go`, `server.log`, `foo.h`, `app.db`, `README.md`, `app.module.json`
  → archivos. Ambigüedad residual documentada (`mutex.lock` sí está como receptor; `server.lock` queda archivo).
  Test: `member_names_that_look_like_extensions_are_identifiers` (falla con la lógica previa).
- **M-5 (tercer pase de `symbol_definitions`):** el match en chunks `grouped` exige la línea de cabecera completa
  que escribe `context_header` (`regexp_matches(text, '(^|\n)# [^\n]* > <name>(\n|$)')`, nombre escapado), en vez
  de `contains(text, ' > name\n')`, que casaba código (`return a > limit\n`). Test (falla con la lógica previa):
  `store::tests::a_grouped_definition_matches_the_header_line_not_the_code`.
- **Nit:** la marca `anchored` en `search_items` (MCP) usa `devctx_search::anchor_tokens` (los 3 identificadores
  que de verdad se buscan), no todos. Test: `anchor_tokens_are_the_truncated_identifier_set`.
- Gate: ver TASK-011 Fixup F.
