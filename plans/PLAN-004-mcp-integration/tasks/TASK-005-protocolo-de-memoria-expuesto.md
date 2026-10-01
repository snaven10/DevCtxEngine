# TASK-005 — El protocolo de memoria se expone por MCP

- **Plan:** PLAN-004 — MCP integration hardening
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-integration`
- **Depende de:** — (paralela a TASK-001, TASK-002, TASK-003, TASK-004)
- **Estado:** `done`

---

## Objetivo

El protocolo de memoria vivía en `MEMORY-PROTOCOL.md` y solo llegaba a un agente si alguien lo
pegaba a mano en `CLAUDE.md`.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-cli/tests/mcp_tools.rs`

## Resultado

- **Estado final:** `done`
- **Resumen:** `MEMORY-PROTOCOL.md` embebido (`include_str!`) y servido como prompt
  `memory-protocol` y como recurso `devctx://memory-protocol`.
- **Archivos tocados:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-cli/tests/mcp_tools.rs`
- **Verificado por:** Compila y pasa la suite (339 tests), clippy limpio, commit `c7d0b82`.
- **Desviaciones:** Ninguna registrada en el cierre del plan.
