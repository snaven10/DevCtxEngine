# TASK-012 — Dedup de chunks y anclaje por identificador exacto

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-006
- **Estado:** `pending`

---

## Objetivo

Que `search` no devuelva el mismo fragmento varias veces, y que una consulta que es (o contiene) un
identificador exacto ponga primero su definición (PLAN-008 B6; evidencia: GrepRAG, +7-15.6 % EM con
boost por identificador y dedup estructural).

## Contexto verificado

- `crates/devctx-search/src/lib.rs:121-122` — la RRF deduplica solo por id de punto.
- `crates/devctx-mcp/src/state.rs:485-506` — `search_branch` puede caer a "sin filtro" y traer la
  misma ruta de varias ramas (hipótesis de los duplicados; TASK-006 cambia la elección de rama).
- Campo: "how are memories linked to symbols" en DevCtxEngine → mismo chunk 3-5×; keyword
  `do_memories_by_symbol` no trae la definición (`state.rs:3147`).
- `crates/devctx-store/src/store.rs:428` — `keyword_search` (BM25); `store.rs:658` —
  `symbol_definitions` (exacto, luego `%.name`/`%::name`).

## Archivos

- **Modificar:** `crates/devctx-search/src/lib.rs`
- **Modificar:** `crates/devctx-mcp/src/state.rs` (si el anclaje usa `symbol_definitions`)

## Pasos

- [ ] **Paso 1 — Repro.** Reproducir los duplicados y anotar si difieren en rama, en id o en rango
      (solapados método/clase). Confirmar o corregir la hipótesis en el Resultado.
- [ ] **Paso 2 — Dedup.** Tras la fusión: misma `(file, start_line, end_line)` → uno (el de la rama
      elegida); rango contenido en otro ya devuelto del mismo archivo → se descarta el contenido si el
      contenedor ya está, o se reemplaza el contenedor por el más chico si el chico rankea más alto.
- [ ] **Paso 3 — Anclaje.** Si la consulta tiene tokens con forma de identificador (snake/camel, o
      `Clase.metodo`), en modo `keyword`/`hybrid` buscar `symbol_definitions` exactas y ponerlas al
      frente (boost fijo en la RRF), marcadas `anchored: true`.

## Criterios de aceptación

- [ ] Test: store con el mismo chunk en dos ramas y un método contenido en su clase → 0 duplicados
      `(file,start,end)` y sin el par contenedor/contenido.
- [ ] Test: `search("do_memories_by_symbol", mode: keyword)` → primer resultado es la definición.
- [ ] Repro de campo re-medido en el Resultado.

## Riesgos

Descartar contenidos puede esconder el método exacto detrás de la clase: la regla prefiere el
fragmento más chico cuando rankea más alto.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
