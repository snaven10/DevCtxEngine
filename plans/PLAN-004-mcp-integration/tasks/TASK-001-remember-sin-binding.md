# TASK-001 — `remember` sin proyecto bindeado guarda como `global`

- **Plan:** PLAN-004 — MCP integration hardening
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-integration`
- **Depende de:** — (primera del plan)
- **Estado:** `done`

---

## Objetivo

`backend_for` devolvía `Binding::None` → `invalid_request(unbound_help)` antes de que `remember`
mirara el `scope` pedido — a diferencia de `recall`, que sí cae a `do_recall_global`. Un `scope:
global` explícito fallaba igual que uno implícito, y el contenido de la memoria se tiraba.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`,
  `crates/devctx-cli/tests/mcp_binding.rs`

## Resultado

- **Estado final:** `done`
- **Resumen:** `remember` sin proyecto bindeado guarda en el store central como `global` en vez de
  fallar. `scope: local` explícito sigue fallando, pero ahora el mensaje de error lleva el
  contenido para no perderlo.
- **Archivos tocados:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`,
  `crates/devctx-cli/tests/mcp_binding.rs`
- **Verificado por:** Compila y pasa la suite (339 tests), clippy limpio, commit `c7d0b82`. No se
  corrió `cargo build`/`test` durante la escritura original del plan (regla dura del usuario en su
  momento); se verificó después, en la rama `fix/mcp-integration`.
- **Desviaciones:** Ninguna registrada en el cierre del plan.
