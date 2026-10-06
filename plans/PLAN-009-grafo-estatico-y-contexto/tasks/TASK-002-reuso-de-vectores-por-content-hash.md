# TASK-002 — Reindex barato: reusar vectores por `content_hash` del chunk

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** — (independiente; conviene antes del primer reindex de desarrollo)
- **Estado:** `done`

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

- [x] **Paso 1 — test que falla.** Embedder falso que cuenta textos: indexar un archivo, cambiar una
      función, reindexar → hoy cuenta todos los chunks; debe contar solo los cambiados. Mismo test
      con `--full` y fuente idéntica → 0 textos embebidos.
- [x] **Paso 2 — lectura previa.** Antes de embeber, `vectors_by_hash` del archivo en la rama
      (excluye `is_deletion` y filas de memoria). Una sola consulta por archivo.
- [x] **Paso 3 — guarda de modelo.** Reusar solo si el modelo y la dimensión activos coinciden con
      `index_state` de la rama; si no hay `index_state` o difiere, embeber todo (como hoy).
- [x] **Paso 4 — embeber los faltantes** en el mismo orden y armar los `VectorPoint` con el vector
      reusado o el nuevo; ids y metadatos como hoy (el id depende de la línea, no del hash).
- [x] **Paso 5 — contador.** `IndexResult.chunks_reused` y una línea en el resumen del `index`
      ("N chunks reutilizados sin re-embeber").
- [x] **Paso 6 — medir** (repo chico, build debug). `index --full` dos veces seguidas en DevCtxEngine con el arnés de
      TASK-001 (o `time`): la segunda debe ser una fracción de la primera.
- [x] **Paso 7 — HNSW por el camino del serve (bug encontrado en PLAN-010 TASK-001, 2026-10-06).**
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

- [x] Test: reindex completo sin cambios de fuente → 0 textos embebidos y mismos vectores.
- [x] Test: cambio de modelo (dimensión distinta o `model_name` distinto) → se embebe todo.
- [x] Test: editar una función → solo sus chunks (y los que cambian de texto por ella) se embeben.
- [ ] Medido: segundo `index --full` de DevCtxEngine ≤ 20 % del primero (anotar ambos tiempos). **Parcial:** build debug en repo chico, 34 % (ver Resultado); release no medido.
- [x] Los resultados de `search` antes/después del reindex reusado son idénticos: probado a nivel de vectores (mismos vectores por hash, test `a_full_reindex_of_unchanged_source_embeds_nothing`); el top-10 del arnés de TASK-001 no se corrió.
- [x] Test: un índice nuevo creado por el camino del serve con `storage.hnsw: true` termina con el
      índice HNSW presente (`status.memory.hnsw.present`), y el test falla en v0.9.0.

## Riesgos

- Un chunk con el mismo texto pero distinto contexto de embedding (si algún día se embebe con
  metadatos además del texto) dejaría de ser equivalente: hoy se embebe solo `chunk.text`
  (`pipeline.rs:992`); dejar un comentario en `embed` que lo fije.
- Leer los vectores viejos de archivos grandes cuesta memoria: es un archivo por vez, acotado.

## Resultado

- **Estado final:** `done` (con una medición parcial: ver Desviaciones).
- **Resumen:** `Indexer::embed` lee los vectores del archivo en la rama (`Store::vectors_by_hash`,
  una consulta por archivo, sin tombstones ni vectores de otra dimensión) y solo embebe los chunks
  cuyo `content_hash` no está, en el mismo orden; reúsa solo si el `index_state` de la rama existe y
  no hay cambio de modelo/dimensión (`reuse_vectors = prev.is_some() && !model_changed`).
  `IndexResult.chunks_reused` sale en el JSON de `index` y en el resumen del CLI. Paso 7: nuevo
  `IndexRequest.hnsw: Option<&str>` (métrica si `storage.hnsw`); al final de `pipeline::run`, si no
  hay índice ni nota `pending_hnsw`, se hace `enable_hnsw` (el CLI directo y `do_index` lo pasan).
