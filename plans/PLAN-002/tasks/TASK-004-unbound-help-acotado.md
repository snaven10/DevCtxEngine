# TASK-004 — `unbound_help` acotado a los candidatos bajo el cwd

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-001
- **Estado:** `done`

---

## Objetivo

Que cuando el servidor igual quede sin bindear, el mensaje diga **por qué** y ofrezca los candidatos
relevantes, no el listado completo de la máquina.

## Contexto verificado

`unbound_help(cwd)` (`crates/devctx-mcp/src/state.rs:926-947`) lista HOY **todos** los proyectos del
registry. Con 13 registrados, el error que recibe el agente tiene 13 líneas de las cuales 11 son de
otro workspace o irrelevantes. Es el mensaje que se ve en el bug de origen.

Tras TASK-003, quedar unbound solo puede pasar por dos razones distintas, y hoy se ven iguales:
`Ambiguous` (hay candidatos pero con grupos mezclados) y `Empty` (no hay nada acá).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs`

## Pasos

- [ ] **Paso 1 — `unbound_help` recibe la `Resolution`**, no solo el cwd.
- [ ] **Paso 2 — Caso `Ambiguous`**: decir que se encontraron N proyectos bajo el cwd pero que no
      comparten grupo, listar **solo esos** con su grupo (o `(sin grupo)`), y sugerir `use_project`
      o poblar `project.group` para que el auto-bind funcione la próxima vez.
- [ ] **Paso 3 — Caso `Empty`**: mantener el mensaje actual (nada arriba ni abajo), con el listado
      completo como orientación — ahí sí es lo útil.
- [ ] **Paso 4 — Truncar listados largos** a ~15 entradas con `… y N más`, para no inundar el
      contexto del agente.

## Criterios de aceptación

- [ ] Con grupos mezclados bajo el cwd, el error lista SOLO los de ahí y nombra el grupo de cada uno.
- [ ] Con nada registrado bajo el cwd, el mensaje es el de hoy (sin regresión).
- [ ] Los dos casos son distinguibles leyendo el texto, sin mirar el código.
- [ ] Un registry de 50 proyectos no produce un error de 50 líneas.

## Riesgos

Ninguno funcional: solo cambia texto de error. Cuidar que los tests que asertan contra el string
actual se actualicen (`grep` por `"not bound to a project"` en tests).

## Resultado

- **Estado final:** `done`
- **Resumen:** `why_unbound(cwd, resolution)` distingue Ambiguous / Empty / RegistryUnavailable; unbound_help lo envuelve. Listados truncados a 15.
- **Archivos tocados:** crates/devctx-mcp/src/state.rs
- **Verificado por:** Test `mixed_groups_stay_unbound_and_name_the_candidates` (verde): lista solo alpha y beta con su grupo y dice «do not share a group». Test `an_empty_directory_starts_unbound` (verde).
- **Desviaciones:** Se agrego el caso RegistryUnavailable, que no estaba planeado — ver TASK-001.
