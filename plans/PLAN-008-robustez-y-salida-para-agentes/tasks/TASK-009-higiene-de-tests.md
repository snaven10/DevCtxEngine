# TASK-009 — Higiene de tests: ningún serve sobrevive a la suite

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `done`

---

## Objetivo

Que después de `cargo test -p devctx-cli` no quede ningún `devctx serve` vivo, y que un serve cuyo
proyecto desapareció se apague solo (PLAN-008 B9).

**Alcance ampliado (acordado, 0.8.3 bloqueante).** Además de la higiene de tests, esta task cierra
dos bugs de producto descubiertos en la revisión y que explican los serves huérfanos:

- **B10 — serve atrapa SIGTERM pero no sale.** El handler de `devctx-api/src/lib.rs` (`terminate()`,
  `with_graceful_shutdown`) corre, pero el drop implícito del `Runtime` en `run_blocking*` espera sin
  tope al pool bloqueante, atascado en una descarga de modelo (fastembed/hf-hub, `ureq` sin
  `timeout_read`), y el graceful shutdown de axum espera conexiones abiertas (un `/index` en vuelo).
  Arreglo: (a) `rt.shutdown_timeout(5 s)` en `run_blocking`, `run_blocking_ready` y el central;
  (b) watchdog en `std::thread` armado al recibir la señal: limpieza (`serve.json`), checkpoint
  acotado y `process::exit`; (c) tope de estancamiento en la descarga de modelos (embed y rerank).
- **B11 — `--idle` no se respeta.** El vigilante de idle era un `tokio::spawn` que moría con los
  workers, y `is_indexing` lo eximía para siempre si el progreso nunca avanzaba. Arreglo: vigilante en
  `std::thread`; `is_indexing` exime solo si el progreso avanzó en los últimos M s (tope; variable
  de entorno para bajarlo en tests).
- **Higiene:** (1) caché de modelos compartida entre tests (no una descarga de ~90 MB por `DEVCTX_HOME`);
  (2) flake ETXTBSY en los tests de binario reemplazado; (3) fugas del test de reconexión (TASK-004)
  y de `localguard-solo`: `Tmp::drop` detiene los serves que arrancó, vía `procown`, y afirma que
  no queda ninguno.

### Criterios de aceptación de la ampliación

- [x] **B10:** `devctx serve` sale ante SIGTERM en <= 2 s en condiciones normales y en <= 10 s con una
      tarea bloqueante atascada (descarga sin respuesta), haciendo checkpoint y borrando `serve.json`.
      Test con `HF_ENDPOINT` apuntando a un listener que acepta y nunca responde.
- [x] **B10c:** una descarga de modelo sin progreso falla con error explícito tras el tope de
      estancamiento (`DEVCTX_MODEL_STALL_SECS`, 120 s por defecto) en vez de colgarse.
- [x] **B11:** un serve sin requests sale tras `--idle` s (test con `--idle 2/3`), también con un
      `/index` atascado (endpoint agujero negro).
- [x] **Caché de modelos:** los tests no descargan el modelo por `DEVCTX_HOME`; tiempos de la suite
      antes/después en el Resultado.
- [x] **ETXTBSY:** los tests de binario reemplazado no flakean (copiar a temporal, `sync_all`, cerrar,
      `rename`, reintento ante ETXTBSY).
- [x] **Fugas:** tras la suite, 0 procesos `target/debug/devctx serve` con cwd bajo `/tmp/devctx_*`.

## Contexto verificado

- `crates/devctx-cli/tests/mcp_binding.rs:93-132` — `Tmp::drop`: `serve --stop` en proyectos a 1 y
  2 niveles + `serve --central --stop` + `remove_dir_all`.
- `mcp_binding.rs:700` — `Tmp::new("localguard-solo")` en `local_scope_refuses_to_guess_a_repository`
  (`:668`); el serve filtrado tenía cwd en ese tmp borrado y `--idle 900`.
- `crates/devctx-cli/src/remote.rs:224-245` — `stop_server` lee `serve.json`; sin archivo no hay a
  quién parar.
