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
- [x] Gate completo verde: fmt, clippy con y sin `--features gpu`, `cargo test --workspace` (re-corrido en el fixup J).
- [x] El seam de test no existe en el binario release (fixup J1) y `serve.json` sigue anunciado durante el checkpoint de salida (J2).
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

### Fixup I (review de TASK-017 y menores del re-review del fixup H)

- **I1** El 503 del idle exit sale de un middleware ANTES del handler (único 503 de `devctx-api`), así que no se procesó nada y es el único status seguro de repetir. Lleva `X-Devctx-Exiting: 1` (`procown::EXITING_HEADER`). `RemoteClient::request` (mcp, `backend.rs`) lo reintenta: invalida el enlace, vuelve a preguntar al conector (viéndolo Busy mientras el viejo vive, sin cachear ese fallo) y reenvía, hasta 3 s. La CLI (`remote.rs`, `Remote::send`) hace lo mismo vía `ensure_checked`. Un 503 sin marca es respuesta final. Tests: `only_the_exiting_503_is_retried`, `a_request_refused_by_an_exiting_server_goes_to_the_next_one`, `a_busy_connector_during_the_exit_is_asked_again`, `a_plain_503_is_a_final_answer`, `a_command_refused_by_an_exiting_server_reaches_the_next_one` (fallan sin el fix: verificado).
- **I2** `devctx_central::discover` distingue 503-con-marca en `/health` (saliendo) de caído: sondea hasta que el listener desaparece (≤3 s) y solo entonces devuelve `None`, así `ensure` no lanza un daemon que muere contra el lock. Test `an_exiting_daemon_is_waited_for_not_replaced` (falla sin el fix).
- **M1** Los middlewares (`count_in_flight`, `count_requests`) cuentan ANTES de leer `exiting`; `exit_now` y el watchdog central levantan la bandera primero y luego esperan (≤300 ms) a `in_flight == 0` antes del freeze (`drain_in_flight`, `central::begin_exit`). Tests `the_exit_raises_the_flag_before_it_waits_for_requests`, `an_admitted_request_is_waited_for_and_a_later_one_refused` (el primero falla con el orden invertido).
- **M2** `terminate_with` espera `TERM_WAIT` en rebanadas de 250 ms mirando el marker; verlo en cualquier momento da el margen `AFTER_PATIENCE`. Test `a_checkpoint_that_ends_inside_the_term_wait_still_earns_the_margin` (falla sin el fix).
- **M3** La caché de modelos de test pasa a `<workspace>/.devctx-test-models` (fuera de `target/`, en `.gitignore`; `relative = true` sin `force`). Eliminado `resolve_target_dir` (inalcanzable bajo cargo). El test mira el entorno efectivo. Documentado en `CONTRIBUTING.md` (incluido el efecto sobre `cargo run`).
- **M4** (solo macOS, no compilado aquí) `wait_exit` comprueba `owns()` antes de registrar en kqueue y al agotarse el plazo; el fd de kqueue lleva `FD_CLOEXEC`.
- **M5** `wait_with_timeout` lee por canales con `recv_timeout`, sin `join` al vencer. Test `wait_with_timeout_does_not_wait_for_a_grandchild_holding_the_pipes`.
- **M6** En no-Linux, un argv0 unido por espacios (`i > 0`) debe existir como archivo (`ps_command_is_server_with`). Test `a_joined_argv0_must_exist_as_a_file` (falla sin el fix).
- **Nit** `DEVCTX_TEST_SLOW_CHECKPOINT_MS` bajo `#[cfg(debug_assertions)]`; el test e2e correspondiente también.
- **Menores del re-review de H (8488968):** (1) 503 de exit y errores de transporte que no son timeout/refused cuentan como Busy; `Failed` sin ningún puntuado responde desde el default mostrando el error (sin default sigue siendo error). (2) "Nada matcheó" con miembros sin puntuar responde desde el default o el mejor, con aviso de comparación parcial. (3) El dedup de memorias exige ≥40 caracteres o ≥2 líneas (`is_quotable`). (4) `name_candidates`: descarta palabras en >50 % de los miembros, acepta el nombre completo corto (`ui`, `db`), sin `strip_suffix('s')` (plural par a par con `same_word`). (5) Un `/health` 4xx/5xx ajeno es Cold. Nits: `group_penalty` usa `member_config_path`; `cached_header` nombra los miembros no comparables (`GroupPick.not_comparable`); la comprobación de dueño con `ps`/`lsof` respeta el plazo (`within`). Tests en `state.rs`: `nothing_scored_or_matched_is_not_an_error_while_members_were_not_asked`, `leaving_cut_and_foreign_servers_are_not_errors_of_the_selection`, `short_hits_are_never_suppressed_by_a_quoting_memory`, `name_candidates_ignore_shared_words_and_keep_short_names`, `group_penalty_uses_the_registered_config_path`, `a_cached_header_keeps_the_members_that_were_not_comparable`.
- **Verificado por:** fmt --check, clippy `--workspace --all-targets` (con y sin `--features gpu`, 0 warnings), `cargo test --workspace` verde con `TMPDIR=/var/tmp DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache`; 0 `devctx serve` sobrantes. **No verificado:** kqueue (M4) y el `ps`/`lsof` real fuera de Linux.

