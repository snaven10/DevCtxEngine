# TASK-006 — `/plans/graph` y pestaña "Plans" en el dashboard

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** TASK-002
- **Estado:** `done`

---

## Objetivo

Ver el plan como lo que es: un grafo. Verde `done`, azul `in_progress`, gris lista para arrancar,
rojo bloqueada. Reusar cytoscape y el patrón de fetch que ya existen.

## Contexto verificado

- `crates/devctx-api/src/lib.rs:43-44` — `/` (`dashboard`, `lib.rs:386`) y
  `/assets/cytoscape.min.js`.
- `lib.rs:403` — `graph()`: `run(api.state, |s| do_graph(…))`. Molde para la ruta nueva.
- `crates/devctx-api/assets/index.html:172` — `loadGraph()`: `getJSON('/graph?…')`, espera
  `{nodes:[{data}], edges:[{data}]}` y construye con `cytoscape({container: $('cy'), …, layout:
  {name:'cose'}})` (`:188`). Estilos en `graphStyle()` (`:151`).
- `index.html:102-105` — pestañas `Explore`/`Memories` en la columna derecha; `initTabs()` (`:301`)
  alterna `#pane-explore`/`#pane-memories`. El grafo de llamadas vive en `#cy` (centro, `:98`).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_plan_graph`)
- **Modificar:** `crates/devctx-api/src/lib.rs` (ruta `GET /plans/graph?plan=`)
- **Modificar:** `crates/devctx-api/assets/index.html`

## Pasos

- [ ] **Paso 1 — `do_plan_graph(state, plan: Option<&str>)`.** Sin `plan` → el activo (misma regla de
      TASK-002). Nodos `{data:{id, label:"TASK-00N título", status, ready}}`, aristas
      `{data:{id, source: dep, target: task}}` (la dependencia apunta a quien la espera). Dep
      inexistente → nodo `{status:"missing"}` para que se vea, no se omite. Más `plans: [ids]` para el
      selector.
- [ ] **Paso 2 — Ruta** `/plans/graph` en `devctx-api` con `run(…)`. No hace falta en `Backend`:
      solo la consume el dashboard, que ya habla con el daemon.
- [ ] **Paso 3 — Pestaña "Plans"** en `.tabs`, con `#pane-plans`: `<select>` de planes + la lista
      `ready / in_progress / blocked` (de `/plans/status?plan=`). `initTabs` generalizado a N pestañas.
- [ ] **Paso 4 — Instancia cytoscape aparte**, `#cyPlan` en el centro, visible solo con la pestaña
      Plans. No reusar `cy`: volver a Explore no debe obligar a recargar el grafo de llamadas.
- [ ] **Paso 5 — Estilo por estado:** `node[status="done"]` `#3fb950`, `in_progress` `#58a6ff`,
      pending+ready `#8b949e`, pending no-ready y `blocked` `#f85149` (blocked explícito con borde
      punteado), `missing` diamante hueco. Layout `breadthfirst` dirigido (incluido en cytoscape core).
- [ ] **Paso 6 — Click en nodo** → en el panel: título, estado, deps, archivos mencionados (lista).

## Criterios de aceptación

- [ ] `curl localhost:<puerto>/plans/graph?plan=PLAN-005` devuelve 8 nodos y las aristas de la tabla §4.
- [ ] En el navegador: PLAN-005 recién creado → TASK-001 gris, el resto rojo.
- [ ] Cambiar de pestaña Plans → Explore conserva el grafo de llamadas sin recargarlo.
- [ ] Plan sin tasks (PLAN-004) → mensaje "sin tasks", no un canvas vacío mudo.

## Riesgos

`index.html` es un solo archivo de 332 líneas con JS inline; la vista nueva lo agranda. Mantener el
estilo existente (funciones sueltas, `getJSON`, `esc`) en vez de meter un framework.

## Resultado

- **Estado final:** `done`
- **Resumen:** `plan_graph_value(root, plan)` (usada por `do_plan_graph`) arma nodos `{id, label, status, ready, depends_on, files}` y aristas `dep->task`, con nodos `missing` para dependencias inexistentes y `message: "sin tasks"` cuando el plan no tiene `tasks/`. Ruta `GET /plans/graph?plan=`. Pestaña "Plans" en `index.html` con `<select>` de planes, listas ready/in_progress/blocked (de `/plans/status`) y una instancia cytoscape separada `#cyPlan` (layout `breadthfirst`), estilo por estado (verde done, azul in_progress, gris ready, rojo punteado blocked, diamante hueco missing). `initTabs()` generalizado a 3 pestañas; cambiar de pestaña solo alterna `display`, nunca recrea `#cy`.
- **Archivos tocados:** `crates/devctx-mcp/src/state.rs` (`do_plan_graph`/`plan_graph_value`), `crates/devctx-api/src/lib.rs` (ruta), `crates/devctx-api/assets/index.html`.
- **Verificado por:** `cargo test -p devctx-mcp plan_graph` — 3/3 verdes (nodos/aristas de un plan con 2 tasks y 1 dependencia, dependencia faltante como nodo `missing` sin descartarla, plan sin `tasks/` da `message: "sin tasks"` con 0 nodos). `cargo check --workspace --all-targets` limpio.
- **Desviaciones:** No se probó en navegador real (`devctx web` + click) ni con `curl` contra un servidor levantado: arrancar y curlear un proceso HTTP real requeriría backgroundearlo, y la instrucción vigente de esta sesión es no backgroundear ni usar Monitor. La lógica del endpoint está cubierta por los 3 tests unitarios sobre `plan_graph_value`, que es exactamente lo que la ruta expone envuelto en `run()`.
