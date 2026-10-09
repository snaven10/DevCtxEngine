# TASK-010 — PageRank global al indexar y centralidad como señal en `search` híbrido

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-006, TASK-007, TASK-008
- **Estado:** `pending`

---

## Objetivo

Que cada símbolo tenga un PageRank global y un in-degree calculados al final del link pass, y que
`search` en modo híbrido (y por lo tanto `build_context`) use la centralidad como **señal adicional**
—una tercera lista ponderada de la RRF— junto a las penalizaciones y el anclaje, sin tocar el modo
vectorial. Implementa DD-12 (parte global) y DD-13.

**Compuerta:** no arranca hasta que las filas de "grafo limpio" de §6 del master se cumplan con
TASK-005…008 (basura 0, externos/tests marcados, precisión de gold edges ≥ meta).

## Contexto verificado

- `search_ranked` (`crates/devctx-search/src/lib.rs:269-357`): `Hybrid` = RRF de vector + keyword
  (`reciprocal_rank_fusion(&[vector, keyword], RRF_K)`, l.301-309) → filtros duros → `apply_penalty`
  (por posición, l.175-197; `demoted_order` l.219-239) → `dedup_hits` (l.751-779) → rerank → si no es
  `Vector`, `anchor_identifiers` (l.351-356).
- RRF genérica: `devctx_core::rank::fuse_by_rank`, `RRF_K = 60` (`crates/devctx-core/src/rank.rs`).
- `SearchResult.raw_score` (`crates/devctx-core/src/types.rs:63-79`): lo usa la selección de miembro
  de grupo (`member_scores`, `crates/devctx-mcp/src/state.rs:4557-4572`) y no debe incluir la
  centralidad.
- `build_context` usa `SearchMode::Hybrid` con 30 hits (`state.rs:4137-4146`).
- Tests de penalización y anclaje en `crates/devctx-search/src/lib.rs` (p. ej.
  `anchoring_is_bounded_in_tokens_and_in_share_of_the_answer`, l.1617).

## Archivos

- **Crear:** `crates/devctx-index/src/pagerank.rs` (iteración de potencia propia, DD-12; sin
  dependencias nuevas).
- **Modificar:** `crates/devctx-index/src/link.rs` (calcular y escribir `symbols.rank`/`in_degree`
  al final del link pass), `crates/devctx-store/src/symbols.rs` (lectura de `rank` para un lote de
  chunks: símbolo más interno por rango, DD-4), `crates/devctx-search/src/lib.rs` (lista de
  centralidad en `Hybrid`), `crates/devctx-core/src/config.rs` (`search.centrality_weight`, default
  0.3; 0 apaga).

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) PageRank en un grafo de 6 nodos con ranking conocido;
      determinismo entre corridas; nodos colgantes; (b) pesos: una arista `from_test` pesa 0.1 y una
      `low` 0.2; `sqrt(n)` de multiplicidad; (c) `Hybrid` con dos candidatos empatados en RRF → gana
      el de mayor `rank`; (d) `Vector` devuelve exactamente lo mismo con y sin `rank`; (e) un hit
      anclado sigue primero aunque su `rank` sea 0; (f) `raw_score` no cambia con la centralidad.
- [ ] **Paso 2 — PageRank global** en el link pass: damping 0.85, tol 1e-6, máx. 50 iteraciones;
      pesos por kind/confianza/multiplicidad/test de DD-12; `in_degree` = llamadas entrantes no-test
      `≥ medium`.
- [ ] **Paso 3 — señal en la RRF.** Ordenar el pool de candidatos por `rank` del símbolo más interno
      de cada hit y sumar `w / (RRF_K + pos + 1)`; solo candidatos ya traídos por algún retriever.
- [ ] **Paso 4 — calibrar `w`** con el arnés de TASK-001 (0, 0.15, 0.3, 0.5): elegir el que maximiza
      la relevancia de `build_context` sin bajar Hit@5/MRR híbrido más de 2 % relativo. Si ningún
      valor mejora, default 0 y se documenta (la infraestructura queda para repo_map/ego-graph).
- [ ] **Paso 5 — medir** costo del PageRank (ms) en backend-a y latencia de `search --hybrid`.

## Criterios de aceptación

- [ ] Tests (a)-(f) verdes; tests de penalización/anclaje/dedup existentes verdes sin modificar.
- [ ] Tabla del arnés con los cuatro valores de `w` en el Resultado y el default elegido con su
      razón.
- [ ] PageRank global < 1 s en backend-a; `search --hybrid` p50 ≤ +20 ms.

## Riesgos

- Centralidad que favorece utilidades genéricas (`StringUtils`, mappers): por eso `sqrt(n)`, tests
  ×0.1, y la calibración puede dejar `w = 0`.
- Un hit cuyo chunk no está dentro de ningún símbolo (chunk de archivo, docs) no tiene `rank`: va al
  final de la lista de centralidad, sin penalidad extra.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
