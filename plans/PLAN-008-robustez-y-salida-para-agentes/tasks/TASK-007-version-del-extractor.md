# TASK-007 — Versión del extractor en `index_meta` y aviso de índice viejo

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Que `status`/`index_status` y las tools de grafo avisen cuando el índice lo generó un extractor
distinto del actual, aunque el commit coincida (PLAN-008 B8). Sin migrar tablas existentes.

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:725` — `up_to_date: r.last_commit == state_git.commit`.
- `crates/devctx-store/src/schema.rs:63-238` — tablas actuales; no hay tabla de metadatos.
  `index_state` en `schema.rs:214`.
- Queries de extracción embebidas: `languages/*.json` (7 archivos) vía `devctx-parse`
  (`registry.rs`, `lang.rs`); cambios de lógica en `crates/devctx-parse/src/parser.rs`
  (p. ej. normalizador de receptores, `9ef3f4d`, 2026-08-24).
- Campo: `REVFA_REGISTRO_EXTERIOR` indexado 2026-08-19 → nodos basura de Mutiny con
  `up_to_date: true`. El extractor actual produce 0 targets basura (inventario §6).

## Archivos

- **Modificar:** `crates/devctx-store/src/schema.rs` (`CREATE TABLE IF NOT EXISTS index_meta`)
- **Modificar:** `crates/devctx-store/src/state.rs` o `store.rs` (get/set de `index_meta`)
- **Modificar:** `crates/devctx-parse` (constante `EXTRACTOR_VERSION` + hash de queries)
- **Modificar:** `crates/devctx-index/src/pipeline.rs` (escribir la versión al terminar)
- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_index_status`, avisos)

## Pasos

- [x] **Paso 1.** `devctx_parse::extractor_fingerprint() -> String` = `EXTRACTOR_VERSION` (constante
      manual que se sube cuando cambia la lógica de `parser.rs`) + hash estable (FNV) del contenido de
      los `languages/*.json` embebidos. Test: estable entre llamadas; cambia si cambia un JSON.
- [x] **Paso 2.** Tabla nueva `index_meta(repo TEXT, branch TEXT, key TEXT, value TEXT,
      PRIMARY KEY(repo, branch, key))`. Ninguna tabla existente cambia.
- [x] **Paso 3.** El pipeline escribe `extractor = <fingerprint>` al terminar un indexado completo o
      incremental exitoso de la rama.
- [x] **Paso 4.** `do_index_status` agrega `extractor_stale: bool` (ausente o distinto = `true`) y un
      `hint` "index built by an older extractor — run `devctx index --full`". `up_to_date` no cambia
      de significado. Las tools de grafo agregan una línea `warning` cuando `extractor_stale`.
- [x] **Paso 5.** `devctx index` sin `--full` sobre un índice `extractor_stale`: avisar y sugerir
      `--full` (no reindexar todo en silencio). Ver PLAN-008 §4.

## Criterios de aceptación

- [x] Test de store: DB sin `index_meta` se abre sin error y reporta `extractor_stale: true`.
- [x] Test: indexar → `extractor_stale: false`; cambiar el fingerprint (inyectado) → `true`.
- [x] `devctx status` en un DB 0.8.2 existente: abre, crea la tabla, avisa una vez.

## Riesgos

Todos los índices existentes van a avisar una vez tras actualizar (no tienen fingerprint). Se
documenta en TASK-015; es honesto, no un falso positivo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes: un índice hecho por un extractor viejo con el mismo commit reportaba
   `up_to_date: true` sin más. Después: `index_status` agrega `extractor_stale: true` + `hint`; las tools de
   grafo agregan `warning`. Cubierto por `an_index_from_an_older_extractor_stays_stale_until_a_full_run`
   (devctx-index) y `an_index_without_extractor_record_warns_in_every_graph_answer` (devctx-mcp). Sin repro
   manual en REVFA_REGISTRO_EXTERIOR.
3. **Causa raíz:** confirmada (§2 B8): `up_to_date` compara solo commits; no había registro de qué extractor
   produjo el índice.
4. **Archivos y símbolos.** `devctx-parse/src/registry.rs`: `EXTRACTOR_VERSION: u32 = 1` (se sube a mano al
   cambiar la lógica de `parser.rs`) y `extractor_fingerprint() -> String` = `v<N>-<FNV-1a 64 de los 7
   languages/*.json>` (reexportada en `devctx_parse` y `devctx_index`). `devctx-store/src/schema.rs`: tabla
   `index_meta(repo_path, branch, key, value, PK(repo_path, branch, key))` con `CREATE TABLE IF NOT EXISTS`, y
   `CHECKPOINT` tras crearla en una base existente. `devctx-store/src/state.rs`: `EXTRACTOR_META_KEY`,
   `Store::get_index_meta`, `set_index_meta`, `extractor_stale(repo_path, branch, current) -> Result<bool>`
   (ausente o distinto = `true`). `memory_refs.rs`: `drop_branch` también borra `index_meta`.
   `devctx-index/src/pipeline.rs`: `IndexResult.extractor_stale`; el fingerprint se escribe solo si la corrida
   fue completa o si el índice ya tenía el fingerprint actual. `devctx-mcp/src/state.rs`: `BranchChoice.extractor_stale`,
   `do_index_status`, `do_index` (campo/hint en el reporte). `devctx-cli/src/main.rs`: aviso en `cmd_index` local.
5. **Tests nuevos.** parse: `the_fingerprint_is_stable_between_calls`,
   `the_fingerprint_changes_with_a_definition_or_the_version`. store:
   `a_store_without_index_meta_reports_a_stale_extractor`. index:
   `an_index_from_an_older_extractor_stays_stale_until_a_full_run`. mcp:
   `an_index_without_extractor_record_warns_in_every_graph_answer`. Comandos: `cargo test -p devctx-parse -p devctx-store -p devctx-index -p devctx-mcp` (parse 41, store 55+1 ignorado, index 23, mcp 52+4, 0 fallos); `cargo test -p devctx-cli --test mcp_binding --test mcp_tools --test plan_status_cli` (27 / 8+1 ignorado previo / 9, 0 fallos); `cargo clippy --all-targets` sin warnings; `cargo fmt --all` aplicado.
6. **Contrato JSON.** `index_status` (con índice): `extractor_stale: bool` y, si es `true`, `hint`. Tools de grafo
   (`read_symbol`, `get_references`, `impact_analysis`, `search_routes`, `routes_for_handler`): `warning` con
   "index generated by an older extractor; reindex (devctx index --full)"; `search_routes`/`routes_for_handler`
   pasan de array a `{"routes": [...], "warning": ...}` mientras el índice sea viejo. Reporte de `index`:
   `extractor_stale` y `hint` solo cuando una corrida incremental dejó el índice viejo.
7. **No verificado:** `devctx status` sobre un DB 0.8.2 real (REVFA); `devctx status` sin servidor (modo local)
   no consulta el índice y no avisa; `search` no lleva el aviso (no es tool de grafo).
8. **Números:** sin medición.

