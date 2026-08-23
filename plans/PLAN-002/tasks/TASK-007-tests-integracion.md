# TASK-007 — Tests de integración de los escenarios de resolución

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-003, TASK-005, TASK-006
- **Estado:** `done`

---

## Objetivo

Que el bug de origen quede cubierto por un test que falle si alguien revierte el descenso, y que
cada rama de la resolución tenga uno.

## Contexto verificado

Ya existe infraestructura de tests de CLI con workspace temporal y HOME aislado:
`crates/devctx-cli/tests/projects_cli.rs` usa `Tmp::new(...)`, `tmp.home()`, `tmp.repo("alpha")` y
los helpers `ok(...)` / `fails(...)`, y verifica persistencia **entre procesos separados**. Ese es
el molde a reusar; no hace falta inventar andamiaje.

## Archivos

- **Crear:** `crates/devctx-cli/tests/mcp_binding.rs`

## Pasos

- [x] **Paso 1 — Helper de workspace**: un tmp dir con N repos inicializados y registrados, con
      `project.group` seteable por repo.
- [x] **Paso 2 — Escenario A (el bug)**: cwd = raíz del workspace, 3 repos mismo grupo →
      bindea al grupo; `remember` SIN `use_project` previo funciona.
- [x] **Paso 3 — Escenario B**: cwd = raíz con un solo repo adentro → bindea ese proyecto.
- [x] **Paso 4 — Escenario C**: grupos mezclados → unbound, y el error lista solo los de ahí
      (TASK-004).
- [x] **Paso 5 — Escenario D (sin regresión)**: cwd DENTRO de un repo → bindea ese repo, igual que hoy.
- [x] **Paso 6 — Escenario E**: `--project` gana sobre el descenso.
- [x] **Paso 7 — Escenario F (hint)**: en modo grupo, una tool de código con `path` apuntando al
      miembro B responde desde B aunque el `default` sea A; y la salida nombra el proyecto resuelto.
- [x] **Paso 8 — Escenario G (prefijo)**: `/tmp/x/revfa` no captura los proyectos de
      `/tmp/x/revfa-otro`.
- [x] **Paso 9 — Escenario H (default de scope)**: en modo grupo, `remember` sin `scope` cae en
      `group`; en modo proyecto sigue en `local`.

## Criterios de aceptación

- [x] Los 8 escenarios pasan.
- [x] El escenario A falla si se revierte TASK-003 (probarlo revirtiendo a mano antes de cerrar).
- [x] Los tests no dependen del HOME real ni del registry real de la máquina.
- [x] La suite existente sigue verde.

## Riesgos

Levantar el servidor MCP en un test es más pesado que invocar la CLI. Si resulta frágil, testear
`resolve_under` / `resolve_from_hint` como unidades y dejar solo A y F como integración de punta a
punta — pero **A no se puede omitir**: es el bug que originó el plan.

## Resultado

**Estado final:** `done` (2026-08-19)

9 tests en `crates/devctx-cli/tests/mcp_binding.rs`, todos verdes. Siete ya
estaban escritos (escenarios A, B, C, D, E, G más un directorio vacío); se
agregaron los dos que faltaban.

**Escenario H — `scope_defaults_follow_the_binding`.** En modo grupo un
`remember` sin `scope` se archiva como `group`; en modo proyecto no. Nota de
implementación: la respuesta de `remember` incluye `scope` en modo grupo pero
NO en modo proyecto, así que la segunda mitad afirma el invariante que sí se
puede observar — que no haya caído en `group`.

**Escenario F — `a_project_hint_selects_the_member`.** El hint NO se llama
`path` como decía este archivo: se implementó como `project`, por nombre. El
test verifica dos cosas distintas con dos tools, porque una sola no alcanza:
`read_file` prueba el ruteo (cada miembro devuelve su propio contenido) y
`impact_analysis` prueba la anotación (`resolved_project` en la respuesta).
`annotate` deja los strings y arrays sin envolver a propósito, así que
`read_file` nunca va a traer `resolved_project` — no es una falla.

**Criterio del mutante, cumplido.** Revirtiendo TASK-003 a mano
(`resolve_under(&cwd)` → `Resolution::Empty`) caen 5 de los 9, incluido
`workspace_root_binds_the_group`, que es el bug de origen. Restaurado después.

**Límite conocido:** `a_project_hint_selects_the_member` sigue pasando con el
descenso revertido, porque un hint por nombre no necesita binding de grupo para
resolver. Prueba el ruteo, no el modo grupo.

**Descubierto al cerrar:** TASK-010 y TASK-011 figuran `pending` en la tabla del
plan pero están implementadas y cableadas — `do_search_group` y
`do_recall_group` existen en `state.rs` y `search`/`recall` las llaman. El
estado declarado estaba desactualizado. Queda pendiente de verdad solo TASK-009,
que además no tiene archivo de tarea.