- TASK-001 cambia `cmd_serve` para escribir `serve.json` después de abrir el store: amplía la
  ventana en la que un serve vive sin anunciarse.

## Archivos

- **Modificar:** `crates/devctx-cli/tests/mcp_binding.rs` (`Tmp::drop`)
- **Modificar:** `crates/devctx-api/src/lib.rs` (vigilancia del directorio del proyecto)

## Pasos

- [x] **Paso 1 — Repro.** Correr solo `local_scope_refuses_to_guess_a_repository` y listar
      `devctx serve` vivos con cwd en `/tmp/devctx_bind_it_*` antes/después. Confirmar la causa.
- [x] **Paso 2 — Producto.** El serve sale (limpio, borrando su `serve.json` si es suyo) cuando el
      directorio del proyecto o su `.devctx/` desaparecen; chequeo barato en el loop de idle
      existente (`devctx-api/src/lib.rs`, ~cada 10-30 s).
- [x] **Paso 3 — Tests.** `Tmp::drop` además espera a que no quede ningún proceso cuyo cwd esté bajo
      el tmp (Linux: `/proc/*/cwd`), con tope corto; y `Tmp` exporta un `--idle` corto para los serves
      auto-lanzados en tests (variable de entorno leída por `spawn_server`, p. ej.
      `DEVCTX_AUTOSERVE_IDLE`).

## Criterios de aceptación

- [x] Tras `cargo test -p devctx-cli --test mcp_binding`, 0 procesos `devctx serve` con cwd bajo
      `/tmp/devctx_bind_it_*` (verificado en el Resultado con el comando usado).
- [x] Test: serve sobre un proyecto en tmp; borrar el directorio → el serve sale en < 60 s.

## Riesgos

Un directorio de proyecto en un disco montado de forma intermitente haría salir al serve; aceptado
(se relanza en la próxima llamada).

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes: serve con `/index` atascado en la descarga del modelo; SIGTERM
   capturado y el proceso seguía vivo indefinidamente (`--idle` tampoco lo sacaba). Después
   (`cargo test -p devctx-cli --test serve_lifecycle -- --nocapture`):
   - B10 SIGTERM normal: 25 ms (`sigterm_stops_an_idle_server_within_two_seconds`).
   - B10 SIGTERM con descarga atascada: 5.02 s, borra `serve.json`
     (`sigterm_stops_a_server_stuck_in_a_model_download`).
   - B10c descarga sin progreso: error "made no progress" a los 2.0 s con `DEVCTX_MODEL_STALL_SECS=2`
     (`a_stalled_model_download_fails_with_an_explicit_error`).
   - B11 `--idle 2` sin requests: 1.97 s (`an_idle_server_exits_after_its_window`); `--idle 3` con
     `/index` atascado: 3.66 s (`an_idle_server_exits_even_with_an_index_stuck`).
   - B9 proyecto borrado con `--idle 40`: 9.96 s (`a_server_whose_project_vanished_exits_by_itself`).
3. **Causa raíz confirmada.** B9 (hipótesis del plan) era consecuencia de B10: los serves filtrados de
   `localguard-solo` y del test de reconexión quedaban atascados en la descarga del modelo (cada
   `DEVCTX_HOME` temporal descargaba el modelo) y `serve --stop` (SIGTERM) no los sacaba. Además, el
   graceful shutdown de axum espera las conexiones abiertas (un `/index` en vuelo), y el drop
   implícito del `Runtime` espera sin tope al pool bloqueante. B11 era consecuencia de B10 (el
   vigilante era un `tokio::spawn`) más la exención eterna de `is_indexing`. hf-hub SÍ respeta
   `HF_ENDPOINT` (fastembed `pull_from_hf`); no permite inyectar un agente `ureq` con timeout.
