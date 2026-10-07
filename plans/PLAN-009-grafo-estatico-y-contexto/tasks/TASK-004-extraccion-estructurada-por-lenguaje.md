# TASK-004 — Extracción estructurada por lenguaje: definiciones, imports, herencia, instanciación, `contains`, firmas

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-003
- **Estado:** `done`

---

## Objetivo

Que cada archivo produzca `FileFacts` completos: todos los símbolos que importan (incluidos
constructores, records, campos, arrow-consts de TS, `impl` de Rust, métodos con receptor de Go) con
su firma y paquete; imports estructurados; herencia/implementación; instanciaciones; usos de tipo; y
aristas `contains`, `imports`, `inherits`/`implements`, `instantiates` y `references` en `edges`.
Todavía sin resolución cross-file (eso es TASK-005). Implementa DD-5 y la parte de extracción de
DD-7.

## Contexto verificado

- `LangDef` (`crates/devctx-parse/src/registry.rs:35-63`): `symbols`, `calls`, `types` (opcional),
  `imports`, `function_kinds`, `container_kinds`. Validación de queries en
  `every_definition_compiles` (`registry.rs:182-198`) — hay que extenderlo a las claves nuevas.
- `LanguageParser` (`crates/devctx-parse/src/parser.rs:16-198`) compila una query por clave; kind =
  nombre de captura (l.149); `function` → `method` si hay contenedor (l.158-160).
- Queries actuales (`crates/devctx-parse/languages/*.json`):
  - Java: `class/interface/enum/method`; sin `constructor_declaration`, `record_declaration`,
    `field_declaration` como símbolo; `calls` solo `method_invocation`; `imports` el nodo entero.
  - TS/TSX: `function/class/interface/method`; sin arrow-const, `type_alias`, `enum`;
    `function_kinds` sin `arrow_function`.
  - Python: `function/class`. Rust: `function/struct/enum/trait/module`, sin `impl`, `const`,
    `type`. Go: `function/method/type`, `container_kinds: []`. JS: sin `types`.
- `TAGS_QUERY` de los crates de gramática (referencia, DD-5): Java captura
  `type_list`→`@reference.implementation`, `object_creation_expression`→`@reference.class`,
  `superclass`→`@reference.class`; TS `new_expression`, `type_annotation`→`@reference.type`;
  JS `method_definition` excluye `constructor`.
- El chunker usa `parsed.symbols` (`crates/devctx-chunk/src/chunker.rs:17-69`): nuevos kinds
  (`constructor`, `field`, `const`) cambian qué chunks salen. `is_callable` = `function|method`
  (l.202-204); decidir si `constructor` cuenta como callable (sí: es código que se busca).
- `container_name` usa el campo `name` o `type` del contenedor (`parser.rs:395-400`): con
  `impl<T: Clone> Cached<T>` (`crates/devctx-mcp/src/state.rs:212`) el contenedor se llamaría
  `Cached<T>` (a confirmar con test, master H5).

## Archivos

- **Crear:** `crates/devctx-parse/src/facts.rs` (`FileFacts`, `SymbolFact`, `RefFact`, `ImportFact`,
  `InheritFact`), `crates/devctx-parse/src/resolve/mod.rs` (trait `LangResolver` de DD-5, con la
  parte `file_facts`), fixtures `crates/devctx-parse/tests/fixtures/<lang>/…` y
  `crates/devctx-parse/tests/facts_<lang>.rs`.
- **Modificar:** `crates/devctx-parse/languages/{java,typescript,tsx,javascript,python,rust,go}.json`
  (claves de DD-5), `crates/devctx-parse/src/registry.rs` (`LangDef` con claves nuevas opcionales y
  su validación), `crates/devctx-parse/src/parser.rs`, `crates/devctx-parse/src/types.rs`
  (`ParsedFile.facts`), `crates/devctx-index/src/pipeline.rs` (escribir las aristas nuevas),
  `crates/devctx-chunk/src/chunker.rs` (kinds nuevos).

## Pasos

- [x] **Paso 1 — fixtures primero.** Por lenguaje, un archivo chico con lo que debe salir
      (símbolos con kind, calificado, padre, firma; imports estructurados; herencias; aristas
      locales esperadas). Java incluye constructor, record, campos `@Inject`, clase interna, `new`;
      TS incluye `export const x = () => {…}` con una llamada adentro, clase con constructor de
      parámetros-propiedad, `implements`; Rust `impl<T> Foo<T>`, `impl Trait for T`, `Foo::bar()`;
      Go método con receptor; Python clase con herencia y llamada a nivel de módulo. Los tests
      fallan.
