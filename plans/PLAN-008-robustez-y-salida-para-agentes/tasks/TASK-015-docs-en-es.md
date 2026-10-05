# TASK-015 — Docs EN + ES

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** docs (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-013, TASK-014, TASK-007, TASK-005
- **Estado:** `pending`

---

## Objetivo

Documentar en inglés y español lo que cambia para el usuario y para el agente.

## Contexto verificado

- Docs EN en `docs/` (p. ej. `docs/03-core-concepts/mcp-integration.md`, `search.md`,
  `context-builder.md`, `symbol-graph.md`, `docs/11-configuration.md`,
  `docs/13-keeping-the-index-fresh.md`); traducción en `docs/es/`.
- `AGENTS.md` ("Things that will bite you") y `MEMORY-PROTOCOL.md` (embebido en el binario,
  `crates/devctx-mcp/src/lib.rs`).
- `CHANGELOG` del release.

## Pasos

- [ ] **Paso 1 — Ciclo de vida.** MCP sin DB propio, `serve.log`, errores de lock con PID, reconexión,
      `mcp.binary_replaced` y "reiniciá la sesión" (mcp-integration + AGENTS.md).
- [ ] **Paso 2 — Hooks.** Bloque nuevo con `env -u`, reinstalar con `devctx hooks install`, y qué se
      indexa al commitear desde un worktree (`13-keeping-the-index-fresh`).
- [ ] **Paso 3 — Honestidad del índice.** `branch_fallback`, `extractor_stale`, `index --full`.
- [ ] **Paso 4 — Tools.** `project`/`limit`/`offset`/`omitted`, `active_only`, `kind`/
      `include_tests`, penalizaciones, `default_excludes` (`11-configuration`), `build_context` en
      grupo.
- [ ] **Paso 5 — CHANGELOG** de 0.8.3 y 0.9.0 (o el que decida PLAN-008 Q-1).

## Criterios de aceptación

- [ ] Cada campo JSON nuevo de PLAN-008 aparece en EN y ES.
- [ ] Los ejemplos de config parsean (copiar a un `config.yaml` de prueba y `devctx status`).

## Riesgos

Docs ES desfasadas respecto de EN: hacer ambas en el mismo commit.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
