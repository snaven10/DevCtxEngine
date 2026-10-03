# TASK-007 — Versión del extractor en `index_meta` y aviso de índice viejo

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

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

- [ ] **Paso 1.** `devctx_parse::extractor_fingerprint() -> String` = `EXTRACTOR_VERSION` (constante
      manual que se sube cuando cambia la lógica de `parser.rs`) + hash estable (FNV) del contenido de
      los `languages/*.json` embebidos. Test: estable entre llamadas; cambia si cambia un JSON.
- [ ] **Paso 2.** Tabla nueva `index_meta(repo TEXT, branch TEXT, key TEXT, value TEXT,
      PRIMARY KEY(repo, branch, key))`. Ninguna tabla existente cambia.
- [ ] **Paso 3.** El pipeline escribe `extractor = <fingerprint>` al terminar un indexado completo o
      incremental exitoso de la rama.
- [ ] **Paso 4.** `do_index_status` agrega `extractor_stale: bool` (ausente o distinto = `true`) y un
      `hint` "index built by an older extractor — run `devctx index --full`". `up_to_date` no cambia
      de significado. Las tools de grafo agregan una línea `warning` cuando `extractor_stale`.
- [ ] **Paso 5.** `devctx index` sin `--full` sobre un índice `extractor_stale`: avisar y sugerir
      `--full` (no reindexar todo en silencio). Ver PLAN-008 §4.

## Criterios de aceptación

- [ ] Test de store: DB sin `index_meta` se abre sin error y reporta `extractor_stale: true`.
- [ ] Test: indexar → `extractor_stale: false`; cambiar el fingerprint (inyectado) → `true`.
- [ ] `devctx status` en un DB 0.8.2 existente: abre, crea la tabla, avisa una vez.

## Riesgos

Todos los índices existentes van a avisar una vez tras actualizar (no tienen fingerprint). Se
documenta en TASK-015; es honesto, no un falso positivo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
