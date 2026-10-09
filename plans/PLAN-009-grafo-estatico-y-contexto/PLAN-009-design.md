# PLAN-009 — Diseño técnico: grafo estático preciso y contexto por grafo

**Fecha:** 2026-10-05
**Master:** [`PLAN-009-grafo-estatico-y-contexto.md`](./PLAN-009-grafo-estatico-y-contexto.md)
**Estado:** propuesta, pendiente de aprobación junto con el master.

Referencias `archivo:línea` contra `main` = v0.9.0 (`fdc7dd2`, `origin/main`). Ojo: el ref local
`main` de este clon quedó en 0.8.4 (`47eea79`); leer siempre con `git show fdc7dd2:<ruta>` u
`origin/main`. Las queries `tags.scm` citadas son las que traen los crates de gramática del
`Cargo.lock` (`tree-sitter-java 0.23.5`, `-typescript 0.23.2`, `-javascript 0.23.1`,
`-python 0.23.6`, `-rust 0.23.3`, `-go 0.23.4`; `tree-sitter 0.25.10`; `duckdb 1.10505.0`).

---

## §0. Cómo se arma hoy (resumen verificado)

```
archivo ─ devctx-parse ─▶ ParsedFile{symbols, imports, edges}
            │  symbols: query JSON, kind = nombre de captura, parent = nombre del contenedor
            │  imports: texto crudo (solo los lee el chunk de archivo, chunker.rs:206-222)
            │  edges:   una `calls` por @callee; target por qualified_target (parser.rs:312-341)
            ▼
devctx-index/pipeline.rs ─ index_file (826-984) ─▶ vectors (chunks) + graph_edges (store_edges 1027-1045)
            ▼
devctx-store ─ graph.rs: resolve_symbol (ends_with sobre source/target), bfs (1 SQL por nodo)
            ─ store.rs: symbol_definitions / symbol_matches / symbol_suggestions (sobre vectors.symbol)
            ▼
devctx-mcp/state.rs ─ do_impact, do_read_symbol, do_references, search_items (anclaje), do_build_context
```

- No hay símbolos con identidad: `Symbol` (`types.rs:4-30`) no tiene nombre calificado, firma ni id;
  `parent` es un nombre. `vectors.symbol` guarda `chunk.symbol_name` (nombre pelado, o
  `a, b, c +n` en un chunk `grouped`, `chunker.rs:305-341`), mientras `graph_edges.source` es
  `Clase.metodo` (`parser.rs:296-306`). El único "join" es por sufijo de string.
- Una sola relación: `GraphEdge.kind` siempre `"calls"` (`parser.rs:131`).
- El `type_map` es plano por archivo y la primera declaración gana (`parser.rs:107-108`, `or_insert`).
- `replace_file_edges` deduplica por `(source, target, kind)` (`graph.rs:75-79`) y la tabla tiene
  `UNIQUE (source, target, kind, repo, branch, source_file)` (`schema.rs:120`): la segunda llamada
  del mismo método al mismo destino desaparece. `target_file` y `metadata` se insertan `''`
  (`graph.rs:81-83`).

## DD-1 — Medir primero, con un arnés que queda en el repo

**Decisión.** TASK-001 construye el arnés y mide la línea base sobre **0.9.0** antes de que entre
código de producto. Cada task posterior prueba su mejora con el mismo arnés. Tres familias:

1. **Métricas del grafo** (`scripts/graph-eval/graph_metrics.sql` + runner): sobre una **copia** del
   DuckDB del proyecto (nunca el archivo vivo: el serve lo tiene abierto). Mismas filas que el
   inventario (§1.1 del master) para poder comparar con 0.8.2, más las nuevas cuando existan las
   tablas: % de `calls` con `dst_id`, por `confidence`, `external`, `from_test`, nodos basura.
2. **Precisión contra verdad de terreno**: un archivo `gold-edges` con ~40 sitios de llamada
   etiquetados a mano **antes** de medir (`archivo:línea → destino calificado esperado`, o
   `external`), Java 20 (backend-a + backend-b), TS 10 (frontend), Python 5
   (legacy-migration), Rust 5 (este repo). Mide precisión y cobertura de la resolución.
3. **Relevancia del contexto**: casos `consulta | archivo(s) esperado(s) | símbolo esperado? | repo`
   en el formato de `scripts/bench-queries.txt` (que ya existe y se reusa). Por caso: posición del
   archivo en `search --hybrid` (Hit@5, Hit@10, MRR) y presencia del archivo/símbolo en
   `devctx context` (build_context, 4096 tokens), con los tokens usados.

Más latencias (`impact` sobre `get`, `map`, `actualizar`, `OfficeService.actualizar`: p50/p95 de 5
corridas con serve caliente) y costo de indexado (tiempo de `index --full`, tamaño del DB, `VmHWM`;
si PLAN-010 TASK-001 ya dejó `scripts/memprobe.sh`, se usa ese).

**Privacidad.** El repo es público en GitHub. Los casos de ACME (rutas de paquetes privados, nombres de
clases de un cliente) **no** entran al repo: el runner lee los casos de `$DEVCTX_EVAL_CASES` y el
archivo de ACME vive fuera (Q-5). En el repo solo quedan los casos de DevCtxEngine y fixtures
sintéticos.

**Alternativa descartada:** medir solo al final (TASK de campo). Sin línea base no hay forma de
saber si la centralidad o el ego-graph ayudan o meten ruido; ambos cambian resultados de tools que
hoy funcionan.

## DD-2 — Schema: dos tablas nuevas, aditivas; `graph_edges` sigue escribiéndose una release

```sql
-- Una fila por símbolo definido en un archivo de una rama.
CREATE TABLE IF NOT EXISTS symbols (
    repo        VARCHAR,
    branch      VARCHAR,
    id          UBIGINT,      -- DD-3; estable entre ramas y reindexados
    parent_id   UBIGINT,      -- contenedor (clase/impl/módulo); el símbolo de archivo para el top level
    file        VARCHAR,
    kind        VARCHAR,      -- file|module|class|interface|enum|record|struct|trait|impl|type|
                              -- function|method|constructor|field|const
    name        VARCHAR,      -- pelado: `actualizar`
    qualified   VARCHAR,      -- `OfficeService.actualizar`, `devctx_store::Store::open`, `src/x.ts::tokenInterceptor`
    container   VARCHAR,      -- `OfficeService` (texto, para no unir en cada lectura)
    package     VARCHAR,      -- Java package / módulo Python / ruta de módulo TS / crate::mod / paquete Go
    signature   VARCHAR,      -- primera(s) línea(s) de la definición sin cuerpo, normalizada (DD-17)
    start_line  INTEGER, end_line INTEGER,
    start_byte  INTEGER, end_byte INTEGER,
    exported    BOOLEAN,      -- public/export/pub/mayúscula Go; para repo_map y resolución
    rank        DOUBLE,       -- PageRank global (DD-12); NULL hasta el link pass
    in_degree   INTEGER,      -- llamadas entrantes no-test resueltas (DD-12)
    is_test     BOOLEAN       -- path_kind(file) == Test (devctx-core/src/kind.rs:98)
);

-- Una fila por ocurrencia (no por par): la 2.ª llamada del mismo método ya no se pierde.
CREATE TABLE IF NOT EXISTS edges (
    repo        VARCHAR,
    branch      VARCHAR,
    kind        VARCHAR,      -- calls|instantiates|imports|inherits|implements|contains|references
    src_id      UBIGINT,      -- siempre resuelto: el símbolo envolvente, o el de archivo/módulo
    dst_id      UBIGINT,      -- NULL si no resolvió a una definición del repo
    dst_name    VARCHAR,      -- cómo quedó calificado el destino (`BetaService.findPaginated`,
                              -- `java.util.List.of`, `flatMap`); nunca una expresión
    file        VARCHAR,      -- archivo de la ocurrencia (= file del src)
    line        INTEGER,
    confidence  VARCHAR,      -- high|medium|low (DD-8)
    resolution  VARCHAR,      -- cómo se resolvió (DD-8): self|local|field|param|ctor_inject|import|
                              -- same_file|same_package|unique_name|inherited|external_known|name_only;
                              -- structural: `contains`, resuelto al parsear (DD-6), no por DD-7
    external    BOOLEAN,      -- sin definición en el repo y reconocido como de afuera (DD-9)
    from_test   BOOLEAN,      -- el archivo de la ocurrencia es de test
    edge_source VARCHAR,      -- 'treesitter' hoy; 'scip' previsto (plan propio)
    hint        VARCHAR       -- TASK-005: el receptor de una llamada como lo vio el archivo
                              -- (`typed field Foo /1`, `name Office`, `chain find name Office`);
                              -- el link pass re-resuelve desde `dst_name` + `hint`, sin la fuente
);
```

- **Sin `PRIMARY KEY`/`UNIQUE`** en las tablas nuevas. El schema ya documenta dos veces que los
  índices ART de esta versión de DuckDB devolvieron 0 filas sobre filas existentes
  (`schema.rs:163-168`, `schema.rs:211-219`). La unicidad la garantiza el escritor (borra el archivo
  y reinserta, como `replace_file_edges` hoy). Índices: solo los que TASK-003 mida como necesarios,
  y nunca sobre columnas por las que se busca igualdad crítica sin un test que lo fije; para las
  consultas por lotes (DD-11) DuckDB usa hash join y zone maps, que no dependen de ART.
- **Escritura por `Appender`** (o un `INSERT … SELECT FROM (VALUES …)` por lotes) en vez de un
  `execute` por fila como `replace_file_edges` (`graph.rs:80-93`). Con una fila por ocurrencia el
  volumen crece (estimación: 1.3-2× las 81 811 filas deduplicadas de backend-a; TASK-003 lo mide).
  `vectors` no puede usar el Appender por la columna array (`store.rs:702`), las tablas nuevas sí.
- **Mantenimiento por archivo** en los mismos puntos que hoy: `index_file` (transacción por archivo,
  `pipeline.rs:956-977`), `delete_file` (`pipeline.rs:812-824`), `copy_file_rows`
  (`memory_refs.rs:333-419`), `drop_branch` (`memory_refs.rs:427+`), `rename_file`
  (`store.rs:822`). Las aristas copiadas entre ramas se copian **sin** `dst_id`/`confidence` y las
  re-resuelve el link pass de la rama destino (DD-6): el destino de una llamada depende de la rama.
- **`graph_edges` sigue escribiéndose** durante 0.10/0.11 con el formato viejo (source calificado,
  target calificado o pelado), derivado de los hechos nuevos. Razón: un usuario que vuelva a 0.9.0
  no se queda sin grafo, y las ramas todavía no reindexadas tienen con qué contestar (DD-19). Se deja
  de escribir y se borra en el plan siguiente. Costo: ~+5-10 % de tamaño, medido en TASK-003.

**Alternativas descartadas.**

| Alternativa | Por qué no |
|---|---|
| Reusar `graph_edges` agregando columnas (`ALTER TABLE ADD COLUMN`) | Primera migración en sitio del índice; un 0.9.0 que abra el DB sigue insertando filas sin las columnas nuevas y con el `UNIQUE` viejo, que colapsa ocurrencias. Dos binarios con semánticas distintas sobre la misma tabla |
| `DROP` y recrear `graph_edges` con forma nueva | Un 0.9.0 tras downgrade falla en cada insert (columnas distintas) y no hay vuelta sin borrar el DB |
| Tabla de enlace `chunk_symbols(chunk_id, symbol_id)` | Ver DD-4: derivable por rango, y sería una tercera tabla que mantener en copy/rename/drop |
| `id` como `VARCHAR` hex (como `vectors.id`) | Joins y tamaño peores; se expone como hex solo en el JSON |

## DD-3 — Identidad de símbolo

`id = fnv1a64( repo ␟ file ␟ kind_class ␟ qualified ␟ disambiguator )` como `UBIGINT`, expuesto en
JSON como 16 hex (`"sym": "9f3ac1…"`).

- **Sin rama** en el hash: el mismo método en `main` y en `feature/x` tiene el mismo id. Permite
  copiar filas entre ramas sin re-taggear (a diferencia de `chunk_id`, que sí lleva rama,
  `devctx-index/src/id.rs`) y que una memoria o un agente guarde un id que sigue valiendo al cambiar
  de rama.
- **`kind_class`** agrupa kinds intercambiables (`function`/`method` → `callable`;
  `class`/`interface`/`enum`/`record`/`struct`/`trait` → `type`) para que el id no cambie si un
  ajuste del extractor reclasifica `function` como `method`.
