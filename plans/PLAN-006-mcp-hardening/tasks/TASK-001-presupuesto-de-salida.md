# TASK-001 — Presupuesto de salida en las tools que no lo tenían

- **Plan:** PLAN-006 — Endurecimiento del MCP
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-hardening`
- **Depende de:** — (independiente)
- **Estado:** `done`

---

## Objetivo

`DEVCTX_MAX_OUTPUT_TOKENS` (`crates/devctx-mcp/src/state.rs`, `env_usize`, default 8000) solo
regía `recall` y `read_file`. Aplicar el mismo patrón de `fit_memories` (reparto por partes
iguales, piso mínimo, `omitted_for_budget` explícito) a `search`, `get_references`,
`impact_analysis`, `memory_context`, `memories_by_symbol`, `memories_by_file` y `search_routes`.
Nunca truncar en silencio.

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:1003-1032` (`do_recall`) y `state.rs:2160-2260`
  (`do_recall_scoped`/`do_recall_global`) — únicas funciones que llamaban `env_usize` antes de
  este cambio, vía el helper `fit_memories` (`state.rs:2276` aprox., con reparto por ítem, piso
  `MIN_SHARE_TOKENS = 128` y tope `MAX_SUMMARIES = 3` antes de empezar a descartar).
- `do_search` (`state.rs:349`) devolvía `hits_to_json(&hits)` sin tope — un `search` de límite alto
  con chunks grandes en `text` no tenía techo.
- `do_search_project` (`state.rs:1952`) envuelve la salida de un subproceso `devctx search
  --format json` en `{project, path, hits}` — mismo problema, otro camino (no pasa por `do_search`).
- `do_references`/`get_references` (`state.rs:3163`) y `linked_response` (`state.rs:3050`, usada
  por `memories_by_symbol` y `memories_by_file`) — arrays sin tope.
- `do_impact`/`impact_analysis` (`state.rs:2693`) — dos arrays (`upstream`/`downstream`) que pueden
  crecer con la profundidad pedida.
- `routes_to_json` (`state.rs:3207`, usada por `search_routes` y `routes_for_handler`) — sin tope.
- `do_memory_context` (`state.rs:2660`) — mismo array `memories` que `recall`, pero sin pasar por
  `fit_memories`.
- `do_build_context` (`state.rs:2750`) **ya se autolimita** con su propio `max_tokens` (default
  4096) y un contador `used`/`dropped` interno — se deja sin tocar; el comentario en el código dice
  por qué.

## Cambios

- **Nueva función pura** `fit_json_array` (`state.rs`, junto a `fit_memories`): mismo reparto por
  ítem con piso `MIN_SHARE_TOKENS = 64`; si el ítem no entra en su parte y se le pasa un
  `text_field`, se trunca ese campo en el lugar (nota `[devctx] truncated: …` incluida); si no hay
  campo para achicar, se descarta y se nombra en `dropped` vía la función `label` que recibe el
  caller.
- `do_search`: si algo se recorta, cambia de array plano a `{ "results": […], "omitted_for_budget":
  {…} }`; si no, sigue siendo el array de siempre. `do_build_context` (que parsea la salida de
  `do_search`) se ajustó para aceptar ambas formas (`parse_memories` ahora también busca la clave
  `results`, no solo `memories`).
- `do_search_project`: mismo criterio sobre el campo `hits`.
- `do_references`: trunca `source` cuando hace falta; agrega `omitted_for_budget` con `file:line`.
- `do_impact`: `upstream` y `downstream` reciben cada una la mitad del presupuesto — un símbolo con
  radio enorme de un lado no le come el presupuesto al otro — y cada lista reporta sus propios
  descartes.
- `routes_to_json`: sin campo de texto para achicar (una ruta es toda metadatos cortos), así que un
  ítem que no entra en su parte se descarta directo; envuelve en `{routes, omitted_for_budget}`
  solo cuando hace falta.
- `linked_response` (memories_by_symbol/by_file): reusa `fit_memories` tal cual (sin summarizer,
  porque esta función no tiene `state`/embedder a mano) y agrega `omitted_for_budget` con títulos.
- `do_memory_context`: reusa `fit_memories`, esta vez con `do_summarize` como resumidor (si el
  extractor está disponible).

## Criterios de aceptación

- [x] `cargo test -p devctx-mcp --lib` verde: 40 tests, 9 nuevos para este cambio
      (`fit_json_array_*`, `routes_to_json_*`, `linked_response_style_output_budgets_an_oversized_memory`).
- [x] Ningún tool devuelve más de lo que cabe en `DEVCTX_MAX_OUTPUT_TOKENS` sin decir qué faltó.
- [x] `do_build_context` queda intacto (ya se autolimita) y sigue leyendo la salida de `do_search`
      tanto en su forma plana como envuelta.
- [x] `devctx-core/Cargo.toml` y `devctx-mcp/Cargo.toml` sin dependencias nuevas.

## Resultado

- **Estado final:** `done`
- **Resumen:** `fit_json_array` en `crates/devctx-mcp/src/state.rs`, cableada en `do_search`,
  `do_search_project`, `do_references`, `do_impact`, `routes_to_json` (usada por `search_routes` y
  `routes_for_handler`); `do_memory_context` y `linked_response` (memories_by_symbol/by_file)
  reusan `fit_memories`.
- **Archivos tocados:** `crates/devctx-mcp/src/state.rs`.
- **Verificado por:** 9 tests unitarios nuevos con fixtures sobredimensionadas, más los 31
  preexistentes, todos verdes (`cargo test -p devctx-mcp --lib`).
- **Desviaciones:** `fit_json_array` no acumula un total corrido entre ítems — el reparto es una
  parte fija por ítem (`presupuesto / cantidad`, con piso), igual que `fit_memories`. Es un
  presupuesto blando: una lista con muchos ítems diminutos puede superar el nominal. Se documenta
  en el comentario de la función y en el riesgo §4 del plan.
