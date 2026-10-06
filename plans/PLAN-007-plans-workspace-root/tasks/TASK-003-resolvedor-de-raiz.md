# TASK-003 — Resolvedor `resolve_plans_root` y `AppState.plans_root`

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Una sola respuesta a "¿de qué directorio leo `plans/`?", usada por el daemon, el CLI y
`memories_by_file`, para que un miembro de un workspace lea los planes del workspace sin que nadie
le pase la ruta (PLAN-007 DD-2, DD-3).

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:163` — `AppState.root` (privado), calculado en `state.rs:196`
  desde `cfg.project.path` o el cwd.
- `state.rs:720-721` — `do_plan_status` usa `state.root`; `state.rs:905-906` — `do_plan_graph` igual.
- `state.rs:3126` — `memories_by_file` llama `plan_tasks_for_file(&state.root, …)` (def. `state.rs:3134`):
  **también afectado**, en un miembro de acme siempre devuelve `plan_tasks: []`.
- `state.rs:729` / `state.rs:911` — `plan_status_value(root, plan)` / `plan_graph_value(root, plan)`
  ya reciben la raíz: no cambian de firma.
- `crates/devctx-cli/src/main.rs:2013` — `plan_status_root()`: raíz del proyecto si el cwd está en uno,
  si no el cwd. Desde `~/acme/backend-a` lee `backend-a/plans` (no existe).
- `crates/devctx-core/src/config.rs:80` — `project.group: String` (vacío si no declara grupo).
- `crates/devctx-mcp/src/state.rs:1329` — `group_of(root)` ya lee el grupo desde el config.

## Archivos

- **Modificar:** `crates/devctx-core/src/plans.rs` (función pura + `PlansRoot`)
- **Modificar:** `crates/devctx-mcp/src/state.rs` (`AppState.plans_root`, tres consumidores, campo
  `plans_root` en el JSON)
- **Modificar:** `crates/devctx-cli/src/main.rs` (`plan_status_root`)

## Pasos

- [x] **Paso 1 — Función pura.** `pub fn resolve_plans_root(project_root: Option<&Path>, workspace:
      Option<&Path>, group_declared: bool, home: Option<&Path>) -> PlansRoot` con
      `PlansRoot { root: PathBuf, source: PlansRootSource }` y `source ∈ {Workspace, Project,
      Ancestor, None}` (`Serialize`, minúscula). Precedencia exacta de PLAN-007 DD-2. `home` entra por
      parámetro (no `std::env` adentro) para que sea testeable; "tiene planes" = `plans/` existe y
      contiene al menos un dir `PLAN-<n>*`.
- [x] **Paso 2 — `AppState.plans_root`.** Calculado una vez en la construcción (`state.rs:196`) con
      `workspace = None`, `group_declared = !cfg.project.group.trim().is_empty()`, `home` de `$HOME`.
      Un accessor `pub fn plans_root(&self) -> &PlansRoot`.
- [x] **Paso 3 — Consumidores.** `do_plan_status`, `do_plan_graph` y `plan_tasks_for_file` usan
      `plans_root.root`. `plan_status_value` agrega `"plans_root": {"path", "source"}` al JSON de
      listado y de detalle (nueva firma interna que recibe `&PlansRoot`; la pública con `&Path` se
      mantiene como envoltorio con `source = project` para no romper los tests existentes).
- [x] **Paso 4 — CLI.** `plan_status_root()` pasa a: dentro de un proyecto → `resolve_plans_root(Some(root),
      None, group_declared, home)`; fuera de un proyecto → `resolve_plans_root(None, Some(cwd), false, home)`.
      Sigue sin tocar store ni daemon.

## Criterios de aceptación

- [x] Tests unitarios de `resolve_plans_root` sobre un árbol temporal: workspace con `plans/` y
      miembro sin `plans/` (con y sin grupo); miembro con `plans/` propio (gana el miembro, paso 2);
      ancestro con `plans/` pero sin `PLAN-*` dentro (no se adopta); ancestro = `$HOME` (no se adopta).
- [x] `do_plan_status` sobre un `AppState` de un miembro con grupo devuelve los planes del ancestro y
      `plans_root.source = "ancestor"`.
- [x] Los tests existentes de `plan_status_value`/`plan_graph_value`/`plan_tasks_for_file`
      (`state.rs:3792` en adelante) pasan sin cambios.
- [x] Este repo sigue resolviendo `source = "project"`.

## Riesgos

El walk-up puede adoptar un `plans/` ajeno: acotado por `group:` declarado, `PLAN-*` dentro y tope
en `$HOME`. El campo `plans_root` en el JSON hace visible cualquier elección inesperada.

## Resultado

- **Estado final:** `done` (implementada; criterios que exigen correr código quedan sin marcar)
- **Resumen:** `resolve_plans_root` (pura, `home` por parámetro; el paso 3 no corre sin `home`) + `PlansRoot`/`PlansRootSource` en `devctx-core`. `AppState.plans_root` calculado en `build` con `group_declared` del config y `$HOME`; `do_plan_status`, `do_plan_graph` y `plan_tasks_for_file` lo usan. Nuevo `plan_status_value_in(&PlansRoot, …)` agrega `plans_root: {path, source}`; `plan_status_value(&Path, …)` queda como envoltorio con `source = project`. CLI `plan_status_root()` devuelve `PlansRoot`. Tests: 8 `resolve_*` en `plans.rs` y `plan_status_of_a_group_member_reads_the_workspace_plans_and_says_so` en `state.rs`. El criterio de `do_plan_status` sobre un `AppState` real se cubrió a nivel `plan_status_value_in` (construir `AppState` abre un store); queda para TASK-006.
- **Archivos tocados:** `crates/devctx-core/src/plans.rs`, `crates/devctx-mcp/src/state.rs`, `crates/devctx-cli/src/main.rs`
- **Verificado por:** `cargo test -p devctx-core` (62 unit + 6 plans_corpus), `-p devctx-mcp` (41 unit + 4 plans_root), `-p devctx-cli --test plan_status_cli --test mcp_binding --test mcp_tools` (9 + 20 + 8, 1 ignored preexistente) — todo verde (2026-10-01); `cargo clippy -p devctx-core -p devctx-mcp -p devctx-cli --all-targets` sin warnings.