- **`qualified`** lleva **todo scope envolvente**, no solo los contenedores de `container_kinds`:
  cada ancestro que es símbolo del archivo (Rust `mod`, función envolvente, método — el `wrapper`
  de un decorador Python, el método de una clase anónima Java), cada contenedor (`impl`, clase,
  trait), los `scope_kinds` del JSON (nodos que no son símbolos: TS/TSX `namespace`/`module`;
  TS/TSX/JS `variable_declarator` y `pair`, la variable o propiedad que tiene un objeto literal;
  Java `enum_constant` con cuerpo, `constructor_declaration` y `variable_declarator`, el campo o
  local que tiene una clase anónima) y, en Go, el tipo del receptor. El nombre de un scope es su
  campo `name` (o `key` en un `pair`) y solo cuenta si es un identificador o una ruta con puntos
  (un `pair` con clave string o un patrón de desestructuración no aporta segmento).
  El tipo de un `impl`/receptor se reduce al nombre que tiene su definición: sin referencia,
  lifetime, `mut`/`const`, puntero, argumentos genéricos ni ruta (`impl<T> Foo<T>`,
  `impl<'a> T for &'a mut Foo`, `impl crate::x::Foo`, `func (s *Svc[T])` → `Foo`/`Svc`); un tipo
  sin nombre propio (`[T]`, `(A, B)`) conserva su texto sin espacios (`[T].fmt`) y su padre cae al
  símbolo de archivo. Ejemplos: `tests.helper`, `deco.wrapper`, `Outer.start.run`, `a.run` y
  `b.run` (dos objetos literales), `Op.PLUS.apply`, `C.a.run`. Así dos homónimos difieren por
  *dónde* están y no por su orden en el archivo. (TASK-004: el paquete/módulo va en la columna
  `symbols.package`, **no** en `qualified`: el archivo ya está en el hash, `qualified` es lo que lee
  una persona y lo que busca TASK-008, y `graph_edges` usa la misma forma `Clase.metodo`.) Un `impl`
  de Rust es símbolo propio desde TASK-004 (kind `impl`, `qualified` = el nombre de su tipo,
  disambiguator = su trait y los argumentos de su tipo): sus métodos cuelgan de él por contención y
  él del tipo homónimo del archivo.
- **`disambiguator`**: el trait de un `impl Trait for Tipo` de Rust, sin espacios ni ruta y con
  sus argumentos genéricos (`Display` y `Debug` de `X.fmt` difieren; `From<A>` y `From<B>`
  también; `fmt::Display` es `Display`); para un `impl` y sus miembros, además, los argumentos
  genéricos del tipo propio y la cláusula `where`, sin espacios, tras `~` (`impl Foo<u8>` y
  `impl Foo<u16>` daban los dos `Foo` y sus `Foo.get` caían al ordinal, así que insertar uno arriba
  movía el id del otro, el `parent_id` de sus métodos y sus `contains`; fixup de la revisión de
  TASK-004 — costo aceptado: renombrar el parámetro de `impl<T> Foo<T>` cambia esos ids; desde la
  versión 4 del extractor, también sin comas finales antes de `>`/`)` ni al final del `where`,
  porque `rustfmt` escribe vertical un `where` largo —`where\n    T: Copy,`— o una lista larga de
  argumentos —`Foo<\n    T,\n>`— y cambiaba el id del `impl`, de sus métodos y sus `contains`;
  desde la versión 5, también sin los comentarios de adentro —`where // orden\n T: Copy`—, y sin
  tocar la coma antes de `)`: `(u8,)` es una tupla de un elemento y no es `(u8)`); la lista de tipos de parámetros normalizada (sin nombres,
  espacios, argumentos genéricos ni paquete: `java.util.List<String>` → `List`, porque dos
  sobrecargas no pueden diferir solo en genéricos) en lenguajes con sobrecarga de cuerpos
  separados — **solo Java** (`"overloads": true`); y la **forma del scope** cuando algún scope
  envolvente es invocable: un carácter por scope, `f` si el nodo está en `function_kinds`, `s` si
  no, tras `@` (`Outer.start.run` → `@sf`; sin scope invocable no se agrega nada, así que los ids
  de métodos de clase y funciones de módulo no la llevan; en lenguajes con `"overloads": true`
  el carácter `f` lleva los tipos de parámetro del invocable, `f(int)`, porque dos sobrecargas
  `O()` y `O(int)` —o `m(int)` y `m(String)`—, cada una con una clase anónima con `run`, daban
  las dos `O.O.run` `@sf` y caían al ordinal: `@sf()` y `@sf(int)`). Resuelve la ambigüedad del `.`:
  `fn a() { struct P }` y `mod a { struct P }` (Rust los permite, son namespaces distintos; TS
  fusiona `function a` con `namespace a`; Java un método `a` y una clase interna `a`) daban los dos
  `a.P`. Se eligió el disambiguator y no `qualified` porque `qualified` es lo que lee una persona
  (`Outer.start.run`, no `Outer.start().run`) y lo que TASK-008 busca por nombre; ni `kind_class`,
  que es la clase del símbolo, no la de sus scopes. Java la usa **siempre**,
  no solo cuando hay homónimos: si dependiera de que exista otra sobrecarga, agregar una le
  cambiaría el id a la original (el mismo defecto que el ordinal); el costo aceptado es que
  cambiar el tipo de un parámetro cambia el id, que es lo que este DD ya dice abajo.
  **TS/TSX no**: las firmas de sobrecarga no son símbolos (hay un solo cuerpo), así que los tipos no
  desambiguan nada y solo volvían el id frágil a un refactor de tipos.
- **Ordinal, último recurso:** solo si dos símbolos del archivo siguen compartiendo `kind_class`,
  `qualified` y `disambiguator` o hay colisión de hash, se agrega `#n` en orden de fuente. Es el
  único caso en que insertar un homónimo arriba mueve un id; el test
  `inserting_a_homonym_above_keeps_existing_ids` fija que no pasa en: Rust `mod tests`, dos impls
  de trait, `fn a`/`mod a` del mismo nombre, decoradores Python, receptores Go, clases anónimas
  Java en métodos distintos, en sobrecargas distintas (constructores o métodos), en un constructor, en
  campos y en constantes de enum, y objetos
  literales TS/JS en variables o propiedades distintas. **Casos que siguen cayendo al ordinal
  (abiertos, conocidos):**
  - redefinición real (Python `def f` dos veces; `function f` repetida en JS no estricto);
  - dos clases anónimas Java con el mismo método dentro de un **mismo** método, inicializador
    `static {}`/de instancia, o lambda; y dos expresiones `new X() {…}` en un mismo campo;
  - objetos literales sin variable ni propiedad que los nombre: argumentos de llamada
    (`describe({ run() {} })` dos veces), `export default {…}`, elementos de arreglo, `return {…}`;
    también los que cuelgan de un `pair` con clave string o computada (`{ 'a': { run() {} } }`,
    `{ [k]: { run() {} } }`: la clave no es un identificador y el `pair` no aporta segmento, así
    que dos de esos `run` en el mismo objeto comparten calificado y se separan por ordinal);
  - Java: clases anónimas dentro de lambdas (la lambda no tiene nombre, no es scope) en un mismo
    método;
  - TS/JS: funciones flecha y expresiones de función anónimas (callbacks) no son símbolos; desde
    TASK-004 sí lo son las asignadas a un `const`/`let`/`var` o a un campo de clase (kind `function`
    /`method`, nombradas por la variable o el campo);
  - Go: dos `func init()` en un archivo (legal en Go) y funciones anónimas (no son símbolos);
  - Rust: dos `impl Foo {}` idénticos (mismo tipo, mismos argumentos, mismo `where`).
- **Varios nombres en una declaración** (TS/JS `const a = () => {}, b = () => {}`, Java
  `int a, b;`, Go `var a, b int`): son hermanos, ninguno padre del otro. En TS/JS y Java cada uno
  abarca su propio `variable_declarator` (el primero desde la palabra clave y su doc), así que sus
  llamadas y usos de tipo son suyos; en Go no hay un nodo por nombre, comparten el rango, ninguno
  contiene al otro y lo que haya adentro se atribuye al primero.
- **Forma:** un solo FNV-1a sobre los cinco campos, separados por `␟` (0x1f), valor fijado por un
  test dorado (`the_id_format_is_pinned`). TASK-003 probó primero una composición XOR (mitad
  archivo ⊕ mitad símbolo) para que `rename_file` re-keyara sin re-parsear; se descartó en la
  revisión: era exacta solo mientras `qualified` no dependiera de la ruta (TASK-004 la hace
  depender), `rename_file` no tiene llamador de producción (el pipeline indexa un renombre como
  borrar + agregar) y su test la validaba con la misma función.
- **Renombre de archivo:** `Store::rename_file` mueve `vectors` y **borra** las filas de `symbols`
  y `edges` de la ruta vieja; el reindex de la ruta nueva las escribe con ids nuevos. Las aristas de
  otros archivos ya resueltas hacia esos ids quedan con `dst_id`/`confidence`/`resolution` en NULL
  para que las re-resuelva el link pass (DD-6), en vez de apuntar a símbolos inexistentes.
- **Desfase grafo/archivos:** el sello del extractor solo lo reescribe un `--full`, así que un
  0.9.0 tras un downgrade puede hacer incrementales bajo un sello `v2` vigente.
  `graph_out_of_step` da `true` si `file_state` y `symbols` no coinciden: archivo con símbolos sin
  fila `file` (agregado por ese binario), fila `file` sin `file_state` (borrado) o fila `file` cuyo
  `symbols.content_hash` no es el `file_state.content_hash` del archivo (modificado: sus filas
  siguen, viejas). La columna `content_hash` de `symbols` solo se llena en la fila `file` (mismo
  valor que `file_state`; la copia entre ramas la lleva tal cual). Tres anti-joins sobre una rama.
  Lo usan `extractor_stale` (status, tools de grafo, inicio de cada corrida) y
  `branch_with_same_content`: una rama fuera de paso no es fuente de copia aunque su sello diga
  "vigente" (si no, sus filas viejas se propagaban a la rama que se indexa y quedaban bajo un sello
  nuevo). Costo: la respuesta se cachea por `(repo, repo_path, rama)` junto con
  `index_state.indexed_at` de la rama (una corrida que escribe la rama termina con un `indexed_at`
  nuevo), y cada escritura de este proceso a la rama (`file_state`, tablas del grafo,
  `index_state`, `drop_branch`) o un rollback la invalida, antes de escribir y **otra vez al
  COMMIT** (cada uno de esos escritores corre en una transacción, la del llamador o la suya, así
  que también en autocommit hay invalidación después de escribir) (hasta el commit otra conexión del serve lee las filas viejas y podía cachearlas con
  el mismo `indexed_at`); cada invalidación sube una generación y una respuesta calculada a
  través de una no se guarda; dentro de una transacción la conexión no usa la caché (ve sus
  propias filas sin commit). Así nunca es más vieja que las filas confirmadas. El primer
  anti-join mira los archivos de lenguaje parseable (`GRAPH_LANGUAGES`), no `symbol_count > 0`:
  un archivo parseable sin símbolos agregado por 0.9.0 también queda sin fila `file`. Sin caché era una consulta por tool call y una por archivo candidato a copia.
- **Versión de la salida, no del código (decidido en la revisión de TASK-004).** El fingerprint
  (`EXTRACTOR_VERSION` + hash de `languages/*.json`) no ve el Rust del extractor: db0a0ea cambió
  ids tocando solo `parser.rs` y los índices de desarrollo de cd7d2a2 siguieron "vigentes". No se
  hashea el código fuente (un refactor, un comentario o `rustfmt` obligarían a todos a un `--full`
  con salida idéntica: el churn que TASK-002 evita). En su lugar: `EXTRACTOR_VERSION` sube a **3**
  con TASK-004, y `crates/devctx-parse/tests/extractor_golden.rs` fija **todo lo que el índice
  persiste de un parse** —paquete; por símbolo kind, `qualified`, id, padre (`contains`),
  contenedor, `exported`, líneas y firma; por llamada fuente → destino y el nombre de fuente que
  escribe `graph_edges`; `references`/`instantiates`, supertipos e imports con su fuente— de un
  fixture por lenguaje (Rust, Java, TS, TSX, JS, Python, Go) contra
  `tests/golden/extractor.txt`, cuya primera línea guarda la versión con que se generó (**4**
  desde los menores de la revisión de 6c3f63a: `where`/genéricos de un `impl` estables ante
  `rustfmt`, y la fuente de `graph_edges` de una función con nombre es el `qualified` de su
  símbolo —`traced.wrapper`, `C.init.run`, `C.m.f`; `const x = function named()` es `x`—, que
  `tests/graph_sources.rs` exige en todos los fixtures; **5** desde la revisión de 3ad7d54:
  comentarios fuera del disambiguator de un `impl`, la coma de una tupla de un elemento se
  conserva, y `export default (foo);` exporta `foo`). Si la
  salida cambia y la versión no, falla con "la salida del extractor cambió: subí
  EXTRACTOR_VERSION y regenerá el golden"; si la versión cambió y el golden no, pide regenerarlo
  (`DEVCTX_UPDATE_GOLDEN=1 cargo test -p devctx-parse --test extractor_golden`).
