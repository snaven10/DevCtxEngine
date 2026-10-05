# TASK-011 — Ranking: penalización de tests/docs, `kind`/`include_tests`, excludes por defecto

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

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

- [ ] **Paso 1 — Clasificador.** `fn path_kind(path) -> Kind { Code, Test, Doc, Config }` puro:
      `test/`, `tests/`, `__tests__/`, `*_test.*`, `*.spec.*`, `*Test.java`, `test_*.py` → Test;
      `*.md`, `docs/`, `README*` → Doc; `*.sql`, `*.yaml`, `*.json`, `*.toml`, `*.properties` → Config.
- [ ] **Paso 2 — Penalización.** Tras la fusión, multiplicar el score por un factor por kind
      (Test/Doc/Config p. ej. 0.6; configurable en `search:` del config). No se excluye nada.
- [ ] **Paso 3 — Filtro duro.** `search` (y luego `build_context`) aceptan `kind: "code"|"test"|
      "doc"|"config"` e `include_tests: bool` (default `true`; `false` = excluir Test). Se aplica en
      el `SearchFilter` cuando el store puede filtrar por ruta, si no post-filtro con sobre-fetch.
- [ ] **Paso 4 — Excludes por defecto.** `Indexing.default_excludes: bool` (default `true`) que suma
      `node_modules/`, `target/`, `dist/`, `build/`, `vendor/`, `*.min.js`, `*.generated.*` a
      `exclude` (lista final según PLAN-008 Q-3). El siguiente `index` poda lo ya indexado (mecanismo
      existente de exclusión).

## Criterios de aceptación

- [ ] Tests unitarios de `path_kind` con rutas reales de Java/TS/Python/Rust.
- [ ] Test de ranking: dos hits de igual similitud, uno en `src/` y otro en `src/test/` → el de
      `src/` primero; con `kind: "test"` solo vuelve el test.
- [ ] Test de indexado: un `dist/app.js` versionado no entra con defaults; entra con
      `default_excludes: false`.
- [ ] En el Resultado: 3 consultas de campo (REVFA_BackEnd) con el top-5 antes/después.

## Riesgos

Ver PLAN-008 §7 (excludes y penalización). El factor es configurable para poder volver a 1.0.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
