# TASK-014 — Tool `skeleton` por archivo

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-004
- **Estado:** `done`

---

## Objetivo

Una tool nueva `skeleton(file)` que devuelve la estructura de un archivo —símbolos con firma, kind y
rango, anidados por contenedor, sin cuerpos— leída de la tabla `symbols`. Sirve para **localizar**
barato (Agentless, `get_symbols_overview` de Serena); la evidencia dice que no reemplaza al código
como contexto de edición, y la descripción del tool lo dice. Implementa DD-17.

## Contexto verificado

- Tras TASK-003/004, `symbols` tiene por archivo: `kind`, `name`, `qualified`, `parent_id`,
  `signature` (normalizada, tope 200 caracteres), rangos; el símbolo de archivo es la raíz.
- Hoy lo más parecido es el chunk `file` (`crates/devctx-chunk/src/chunker.rs:206-222`: imports
  crudos + lista plana `- nombre (kind)`), que no tiene firmas ni jerarquía.
- `read_file` (`do_read_file`, `crates/devctx-mcp/src/state.rs:922`) resuelve rutas relativas al
  repo; reusar esa resolución para el parámetro `file`.
- Patrón de alta de tool: ver TASK-011.

## Archivos

- **Modificar:** `crates/devctx-store/src/symbols.rs` (`file_symbols(repo, branch, file)` ordenado),
  `crates/devctx-mcp/src/state.rs` (`do_skeleton`, render), `crates/devctx-mcp/src/backend.rs`,
  `crates/devctx-mcp/src/lib.rs` (`#[tool]`), `crates/devctx-api/src/lib.rs` (`GET /skeleton`),
  `crates/devctx-cli/src/main.rs` (`devctx skeleton <archivo>`).

## Pasos

- [x] **Paso 1 — tests que fallan.** (a) archivo Java con clase, clase interna, constructor y métodos
      → árbol con indentación por `parent_id`, firmas y `[l1-l2]`; (b) TS con arrow-const exportado;
      (c) archivo no indexado → error con sugerencias de rutas cercanas (`file_index`); (d) rama v1
      → error que pide `devctx index --full`; (e) nunca aparece una línea de cuerpo.
- [x] **Paso 2 — render** textual (va al contexto del modelo): una línea de imports resumidos
      (cantidad + los primeros), luego el árbol; tope por `DEVCTX_MAX_OUTPUT_TOKENS` con
      `[devctx] N símbolos más`.
- [x] **Paso 3 — exposición** MCP/API/CLI; descripción: "para ubicar dónde está algo; para editar,
      `read_symbol`/`read_file`".
- [x] **Paso 4 — medir** tokens de `skeleton` vs `read_file` en 5 archivos grandes de los repos del
      arnés.

## Criterios de aceptación

- [x] Tests (a)-(e) verdes.
- [x] Tabla de tokens skeleton vs archivo completo en el Resultado (esperado: ≤ 15 % en archivos de
      > 300 líneas).

## Riesgos

- Firmas multilínea mal cortadas (anotaciones Java, decoradores TS): normalización de DD-17 y
  fixtures específicos.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. Tests (a)-(e) verdes más fixtures multilínea (anotaciones Java,
  decoradores TS, genéricos largos, Rust con `where`); tokens del skeleton **4,9-10,2 %** del
  archivo en 12 archivos de 623-4 684 líneas (meta ≤ 15 %); p95 caliente **≤ 26 ms**.
- **Resumen:** tool `skeleton(file)` (MCP, `GET /skeleton?file=…`, `devctx skeleton <archivo>`,
  `Backend` local y remoto con 404 tipado). Lee las filas de `symbols` del archivo (nunca el
  archivo) y dibuja texto: `<archivo>: N symbols`, `imports (N): …` (los primeros 8 por línea;
  solo esos se leen), y una línea por símbolo en preorden del árbol por `parent_id` (dos espacios
  por nivel) con la firma limpia y `[l1-l2]`. Tope `DEVCTX_MAX_OUTPUT_TOKENS` con `[devctx] N
  more symbols (lines a-b) … read_file with start_line=a`. Descripción: "Use it to locate where
  something is; to edit, read_symbol / read_file".
- **Decisión: los tests se marcan, no se ocultan.** `repo_map` elige qué mostrar de todo el repo
  y los tests no orientan; `skeleton` contesta por un archivo que el usuario nombró: si es un
  test, ocultarlo daría un vacío; si es un archivo con `#[cfg(test)] mod tests`, ocultar esa
  parte haría que el rango del archivo no cuadre con lo mostrado y escondería justo lo que se
  puede estar buscando (el test de una función). Marcar cuesta ~2 tokens por línea de test; en un
  archivo de test la cabecera lo dice una vez (`, test file`) y las líneas no repiten la marca.