- FNV-1a 64 porque ya está en el árbol (`registry.rs:96-100`) y es estable entre plataformas y
  versiones (a diferencia de `DefaultHasher`). Probabilidad de colisión con 10⁶ símbolos ≈ 3·10⁻⁸;
  igual, el escritor detecta una colisión dentro del archivo y la resuelve con el ordinal.
- **Estabilidad:** el id sobrevive a reindexar, a mover el cuerpo dentro del archivo, a cambiar la
  firma de retorno y a insertar homónimos en otros scopes. Cambia si cambia archivo, scope
  envolvente, nombre, trait del `impl` o (Java) parámetros: es otro símbolo a efectos de "quién lo
  llama". No se intenta seguir renombres (fuera de alcance).
- El **símbolo de archivo** (`kind = file`, `qualified = <ruta>`) existe siempre: es el `src` de las
  llamadas a nivel de módulo (Python, scripts, `export const x = f()` en TS) y de las aristas
  `imports`, y la raíz de `contains`.

## DD-4 — Chunks ↔ símbolos por rango, sin tabla de enlace

Un símbolo se liga a sus chunks por `repo, branch, file` + solapamiento de líneas
(`vectors.start_line/end_line` contra `symbols.start_line/end_line`), con la regla: el chunk de
nivel `function`/`block` cuyo rango está contenido en el del símbolo es "código del símbolo"; un
`grouped` que contiene el rango del símbolo también (es la tercera pasada de hoy,
`store.rs:905-929`, pero sin regex sobre el texto); `file`/`class`/`doc` son resúmenes y no
cuentan (`is_summary`, `devctx-search/src/lib.rs:735-737`).

- Cero costo de escritura y nada que mantener al copiar/renombrar.
- Las búsquedas por `symbol_id` de un hit se hacen al revés: chunk → símbolo más interno que lo
  contiene. `idx_vectors_file` (`schema.rs:101`) ya cubre `(repo, branch, file)`.
- `vectors.symbol` **no se toca** (seguir guardando lo que guarda), para no cambiar el texto
  embebido ni el contrato de `search`.

## DD-5 — Extracción: queries declarativas en `languages/*.json`, resolución en Rust

**Decisión.** Lo que es *patrón sintáctico* vive en el JSON (lo cubre el hash del fingerprint,
`registry.rs:102-117`, y lo valida `every_definition_compiles`, `registry.rs:182-198`). Lo que es
*semántica* (cómo resuelve un import cada lenguaje, scopes, receptores) vive en Rust, un módulo por
lenguaje detrás de un trait.

Claves nuevas del JSON (todas opcionales salvo `definitions`; las viejas se mantienen mientras haya
código que las lea y se retiran en TASK-004):

| Clave | Capturas | Para qué |
|---|---|---|
| `definitions` | `@definition.<kind>` sobre el nodo + `@name` (+ `@params`, `@return` opcionales) | Reemplaza `symbols`. Convención `tags.scm`; agrega `constructor`, `record`, `field`, `const`, TS `arrow`/`function_expression` asignadas a `const`, Rust `impl`/`const`/`type` |
| `references` | `@reference.call` + `@name` (+ `@receiver`), `@reference.new` + `@type`, `@reference.type` | Llamadas, instanciaciones, usos de tipo (campos/params/variables) |
| `inherits` | `@inherit.extends` / `@inherit.implements` + `@type`, dentro de `@definition.*` | `extends`/`implements`/`impl Trait for`/herencia Python/embebido Go |
| `imports` | `@import` + `@import.path`, `@import.name`, `@import.alias`, `@import.wildcard` | Imports estructurados en vez de texto |
| `scopes` | `@scope` | Nodos que abren un scope léxico (método, lambda, bloque, clase) para el `type_map` con scope |
| `types` | `@name` + `@type` (como hoy) + `@init` | Bindings con scope; `@init` para inferir `var`/`let`/`:=` del inicializador |
| `function_kinds` / `container_kinds` | — | Como hoy, más `arrow_function`/`function_expression` (TS/JS) y `impl_item` con tipo genérico normalizado |

Las queries `TAGS_QUERY` que traen los crates de gramática (`tree_sitter_java::TAGS_QUERY`, etc.)
**se usan como referencia, no en runtime**: son delgadas (Java: 20 líneas; no capturan
constructores, campos ni imports; TS solo agrega firmas y `new` sobre JS) y no se pueden extender
sin copiarlas. Copiarlas al JSON propio deja una sola fuente y la cubre el fingerprint.

El trait (en `devctx-parse`, nuevo módulo `resolve/`):

```rust
pub trait LangResolver {
    /// Hechos locales del archivo: lo que se puede decidir sin mirar otros archivos.
    fn file_facts(&self, tree: &Tree, src: &[u8], def: &LangDef, path: &str) -> FileFacts;
    /// Cómo un import de este lenguaje nombra archivos/paquetes/símbolos del repo.
    fn import_targets(&self, imp: &ImportFact, ctx: &RepoIndex) -> Vec<ImportTarget>;
    /// Si un nombre sin definición en el repo es de la plataforma (JDK, builtins, std).
    fn is_platform(&self, qualified: &str) -> bool;
}
```

`ParsedFile` gana `facts: FileFacts` (símbolos con id/firma/paquete, referencias con su receptor ya
tipado localmente o marcado como no tipable, imports estructurados, herencias). `edges` y `symbols`
viejos se siguen derivando para el chunker y para `graph_edges` (DD-2).

**Implementado en TASK-004** (desvíos): `references` usa `@reference.call` + `@name` (+ `@path` para
`Foo::bar` de Rust), `@reference.new` + `@type`, `@reference.type` (+ `@type`) y la clave
`type_names` dice qué nodos son nombres de tipo dentro de un uso (`List<Foo>` = `List` y `Foo`; los
parámetros de tipo declarados, `T`, se descartan); `inherits` captura el supertipo
(`@inherit.extends`/`@inherit.implements`) y el definidor es el símbolo más interno que lo
contiene; `imports` agrega `@import.default`, `@import.namespace` y `@import.tree` (el `use` de
Rust lo expande el resolver); `package` (Java, Go) es una clave nueva. `scopes` y `@init` en
`types` quedan para TASK-005-007, que son quienes los consumen (resolución con scope). El trait
`LangResolver` (`resolve/mod.rs`) tiene solo la parte local —`package_from_path`, `exported`,
`expand_import_tree`, `import_target`, `is_platform`—: las queries las compila y corre
`LanguageParser` una vez por lenguaje y hilo, no el trait (compilarlas por archivo era el costo
dominante del parse); `import_targets` llega con el link pass. Formato del destino de `imports`:
Java `a.b.C`/`a.b.*`, Python `a.b`/`.m.x`/`m.*`, Rust `crate::a::B`/`a::*`, Go `net/http`, TS/JS
`./x#Nombre`/`./x#default`/`./x#*`/`./x`.

**Alternativas descartadas:** todo en Rust (pierde el hash del fingerprint como detector de cambio y
la revisión "un archivo por lenguaje" que justificó `registry.rs:8-12`); todo en queries
(`tree-sitter` no resuelve nombres, lo dice su propia doc; los imports relativos de TS o los
paquetes Java son lógica, no patrones); stack-graphs / TSG (archivado, RUSTSEC-2026-0303).

## DD-6 — Dos fases: hechos por archivo + link pass por rama

La resolución cross-file necesita ver todo el repo, y el indexado es por archivo e incremental. Se
separa:

1. **Fase local (por archivo, dentro de `index_file`)**: parse → `FileFacts` → filas de `symbols`
   (completas) y de `edges` con `src_id`, `dst_name` calificado **localmente** (receptor tipado por
   scope/campo/param/import del mismo archivo), `dst_id = NULL`, `resolution` provisoria.
2. **Link pass (por rama, al final de cada corrida de `pipeline::run`, después de los archivos y
   antes del sello del extractor, `pipeline.rs:542-562`)**: carga en memoria un `RepoIndex` de la
   rama (`qualified → ids`, `name → ids`, `package/módulo → archivos`, `archivo → exports`,
   herencia), resuelve cada arista pendiente y escribe `dst_id`, `confidence`, `resolution`,
   `external`; después recalcula `rank`/`in_degree` (DD-12).

**`contains` es intra-archivo y no pasa por el link pass.** Ambos extremos son símbolos del mismo
archivo (lo afirma un `debug_assert` en `graph_rows` y el test `every_relation_of_a_file_is_an_edge`),
así que se escribe resuelto al parsear (`dst_id`, `high`, `resolution = 'structural'`) y la copia
entre ramas lo conserva. El link pass de TASK-005 **excluye `kind = 'contains'`** de lo que carga y
resuelve. Una contención entre archivos que llegue en el futuro (p. ej. un `mod foo;` de Rust hacia
`foo.rs`, un `partial`) se escribe **sin resolver** (`dst_id` NULL, sin `structural`), como
cualquier otra arista, y la resuelve el link pass.

**`exported` no sirve de filtro de candidatos intra-paquete.** `Symbol.exported` significa "visible
fuera de su paquete/módulo" (`public`, `export` o `export { x }`, `pub`, mayúscula Go, sin `_` en
Python y no anidado en una función). Java package-private/`protected` y Go en minúscula dan `false`
aunque el resto del paquete los ve: un link pass que descarte candidatos con `exported = false`
pierde toda la resolución dentro del paquete en Java y Go. Filtrar por `exported` solo vale para
candidatos de **otro** paquete/módulo.

**Incremental.** El link pass re-resuelve (a) las aristas de los archivos escritos en la corrida,
(b) las aristas de otros archivos cuyo `dst_name` (o su último segmento) coincide con un símbolo
agregado, borrado o renombrado en la corrida, y (c) las que quedaron sin resolver. Si los archivos
cambiados superan un umbral (p. ej. 20 % de la rama) hace el pase completo, que es más simple y, con
~10⁵ aristas, cuesta segundos (TASK-005 lo mide). PageRank global se recalcula entero (milisegundos).

**Implementado en TASK-005 (con los cambios de su revisión).** El motor (`RepoIndex` y reglas) es
puro y vive en `devctx-parse/src/resolve/link.rs`; `devctx-index/src/link.rs` carga la rama (todos
los símbolos y las aristas salvo `contains`), elige qué re-resolver y escribe:
- *Selección incremental:* (a) aristas de los archivos escritos o borrados (solo fuentes: un `.md`
  no cuenta); (b) las de otros archivos cuyo `dst_id` ya no existe, cuyo último segmento de
  `dst_name` **o algún token de `hint`** (el tipo del receptor, el callee anterior de una cadena) es
  un nombre que define un archivo escrito —métodos, campos **y tipos**—, o cuya fuente está dentro
  de un tipo que hereda (transitivamente) de un tipo escrito (los nombres del hint se leen por
  posición —tipo del receptor, nombre, ruta de un `member`, callees de una cadena—, nunca sus
  palabras `typed`, `field`, `name`…); (c) las sin decidir, **sin** las descartadas, que
  esperan a su nombre por (b); (d) desde TASK-006, todas las aristas de los archivos TS/JS que
  importan un archivo escrito (por cualquier ruta que su especificador pueda nombrar, así que un
  archivo agregado o borrado cuenta), también a través de barrels que lo re-exportan: lo que un
  import liga puede cambiar sin que cambie ningún nombre del archivo escrito (un `export … from`
  que se mueve); con ellos, los nombres de los símbolos de los importadores también reabren por (b)
  (una cadena `a.f().g()` en un tercer archivo), y una fila `imports` de TS/JS se compara por el
  nombre tras `#`. Pase completo si lo escrito (más los importadores) supera 1/5 de la rama, o si
  el entorno TS/JS (`tsconfig`, `package.json`) no es aquel con que se enlazó la rama
  (`index_meta.link_tsconfig`); uno ilegible no cuenta: se usa el último bueno
  (`index_meta.link_script_env`). Un test fija que el incremental
  deja la rama **igual fila a fila** que un pase completo tras cambiar un `extends`, un tipo de
  retorno, agregar `@Data` y mover un paquete.
- *Siempre al día:* `index_meta.link_pending` se marca antes de la fase de archivos y se borra al
  completar un pase (después de grabar `link_version`): una corrida cortada (parada, crash) deja la
  marca y la siguiente enlaza toda la rama aunque no escriba nada. `index_meta.link_version` guarda `LINK_VERSION` (la versión de las
  reglas, en Rust): si difiere, pase completo (~3 s en backend-a). El extractor tiene la suya
  (`EXTRACTOR_VERSION`).
