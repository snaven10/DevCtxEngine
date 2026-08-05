# DevAI → Rust + DuckDB — Plan de reescritura

> Estado: **propuesta / plan**. Reescritura completa del binario Go + servicio ML Python
> a un único binario **Rust**, reemplazando **Qdrant/LanceDB + SQLite** por **DuckDB** (una sola BD).

## 1. Decisiones tomadas

| Tema | Decisión |
|---|---|
| Capa ML | **fastembed-rs + `ort` (ONNX Runtime)** para modelos locales; OpenAI/Voyage/custom por HTTP |
| Base de datos | **DuckDB unificada** — vectores (extensión VSS) + tablas relacionales (grafo, rutas, memorias, estado de índice) en un solo archivo |
| Estrategia | **Proyecto Rust nuevo, incremental**, módulo por módulo con paridad verificable contra el binario Go/Python de referencia |
| Modelos | Multi-modelo de primera clase: MiniLM (384), BGE (384/768), multilingües, y **Granite** (`granite-embedding-97m-multilingual-r2`, 384, ONNX int8; y `-311m`, 768). La dimensión del vector es **parametrizable**, no fija en 384 |

## 2. Qué es hoy DevAI (referencia)

Arquitectura híbrida, ~20k LOC:

- **Go (~10.4k LOC)** — orquestador delgado: CLI (cobra), servidor **MCP** (21 tools, `mark3labs/mcp-go`), API HTTP (`internal/api`), TUI (Bubble Tea), config/storage routing, y un **puente JSON-RPC 2.0 sobre stdio** hacia el sidecar Python (NO es gRPC; el `.proto` es aspiracional y no se usa).
- **Python (~9.7k LOC, `devai_ml`)** — hace todo el trabajo real: embeddings, chunking, parsers tree-sitter, retrieval/reranking, summarization, y los stores (LanceDB local / Qdrant compartido / SQLite para grafo+memorias+estado).

El contrato real entre las dos capas son los **~27 métodos JSON-RPC** que invoca `internal/mlclient/client.go`.

### Ganancia estructural del rewrite
Al ser **un solo binario Rust**, el puente JSON-RPC-sobre-stdio y el sidecar Python **desaparecen**: todo pasa a ser llamadas en proceso. Se elimina el arranque del intérprete, el watchdog de respawn, el timeout de 120s y toda la lógica de serialización entre procesos.

## 3. Arquitectura Rust propuesta (Cargo workspace)

```
devai/
├─ Cargo.toml                 # [workspace]
├─ crates/
│  ├─ devai-cli/              # binario `devai` — clap (reemplaza cobra)
│  ├─ devai-core/             # tipos compartidos, config (.devai/config.yaml), errores
│  ├─ devai-store/            # DuckDB: vectores (VSS) + grafo + rutas + memorias + index_state
│  ├─ devai-embed/            # fastembed-rs/ort local + OpenAI/Voyage/custom (reqwest); model registry
│  ├─ devai-parse/            # tree-sitter (18 langs) + extractores de rutas (7 frameworks)
│  ├─ devai-chunk/            # semantic_chunker + memory_chunker
│  ├─ devai-rerank/           # cross-encoder (fastembed TextRerank, ms-marco-MiniLM-L-12-v2)
│  ├─ devai-summarize/        # extractiva (default) + opcional
│  ├─ devai-index/            # pipeline orchestrator + git state (diff→parse→chunk→embed→store)
│  ├─ devai-mcp/              # servidor MCP (crate `rmcp`, SDK oficial) — 21 tools
│  ├─ devai-api/             # API HTTP opcional (axum)
│  └─ devai-tui/             # TUI opcional (ratatui)
```

### Mapeo de dependencias Go/Python → Rust

