# TASK-005 — Motor de resolución y link pass + resolver Java

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-004
- **Estado:** `done`

---

## Objetivo

Que cada arista de `edges` quede resuelta a una definición del repo (`dst_id`) o marcada como
externa o ambigua, con `confidence` y `resolution`, mediante (a) un `type_map` con scope dentro del
archivo y (b) un link pass por rama al final de cada corrida de indexado. Y el primer resolver
completo: **Java/Quarkus**, el lenguaje de más valor para el usuario. Implementa DD-6, DD-7 (reglas
generales + Java), DD-8 y DD-9.

## Contexto verificado

- `type_map` plano por archivo, primera declaración gana: `extract_type_bindings`
  (`crates/devctx-parse/src/parser.rs:89-112`, `or_insert` en l.108).
- `qualified_target` (`parser.rs:312-341`): `self/this/cls/super` → contenedor; inicial mayúscula →
  tipo (l.327, origen de `LOG.*`); local/campo → `type_map`; si no, nombre pelado. `receiver_of`
  (l.345-358) y `nameable` (l.371-380).
- Java `types` solo `type_identifier` (`languages/java.json`): sin `generic_type`, `var` como tipo.
- Caso real: `DraftResource.java` (backend-b), clases internas
  `AlphaServiceAdapter` (campo `AlphaService service`, l.234),
  `BetaServiceAdapter` (l.293), `GammaServiceAdapter` (l.352); llamada
  `service.findPaginated(…)` en l.310 → hoy `AlphaService.findPaginated`.
- Inventario: 481 `var.*`, 1 272 `LOG.*`, 13 793 targets pelados que colisionan con métodos internos
  (`map` 1 247, `item` 1 446, `build` 771), `persist` sin calificar desde
  `RenderResultHandler.java:107` (receptor con genérico o encadenado).
- Momento del link pass: después de procesar archivos y antes del sello del extractor
  (`crates/devctx-index/src/pipeline.rs:542-562`); la poda de stale (l.478+) ya corrió.
- `path_kind` para `from_test`: `crates/devctx-core/src/kind.rs:98`.

## Archivos

(Como quedó; la versión original ponía el motor en `devctx-index`.)

- **Crear:** `crates/devctx-parse/src/resolve/scope.rs` (scopes léxicos y bindings por scope),
  `crates/devctx-parse/src/resolve/java.rs` (resolver Java: `init_type`, `declared_type`, `arity`),
  `crates/devctx-parse/src/resolve/link.rs` (**el motor**: `RepoIndex`, reglas DD-7 1-9, puro),
  `crates/devctx-index/src/link.rs` (orquestación del link pass: carga, selección incremental,
  escritura por lotes, `LINK_VERSION`, `link_pending`), fixtures Java en
  `crates/devctx-parse/tests/fixtures/java/link/` (multi-archivo, con paquetes),
  `crates/devctx-index/examples/graph_bench.rs`.
- **Modificar:** `crates/devctx-parse/src/parser.rs` (retira `qualified_target`/`extract_type_bindings`
  planos en favor del scope; `hint` por llamada), `crates/devctx-parse/languages/java.json`
  (`scopes`, `types` con roles), `crates/devctx-index/src/pipeline.rs` (llamar al link pass;
  `IndexResult.edges_discarded`, `edges_unresolved`, `edges_linked`),
  `crates/devctx-store/src/{schema.rs, symbols.rs}` (`edges.hint`; lecturas por rama y escritura por
  lotes del link pass).

## Pasos

- [x] **Paso 1 — tests que fallan (Java).** Fixture multi-archivo: (a) las tres clases internas de
      `DraftResource` con `service` de tipos distintos → cada llamada a su servicio, `high`,
      `field`; (b) `var q = em.createQuery(…); q.setParameter(…)` → nada con `var.`; (c)
      `LOG.info(…)` con `private static final Logger LOG` → `Logger.info`, `external`; (d)
      constructor injection `this.repo = repo` → `ctor_inject`; (e) `import x.y.Z` y mismo paquete;
      (f) `Office.findByCode(c).flatMap(o -> …)` → `findByCode` resuelto, `flatMap` descartado
      y contado; (g) `new Foo()` → `instantiates`; (h) estático Panache no definido →
      `inherited` + `external`; (i) `for (Foo f : lista) f.bar()` con `List<Foo> lista`.
