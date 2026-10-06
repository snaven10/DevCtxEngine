# TASK-003 — Ciclo de vida del MCP: salida en EOF y aviso de versión desfasada

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Fijar con tests el ciclo de vida acordado (PLAN-008 D2): el MCP sale cuando su cliente cierra
stdin, **no** sale cuando su binario cambia, y en cambio lo dice en `index_status` y
`list_projects` para que el agente sugiera reiniciar la sesión.

## Contexto verificado

- `crates/devctx-mcp/src/lib.rs:1296-1302` — `serve_stdio_bound`: `serve(rmcp::transport::stdio())`
  + `service.waiting().await`. rmcp cierra el servicio en EOF de stdin.
- `lib.rs:1314-1319` — `run_stdio_bound` crea un runtime multi-thread; su `Drop` espera las tareas
  `spawn_blocking` en curso (`lib.rs:1251-1262`, `blocking`): un `index_repo` o `recall` federado
  largo mantiene vivo el proceso después del EOF. **No hay test que fije la salida.**
- `lib.rs:1166` — única versión expuesta: `Implementation::new("devctx", CARGO_PKG_VERSION)`.
- `crates/devctx-mcp/src/state.rs:704` (`do_index_status`), `state.rs:2126` (`do_list_projects`).
  `do_list_projects` ya lee `DEVCTX_UPDATE_AVAILABLE` (PLAN-006 TASK-002).