- [x] **Paso 2 — claves JSON.** `definitions`, `references`, `inherits`, `imports` estructurado,
      `scopes`, `types` con `@init`, `platform_prefixes`/`builtins` (DD-9). Partir de los
      `TAGS_QUERY` de cada crate y extender. Las claves viejas (`symbols`, `calls`) se retiran del
      JSON cuando nada las lea.
- [x] **Paso 3 — firmas.** Texto desde el inicio de la definición hasta antes del cuerpo,
      espacios colapsados, tope 200 caracteres (DD-17). Rust: nombre de contenedor sin parámetros
      genéricos (`Cached`, no `Cached<T>`).
- [x] **Paso 4 — aristas estructurales.** `contains` (padre → hijo, incluido archivo → top level),
      `imports` (símbolo de archivo → import, `dst_name` = ruta/paquete importado),
      `inherits`/`implements` (tipo → supertipo por nombre), `instantiates` (`new T`),
      `references` (tipos de campos/params/variables). Todas con `dst_id` NULL hasta TASK-005.
- [x] **Paso 5 — compatibilidad.** `ParsedFile.symbols`/`edges` se siguen derivando para el chunker
      y `graph_edges`; revisar que el chunker con los kinds nuevos no duplique (un campo no es un
      chunk propio; un constructor sí).
- [x] **Paso 6 — medir** con el arnés: símbolos por kind, aristas por kind en backend-b
      y DevCtxEngine; tiempo de parse (fase de parse separada del embedding).

## Criterios de aceptación

- [x] Fixtures de los 7 lenguajes verdes; `every_definition_compiles` cubre todas las claves nuevas.
- [x] `export const tokenInterceptor = (…) => …` es un símbolo `function` y sus llamadas tienen
      `src` en él (fixture TS que lo reproduce).
- [x] Rust: `impl<T: Clone> Cached<T>` produce métodos `Cached.m`, nunca `Cached<T>.m` (test).
- [x] Go: `func (s *Server) Handle()` produce `Server.Handle` (test).
- [x] En backend-b quedan ≥ 6 kinds de arista con filas; anotado en el Resultado.
- [x] Fase de parse ≤ +30 % sobre 0.9.0 (DD-21), medida.

## Riesgos

- Queries más grandes = parse más lento: medir por lenguaje; si una query es cara, partirla.
- Cambiar qué símbolos salen cambia chunks y por lo tanto embeddings: TASK-002 limita el costo;
  revisar con el arnés que `search` no empeore por chunks nuevos (constructores, arrow-consts).

## Resultado

- **Estado final:** `done`.
- **Resumen:** cada archivo produce `FileFacts` (paquete, imports estructurados, supertipos,
  instanciaciones y usos de tipo) además de sus símbolos y llamadas, y el pipeline escribe en
  `edges` los 7 kinds: `calls`, `contains`, `imports`, `inherits`, `implements`, `instantiates`,
  `references`. Kinds de símbolo nuevos: `constructor`, `record`, `field`, `const`, `impl` (Rust),
  arrow-consts/`function` expressions de TS/JS asignadas a `const` y campos de clase con flecha,
  `enum`/`type` de TS, `struct`/`interface` de Go. Símbolos con `package`, firma completa (cabeza
  hasta el cuerpo, multilínea colapsada, tope 200; arrow hasta `=>`) y `exported`. Las claves viejas
  `symbols`/`calls` se retiraron del JSON (nada las lee). **`EXTRACTOR_VERSION` = 3.**
