# TASK-006 — Resolver TypeScript/JavaScript (imports relativos y alias, DI, arrow-const)

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-005
- **Estado:** `done`

---

## Objetivo

Que en un repo Angular/Nx (frontend) las llamadas y usos se resuelvan por imports (relativos,
alias de `tsconfig`, barrels `index.ts`), por inyección de dependencias (propiedades de parámetro del
constructor, `inject(T)`) y desde funciones arrow exportadas, con paquetes npm marcados externos.
Implementa DD-7 (TS/JS).

## Contexto verificado

- `typescript.json`/`tsx.json`: `types` solo `public_field_definition`, `variable_declarator` y
  `required_parameter` con `type_identifier`; no captura `accessibility_modifier` de parámetros del
  constructor (propiedades-parámetro), ni `inject(…)`. `javascript.json` no tiene `types`.
- `function_kinds` TS: `function_declaration`, `method_definition` (sin `arrow_function`): las
  llamadas dentro de un `export const x = () =>` hoy no tienen fuente y se descartan
  (`crates/devctx-parse/src/parser.rs:125-127`). TASK-004 las hace símbolo y fuente.
- Caso real: en una librería de auth de frontend, una función flecha exportada (`export const
  tokenInterceptor: HttpInterceptorFn = (…) =>`) y dos `AuthService` homónimos en dos librerías del
  monorepo: el import decide cuál.
- frontend es un monorepo Nx con micro frontends y librerías con alias (`CLAUDE.md` del workspace).

## Archivos

- **Crear:** `crates/devctx-parse/src/resolve/typescript.rs` (compartido TS/TSX/JS),
  `crates/devctx-index/src/tsconfig.rs` (lectura best-effort de `tsconfig.base.json`/`tsconfig.json`
  de la raíz: `compilerOptions.baseUrl` y `paths`; JSON con comentarios), fixtures multi-archivo en
  `crates/devctx-parse/tests/fixtures/ts/`.
- **Modificar:** `crates/devctx-parse/languages/{typescript,tsx,javascript}.json` (propiedades-
  parámetro, `inject`, `new_expression`, `extends`/`implements`, imports con nombres/alias,
  `platform_prefixes`/globals), `crates/devctx-index/src/link.rs` (import → archivo/símbolo).

## Pasos

- [x] **Paso 1 — tests que fallan.** Fixture: `auth.service.ts` ×2 en carpetas distintas;
      `interceptor.ts` con `export const tokenInterceptor = (…) => { const s = inject(AuthService);
      s.token(); }` importando uno de los dos por ruta relativa; un componente con
      `constructor(private readonly auth: AuthService)` que llama `this.auth.login()`; un import
      por alias `@acme/auth` mapeado en `tsconfig.base.json`; un barrel `index.ts` que reexporta;
      `rxjs`/`@angular/core` como externos.
- [x] **Paso 2 — imports.** Relativos (`.ts`, `.tsx`, `.js`, `/index.*`), alias por `paths`
      (best-effort; si no hay `tsconfig`, solo relativos), reexports de barrels a un nivel; npm →
      externo.
- [x] **Paso 3 — DI.** Propiedades-parámetro del constructor y `inject(T)` → campo tipado
      (`ctor_inject`).
- [x] **Paso 4 — arrow-const y funciones de módulo** como fuente (TASK-004) y como destino
      importado (`import { tokenInterceptor } from …`).
- [x] **Paso 5 — JS** con lo que aplique sin tipos (imports, `this`, arrow-const); receptores
      sin tipo → reglas 7-8 (`medium`/`low`).
- [x] **Paso 6 — medir** sobre frontend (con OK del usuario, es el repo más caro) o sobre una
      sublibrería copiada al scratchpad: gold edges TS, % sin decidir, externos.

## Criterios de aceptación

- [x] Tests del paso 1 verdes; la llamada desde el interceptor resuelve al `AuthService` importado,
      no al otro (`high`, `import`/`ctor_inject`).
- [x] Gold edges TS: precisión ≥ 90 % en `high`, ≥ 75 % global.
- [x] `read_symbol("tokenInterceptor")` tiene definición (lo verifica TASK-008 sobre la tabla;
      acá, que el símbolo está en `symbols` con su firma).

## Riesgos