- *Descartes:* una llamada sin tipo a un nombre que el repo no define no se borra: queda con
  `resolution = 'discarded'` (`low`, sin `dst_id`, `external = false`), fuera de la vista
  `live_edges` (la que van a leer las tools de TASK-008, y `Store::branch_undecided_calls`) y de los conteos del
  arnés, y el modo (b) la reabre por nombre cuando aparece la definición (el resultado no depende
  del orden en que se indexan los archivos).
- *Escritura:* solo los archivos con filas cambiadas, **64 archivos por transacción** y un `DELETE …
  kind <> 'contains' AND file IN (…)` por lote; cada archivo entra entero en una transacción, pero
  un lector concurrente puede ver la rama mitad con respuestas viejas y mitad con nuevas mientras
  dura el pase (por archivo, transacción + scan costaban 10 ms: 14,1 → 2,5 s en backend-a).
- Medido en backend-a: pase completo 2,6 s, incremental de un archivo 0,47 s, `VmHWM` +114 MiB.

**Implementado en TASK-007** (Python, Rust, Go):
- *Entorno:* el fingerprint de `index_meta.link_tsconfig` y el entorno guardado en
  `index_meta.link_script_env` (las claves conservan su nombre de TASK-006) cubren ahora todo el
  `LinkEnv` (`resolve/env.rs`): el entorno TS/JS, cada `Cargo.toml` (crate del workspace por `[lib]
  name` o `[package] name`, claves de sus tablas de dependencias), cada `go.mod` (`module`,
  `require`, `replace` local) y cada `pyproject.toml`/`setup.cfg`/`requirements*.txt` (nombres de
  import de lo declarado, best effort). Un manifest cambiado relinkea la rama entera; uno ilegible
  toma su última versión buena **por ruta** y nada más (`devctx-index/src/env.rs`); los de
  `target/`, `vendor/`, `dist/`, `.venv/` y similares no cuentan. Un entorno solo TS/JS da el mismo
  JSON y el mismo fingerprint que antes, y el guardado de TASK-006 se lee como `LinkEnv`.
- *Modo (d)* también para Python (todo módulo reexporta lo que importa: los importadores de un
  importador, hasta 4 niveles) y Rust (archivos cuyo `use` nombra el módulo de un archivo escrito,
  propagando por los que tienen `pub use`). Go no lo necesita: un import nombra un directorio, y lo
  que cambia en él lo ven los nombres (modo b) y el `go.mod` (entorno).
- *Escenarios* nuevos en `an_incremental_link_pass_equals_a_full_one`, cada uno verificado fallando
  sin su regla: un `__init__.py` que reexporta, un `requirements.txt` que declara un paquete, un
  `pub use` de la raíz del crate, un `Cargo.toml` que agrega un crate al workspace y un `go.mod` que
  renombra el `module`.
- *La clave `#` del modo (b)* para las filas `imports` de script (`dst_key`) queda como defensa;
  desde que un paquete declarado es siempre external (mi2 de TASK-006), ningún escenario la detecta
  sola, porque la respuesta de una fila de import depende solo del módulo, y eso ya lo cubren el
  modo (d) y el fingerprint del entorno. No quitarla.
- Medido: ver el Resultado de TASK-007 (pase completo de este repo ~0,2-0,4 s; incremental de un
  archivo muy importado, del mismo orden: lo domina cargar la rama).
- *Revisión de TASK-007:* el alcance de un import de Python suma el `__init__.py` de cada paquete
  del camino (`import pkg.sub` liga `pkg`), y un `__init__.py` escrito reabre su directorio (las
  raíces de sus scripts cambian). Los nombres de los importadores que reabren por (d) son solo los
  que pueden tipar una cadena —tipos, campos, invocables con retorno declarado no vacío—, en vez de
  la lista fija `GENERIC_NAMES`. Una llamada por path de Rust resuelta externa no se reabre por
  nombre (depende del entorno y de los `use`: pase completo, modo a, modo d). En un importador Rust
  que no se escribió se reabren solo las filas que nombran lo que ligan sus `use` (más cadenas y
  miembros; un glob de otro módulo lo reabre entero). Cada una con su escenario en la equivalencia.

**Memoria.** `RepoIndex` de frontend (2133 archivos): estimación < 100 MB (strings +
`HashMap`s); TASK-005 lo mide con el método de PLAN-010 y es criterio de aceptación (master §6).

**Alternativa descartada:** resolver al consultar (como hoy `resolve_symbol` por sufijo). Es lo que
hace lentos a `impact`/`get_references` y deja todo como coincidencia de strings.

## DD-7 — Algoritmo de resolución

Orden de intento por ocurrencia; la primera regla que resuelve gana y fija `resolution`:

| # | Regla | `resolution` | `confidence` |
|---|---|---|---|
| 1 | `this`/`self`/`cls`/`Self`/`super` → contenedor (super → padre por herencia) | `self` / `inherited` | high |
| 2 | Receptor es local/param con tipo en el **scope** léxico vigente | `local` / `param` | high si el tipo resuelve a un símbolo del repo o a un import externo |
| 3 | Receptor es campo de la **clase envolvente** (incluye clases internas por separado) | `field` | high |
| 4 | Campo asignado desde un param de constructor (`this.x = x`) o propiedad de parámetro TS (`constructor(private x: T)`) o `inject(T)` | `ctor_inject` | high |
| 5 | Receptor es un tipo (identificador que resuelve a un tipo por import/paquete, **no** por mayúscula) | `import` / `same_package` | high |
| 6 | Llamada sin receptor → método del contenedor o heredado; si no, función del mismo archivo; si no, import nombrado | `self` / `inherited` / `same_file` / `import` | high |
| 7 | Nombre único en el repo para ese `name` y aridad compatible | `unique_name` | medium |
| 8 | Varios candidatos en el repo, ninguno decidible | `name_only` (`dst_id` NULL) | low |
| 9 | Sin candidatos en el repo y el tipo/paquete es de afuera (import no-repo, JDK, builtins, std) | `external_known` | high (de que es externo) |

Reglas por lenguaje (orden de valor para el usuario: Java y TS primero):

- **Java.** Paquete del archivo + imports exactos + `import x.*` + mismo paquete → tipos.
  `var x = new T(…)` / `(T) e` / `T.of(…)`-style estáticos de un tipo del repo → `T`. Genéricos:
  `List<Foo>` → receptor `List` (externo) y se guarda el argumento para for-each
  (`for (Foo f : lista)`) — esfuerzo acotado, sin inferencia. Constantes en MAYÚSCULA (`LOG`) se
  resuelven como campo con su tipo declarado, nunca como tipo (`parser.rs:327` hoy las toma por
  tipo). Clases internas: cada clase tiene su propio mapa de campos (arregla
  `DraftResource.java:310`, master §2 H3). Panache: un estático no definido en una clase que
  extiende `PanacheEntity*`/`PanacheRepository*` → `inherited` + `external` (`Office.findById`).
  `object_creation_expression` → `instantiates`.
- **TypeScript/JS.** `import {X as Y} from './rel'` → archivo relativo (`.ts`, `.tsx`, `/index.ts`);
  paths de `tsconfig.json` (`compilerOptions.paths`, alias Nx `@acme/...`) best-effort leyendo el
  `tsconfig.base.json` de la raíz; paquetes npm → externos. `export const x = (…) => …` y
  `const x = function` son símbolos `function` y **fuente** de sus llamadas (`function_kinds` hoy no
  incluye `arrow_function`, por eso esas llamadas se descartan, `parser.rs:125-127`). DI de Angular:
  propiedades de parámetro del constructor y `inject(T)` → campo tipado. Decoradores: se ignoran
  como llamadas (ruido), se guardan como `references` al tipo del decorador solo si resuelve en repo.
- **Python.** `import a.b as c`, `from .x import y` (relativo al paquete), `from x import *`
  (exports del módulo si está en repo). Llamadas a nivel de módulo → `src` = símbolo de archivo
  (hoy se descartan). `self.x = Foo()` en `__init__` → campo tipado. Builtins (`len`, `print`,
  `dict.get`, …) y stdlib → `external_known`. Ocurrencias no se deduplican (DD-2).
- **Rust.** `use crate::a::B`, `use super::*`, `mod` → módulos. `Foo::bar` usa `Foo` como receptor
  (hoy queda `bar`, `rust.json` `scoped_identifier`). `impl Trait for T` → `implements` y métodos
  calificados `T.m`; `impl<T> Foo<T>` → contenedor `Foo` (hoy el nombre del contenedor es el texto
  del tipo, `parser.rs:395-400`, y sale `Cached<T>`; a confirmar con test en TASK-004). `std::`,
  crates del `Cargo.lock` que no son del workspace → externos.
- **Go.** Métodos con receptor `(s *Server) Handle` → `Server.Handle` (hoy `container_kinds: []`,
  `go.json`). Imports con prefijo del `module` de `go.mod` → repo; resto externo. Prioridad baja: no
  hay repo Go del usuario; se cubre con fixtures.

**Cadenas fluent** (`a.b().c()`): el receptor de `c` es una expresión. Si el método anterior
resolvió a un símbolo del repo **con tipo de retorno declarado** en su firma, ese tipo es el
receptor (un nivel, sin genéricos). Si no, y `c` no tiene ninguna definición en el repo → la
ocurrencia **se descarta** y se cuenta en `IndexResult.edges_discarded` (honestidad, igual que
`files_skipped`). Si `c` sí existe en el repo → se guarda como `name_only`/low. Esto elimina los
`map`/`flatMap`/`subscribe` que hoy son el grueso de los 2 569 callers de `map`.

**Implementado en TASK-005** (Java; el resto con reglas genéricas hasta TASK-006/007; incluye la
revisión):
- *Local:* `types` del JSON con roles (`@bind.field|param|local|assign`, `@init`, `@iter`) y una
  clave nueva `scopes` (lista de node kinds, no query): cada binding vive en el scope más interno;
  un local se ve después de su declaración; `var` toma su tipo de `new T`/`(T) e`, o de `T.of(…)`
  como conjetura (`static`) que el link pass confirma solo si `T` tiene el método; for-each toma
  el elemento de `List<Foo>`/`Foo[]`; parámetros de lambda sin tipo y variables de patrón
  (`instanceof Foo f`) son bindings (sin tipo / con el del patrón), así un parámetro nunca toma el
  tipo del campo homónimo; `this.x = x` en un constructor da la etiqueta `ctor_inject` pero el
  **tipo declarado del campo** (no el del parámetro: `Foo(ServiceImpl s)` con `Service s` sigue en
  `Service`). Un receptor con puntos cuyo primer segmento es un binding es `member <campos>
  <hint>` (`target.parent.m()`, `this.cfg.server.port()`), nunca un paquete; una llamada sin
  receptor o con `this` dentro de una clase anónima es `anon <Tipo> …`. Cada llamada lleva
  `edges.hint`; `dst_name` nunca se reescribe.
- *Link:* además de la tabla: un `member` se tipa campo por campo con el tipo declarado de cada uno;
  un nombre con puntos que el scope no ligó es paquete externo solo con evidencia (prefijo de
  plataforma, o un import desde esa raíz fuera de los paquetes del repo); llamada sin calificar no
  encontrada en la jerarquía → import estático (simple o comodín de una clase externa) o supertipo
  externo → externa; en una clase anónima, primero su supertipo (si es externo y la clase
  envolvente también tiene el nombre: sin decidir); cadena hasta 4 llamadas atrás, con el retorno
  declarado (Java) y **nunca más segura que la llamada anterior**; tras una llamada externa nada
  tipa el receptor: si el nombre existe en el repo o tiene forma de accessor queda `name_only`
  (sin `external`, alcanzable por nombre en DD-10), si no `chain_external` `medium`; receptor
  declarado sin tipo = como cadena sin tipo (descarte si el nombre no está en el repo); métodos de
  `Object` sobre receptor sin tipo → externo `medium`; getter/setter de Lombok → el campo,
  `medium`; una sobrecarga que ninguna aridad toma busca en los supertipos y si no, `medium`; un
  tipo con FQN duplicado (dos módulos) o hallado solo por nombre (`unique_name`, fuera de Java)
  nunca da `high` a lo que tipa; la aridad se lee solo de firmas Java. Medido: ver el Resultado de
  TASK-005.