- [x] **Paso 2 — scopes.** `type_map` por scope léxico (clase → método → bloque/lambda), campos por
      clase (cada clase interna con los suyos), params, locales; `var` desde `new T`/cast/estático de
      tipo del repo; genéricos: tipo base + argumento para for-each.
- [x] **Paso 3 — reglas locales (1-6 de DD-7)** en la fase por archivo: `dst_name` calificado
      localmente y `resolution` provisoria.
- [x] **Paso 4 — link pass.** `RepoIndex` de la rama en memoria (`qualified → ids`, `name → ids`,
      paquete → archivos, herencia); reglas 5-9; incremental según DD-6 (archivos escritos +
      aristas cuyo `dst_name` toca símbolos agregados/borrados + no resueltas; pase completo sobre
      umbral). Escritura por lotes.
- [x] **Paso 5 — descarte de fluent no tipable** (DD-7) con contador en `IndexResult` y en el
      resumen del `index`.
- [x] **Paso 6 — `external`/`from_test`** según DD-9; listas de plataforma Java en `java.json`.
- [x] **Paso 7 — medir** con el arnés sobre backend-b y, con OK del usuario,
      backend-a: tabla de §6 (basura, % sin decidir, externos, tests) y precisión de gold edges
      Java. Memoria del link pass (`VmHWM`) y tiempo incremental de 1 archivo.

## Criterios de aceptación

- [x] Tests (a)-(i) verdes.
- [x] **El link pass excluye `kind = 'contains'`** (m-b de la revisión de TASK-004): ni lo carga
      ni lo reescribe, también en el modo incremental (b) de DD-6, que elige aristas por
      `dst_name`; con un test que lo cubre.
- [x] backend-a (o backend-b si no se aprobó el grande): 0 `var.*`, 0 constantes
      tomadas por tipo, `calls` sin decidir ≤ 15 %, `from_test` = `path_kind` Test en el 100 %.
- [x] Gold edges Java: precisión ≥ 90 % en `high`, ≥ 75 % global.
- [x] Link pass incremental de 1 archivo < 2 s en backend-a; `VmHWM` del indexado ≤ +200 MB
      sobre la línea base (DD-21). Medidos y anotados.
- [x] Compuerta de ruido (master §5) evaluada y escrita en el Resultado.

## Riesgos

- Reglas demasiado agresivas inventan resoluciones: `unique_name` es `medium`, nunca `high`; el gold
  set lo audita por `resolution`.
- Panache/Mutiny tienen convenciones que cambian por versión: las listas viven en el JSON y se
  pueden ajustar sin código.
- El link pass completo en repos grandes: si supera 10 s, perfilar antes de seguir (es el camino de
  cada `index --full`).

## Resultado

- **Estado final:** `done`.
- **Resumen:** cada llamada sale del parse con su receptor tipado por **scope léxico** y un `hint`
  persistido (`edges.hint`: `typed field BetaService`, `name Office`, `chain findByCode name
  Office`, `bare`, `this`…), y un **link pass** por rama, al final de cada corrida, resuelve las
  aristas del branch a `dst_id`/`confidence`/`resolution`/`external` o descarta las llamadas que nada
  tipa (DD-7). Motor puro en `devctx-parse` (`resolve/link.rs`, testeable sobre parses, sin store),
  reglas Java en `resolve/java.rs`, scopes en `resolve/scope.rs`; orquestación, selección
  incremental y escritura en `devctx-index/src/link.rs`. **`EXTRACTOR_VERSION` = 6** (los menores de
  la revisión subieron a 4 y 5, ver abajo).
