# TASK-007 — Resolvers Python, Rust y Go

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-005
- **Estado:** `done`

---

## Objetivo

Que Python (workers de ACME), Rust (este repo) y Go (fixtures) tengan resolución por imports/módulos,
receptores tipados y externos marcados, con las llamadas a nivel de módulo de Python como aristas.
Implementa DD-7 (Python, Rust, Go).

## Contexto verificado

- Python: `types` solo `assignment` y `typed_parameter` con anotación (`languages/python.json`); sin
  `self.x = Foo()`; llamadas de módulo descartadas (`crates/devctx-parse/src/parser.rs:125-127`,
  TASK-003 ya les da fuente). Builtins como targets pelados (`len`, `print`). Repo de campo:
  legacy-migration (módulos de la raíz y de `src/`, scripts con `if __name__ == "__main__"`).
- Rust: `calls` con `scoped_identifier name:` → `Foo::bar` queda `bar` (`languages/rust.json`);
  `container_kinds: ["impl_item", "trait_item"]`; `impl Trait for T` no se registra; `types` solo
  `type_identifier` en `let`/params/campos. 49 `impl X for Y` en `crates/*/src/*.rs` de este repo.
- Go: `container_kinds: []` (`languages/go.json`), así que `(s *Server) Handle` no se califica;
  `selector_expression` con `operand` en `receiver_of` (`parser.rs:349`). No hay repo Go del usuario
  (Q-6 del master).

## Archivos

- **Crear:** `crates/devctx-parse/src/resolve/{python,rust,go}.rs`, fixtures multi-archivo en
  `crates/devctx-parse/tests/fixtures/{python,rust,go}/`.
- **Modificar:** `crates/devctx-parse/languages/{python,rust,go}.json`, `crates/devctx-index/src/link.rs`
  (módulos Python por ruta/paquete, `crate::`/`super::` de Rust leyendo los `Cargo.toml` del
  workspace para saber qué crates son del repo, `module` de `go.mod`).

## Pasos

- [x] **Paso 1 — tests que fallan.** Python: dos llamadas a `helper()` desde el mismo método → dos
      aristas; llamada a nivel de módulo; `from .lookup import lookup_id` (relativo);
      `self.repo = Repo()` en `__init__` y `self.repo.save()`; `len(x)` → `external_known`. Rust:
      `Store::open(…)` → `Store.open` resuelto por `use`; `Self::helper()`; método de `impl Trait for
      T` → `T.m` + `implements`; `serde_json::from_str` → externo. Go: método con receptor; import
      del propio módulo vs `fmt`.
- [x] **Paso 2 — Python.** Paquetes por carpeta (`__init__.py`) y raíz del repo; `import a.b as c`,
      `from x import y`, `from x import *` (exports si está en repo); atributos de instancia
      tipados por asignación en `__init__`; builtins/stdlib en `python.json`.
- [x] **Paso 3 — Rust.** `use` (incluye grupos `{A, B}` y `self`), `mod`, `crate::`/`super::`;
      path como receptor en `Foo::bar`; `Self`; `impl Trait for T` → `implements`; crates del
      workspace = repo, el resto = externo (`std`, `core`, `alloc` y dependencias).
- [x] **Paso 4 — Go (básico).** Receptor del método como contenedor; imports con prefijo del
      `module` de `go.mod` → repo; resto externo. Nada más (Q-6).
- [x] **Paso 5 — medir.** DevCtxEngine con el arnés (gold edges Rust) y legacy-migration
      (86 archivos, barato) para Python.

## Criterios de aceptación

- [x] Tests del paso 1 verdes.
- [x] Gold edges Python y Rust: precisión ≥ 90 % en `high`, ≥ 75 % global.
- [x] DevCtxEngine: `serde_json::from_str`, `tokio::spawn` quedan `external` (base para el
      criterio B2 que cierra TASK-008).

## Riesgos

- Python dinámico: lo no tipable cae a `medium`/`low`, nunca se inventa `high`.
- Macros de Rust (`format!`, `tokio::main`) no son llamadas: excluirlas de `references.call`.

## Resultado

