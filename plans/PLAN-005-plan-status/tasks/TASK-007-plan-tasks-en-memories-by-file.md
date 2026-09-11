# TASK-007 — `memories_by_file` devuelve también las tasks que mencionan el archivo

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Parado sobre un archivo, `memories_by_file` contesta "¿qué se decidió sobre esto?". Sumarle "¿qué
task de qué plan lo va a tocar o lo tocó?" cuesta una lectura de `plans/` y **cero almacenamiento**.

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:2676` — `do_memories_by_file(state, file, limit)`: consulta la
  junction, resuelve nombre pelado vía `file_index().resolve`, y devuelve `linked_response(…)`
  (`state.rs:2723`), un `String` JSON objeto.
- `crates/devctx-mcp/src/backend.rs:430` — Local → `do_memories_by_file`; Remote →
  `GET /memories/by-file/:file`, que en el daemon (`crates/devctx-api/src/lib.rs:603`) llama **la
  misma** `do_memories_by_file`. Cambiar la función cubre los dos caminos.
- `crates/devctx-mcp/src/lib.rs:517` — `note(out, key, value)` ya agrega un campo a un objeto JSON;
  el mismo enfoque sirve acá sin tocar `linked_response`.
- Referencias peladas en tasks reales: `state.rs`, `lib.rs`, `graph.rs`, `lang.rs:177` (PLAN-002/003).
  `lib.rs` existe en los 15 crates.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_memories_by_file`)

## Pasos

- [ ] **Paso 1 — Regla de emparejamiento** (en `devctx_core::plans`, pura): una `FileRef` matchea la
      consulta si (a) son iguales, o (b) ambas tienen ≥ 2 componentes y una es sufijo por componentes
      de la otra. **Una ref pelada (`lib.rs`) solo matchea una consulta pelada idéntica.** Así
      `crates/devctx-mcp/src/state.rs` no trae cada task que dijo `lib.rs`.
- [ ] **Paso 2 — Campo nuevo** `plan_tasks: [{plan, task, title, status, line?}]` en la respuesta,
      ordenado: no-done primero, luego por plan descendente. Tope 10. Campo **siempre presente**
      (vacío = no hay), para no confundir "no hay" con "versión vieja".
- [ ] **Paso 3 — Usar el archivo resuelto.** Si la consulta era pelada y `file_index().resolve` la
      resolvió, emparejar con la ruta resuelta **y** con la pelada.
- [ ] **Paso 4 — Fallo del parser no rompe la tool.** Si `plans/` no se puede leer, `plan_tasks: []`
      y la parte de memorias igual contesta.

## Criterios de aceptación

- [ ] Test: `plans/` fixture con una task que menciona `` `crates/a/src/x.rs:10` ``;
      `memories_by_file("crates/a/src/x.rs")` trae esa task con `line: 10`.
- [ ] Una task que menciona `` `lib.rs` `` NO aparece para `crates/a/src/lib.rs`.
- [ ] Archivo sin memorias pero mencionado en una task → `memories: []`, `plan_tasks: [1]`.
- [ ] La forma previa de la respuesta no cambia (solo se agrega un campo).

## Riesgos

- Costo: una lectura de `plans/` por llamada. Mismo orden que TASK-002.
- **`memories_by_symbol` queda fuera** (PLAN §7): símbolo → archivo exige resolver definiciones, y con
  nombre pelado es inferencia (21 declaraciones de `actualizar` en REVFA_BackEnd, PLAN-003).

## Resultado

<!-- SE LLENA AL CERRAR -->