- **Números (build debug, repo de 19 archivos / 539 chunks, minilm-l6, HOME aislado):**
  `index --full` desde cero 155,5 s; segundo `--full` sin cambios 52,6 s con 539/539 chunks
  reutilizados (0 embebidos). El embedding (~103 s) desapareció; los ~52 s restantes son costo fijo
  (carga del modelo, parse en debug, reconstrucción de HNSW). Ratio 34 %: **no** cumple ≤ 20 % en
  debug. Con el binario release no medido (el release instalado en `target/release` es anterior al
  cambio y recompilarlo cuesta ~25 min).
- **Hallazgos confirmados:** H-HNSW (PLAN-010 TASK-001) confirmado: el camino del serve nunca creaba
  HNSW. La premisa de DD-20 (embedding domina el costo) se confirma: 2/3 del tiempo era embedding.
- **Archivos tocados:** `crates/devctx-index/src/pipeline.rs` (`IndexRequest.hnsw`,
  `IndexResult.chunks_reused`, `Ctx.reuse_vectors`, `embed -> (Vec<VectorPoint>, usize)`, rebuild
  de HNSW), `crates/devctx-store/src/store.rs` (`Store::vectors_by_hash(repo, branch, file) ->
  Result<HashMap<String, Vec<f32>>>`), `crates/devctx-mcp/src/state.rs` (pasa `hnsw`, campo JSON
  `chunks_reused`, test), `crates/devctx-cli/src/main.rs` (pasa `hnsw`, línea de resumen),
  `crates/devctx-index/src/lib.rs` (tests), `AGENTS.md` (§6). Sin claves nuevas de config.
- **Verificado por:** tests nuevos en `devctx-index`: `a_full_reindex_of_unchanged_source_embeds_nothing`,
  `editing_one_function_embeds_only_its_chunks`, `a_different_model_embeds_everything_again`,
  `a_new_index_gets_hnsw_when_the_config_asks_for_it`; en `devctx-mcp`:
  `do_index_leaves_an_hnsw_index_when_the_config_wants_one` (camino `do_index`). Los de reuso y los
  dos de HNSW fallan sin el cambio (comprobado desactivando `reuse_vectors` y `hnsw`); el de modelo
  distinto es guarda (pasa también en el padre). Gate: `cargo fmt --check`, `clippy --workspace
  --all-targets -D warnings` (también `--features gpu`) y `cargo test` por paquete, todo verde;
  `connection_refused_recognises_a_real_ureq_error` falló una vez en la corrida de `devctx-cli`
  (reset de conexión en un puerto efímero) y pasó solo y en la rerun completa.
- **Desviaciones:** (1) la medición es debug/repo chico, no DevCtxEngine release: 34 % vs el ≤ 20 %
  pedido. (2) El top-10 del arnés de TASK-001 no se corrió; la equivalencia se probó comparando los
  vectores almacenados antes/después. (3) El test de HNSW se omite sin la extensión VSS (offline).
- **Riesgos abiertos / siguiente:** `embed` sigue cargando el modelo aunque no haga falta (costo fijo
  visible: "Loading embedder" en un reindex 100 % reusado); cargarlo perezoso lo recortaría. Medir en
  release con TASK-016. Los índices ya creados por el serve sin HNSW lo reciben en la siguiente
  corrida de `index`.

### Fixup (post-review de 7d92cd8)

- **Embedder perezoso.** Nuevo `devctx_embed::LazyEmbedder` (dimensión y nombre salen de la
  config/registro; el modelo se construye en el primer `embed`). Lo usan `cmd_index` (CLI directo)
  y `do_index_inner` (serve: carga vía la caché con release por inactividad, fase `loading model`
  solo cuando de verdad carga). Un reindex 100 % reusado ya no carga el modelo: sin el transitorio
  de ~700 MiB (nota para PLAN-010).