**Implementado en TASK-006** (TypeScript/TSX/JavaScript, un resolver compartido,
`resolve/typescript.rs`):
- *Local:* `scopes` (cuerpo de clase, método, función, flecha, bloque, `for`, `catch`) y `types`
  con roles: campo con tipo o inicializado (`inject(T)`, `new T()`, `x as T` → su tipo),
  propiedad-parámetro del constructor (`@bind.inject`: campo de la clase, `ctor_inject`),
  parámetros (también desestructurados, de flecha, opcionales), locales y variables de `catch`, sin
  tipo si no lo escriben; una variable cuyo valor es una función o una clase no es binding (es
  símbolo). `inject(T)` da `ctor_inject` también en un local. Sin `this` implícito
  (`LangResolver::implicit_this`): un nombre pelado nunca es un miembro, y una llamada pelada a un
  parámetro o local (`next(req)`) es sin tipo. `this` es la clase solo en un método o campo de una
  clase **declarada**; en un método de objeto literal, una *class expression* o una `function`, sin
  tipo. Solo el `inject` importado de `@angular/core` inyecta. Las funciones son bindings con scope
  (`function f` elevada; `const f = () =>` desde su declaración, en todo el módulo si es de nivel
  superior): una llamada pelada ligada a una función de un scope envolvente es `bare`; una que
  ningún scope liga, o ligada a la función del módulo, es `free` (hint nuevo: el link no mira las
  funciones anidadas de los envolventes, así una de un bloque hermano no la toma); dos homónimas
  de bloques hermanos dan `medium`. Un receptor bindeado a una función o clase es su nombre
  (`Klass.make()` de `const Klass = class {…}` → `Klass.make`, `same_file`, si es el único
  invocable `recv.callee` del archivo; `fn.call()` sin decidir). Una unión con
  `null`/`undefined` es el otro miembro. Cada fila `imports` lleva en `hint`
  `export` (re-export de barrel) y `as <local>`.
- *Link:* el especificador se resuelve relativo (`.ts`, `.tsx`, `.js`→`.ts`, `/index.*`), por el
  patrón `paths` más largo (exacto primero) o bajo `baseUrl` del `tsconfig.base.json` (o
  `tsconfig.json`) de la raíz, un nivel de `extends` relativo (string o array); un especificador
  que no es del repo es paquete → externo **solo con evidencia** (revisión de TASK-006): una
  dependencia del `package.json` raíz o del más cercano —el más profundo que contiene al archivo:
  con `a/` y `a/b/`, un archivo de `a/b/` mira `a/b` y la raíz, nunca `a/`—, o un builtin de
  Node; con esa evidencia es externo aunque el repo defina el mismo nombre (segunda revisión). El
  `name` de un `package.json` del repo hace del paquete uno del repo, salvo los de `node_modules`,
  `dist`, `vendor`, `third_party`, `bower_components` y lo que excluye el indexado. Un alias o
  relativo sin archivo, o un módulo que ningún manifiesto declara, queda sin decidir, y una
  llamada a un nombre así importado nunca cae a `unique_name`. Un `tsconfig` o `package.json`
  ilegible toma de lo guardado solo esa parte (el resto, fresco). Los
  barrels se siguen hasta 4 niveles (`export { X as Y } from`, `export * from`; dos `export *` que
  dan el mismo nombre no lo exportan): `high` si todas las ramas se siguieron, `medium` si no. Un
  import `default` se toma por el nombre local (`medium`, el símbolo no guarda si es el default).
  Receptor pelado: tipo o objeto del archivo, import (clase → miembro; objeto → `obj.m`; `* as ns`
  → export del módulo), global de plataforma (`builtins`: DOM, Node, Jest), y si no, por nombre.
  Llamada pelada: función de un scope envolvente (nunca miembro de la clase), import, global, por
  nombre. Cadena: el retorno declarado de la firma (`(): Observable<T>` → `Observable`). Un
  `implements` de una interfaz externa no vuelve externo a un miembro ausente. Sin tipar campo por
  campo los receptores `member`. Medido: ver el Resultado de TASK-006.

**Implementado en TASK-007** (Python, Rust y Go; `resolve/{python,rust,go}.rs`, el entorno en
`resolve/env.rs`):
- *Común:* `scopes` y `types` con roles en los tres; un nombre pelado nunca es miembro
  (`implicit_this` falso) y un atributo de `self` que ningún scope liga es `member x this`, tipado
  por la declaración del campo en Rust y Go (`fields_by_link`). Un binding cuyo valor es una llamada
  guarda lo que devuelve (`typed <via> make()`, `Store.open()?` en Rust): el link lo tipa por el
  símbolo llamado — una clase o struct (su instancia), una función con tipo de retorno declarado
  (`Self` es el tipo de su `impl`, `?`/`.unwrap()` desenvuelven `Result`/`Option`), algo de afuera
  (`FromExternalCall`: como tras una llamada externa, `chain_external` `medium` si el nombre no
  está en el repo; el `T::new()` de un tipo externo, `medium`). De varios patrones que capturan un
  mismo nombre en un mismo scope gana el primero del JSON. Un import que no se puede seguir, o un
  nombre que el lenguaje no ve, queda sin decidir: **nunca** `unique_name` contra un homónimo; un
  paquete o crate declarado es externo aunque el repo tenga un homónimo.
- *Python:* módulos por ruta (`x.py`, `x/__init__.py`, `x.pyi`, directorio sin `__init__`),
  relativos al paquete; absolutos desde la carpeta del script (si no es paquete), la raíz, `src/` y
  la de cada manifest; un nombre de un módulo es lo que define o lo que importa (reexporte, 4
  niveles), o su submódulo si es paquete; `import *`. Llamada pelada: función anidada en scope, del
  módulo, importada, de un `import *`, builtin; una clase llamada es su instanciación (destino: la
  clase). `self.x = v` en un método tipa el atributo (`ctor_inject` desde un parámetro tipado de
  `__init__`); dos asignaciones que no coinciden lo dejan sin tipo. Externo: `platform_modules`
  (stdlib), `builtins`, o un paquete declarado. Una cadena se tipa por la anotación de retorno.
  Python no declara campos: `self.a.b()` sin tipo.
- *Rust:* crates por `Cargo.toml`; módulos por ruta bajo `src/` (`lib.rs`, `main.rs`, `mod.rs`
  nombran su directorio; tests, ejemplos y `src/bin/` son raíz propia) y por `mod` inline; `use`
  con su scope de módulo (el `use super::*` de un `mod tests` es de ese `mod`); `crate::`, `self::`,
  `super::`, `Self::`; `pub use` hasta 4 niveles; globs. Los `impl` de un tipo, en cualquier archivo
  de su crate, son sus hijos (dueño por resolución del nombre del `impl`, o el único tipo homónimo
  del crate) e `impl Trait for T` le da el supertipo; un método de trait sin cuerpo es símbolo. Una
  llamada por path (`path <prefijo>`): función asociada de un tipo (una variante de enum: el enum),
  función de un módulo. Externo: `std`/`core`/`alloc` (`platform_modules`), el prelude (`builtins`)
  o un crate que declara el `Cargo.toml` del archivo (o el del workspace). Un método derivable que
  falta en un tipo del repo (`clone`, `default`, `to_string`…) es externo `medium` (`inherited`).
  Macros (`format!`, `vec!`, `#[tokio::main]`) no son llamadas.
- *Go:* paquete = directorio (y su cláusula `package`); un import bajo el `module` de un `go.mod`
  (o un `replace` local) es del repo; uno cuyo primer segmento no tiene punto, la stdlib; cualquier
  otro, otro módulo cuando hay `go.mod` (sin `go.mod`, sin decidir). El receptor es `this`; un
  método en otro archivo del paquete tiene el tipo como dueño.
- *Revisión de TASK-007:* Rust — un crate del workspace cuenta solo si es el propio o una
  dependencia por path del `Cargo.toml` del archivo (por el path, así un `package =` llega; las
  `workspace = true`, por la raíz); una declarada del registry es externa aunque haya un homónimo;
  una no declarada, sin decidir. Varios bounds se prueban todos. Fuera de Java el método inherente
  gana al de un `impl` de trait y varios candidatos dan `medium`; dos ítems homónimos (`#[cfg]`)
  también. Un `use` dentro de una función es de ella. `let x = a.f()` hace de `x.g()` una cadena
  tras `a.f()` (también Go y Python); `Arc::new(x)` vale `x`; `T::default()` sin `impl`, `T`
  (`medium`). Un glob cuyo módulo reexporta el nombre desde afuera lo da externo, y una rama que no
  se sigue va antes que la externa (también en barrels TS y `import *` de Python). Go — `go.mod`
  ancestro para tratar como externo un import con punto, prefijo de módulo más largo, nombre local
  por la cláusula `package` del destino, homónimos por build tags en `medium`. Python — `Any`,
  `object`, `TypeVar` y uniones de varios tipos no tipan; `type[X]` es `X`; `Self` es la clase; un
  tipo que no resuelve no cae a `unique_name`; dos imports del mismo nombre local dan `medium`.

## DD-8 — `confidence` y `resolution`

- `confidence` es **ordinal y de tres valores** (high/medium/low), no un float: nadie sabe calibrar
  0.73 contra 0.81, y el consumidor (impact, traverse, PageRank) solo necesita filtrar y ponderar.
- `resolution` guarda el **porqué** para auditar (`gold-edges` de DD-1 reporta precisión por
  `resolution`). Si una regla resulta mala en campo, se baja su `confidence` sin tocar el resto.
- TASK-005 agrega `return_type` (evidencia fuerte: la cadena se tipó con el retorno declarado de
  un método del repo, y la arista tiene `dst_id`), `chain_external` (débil: después de una llamada
  externa, un nombre que el repo no tiene; siempre `medium`) y `discarded` (fila de una llamada
  descartada, fuera de conteos y lecturas), y usa `inherited` también para un miembro heredado de
  un supertipo externo (Panache, `Object`). Un getter de Lombok se resuelve con `dst_id` apuntando
  al **campo** (kind `field`) desde una arista `calls`, `medium`: quien lea aristas `calls` no debe
  suponer que el destino es invocable. Externos con evidencia indirecta y todo lo derivado de un
  `unique_name` o de un tipo ambiguo salen `medium`.
- TASK-007: una llamada a una clase de Python (o a una variante de enum de Rust) se resuelve con
  `dst_id` apuntando a la **clase** (o al enum) desde una arista `calls`; un método derivable de
  Rust que falta en el tipo es `inherited` + `external` `medium`; un receptor que vale lo que
  devuelve una llamada externa sigue las reglas de la cadena tras una externa (`chain_external`
  `medium`), y el de una llamada a una función del repo con retorno declarado sale `return_type`
  o con la etiqueta del binding (`local`, `field`).
- Revisión de TASK-007: en Rust y Go, un miembro que falta en un tipo con un supertipo externo
  (`impl Display`, un `sync.Mutex` embebido) es `inherited` + `external` `medium` (en Java y Python
  sigue `high`: Panache, una base de `Exception`); dos `impl` de trait con el mismo método, dos
  ítems homónimos bajo `#[cfg]` o dos archivos de un paquete Go con el mismo nombre (build tags)
  nunca dan `high`.
- `structural` es el valor de `contains` (DD-6), escrito al parsear: no es una regla de DD-7 y no
  pisa `same_file`, que es de la regla 6. El arnés no lo cuenta: `calls_por_confianza` filtra
  `kind = 'calls'` y `score.py` lee `graph_edges` (solo llamadas).
- Las tools exponen `min_confidence` (default: `medium` en `impact`/`traverse` y también en
  `get_references`). **TASK-008 (pedido de la revisión):** `get_references` lista por defecto `high`
  y `medium`, cada referencia con su `confidence`; `low` y las filas sin decidir solo con
  `min_confidence: "low"` (marcadas `undecided: true`), y lo que el default dejó afuera se cuenta en
  `below_confidence` — el borrador decía `low` por defecto.

## DD-9 — `external` y `from_test`

- `external = true` solo con evidencia: regla 9 de DD-7 (sin candidatos en el repo **y** tipo o
  paquete de afuera). Un nombre sin definición y sin evidencia queda `external = false`,
  `dst_id = NULL`, `low`: "no sé" no es "es de una librería".
- `from_test` = `path_kind(file) == PathKind::Test` (`devctx-core/src/kind.rs:98`), el mismo
  criterio que ya usa la penalización de PLAN-008. `symbols.is_test` igual. **Revisión de
  TASK-010:** además, en Rust, el código bajo `#[cfg(test)]`, en un `mod tests` o marcado
  `#[test]` dentro de un archivo que no es de test (`FileFacts.test_lines`). **Segunda revisión:**
  un `cfg` marca test solo si `test` es **obligatorio** (solo, o dentro de un `all(…)`): bajo
  `any(…)` o `not(…)`, como nombre de feature (`feature = "test-utils"`) o dentro de otra palabra
  (`attest`), nunca (`any(test, feature = "mock")` es producción con la feature). Se reconocen
  también `#[tokio::test(…)]`, `#[rstest]`, un comentario entre el atributo y el ítem y un
  `#![cfg(test)]` interno (todo el archivo). Falsos negativos que quedan, del lado seguro (código
  de test tomado por producción): `#[cfg(test)] mod tests;` cuyo cuerpo vive en otro archivo
  (`tests.rs` fuera de un directorio de test), macros que generan tests, y atributos de test de
  otros crates no listados. `EXTRACTOR_VERSION` 19.