- `tsconfig` con `extends` encadenados y comentarios: best-effort, un nivel de `extends`; si falla,
  solo relativos y se dice en el resumen del `index` una vez.
- Barrels profundos (reexport de reexport): un nivel; el resto queda `medium`/`low`.

## Resultado

- **Estado final:** `done`.
- **Resumen:** las llamadas, usos de tipo e imports de TypeScript/TSX/JavaScript se resuelven por lo
  que el archivo importa: rutas relativas (`.ts`/`.tsx`/`.js`→`.ts`/`index.*`), alias `paths` y
  `baseUrl` del `tsconfig.base.json` de la raíz, barrels (`export { X } from`, `export * from`,
  anidados hasta 4 niveles) y paquetes npm como externos (con evidencia, DD-9). La DI de Angular
  tipa: propiedades-parámetro del constructor y `inject(T)` (en un campo o en un local de una
  función flecha exportada) → `ctor_inject`. TS/TSX/JS tienen **scopes** (clase, método, función,
  flecha, bloque, `for`, `catch`): parámetros (también desestructurados y de flecha), variables de
  `catch` y locales son bindings que sombrean campos; un nombre pelado nunca es un miembro (no hay
  `this` implícito); `this` fuera de una clase declarada (método de objeto literal, *class
  expression*, `function`) no se tipa. El tipo de retorno declarado (`(): Observable<T>`) tipa la
  siguiente llamada de una cadena. **`EXTRACTOR_VERSION` = 9, `LINK_VERSION` = 4.**
- **Números (sandbox `mktemp -d -p /var/tmp`, 249 MB con los dos binarios y un clon de este repo):**
  `graph_bench` (vectores constantes) sobre un snapshot de solo lectura de **tres sublibrerías de
  frontend** (lib-auth, la librería de datos de auth y una app: 93 `.ts`, 162 archivos, más
  el `tsconfig.base.json`), sin `.devctx` copiado (verificado con `find`), repo git propio; antes =
  331ca05, después = esta task; mismos snapshots. **No se indexó frontend entero** (requiere OK del
  usuario).

  | frontend (subconjunto) | antes | después |
  |---|---|---|
  | `calls` guardadas (sin descartadas) | 1 919 | 1 936 |
  | con `dst_id` | 18,3 % | 23,6 % |
  | `external` | 8,6 % | 65,6 % |
  | **sin decidir** | **73,1 %** | **10,8 %** |
  | descartadas | 292 | 275 |
  | link pass completo (3 corridas) | 25-34 ms | 30-38 ms |
  | link pass incremental, 1 archivo (el servicio más importado) | 12 ms | 17 ms |
  | `VmHWM` (`graph_bench`) | 99-106 MiB | 107 MiB |

  Gold edges TS (20 sitios: los 10 de TASK-001 re-anclados al commit medido + 10 nuevos que cubren
  `inject()` en campo y en local, función flecha exportada como fuente, alias de `tsconfig`, barrels
  de 1-3 niveles, homónimos entre librerías y externos de Angular/RxJS; etiquetados leyendo el
  código **antes** de medir; versión auditable con alias opacos en
  `scripts/graph-eval/gold-ts-anon.txt`):

  | | correcto | en `high` | cobertura (no `low`) |
  |---|---|---|---|
  | antes | 6/20 (30 %) | 3/3 | 30 % |
  | **después** | **20/20 (100 %)** | **20/20 (100 %)** | 100 % |

  Los dos homónimos (`AuthService` de cada librería) caen cada uno en su archivo (verificado por
  `dst_id` → `symbols.file`, que `score.py` no mira). Además, una muestra aleatoria de 40 aristas
  `high` del índice leída contra el código: 40/40 correctas (`self`, `import`, `ctor_inject`, y
  externos de Angular, RxJS, Jest y del DOM). Sin regresión en los demás lenguajes: `graph_bench`
  sobre un clon de este repo da los mismos `calls`, % sin decidir y `high` por extensión en Rust,
  Python, Java y Go antes y después.
- **Compuerta de ruido (master §5):** TS con basura 0 (los receptores siguen siendo nombres o
  hints), externos con evidencia (import de paquete, global de plataforma, retorno declarado
  externo), gold ≥ meta: **cumplida para TypeScript** (en el subconjunto medido).
