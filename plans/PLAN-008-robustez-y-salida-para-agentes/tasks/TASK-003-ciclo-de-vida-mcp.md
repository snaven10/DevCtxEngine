# TASK-003 — Ciclo de vida del MCP: salida en EOF y aviso de versión desfasada

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

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

- [ ] **Paso 1 — Verificar EOF.** Test: lanzar `devctx mcp`, hacer `initialize`, cerrar stdin → el
      proceso sale en < 3 s. Repetir con una tool bloqueante en curso (p. ej. `recall` federado contra
      un repo lento simulado) y fijar el tope.
- [ ] **Paso 2 — Tope de salida.** Si el paso 1 muestra que el runtime espera indefinidamente:
      `rt.shutdown_timeout(Duration::from_secs(5))` tras `block_on`, y log a stderr de qué se abandonó.
- [ ] **Paso 3 — Versión desfasada.** Al arrancar, guardar `CARGO_PKG_VERSION` y la ruta del binario.
      En `index_status` y `list_projects` agregar `"mcp": {"version", "binary_replaced": bool,
      "installed_version"?}`. `binary_replaced` = el exe resuelve a `(deleted)` o el mtime del archivo
      instalado es posterior al arranque; `installed_version` solo si se puede obtener barato
      (`<exe> --version` con timeout corto, cacheado). Si está desfasado, una línea `hint`: "this
      session runs devctx X; Y is installed — restart the AI session to pick it up".
- [ ] **Paso 4.** Confirmar que nada en el MCP sale ni se reinicia por el cambio del binario.

## Criterios de aceptación

- [ ] Test de EOF verde (con y sin tarea bloqueante).
- [ ] Test: MCP lanzado desde una copia del binario que luego se reemplaza → `index_status` reporta
      `binary_replaced: true` y el proceso sigue contestando.
- [ ] El handshake no se demora (sin llamadas de red ni `--version` en el camino de `initialize`).

## Riesgos

`<exe> --version` sobre un binario nuevo puede tardar o colgarse: timeout corto y cache.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
