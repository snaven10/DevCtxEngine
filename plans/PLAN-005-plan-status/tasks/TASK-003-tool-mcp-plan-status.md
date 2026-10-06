# TASK-003 — Tool MCP `plan_status(plan?, project?)`

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** TASK-002
- **Estado:** `done`

---

## Objetivo

Exponer `do_plan_status` como **una** tool, con la descripción más corta que siga siendo útil. El
recorte de 24% en descripciones (`c7d0b82`: 9729 → 7385 chars) no se devuelve con esta tool.

## Contexto verificado

- `crates/devctx-mcp/src/lib.rs:558` — `#[tool_router] impl DevctxServer`.
- `lib.rs:907-921` — `impact_analysis`: el patrón completo con hint de proyecto:
  `let (backend, resolved) = self.backend_for(req.project.as_deref())?;` → `run_blocking` →
  `Self::annotate(out, resolved)`.
- `lib.rs:178-187` — `ImpactReq` declara `project: Option<String>` con doc de una línea
  (*"Answer this call from a different project than the one bound (this call only)."*): tras PLAN-004
  el párrafo largo ya no se repite. Copiar esa misma línea.
- `lib.rs:480` — `backend_for`: en modo grupo responde el miembro default y lo nombra en
  `resolved_project`. Los planes son por repo, así que ese comportamiento es el correcto.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs` (struct `PlanStatusReq` + método `plan_status`)

## Pasos

- [ ] **Paso 1 — `PlanStatusReq { project: Option<String>, plan: Option<String> }`**, docs de una
      línea cada uno. `plan`: *"Plan id (PLAN-005 or 5). Omit to list plans."*
- [ ] **Paso 2 — Descripción < 200 chars.** Propuesta (148 chars):
      `"Plans in plans/ (markdown, source of truth): no arg lists progress and the active plan; with \
      `plan`, ready/in-progress/blocked tasks. JSON."`
- [ ] **Paso 3 — Cuerpo** idéntico al de `impact_analysis`: `backend_for` → `run_blocking(move ||
      backend.plan_status(req.plan.as_deref()))` → `annotate`.
- [ ] **Paso 4 — Instrucciones del servidor.** Si `lib.rs` tiene un texto de instrucciones/protocolo
      que enumera tools, agregar una línea: *"tras compactar, `plan_status` antes de retomar"*.
      *(No verificado dónde vive ese texto tras PLAN-004 — revisar `MEMORY-PROTOCOL.md` y el prompt
      `memory-protocol`.)*

## Criterios de aceptación

- [ ] `description` de la tool < 200 caracteres (test: medir desde `list_tools`, o assert en código).
- [ ] Con `project` apuntando a otro repo registrado, lee **sus** `plans/` y trae `resolved_project`.
- [ ] Sin binding y sin `project` → el mismo `unbound_help` que las demás tools de código.

## Riesgos

Ninguno propio: es cableado. El riesgo está en TASK-002.

## Resultado

- **Estado final:** `done`
- **Resumen:** `PlanStatusReq { project, plan }` + método `plan_status` en `DevctxServer`, mismo patrón que `impact_analysis` (`backend_for` → `run_blocking` → `annotate`). Descripción de 139 caracteres. Línea agregada a `with_instructions` sobre llamar `plan_status` tras compactar.
- **Archivos tocados:** `crates/devctx-mcp/src/lib.rs`.
- **Verificado por:** `cargo check --workspace --all-targets` limpio. Longitud de la descripción medida por script (139 < 200). El escenario completo por protocolo MCP real (con y sin `plan`, con `project`, plan inexistente) se cubre en TASK-008 (`mcp_tools.rs`) para no duplicar la fixture de planes.
- **Desviaciones:** Ninguna.
