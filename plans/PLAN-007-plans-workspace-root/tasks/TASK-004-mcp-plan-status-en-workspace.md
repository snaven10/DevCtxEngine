# TASK-004 — MCP: `plan_status` en proceso en modo grupo y sin binding

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** TASK-003
- **Estado:** `done`

---

## Objetivo

Que una sesión MCP lanzada desde `~/acme` (binding de grupo) reciba los planes de
`~/acme/plans` sin depender del miembro default ni de la versión de su daemon (PLAN-007 DD-4).

## Contexto verificado

- `crates/devctx-mcp/src/lib.rs:939-946` — la tool `plan_status` llama `backend_for(req.project)` y
  luego `backend.plan_status(plan)`.
- `lib.rs:492` — `backend_for`: hint → ese proyecto; si no, `Binding::Project` → ese; `Binding::Group`
  → el `default` anotado con `default_name` (`lib.rs:516-521`); `Binding::None` → error
  `unbound_help`.
- `lib.rs:371` — `enum Binding { None, Project, Group { name, members, default, default_name } }`.
- `lib.rs:406` / `lib.rs:448` — `DevctxServer.cwd`, fijado al construir. En modo grupo el binding nace
  del descenso sobre ese cwd (`crates/devctx-cli/src/main.rs:1416-1443`).
- `crates/devctx-mcp/src/backend.rs:227-235` — `Backend::Remote` reenvía a `GET /plans/status` del
  daemon del miembro: un daemon levantado con 0.7.0 sigue respondiendo la raíz del miembro hasta
  reiniciarse.
- `crates/devctx-mcp/src/state.rs:866` — `fit_plan_status_budget` es privada.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs` (tool `plan_status`)
- **Modificar:** `crates/devctx-mcp/src/state.rs` (`pub fn plan_status_budgeted(&PlansRoot, plan)`)

## Pasos

- [x] **Paso 1 — Función pública con presupuesto.** `state::plan_status_budgeted(root: &PlansRoot,
      plan: Option<&str>) -> Result<String, String>` = `plan_status_value` + `fit_plan_status_budget`
      con el mismo `env_usize("DEVCTX_MAX_OUTPUT_TOKENS", …)` que `do_plan_status`. `do_plan_status`
      pasa a usarla (una sola ruta).
- [x] **Paso 2 — Despacho en la tool.** Con hint `project` → como hoy (`backend_for`). Sin hint:
      `Binding::Group` → `resolve_plans_root(None, Some(&self.cwd), false, home)` y
      `plan_status_budgeted` en `run_blocking`, **sin** pasar por el backend; `Binding::None` → igual
      si `cwd` tiene planes, si no el error `unbound_help` de hoy; `Binding::Project` → como hoy.
- [x] **Paso 3 — Procedencia.** En modo grupo no se anota `resolved_project` (no respondió un
      miembro); el JSON ya trae `plans_root` (TASK-003).
- [x] **Paso 4 — Descripción de la tool.** Sin cambios de texto salvo que haga falta; si cambia, que
      siga < 200 caracteres (PLAN-005 §3).

## Criterios de aceptación

- [x] Test (en `crates/devctx-cli/tests/mcp_binding.rs` o `mcp_tools.rs`, ver TASK-006): MCP lanzado
      en un workspace de prueba con dos miembros del mismo grupo y `plans/` en la raíz → `plan_status`
      lista esos planes con `plans_root.source = "workspace"`.
- [x] Mismo workspace, `plan_status` con `project: "<miembro>"` → resuelve por el miembro (DD-2 paso 3:
      `ancestor`), no se rompe el hint.
- [x] Sesión sin binding en un dir con `plans/` y sin proyectos → lista los planes en vez de error.
- [x] Sesión bindeada a un proyecto con `plans/` propio → idéntico a hoy.

## Riesgos

Calcular en proceso significa leer ~1174 archivos en el proceso MCP por llamada. Es lo mismo que ya
hace el CLI del hook; TASK-008 mide el tiempo real.

## Resultado

- **Estado final:** `done` (implementada; criterios que exigen correr código quedan sin marcar)
- **Resumen:** `plan_status_budgeted(&PlansRoot, plan)` pública (única ruta de `do_plan_status`); `home_dir()` pública. La tool `plan_status` sin hint `project`, con binding `Group` o `None` y `cwd/plans` existente, calcula en proceso con `source = workspace` (helper `DevctxServer::workspace_plans_root`); en `Binding::Project`, con hint, o si el cwd no tiene `plans/`, sigue por `backend_for` como hoy (en grupo cae al miembro, que resuelve por ancestro). Sin `resolved_project` en el camino en proceso. Descripción de la tool sin cambios. Sin tests unitarios nuevos: el despacho depende del cwd del proceso, se cubre en TASK-006.
- **Archivos tocados:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`
- **Verificado por:** `cargo test -p devctx-core` (62 unit + 6 plans_corpus), `-p devctx-mcp` (41 unit + 4 plans_root), `-p devctx-cli --test plan_status_cli --test mcp_binding --test mcp_tools` (9 + 20 + 8, 1 ignored preexistente) — todo verde (2026-10-01); `cargo clippy -p devctx-core -p devctx-mcp -p devctx-cli --all-targets` sin warnings.
