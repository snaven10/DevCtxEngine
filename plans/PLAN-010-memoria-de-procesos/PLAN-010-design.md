# PLAN-010 — Diseño técnico: memoria de procesos

**Fecha:** 2026-10-05
**Master:** [`PLAN-010-memoria-de-procesos.md`](./PLAN-010-memoria-de-procesos.md)
**Estado:** propuesta, pendiente de aprobación junto con el master.

Referencias `archivo:línea` contra `main` = v0.8.4 salvo que digan otra cosa. Las de fastembed/ort
son de `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/{fastembed-4.9.1,ort-2.0.0-rc.9}`.

---

## §1. Modelo de memoria de un proceso devctx (lo que se cree hoy)

Un serve con modelo cargado tiene, a grandes rasgos:

| Componente | Origen | Tipo de página | Liberable |
|---|---|---|---|
| Binario + libs (incl. `libonnxruntime` estática) | ELF | `RssFile` compartido | no |
| Pesos del modelo dentro de ORT | `commit_from_memory` / `commit_from_file` | `RssAnon` privado | al soltar el embedder (300 s) |
| Pesos re-empaquetados (prepacking MLAS) | ORT al crear la sesión | `RssAnon` privado | idem |
| Arena de CPU de ORT (activaciones) | `session.run` | `RssAnon`; la arena **no devuelve** al SO hasta destruir la sesión | idem + `malloc_trim` |
| Tokenizer (`tokenizer.json` 25 MB parseado) | `tokenizers` | `RssAnon` | idem |
| Buffer pool de DuckDB | consultas | `RssAnon`, acotado por `memory_limit` (2 GB hoy) | parcialmente (DuckDB libera bajo presión) |
| Índice HNSW (VSS / usearch) | `LOAD vss` + primer uso | `RssAnon`, **fuera** del buffer manager (según doc de VSS; a verificar) | no, mientras el DB esté abierto |
| Heap de Rust (resultados, caches) | — | `RssAnon` | `malloc_trim` |

**Hipótesis a verificar en TASK-001** (ninguna está medida):

- **HM-1**: de los ~500 MB de un serve en reposo con modelo, ≥ 250 MB son modelo + ORT (pesos +
  prepacking + arena retenida) y el resto DuckDB + HNSW.
- **HM-2**: el HNSW de DevCtxEngine (~N vectores × 384 × 4 B + grafo) cuesta decenas de MB, y el de
  REVFA_FrontEnd cientos. Se cuenta con `SELECT count(*) FROM vectors` y se compara RSS antes/después
  del primer `search` vectorial tras abrir el DB.
- **HM-3**: con el modelo liberado, el serve vuelve a < 60 MB (los 12-43 MB medidos + DuckDB/HNSW).
  Si no vuelve, la arena de ORT o el heap fragmentado retienen memoria aunque se suelte la sesión.
- **HM-4**: tras 0.8.5, el pico de indexación (`VmHWM`) lo dominan las activaciones de **un** lote
  × `intra_threads=20` scratch, no la suma de lotes.

## DD-1 — Medir primero, con instrumentación permanente

**Decisión.** TASK-001 agrega un bloque `memory` a la respuesta de `status` (serve: `GET /status` y
la tool `index_status`; central: `GET /health` o un `GET /status` nuevo) y logs de carga/liberación
de modelos con RSS antes/después. Es código del producto, no un script de un día: los usuarios
reportan "devctx come mucha RAM" y hoy no hay cómo contestar sin `/proc`.

Contrato propuesto (aditivo):

```json
"memory": {
  "process": { "rss_bytes": 0, "rss_anon_bytes": 0, "rss_file_bytes": 0,
               "rss_shmem_bytes": 0, "hwm_bytes": 0, "source": "proc|unavailable" },
  "models": { "embedder": { "loaded": true, "key": "ml-granite", "loaded_at": "...",
                            "idle_secs": 12, "engine": "fastembed|ort" },
              "reranker": { "loaded": false } },
  "duckdb": { "memory_limit": "2.0 GiB", "threads": 4, "memory_usage_bytes": 0,
              "temp_files_bytes": 0, "database_size": "..." },
  "hnsw": { "present": true, "metric": "cosine", "vectors": 0, "estimated_bytes": 0 }
}
```