- **Archivos tocados:**
  - `crates/devctx-store/src/skeleton.rs` (nuevo): `pub struct FileImports { total, first }`,
    `Store::file_imports(repo, branch, file, limit) -> Result<FileImports>`,
    `Store::has_file_rows(repo, branch, file) -> Result<bool>`,
    `Store::file_suggestions(repo, branch, asked, n) -> Result<Vec<String>>`,
    `pub fn rank_file_suggestions(asked, files, n) -> Vec<String>`; `store.rs::edit_distance`
    pasa a `pub(crate)`. `file_symbols` ya existía (ordenado por byte) y se reusa.
  - `crates/devctx-mcp/src/skeleton.rs` (nuevo, puro): `SkeletonQuery { file, label }`
    (`of`, `validate`, `query_pairs`), `SkeletonInput`, `render(input, budget) -> String`,
    `IMPORTS_SHOWN` (8), `MIN_SKELETON_BUDGET` (256).
  - `crates/devctx-mcp/src/state/skeleton.rs` (nuevo): `SKELETON_NEEDS_REINDEX`, `repo_relative`,
    `skeleton_on`; `state.rs`: `pub fn do_skeleton(state, q)`, `pub fn skeleton_at(store, root,
    default_branch, q)`.
  - `crates/devctx-mcp/src/repo_map.rs`: `clean_signature(signature, kind, name)` (`pub(crate)`)
    sale de `display_signature` sin cambiar su salida (tests de repo_map sin tocar).
  - `crates/devctx-mcp/src/backend.rs`: `Backend::skeleton`, `SKELETON_OLD_SERVER`,
    `skeleton_path`; `lib.rs`: tool `skeleton` (`SkeletonReq { project, file }`; como
    `read_file`, una ruta absoluta sirve de pista de proyecto; con `project` resuelto, primera
    línea `[devctx] skeleton from <proyecto>` dentro del presupuesto).
  - `crates/devctx-api/src/lib.rs`: `GET /skeleton` (400 con `file` vacío; los errores de la tool
    son 500, nunca 404). `crates/devctx-cli`: `devctx skeleton <archivo>`, `Remote::skeleton`
    (404/405 → "restart it"), mapa de ayuda EN/ES.
  - Docs: `docs/03-core-concepts/{symbol-graph,mcp-integration}.md` y sus pares ES (sección
    nueva con salida real sobre este repo, tabla de tools, 27 tools), `CHANGELOG.md`
    (Unreleased), DD-17 en el design.
  - Sin claves nuevas de `languages/*.json` ni de config; sin cambio de `EXTRACTOR_VERSION`.
- **Contrato:** tool nueva, texto. Nada cambió en `read_file`, `read_symbol`, `repo_map` ni en
  las demás: sus tests (y los de `mcp_tools`) quedaron verdes sin tocar asserts; solo se
  agregaron tests.
