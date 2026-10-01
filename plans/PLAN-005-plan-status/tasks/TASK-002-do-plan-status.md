# TASK-002 — `do_plan_status`: listado, próximas tasks y presupuesto; `Backend` y ruta HTTP

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** TASK-001
- **Estado:** `done`

---

## Objetivo

La función que contesta "¿dónde estoy?" sobre la salida del parser, con la forma de respuesta que
consumen la tool (TASK-003), la vista web (TASK-006) y — vía el mismo JSON — el CLI (TASK-004).
Y el cableado local/daemon, sin el cual la tool falla cuando hay `devctx serve` corriendo.

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:161` — `AppState.root` (privado); las `do_*` de `state.rs` lo leen.
- `state.rs:508` `CHARS_PER_TOKEN = 4`, `state.rs:511` `env_usize`, `state.rs:519`
  `DEFAULT_MAX_OUTPUT_TOKENS = 8000`. Precedente de recorte con conteo: `omitted_for_budget` en
  `do_recall` (`state.rs:1965-1972`).
- `crates/devctx-mcp/src/backend.rs:419-438` — patrón `Backend::Local(s) => do_x(s)` /
  `Backend::Remote(r, _) => r.get(…)`.
- `crates/devctx-api/src/lib.rs:42-68` — rutas; `run(api.state, |s| do_x(s))` (`lib.rs:670`) envuelve
  el `Result<String,String>` en JSON.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (nueva `do_plan_status`)
- **Modificar:** `crates/devctx-mcp/src/backend.rs` (`Backend::plan_status`)
- **Modificar:** `crates/devctx-api/src/lib.rs` (ruta `GET /plans/status?plan=`)

## Pasos

- [ ] **Paso 1 — Plan activo.** El de `mtime` mayor **entre los que tienen al menos una task no-done**.
      Empate (clone/checkout iguala mtimes) → número de plan más alto. Ninguno → `active: null`.
- [ ] **Paso 2 — Sin `plan`: listado.** Por plan: `{id, title, done, total, active, task_files,
      warnings_count}`. Nada de tasks: el listado debe ser barato.
- [ ] **Paso 3 — Con `plan`: detalle.** Aceptar `PLAN-005`, `005`, `5` o el nombre del dir. Devolver
      `{plan, title, done, total, ready: [{id,title}], in_progress: […], blocked: [{id, title,
      waiting_on: [deps]}], warnings: [...]}` donde `warnings` junta: ciclos, deps faltantes,
      desacuerdos tabla/archivo y los del parser. **No incluir las `done`** (se cuentan, no se listan).
- [ ] **Paso 4 — Plan inexistente.** Error con los ids disponibles, no respuesta vacía: un vacío se
      lee como "no hay nada pendiente".
- [ ] **Paso 5 — Presupuesto.** Tope `DEVCTX_MAX_OUTPUT_TOKENS × CHARS_PER_TOKEN` sobre el JSON.
      Si se pasa: recortar primero `warnings`, luego `blocked`, nunca `ready`; agregar
      `"omitted": {"blocked": n, "warnings": m}`.
- [ ] **Paso 6 — `Backend::plan_status(plan: Option<&str>)`**: Local → `do_plan_status`; Remote →
      `r.get("/plans/status?plan=…")` con `urlencode` (ya existe en `backend.rs`).
- [ ] **Paso 7 — Ruta** `GET /plans/status` en `devctx-api` con `Query { plan: Option<String> }`.

## Criterios de aceptación

- [ ] Test en `state.rs` (o `crates/devctx-cli/tests/mcp_tools.rs`) con un `plans/` temporal de 2
      planes: el que tiene tasks pendientes es `active` aunque el otro sea más nuevo y esté todo `done`.
- [ ] `blocked[].waiting_on` nombra exactamente las deps no-done.
- [ ] Con `DEVCTX_MAX_OUTPUT_TOKENS=50` y un plan de 40 tasks, la salida trae `omitted` y conserva `ready`.
- [ ] Repo sin `plans/` → listado vacío, no error.

## Riesgos

Leer el disco en cada llamada: ~25 archivos. Despreciable hoy; si molesta, cachear por mtime (fuera de alcance, PLAN §7).

## Resultado

- **Estado final:** `done`
- **Resumen:** `do_plan_status`/`plan_status_value` en `state.rs`: listado (`plan_status_list`) y detalle (`plan_status_detail`) sobre `devctx_core::plans`. Plan activo = mtime mayor entre planes con tasks pendientes, empate por número de plan más alto. `Backend::plan_status` (Local/Remote) y ruta `GET /plans/status?plan=`.
- **Archivos tocados:** `crates/devctx-mcp/src/state.rs`, `crates/devctx-mcp/src/backend.rs`, `crates/devctx-api/src/lib.rs`.
- **Verificado por:** `cargo test -p devctx-mcp` — 27/27 verdes, incluye los 4 tests nuevos de `plan_status` (activo por mtime con desempate, `waiting_on` exacto, presupuesto con `DEVCTX_MAX_OUTPUT_TOKENS=50` sobre 40 tasks conservando `ready`, repo sin `plans/` da lista vacía sin error).
- **Desviaciones:** La función pública quedó como `plan_status_value(root: &Path, plan)` en vez de tomar `&AppState` — la pensé así desde el principio para que TASK-004 (CLI) no necesite construir un `AppState` completo (store, embedder) solo para leer markdown.