- **Hallazgos de §2:** confirmado que antes de esta task las llamadas TS no tenían más regla que el
  nombre (73 % sin decidir); la estimación de memoria de DD-6 para frontend no se pudo verificar
  (subconjunto).
- **Archivos tocados:** `crates/devctx-parse/src/{facts.rs, parser.rs, registry.rs,
  resolve/mod.rs, resolve/scope.rs, resolve/link.rs, resolve/typescript.rs (nuevo)}`,
  `crates/devctx-parse/languages/{typescript,tsx,javascript}.json` (`types` con roles, `scopes`
  nueva en TS/JS, `builtins` de navegador/Node/Jest), `crates/devctx-parse/tests/{link_typescript.rs
  (nuevo), fixtures/ts/… (13 archivos, nuevos), extractor_golden.rs, golden/extractor.txt}`,
  `crates/devctx-index/src/{tsconfig.rs (nuevo), link.rs, pipeline.rs, lib.rs}`,
  `scripts/graph-eval/{gold-ts-anon.txt (nuevo), README.md}`, `CHANGELOG.md`, el design (DD-6,
  DD-7) y el master (tabla). Públicos nuevos: `devctx_parse::resolve::typescript::{TsConfig
  {base_url, paths}, TsConfig::{parse, extends_of, over, fingerprint, specifier}, TsImport
  {spec, name, local, wildcard, reexport}, TsImport::from_row, Specifier, init_type, return_type,
  probes, join, dir_of, SCRIPT_EXTENSIONS}`, `RepoIndex::{with_ts_config, importers_of}`,
  `LangResolver::implicit_this`, `Scopes::implicit_this`, `Binding.member`,
  `ImportFact.reexport`, `ImportFact::hint`. Capturas nuevas en `types`: `@bind.inject`. Clave de
  `index_meta` nueva: `link_tsconfig`. Las filas `imports` llevan `edges.hint` (`export`, `as X`).
- **Tests** (`TMPDIR=/var/tmp`): `link_typescript` (10: el interceptor al `AuthService` importado,
  la función flecha como símbolo/fuente/destino, DI por constructor + alias + barrel + retorno
  declarado, `inject()` en un campo con import renombrado, sombreado por parámetro de flecha/`catch`/
  cast, imports por nombre/namespace/default/paquete y globales, `this` en objeto literal y *class
  expression*, JS sin tipos, sin `tsconfig`, lectura del `tsconfig` con comentarios): **9 de 10
  fallaban** contra el código previo con un `with_ts_config` vacío (el de lectura del `tsconfig`
  pasaba: es la pieza nueva). `resolve::typescript::tests` (JSONC, retorno, filas de import,
  especificadores, rutas), `resolve::scope::tests` (uniones con `null`), `tsconfig::tests`
  (`extends`, archivo roto). `an_incremental_link_pass_equals_a_full_one` con **tres escenarios
  TS** más, cada uno detectado por una sola regla, verificado mutándola: un barrel que cambia su
  `export { Svc } from` (falla sin la regla de importadores), el alias de `tsconfig.base.json` que
  cambia sin tocar fuentes (falla sin la del fingerprint del `tsconfig`) y el barrel interno de un
  barrel (falla si la de importadores no atraviesa barrels). Gate: `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets --locked -- -D warnings` (y `--features gpu`); `cargo
  test --workspace --locked`: verde.
- **Contrato JSON:** sin cambios en las tools; `index` imprime una línea si el `tsconfig` no se
  puede leer.