- **Estado final:** `done`.
- **Resumen:** Python, Rust y Go se resuelven por lo que el archivo importa y por receptores tipados
  con scope, con externos solo con evidencia. Un entorno de enlace nuevo (`LinkEnv`) suma al de
  TS/JS cada `Cargo.toml`, `go.mod`, `pyproject.toml`/`setup.cfg`/`requirements*.txt`: entra al
  fingerprint (cambiar uno relinkea la rama) con fallback por archivo a la última versión buena.
  Un binding cuyo valor es una llamada (`x = make()`, `let s = Store::open(p)?`, `x := New()`)
  guarda lo que devuelve (`typed <via> make()`) y el link lo tipa por el símbolo llamado. Python:
  módulos por ruta y paquete, relativos, reexportes por `__init__.py` (y por cualquier módulo),
  `import *`, `self.x = …` en `__init__`, llamadas de módulo, builtins/stdlib/paquetes declarados.
  Rust: crates por `Cargo.toml`, `use` con su scope de módulo, `pub use`, `crate::`/`super::`/
  `Self::`, los `impl` de un tipo en otros archivos, `impl Trait for T`, campos por declaración,
  `std`/`core`/`alloc`/prelude y crates declarados. Go: `module` de `go.mod`, receptor como `self`,
  métodos en otro archivo del paquete. **`EXTRACTOR_VERSION` 11 → 14, `LINK_VERSION` 6 → 9** (uno
  por resolver; cada commit de resolver sube los dos y regenera el golden).
- **Números** (sandbox `mktemp -d -p /var/tmp`, 563 MB con tres binarios `graph_bench`, dos índices
  por repo, los snapshots y el caché de `uv`; `graph_bench` con vectores constantes; antes = c1922d0,
  después = 8a83a34; mismos snapshots; la máquina con carga alta: tiempos indicativos):

  | | legacy-migration (Python) antes | después | DevCtxEngine (Rust) antes | después |
  |---|---|---|---|---|
  | `calls` vivas (+ descartadas) | 1 471 (+89) | 1 177 (+383) | 21 226 (+5 663) | 21 223 (+5 666) |
  | con `dst_id` | 18,9 % | 29,0 % | 25,2 % | 30,6 % |
  | `external` | 32,0 % | 69,4 % | 20,7 % | 49,6 % |
  | **sin decidir** | **49,2 %** | **1,6 %** | **54,1 %** | **19,8 %** |
  | `high` | 44,4 % | 89,7 % | 39,4 % | 68,9 % |
  | gold: correcto / en `high` / cobertura (no `low`) | 13/25 (52 %) · 8/8 · 13/25 | **24/25 (96 %) · 24/24 (100 %)** · 24/25 | 11/21 (52 %) · 5/5 · 12/21 | **21/21 (100 %) · 21/21 (100 %)** · 21/21 |
  | link pass completo | 26 ms | 17-36 ms | 327 ms | 186-275 ms |
  | link pass incremental, 1 archivo | — | completo (*) | — | 305 ms (**) |
  | `VmHWM` (`graph_bench`) | 95 MiB | 91 MiB | 145 MiB | 155-156 MiB |

  (*) el módulo de logging del paquete, que importa casi todo el repo: escritos + importadores
  superan 1/5 → pase completo.
  (**) `crates/devctx-store/src/store.rs`: 24 155 de 36 601 aristas reabiertas por nombre (`new`,
  `get`, `open`…, modo b), del orden del pase completo.

  Desglose de externos, después (resolución × confianza). Python: `external_known` `high` 660,
  `chain_external` `medium` 102, `inherited` `high` 55 (antes: `external_known` `high` 455,
  `chain_external` `medium` 15). Rust: `external_known` `high` 8 119, `chain_external` `medium`
  1 866, `external_known` `medium` 405 (el `T::new()` de un tipo externo), `inherited` `medium` 128
  (métodos derivables), `inherited` `high` 11 (antes: `external_known` `high` 4 357,
  `chain_external` `medium` 44). Las descartadas de Python suben (89 → 383) porque los parámetros
  sin tipo ya no toman el tipo de un atributo homónimo por el scope plano: son receptores sin tipo
  (métodos de objetos de librería sobre un parámetro sin anotar o una tupla desestructurada) con nombres
  que el repo no define. En los `.py` de este repo (fixtures y scripts): sin decidir 36,5 % → 4,6 %.

  Gold Rust (`scripts/graph-eval/gold-devctx.txt`): los 6 de TASK-001 re-anclados a c1922d0 más 15
  nuevos, etiquetados leyendo el código antes de medir (`use` entre crates con `pub use`,
  `Self::`, `impl` en otro archivo, campo y valor tipados, trait por `Arc<dyn …>`, `super::` desde
  `mod tests`, cadena con `?`, `crate::` con homónimos, crates declarados y `std`). Gold Python: 25
  sitios de legacy-migration (6 de TASK-001 re-anclados + 19 nuevos, etiquetados antes de medir),
  versión auditable con alias opacos en `scripts/graph-eval/gold-python-anon.txt`; el único que
  falla es un parámetro sin tipo que recibe una instancia (honestamente sin decidir). Go no tiene
  repo del usuario (Q-6): cubierto por fixtures.

  *Auditoría contra el código:* Rust, 25 aristas `high` internas al azar, 25 `external` `high`, 10
  `external_known` `medium`, 8 `inherited` `medium` y 10 `chain_external`: 78/78 correctas
  (incluye una variante de enum → su enum, `Self::` de un `impl` en el mismo archivo, `run()` del
  archivo desde `mod tests`, `?` sobre `Result<Option<_>>` → `Option.or_else`). Python, 20 `high`
  internas y 20 externas al azar: 40/40 (funciones anidadas de `main`, imports desde un script
  que agrega la raíz a `sys.path`, `assertEqual` heredado de `unittest.TestCase`, una función del
  repo anotada `-> logging.Logger`).
  *Sin regresión:* el desglose por resolución × confianza de `.java`, `.ts`, `.tsx` y `.js` del clon
  de este repo es idéntico antes y después (y `.rs`/`.py` idénticos entre el commit de Rust y el de
  Go); `link_java` y `link_typescript` siguen verdes sin cambios.
