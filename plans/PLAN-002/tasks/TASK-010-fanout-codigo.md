# TASK-010 — Fan-out de código: buscar en todos los miembros del grupo

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-002
- **Estado:** `pending`

---

## Objetivo

Que estando vinculado a un grupo, `search` busque en **todos** los miembros y devuelva un único
ranking. Un binding de grupo significa el producto; contestar desde un solo repo contesta otra cosa.

## Contexto verificado

**Restricción dura**: DuckDB admite un solo proceso escritor por archivo, y los `devctx serve` de
cada proyecto son dueños de sus stores. Por eso `do_search_project` (`state.rs`) **no abre** el store
ajeno: lanza el propio binario con `current_dir(path)` y `--format json`. El fan-out es N de esos.

El registry ya guarda `embed_dim` por proyecto, documentado como *"compared before any cross-project
vector work"* — la comparación que esta task por fin usa. Los 11 repos REVFA son `ml-granite`/384d.

El modo `hybrid` de `search` ya fusiona por **RRF**; hay precedente de la técnica en el repo.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (nueva `do_search_group`)
- **Modificar:** `crates/devctx-mcp/src/lib.rs` (la tool `search` despacha según el binding)

## Pasos

- [ ] **Paso 1 — `do_search_group(members, query, limit, language, mode)`.** Correr la búsqueda por
      miembro reusando el mecanismo de `do_search_project`.
- [ ] **Paso 2 — En paralelo, con tope.** Los miembros son independientes; 11 subprocesos secuenciales
      es latencia gratis. Acotar la concurrencia (~4) para no saturar en workspaces grandes.
- [ ] **Paso 3 — Chequear `embed_dim` ANTES de fusionar.** Los miembros que no coincidan con la
      dimensión mayoritaria se excluyen y se reportan en `skipped_projects` con el motivo. Fusionar
      vectores de dimensiones distintas produce un ranking sin sentido que se ve igual de bien.
- [ ] **Paso 4 — Fusionar por RRF**, no por score crudo: los scores de dos stores no son
      directamente comparables aunque el modelo sea el mismo. `1/(k + rank)` con k=60.
- [ ] **Paso 5 — Cada hit lleva su `project`.** Sin eso el resultado es inutilizable: dos archivos
      con la misma ruta relativa en repos distintos son indistinguibles.
- [ ] **Paso 6 — Un miembro que falla no tumba la búsqueda.** Se reporta en `failed_projects` y el
      resto contesta. Degradación graciosa, como ADR-16.
- [ ] **Paso 7 — Despacho en la tool**: hint → ese proyecto; grupo sin hint → fan-out; proyecto → como hoy.

## Criterios de aceptación

- [ ] En modo grupo, `search` sin `project` devuelve hits de más de un repo.
- [ ] Cada hit dice de qué proyecto vino.
- [ ] Con `project`, busca SOLO ahí (el hint sigue mandando).
- [ ] Un miembro con `embed_dim` distinto se excluye y aparece en `skipped_projects`.
- [ ] Un miembro caído aparece en `failed_projects` y los demás igual contestan.
- [ ] En modo proyecto el comportamiento no cambia (sin regresión).

## Riesgos

- **Latencia**: 11 búsquedas contra 1. El paralelismo del Paso 2 lo acota, pero el fan-out es
  inherentemente más caro. Si molesta, la salida ya trae de dónde vino cada hit y el usuario puede
  acotar con `project`.
- **Presupuesto de contexto**: 11 repos × `limit` hits es mucho texto. El `limit` es del resultado
  FUSIONADO, no por miembro.

## Resultado

<!-- SE LLENA AL CERRAR -->