4. **Archivos y símbolos.** `devctx-api/src/lib.rs`: `serve_with(.., ready, on_exit)` (parámetro
   nuevo `on_exit: impl Fn() + Send + Sync + 'static`), `run_blocking_ready(.., ready, on_exit)`,
   `rt.shutdown_timeout(5 s)` en `run_blocking*`, `exit_now`, `arm_exit_watchdog` (gracia
   `DEVCTX_SHUTDOWN_GRACE_SECS`, 5 s), `spawn_idle_watchdog` (`std::thread`, sondeo `idle/4` acotado
   a 250 ms-30 s, también sale si el proyecto desapareció); `devctx-api/src/central.rs`: mismo
   `shutdown_timeout` y vigilante de idle en `std::thread`; `devctx-mcp/src/state.rs`:
   `IndexProgress.advanced`, `is_indexing` solo si avanzó (`DEVCTX_INDEX_STALL_SECS`, 900 s),
   `AppState::project_vanished`; `devctx-core/src/modelload.rs` (nuevo): `guard_load`, `stall_limit`,
   `MODEL_STALL_ENV` (descarga con tope por estancamiento del caché, 120 s), usado en
   `devctx-embed/src/local.rs` y `devctx-rerank/src/local.rs`; `devctx-cli/src/main.rs`: `cmd_serve`
   pasa `withdraw` (borra su `serve.json`).
5. **Tests.** Nuevo `crates/devctx-cli/tests/serve_lifecycle.rs` (6 tests, arriba) y
   `tests/common/mod.rs` (`share_models`, `spawn_retrying`, `install_copy`, `reap_servers_under`).
   Comandos: `cargo test -p devctx-cli` (todas las suites en verde, `mcp_binding` 3 corridas),
   `cargo test -p devctx-core -p devctx-api -p devctx-mcp -p devctx-embed -p devctx-rerank -p
   devctx-central --lib`, `cargo clippy --workspace --all-targets` (0 warnings), `cargo fmt --all`.
   - **Caché de modelos compartida:** cada `Tmp` enlaza `DEVCTX_HOME/models` al caché real
     (`DEVCTX_MODEL_CACHE` si está definido, si no `$XDG_DATA_HOME|~/.local/share/devctx/models`).
     `mcp_binding` + `mcp_tools`: 2 m 37.8 s (mcp_binding 144.6 s) antes, 38.9 s (mcp_binding 35.1 s)
     después.
   - **ETXTBSY:** `install_copy` (temporal, `sync_all`, cierre, `rename`) y `spawn_retrying`
     (reintenta ETXTBSY, 50 ms x 100) en los tres tests de binario reemplazado/borrado.
   - **Fugas:** `Tmp::drop` (las 5 suites con serves) llama `reap_servers_under`: detiene vía
     `procown::terminate` (SIGTERM, SIGKILL) todo serve cuyo cwd esté bajo el tmp y hace fallar el
     test si queda alguno. Con el arreglo de B10 no hubo nada que reaper en 3 corridas.
6. **Contrato JSON:** sin cambios.
7. **No verificado:** el test de B10 contra el código previo al arreglo no se corrió (la evidencia es
   que la salida es de 5.0 s = la gracia del watchdog, el camino ordenado no termina);
   `DEVCTX_AUTOSERVE_IDLE` (paso 3 original) no se implementó: `Tmp::drop` ya detiene los serves;
   tiempos en WSL2 con caché de modelos caliente.
8. **Números:** ver punto 2 y caché de modelos. Servers `target/debug/devctx serve` vivos tras la
   suite completa: 0.

### Fixup D1 (review)

Revisión: TASK-009 podía cortar un índice legítimo y salir con el WAL sin plegar. Arreglado:

1. **SIGTERM durante `/index` (bloqueante).** `AppState::cancel_indexing` (flag `index_cancel`) se
   levanta al recibir la señal; `ProgressSink::cancelled` lo consulta entre archivos y el pipeline
   (`devctx-index/src/pipeline.rs::run`) para, no poda, no avanza el `IndexRecord`, hace checkpoint y
   devuelve `IndexResult::cancelled` (`/index` responde `{"cancelled":true,…}`). HNSW/FTS caídos
   quedan anotados en `index_meta` (`PENDING_HNSW_META_KEY`/`PENDING_FTS_META_KEY`, escritos ANTES
   del drop) y los reconstruye el siguiente run completo que quede solo en esa base
   (`Store::instance_id`). El watchdog de stop no fuerza la salida mientras el índice cancelado
   reporte progreso (≤15 s de silencio), con tope duro `INDEX_CANCEL_CAP` = 60 s.
