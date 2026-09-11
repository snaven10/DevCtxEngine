# TASK-003 — Resultados vacíos silenciosos pasan a ser error

- **Plan:** PLAN-004 — MCP integration hardening
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-integration`
- **Depende de:** — (paralela a TASK-001, TASK-002)
- **Estado:** `done`

---

## Objetivo

Precedente: commit `9590e40`, `local_recall` leía `/recall` con `as_array()` cuando el endpoint
responde `{"memories": [...]}`, y el mismatch se leía como "sin memorias" en vez de como error.
`memories_of` tenía el mismo riesgo: forma inesperada → vacío en vez de forma inesperada → error.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs`, `crates/devctx-cli/tests/projects_cli.rs`

## Resultado

- **Estado final:** `done`
- **Resumen:** `memories_of` pasa de "forma inesperada → vacío" a "forma inesperada → error"; test
  unitario nuevo para la forma inesperada; se revisó el test `#[ignore]`d de `projects_cli.rs`.
- **Archivos tocados:** `crates/devctx-cli/src/main.rs`, `crates/devctx-cli/tests/projects_cli.rs`
- **Verificado por:** Compila y pasa la suite (339 tests), clippy limpio, commit `c7d0b82`.
- **Desviaciones:** Ninguna registrada en el cierre del plan.