| Función | Hoy | Rust |
|---|---|---|
| CLI | cobra | `clap` (derive) |
| MCP server | mark3labs/mcp-go | `rmcp` (SDK MCP oficial de Rust) |
| Embeddings locales | sentence-transformers (torch) | `fastembed` + `ort` + `tokenizers` |
| Embeddings API | httpx | `reqwest` |
| Vector store | LanceDB / Qdrant | **DuckDB + VSS** (`duckdb` crate) |
| Relacional (grafo/memorias/estado) | SQLite (`modernc.org/sqlite`) | **DuckDB** (mismo archivo) |
| Tree-sitter | py-tree-sitter | `tree-sitter` + grammars por-lang |
| Reranker | FlashRank | `fastembed` TextRerank |
| Summarización | extractiva/flan-t5/openai | extractiva en Rust; abstractiva vía `ort` (opcional) |
| Git | shell out | shell out a `git` (igual) o `git2` |
| Config YAML | gopkg.in/yaml.v3 | `serde_yaml` |
| TUI | Bubble Tea | `ratatui` |
| API HTTP | net/http | `axum` |

## 4. Diseño de datos en DuckDB

Una sola BD `index.duckdb` con la extensión VSS cargada (`INSTALL vss; LOAD vss;`).

### 4.1 Tabla `vectors` (reemplaza LanceDB + Qdrant)
Columnas idénticas al schema canónico actual (LanceDB Arrow / payload Qdrant):

| columna | tipo DuckDB | notas |
|---|---|---|
| id | VARCHAR PK | code: `sha256("{repo}:{branch}:{file}:{start_line}")[:32]`; memoria: `mem_<hash[:24]>` (+`_c{n}`) |
| text | VARCHAR | texto completo del chunk (lo lee el reranker) |
| vector | `FLOAT[N]` | **N = dimensión del modelo** (384/768/…); debe igualar `index_state.model_dimension` |
| repo, branch, commit, file, symbol, symbol_type, language | VARCHAR | |
| start_line, end_line | INTEGER | |
| chunk_level | VARCHAR | file/class/function/block / memory / memory_chunk |
| content_hash | VARCHAR | sha256[:16] |
| is_deletion | BOOLEAN | |
| memory_type, memory_scope, memory_tags | VARCHAR | tags = CSV |
| indexed_at | VARCHAR | ISO-8601 |

- Índice: `HNSW` de la extensión VSS con métrica **cosine** (`array_cosine_distance`) para igualar la semántica de Qdrant.
- Búsqueda: `SELECT ... ORDER BY array_cosine_distance(vector, $q) LIMIT $k` + `WHERE` para filtros (escalar `=`, lista `IN`).
- Operaciones a soportar (paridad con el Protocol actual): `upsert` (delete-by-id + insert), `search`, `delete_by_file`, `delete_memory_vectors` (barre `id` + `id_c1..id_c256`), `rename_file`, `count`, `scroll_all`.

> ⚠️ **Riesgo #1 — HNSW persistente en DuckDB es experimental.** Requiere
> `SET hnsw_enable_experimental_persistence=true` y en algunas versiones el índice
> no se actualiza tras ciertos DML (hay que reconstruirlo). **Mitigación**: arrancar con
> **brute-force** (`array_cosine_distance` sin índice) — para un índice por-proyecto
> (decenas–cientos de miles de chunks) es más que suficiente y 100% robusto. Añadir HNSW
> detrás de un flag cuando el volumen lo justifique.

### 4.2 Tablas relacionales (mismas que hoy en SQLite, ahora DuckDB)
- `graph_edges(source, target, kind, source_file, target_file, line, repo, branch, metadata)` — UNIQUE(source,target,kind,repo,branch,source_file). Alimenta `impact_analysis` (BFS up/down), callers/callees.
- `routes(framework, http_method, path, handler_class, handler_method, handler_symbol, file, line, repo, branch, indexed_at)` — UNIQUE(framework,http_method,path,repo,branch).
- `memories(id, title, content, memory_type, scope, project, topic_key, tags, author, repo, branch, files, revision_count, duplicate_count, normalized_hash, vector_id, session_id, created_at, updated_at, deleted_at)` — dedup por topic_key → hash-en-ventana-900s → insert.
- `memory_symbol_references(memory_id, symbol, file, line, repo, branch, source)` — M:N memoria↔símbolo/archivo.
- `sessions(id, project, directory, started_at, ended_at, summary)`.
- `index_state / file_state / branch_lineage` — indexación incremental (skip por content-hash) y detección de cambio de modelo.

