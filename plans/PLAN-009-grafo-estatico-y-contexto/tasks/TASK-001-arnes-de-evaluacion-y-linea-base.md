# TASK-001 — Arnés de evaluación del grafo y del contexto + línea base sobre 0.9.0

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** — (primera del plan)
- **Estado:** `done`

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
  - legacy-migration: módulos de la raíz y de `src/`, scripts con `if __name__ ==
    "__main__"` (rutas y líneas reales en el gold privado).
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

- [x] **Paso 1 — formato de casos.** `consulta | archivo(s) esperado(s) separados por , | símbolo
      esperado (opcional) | repo | idioma`. Reglas en el README: los targets se escriben **antes**
      de medir; no se editan después salvo error de transcripción, anotado.
- [x] **Paso 2 — casos.** ~30 en total: 10 DevCtxEngine (reusar de `bench-queries.txt` + 3 de
      identificador), 8 backend-a, 2 backend-b, 6 frontend (incluye
      `tokenInterceptor` por nombre y por descripción), 4 legacy-migration. Al menos un
      tercio en español y un tercio "de comportamiento" (no de identificador).
- [x] **Paso 3 — gold edges.** ~40 sitios `repo|archivo:línea|destino calificado esperado|external?`:
      20 Java (incluye `DraftResource.java:310 → BetaService.findPaginated`, un `var q =
      em.createQuery(…)`, un `LOG.info`, una llamada en cadena Mutiny, un `new X()`, una llamada en
      constructor, un estático Panache), 10 TS (DI por constructor, `inject()`, import relativo,
      alias Nx, llamada dentro de un arrow-const), 5 Python (módulo, `self.x`, builtin, import
      relativo, segunda llamada repetida), 5 Rust (`Foo::bar`, `Self::`, trait impl, `use`, método
      en `impl<T>`).
- [x] **Paso 4 — `graph_metrics.sql`.** Reproduce las filas del inventario: aristas por kind,
      sources/targets distintos, % sin calificar, % "internas" por nombre, % desde tests (`/test/`
      y `path_kind`), `var.*`, calificadores en MAYÚSCULA que son campos (`LOG`), targets con
      `(`/espacios, nombres con > 1 forma calificada. Segunda sección (vacía hasta TASK-003): % de
      `calls` con `dst_id`, por `confidence`/`resolution`, `external`, `from_test`, ocurrencias.