- Las listas de plataforma (JDK, builtins de Python, `std`/`core` de Rust, `fmt`/stdlib de Go,
  globals de JS/DOM) viven en el JSON del lenguaje (`platform_prefixes`, `builtins`) para que las
  cubra el fingerprint. TASK-007 agrega `platform_modules` (módulos de primer nivel de la plataforma:
  la stdlib de Python, `std`/`core`/`alloc`/`proc_macro`/`test` de Rust); la stdlib de Go se
  reconoce por la regla del lenguaje (primer segmento sin punto). La evidencia de los paquetes de
  terceros viene de los manifests (`Cargo.toml`, `go.mod`, `pyproject.toml`/`setup.cfg`/
  `requirements*.txt`), que entran al fingerprint del entorno (DD-6). Revisión de TASK-007: en
  Python solo un nombre declarado exacto (normalizado, o de la tabla de alias) es evidencia fuerte;
  uno adivinado de una distribución (`python-utils` → `utils`) pierde con un módulo del repo de ese
  nombre, y un nombre de la stdlib que el repo también tiene queda sin decidir. En Go, la regla de
  "otro módulo" exige un `go.mod` sobre el archivo.

## DD-10 — Los lookups por nombre pasan a la tabla `symbols` (mismos contratos)

| Hoy (nombre/sufijo sobre `vectors.symbol` o `graph_edges`) | Después |
|---|---|
| `Store::symbol_definitions` (`store.rs:865-930`): exacto → `ends_with('.'|'::')` → regex sobre `grouped` | `symbols` por `name`/`qualified` → chunks por rango (DD-4). Misma firma y mismo orden (archivo, línea) |
| `Store::symbol_matches` (`store.rs:995-1032`, anclaje de `search_ranked`) | ídem con `SearchFilter`; `rank_definitions` (`devctx-search/src/lib.rs:715-728`) sigue decidiendo cuáles fijar; `ANCHOR_MAX` 3, `ANCHOR_TOKENS` 3 sin cambio |
| `Store::symbol_suggestions` (`store.rs:940-987`) | sobre `symbols.name/qualified` (sin `grouped`, que ya no aparece como nombre) |
| `Store::external_call_sites` (`graph.rs:227-258`) | `count(*) FROM edges WHERE external AND dst_name …`; el fallback por último segmento solo si ese segmento **no** existe en `symbols` (pendiente 2 de PLAN-008) |
| `Store::resolve_symbol` (`graph.rs:183-210`) | `symbols` por `name`/`qualified`; devuelve ids + nombre calificado; `resolved_symbols` del JSON sigue igual |
| `Store::find_references` (`graph.rs:304-332`) | `edges` por `dst_id` (más `name_only` del mismo nombre, marcadas), una fila por ocurrencia |
| `is_anchored_definition` (`state.rs:675-682`) | compara `symbol_id` del hit con los ids anclados en vez de sufijos |
| `split_file_symbol` (`state.rs:5622-5631`) | acepta `archivo::símbolo` solo si la parte izquierda existe como archivo en `symbols`/`file_index` o tiene una extensión de lenguaje registrada; `Foo.Bar::baz` deja de leerse como archivo `Foo.Bar` (pendiente 1 de PLAN-008) |
| `graph_defined_symbols` (`graph.rs:153-160`, vista web) | "definido" = existe en `symbols`, no "aparece como source" |

Contratos que **no** cambian: campos y semántica de `anchored`, `suggestions`, `external`,
`called_from`, `next_step`, `resolved_symbols`, `branch_fallback`, `omitted`/`next_offset`. Se
agregan (aditivos) `sym` (id hex) y `confidence` donde corresponda.

**Implementado en TASK-008, con estas diferencias:** las lecturas nuevas son funciones propias del
store (`crates/devctx-store/src/lookup.rs`: `lookup_symbols`, `symbol_code_chunks`,
`lookup_definitions`, `lookup_anchors`, `lookup_external`, `lookup_references`,
`lookup_graph_view`…) y no un reemplazo en sitio: `symbol_definitions`, `symbol_matches`,
`resolve_symbol`, `find_references`, `external_call_sites` y `graph_edges` quedan como el camino
viejo (DD-19) y `impact_analysis` los sigue usando hasta TASK-009. Toda lectura de aristas pasa por
`live_edges`. El MCP elige el camino por rama (`Reader::of`: vigente y con filas en `symbols`). El
anclaje fija **un chunk por definición** (`search_anchored` + `AnchorLookup::Symbols`) y marca
`anchored` por el id del chunk de una definición encontrada (con `sym`); un identificador que el
grafo no define se sigue buscando por nombre. `get_references` lista una fila por ocurrencia de toda
~~relación menos `contains`~~ — **revisión:** por defecto `calls` e `instantiates` (la semántica
de 0.9.0 era "llamadas", y `new Foo()` llama al constructor de `Foo`), con `via`; las demás
relaciones (`references`, `imports`, `inherits`, `implements`, o `all`) con el parámetro opt-in
`kinds`, que se **suma** al default (segunda revisión: pedir usos de tipo no saca las llamadas). Un externo no lleva `suggestions` (como 0.9.0) y un `archivo::nombre` sin resultado nunca
es externo. El anclaje lee el código de todas las definiciones de un identificador en **una**
consulta (`code_chunks_of`, `symbols` ⋈ `vectors` por archivo y rango) y reusa la elección de rama
del search. `split_file_symbol` exige `/` (que no empiece con `@`, un scope de npm) o una extensión
de lenguaje conocida (no consulta el índice). `read_symbol` sigue con un entry por chunk; campos e `impl` van
después del resto. Se agregan `kind`, `qualified`, `via`, `below_confidence`.

## DD-11 — `impact_analysis` por lotes, con tope y marcas

- Una consulta **por nivel de profundidad** sobre la frontera completa (tabla temporal o
  `dst_id IN (SELECT …)`), no una por nodo (`graph.rs:374-375`). Con profundidad 3 son 3+3
  consultas, no miles.
- Sobre ids: la expansión de un nombre pelado se hace una vez (`resolve_symbol`) y se reporta como
  hoy en `resolved_symbols`.
- Filtros por defecto: `min_confidence = medium`, `include_tests = false`, `include_external =
  false`. Con `include_*` se ven, siempre marcados.
- Tope de expansión por dirección (`max_nodes`, default 200) aplicado **antes** de calcular el
  siguiente nivel; lo que no entra se cuenta en `omitted` (contrato de PLAN-008 TASK-010) en vez de
  computarse y tirarse después (hoy el presupuesto recorta la salida pero no el cómputo).
- Orden dentro de cada nivel: `confidence` desc, luego `rank` desc (DD-12), luego nombre.
- Salida por nodo: `symbol`, `sym`, `file`, `line`, `depth`, `confidence`, `via` (kind de arista);
  `test`/`external` solo cuando son `true`.

**Implementado en TASK-009, con estas diferencias:** `crates/devctx-store/src/impact.rs`
(`Store::impact_graph`, `walk`) y `crates/devctx-mcp/src/state/impact.rs` (`impact_on`, mismo
`Reader` de TASK-008). `Store::impact_analysis` (0.9.0 sobre `graph_edges`) queda como camino
viejo, sin filtros ni tope, para que una rama vieja conteste igual que 0.9.0 más el `warning`.
- Aristas seguidas: `calls` e `instantiates` (el default de `get_references`), siempre por
  `live_edges`. Una sentencia por nivel; la frontera va como lista `IN (…)` hasta 512 ids (1 024 antes del NIT 1, abajo) y como
  un parámetro `UBIGINT[]` por encima (revisión, abajo; números en el Resultado de TASK-009).
- Un nodo que un filtro deja afuera (confianza, test, externo) no se recorre; se cuenta una vez
  por nodo (`below_confidence`, `excluded: {tests, external}`), y si un nivel posterior lo
  alcanza con una arista mejor, se muestra y deja de contarse. `confidence` y `via` son los de la
  mejor arista que lo alcanzó en su nivel. Un nodo sin definición (externo o sin decidir) no lleva
  `sym`/`file`/`line` y nunca se recorre; `file`/`line` son los de la definición.
- Tope: si un nivel no entra en lo que queda de `max_nodes`, se muestran los primeros (en el
  orden de DD-11), el resto se cuenta en `omitted` (`reason: "limit"`) y en `omitted_by_limit:
  {count, max_nodes, upstream|downstream: {count, depth}, hint}`, y no se lee ningún nivel más.
  Si un nivel llena el tope justo, el siguiente se lee (una consulta) solo para contarlo.
  `max_nodes = 0` = sin tope. `omitted.count` suma tope y presupuesto; `omitted_for_budget`
  sigue nombrando lo que cortó el presupuesto (mitad por dirección, como 0.9.0).
- Un nombre **sin definición** en el repo (que 0.9.0 recorría por nombre) toma como semilla sus
  sitios externos con la regla de `lookup_external` (como `get_references`): su upstream son los
  llamadores de esos sitios, no tiene downstream, y la respuesta agrega `external: true` +
  `called_from` + `next_step`, o `suggestions` si tampoco es externo (como `read_symbol`). Un
  nombre con definiciones usa solo esas (no suma los sitios externos homónimos), igual que
  `get_references`.
- La semilla que es el nombre pedido tal cual (`qualified` igual) no se reporta aunque se
  alcance por un ciclo; cualquier otra semilla sí (el par `Resource.m → Service.m`), como 0.9.0.
- La TUI local da la respuesta de la tool (`impact_at`): mismo lector, misma rama.
- **Revisión de TASK-009:** las llamadas **sin decidir** al nombre cuentan en upstream (por
  defecto en `below_confidence`, con `count_undecided_references`; con `low`, sus llamadores en el
  primer nivel, `undecided`, sin recorrerlos; nunca con `archivo::nombre`); con un nombre
  calificado toda semilla es el nombre pedido; `resolved_symbols` se compara con el nombre sin
  archivo; la respuesta trae `filters` (lo aplicado) para que un cliente sepa si un servidor viejo
  los ignoró. **Frontera:** lista `IN` hasta 1 024 ids y un parámetro `UBIGINT[]` por encima
  (`FrontierSql::Auto`), medido en backend-a (Resultado de TASK-009): el `IN` gana en recorridos
  reales y el parámetro desde 2 000 ids; la lista tipada y la tabla temporal pierden. Los ganchos
  de medición quedan detrás del feature `bench`.
- **Revisión de TASK-009, segunda ronda (hecha en el primer commit de TASK-010):**
  - *m-a:* `below_confidence` cuenta **solo símbolos**; las llamadas al nombre sin decidir van a
    un campo propio, `undecided_calls: {count, hint}`, contadas **en llamadas**, sin las de
    tests, las de llamadores que ya aparecen por otra arista ni las de la propia definición
    (antes `get` de backend-a mostraba 370 en `below_confidence`, unidades mezcladas). Con `low`
    se listan sus llamadores y el campo no aparece. Aditivo.
  - *NIT 1:* `AUTO_IN_MAX` baja de 1 024 a **512**. Todo recorrido real medido (frontera máxima
    373 ids) sigue con `IN`; en 1 000 ids el parámetro ya ganaba (24 contra 36 ms). Re-medido en
    TASK-010 con una consulta de nivel sobre fronteras sintéticas de backend-a (p50 upstream,
    `IN` / `UBIGINT[]`, máquina con carga): 256 ids 21,4 / 15,2 ms; 373: 25,8 / 17,2; 512:
    25,7 / 19,0; 768: 26,9 / 18,3; 1 024: 28,2 / 18,6. El parámetro gana en todas, también
    debajo de 512; el umbral queda en 512 porque los recorridos reales de TASK-009 (que encadenan
    varios niveles y abren el store) favorecieron al `IN` por debajo de ~400 ids.
  - *NIT 2:* un llamador listado `undecided` en el nivel 1 que un nivel posterior alcanza con una
    arista decidida deja su lugar: se muestra decidido, en esa profundidad, y se recorre.
  - *NIT 3:* en el camino viejo (índice de otro extractor o sin filas), una consulta con filtros
    agrega a `warning` que no se aplicaron también en `Backend::Local` (y en la API y el CLI
    local, que comparten `impact_on`); el aviso no se duplica si un cliente remoto lo vuelve a
    poner.
