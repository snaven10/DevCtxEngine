# TASK-017 — Dispatch por interfaz en `impact_analysis` (y después en `traverse`)

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-009
- **Estado:** `done`

---

## Objetivo

Que `impact_analysis` no pierda a quien llama a una implementación a través de su interfaz, su clase
abstracta o su trait: hoy `impact("ServiceImpl.update")` no ve las llamadas a `IService.update`, y
en Java con inyección de dependencias ese es el falso negativo más caro (el llamador tiene el campo
tipado por la interfaz). Cada nodo del recorrido (no solo la semilla) se expande a los métodos
override-equivalentes de supertipos y subtipos, con confianza `min(medium, la de la arista
original)` y `via: "dispatch"`, nunca `high`. `traverse` (TASK-013) usa la misma
expansión. Agregada a PLAN-009 por decisión del usuario (2026-10-08, §12), a propuesta de la revisión
de TASK-009.

## Contexto verificado

- `Store::impact_graph` (`crates/devctx-store/src/impact.rs`, TASK-009) recorre `live_edges` por
  niveles y sigue `calls` e `instantiates` (desviación 3 de TASK-009): no sigue `implements` ni
  `inherits`.
- Las aristas `implements`/`inherits` ya existen en `edges` desde TASK-004 y se resuelven en el link
  pass de TASK-005/006/007 (Java `implements`/`extends`, TS `implements`/`extends`, Rust
  `impl Trait for T`, Python bases, Go embebido).
- La resolución de un método por supertipo ya existe en el link pass (`inherited`), y en Rust el
  trait de un `impl Trait for T` con T externo queda `medium` (TASK-008 N2, TASK-009 MINOR).

## Archivos

- **Modificar:** `crates/devctx-store/src/impact.rs` (expansión de la semilla y de cada nodo por
  dispatch, con tope), `crates/devctx-mcp/src/state/impact.rs` (salida `via: "dispatch"`, conteo),
  docs EN + ES de `impact_analysis`, `PLAN-009-design.md` (DD-11).
- **Después, en TASK-013:** la misma expansión en `traverse`.

## Pasos

