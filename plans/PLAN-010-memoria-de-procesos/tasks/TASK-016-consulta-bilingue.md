# TASK-016 — Expansión bilingüe de la consulta: `queries[]` con fusión, elegida con medición

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Release:** `0.10.x`
- **Depende de:** TASK-014
- **Estado:** `pending`

---

## Objetivo

Aprovechar que el cliente del MCP es un LLM que sabe traducir: que una búsqueda en español pueda ir
acompañada de su versión en inglés (la lengua de los identificadores y de la mayoría de los
comentarios) y que devctx fusione los resultados. Sin modelo nuevo, sin reindex, sin traducir del
lado del servidor. Elegir **el método** con números entre las variantes de abajo. PLAN-010 DD-14.

## Contexto verificado

- `SearchReq` (`crates/devctx-mcp/src/lib.rs:35-70`) recibe un solo `query: String`; modos `vector`
  (default), `keyword`, `hybrid`. La fusión ya existe: `reciprocal_rank_fusion(lists, k)`
  (`crates/devctx-search/src/lib.rs:1005`, `RRF_K = 60`, `:18`), usada por el híbrido (`:368`) y por
  la búsqueda de grupo (`crates/devctx-mcp/src/state.rs:2816-2994`).
- El embedder ya acepta lotes (`EmbeddingProvider::embed(&[String])`): dos consultas cuestan un
  forward de lote 2, no dos cargas.
- `recall` (memorias) tiene el mismo problema y la misma solución.
- Línea base de la brecha: ES vector Hit@5 8/13 vs EN 14/17 (PLAN-009 TASK-001); se re-mide sobre el
  set de TASK-014.

## Variantes a medir

| id | Método | Cambio en el servidor | Costo por búsqueda |
|---|---|---|---|
| Q0 | Solo la consulta original (hoy) | — | 1 embedding + 1 búsqueda |
| Q1 | El cliente hace dos llamadas (ES y EN) y fusiona él | ninguno (solo la descripción de la tool) | 2 llamadas MCP, ~2× tokens de respuesta |
| Q2 | `queries: [es, en]` → una búsqueda por consulta, fusión RRF | parámetro nuevo | 1 embedding de lote 2 + 2 búsquedas |
| Q3 | `queries` → promedio de los vectores normalizados, una sola búsqueda | parámetro nuevo | 1 embedding de lote 2 + 1 búsqueda |
| Q4 | Q2 con la consulta original pesando más (RRF ponderado, p. ej. 1.0 / 0.7) | parámetro nuevo | igual que Q2 |

Q1 se simula fuera de línea con el arnés (dos corridas y fusión en `score.py`) para no pagarla en
código. Si Q1 ≈ Q2, Q2 gana igual por tokens y latencia del cliente.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs` (`SearchReq.queries: Option<Vec<String>>`, idem en
  `recall` y en el `search` de grupo; descripción de la tool que pide mandar la traducción),
  `crates/devctx-search/src/lib.rs` (búsqueda multi-consulta + fusión), CLI `devctx search --also
  "<texto>"` (para el arnés), `scripts/graph-eval/` (campo opcional `consulta_en` en los casos;
  `score.py` con Q1 simulada).

## Pasos

- [ ] **Paso 1 — traducciones del set, antes de medir.** Para cada caso ES de TASK-014, la consulta en
      inglés escrita a mano en un campo nuevo (`consulta_en`), y para cada caso EN su versión ES (para
      ver si la expansión también ayuda o daña al inglés). Se fijan antes de correr nada. Variante de
      robustez: un segundo juego generado por un LLM con un prompt fijo (versionado), para no medir
      solo traducciones "perfectas".
- [ ] **Paso 2 — Q1 fuera de línea.** Con el arnés actual: corrida ES + corrida EN + fusión RRF en
      `score.py`. Da el techo esperado sin tocar código.
- [ ] **Paso 3 — implementar Q2/Q3/Q4 detrás del mismo parámetro** (`queries`, máximo 4, deduplicadas;
      `query` sigue siendo obligatorio y es la primera). Modo híbrido: BM25 por consulta también
      (aprovecha TASK-015 si ya está). Rerank (si está activo): **una** pasada del cross-encoder con
      la consulta original sobre el pool fusionado, no una por consulta.
- [ ] **Paso 4 — medir** Q0…Q4 × {vector, híbrido} × {traducción a mano, traducción LLM}, 3 corridas,
      mediana; latencia p50/p95 del serve caliente (E5 de TASK-001).
- [ ] **Paso 5 — elegir y documentar.** Dejar solo el método ganador (sin perilla si no hace falta)
      y la descripción de la tool que le dice al agente cuándo mandar `queries` (consulta en otro
      idioma que el código, o términos de negocio). `recall` igual, medido con el recall de TASK-014.

## Criterios de aceptación

- [ ] El método elegido cumple la **regla por task** de PLAN-010 DD-12 (mejora puntual en ES, ninguna regresión con IC) contra Q0, recall incluido.
- [ ] Latencia p95 de `search` con 2 consultas ≤ 1.5× la de una (sin rerank); con una sola consulta,
      sin cambio (≤ +10 %, §6).
- [ ] Compatibilidad: un cliente que no manda `queries` obtiene exactamente lo de hoy (test).
- [ ] Tests: dedupe, tope de 4, orden estable de la fusión, rerank una sola vez sobre el pool fusionado.
- [ ] Si ninguna variante cumple la regla por task con traducción LLM, se cierra `skipped` con la tabla (el
      resultado con traducción a mano solo no alcanza para cambiar la tool).

## Riesgos

- **El agente no manda la traducción.** La descripción de la tool es la única palanca; medir en el
  campo (TASK-011) cuántas llamadas reales traen `queries`.
- **La traducción de un término de negocio no existe en el código** (nombres de dominio en español en
  los identificadores de los repos privados). Por eso Q2 fusiona en vez de reemplazar, y Q4 da más
  peso a la original.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
