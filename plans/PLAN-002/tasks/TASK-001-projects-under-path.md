# TASK-001 — `projects_under(path)` y resolución de grupo en el registry

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** — (primera del plan)
- **Estado:** `done`

---

## Objetivo

Que el registry pueda contestar "¿qué proyectos registrados viven **dentro** de esta ruta?" y, si son
varios, si comparten grupo. Es la consulta que hoy no existe y por la que el servidor se rinde.

## Contexto verificado

`client.list(false)` devuelve filas con `name`, `path`, `config_path` y demás — ya se usa en
`unbound_help` (`crates/devctx-mcp/src/state.rs:926`) y en `registry_snapshot`
(`crates/devctx-cli/src/main.rs:1099`). El `path` de cada fila es absoluto.

El `group` NO viene en las filas de `list`: vive en `project.group` del `config.yaml` de cada repo
(verificado: los 11 repos REVFA tienen `group: REVFA`). Hay que leerlo por `config_path`, o
extender la fila del registry. **Preferir leer el config**: no cambia el esquema (PLAN §4).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs`

## Pasos

- [ ] **Paso 1 — `projects_under(cwd) -> Vec<ProjectRow>`.** Filtrar `client.list(false)` por filas
      cuyo `path` sea descendiente de `cwd`. Comparar por componentes de ruta canonizada, NO por
      `starts_with` de string: `/a/revfa` no debe matchear `/a/revfa-otro`.
- [ ] **Paso 2 — Excluir el caso exacto.** Si `path == cwd` no es "descendiente": ese caso ya lo
      resuelve `load_project()` hacia arriba y no debe entrar acá.
- [ ] **Paso 3 — `group_of(row) -> Option<String>`.** Leer `project.group` del `config_path` de la
      fila. Vacío o ausente → `None`.
- [ ] **Paso 4 — `resolve_under(cwd) -> Resolution`** con el enum:
      `Single(row)` | `Group { name, members }` | `Ambiguous(Vec<row>)` | `Empty`.
      `Group` solo si TODOS los descendientes tienen el mismo grupo no vacío; si uno solo difiere o
      está vacío → `Ambiguous`.
- [ ] **Paso 5 — Anidamiento.** Si un descendiente está dentro de otro descendiente, quedarse con el
      más profundo que contenga al cwd… y si ninguno contiene al cwd, con todos los de primer nivel.
      Documentar la decisión en el código.

## Criterios de aceptación

- [ ] Con cwd = raíz de un workspace con N repos registrados del mismo grupo → `Group` con los N.
- [ ] Con cwd = raíz con un solo repo registrado adentro → `Single`.
- [ ] Con repos de grupos distintos (o alguno sin grupo) → `Ambiguous`, nunca `Group`.
- [ ] Sin nada registrado adentro → `Empty`.
- [ ] `/tmp/revfa` NO devuelve los proyectos de `/tmp/revfa-otro` (prueba explícita del prefijo).
- [ ] La función no abre ningún store de proyecto: solo registry + lectura de `config.yaml`.

## Riesgos

Leer N `config.yaml` en el arranque cuesta N lecturas de disco. Con 11 repos es despreciable, pero
si el registry crece a cientos conviene cachear. Medir antes de optimizar.

## Resultado

- **Estado final:** `done`
- **Resumen:** `projects_under` + `resolve_under` + `group_of` en state.rs. Comparacion por componentes de ruta canonizada, no por prefijo de string.
- **Archivos tocados:** crates/devctx-mcp/src/state.rs
- **Verificado por:** Test `a_name_prefix_is_not_a_parent_directory` (verde): /x/ws no captura /x/ws-other. Los 4 casos de Resolution los cubren los otros tests de mcp_binding.rs.
- **Desviaciones:** Se agrego una quinta variante NO planeada, `Resolution::RegistryUnavailable`: la version original tragaba el error del registry y devolvia vacio, volviendo indistinguible «no hay proyectos» de «no pude leer». Lo detectaron los tests. Ademas `ProjectRow` gano `embed_dim`, que necesita TASK-010.
