# TASK-003 — Schema `symbols` + `edges`, ids estables, escritura por lotes y mantenimiento

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-001 (para el merge: la línea base se mide con 0.9.0 antes)
- **Estado:** `done`

---

## Objetivo

Que el índice tenga una tabla de símbolos con identidad estable y una tabla de aristas tipadas con
una fila por ocurrencia, escritas en la misma transacción por archivo que hoy y mantenidas en
borrado, copia entre ramas, renombre y poda de rama. Con la extracción actual todavía (sin
resolución nueva), pero ya sin perder ocurrencias repetidas ni llamadas a nivel de módulo. Sube
`EXTRACTOR_VERSION` a 2. Implementa DD-2, DD-3, DD-4 y la parte de rollout de DD-19.

## Contexto verificado

- DDL relacional en `crates/devctx-store/src/schema.rs:109-271` (`RELATIONAL_DDL`), aplicado con
  `CREATE TABLE IF NOT EXISTS` por `init_schema` (l.12-25), que ya sabe hacer `CHECKPOINT` tras crear
  una tabla en un DB existente (l.15-22, patrón de `index_meta`).
- Advertencia ART: `schema.rs:163-168` y `:211-219` documentan índices que devolvieron 0 filas.
- Escritor actual: `Store::replace_file_edges` (`crates/devctx-store/src/graph.rs:64-96`) borra por
  archivo e inserta fila a fila con dedupe `(source,target,kind)` (l.75-79); `delete_file_edges`
  (l.99-105).
- Puntos de mantenimiento: `Indexer::index_file` (transacción, `crates/devctx-index/src/pipeline.rs:956-977`),
  `Indexer::delete_file` (`pipeline.rs:812-824`), `Store::copy_file_rows`
  (`crates/devctx-store/src/memory_refs.rs:333-419`, copia aristas en l.375-391),
  `Store::drop_branch` (`memory_refs.rs:427+`), `Store::rename_file` (`crates/devctx-store/src/store.rs:822-828`).
- `Symbol`/`ParsedFile` (`crates/devctx-parse/src/types.rs:4-65`) no tienen calificado, firma ni
  id; `parent` es un nombre. `qualified_source` arma `Clase.metodo` (`parser.rs:296-306`).
- Llamadas sin función envolvente se descartan (`parser.rs:125-127`).
- `EXTRACTOR_VERSION = 1` (`crates/devctx-parse/src/registry.rs:93`); FNV-1a en `registry.rs:96-100`;
  test `the_fingerprint_is_stable_between_calls` exige `v1-` (l.226-230) y hay que actualizarlo.
- Sello y aviso: `pipeline.rs:542-562`; `Store::extractor_stale`
  (`crates/devctx-store/src/state.rs:183-188`).

## Archivos

- **Crear:** `crates/devctx-store/src/symbols.rs` (tipos `StoredSymbol`, `StoredEdgeV2` o nombre
  equivalente; `replace_file_symbols`, `replace_file_graph`, lecturas básicas por archivo/id),
  `crates/devctx-parse/src/symbol_id.rs` (DD-3).
- **Modificar:** `crates/devctx-store/src/schema.rs` (DDL de DD-2), `crates/devctx-store/src/lib.rs`
  (exports), `crates/devctx-store/src/memory_refs.rs` (`copy_file_rows`, `drop_branch`),
  `crates/devctx-store/src/store.rs` (`rename_file`), `crates/devctx-index/src/pipeline.rs`
  (`index_file`, `delete_file`, `store_edges`), `crates/devctx-parse/src/types.rs` (campos nuevos:
  `qualified`, `id`, `parent_id`, `signature` provisoria = primera línea), `crates/devctx-parse/src/parser.rs`
  (símbolo de archivo, fuente de módulo), `crates/devctx-parse/src/registry.rs` (`EXTRACTOR_VERSION = 2`).

## Pasos

- [x] **Paso 1 — tests que fallan.** (a) dos llamadas al mismo destino desde el mismo método → dos
      filas en `edges`; (b) llamada a nivel de módulo en Python → fila con `src` = símbolo de
      archivo; (c) id estable: mismo símbolo en dos ramas → mismo id; mover el cuerpo dentro del
      archivo → mismo id; sobrecarga Java → ids distintos.
- [x] **Paso 2 — DDL.** `symbols` y `edges` según DD-2, sin PK/UNIQUE. Índices: ninguno al
      principio; medir `read_symbol`/`find_references` sobre la copia de backend-a y agregar solo
      los que hagan falta, cada uno con un test de igualdad que lo fije.