- **Sin rebuild innecesario.** HNSW y BM25 ya no se tiran al empezar la corrida sino justo antes de
  la primera escritura (`Derived::take_down`, con la nota `pending_*` antes de cada drop). Un `--full`
  que no cambia nada no escribe: el archivo con mismo hash, mismo extractor, todos los chunks
  reusados y mismos conteos no se reescribe (el `commit` de las filas queda el de la corrida que las
  escribió, como en cualquier archivo que el incremental salta). Se reconstruye si se tiró, si hay
  nota pendiente, o si no había índice y la config lo pide (Paso 7); si la métrica configurada
  difiere de la existente también. Se quitó el pre-drop del CLI y su creación duplicada de HNSW
  (ahora solo informa). Un incremental sin cambios tampoco reconstruye (antes sí). Caso que sigue
  reescribiendo: archivos que otra rama ya tiene idénticos (camino de copia entre ramas).
- **I1 (review): fingerprint de embedding.** `devctx_embed::embedding_fingerprint` (provider |
  modelo | dim | engine `fastembed-4/ort-2.0.0-rc.9` | `max_chars` | l2) por rama en `index_meta`
  (`embedding_fingerprint`). Al empezar una corrida con fingerprint distinto al guardado se escribe
  `transition` antes de tocar archivos y se fuerza full sin reuso; se estampa al completar. Reuso solo
  si guardado == activo; un índice sin fingerprint (anterior) se confía pero no se reusa hasta que una
  corrida lo estampe. El engine `ort` de PLAN-010 debe cambiar `LOCAL_ENGINE` hasta probar
  bit-exactitud. `EXTRACTOR_VERSION` intacto.
- **M1:** la corrida avisa ("this database has no HNSW index…") y AGENTS.md lo dice: un repo sin HNSW
  paga la construcción completa en la primera corrida con `storage.hnsw`. **M2:** `load_vss` recuerda
  el fallo en el proceso (flag en `Shared`), no reintenta `INSTALL` por red en cada corrida.
- **Medición RELEASE** (`cargo build --release -p devctx-cli`, `index --full` directo,
  `DEVCTX_NO_AUTOSERVE=1`, HOME/DEVCTX_HOME aislados, minilm-l6, memprobe.sh; baseline = 7d92cd8
  compilado aparte):

  | repo (archivos / chunks) | binario | desde cero | 2.º `--full` | ratio | VmHWM 2.º | chunks reusados / embebidos |
  |---|---|---|---|---|---|---|
  | DevCtxEngine (279 / 4859) | 7d92cd8 | 306 s | 20.0 s | 6,5 % | 388 MiB | 4859 / 0 |
  | DevCtxEngine | fixup | 240 s | 4,4 s | **1,8 %** | **115 MiB** | 4859 / 0 |
  | backend-b (216 / 3253), clon de solo lectura | 7d92cd8 | 141 s | 21,0 s | 14,9 % | 387 MiB | 3253 / 0 |
  | backend-b | fixup | 169 s | 2,4 s | **1,4 %** | **103 MiB** | 3253 / 0 |

  VmHWM de la corrida desde cero: 447-472 MiB (ambos binarios). Los tiempos desde cero varían entre
  corridas (otra carga en la máquina); el ratio usa el desde cero de la misma fila. Criterio ≤ 20 %:
  **cumplido** en ambos repos, en release. En 7d92cd8 ya se cumplía en release; el debug (34 %) lo
  dominaban parse y carga del modelo en debug.
- **Pendiente:** equivalencia del top-10 con el arnés de TASK-001 — PENDIENTE hasta que TASK-001
  cierre (hoy solo está probada la igualdad de vectores almacenados antes/después).
- **Tests nuevos (devctx-index):** `a_no_change_full_run_never_loads_the_embedder`,
  `a_no_change_full_run_does_not_rebuild_the_derived_indexes`,
  `owed_or_missing_indexes_are_built_even_when_nothing_changed`,
  `an_interrupted_run_under_another_setup_poisons_no_reuse`,
  `an_index_without_a_fingerprint_is_stamped_not_reused`; `devctx-embed`:
  `the_fingerprint_changes_with_anything_that_changes_the_vectors`. Los de rebuild y fingerprint
  fallan con el comportamiento anterior (en el padre ni compilan: no existen `LazyEmbedder` ni
  `IndexRequest.embed_fingerprint`; la lógica que prueban es la que cambió).