- **Dispatch por interfaz (TASK-017, decisión del usuario del 2026-10-08):** cada nodo de cada
  nivel se expande a sus métodos override-equivalentes por las aristas `inherits`/`implements`
  resueltas de `live_edges` (mismo nombre, kind invocable, aridad compatible; `private`/`static`
  no; transitivo hasta 4 niveles de supertipos; Go afuera). Upstream, `Impl.m` alcanza a los
  llamadores de los métodos que sobrescribe, a la profundidad de un llamador directo; downstream,
  un método de interfaz o abstracto alcanza a sus implementaciones un nivel debajo de él. El nodo
  lleva `via: "dispatch"` y `through`; confianza `min(medium, arista original, arista de herencia
  más débil)` (downstream, tampoco más que lo que alcanzó al origen): nunca `high`, un `low` no
  sube; a igual confianza gana la directa; entre sobrecargas de igual aridad en un tipo, la de
  tipos simples exactos. Tope de 16 equivalentes por nodo (producción antes que tests; con tests
  excluidos, estos no entran al tope), lo demás **solo** en `omitted_by_limit.dispatch: {count,
  max_per_node}` (no en `omitted_by_limit.count` ni en `omitted.count`, que cuentan nodos), una vez
  por método. Activo por defecto; `dispatch: false` lo apaga, `min_confidence: high` lo deja afuera
  sin leerlo, y `filters.dispatch` dice si aplicó. **Costo (revisión, MAJOR 2):** las aristas de
  supertipo se leen una vez por llamada como pares de ids; por nivel, a lo sumo una sentencia y solo
  si la frontera tiene un método de un tipo con jerarquía, que lee los métodos con su nombre en tipos
  del grafo; nada se lee apagado, con `high` o sin semillas. Medido en un fixture sintético de 1 500
  aristas de supertipo y 20 120 métodos (`dispatch_bench`, p50 / p95 ms, antes → después): con
  dispatch upstream 40,5 / 43,6 → 25,5 / 27,4; semilla sin jerarquía 30,6 / 34,0 → 14,0 / 18,1 (sin
  dispatch 11,6 / 14,2); `min_confidence: high` 34,1 / 38,2 → 8,3 / 9,9; interfaz con 500
  implementaciones 34,7 / 39,1 → 37,5 / 43,8. En backend-a/b, p95 ≤ 118 ms en ronda limpia y ≤ 242
  con picos de carga (Resultado de TASK-017). Para TS hizo falta que el método de una interfaz sea
  símbolo (`EXTRACTOR_VERSION` 20). Downstream también despacha desde un método concreto a sus
  overrides (dispatch virtual). ~~Pendiente para TASK-013~~ — **hecho en TASK-013:** en TS/JS un
  implementador con menos parámetros despacha (aridad del impl ≤ la del supertipo; con más
  obligatorios, no); lo que corta el tope de profundidad 4 se cuenta en
  `omitted_by_limit.dispatch` (`beyond_depth`, `max_depth`, y dentro de su `count`), sin sentencias
  nuevas; el escenario (e) por `impact_graph` (incremental = completo tras cambiar un
  `implements`); tests de diamante (gana la cadena más segura), ciclo, TS con varios `implements`,
  blanket impl de Rust y los asserts BFS de 0.9.0 con el índice de supertipos leído. **Revisión de
  TASK-017 (pedido):** cuando el dispatch no se evaluó, la respuesta lo dice con una nota constante
  leída de los filtros: `dispatch: {"evaluated": false, "reason": "dispatch:false" |
  "min_confidence high"}` (aditivo; también en `traverse` cuando sigue `calls`).

## DD-12 — PageRank: global al indexar, personalizado al consultar

- **Grafo**: nodos = símbolos de la rama (sin externos, que no tienen fila); aristas = `edges` con
  `dst_id` no nulo. Se agrega por par `(src, dst, kind)` con multiplicidad.
- **Pesos por kind**: `calls` 1.0, `instantiates` 1.0, `inherits`/`implements` 0.8, `references`
  0.5, `imports` 0.3, `contains` 0 (no aporta centralidad; sirve a `traverse` y al repo-map).
  **Por confianza**: high 1.0, medium 0.6, low 0.2. **Multiplicidad**: `sqrt(n)` (patrón de Aider:
  un helper llamado 300 veces no debe comerse el ranking). **Tests**: aristas `from_test` × 0.1
  (los tests sí indican qué es importante, pero el 57 % de las aristas no puede dominar).
- **Global** (`symbols.rank`): damping 0.85, iteración de potencia, tolerancia 1e-6, máx. 50
  iteraciones, nodos colgantes redistribuidos uniforme. Implementación propia (~60 líneas): no se
  agrega `petgraph` ni otra dependencia. Se recalcula entero al final del link pass. `in_degree` =
  llamadas entrantes no-test con `confidence ≥ medium`.
- **Personalizado** (repo-map, DD-14): misma matriz, vector de personalización con los
  archivos/símbolos de foco; se calcula al consultar sobre una adyacencia cacheada en `AppState`
  por `(repo, branch, generación del índice)` y se invalida al terminar un `index`. Presupuesto: <
  200 ms en backend-a con caché caliente; la primera carga < 1 s.

**Alternativa descartada:** PageRank sobre archivos (Aider). Con `symbols` se puede rankear a nivel
símbolo y agregar a archivo sumando; al revés no.

**Implementado en TASK-010 (parte global), con estas diferencias:**
- `crates/devctx-index/src/pagerank.rs` (`pagerank`, `rank_branch`) y, en el store,
  `branch_rank_edges` (sobre `live_edges`: una descartada nunca cuenta; agregadas por
  `(src, dst, kind, confidence, from_test)` con su `n`), `write_branch_ranks` (tabla temporal y un
  `UPDATE`) y `chunk_ranks` (DD-4). El link pass que **completa** rankea la rama al final; uno
  cortado deja el pase debido y la próxima corrida rankea. **`LINK_VERSION` 21:** un índice
  existente se relinkea y rankea en su próxima corrida, sin reindexar ni embeber.
- **Normalización:** un nodo reparte su rank por `peso / Σ kind × sqrt(n)` de sus aristas. Kind y
  multiplicidad deciden **cómo** reparte; confianza y test deciden **cuánto**: lo que no reparte
  se redistribuye uniforme, como el de un nodo colgante. Con la normalización clásica (por la
  suma de los pesos) los factores de confianza y de test se cancelan en un nodo cuyas aristas los
  comparten todas (un método de test: todas sus aristas son `from_test`), y el ×0.1 no tendría
  efecto.
- `in_degree` = ocurrencias de `calls` entrantes, no-test, `high`/`medium`. `rank` es una
  probabilidad (suma 1 por rama). Determinista: nodos y aristas se ordenan antes de iterar (test
  que falla si se quita el orden).
- Costo medido (Resultado de TASK-010): backend-a, 22 876 símbolos, 69-165 ms por pase (cálculo y
  escritura); DevCtxEngine 58 ms; backend-b 21 ms.
- **Revisión de TASK-010:** (MAJOR 4) en Rust, el código de test dentro de un archivo que no es de
  test —`#[cfg(test)]`, `mod tests`, `#[test]`— es test: `FileFacts.test_lines`, sus símbolos
  `is_test` y sus llamadas `from_test` (DD-9 se extiende; `EXTRACTOR_VERSION` 18). Antes un helper
  usado solo desde tests en línea (`Store.open_in_memory`, 182 llamadas) entraba al top del rank.
  (MINOR 5) solo se escriben los símbolos cuyo `rank`/`in_degree` cambió: un pase sobre el mismo
  grafo no escribe nada. (MINOR 6) la lectura por chunk es el **percentil dentro de la rama**,
  no la probabilidad (que favorecía a la rama chica en un pool de varias ramas). (MINOR 8) tope
  de **100** iteraciones (no 50): `LinkStats.{rank_iterations, rank_converged}` y la línea del
  link pass lo dicen.

## DD-13 — Cómo compone con el ranking actual

Orden del pipeline de `search_ranked` (`devctx-search/src/lib.rs:269-357`) con la señal nueva:

```
retrievers (vector, keyword) ─▶ RRF ─[+ lista de centralidad, peso w]─▶ filtros duros (kind/include_tests)
   ─▶ penalización por posición (apply_penalty) ─▶ dedup_hits ─▶ rerank opcional ─▶ anclaje por identificador
```

- La centralidad entra como **tercera lista de la RRF**, ponderada: se ordena el pool de candidatos
  por `rank` del símbolo más interno de cada hit (DD-4) y se suma `w / (RRF_K + pos + 1)` con
  `w = 0.3` inicial (calibrado en TASK-010 con el arnés). Es la misma moneda que ya usa la fusión
  (posiciones, no scores), y no puede promover un hit que ningún retriever trajo (solo reordena el
  pool).
- **Solo en `Hybrid`** (que es el modo de `build_context`, `state.rs:4137-4146`). `Vector` (default
  de `search` CLI/MCP) queda intacto: su orden es contrato medido en PLAN-008 TASK-016.
- La penalización de tests/docs sigue después y por posición; el anclaje sigue al final y gana
  siempre (una definición nombrada en la consulta no se desplaza por centralidad).
- `raw_score` no cambia de semántica: es el del retriever/RRF de vector+keyword, sin la centralidad,
  para que la selección de miembro de grupo (`member_scores`, `state.rs:4557-4572`) siga comparando
  cosenos.

**Implementado en TASK-010, con estas diferencias:**
- `RankOptions.centrality` (`search.centrality_weight`); `with_centrality` corre sobre la salida de
  la RRF, reescribe `score` (la fusión + la centralidad, para que el orden y los scores sigan
  monótonos) y deja la fusión de vector + keyword en `raw_score`. ~~Un hit sin rank va al final de
  la lista de centralidad~~ — revisión: un hit sin rank no recibe bonus (abajo). Sin ningún rank en el pool (índice anterior,
  rama sin rankear, chunks de ningún símbolo) es la misma lista, sin tocar.
- **Hit anclado:** el anclaje le daba el score más alto de la respuesta, que con centralidad ya no
  es de un retriever. Ahora su retriever score es el mejor retriever score de la respuesta (y su
  `score`, el más alto, como antes), así la selección de miembro nunca ve centralidad, ni un
  logit del cross-encoder ni el clamp de la penalización. Sin centralidad, sin rerank y sin un hit
  degradado arriba, es el mismo número que antes.
- El MCP (`search`, `build_context`) usa la centralidad **solo con un grafo de símbolos vigente**
  (`Reader::SymbolGraph`): una rama de otro extractor, fuera de paso o sin filas fusiona solo
  vector + keyword. ~~El CLI local y la TUI toman el peso de la config~~ — revisión: la misma
  regla (`local_centrality`).
- ~~**Calibración**: `w` = 0,3 se queda~~ — **revisión de TASK-010 (MAJOR 1, decisión de la
  revisora): default `w` = 0, opt-in en 0.10.0.** Con 12 de 30 casos (sin backend-a, el de MRR
  híbrido más alto), el brief pasa 11 → 12 por un solo caso (el ruido es ±1), el MRR sube solo por
  backend-b y Hit@10 de DevCtxEngine baja 9 → 8. La infraestructura queda para repo_map y el
  ego-graph. La tabla de calibración queda en el Resultado como evidencia.
- **Pendiente explícito de TASK-016 (MAJOR 2):** re-evaluar `w` con los 30 casos **y el corte por
  idioma**. TASK-001 midió híbrido < vectorial en español (Hit@5 6/13 contra 8/13). En la
  calibración, español (4 casos: 3 de DevCtxEngine, 1 de backend-b): Hit@5 híbrido 1/4 en los
  cuatro `w`; MRR 0,309 / 0,313 / 0,315-0,327 / 0,324 (`w` 0 / 0,15 / 0,3 / 0,5); brief 3/4 →
  4/4 desde 0,3 (el caso de backend-b); Hit@10 de DevCtxEngine 2/3 → 1/3 desde 0,3 (el caso 7 baja
  8 → 11). Inglés (8 casos): Hit@5 7/8 → 8/8, MRR 0,706 → 0,813, brief 8/8. Con 4 casos en
  español no se puede concluir; encender la centralidad exige no empeorar el corte en español.