- **Desviaciones:** (1) barrels hasta **4 niveles** de re-export, no uno: TS los sigue de forma
  determinista y el patrón Nx real es `index.ts` → `lib/index.ts` → `services/index.ts`; `high` solo
  si cada rama se pudo seguir, `medium` si un `export *` de un paquete o un módulo no encontrado
  podría tenerlo, o al pasar el tope. (2) `inject(T)` da `ctor_inject` también en un local (guards e
  interceptores funcionales), no solo en campos. (3) Los decoradores no se ignoran: `@Injectable()`
  queda como llamada al import, externa. (4) Un import `default` no sabe qué símbolo es el default
  (no se guarda): se toma el del nombre local, `medium`. (5) `this` fuera de una clase declarada se
  trata como receptor sin tipo (low o descarte), en vez de resolver métodos de objetos literales;
  una llamada pelada a un parámetro o local (`next(req)`) también. (6) Lección 2: el import desde un
  paquete (o un alias sin `tsconfig`) de un nombre que algún archivo TS/JS del repo define en su
  nivel superior no es externo; un alias de `paths` sin archivo es del repo (sin decidir), un
  especificador que falla bajo `baseUrl` es un paquete. (7) `implements` de una interfaz externa no
  vuelve externo un miembro ausente (las interfaces no implementan). (8) Incremental: modo (d)
  nuevo, todas las aristas de los archivos TS/JS que importan un archivo escrito, también a través
  de barrels; y el `tsconfig` cambiado relinkea la rama entera (fingerprint en `index_meta`).
  (9) Los receptores `member` (`this.a.b.m()`) en TS siguen por nombre.
- **Riesgos abiertos / siguiente:** (1) **no medido sobre frontend entero** (OK del usuario
  pendiente): costo del link pass y del modo (d) con 2 133 archivos, `VmHWM`; (2) solo el
  `tsconfig` de la raíz (un nivel de `extends` relativo): los `paths` de un `tsconfig.json` por app
  no se leen; (3) las *signals* de Angular (`this.sig()`, llamar un campo) quedan sin decidir;
  (4) CommonJS (`require`, `module.exports`), `export =`, imports dinámicos, componentes JSX y
  declaraciones ambientales `.d.ts` no se resuelven; (5) el gold TS está sesgado a lib-auth (los
  sitios elegidos en TASK-001) y no hay inyección por constructor real en frontend (cubierta por
  fixtures); (6) sandbox de medición en `/var/tmp/devctx-t006-*`.
