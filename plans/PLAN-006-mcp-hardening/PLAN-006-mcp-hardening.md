# PLAN-006 — Endurecimiento del MCP: presupuesto, handshake y un 404 real

**Fecha:** 2026-09-12
**Fase:** 2 (Ejecución) — implementado en la misma sesión que este plan. Rama `fix/mcp-hardening`
(sale de `feature/plan-status`, verde: 377 tests, clippy limpio).
**Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
**Origen:** Cuatro hallazgos puntuales pedidos por el usuario, sin relación entre sí salvo que
todos tocan la superficie MCP/API: presupuesto de salida incompleto, un chequeo de red bloqueando
el handshake, un 404 reportado en `memory_forget`, y verificar en caliente los endpoints de
PLAN-005 (que solo tenían tests unitarios).

---

## 1. Qué resuelve

1. `DEVCTX_MAX_OUTPUT_TOKENS` (`crates/devctx-mcp/src/state.rs`, función `env_usize`, default
   8000) solo se aplicaba a `recall` y `read_file`. `search`, `get_references`, `impact_analysis`,
   `memory_context`, `memories_by_symbol`, `memories_by_file` y `search_routes` podían devolver
   payloads sin tope — por encima del límite propio de 25k tokens de Claude Code, que vuelca la
   salida a un archivo.
2. `devctx mcp` corría un chequeo de actualización con timeout de red de 4s **antes** de servir
   (`crates/devctx-cli/src/main.rs`, `cmd_mcp`), demorando el handshake de todo cliente MCP.
3. El usuario reportó `memory_forget` devolviendo 404 sobre memorias `group`, verificado dos veces
   sobre una memoria que `recall` sí encontraba.
4. Los endpoints `/plans/status` y `/plans/graph` de PLAN-005 solo tenían tests unitarios; faltaba
   levantar el servidor real y golpearlos con `curl`.

## 2. Hallazgos verificados (2026-09-12)

### 2.1 Presupuesto de salida — qué tools quedaban afuera

Contra `crates/devctx-mcp/src/state.rs` en HEAD de `feature/plan-status` (`0c767d4`):

- `do_recall`/`do_recall_scoped`/`do_recall_global` (recall) y `do_read_file` (`cap_whole_file`,
  `state.rs:519-550` aprox.) eran las únicas funciones que llamaban `env_usize("DEVCTX_MAX_OUTPUT_TOKENS", …)`.
- `do_search` (línea ~349), `do_search_project` (~1952), `do_references`/`get_references` (~3163),
  `do_impact`/`impact_analysis` (~2693), `do_memory_context` (~2660), `linked_response` (usada por
  `memories_by_symbol` ~2922 y `memories_by_file` ~2944) y `routes_to_json` (usada por
  `search_routes`/`routes_for_handler` ~3184) no tenían ningún tope: un `search` sobre una consulta
  amplia, o un `impact_analysis` con blast radius grande, podían devolver miles de items.
- `do_build_context` (~2750) ya se autolimita con su propio parámetro `max_tokens` (default 4096) y
  un contador `used`/`dropped` — no necesitaba `DEVCTX_MAX_OUTPUT_TOKENS`; se deja igual y se anota
  por qué en el código.

### 2.2 El chequeo de actualización bloquea el handshake

`crates/devctx-cli/src/main.rs`, `cmd_mcp` (línea ~1363): llamaba
`update_check::available(RELEASE_REPO, …)` en línea recta, antes de armar el `binding` y de que
`devctx_mcp` empiece a servir sobre stdio. `update_check::fetch_latest`
(`crates/devctx-cli/src/update_check.rs:57-69`) tiene un timeout de red de 4s
(`Duration::from_secs(4)`). Sin caché (primera vez, o pasadas 24h) esos hasta 4s se suman al tiempo
que un cliente MCP espera la respuesta a `initialize`.

### 2.3 El 404 de `memory_forget` — NO es un problema de scope

La hipótesis de partida (¿el delete filtra por una clave de proyecto que las memorias `group` no
llevan?) se descarta con evidencia:

- `devctx_store::Store::forget_memory` (`crates/devctx-store/src/memory.rs:196-212`) borra por
  `WHERE id = ?`, sin filtro de proyecto — cualquier id se borra desde cualquier store que lo tenga.
- El handler del daemon central, `forget_memory` en `crates/devctx-api/src/central.rs:512-518`,
  llama exactamente a ese mismo `store().forget_memory(&id)` sin scope adicional.