- `process` lee `/proc/self/status` (`VmRSS`, `RssAnon`, `RssFile`, `RssShmem`, `VmHWM`) en Linux;
  fuera de Linux, `source: "unavailable"` y ceros/`null` (no se inventa).
- `duckdb` sale de `duckdb_settings()` (`memory_limit`, `threads`) y `duckdb_memory()` /
  `pragma database_size`; si la función no existe en la versión de DuckDB vendorizada, se reporta
  `null` con el motivo.
- `hnsw.estimated_bytes` = `vectors × dim × 4 × factor`, con el factor documentado como estimación;
  el número real lo da la medición de RSS de TASK-001, no este campo.
- El central reporta lo mismo menos `hnsw` si no lo usa.

**Metodología de medición** (la usan todas las tasks): script `scripts/memprobe.sh` que, dado un PID,
muestrea `/proc/<pid>/status` + `/proc/<pid>/smaps_rollup` (`Pss`, `Shared_Clean`, `Private_Dirty`)
cada 200 ms durante un comando y reporta máximo y reposo. Escenarios fijos:
(E1) central en reposo tras idle; (E2) serve DevCtxEngine `index --full`; (E3) serve REVFA_FrontEnd
indexación incremental; (E4) 3 serves con modelo cargado simultáneamente; (E5) 20 `search`
consecutivos para p50/p95; (E6) serve recién abierto antes/después del primer `search` vectorial
(costo HNSW).

**Alternativa descartada:** medir sólo con `/proc` desde afuera. Funciona una vez y no deja nada al
usuario; además no separa modelo de DuckDB.

## DD-2 — Reemplazar fastembed por `ort` + `tokenizers` propios

**Decisión.** Un módulo nuevo `devctx-embed::onnx` (detrás de la feature `local`) con:
- `OnnxModel` — sesión ORT + metadatos (inputs que pide, outputs, pooling, normalización).
- `TokenizerSpec` — `tokenizers::Tokenizer` configurado exactamente como
  `fastembed::common::load_tokenizer` (`fastembed-4.9.1/src/common.rs:52-133`).
- `embed_batch(texts) -> Vec<Vec<f32>>` en **un hilo llamador**, lotes en serie (lo que hizo
  0.8.5 en la capa de arriba ahora es propiedad del motor).
`devctx-rerank` usa el mismo módulo (exportado desde `devctx-embed` o un crate chico
`devctx-onnx` si las features lo exigen — se decide en TASK-004 según el grafo de dependencias).

**Por qué no las alternativas:**

| Alternativa | Por qué no |
|---|---|
| Seguir con fastembed 4.9 y parchear desde afuera | `par_chunks`, `intra_threads` y `commit_from_memory` están dentro de `try_new*`/`transform`; no hay hooks |
| Fork de fastembed | Mantener un fork de un crate que usamos al 10 % (dos pooling, un tokenizer, sesión) cuesta más que las ~300 líneas propias |
| Subir a fastembed 5.x | Arrastra `ort` rc.10 + ONNX Runtime más nuevo (cambia numéricos → reindex, contra DD-3) y sigue sin exponer mmap/arena |
| Candle u otro runtime | Reescribe la inferencia, pierde CUDA EP y la compatibilidad numérica |

**Lo que el motor debe replicar de fastembed** (fuentes en TASK-004):
1. Tokenizer: `PaddingStrategy::BatchLongest`, `pad_id` de `config.json["pad_token_id"]` (default 0),
   `pad_token` de `tokenizer_config.json`, `TruncationParams { max_length: min(max_length,
   model_max_length) }`, y los `special_tokens_map.json` agregados como `AddedToken` especiales.
   `encode_batch(inputs, add_special_tokens=true)`.
2. Entradas: `input_ids`, `attention_mask` como `i64`; `token_type_ids` sólo si la sesión lo declara
   (`impl.rs:128-131`).
3. Salida: precedencia `OnlyOne` → `last_hidden_state` → `sentence_embedding`
   (`text_embedding/output.rs:13-19`).
