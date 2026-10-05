# TASK-011 — Ranking: penalización de tests/docs, `kind`/`include_tests`, excludes por defecto

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Aplicar PLAN-008 D3: tests/specs/docs/SQL/README bajan en el ranking pero no desaparecen; el agente
puede filtrar duro con `kind`/`include_tests`; vendor/generado no entra al índice.

## Contexto verificado

- `crates/devctx-search/src/lib.rs:121-122` — `reciprocal_rank_fusion(lists, k)`; `do_search`
  (`crates/devctx-mcp/src/state.rs:365-440`) arma `SearchFilter` (lenguajes, rama, deletions).
- `crates/devctx-core/src/config.rs:176-187` — `Indexing.exclude: Vec<String>`, vacío por defecto.
- `crates/devctx-index/src/git.rs:85-99` — archivos = `git ls-files` + no trackeados no ignorados;
  `crates/devctx-index/src/pipeline.rs:329-340` — `build_exclude` (motor gitignore).
- Inventario: en REVFA_BackEnd el 57 % de las aristas sale de `/test/`; en búsquedas de campo los
  javadocs largos y specs desplazan al código.

## Archivos

- **Modificar:** `crates/devctx-search/src/lib.rs` (clasificador de ruta + factor)
- **Modificar:** `crates/devctx-mcp/src/state.rs`, `lib.rs` (parámetros `kind`, `include_tests`)
- **Modificar:** `crates/devctx-core/src/config.rs`, `crates/devctx-index/src/pipeline.rs` (excludes)

## Pasos

- [x] **Paso 1 — Clasificador.** `fn path_kind(path) -> Kind { Code, Test, Doc, Config }` puro:
      `test/`, `tests/`, `__tests__/`, `*_test.*`, `*.spec.*`, `*Test.java`, `test_*.py` → Test;
      `*.md`, `docs/`, `README*` → Doc; `*.sql`, `*.yaml`, `*.json`, `*.toml`, `*.properties` → Config.
- [x] **Paso 2 — Penalización.** Tras la fusión, multiplicar el score por un factor por kind
      (Test/Doc/Config p. ej. 0.6; configurable en `search:` del config). No se excluye nada.
- [x] **Paso 3 — Filtro duro.** `search` (y luego `build_context`) aceptan `kind: "code"|"test"|
      "doc"|"config"` e `include_tests: bool` (default `true`; `false` = excluir Test). Se aplica en
      el `SearchFilter` cuando el store puede filtrar por ruta, si no post-filtro con sobre-fetch.
- [x] **Paso 4 — Excludes por defecto.** `Indexing.default_excludes: bool` (default `true`) que suma
      `node_modules/`, `target/`, `dist/`, `build/`, `vendor/`, `*.min.js`, `*.generated.*` a
      `exclude` (lista final según PLAN-008 Q-3). El siguiente `index` poda lo ya indexado (mecanismo
      existente de exclusión).

## Criterios de aceptación

- [x] Tests unitarios de `path_kind` con rutas reales de Java/TS/Python/Rust.
- [x] Test de ranking: dos hits de igual similitud, uno en `src/` y otro en `src/test/` → el de
      `src/` primero; con `kind: "test"` solo vuelve el test.
- [x] Test de indexado: un `dist/app.js` versionado no entra con defaults; entra con
      `default_excludes: false`.
- [ ] En el Resultado: 3 consultas de campo (REVFA_BackEnd) con el top-5 antes/después.

## Riesgos

