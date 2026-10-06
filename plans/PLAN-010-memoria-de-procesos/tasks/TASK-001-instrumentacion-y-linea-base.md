# TASK-001 — Instrumentación de memoria (`status.memory`) y línea base post-0.8.5

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** — (primera del plan; requiere 0.8.5 publicado en `main`)
- **Estado:** `done`

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

- [x] **Paso 1 — `procmem`.** `ProcMemory { rss, rss_anon, rss_file, rss_shmem, hwm, pss? }` desde
      `/proc/self/status` (y `smaps_rollup` para `Pss`/`Shared_Clean`/`Private_Dirty` si es barato).
      Parser puro testeable con un texto de ejemplo. Fuera de Linux: `None`.
- [x] **Paso 2 — `Store::memory_report`.** `memory_limit`, `threads` desde `duckdb_settings()`;
      uso desde `duckdb_memory()` (si la versión vendorizada no la tiene, `None` + motivo);
      `SELECT count(*) FROM vectors`; HNSW presente + métrica; `estimated_bytes` documentado como
      estimación.
- [x] **Paso 3 — estado de modelos.** Exponer desde `AppState` y `Central` un snapshot sin cargar
      nada (no llamar `embedder()`: leer el `Mutex<Option<Cached>>`).
- [x] **Paso 4 — contrato JSON.** Bloque `memory` (forma en PLAN-010-design DD-1) en `GET /status`,
      `index_status` y el central. Aditivo.
- [x] **Paso 5 — logs.** Al cargar y al liberar un modelo: una línea a stderr/`serve.log` con
      clave, motor, duración de carga y RSS antes/después.
- [x] **Paso 6 — `scripts/memprobe.sh`.** Dado un PID y un comando: muestrea `/proc/<pid>/status`
      y `smaps_rollup` cada 200 ms, reporta máximo, final y reposo a los N s. Sin dependencias raras
      (bash + awk).
- [x] **Paso 7 — línea base (0.8.5 instalado, con aprobación del usuario).** Escenarios E1-E6 del
      diseño (DD-1). Para E6 (costo HNSW): serve recién abierto → RSS → primer `search` vectorial →
      RSS. Para E2 cronometrar también throughput (chunks/s) y para E5 latencia p50/p95.
- [x] **Paso 8 — zombies.** Para cada `<defunct>` con nombre `devctx`: `ps -o ppid`, y del padre
      `/proc/<ppid>/exe` (¿`(deleted)`?) y versión. Anotar en el Resultado.
