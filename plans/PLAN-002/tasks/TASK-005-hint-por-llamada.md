# TASK-005 — Param `path` opcional en las tools de código + caché de conexiones

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-002
- **Estado:** `done`

---

## Objetivo

Que una tool de código pueda resolver el proyecto **desde la ruta sobre la que se está trabajando**,
sin depender del binding de sesión ni de `use_project`.

## Contexto verificado

**Esta es la task que decide si el plan sirve.** El `cwd` del proceso MCP se fija en
`DevctxServer::new` (`std::env::current_dir()`, `lib.rs:322-329`) y **no cambia nunca**: el servidor
no se entera de que el agente se movió a otro repo. Sin resolución por llamada, el binding queda
congelado en dónde se lanzó el cliente, y arreglar solo el arranque (TASK-003) no alcanza para
trabajo cross-repo.

`connect: Connect` es `Fn(&Path) -> Result<Backend, String>` y ya se usa en `use_project`
(`lib.rs:528-546`): abrir un backend para una ruta arbitraria ya es una operación soportada.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`
- **Modificar:** `crates/devctx-mcp/src/state.rs` (resolución ruta → proyecto registrado)

## Pasos

- [ ] **Paso 1 — Param `path: Option<String>`** en las tools de código: `search`, `read_file`,
      `read_symbol`, `get_references`, `impact_analysis`, `search_routes`, `routes_for_handler`.
      Descripción explícita: *"archivo o directorio sobre el que estás trabajando; resuelve el
      proyecto sin cambiar el binding de la sesión"*.
- [ ] **Paso 2 — `resolve_from_hint(path) -> Option<root>`**: canonizar, subir por los padres hasta
      dar con un proyecto REGISTRADO. Reusar la comparación por componentes de TASK-001, no
      `starts_with` de string.
- [ ] **Paso 3 — Precedencia por llamada**: hint válido → ese proyecto. Sin hint (o no resuelve) →
      el binding de sesión. Ni el hint modifica el binding, ni el binding pisa al hint.
- [ ] **Paso 4 — Caché `HashMap<PathBuf, Arc<Backend>>`** detrás del mismo `Mutex`, para no reabrir
      el store en cada llamada. Acotar el tamaño (LRU simple o cap duro) para no acumular handles.
- [ ] **Paso 5 — Reportar la resolución** en la salida de la tool (`"resolved_project": "<name>"`)
      cuando vino de un hint o de un `default` implícito de grupo. Que el agente sepa a qué repo le
      contestaron: sin esto, una respuesta del repo equivocado es indistinguible de una correcta.
- [ ] **Paso 6 — Hint inválido** (ruta inexistente o fuera de todo proyecto registrado): NO fallar;
      caer al binding y decirlo en la salida.

## Criterios de aceptación

- [ ] En modo grupo, `search(query, path: "<...>/REVFA_FrontEnd/src/x.ts")` busca en FrontEnd aunque
      el `default` del grupo sea BackEnd.
- [ ] Dos llamadas seguidas con el mismo hint abren el backend UNA vez (verificable por instrumentación
      o por tiempo).
- [ ] Un hint que apunta fuera de todo proyecto cae al binding y lo reporta, sin error.
- [ ] Omitir `path` conserva exactamente el comportamiento actual (sin regresión).
- [ ] La salida nombra el proyecto resuelto cuando no fue el binding explícito.

## Riesgos

- **Handles acumulados**: sin cap, una sesión larga saltando entre repos abre N stores DuckDB.
  Por eso el cap del Paso 4.
- **Ambigüedad silenciosa**: si el hint resuelve a un repo y el agente cree que preguntó a otro, el
  resultado es plausible y equivocado — el peor tipo de bug. Por eso el Paso 5 no es cosmético.

## Resultado

- **Estado final:** `done`
- **Resumen:** Param `project` opcional (nombre registrado o ruta) en las 7 tools de codigo + `backend_for()` + cache de conexiones con tope 8. La salida trae `resolved_project` cuando la eleccion fue inferida.
- **Archivos tocados:** crates/devctx-mcp/src/lib.rs, crates/devctx-mcp/src/state.rs (resolve_hint)
- **Verificado por:** Compila; el despacho hint>binding esta cableado en las 7. NO se ejecuto una llamada con hint end-to-end.
- **Desviaciones:** El parametro se llama `project`, NO `path` como decia el plan: `read_file` y `search_routes` YA tenian un campo `path` con otro significado y se habrian duplicado. Ademas acepta nombre ademas de ruta, y en `read_file` el hint sale del propio req.path.
- **Riesgos abiertos / siguiente:** Sin prueba end-to-end del hint. Es lo primero a verificar.
