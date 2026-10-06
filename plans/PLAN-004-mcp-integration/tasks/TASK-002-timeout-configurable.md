# TASK-002 — Timeout configurable por llamada al daemon

- **Plan:** PLAN-004 — MCP integration hardening
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `fix/mcp-integration`
- **Depende de:** — (paralela a TASK-001)
- **Estado:** `done`

---

## Objetivo

`RemoteClient::agent()` aplicaba un timeout fijo de 3600s a *toda* llamada HTTP enrutada —
`search`, `read_file`, `recall`, no solo `index` — así que un daemon colgado o una red caída
dejaban al caller esperando una hora para enterarse.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/backend.rs`, `docs/11-configuration.md`

## Resultado

- **Estado final:** `done`
- **Resumen:** Timeout configurable vía `DEVCTX_DAEMON_TIMEOUT_SECS` (default 120s) para la
  mayoría de las llamadas; `index_repo` se queda en un timeout largo fijo (1800s), porque indexar
  de verdad puede tardar minutos.
- **Archivos tocados:** `crates/devctx-mcp/src/backend.rs`, `docs/11-configuration.md`
- **Verificado por:** Compila y pasa la suite (339 tests), clippy limpio, commit `c7d0b82`.
- **Desviaciones:** Ninguna registrada en el cierre del plan.