- **Tests** (escritos antes del código, vistos fallar contra stubs: los 6 de `render`, los 2 del
  store y 6 de los 8 de estado; (d) y la entrada del CLI pasaban ya con la cola, y (d) se verificó
  por mutación):
  - `devctx-store skeleton::tests`: `suggestions_rank_tail_then_name_then_near_miss`,
    `imports_are_counted_and_only_the_first_read`.
  - `devctx-mcp skeleton::tests`: `a_tree_by_parent_with_signatures_and_ranges`,
    `the_order_is_the_source_order_whatever_the_input` (falla ordenando sin nombre: por id),
    `the_budget_is_kept_and_what_is_cut_is_named`, `test_code_is_marked_not_hidden`,
    `imports_are_one_line`, `cut_signatures_are_named`.
  - `devctx-mcp state::skeleton::tests`: (a) `a_java_class_with_an_inner_class`,
    `multi_line_signatures_are_one_line`, (b) `a_typescript_arrow_const`, (c)
    `a_file_not_indexed_is_an_error_with_nearby_paths`, (d) `an_old_index_asks_for_a_full_reindex`
    (extractor viejo y rama sin filas), (e) `no_body_line_ever_appears` (Java, TS, Rust, Python),
    `test_code_is_marked_and_python_is_drawn`, `the_cli_entry_point_draws_the_same`;
    `skeleton_bench` (ignorado, la medición).
  - Exposición: `backend::tests::skeleton_on_an_old_server_says_to_restart_it`,
    `remote::tests::skeleton_on_an_old_server_says_to_restart_it`,
    `tests::skeleton_takes_its_file_from_the_query` (API), `mcp_tools::skeleton_refuses_an_empty_file`.
  - **Mutaciones** (revertir + `touch`, cada una hace fallar su test): orden sin nombre; sin reserva
    de la línea de cierre; sin marca `[test]`; sin el chequeo de índice viejo; sin el resguardo
    `kind nombre …`; sin sugerencias; sin quitar la raíz de una ruta absoluta; `LIMIT` de imports
    abierto; sin cortar el valor de un campo; sin quitar el `;`; sin la ruta `/skeleton`; sin el
    404 tipado (Backend y CLI); la tool sin validar `file`.
  - Commits: `9ba64c7` feat(skeleton) lectura y render; `7fa1f27` feat(skeleton) exposición
    MCP/API/CLI/Backend; y el de docs.
  - Gate por commit: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked
    -- -D warnings` con y sin `--features gpu`; `TMPDIR=<sandbox>/tmp cargo test --workspace
    --locked --no-fail-fast`. En el primero falló una vez por carga
    `serve_lifecycle::a_foreign_lock_still_fails_fast` (2,25 s contra 2 s, carga ~14-17); solo
    pasó al reintentarlo. El segundo: verde (1 206 tests).
- **Números** (sandbox `mktemp -d -p /var/tmp`; copias de solo lectura con `tar` sin
  `.devctx`/`target`/`node_modules` —y sin las cachés de build del frontend—, `find` sin
  `.devctx` antes de indexar; índices con el embedder sin modelo del arnés `skeleton_bench`;
  binario de test `--release`; carga de la máquina 14-20). "Archivo" = tokens del archivo entero
  (lo que cuesta `read_file` sin tope; con el tope por defecto se cortaría en 8 000); skeleton =
  la salida de la tool (sin corte en todos: ninguno pasó de 8 000); latencia de 40 llamadas
  calientes de `do_skeleton`:

  | Repo | Archivo | Líneas | Tokens archivo | Tokens skeleton | % | Símbolos | p50 / p95 ms |
  |---|---|---|---|---|---|---|---|
  | backend-a | A1 | 633 | 7 487 | 763 | 10,2 | 39 | 20,0 / 26,1 |
  | backend-a | A2 | 913 | 10 999 | 804 | 7,3 | 41 | 18,0 / 22,0 |
  | backend-a | A3 | 1 418 | 15 100 | 1 234 | 8,2 | 58 | 18,4 / 22,1 |
  | backend-a | A4 | 4 374 | 49 363 | 2 440 | 4,9 | 74 | 17,2 / 20,8 |
  | backend-b | B1 | 1 024 | 12 179 | 966 | 7,9 | 48 | 12,5 / 17,7 |
  | backend-b | B2 | 1 613 | 16 784 | 1 650 | 9,8 | 84 | 12,1 / 17,1 |
  | frontend | F1 | 835 | 8 245 | 842 | 10,2 | 35 | 17,3 / 20,7 |
  | frontend | F2 | 1 248 | 12 317 | 1 108 | 9,0 | 74 | 18,0 / 23,7 |
  | frontend | F3 | 623 | 6 207 | 500 | 8,1 | 39 | 18,1 / 20,7 |
  | DevCtxEngine | `crates/devctx-store/src/impact.rs` | 3 065 | 31 187 | 2 484 | 8,0 | 159 | 15,0 / 19,9 |
  | DevCtxEngine | `crates/devctx-mcp/src/repo_map.rs` | 1 505 | 13 854 | 1 287 | 9,3 | 102 | 15,2 / 18,8 |
  | DevCtxEngine | `crates/devctx-cli/src/main.rs` | 4 684 | 44 075 | 2 936 | 6,7 | 194 | 16,3 / 22,6 |

  (A*/B*/F* son alias de archivos de repos privados: servicios, un recurso y componentes/
  servicios Angular.) Archivos chicos de muchos símbolos rondan el 10 %
  (`crates/devctx-store/src/skeleton.rs`, 264 líneas: 10,1 %); los grandes con métodos largos
  bajan a 5-7 %. Costo de indexado: ninguno nuevo (no se escribe nada).
- **Hallazgos de §2:** H5 confirmado en el render: un `impl<T: Clone> Cache<T> where …` es hijo
  del `struct` (`parent_id`), así que el skeleton lo anida bajo el tipo. Firmas TS: el decorador
  de un miembro no es parte del nodo (rango desde el nombre), el de una clase exportada tampoco
  (lo tiene el `export_statement`); en Java las anotaciones sí están en la firma guardada y el
  render las quita.
- **Desviaciones:**
  1. Sin `kind` por línea ni número de líneas en la cabecera (ver DD-17): el `kind` lo dice la
     firma, y el fin del símbolo de archivo excede en uno la última línea cuando hay salto final.
  2. Sugerencias desde las rutas con símbolos de la rama (`symbols`, `kind = 'file'`), no desde
     `FileIndex` (que mira `vectors` de todas las ramas): solo se sugiere lo que tendría skeleton.
  3. Error propio para "indexado pero sin símbolos" (manda a `read_file`) y para una ruta absoluta
     fuera del repo.
  4. Presupuesto con mínimo de 256 (como `repo_map`) y `0` = sin tope (como `read_file`).
- **Lo que NO se verificó / riesgos abiertos:**
  1. La normalización de firmas del extractor no se tocó: una anotación Java > 200 caracteres se
     come la firma guardada; el skeleton lo dice (`method x …`), `repo_map` sigue mostrando el
     pedazo de anotación. Arreglarlo es subir `EXTRACTOR_VERSION` (quitar anotaciones antes del
     tope): para un plan siguiente o TASK-016.
  2. El skeleton es la vista de la última versión indexada: si el archivo cambió después, los
     rangos pueden no cuadrar (no se lee el archivo para compararlo).
  3. Los imports TS salen uno por nombre importado (`@angular/core#Input`): fiel al índice, más
     largo que agrupar por módulo.
  4. legacy-migration (Python) no se midió; Python cubierto solo por fixture. macOS/Windows solo
     por el cross-check de CI (no corrido en esta task).
