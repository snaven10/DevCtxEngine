# TASK-001 — Instrumentación de memoria (`status.memory`) y línea base post-0.8.5

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** — (primera del plan; requiere 0.8.5 publicado en `main`)
- **Estado:** `pending`

---

## Objetivo

Que devctx diga, por sí mismo, cuánta memoria usa cada proceso y en qué (modelo cargado o no, ORT,
DuckDB, HNSW), y dejar medida la línea base sobre 0.8.5 con una metodología repetible que usan todas
las tasks siguientes. Confirmar o refutar HM-1…HM-4 del diseño (§1).

## Contexto verificado

- El serve cachea embedder y reranker en `AppState` (`crates/devctx-mcp/src/state.rs:241`,
  accesores `embedder()`/`reranker()` en `:305-333`, struct `Cached { value, last_used }`) y los
  suelta en `release_idle_models` (`:350-373`) con `trim_allocator` (`:189-199`, `malloc_trim`).
- Rutas del serve: `GET /status` (`crates/devctx-api/src/lib.rs:51`, handler `status` en `:1005`),
  `GET /health` (`:47`, handler en `:914`); la tool MCP `index_status` sale de `do_index_status`
  (`state.rs:1216`). Central: `GET /health` (`crates/devctx-api/src/central.rs:43`), sin `/status`.
- DuckDB: límites aplicados en `Store::apply_resource_limits` (`crates/devctx-store/src/store.rs:221-230`)
  con el resultado descartado; HNSW creado en `Store::enable_hnsw` (`store.rs:267-284`), métrica
  leída por `hnsw_metric` (`store.rs:~288`).
- El central (post-0.8.5) tiene su embedder con liberación por inactividad (hotfix D2): re-verificar
  el símbolo exacto en `crates/devctx-central/src/lib.rs` al arrancar.
- Zombies: `spawn_server` cosecha al hijo desde 0.8.3 (`crates/devctx-cli/src/remote.rs:461-470`);
  el central desde antes (`crates/devctx-central/src/client.rs:255-269`). PLAN-010 H7.

## Archivos

- **Crear:** `crates/devctx-core/src/procmem.rs` (lectura de `/proc/self/status` y `smaps_rollup`,
  `cfg(target_os = "linux")`, `None` en el resto), `scripts/memprobe.sh` (muestreo externo).
- **Modificar:** `crates/devctx-core/src/lib.rs` (export), `crates/devctx-store/src/store.rs`
  (`Store::memory_report()` — settings efectivos, `duckdb_memory()`, conteo de vectores y presencia
  de HNSW), `crates/devctx-mcp/src/state.rs` (estado de modelos: cargado, clave, `loaded_at`,
  `idle_secs`, motor), `crates/devctx-api/src/lib.rs` (`status` incluye `memory`),
  `crates/devctx-api/src/central.rs` (`memory` en `/health` o `GET /status` nuevo),
  `crates/devctx-central/src/lib.rs` (estado del embedder del central).

## Pasos

- [ ] **Paso 1 — `procmem`.** `ProcMemory { rss, rss_anon, rss_file, rss_shmem, hwm, pss? }` desde
      `/proc/self/status` (y `smaps_rollup` para `Pss`/`Shared_Clean`/`Private_Dirty` si es barato).
      Parser puro testeable con un texto de ejemplo. Fuera de Linux: `None`.
- [ ] **Paso 2 — `Store::memory_report`.** `memory_limit`, `threads` desde `duckdb_settings()`;
      uso desde `duckdb_memory()` (si la versión vendorizada no la tiene, `None` + motivo);
      `SELECT count(*) FROM vectors`; HNSW presente + métrica; `estimated_bytes` documentado como
      estimación.
- [ ] **Paso 3 — estado de modelos.** Exponer desde `AppState` y `Central` un snapshot sin cargar
      nada (no llamar `embedder()`: leer el `Mutex<Option<Cached>>`).
- [ ] **Paso 4 — contrato JSON.** Bloque `memory` (forma en PLAN-010-design DD-1) en `GET /status`,
      `index_status` y el central. Aditivo.
- [ ] **Paso 5 — logs.** Al cargar y al liberar un modelo: una línea a stderr/`serve.log` con
      clave, motor, duración de carga y RSS antes/después.
- [ ] **Paso 6 — `scripts/memprobe.sh`.** Dado un PID y un comando: muestrea `/proc/<pid>/status`
      y `smaps_rollup` cada 200 ms, reporta máximo, final y reposo a los N s. Sin dependencias raras
      (bash + awk).
- [ ] **Paso 7 — línea base (0.8.5 instalado, con aprobación del usuario).** Escenarios E1-E6 del
      diseño (DD-1). Para E6 (costo HNSW): serve recién abierto → RSS → primer `search` vectorial →
      RSS. Para E2 cronometrar también throughput (chunks/s) y para E5 latencia p50/p95.
- [ ] **Paso 8 — zombies.** Para cada `<defunct>` con nombre `devctx`: `ps -o ppid`, y del padre
      `/proc/<ppid>/exe` (¿`(deleted)`?) y versión. Anotar en el Resultado.
- [ ] **Paso 9 — completar la columna "Antes" de PLAN-010 §6** con los números de 0.8.5 (editar el
      master en el mismo commit de cierre).

## Criterios de aceptación

- [ ] `curl /status` del serve y del central devuelven `memory` con los cuatro sub-bloques; test de
      integración que valida forma y tipos (no números).
- [ ] Con el modelo liberado, `memory.models.embedder.loaded == false` sin haber disparado una carga
      (test: `status` sobre un serve recién abierto no carga el modelo).
- [ ] Parser de `/proc/self/status` con test unitario sobre texto fijo; compila en Windows/macOS
      (`cfg`).
- [ ] Resultado con la tabla E1-E6 completa y el veredicto de HM-1…HM-4 (cuánto de los ~500 MB es
      modelo/ORT vs DuckDB/HNSW).

## Riesgos

- `duckdb_memory()` puede no existir en la versión de DuckDB vendorizada: se degrada a `null`.
- Medir con otros procesos vivos contamina `Pss`: los escenarios se corren con `devctx serve --stop`
  previo en los proyectos no medidos.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
