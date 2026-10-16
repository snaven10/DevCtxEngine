# Grafo de símbolos

> 🇬🇧 [Read in English](../../03-core-concepts/symbol-graph.md)

Un grafo sobre los símbolos del código indexado: quién llama, construye,
importa, extiende o usa qué — y hasta dónde llegaría un cambio.

---

## Qué es

Durante el indexado, tree-sitter parsea cada archivo soportado y extrae sus
símbolos (clases, funciones, métodos, campos…) y cada relación que el archivo
escribe: llamadas, instanciaciones, imports, supertipos, usos de tipo y qué
símbolo contiene a cuál. Al final de cada corrida, un **link pass** resuelve
cada relación a la definición que alcanza, entre archivos, y registra qué tan
seguro está y por qué.

```bash
devctx symbol authenticate                 # la definición y su código
devctx impact authenticate                 # llamadores y llamados transitivos
devctx traverse OrderDto --kinds references --dir in   # quién usa un tipo
devctx map --query search_ranked           # un mapa rankeado del repositorio
devctx skeleton src/api/client.ts          # la estructura de un archivo
```

Los agentes usan `read_symbol`, `get_references`, `impact_analysis`,
`traverse`, `repo_map` y `skeleton`; cuál cuándo, [más abajo](#qué-herramienta-cuándo).

## Por qué existe

La búsqueda semántica responde *"¿dónde está el código sobre X?"*. No puede
responder *"¿qué se rompe si cambio esto?"*, porque esa pregunta es de
estructura, no de significado — y la respuesta incluye código que nunca menciona
X.

El grafo responde la pregunta estructural sin ranking: una arista existe o no
existe. Pero **su ausencia no prueba nada** — ver los límites más abajo, porque
esa distinción es la que evita un refactor roto.

## Qué hay en el grafo

Esto describe un índice hecho por 0.10 o posterior. Uno hecho antes solo tiene
las aristas de llamada de 0.9 (`graph_edges`, más abajo) y se responde a la
manera vieja hasta que se reconstruye: ver [Mantener el índice al día → Pasar a
0.10](../13-mantener-el-indice-al-dia.md#pasar-a-010-el-grafo-de-símbolos).

### Símbolos

La tabla `symbols` tiene una fila por definición, por rama:

| Campo | Contiene |
|---|---|
| `id` | Un id estable, que se muestra como `sym` (16 dígitos hex): un hash de repositorio, archivo, clase de kind, nombre calificado y un desambiguador (los tipos de parámetro de un método Java, el trait y los argumentos de tipo de un `impl` de Rust). No lleva rama: el mismo método tiene el mismo `sym` en todas las ramas. Cambia si cambian el archivo, el scope que lo envuelve, el nombre o (Java) los tipos de parámetro — para "quién lo llama" es otro símbolo |
| `kind` | `file`, `module`, `class`, `interface`, `enum`, `record`, `struct`, `trait`, `impl`, `type`, `function`, `method`, `constructor`, `field`, `const` |
| `name`, `qualified` | `update` y `OrderService.update`: todo scope envolvente, sin el paquete (`Outer.Inner.m`, `tests.helper` en un `mod tests` de Rust, `N.f` dentro de un `namespace N` de TypeScript) |
| `parent_id` | El contenedor — clase, `impl`, módulo — o el símbolo de archivo en el nivel superior |
| `package` | Paquete Java o Go, módulo Python, ruta de módulo Rust, ruta de módulo TypeScript |
| `signature` | La cabeza de la definición, hasta su cuerpo, en una línea: a lo sumo 200 caracteres después de sus anotaciones iniciales, terminada en `…` cuando se corta |
| `exported` | Visible fuera de su paquete o módulo (`public`, `export`, `pub`, mayúscula en Go, sin `_` en Python) |
| `rank`, `in_degree` | El PageRank global de la rama y las llamadas entrantes que no son de test, de confianza `high`/`medium` ([Búsqueda → Centralidad](busqueda.md#centralidad-solo-híbrida-opt-in)) |
| `is_test` | Código de test: un archivo de test, o código Rust `#[cfg(test)]`, `mod tests`, `#[test]` dentro de un archivo de código |

Cada archivo tiene además un **símbolo de archivo** (`kind = file`): la fuente
de las llamadas hechas fuera de toda función (un script de Python,
`export const x = f()`), de los imports del archivo, y la raíz de lo que
contiene.

### Aristas

La tabla `edges` tiene una fila por **ocurrencia** — dos llamadas desde un
mismo método al mismo destino son dos filas — de siete relaciones:

| Kind | De → a | Sale de |
|---|---|---|
| `calls` | llamador → llamado | una llamada (en Python, llamar a una clase es un `calls` a la clase) |
| `instantiates` | el código → el tipo construido | `new Foo()` (Java, TS/JS), una expresión de struct en Rust, un literal compuesto en Go |
| `imports` | símbolo de archivo → lo que importa | `import`, `use`, `from … import`, re-exports |
| `inherits` | tipo → supertipo | `extends`, bases de Python, un supertrait de Rust, un tipo embebido en Go |
| `implements` | tipo → interfaz o trait | `implements`, `impl Trait for T` |
| `references` | el código → el tipo que nombra | el tipo de un campo, parámetro, variable o valor de retorno |
| `contains` | contenedor → miembro | el anidamiento, resuelto al parsear |

Cada arista lleva:

| Campo | Significado |
|---|---|
| `dst_id` | La definición alcanzada, cuando el link pass resolvió una |
| `dst_name` | Cómo se escribió y calificó el destino (`OrderService.findPage`, `java.util.List.of`) — nunca una expresión |
| `confidence` | `high`, `medium` o `low`: qué tan seguro está el índice de que la ocurrencia se refiere a esa definición |
| `resolution` | Por qué: `self`, `local`, `param`, `field`, `ctor_inject`, `import`, `same_file`, `same_package`, `inherited`, `return_type` (tipado por un tipo de retorno declarado a lo largo de una cadena), `unique_name`, `name_only`, `external_known`, `chain_external`, `structural` (`contains`) |
| `external` | El destino está fuera del repositorio, **con evidencia**: la plataforma (JDK, la biblioteca estándar y los builtins de Python, `std`/`core`/`alloc` de Rust, la biblioteca estándar de Go, los globales del DOM/Node) o una dependencia que declara un manifiesto (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, `setup.cfg`, `requirements*.txt`). "No encontrado" no es "externo": un nombre sin definición y sin evidencia queda sin decidir, `low` |
| `from_test` | La ocurrencia está en código de test (la misma regla que `is_test`) |
| `hint` | Lo que el archivo dijo del receptor de una llamada (`typed field OrderService`, `chain findByCode name Order`): el link pass vuelve a resolver una arista desde ahí sin releer la fuente |

Una llamada sobre un receptor que nada tipa, a un nombre que el repositorio no
define (`.map()` sobre un stream sin tipo, un método de un parámetro de lambda
sin tipo), se **descarta**: queda en la tabla con `resolution = discarded`,
fuera de toda respuesta (las tools leen la vista `live_edges`), y vuelve sola
cuando se indexa un archivo que define el nombre. `index_repo` informa cuántas
hay (`edges_discarded`) y cuántas llamadas quedan sin decidir
(`edges_unresolved`).

`graph_edges`, la tabla de llamadas de 0.9 (una fila por par llamador → llamado,
con nombres en vez de ids), se sigue escribiendo al lado, con la extracción de
0.9.0 (su código, copiado tal cual), así que la tabla es idéntica a la que
escribe 0.9.0 y un 0.9.0 al que volvés responde como antes. Nada en 0.10 la lee
salvo el camino viejo de una rama que todavía no se reconstruyó.

### Cómo se resuelve una referencia

Por ocurrencia, en orden; la primera regla que responde decide `resolution` y
`confidence`:

| # | Regla | `resolution` | `confidence` |
|---|---|---|---|
| 1 | `this` / `self` / `Self` / `super` → el tipo envolvente (o su supertipo) | `self`, `inherited` | `high` |
| 2 | El receptor es un local o parámetro con tipo en el scope léxico | `local`, `param` | `high` |
| 3 | El receptor es un campo de la clase envolvente (cada clase interna tiene los suyos) | `field` | `high` |
| 4 | Un campo asignado desde un parámetro del constructor, una propiedad-parámetro de TypeScript, `inject(T)` de Angular | `ctor_inject` | `high` |
| 5 | El receptor es un tipo, hallado por un import o por el paquete (nunca por empezar con mayúscula) | `import`, `same_package` | `high` |
| 6 | Sin receptor: un miembro del contenedor o de un supertipo, después una función del archivo, después un import | `self`, `inherited`, `same_file`, `import` | `high` |
| 7 | La única definición de ese nombre en el repositorio (solo Java y TypeScript; nunca a través de un import que no se puede seguir) | `unique_name` | `medium` |
| 8 | Varios candidatos, nada decide | `name_only` (sin `dst_id`) | `low` |
| 9 | Ningún candidato en el repositorio y el tipo o paquete es de afuera (con evidencia) | `external_known` | `high` |

Una cadena fluent (`a.b().c()`) tipa el receptor de `c` por el **tipo de
retorno declarado** de `b` cuando `b` resolvió dentro del repositorio
(`return_type`); después de una llamada a una librería nada lo tipa: `c` es
`chain_external` (`medium`) cuando el repositorio no tiene `c`, y `name_only`
cuando sí. La confianza nunca crece a lo largo de una cadena, y lo que descansa
en una conjetura (`unique_name`, un tipo definido dos veces, un import que no se
pudo seguir) nunca es `high`.

**Esto no es un compilador.** No hay inferencia de tipos más allá de
declaraciones, inicializadores (`var x = new T()`, `x := New()`,
`let s = Store::open(p)?`), elementos de un for-each y tipos de retorno
declarados; no hay resolución de sobrecargas por tipo de argumento (Java elige
por aridad); no hay análisis de flujo. `confidence` dice cuánto se puede confiar
en cada arista, y se midió (ver abajo).

### Qué resuelve, por lenguaje

Cada fila está respaldada por un test; los nombres de los tests están en un
comentario en la fuente de esta página.

| Lenguaje | Resuelve | No resuelve |
|---|---|---|
| **Java** | Campos por clase interna; `var` desde su inicializador; constantes (`LOG`) como campos de su tipo declarado, nunca como tipos; inyección por constructor (`this.x = x`) y campos `@Inject`; imports, mismo paquete, imports comodín, `java.lang`; `new T()` como `instantiates`; supertipos, incluidos los estáticos que hereda una entidad Panache (externos); elementos de un for-each; imports estáticos (JUnit, Mockito); métodos de `Object` sobre un receptor sin tipo (externos, `medium`); accessors de Lombok `@Data`/`@Getter` (al campo, `medium`); receptores a través de campos (`this.cfg.server.port()`); clases anónimas | El tipo de un parámetro de lambda sin tipo; sobrecargas elegidas por tipo de argumento (solo por aridad); genéricos más allá del elemento de una colección; lo que devuelven `Optional.get()`/`List.get()` |
| **TypeScript / JavaScript** | Imports relativos (`.js` escrito por `.ts`, archivos `index`); alias de `paths` y `baseUrl` del `tsconfig.base.json` / `tsconfig.json` de la raíz (un `extends`); barrels hasta 4 niveles; imports por nombre, default, namespace y de paquete; paquetes externos solo con evidencia (una dependencia de `package.json`, un builtin de Node); DI de Angular (propiedades-parámetro del constructor, `inject(T)` de `@angular/core` en un campo o un local); funciones flecha ligadas a un `const` como símbolos y fuentes de sus llamadas; scopes léxicos (parámetros, `catch` y funciones de bloque sombrean); tipos de retorno declarados a lo largo de una cadena; métodos de interfaz (y métodos abstractos) como símbolos | CommonJS (`require`, `module.exports`), `export =`, `import()` dinámico, componentes JSX como llamadas, declaraciones ambientales `.d.ts`; los `paths` de un `tsconfig.json` por app; los bloques `namespace` no son símbolos (solo califican a sus miembros); las firmas de sobrecarga no son símbolos (solo la implementación); `this` en un método de objeto literal o en una *class expression*; las signals de Angular llamadas como funciones; receptores a través de varios campos (`this.a.b.m()`, por nombre) |
| **Python** | Módulos por ruta y paquete (`__init__.py`, la raíz del repositorio, `src/`, el directorio propio de un script); imports relativos, `import *`, re-exports; llamadas a nivel de módulo (desde el símbolo de archivo); una llamada a una clase (a la clase); `self.x = …` en un método, tipado desde un parámetro tipado de `__init__`; `x = Foo()` / `x = make()` por la clase o la anotación de retorno; herencia y `super()`; funciones anidadas; la biblioteca estándar, los builtins y las distribuciones declaradas como externos; formas de `typing` (`Any`, `TypeVar` y uniones no tipan nada, `type[X]` es `X`, `Self` es la clase) | Parámetros sin anotar; `self.a.b()` (Python no declara campos); decoradores que cambian un tipo; los `-r` de un archivo de requirements |
| **Rust** | Crates por `Cargo.toml` (el propio, dependencias por path, `workspace = true`); módulos por ruta y `mod` en línea; `use` (grupos, alias, globs) con el scope de su módulo, función o bloque; `crate::`, `self::`, `super::`, `Self::`; re-exports con `pub use`; los `impl` de un tipo en cualquier archivo de su crate; `impl Trait for T`; cada bound de un parámetro de tipo; campos por su declaración; `let x = T::new()?` por el tipo de retorno (`?`/`unwrap()` desenvuelven `Result`/`Option`); `std`/`core`/`alloc`, el prelude y los crates declarados como externos; `T::default()` de un derive (ningún `impl` lo define) vale un `T`, en `medium`; gemelos por `#[cfg]` como `medium` | Las macros no son llamadas, y los ítems que define una macro no son símbolos; módulos declarados con `#[path]` o un `mod` condicional; un `impl<T: X> Y for T` genérico no pertenece a ningún tipo (sus métodos son `T.m`); `let x = a.f()` con `f` fuera del repositorio (una cadena, sin más tipo) |
| **Go** | El `module` de `go.mod` (y un `replace` local) decide qué es del repositorio; la biblioteca estándar y otros módulos como externos; un método por el tipo de su receptor, también desde otro archivo del paquete; campos por su declaración; `x := New()` por su resultado declarado; métodos de interfaz como símbolos; gemelos por build tags como `medium`; un tipo externo embebido hace externo a un método que falta (`medium`) | Dispatch por embebido (embeber promueve métodos, no los sobrescribe); un método promovido desde un tipo embebido del repositorio no tiene test; verificado solo con fixtures: no se midió ningún repositorio Go |
| Kotlin y archivos de texto crudo | — | Ni símbolos ni aristas (sus rutas de Spring sí se extraen) |

<!-- Evidencia de la columna "Resuelve" (crates/devctx-parse/tests salvo que se indique):
Java: link_java.rs a_each_inner_class_types_its_own_field, b_var_is_never_a_type,
c_a_constant_is_a_field_with_its_type, d_constructor_injection, e_same_package_and_import,
f_fluent_chains, g_new_is_instantiates, h_an_undefined_panache_static_is_inherited_and_external,
i_for_each_variables, static_imports_object_methods_literals_and_lombok,
a_dotted_receiver_is_typed_through_its_fields, lambda_parameters_and_patterns_bind_their_names,
an_anonymous_class_scopes_its_bare_calls,
confidence_never_escalates, a_call_after_an_external_one_is_not_external_by_default;
facts_java.rs java_symbols_with_kind_parent_signature_and_visibility (@Inject, record, constructor).
TypeScript/JavaScript: link_typescript.rs constructor_injection_aliases_and_barrels,
inject_in_a_field_follows_a_renamed_import, an_exported_arrow_function_is_a_symbol_source_and_destination,
local_bindings_shadow_fields, imports_by_name_namespace_default_and_package,
a_package_needs_a_manifest_or_a_node_builtin, only_angulars_inject_injects,
this_in_object_literals_and_class_expressions_is_not_a_class, javascript_without_types,
a_tsconfig_with_comments_is_read, a_block_local_function_does_not_shadow_outside_its_block,
a_call_through_an_interface_reaches_its_declared_method; facts_typescript.rs
an_interface_method_is_a_symbol (las firmas de sobrecarga no son símbolos);
src/symbol_id.rs qualified_names_carry_every_enclosing_symbol (namespace N → N.f).
Python: link_python.rs two_calls_from_one_method_are_two_edges, module_level_calls_are_edges_of_the_file,
relative_star_and_reexported_imports, instance_attributes_are_typed_in_init,
builtins_stdlib_and_declared_packages_are_external, inheritance_and_nested_functions,
a_script_imports_its_siblings_and_the_package, a_local_from_a_method_call_is_a_chain,
typing_special_forms, local_bindings_shadow_attributes.
Rust: link_rust.rs paths_resolve_by_use_and_self, impls_in_other_files_and_trait_impls,
declared_crates_and_std_are_external, macros_are_not_calls, modules_and_functions, values_and_fields,
declared_dependencies_decide_which_crate, every_bound_is_tried, cfg_homonyms_are_medium,
a_use_in_a_function_is_scoped_to_it, a_use_in_a_block_is_scoped_to_it, owners_values_and_variants,
a_derived_default_is_its_type, an_external_supertype_is_medium_evidence;
facts_rust.rs only_a_required_test_cfg_marks_test_code;
devctx-mcp state::impact::tests dispatch_follows_a_rust_blanket_impl (los métodos de un impl genérico son T.m).
Go: link_go.rs methods_by_their_receiver, the_module_decides_what_is_the_repositorys,
a_module_needs_its_go_mod, an_external_embedding_is_medium_evidence, package_names_tags_and_shadows;
facts_go.rs go_symbols_imports_and_embedding (embeber es inherits).
Llamadas descartadas: devctx-mcp state::lookup::tests discarded_calls_reach_no_answer;
link_java.rs f_fluent_chains (dos descartadas). -->

**Precisión medida.** Sitios de llamada etiquetados a mano antes de medir (los
"gold edges" de `scripts/graph-eval/`), octubre de 2026: Java 20 de 25
correctos, y 19 de 19 de los `high`; TypeScript 20 de 20; Python 26 de 27; Rust
26 de 26 (22 de 22 `high`). Llamadas sin decidir: 4–8 % en dos repositorios
Java, 11 % en un subconjunto de un workspace Angular, 2 % en un repositorio
Python, 17,5 % en el Rust de este repositorio — sobre todo cadenas tras una
llamada a una librería con un nombre que el repositorio también define, sin
decidir a propósito. Go se verificó solo con fixtures. Son unas pocas decenas
de sitios por lenguaje: un orden de magnitud, no una garantía.

**Limitaciones conocidas de 0.10.0** (verificación de campo, PLAN-009
TASK-016; aceptadas por el mantenedor el 2026-10-13):

- **Python registra 5 de las 7 relaciones.** No hay `implements` (Python no
  declara interfaces), y una llamada a una clase, `Clase()`, se guarda como
  `calls` a la clase, no como `instantiates`. Los repositorios Java,
  TypeScript y Rust medidos registran las 7.
- **Rust deja sin decidir el 16,0 % de sus llamadas** (sin destino resuelto ni
  marca `external`), contra una meta de 15 % como máximo; los repositorios
  Java, TypeScript y Python midieron 1,6–14,7 %. Lo que queda son cadenas tras
  una llamada a una librería con un nombre que el repositorio también define,
  y receptores sin tipo conocido: sin decidir a propósito, porque marcarlos
  externos le escondería llamadores a `impact_analysis`.

## Lenguajes soportados

**Parseo completo — símbolos y aristas (7):**

| Lenguaje | Extensiones |
|---|---|
| Python | `.py` `.pyi` |
| JavaScript | `.js` `.mjs` `.cjs` `.jsx` |
| TypeScript | `.ts` `.mts` `.cts` |
| TSX | `.tsx` |
| Go | `.go` |
| Java | `.java` |
| Rust | `.rs` |

**Indexados como texto crudo — buscables, pero sin símbolos ni aristas:**

`.html` `.htm` `.css` `.scss` `.sass` `.less` `.json` `.yaml` `.yml` `.xml`
`.md` `.markdown` `.sql` `.graphql` `.gql` `.proto` `.kt` `.kts`

Se fragmentan con solapamiento y se embeben, así que la búsqueda los encuentra.
Simplemente no aparecen en el grafo.

Kotlin es el caso notable: no tiene gramática de tree-sitter conectada, así que
se indexa como texto — **pero sus rutas de Spring sí se extraen**, porque la
detección de rutas tiene un camino aparte.

Cualquier otra cosa no se indexa.

## Qué herramienta cuándo

| Pregunta | Herramienta |
|---|---|
| ¿Dónde está el código sobre una idea? | `search` (no el grafo) |
| Sé el nombre: mostrame la definición | `read_symbol` |
| ¿Dónde se llama o se construye esto — archivo y línea? | `get_references` |
| ¿Qué se rompe si lo cambio (llamadores y llamados, transitivos)? | `impact_analysis` |
| ¿Qué contiene esta clase, qué implementa esta interfaz, quién importa o usa este tipo — salto a salto, por cualquier relación? | `traverse` |
| Soy nuevo en esta zona: ¿cuáles son sus piezas centrales? | `repo_map`, con un foco |
| ¿Dónde está, en este archivo, lo que busco? | `skeleton`, y después `read_symbol` / `read_file` sobre el rango |

`get_references` es un salto y lista ocurrencias; `impact_analysis` recorre
`calls` e `instantiates` (más el dispatch) hasta una profundidad, con tope por
dirección; `traverse` recorre las relaciones que elijas, y devuelve también las
aristas. `repo_map` y `skeleton` dan firmas, nunca código: ubican, no
reemplazan la lectura.

## Operaciones

### `get_references(símbolo)` — ¿quién llama a esto?

Cada sitio de llamada de un símbolo en el código indexado — llamadas e
instanciaciones (`new Foo()`), una por ocurrencia, así que dos llamadas desde el
mismo método son dos referencias. La respuesta directa a
*"¿es seguro cambiar esto?"* a un salto. `símbolo` es un nombre pelado (`charge`),
uno calificado (`Card.charge`, `Card::charge`) o `archivo::nombre`
(`src/pay.rs::charge`).

En un índice hecho por 0.10 o posterior, cada referencia conserva `file`, `line` y
`source` y agrega:

| Campo | Significado |
|---|---|
| `confidence` | `high`, `medium` o `low`: qué tan seguro está el índice de que esta ocurrencia es *esta* definición |
| `via` | La relación: `calls` o `instantiates` por defecto; con `kinds`, también `references` (un uso de tipo), `imports`, `inherits`, `implements` |
| `sym` | El id (16 dígitos hex) de la definición referida, cuando resolvió a una |
| `external` / `test` / `undecided` | Presentes (y `true`) solo cuando la ocurrencia es una llamada a una librería, está en un archivo de test o no se pudo decidir |

**`min_confidence`** (`"high"`, `"medium"` —el default— o `"low"`) fija la
confianza más baja que se lista. Por defecto se listan `high` y `medium`, y la
`medium` va marcada por su campo; las referencias ambiguas y las llamadas que el
índice no pudo decidir quedan afuera y se cuentan en
`below_confidence: {count, min_confidence, hint}`, nunca se pierden en silencio.
Con `min_confidence: "low"` se listan, cada una marcada. Para un nombre sin
definición en el repositorio (una función de librería), la respuesta lista sus
sitios de llamada externos.

**`kinds`** agrega relaciones a las llamadas e instanciaciones del default:
cualquiera de `references` (usos de tipo), `imports`, `inherits`, `implements`, o
`all` (`GET /references/<símbolo>?kinds=references,imports` por HTTP). Un servidor
anterior a 0.10 ignora `min_confidence` y `kinds`; el cliente lo dice en `warning`.

### `impact_analysis(símbolo)` — radio de impacto

Llamadores *y* llamados transitivos. Los llamadores son el radio de impacto:
todo lo que se podría romper. Los llamados son de qué depende este símbolo para
funcionar.

Corrélo antes de refactorizar cualquier cosa pública. Esta es la operación que
la gente olvida que existe y después lamenta no haber corrido.

Cada lado es una lista de `{symbol, depth}` (profundidad 1 = directo), lo más
profundo al final. En un índice hecho por 0.10 o posterior el recorrido sigue el
grafo de símbolos **por id** — una consulta por nivel, sin importar el tamaño de
la frontera — y cada símbolo trae además `confidence` (de la llamada que lo
alcanzó), `via` (`calls`, `instantiates` o `dispatch`), `sym`, `file` y `line`
(de su definición), y `test`, `external` o `undecided` cuando son verdaderos. Dentro de
una profundidad, el orden es por confianza, después por rank (el PageRank global
que calcula cada corrida de `index`; ver
[Búsqueda](busqueda.md#centralidad-solo-híbrida-opt-in)) y después por nombre.

Lo que los defaults dejan afuera **se cuenta, nunca desaparece en silencio**, y no
se recorre:

| Parámetro (flag del CLI) | Default | Afuera por defecto, contado en |
|---|---|---|
| `min_confidence` (`--min-confidence`) | `medium` | símbolos alcanzados solo por llamadas de confianza `low` → `below_confidence: {count, min_confidence, hint}` (cuenta símbolos); llamadas al nombre que el índice no pudo decidir (`svc.update()` con un receptor sin tipo) → `undecided_calls: {count, hint}` (cuenta llamadas, ninguna desde un test ni desde un llamador ya listado); con `low` se listan los dos, los llamadores de las llamadas sin decidir marcados `undecided` y sin recorrerlos |
| `include_tests` (`--include-tests`) | `false` | símbolos de archivos de test → `excluded.tests` |
| `include_external` (`--include-external`) | `false` | llamados de librerías → `excluded.external` |
| `max_nodes` (`--max-nodes`) | `200` por dirección (`0` = sin tope) | los símbolos de la profundidad que lo desborda → `omitted: {count, reason: "limit"}` y `omitted_by_limit: {count, max_nodes, upstream/downstream: {count, depth}}`; no se lee ningún nivel más profundo |
| `dispatch` (`--no-dispatch`) | `true` | nada: se sigue el dispatch por interfaces, clases abstractas y traits (abajo); `false` sigue solo llamadas directas. Un nodo con más métodos override-equivalentes de los que se expande (16) → `omitted_by_limit.dispatch: {count, max_per_node}` y solo ahí (cuenta métodos; `omitted_by_limit.count` y `omitted.count` siguen contando nodos) |

**Dispatch.** Un llamador que tiene un servicio por su interfaz (un campo
inyectado como `IService`, un parámetro tipado por un trait) llama a
`IService.update`, nunca a `ServiceImpl.update`; el recorrido sigue esa llamada
igual, por las aristas `inherits`/`implements` resueltas, en cada profundidad (no
solo desde el símbolo pedido):

- **upstream**, `ServiceImpl.update` alcanza a los llamadores de los métodos que
  sobrescribe en sus supertipos (`IService.update`, `BaseService.update`), a la
  profundidad de un llamador directo;
- **downstream**, un método de interfaz o abstracto alcanza a los métodos que lo
  implementan, un nivel debajo de él, y a lo que estos llaman.

Un método es override-equivalente si tiene el mismo nombre y una aridad que puede
coincidir (los parámetros con default, opcionales o variádicos la amplían; en
TypeScript y JavaScript el método que implementa puede tomar menos parámetros que
el que implementa); un
método private o static no sobrescribe nada; entre sobrecargas de igual aridad en
un supertipo gana la que coincide exacto en los tipos simples de sus parámetros. La cadena de supertipos se sigue
hasta cuatro niveles (`ServiceImpl` → `BaseService` → `IService` → su
super-interfaz); un método override-equivalente más lejos se cuenta en
`omitted_by_limit.dispatch` (en su `count`, y aparte en `beyond_depth`, con
`max_depth: 4`), sin seguirlo. Un método de supertipo que implementan muchos de
los métodos recorridos (una interfaz con mil implementaciones, todas raíces) se
atribuye a lo sumo a 16 de ellos, primero las cadenas más seguras: sus llamadores se alcanzan todos, y los pares sin
atribuir se cuentan en `omitted_by_limit.dispatch.origins_capped` (con
`max_origins`); una llamada arma como mucho 8 192 filas de dispatch, y el resto se
cuenta en `rows_capped` (con `max_rows`; la respuesta dice entonces
`total_is_lower_bound`). Ese nodo lleva `via: "dispatch"` y `through` (el método por el que
pasó), y su confianza es `min(medium, la arista que representa)`: nunca `high`, y
una llamada `low` sigue `low` (contada en `below_confidence` por defecto). Para
dejar el dispatch afuera: `dispatch: false` (`--no-dispatch`, `?dispatch=false`), o
`min_confidence: "high"`, que deja afuera también toda llamada `medium` (y no lee
ni cuenta ningún dispatch). En los dos casos la respuesta lo dice, leyendo solo
los filtros: `dispatch: {"evaluated": false, "reason": "dispatch:false"}` o
`{"evaluated": false, "reason": "min_confidence high"}` (no aparece cuando el
dispatch se siguió). Cubre
Java, TypeScript (`implements`, clases abstractas) y traits de Rust; las clases
base de Python como mejor esfuerzo; Go no (embeber promueve métodos, no los
sobrescribe, y una interfaz se satisface sin una arista que seguir).

Por HTTP: `GET /impact/<símbolo>?depth=3&min_confidence=low&include_tests=true&max_nodes=500&dispatch=false`.
La respuesta dice en `filters` qué filtros aplicó; un filtro inválido
(`min_confidence=most`) sigue siendo un 500 con `error` por HTTP, no un 400 como
en `traverse`, `repo_map` y `skeleton`. Un nombre pelado significa las
definiciones del repositorio con ese nombre (como en `get_references`), no toda
llamada escrita con ese nombre — a lo sumo 10 000, una guarda contra un nombre
patológico (pasado ese tope el resto no son semillas, y nada informa el corte).
Los campos de 0.9 conservan su significado: `resolved_symbols` (un nombre pelado
que representaba varias declaraciones), `branch_fallback`, `warning`, y `omitted`
/ `omitted_for_budget` (la mitad del presupuesto de salida por dirección;
`omitted.count` suma lo que cortaron el presupuesto y `max_nodes`). Un nombre sin
definición en el repositorio responde como `read_symbol`: `external: true` con
`called_from` (su upstream son entonces los llamadores de sus sitios de llamada) o
`suggestions`. En un índice anterior a 0.10 el recorrido es el de 0.9 — por
nombre, sin confianza, sin filtros, sin tope — con el `warning` que lo dice (y, si
se pasaron filtros, que no se aplicaron); un servidor anterior a 0.10 ignora los
parámetros, y el cliente lo dice en `warning`.

### `traverse(símbolo)` — recorrer el grafo por tu cuenta

Salto a salto, por las relaciones que elijas: `calls`, `instantiates`,
`imports`, `inherits`, `implements`, `contains`, `references` (`kinds`, separadas
por coma o `all`; default `calls`), `out` (default), `in` o `both`
(`direction`), de 1 a 4 niveles (`depth`, default 1; más se rechaza). Es el
recorrido de `impact_analysis` — una consulta por nivel y dirección sobre toda la
frontera, solo sobre aristas vivas, por id, con los mismos filtros — con otras
relaciones, y con las aristas en la respuesta.

Cuándo usar cuál: `traverse` para explorar estructura (qué contiene una clase, su
jerarquía, quién importa o usa un tipo, una cadena de llamadas salto a salto);
`impact_analysis` para el radio de impacto de un cambio (llamadores y llamados
transitivos, con tope por dirección); `get_references` para los sitios de llamada
— archivo y línea — de un símbolo.

| Pregunta | Llamada |
|---|---|
| Llamadores, y los llamadores de esos | `{symbol: "OrderService.place", kinds: "calls", direction: "in", depth: 2}` |
| Dónde se construye un tipo | `{symbol: "Invoice", kinds: "instantiates", direction: "in"}` |
| Qué importa un archivo / quién importa un tipo | `{symbol: "src/api/client.ts", kinds: "imports"}` / `{symbol: "OrderDto", kinds: "imports", direction: "in"}` |
| Subclases | `{symbol: "BaseRepo", kinds: "inherits", direction: "in"}` |
| Implementaciones | `{symbol: "PaymentGateway", kinds: "implements", direction: "in"}` |
| Miembros de una clase | `{symbol: "OrderService", kinds: "contains"}` |
| Usos de un tipo | `{symbol: "OrderDto", kinds: "references", direction: "in"}` |
| Los supertipos de una clase, dos niveles arriba | `{symbol: "ServiceImpl", kinds: "inherits,implements", depth: 2}` |

`symbol` acepta un símbolo pelado, calificado o `archivo::nombre`, o la ruta de
un archivo (el archivo mismo: de él salen los imports); si un símbolo tiene ese
nombre, el símbolo gana sobre la ruta. Un nombre pelado con
varias definiciones se recorre desde todas y se listan en `candidates`
(`{sym, symbol, kind, file, line}`, dentro de un cuarto del presupuesto de
salida —la página se queda con lo que no usan—, el
resto nombrado en `omitted_for_budget.candidates`; hasta 10 000 definiciones, como
`impact_analysis`, y `candidates_truncated` si hay más); `sym` (el id hex que
lleva un nodo o un candidato) arranca desde exactamente una.

La respuesta es `{root, candidates?, nodes, edges, total, next_offset?, omitted?,
partial?, total_is_lower_bound?, filters, below_confidence?, excluded?, omitted_by_limit?, dispatch?,
omitted_for_budget?, branch_fallback?}`:

- `nodes`: `{sym, symbol, kind, file, line, depth, confidence, via}` (`via` es la
  relación que lo alcanzó, o `dispatch`, con `through`), más `test`, `external` o
  `undecided` cuando son verdaderos; por profundidad, después confianza, rank y
  nombre. El nombre pedido nunca se lista (como en `impact_analysis`); de las
  definiciones de un nombre pelado, una alcanzada desde otra sí.
- `edges`: cada arista por la que un nodo listado se alcanzó en su profundidad,
  una por par y relación — `{from, to, kind, confidence, line, occurrences?}`,
  con `from` y `to` como `sym` (el nombre, para un extremo sin definición), en el
  sentido propio de la arista (`from` tiene la ocurrencia).
- Filtros como `impact_analysis`: `min_confidence` (`medium`), `include_tests` e
  `include_external` (`false`): lo que dejan afuera se cuenta (`below_confidence`,
  `excluded`) y no se recorre. Con `calls` se sigue el dispatch por interfaces,
  clases abstractas y traits (`via: "dispatch"`, nunca `high`; `dispatch: false`
  lo apaga, y `dispatch: {evaluated: false, reason}` dice cuándo no se siguió).
  Con `calls` `in` (o `both`) desde un nombre — no un `sym` ni un
  `archivo::nombre` — las llamadas a ese nombre que el índice no pudo decidir
  también son llamadores, como en `impact_analysis`: se cuentan en
  `undecided_calls: {count, hint}` (ni desde tests ni desde un llamador listado), y
  con `min_confidence: "low"` sus llamadores se listan en la profundidad 1,
  `undecided: true`, sin recorrerlos, con una arista al nombre (uno que una
  arista decidida alcanza más abajo es ese nodo decidido, y se recorre). Con
  `partial` la cuenta puede pasarse, como en `impact_analysis` pasado su tope:
  un llamador de un nivel que no se leyó no está "listado". Una raíz que es un
  archivo (una ruta) no tiene llamadas sin decidir.
- Paginación (el contrato de `search_routes`): `limit` (50) nodos desde `offset`;
  `total`, `next_offset` y `omitted: {count, reason, next_offset}`. Un hub — un
  DTO referenciado desde cientos de archivos — se pagina, no se vuelca: cuando el
  recorrido ya tiene más nodos de los que la página necesita no lee ningún nivel
  más profundo, `partial: {from_depth}` lo dice, y `total` y `omitted.count`
  cuentan entonces solo lo que se leyó — `total_is_lower_bound: true` (la página
  siguiente sigue leyendo; el CLI imprime `>= N`).
- El presupuesto de salida (`DEVCTX_MAX_OUTPUT_TOKENS`) se reparte por mitades
  entre nodos y aristas; lo que no entra se nombra en `omitted_for_budget`.

No hay camino 0.9: sobre un índice anterior a 0.10 `traverse` es un error que
pide `devctx index --full`; sobre un índice vigente sin ningún símbolo (ningún
lenguaje con extracción de símbolos) lo dice y manda a `search` o `read_file`,
porque un reindexado no cambiaría nada. Por HTTP: `POST /traverse` con los
parámetros como cuerpo JSON — un parámetro inválido (una profundidad mayor a 4,
una relación desconocida) es un 400 con su mensaje, cualquier otro error de la
tool un 500 con `error`; un servidor más viejo que la tool responde
404, y el cliente lo traduce a "reinicialo: `devctx serve --stop`". Desde la
terminal: `devctx traverse OrderDto --kinds references --dir in --depth 2`
(`--sym`, `--min-confidence`, `--include-tests`, `--include-external`,
`--no-dispatch`, `--limit`, `--offset`, `--json`).

### `repo_map` — un mapa para orientarse

Unos mil tokens que le dicen a un agente dónde está cada cosa: las firmas (nunca
los cuerpos) de los símbolos más relevantes para su foco, agrupadas por archivo,
ordenadas por PageRank personalizado a ese foco, los miembros bajo sus
contenedores. Sirve para orientarse al empezar una tarea; para leer código está
`read_symbol` o `read_file`, y para los vecinos de un símbolo, `traverse`.

| Parámetro | Qué es |
|---|---|
| `budget` | Tokens que puede ocupar el mapa (default 1024; se usan al menos 256 y como mucho `DEVCTX_MAX_OUTPUT_TOKENS`; 0 se rechaza) |
| `focus_files` | Archivos que tenés abiertos o estás editando (ruta relativa al repo o su final): lo que usan rankea primero; ellos no se listan |
| `focus_symbols` | Símbolos de la tarea: pelados, `Tipo.miembro` o `archivo::nombre` |
| `query` | La tarea en palabras: sus identificadores (`camelCase`, `snake_case`, `Tipo.miembro`) cuentan como símbolos de foco, y también una consulta de una sola palabra |

Cómo rankea (DD-12, DD-14): sin foco, por el rank global que escribe el link pass
(`symbols.rank`). Con foco, la misma matriz se rankea de nuevo, personalizada:
cada archivo de foco pesa 50 en el vector de personalización, repartido entre sus
símbolos, cada término de `focus_symbols` o de `query` pesa 10, repartido entre
sus definiciones, y las aristas hacia un símbolo nombrado cuentan ×10; las aristas hacia un `_nombre` o hacia un nombre definido en más de
cinco archivos cuentan ×0,1. Solo cuentan las aristas vivas (una llamada que el
link pass descartó no aporta nada). El código de test (un archivo de test, o
código `#[cfg(test)]`) y las librerías nunca aparecen, ni los símbolos de
archivo ni los archivos de foco. Cuántas firmas entran se decide con una
búsqueda binaria sobre la lista rankeada, que para cuando el mapa queda dentro
del 15 % por debajo del presupuesto (tokens = caracteres / 4, como en todo el
producto). Los archivos van por la suma del rank de sus símbolos, los símbolos
por línea, indentados bajo sus contenedores; `⋮` indica que el archivo tiene más:

```
crates/devctx-search/src/lib.rs
│ pub enum SearchMode
│ pub struct RankOptions
│   impl RankOptions
│     pub fn keeps(&self, kind: PathKind) -> bool
│ pub fn search_ranked(store: &Store, query: &str, filter: &SearchFilter, limit: usize, …
│ pub fn identifier_tokens(query: &str) -> Vec<String>
│ pub fn reciprocal_rank_fusion(lists: &[Vec<SearchResult>], k: f32) -> Vec<SearchResult>
⋮
crates/devctx-core/src/types.rs
│ pub struct SearchResult
│   impl SearchResult
│     pub fn rewrite_score(&mut self, score: f32)
⋮
[devctx] focus: query identifier search_ranked (1 definition)
[devctx] repo map: 45 of 4267 symbols in 9 files, ranked by PageRank personalized to the focus; 4222 more did not fit the 1024-token budget (a focus or a larger budget shows others). To read code: read_symbol / read_file.
```

(Un extracto de `devctx map --query search_ranked` sobre este repositorio.)

Las líneas finales `[devctx]` dicen qué encontró el foco (`not found` para un
nombre o archivo que no existe, y que entonces el ranking quedó global) y cuántos
símbolos no entraron. Sin foco, el mapa de un repo grande es su código central
global, que dice poco: dale un foco.

El serve guarda la adyacencia de la rama en memoria, por generación del índice:
un mapa cuesta un PageRank personalizado y el render; se vuelve a leer tras cada
`index` y se suelta cuando no se usa, igual que un modelo
(`DEVCTX_MODEL_IDLE_SECS`). `status` la reporta en `memory.repo_map`. Guarda
una rama: mapas de dos ramas pedidos por turno la rearman cada vez. Las listas de
`GET /repo-map` y de `devctx map --focus/--symbol` van separadas por coma, así que
una ruta con coma solo se puede pasar por la tool MCP. Los tokens se estiman como
caracteres / 4, como en todo el producto; los `│` y `⋮` del dibujo pueden costarle
más a un tokenizador real.

No hay camino 0.9: sobre un índice anterior a 0.10 `repo_map` es un error que
pide `devctx index --full`; sobre un índice vigente sin ningún símbolo lo dice y
manda a `search` o `read_file`. Por HTTP: `GET
/repo-map?budget=…&focus_files=a,b&focus_symbols=…&query=…` (listas separadas
por coma; un parámetro inválido, como un presupuesto de 0, es un 400, cualquier
otro error de la tool un 500 con `error`); un servidor más viejo que la tool responde 404, y el cliente lo
traduce a "reinicialo: `devctx serve --stop`". Desde la terminal: `devctx map
--focus src/api/client.ts --symbol OrderService.place --query "…" --budget 1500`.

### `skeleton(archivo)` — la estructura de un archivo

Los símbolos de un archivo — clases, métodos, funciones, campos — con su firma y
su rango de líneas, anidados por contenedor, sin cuerpos, y sus imports en una
línea. Sirve para **ubicar** dónde está algo, por una fracción de los tokens del
archivo; no es el código: para editar, leelo con `read_symbol` o `read_file` (un
rango de líneas del skeleton).

```
crates/devctx-store/src/skeleton.rs: 13 symbols
imports (6): duckdb::params, crate::error::Result, crate::store::Store, super::*, crate::StoredSymbol, crate::StoredSymbolEdge
pub struct FileImports [13-18]
  pub total: usize [15-15]
  pub first: Vec<String> [17-17]
impl Store [20-82]
  pub fn file_imports(&self, repo: &str, branch: &str, file: &str, limit: usize) -> Result<FileImports> [24-51]
  pub fn has_file_rows(&self, repo: &str, branch: &str, file: &str) -> Result<bool> [55-63]
  pub fn file_suggestions(&self, repo: &str, branch: &str, asked: &str, n: usize) -> Result<Vec<String>> [67-81]
pub fn rank_file_suggestions(asked: &str, files: &[String], n: usize) -> Vec<String> [92-134]
mod tests [137-264] [test]
  fn files(v: &[&str]) -> Vec<String> [141-143] [test]
  fn suggestions_rank_tail_then_name_then_near_miss() [148-193] [test]
  fn file_sym(file: &str) -> StoredSymbol [195-207] [test]
  fn imports_are_counted_and_only_the_first_read() [212-263] [test]
```

(`devctx skeleton crates/devctx-store/src/skeleton.rs` sobre este repositorio: 244 tokens para un archivo de 2 406.)

- `file` es una ruta relativa al repo, como la toma `read_file` (se aceptan una
  ruta absoluta dentro del repositorio, también a través de un enlace simbólico, y
  un `./` al principio). Una ruta con `..`, o una absoluta fuera del repositorio, se
  rechaza antes de mirar nada, con el mismo mensaje exista o no. `devctx skeleton`
  lee la ruta relativa al directorio de trabajo.
- El árbol sigue `parent_id`: dos espacios por nivel; los hermanos por línea, byte,
  fin (el más largo primero), kind, nombre e id: cualquier orden de las filas dibuja
  el mismo texto. Un `impl` de Rust es miembro de su tipo, como lo registra el
  extractor, así que se dibuja bajo el tipo aunque esté escrito más abajo: el rango
  dice dónde.
- La firma es la guardada (DD-17: la definición hasta su cuerpo, espacios
  colapsados; su cabeza —después de anotaciones, decoradores y atributos
  iniciales— a lo sumo 200 caracteres, con `…` si se cortó; las anotaciones que
  entran enteras en otros 200 quedan delante) mostrada sin esas anotaciones, sin el
  valor de un campo o constante y sin un `;` final. Un índice anterior a la versión
  21 del extractor contaba las anotaciones dentro de los 200: un corte que dejó una
  anotación sin cerrar se dibuja `kind nombre …`.
- `imports (N): …` nombra los primeros 8 destinos distintos, por línea; el resto se
  cuenta (`+M more`).
- El código de test se **marca, no se oculta**: un módulo `#[cfg(test)]` y sus
  miembros terminan en `[test]`, y un archivo de test lo dice una vez en su primera
  línea (`test file`). El archivo es lo que se pidió: ocultar parte sería contestar
  otra pregunta.
- El texto entra en `DEVCTX_MAX_OUTPUT_TOKENS` (8000; `0` es sin tope; se usa al
  menos 256; si las líneas fijas no dejan lugar, la de imports se reduce a su
  cuenta). Los símbolos se cortan desde el final, en orden de árbol, y la línea de
  cierre dice cuántos y la menor y la mayor línea que cubren: `[devctx] N more symbols (lines a-b)
  did not fit the B-token budget (DEVCTX_MAX_OUTPUT_TOKENS); read_file with
  start_line=a shows them`.
- Un archivo que el índice no tiene es un error que dice si existe en disco y
  nombra las rutas indexadas más cercanas (la misma cola de ruta, el mismo nombre de
  archivo, uno parecido, el mismo directorio). Un archivo indexado como texto (un
  lenguaje sin extracción de símbolos: SQL, YAML…) es un error que manda a
  `read_file`.
- No se lee nada del archivo: el skeleton es la vista del índice de la última
  versión indexada.
- Lo que el extractor no da, el skeleton no lo muestra: los bloques `namespace` de
  TypeScript no son símbolos (sus miembros salen planos, arriba), las firmas de
  sobrecarga de TypeScript no se extraen (solo la implementación) y las funciones
  anidadas en funciones de Python se extraen pero no tienen fixture de esta tool.

No hay camino 0.9: sobre un índice anterior a 0.10 `skeleton` es un error que
pide `devctx index --full`; sobre un índice vigente sin ningún símbolo (ningún
lenguaje con extracción de símbolos) lo dice y manda a `search` o `read_file`.
Por HTTP: `GET /skeleton?file=…` (un `file` vacío es un 400, cualquier otro error
de la tool — incluido un archivo que el índice no tiene — un 500 con `error`,
nunca un 404); un serve anterior a la tool responde 404, que el cliente
traduce a "reinicialo: `devctx serve --stop`". Desde la terminal: `devctx skeleton
src/api/client.ts`.

### `read_symbol(nombre)` — la definición

Código, archivo, rango de líneas y tipo. Usalo cuando sabés el nombre y querés
la cosa misma; usá `search` cuando querés código *sobre una idea*. `limit`
(default 5) acota las definiciones — un nombre puede estar definido muchas veces.
El nombre puede ser pelado, calificado (`Card.charge`, `Card::charge`, con su
paquete o crate adelante) o `archivo::nombre`; `Foo.Bar::baz` es el miembro `baz`
de `Foo.Bar`, no un archivo.

En un índice hecho por 0.10 o posterior cada definición trae además `sym` (su id,
16 dígitos hex, el mismo en todas las ramas), `kind` (`function`, `method`,
`class`, `field`…) y `qualified`. Una definición a la que el chunker no le da
código propio (un campo, una constante) responde con su firma como `code`.

**Un fallo se explica, no solo viene vacío.** Cuando no se encuentra ninguna
definición, `read_symbol` agrega:

- `suggestions` — hasta 5 nombres para probar: las formas calificadas que el grafo
  de llamadas conoce de un nombre pelado (`charge` → `Card.charge`), y luego los
  nombres definidos más cercanos por prefijo, sufijo, contención o distancia de
  edición (`AuthServce` → `AuthService`);
- `external: true`, `called_from: N` y un `next_step` cuando el nombre solo aparece
  como *destino* de llamadas — lo más probable es una función de librería o del
  runtime, aunque puede vivir en un archivo que el índice excluye o en otra rama.
  Un nombre calificado (`serde_json::from_str`, `Panache.withTransaction`) se busca
  tal cual y después por su último segmento, porque el grafo suele guardar solo el
  callee pelado. Un externo no recibe `suggestions` difusas (`with_context` no es
  un typo de `build_context`). En un índice 0.10 un externo no trae ninguna
  `suggestions`, como en 0.9; y en un índice 0.10 `external` sale de la evidencia del propio índice (una dependencia
  declarada, la plataforma), y el último segmento solo se prueba cuando el
  repositorio no define nada con ese nombre: un `Foo::new` que no existe no es
  "externo" porque se llame algún `new` — recibe `suggestions` (`Thing.new`). Un
  `archivo::nombre` que no encuentra nada en ese archivo tampoco es externo.

Las funciones chicas que el chunker agrupó en un solo fragmento (`a, b, c`) se
encuentran dentro de él, así que no se reportan como externas por error.

**Un índice hecho antes de 0.10** (o por otro extractor) no tiene tabla de
símbolos: `read_symbol`, `get_references`, el anclaje de `search` y la vista web
del grafo responden como 0.9, por nombre sobre el grafo de llamadas, y lo dicen en
`warning` — reindexá con `devctx index --full` para tener los campos de arriba.
`search` en `keyword`/`hybrid` también lo dice: su anclaje de identificadores fue
por nombre. Una lectura fallida de la tabla de símbolos trae su propio `warning`
(un error de lectura, no un motivo para reindexar).

## De qué rama responde

El grafo es por rama. Cuando la rama en checkout no tiene filas indexadas,
`read_symbol`, `get_references`, `impact_analysis`, `search_routes`,
`routes_for_handler` (y `search`, `build_context`, `memories_by_symbol`) responden
desde la rama por defecto o la indexada más recientemente y agregan
`branch_fallback: {current, used, why}`; sin ningún índice fallan con un error
explícito en vez de `[]`. Ver
[Búsqueda → Conciencia de ramas](busqueda.md#conciencia-de-ramas).

## Un índice hecho antes de 0.10

El grafo de arriba lo escribe `devctx index --full` con un binario 0.10. Hasta
entonces una rama informa `extractor_stale: true` en `index_status`, y:

| Herramienta | Responde |
|---|---|
| `read_symbol`, `get_references`, `search` (`keyword`/`hybrid`), la vista web del grafo | Como 0.9, por nombre sobre `graph_edges`, con un `warning` que lo dice |
| `impact_analysis` | El recorrido de 0.9 — por nombre, sin confianza, sin filtros, sin tope — con el `warning` (que agrega, si se pasaron filtros, que no se aplicaron) |
| `build_context` con `related: true` | El brief sin la sección, y `[devctx] related: not shown: …` |
| `traverse`, `repo_map`, `skeleton` | Un error: `… reads the symbol graph, and this branch's index predates it (an older extractor): reindex with devctx index --full (vectors are reused)` |
| búsqueda híbrida con `search.centrality_weight` > 0 | Solo vector + palabra clave: no hay ranks que sumar |

El paso a 0.10, cuánto cuesta y cómo volver atrás están en
[Mantener el índice al día](../13-mantener-el-indice-al-dia.md#pasar-a-010-el-grafo-de-símbolos).

## Límites que conviene conocer

**Un nombre pelado significa las definiciones del repositorio con ese nombre.**
`get_references`, `impact_analysis` y `traverse` resuelven `update` a cada
símbolo llamado `update` y recorren desde todos por id, no a cada llamada
escrita `.update(` — el `.map()` de una librería no es un llamador de tu `map`.
La respuesta lista lo que el nombre pelado representó (`resolved_symbols`,
`candidates`); un nombre común como `update` puede representar unas cuantas
decenas de métodos, así que leé esa línea. Preguntá con el nombre calificado
(`OrderService.update`), con `archivo::nombre` o con un `sym`
cuando querés decir uno solo. Un nombre sin definición en el repositorio
representa sus sitios de llamada externos.

**Las llamadas que el índice no pudo decidir se cuentan, no se esconden.** Una
llamada sobre un receptor sin tipo a un nombre que el repositorio tiene
(`svc.update()` con `svc` desconocido) no tiene destino: nunca se recorre, se
cuenta en `undecided_calls` / `below_confidence`, y `min_confidence: "low"` la
lista, marcada `undecided`.

**Un resultado vacío sigue significando "no encontré", nunca "no hay".** Tres
de las razones antes eran silenciosas y ya no lo son: una rama sin filas
(`branch_fallback`), un índice construido por un extractor más viejo (`warning`
en cada respuesta de grafo, `extractor_stale` en `index_status` — corré `devctx
index --full`) y lo que un filtro dejó afuera (`below_confidence`, `excluded`,
`omitted`). Las de abajo siguen sin anunciarse. Ante un vacío, cruzá con
`search --keyword` antes de renombrar o borrar.

**La resolución es estática y por lenguaje** — imports, tipos declarados,
scopes y las reglas de la tabla de arriba —, no la de un compilador. Una arista
`high` equivocada es rara en los repositorios medidos; una que falta, no.

**El despacho dinámico es visible a medias.** Una llamada por una interfaz, una
clase abstracta o un trait se sigue (`via: "dispatch"`, nunca `high`). Una
llamada por un callback, un valor función, reflexión, un servicio buscado por
nombre o un registro llaveado por strings no deja ninguna arista. El grafo va a
sub-reportar justo donde un lenguaje es más dinámico.

**Solo 7 lenguajes producen aristas.** En un repositorio políglota, el grafo
cubre una parte, y no hay aviso que diga cuál. `devctx status` muestra el conteo
de símbolos; uno sospechosamente bajo suele significar que el código está en un
lenguaje que se está indexando como texto.

## Cómo complementa a la búsqueda

| Pregunta | Herramienta |
|---|---|
| *¿Dónde está el código sobre autenticación?* | `search` |
| *¿Qué llama a `authenticate`?* | `get_references` |
| *¿Qué se rompe si lo cambio?* | `impact_analysis` |
| *¿Qué lo rodea — sus tipos, su jerarquía, sus miembros?* | `traverse` |
| *¿Por dónde empiezo en este repositorio?* | `repo_map` |
| *¿Por qué está escrito así?* | `memories_by_symbol` |
| *¿Qué debería leer antes de responder?* | `build_context` |

La búsqueda es difusa y rankeada. El grafo es estructural, y dice qué tan
seguro está de cada arista. La fila del *por qué* es la que a nadie se le
ocurre preguntar, y es la única que el código no puede responder en absoluto.

## Modelo mental

La búsqueda es un mapa del territorio: te muestra qué está cerca de qué, por
significado. El grafo es la red vial: te muestra qué conecta realmente con qué,
y por lo tanto adónde se va el tráfico cuando cerrás una calle.

Querés el mapa para encontrar el barrio, y la red vial antes de romper el
pavimento.
