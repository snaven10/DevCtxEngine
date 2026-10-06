# TASK-006 — Tests de integración: workspace sin init + corpus de fixtures reales

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** rust-testing (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** TASK-002, TASK-004, TASK-005
- **Estado:** `done`

---

## Objetivo

Cubrir los bordes que en este repo históricamente no tienen tests (CLI ↔ daemon, MCP ↔ subproceso —
ver PLAN-005/PLAN-006) para la raíz del workspace, y fijar el parser contra texto real de acme para
que una regresión de tolerancia se vea en `cargo test`, no en el dashboard.

## Contexto verificado

- `crates/devctx-cli/tests/mcp_binding.rs:145` — `make_project(home, parent, name, group)` ya arma
  proyectos registrados con grupo bajo un `parent`; `:179` `start_mcp_in(home, cwd, extra)`;
  `:198` `workspace_root_binds_the_group`.
- `crates/devctx-cli/tests/plan_status_cli.rs` — CLI sin daemon ni store; `:124`
  `table_output_on_the_real_repo_plans_fits_the_hook_budget` exige ≤ 600 bytes sobre **los planes de
  este repo** — PLAN-007 los hace crecer.
- `crates/devctx-cli/tests/mcp_tools.rs:258` — `write_plan_status_fixture`, `:307`
  `plan_status_lists_plans_and_names_the_active_one`.
- Lección de PLAN-002 (memoria `recall` "fan-out de grupo"): probar el MCP contra un proceso recién
  arrancado, con stdin abierto.

## Archivos

- **Crear:** `crates/devctx-core/tests/plans_corpus.rs` (o `#[cfg(test)]` en `plans.rs` si el
  especialista prefiere; los fixtures van en `crates/devctx-core/tests/fixtures/plans/`)
- **Modificar:** `crates/devctx-cli/tests/plan_status_cli.rs`, `crates/devctx-cli/tests/mcp_binding.rs`

## Pasos

- [x] **Paso 1 — Corpus.** Un mini-workspace de fixtures con ~10 tasks y 2 planes copiados (texto
      real, recortado) de acme: `**Status:** done`, front matter, `## Estado`, línea con ` · `,
      `✅ **\`done\`** — ejecutada…`, `Depende de` con prosa, `TASK-001b`, `TASK-R01`, un `TASK-001-DESIGN.md`
      compañero, una tabla de rollback y una de estado. El test afirma el conteo **exacto** de warnings
      y cuáles son (los legítimos de PLAN-007 §6).
- [x] **Paso 2 — CLI desde un miembro.** Árbol `ws/plans/PLAN-001-x/…` + `ws/api/.devctx/config.yaml`
      con `group: acme`; `devctx plan-status --format json` con cwd `ws/api` → lista PLAN-001,
      `plans_root.source = "ancestor"`. Sin `group:` → lista vacía (no adopta).
- [x] **Paso 3 — MCP en workspace.** Con `make_project` (dos miembros, mismo grupo) y `plans/` en la
      raíz: `start_mcp_in(ws)` + `tools/call plan_status` → `source = "workspace"`, los planes de la raíz.
- [x] **Paso 4 — Web en workspace.** `devctx web --addr 127.0.0.1:<libre> --no-open` con cwd `ws`
      bajo `timeout`, `GET /plans/status` → los planes de la raíz. Si levantar el dashboard en test
      es frágil (puertos, ver PLAN-002 "puerto fijo"), cubrir con `tower::oneshot` sobre el router de
      `devctx-api` con un `AppState` de miembro (patrón de PLAN-006 TASK-003) y dejar la prueba con
      binario real para TASK-008.
- [x] **Paso 5 — Presupuesto del hook.** Re-medir el test de `plan_status_cli.rs:124`. Si PLAN-007 lo
      pasa de 600 bytes, ajustar el test a lo que el hook realmente inyecta (bloque del plan activo),
      con el porqué en un comentario — nunca recortando planes.

## Criterios de aceptación

- [ ] Todos los tests nuevos fallan contra `main` (v0.7.0) y pasan en la rama — anotar en Resultado
      cuáles se verificaron fallando primero.
- [x] Ningún test lee `/home/you/acme` (los fixtures son copias dentro del repo).
- [x] Sin puertos fijos (el dashboard usa un puerto efímero: bind a `:0`, leer, soltar).

## Resultado

- **Estado final:** `done` (escritos; **no compilados ni ejecutados**)
- **Resumen:** Corpus fijo y tests de borde para la raíz del workspace.
  - Corpus (`devctx-core/tests/plans_corpus.rs` + `tests/fixtures/plans/corpus/plans/PLAN-101-alfa`, `PLAN-102-beta`, 13 tasks): `**Status:**`, front matter, `## Estado`, línea con ` · `, `✅ **\`done\`** — ejecutada…`, `Depende de` con prosa, `TASK-001b`, `TASK-R01`, compañero `TASK-001-DESIGN.md`, tabla de rollback junto a la de estado, `skipped`. Afirma los warnings **exactos** (3 en PLAN-101: id duplicado, Estado no reconocido, tabla vs archivo; 2 en PLAN-102: archivo no-task, `Depende de` sin TASK) y la ausencia de los viejos (`token no reconocido`, `múltiples documentos`, ciclos).
  - CLI desde un miembro (`plan_status_cli.rs`): `ancestor` con `group:`; lista vacía y `none` sin `group:`; `project` si el miembro tiene `plans/` propio; el walk para antes de `$HOME`.
  - Presupuesto del hook (Q-1): el test pasa a un fixture fijo de 5 planes generado en el test (~330 bytes de salida, tope 600), con el porqué en el comentario; ya no lee los planes vivos del repo.
  - MCP (`mcp_binding.rs`): grupo con `plans/` en la raíz → `workspace`; grupo con `project` → `ancestor` + `resolved_project`; sin binding con `plans/` → lista; sin binding sin `plans/` → sigue fallando con `not bound to a project`; proyecto con `plans/` propio → `project`. Helper nuevo `call_tool_with_user_home` (fija `HOME`, que hereda el daemon que el MCP auto-lanza).
  - Web (`mcp_binding.rs`): `devctx web` real en un workspace sin `.devctx/` con puerto efímero → `GET /plans/status` devuelve los planes (`ancestor`) y el banner nombra el origen; sin proyectos → falla con `No DevCtxEngine project found` + el texto de `why_unbound`.
  - `AppState` real (`devctx-mcp/tests/plans_root.rs`): `do_plan_status` de un miembro con `group:` → `ancestor` (lista y detalle); sin grupo → `none`; con `plans/` propio → `project`; `$HOME` como tope.
- **Archivos tocados:** `crates/devctx-core/tests/plans_corpus.rs` (nuevo), `crates/devctx-core/tests/fixtures/plans/corpus/plans/**` (22 fixtures, nuevos), `crates/devctx-mcp/tests/plans_root.rs` (nuevo), `crates/devctx-cli/tests/plan_status_cli.rs`, `crates/devctx-cli/tests/mcp_binding.rs`.
- **Verificado por:** no compilado ni ejecutado — pendiente de corrida de tests (`cargo test -p devctx-core --test plans_corpus`, `-p devctx-mcp --test plans_root`, `-p devctx-cli --test plan_status_cli --test mcp_binding`). Sólo se pasó `rustfmt` sobre los archivos nuevos/tocados. **Pendiente**: confirmar que los tests nuevos fallan contra `main` (v0.7.0) y pasan en la rama (criterio de aceptación no verificado).
- **Riesgos conocidos:** (1) `web_in_a_workspace_without_devctx_…` levanta un dashboard real: puerto efímero con carrera mínima entre soltar y rebindear, `KillOnDrop` y `Tmp::drop` lo limpian; espera hasta 90 s. (2) Los tests de MCP con `HOME` fijado dependen de que el daemon auto-lanzado herede el entorno. (3) Los warnings esperados del corpus dependen del orden de archivos (`TASK-001-ALCANCE` antes de `TASK-001-DESIGN`).
