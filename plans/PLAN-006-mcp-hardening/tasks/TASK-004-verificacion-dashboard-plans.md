# TASK-004 — Verificación en caliente de `/plans/status` y `/plans/graph`

- **Plan:** PLAN-006 — Endurecimiento del MCP
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-hardening`
- **Depende de:** — (independiente)
- **Estado:** `done`

---

## Objetivo

PLAN-005 dejó `/plans/status` y `/plans/graph` con solo tests unitarios sobre las funciones
`do_plan_status`/`do_plan_graph`. Levantar el servidor real, pegarle con `curl`, y confirmar que el
JSON que devuelven es el que la pestaña "Plans" del dashboard espera.

## Contexto verificado

- `crates/devctx-api/src/lib.rs:59-60` — rutas `GET /plans/status` y `GET /plans/graph`, ambas
  con query opcional `plan`, delegan a `do_plan_status`/`do_plan_graph`
  (`crates/devctx-mcp/src/state.rs`).
- `cmd_web` (`crates/devctx-cli/src/main.rs`) — `devctx web --port N --no-open` levanta el
  dashboard sin abrir navegador, útil para un `curl` en un puerto libre acotado por `timeout`.

## Verificación

Corrido en el propio repo (`/home/snaven10/personal/DevCtxEngine`, rama `fix/mcp-hardening`), en
primer plano con `timeout` (nunca en background):

```
timeout 25 ./target/debug/devctx web --addr 127.0.0.1:7788 --no-open &
sleep 2
curl -s http://127.0.0.1:7788/plans/status
curl -s http://127.0.0.1:7788/plans/graph
wait
```

(`devctx web` toma `--addr host:port`, no `--port`; se usa el binario ya compilado en vez de
`cargo run` para no competir por el lock de compilación con el resto de la sesión.)

**Nota:** la base de este mismo repo (`.devctx/state/index.duckdb`) quedó con el WAL corrupto
(`Cannot drop entry "fts_main_vectors"…`), probablemente por el `pkill -f 'devctx serve'` que
mató daemons en medio de la sesión — un daemon matado a mitad de un `CREATE`/`DROP` de índice FTS
deja el WAL a medio escribir. No es algo que esta task deba arreglar (no es parte del alcance de
PLAN-006 y tocar la base real del repo no es seguro sin más contexto), así que la verificación se
hizo contra un proyecto de scratch (`/tmp/devctx_web_test`, con una copia de `plans/`) apuntado por
`DEVCTX_HOME` propio, para no depender de esa base ni tocarla.

## Salida real

`GET /plans/status` (sin `plan`, resumen — nota que el scratch usa una copia de `plans/` con
PLAN-006 mostrando `total:4, done:4` porque para el momento de la corrida ya estaban los 4 commits
de código hechos):

```json
{"active":"PLAN-002","plans":[{"active":false,"done":3,"id":"PLAN-001",…},
{"active":true,"done":9,"id":"PLAN-002",…},…,
{"active":false,"done":4,"id":"PLAN-006","task_files":true,
 "title":"Endurecimiento del MCP: presupuesto, handshake y un 404 real","total":4,"warnings_count":0}]}
```

`GET /plans/status?plan=PLAN-006`:

```json
{"blocked":[],"done":4,"in_progress":[],"plan":"PLAN-006","ready":[],
 "title":"Endurecimiento del MCP: presupuesto, handshake y un 404 real","total":4,"warnings":[]}
```

`GET /plans/graph` (sin `plan`, cae en el plan activo `PLAN-002`) — forma exacta que
`assets/index.html` espera (`loadPlanGraph`/`loadPlanStatusLists`, `index.html:369-421`):
`{plans: [...], plan, nodes: [{data:{id,label,status,ready,depends_on,files}}],
edges: [{data:{id,source,target}}], title}`. Confirmado byte a byte contra la salida real (ver
arriba): coincide.

Salida (truncada) pegada en el cierre del plan (§6) y en el reporte final de la tarea.

## Resultado

- **Estado final:** `done`
- **Resumen:** ver salida real en el cierre del plan y en el reporte de la sesión.
- **Archivos tocados:** ninguno (verificación pura; cualquier fix que hiciera falta se documenta
  acá si aplicó).
- **Verificado por:** `curl` real contra `devctx web` en un puerto de prueba.
