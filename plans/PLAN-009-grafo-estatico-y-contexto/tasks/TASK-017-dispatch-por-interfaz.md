# TASK-017 — Dispatch por interfaz en `impact_analysis` (y después en `traverse`)

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-009
- **Estado:** `pending`

---

## Objetivo

Que `impact_analysis` no pierda a quien llama a una implementación a través de su interfaz, su clase
abstracta o su trait: hoy `impact("ServiceImpl.update")` no ve las llamadas a `IService.update`, y
en Java con inyección de dependencias ese es el falso negativo más caro (el llamador tiene el campo
tipado por la interfaz). La semilla se expande a los métodos override-equivalentes de supertipos y
subtipos, con aristas `medium` y `via: "dispatch"`, nunca `high`. `traverse` (TASK-013) usa la misma
expansión. Agregada a PLAN-009 por decisión del usuario (2026-10-08, §12), a propuesta de la revisión
de TASK-009.

## Contexto verificado

- `Store::impact_graph` (`crates/devctx-store/src/impact.rs`, TASK-009) recorre `live_edges` por
  niveles y sigue `calls` e `instantiates` (desviación 3 de TASK-009): no sigue `implements` ni
  `inherits`.
- Las aristas `implements`/`inherits` ya existen en `edges` desde TASK-004 y se resuelven en el link
  pass de TASK-005/006/007 (Java `implements`/`extends`, TS `implements`/`extends`, Rust
  `impl Trait for T`, Python bases, Go embebido).
- La resolución de un método por supertipo ya existe en el link pass (`inherited`), y en Rust el
  trait de un `impl Trait for T` con T externo queda `medium` (TASK-008 N2, TASK-009 MINOR).

## Archivos

- **Modificar:** `crates/devctx-store/src/impact.rs` (expansión de la semilla y de cada nodo por
  dispatch, con tope), `crates/devctx-mcp/src/state/impact.rs` (salida `via: "dispatch"`, conteo),
  docs EN + ES de `impact_analysis`, `PLAN-009-design.md` (DD-11).
- **Después, en TASK-013:** la misma expansión en `traverse`.

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) `impact("ServiceImpl.update")` ve a quien llama por
      `IService.update` (Java, campo tipado por la interfaz e inyectado); (b) `impact("IService.update")`
      ve las implementaciones y sus llamadores; (c) una interfaz con muchas implementaciones no explota:
      tope por semilla, con el resto contado en `omitted`/`excluded`; (d) TS `implements` y Rust
      trait → impls; (e) escenario en `an_incremental_link_pass_equals_a_full_one` si cambia un
      `implements` (la expansión se lee en la consulta, pero el escenario fija que las aristas de
      herencia del incremental igualan al completo).
- [ ] **Paso 2 — override-equivalencia.** Mismo nombre y aridad o firma compatible, en supertipos y
      subtipos vía `implements`/`inherits` resueltos (`dst_id`). Las aristas sumadas son `medium`
      con `via: "dispatch"`, nunca `high`; un `min_confidence: high` las excluye y las cuenta.
- [ ] **Paso 3 — lenguajes.** Java (interfaces y clases abstractas con DI), TS (`implements`), Rust
      (trait → impls). Python y Go como best-effort documentado.
- [ ] **Paso 4 — default.** Incluido por defecto en `impact`, con conteo cuando se recorta. Contrato
      solo aditivo: la salida de TASK-009 sigue siendo un subconjunto.
- [ ] **Paso 5 — medir.** p95 de `impact` ≤ 300 ms en backend-a y backend-b con la expansión
      activa; gold con un sitio real de DI en backend-b (con alias en el repo).

## Criterios de aceptación

- [ ] Tests (a)-(e) verdes, cada uno fallando antes del fix.
- [ ] Ninguna arista de dispatch sale `high`.
- [ ] Gold de DI en backend-b correcto; p95 de `impact` ≤ 300 ms en backend-a.
- [ ] Docs EN + ES y descripción del tool explican `via: "dispatch"` y cómo excluirlo.

## Riesgos

- Interfaces con decenas de implementaciones (repositorios genéricos, `Runnable`): el tope por
  semilla evita la explosión; lo recortado se cuenta.
- Sobrecargas con la misma aridad y tipos distintos: sin firma resuelta, quedan como candidatos
  `medium`, nunca `high`.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