4. Pooling: `Mean` con máscara (`pooling.rs:34-76`, divisor con piso 1.0) o `Cls` (`pooling.rs:18-27`),
   según `get_default_pooling_method` para builtins (`impl.rs:~150-195`) y `Pooling::Mean` para
   granite (`local.rs:227`).
5. Normalización: `v / (‖v‖ + 1e-12)` (`common.rs:135-140`) **y después** el `l2_normalize` propio
   (`local.rs:84-86`) — se conservan los dos pasos en el mismo orden para no mover bits.
6. Cuantización: ningún builtin del registro usa `QuantizationMode::Dynamic` (`impl.rs:210-224` vs
   `crates/devctx-embed/src/registry.rs:24-83`), así que el lote no altera los vectores por esa vía.
7. Sesión: `GraphOptimizationLevel::Level3` (igual que hoy, `impl.rs:79,108`).
8. Builtins: descarga por `hf-hub` con la **misma caché** (`with_cache_dir(model_cache_dir())`,
   `local.rs:172-174`) y el mismo `model_code`/`model_file` que el catálogo de fastembed
   (`fastembed-4.9.1/src/models/text_embedding.rs`), envuelta en `devctx_core::modelload::guard_load`.

## DD-3 — Fidelidad numérica y "sin reindex"

**Decisión.**
- `ort` queda en `=2.0.0-rc.9` (ONNX Runtime 1.20.0, `ort-sys-2.0.0-rc.9/build.rs:7`), la misma que
  arrastra fastembed hoy. Actualizar ORT es otro plan.
- Criterio: por vector, `cos(nuevo, dorado) ≥ 0.9999`; por consulta, top-10 de `search` idéntico
  sobre un DB de fixtures. Para el reranker: `|Δscore| ≤ 1e-3` y mismo orden.
- Los dorados se generan en TASK-003 **con el código actual** (fastembed) y se versionan en el repo
  como fixtures binarios chicos (f32 LE + manifiesto JSON con modelo, versión, textos, batch).
- **Batch y padding**: con `BatchLongest`, el vector de un texto puede variar en el último ULP según
  con qué otros textos comparta lote (ModernBERT con padding + máscara). El arnés compara también
  "mismo texto en lotes distintos" para saber cuánto ruido es inherente y no culparle al motor.
- Se registra el motor en `index_meta` (`embed_engine = ort-direct/1`, tabla de PLAN-008 TASK-007)
  sólo como diagnóstico: **no** dispara aviso de reindex.

**Si no se alcanza la tolerancia:** TASK-004 queda `blocked` con el diff medido; se evalúa con el
usuario si la diferencia es aceptable (re-medir calidad de búsqueda) o se abandona DD-2.

## DD-4 — `max_length`

**Decisión.** Nueva clave `embeddings.max_length` (default **512**, el de hoy:
`fastembed-4.9.1/src/text_embedding/mod.rs:8`, recortado por `model_max_length` y por
`LocalModelSpec::max_input_tokens`, `registry.rs:20`). No se cambia el default en este plan (Q-2).

**Tradeoff de bajar a 256:** la atención es O(n²): pasar de 512 a 256 divide por ~4 la memoria de
activaciones por texto. Pero **cambia el vector de todo chunk que tenga > 256 tokens** (se trunca
distinto), así que los índices existentes quedan mezclados: chunks viejos a 512 y nuevos a 256 en el
mismo espacio. Por eso: el valor efectivo se guarda en `index_meta` (`embed_max_length`) y, si la
config no coincide con lo indexado, `status` avisa "reindex recomendado" (mismo mecanismo que
`extractor_stale`). Además `DEVCTX_EMBED_MAX_CHARS` (4096, `local.rs:23`) ya recorta antes del
tokenizer; se documenta la relación.

## DD-5 — Pesos compartidos entre procesos

**Objetivo.** Que N procesos con el mismo modelo compartan las páginas de pesos vía page cache
(`RssFile`/`Shared_Clean`) en vez de N copias en `RssAnon`.

**Opciones (el spike de TASK-006 elige con números):**