- **Revisión de bf8abde/3dcd6ee (REQUEST CHANGES, sin BLOCKER), resuelta en commits encima de
  ed417c9:**
  - *M1* una llamada pelada a un nombre importado que no se puede seguir (de un paquete, de un
    nombre que también define un archivo del repo) queda **sin decidir**, nunca `unique_name`
    contra el homónimo (`import { map } from 'rxjs/operators'` con un `map` del repo).
  - *M2* un paquete es externo solo con evidencia: dependencia (`dependencies`,
    `devDependencies`, `peerDependencies`, `optionalDependencies`) del `package.json` raíz o del
    más cercano al archivo, o módulo builtin de Node (`fs`, `node:x`); el nombre de un paquete del
    workspace (`name` de un `package.json` del repo) es del repo. Lo demás (un alias de un
    `tsconfig.json` de app que no se lee, un módulo que ningún manifiesto declara) queda sin decidir
    también para imports namespace y default. `RepoIndex::with_script_env` (`ScriptEnv {tsconfig,
    manifests}`, `Manifest::parse`). Un símbolo no exportado no cuenta como "definido en el repo"
    (`LinkSymbol.exported`).
  - *M3* el modo (b) lee el nombre de una fila `imports` de TS/JS después de `#` (`rxjs#map` →
    `map`).
  - *M4* un `tsconfig`/`package.json` ilegible no relinkea: se usa el último entorno bueno
    (`index_meta.link_script_env`, con su fingerprint en `link_tsconfig`); si no hay uno guardado,
    `tsconfig.base.json` roto cae a `tsconfig.json`; BOM UTF-8 ignorado; `extends` en array
    (TypeScript 5) soportado; un `extends` de paquete o un base que no existe o no parsea se avisa
    una vez en el resumen.
  - *m1* el modo (d) agrega a los nombres "frescos" los de los símbolos de los importadores (una
    cadena `a.f().g()` en un tercer archivo). *m2* si escritos + importadores superan 1/5 de la
    rama, pase completo; las rutas de cada especificador se calculan una vez por pase. *m3*
    `export_of` memoiza por (archivo, nombre, profundidad). *m5* solo el `inject` importado de
    `@angular/core` inyecta. *NIT* `paths` documentado como ordenado por patrón; el tipo hallado
    por un import no seguro se etiqueta `import_weak` (resolución `import`, `medium`), no
    `unique_name`; una función de un bloque hermano (o declarada en un bloque que no rodea la
    llamada) no sombrea: las funciones son bindings con scope (`function f` elevada, `const f = () =>`
    desde su declaración) y una llamada pelada que ningún scope liga, o ligada a la función del
    módulo, lleva el hint nuevo **`free`**.
  - *m4 (privacidad)* rutas con número de línea y la lista de micro frontends saneadas en TASK-001,
    TASK-006, TASK-008 y el master (commit aparte).
  - **`EXTRACTOR_VERSION` = 10, `LINK_VERSION` = 5.** Commits: ec29b32 (privacidad), e0e1a8c (código: parse e index
    juntos, porque `TsConfig::extends_of` cambia de forma para los dos) y el de esta documentación.
    Gate sobre e0e1a8c: `fmt --check`, `clippy -D warnings` (y `--features gpu`), `cargo test
    --workspace --locked`: verde.
  - *Tests que fallaban antes del fix:* en `link_typescript` (14 tests), contra el código de
    ed417c9 con `with_script_env` como envoltorio vacío: `a_package_import_is_never_a_repository_homonym`
    (M1: daba `medium/unique_name` → `util.formatName`), `a_package_needs_a_manifest_or_a_node_builtin`
    (M2: `fromAuth.selectUser()` daba externo `high`), `only_angulars_inject_injects` (m5),
    `a_block_local_function_does_not_shadow_outside_its_block` (NIT: la segunda llamada iba a
    `outer.cb`) y `the_interceptor_reaches_the_imported_auth_service` (el `inject` de Angular caía a
    `unique_name` contra el helper local: M1). En `devctx-index`, verificados mutando cada regla
    (el fix ya estaba escrito): el escenario `rxjs#map` de `an_incremental_link_pass_equals_a_full_one`
    falla si el modo (b) no parte por `#` (M3); el de `a.f().g()` falla sin los nombres de los
    importadores (m1); `an_unreadable_tsconfig_keeps_the_last_good_one` y
    `tsconfig::tests::an_unreadable_file_keeps_the_last_good_environment` fallan si se ignora el
    entorno guardado (M4: relinkeaba 3 aristas y perdía el alias).
  - *Re-medición* (mismo snapshot más el `package.json` raíz de frontend, que el snapshot original
    no tenía; antes = 331ca05, ed417c9 = TASK-006 sin la revisión, ahora = con la revisión):

    | | antes | ed417c9 | ahora | ahora, sin el `package.json` raíz |
    |---|---|---|---|---|
    | sin decidir | 73,1 % | 10,8 % | **10,8 %** | 34,1 % |
    | con `dst_id` | 18,3 % | 23,6 % | 23,6 % | 24,2 % |
    | externas | 8,6 % | 65,6 % | **65,6 %** | 41,6 % |
    | · `external_known` `high` | 157 | 1 085 | 1 085 | 629 |
    | · `chain_external` `medium` | 8 | 185 | 185 | 164 |
    | gold TS correcto / en `high` | 6/20 · 3/3 | 20/20 · 20/20 | **20/20 · 20/20** | — |
    | link pass completo | 25 ms | 27 ms | 35 ms | — |

    Con el `package.json` raíz las cifras no cambian: en este subconjunto todo lo que se marcaba
    externo tenía evidencia. Sin él (solo el `package.json` de una librería) un tercio de las
    llamadas vuelve a "sin decidir" en vez de quedar externo sin evidencia: es lo que M2 pide.
    Desglose de las 1 085 `external_known` por receptor: 558 sin receptor (`free`: `inject`,
    `signal`, `firstValueFrom`, `expect`, `it`, `atob`…), 353 por nombre (`console`,
    `localStorage`, `TestBed`, `Validators`, `Date`…), 159 tipadas (`Router`, `FormGroup`,
    `HttpClient`, `BehaviorSubject`…), 15 por retorno declarado. *Auditoría contra el código:* 20
    `chain_external` al azar (cadenas de `expect(…).toBe`, `jest.fn().mockReturnValue`,
    `http.post(…).pipe`, `pipe(…).subscribe`, `firstValueFrom(…).then`): 20/20 correctas; 30
    `external_known` al azar (10 por nombre, 10 `free`, 10 tipadas): 30/30. De imports namespace o
    default externos el subconjunto tiene **uno solo** (`Swal.close()` de `sweetalert2`, que está en
    `dependencies`): correcto; no hay 20 que auditar en estas librerías (riesgo abierto: frontend
    entero).