- **Números (sandbox `mktemp -d -p /var/tmp`, release, minilm-l6; copias de los índices de los
  sandbox de TASK-003, nunca los vivos):**

  | | backend-b antes (binario de TASK-003) | backend-b después | DevCtxEngine antes | DevCtxEngine después |
  |---|---|---|---|---|
  | kinds de arista con filas | 1 | **7** | 1 | **7** |
  | `calls` | 12 332 | 12 332 | 22 493 | 22 493 |
  | `references` / `contains` / `imports` | — | 4 778 / 2 845 / 1 617 | — | 6 108 / 4 283 / 1 070 |
  | `instantiates` / `inherits` / `implements` | — | 462 / 33 / 8 | — | 522 / 7 / 66 |
  | símbolos (sin `file`) | 1 385 | 2 845 (+1 417 `field`, 21 `constructor`, 14 `const`, 7 `record`, +1 `interface`) | 2 984 | 4 283 (+873 `field`, 244 `const`, 163 `impl`, 19 `type`; `function`/`method`/`struct`… iguales) |
  | destinos basura (`(`, espacio, salto) | 0 | 0 | 5 (`CudaFallback<M, E>.…`, binario previo al fixup de TASK-003) | **0** |
  | aristas huérfanas / ids duplicados | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
  | chunks reusados al `--full` | — | 3 204 de 3 284 (80 nuevos/cambiados: constructores, records, listas `# methods`) | — | 4 869 de 4 933 con el mismo `content_hash` (98,7 %) |
  | `index --full` | — | 24,8 s (sobre el índice viejo, reuso) | — | 246 s (sin reuso: la copia cambió de `repo_path`; equivale a desde cero) |
  | bloques usados/libres | 79 / 0 | 112 / 66 | — | — |

  Fase de parse (DD-21, `examples/parse_bench`, `parse()` por archivo como el indexador, 7 corridas,
  máquina con carga 4-7; 0.9.0 = `git archive fdc7dd2`): backend-b (154 archivos Java) 0.9.0 mejor
  1,04-1,43 s → **0,91-0,93 s**; DevCtxEngine (120 .rs) 2,68-4,30 s → **2,61-3,07 s**. Sin la caché
  de parsers el primer corte medía **+83 %** en Java (las 6 queries se compilaban por archivo): por
  eso `devctx_parse::parse` guarda un `LanguageParser` por lenguaje y hilo. Criterio ≤ +30 %: cumplido
  (más rápido que 0.9.0). Relevancia (arnés, casos de DevCtxEngine, sobre el mismo commit, antes =
  índice de TASK-003 y después = índice limpio del binario nuevo): **posiciones idénticas** en los 10
  casos, vectorial e híbrido. El brief salió vacío en ambos lados en el sandbox (mismo resultado
  antes y después; no se investigó).
- **Hallazgos de §2:** H5 `Cached<T>` confirmado y cerrado desde TASK-003; ahora con
  `impl<T: Clone> Cached<T>` en fixture (`Cached.new`, fuente `Cached.new` en `graph_edges`). H4: las
  llamadas de un arrow-const tienen fuente (`tokenInterceptor`); H5 Rust `Foo::bar` → `Foo.bar`,
  `Self::m` → tipo del impl, `std::fs::read` → `std::fs::read`; Go `Server.Handle` también como fuente
  de `graph_edges`.
- **Archivos tocados:** `crates/devctx-parse/src/{facts.rs (nuevo), resolve/mod.rs (nuevo),
  parser.rs, registry.rs, lang.rs, lib.rs, symbol_id.rs, types.rs}`,
  `crates/devctx-parse/languages/*.json` (7), `crates/devctx-parse/tests/{common/mod.rs,
  facts_{java,typescript,tsx,javascript,python,rust,go}.rs, extractor_golden.rs,
  golden/extractor.txt, fixtures/<lang>/…}`, `crates/devctx-parse/examples/parse_bench.rs`,
  `crates/devctx-chunk/src/chunker.rs`, `crates/devctx-index/src/{pipeline.rs, lib.rs}`,
  `crates/devctx-store/src/{symbols.rs, state.rs, memory_refs.rs}`, `scripts/graph-eval/README.md`,
  `CHANGELOG.md`. Públicos nuevos: `FileFacts`, `ImportFact`, `InheritFact`, `RefFact` y las
  constantes de kind (`facts`); `LangResolver`, `resolver_for` (`resolve`); `ParsedFile.facts`,
  `Symbol.exported`; `Lang::{definitions_query, references_query, inherits_query, package_query,
  type_names}` (se retiran `symbol_query`/`calls_query`). Claves JSON nuevas: `definitions`,
  `references`, `inherits`, `package`, `type_names`, `platform_prefixes`, `builtins` (se retiran
  `symbols` y `calls`); `imports` con capturas estructuradas.
