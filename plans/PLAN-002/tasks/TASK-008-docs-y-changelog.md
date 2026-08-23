# TASK-008 — Docs (EN + ES) y nota de cambio de comportamiento

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-003, TASK-005, TASK-006
- **Estado:** `done`

---

## Objetivo

Que la documentación deje de describir el binding manual como el camino normal, y que el cambio de
default de `scope` quede declarado donde alguien lo va a leer.

## Contexto verificado

`docs/03-core-concepts/mcp-integration.md` §"Project binding" hoy dice:

> `devctx mcp --project <path>` sets the project root explicitly. Without it, the root is discovered
> from the working directory. […] When that happens, tools report that no project is bound. The
> recovery is two calls: `list_projects` → `use_project <name>`.

Eso queda **obsoleto**: tras este plan, el caso que describe se resuelve solo. El doc tiene versión
en español enlazada (`../es/03-conceptos-fundamentales/integracion-mcp.md`) — ambas se actualizan
o quedan desincronizadas.

La descripción de la tool `use_project` (`crates/devctx-mcp/src/lib.rs:522`) también dice "Needed
when the server was started outside any repository", que ya no es cierto en general.

## Archivos

- **Modificar:** `docs/03-core-concepts/mcp-integration.md`
- **Modificar:** `docs/es/03-conceptos-fundamentales/integracion-mcp.md`
- **Modificar:** `crates/devctx-mcp/src/lib.rs` (descripción de `use_project` y de las tools con `path`)
- **Modificar:** `CHANGELOG.md` (si existe; si no, la nota de release)

## Pasos

- [ ] **Paso 1 — Reescribir §"Project binding"** con la precedencia real de TASK-003 (los 4 niveles)
      y el modo grupo. Explicar el descenso con el ejemplo del workspace de N repos.
- [ ] **Paso 2 — Documentar el hint `path`** como la forma recomendada de trabajo cross-repo, y
      `use_project` como lo que pasa a ser: un override explícito, no el camino normal.
- [ ] **Paso 3 — Espejar en español.** Misma estructura, no una traducción literal desfasada.
- [ ] **Paso 4 — Actualizar la descripción de la tool `use_project`**: es lo que lee el agente, y
      hoy le dice que es "necesario" cuando ya no lo es.
- [ ] **Paso 5 — Changelog con el cambio de comportamiento**, en dos líneas explícitas:
      (a) desde la raíz de un workspace el servidor ahora arranca bindeado al grupo;
      (b) en modo grupo, `remember` sin `scope` guarda en `group`, no en `local`.
- [ ] **Paso 6 — Nota de atribución**: el campo `repo` de una memoria sale del binding, así que en
      modo grupo refleja el miembro `default`. Decirlo, porque es la parte contraintuitiva.

## Criterios de aceptación

- [ ] Ni el doc EN ni el ES describen `use_project` como el camino normal para el caso resuelto.
- [ ] La precedencia de 4 niveles está escrita, en el mismo orden que el código.
- [ ] Los dos cambios de comportamiento están en el changelog, cada uno en su línea.
- [ ] La descripción de las tools con `path` explica que NO cambia el binding de sesión.
- [ ] EN y ES dicen lo mismo (revisar lado a lado, no de memoria).

## Riesgos

Ninguno de runtime. El riesgo real es omitirla: sin esto la próxima persona (o la próxima sesión)
va a leer el doc viejo y concluir que el binding manual sigue siendo necesario — que es exactamente
como se perpetúan las instrucciones que ya no se ejecutan.

## Resultado

- **Estado final:** `done`
- **Resumen:** Reescrita la seccion «Project binding» EN y ES con la precedencia de 4 niveles, el modo grupo y el hint. Descripcion de la tool use_project corregida.
- **Archivos tocados:** docs/03-core-concepts/mcp-integration.md, docs/es/03-conceptos-fundamentales/integracion-mcp.md, crates/devctx-mcp/src/lib.rs, docs/08-design-decisions.md
- **Verificado por:** Revision lado a lado EN/ES.
- **Desviaciones:** No hay CHANGELOG.md en el repo. En vez de inventar el archivo se escribio **ADR-18** en docs/08-design-decisions.md, que es la convencion que este repo ya usa.
- **Riesgos abiertos / siguiente:** ADR-18 documenta el «miembro default», que TASK-009/010 despues corrigieron. HAY QUE ACTUALIZARLO antes de cerrar el plan.
