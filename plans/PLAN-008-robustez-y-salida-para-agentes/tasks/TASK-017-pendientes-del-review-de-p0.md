# TASK-017 — Pendientes del review de P0

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-008-p1`
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Cerrar lo que el review de P0 dejó anotado en "Pendientes para P1" de PLAN-008. Ninguno era
bloqueante de 0.8.3/0.8.4, pero todos son defectos reales del ciclo de vida, de la atomicidad del
store o de la higiene de tests. Agregada a P1 por decisión del usuario (2026-10-04).

## Contexto verificado

Los ítems y su evidencia están en "Pendientes para P1" de PLAN-008 (líneas tras §12). Resumen:

1. **Idle exit con el listener abierto.** El idle watchdog del serve (`devctx-api/src/lib.rs`) y el
   del central (`devctx-api/src/central.rs`) congelan y hacen checkpoint mientras axum todavía
   acepta requests. `e4f0cc9` cerró la pérdida de datos; la ventana sigue.
2. **`in_tx` tras un panic.** Si `f` hace panic dentro de `Store::in_transaction`
   (`devctx-store/src/store.rs`), `in_tx` queda en `true` y las transacciones siguientes corren sin
   `BEGIN` propio.
3. **`ps_command_is_server` fuera de Linux** (`devctx-core/src/procown.rs`) acepta un token
   `devctx*` en cualquier posición (`/usr/bin/vim /x/devctx serve`).
4. **Borrado de rama en `memory_refs.rs` (~430)** sin transacción.
5. **`devctx_index_fails_when_the_server_cancels_its_run`** usa `wait_with_output()` sin timeout.
6. **`init` interactivo con granite + offline** guarda un `model_dir` vacío.
7. **m-2 (macOS):** `Handle::wait_exit` fuera de Linux sondea `!owns()`; debería usar kqueue
   (`EVFILT_PROC` / `NOTE_EXIT`).
8. **m-4:** la guardia de la caché de modelos de tests no cubre tests fuera de `devctx-cli/tests`
   (el `#[ignore]` de minilm usa el `model_cache_dir()` real), ni un `CARGO_TARGET_DIR` relativo,
   ni `build.target-dir` de `.cargo/config`.
9. **D1b:** no hay test e2e de `serve --stop` contra un checkpoint final real de más de 6 s.

## Pasos

- [x] **1.** Antes del freeze de un idle exit, cerrar el listener (o responder 503 a todo request
      nuevo) en el serve y en el central. Test: un request que llega durante el idle exit recibe
      503 o conexión rechazada, nunca un `Frozen` a medias.
- [x] **2.** Guard RAII que resetea `in_tx` (y hace `ROLLBACK`) aunque `f` haga panic. Test con
      `catch_unwind`.
- [x] **3.** Fuera de Linux, el binario `devctx*` tiene que ser argv0 (o el primer token que
      resuelve a un ejecutable), no cualquier argumento. Test de parsing puro.
- [x] **4.** Borrado de rama en una sola transacción. Test: un error a mitad no deja la rama a
      medio borrar.
- [x] **5.** Timeout en el test (hilo con `wait_timeout` o similar) para que un CLI colgado falle
      el test en vez de colgar la suite.
- [x] **6.** `init` interactivo con un modelo que necesita archivos + offline: no guarda un
      `model_dir` vacío; avisa y deja el comando de descarga. Test.
- [x] **7.** kqueue en macOS para esperar la salida real (cfg `target_os = "macos"`); el resto
      fuera de Linux sigue con el sondeo. Verificado por el cross-check de CI (compila); sin test de
      comportamiento local.
- [x] **8.** La guardia de la caché cubre todos los tests del workspace (también los `#[ignore]`) y
      resuelve `CARGO_TARGET_DIR` relativo y `build.target-dir`. Test.
- [x] **9.** Test e2e: `serve --stop` contra un serve cuyo checkpoint final dura más de 6 s
      (seam de test que alarga el checkpoint) → `stop` espera y el serve sale limpio.

## Criterios de aceptación

- [x] Cada ítem con su test, y los tests de 1, 2, 4, 5 y 6 fallan con el código previo.
- [x] Gate completo verde: fmt, clippy con y sin `--features gpu`, `cargo test --workspace`.
- [ ] Cross-check de CI (Windows y macOS) verde.
- [ ] Ningún `target/debug/devctx serve` sobrante tras la suite.

## Resultado

