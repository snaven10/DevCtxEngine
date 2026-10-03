# PLAN-009 — Grafo estático preciso y contexto por grafo

**Fecha:** 2026-10-03
**Fase:** 0 (borrador) — esqueleto aprobado por el usuario el 2026-10-03. Se detalla después de cerrar PLAN-008; las tasks son stubs de un
párrafo y sus referencias `archivo:línea` se re-verifican al detallarlas.
**Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
**Origen:** consenso de dos sesiones sobre el inventario del grafo (`devctx-graph-inventory.md`) y el
estado del arte (`research-state-of-art.md`), scratchpad de la sesión 695c60fd…, 2026-10-03.
**Requiere:** cambio de schema + reindex completo de cada proyecto (~1 h por ~1400 archivos en 8 cores).

---

## 1. Problema (medido)

Medido sobre una copia del índice de `REVFA_BackEnd` (rama `development`, binario 0.8.2):

| Métrica | Valor |
|---|---|
| Tipos de arista | **uno solo**: `calls` (81 811). Sin `imports`, `inherits`/`implements`, `contains` |
| IDs de símbolo | no hay tabla `symbols`; `vectors.symbol` es el nombre pelado y `graph_edges.source` es `Clase.metodo` → no hay join grafo ↔ definición |
| `target_file` / `metadata` poblados | 0 (`graph.rs:82` inserta `''`) |
| Targets sin calificar | 53 389 (65 %) |
| Aristas hacia JDK/librerías | ~70 % (sin marca `external`) |
| Aristas desde archivos de test | 46 966 (57 %) (sin marca `test`) |
| Nombres de método con >1 forma calificada | 721 de 7 807 |
| Calificador falso `var.*` (Java `var` tomado como tipo) | 481 |
| `impact_analysis("map")` / `("get")` | ~5.5 s / ~3.9 s: una consulta SQL por nodo (`graph.rs:300`), expansión sin tope |

Además, de campo: TS `export const x = () =>` no se extrae como símbolo; Python `get_references`
deduplica por caller (pierde la 2.ª llamada), `impact` no ve callers a nivel de módulo, ruido de
builtins; Java: caller vía campo inyectado por constructor en clase interna no se resuelve
(`PrepartidaResource.java:310`, REVFA_REGISTRO_EXTERIOR).

Causas (inventario §7): `type_map` plano por archivo con `or_insert` (`parser.rs:89-112`), resolución
puramente sintáctica del receptor (`qualified_target`, `parser.rs:312-343`; `receiver_of`,
`:345-360`), receptor en mayúscula asumido tipo (`:327`), imports extraídos pero no persistidos
(`devctx-chunk/src/chunker.rs:209`), herencia no extraída, dedupe `UNIQUE(source,target,kind,…,
source_file)` que colapsa llamadas repetidas.

## 2. Decisión de enfoque

**Híbrido: (A) tree-sitter mejorado como base obligatoria + (C) ingestión SCIP opcional después (plan
propio).** Orden de preferencia de la investigación: E (A+C) > A sola > C sola > D > B.