- [x] **Paso 5 — `run.sh`.** Por repo de los casos: `search --hybrid --format json --limit 20` y
      `--mode vector`, `context --max-tokens 4096` (texto: busca `// archivo:` y el nombre del
      símbolo), `impact` sobre la lista fija con `time` (5 corridas, serve caliente: un `devctx
      status` antes), copia del DB a `$TMPDIR` y `graph_metrics.sql`. Salida: tabla markdown lista
      para pegar en un `## Resultado

- **Estado final:** `done`. El arnés está y la línea base de 0.9.0 está medida, con dos huecos declarados:
  el costo de indexado de backend-a / frontend no se midió y los tiempos de indexado que sí
  se midieron no son comparables (máquina a carga 50-125; ver "Costo").
- **Método.** Binario `devctx 0.9.0` instalado (copiado a un sandbox `/var/tmp/devctx-eval-XXXXXX`, con
  `DEVCTX_HOME`, `HOME` y `DEVCTX_MODEL_CACHE` propios; nada de `~/.local/share/devctx` ni del serve vivo),
  modelo `ml-granite` (el de la máquina), serve del sandbox caliente, `search --no-rerank --limit 20`,
  `context --max-tokens 4096 --no-memories`, `impact` depth 3 × 5 corridas. Repos y commits medidos:
  DevCtxEngine `ecce342` (clon indexado desde cero), backend-b `<commit-b>` y legacy-migration
  `<commit-d>` (clones indexados desde cero con 0.9.0), backend-a `<commit-a>` (rama `development`) y
  frontend `<commit-c>` (rama `development`): **copias del DuckDB vivo** (solo lectura sobre acme) con el
  clon en ese commit, porque reindexarlos con la máquina cargada era inviable (5 índices en paralelo: 312
  chunks en 572 s). Equivalencia verificada (ver Hallazgos): el grafo y los chunks de esas copias son los
  mismos que produce 0.9.0. Los 30 casos y los 42 gold edges se etiquetaron leyendo el código de esos commits,
  antes de medir. Los casos y gold de ACME viven **fuera del repo**: `/home/you/acme/scripts/devctx-eval/`
  (`cases-acme.txt` 20 casos, `gold-acme.txt` 36 sitios, `impact-acme.txt`; esa carpeta no está en un repo git).
  Carga: `DEVCTX_EVAL_CASES`, `DEVCTX_EVAL_GOLD`, `DEVCTX_EVAL_IMPACT`, `DEVCTX_EVAL_ROOT`.

### Línea base: relevancia (30 casos; `—` = no apareció en el top 20)

| Repo | casos | vector Hit@5 / Hit@10 / MRR | híbrido Hit@5 / Hit@10 / MRR | brief: archivo / símbolo |
|---|---|---|---|---|
| DevCtxEngine | 10 | 9/10 · 9/10 · 0.774 | 9/10 · 9/10 · 0.725 | 9/10 · 3/4 |
| backend-a | 8 | 7/8 · 7/8 · 0.515 | 7/8 · 7/8 · 0.802 | 7/8 · 7/8 |
| backend-b | 2 | 1/2 · 1/2 · 0.100 | 1/2 · 1/2 · 0.542 | 1/2 · 2/2 |
| frontend | 6 | 2/6 · 2/6 · 0.333 | 2/6 · 2/6 · 0.264 | 3/6 · 2/6 |
| legacy-migration | 4 | 3/4 · 3/4 · 0.625 | 2/4 · 3/4 · 0.560 | 4/4 · 3/4 |
| **Total** | **30** | **22/30 · 22/30 · 0.552** | **21/30 · 22/30 · 0.619** | **24/30 (80.0 %) · 17/24 (70.8 %)** |

Por idioma (13 casos en español, 17 en inglés): vector es Hit@5 8/13, MRR 0.408; híbrido es Hit@5 6/13, MRR 0.435;
vector en Hit@5 14/17, MRR 0.662; híbrido en Hit@5 15/17, MRR 0.760. Tokens del brief: ≈ 3 960-4 100 estimados
(caracteres / 4; `context` no informa el uso real). **El híbrido empeora el español** (Hit@5 6/13 contra 8/13 del
vector) y mejora el inglés: relevante para la compuerta de TASK-010 y para el ego-graph de TASK-012.

Posición por caso, en el orden de cada archivo de casos (`cases-devctx.txt` #1-10; `cases-acme.txt` #1-20 =
BackEnd 1-8, backend-b 9-10, FrontEnd 11-16, Legacy 17-20). Brief: `F` archivo, `S` símbolo, `·` ausente,
`-` sin símbolo esperado.

| Grupo | vector | híbrido | brief |
|---|---|---|---|
| DevCtxEngine 1-10 | 3,1,3,1,1,13,1,1,1,1 | 1,1,2,1,1,—,4,1,1,2 | F-,F-,F-,F-,F-,··,F-,FS,FS,FS |
| ACME 1-8 (BackEnd) | 5,3,3,—,4,1,1,1 | 12,1,1,1,3,1,1,1 | ··,FS,FS,FS,FS,FS,FS,FS |
| ACME 9-10 (backend-b) | 5,— | 12,1 | ·S,FS |
| ACME 11-16 (FrontEnd) | 1,—,1,—,—,— | 2,—,1,—,—,12 | FS,··,FS,··,··,F· |
| ACME 17-20 (Legacy) | 2,1,—,1 | 6,1,14,1 | F·,FS,FS,FS |

Ruido: dos corridas del mismo índice de DevCtxEngine dieron el caso 7 en posición 3 y en 4 (MRR híbrido 0.733 y
0.725): el híbrido tiene ±1 posición de variación entre corridas, del orden de la regla "sin regresión > 2 %
relativo" de §6. TASK-016 debe correr el arnés 2-3 veces y usar la mediana. `--mode vector` no existe en el CLI
0.9.0: vectorial es `search` sin flag (`--hybrid` es el híbrido).

### Línea base: grafo

backend-a `<commit-a>` contra el inventario 0.8.2 (copia del mismo índice, rama `development`): **cada fila coincide
con el inventario**; 0.9.0 no cambió el grafo (la extracción de `devctx-parse`/`devctx-chunk` es idéntica entre
v0.8.2 y v0.9.0 salvo la huella del extractor).

| Métrica (`graph_metrics.sql`) | BackEnd 0.9.0 | inventario 0.8.2 | backend-b | FrontEnd | Legacy | DevCtxEngine |
|---|---|---|---|---|---|---|
| Tipos de arista | `calls` | `calls` | `calls` | `calls` | `calls` | `calls` |
| Filas | 81 811 | 81 811 | 7 765 | 27 058 | 857 | 15 643 |
| Sources / targets distintos | 9 277 / 8 990 | 9 277 / 8 990 | 953 / 1 979 | 6 134 / 5 110 | 134 / 253 | 2 098 / 2 001 |
| `target_file` / `metadata` poblados | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| Targets sin calificar | 53 389 (65.3 %) | 53 389 (65.3 %) | 4 556 (58.7 %) | 18 901 (69.9 %) | 765 (89.3 %) | 14 702 (94.0 %) |
| "Internas" (cota por nombre) | 24 779 (30.3 %) | 24 779 (30.3 %) | 1 466 (18.9 %) | 6 977 (25.8 %) | 151 (17.6 %) | 5 616 (35.9 %) |
| Desde archivos de test | 46 966 (57.4 %) | 46 966 (57.4 %) | 3 444 (44.4 %) | 1 754 (6.5 %) | 104 (12.1 %) | 2 294 (14.7 %) |
| Pelados que colisionan con un método interno | 13 793 (16.9 %) | 13 793 (17 %) | 650 (8.4 %) | 3 119 (11.5 %) | 122 (14.2 %) | 5 197 (33.2 %) |
| Nombres definidos con > 1 forma calificada | 721 de 7 807 (`setUp` 93, `listar` 24, `crear` 23) | 721 de 7 807 (idem) | 98 de 737 | 676 de 4 227 (`ngOnInit` 152) | 1 de 133 | 80 de 1 901 |
| `var.*` | 481 | 481 | 114 | 0 | 0 | 0 |
| `LOG`/`LOGGER` como tipo | 1 272 | 1 272 | 68 | 0 | 0 | 0 |
| Targets con `(`/espacio/salto | 0 | 0 | 0 | **135** | 0 | 2 |
| Chunks de código / archivos | 19 306 / 1 623 (en `vectors` 19 799 con memorias; `method` 11 711 y `class` 2 961 como el inventario) | 19 795 en `vectors` (11 711 `method`, 2 961 `class`) | 3 253 / 216 | 13 762 / 1 580 | 312 / 62 | 4 796 / 276 |
| Calificadores más frecuentes (BackEnd) | `String` 2 884, `Uni` 2 197, `LOG` 1 272, `Response` 1 270 | `String` 2 884, `Uni` 2 197, `Response` 1 368, `LOG` 1 272 | | | | |

(Diferencia menor: `Response` 1 270 contra 1 368 del inventario, que contaba otra forma del calificador.) Las cifras de
FrontEnd, Legacy y DevCtxEngine son nuevas: el inventario era solo Java. Cota por nombre = el último segmento del
destino coincide con un método/función/clase definido en el repo (el grafo viejo no tiene identidad de símbolo).

### Línea base: precisión de resolución (42 gold edges, "nombre calificado igual al esperado", línea ±2)

| Grupo | sitios | correcto | error (resuelve a otro) | sin calificar | sin arista | precisión | sobre decididos |
|---|---|---|---|---|---|---|---|
| **Todos** | 42 | 19 | 3 | 12 | 8 | **45.2 %** | 86.4 % |
| Java (BackEnd 16 + backend-b 4) | 20 | 10 | 3 | 5 | 2 | 50.0 % | 76.9 % |
| TypeScript (FrontEnd) | 10 | 3 | 0 | 2 | 5 | 30.0 % | 100 % |
| Python (Legacy) | 6 | 3 | 0 | 2 | 1 | 50.0 % | 100 % |
| Rust (DevCtxEngine) | 6 | 3 | 0 | 3 | 0 | 50.0 % | 100 % |
| Internos (32) / externos (10) | | 12 / 7 | 2 / 1 | 12 / 0 | 6 / 2 | 37.5 % / 70.0 % | 85.7 % / 87.5 % |

"Sin calificar" = la arista existe pero el destino es el nombre pelado (`findPaginated`): no decide. "Sin arista" =
ninguna arista en esa línea (±2). Para los `external`, "correcto" solo dice que el grafo no los resolvió a una definición
del repo por nombre: 0.9.0 no marca externos. Errores: `DraftResource` :304 y :363 resuelven a
`AlphaService.findPaginated` (H3 confirmada, y es peor: también el adaptador de gamma) y `Office.findById`
(Panache) cuenta como ligado a una definición del repo por nombre (hay otro `findById` definido: es la cota por nombre). "Sin arista": las 5 llamadas de TypeScript dentro de arrow-const
(`tokenInterceptor`, `sessionGuard`, `accessGuard`: el `export const x = () =>` no es símbolo), la 2.ª `log.debug` de un
método de Python (deduplicada por par fuente→destino) y `new X()` en Java (la arista de `new AlphaServiceAdapter`
no existe). Cobertura (calificado, sin `high`/`low` todavía): 22/42 = 52 %.

### Línea base: `impact` y búsqueda (serve caliente, máquina cargada: orden de magnitud, no valor fino)

| Repo / símbolo | p50 | p95 | upstream / downstream |
|---|---|---|---|
| BackEnd `get` | 10.9 s | 16.6 s | 1 884 / 0 |
| BackEnd `map` | 14.0 s | 16.8 s | 2 569 / 11 |
| BackEnd `actualizar` | 1.44 s | 1.57 s | 19 / 210 |
| BackEnd `OfficeService.actualizar` | 0.32 s | 0.36 s | 1 / 39 |
| DevCtxEngine `get` / `map` / `open` | 2.08 / 3.62 / 1.74 s | 2.33 / 4.21 / 2.20 s | 782 / 1 122 / 662 líneas |
| DevCtxEngine `Store.open` / `search_ranked` | 0.18 / 0.35 s | 0.27 / 1.00 s | 22 / 271 líneas |

`search`: p50 0.24-0.37 s vectorial y 0.40-0.89 s híbrido; `context`: p50 0.26-0.70 s. Las primeras llamadas (carga del
modelo en el serve) llegan a 5-10 s y ensucian el p95 de esas listas. Los upstream/downstream de BackEnd coinciden con el
inventario (`map` 2 569 / 11; `OfficeService.actualizar` 1 / 39; `get` 1 884 contra 1 883).

### Costo de indexado (`index --full` desde cero, `memprobe.sh`, ruta directa con HNSW)

| Repo | archivos / chunks | tiempo | VmHWM | DB |
|---|---|---|---|---|
| DevCtxEngine | 276 / 4 796 | 2 393 s | 840 MiB | 28.1 MB |
| backend-b | 216 / 3 253 | 2 489 s | 727 MiB | 21.0 MB |
| Legacy | 62 / 312 | 572 s | 826 MiB | 6.8 MB |
| BackEnd (copia viva, no reindexado) | 1 623 / 19 306 | no medido | no medido | 622 MB |
| FrontEnd (copia viva, no reindexado) | 1 580 + 1 410 (2 ramas) / 13 762 + 12 119 | no medido | no medido | 2.34 GB |

**Los tiempos no sirven como línea base de ratio**: la máquina tenía carga 50-125 en 20 núcleos (otras sesiones y el
serve vivo indexando) y yo corrí 3-5 índices a la vez para terminar; Legacy (312 chunks) tardó 572 s. El `VmHWM` sí es
estable entre repos (727-840 MiB, dominado por el modelo). Para el criterio "≤ +10 % de tiempo" de §6 TASK-016 debe
remedir **antes y después en el mismo estado de máquina** (o con CPU-segundos con `DEVCTX_NO_AUTOSERVE=1` y la
máquina libre) con el procedimiento del README. BackEnd y FrontEnd: el número de PLAN-008 TASK-016 (FrontEnd > 25 min
cargada; ~1 h por 1 400 archivos en 8 núcleos, `AGENTS.md` §6) sigue siendo la referencia.

- **Hallazgos de §2 confirmados o corregidos.** (1) H3 confirmada en 0.9.0, y ampliada: `DraftResource` :304 (en
  `<commit-b>`; el esqueleto decía :310) y :363 resuelven a `AlphaService.findPaginated`. (2) 0.9.0 no cambió el
  grafo respecto de 0.8.2 (tabla de BackEnd) y los índices vivos son equivalentes a 0.9.0: con una mini-reindexación
  con 0.9.0 de 41 archivos de BackEnd y 69 de FrontEnd, las 2 697 y 473 aristas y los 662 y 282 chunks son idénticos a
  los de las copias vivas (diferencia simétrica 0); el índice vivo de Legacy (de agosto, binario anterior al 24-ago)
  difiere en 2 aristas Python con destino-expresión (`SequenceMatcher(None, a, b).ratio` pasó a `ratio`), así que
  Legacy se midió con un índice nuevo. (3) FrontEnd tiene **135 destinos que son expresiones** y las funciones flecha
  exportadas (`tokenInterceptor`, `sessionGuard`, `accessGuard`) no son símbolos: sin aristas desde ellas. (4)
  `devctx symbol tokenInterceptor` en 0.9.0 devuelve `definitions: []` **con `external: true`** (falso, está definido
  en el repo): el bug es peor que el `[]` del master. (5) El candidato de PLAN-008 medía `external` fallando con nombres
  calificados (B2) y `Foo::new` como falso externo: 0.9.0 final ya da `external: true` + `called_from` para
  `serde_json::from_str` (60) y `Panache.withTransaction` (9), y `Foo::new` sin `external`; esas dos filas de §6 ya
  están en la meta salvo re-verificación. (6) `search --format json` de 0.9.0 devuelve `{"results": [...]}`, no el
  arreglo pelado: `scripts/bench-retrieval.sh` estaba roto (posiciones en blanco y "found" falso); corregido. (7) El
  híbrido es peor que el vectorial en español en este set (6/13 contra 8/13 en Hit@5). H5 (`Cached<T>`) y las
  estimaciones de DD-2/DD-6 no se tocaron aquí.
- **Archivos tocados:** `scripts/graph-eval/{run.sh,graph_metrics.sql,score.py,cases-devctx.txt,gold-devctx.txt,
  impact-devctx.txt,README.md}` (nuevos), `scripts/bench-retrieval.sh` (2 líneas: acepta `{"results": …}`), este
  archivo y el master (§5 y la columna "Antes" de §6). Fuera del repo: `/home/you/acme/scripts/devctx-eval/
  {cases-acme.txt,gold-acme.txt,impact-acme.txt}`.
- **Verificado por:** `python3 -m py_compile scripts/graph-eval/score.py` (ok); `bash -n` y `shellcheck -x` sobre
  `run.sh` (shellcheck 0.x instalado en un venv del sandbox: sin avisos); `scripts/graph-eval/run.sh` de punta a
  punta contra el clon indexado de DevCtxEngine **sin `DEVCTX_EVAL_CASES`** (rc 0, tabla completa, 3 pasos); la misma
  corrida con los casos de ACME, repo por repo; `rg -i '<patrones privados>' scripts/graph-eval` vacío;
  `devctx plan-status PLAN-009` sin avisos. No hay `cargo test` aquí: no se tocó `crates/`.
- **Desviaciones:** (1) BackEnd y FrontEnd se midieron sobre copias del índice vivo (equivalencia verificada), no
  reindexados en el sandbox (carga de máquina); el costo de indexado de esos dos no se midió. (2) Anclas del
  esqueleto corregidas contra el commit medido: `DraftResource` :310 → :304 (clases internas en :232/:291/:350),
  `tokenInterceptor` :78 → :13 en `<commit-c>` (el :78 es de `<commit-e>`; las 3 líneas del interceptor se fijaron
  contra `<commit-c>` antes de medir); las de BackEnd y Legacy coincidían. (3) 42 gold edges en vez de 40 (Python 6 por
  el par `log.debug` repetido, Rust 6) y se agregó `impact-devctx.txt`. (4) `run.sh` no reindexa (el costo se mide
  aparte, README) y hay `DEVCTX_EVAL_STEPS`/`DEVCTX_EVAL_ONLY` para correr partes. (5) El brief no informa tokens: se
  estiman en caracteres / 4. (6) Las métricas sobre `graph_edges` usan "interna" como cota por nombre, igual que el inventario.
- **No verificado:** el costo de indexado de BackEnd/FrontEnd y los tiempos de los demás en máquina libre; los números
  de `impact` y `search` son de una máquina cargada; no se midió con `--rerank`; `build_context` de grupo y las tools
  de memoria (`memories_by_symbol`) no se re-midieron; Windows/macOS no aplica (scripts bash/Linux, `memprobe.sh` solo
  Linux). El gold es de 42 sitios, la precisión por lenguaje descansa en 6-10 casos cada uno.
- **Riesgos abiertos / siguiente:** TASK-003 ya puede mergear (la línea base está cerrada). TASK-016 debe: correr el
  arnés 2-3 veces (mediana, por el ruido del híbrido), remedir el costo antes/después en el mismo estado de máquina,
  y reindexar BackEnd/FrontEnd en el sandbox (los clones de `<commit-a>` y `<commit-c>` están en el sandbox para
  comparar contra el mismo corpus). El sandbox `/var/tmp/devctx-eval-XXXXXX` (≈ 3 GB; copias de índices privados)
  se puede borrar cuando ya no haga falta.