- **Revisión (READY WITH CAVEATS) — fixup** sobre `fb29489`, cada punto con su test escrito antes
  (o mutación con `touch`), visto fallar:
  - `cbff8ae` fix(parse) — *MAJOR 3*: el extractor topa la cabeza de la firma, no las anotaciones
    (ver DD-17); `EXTRACTOR_VERSION` 20 → 21, golden regenerado (solo la versión). El
    disambiguador del id no sale de la firma: confirmado (`params`, `impl_args`, forma), ids sin
    cambio (el test lo compara). Primera versión quitaba las anotaciones del todo y rompía Lombok
    (`an_incremental_link_pass_equals_a_full_one`: `@lombok.Data` en la firma de clase lo lee
    `link.rs::accessor`); por eso se guardan delante las que entran enteras. Test
    `leading_annotations_do_not_eat_the_signature` (anotación de 240+ con `:-)` dentro de la
    cadena, decorador TS largo, cabeza cortada con `…`, 200 justos sin `…`, id igual) fallaba
    con la firma `@Operation(summary = "sss…`; `multi_line_signatures_are_one_line` ahora exige
    `public void annotated()` en skeleton **y** en `repo_map`.
  - `583f8c3` fix(skeleton) — *MAJOR 1* `the_closing_line_names_the_lowest_cut_line` (struct A
    l.10, B l.20, impl A l.300 con 100 métodos, impl B l.600: decía `start_line=3xx`, ahora 20);
    *MAJOR 2* `a_path_out_of_the_repository_is_refused_without_looking` (`../x`, `web/../../x`,
    `<raíz>/../x`: mismo mensaje exista o no, sin la raíz) y `symbolic_links_resolve_and_leak_nothing`
    (absoluta por enlace al repo = misma salida; enlace del repo hacia afuera: sin oráculo);
    *MINOR 3* en `cut_signatures_are_named` (`get` dentro de `summary="get the order…"`, `id` en
    `@Path("/{id}…")`); *MINOR 4* `the_imports_line_gives_way_to_the_budget`; *MINOR 5* en el test
    de enlaces; *MINOR 6* `a_skeleton_path_is_relative_to_the_working_directory` (CLI); NITs:
    `the_label_is_not_a_wire_parameter` y el label antepuesto en
    `backend::tests::skeleton_on_an_old_server_says_to_restart_it`; sin `…` falso a 200 justos;
    índice vigente sin símbolos manda a `read_file` (`an_old_index_asks_for_a_full_reindex`).
  - Mutaciones (todas detectadas): sin separar anotaciones; sin conservarlas; cortando las
    largas; sin comillas en `closing_bracket`; sin `…`; cierre con el primero; sin achicar
    imports; resguardo por subcadena; las dos defensas de `..` juntas (cada una sola alcanza, son
    redundantes a propósito); "en disco" sin canonicalizar; sin canonicalizar la absoluta por
    enlace; sin filas → `--full`; `label` deserializable; CLI sin cwd.
  - *MINOR 7*: documentado en las docs EN/ES (namespaces TS no son símbolos, sobrecargas TS no se
    extraen, Python anidado sin fixture). *MINOR 8*: eran 12 archivos de **623**-4 684 líneas (la
    13.ª fila era `skeleton.rs`, 264, fuera de la tabla), corregido aquí, en DD-17 y en el
    CHANGELOG. *NIT* del orden documentado: corregido en las docs.
  - No re-medido tras el fixup: la tabla de tokens es de la versión 20 del extractor; con
    anotaciones conservadas delante y quitadas al mostrar, lo mostrado no cambia salvo en las
    firmas que antes se cortaban.
  - Gate: commit 1 — un fallo por carga en `remote::tests::the_autospawn_mark_is_read_from_the_environment`
    (lee `/proc/<pid>/environ` justo tras el spawn), verde 3/3 al reintentarlo; commit 2 — verde
    (1 213).