- [x] **Paso 3 — ids.** `symbol_id(repo, file, kind_class, qualified, disambiguator)` con FNV-1a 64
      (DD-3); colisión dentro de un archivo → ordinal. Expuesto en JSON como 16 hex.
- [x] **Paso 4 — símbolo de archivo y fuentes de módulo.** El parser emite un símbolo `file` por
      archivo y usa su id como `src` de las llamadas sin función envolvente (en vez de descartarlas).
- [x] **Paso 5 — escritura por lotes.** `replace_file_symbols` / `replace_file_graph` borran el
      archivo y insertan con `Appender` (o `INSERT … VALUES` por lotes), dentro de la transacción de
      `index_file`. `graph_edges` se sigue escribiendo como hoy (DD-2), derivado de lo mismo.
- [x] **Paso 6 — mantenimiento.** `delete_file`, `drop_branch`, `rename_file` (actualiza `file` y
      recalcula ids del archivo: el id lleva el archivo) y `copy_file_rows` (copia `symbols` tal cual
      y `edges` con `dst_id`/`confidence` en NULL para que los resuelva el link pass de TASK-005).
- [x] **Paso 7 — `EXTRACTOR_VERSION = 2`** y test del fingerprint actualizado. `index_status` sobre
      un índice v1 dice `extractor_stale: true` (ya lo hace el mecanismo de PLAN-008; agregar el test
      con un índice v1 sembrado).
- [x] **Paso 8 — medir** sobre backend-b y DevCtxEngine con el arnés: filas de
      `edges` vs `graph_edges`, tamaño del DB, tiempo de `index --full` (con TASK-002 si ya entró).

## Criterios de aceptación

- [x] Los tres tests del paso 1 pasan; los de `graph.rs` existentes siguen verdes (lectura vieja).
- [x] `index --full` deja `symbols` y `edges` poblados; `drop_branch`, `delete_file` y
      `copy_file_rows` no dejan filas huérfanas (test por cada uno).
- [x] Un 0.9.0 que abre el DB resultante no falla (tablas nuevas ignoradas, `graph_edges` válido):
      test de compatibilidad leyendo `graph_edges` con las consultas de 0.9.0.
- [x] Medido y anotado: crecimiento de filas (ocurrencias vs pares) y del DB; dentro de DD-21 o
      con la desviación explicada.

## Riesgos

- Volumen: una fila por ocurrencia puede duplicar las aristas; si el DB pasa +30 %, evaluar dejar
  `graph_edges` de escribir antes (Q del Resultado, no decisión unilateral).