- **Verificado por:** `facts_java` (constructor, record, `@Inject`, clase interna, `new`, clase
  anónima con forma `sf()`), `facts_typescript` (`an_exported_arrow_const_is_a_function_and_the_source_of_its_calls`,
  propiedades de parámetro, `implements`, imports con alias/default/namespace/side-effect),
  `facts_tsx`, `facts_javascript`, `facts_python` (bases, llamada a nivel de módulo, imports
  relativos), `facts_rust` (`a_bounded_generic_impl_names_its_methods_after_the_bare_type`, `impl
  Trait for`, path calls, árboles `use`), `facts_go` (`a_method_with_a_receiver_is_qualified_by_its_type`,
  embebido, imports agrupados con su línea), `resolve::tests`, `the_graph_languages_are_the_parsed_ones`,
  `every_definition_compiles` (compila todas las claves, rechaza capturas desconocidas y node kinds
  inexistentes), `extractor_golden`; chunker `constructors_are_chunks_and_fields_are_not`; index
  `every_relation_of_a_file_is_an_edge` (los 7 kinds, `contains` resuelto, `graph_edges` solo
  `calls`) y `copying_a_file_leaves_no_orphans_or_duplicates` (la copia conserva `contains`). Los
  tests nuevos de `devctx-parse` no compilan contra el padre (API nueva); los de comportamiento
  ya existentes se actualizaron donde cambió la semántica (`impl` como padre de sus métodos, Go
  `struct`, conteos con `contains`). Gate: `cargo fmt --check`; `cargo clippy --workspace
  --all-targets -D warnings` (y `--features gpu`); `cargo test` por paquete (los 15) con
  `TMPDIR=/var/tmp`: verde. Guardia de identificadores privados: vacía.
- **Contrato JSON:** sin cambios en las tools (siguen leyendo `graph_edges`/`vectors`); cambia el
  contenido de `graph_edges` (fuentes de arrow-consts y métodos Go, destinos de path calls de Rust).
- **Agregados de la revisión (en esta task):**
  - *Versión de la salida:* `EXTRACTOR_VERSION` 2 → 3 y golden obligatorio
    (`tests/extractor_golden.rs` + `tests/golden/extractor.txt` con `version 3` en la primera línea);
    se descartó hashear el código del extractor (churn de `--full` ante refactors). Anotado en DD-3.
    **Los índices de desarrollo y los sandbox de medición hechos antes (incluidos los de TASK-003 y
    los de cd7d2a2, con ids Java viejos tras db0a0ea) necesitan `index --full`.**
  - *NIT autocommit:* `save_file_state`, `delete_file_state`, `save_index_record`,
    `replace_file_symbols`, `delete_file_graph` y `drop_branch` corren en una transacción (la del
    llamador o la suya) con la invalidación adentro, así su COMMIT vuelve a invalidar también fuera
    de una transacción (antes `delete_file_graph`/`drop_branch` invalidaban fuera de la suya y no
    re-invalidaban). Test `an_autocommit_writer_invalidates_after_its_write_too` (exige dos
    invalidaciones por escritura; en el padre hay una).
- **Desviaciones:** `SymbolFact` no existe como tipo aparte (los símbolos siguen siendo `Symbol`,
  que lee el chunker); el paquete va en `symbols.package` y no en `qualified` (DD-3); `contains`
  se escribe ya resuelto (`dst_id`, `high`, `same_file`) y la copia entre ramas lo conserva: ambos
  extremos son del archivo y los ids no llevan rama (la task decía todas con `dst_id` NULL); `scopes`
  y `@init` quedan para TASK-005-007 (los consumen ellas); los decoradores TS siguen como `calls`
  (filtrarlos es de TASK-006); `exported` de Rust es "tiene `pub`" (métodos de un `impl Trait`
  quedan `false`); parámetros de tipo (`T`) no generan `references`, nombres de builtins sí (DD-9:
  las listas marcan `external`, no descartan).
- **Riesgos abiertos / siguiente:** (1) el sandbox de DevCtxEngine no reusó vectores por cambiar de
  `repo_path` (medida de reuso de ese repo por `content_hash`, no por corrida); (2) en la primera
  corrida de backend-b el `project.path` de la copia apuntaba al sandbox de TASK-003 y el índice se
  escribió allí: se restauró byte a byte desde la copia previa y la medición se hizo sobre el
  resultado copiado a este sandbox; (3) `references` a parámetros de tipo de un método genérico
  sin `type_parameters` en un ancestro (raro) siguen saliendo; (4) no verificado en macOS/Windows
  ni en backend-a/frontend; (5) Python sin `src/`-layout correcto en `package_from_path`.