- **Criterios:** tests del paso 1 verdes (ver abajo); gold Python y Rust ≥ 90 % en `high` (100 % y
  100 %) y ≥ 75 % global (96 % y 100 %); `serde_json::from_str` y `tokio::spawn` quedan
  `external_known` `high` en DevCtxEngine (gold y `link_rust`).
- **Hallazgos de §2:** H5 (`Cached<T>`) sigue resuelto (TASK-004); un `impl` en otro archivo del crate
  que su tipo se asocia ahora a ese tipo (en el gold, `Store.branch_symbols` y
  `Store.save_file_state`, de `impl Store` en `symbols.rs` y `state.rs`). Confirmado que antes de esta task Rust y
  Python resolvían solo por nombre (más de la mitad sin decidir) y que el scope plano daba a un
  parámetro el tipo de un atributo homónimo (`self.repo = repo` con otro `repo` en otro método).
- **Archivos tocados:** `crates/devctx-parse/src/{parser.rs, registry.rs, facts.rs,
  resolve/mod.rs, resolve/scope.rs, resolve/link.rs, resolve/typescript.rs, resolve/env.rs (nuevo),
  resolve/python.rs (nuevo), resolve/rust.rs (nuevo), resolve/go.rs (nuevo)}`,
  `crates/devctx-parse/languages/{python,rust,go}.json`, `crates/devctx-parse/tests/{link_python.rs,
  link_rust.rs, link_go.rs (nuevos), common/linked.rs (nuevo), common/mod.rs, facts_rust.rs,
  facts_go.rs, extractor_golden.rs → golden/extractor.txt, fixtures/{python,rust,go}/link/… (nuevos)}`,
  `crates/devctx-index/src/{env.rs (nuevo), link.rs, pipeline.rs, lib.rs}`,
  `scripts/graph-eval/{gold-devctx.txt, gold-python-anon.txt (nuevo), README.md}`, `CHANGELOG.md`,
  el design (DD-6/7/8/9) y el master. Públicos nuevos: `devctx_parse::resolve::env::{LinkEnv
  {script, cargo, go, python}, LinkEnv::{fingerprint, to_json, from_json, sort}, ManifestKind,
  CargoManifest::parse, GoModule::parse, PyManifest::parse, py_import_names}`,
  `RepoIndex::with_env`, `LinkSymbol.{start_line, end_line}`, `resolve::python::{Python,
  PyImport, type_text, return_type}`, `resolve::rust::{Rust, RsIndex, normalize_path,
  type_of_text, return_type, field_type, unwrapped}`, `resolve::go::{Go, type_text, return_type,
  field_type, import_name}`, `LangResolver::{fields_by_link, late_module_bindings, literal_type,
  type_text, import_hint}`, `Scopes::resolver`, `Binding.this`. `RepoIndex::importers_of` pasó a
  `link.rs` (TS, Python y Rust). Claves JSON nuevas: `platform_modules`; capturas nuevas en
  `types`: `@bind.attr` (Python), `@bind.receiver` (Go), y `@_x` (solo para predicados). Hints
  nuevos: `typed <via> X()` / `X()?`, `path <prefijo>`, `chain f? …`; las filas `imports` de un
  `from … import` de Python llevan `from`, y un `pub use` de Rust `export`.