2. **Watchdog nunca desarmado.** `Lifecycle::final_checkpoint` (running/done): con el checkpoint
   final en curso el watchdog espera (≤60 s); con él hecho sale sin repetirlo.
3. **Tiempos `--stop`.** Gracia 3 s + presupuesto de checkpoint 1.5 s (4.5 s) < `TERM_WAIT` 5 s;
   si `/index/progress` dice `running`, `serve --stop`/`reclaim_db` esperan 65 s antes de SIGKILL.
   Línea de tiempo documentada en `devctx-api/src/lib.rs::Lifecycle`.
4. **`project_vanished` (bloqueante).** Solo `ErrorKind::NotFound`, 3 polls seguidos
   (`VANISH_STRIKES`); watchdog propio (`DEVCTX_VANISH_POLL_MS`, 10 s por defecto) que corre sin
   `--idle` y difiere con requests en vuelo (middleware `count_in_flight`) o índice avanzando.
5. **`guard_load`.** Un stall envenena el modelo en el proceso (`modelload::is_poisoned`); los
   reintentos fallan al instante con "restart the server (`devctx serve --stop`)"; `init_on`
   (embed/rerank) no cae a CPU si el error es un stall (`is_stall_error`).
6. **Fases sin progreso (bloqueante).** `ProgressSink::phase` (+ `heartbeat` cada 5 s) en poda,
   HNSW, FTS y checkpoint; el progreso arranca (`SharedProgress::begin`) antes de cargar el modelo
   y la carga solo marca progreso cuando la caché de modelos crece (`cache_fingerprint`).
7. **Tests.** Unit: `only_an_index_that_moved_recently_is_exempt_from_idle`,
   `a_phase_report_renews_the_exemption`, `cancelling_reaches_every_run_through_its_sink`,
   `a_project_counts_as_vanished_only_after_repeated_not_found`,
   `a_permission_error_is_not_a_missing_directory`, `a_stalled_load_poisons_the_model_for_the_process`,
   `a_forced_checkpoint_folds_the_wal_past_an_open_write_transaction`, 4 del pipeline (cancelación,
   fases, índices pendientes). Integración (`serve_lifecycle.rs`, `#![cfg(unix)]`, embedder
   `custom` falso por HTTP): `an_idle_server_waits_for_an_index_that_is_advancing`,
   `sigterm_during_an_index_cancels_it_and_leaves_a_sound_database`,
   `serve_stop_during_an_index_waits_for_the_cancellation`,
   `a_forced_exit_mid_index_leaves_a_sound_database` (WAL plegado + `status` + `index --full` sin
   "Failed to delete all rows"); `an_idle_server_exits_even_with_an_index_stuck` ahora sí ejercita la
   rama de estancamiento (`DEVCTX_INDEX_STALL_SECS=2`). Contra el código previo fallan 5 de ellos;
   el de salida forzada pasa también antes (es guardia de regresión).
- **Menores:** `exit_now` sale con `libc::_exit` (sin destructores estáticos de DuckDB) y escala a
  `FORCE CHECKPOINT` (`AppState::checkpoint_for_exit`); el central hace checkpoint antes de salir
  por idle; doc de `do_search_project` despegada de `run_in_member`.
- **Hallazgo:** con el DuckDB embebido, un `CHECKPOINT` simple NO es rechazado por otra
  transacción de escritura abierta (pliega lo confirmado); el riesgo real era el corte por tiempo
  y la espera del mutex, no el rechazo. `FORCE CHECKPOINT` queda como escalada.
- **No verificado:** fallos EIO reales de drvfs/9p (simulado con EACCES); `heartbeat` en un HNSW
  >900 s real; el camino ordenado (fin de `main`) sigue usando `exit` normal.

### Fixup D2 (review)

