# TASK-004 — Extracción estructurada por lenguaje: definiciones, imports, herencia, instanciación, `contains`, firmas

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-003
- **Estado:** `pending`

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

- [ ] **Paso 1 — fixtures primero.** Por lenguaje, un archivo chico con lo que debe salir
      (símbolos con kind, calificado, padre, firma; imports estructurados; herencias; aristas
      locales esperadas). Java incluye constructor, record, campos `@Inject`, clase interna, `new`;
      TS incluye `export const x = () => {…}` con una llamada adentro, clase con constructor de
      parámetros-propiedad, `implements`; Rust `impl<T> Foo<T>`, `impl Trait for T`, `Foo::bar()`;
      Go método con receptor; Python clase con herencia y llamada a nivel de módulo. Los tests
      fallan.
- [ ] **Paso 2 — claves JSON.** `definitions`, `references`, `inherits`, `imports` estructurado,
      `scopes`, `types` con `@init`, `platform_prefixes`/`builtins` (DD-9). Partir de los
      `TAGS_QUERY` de cada crate y extender. Las claves viejas (`symbols`, `calls`) se retiran del
      JSON cuando nada las lea.
- [ ] **Paso 3 — firmas.** Texto desde el inicio de la definición hasta antes del cuerpo,
      espacios colapsados, tope 200 caracteres (DD-17). Rust: nombre de contenedor sin parámetros
      genéricos (`Cached`, no `Cached<T>`).
- [ ] **Paso 4 — aristas estructurales.** `contains` (padre → hijo, incluido archivo → top level),
      `imports` (símbolo de archivo → import, `dst_name` = ruta/paquete importado),
      `inherits`/`implements` (tipo → supertipo por nombre), `instantiates` (`new T`),
      `references` (tipos de campos/params/variables). Todas con `dst_id` NULL hasta TASK-005.
- [ ] **Paso 5 — compatibilidad.** `ParsedFile.symbols`/`edges` se siguen derivando para el chunker
      y `graph_edges`; revisar que el chunker con los kinds nuevos no duplique (un campo no es un
      chunk propio; un constructor sí).
- [ ] **Paso 6 — medir** con el arnés: símbolos por kind, aristas por kind en backend-b
      y DevCtxEngine; tiempo de parse (fase de parse separada del embedding).

## Criterios de aceptación

- [ ] Fixtures de los 7 lenguajes verdes; `every_definition_compiles` cubre todas las claves nuevas.
- [ ] `export const tokenInterceptor = (…) => …` es un símbolo `function` y sus llamadas tienen
      `src` en él (fixture TS que lo reproduce).
- [ ] Rust: `impl<T: Clone> Cached<T>` produce métodos `Cached.m`, nunca `Cached<T>.m` (test).
- [ ] Go: `func (s *Server) Handle()` produce `Server.Handle` (test).
- [ ] En backend-b quedan ≥ 6 kinds de arista con filas; anotado en el Resultado.
- [ ] Fase de parse ≤ +30 % sobre 0.9.0 (DD-21), medida.

## Riesgos

- Queries más grandes = parse más lento: medir por lenguaje; si una query es cara, partirla.
- Cambiar qué símbolos salen cambia chunks y por lo tanto embeddings: TASK-002 limita el costo;
  revisar con el arnés que `search` no empeore por chunks nuevos (constructores, arrow-consts).

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