- **Tests** (`TMPDIR=/var/tmp cargo test --workspace --locked`): `link_python` (10), `link_rust`
  (8), `link_go` (4) sobre fixtures multi-archivo; antes de implementar fallaban **8 de 10**, **7 de
  8** y **4 de 4** (pasaban ya: dos llamadas a `helper()` = dos aristas —TASK-003— y "un paquete
  necesita su manifest", porque sin reglas todo quedaba sin decidir; y "las macros no son llamadas",
  que ya era cierto: el árbol de una macro no tiene `call_expression`). `resolve::env::tests` (6),
  `env::tests` (3), `resolve::{python,rust,go}::tests`, `link::tests::hint_names_are_read_by_position`
  (call markers, `path`). En `devctx-index`: `a_changed_manifest_relinks_and_an_unreadable_one_does_not`
  (falla si los manifests no entran al entorno, verificado mutándolo) y cinco escenarios más en
  `an_incremental_link_pass_equals_a_full_one`, cada uno verificado fallando sin su regla (mutación
  de la regla, el fix ya escrito): `__init__.py` que reexporta (sin Python en el modo d),
  `requirements.txt` (sin requirements en el entorno), `pub use` de la raíz del crate (sin Rust en
  el modo d), `Cargo.toml` que agrega un crate (sin Cargo en el entorno) y `go.mod` que renombra el
  `module` (sin go.mod en el entorno). `an_added_homonym_reopens_a_resolved_edge_by_name` pasó a
  TypeScript (en Python un nombre libre ya no es `unique_name`). Gate antes de cada commit de
  código: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked -- -D
  warnings` (y `--features gpu`); `cargo test --workspace --locked`: verde.
- **Contrato JSON:** sin cambios en las tools; `index` avisa una línea por manifest ilegible.
- **Desviaciones:** (1) **cuatro commits de código** (entorno compartido, Python, Rust, Go), cada uno
  con su gate y su golden, en vez de uno. (2) Un binding de una llamada guarda la llamada (`typed
  local make()`) en vez de un tipo: el link lo tipa por el símbolo (clase, retorno declarado, algo de
  afuera); en Rust `?` y `.unwrap()` desenvuelven `Result`/`Option`. (3) Una llamada a una clase de
  Python (o a una variante de enum de Rust) apunta a la clase (o al enum), no a `__init__`. (4) La
  regla 7 (`unique_name`) no se usa en los tres lenguajes nuevos: un nombre que el lenguaje no ve
  queda sin decidir (lección 2), aunque haya un único homónimo en el repo. (5) Go: con `go.mod`,
  todo import fuera del `module` es externo aunque no esté en `require` (Go no compila uno así). (6)
  Los métodos derivables de Rust que faltan son externos `medium`; el `T::new()` de un tipo externo
  da un valor externo `medium`. (7) Un `use` dentro de una función cuenta para todo su módulo.
  (8) `self.x.y()` en Python queda sin tipo (Python no declara campos; no se crean símbolos de
  atributos). (9) Se agregaron símbolos: métodos de trait sin cuerpo (Rust) y de interfaz (Go).
  (10) Modo (d) para Python con todo módulo como reexportador: en un repo con un módulo que importa
  casi todo (el de logging) el incremental cae al completo.
- **Riesgos abiertos / siguiente:** (1) **no verificado** en un repo Go real, ni en un repo Python o
  Rust grande ajeno; macOS/Windows solo compilado (CI). (2) Python dinámico: sin inferencia de
  `x = otro_objeto.metodo()`, de parámetros sin anotar ni de decoradores que cambian el tipo; los
  nombres de distribución → módulo son best effort (tabla corta + normalización). (3) Rust: los
  módulos se toman por ruta (sin `#[path]` ni `mod` condicional), los `impl` genéricos de traits
  (`impl<T: X> Y for T`) no se asocian a ningún tipo, las macros que definen ítems no se ven, y
  `let x = a.metodo()` queda sin tipo. (4) El incremental de un archivo de Rust muy nombrado
  reabre buena parte de la rama por nombre (modo b), con costo del orden del completo (~0,3 s acá).
  (5) Sandbox de medición en `/var/tmp/devctx-t007-*`.
