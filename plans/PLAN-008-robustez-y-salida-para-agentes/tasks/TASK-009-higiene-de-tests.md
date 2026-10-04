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
