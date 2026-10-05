# TASK-017 — Pendientes del review de P0

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-008-p1`
- **Depende de:** —
- **Estado:** `pending`

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

- [ ] **1.** Antes del freeze de un idle exit, cerrar el listener (o responder 503 a todo request
      nuevo) en el serve y en el central. Test: un request que llega durante el idle exit recibe
      503 o conexión rechazada, nunca un `Frozen` a medias.
- [ ] **2.** Guard RAII que resetea `in_tx` (y hace `ROLLBACK`) aunque `f` haga panic. Test con
      `catch_unwind`.
- [ ] **3.** Fuera de Linux, el binario `devctx*` tiene que ser argv0 (o el primer token que
      resuelve a un ejecutable), no cualquier argumento. Test de parsing puro.
- [ ] **4.** Borrado de rama en una sola transacción. Test: un error a mitad no deja la rama a
      medio borrar.
- [ ] **5.** Timeout en el test (hilo con `wait_timeout` o similar) para que un CLI colgado falle
      el test en vez de colgar la suite.
- [ ] **6.** `init` interactivo con un modelo que necesita archivos + offline: no guarda un
      `model_dir` vacío; avisa y deja el comando de descarga. Test.
- [ ] **7.** kqueue en macOS para esperar la salida real (cfg `target_os = "macos"`); el resto
      fuera de Linux sigue con el sondeo. Verificado por el cross-check de CI (compila); sin test de
      comportamiento local.
- [ ] **8.** La guardia de la caché cubre todos los tests del workspace (también los `#[ignore]`) y
      resuelve `CARGO_TARGET_DIR` relativo y `build.target-dir`. Test.
- [ ] **9.** Test e2e: `serve --stop` contra un serve cuyo checkpoint final dura más de 6 s
      (seam de test que alarga el checkpoint) → `stop` espera y el serve sale limpio.

## Criterios de aceptación

- [ ] Cada ítem con su test, y los tests de 1, 2, 4, 5 y 6 fallan con el código previo.
- [ ] Gate completo verde: fmt, clippy con y sin `--features gpu`, `cargo test --workspace`.
- [ ] Cross-check de CI (Windows y macOS) verde.
- [ ] Ningún `target/debug/devctx serve` sobrante tras la suite.

## Resultado

- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