- Los tests ya NUNCA caen por defecto en `~/.local/share/devctx/models` (35bfb7d enlazaba `DEVCTX_HOME/models`
  a la caché real si `DEVCTX_MODEL_CACHE` no estaba): `common::shared_model_cache` usa
  `$CARGO_TARGET_DIR|<workspace>/target` + `test-model-cache`. La primera corrida que necesite un modelo lo
  descarga ahí. Test: `without_an_explicit_cache_the_tests_never_use_the_users_real_models` (projects_cli).
- El test `dirs::the_model_cache_hangs_off_the_data_directory` ahora tolera `DEVCTX_MODEL_CACHE` seteado (el
  default de producción no cambia: sigue colgando de `data_dir()`).
- `common::stop_servers_under` sigue usando `procown::terminate` (ahora devuelve `Termination`; el resultado se
  ignora como antes y los restos se verifican con `servers_under`).
- N4: `devctx init --model ml-granite` sin los archivos del modelo ya no falla con un comando inexistente
  (`models download`): con `--yes` o en terminal los descarga; sin terminal ni `--yes` el error trae el comando
  exacto `devctx models --download <m>`. Test: `init_with_a_model_that_is_not_on_disk_names_the_download_command`.
  La descarga con `--yes` no se probó (requiere red).
- No verificado: `an_idle_server_waits_for_an_index_that_is_advancing` (serve_lifecycle) falla con >=2 hilos de
  test y pasa sola o con `--test-threads=1`; se reprodujo idéntico sobre el padre (sin estos cambios).

### Fixup D1b (review)

- **Corrección a D1 punto 1 (marcas pendientes).** No las salda "el siguiente full": las salda
  *cualquier* run que termine siendo el último en vuelo sobre esa base (full, incremental o la lista
  de paths del watcher), y lo hace reconstruyendo HNSW/FTS sobre toda la base. Eso es lo correcto
  (nadie más lo haría; mientras tanto las búsquedas van por fuerza bruta). Lo que estaba mal: la
  marca se borraba aunque `enable_hnsw`/`rebuild_fts` devolviera `Ok(false)` (extensión no
  disponible). Ahora solo se borra si la reconstrucción ocurrió (`pipeline.rs::run`).
- **Escrituras por archivo atómicas (menor 4).** `Store::in_transaction` / `in_transaction_opt`
  (reentrante; `upsert` se une a la transacción abierta). `index_file` parsea/trocea/embebe primero
  y luego escribe delete+upsert+edges+routes+`file_state` en UNA transacción; la ruta de copia entre
  ramas y `delete_file` también. Coste medido (`bench_index_small_fixture`, 300 archivos, store en
  disco, debug, mediana de 3): 22.9 s antes, 19.7 s después (−14 %: menos commits).
- **Congelado antes del checkpoint de salida (2).** `Store::freeze(wait)`: flag compartido por todas
  las conexiones del mismo open + `RwLock` (cada escritura toma el lado compartido durante su
  sentencia vía `Store::w()`); tras el freeze toda escritura falla con `StoreError::Frozen` y no llega
  al WAL. `AppState::checkpoint_for_exit(freeze_wait)` congela primero. `exit_now`: `on_exit` antes
  del checkpoint, y el hilo del checkpoint hace `_exit` en cuanto vuelve (el hilo llamante es el
  presupuesto de 1.5 s). El camino ordenado también congela (el wind-down del runtime ya no puede
  escribir).
- **Carrera watchdog/camino ordenado (5).** `Lifecycle::claim_orderly_checkpoint` /
  `claim_exit_checkpoint` (CAS `IDLE→RUNNING` / `IDLE→EXITING`): si `exit_now` encuentra
  `RUNNING` espera a `DONE` (≤60 s) en vez de cortar; si el ordenado llega tarde, se aparta.
- **Fases post-bucle (1).** `cancelled()` antes de los drops iniciales (un run que arranca tras el
  stop no tira HNSW/FTS), dentro de la poda (para entre archivos, no avanza el record) y antes de cada
  reconstrucción (se salta; la marca la deja para el siguiente run).