- Índices ART: no agregar PK/UNIQUE; si un índice da resultados raros en test, sacarlo.
- `rename_file` cambia ids (el archivo es parte del id): documentarlo; memorias siguen por nombre.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`.
- **Resumen:** tablas `symbols` y `edges` (DD-2, sin PK/UNIQUE/índices) escritas por `Appender` en la
  misma transacción por archivo que vectors/`graph_edges`/routes/file_state; ids estables DD-3;
  símbolo `file` por archivo como fuente de llamadas sin función envolvente; una fila por
  ocurrencia; mantenimiento en `delete_file`, `drop_branch`, `copy_file_rows`, `rename_file`;
  `EXTRACTOR_VERSION = 2`. `graph_edges` sigue igual que en 0.9.0 (mismo contenido: solo llamadas
  con función nombrada, deduplicadas por par).
- **Id (DD-3, con una desviación de forma):** `id = fnv1a64("F"␟repo␟file) XOR
  fnv1a64("S"␟kind_class␟qualified␟disambiguator)` (`devctx_core::symbol_id`). Mismas entradas y
  misma estabilidad que el hash único de DD-3, y además: un renombre de archivo desplaza todos los
  ids del archivo por la misma máscara, así `rename_file` re-keya sin re-parsear (el
  disambiguator no se guarda y se cancela), y las colisiones dentro de un archivo no dependen de
  la ruta (el ordinal es el mismo antes y después del renombre). `qualified` = cadena completa de
  contenedores (`Outer.Inner.metodo`; provisorio, TASK-004 agrega paquete/módulo).
  `disambiguator` = tipos de parámetros normalizados en lenguajes con `"overloads": true`
  (java, typescript, tsx; clave nueva de `languages/*.json`), si no vacío; homónimos restantes →
  `#n` en orden de fuente; colisión de hash → `#n` también. Expuesto como 16 hex (`sym_hex`).
- **Hallazgo y fix fuera del plan (bloqueante):** abrir un índice 0.9.0 **con HNSW**
  (`storage.hnsw: true`, lo que escribe `init`) con este binario fallaba: el `CHECKPOINT` tras
  crear las tablas nuevas en `init_schema` necesita ligar el índice HNSW y VSS se cargaba
  *después* → "Cannot bind index 'vectors', unknown index type 'HNSW'", base invalidada (FATAL).
  Medido en el sandbox sobre un índice real. Fix: `Store::open`/`open_in_memory` cargan las
  extensiones antes de `init_schema`, y `init_schema` no hace el checkpoint si hay HNSW y VSS no
  está cargado (offline: el DDL queda en el WAL, como antes). El mismo patrón existía desde
  `index_meta` (0.8.3). Test `an_existing_hnsw_database_gains_the_tables` (falla con el orden
  viejo). El WAL que dejó la corrida fallida se reprodujo sin problema al reabrir con el fix.
- **Números (sandbox `mktemp -d -p /var/tmp`, release, minilm-l6, `index --full` directo,
  memprobe; padre = dc1aed3 compilado aparte; máquina con carga 30-56 durante casi todo):**

  | repo (archivos / chunks) | `graph_edges` | `edges` (ocurrencias) | ratio | `symbols` | pares con >1 ocurrencia | desde símbolo `file` | huérfanas |
  |---|---|---|---|---|---|---|---|
  | DevCtxEngine (281 / 4928) | 16 123 | 22 493 | **1,40×** | 3 083 | 3 596 | 120 | 0 |
  | backend-b (216 / 3253) | 7 765 | 12 332 | **1,59×** | 1 540 | 1 828 | 11 | 0 |

  Dentro de la estimación de DD-2 (1,3-2×). `from_test` 14,0 % / 37,7 % de las `calls`; basura
  heredada de la extracción actual (TASK-005 la ataca): backend-b `var.*` 502, `LOG.*` 78;
  DevCtxEngine 5 destinos-expresión.

  | repo | DB desde cero padre → nuevo | bloques usados (256 KiB) | `symbols`+`edges` | tiempo desde cero padre / nuevo | `--full` tras el bump (vectores reusados) | VmHWM |
  |---|---|---|---|---|---|---|
  | DevCtxEngine | 29,1 → 37,8 MB (**+29,7 %**) | 111 → 119 (+7,2 %) + 25 libres | 5 bloques | 278 s (carga 8) y 736 s (carga ~50) / 764 s (carga ~50) | 54,9 s = 19,7 % del desde cero con carga 8 | 453 → 452 MiB; bump 288 MiB |
  | backend-b | 19,7 → 20,7 MB (**+5,3 %**) | 75 → 79, 0 libres | 3 bloques | 442 s / 509 s (ambos con carga ~50) | 36,9 s = 8,3 % | 467 → 502 MiB; bump 241 MiB |

  Sobrecosto de escritura, comparación justa (mismo índice, reescritura completa con vectores
  reusados, alternando binarios en backend-b): padre 35,9 / 46,1 s, nuevo 41,6 / 33,3 s →
  **no distinguible del ruido** con esta carga. Los tiempos desde cero no son comparables entre
  sí (TASK-001 ya lo advirtió); el par con carga parecida en DevCtxEngine da +3,8 %.
  Tamaño: DD-21 (≤ +30 %) **cumplido, en el borde en DevCtxEngine**: las tablas nuevas ocupan
  5 bloques (~1,3 MB); el resto del salto son 25 bloques libres en el archivo (hipótesis no
  verificada: un auto-checkpoint del WAL a mitad de corrida reescribe bloques parciales; el padre
  terminó con 0 libres). Q para el usuario si molesta: compactar al final del `--full` o dejar de
  escribir `graph_edges` antes (riesgo previsto en esta task).
- **Downgrade verificado:** el binario padre (lectura de `graph.rs` idéntica a 0.9.0 salvo
  comentarios, `git diff fdc7dd2`) abre el DB nuevo y `impact replace_file_edges` da salida
  **idéntica** byte a byte a la del binario nuevo; las tablas nuevas se ignoran.
- **Índices:** ninguno. Las únicas consultas sobre las tablas nuevas hoy son el borrado/lectura
  por `(repo, branch, file)` del escritor (escaneo columnar; sin costo visible en la medición de
  arriba). `read_symbol`/`find_references` todavía no leen `symbols`/`edges` (TASK-008): la
  medición de índices sobre backend-a queda para esa task, con su test de igualdad.
- **Archivos tocados:** `crates/devctx-core/src/{lib.rs,symbol_id.rs}` (nuevo: `fnv1a64`,
  `file_part`, `symbol_part`, `symbol_id`, `file_symbol_id`, `rename_delta`, `kind_class`,
  `sym_hex`, `FILE_KIND`); `crates/devctx-parse/src/{symbol_id.rs (nuevo: ParsedFile::assign_ids),
  types.rs, parser.rs, lang.rs, registry.rs, lib.rs}`, `languages/{java,typescript,tsx}.json`
  (`"overloads": true`); `crates/devctx-store/src/{symbols.rs (nuevo), schema.rs, store.rs,
  memory_refs.rs, lib.rs}`; `crates/devctx-index/src/{pipeline.rs, lib.rs}`;
  `crates/devctx-cli/tests/serve_lifecycle.rs`; `CHANGELOG.md`.
  Públicos nuevos: `Symbol.{qualified, signature, params, id, parent_id}`,
  `GraphEdge.{byte, source_byte, src_id}`, `ParsedFile.{module_edges, file_symbol}`,
  `Lang::overloads`, `LangDef.overloads`; `StoredSymbol`, `StoredSymbolEdge`;
  `Store::{replace_file_symbols, replace_file_symbol_edges, replace_file_graph,
  delete_file_graph, file_symbols, symbol_by_id, file_symbol_edges, graph_row_counts}`.
  `Store::rename_file` ahora es transaccional y re-keya el grafo. `ParsedFile.symbols` y el texto de
  los chunks no cambian (el reuso de vectores de TASK-002 sigue: 4928/4928 y 3253/3253 reusados).
- **Verificado por:** tests nuevos — core: `the_id_is_a_pure_function_of_its_inputs`,
  `a_rename_shifts_every_id_by_one_mask`, `interchangeable_kinds_share_a_class`; parse:
  `moving_a_body_within_the_file_keeps_the_id`, `java_overloads_get_distinct_ids`,
  `homonyms_without_types_take_an_ordinal`,
  `parents_and_edge_sources_point_at_symbols_of_the_file`,
  `rust_impl_methods_hang_from_their_type`, `nested_containers_qualify_with_the_whole_chain`;
  store: `repeated_occurrences_are_rows_of_their_own`,
  `a_rewrite_replaces_only_its_own_file_and_branch`, `a_failed_transaction_keeps_the_old_rows`
  (el Appender respeta la transacción), `deleting_a_file_leaves_no_orphans`,
  `a_rename_rekeys_the_file_like_a_fresh_parse`,
  `an_existing_database_gains_the_tables_with_a_checkpoint`,
  `an_existing_hnsw_database_gains_the_tables`; index: `two_calls_from_one_method_are_two_edges`
  (a), `a_module_level_python_call_is_sourced_from_the_file_symbol` (b),
  `a_symbol_has_one_id_on_every_branch` (c), `copying_a_file_leaves_no_orphans_or_duplicates`,
  `deleting_a_file_drops_its_graph_rows`, `dropping_a_branch_drops_its_graph_rows`,
  `a_v1_index_reads_stale_until_a_full_run_fills_the_graph`, `graph_edges_stays_readable_by_0_9_0`;
  cli: `a_forced_exit_mid_index_leaves_a_sound_database` ahora exige que cada archivo conserve
  `[file, function]` en `symbols` tras el corte a mitad de escritura (el stall está entre vectores
  y grafo). Contra el padre no compilan (APIs nuevas); mutaciones comprobadas: quitar
  `module_edges` del escritor, `copy_file_graph`, `delete_file_graph` o el `DELETE FROM symbols` de
  `drop_branch` hace fallar 5 de los tests de índice; el orden viejo de extensiones hace fallar el
  de HNSW. Test del fingerprint: `v2-`. Gate: `cargo fmt --check`; `cargo clippy --workspace
  --all-targets --locked -D warnings` (y `--features gpu`); `cargo test --locked` por paquete
  (los 15) con `TMPDIR=/var/tmp`: todo verde, sin flakes observados. Guardia de identificadores
  privados: vacía.
- **Contrato JSON:** sin cambios en esta task (las tools siguen leyendo `graph_edges`/`vectors`).
- **Desviaciones:** composición XOR del id (arriba); `symbols.package`/`exported`/`rank`/
  `in_degree` y `edges.confidence`/`resolution`/`external`/`dst_id` quedan NULL (TASK-004/005/010);
  la fuente de una llamada dentro de una función que no es símbolo (constructor Java, función
  anónima) es el símbolo más interno que la contiene (la clase) o el de archivo; `rename_file` no
  toca `graph_edges.source_file` (no lo hacía antes; actualizar una columna con `UNIQUE` en DuckDB
  es arriesgado y el pipeline trata los renombres como borrar + indexar).
- **Riesgos abiertos / siguiente:** (1) tamaño en el borde en DevCtxEngine (bloques libres); (2)
  TASK-004 cambia `qualified` (paquete/módulo): los ids cambian, sin problema mientras no haya
  release, pero un índice v2 de desarrollo necesitará `--full` (cambiar lógica del extractor ⇒
  subir `EXTRACTOR_VERSION` otra vez si ya hubiera índices v2 que importen); (3) no verificado en
  macOS/Windows (solo Linux) ni con un `devctx` 0.9.0 instalado real (se usó el padre compilado,
  misma lectura); backend-a/frontend no reindexados.
- **Fixup (review):** hallazgos de la revisión de 30c3d51, cada uno con test que falla en el padre.
  - *MAJOR-1, ordinal inestable:* `qualified` solo tomaba ancestros de `container_kinds`, así que
    había homónimos reales (Rust `helper` arriba y en `mod tests`; `X.fmt` de `impl Display` e
    `impl Debug`; los `wrapper` de decoradores Python; métodos Go `String` de dos tipos; `run` de
    clases anónimas Java) y el `#n` en orden de fuente movía ids al insertar uno arriba. Ahora
    `qualifier_chain` (`parser.rs`) mete todo ancestro que es símbolo del archivo, todo contenedor,
    los `scope_kinds` nuevos del JSON (TS/TSX `internal_module`/`module`) y el receptor Go;
    `container_name` quita genéricos (`impl<T> Foo<T>` → `Foo`, arregla también `parent_of`);
    `Symbol.trait_of` (trait de `impl Trait for`, con sus genéricos) entra al disambiguator. El
    ordinal queda para redefiniciones. Tests: `inserting_a_homonym_above_keeps_existing_ids`
    (Rust/Python/Go/Java), `qualified_names_carry_every_enclosing_symbol`,
    `a_generic_impl_hangs_from_its_type`.
  - *MAJOR-2, re-key por XOR:* opción (a). `rename_file_graph` borra las filas de la ruta vieja y
    deja sin resolver las aristas de otros archivos que apuntaban a ellas; el reindex las recrea.
    Sin camino XOR, el id vuelve al hash único de DD-3 (`fnv1a64(repo␟file␟kind_class␟qualified␟
    disambiguator)`), con valor dorado fijado (`the_id_format_is_pinned`). Se quitan
    `file_part`/`symbol_part`/`rename_delta`. Test: `a_rename_drops_the_old_rows_for_the_reindex`
    (reemplaza `a_rename_rekeys_the_file_like_a_fresh_parse`). CHANGELOG corregido ("kept on
    rename" decía lo contrario de lo que pasa ahora).
  - *MINOR-1:* `"overloads"` fuera de typescript/tsx (`typescript_ids_ignore_parameter_types`).
    Java usa los parámetros **siempre**: hacerlo solo con homónimos le cambiaría el id a la
    sobrecarga original al agregar otra (el mismo defecto de MAJOR-1). Documentado en DD-3.
  - *MINOR-2:* `Store::extractor_stale` recibe ahora el `repo` corto y también da `true` si
    `graph_out_of_step` (archivo con símbolos sin fila `file`, o fila `file` sin `file_state`), así
    un downgrade a 0.9.0 con incrementales bajo el sello `v2` se reporta y el incremental no
    re-sella ni copia. Límite: archivos *modificados* por 0.9.0 no se detectan. Test:
    `a_graph_out_of_step_with_its_files_reads_as_stale`.
  - *MINOR-3:* `an_existing_hnsw_database_gains_the_tables` falla si VSS no carga, salvo
    `DEVCTX_TEST_ALLOW_NO_VSS=1` (patrón de `require_fts`); test nuevo
    `an_hnsw_database_without_vss_skips_the_checkpoint` (conexión sin autoload de extensiones).
    `Store::try_checkpoint`/`force_checkpoint` aplican `checkpoint_is_safe` y devuelven
    `StoreError::CheckpointUnsafe`; `checkpoint` lo traga como antes.
  - *NITs:* golden de `symbol_id`; la firma corta en el `body` (`the_signature_excludes_a_one_line_body`:
    `fn g() -> i32 { 1 }` → `fn g() -> i32`).
  - `EXTRACTOR_VERSION` sigue en 2 (nada publicado). Bloques libres / `graph_edges` / hotfix
    0.9.1: **decisión pendiente del usuario**, sin cambios de código; no se midió
    `PRAGMA database_size` tras un segundo `--full`.
  - Gate: `cargo fmt --check`; `cargo clippy --workspace --all-targets -D warnings` (y
    `--features gpu`); `cargo test` por paquete con `TMPDIR=/var/tmp`: verde. Guardia privada vacía.
