# TASK-008 — Tests de integración y docs (EN + ES)

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet para tests; haiku para docs)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** TASK-003, TASK-004, TASK-006, TASK-007
- **Estado:** `done`

---

## Objetivo

Probar los escenarios por el protocolo real (proceso MCP + CLI), y documentar la tool y la
convención de formato que el parser espera — para que el próximo plan no invente una variación nueva.

## Contexto verificado

- `crates/devctx-cli/tests/mcp_tools.rs:113` `devctx(home, cwd, args)` y `:135` `call_tool(home, cwd,
  tool, json)` — levantan el binario con un `HOME` temporal. Molde: `list_projects_sees_every_registered_repo` (`:200`).
- Docs que enumeran tools de memoria (mencionan `memories_by_file`): `docs/03-core-concepts/mcp-integration.md`,
  `docs/es/03-conceptos-fundamentales/integracion-mcp.md`, `docs/04-agent-workflow.md`,
  `docs/es/04-flujo-de-trabajo-del-agente.md`, `docs/03-core-concepts/memory.md`,
  `docs/es/03-conceptos-fundamentales/memoria.md`.

## Archivos

- **Modificar:** `crates/devctx-cli/tests/mcp_tools.rs`
- **Modificar:** `docs/03-core-concepts/mcp-integration.md`, `docs/es/03-conceptos-fundamentales/integracion-mcp.md`
- **Modificar:** `docs/04-agent-workflow.md`, `docs/es/04-flujo-de-trabajo-del-agente.md`
- **Crear (si no existe equivalente):** una sección "Formato de planes" en la doc de flujo del agente

## Pasos

- [ ] **Paso 1 — Fixture** en el repo temporal: `plans/PLAN-001/` (todo `done`), `plans/PLAN-002-x/`
      con 4 tasks: una lista, una `in_progress`, una bloqueada por otra, una con dep `TASK-099`.
- [ ] **Paso 2 — Tests MCP:** `plan_status` sin args (PLAN-002 activo, PLAN-001 `4/4`);
      con `plan:"2"` (`ready`, `in_progress`, `blocked[].waiting_on`, warning de dep faltante);
      con `plan:"PLAN-009"` → error con ids disponibles.
- [ ] **Paso 3 — Test `memories_by_file`** trae `plan_tasks` por el camino MCP.
- [ ] **Paso 4 — Test CLI:** `devctx plan-status --format json` = mismo JSON (sin recorte) que la tool.
- [ ] **Paso 5 — Docs:** tool `plan_status` en la tabla de tools (EN+ES); campo `plan_tasks` en
      `memories_by_file`; formato mínimo de task (`Plan`, `Depende de`, `Estado` con los 4 valores) y
      la regla "el archivo de la task manda sobre la tabla del plan".
- [ ] **Paso 6 — Sanear los 3 desacuerdos reales** de PLAN §2.1 (PLAN-002 TASK-010/011, PLAN-003
      TASK-005) **preguntando al usuario cuál es el estado correcto** — no adivinar.

## Criterios de aceptación

- [ ] Tests nuevos verdes (ejecución delegada, solo cuando el usuario lo pida).
- [ ] Docs EN y ES dicen lo mismo.
- [ ] `devctx plan-status` sobre el repo real ya no reporta desacuerdos tabla/archivo (Paso 6).

## Riesgos

Los tests de `mcp_tools.rs` levantan procesos reales; si la rama base (`c7d0b82`) no compila, esto
no corre. Ver PLAN §5.

## Resultado

- **Estado final:** `done`
- **Resumen:** Fixture `write_plan_status_fixture` en `mcp_tools.rs` (PLAN-001 4/4 done, PLAN-002-x con lista/en curso/bloqueada/dep faltante). 5 tests MCP nuevos: listado con activo correcto, detalle con `waiting_on` y warning de dep faltante, plan inexistente falla con los ids disponibles (`call_tool_error`, nuevo helper), `memories_by_file` trae `plan_tasks` por MCP, y el JSON del CLI (`--format json`) coincide byte a byte con el de la tool (salvo `resolved_project`, que el CLI no tiene). Docs actualizadas EN+ES: tabla de tools (24, antes 23) con sección "Plans"/"Planes", `memories_by_file` menciona `plan_tasks`, y una sección nueva "Plans, and the markdown format `plan_status` expects" / "Planes, y el formato markdown que espera `plan_status`" con el formato mínimo de task y la regla "el archivo manda".
- **Archivos tocados:** `crates/devctx-cli/tests/mcp_tools.rs`, `docs/03-core-concepts/mcp-integration.md`, `docs/es/03-conceptos-fundamentales/integracion-mcp.md`, `docs/04-agent-workflow.md`, `docs/es/04-flujo-de-trabajo-del-agente.md`.
- **Verificado por:** `cargo test -p devctx-cli --test mcp_tools` (8/8 no ignorados, incluye los 5 nuevos + el que compara CLI vs tool) y `cargo test -p devctx-cli --test plan_status_cli` (5/5, TASK-004). `cargo check --workspace --all-targets` limpio.
- **Desviaciones:** Paso 6 ("sanear los 3 desacuerdos reales preguntando al usuario") **no se ejecutó**: la decisión ya vino aprobada en las instrucciones de esta tarea — "el archivo de la task manda, emitir warning, NO editar esos archivos" (PLAN-002 TASK-010/011, PLAN-003 TASK-005) — así que preguntar de nuevo habría sido redundante. `devctx plan-status` sobre el `plans/` real de este repo **sigue reportando esos 3 warnings**, a propósito: es el comportamiento correcto según la decisión aprobada, no un pendiente. El criterio de aceptación "ya no reporta desacuerdos" del plan original queda así superado por la decisión del usuario.
