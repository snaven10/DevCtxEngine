# TASK-002 — Reindex barato: reusar vectores por `content_hash` del chunk

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** — (independiente; conviene antes del primer reindex de desarrollo)
- **Estado:** `pending`

---

## Objetivo

Que un `index --full` provocado por un cambio de extractor (el que este plan exige al subir
`EXTRACTOR_VERSION`) y cualquier incremental solo embeban los chunks cuyo texto cambió. Hoy embeben
todos los chunks del archivo. Implementa DD-20.

## Contexto verificado

- `Indexer::index_file` (`crates/devctx-index/src/pipeline.rs:826-984`): parsea, chunkea
  (`chunk_file`, l.935) y embebe **todos** los chunks (`self.embed`, l.950) antes de la transacción
  que borra las filas viejas del archivo (`delete_by_file`, l.957-958) e inserta las nuevas.
- `Indexer::embed` (`pipeline.rs:988-1025`): `self.embedder.embed(&texts)` sobre todos los textos;
  cada `VectorPoint` lleva `metadata.content_hash = chunk.content_hash` (l.1017).
- `Chunk::content_hash` = sha256[:16] del texto del chunk (`crates/devctx-chunk/src/chunk.rs:24-25`,
  `Chunk::new` l.30-50). La columna `vectors.content_hash` existe (`crates/devctx-store/src/schema.rs:93`).
- En una corrida `--full` no se salta ningún archivo por hash (`pipeline.rs:863-873` solo aplica a
  incrementales) y la copia entre ramas (`branch_with_same_content`, l.885-922) no aplica a la
  misma rama.
- Modelo y dimensión registrados: `index_state.model_name`, `model_dimension`
  (`schema.rs:230-241`).
- Costo real: ~1 h por ~1400 archivos en 8 cores (`AGENTS.md` §6), dominado por el embedding.

## Archivos

- **Modificar:** `crates/devctx-index/src/pipeline.rs` (`index_file`, `embed`),
  `crates/devctx-store/src/store.rs` (lectura `content_hash → vector` de un archivo en una rama;
  helper nuevo, p. ej. `Store::vectors_by_hash(repo, branch, file)`), `IndexResult`
  (`pipeline.rs:~170-200`: contador `chunks_reused`).
- **Tests:** `crates/devctx-index` (unit con un embedder falso que cuenta textos) y, si hace falta,
  uno de integración en `crates/devctx-cli/tests/`.

## Pasos

- [ ] **Paso 1 — test que falla.** Embedder falso que cuenta textos: indexar un archivo, cambiar una
      función, reindexar → hoy cuenta todos los chunks; debe contar solo los cambiados. Mismo test
      con `--full` y fuente idéntica → 0 textos embebidos.
- [ ] **Paso 2 — lectura previa.** Antes de embeber, `vectors_by_hash` del archivo en la rama
      (excluye `is_deletion` y filas de memoria). Una sola consulta por archivo.
- [ ] **Paso 3 — guarda de modelo.** Reusar solo si el modelo y la dimensión activos coinciden con
      `index_state` de la rama; si no hay `index_state` o difiere, embeber todo (como hoy).
- [ ] **Paso 4 — embeber los faltantes** en el mismo orden y armar los `VectorPoint` con el vector
      reusado o el nuevo; ids y metadatos como hoy (el id depende de la línea, no del hash).
- [ ] **Paso 5 — contador.** `IndexResult.chunks_reused` y una línea en el resumen del `index`
      ("N chunks reutilizados sin re-embeber").
- [ ] **Paso 6 — medir.** `index --full` dos veces seguidas en DevCtxEngine con el arnés de
      TASK-001 (o `time`): la segunda debe ser una fracción de la primera.
- [ ] **Paso 7 — HNSW por el camino del serve (bug encontrado en PLAN-010 TASK-001, 2026-10-06).**
      Desde 75b5289 (v0.4.1) el pipeline (`devctx-index/src/pipeline.rs` ~263 y ~583-595) solo
      reconstruye HNSW si ya existía o hay nota `pending_hnsw`; en un índice nuevo nunca lo crea. Lo
      crea únicamente el CLI directo (`devctx-cli/src/main.rs` ~3631-3634, con
      `DEVCTX_NO_AUTOSERVE=1`). Con autoserve —el camino de casi todos los usuarios— la búsqueda va a
      full scan (p95 ~70 ms vs ~43 ms con HNSW, medido). Fix: al final de `pipeline::run`, si
      `storage.hnsw` está activo y no hay índice ni nota, `enable_hnsw` (pasar la config en
      `IndexRequest`). Como el rollout de PLAN-009 exige `index --full`, esto hace que todos los repos
      reindexados queden con HNSW. Test: índice nuevo vía `do_index` (camino del serve) con
      `hnsw: true` → `status.memory.hnsw.present == true`. `AGENTS.md:205-210` documenta hoy la
      limitación: actualizarlo.

## Criterios de aceptación

- [ ] Test: reindex completo sin cambios de fuente → 0 textos embebidos y mismos vectores.
- [ ] Test: cambio de modelo (dimensión distinta o `model_name` distinto) → se embebe todo.
- [ ] Test: editar una función → solo sus chunks (y los que cambian de texto por ella) se embeben.
- [ ] Medido: segundo `index --full` de DevCtxEngine ≤ 20 % del primero (anotar ambos tiempos).
- [ ] Los resultados de `search` antes/después del reindex reusado son idénticos (top-10 del
      arnés de TASK-001 sobre DevCtxEngine).
- [ ] Test: un índice nuevo creado por el camino del serve con `storage.hnsw: true` termina con el
      índice HNSW presente (`status.memory.hnsw.present`), y el test falla en v0.9.0.

## Riesgos

- Un chunk con el mismo texto pero distinto contexto de embedding (si algún día se embebe con
  metadatos además del texto) dejaría de ser equivalente: hoy se embebe solo `chunk.text`
  (`pipeline.rs:992`); dejar un comentario en `embed` que lo fije.
- Leer los vectores viejos de archivos grandes cuesta memoria: es un archivo por vez, acotado.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