- **Segunda revisión (APROBADA con caveats), resuelta en commits encima de beafcc4:**
  - *mi1* (2234c91, su propio commit): `choose` cae a lo guardado **por componente**: lo que se
    pudo leer va fresco; de lo guardado solo el `tsconfig` si el suyo es ilegible, o el manifest del
    mismo directorio; un manifest ilegible que lo guardado no tenía se ignora. Antes un
    `package.json` que no es JSON (la plantilla de un generador Nx) congelaba el entorno viejo para
    siempre y un alias movido seguía dando `high` al archivo de antes. El entorno que se guarda es
    siempre el compuesto (solo partes legibles).
  - *mi2* un import de un paquete que declara un manifest (o builtin de Node) es externo aunque el
    repo defina el mismo nombre (`export function filter()` ya no deja sin decidir los `filter` de
    `rxjs`); `defined_in_repo` se quitó. Con esto el escenario `rxjs#map` de la equivalencia (M3)
    dejó de cambiar la arista vigilada: pasó a un test propio
    (`a_repository_homonym_does_not_unmake_a_package_import`) y la clave por `#` del modo (b) queda
    como defensa, sin escenario que solo ella detecte (toda respuesta de una fila `imports`
    depende ya solo del módulo que nombra, que cubren el modo (d) y el fingerprint del entorno).
  - *mi3* los `package.json` bajo `node_modules`, `dist`, `vendor`, `third_party`,
    `bower_components` (a cualquier profundidad) y lo que excluye el indexado no son paquetes del
    workspace.
  - *mi4* "el más cercano" es el manifest más profundo que contiene al archivo: con `a/` y `a/b/`,
    un archivo de `a/b/` mira `a/b` y la raíz, nunca `a/` (documentado en DD-7).
  - *NIT* los nombres de importadores que reabren por (d) excluyen `constructor`, los hooks de
    Angular (`ngOnInit`…) y `subscribe`/`pipe`/`then`/`toString`/`valueOf` (no tipan nada que una
    cadena use y reabrirían casi toda la rama); un receptor bindeado a una función o clase es su
    nombre (`fn.call()` sin decidir en vez de descartado; `Klass.make()` de `const Klass = class
    {…}` resuelve a `Klass.make`, `same_file`, por ser el único invocable `recv.callee` del
    archivo); dos funciones homónimas de bloques hermanos (`outer.cb` ×2) bajan a `medium`.
  - **`EXTRACTOR_VERSION` = 11, `LINK_VERSION` = 6.** Commits: 2234c91 (mi1), 7b43503 (mi2, mi3 y
    NITs) y el de esta documentación. Gate sobre cada uno: `fmt --check`, `clippy -D warnings` (y
    `--features gpu`), `cargo test --workspace --locked`: verde.
  - *Tests que fallaban antes del fix:* `tsconfig::tests::a_broken_manifest_added_later_freezes_nothing_else`
    (el alias movido no se veía), `a_broken_tsconfig_keeps_only_the_stored_tsconfig` (la dependencia
    nueva no se veía) y `a_broken_manifest_does_not_freeze_the_aliases` (integración: `high` al
    archivo viejo) para mi1; en `link_typescript`, `a_package_import_is_never_a_repository_homonym`
    (`filter`/`formatName` de un paquete declarado daban `low`), `imports_by_name_namespace_default_and_package`
    (`fromPackage`), `a_block_local_function_does_not_shadow_outside_its_block` (`high` con dos
    candidatos) y `bound_functions_and_classes_as_receivers` (`Klass.make`/`call` descartados);
    `manifests_of_build_output_and_vendored_code_are_left_out` (mi3, los cinco manifests entraban).
    `a_repository_homonym_does_not_unmake_a_package_import` se escribió después del fix.
  - *Re-medición* (mismo snapshot con el `package.json` raíz): sin cambios — 10,8 % sin decidir,
    65,6 % externas (1 085 `external_known` `high`, 185 `chain_external` `medium`), gold 20/20 con
    20/20 en `high`. Link pass completo 49-110 ms con la máquina a carga 37 (el binario anterior,
    en la misma carga, 89 ms): no comparable con las cifras de arriba.

