# TASK-001 — Arnés de evaluación del grafo y del contexto + línea base sobre 0.9.0

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** — (primera del plan)
- **Estado:** `pending`

---

## Objetivo

Que cada task posterior pueda probar con números que mejora el grafo y el contexto sin romper lo
que ya funciona, y dejar medida la línea base sobre **0.9.0** (el inventario es de 0.8.2). Es la
implementación de DD-1: métricas del grafo, precisión contra gold edges, relevancia de
`search`/`build_context`, latencias y costo de indexado.

## Contexto verificado

- Ya existe un arnés de recuperación: `scripts/bench-retrieval.sh` (posición del archivo esperado en
  `devctx search --format json`, vector vs `--hybrid`, resumen por idioma) y
  `scripts/bench-queries.txt` (41 líneas, formato `consulta|archivo|idioma`, targets escritos antes
  de medir). Se extiende, no se reemplaza.
- CLI disponible: `devctx search … --format json [--hybrid] [--no-rerank]`, `devctx context <q>
  [--max-tokens N] [--no-memories]`, `devctx impact <símbolo> [--depth N]`, `devctx symbol`
  (`crates/devctx-cli/src/main.rs`, enum `Command` desde la l.52).
- El DB vivo lo tiene abierto el serve: las métricas SQL corren sobre una **copia** (el inventario
  hizo lo mismo, `devctx-graph-inventory.md` §8). DuckDB por CLI o python+duckdb en el scratchpad.
- Esquema actual: `graph_edges` y `vectors` (`crates/devctx-store/src/schema.rs:76-124`).
- Anclas verificadas para los casos (2026-10-05):
  - backend-a: `src/main/java/com/example/app/service/OfficeService.java` (`actualizar`, l.113),
    `src/main/java/com/example/app/async/handler/RenderResultHandler.java`
    (`reactivarItem`, l.91; llamada l.69), paquete `item/batch/service/` (p. ej.
    `ItemHistoryBatchService.java`).
  - backend-b: `src/main/java/com/example/b/resource/DraftResource.java`
    (clases internas con campo `service`: l.234, l.293, l.352; llamada l.310).
  - frontend: `lib-auth/src/lib/providers/token.interceptor.ts:78`
    (`export const tokenInterceptor`), `lib-auth/src/lib/services/auth.service.ts:60` y
    `lib-auth-data/src/lib/services/auth.service.ts:26` (dos `AuthService`).
  - legacy-migration: `src/pipeline.py`, `src/resolver.py`, scripts con `if __name__ ==
    "__main__"` (`e2e.py:164`, `backfill_folio_final.py:289`).
  - DevCtxEngine: los 41 casos de `bench-queries.txt` + identificadores (`search_ranked`,
    `symbol_definitions`, `replace_file_edges`, `Cached`).
- Repo público en GitHub (`origin`): los casos de ACME no se commitean (Q-5 del master).

## Archivos

- **Crear:** `scripts/graph-eval/run.sh` (orquesta todo, lee `$DEVCTX_EVAL_CASES`),
  `scripts/graph-eval/graph_metrics.sql` (métricas del grafo; versión para el schema viejo y, cuando
  exista, para `symbols`/`edges`), `scripts/graph-eval/score.py` (Hit@k, MRR, presencia en el brief,
  precisión de gold edges; solo stdlib), `scripts/graph-eval/cases-devctx.txt` (casos de este repo),
  `scripts/graph-eval/gold-devctx.txt` (gold edges de Rust), `scripts/graph-eval/README.md`.
- **Crear fuera del repo (con OK del usuario, Q-5):** `/home/you/acme/scripts/devctx-eval/cases-acme.txt`
  y `gold-acme.txt`.
- **Modificar:** `scripts/bench-retrieval.sh` solo si conviene que lea el formato nuevo (opcional).

## Pasos

- [ ] **Paso 1 — formato de casos.** `consulta | archivo(s) esperado(s) separados por , | símbolo
      esperado (opcional) | repo | idioma`. Reglas en el README: los targets se escriben **antes**
      de medir; no se editan después salvo error de transcripción, anotado.