- Campo: tres `devctx mcp` con binario `(deleted)` vivos desde el día anterior; no se sabe si sus
  clientes seguían conectados.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs` (`run_stdio_bound`, `index_status`, `list_projects`)
- **Modificar:** `crates/devctx-mcp/src/state.rs` (campos nuevos en las respuestas)
- **Tests:** `crates/devctx-cli/tests/mcp_binding.rs` o `mcp_tools.rs`

## Pasos

- [x] **Paso 1 — Verificar EOF.** Test: lanzar `devctx mcp`, hacer `initialize`, cerrar stdin → el
      proceso sale en < 3 s. Repetir con una tool bloqueante en curso (p. ej. `recall` federado contra
      un repo lento simulado) y fijar el tope.
- [x] **Paso 2 — Tope de salida.** Si el paso 1 muestra que el runtime espera indefinidamente:
      `rt.shutdown_timeout(Duration::from_secs(5))` tras `block_on`, y log a stderr de qué se abandonó.
- [x] **Paso 3 — Versión desfasada.** Al arrancar, guardar `CARGO_PKG_VERSION` y la ruta del binario.
      En `index_status` y `list_projects` agregar `"mcp": {"version", "binary_replaced": bool,
      "installed_version"?}`. `binary_replaced` = el exe resuelve a `(deleted)` o el mtime del archivo
      instalado es posterior al arranque; `installed_version` solo si se puede obtener barato
      (`<exe> --version` con timeout corto, cacheado). Si está desfasado, una línea `hint`: "this
      session runs devctx X; Y is installed — restart the AI session to pick it up".
- [x] **Paso 4.** Confirmar que nada en el MCP sale ni se reinicia por el cambio del binario.

## Criterios de aceptación

- [x] Test de EOF verde (con y sin tarea bloqueante).
- [x] Test: MCP lanzado desde una copia del binario que luego se reemplaza → `index_status` reporta
      `binary_replaced: true` y el proceso sigue contestando.
- [x] El handshake no se demora (sin llamadas de red ni `--version` en el camino de `initialize`).

## Riesgos

`<exe> --version` sobre un binario nuevo puede tardar o colgarse: timeout corto y cache.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes (verificado quitando el tope): `the_mcp_exits_after_eof_even_with_a_blocking_tool_in_flight`
   (serve congelado con SIGSTOP, `index_status` en vuelo, stdin cerrado) -> el MCP seguía vivo a los 30 s.
   Sin tarea en vuelo ya salía bien (`the_mcp_exits_when_its_client_closes_stdin`, < 3 s). Después: sale a ~6 s.
3. **Causa raíz:** confirmada (hipótesis del Paso 1). Hay dos esperas encadenadas tras el EOF: rmcp
   drena las respuestas en vuelo hasta 5 s (`QuitReason::Closed`; razonable, un `echo '{...}' | devctx mcp`
   sigue recibiendo su respuesta) y luego el `Drop` del runtime espera las `spawn_blocking` sin tope.
   Se acota lo segundo con `rt.shutdown_timeout(SHUTDOWN_GRACE = 1 s)`: peor caso EOF -> salida ≈ 6 s
   (el plan decía 5 s de tope propio; se usó 1 s porque rmcp ya consume 5).
4. **Archivos y símbolos.**
   - Nuevo `crates/devctx-mcp/src/lifecycle.rs`: `pub fn init()`, `pub fn mcp_json() -> serde_json::Value`
     (captura versión, exe y mtime al arrancar con dos `stat`; `<exe> --version` solo si el binario fue
     reemplazado, timeout 2 s, cacheado en `OnceLock`, fuera del camino de `initialize`).
   - `devctx-mcp/src/lib.rs`: `pub mod lifecycle`; `serve_stdio_bound` llama `lifecycle::init()`;
     `run_stdio_bound` aplica `shutdown_timeout`; `DevctxServer::mcp_info()`; `index_status` y
     `list_projects` agregan la clave `mcp`.
   - `devctx-central/src/client.rs` (`spawn`): `current_exe()` -> `devctx_core::self_exe()`. Era el mismo
     defecto de TASK-001: tras reemplazar el binario, el MCP no podía levantar el central daemon
     (`list_projects` fallaba con "no central store daemon"). Lo destapó el test de reemplazo.
5. **Tests nuevos** (`crates/devctx-cli/tests/mcp_binding.rs`): `the_mcp_exits_when_its_client_closes_stdin`,
   `the_mcp_exits_after_eof_even_with_a_blocking_tool_in_flight`, `a_replaced_binary_is_reported_and_the_mcp_keeps_answering`.
   Unitarios (`lifecycle::tests`): `parses_the_clap_version_line`, `current_binary_is_not_replaced`,
   `a_replaced_binary_suggests_restarting_with_both_versions`, `a_replaced_binary_with_unknown_installed_version_still_hints`,
   `a_hanging_probe_gives_up`. Comandos y resultados:
   `cargo test -p devctx-mcp -p devctx-central` -> mcp 46 unit (+5 de `lifecycle`) y 19 integ., central 4: 0 fallos;
   `cargo test -p devctx-cli --test mcp_binding --test mcp_tools --test plan_status_cli` -> 26 / 8 (1 ignored, previo) / 9: 0 fallos;
   `cargo clippy --all-targets -p devctx-mcp -p devctx-central -p devctx-cli` sin warnings; `cargo fmt --all` aplicado.
6. **Contrato JSON:** `index_status` y `list_projects` agregan
   `"mcp": {"version": "0.8.2", "binary_replaced": false}`; si el binario fue reemplazado además
   `"installed_version"` (solo si `<exe> --version` respondió en 2 s) y `"hint"` ("this session runs devctx X;
   Y is installed — restart the AI session to pick it up"). `binary_replaced` = el exe termina en
   ` (deleted)` o el mtime del archivo cambió desde el arranque. Nada sale ni se reinicia por el cambio.
7. **No verificado:** plataformas sin `/proc` (el sufijo `(deleted)` no existe; queda solo el mtime);
   reemplazo "in place" con el mismo mtime; los `current_exe()` de `devctx-mcp/src/state.rs` (TASK-008),
   `devctx-tui`, `devctx-api/src/central.rs`, `hooks.rs`, `main.rs:1614`, siguen sin `self_exe`.
8. **Números:** EOF sin tarea en vuelo < 3 s; con tarea bloqueante en vuelo ≈ 6 s (5 s rmcp + 1 s runtime).
