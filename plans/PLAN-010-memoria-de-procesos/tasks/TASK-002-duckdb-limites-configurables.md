# TASK-002 — DuckDB: `memory_limit`/`threads` en config, valores efectivos y defaults medidos

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Que el presupuesto de DuckDB de cada proceso sea configurable desde `config.yaml` (además del env),
que el valor efectivo se verifique y se reporte, y que el default salga de medir la creación del HNSW
sobre el store más grande disponible (REVFA_FrontEnd, 2.3 GB), no de una suposición. PLAN-010 DD-7.

## Contexto verificado

- **Ya hay límites** (corrección al informe, PLAN-010 H4): `DEFAULT_MEMORY_LIMIT = "2GB"`,
  `DEFAULT_THREADS = 4` (`crates/devctx-store/src/store.rs:43-44`), aplicados por
  `apply_resource_limits` (`store.rs:221-230`) en `Store::open` (`store.rs:121`) y
  `open_in_memory` (`store.rs:147`); override `DEVCTX_DB_MEMORY_LIMIT` (validado por
  `is_size_literal`, `store.rs:51-53`) y `DEVCTX_DB_THREADS`. El resultado del `SET` se descarta
  (`let _ =`, `store.rs:227`). Test existente: `a_new_store_caps_its_own_thread_count`
  (`store.rs:1281-1296`). Commit de origen: `72b1a5c`.
- `try_clone` comparte la instancia y hereda los settings (doc en `store.rs:215-220`).
- `Storage` (`crates/devctx-core/src/config.rs:150-173`) tiene `db_path`, `hnsw`, `metric`, `fts`;
  sin perillas de memoria. Central: `CentralConfig` (`crates/devctx-central/src/config.rs:93-102`)
  con `defaults.storage: Option<Storage>` (`:76`); abre su store con `Store::open(&paths.db, dim)`
  (`crates/devctx-central/src/lib.rs:88`).
- HNSW: `LOAD vss` + `hnsw_enable_experimental_persistence` (`store.rs:244`); `enable_hnsw`
  (`store.rs:267-284`) hace `CREATE INDEX … USING HNSW` + checkpoint.

## Archivos

- **Modificar:** `crates/devctx-core/src/config.rs` (`Storage.memory_limit`, `Storage.threads`, con
  `#[serde(default)]`), `crates/devctx-store/src/store.rs` (`DbLimits`, `Store::open_with`, lectura
  de vuelta, `temp_directory`/`max_temp_directory_size`), los llamadores de `Store::open` que tienen
  config a mano (`crates/devctx-mcp/src/state.rs:~205`, `crates/devctx-central/src/lib.rs:88`, CLI),
  `crates/devctx-central/src/config.rs` (si el central necesita sección propia).

## Pasos

- [ ] **Paso 1 — matriz de medición (antes de tocar defaults).** Sobre una **copia** del DB de
      REVFA_FrontEnd en el scratchpad (nunca el original): `DROP INDEX` del HNSW y re-crearlo con
      `memory_limit` ∈ {512MB, 1GB, 2GB} × `threads` ∈ {2, 4}. Medir con `scripts/memprobe.sh`:
      ¿termina?, tiempo, `VmHWM`, bytes de spill en `<db>.tmp`. Repetir el `search` vectorial
      (latencia) con cada combinación. Mismo ejercicio rápido sobre el DB del central.
- [ ] **Paso 2 — config.** `storage.memory_limit` (string, literal de tamaño DuckDB, validado con
      `is_size_literal`) y `storage.threads` (usize). Vacío/0 = default. Precedencia env > config >
      default (DD-7).
- [ ] **Paso 3 — `Store::open_with(path, dim, &DbLimits)`.** `Store::open` queda como atajo con
      defaults para no romper llamadores/tests. Tras el `SET`, leer `duckdb_settings()`; si difiere o
      falló, aviso en stderr con el valor efectivo.
- [ ] **Paso 4 — spill acotado.** `SET temp_directory` explícito junto al DB y
      `max_temp_directory_size` (default razonable, configurable sólo por env en esta task).
- [ ] **Paso 5 — defaults.** Fijar `DEFAULT_MEMORY_LIMIT`/`DEFAULT_THREADS` (serve y central pueden
      diferir) con la matriz del paso 1 y lo que el usuario responda en Q-1. Documentar el porqué en
      el comentario de `store.rs:29-44`.
- [ ] **Paso 6 — reporte.** El valor efectivo aparece en `status.memory.duckdb` (TASK-001).

## Criterios de aceptación

- [ ] Test: config con `memory_limit: 768MB` → `duckdb_settings()` lo refleja; con
      `DEVCTX_DB_MEMORY_LIMIT=1GB` además, gana el env.
- [ ] Test: un literal inválido en config se ignora con aviso y queda el default (no inyecta SQL).
- [ ] La matriz del paso 1 está en el Resultado y el default elegido crea el HNSW de la copia de
      REVFA_FrontEnd sin OOM.
- [ ] `a_new_store_caps_its_own_thread_count` sigue verde (o se adapta al nuevo default).

## Riesgos

- Si el HNSW no respeta `memory_limit` (HM-2), bajar el límite no baja el pico de creación y sí
  aumenta el spill de las consultas: el default se elige por el escenario de consulta, y el pico de
  HNSW se documenta como fuera del límite.
- Un `memory_limit` bajo puede hacer fallar con "Out of Memory Error" una consulta grande en vez de
  spillear (DuckDB no spillea todo). Si pasa en la matriz, se anota cuál y se sube el default.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