- **Números (sandbox `mktemp -d -p /var/tmp`; `graph_bench`, el ejemplo nuevo que indexa con vectores
  constantes: parse, escritura y link pass reales, sin embedding; mismos snapshots y mismo binario de
  comparación; antes = 3ad7d54, después = esta task; arnés `scripts/graph-eval/run.sh` paso `graph`,
  casos de acme por `DEVCTX_EVAL_*`):**

  | | backend-a antes | backend-a después | backend-b antes | backend-b después | DevCtxEngine antes | DevCtxEngine después |
  |---|---|---|---|---|---|---|
  | `calls` guardadas | 120 501 | 110 616 | 12 332 | 9 328 | 21 721 | 16 979 |
  | con `dst_id` | 0 % | 20,9 % | 0 % | 46,0 % | 0 % | 24,3 % |
  | `external` | 0 % | 76,5 % | 0 % | 51,3 % | 0 % | 22,7 % |
  | **sin decidir** (sin `dst_id` ni `external`) | 100 % | **2,7 %** | 100 % | **2,7 %** | 100 % | 53,0 % (Rust: TASK-007) |
  | descartadas (fluent/no tipable, contadas en `IndexResult.edges_discarded`) | — | 9 885 | — | 3 004 | — | 4 742 |
  | basura `edges.dst_name` `var.*` / `LOG.*` / expresión | 892 / 1 761 / 0 | **0 / 0 / 0** | 502 / 78 / 0 | **0 / 0 / 0** | 0 / 0 / 0 | 0 / 0 / 0 |
  | basura `graph_edges` `var.*` / `LOG.*` | 481 / 1 272 | **0 / 0** | 114 / 68 | **0 / 0** | 0 / 0 | 0 / 0 |
  | `from_test` ≠ `is_test` (= `path_kind` Test) | 0 | 0 | 0 | 0 | 0 | 0 |
  | link pass completo | — | 2,8 s (carga 0,46 s, escritura 1,76 s) | — | 0,20 s | — | 0,27 s |
  | link pass incremental, 1 archivo | — | **0,62 s** | — | — | — | — |
  | `VmHWM` del indexado (`graph_bench`) | 314 MiB | 417 MiB (**+103 MiB**) | 151 MiB | 160 MiB | 145 MiB | 157 MiB |

  Gold edges (precisión de resolución sobre la tabla `edges`, `score.py gold --new-edges`; "antes" =
  la cota por nombre de `graph_edges` de TASK-001, porque antes ninguna arista tenía `dst_id`):

  | | sitios | antes (nombre en `graph_edges`) | después: correcto | en `high` | cobertura (no `low`) |
  |---|---|---|---|---|---|
  | **Java** (backend-a 16 + backend-b 4) | 20 | 10 (50 %) | **18 (90 %)** | **18/18 (100 %)** | 90 % |
  | Rust (DevCtxEngine) | 6 | 5 | 5 (83 %) | 4/4 | 83 % |

  Los dos Java que fallan son parámetros de lambda sin tipo (`s -> s.query(…)` sobre una sesión, se
  descarta; `h -> h.onDone(…)` sobre un `Stream` de handlers, `name_only`): no hay inferencia del
  tipo de un lambda. Las tres llamadas de `DraftResource` (:245/:304/:363) resuelven a su servicio,
  `high` (`ctor_inject`: el campo de cada adaptador se asigna en su constructor), y `new
  AlphaServiceAdapter` a la clase interna (`same_file`). Rust (`Instant::now`, `Store::open`) queda
  con las reglas genéricas hasta TASK-007.
- **Compuerta de ruido (master §5):** en Java, basura 0, externos marcados con evidencia, tests
  marcados al 100 % y gold ≥ meta: **cumplida para Java**. TS/Python/Rust/Go siguen con reglas
  genéricas (DevCtxEngine 53 % sin decidir): TASK-010 no arranca hasta TASK-006/007, como dice §5.
- **Diseño:**
  - *Fase local* (`parser.rs`): `Scopes` (`resolve/scope.rs`) junta las declaraciones de `types` con
    su rol (`@bind.field|param|local|assign`) en el scope más interno de `scopes` (clase, método,
    bloque, lambda, `for`…): cada clase interna con sus campos, locales visibles después de su
    declaración, `var` desde `new T`/`(T) e`/`T.of(…)` (este último como conjetura `static` que el
    link pass confirma), elemento de un for-each (`List<Foo>`/`Foo[]`), `this.x = x` en un
    constructor → `ctor_inject`. El receptor se clasifica (`Recv`): `bare`, `this`, `super`,
    `typed <via> <T>`, `name <r>`, `untyped`, `expr`, `chain <callee> <hint>` (hasta 4 llamadas
    atrás) y literales (`"x".equals` → `String`); una constante en MAYÚSCULAS nunca es tipo. El
    target de `graph_edges` sigue el formato 0.9.0 pero sale del mismo receptor (por eso sin
    `var.*`/`LOG.*`). Sin `scopes` (TS, Python, Rust, Go) hay un único scope, el del archivo, con
    "primera declaración gana": el comportamiento previo.
  - *Link pass* (`RepoIndex`, `resolve/link.rs`): por id, `qualified`, nombre, hijos por `parent_id`,
    imports por archivo, paquetes y tipos `paquete.Qualified`; supertipos resueltos al construir.
    Reglas DD-7 1-9 con las de Java: tipo anidado/top-level del archivo, import simple, mismo
    paquete, import comodín, `java.lang`; import de un paquete ajeno = externo; miembro por
    herencia (BFS); miembro ausente con un supertipo externo (Panache, `Object`, `Enum`) →
    `inherited` + `external`; llamada sin calificar → miembro, import estático (también comodín de
    una clase externa: JUnit/Mockito) o heredado externo; cadena → tipo de retorno declarado del
    método anterior (`return_type`) o, si el anterior es externo, externa (`medium` si el nombre
    existe en el repo); receptor sin tipo + nombre fuera del repo → descarte; `toString`/`equals`…
    sobre un receptor sin tipo → externo `medium`; getters de Lombok (`@Data`, `@Getter`…) → el
    campo, `medium`. `unique_name` siempre `medium`, `name_only` `low`.
  - *Incremental* (DD-6): (a) aristas de archivos escritos o borrados en la corrida, (b) las de otros
    archivos cuyo `dst_id` ya no existe o cuyo último segmento de `dst_name` es un símbolo de un
    archivo escrito, (c) las sin decidir; pase completo si lo escrito supera 1/5 de la rama. Solo se
    reescriben los archivos con alguna fila cambiada; `contains` ni se carga ni se borra
    (`replace_files_linked_edges` borra `kind <> 'contains'`). El pase se salta si la corrida no
    escribió filas del grafo; se corta entre archivos si llega una parada (las aristas que quedan
    las toma (c) la próxima vez).