| Opción | Mecanismo | Pros | Contras |
|---|---|---|---|
| A. `commit_from_file(.onnx)` | ORT lee el protobuf | trivial | ORT copia los initializers → sigue siendo anónimo; sólo evita la copia extra de Rust |
| B. **`.ort` + mmap + `commit_from_memory_directly`** | `memmap2::Mmap` del `.ort`; config `session.use_ort_model_bytes_directly` y `…_for_initializers` (`impl_commit.rs:137-140`) | initializers apuntan al mmap → compartidos | hay que generar el `.ort`; `InMemorySession<'a>` exige que el mmap viva más que la sesión |
| C. ONNX con datos externos + mmap | `with_external_initializer_file` (`impl_options.rs:163`) | no requiere formato ORT | ORT 1.20 igual copia a tensores propios en CPU en la mayoría de los casos; ganancia dudosa |

**Decisión tentativa: B**, con:
- Generación: primera carga desde `.onnx` con `with_optimized_model_path(<cache>/<key>-<hash>-ort1.20-L{n}.ort)`
  y `session.save_model_format = ORT`; cargas siguientes, mmap del `.ort`. Clave por hash del
  `.onnx` + versión de ORT + nivel de optimización; si no coincide, se regenera.
- Nivel de optimización del `.ort`: probar `Level3` vs `Level2` (extended). `Level3` puede incluir
  fusiones dependientes del hardware: el `.ort` es **por máquina**, nunca se distribuye.
- Propiedad: un struct que posee `Arc<Mmap>` y la sesión con el lifetime borrado (self-referential
  via `self_cell`/`ouroboros` o `unsafe` acotado y documentado). El drop suelta primero la sesión.
- **Prepacking**: MLAS re-empaqueta los MatMul int8 en memoria privada. El spike mide B con
  `with_prepacking(true)` (default) y `false`: si el prepacking se come la ganancia, se evalúa el
  costo de latencia de apagarlo. Si ninguna variante baja `RssAnon` ≥ el tamaño del modelo con
  latencia ≤ +10 %, DD-5 se cierra `skipped` con los números.
- CUDA: con `device: cuda` los pesos viven en la GPU; el mmap aplica sólo al camino CPU.

## DD-6 — Perillas de runtime ONNX

**Decisión.** Claves nuevas en `embeddings:` y `reranking:` (con env equivalentes para tests y
compatibilidad), con defaults fijados por medición en TASK-007:

| Clave | Hoy | Propuesta inicial (a medir) |
|---|---|---|
| `intra_threads` | `available_parallelism()` (20) | `min(4, nproc/2)` |
| `inter_threads` | default ORT | 1 (`with_parallel_execution(false)`) |
| `batch_size` | env `DEVCTX_EMBED_BATCH_SIZE`, 8 tras 0.8.5 | config + env, 8 |
| `memory_pattern` | default ORT (on) | **off**: las formas son dinámicas (`BatchLongest`), el plan precalculado no se reusa y retiene buffers |
| `cpu_arena` | on | medir on vs off; si on, `arena_extend_strategy = kSameAsRequested` vía `OrtArenaCfg`/`ort::api()` |
| `max_length` | 512 | 512 (DD-4) |

Precedencia: env > config > default (igual que DD-7), para que un usuario pueda probar sin editar
YAML. El central usa los de `defaults.embeddings` de su config.

## DD-7 — DuckDB acotado, configurable y observable

**Estado real** (master H4): ya hay `SET memory_limit='2GB'; SET threads=4` en `Store::open`
(`store.rs:121,221-230`), override por env, resultado descartado.

**Decisión.**
- `Storage` (`crates/devctx-core/src/config.rs:150-173`) gana `memory_limit: String` y
  `threads: usize` (vacío/0 = default). El central los toma de `defaults.storage` o de una sección
  propia. Precedencia: `DEVCTX_DB_MEMORY_LIMIT`/`DEVCTX_DB_THREADS` > config > default.
- `Store::open` recibe los límites (firma nueva `Store::open_with(path, dim, &DbLimits)`, la vieja
  queda como atajo con defaults) y **lee de vuelta** los valores de `duckdb_settings()`; si el `SET`
  falló, `eprintln!` con el motivo y el valor efectivo se reporta en `status.memory.duckdb`.
