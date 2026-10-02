# TASK-007 — Docs EN + ES: formato tolerante y raíz de planes

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** docs (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** TASK-002, TASK-005
- **Estado:** `done`

---

## Objetivo

Que la doc diga lo que el parser acepta de verdad y de dónde lee los planes, en inglés y en español
con el mismo contenido.

## Contexto verificado

- `docs/04-agent-workflow.md:131-160` — sección "Plans, and the markdown format `plan_status`
  expects": hoy dice que `Estado` toma **cuatro** valores y que `Depende de` lista `TASK-NNN`.
- `docs/es/04-flujo-de-trabajo-del-agente.md:130-160` — la misma sección en español.
- `docs/03-core-concepts/mcp-integration.md:126,137,140` y
  `docs/es/03-conceptos-fundamentales/integracion-mcp.md:130,141,144` — `plan_status` "reads
  `plans/PLAN-*/`" y `memories_by_file` → `plan_tasks`: no mencionan workspace.
- `CHANGELOG` del repo si existe una sección "Unreleased" (verificar con `rg -n Unreleased`).

## Archivos

- **Modificar:** los cuatro docs de arriba (y el changelog si aplica).

## Pasos

- [ ] **Paso 1 — Formato.** Claves aceptadas (`Estado`/`Status`/`State`, front matter, `## Estado`,
      campos con ` · `), los cinco estados con sus sinónimos y `skipped`, qué **no** cuenta
      (`Estado final`, `Estado objetivo`). Mantener el ejemplo canónico con `` `pending` `` como forma
      recomendada.
- [ ] **Paso 2 — Dependencias.** Solo cuentan los `TASK-<id>`; la prosa se ignora; `PLAN-N/TASK-M`
      es externa y no bloquea; el riesgo de aristas desde prosa (PLAN-007 §7) dicho en una línea.
- [ ] **Paso 3 — Ids.** Sufijos de letra (`TASK-001b`), `TASK-R01`; archivos compañeros con el mismo
      id generan warning.
- [ ] **Paso 4 — Raíz.** La precedencia de PLAN-007 DD-2 y el campo `plans_root` en la salida; el
      caso workspace multi-repo (MCP en modo grupo, `devctx web` desde el workspace, CLI desde un
      miembro). Recordar que un daemon viejo sigue sirviendo su raíz hasta `devctx serve --stop`.

## Criterios de aceptación

- [ ] EN y ES dicen lo mismo (mismas secciones, mismos ejemplos).
- [ ] Ningún ejemplo de la doc produce un warning con el parser nuevo.

## Resultado

- **Estado final:** `done`
- **Resumen:** `docs/04-agent-workflow.md` y su par ES documentan los cinco estados con sinónimos (incl. `skipped`), dónde se busca el estado (campo, front matter, `## Estado`), `Depende de` (solo `TASK-<id>`, externas no bloquean, ids con sufijo, ids duplicados), la tabla del plan y el orden de resolución de la raíz con `plans_root` y el caso workspace multi-repo (MCP en modo grupo, `devctx web`, CLI desde un miembro, `devctx serve --stop`). Las dos páginas de integración MCP (EN/ES) apuntan a esa sección. No hay CHANGELOG en el repo.
- **Archivos tocados:** `docs/04-agent-workflow.md`, `docs/es/04-flujo-de-trabajo-del-agente.md`, `docs/03-core-concepts/mcp-integration.md`, `docs/es/03-conceptos-fundamentales/integracion-mcp.md`.
- **Verificado por:** lectura del código de `plans.rs`; sin compilar ni ejecutar. Tras la corrida: `cargo test` verde y corpus real sin warnings de formato, pero ningún ejemplo de la doc se corrió contra el parser.
