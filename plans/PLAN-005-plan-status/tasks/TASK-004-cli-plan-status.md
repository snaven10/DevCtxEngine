# TASK-004 — Subcomando `devctx plan-status [PLAN] [--format json]`

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Que el hook `SessionStart` (TASK-005) pueda preguntar "¿qué sigue?" en una línea de shell, rápido y
sin depender del daemon ni del store: solo leer markdown.

## Contexto verificado

- `crates/devctx-cli/src/main.rs:52` — `enum Command`; `Impact` (`main.rs:231-238`) es el molde de
  variante con posicional + flag.
- `main.rs:450` — `enum OutputFormat { Table, Json }` (`ValueEnum`), ya usado por `Search` y `Recall`.
- `main.rs:567` — despacho `Command::Impact { symbol, depth } => cmd_impact(symbol, depth)`.
- `main.rs:3282` `load_project()` y `main.rs:3290` `project_root(cfg)` → la raíz del proyecto.
- El hook corre `devctx status` con `timeout 5` y ya filtra el ruido `newer devctx|started background
  server` (`~/.claude/hooks/devctx-session-start.sh`). Un comando que **no** use `remote::ensure`
  no dispara el arranque del daemon.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs` (variante `PlanStatus`, `cmd_plan_status`)

## Pasos

- [ ] **Paso 1 — Variante** `PlanStatus { plan: Option<String>, #[arg(long, value_enum, default_value
      = "table")] format: OutputFormat }` → clap la expone como `plan-status`.
- [ ] **Paso 2 — Raíz.** `load_project()` + `project_root()`; si no hay proyecto devctx, caer al
      `cwd` (los planes existen aunque el repo no esté indexado). **No** llamar `remote::ensure`.
- [ ] **Paso 3 — Mismo JSON que la tool.** Llamar `devctx_core::plans` y armar la respuesta con la
      misma forma que `do_plan_status`. Para no duplicar, mover la construcción del JSON (sin el
      presupuesto) a una función pública que ambos llamen — `do_plan_status` quedaría como
      `root → función pública + recorte`. Decidir dónde vive al implementar TASK-002; el CLI ya
      depende de `devctx-mcp`.
- [ ] **Paso 4 — Formato `table` compacto, pensado para ≤ 600 chars:**
      ```
      PLAN-005 plan_status — 2/8 done (activo)
        listas:     TASK-003 Tool MCP plan_status · TASK-004 Subcomando …
        en curso:   —
        bloqueadas: 4 (TASK-008 espera TASK-006, TASK-007)
        ⚠ 1 warning
      ```
      Sin `PLAN`: una línea por plan (`PLAN-00N  done/total  título`), el activo marcado con `*`,
      y debajo el bloque del activo como arriba.
- [ ] **Paso 5 — Salida de error.** Plan inexistente → exit ≠ 0 con los ids disponibles. Sin
      `plans/` → exit 0 y `sin planes`.

## Criterios de aceptación

- [ ] Test en `crates/devctx-cli/tests/` con el helper `devctx(home, cwd, args)` de `mcp_tools.rs:113`
      sobre un repo temporal con `plans/`: `--format json` parsea y trae `active`.
- [ ] En un repo sin `.devctx/` pero con `plans/`, funciona.
- [ ] No arranca el daemon (no aparece `started background server` en stderr).
- [ ] Salida `table` sin `PLAN` sobre el `plans/` real de DevCtxEngine ≤ 600 bytes.

## Riesgos

Duplicar la lógica de respuesta entre CLI y MCP es cómo nacen las divergencias de PLAN §2.1. El
Paso 3 existe para que haya **una** función.

## Resultado

<!-- SE LLENA AL CERRAR -->