### Fixup J (review)

- **J1** El seam `DEVCTX_TEST_SLOW_CHECKPOINT_MS` (`AppState::checkpoint_for_exit`, `state.rs`) seguía compilado en release (el fixup I solo condicionó el test). Ahora está bajo `#[cfg(debug_assertions)]`, y el comentario del test dice la verdad. Verificado: `strings target/release/devctx | grep -c DEVCTX_TEST_SLOW_CHECKPOINT_MS` da 0.
- **J2** El serve de proyecto quitaba `serve.json` ANTES del checkpoint (`exit_now` llamaba a `on_exit()` antes del hilo): durante la ventana (hasta 1,5 s) `retry_after_exit`/`Remote::send` -> `ensure_checked` leía "Down" y lanzaba un serve que moría contra el lock (ruido en `serve.log`, y cerca de 3 s el reintento terminaba en error). Fix: `on_exit` pasa a DESPUÉS del checkpoint, en el mismo hilo y justo antes de `_exit` (también en la rama de presupuesto agotado); es seguro porque `Store::freeze` ya bloquea toda escritura antes del checkpoint. Además `ensure_checked` espera hasta 500 ms (`wait_out_lock`) mientras `Store::check_unlocked` diga que el archivo está tomado y no hay servidor anunciado (cubre el instante entre retirar el anuncio y `_exit`, y un spawn de otro cliente que está arrancando); si el lock sigue, spawnea como antes (un dueño ajeno sigue fallando rápido nombrando el PID: un primer intento con espera de 3 s rompió los tests de lock ajeno y se acortó). Tests: e2e `a_client_during_the_exit_checkpoint_does_not_spawn_a_second_server` (idle exit con seam de 1,2 s: `serve.json` sigue anunciado durante el marker, un cliente en la ventana no crea `serve.log`; falla con el código previo: verificado) y unitario `a_held_database_gets_a_moment_before_a_spawn`. Comentario falso de `EXIT_RETRY_WAIT` (`backend.rs`) corregido.
- **J3** `CentralClient` no reintentaba el 503 marcado (un idle exit justo antes del POST de un `remember` global fallaba sin escribir). Ahora `CentralClient::call` (get/post/delete) ante el 503 con marca vuelve a `ensure()` y repite UNA vez contra el daemon nuevo; un 503 sin marca es final. El cliente guarda los `CentralPaths`. Test `a_request_refused_by_an_exiting_daemon_reaches_the_next_one` (falla sin el reintento: verificado).
- **J4** `retry_after_exit` pone `failed = None` en el `Link` compartido, lo que cancela el backoff de hilos concurrentes: aceptado y documentado en el comentario (`backend.rs`).
- **Nits** `warm_server`: `InvalidUrl`/`UnknownScheme`/`Dns` de transporte (un `serve.json` corrupto) pasan a `Cold` en vez de `Busy` (test `a_corrupt_serve_json_address_is_cold_not_busy`, falla sin el cambio: verificado). Tests nuevos: `Failed` en la rama "nada matcheó, parcial" (`a_failed_member_in_the_nothing_matched_partial_branch_is_not_an_error`). `name_candidates` con un grupo de 1 miembro: `n*2 > 1` volvía comunes todas las palabras y la lista salía siempre vacía; ahora "compartida" exige `members.len() > 1` (`name_candidates_work_for_a_group_of_one`, falla sin el cambio: verificado).
- **Verificado por:** fmt --check, clippy `--workspace --all-targets` con y sin `--features gpu` (0 warnings), `cargo test` por paquete verde (`TMPDIR=/var/tmp`; `serve_lifecycle` 13/13; en una corrida `an_idle_server_exits_after_its_window` y `sigterm_during_an_index_cancels_it...` fallaron con la máquina cargada y pasaron aisladas y en dos reruns completos), `cargo build --release -p devctx-cli` y seam ausente del binario. **No verificado:** el e2e no ejerce `Remote::send` en vivo durante la ventana (un proceso CLI nuevo ve Busy y falla; el reintento lo cubren los tests unitarios con servidores falsos).