- **Clientes (3).** `remote::index_cancelled`: `devctx index` (remoto) y `projects reindex` avisan y
  salen ≠ 0; el watcher re-encola el lote (`Outcome::Cancelled`, pausa 5 s).
- **`stop` sin índice.** `TERM_WAIT` = `devctx_api::STOP_TIMELINE` (gracia 3 s + 1.5 s) + 1.5 s = 6 s,
  y `procown::terminate_patient` sigue esperando (≤65 s) mientras exista
  `devctx_api::checkpoint_marker` (`serve.checkpointing`, con el pid) — el listener ya está cerrado
  durante el checkpoint final, así que `/health` no sirve para preguntarlo.
- **Gracia (7).** `DEVCTX_SHUTDOWN_GRACE_SECS` solo acorta (`grace_from`, tope 3 s).
- **Central (6).** El checkpoint de salida por idle usa su propia conexión clonada (no el mutex) +
  freeze + FORCE; el idle no sale con requests en vuelo salvo que lleven ≥4 ventanas (atascadas).
- **Nits.** `cmd_index --full` anota `PENDING_HNSW` antes de su `drop_hnsw` y ya no reconstruye
  HNSW/FTS dos veces; `modelload::guard_load_with` (el test ya no hace `set_var`); el envenenamiento
  se levanta cuando el loader abandonado termina (`unpoison`).
- **Flaky `an_idle_server_waits_for_an_index_that_is_advancing` — causa raíz.** No era el tiempo del
  test: `do_index_inner` llamaba `sink.finish()` (running=false) ANTES de `prune_untracked_branches` +
  `report_index`, y el reloj de idle se sellaba solo al *inicio* del request. En esa ventana el
  watchdog veía "sin índice" e "idle 9.9 s" (desde el inicio del `/index`) y hacía `_exit` con la
  respuesta sin enviar (`reply: ""`; log del serve: `idle for 9.98s` justo tras `finish`). Con más
  hilos esa ventana se alarga. Arreglo: `finish` por guardia al final de `do_index_inner`, `finish`
  sella `advanced`, el watchdog cuenta `AppState::last_index_activity` como actividad y `track`
  sella también al terminar el request. 5/5 corridas verdes con paralelismo por defecto (el padre
  falló 3 de 4).
- **Tests nuevos.** Store: `a_frozen_store_refuses_writes_and_leaves_the_wal_alone`,
  `a_freeze_waits_for_a_write_in_progress_up_to_its_budget`,
  `a_transaction_commits_everything_or_nothing`. Index:
  `a_stop_after_the_files_skips_the_rebuilds_and_keeps_them_owed`, `a_stop_during_the_prune_stops_it`,
  `a_run_started_after_the_stop_drops_nothing` (los 3 fallan con el pipeline padre),
  `a_write_attempted_after_the_freeze_never_reaches_the_wal`. API: `the_final_checkpoint_is_claimed_once`,
  `the_grace_override_cannot_outgrow_the_stop_timeline`. Core: `patience_holds_sigkill_off_while_the_process_asks_for_it`,
  `the_poison_lifts_once_the_abandoned_loader_finishes`. CLI: `the_checkpoint_marker_speaks_only_for_its_own_pid`,
  `a_cancelled_index_answer_is_recognised`, `a_cancelled_answer_is_not_a_success`,
  `devctx_index_fails_when_the_server_cancels_its_run` (falla en el padre: sale 0). Reescrito
  `a_forced_exit_mid_index_leaves_a_sound_database`: `--full` sobre un índice existente, atascado
  DENTRO de las escrituras de un archivo (seam `DEVCTX_TEST_STALL_IN_WRITE`), verifica sin `--full`
  (`Store::files_missing_vectors` vacío + incremental sin "Failed to delete"); con la estructura
  padre (autocommits) falla con `[("master","m003.rs")]`.
- **No verificado:** un checkpoint final real de >6 s con `serve --stop` de punta a punta (la
  paciencia está probada en `procown` y el marcador en `remote`); EIO real de drvfs; reconstrucción
  HNSW con la extensión VSS ausente (la rama `Ok(false)` no se ejercita en tests).
