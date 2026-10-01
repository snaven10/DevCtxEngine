# TASK-004 — Recorte del costo de contexto de las descripciones de tools

- **Plan:** PLAN-004 — MCP integration hardening
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-integration`
- **Depende de:** — (paralela a TASK-001, TASK-002, TASK-003)
- **Estado:** `done`

---

## Objetivo

El párrafo de ~230 caracteres sobre el parámetro `project` estaba repetido 7 veces; varias
descripciones de tool eran más largas de lo que su contenido informativo justificaba.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`

## Resultado

- **Estado final:** `done`
- **Resumen:** El párrafo de `project` se recortó a una oración por sitio; la explicación completa
  se movió a `instructions`. `remember`/`recall`/`use_project`/`memories_by_symbol`/`memory_move`/
  `list_projects` se recortaron sin perder el comportamiento esencial.
- **Archivos tocados:** `crates/devctx-mcp/src/lib.rs`
- **Verificado por:** Compila y pasa la suite (339 tests), clippy limpio, commit `c7d0b82`. Medido:
  9729 → 7385 caracteres (-24%) en las descripciones de tools.
- **Desviaciones:** Ninguna registrada en el cierre del plan.
