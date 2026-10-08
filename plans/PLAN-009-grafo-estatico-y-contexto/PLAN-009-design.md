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
  que se mueve). Pase completo si lo escrito supera 1/5 de la rama, o si el `tsconfig` (alias) no
  es aquel con que se enlazó la rama (`index_meta.link_tsconfig`). Un test fija que el incremental
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
  tipo. Una unión con `null`/`undefined` es el otro miembro. Cada fila `imports` lleva en `hint`
  `export` (re-export de barrel) y `as <local>`.
- *Link:* el especificador se resuelve relativo (`.ts`, `.tsx`, `.js`→`.ts`, `/index.*`), por el
  patrón `paths` más largo (exacto primero) o bajo `baseUrl` del `tsconfig.base.json` (o
  `tsconfig.json`) de la raíz, un nivel de `extends` relativo; lo que no es del repo es paquete →
  externo, salvo que el nombre importado lo defina en su nivel superior algún archivo TS/JS del
  repo (DD-9: sin evidencia, no externo); un alias o relativo sin archivo queda sin decidir. Los
  barrels se siguen hasta 4 niveles (`export { X as Y } from`, `export * from`; dos `export *` que
  dan el mismo nombre no lo exportan): `high` si todas las ramas se siguieron, `medium` si no. Un
  import `default` se toma por el nombre local (`medium`, el símbolo no guarda si es el default).
  Receptor pelado: tipo o objeto del archivo, import (clase → miembro; objeto → `obj.m`; `* as ns`
  → export del módulo), global de plataforma (`builtins`: DOM, Node, Jest), y si no, por nombre.
  Llamada pelada: función de un scope envolvente (nunca miembro de la clase), import, global, por
  nombre. Cadena: el retorno declarado de la firma (`(): Observable<T>` → `Observable`). Un
  `implements` de una interfaz externa no vuelve externo a un miembro ausente. Sin tipar campo por
  campo los receptores `member`. Medido: ver el Resultado de TASK-006.

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
- `structural` es el valor de `contains` (DD-6), escrito al parsear: no es una regla de DD-7 y no
  pisa `same_file`, que es de la regla 6. El arnés no lo cuenta: `calls_por_confianza` filtra
  `kind = 'calls'` y `score.py` lee `graph_edges` (solo llamadas).
- Las tools exponen `min_confidence` (default: `medium` en `impact`/`traverse`, `low` en
  `get_references`, porque "dónde se nombra esto" tolera ambigüedad si se la marca).

## DD-9 — `external` y `from_test`

- `external = true` solo con evidencia: regla 9 de DD-7 (sin candidatos en el repo **y** tipo o
  paquete de afuera). Un nombre sin definición y sin evidencia queda `external = false`,
  `dst_id = NULL`, `low`: "no sé" no es "es de una librería".
- `from_test` = `path_kind(file) == PathKind::Test` (`devctx-core/src/kind.rs:98`), el mismo
  criterio que ya usa la penalización de PLAN-008. `symbols.is_test` igual.
- Las listas de plataforma (JDK, builtins de Python, `std`/`core` de Rust, `fmt`/stdlib de Go,
  globals de JS/DOM) viven en el JSON del lenguaje (`platform_prefixes`, `builtins`) para que las
  cubra el fingerprint.

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
  `graph_edges`/`vectors` y lo dicen (`extractor_stale` ya está en la respuesta); las tools nuevas
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