- `temp_directory` explícito junto al DB (`<db>.tmp`, que ya es el default de DuckDB) y
  `max_temp_directory_size` acotado, para que un spill no llene el disco en silencio.
- Defaults: se fijan con la matriz de TASK-002 (HNSW sobre copia de REVFA_FrontEnd con 512 MB /
  1 GB / 2 GB × threads 2/4: ¿termina?, tiempo, `VmHWM`, spill). Propuesta en Q-1.

## DD-8 — `embeddings.provider: central` (opcional)

**Decisión.** Modo opt-in, apagado por defecto, evaluado con números después de TASK-008.

- **Servidor (central):** `POST /embed { "texts": [...], "model": "<key>", "dimension": 384,
  "max_length": 512 }` → `{ "vectors": [[...]], "model": "<key>", "dimension": 384,
  "engine": "ort-direct/1", "version": "<devctx>" }`. Rechaza (409) si `model`/`dimension`/
  `max_length` no coinciden con su embedder (`shares_vector_space`, `devctx-central/src/lib.rs:158`,
  por fin tiene uso).
- **Sin mutex:** el handler toma el `Arc<dyn EmbeddingProvider>` con el lock **sólo para clonarlo**
  y embebe fuera (`spawn_blocking`), a diferencia de `remember`/`recall` hoy
  (`devctx-api/src/central.rs:495-515,688-696`). Se aprovecha para sacar también la inferencia de
  `remember`/`recall` de debajo del lock (mismo patrón: embeber primero, escribir con lock).
- **Concurrencia:** semáforo (`tokio::sync::Semaphore`, default 1-2 permisos, configurable) y cola
  acotada; si la cola está llena, 503 con `Retry-After`. El indexador del serve manda lotes de
  `batch_size` textos por request.
- **Cliente:** `RemoteProvider` en `devctx-embed` que reusa el transporte de `CustomProvider`
  (`api.rs:149-189`) pero descubre URL y token vía `CentralClient` (serve.json del central), valida
  `model`/`dimension` de cada respuesta y la versión mayor/menor del binario. De paso, `OpenAiProvider`
  y `VoyageProvider` ganan `base_url` (hoy fijas en `api.rs:10-11`).
- **Central caído:** reintento perezoso con el mismo `ensure` que ya auto-lanza el central
  (`devctx-central/src/client.rs:~225-270`); si no levanta, **error explícito** con causa. No hay
  fallback silencioso a `local` (cargar el modelo es justo lo que el modo evita); opción
  `embeddings.central_fallback: local` para quien prefiera degradar a cargar.
- **Versionado:** header `X-Devctx-Version`; el central rechaza clientes de otra minor con un
  mensaje que dice cuál reiniciar (mismo criterio que `mcp_version` de PLAN-008 D2).

**Cuándo NO conviene:** indexación completa grande (latencia HTTP + serialización JSON de vectores).
Si TASK-009 mide que el throughput cae > 30 %, el modo queda documentado sólo para serves de
consulta (búsqueda/recall), y la indexación sigue local.

## DD-9 — Opción B descartada

Un único proceso "workspace" con todos los proyectos (un modelo, un pool) es lo más eficiente en
RAM, pero rehace binding, locks, `serve.json`, idle y reconexión que PLAN-008 P0 acaba de
estabilizar (TASK-001…009). DD-5 + DD-8 dan la mayor parte del ahorro sin tocar ese ciclo de vida.
Se descarta con el usuario el 2026-10-05.

## DD-10 — Orden y compuertas

1. TASK-001 (línea base) y TASK-003 (dorados, mientras fastembed existe) primero.
2. El motor nuevo (TASK-004/005) convive con fastembed hasta pasar el arnés; recién ahí TASK-008.
3. TASK-006 y TASK-007 dependen de la línea base: sin "antes" no hay "después".
4. TASK-009 al final y opt-in; su default (Q-3) se decide con sus números.
5. Cross-check Windows/macOS vía push a `fix/**` antes de cualquier tag.