- **Revisión de d8cda45 (REQUEST CHANGES, sin BLOCKER; dos revisiones en paralelo), resuelta en
  commits encima de d8cda45:**
  - *Rust/Go (eb2b376).* R1: un crate del workspace cuenta solo si es el propio o una dependencia
    por path del `Cargo.toml` del archivo (mapeada por el path, así un `package =` renombrado llega;
    las `workspace = true`, por la entrada de la raíz); una dependencia declarada del registry es
    externa aunque el workspace tenga un homónimo; un crate que nadie declara queda sin decidir
    (`CargoManifest.{local, inherited}`, tablas inline en el lector de TOML, crates homónimos de
    workspaces anidados como `name@dir`). R2: un parámetro de tipo y `dyn A + B` se tipan por todos
    sus bounds y el link prueba cada uno. R3: fuera de Java el método inherente gana al de un `impl`
    de trait, y de varios candidatos (`From<A>`/`From<B>`) ninguno es `high`. R4: dos ítems
    homónimos (`#[cfg]` dual) dan `medium`. R5: un `use` dentro de una función vale solo en ella
    (rango de líneas, como los `mod`) y le gana al módulo. R6: en Rust y Go un método que falta con
    un supertipo externo es `inherited` `medium`. Menores: el dueño de un `impl` sale de su tipo
    escrito con ruta (y el fallback solo con `Unknown`), `Arc/Box/Rc::new(x)` vale lo que `x`, una
    fn anidada o un valor local sombrean al `make` importado, una fn asociada con mayúscula no es
    variante, lifetimes en los argumentos de un tipo; Go: `go.mod` ancestro para tratar como externo
    un import con punto, prefijo de módulo más largo, nombre local por la cláusula `package`,
    homónimos por build tags en `medium`, interfaces anónimas sin símbolos, `var a, err = f()` tipa
    solo `a`. Medición (palanca pedida): `let x = a.f()` hace de `x.g()` una cadena tras `a.f()`
    (Rust, Go, Python), `T::default()` sin `impl` vale `T` (`medium`) y un `mod x;` de un crate de
    tests llega a `tests/x/mod.rs`. **`EXTRACTOR_VERSION` 15, `LINK_VERSION` 10.**
  - *Python (6dd91d4).* P1: `Any`, `object`, un `TypeVar` y una unión de varios tipos no tipan nada
    (antes `Any` daba `external_known` `high` y un tipo que no resuelve caía a `unique_name`);
    `type[X]` es `X`; `-> Self` es la clase. P2: el alcance de un import suma el `__init__.py` de
    cada paquete del camino, y un `__init__.py` escrito reabre su directorio. P3: solo un nombre
    declarado exacto (o de la tabla de alias) le gana a un módulo del repo; uno adivinado
    (`python-utils` → `utils`) pierde con el módulo del repo, y un nombre de la stdlib que el repo
    también tiene queda sin decidir (`PyManifest.guessed`). Menores: dos imports del mismo nombre
    local (`try`/`except ImportError`) dan `medium`; `from ...` más allá del paquete de arriba no es
    módulo. **`EXTRACTOR_VERSION` 16, `LINK_VERSION` 11.**
  - *Rendimiento (51fb4e1).* Los nombres de un importador que reabren por (d) son solo los que
    pueden tipar una cadena (tipos, campos, invocables con retorno declarado no vacío): reemplaza
    `GENERIC_NAMES`, que dejaba afuera un `pipe(): Repo`. Una llamada por path de Rust resuelta
    externa no se reabre por nombre. En un importador Rust se reabren solo las filas que nombran lo
    que ligan sus `use` (más cadenas y miembros; un glob de otro módulo lo reabre entero). Fix
    encontrado por un escenario: un glob cuyo módulo reexporta el nombre desde afuera lo da externo,
    y una rama que no se puede seguir va antes que la externa. **`LINK_VERSION` 12.**
    `store.rs` tocado: **24 307 → 18 179 aristas re-resueltas**; 243 → 230 ms (lo domina cargar la
    rama, ~140 ms, como en el pase completo: no se pudo bajar del orden del completo).
  - *Menores (90b4d97).* TS y Python: en un barrel o un `import *`, la rama que no se puede seguir
    va antes que la de un paquete (sin decidir, no externo `high`). TOML: arrays anidados en varias
    líneas. Manifests de Python: `requirements/*.txt`, `nombre @ url`,
    `[tool.poetry.dependencies.foo]`, línea en blanco en `install_requires`, `ruamel.yaml` →
    `ruamel`. `tsconfig` (mi1): con la base rota y `tsconfig.json` legible gana el fresco; una base
    de `extends` que no se lee conserva el último `tsconfig` bueno. **`LINK_VERSION` 13.**
  - *Tests que fallaban antes del fix* (escritos primero; ver cada uno fallar): `link_rust` 7 de 7
    nuevos (`declared_dependencies_decide_which_crate`, `every_bound_is_tried`,
    `trait_impl_homonyms_are_not_sure`, `cfg_homonyms_are_medium`,
    `a_use_in_a_function_is_scoped_to_it`, `an_external_supertype_is_medium_evidence`,
    `owners_values_and_variants`) y, de la medición, `a_derived_default_is_its_type`,
    `a_local_from_a_method_call_is_a_chain` (también en Python) y
    `a_test_crate_reaches_its_common_module`; `link_go` 3 de 3 (`an_external_embedding_is_medium_evidence`,
    `package_names_tags_and_shadows`, `modules_are_matched_by_ancestor_and_longest_prefix`);
    `link_python` 3 de 3 (`typing_special_forms`, `weak_external_evidence_loses_to_the_repository`,
    `import_forms_and_fallbacks`) más `an_unfollowable_star_import_is_not_outweighed_by_a_package`;
    `link_typescript::an_unfollowable_branch_is_not_outweighed_by_a_package`;
    `resolve::env::tests::manifest_reader_coverage`; `tsconfig::review_tests` (2). Escenarios
    nuevos en `an_incremental_link_pass_equals_a_full_one`, cada uno verificado fallando sin su
    regla (mutándola): `import pkg.sub` + `__init__.py` (sin los prefijos en el alcance), un
    directorio que pasa a paquete (sin la reapertura del directorio), `pipe(): Foo` de un importador
    (con la lista `GENERIC_NAMES`), la fila `use crate::Svc` de un importador (con el filtro de
    importadores dejando afuera las filas que no son llamadas), el glob de un prelude (sin Rust en
    el modo d); y uno que faltaba, un `impl` de otro archivo que pierde `save` (lo cubre el destino
    que desaparece). `a_unique_name_type_does_not_make_a_typed_call_high` pasó a TypeScript.
  - *Gold:* Rust a 26 sitios (6 nuevos: dependencia heredada del workspace, dos homónimos por
    `#[cfg]`, un `use` dentro de una fn, un `clone` de un tipo con `impl Default`, un `store` sobre
    `Arc::new(T::default())`; se quitó uno repetido de `EmbeddingProvider.embed`); Python a 27 (2
    nuevos: un paquete declarado con su nombre exacto y un módulo de la stdlib sin homónimo, que la
    evidencia débil de P3 no debe tocar). **No hay en los repos medibles sitios de R2 (varios
    bounds), R3 (dos `impl From`), P1 (`Any`/`Union`/`Self`/`TypeVar`) ni del `try`/`except`:
    quedan cubiertos solo por fixtures.**
  - *Re-medición* (mismos snapshots; "ronda 1" = 8a83a34, "ahora" = 90b4d97):

    | | Rust ronda 1 | Rust ahora | Python ronda 1 | Python ahora |
    |---|---|---|---|---|
    | sin decidir | 19,8 % | **17,5 %** | 1,6 % | 1,6 % |
    | gold correcto / en `high` | 21/21 · 21/21 (con el gold de 21) | **26/26 · 22/22** | 24/25 · 24/24 | **26/27 · 26/26** |
    | externas | 49,6 % | 52,0 % | 69,4 % | 69,5 % |
    | link pass completo | 186-275 ms | 393 ms (carga alta) | 17-36 ms | 27 ms |

    Rust ahora, externas: `external_known` `high` 8 475 (+357: llamadas a `std` dentro de `mod
    tests` con `use super::*` que antes quedaban sin decidir por el orden de los globs),
    `chain_external` `medium` 2 235, `external_known` `medium` 451, `inherited` `medium` 159 (los
    `inherited` `high` desaparecen: R6). El 17,5 % sin decidir por hint: `chain chain` 26,6 %,
    `untyped` 23,8 %, `chain typed` 15,5 %, `chain path` 12,2 %, `expr` 5,3 %, `chain untyped`
    4,6 %, `chain free` 2,7 %, `member` 2,6 %: lo que queda son sobre todo cadenas tras una llamada
    externa a un nombre que el repo también define (sin decidir por diseño, TASK-005) y receptores
    sin tipo. Los gold que antes pasaban siguen pasando; en `high` 22/22 (los 4 restantes son
    `medium` a propósito: homónimos por `#[cfg]`, el derivable y `T::default()`).
    *Auditoría:* 20 aristas que pasaron a externas `high` en esta ronda, al azar: todas son llamadas
    a `std` o a crates declarados (`Instant`, `Duration`, `Mutex::new`, `Path::new` en tests con `use
    super::*`, `middleware::from_fn_with_state` de axum, `Vec::iter` tras `unwrap()`); dos leídas
    línea a línea contra el código (el `iter` y un `PathBuf::clone` tras un método del repo que
    declara `-> PathBuf`): correctas. 12 `return_type` nuevas al azar: consistentes con el hint, tres
    leídas contra el código (`open_store()?` → `Store`, `serve_for()` → `Fake`,
    `CudaFallback::<…>::new` → `Self`): correctas.
    Java y TS: mismo desglose que antes de TASK-007.
  - *No verificado / abierto:* no hay repo Python grande en el sandbox para medir el costo del modo
    (d) de Python; el incremental de Rust sigue del orden del completo porque lo domina la carga de la
    rama; no hay sitios reales de R2/R3/P1; el "Klass anidado sombreado" de TS (NIT) no se tocó.

- **Segunda revisión.**
  - *Selección incremental (59f586b).* La regla "una llamada por path externa no se reabre por
    nombre" queda solo para `external_known` cuyo primer segmento es la plataforma o un crate de
    afuera declarado, sin ítem, módulo ni `use` del archivo con ese nombre; un `inherited` o un path
    por un módulo del repo vuelven al modo (b). Los `const`/`static` de un importador reabren por (d).
    Escenarios nuevos en la equivalencia incremental = completo (24 pasos). **`LINK_VERSION` 14.**
  - *Rust (62c66fe).* Un tipo que no resuelve no tipa nada en Rust y Go (nunca `unique_name`); un
    `use` dentro de un bloque llega solo a sus líneas; sin manifest gana la declaración del ancestro
    más cercano; `use super::super::*` desde un `mod` de primer nivel bloquea el filtro de
    importadores; `rs_build` indexa por archivo. **`EXTRACTOR_VERSION` 17, `LINK_VERSION` 15.**
  - *Python (15ad34a).* `requirements/*.txt` declara para el directorio de arriba; un directorio sin
    `.py` no sombrea la stdlib; `nombre@url` sin espacios; `-r` documentado como no seguido.
    **`LINK_VERSION` 16.**