| Opción | A favor | En contra | Decisión |
|---|---|---|---|
| **(A)** tree-sitter + defs/refs estilo `tags.scm` + resolución de imports + tipos con scope + `confidence` por arista + descartar aristas fluent no tipables | Sin dependencias de runtime, encaja con el binario único, incremental por archivo como hoy; grafos gruesos pero limpios ya mejoran la localización (LocAgent https://arxiv.org/abs/2503.09089, RepoGraph https://arxiv.org/abs/2410.14684). Queries de partida en las gramáticas (https://tree-sitter.github.io/tree-sitter/4-code-navigation.html) | Sin inferencia de tipos, genéricos, overloads ni dispatch dinámico; queries y reglas de import por lenguaje (~6). La doc de tags dice explícitamente que no resuelve nombres | **Base. Este plan.** |
| **(C)** Ingerir `index.scip` (scip-java, scip-typescript, scip-python, rust-analyzer) con el crate `scip` (Apache-2.0) | Precisión de compilador, `is_implementation` para herencia; gobernanza abierta desde 2026 (https://sourcegraph.com/blog/the-future-of-scip, https://github.com/sourcegraph/scip, https://docs.rs/scip/latest/scip/) | Los indexers necesitan el build resuelto (scip-java +45-50 % de compilación: https://sourcegraph.github.io/scip-java/docs/benchmarks.html); el índice envejece entre corridas; no trae call graph explícito (se deriva con `enclosing_range`, https://raw.githubusercontent.com/sourcegraph/scip/main/scip.proto) | **Después, plan propio.** El schema de este plan deja `source=treesitter|scip` previsto |
| **(D)** LSP on demand (estilo Serena, https://github.com/oraios/serena) | Precisión máxima en vivo | Un language server por lenguaje y repo (JVM, Node), arranque lento, RAM; JSON-RPC por símbolo es cuello de botella para indexar (https://arxiv.org/abs/2604.18413) | Último recurso, fuera de alcance |
| **(B)** stack-graphs | Teoría ideal, incremental sin build | **Archivado 2025-09-09**, RUSTSEC-2026-0303 unmaintained, solo Java/JS/Python/TS (https://github.com/github/stack-graphs, https://vulners.com/rustsec/RUSTSEC-2026-0303) | **Descartado** |

Kythe (exige integración con el build) y universal-ctags (GPL, redundante con tree-sitter) también
descartados.

Sobre el consumo: el valor está en tools que el agente llama (repo-map, recorrido, skeleton), no en
empujar contexto masivo. Repo-map con PageRank personalizado: Aider eligió el archivo correcto en el
70.3 % de SWE-bench Lite (https://aider.chat/2024/05/22/swe-bench-lite.html, pesos en
https://raw.githubusercontent.com/Aider-AI/aider/main/aider/repomap.py). Skeletons sirven para
*localizar*, no como contexto de edición (https://arxiv.org/abs/2607.09691). Resúmenes en lenguaje
natural por símbolo: **no** (4/45 vs 27/45 con código fuente, mismo paper).

## 3. Tasks (orden acordado)

| TASK | Título | Alcance (una línea) | Depende de | Estado |
|------|--------|---------------------|------------|--------|
| TASK-001 | Tabla `symbols` con IDs estables y aristas tipadas | `symbols(id, repo, branch, file, kind, name, qualified, parent_id, signature, span)`; aristas `calls/imports/inherits/contains` con `target_symbol_id` cuando resuelve | — | `pending` |
| TASK-002 | `confidence` y marcas `external`/`test` por arista | `confidence ∈ {high, medium, low}` según cómo se resolvió; `external` si no hay definición en el repo; `test` por ruta (reusa `path_kind` de PLAN-008 TASK-011); aristas fluent no tipables se descartan | TASK-001 | `pending` |
| TASK-003 | Resolución Java/TS/Go/Rust: `var`, campos, constructor, imports, scope | `type_map` con scope por función, `var` inferido del inicializador simple, campos inyectados por constructor (incl. clases internas), imports para calificar, constantes MAYÚSCULA no son tipos, Rust `Foo::bar`, Go receptores | TASK-002 | `pending` |
| TASK-004 | TS arrow-const y arreglos de Python | `export const x = () =>` como símbolo; Python: no deduplicar por caller, callers a nivel de módulo, filtro de builtins | TASK-002 | `pending` |
| TASK-005 | `impact_analysis` por lotes y con tope | Una consulta por nivel de profundidad (no por nodo), tope de expansión, ranking por `confidence` y excluyendo `external`/`test` por defecto | TASK-003, TASK-004 | `pending` |
| TASK-006 | Repo-map con PageRank personalizado bajo presupuesto | Tool `repo_map(budget, focus_files, focus_symbols)` que devuelve solo firmas; pesos estilo Aider | TASK-005 | `pending` |
| TASK-007 | Centralidad como prior en `build_context`/`search` | PageRank/in-degree global precalculado al indexar, sumado como señal en la RRF | TASK-006 | `pending` |
| TASK-008 | Expansión ego-graph de k saltos como firmas | Callers/callees directos de los hits de `build_context` como firmas, no cuerpos | TASK-007 | `pending` |
| TASK-009 | Tool `traverse` | `traverse(symbol, edge_types, depth, direction)` sobre file/class/function con presupuesto y paginación | TASK-005 | `pending` |
| TASK-010 | Vista skeleton por archivo | Tool `file_skeleton(file)`: símbolos de primer nivel con firma y rango | TASK-001 | `pending` |
| TASK-011 | Docs EN + ES y guía de reindex | Schema nuevo, `devctx index --full` obligatorio, tools nuevas | TASK-008, TASK-009, TASK-010 | `pending` |
| TASK-012 | Verificación de campo con métricas del §1 | Re-medir la tabla del §1 en REVFA_BackEnd y los casos de campo | TASK-011 | `pending` |

## 4. Riesgos

- **Migración de schema.** Primera migración real de tablas del índice: `symbols` nueva,
  `graph_edges` con columnas nuevas (`target_symbol_id`, `confidence`, `external`, `test`,
  `edge_source`). Hay que decidir si se migra en sitio o se exige `index --full` (recomendación
  preliminar: tabla nueva + reindex obligatorio detectado por la versión de extractor de PLAN-008
  TASK-007, sin migrar datos viejos).
- **Costo de reindex.** ~1 h por ~1400 archivos en 8 cores (AGENTS.md §6), por proyecto y rama;
  revfa tiene 13 repos. Además +10-30 % de tiempo de indexado estimado por la resolución extra
  (estimación propia de la investigación, no medida).
- **Grafo ruidoso vuelve contraproducente a PageRank**: TASK-006…008 no arrancan sin que TASK-002…005
  bajen el ruido medido (externos, tests, `var.*`).
- **Precisión por debajo de compilador** (sin genéricos/overloads/dispatch): se acepta y se expone vía
  `confidence`; SCIP es la salida para quien la necesite.
- **Tamaño del DB** crece con `symbols` y aristas nuevas: medir en TASK-012.

## 5. Fuera de alcance (planes propios después)

- Ingestión SCIP (`devctx graph import-scip`, autodetección de `index.scip`).
- Señales de git: co-change y blame por símbolo (HAFixAgent, https://arxiv.org/abs/2511.01047).
- `tests_for(symbol)` y enlaces test ↔ código.
- LSP on demand.
- Resúmenes en lenguaje natural por símbolo.

## 6. Verificación (a detallar)

Re-medir la tabla del §1 sobre REVFA_BackEnd tras reindex; casos de campo de §1 (TS arrow-const,
Python, `PrepartidaResource.java:310`); latencia de `impact_analysis("map")` < 1 s; tokens de
`repo_map` ≤ presupuesto.

## 7. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