- **Archivos tocados:** `crates/devctx-parse/src/{parser.rs, types.rs, lang.rs, registry.rs,
  resolve/mod.rs, resolve/scope.rs (nuevo), resolve/java.rs (nuevo), resolve/link.rs (nuevo)}`,
  `crates/devctx-parse/languages/java.json` (`types` con roles, `scopes` nueva),
  `crates/devctx-parse/tests/{link_java.rs (nuevo), fixtures/java/link/… (8 archivos, nuevos),
  extractor_golden.rs, golden/extractor.txt}`, `crates/devctx-store/src/{schema.rs, symbols.rs}`,
  `crates/devctx-index/src/{link.rs (nuevo), pipeline.rs, lib.rs}`,
  `crates/devctx-index/examples/graph_bench.rs` (nuevo), `crates/devctx-cli/src/main.rs`,
  `crates/devctx-mcp/src/state.rs`, `scripts/graph-eval/{run.sh, score.py, graph_metrics.sql,
  README.md}`, `CHANGELOG.md`, el design (DD-2/5/6/7/8) y el master (§5). Públicos nuevos:
  `devctx_parse::resolve::{scope::{Scopes, Binding, Via, TypeText}, java::{Java, declared_type,
  arity, is_constant_name}, link::{RepoIndex, LinkSymbol, LinkEdge, Resolved, Outcome, link_rows}}`,
  `LangResolver::init_type`, `GraphEdge.hint`, `Lang::scopes`; `StoredSymbolEdge.hint`,
  `Store::{branch_symbols, branch_linkable_edges, replace_files_linked_edges}`;
  `IndexResult.{edges_discarded, edges_unresolved}`. Clave JSON nueva: `scopes` (lista de node
  kinds); capturas nuevas en `types`: `@init`, `@iter`, `@bind.*`. Columna nueva: `edges.hint`
  (`ALTER TABLE … ADD COLUMN IF NOT EXISTS`, como `symbols.content_hash`).
- **Verificado por:** `link_java` (casos (a)-(i) + `the_link_pass_declines_contains` +
  `static_imports_object_methods_literals_and_lombok`; no compilan contra el padre: API nueva),
  `resolve::scope::tests`, `resolve::java::tests`; en `devctx-index`
  `the_link_pass_resolves_across_files_and_leaves_contains_alone`,
  `an_incremental_link_pass_follows_a_definition_that_moved` (modo (b) por `dst_id` colgado; las
  filas `contains` del archivo no reindexado quedan idénticas) y
  `an_added_homonym_reopens_a_resolved_edge_by_name` (modo (b) por `dst_name`: una arista resuelta
  y vigente se reabre porque un archivo escrito agrega un homónimo); los dos últimos fallan si se
  quita la regla correspondiente de la selección (verificado mutándola), y llevan relleno para no
  caer en el pase completo. `every_relation_of_a_file_is_an_edge` actualizado (todo lo que no es
  `contains` sale decidido). Gate: `cargo fmt --check`; `cargo clippy --workspace --all-targets
  --locked -D warnings` (y `--features gpu`); `cargo test --workspace --locked` con
  `TMPDIR=/var/tmp`: verde. Guardia de identificadores privados: vacía. `devctx plan-status`: sin
  avisos.
