# TASK-005 — Motor de resolución y link pass + resolver Java

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-004
- **Estado:** `pending`

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

- **Crear:** `crates/devctx-parse/src/resolve/scope.rs` (scopes léxicos y `type_map` por scope),
  `crates/devctx-parse/src/resolve/java.rs`, `crates/devctx-index/src/link.rs` (link pass:
  `RepoIndex`, reglas 5-9 de DD-7, escritura de `dst_id`/`confidence`/`resolution`/`external`),
  fixtures Java en `crates/devctx-parse/tests/fixtures/java/` (multi-archivo, con paquetes).
- **Modificar:** `crates/devctx-parse/src/parser.rs` (retira `qualified_target`/`extract_type_bindings`
  planos en favor del scope), `crates/devctx-parse/languages/java.json` (`scopes`, `types` con
  `generic_type`, `@init`, `platform_prefixes`), `crates/devctx-index/src/pipeline.rs` (llamar al
  link pass; `IndexResult.edges_discarded`, `edges_unresolved`), `crates/devctx-store/src/symbols.rs`
  (lecturas/escrituras del link pass por lotes).

## Pasos

- [ ] **Paso 1 — tests que fallan (Java).** Fixture multi-archivo: (a) las tres clases internas de
      `DraftResource` con `service` de tipos distintos → cada llamada a su servicio, `high`,
      `field`; (b) `var q = em.createQuery(…); q.setParameter(…)` → nada con `var.`; (c)
      `LOG.info(…)` con `private static final Logger LOG` → `Logger.info`, `external`; (d)
      constructor injection `this.repo = repo` → `ctor_inject`; (e) `import x.y.Z` y mismo paquete;
      (f) `Office.findByCodigo(c).flatMap(o -> …)` → `findByCodigo` resuelto, `flatMap` descartado
      y contado; (g) `new Foo()` → `instantiates`; (h) estático Panache no definido →
      `inherited` + `external`; (i) `for (Foo f : lista) f.bar()` con `List<Foo> lista`.
- [ ] **Paso 2 — scopes.** `type_map` por scope léxico (clase → método → bloque/lambda), campos por
      clase (cada clase interna con los suyos), params, locales; `var` desde `new T`/cast/estático de
      tipo del repo; genéricos: tipo base + argumento para for-each.
- [ ] **Paso 3 — reglas locales (1-6 de DD-7)** en la fase por archivo: `dst_name` calificado
      localmente y `resolution` provisoria.
- [ ] **Paso 4 — link pass.** `RepoIndex` de la rama en memoria (`qualified → ids`, `name → ids`,
      paquete → archivos, herencia); reglas 5-9; incremental según DD-6 (archivos escritos +
      aristas cuyo `dst_name` toca símbolos agregados/borrados + no resueltas; pase completo sobre
      umbral). Escritura por lotes.
- [ ] **Paso 5 — descarte de fluent no tipable** (DD-7) con contador en `IndexResult` y en el
      resumen del `index`.
- [ ] **Paso 6 — `external`/`from_test`** según DD-9; listas de plataforma Java en `java.json`.
- [ ] **Paso 7 — medir** con el arnés sobre backend-b y, con OK del usuario,
      backend-a: tabla de §6 (basura, % sin decidir, externos, tests) y precisión de gold edges
      Java. Memoria del link pass (`VmHWM`) y tiempo incremental de 1 archivo.

## Criterios de aceptación

- [ ] Tests (a)-(i) verdes.
- [ ] backend-a (o backend-b si no se aprobó el grande): 0 `var.*`, 0 constantes
      tomadas por tipo, `calls` sin decidir ≤ 15 %, `from_test` = `path_kind` Test en el 100 %.
- [ ] Gold edges Java: precisión ≥ 90 % en `high`, ≥ 75 % global.
- [ ] Link pass incremental de 1 archivo < 2 s en backend-a; `VmHWM` del indexado ≤ +200 MB
      sobre la línea base (DD-21). Medidos y anotados.
- [ ] Compuerta de ruido (master §5) evaluada y escrita en el Resultado.

## Riesgos

- Reglas demasiado agresivas inventan resoluciones: `unique_name` es `medium`, nunca `high`; el gold
  set lo audita por `resolution`.
- Panache/Mutiny tienen convenciones que cambian por versión: las listas viven en el JSON y se
  pueden ajustar sin código.
- El link pass completo en repos grandes: si supera 10 s, perfilar antes de seguir (es el camino de
  cada `index --full`).

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