> ⚠️ **Riesgo #2 — Búsqueda de texto (FTS).** Hoy hay una tabla FTS5 (`graph_symbols_fts`,
> con expansión camelCase y `remove_diacritics`). DuckDB tiene extensión `fts` pero es más
> débil (índice no incremental → hay que `PRAGMA` reconstruir). **Opciones**: (a) DuckDB `fts`
> con rebuild en `fts_rebuild`; (b) matching por `LIKE`/expansión propia en Rust; (c) mantener
> `rusqlite` con FTS5 solo para este índice. Recomendado: empezar con (b), simple y sin extensión.

## 5. Capa de embeddings (multi-modelo)

`devai-embed` con un `ModelRegistry` que replica el actual y mantiene la dimensión como dato:

- **Local (fastembed/ort)**: MiniLM-L6/L12 (384), BGE-small (384)/base (768), multilingües MiniLM (384)/mpnet (768), **Granite-97m (384, ONNX int8)** y **Granite-311m (768)**. Todos L2-normalizados.
  - fastembed-rs trae varios de estos; los que no estén (p.ej. Granite) se cargan como **modelo ONNX definido por el usuario** (`UserDefinedEmbeddingModel`) con su `model.onnx` + `tokenizer.json` (HF `tokenizers`). Granite ya viene en ONNX int8 (`onnx/model_quint8_avx2.onnx`) → requiere CPU con AVX2.
- **API**: OpenAI (small/large/ada), Voyage (code-3/3-lite/3, `input_type=document`), custom (POST `{endpoint}/embed`).
- Guardas de RAM: cap de caracteres (`DEVAI_EMBED_MAX_CHARS`, 4096) y batch (`DEVAI_EMBED_BATCH_SIZE`, 16).

> ⚠️ **Riesgo #3 — Cambio de dimensión = reindex.** Si la dimensión del modelo activo
> ≠ `index_state.model_dimension`, se fuerza reindex completo. La columna `vector FLOAT[N]`
> se fija por-BD según el modelo; cambiar de 384↔768 recrea la tabla `vectors`.

## 6. Detalles a preservar (no perder en la traducción)

- **IDs deterministas** de code (`sha256(...)[:32]`) y memoria (`mem_<hash>` + sufijos `_c{n}`); `delete_memory_vectors` depende del patrón `_c{n}`.
- **Chunker semántico multinivel** (file/class/function/block), nunca parte a mitad de símbolo; header de contexto `# file > class > method`; funciones grandes (>1024 tok) se parten en bloques snapeando a líneas en blanco.
- **Memory chunker + blend**: intro-vector (`memory`) + ventanas de cuerpo (`memory_chunk`, overlap 30, ≤40); en recall `score = alpha*intro_sim + (1-alpha)*max_chunk_sim` (alpha=0.5).
  - ⚠️ La conversión `sim = 1 - d/2` asume **L2² sobre vectores normalizados**. Con `array_cosine_distance` en DuckDB, `distancia = 1 - cos` → usar `sim = 1 - d` directamente. **Ajustar esta fórmula** al elegir la métrica.
- **Resolución FQN de llamadas** en parsers (import maps + field-type maps para promover `repo.findById()` a target calificado; source id `<file>::Class.method`).
- **7 extractores de rutas** por regex (quarkus/spring/fastapi/flask/express/nest/angular).
- **Reranking**: fetch 15 candidatos → cross-encoder → truncar a `limit`; degradar a orden original ante cualquier fallo.
- **Guardia de privacidad** en summarización (`require_local=true` bloquea proveedores cloud por defecto).

## 7. Se puede eliminar (simplificación del fork)

- Todo el puente **JSON-RPC/stdio** y el **runtime/venv de Python** (`internal/mlclient`, `internal/runtime`, `ml/`, `proto/`).
- **Modo hybrid + Qdrant + sync** (`HybridVectorStore`, `QdrantVectorStore`, `push/pull/sync_index`, health thread, retry deque) — salvo que quieras conservar un backend compartido de equipo (decisión abierta, ver §9).
- `make setup` de Python, install de runtime portable, etc.