- **Contrato JSON:** `index_repo` agrega `edges_discarded` y `edges_unresolved` (aditivo); el CLI
  imprime una línea `graph: …`. Las tools de lectura no cambian (leen `graph_edges`, TASK-008).
- **Desviaciones:** (1) el motor (`RepoIndex` + reglas) vive en `devctx-parse/src/resolve/link.rs`,
  no en `devctx-index/src/link.rs` (que orquesta y escribe): así los tests (a)-(i) corren sobre
  parses sin store; no hizo falta tocar `symbols.rs` para lecturas por lotes más allá de dos
  consultas por rama. (2) `scopes` es una lista de node kinds, no una query `@scope`. (3) El link
  pass escribe **varios archivos por transacción** (64; cada archivo entero dentro de una sola
  transacción, nunca partido) y un `DELETE` por lote: con una transacción y un `DELETE` (scan de la
  tabla sin índice) por archivo, el pase completo de backend-a tardaba 14,1 s, 12,9 s en escrituras;
  así 2,8 s. Sin índices nuevos. (4) Valores de `resolution` nuevos: `return_type` (cadena tipada por
  el retorno declarado o externa por la llamada anterior); `inherited` también para externos
  heredados. (5) `ctor_inject` gana sobre `field` cuando el campo se asigna desde un parámetro del
  constructor (más informativo; ambos `high`). (6) Más allá de DD-7: el receptor declarado sin tipo
  (`var q = em.createQuery(…)`, parámetro de lambda) se trata como una cadena (descarte si el nombre
  no está en el repo); externas en cadena tras una llamada externa; métodos de `Object` y getters de
  Lombok (`medium`); imports estáticos comodín de una clase externa. (7) `dst_name` nunca se
  reescribe: el link pass resuelve desde `dst_name` + `hint`, que es lo que lo hace idempotente y
  re-resoluble sin la fuente. (8) PageRank/`in_degree` quedan para TASK-010. (9) El `graph_bench`
  usa vectores constantes: mide grafo y costo de parse+link, no búsqueda; los tiempos de pared no
  son comparables con TASK-001 (otra máquina/carga).
- **Riesgos abiertos / siguiente:** (1) un descarte no vuelve si después aparece la definición (hay
  que reindexar el archivo); (2) parámetros de lambda sin tipo (2 de 20 gold Java); (3) las reglas
  "externa tras llamada externa" y "comodín de un paquete ajeno" pueden marcar externo algo del repo
  devuelto por una API genérica (`Optional.get()`): por eso `medium` cuando el nombre existe en el
  repo; (4) TS/Python/Rust/Go con reglas genéricas hasta TASK-006/007; (5) no verificado en
  frontend/legacy-migration ni en macOS/Windows; el `VmHWM` medido es del `graph_bench` (sin modelo),
  la diferencia (+103 MiB en backend-a) es la del link pass; (6) los sandbox de medición viven en
  `/var/tmp/devctx-t005-*` (snapshots de los repos, índices `graph_bench`).