Ver PLAN-008 §7 (excludes y penalización). El factor es configurable para poder volver a 1.0.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done` (el criterio de las 3 consultas de campo en REVFA_BackEnd queda para TASK-016: no se tocó `~/revfa`).
2. **Repro antes/después.** Antes: en una búsqueda sin filtro, un spec/README/SQL con similitud igual o mayor
   quedaba por encima del código; `build/`, `target/` y todo lo versionado generado entraba al índice (solo
   había un salto interno de `node_modules`/`vendor`/`third_party`/`dist`/`bower_components` en `is_generated`,
   que no se podía apagar). Después: cubierto por los tests de §5.
3. **Causa raíz.** Corregido respecto de §3/D3: **sí** había una exclusión por defecto parcial
   (`VENDOR_DIRS` en `pipeline.rs::is_generated`), fija y no configurable. Se movió a `DEFAULT_EXCLUDES`
   (configurable) y se agregaron `target/`, `build/`, `*.min.js`, `*.generated.*`. Además un exclude nuevo
   **no podaba** en una corrida incremental (solo en las completas): ver "Poda".
4. **Archivos y símbolos.**
   - `devctx-core/src/kind.rs` (nuevo): `PathKind{Code,Test,Doc,Config}` (`parse`, `as_str`),
     `path_kind(path, language) -> PathKind` (pura), `KindPenalty{test,doc,config}` (`factor`, `NONE`).
   - `devctx-core/src/config.rs`: `DEFAULT_EXCLUDES`, `Indexing.default_excludes` (default `true`),
     `Indexing.include_build` (default `false`), `Indexing::effective_excludes()`, `impl Default for Indexing`,
     `SearchCfg{penalty}` y `ProjectConfig.search`.
   - `devctx-search/src/lib.rs`: `RankOptions{kind, include_tests, penalty}` (`keeps`), `KindSel{kind,
     include_tests}` (`options`, `cli_args`), `search_ranked(.., &RankOptions)`; `search(..)` mantiene su firma
     y usa `RankOptions::default()` (penalización por defecto).
   - `devctx-index/src/pipeline.rs`: `EXCLUDE_META_KEY="excludes"`, `exclude_fingerprint(&[String])`; se quitó
     `VENDOR_DIRS` de `is_generated`.
   - Cableado: `do_search/do_search_group/do_search_project/search_one(.., &KindSel)`, `Backend::search/
     search_project(.., &KindSel)`, MCP `search`/`search_project` (`kind`, `include_tests`), HTTP `/search`
     (`kind`, `include_tests`), CLI `devctx search --kind <k> --no-tests`, `remote.search(.., &KindSel)`;
     `effective_excludes()` en el pipeline (CLI, serve) y en `watch`.
5. **Reglas de `kind`** (precedencia Test > Doc > Config > Code). Test: directorios `test(s)`, `__tests__`,
   `__mocks__`, `spec(s)`, `e2e`, `testdata`, `fixtures`; nombres `*_test.*`, `*_tests.*`, `test_*`, `*.test.*`,
   `*.spec.*`, `conftest.py`, y `*Test|*Tests|*IT` con extensión java/kt/scala/groovy/cs/php/swift (T mayúscula:
   `Contest.java` es código). Doc: `*.md|mdx|rst|adoc|txt`, `docs|doc|documentation/`, `README*`, CHANGELOG,
   LICENSE, CONTRIBUTING, AUTHORS (o lenguaje `markdown`/`text`). Config: `sql|yaml|yml|json|jsonc|toml|
   properties|ini|cfg|conf` (o lenguaje equivalente). Sin archivo (memorias) = Code. Un `#[cfg(test)]` dentro
   de `src/lib.rs` sigue siendo Code (no se deduce de la ruta).
6. **Penalización.** Factor 0.6 para Test, Doc y Config (config `search.penalty.{test,doc,config}`; `1.0` la
   apaga). Se aplica sobre el score tras la fusión/recuperación y **antes** del dedup (así dedup ve el orden
   penalizado) y también sobre la salida del reranker (que ahora recibe todo el pool y luego se corta a
   `limit`; con score negativo se divide para que siempre baje). Las definiciones ancladas por identificador
   (TASK-012) no se penalizan: van al frente con el mejor score; solo un filtro duro explícito las quita.
7. **Filtros duros.** `kind: code|test|doc|config` e `include_tests: bool` (default `true`; un `kind`
   explícito gana). Post-filtro con sobre-fetch (pool de 400 por recuperador) porque el store no puede
   filtrar por kind. Aplican a `search`, `search_project` y búsqueda de grupo (por flags al hijo); `kind`
   inválido falla con `` `kind` must be one of code, test, doc, config ``. `build_context` queda para TASK-013
   (llama a `do_search` con `KindSel::default()`).
8. **Excludes.** Por defecto: `node_modules/ target/ dist/ build/ vendor/ third_party/ bower_components/
   *.min.js *.generated.*` (gitignore; se suman *antes* de `indexing.exclude`, por lo que un `!vendor/` del
   usuario re-incluye). Opt-out: `indexing.include_build: true` (solo `build/`) o
   `indexing.default_excludes: false` (todo). Decisión Q-3: `include_build` en vez de pedir al usuario un
   `!build/`, más descubrible.