- **Estado final:** `done`. El cross-check de CI (Windows/macOS) queda pendiente del push a `fix/**`; el kqueue de macOS solo se verifica compilando allí.
- **Resumen:**
  1. Idle exit: `count_in_flight` (serve, `lib.rs`) y `refuse_while_exiting` (central, `central.rs`) responden 503 desde que empieza el exit, antes del freeze. Tests `requests_during_an_exit_get_503` y `requests_during_the_idle_exit_get_503` (fallan sin el chequeo: verificado).
  2. `Store::in_transaction` usa un guard RAII (`TxGuard`) que hace ROLLBACK y limpia `in_tx` aunque `f` haga panic. Test `a_panic_inside_a_transaction_rolls_back_and_resets_in_tx` (falla sin el reset: verificado).
  3. `ps_command_is_server` fuera de Linux: un token posterior que empieza con `/` no continúa la ruta de argv0. Test de parsing puro (`/usr/bin/vim /x/devctx serve`).
  4. `drop_branch` en una transacción. Test `dropping_a_branch_that_fails_half_way_leaves_it_whole` (falla sin la transacción: verificado).
  5. `common::wait_with_timeout` y su uso en `devctx_index_fails_when_the_server_cancels_its_run`; test `a_hung_child_fails_the_wait_instead_of_hanging_the_suite` (el helper no existía antes: no hay forma de "fallar" sin él).
  6. `cmd_init` decide el `model_dir` con `model_files` para cualquier modelo (elegido, copiado o heredado del default de la máquina): nunca vacío para un modelo con archivos; si no puede descargarse, falla nombrando `devctx models --download`. Tests `init_never_writes_an_empty_model_dir_for_a_model_that_needs_files` (e2e, falla sin la lógica: verificado) y `a_model_that_needs_files_never_gets_an_empty_model_dir`. Nota: no encontré un camino interactivo que lo produjera con offline elegido en el wizard (ahí `choose_model` ya abortaba); el caso real reproducible era un default de máquina con `model_dir` vacío, que `--yes` e interactivo heredaban.
  7. `kqueue_wait_exit` (`EVFILT_PROC`/`NOTE_EXIT`) bajo `cfg(target_os = "macos")`; si kqueue no sirve cae al sondeo.
  8. `.cargo/config.toml` fija `DEVCTX_MODEL_CACHE` (`[env]`, relativo al workspace) para todos los tests del workspace, `#[ignore]` incluidos; `resolve_target_dir` resuelve `CARGO_TARGET_DIR` relativo y deduce `build.target-dir` desde la ubicación del binario de test. Tests `the_test_model_cache_is_pinned_for_the_whole_workspace` y `under_cargo_test_the_model_cache_is_not_the_users_real_one`. Efecto: `cargo run` también ve esa caché salvo que se defina `DEVCTX_MODEL_CACHE`.
  9. Seam `DEVCTX_TEST_SLOW_CHECKPOINT_MS` en `AppState::checkpoint_for_exit`; test e2e `serve_stop_waits_for_a_final_checkpoint_longer_than_its_fixed_wait` (checkpoint de 9 s).
- **Bug real encontrado por el e2e del ítem 9:** el cliente de `serve --stop` mandaba SIGKILL en cuanto el marker de checkpoint desaparecía, con el servidor todavía saliendo (status 9 intermitente, ~1 de 3). Fix en `procown::terminate_with`: tras una espera paciente, `AFTER_PATIENCE` (2 s) antes de SIGKILL. Test `a_finished_checkpoint_is_given_a_moment_to_exit_before_sigkill`. El e2e pasó 13/13 después.
- **Archivos tocados:** `crates/devctx-api/src/{lib,central}.rs`, `crates/devctx-store/src/{store,memory_refs}.rs`, `crates/devctx-core/src/{procown,dirs}.rs`, `crates/devctx-mcp/src/state.rs`, `crates/devctx-cli/src/main.rs`, `crates/devctx-cli/tests/{common/mod,projects_cli,serve_lifecycle}.rs`, `.cargo/config.toml`.
- **Verificado por:** `cargo fmt --all --check`; `cargo clippy --workspace --all-targets` y con `--features gpu` (0 warnings); `cargo test --workspace` (lib/bins, `devctx-cli --tests`, doc) con `TMPDIR=/var/tmp DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache`, todo verde; 0 procesos `devctx serve` sobrantes. Fallo en el código previo comprobado revirtiendo la lógica para los ítems 1, 2, 4 y 6. **No verificado:** kqueue (solo compila en CI de macOS), la ruta interactiva de `init` con terminal real, el cross-check de Windows.