- [x] **Paso 9 — completar la columna "Antes" de PLAN-010 §6** con los números de 0.8.5 (editar el
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

- **Estado final:** `done` (el usuario redujo PLAN-010 a esta task; el resto se decide con estos números).
- **Resumen:** `status.memory` existe en el serve (`GET /status` y la tool `index_status`) y en el
  central (`GET /status` nuevo; `/health` queda como sonda mínima). Bloques `process` / `models` /
  `duckdb` / `hnsw`, aditivo, sin cargar nada al consultarlo. Logs `memory: loaded|released …; rss A -> B`
  en stderr del serve y del central. `scripts/memprobe.sh` (bash + awk, reloj monotónico). Línea base
  E1-E6 medida en **v0.9.0** (no 0.8.5: es lo que hay en `main`).
- **Archivos tocados:** `crates/devctx-core/src/procmem.rs` (nuevo) y `lib.rs`;
  `crates/devctx-store/src/store.rs` (`MemoryReport`, `Store::memory_report`) y `lib.rs`;
  `crates/devctx-mcp/src/state.rs` (`Cached.key/loaded_at`, `AppState::memory_json`, `log_model_event`,
  `do_index_status`); `crates/devctx-api/src/lib.rs` (test de `/status`); `crates/devctx-api/src/central.rs`
  (`GET /status`, `central_status_json`); `crates/devctx-central/src/lib.rs` (`embedder_state`,
  `db_memory_report`, log de carga); `scripts/memprobe.sh` (nuevo).
- **Verificado por:** tests nuevos (`procmem` parser sobre texto fijo + rama "unavailable";
  `memory_report` sobre DuckDB real; `memory_json` y `/status` del serve: forma, tipos y
  `models.embedder.loaded == false` sin haber cargado; `/status` del central: forma, y `loaded` pasa a
  `true` sólo tras un `recall`). Medición real descrita abajo.
- **Desviaciones:**
  1. Línea base sobre v0.9.0 y con el binario **instrumentado** (los cambios son lecturas y logs; no tocan
     el camino de indexación ni de carga), sin compilar un 0.9.0 puro aparte.
  2. `loaded_at` es epoch en segundos (+ `age_secs`), no ISO-8601: no hay crate de calendario en el árbol.
  3. `engine` es `"fastembed"` (local) o `"remote"`; el motor `ort` llega con TASK-004 si se hace.
  4. El central expone `GET /status` nuevo en vez de ensanchar `/health`.
  5. `smaps_rollup`/`Pss` no entra en `status` (sólo en `memprobe.sh`, que lo lee de afuera).
  6. Paso 7 con "aprobación del usuario": el encargo autorizó compilar/medir; todo corrió en un sandbox
     (`DEVCTX_HOME`, `HOME`, caché de modelos y repos clonados bajo un `mktemp -d -p /var/tmp`).
  7. Hallazgo de camino: con el serve (autoserve) **el índice HNSW no se crea** aunque `storage.hnsw: true`;
     sólo lo crea el camino directo `DEVCTX_NO_AUTOSERVE=1 devctx index` (ya lo dice AGENTS.md §6).
     `status.memory.hnsw.present` lo hace visible.
- **Riesgos abiertos / siguiente:** ver "Insumos para decidir". No se midió REVFA_FrontEnd completo en
  su tamaño real de usuario (18 GB en disco incluye no-git); el clon git (2135 archivos) es el medido.

### Método y máquina

Intel Core Ultra 7 255HX, 20 hilos, 76 GiB RAM (otros procesos del usuario vivos: ~25 GiB usados),
WSL2 Linux 6.18. Release sin `gpu`, modelo `ml-granite` (CPU, `local`, 384 dim) descargado en una caché
de sandbox; `DuckDB memory_limit` efectivo 1.8 GiB, 4 hilos. Repos: DevCtxEngine (clon, 300 archivos
git, 270 indexados, 4 677 chunks); REVFA_FrontEnd (clon git, 2 135 archivos, 1 644 indexados, 14 365
chunks); REVFA_EUI_Microservice (81 archivos, 383 chunks). Muestreo con `scripts/memprobe.sh` cada
0.2 s sobre el PID del serve (E2/E3) o 0.5 s (E1); `VmHWM` del kernel como pico real. Una caída del reloj
de pared de WSL durante E3 inflaba los tiempos: se descontó; `memprobe.sh` quedó con reloj monotónico
(`/proc/uptime`). Cifras en MiB. Todos los procesos de la medición se pararon con `serve --stop`.

### Línea base v0.9.0

| E | Escenario | Resultado |
|---|---|---|
| E1 | Central en reposo | arranque 82-101; con embedder cargado (tras `remember --scope global`) 303 estable, pico RSS 623, `VmHWM` 724; **tras 300 s de inactividad 105** (anon 28, file 77) |
| E2 | Serve DevCtxEngine `index --full` (4 677 chunks) | 528 s = **8.9 chunks/s**; `VmHWM` **897**; RSS final tras el índice 276, y 159 tras soltar el modelo |
| E3 | Serve REVFA_FrontEnd | completo: 14 365 chunks en 1 821 s = **7.9 chunks/s**, `VmHWM` **786**. Incremental (149 archivos editados, 1 201 chunks): 155 s = 7.7 chunks/s, `VmHWM` **753** |
| E4 | 3 serves con modelo cargado | RSS 315 / 322 / 284 (suma 921); **Pss** 256 / 262 / 224 (suma 742); `RssAnon` 233 / 241 / 205 (suma 679), `RssFile` ~80 c/u (sólo binario); `VmHWM` ~710-730 c/u. Tras soltar (300 s): 84 / 91 / 56 (Pss 55 / 62 / 28) |
| E5 | 20 `search` seguidos (serve caliente; tiempo de pared del CLI, incluye spawn) | 3 corridas: p50 35 / 30 / 37 ms, p95 70 / 40 / 43 ms (la última con HNSW). Primer `search` (carga del modelo): 1.09 s |
| E6 | Serve recién abierto -> primer `search` vectorial | **Sin HNSW** (DevCtxEngine, 4 677 vec): abre en 82 (anon 9) -> 439 tras el search (`VmHWM` 712) -> 299-308 al segundo. **Con HNSW** (mismo DB): abre en **97 (anon 26)** -> 445 (`VmHWM` 725) -> 308. Front (14 365 vec): abre en 128 (anon 57) con HNSW vs ~85 sin él |

Contabilidad propia de `status.memory` en esos serves: `duckdb.memory_usage_bytes` 12-42 MiB (límite
1.8 GiB, jamás cerca; `temp_files_bytes` 0), `hnsw.reported_bytes` 21 MB (4 677 vec) y 38 MB (14 365 vec).
Crear el HNSW por el camino directo: 1.7 s (4.7 k vec) y ~6 s (14 k vec), pico `VmHWM` 671-675.

### Veredicto de HM-1…HM-4

- **HM-1 (>= 250 de ~500 MB son modelo + ORT): confirmada en proporción, no en cifra absoluta.** El serve
  con modelo en 0.9.0 se estabiliza en **300-315**, no en ~500; el modelo+ORT son ~220-230 de `RssAnon`
  (~70 %); DuckDB (12-42) y HNSW (17-48) son poco.
- **HM-2 (HNSW cuesta decenas/cientos de MB): decenas, lineal.** ~3.4 KB por vector medido (+17 MiB con
  4.7 k vectores, +48 con 14.4 k). Extrapolación (no medida): 100 k vectores ~340 MiB.
- **HM-3 (soltado el modelo vuelve a < 60): parcial.** 84 / 91 / 56 en serves con datos y 105 en el central:
  quedan ~25-35 MiB sobre un serve vacío (82); `malloc_trim` devuelve casi todo, no todo.
- **HM-4 (el pico de indexar lo domina un lote): confirmada, con matiz.** El pico de indexar (753-897) es
  casi todo el **transitorio de carga del modelo** (`VmHWM` 712 con sólo abrir el modelo y hacer un
  `search`, contra 300 estable); indexar agrega ~75-190 encima. Los 18.2 GB / 6.5 GB de 0.8.4 ya no existen
  desde 0.8.5.

### Insumos para decidir (sin decidirlo)

- Metas de PLAN-010 §6 **ya cumplidas en 0.9.0**: pico de indexación completa (897 vs <= 1.5 GB), pico
  incremental en REVFA_FrontEnd (753 vs <= 1.5 GB), central en reposo tras el idle (105 vs <= 150).
  El motivo "controlar el pico por lote" de TASK-004 ya no tiene síntoma en 0.9.0.
- Lo que queda por encima de las metas: serve con modelo cargado 300-315 vs <= 250 (faltan 50-65 MiB), y el
  **transitorio de carga ~710 `VmHWM`** (400 por encima del estable) que se paga en cada recarga tras el
  idle de 300 s. Esos dos son lo que TASK-006/007 (pesos mapeados / perillas ORT) atacarían; el costo del
  motor propio (TASK-003/004/005) sólo se justifica si ahí hay ganancia medible.
- Compartir pesos entre serves (TASK-006): hoy 3 serves = 679 de `RssAnon`, pesos 100 % anónimos; el techo
  de ahorro es ~100 por serve (tamaño del modelo), no los ~225 del modelo+ORT completo.
- `provider: central` (TASK-009): con 3 serves la suma `Pss` es 742; con el modelo sólo en el central el
  serve queda en ~56-91 y el central 303 -> orden de ~480 de ahorro con 3 proyectos, cuesta latencia
  (HTTP) y el acoplamiento al central.
- Límites de DuckDB (TASK-002): `memory_usage` 12-42 MiB contra 1.8 GiB de límite; el HNSW queda fuera del
  buffer manager. Sin síntoma en estos tamaños; sólo cobra sentido en repos de >100 k vectores (no medido).
- Latencia y throughput de referencia para el "<= +10 %" y "-15 %": p50 30-37 ms / p95 40-70 ms;
  7.7-8.9 chunks/s.
- Zombis (Paso 8): hay 5 `[devctx] <defunct>` de 1-3 días; sus padres son 3 procesos `devctx mcp` con el
  ejecutable reemplazado (`/proc/<ppid>/exe -> ... (deleted)`), es decir lanzados antes de actualizar el
  binario y de la cosecha de 0.8.3. No es regresión del código actual; se limpian al reiniciar esos MCP.