La causa real está una capa más abajo, en el enrutamiento HTTP del backend **por proyecto** (no el
central): `Backend::Remote::memory_forget` (`crates/devctx-mcp/src/backend.rs:372-377`) hace
`r.post("/memory/forget", …)`, y `Backend::Remote::memory_move` hace `r.post("/memory/move", …)`
contra el daemon por-proyecto que `devctx mcp` levanta con `remote::ensure(&cfg)`
(`crates/devctx-cli/src/main.rs:1470`, el camino normal de `mcp_backend`). El router de ese daemon
— `crates/devctx-api/src/lib.rs`, función `router()` (antes de este plan, líneas 42-71) — **nunca
tuvo rutas `/memory/forget` ni `/memory/move`**: ni la ruta ni el `use` de `do_memory_forget`/
`do_memory_move` existían en `devctx-api/src/lib.rs` (sí en `devctx-mcp/src/state.rs`, y sí las
usa `Backend::Local`). Con eso, cualquier `memory_forget` enrutado por un daemon por-proyecto —
que es el modo normal cuando `devctx mcp` encuentra o levanta uno, para no competir por el único
escritor de DuckDB — cae en el fallback 404 de axum: la llamada nunca llega a
`do_memory_forget`, así que el `String` "not in this project, and the shared store is
unreachable" tampoco aparece — es un 404 de axum, literal, sobre una ruta que no existe.

Que el usuario lo haya visto con una memoria `group` es consistente (esas necesariamente pasan por
el `central()` fallback dentro de `do_memory_forget`, así que solo se ejercitan con red de por
medio), pero el bug no es específico de `group`: **cualquier** `memory_forget`/`memory_move`
enrutado por `Backend::Remote` lo pega, sea la memoria local o compartida. No se pudo escribir un
test que reproduzca el 404 exacto sin levantar un servidor axum real (los tests unitarios de
`devctx-mcp` no montan HTTP) — la cobertura queda en TASK-003 como test de integración liviano que
llama al handler `memory_forget` de `devctx-api` directamente.

### 2.4 Los endpoints del dashboard (PLAN-005) sí sirven

`crates/devctx-api/src/lib.rs` registra `/plans/status` y `/plans/graph`
(`plans_status`/`plans_graph`, delegan a `do_plan_status`/`do_plan_graph`). Solo tenían
`cargo test` unitario sobre las funciones `do_*`; faltaba levantar `devctx web` y golpear las rutas
reales — hecho en TASK-004, con la salida pegada en el reporte de cierre.

## 3. Decisión

Cuatro tasks independientes entre sí (comparten archivo en algunos casos, pero no lógica), cada una
con su propio commit convencional:

| Task | Qué | Depende de | Estado |
|------|-----|------------|--------|
| TASK-001 | Presupuesto de salida en las tools que no lo tenían (`fit_json_array`, reuso de `fit_memories`) | — | `done` |
| TASK-002 | El chequeo de actualización no bloquea el handshake de `devctx mcp` | — | `done` |
| TASK-003 | Rutas `/memory/forget` y `/memory/move` faltantes en `devctx-api` (causa real del 404) | — | `done` |
| TASK-004 | Verificación en caliente de `/plans/status` y `/plans/graph` | — | `done` |

No hay orden real de dependencia: las cuatro tocan archivos distintos (TASK-001 y TASK-003 rozan
`devctx-mcp`/`devctx-api` pero en funciones separadas) y se hicieron en secuencia en la misma
sesión para evitar pisarse en `state.rs`.

## 4. Riesgos

- **`fit_json_array` es un presupuesto blando, no un tope duro.** Igual que `fit_memories`, un
  ítem chico siempre sobrevive gracias al piso (`MIN_SHARE_TOKENS`), así que una lista con muchos
  ítems diminutos puede superar el presupuesto nominal. Es la misma filosofía que ya regía
  `recall`; documentado en el comentario de la función.
- **El fix del 404 solo cubre el daemon por-proyecto.** Si en el futuro aparece un daemon
  "central" con su propio bug de rutas, este plan no lo toca — ya tiene sus rutas correctas
  (`crates/devctx-api/src/central.rs:54`).
- **El chequeo de actualización en background puede perder la carrera con el primer `list_projects`.**
  Es aceptable: el día uno sin caché ya devolvía "sin novedades" hasta que el chequeo terminaba;
  ahora el peor caso es el mismo, solo que nunca bloquea el handshake.

## 5. Verificación

- `cargo test --workspace` y `cargo clippy --workspace --all-targets` en verde (conteos en el
  cierre, §6).
- TASK-001: tests unitarios sobre `fit_json_array` y sobre `routes_to_json` con fixtures
  sobredimensionadas.
- TASK-002: tiempo de respuesta a `initialize` sobre stdin, antes/después (cierre, §6).
- TASK-003: se agrega un test de integración en `devctx-api` que llama al handler `memory_forget`
  con un id existente y confirma 200 (no 404) — cubre la regresión sin depender del modelo de
  embeddings.
- TASK-004: `devctx web` real en un puerto libre, `curl /plans/status` y `curl /plans/graph`,
  salida pegada en el cierre.

## 6. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