- **Revisión de c239de2 y d9ffc04 (REQUEST CHANGES), resuelta en commits encima de d9ffc04:**
  - *MAJOR 1* receptor con puntos: si el primer segmento es un binding, `member <campos> <hint>`
    tipado campo por campo; un nombre con puntos sin binding es paquete externo solo con evidencia
    (plataforma o import desde esa raíz). *MAJOR 2* tras una llamada externa: `name_only` sin
    `external` si el nombre existe en el repo o tiene forma de accessor, si no `chain_external`
    `medium`; nunca `high`. *MAJOR 3* parámetros de lambda (sin tipo) y variables de patrón
    (`instanceof`, `type_pattern`) son bindings. *MAJOR 4* el modo (b) también re-elige por los
    tokens del `hint` y por herencia (tipos debajo de un tipo escrito), y
    `an_incremental_link_pass_equals_a_full_one` compara fila a fila incremental contra
    `link_branch(full=true)` en 4 escenarios (`extends`, tipo de retorno, `@Data`, `package`):
    **0 filas distintas** (falla si se quita la regla de tokens). *MAJOR 5* `index_meta.link_pending`
    (test `a_link_pass_cut_short_is_done_by_the_next_run`). *MAJOR 6* `LINK_VERSION` en
    `index_meta.link_version` (test `a_new_link_version_relinks_the_branch`). *MAJOR 7* el
    descarte queda como fila `resolution = 'discarded'`, fuera de conteos y lectores, reabierta por
    nombre (test `a_discarded_call_is_kept_and_reopens_by_name`).
  - *MINOR:* confianza nunca escala en una cadena ni por un tipo `unique_name`/FQN duplicado;
    sobrecarga sin aridad compatible → supertipos o `medium` (la aridad solo en firmas Java); `,)`
    se pliega salvo en un grupo de un elemento (`Fn(A, B,)` = `Fn(A, B)`, `(u8,)` ≠ `(u8)`); la
    clase anónima es un scope (`anon <Tipo>`); `graph_written` solo con fuentes. Desviaciones: 64
    archivos por transacción documentado (lectura mitad vieja/mitad nueva), motor en `devctx-parse`
    (DD-6 y "Archivos" actualizados), `return_type` separado de `chain_external`, `ctor_inject`
    solo etiqueta (tipo declarado del campo). NITs: aserción vacía de `contains` en `link_java`
    quitada, clonado perezoso en el incremental, el accessor de Lombok apunta a un `field` desde una
    arista `calls` (DD-8). Privacidad: literales y nombres de dominio reemplazados por genéricos
    (`findByCode`, `"done"`, `onDone`), barrido de lo agregado sin otros hallazgos.
    **`EXTRACTOR_VERSION` = 7.**
  - *Re-medición* (mismo método; antes = 3ad7d54, d9ffc04 = esta task sin la revisión, después =
    con la revisión; gold ampliado en el sandbox con 5 sitios de backend-a elegidos para la
    revisión —dos `obj.campo.m()`, un `list.get(0).getX()` de Lombok, un `Optional.get().m()`
    encadenado y un parámetro de lambda; no hay en los repos medidos una lambda que sombree un
    campo, cubierta por `lambda_parameters_and_patterns_bind_their_names`—, etiquetados antes de
    medir):

    | | antes | d9ffc04 | después |
    |---|---|---|---|
    | gold Java (25 sitios) correcto / en `high` | 0 (sin `dst_id`) | 20/25 (80 %) · 20/20 | **20/25 (80 %) · 19/19 (100 %)** |
    | gold Rust (6) | 0 | 5/6 · 4/4 | 5/6 · 4/4 |
    | backend-a sin decidir / externas / con `dst_id` | 100 % / 0 / 0 | 2,7 % / 76,5 % / 20,9 % | **8,1 % / 70,6 % / 21,3 %** (+11 986 descartadas, fuera) |
    | backend-b sin decidir / externas / con `dst_id` | 100 % / 0 / 0 | 2,7 % / 51,3 % / 46,0 % | **4,2 % / 49,6 % / 46,3 %** (+3 062 descartadas) |
    | basura `var.*` / `LOG.*` | 892 / 1 761 | 0 / 0 | 0 / 0 |
    | link pass backend-a completo / incremental 1 archivo | — | 2,8 s / 0,62 s | 2,6 s / 0,47 s |
    | `VmHWM` backend-a (`graph_bench`) | 314 MiB | 417 MiB | 428 MiB (+114 MiB) |

    El 2,7 % de d9ffc04 estaba inflado por las externas en `high` tras una llamada externa
    (MAJOR 2: 14 108 llamadas ahora `chain_external` `medium` y el resto `name_only`) y por los
    receptores con puntos tomados por paquetes (MAJOR 1). En el gold, d9ffc04 acertaba
    `holder.amount.equals(…)` por la heurística de paquete equivocada; ahora acierta por la regla
    de `Object` (`medium`). Los 5 fallos Java: tres parámetros de lambda o una `var` desde un
    estático que devuelve `Optional` (descartados: "sin arista"), y dos `name_only` (un getter de
    Lombok tras `list.get(0)`, honestamente sin decidir; un handler de un `Stream`).
- **Menores de la revisión de TASK-004 (commits previos a esta task):** 3ad7d54 (m-a `where`/genéricos
  estables ante `rustfmt`, m-c fuente de `graph_edges` por el `qualified` del símbolo, `const x =
  function named()`, `export default foo;` → `EXTRACTOR_VERSION` 4) y c239de2 (comentarios fuera del
  disambiguator, `(u8,)` ≠ `(u8)`, guardia de fixtures sin llamadas, `export default (foo);` → 5).
