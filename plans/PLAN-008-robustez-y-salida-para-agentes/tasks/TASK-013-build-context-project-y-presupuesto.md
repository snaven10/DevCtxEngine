# TASK-013 — `build_context`: `project`, selección en grupo y presupuesto por relevancia

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet; opus si la selección en grupo se complica)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-010, TASK-011, TASK-012
- **Estado:** `pending`

---

## Objetivo

Que `build_context` arme contexto del repo correcto y con el código antes que la prosa, sin cortar
en el primer chunk grande ni traer memorias truncadas (PLAN-008 B7).

## Contexto verificado

- `crates/devctx-mcp/src/lib.rs:232-239` — `BuildContextReq { query, max_tokens, include_memories }`,
  sin `project`; tool en `lib.rs:1018`.
- `crates/devctx-mcp/src/state.rs:2972-3093` — `do_build_context`: recall de 5 memorias en todos los
  tiers (`:2996`), `do_search(…, 30, None, SearchMode::Vector, false)` (`:3023`), corte con `break`
  en el primer chunk que no cabe (`:3044`), memorias por archivo de los 5 primeros archivos.
- Truncados: `do_recall_scoped` recorta memorias con "exceeded its share of the output budget"
  (`state.rs:2493`, `:2506`, `:2564`) antes de que `build_context` las vea.
- Grupo: `DevctxServer::backend_for` (`lib.rs:492-512`) devuelve `Binding::Group.default` sin hint.
- Campo: sesión de grupo revfa → tarea de FrontEnd respondida desde `tickets-srv`.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`,
  `crates/devctx-mcp/src/backend.rs`, `crates/devctx-api/src/lib.rs`

## Pasos

- [ ] **Paso 1 — `project`.** Parámetro `project` como en las demás tools (acepta un repo distinto del
      bindeado en sesiones de grupo).
- [ ] **Paso 2 — Grupo sin `project`.** Correr la búsqueda en cada miembro (reusar la federación de
      `search` en grupo) y elegir el repo con mejor score top-k; nombrarlo en la salida
      (`[devctx] context from <repo> (best match among N members)`). Si el margen entre el primero y
      el segundo es chico, devolver error pidiendo `project` con los candidatos. Nunca default
      silencioso.
- [ ] **Paso 3 — Ranking.** Búsqueda `hybrid` con las penalizaciones de TASK-011 y dedup de TASK-012;
      `kind`/`include_tests` pasan derecho.
- [ ] **Paso 4 — Presupuesto.** Reemplazar `break` por "saltar y seguir" (un chunk grande no impide
      los siguientes que sí caben); dentro de un chunk de código, recortar comentarios de
      documentación iniciales largos antes que el cuerpo; memorias como título + primeras líneas con
      `id` (sin la marca de truncado por cada una) y una sola línea final con el conteo de omitidos.

## Criterios de aceptación

- [ ] Test: grupo de 2 miembros, consulta que solo matchea en el no-default → contexto del correcto,
      nombrado.
- [ ] Test: consulta ambigua entre miembros → error que lista candidatos y pide `project`.
- [ ] Test: primer hit más grande que el presupuesto → los siguientes que caben aparecen.
- [ ] Test: ningún "exceeded its share" en la salida de `build_context`; sí un conteo final.
- [ ] En el Resultado: la consulta de campo (FrontEnd en revfa) antes/después.

## Riesgos

La selección en grupo cuesta N búsquedas (una por miembro): mismo costo que `search` en grupo hoy;
medirlo y reportarlo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
