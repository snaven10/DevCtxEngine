# TASK-007 — Tests de integración de los escenarios de resolución

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-003, TASK-005, TASK-006
- **Estado:** `pending`

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

- [ ] **Paso 1 — Helper de workspace**: un tmp dir con N repos inicializados y registrados, con
      `project.group` seteable por repo.
- [ ] **Paso 2 — Escenario A (el bug)**: cwd = raíz del workspace, 3 repos mismo grupo →
      bindea al grupo; `remember` SIN `use_project` previo funciona.
- [ ] **Paso 3 — Escenario B**: cwd = raíz con un solo repo adentro → bindea ese proyecto.
- [ ] **Paso 4 — Escenario C**: grupos mezclados → unbound, y el error lista solo los de ahí
      (TASK-004).
- [ ] **Paso 5 — Escenario D (sin regresión)**: cwd DENTRO de un repo → bindea ese repo, igual que hoy.
- [ ] **Paso 6 — Escenario E**: `--project` gana sobre el descenso.
- [ ] **Paso 7 — Escenario F (hint)**: en modo grupo, una tool de código con `path` apuntando al
      miembro B responde desde B aunque el `default` sea A; y la salida nombra el proyecto resuelto.
- [ ] **Paso 8 — Escenario G (prefijo)**: `/tmp/x/revfa` no captura los proyectos de
      `/tmp/x/revfa-otro`.
- [ ] **Paso 9 — Escenario H (default de scope)**: en modo grupo, `remember` sin `scope` cae en
      `group`; en modo proyecto sigue en `local`.

## Criterios de aceptación

- [ ] Los 8 escenarios pasan.
- [ ] El escenario A falla si se revierte TASK-003 (probarlo revirtiendo a mano antes de cerrar).
- [ ] Los tests no dependen del HOME real ni del registry real de la máquina.
- [ ] La suite existente sigue verde.

## Riesgos

Levantar el servidor MCP en un test es más pesado que invocar la CLI. Si resulta frágil, testear
`resolve_under` / `resolve_from_hint` como unidades y dejar solo A y F como integración de punta a
punta — pero **A no se puede omitir**: es el bug que originó el plan.

## Resultado

<!-- SE LLENA AL CERRAR -->