9. **Poda.** El pipeline ya podaba lo no seleccionado, pero solo en corridas completas (`--full`, modelo
   cambiado, primera vez). Se agregó una huella (FNV-1a) del set efectivo de excludes en `index_meta`
   (`excludes`): si cambia (o no existe: índices anteriores a esta versión) la siguiente `devctx index`
   corre completa y poda; se sella solo tras una corrida completa. Efecto: **el primer `index` tras actualizar
   es completo** (re-embebe todo) en cada rama indexada. Una corrida por lista de rutas (hook/watch) no poda ni sella.
10. **Tests** (nuevos; los de ranking/excludes no compilan o fallan en el padre, que no tiene
    `search_ranked`, `effective_excludes` ni huella): devctx-core `kind::tests::{tests_in_java_ts_python_rust_go,
    docs, config_and_data, code_is_everything_else_and_lookalikes_stay_code,
    test_directories_beat_extensions_and_windows_separators_work, language_decides_only_when_the_name_is_silent,
    parse_and_factor}`, `config::tests::{default_excludes_are_on_and_include_build_drops_only_build,
    search_penalty_parses_with_per_kind_defaults}`; devctx-search `tests_docs_and_config_rank_below_code_but_stay_in_the_results`,
    `the_penalty_values_are_configurable_per_kind`, `a_kind_filter_keeps_only_that_kind`,
    `include_tests_false_drops_tests_only`, `a_selective_filter_looks_past_the_default_pool`,
    `an_anchored_definition_survives_the_penalty_but_not_a_hard_filter`; devctx-index
    `default_excludes_keep_tracked_build_output_out_and_can_be_turned_off`, `include_build_reindexes_build_but_not_the_other_defaults`,
    `a_changed_exclude_set_prunes_on_the_next_incremental_run`; e2e `kind_filters_and_default_excludes_work_through_the_cli_and_the_tool`
    (search_shapes_cli). Se reubicó la aserción de `node_modules/` de `machine_written_files_are_recognised` a
    los tests de `effective_excludes` (la lógica ya no vive en `is_generated`). Comando:
    `TMPDIR=/var/tmp DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache cargo test --workspace`: todo verde;
    `cargo fmt --check` y `clippy --workspace --all-targets` (con y sin `--features gpu`) con `-D warnings`: 0.
11. **Contrato JSON.** `search` (MCP/HTTP): entrada `kind?`, `include_tests?`; la salida no cambia. `search_project`:
    entrada `kind?`, `include_tests?`. Config nueva: `indexing.default_excludes`, `indexing.include_build`,
    `search.penalty.{test,doc,config}`.
12. **No verificado.** Las 3 consultas de campo en REVFA_BackEnd con top-5 antes/después (TASK-016); el valor
    0.6 es el sugerido por la task, no calibrado contra datos reales; una API HTTP vieja ignora `kind`
    (campo desconocido); `devctx-tui` no usa los filtros; docs EN/ES (TASK-015); Windows.
13. **Números.** Sobre-fetch de 400 candidatos por recuperador con filtro duro; 9 excludes por defecto.

### Fixup E (review)

- **Efecto colateral corregido:** el hash de excludes en `index_meta` forzaba un reindex **completo** en el
  primer `index` tras actualizar (o sin hash guardado): ~1 h por 1400 archivos re-embebiendo lo que no cambió.
  Ahora un set de excludes distinto (o ausente) ya no impide el incremental: la corrida *reconcilia*
  (`reconcile_excludes` en `devctx-index/src/pipeline.rs`) — borra las filas de los archivos indexados que el
  set ahora excluye (`files_pruned`), agrega los archivos trackeados que el set ya no excluye y no están en el
  índice, y estampa el hash nuevo. Una corrida por lista de rutas sigue sin estampar. Test (falla en 409c0d4):
  `an_index_without_an_exclude_fingerprint_is_reconciled_incrementally` (store sin hash → `full_reindex: false`,
  4 podados, `files_indexed == 0`, hash estampado); `a_changed_exclude_set_prunes_on_the_next_incremental_run`
  ahora también exige `!full_reindex` y que al volver a incluir solo se embeben los 4 archivos nuevos.
- **I2 (de 012, toca el post-filtro de 011):** con filtro duro el pool es `max(400, 4×limit)`.