## 8. Fases (incremental, con paridad verificable)

Cada fase deja el binario compilando y con tests; se compara salida contra el DevAI actual sobre un repo de prueba.

- **F0 — Andamiaje**: workspace Cargo, `devai-core` (config YAML, tipos), CLI esqueleto con `clap` (`version`, `init`, `status`). CI (fmt/clippy/test).
- **F1 — Store DuckDB**: `devai-store` con tabla `vectors` (brute-force cosine) + relacionales; migraciones; tests de upsert/search/delete/scroll. **Hito: paridad de esquema.**
- **F2 — Embeddings**: `devai-embed` con MiniLM-L6 local (fastembed) + registry + dimensión parametrizable; luego Granite (ONNX int8) y providers API. Tests de dimensión/normalización.
- **F3 — Parse + Chunk**: tree-sitter (arrancar con python/js/ts/go/java/rust) + chunker semántico; después el resto de langs y extractores de rutas. Tests de golden chunks.
- **F4 — Indexación**: `devai-index` (git diff → parse → chunk → embed → store) + `index_state` incremental. **Hito: `devai index` produce vectores equivalentes.**
- **F5 — Búsqueda + rerank + memorias**: `search`, `read_symbol`, `get_references`, `impact_analysis`, `remember/recall` con blend, rutas. Reranker cross-encoder.
- **F6 — MCP server** (`rmcp`): las 21 tools sobre las capas anteriores. **Hito: un agente se conecta y funciona.**
- **F7 — Extras**: API HTTP (axum), TUI (ratatui), git hooks, `model` mgmt, summarización.
- **F8 — HNSW opcional + FTS**: activar índice VSS detrás de flag; decidir FTS.

## 9. Decisiones

- **Modo compartido de equipo — DESCARTADO (solo-local).** Se elimina shared/hybrid/Qdrant y `push/pull/sync_index`. Cada dev tiene su `index.duckdb` local. (§7)
- **Ubicación del workspace Rust: `rust/`.** Convive con el árbol Go/Python de referencia durante el port incremental; se promueve a la raíz al alcanzar paridad.

### Resueltas
- **Búsqueda híbrida — implementada.** Crate `devai-search` con 3 modos
  (vector / keyword / hybrid). Hybrid fusiona los rankings vectorial + BM25 por
  Reciprocal Rank Fusion (`Σ 1/(k+rank)`, k=60) y luego rerank opcional; degrada a
  vector-only si no hay índice FTS. CLI `search --hybrid`/`--keyword`; tool MCP
  `search` con `mode`. Centraliza la lógica antes duplicada en CLI/MCP.
- **FTS (BM25) — implementado, opt-in.** `storage.fts: true` reconstruye un índice
  full-text con la extensión DuckDB `fts` (`PRAGMA create_fts_index`) tras indexar;
  `devai search --keyword` usa `match_bm25` sobre `vectors.text` (con los mismos
  filtros). Rebuild-on-demand (no incremental); best-effort con fallback si la
  extensión no está. Complementa la búsqueda vectorial (fusión híbrida = follow-up).
- **HNSW (VSS) — implementado, opt-in.** Brute-force por defecto; `storage.hnsw: true`
  construye el índice `USING HNSW (vector) WITH (metric='cosine')` tras indexar. La
  extensión VSS se carga best-effort (`INSTALL vss; LOAD vss`) con fallback a
  brute-force si no está disponible. La persistencia HNSW usa el flag experimental
  de DuckDB. La búsqueda no cambia: el optimizador VSS usa el índice para
  `ORDER BY array_cosine_distance(...) LIMIT` (sin filtros).

### Abiertas (a decidir en su fase)
1. **Summarización abstractiva** (flan-t5): ¿portar vía `ort` o dejar solo extractiva al inicio?
2. **TUI/API**: ¿en alcance del fork o posponer?
```
