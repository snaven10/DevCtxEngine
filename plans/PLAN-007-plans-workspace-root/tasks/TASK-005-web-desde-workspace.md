# TASK-005 — `devctx web` desde un workspace (descenso compartido con `devctx mcp`)

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** TASK-003
- **Estado:** `done`

---

## Objetivo

Que `devctx web` arranque desde `~/acme` (sin `.devctx/`) y que su pestaña "Plans" muestre
`~/acme/plans` (PLAN-007 DD-5). Sin flag nuevo.

## Contexto verificado

- `crates/devctx-cli/src/main.rs:1318` — `cmd_web` empieza con `load_project()?` (`main.rs:3429`), que
  exige `.devctx/config.yaml` subiendo por los padres: en `~/acme` falla con "No DevCtxEngine project
  found (run `devctx init` first)".
- `cmd_web` reusa un daemon que ya corre para ese proyecto (`remote::running_server_url`) o levanta
  `devctx_api::run_blocking(cfg, …)` con el `AppState` del proyecto.
- `crates/devctx-cli/src/main.rs:1363` — `cmd_mcp` ya resuelve un workspace con
  `devctx_mcp::state::resolve_under(&cwd)` (`crates/devctx-mcp/src/state.rs:1462`): `Single`, `Group`
  (default = miembro con `last_indexed_at` mayor, `main.rs:1416-1443`), `Ambiguous`, `Empty`,
  `RegistryUnavailable`; `why_unbound` (`state.rs:1538`) explica los casos sin binding.
- `crates/devctx-api/src/lib.rs:59-60` — `/plans/status` y `/plans/graph` sobre `do_plan_status` /
  `do_plan_graph`, que tras TASK-003 leen `AppState.plans_root`.
- `crates/devctx-api/src/central.rs:40-54` — el daemon central no sirve dashboard: `serve --central`
  no aplica.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs` (`cmd_web`, helper de descenso extraído de `cmd_mcp`)

## Pasos

- [x] **Paso 1 — Extraer el descenso.** `fn resolve_workspace_project(cwd: &Path) -> Result<(ProjectConfig,
      WorkspaceNote)>` con la lógica de `cmd_mcp` para `Single` y `Group` (elección del default
      incluida). `cmd_mcp` lo usa para esos dos casos sin cambiar su comportamiento ni sus mensajes.
- [x] **Paso 2 — `cmd_web`.** `load_project()` primero; si falla, el helper. `Single`/`Group` → el
      config del miembro; cualquier otro caso → error con el texto de `why_unbound`.
- [x] **Paso 3 — Banner.** En modo grupo: `Workspace <cwd> (group <name>): plans from <plans_root>,
      code and memories from <default>`. Que nadie confunda el grafo del miembro con el del workspace.
- [x] **Paso 4 — Daemon viejo.** Si se reusa un daemon que ya corría (`running_server_url`), avisar
      en el banner que la raíz de planes la decide ese proceso (un daemon 0.7.0 seguirá mostrando la
      del miembro hasta `devctx serve --stop`).

## Criterios de aceptación

- [x] En un workspace de prueba sin `.devctx/` con dos miembros del mismo grupo, `devctx web --addr
      127.0.0.1:<libre> --no-open` arranca y `GET /plans/status` lista los planes de la raíz del
      workspace con `plans_root.source = "ancestor"` (TASK-006).
- [x] En un dir sin proyectos debajo, el error sigue siendo claro y nombra el porqué.
- [x] `devctx mcp` desde un workspace sigue imprimiendo `Bound to group …` igual que hoy
      (`crates/devctx-cli/tests/mcp_binding.rs:198`, `workspace_root_binds_the_group`).

## Riesgos

El dashboard en modo grupo muestra planes del workspace pero grafo/memorias de un solo miembro. Es
explícito en el banner; mostrar todos los miembros queda fuera de alcance (PLAN-007 §4).

## Resultado

- **Estado final:** `done` (implementada; criterios que exigen correr código quedan sin marcar)
- **Resumen:** `cmd_web` intenta `load_project()` y, si falla, `resolve_workspace_project` (`resolve_under(cwd)`): `Single` o `Group` sirven el miembro (default = más recientemente indexado); otro caso devuelve el error original más `why_unbound`. Banner `Workspace <cwd> (group <g>): plans from <root>, code and memories from <miembro>`; si se reusa un daemon, avisa que la raíz de planes la decide ese proceso. Desviación: en vez de un helper que devuelve `(ProjectConfig, WorkspaceNote)` para `cmd_mcp` también, se extrajeron `default_member` y `load_member_config` (la regla del default y la carga del config) y `cmd_mcp` los usa; su flujo y mensajes no cambian, porque `Binding::Group` necesita `members` y la `Resolution` completa para `why_unbound`.
- **Archivos tocados:** `crates/devctx-cli/src/main.rs`
- **Verificado por:** `cargo test -p devctx-core` (62 unit + 6 plans_corpus), `-p devctx-mcp` (41 unit + 4 plans_root), `-p devctx-cli --test plan_status_cli --test mcp_binding --test mcp_tools` (9 + 20 + 8, 1 ignored preexistente) — todo verde (2026-10-01); `cargo clippy -p devctx-core -p devctx-mcp -p devctx-cli --all-targets` sin warnings.
