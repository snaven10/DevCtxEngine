# PLAN-009 — Grafo estático preciso y contexto por grafo

**Fecha:** 2026-10-05 (esqueleto del 2026-10-03, detallado tras cerrar PLAN-008)
**Fase:** 1 (Planificación) — **aprobado por el usuario 2026-10-06** (decisiones en §12). Es la
**prioridad** sobre PLAN-010 (decisión del usuario y de las dos sesiones, 2026-10-05).
**Diseño:** [`PLAN-009-design.md`](./PLAN-009-design.md)
**Fase anterior:** [PLAN-008](../PLAN-008-robustez-y-salida-para-agentes/PLAN-008-robustez-y-salida-para-agentes.md)
(0.9.0: `kind`/`path_kind`, `raw_score`, anclaje por identificador, `external`/`suggestions`,
penalización por posición, selección de miembro de grupo). Este plan absorbe sus pendientes 1 y 2
de "Pendientes después de 0.9.0".
**Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`). Sale de `main` = v0.9.0
(`fdc7dd2`, `origin/main`; el ref local `main` de este clon quedó en 0.8.4). Rama sugerida:
`feat/plan-009-grafo`.
**Origen:** inventario del grafo medido sobre backend-a (`devctx-graph-inventory.md`) y estado
del arte (`research-state-of-art.md`), scratchpad de la sesión 695c60fd…, 2026-10-03; datos de campo
de PLAN-008 TASK-016.
**Release:** `0.10.0` u `0.11.0` según cómo salga PLAN-010 (Q-1).

Todas las referencias `archivo:línea` son contra `fdc7dd2`. Cada task las re-verifica al arrancar.

---

## 1. Qué resuelve

devctx existe para darle a un agente **el contexto útil por token** de un repo. Hoy el grafo de
código, que debería ser la parte barata y precisa de ese contexto, es la más pobre: una sola
relación (`calls`), sin identidad de símbolo, resuelta por coincidencia de strings, sin distinguir
librerías ni tests, y que no alimenta ni el ranking ni `build_context`. Consecuencias que el usuario
ve:

- `impact_analysis("map")` tarda ~5.5 s y devuelve 2 569 "callers" que son casi todos `.map()` de
  Mutiny/streams, no el `map` del repo.
- `get_references` pierde la segunda llamada desde el mismo método; en Python no ve llamadas a nivel
  de módulo.
- `read_symbol("tokenInterceptor")` en frontend no encuentra nada: `export const x = () =>`
  no es un símbolo.
- En backend-b, la llamada `service.findPaginated(…)` de `DraftResource.java:310`
  se atribuye a `AlphaService` cuando el campo es `BetaService` (§2 H3).
- `build_context` arma el brief solo con búsqueda (híbrida desde PLAN-008 TASK-013): nunca trae al
  caller o la implementación de lo que encontró, ni una vista de "qué hay en este repo".

El objetivo es un grafo **grueso pero limpio** (file/class/function con `calls`, `instantiates`,
`imports`, `inherits`/`implements`, `contains`, `references`), con identidad estable, confianza por
arista y ruido marcado, y tres formas de consumirlo que el agente llama: `repo_map`, `traverse` y
`skeleton`, más el grafo como señal en `search` híbrido y en `build_context`. La evidencia
(LocAgent, RepoGraph, Aider; master del esqueleto y `research-state-of-art.md`) dice que un grafo
así ya mejora la localización sin precisión de compilador.

### 1.1 Números medidos

**Inventario del grafo** (copia del índice de backend-a, rama `development`, binario 0.8.2,
2026-10-02; 1 684 archivos versionados hoy):

| Métrica | Valor |
|---|---|
| Tipos de arista | **uno**: `calls` (81 811 filas, ya deduplicadas por par) |
| Sources / targets distintos | 9 277 / 8 990 |
| Tabla de símbolos / IDs | no existe; `vectors.symbol` pelado vs `graph_edges.source` `Clase.metodo` |
| `target_file` / `metadata` poblados | 0 |
| Targets sin calificar | 53 389 (65.3 %) |
| Aristas a métodos definidos en el repo ("internas", cota por nombre) | 24 779 (30.3 %); el resto (69.7 %) JDK/librerías |
| Aristas desde archivos de test | 46 966 (57.4 %) |
| Targets pelados que colisionan con un método interno (`item`, `map`, `build`, `put`…) | 13 793 (17 %) |
| Nombres de método con > 1 forma calificada | 721 de 7 807 (`setUp` 93, `listar` 24, `crear` 23) |
| Calificador falso `var.*` (Java `var` tomado como tipo) | 481 |
| Calificadores más frecuentes | `String` 2 884, `Uni` 2 197, `Response` 1 368, **`LOG` 1 272** (constante tomada por tipo) |
| Chunks en `vectors` | 19 795 (11 711 `method`, 2 961 `class`) |

Latencia de `impact` (modelo en python de las mismas consultas SQL, profundidad 3):

| Símbolo | seeds | upstream | downstream | tiempo |
|---|---|---|---|---|
| `OfficeService.actualizar` | 1 | 1 | 39 | 0.09 s |
| `actualizar` (21 homónimos) | 22 | 9 | 200 | 0.63 s |
| `get` | 51 | 1 883 | 0 | 3.9 s |
| `map` | 4 | 2 569 | 11 | 5.5 s |

**Campo de PLAN-008 (TASK-016, 0.9.0 candidato)**: anclaje híbrido pone la definición primero en
7/8 consultas; `external` falla con nombres calificados (`serde_json::from_str`, `tokio::spawn`,
`Panache.withTransaction` → solo `suggestions: []`, hallazgo B2); sugerencias ruidosas para externos
(`with_context` → `build_context`, B3); `build_context` de grupo elige bien 7/7; `VmHWM` de
`index --full` en DevCtxEngine 882 MB; indexar frontend (2 133 archivos) > 25 min con la
máquina cargada.

**Costo de reindexar**: ~1 h por ~1 400 archivos en 8 cores (`AGENTS.md` §6), dominado por el
embedding. El usuario aceptó reindexar ("no hay clavo en reindexar"); TASK-002 lo abarata.

TASK-001 re-mide todo lo de arriba sobre **0.9.0** (el inventario es de 0.8.2) y esa pasa a ser la
columna "Antes" de §6.

## 2. Hallazgos y causa raíz (verificado contra `fdc7dd2`, 2026-10-05)

### H1 — No hay identidad de símbolo ni join grafo ↔ definición

`Symbol` (`crates/devctx-parse/src/types.rs:4-30`) tiene nombre, kind, rango y `parent` como
**nombre**; no tiene calificado, firma ni id. El chunk guarda `symbol_name` pelado
(`crates/devctx-index/src/pipeline.rs:1011`), o `a, b, c +n` si es un `grouped`
(`crates/devctx-chunk/src/chunker.rs:305-341`). El grafo guarda `Clase.metodo`
(`crates/devctx-parse/src/parser.rs:296-306`). Todos los lookups son por sufijo:
`Store::symbol_definitions` (`crates/devctx-store/src/store.rs:865-930`, con una tercera pasada
por regex sobre el texto de los `grouped`), `Store::symbol_matches` (`store.rs:995-1032`, el
anclaje), `Store::symbol_suggestions` (`store.rs:940-987`), `Store::resolve_symbol`
(`crates/devctx-store/src/graph.rs:183-210`, `ends_with` sobre `source` y `target`),
`Store::external_call_sites` (`graph.rs:227-258`) e `is_anchored_definition`
(`crates/devctx-mcp/src/state.rs:675-682`).

### H2 — Una sola relación; imports y herencia se tiran

`extract_edges` emite solo `kind: "calls"` (`parser.rs:131`). `ParsedFile.imports` es texto crudo
(`types.rs:33-39`) y solo lo usa el chunk de archivo (`chunker.rs:206-222`). No hay captura de
`extends`/`implements`/`impl Trait for`, ni de `new X()` (`languages/java.json` `calls` solo
`method_invocation`), ni de constructores o records como símbolos (`java.json` `symbols`).

### H3 — Resolución plana: el primer `type_map` gana en todo el archivo

`extract_type_bindings` arma un mapa archivo → `{variable: Tipo}` con `or_insert`
(`parser.rs:89-112`, línea 108): sin scopes, sin clases internas, sin genéricos (`types` de
`java.json` solo captura `type_identifier`), con `var` tomado como tipo. Caso real verificado:
`DraftResource.java` (backend-b) tiene tres clases internas con un campo
`service` cada una (`AlphaService` l.234, `BetaService` l.293, `GammaService` l.352);
la llamada de la l.310 (dentro de `BetaServiceAdapter`) queda `AlphaService.findPaginated`.
`qualified_target` (`parser.rs:312-341`) toma cualquier receptor en mayúscula por tipo (l.327):
`LOG.info` → calificador `LOG` (1 272 aristas).

### H4 — Llamadas que se descartan o se colapsan

- Sin función envolvente no hay arista (`parser.rs:125-127`): se pierden las llamadas a nivel de
  módulo de Python y **todas** las de un `export const x = () =>` de TS, porque `arrow_function` no
  está en `function_kinds` (`typescript.json`, `tsx.json`) — y tampoco es símbolo.
- `replace_file_edges` deduplica por `(source, target, kind)` (`graph.rs:75-79`) sobre una tabla con
  `UNIQUE (source, target, kind, repo, branch, source_file)` (`crates/devctx-store/src/schema.rs:120`):
  la 2.ª llamada del mismo método al mismo destino desaparece (el bug de `get_references` en
  Python). `target_file`/`metadata` se insertan `''` (`graph.rs:81-83`).
- Cadenas fluent: `nameable` (`parser.rs:371-380`) evita nodos basura desde PLAN-003, pero cae al
  nombre pelado (`flatMap`, `map`, `subscribe`), que es la fuente de los miles de "callers" falsos.

### H5 — Lenguajes: huecos concretos

- **Rust**: `Foo::bar` captura solo `name` (`rust.json` `calls`, `scoped_identifier`) → `bar`
  pelado. `impl` no es símbolo; el contenedor se nombra con el texto del tipo
  (`container_name`, `parser.rs:395-400`), así que `impl<T: Clone> Cached<T>`
  (`crates/devctx-mcp/src/state.rs:212`) debería producir sources `Cached<T>.…` (a confirmar con
  test en TASK-004).
- **Go**: `container_kinds: []` (`go.json`) → métodos con receptor sin calificar.
- **Python**: builtins (`len`, `print`, `dict.get`) como targets pelados; sin módulo como fuente.
- **TypeScript**: sin DI (propiedades de parámetro del constructor, `inject()`), sin arrow-const.

### H6 — `impact` hace una consulta SQL por nodo y no tiene tope

`bfs` (`graph.rs:348-388`) llama `neighbours_of` por cada nodo de la cola (l.374-375). El
presupuesto de salida (`do_impact`, `state.rs:4031-4083`) recorta lo que se muestra, no lo que se
computa.

### H7 — El grafo no participa del contexto

`search_ranked` (`crates/devctx-search/src/lib.rs:269-357`): RRF de vector + keyword →
penalización por posición → dedup → rerank → anclaje. Ninguna señal de grafo. `do_build_context`
(`state.rs:4114-4171`) usa `search_items` híbrido con 30 hits y `compose_context`
(`state.rs:4205-4303`) solo concatena chunks y memorias.

### H8 — Hay con qué construir

- Los crates de gramática del `Cargo.lock` traen `TAGS_QUERY` (`tree_sitter_java::TAGS_QUERY`,
  etc.), pero son delgadas (Java 20 líneas, sin constructores, campos ni imports): sirven de
  referencia, no de reemplazo (DD-5).
- El mecanismo de rollout ya existe: `EXTRACTOR_VERSION` (`crates/devctx-parse/src/registry.rs:93`)
  + fingerprint de los JSON (`registry.rs:102-117`) + `index_meta` + `Store::extractor_stale`
  (`crates/devctx-store/src/state.rs:183-188`) + `branch_with_same_content` que no copia de ramas
  viejas (`state.rs:247-278`) + sello solo en `--full` (`pipeline.rs:542-562`).
- `vectors.content_hash` por chunk ya existe (`schema.rs:93`): permite reusar vectores al reindexar
  (DD-20).
- El arnés `scripts/bench-retrieval.sh` + `scripts/bench-queries.txt` (41 líneas, targets escritos
  antes de medir) ya existe y se extiende (DD-1).

### H9 — Pendientes de PLAN-008 que caen acá

1. `split_file_symbol` (`state.rs:5622-5631`) toma `Foo.Bar::baz` como archivo `Foo.Bar` (con
   `Bar` como extensión) cuando no hay `/` → TASK-008.
2. `external_call_sites` cae al último segmento (`graph.rs:254-257`): un `Foo::new` inexistente
   puede salir `external: true` si el `new` del repo es hoja → TASK-008 (con `symbols` el último
   segmento se verifica contra definiciones, no contra "nunca es source").

## 3. Decisiones

Resumen; detalle y alternativas en [`PLAN-009-design.md`](./PLAN-009-design.md).

| Id | Decisión |
|---|---|
| DD-1 | Medir primero: arnés en el repo (métricas del grafo, gold edges, relevancia de contexto, latencias, costo de indexado); casos de ACME fuera del repo |
| DD-2 | Tablas nuevas `symbols` y `edges` (una fila por ocurrencia), sin PK/UNIQUE; `graph_edges` se sigue escribiendo una release |
| DD-3 | Id de símbolo `fnv1a64(repo, file, kind_class, qualified, disambiguator)`, **sin rama**, `UBIGINT` |
| DD-4 | Chunk ↔ símbolo por rango de líneas, sin tabla de enlace; `vectors` no cambia |
| DD-5 | Patrones en `languages/*.json` (estilo `tags.scm`), semántica en Rust con un `LangResolver` por lenguaje |
| DD-6 | Dos fases: hechos por archivo + link pass por rama (incremental, con pase completo bajo umbral) |
| DD-7 | Resolución por reglas ordenadas (self → scope → campo → inyección → tipo por import → mismo archivo → nombre único → ambiguo → externo); fluent no tipable se descarta |
| DD-8 | `confidence` high/medium/low + `resolution` (el porqué), auditables contra gold edges |
| DD-9 | `external` solo con evidencia; `from_test` = `path_kind`; listas de plataforma en el JSON |
| DD-10 | Los lookups por nombre pasan a `symbols` con los mismos contratos JSON; absorbe pendientes 1 y 2 de PLAN-008 |
| DD-11 | `impact` por niveles (una consulta por profundidad), tope antes de computar, filtros por defecto |
| DD-12 | PageRank global al indexar (pesos por kind/confianza, `sqrt(n)`, tests × 0.1) y personalizado al consultar, implementación propia |
| DD-13 | Centralidad = tercera lista ponderada de la RRF, solo en `Hybrid`/`build_context`; `Vector` intacto; el anclaje sigue ganando |
| DD-14 | Tool `repo_map` estilo Aider: firmas bajo presupuesto con PageRank personalizado |
| DD-15 | Ego-graph de 1 salto en `build_context` como firmas, ≤ 20 % del presupuesto |
| DD-16 | Tool `traverse` sobre los 7 tipos de arista, con dirección, profundidad, límite y omitidos |
| DD-17 | Tool `skeleton` por archivo desde `symbols` |
| DD-18 | Selección de miembro de grupo: `/symbol` sobre `symbols`; la fórmula solo cambia si el arnés lo justifica |
| DD-19 | Rollout por `EXTRACTOR_VERSION` 1 → 2: aviso de índice viejo, camino viejo en ramas sin reindexar, tools nuevas piden `index --full` |
| DD-20 | Reindex barato: reusar vectores por `content_hash` del chunk (mismo modelo) |
| DD-21 | Presupuesto de rendimiento (tiempo, tamaño, RAM, latencias) como criterio, no como deseo |

## 4. Fuera de alcance

- **Ingestión SCIP** (`devctx graph import-scip`, autodetección de `index.scip`): plan propio
  después. El schema deja `edge_source` previsto.
- **stack-graphs** (archivado, RUSTSEC-2026-0303), **LSP on demand**, Kythe, universal-ctags.
- Inferencia de tipos real: genéricos más allá de quitar argumentos y for-each, overloads por tipo
  de argumento, dispatch dinámico, tipos de retorno encadenados de más de un nivel.
- Kotlin y lenguajes en raw text (`lang.rs`, `raw_text_language`): siguen sin grafo.
- Señales de git (co-change, blame por símbolo), `tests_for(symbol)`, resúmenes en lenguaje natural
  por símbolo (evidencia en contra: 4/45 vs 27/45, arXiv 2607.09691).
- Seguir renombres/movimientos de símbolos entre commits (el id cambia; DD-3).
- Re-ligar memorias por id de símbolo (`memory_symbol_references` sigue por nombre/archivo).
- Borrar `graph_edges` y el camino de lectura viejo: plan siguiente (DD-2, DD-19).
- Dependencias nuevas de runtime pesadas (grafos, solvers, JVM/Node): el binario sigue siendo uno.
- Escribir en `/home/you/acme` más allá de lo que TASK-001 y TASK-016 piden con aprobación
  (archivo de casos privado, copias de DB en el scratchpad, reindex con aprobación).

### Pendientes post-009

- **`project.path` absoluto en `.devctx/config.yaml`.** Copiar un repo junto con su `.devctx/`
  (un sandbox, un clon por `cp -r`) deja la copia apuntando al repo **original**: `devctx index`
  en la copia indexa y escribe el índice del original (pasó en la medición de TASK-004, riesgo
  abierto (2)). Opciones: resolver `project.path` relativo a la ubicación del config (o
  derivarlo de ella y dejar el campo solo como dato), o avisar —y no indexar sin confirmación—
  cuando no coincide con el cwd / la raíz git donde se corre. No entra en PLAN-009.

## 5. Tasks y orden

| Task | Qué | Especialista | Depende de | Estado |
|------|-----|--------------|------------|--------|
| TASK-001 | Arnés de evaluación del grafo y del contexto + línea base sobre 0.9.0 | general-purpose (Rust) | — | `done` |
| TASK-002 | Reindex barato: reusar vectores por `content_hash` del chunk | general-purpose (Rust) | — | `done` |
| TASK-003 | Schema `symbols` + `edges`, ids estables, escritura por lotes y mantenimiento por archivo/rama | general-purpose (Rust) | TASK-001 | `done` |
| TASK-004 | Extracción estructurada por lenguaje: definiciones, imports, herencia, instanciación, `contains`, firmas | general-purpose (Rust) | TASK-003 | `done` |
| TASK-005 | Motor de resolución y link pass + resolver Java (scopes, campos, inyección, `var`, imports, fluent) | general-purpose (Rust) | TASK-004 | `done` |
| TASK-006 | Resolver TypeScript/JavaScript (imports relativos y alias, DI, arrow-const) | general-purpose (Rust) | TASK-005 | `done` |
| TASK-007 | Resolvers Python, Rust y Go | general-purpose (Rust) | TASK-005 | `done` |
| TASK-008 | Lookups por símbolo: `read_symbol`, anclaje, `get_references`, `external`, sugerencias, vista web; pendientes 1 y 2 de PLAN-008 | general-purpose (Rust) | TASK-005 | `done` |
| TASK-009 | `impact_analysis` por lotes, con tope, confianza y marcas | general-purpose (Rust) | TASK-008 | `done` |
| TASK-010 | PageRank global al indexar y centralidad como señal en `search` híbrido | general-purpose (Rust) | TASK-006, TASK-007, TASK-008 | `done` |
| TASK-011 | Tool `repo_map` con PageRank personalizado bajo presupuesto | general-purpose (Rust) | TASK-010 | `pending` |
| TASK-012 | Ego-graph en `build_context` y evaluación de la selección de grupo | general-purpose (Rust) | TASK-009, TASK-010 | `pending` |
| TASK-013 | Tool `traverse` | general-purpose (Rust) | TASK-009, TASK-017 | `pending` |
| TASK-014 | Tool `skeleton` por archivo | general-purpose (Rust) | TASK-004 | `pending` |
| TASK-015 | Docs EN + ES y guía de reindex | general-purpose (Rust) | TASK-002, TASK-011, TASK-012, TASK-013, TASK-014 | `pending` |
| TASK-016 | Verificación de campo contra §6 | — (orquestador, con aprobación del usuario) | TASK-015 | `pending` |
| TASK-017 | Dispatch por interfaz en `impact_analysis` (y `traverse`): override-equivalentes vía `implements`/`inherits`, `medium`, `via: "dispatch"` | general-purpose (Rust) | TASK-009 | `pending` |

(La columna `Estado` va última: el parser de `plan_status` lee la columna con encabezado `Estado`.)

**Paralelizables:**
- **TASK-001 y TASK-002 arrancan juntas**: una solo agrega `scripts/graph-eval/` y mide con el
  binario 0.9.0 instalado; la otra toca `index_file`/`embed` en `pipeline.rs`. TASK-002 es
  independiente del resto del plan y conviene tenerla antes del primer reindex de desarrollo.
- **TASK-003 no se mergea hasta que TASK-001 cerró la línea base** (la base es de 0.9.0, no de la
  rama). Se puede desarrollar antes.
- Tras TASK-005: **TASK-006, TASK-007 y TASK-008 en paralelo** (resolvers en `devctx-parse` vs
  lecturas en `devctx-store`/`devctx-mcp`). TASK-014 puede ir en cualquier momento tras TASK-004.
- Tras TASK-009: **TASK-013 en paralelo con TASK-010/011**.
- **Compuerta de ruido**: TASK-010 (PageRank) no arranca hasta que las métricas de TASK-005…008
  cumplan las filas de "grafo limpio" de §6 (basura 0, externos y tests marcados, precisión de gold
  edges ≥ meta). Un grafo ruidoso vuelve contraproducente a PageRank.

**Esquema de ejecución (igual que PLAN-008 y PLAN-010):** una sesión ejecuta en la rama, otra revisa
cada lote en modo solo lectura antes de seguir. Compilar y correr tests queda autorizado solo cuando
el usuario apruebe el plan. Cada fix lleva primero un test que falla sin él. Gate local verde por
lote (`cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo test --all --locked`). **Antes de taggear, cross-check obligatorio** de Windows y macOS
empujando una rama `fix/**` que dispare el job `cross-check` de `.github/workflows/rust.yml`
(lección de v0.8.3, que quedó como tag sin release).

**Coordinación con PLAN-010:** su TASK-001 (instrumentación de memoria y `scripts/memprobe.sh`) corre
en paralelo en otra rama (`feat/plan-010-medicion`); este plan usa `memprobe.sh` si ya está en `main`
cuando TASK-001/005/016 midan, y si no, `/usr/bin/time -v` + `/proc/<pid>/status`. Zonas de
conflicto: `devctx-store` (`schema.rs`, `store.rs`), `pipeline.rs`, `state.rs` (`status`). El resto
de PLAN-010 puede recortarse; si entra, rebasea sobre lo que este plan haya mergeado.

## 6. Criterios de aceptación globales (medidos en TASK-016)

"Antes" = inventario 0.8.2 / PLAN-008 TASK-016; **TASK-001 re-mide sobre 0.9.0** y completa la
columna. Repos de medición: backend-a (Java grande), backend-b (Java,
234 archivos, rápido de reindexar), frontend (TS), legacy-migration (Python) y
DevCtxEngine (Rust).

| Criterio | Antes | Meta |
|---|---|---|
| Tipos de arista en el índice | 1 (`calls`; confirmado en los 5 repos medidos con 0.9.0) | ≥ 6: `calls`, `instantiates`, `imports`, `inherits`/`implements`, `contains`, `references` |
| Nodos basura (`var.*`, constantes como tipo `LOG.*`, targets con `(`/espacios/saltos) | 481 + 1 272 + 0 (backend-a; idéntico en 0.9.0). Otros repos: backend-b 114 + 68 + 0, FrontEnd 0 + 0 + **135**, legacy-migration 0, DevCtxEngine 0 + 0 + 2 | **0** |
| `calls` guardadas sin `dst_id` ni marca `external` (no decididas) | 65 % sin calificar, 0 % ligadas (BackEnd 65.3 %; backend-b 58.7 %, FrontEnd 69.9 %, Legacy 89.3 %, DevCtxEngine 94.0 %; 0 % con `dst_id`, la columna no existe) | ≤ 15 % de las `calls` guardadas |
| Aristas sin definición en el repo con marca `external` (o descartadas) | 0 % marcadas (el esquema no tiene la marca; cota por nombre: 30.3 % de las aristas de BackEnd son "internas", 69.7 % de afuera) | 100 % de las que tienen evidencia (DD-9); el resto `low` |
| Aristas desde tests marcadas `from_test` | 0 % marcadas (no hay columna); desde tests: BackEnd 57.4 %, backend-b 44.4 %, DevCtxEngine 14.7 %, FrontEnd 6.5 %, Legacy 12.1 % | 100 % (= `path_kind` Test) |
| Precisión de resolución sobre gold edges (40 sitios) | **42 sitios, 0.9.0: correcto 19/42 = 45.2 %** (internos 12/32 = 37.5 %); sobre los decididos (calificados) 19/22 = 86.4 %; cobertura (calificado) 22/42 = 52 %; no hay `high`/`low` que separar | ≥ 90 % en `high`, ≥ 75 % global; cobertura (no `low`) ≥ 80 % |
| Ocurrencias repetidas en un método | colapsadas en 1 (confirmado: la 2.ª `log.debug` de `migrate_one` no tiene arista) | una fila por ocurrencia (test Python de `get_references`) |
| `DraftResource.java:310` (en <commit-b> la llamada está en **:304**) | `AlphaService.findPaginated` (confirmado en 0.9.0; también :363, que es de `GammaService`) | `BetaService.findPaginated`, `high`, `resolution = field` |
| `read_symbol("tokenInterceptor")` (frontend) | `[]` (0.9.0: `definitions: []` **y** `external: true`, falso; en <commit-c> la definición está en el mismo archivo) | definición en el archivo del interceptor de la librería de auth |
| `impact("get")` / `impact("map")` en backend-a | 3.9 s / 5.5 s (modelo), 1 883 / 2 569 upstream. **0.9.0 medido** (p50 / p95, serve caliente, máquina con carga 50-125): get 10.9 / 16.6 s, map 14.0 / 16.8 s; upstream 1 884 / 2 569 | p95 ≤ 300 ms; por defecto sin tests/externos, con `omitted` |
| `external` con nombre calificado (`serde_json::from_str`, `Panache.withTransaction`) | `suggestions: []` (B2, candidato de PLAN-008). **0.9.0 final medido: `external: true` + `called_from` (60 y 9)**; `tokio::spawn` sin llamadas: `suggestions: []` | `external: true` + `called_from` |
| `Foo::new` inexistente con un `new` hoja en el repo | `external: true` (falso) en el candidato; **0.9.0 final medido: `{definitions: [], suggestions: []}`, sin `external`** | no externo; `suggestions` |
| `memories_by_symbol("Foo.Bar::baz")` | parte como archivo `Foo.Bar` (de PLAN-008; no re-medido: es una tool de memoria del serve vivo, fuera del sandbox) | símbolo calificado, sin archivo |
| `repo_map(budget=1024)` | no existe (confirmado: ni CLI ni tools MCP de 0.9.0) | tokens ≤ 1024 × 1.15; archivo esperado presente en ≥ 70 % de los casos del arnés con foco |
| Relevancia de `build_context` (archivo/símbolo esperado en el brief, set del arnés) | **0.9.0: archivo en el brief 24/30 = 80.0 %; símbolo 17/24 = 70.8 %** (30 casos; ver Resultado de TASK-001) | ≥ base + 10 pp, y ningún caso pasa de acierto a fallo sin explicación en el Resultado |
| `search --hybrid` (Hit@5 y MRR del arnés) | **0.9.0 híbrido: Hit@5 21/30, Hit@10 22/30, MRR 0.619; vectorial: 22/30, 22/30, 0.552** (ruido entre corridas ≈ ±1 posición, ±0.01 de MRR) | ≥ base (sin regresión > 2 % relativo); `--mode vector` idéntico a 0.9.0 |
| Contratos JSON de `search`, `read_symbol`, `get_references`, `impact_analysis`, `build_context` | 0.9.0 | solo cambios aditivos (`sym`, `confidence`, `via`…); tests de `mcp_tools` verdes |
| `index --full` desde cero | DevCtxEngine 276 archivos / 4 796 chunks: 2 393 s; backend-b 216 / 3 253: 2 489 s; Legacy 62 / 312: 572 s — **con la máquina a carga 50-125 y 3-5 índices a la vez: NO comparables**; BackEnd y FrontEnd no reindexados (ver Resultado) | ≤ +10 % |
| `index --full` tras subir `EXTRACTOR_VERSION` (con TASK-002) | = desde cero | ≤ 20 % del tiempo de 0.9.0 desde cero |
| Tamaño del DB | DevCtxEngine 28.1 MB, backend-b 21.0 MB, Legacy 6.8 MB, BackEnd 622 MB (1 623 archivos, 19 306 chunks), FrontEnd 2.34 GB (2 ramas indexadas) | ≤ +30 % |
| Pico de RAM al indexar (`VmHWM`, frontend) | **FrontEnd no medido** (> 25 min con la máquina cargada según PLAN-008 TASK-016; aquí la carga de 50-125 lo hizo inviable). Medido: DevCtxEngine 840 MiB, backend-b 727 MiB, Legacy 826 MiB | ≤ +200 MB |
| Índice viejo tras actualizar | — (0.9.0 ya avisa: `extractor_stale: true` + `hint` en `status`, y `warning` en `symbol`) | `extractor_stale: true` + aviso en `index_status` y en tools de grafo; tools nuevas piden `index --full` |

## 7. Riesgos

- **Tiempo y tamaño de indexado.** Una fila por ocurrencia, `symbols` y el link pass suman.
  Mitigación: DD-21 como criterio de cada task; escritura por lotes (DD-2); link pass incremental
  (DD-6); TASK-002 hace que el reindex que exige el plan no pague el embedding de nuevo.
- **Memoria.** El `RepoIndex` del link pass y la adyacencia cacheada para PageRank personalizado
  viven en el serve. Mitigación: medidos con el método de PLAN-010 (TASK-005, TASK-011); la caché se
  suelta con la misma política de inactividad que los modelos (`release_idle_models`,
  `state.rs`, PLAN-010 H3). Si PLAN-010 baja los techos de DuckDB, se re-mide.
- **Regresión de contratos de tools existentes** (anclaje, `external`, `suggestions`, selección de
  grupo, orden de `search`). Mitigación: los tests de PLAN-008 que fijan esos contratos se corren
  sin modificar; DD-13 deja `Vector` intacto; DD-18 no cambia la fórmula sin evidencia.
- **Índices ART de DuckDB** que devolvieron 0 filas sobre filas existentes (`schema.rs:163-168`,
  `:211-219`). Mitigación: sin PK/UNIQUE en las tablas nuevas; índices solo medidos y con test.
- **Precisión menor que la de un compilador.** Se acepta y se expone con `confidence`; el gold set
  mide cuánto. SCIP es la salida para quien la necesite (plan propio).
- **PageRank sobre un grafo todavía ruidoso** promueve helpers genéricos. Mitigación: compuerta de
  ruido antes de TASK-010; peso de la centralidad calibrado con el arnés; solo en `Hybrid`.
- **Reindex de 13 repos de acme.** Mitigación: TASK-002; el aviso es por rama y no rompe nada
  mientras tanto (DD-19, camino viejo).
- **Dos sesiones y PLAN-010 tocando `devctx-store`/`state.rs`/`pipeline.rs`.** Mitigación:
  conflictos de zona declarados en §5; rebase por lote; revisión solo lectura de la otra sesión.
- **Casos de ACME en un repo público.** Mitigación: DD-1 (casos fuera del repo, `$DEVCTX_EVAL_CASES`).

## 8. Verificación

- Gate por lote (§5). Tests clave (detalle en cada TASK):
  1. fixtures por lenguaje con el grafo esperado completo (símbolos, aristas, `confidence`,
     `resolution`) — TASK-004…007;
  2. `DraftResource`-like (tres clases internas con el mismo campo) y `var`/`LOG` — TASK-005;
  3. Python: dos llamadas desde el mismo método y llamada a nivel de módulo — TASK-007/TASK-008;
  4. contratos de PLAN-008 (anclaje, `external`, `suggestions`, `split_file_symbol`) más los casos
     nuevos de H9 — TASK-008;
  5. `impact` no hace consultas por nodo (contador de consultas en el `Store` de test) y respeta el
     tope — TASK-009;
  6. PageRank: grafo chico con ranking esperado, determinismo — TASK-010;
  7. `repo_map` nunca supera presupuesto × 1.15 — TASK-011;
  8. reindex con extractor cambiado reusa vectores (contador de textos embebidos = 0 para chunks
     iguales) — TASK-002.
- Arnés de TASK-001 corrido al cierre de TASK-005, 008, 010, 012 y 016, con la tabla en el Resultado.
- `devctx plan-status PLAN-009` desde la raíz de este repo: 0 warnings (verificado al escribir).

Nada de esto se compiló ni se testeó: es un plan.

## 9. Rollback

Cada task es un commit convencional propio. Puntos sensibles:
- **TASK-002** solo evita embeber; revertirla vuelve a embeber todo, sin tocar datos.
- **TASK-003** crea tablas nuevas y sube `EXTRACTOR_VERSION`. Revertir el binario deja `symbols` y
  `edges` huérfanas e inocuas; `graph_edges` sigue al día (DD-2); el fingerprint distinto pide
  reindex, que es lo honesto. No se migran ni reescriben datos existentes.
- **TASK-005…008** cambian lo que se escribe y se lee; mientras exista el camino viejo (DD-19) una
  reversión parcial cae a él.
- **TASK-010…014** son señales/tools nuevas: `Vector` no cambia, la centralidad tiene peso
  configurable (0 la apaga), el ego-graph tiene `related: false`, las tools nuevas son aditivas.
- Ninguna task borra `graph_edges` ni datos de memorias.

## 10. Release

- `0.10.0` si PLAN-010 queda reducido a su TASK-001 (o sale después); `0.11.0` si PLAN-010 sale
  primero como `0.10.0`. Es minor en cualquier caso: tools nuevas, señales que cambian resultados,
  schema nuevo y reindex pedido.
- Un solo release al cerrar TASK-016. No se recomienda un release intermedio con el schema nuevo y
  sin las tools: obliga a reindexar dos veces a los 13 repos.
- Notas del release: el aviso de reindex, el costo esperado con TASK-002 y cómo volver (DD-19).
- **Merge a `main`: squash** (decisión del usuario, 2026-10-08). La rama de trabajo
  `fix/plan-009-grafo` conserva la historia por task y sus revisiones; a `main` entra un solo
  commit, así la historia de `main` no arrastra texto que se saneó hacia adelante en la rama.

## 11. Contrato de resultado (lo que cada task reporta al cerrar)

Cada TASK llena su `## Resultado` con:

1. **Estado final** (`done` / `blocked` / `skipped`) y una línea de por qué si no es `done`.
2. **Números antes/después** con el arnés de TASK-001 (o "no medible en esta task" con el motivo),
   incluido el costo: tiempo de indexado, tamaño del DB, RAM cuando aplique.
3. **Hallazgos de §2 confirmados o corregidos** (sobre todo H5 `Cached<T>` y las estimaciones de
   DD-2/DD-6).
4. **Archivos tocados** y símbolos públicos nuevos o cambiados (firma); claves nuevas de
   `languages/*.json` y de config.
5. **Tests**: nombres, comando con que se corrieron y resultado.
6. **Contrato JSON**: campos agregados/cambiados por tool.
7. **Lo que NO se verificó**, explícito (macOS/Windows solo compilado, repos no reindexados…).

## 12. Preguntas abiertas (para el usuario)

**Respondidas por el usuario el 2026-10-06** (todas según la recomendación):
- Q-1 → sale como **`0.10.0`** (la 0.9.0 ya existe por PLAN-008 P1; el número de plan no es el de versión).
- Q-2 → filtros por defecto **como se propone**.
- Q-3 → **se mantiene** TASK-002.
- Q-4 → ego-graph **por defecto solo si el arnés lo justifica**; si no, apagado.
- Q-5 → casos de ACME **fuera del repo** (`/home/you/acme/scripts/devctx-eval/`, `$DEVCTX_EVAL_CASES`).
- Q-6 → Go **básico con fixtures**.

**Decidido por el usuario el 2026-10-08:**
- **Dispatch por interfaz → TASK-017**, antes de TASK-013. Propuesta de la revisión de TASK-009:
  `impact("ServiceImpl.update")` no veía a quien llama por `IService.update`, el falso negativo más
  caro en Java con DI. La semilla se expande a los override-equivalentes vía `implements`/`inherits`
  como `medium` con `via: "dispatch"`, incluido por defecto; `traverse` (TASK-013) pasa a depender
  de TASK-017.
- **Merge a `main` por squash** (§10).

Texto original de las preguntas:

- **Q-1 — Número de release.** `0.10.0` si PLAN-010 queda en su TASK-001; `0.11.0` si sale antes.
  *Recomendación:* decidir al cerrar TASK-016 según qué haya en `main`; no condiciona nada antes.
- **Q-2 — Defaults de filtros.** `impact`/`traverse` sin tests ni externos y con
  `min_confidence: medium` por defecto; `get_references` con todo pero marcado. *Recomendación:*
  así (DD-8/DD-11); `get_references` es "dónde se nombra" y esconder tests ahí sorprendería.
- **Q-3 — TASK-002 (reuso de vectores) dentro del plan.** *Recomendación:* sí; sin ella el rollout
  cuesta ~1 h por repo grande × 13 repos, y también acelera cada incremental.
- **Q-4 — Ego-graph activo por defecto en `build_context`.** *Recomendación:* sí, con tope de 20 %
  del presupuesto y `related: false` para apagarlo; se confirma con el arnés en TASK-012 y si no
  mejora queda apagado por defecto.
- **Q-5 — Dónde viven los casos de evaluación de ACME.** El repo es público. *Recomendación:* en
  `/home/you/acme/scripts/devctx-eval/` (fuera del repo, creado en TASK-001 con tu OK) y
  `$DEVCTX_EVAL_CASES` apuntando ahí; en el repo solo casos de DevCtxEngine y fixtures sintéticos.
- **Q-6 — Go.** No hay repo Go en tu workspace. *Recomendación:* resolver Go básico (receptores,
  imports con `go.mod`) con fixtures, sin invertir más hasta que haya un caso real.

## 13. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
- **Cerrado:**
- **Shipeó:**
- **Verificación:**
- **Quedó afuera:**
- **Continúa en:**