- [x] **Paso 1 — tests que fallan.** (a) `impact("ServiceImpl.update")` ve a quien llama por
      `IService.update` (Java, campo tipado por la interfaz e inyectado); (b-up) upstream:
      `impl.m` → override-equivalentes de sus supertipos → sus llamadores; (b-down) downstream:
      `impact("IService.update")` → sus implementaciones; (b-deep) dispatch en profundidad ≥ 2
      (un llamador de nivel 1 que a su vez se llama por interfaz); (b-chain) cadena transitiva
      abstracta → interfaz; (c) una interfaz con muchas implementaciones no explota: tope, con lo
      recortado en `omitted_by_limit.dispatch`; (c') una arista `low` sigue `low` tras dispatch;
      (d) TS `implements` y Rust trait → impls; (e) escenario en
      `an_incremental_link_pass_equals_a_full_one` si cambia un `implements` (la expansión se lee en
      la consulta, pero el escenario fija que las aristas de herencia del incremental igualan al
      completo).
- [x] **Paso 2 — override-equivalencia, por nodo.** Mismo nombre y aridad o firma compatible, en
      supertipos y subtipos vía `implements`/`inherits` resueltos (`dst_id`). La expansión se hace
      **por cada nodo de cada nivel, no solo en la semilla** (si no, se pierde el dispatch en
      profundidad ≥ 2), con **a lo sumo UNA consulta extra por nivel** (por lote de la frontera, no
      por nodo). Semántica por dirección:
      - **upstream:** `impl.m` → métodos override-equivalentes de sus supertipos → sus llamadores;
      - **downstream:** método de interfaz o abstracto → sus implementaciones.
      La cadena de supertipos es **transitiva** (abstracta → interfaz) con tope de profundidad.
      La confianza de una arista por dispatch es **`min(medium, confianza de la arista original)`**:
      tope `medium`, nunca `high`, y un `low` no sube. `via: "dispatch"`; un `min_confidence: high`
      las excluye y las cuenta.
- [x] **Contrato:** lo recortado por el tope de dispatch se cuenta en **un solo campo,
      `omitted_by_limit.dispatch`** (aditivo).
- [x] **Paso 3 — lenguajes.** Java (interfaces y clases abstractas con DI), TS (`implements`), Rust
      (trait → impls). Python y Go como best-effort documentado.
- [x] **Paso 4 — default.** Incluido por defecto en `impact`, con conteo cuando se recorta. Contrato
      solo aditivo: la salida de TASK-009 sigue siendo un subconjunto.
- [x] **Paso 5 — medir.** p95 de `impact` ≤ 300 ms en backend-a y backend-b con la expansión
      activa; gold con un sitio real de DI en backend-b (con alias en el repo).

## Criterios de aceptación

- [x] Tests (a)-(e) verdes, cada uno fallando antes del fix.
- [x] Ninguna arista de dispatch sale `high` ni sube la confianza de la arista original (`min(medium, original)`).
- [x] Gold de DI en backend-b correcto; p95 de `impact` ≤ 300 ms en backend-a.
- [x] Docs EN + ES y descripción del tool explican `via: "dispatch"` y cómo excluirlo.

## Riesgos

- Interfaces con decenas de implementaciones (repositorios genéricos, `Runnable`): el tope por
  semilla evita la explosión; lo recortado se cuenta.
- Sobrecargas con la misma aridad y tipos distintos: sin firma resuelta, quedan como candidatos
  `medium`, nunca `high`.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. Tests (a)-(e) verdes; p95 de `impact` ≤ 300 ms en backend-a y backend-b
  con la expansión activa; gold de DI en backend-b (y en backend-a) correcto. `traverse` sigue
  pendiente en TASK-013, que reusa `DispatchIndex`.
- **Resumen:** `impact_analysis` sigue el dispatch por interfaz, clase abstracta o trait, por
  defecto, en **cada nodo de cada nivel**: upstream, `Impl.m` alcanza a los llamadores de los
  métodos que sobrescribe en sus supertipos (a la profundidad de un llamador directo); downstream,
  un método de interfaz o abstracto alcanza a sus implementaciones (un nivel debajo de él) y a lo
  que estas llaman. Por las aristas `inherits`/`implements` resueltas de `live_edges`, transitivo
  hasta `DISPATCH_DEPTH` (4) niveles de supertipos. Override-equivalente = mismo nombre, kind
  invocable (`method`/`function`) y aridad compatible (rangos con defaults, opcionales y variádicos
  leídos de la firma; una firma ilegible acepta); un método `private` o `static` no sobrescribe.
  Confianza `min(medium, arista original)`, contando también la arista de herencia más débil de la
  cadena; downstream, tampoco más que la arista que alcanzó al método de origen: nunca `high`, un
  `low` sigue `low`. El nodo lleva `via: "dispatch"` y `through` (el método por el que pasó); a
  igual confianza gana la arista directa. Tope de `DISPATCH_MAX_PER_NODE` (16) equivalentes por
  nodo, por rank y nombre; lo recortado va a `omitted_by_limit.dispatch`. `dispatch: false` lo
  apaga (tool, `GET /impact?dispatch=false`, `devctx impact --no-dispatch`); `min_confidence:
  high` lo deja afuera y lo cuenta en `below_confidence`.
- **Primer commit (necesario para TS):** en TypeScript el método que declara una interfaz no era
  símbolo (Java, Rust y Go sí), así que `this.svc.update()` con `svc: IService` quedaba
  `name_only` `low` sin destino y el dispatch no tenía de dónde partir. `typescript.json`/`tsx.json`
  agregan `(interface_body (method_signature …))` y `abstract_method_signature` como `method`; la
  llamada se resuelve a `IService.update` (`ctor_inject`/`field`, `high`). Las firmas de sobrecarga
  de una clase y las propiedades de una interfaz siguen sin ser símbolo. **`EXTRACTOR_VERSION` 20**
  (el golden solo cambia de versión: sus fixtures no tienen métodos de interfaz).
- **Diseño:** `crates/devctx-store/src/dispatch.rs` (nuevo): `Store::dispatch_index` lee **una vez
  por llamada** (dos sentencias constantes, como la semilla) las aristas de supertipo resueltas y los
  métodos de los tipos de sus dos extremos (`DispatchIndex`); `DispatchIndex::equivalents(frontier,
  dir)` expande la frontera en memoria (BFS por la cadena con la confianza más segura por tipo,
  aridad, `private`/`static`, tope por nodo). En `impact.rs`, `dispatch_level`: upstream, la
  consulta de nivel lee los llamadores de la frontera **y** de sus equivalentes en la misma sentencia
  (el conjunto respeta `FrontierSql::Auto`) y las filas de un equivalente pasan a ser filas
  `dispatch` del nodo de origen; downstream, la consulta de nivel de siempre más las
  implementaciones como filas `dispatch` (`from_reach`: no más seguras que lo que alcanzó al
  origen). Un nivel cuesta **las mismas sentencias que sin dispatch**; una rama sin aristas de
  supertipo paga una sentencia por llamada. `walk` recibe `Level {rows, cut}` y cuenta los
  recortados que no se mostraron por otro camino. Go queda afuera (`.go`): embeber promueve, no
  sobrescribe, y las interfaces son implícitas.
- **Números** (sandbox `mktemp -d -p /var/tmp`; copias de backend-a y backend-b con `tar` sin
  `.devctx`/`target`/`node_modules`, `find` vacío antes del primer índice; índices con el embedder
  sin modelo del arnés `impact_bench`, binario `--release` del árbol de cada commit; 40 corridas
  por caso, dos rondas intercaladas b/a; `DEVCTX_IMPACT_BENCH_TOOL_ONLY`; cada llamada abre el
  store como el serve). p50 / p95 en ms, **peor de las dos rondas**, la tool por defecto con
  dispatch y la misma llamada con `dispatch: false` (el camino de TASK-009), en la misma corrida:

  | Caso | con dispatch | sin dispatch | nodos (de ellos por dispatch) |
  |---|---|---|---|
  | backend-a `get` | 35,9 / 40,7 | 24,5 / 28,7 | 0 |
  | backend-a `map` | 77,3 / 85,9 | 64,9 / 72,0 | 18 |
  | backend-a método de actualización pelado | 79,4 / 87,7 | 66,2 / 72,6 | 202 |
  | backend-a el de un servicio, calificado | 69,9 / 81,7 | 57,1 / 65,5 | 17 |
  | backend-a `emit` | 92,4 / 105,5 | 79,6 / 88,5 | 234 (+22 por tope) |
  | backend-a `toJson` | 81,7 / 91,9 | 68,1 / 81,1 | 200 (+27) |
  | backend-a método de una interfaz con 7 implementaciones | 75,7 / 84,7 | 37,8 / 43,7 | 62 (5) |
  | backend-a una de esas implementaciones | 84,7 / 94,7 | 53,6 / 61,0 | 12 (1) |
  | backend-b `get` | 23,1 / 25,7 (ronda 2) | 19,8 / 23,0 | 5 |
  | backend-b `map` | 33,2 / 37,2 (ronda 2) | 28,2 / 32,8 | 138 |
  | backend-b método de actualización pelado | 27,4 / 35,7 | 23,2 / 27,7 | 205 |
  | backend-b el de un servicio, calificado | 28,5 / 34,8 | 24,8 / 32,5 | 20 |
  | backend-b helper de conversión | 28,3 / 33,7 | 26,2 / 31,3 | 33 |
  | backend-b constructor de un DTO | 26,3 / 31,7 | 24,7 / 30,4 | 9 |
  | backend-b `toJson` | 28,0 / 32,3 | 25,8 / 30,7 | 65 (1) |
  | backend-b servicio llamado por un adaptador (gold, b-deep) | 34,7 / 40,3 | 29,1 / 32,9 | 79 (1) |
  | backend-b un adaptador (gold) | 30,0 / 36,3 | 22,1 / 26,6 | 40 (1) |
  | backend-b la interfaz de los adaptadores (gold) | 29,0 / 33,5 | 17,9 / 21,4 | 29 (3) |

  La ronda 1 de backend-b coincidió con carga 12 (`get` 77 / 134, `map` 125 / 193 con dispatch;
  118 y 148 de p95 sin él): ruido de la máquina, no del dispatch; el peor p95 de todo, 193 ms,
  sigue bajo 300. Carga 4,6-12,2 en las corridas. **Criterio: cumplido** (p95 ≤ 300 ms en `get`/`map`
  de los dos repos con la expansión activa). Contra la línea de TASK-009 en backend-a (get 24,5 /
  map 57,1 de p95, otra sesión y otra carga): get 40,7, map 85,9; sin dispatch en esta misma
  corrida, 28,7 y 72,0. El costo del dispatch es ~10-15 ms constantes en backend-a (leer el grafo
  de supertipos: 20 aristas resueltas, ~40 tipos) más la consulta de nivel con más ids donde hay
  equivalentes (+30-40 ms en los casos de la interfaz), y 2-12 ms en backend-b. **La primera
  versión** (un CTE recursivo por nivel sobre `live_edges` y `symbols`, una sentencia más por
  nivel) duplicaba el p95 en backend-a aunque no hubiera nada que despachar (`map` 146 contra 86,
  `get` 52 contra 33): la reemplazó el commit `perf` (`DispatchIndex`). No medido: costo de
  indexado, tamaño y RAM (el dispatch se lee en la consulta; el commit de TS agrega un símbolo por
  método de interfaz y fuerza `--full` por la versión del extractor, que reusa los vectores).
- **Gold de DI** (privado en el sandbox, con nombres reales; anónimo en
  `scripts/graph-eval/gold-java-anon.txt`, alias nuevos `ClassA.Inner2..4`, `m20..m22`, `ClassY`,
  `ClassZ`, `ClassAA`): backend-b, un recurso que elige un adaptador por tipo en tiempo de ejecución
  y lo llama por su interfaz interna (receptor tipado por el retorno de un método): (1) desde un
  adaptador, upstream, el método del recurso por dispatch (prof. 1); (2) desde el servicio que
  llama el adaptador, el mismo método del recurso en **profundidad 2** (b-deep real: TASK-009 se
  cortaba en el adaptador); (3) desde la interfaz, downstream, las tres implementaciones. backend-a,
  un listener que llama a un handler por su interfaz: (4) desde un handler, upstream, el método del
  listener; (5) desde la interfaz, sus siete implementaciones (cinco listadas, dos en
  `omitted_for_budget` por el presupuesto de salida). **5/5 casos, 13/13 nodos, todos `medium`
  `dispatch`, ninguno `high`**; sin dispatch, ninguno de los 13 aparecía.
- **Hallazgos de §2:** sin cambios a H1-H9. El falso negativo de DI que motivó la task existe en
  los dos repos Java medidos (gold arriba); son pocos sitios (backend-a: 20 aristas de supertipo
  resueltas, la interfaz más implementada tiene 7; backend-b: 3), así que el tope de 16 no recortó
  nada en campo.
- **Archivos tocados:** `crates/devctx-parse/languages/{typescript,tsx}.json`,
  `crates/devctx-parse/src/registry.rs` (`EXTRACTOR_VERSION` 20),
  `crates/devctx-parse/tests/{facts_typescript.rs, link_typescript.rs, golden/extractor.txt,
  fixtures/ts-dispatch/{svc,api}.ts}`, `crates/devctx-store/src/{dispatch.rs (nuevo), impact.rs,
  lib.rs}`, `crates/devctx-mcp/src/{state/impact.rs, lib.rs, backend.rs}`,
  `crates/devctx-api/src/lib.rs`, `crates/devctx-cli/src/main.rs`, `crates/devctx-index/src/lib.rs`
  (escenario (e)), `CHANGELOG.md`, docs EN/ES (`symbol-graph`/`grafo-de-simbolos`,
  `mcp-integration`/`integracion-mcp`), `scripts/graph-eval/{gold-java-anon.txt, README.md}`, el
  design (DD-11) y el master. Públicos nuevos: `devctx_store::{DISPATCH_DEPTH,
  DISPATCH_MAX_PER_NODE, DISPATCH_VIA}`, `ImpactOptions::dispatch` (default `true`),
  `ImpactNode::through`, `ImpactSide::dispatch_capped`, `devctx_mcp::state::ImpactQuery::dispatch`;
  internos `DispatchIndex`, `Equivalent(s)`, `Level`, `Store::{dispatch_index, dispatch_level,
  frontier_set}`. Sin cambios de schema ni de config; `languages/*.json` cambia (TS).
- **Tests** (`TMPDIR=<sandbox> cargo test --workspace --locked`, gate completo antes de cada commit
  de código: `fmt --check`, `clippy -D warnings` con y sin `--features gpu`; 1 079 pasan tras el
  primer commit y 1 099 tras el segundo y el tercero, 0 fallan; un flake de timing, `remote::tests::the_autospawn_mark_is_read_from_the_environment`, que
  lee `/proc/<pid>/environ` justo después de lanzar el proceso, falló una vez con la máquina cargada
  y pasó en tres reintentos y en la corrida completa siguiente):
  - (a) `impact_sees_a_caller_through_an_injected_interface` (tool, Java con `@Inject` del campo
    tipado por la interfaz; también `dispatch: false` como subconjunto, downstream desde la
    interfaz y `min_confidence: high`).
  - (b-up) `upstream_dispatch_reaches_the_callers_of_the_overridden_method`; (b-down)
    `downstream_dispatch_reaches_the_implementations`; (b-deep)
    `dispatch_expands_every_level_not_only_the_seed` (upstream y downstream en profundidad 2);
    (b-chain) `dispatch_climbs_a_chain_of_supertypes_up_to_its_cap` (abstracta → interfaz →
    super-interfaces, la quinta fuera del tope).
  - (c) `many_implementations_are_capped_and_counted` (store, 40 implementaciones) y
    `the_dispatch_cap_is_counted_in_omitted_by_limit` (tool, 20 subclases de Python).
  - (c') `a_low_edge_stays_low_through_dispatch_and_high_excludes_it` (también: ningún nodo
    `dispatch` es `high` con ningún filtro) y `a_downstream_dispatch_is_no_surer_than_what_reached_its_method`.
  - (d) `dispatch_follows_typescript_rust_and_python_but_not_go_embedding` (tool: TS `implements`,
    trait de Rust, clase base de Python, en los dos sentidos; embeber en Go no despacha), más
    `an_interface_method_is_a_symbol` (facts TS) y `a_call_through_an_interface_reaches_its_declared_method`
    (link TS).
  - (e) `an_incremental_link_pass_equals_a_full_one` suma el paso "una clase cambia su
    `implements`": la llamada de otro archivo a un método `default` heredado cambia de interfaz y el
    incremental iguala al completo.
  - Otros: `only_override_equivalent_methods_are_dispatched_to` (aridad, `private`),
    `a_discarded_supertype_edge_dispatches_nothing` (solo `live_edges`; con todas descartadas, ni una
    sentencia de dispatch), `a_direct_edge_beats_a_dispatch_of_the_same_confidence`,
    `dispatch_adds_no_statement_per_level` (300 llamadores por interfaz: sentencias = niveles, una
    lectura del grafo por llamada), `dispatch_is_a_filter_of_the_query` y los unitarios de
    `dispatch.rs` (aridad por lenguaje, rangos, confianza, `private`/`static`).
  - **Contrato (lección 2):** ningún assert existente cambió. `the_0_9_0_impact_asserts_hold_with_dispatch`
    corre los `check_*` de 0.9.0 sobre un grafo donde el dispatch **alcanza algo** (una subclase que
    sobrescribe `persist`), con los defaults y con todo abierto; (a) fija que la respuesta sin
    dispatch es un subconjunto de la con dispatch. Solo se tocaron, sin asserts: el helper `all_in()`
    de los tests de la tool (`..Default::default()`) y el literal de `ImpactOptions` del arnés.
  - **Fallaban antes del fix:** `an_interface_method_is_a_symbol` (antes del cambio en el JSON) y
    los ocho primeros del store (b-up, b-down, b-deep, b-chain, c, c', aridad y el de sentencias),
    corridos con la API agregada y sin la expansión. El resto, **por mutación** (revertir la regla,
    `touch`, correr): profundidad 1 (falla b-chain); sin `min(medium, …)` (fallan b-up, c' y el de
    arista directa); expansión solo de la semilla (fallan b-deep, b-down, el de `from_reach` y el de
    `check_*`); sin tope por nodo (falla c); sin aridad (falla el de override); sin `from_reach`
    (falla el suyo); `edges` en vez de `live_edges` (falla el de descartadas); sin preferencia por la
    directa (falla el suyo); `private` permitido (falla el de override); grafo leído por nivel
    (falla el de sentencias); Go incluido (falla d); dispatch apagado (fallan a, d y el tope de la
    tool); sin `through`, sin el conteo en `omitted_by_limit`, sin `filters.dispatch`, `dispatch`
    ignorado (falla cada test que lo mira); el link TS sin el JSON (falla el de link). (e) **no
    falló antes**: no hay fix, la expansión se lee en la consulta (lo anticipaba la spec); el paso
    verifica que la arista vigilada cambia.
- **Contrato JSON** (aditivo): nodo con `via: "dispatch"` y `through`; `filters.dispatch`;
  `omitted_by_limit.dispatch: {count, max_per_node}` (sumado a `omitted_by_limit.count` y a
  `omitted.count`; el `hint` de arriba es el del dispatch si solo cortó el dispatch). Parámetro nuevo
  `dispatch` (tool, `GET /impact`, `--no-dispatch`); la nota de filtros no aplicados lo nombra.
- **Desviaciones:**
  1. **Extracción de TS** (primer commit, `EXTRACTOR_VERSION` 20): la spec no lo listaba, pero sin
     él el dispatch de TS no tenía de dónde partir. Pide `index --full` (reusa vectores).
  2. **Parámetro `dispatch` y campo `through`**, no pedidos: `min_confidence: high` como única forma
     de excluirlo deja afuera también toda llamada `medium`; `through` dice por qué aparece un nodo.
  3. **Cero sentencias por nivel** en vez de "a lo sumo una": la versión de una por nivel duplicaba
     el p95; el grafo de supertipos se lee una vez por llamada (dos sentencias constantes).
  4. Downstream la implementación aparece **un nivel debajo** del método de interfaz (es la
     expansión de ese nodo); upstream, el llamador por la interfaz a la profundidad de un llamador
     directo (la interfaz no se lista: lo dice `through`).
  5. La confianza también se acota por la arista de herencia más débil de la cadena.
  6. Go excluido explícitamente; Python funciona (test (d)) como mejor esfuerzo.
  7. `omitted_by_limit.dispatch` cuenta métodos equivalentes no seguidos (upstream son métodos de
     supertipos, no nodos); se suma a `omitted.count`.
- **Riesgos abiertos / siguiente:** (1) TASK-013 (`traverse`) reusa `DispatchIndex`. (2) En un repo
  con miles de tipos con supertipos, `dispatch_index` lee todos sus métodos por llamada (en los
  medidos, ~10-15 ms): si crece, cachearlo por rama y versión del link pass. (3) Sobrecargas con la
  misma aridad y tipos distintos quedan como candidatos `medium` (sin tipos resueltos de
  parámetros). (4) No verificado: macOS/Windows (solo CI), frontend (TS) y legacy-migration
  (Python) no reindexados con el extractor 20.