- **Revisión de TASK-010:** (MAJOR 3) un hit sin rank no recibe bonus (antes quedaba al final de
  la lista con `w / (K + pos + 1)`, lo que degradaba de forma sistemática docs, config, chunks de
  archivo y memorias, ~13 puestos en un pool de 80). Aun con bonus 0 quedan **relativamente**
  degradados: todo hit con rank recibe algo positivo y ellos nada. Es otra razón para que sea
  opt-in; TASK-016 lo mide (paso 3b). (MINOR 9) la lista se arma después de los
  filtros duros. (MINOR 2) si la centralidad no se aplicó, el hit anclado tiene el score de
  siempre (sin `raw_score` propio), también con reranker. (MINOR 1) el CLI local y la TUI aplican
  la misma regla que el MCP (`local_centrality`: grafo de símbolos vigente). (MINOR 4)
  `SearchCfg::centrality` acota a `[0, 1]` y usa 0 si no es finito, con aviso al cargar. (MINOR 3)
  encendida, el `score` híbrido de un hit con rank sube su bonus y aparece `raw_score`: no es
  aditivo, por eso es opt-in y está documentado.

## DD-14 — Tool `repo_map`

`repo_map(budget=1024, focus_files=[], focus_symbols=[], query=None, project=None)` →
texto (como `build_context`, no JSON: va al contexto del modelo).

- Personalización (pesos de Aider adaptados): símbolos mencionados en `focus_symbols` o como
  identificador en `query` (`identifier_tokens`, `devctx-search/src/lib.rs:600-641`) × 10;
  archivos en `focus_files` × 50 como referenciadores; nombres con prefijo `_` × 0.1; nombres
  definidos en > 5 archivos × 0.1. Sin foco → PageRank global.
- Selección: búsqueda binaria sobre cuántas firmas entran en `budget` (tokens = chars/4, la misma
  estimación de todo el producto), tolerancia 15 %. Agrupado por archivo, archivos en orden de
  rank agregado, dentro de cada archivo por línea, con la jerarquía de contenedores:

```
src/main/java/com/example/app/service/OfficeService.java
│ class OfficeService
│   public Uni<OfficeDTO> actualizar(Long idOffice, OfficeRequestDTO request, String usuario)
│   public Uni<List<OfficeDTO>> listar()
⋮
```

- Excluye tests y externos siempre (no tienen firma en el repo / no orientan). Cierra con
  `[devctx] N símbolos más no entraron en el presupuesto`.

## DD-15 — Ego-graph en `build_context`

- Para los **primeros 5 hits de código** que entraron al brief, sus vecinos a 1 salto
  (`calls`/`instantiates` en ambas direcciones, `inherits`/`implements` hacia arriba),
  `confidence ≥ medium`, sin tests ni externos, ordenados por `rank`, hasta 4 por hit.
- Se renderizan como **firmas**, nunca cuerpos, en una sección `## Related (signatures)` con la
  relación (`called by`, `calls`, `implements`), y tienen un tope de **20 % del presupuesto**. Lo
  que no entra se cuenta en el `[devctx] omitted` existente (`state.rs:4295-4300`).
- Va **después** del código y **antes** de "Recorded against this code" (`compose_context`,
  `state.rs:4205-4303`): primero lo que contesta, después lo que lo rodea.
- `k` configurable (`graph_hops`, default 1, máx. 2) y desactivable (`related: false`).

## DD-16 — Tool `traverse`

`traverse(symbol | sym, edge_types=["calls"], direction="out"|"in"|"both", depth=1 (máx. 4),
limit=50, min_confidence="medium", include_tests=false, include_external=false, offset=0)` → JSON
`{root, nodes:[{sym, symbol, kind, file, line, depth}], edges:[{from, to, kind, confidence, line}],
total, next_offset?, omitted?, branch_fallback?}`.

- `symbol` admite nombre pelado, calificado o `archivo::símbolo`; si es ambiguo, devuelve los
  candidatos (como `resolved_symbols`) y recorre desde todos, salvo que se pase `sym`.
- `edge_types ⊆ {calls, instantiates, imports, inherits, implements, contains, references}`.
- Implementado con la misma expansión por niveles de DD-11 (una consulta por nivel).
- Contrato de paginación y omitidos de PLAN-008 TASK-010 (`devctx_core::hits`).

**Implementado en TASK-013, con estas diferencias:** el parámetro es `kinds` (separadas por coma o
`all`, como en `get_references`; default `calls`) y no `edge_types`; `direction` default `out`.
`crates/devctx-store/src/traverse.rs` (`Store::traverse`, `traverse_walk`, `traverse_roots`) reusa
la expansión de DD-11 (`edge_level`: la consulta de nivel de `impact` con las relaciones como
parámetro; una sentencia por nivel y dirección, `FrontierSql::Auto`, `live_edges`) y, si se sigue
`calls`, el `DispatchIndex` de TASK-017 (las filas de otras relaciones hacia un equivalente no son
dispatch). `crates/devctx-mcp/src/state/traverse.rs` (`TraverseQuery`, `traverse_on`), `do_traverse`/
`traverse_at`, `Backend::traverse`, `POST /traverse`, `devctx traverse`. Salida `{root, candidates?,
nodes, edges, total, next_offset?, omitted?, partial?, filters, below_confidence?, excluded?,
omitted_by_limit?, dispatch?, omitted_for_budget?, branch_fallback?}`:
- nodos `{sym, symbol, kind, file, line, depth, confidence, via, through?}` + `test`/`external`/
  `undecided` cuando son verdaderos; el nombre pedido nunca se lista y, de las definiciones de un
  nombre pelado, una alcanzada desde otra sí (la regla de `impact`); orden de DD-11 por profundidad;
- aristas: las que alcanzaron a cada nodo listado **en su profundidad**, una por par, relación y
  dispatch (`occurrences` cuando son varias), en su sentido propio, `from`/`to` como `sym` (el
  nombre para un extremo sin definición); una arista de dispatch es `kind: "calls", via:
  "dispatch"`;
- paginación: `limit` (50) desde `offset`; el recorrido **deja de leer niveles** en cuanto tiene
  más nodos de los que la página necesita (`offset + limit`), y lo dice con `partial: {from_depth}`
  (`total` cuenta entonces lo leído; la página siguiente lee más). Un nivel siempre se lee entero,
  así que las páginas son estables;
- `symbol` acepta además la ruta de un archivo (su símbolo de archivo, de donde salen los
  `imports`); `sym` (hex) elige una raíz; un nombre ambiguo recorre desde todas (`candidates`);
- sin camino 0.9: rama vieja o sin filas → error que pide `devctx index --full`; un `serve`
  anterior responde 404 y el cliente (MCP remoto y CLI) dice que hay que reiniciarlo;
- no lista los llamadores de llamadas **sin decidir** al nombre (eso es de `impact_analysis` y
  `get_references`): `traverse` sigue aristas por id.

## DD-17 — Vista `skeleton`

`skeleton(file, project=None)` → texto: los símbolos del archivo desde `symbols` en orden de línea,
indentados por `parent_id`, cada uno con su `signature`, `kind` y rango `[l1-l2]`, sin cuerpos;
los imports estructurados en una línea resumida. Firma normalizada = texto desde el inicio de la
definición hasta antes del cuerpo (`{`, `:` en Python, `=>`/`{` en arrow), colapsando espacios, con
tope de 200 caracteres. La doc aclara que sirve para **localizar**, no como contexto de edición
(evidencia en el master §2).

## DD-18 — Selección de miembro de grupo

Hoy `build_context` de grupo puntúa miembros por cosenos crudos y desempata con
`defines` (`/symbol/<name>` por cada identificador, `state.rs:5122-5128`, `PICK_IDENT_TOKENS` 2).
**Decisión:** el `/symbol` pasa a resolver contra `symbols` (más barato y sin falsos positivos de
sufijo); la fórmula de selección **no cambia** en este plan. Se mide en TASK-012 una variante con
bonus por `rank` del símbolo definido; solo entra si el arnés muestra mejora en los 7 casos de
PLAN-008 TASK-016 sin romper ninguno. Razón: la selección se calibró en campo (7/7) y es barata de
romper.

## DD-19 — Rollout: `EXTRACTOR_VERSION` 1 → 2 y reindex guiado

- TASK-003 sube `EXTRACTOR_VERSION` (`registry.rs:93`). El mecanismo de PLAN-008 TASK-007 ya hace
  el resto: `Store::extractor_stale` (`crates/devctx-store/src/state.rs:183-188`) marca la rama,
  `index_status` y las tools de grafo avisan, `branch_with_same_content` deja de copiar filas de
  ramas viejas (`crates/devctx-store/src/state.rs:247-278`), y solo `index --full` re-sella
  (`pipeline.rs:542-562`).
- **Lecturas sobre una rama no reindexada** (sin filas en `symbols`): las tools existentes
  (`read_symbol`, `get_references`, `impact_analysis`, anclaje) usan el camino viejo sobre
  `graph_edges`/`vectors` y lo dicen (`extractor_stale` ya está en la respuesta; TASK-008: una rama
  vigente pero sin filas en `symbols` —un repo sin lenguajes parseables— también, con un `warning`
  propio; un error al leer `symbols` lleva otro, que no pide reindexar; y `search` en
  `keyword`/`hybrid` avisa cuando su anclaje fue por nombre); las tools nuevas
  (`traverse`, `repo_map`, `skeleton`) contestan un error con la acción: "corré `devctx index
  --full`". Se borra el camino viejo en el plan siguiente, junto con `graph_edges`.
- **Downgrade** a 0.9.0: las tablas nuevas quedan huérfanas e inocuas; `graph_edges` está al día
  (DD-2); 0.9.0 ve un fingerprint `v2-…` distinto del suyo y pide reindex: honesto.

## DD-20 — Reindex barato: reusar vectores por `content_hash` del chunk

El costo de reindexar es el embedding (~1 h por ~1400 archivos en 8 cores, `AGENTS.md` §6;
frontend > 25 min con la máquina cargada, PLAN-008 TASK-016). Un cambio de extractor que no
cambia el texto de un chunk no cambia su vector.

**Decisión.** En `index_file`, antes de embeber, se leen los vectores existentes del archivo en esa
rama (`content_hash → vector`, columna que ya existe en `vectors`, `schema.rs:93`) y solo se embeben
los chunks cuyo hash no está. Aplica a `--full` y a incrementales (editar un archivo re-embebe solo
sus chunks cambiados). Guardas: mismo modelo y dimensión que los registrados en `index_state`
(`model_name`, `model_dimension`, `schema.rs:230-241`); si no coinciden, no hay reuso.

Efecto esperado: el `index --full` que exige este plan cuesta parse + link + los chunks nuevos
(arrow-consts, constructores), no el repo entero. Meta en el master §6.

## DD-21 — Presupuesto de rendimiento

| Recurso | Presupuesto | Dónde se mide |
|---|---|---|
| `index --full` desde cero | ≤ +10 % sobre 0.9.0 (el embedding domina; parse + link ≤ +30 % de la fase de parse) | TASK-001 vs TASK-016 |
| `index --full` tras subir `EXTRACTOR_VERSION` (con DD-20) | ≤ 20 % del tiempo de 0.9.0 desde cero | TASK-002, TASK-016 |
| Link pass incremental (1 archivo) | < 2 s en backend-a | TASK-005 |
| Tamaño del DB | ≤ +30 % en **bloques usados** (`total_blocks - free_blocks` de `pragma_database_size()`), índice desde cero, con `graph_edges` todavía escrito | TASK-003, TASK-016 |
| Pico de RAM del indexado (`VmHWM`) | ≤ +200 MB sobre 0.9.0 en frontend | TASK-005, TASK-016 (coordina con PLAN-010) |
| `impact` sobre `get`/`map` | p95 ≤ 300 ms (serve caliente) | TASK-009 |
| `traverse` depth 2, `repo_map` 1k tokens | p95 ≤ 200 ms / ≤ 500 ms | TASK-011, TASK-013 |
| `build_context` | ≤ +150 ms p50 por el ego-graph y la centralidad | TASK-012 |

**Tamaño: decidido (usuario, revisión de TASK-003).** El presupuesto se mide en bloques usados, no
en bytes del archivo: TASK-003 dio +7,2 % (111 → 119 bloques de 256 KiB en DevCtxEngine). Los
bloques libres se **miden y no se tocan**: no se compacta al final del `--full` y `graph_edges` se
sigue escribiendo (el downgrade a 0.9.0 depende de él). Medido en los follow-ups de TASK-003
(DevCtxEngine, sandbox, índice 0.9.0 → `--full` del binario nuevo): 111 usados / 0 libres →
167 / 75 (27,7 → 60,5 MiB; hipótesis no verificada: la reescritura deja filas borradas y bloques libres); un segundo
`--full` y un incremental que no escriben nada no los cambian (167/75, 166/76); un `--full` que
reescribe 40 archivos los **sube** (178 usados / 95 libres, 68,2 MiB): DuckDB no reutiliza esos
bloques libres por sí solo en este patrón de escritura. Lo vigila TASK-016.