- [ ] **Paso 2 — casos.** ~30 en total: 10 DevCtxEngine (reusar de `bench-queries.txt` + 3 de
      identificador), 8 backend-a, 2 backend-b, 6 frontend (incluye
      `tokenInterceptor` por nombre y por descripción), 4 legacy-migration. Al menos un
      tercio en español y un tercio "de comportamiento" (no de identificador).
- [ ] **Paso 3 — gold edges.** ~40 sitios `repo|archivo:línea|destino calificado esperado|external?`:
      20 Java (incluye `DraftResource.java:310 → BetaService.findPaginated`, un `var q =
      em.createQuery(…)`, un `LOG.info`, una llamada en cadena Mutiny, un `new X()`, una llamada en
      constructor, un estático Panache), 10 TS (DI por constructor, `inject()`, import relativo,
      alias Nx, llamada dentro de un arrow-const), 5 Python (módulo, `self.x`, builtin, import
      relativo, segunda llamada repetida), 5 Rust (`Foo::bar`, `Self::`, trait impl, `use`, método
      en `impl<T>`).
- [ ] **Paso 4 — `graph_metrics.sql`.** Reproduce las filas del inventario: aristas por kind,
      sources/targets distintos, % sin calificar, % "internas" por nombre, % desde tests (`/test/`
      y `path_kind`), `var.*`, calificadores en MAYÚSCULA que son campos (`LOG`), targets con
      `(`/espacios, nombres con > 1 forma calificada. Segunda sección (vacía hasta TASK-003): % de
      `calls` con `dst_id`, por `confidence`/`resolution`, `external`, `from_test`, ocurrencias.
- [ ] **Paso 5 — `run.sh`.** Por repo de los casos: `search --hybrid --format json --limit 20` y
      `--mode vector`, `context --max-tokens 4096` (texto: busca `// archivo:` y el nombre del
      símbolo), `impact` sobre la lista fija con `time` (5 corridas, serve caliente: un `devctx
      status` antes), copia del DB a `$TMPDIR` y `graph_metrics.sql`. Salida: tabla markdown lista
      para pegar en un `## Resultado`.
- [ ] **Paso 6 — costo de indexado.** `index --full` cronometrado en DevCtxEngine y en
      backend-b (234 archivos) **sobre copias de trabajo en el scratchpad** o con el
      OK del usuario; tamaño del DB antes/después; `VmHWM` con `scripts/memprobe.sh` si PLAN-010
      TASK-001 ya lo dejó en `main`, si no `/usr/bin/time -v`. backend-a y frontend:
      solo si el usuario aprueba el tiempo (o se toma el número de PLAN-008 TASK-016 y se dice).
- [ ] **Paso 7 — línea base.** Correr todo con el `devctx 0.9.0` instalado. Si el índice de un repo
      tiene un extractor distinto del de 0.9.0 (`index_status` → `extractor_stale`), medirlo sobre
      una copia reindexada o declararlo en el Resultado.
- [ ] **Paso 8 — completar la columna "Antes" de PLAN-009 §6** en el mismo commit de cierre.

## Criterios de aceptación

- [ ] `scripts/graph-eval/run.sh` corre de punta a punta contra DevCtxEngine sin casos de ACME
      (`DEVCTX_EVAL_CASES` sin setear) y produce la tabla.
- [ ] Con los casos de ACME, el Resultado tiene: métricas del grafo de backend-a en 0.9.0
      comparadas fila a fila con el inventario 0.8.2; Hit@5/Hit@10/MRR híbrido y vectorial;
      presencia en el brief por caso; precisión de gold edges en 0.9.0 (por "nombre calificado
      igual al esperado"); latencias de `impact`; costo de indexado.
- [ ] Ningún archivo bajo `scripts/graph-eval/` nombra rutas, clases ni paquetes de ACME
      (`rg -i '<patrones privados>' scripts/graph-eval` vacío).
- [ ] La columna "Antes" de §6 del master queda completa o con el motivo de cada celda vacía.

## Riesgos

- El serve vivo retiene el DB: siempre copiar antes de consultar; nunca `serve --stop` en acme sin
  permiso.
- Reindexar los repos grandes cuesta horas: se usa backend-b como Java rápido y se
  declara qué no se re-midió.
- Ruido entre corridas (latencia, temperatura del serve): 5 corridas y p50/p95, no un número.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
