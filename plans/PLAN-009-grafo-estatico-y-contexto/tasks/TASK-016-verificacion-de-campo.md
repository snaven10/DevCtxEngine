# TASK-016 — Verificación de campo contra §6

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** — (orquestador, con aprobación del usuario; la sesión revisora mide)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-015
- **Estado:** `in_progress`

---

## Objetivo

Medir, con el binario candidato y el arnés de TASK-001, cada fila de los criterios globales del
master (§6) contra la línea base 0.9.0, reproducir los casos de campo y dejar asentado qué se
verificó y qué no, antes del cross-check y del tag.

## Contexto verificado

- Arnés: `scripts/graph-eval/` (TASK-001); casos de ACME en `$DEVCTX_EVAL_CASES` (fuera del repo).
- Línea base 0.9.0: columna "Antes" de §6, completada en TASK-001.
- Casos de campo: `DraftResource.java:310` (backend-b),
  `tokenInterceptor` (frontend), Python de legacy-migration, `impact("get"/"map")`
  en backend-a, B2/B3/B4 de PLAN-008 TASK-016, pendientes 1 y 2 de PLAN-008.
- Método de PLAN-008 TASK-016: sandbox aislado, binario nuevo compilado aparte, VIEJO = tag
  `v0.9.0`; tokens aproximados como chars/4; solo lectura sobre `~/acme` salvo reindex aprobado.
- CI: job `cross-check` en pushes a `fix/**` (`.github/workflows/rust.yml`).

## Archivos

- **Modificar:** este archivo (`## Resultado`) y el master (§13 Cierre) al cerrar el plan.

## Pasos

- [x] **Paso 1 — aprobación.** Pedir al usuario qué repos reindexar (estimación de tiempo con el
      reuso de vectores medido en TASK-002): mínimo backend-b, legacy-migration,
      DevCtxEngine; idealmente backend-a y frontend.
- [x] **Paso 2 — reindex** con el candidato (en copias o en sitio, según lo aprobado),
      cronometrando y midiendo tamaño y `VmHWM`.
- [x] **Paso 3 — arnés completo** y tabla de §6 fila por fila (Antes / Meta / Medido / Veredicto).
- [x] **Paso 3b — centralidad (pendiente de TASK-010, revisión MAJOR 1 y 2).** Correr el arnés
      híbrido y el brief con `search.centrality_weight` 0, 0.15, 0.3 y 0.5 sobre los 30 casos,
      **con el corte por idioma** (TASK-001 midió híbrido < vectorial en español). Encenderla por
      default solo si mejora el brief sin bajar Hit@5/MRR híbrido > 2 % relativo **ni empeorar el
      corte en español**; si no, sigue en 0 (opt-in). Pendientes de la segunda revisión de
      TASK-010: que los empates de percentil compartan el bonus promedio (hoy los desempata el
      orden de la fusión); re-medir la latencia de la CTE de percentil de `chunk_ranks` con
      `w` > 0 (la medida de TASK-010 fue con la lectura por probabilidad); cachear
      `local_centrality` en la TUI (hoy se calcula al abrir el motor local, una vez).
- [x] **Paso 4 — casos de campo** uno por uno con la salida real de la tool.
- [x] **Paso 5 — contratos.** Respuestas de `search` (vector), `read_symbol`, `get_references`,
      `impact_analysis`, `build_context` de 0.9.0 vs candidato sobre los mismos casos: solo
      diferencias aditivas o mejoras justificadas.
- [x] **Paso 6 — rama v1.** Un repo sin reindexar con el candidato: aviso de índice viejo, tools
      viejas contestan, tools nuevas piden `index --full`.
- [ ] **Paso 7 — cross-check** Windows/macOS empujando una rama `fix/**`; anotar el run.
- [x] **Paso 8 — cierre del plan** (§13 del master) con lo verificado y lo no verificado.

## Criterios de aceptación

- [ ] Tabla de §6 completa con veredicto por fila; toda fila que no cumple tiene causa y decisión
      (fix, `skipped` con motivo, o aceptación explícita del usuario).
- [ ] Cross-check verde antes de taggear.
- [ ] `devctx plan-status PLAN-009` sin warnings al cerrar.

## Riesgos

- El reindex de los repos grandes puede no caber en una sesión: se mide lo aprobado y se declara el
  resto como no verificado, como hizo PLAN-008 TASK-016 con frontend.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `in_progress` — medición completa (Pasos 1-6 y 8 escritos); falta el Paso 7
  (cross-check en CI, lo dispara un push que esta sesión no hace), la revisión del commit de bug
  `fb1707c` y del cambio de criterio del gold `e91a8b9`, y las decisiones del usuario sobre las
  filas que no cumplen (abajo, "Pendiente de decisión").
- **Método.** Sandbox `/var/tmp/<sandbox>` (`mktemp -d`), con `DEVCTX_HOME`/`HOME`/`XDG_*` propios
  por binario (nada de `~/.local/share/devctx` ni `~/.config/devctx`), modelo `ml-granite` copiado
  en solo lectura, todo en CPU con `nice -n 10`, índices por la vía directa
  (`DEVCTX_NO_AUTOSERVE=1`), de a un repo por vez. **VIEJO** = `v0.9.0` compilado desde el tag
  (`git archive v0.9.0`, `--locked`); **CANDIDATO** = `e0b8937` (el binario aún dice `0.9.0`: falta
  subir la versión antes del tag). Corpus = los commits de TASK-001: DevCtxEngine `b009f80`,
  backend-a `<commit-a>`, backend-b `<commit-b>`, frontend `<commit-c>`, legacy-migration
  `<commit-d>` (copias con `tar`, sin `.devctx`/`target`/`node_modules`/…; verificado con `find`
  antes de cada index). Por repo: VIEJO desde cero (`r090`) y CANDIDATO desde cero (`rcand`)
  seguidos; después de todas las medidas con VIEJO, el CANDIDATO `--full` **en sitio** sobre el
  índice VIEJO (el upgrade real), y el VIEJO otra vez sobre ese índice (downgrade). Tiempo con
  reloj monotónico, CPU y pico de RAM con `wait4` (`ru_maxrss` = `VmHWM`), tamaño con
  `pragma_database_size()` sobre una copia. Arnés: `scripts/graph-eval/run.sh` del repo, los 30
  casos de TASK-001 (10 en el repo + 20 fuera, `$DEVCTX_EVAL_CASES`), 3 rondas y mediana. Carga de
  la máquina anotada en cada corrida (4-42 en 20 núcleos: otras sesiones).

### Tabla de §6

"Antes" = TASK-001 sobre 0.9.0. "Re-base" = VIEJO medido aquí, mismo estado de máquina y mismos
índices nuevos. Dos números del candidato cuando difieren: **índice nuevo** (desde cero) / **índice
actualizado** (`--full` sobre el de 0.9.0, conserva sus vectores).

| Criterio | Antes | Meta | Medido (candidato) | Veredicto |
|---|---|---|---|---|
| Tipos de arista | 1 (`calls`) | ≥ 6 | 7 en DevCtxEngine, backend-a, backend-b y frontend; **5 en legacy-migration** (Python: no hay `implements`; la instanciación `Clase()` se guarda como `calls`) | cumple en 4/5; legacy por lenguaje |
| Nodos basura (`var.*`, `LOG.*`, expresiones) | backend-a 481 + 1 272 + 0, frontend 135 expresiones… | 0 | 0 / 0 / 0 en los 5 | cumple |
| `calls` sin `dst_id` ni `external` | 58-94 % sin calificar | ≤ 15 % | backend-a 8,4 %, backend-b 7,4 %, frontend 14,7 %, legacy 1,6 %, **DevCtxEngine 16,0 %** (17,4 % en `a6c39db`) | **no cumple** (Rust; ya lo decía TASK-007/010) |
| Externos marcados con evidencia | 0 % (sin marca) | 100 % con evidencia | auditoría de 20 llamadas por imports namespace/default del frontend: 20/20 `external_known` `high`, los 4 paquetes en `dependencies`; gold externos 2/2 Python, 1/1 Rust, 2/2 TS; 1 Java sin arista | cumple (muestra) |
| `from_test` | 0 % | 100 % (= `path_kind` Test) | en `calls`: 0 diferencias en los 5. La métrica del arnés da 234 en DevCtxEngine: `imports`/`contains` del símbolo archivo dentro de `mod tests` (la ocurrencia es de test, el símbolo archivo no) — la métrica supone la regla de antes de TASK-010 | cumple (la métrica queda por corregir) |
| Precisión sobre gold (42 sitios de TASK-001) | 19/42 = 45,2 % | ≥ 90 % `high`, ≥ 75 % global, cobertura ≥ 80 % | criterio nuevo (`e91a8b9`): 40/42 = 95,2 %, `high` 40/40, cobertura 40/42; criterio viejo: 38/42 = 90,5 %, `high` 38/40 (los 2 Python `mod.f`), cobertura 40/42. Re-base 0.9.0: 19/42 con los dos criterios (no usan la tabla `edges`). Gold de Rust ampliado (26 en `a6c39db`): 26/26, `high` 22/22 | cumple con los dos criterios (**provisorio**: `e91a8b9` espera revisión) |
| Ocurrencias repetidas | colapsadas | una fila por ocurrencia | legacy: la 2.ª `log.debug` del método tiene su fila (líneas distintas), y la llamada de módulo `sys.exit(main())` sale del símbolo archivo | cumple |
| `DraftResource` (la llamada del adaptador de `BetaService`) | `AlphaService.findPaginated` | `BetaService.findPaginated`, `high`, `field` | `BetaService.findPaginated`, `high`, **`ctor_inject`** (el campo se asigna en el constructor: regla 4 de DD-7, desviación anotada en TASK-005); también las de los otros dos adaptadores, cada una a su servicio; `get_references` lo lista | cumple (resolución con otro nombre, documentado) |
| `read_symbol("tokenInterceptor")` | `[]` + `external: true` falso | definición en el interceptor de la librería de auth | la definición (`function`) en el archivo del interceptor | cumple |
| `impact("get"/"map")` backend-a | 10,9/14,0 s p50 (carga 50-125); re-base 3,5/4,9 s | p95 ≤ 300 ms, sin tests/externos, con `omitted` | p95 90 / 179 ms (índice nuevo), 89 / 197 ms (actualizado), por el CLI, 5 corridas; `excluded`/`below_confidence` presentes | cumple |
| `external` calificado | `external` + `called_from` (60 y 9) | idem | `serde_json::from_str` 66; `tokio::spawn` (antes `suggestions: []`) `external: true` + 4; `with_context` 30 | cumple |
| `Foo::new` inexistente | sin `external`, `suggestions: []` | no externo; `suggestions` | sin `external`, 5 sugerencias (`App.new`…) | cumple |
| `memories_by_symbol("Foo.Bar::baz")` | no re-medido | símbolo, sin archivo | no medible sin memorias en el sandbox (la consigna prohíbe `remember`); la respuesta es la misma en los dos binarios (`matched_by: text-inference`, 0) | **no medido** |
| `repo_map(budget=1024)` | no existe | ≤ 1 177 tokens; archivo esperado ≥ 70 % con foco | máx. 1 024 tokens; foco por identificador 7/7; foco por vecino (TASK-011 m4): más fuerte 19/20, mediano 18/20, al azar 18/20, solo a 2 saltos 2/5 | cumple (a 2 saltos no) |
| Relevancia del brief (archivo) | 24/30 = 80,0 % | ≥ base + 10 pp, sin acierto → fallo sin explicar | 27/30 = 90,0 % (+10,0 pp) / **26/30 = 86,7 % (+6,7 pp)**; símbolo 17/24 → 20/24; ningún caso pasa de acierto a fallo (con `related` sí: abajo) | **cumple en índice nuevo, no en actualizado** |
| `search --hybrid` | Hit@5 21/30, MRR 0,619; re-base 21/30, 0,616 | ≥ base, sin regresión > 2 % | 22/30, 0,653 / 23/30, 0,647 | cumple |
| `--mode vector` idéntico | — | idéntico a 0.9.0 | VIEJO y CANDIDATO sobre el mismo índice actualizado: 30/30 respuestas iguales (archivo, líneas, score) | cumple |
| Contratos JSON | 0.9.0 | solo aditivos | esquemas de entrada: solo parámetros nuevos; respuestas (90 llamadas, 5 repos): todo lo quitado es una mejora o un filtro de Q-2 (abajo) | cumple |
| `index --full` desde cero | no comparable (carga 50-125) | ≤ +10 % | suma de los 5: −8,7 % reloj, −1,6 % CPU. Por par: legacy +6,6 %, backend-b −6,1 %, DevCtxEngine −1,4 %, backend-a −24 % (otra carga), frontend **+23 %** y repetido −5,7 % | cumple en suma; un par de frontend pasa (ruido ±20 %) |
| `--full` tras subir el extractor | = desde cero | ≤ 20 % de 0.9.0 desde cero | 7,1-9,7 %; frontend 18,7 % (15,9 % contra la 2.ª medida de 0.9.0) | cumple (frontend cerca) |
| Tamaño del DB (bloques usados, desde cero) | 28,1 / 21,0 / 6,8 MB… | ≤ +30 % | +9 %, +17 %, +4 %, +9 %, +13 % | cumple; **upgrade en sitio: +52 % a +58 %** usados en backend-a/frontend (hallazgo) |
| Pico de RAM (frontend) | no medido | ≤ +200 MB | +139 MiB (791 → 930) y +101 MiB (787 → 888); upgrade 917 MiB. Fuera de la meta: backend-a +237 MiB (797 → 1 034) | cumple en frontend |
| Índice viejo tras el cambio de binario | `extractor_stale` + `hint` | idem + tools nuevas piden `--full` | en los 5: `extractor_stale: true` + `hint`; `read_symbol`/`get_references`/`impact`/`search` híbrido con `warning`; vectorial sin aviso; `build_context` `related` → `not shown … index --full`; `traverse`/`repo_map`/`skeleton` → error con `devctx index --full (vectors are reused)` | cumple |

**Gold con el criterio viejo y el nuevo (pedido de la revisora sobre `e91a8b9`).** El caso que el
criterio viejo juzgaba mal: un sitio Python `FileA:l → FileB.f2` (`from . import FileB` y
`FileB.f2()`); la arista sale `high`/`import` hacia `f2`, cuyo `qualified` guardado es `f2` (los
símbolos de módulo de Python no llevan el módulo) y cuyo archivo es `FileB`. Viejo: "error"
(`f2` ≠ `FileB.f2`); nuevo: correcto, porque el nombre coincide y el archivo del destino es la
columna 5 del gold — el criterio con que TASK-007 juzgó sus 27 sitios. Sobre los **mismos**
`edges_new.tsv`, por repo (correcto / sitios, tabla `edges`):

| Índice | backend-a | backend-b | frontend | legacy | DevCtxEngine (26, `a6c39db`) |
|---|---|---|---|---|---|
| candidato nuevo, viejo → nuevo | 14 → 14 / 16 | 4 → 4 / 4 | 10 → 10 / 10 | **4 → 6** / 6 | 26 → 26 / 26 |
| candidato actualizado, viejo → nuevo | 14 → 14 | 4 → 4 | 10 → 10 | **4 → 6** | — |
| 0.9.0 (`graph_edges`), viejo → nuevo | 9 → 9 | 1 → 1 | 3 → 3 | 3 → 3 | — |

Solo cambian los 2 sitios Python; la tabla de `graph_edges` (la de "Antes") no usa el juicio
tocado, así que la columna "Antes" re-puntuada da lo mismo (19/42). Los dos fallos que quedan en
backend-a: un `var q = em.createQuery(…)` sin arista (`ClassB.m3`) y una llamada por interfaz
`low`/`name_only` (`ClassC.m4`).

### Paso 3b — centralidad y `related` (30 casos, mediana de 3 rondas)

| Configuración | híbrido Hit@5 / MRR | español (13) híbrido Hit@5 / MRR | brief archivo / en el código / símbolo |
|---|---|---|---|
| 0.9.0 (re-base) | 21/30 · 0,616 | 6/13 · 0,441 | 24 / 24 / 18 |
| cand. nuevo, `w` 0 | 22 · 0,653 | 6 · 0,450 | 27 / 27 / 20 |
| `w` 0,15 | 23 · 0,652 | 7 · 0,504 | 27 / 27 / 20 |
| `w` 0,3 | 23 · 0,641 | 7 · 0,468 | 27 / 27 / 20 (es 10 → 11, en 17 → 16) |
| `w` 0,5 | 22 · 0,633 | 6 · 0,461 | 27 / 27 / 22 |
| `related` / `related` + 2 saltos | 22 · 0,650 / 0,652 | 6 · 0,443 / 0,448 | 26 / 25 / 20 (los dos) |
| cand. actualizado, `w` 0 | 23 · 0,647 | 6 · 0,409 | 26 / 26 / 20 |
| `w` 0,15 | 22 · 0,638 | 6 · 0,458 | 26 / 26 / 20 |
| `w` 0,3 | 22 · 0,615 | 6 · 0,448 | 26 / 26 / 21 |
| `w` 0,5 | 22 · 0,593 | 6 · 0,447 | 26 / 26 / 22 |
| `related` / + 2 saltos | 23 · 0,647 | 6 · 0,409 | 26 / 25 / 20 (los dos) |

- **Decisión `w`: sigue en 0 (opt-in).** Ningún peso mejora el brief (archivo 27 = 27, 26 = 26);
  en el índice actualizado `w` 0,3 y 0,5 bajan el MRR híbrido 4,9 % y 8,3 % (> 2 %); `w` 0,15 sube
  el español en el índice nuevo y no en el actualizado.
- **Decisión `related`: sigue apagado.** Archivo en el código 27 → 25 y 26 → 25: un caso de
  backend-a (A1) pasa de acierto a fallo (la sección toma el 20 % y saca su código) y uno de
  backend-b pasa de fallo a "solo citado". Dos saltos no aportan nada sobre uno (m4): se puede
  retirar o dejar como está; no cambia ningún caso.
- Latencia de la CTE de percentil con `w` > 0 (m de TASK-010): p50 híbrido 49-108 ms con `w` > 0
  contra 68-124 ms con `w` 0 (rondas con distinta carga): dentro del ruido. Empates de percentil
  con bonus promedio y caché de `local_centrality` en la TUI: no se tocaron (con `w` = 0 no
  actúan; quedan anotados para cuando se encienda).
- Métrica nueva "archivo en el código" (`46f5df1`): con `related` apagado coincide siempre con
  "archivo en el brief"; con `related` aparece 1 caso solo citado.

### Paso 2 — costo de indexado

| Repo (archivos / chunks del cand.) | VIEJO desde cero: s · CPU s · VmHWM · bloques | CAND desde cero | CAND `--full` sobre VIEJO (en sitio) |
|---|---|---|---|
| DevCtxEngine (277 / 4 806) | 566 · 9 463 · 889 MiB · 106 | 558 · 9 254 · 913 · 116 | 53 s (9,4 %) · 219 · 896 · 114 → 173; 4 737 reusados |
| backend-b (216 / 3 284) | 529 · 8 703 · 718 · 76 | 496 · 8 508 · 740 · 89 | 38 s (7,1 %) · 152 · 750 · 85 → 124; 3 204 |
| legacy-migration (62 / 312) | 59 · 957 · 826 · 25 | 63 · 1 014 · 845 · 26 | 4 s · 5 · 169 · 30 → 36; 312 |
| backend-a (1 625 / 21 615) | 3 415 · 50 486 · 797 · 479 | 2 587 · 43 969 · 1 034 · 523 | 330 s (9,7 %) · 2 340 · 1 083 · 504 → 786; 20 242 |
| frontend (1 581 / 14 167) | 1 466¹ · 24 777 · 791 · 279; repetido 1 727 · 28 564 · 787 | 1 804 · 30 124 · 930 · 316; repetido (antes que el VIEJO) 1 629 · 27 489 · 888 | 274 s (18,7 %) · 2 132 · 917 · 293 → 463; 13 309 |

¹ El reloj del sistema saltó ~3 h 20 min durante esa corrida (`time.time`); el valor sale de los
`starttime` de `/proc` del proceso y del siguiente (incluye ~5 s de `dbsize`); desde ahí el script
usa reloj monotónico. Los bloques "antes" del upgrade son mayores que los de "desde cero" porque
el Paso 6 abrió esos índices con el candidato y el serve 0.9.0 corrió el arnés.

### Paso 4 — casos de campo (salida real)

- backend-b `DraftResource`, la llamada del adaptador de `BetaService` → `BetaService.findPaginated` `high`/`ctor_inject`;
  `read_symbol("BetaService.findPaginated")`: 0.9.0 `definitions: []` + sugerencias, candidato la
  definición; `get_references`: 0.9.0 4 llamadores sin ese sitio, candidato 5 con
  `DraftResource.BetaServiceAdapter.findPaginated`.
- frontend `tokenInterceptor`: 0.9.0 `external: true` + `called_from: 2` (falso); candidato la
  definición; `impact` baja de 2 llamadores (tests) a 0 por defecto (`excluded.tests: 3`) y
  gana 58 llamados (`ClassA.m1`, `ClassA.m2`… del servicio de auth).
- legacy-migration: dos `log.debug` del mismo método en dos filas (`external_known`); la llamada
  de módulo `sys.exit(main())` sale del símbolo archivo y llega a `main` (`same_file`).
- backend-a `impact("get"/"map")`: arriba (p95 90 / 179 ms).
- B2 (`serde_json::from_str`, `tokio::spawn`, `Panache.withTransaction`): externos con
  `called_from`; B3 (`with_context`): `external: true` + `called_from: 30`, sin sugerencias
  ruidosas; B4 y pendiente 1 de PLAN-008 (`memories_by_symbol` de `Foo.Bar::baz` y
  `links.rs::memories_by_symbol`): `text-inference`, 0 memorias en los dos binarios — **no
  medible** sin memorias (cubierto por los tests de TASK-008); pendiente 2 (`Foo::new`): no
  externo, con sugerencias.

### Paso 5 — contratos (0.9.0 contra candidato)

Esquemas de entrada (`tools/list`): 3 tools nuevas (`repo_map`, `skeleton`, `traverse`) y solo
parámetros agregados (`build_context`: `graph_hops`, `related`; `get_references`: `kinds`,
`min_confidence`; `impact_analysis`: `dispatch`, `include_external`, `include_tests`,
`max_nodes`, `min_confidence`); ninguno quitado ni con otro tipo. Respuestas, 90 llamadas sobre
los mismos casos (por repo: 3 `search` vectorial, 3 `build_context`, y `read_symbol`,
`get_references`, `impact_analysis` sobre 3-8 símbolos): todos los campos nuevos son aditivos
(`sym`, `qualified`, `kind`, `confidence`, `via`, `filters`, `excluded`, `below_confidence`,
`undecided_calls`, `omitted_for_budget`…). Lo que desaparece en los diffs de claves: (a)
`suggestions` → `definitions` cuando el candidato encuentra la definición (backend-a, backend-b,
frontend, DevCtxEngine) o `external` falso → `definitions` (`tokenInterceptor`); (b) entradas de
`upstream`/`downstream` que son tests o externos, ocultas por los filtros por defecto de Q-2 y
contadas en `excluded`. `search` vectorial: mismas claves; `raw_score` aparece donde hay un hit
penalizado (ya en 0.9.0). Sin regresiones de contrato.

### Paso 6 — rama v1 (índice 0.9.0 sin reindexar, candidato)

En los 5 repos: `index_status` `extractor_stale: true` + `hint` (`run devctx index --full`);
`read_symbol`, `get_references`, `impact_analysis` y `search` híbrido contestan por el camino
viejo con `warning` (vectorial sin aviso); `build_context` con `related` dice `related: not
shown: … run devctx index --full`; `traverse`, `repo_map` y `skeleton` → error `… reads the symbol
graph, and this branch's index predates it (an older extractor): reindex with devctx index
--full (vectors are reused)`. `mcp.version` dice `0.9.0` también en el candidato: hay que subir la
versión de los crates antes del tag.

### Downgrade (VIEJO sobre los 5 índices actualizados)

Contesta todo, con `extractor_stale: true` y su aviso, sin errores; su búsqueda vectorial da lo
mismo que el candidato sobre la misma base (30/30). Sus respuestas de grafo **no** son las de
antes del upgrade (idénticas 30 de 60 sin contar el `warning`): `graph_edges` lo escribe el
extractor nuevo (clases anidadas calificadas por su clase exterior, `var`/`LOG` fuera, `List.get`
en vez de `get`, fuentes `tests.x` en Rust). Un caso era pérdida: las llamadas de Rust por ruta se
guardaban con `::` (`devctx_search::search_ranked`, 1 246 filas en DevCtxEngine) y 0.9.0, que
resuelve por `= nombre` o sufijo `.nombre`, perdía a esos llamadores (`get_references` de
`search_ranked`: 9 → 7, sin `cmd_search` ni `search_items`). **Bug arreglado en `fb1707c`** (abajo).
Lo que queda (un recorrido de `impact` de 0.9.0 por una clase anidada corta un nivel antes) es la
mejora de los nombres vista por un lector viejo; se documenta en 13 (EN/ES) y el CHANGELOG.

### Pendientes "para TASK-016" — estado

| Pendiente | Estado |
|---|---|
| Auditoría de 20 imports namespace/default TS (TASK-006) | hecha: 47 imports (9 namespace, 38 default; 30 de paquete), 63 usos con arista, todos `external_known` `high`; muestra de 20 (semilla fija): 20/20 correctos (4 paquetes npm, todos en `dependencies` del `package.json`). Los 17 imports relativos no tienen llamadas por ese nombre: sin muestra de campo |
| repo_map con vecino mediano / al azar / a 2 saltos (TASK-011 m4) | medido: 19/20, 18/20, 18/20, 2/5 |
| Empates de percentil, CTE con `w` > 0, caché de `local_centrality` (TASK-010) | CTE medida (ruido); empates y caché sin tocar (con `w` 0 no actúan) |
| m4 hop 2 sin aporte | confirmado: 0 casos cambian; +22-51 ms p50 |
| m5 caché de `dispatch_index`, m6 reader/store abiertos dos veces, m9 detección por substring | no hechos (anotados) |
| m10 DD-18 "no evaluable", 5/7 contra 7/7 | no re-medido (sin el grupo de serves vivos) |
| Nota con holgura chica, tabla de 400 briefs con memorias y `linked`, `if out.contains` vacuo, aviso de `--hops` solo en el CLI | no hechos (anotados; el aviso sigue solo en el CLI, `backend.rs::note_unapplied_related` sigue por substring) |
| Perf de `edge_level` bajo carga (+159 ms) | con la máquina a carga 5-8: `related` +14-27 ms p50 (p95 +23-57), dos saltos +22-51 ms: dentro de la meta (+150 ms); el +159 ms bajo carga no se reprodujo; las optimizaciones (aristas por id, `UNION ALL`, `prepare_cached`) quedan para después |
| Métrica "archivo citado vs código presente" | hecha (`46f5df1`) |
| Costo de `--full` sobre un índice 0.9.0 real, tamaño, `VmHWM` | medido (Paso 2) |
| Downgrade sobre una copia | medido; bug `fb1707c` |
| Reindexar el frontend del arnés | hecho: todos los índices son nuevos, de los dos binarios |
| Reevaluar `w` y `related` con corte por idioma | hecho (Paso 3b): los dos siguen apagados |
| TASK-013 m2: pares `owners × reach` en downstream | no medido |

### Hallazgos

1. **Bug (arreglado, `fb1707c`)**: `graph_edges` guardaba las llamadas de Rust por ruta con `::` y
   un 0.9.0 tras un downgrade perdía a esos llamadores.
2. **`calls` sin decidir en Rust 16,0 %** (meta ≤ 15 %): cadenas tras una llamada externa a un
   nombre que el repo también define y receptores sin tipo — sin decidir por diseño (TASK-005/007).
   Bajarlo es una decisión de diseño (marcar más cadenas como externas arriesga falsos negativos en
   `impact`), no un bug: pendiente de decisión.
3. **Relevancia del brief**: +10,0 pp en índice nuevo, +6,7 pp en índice actualizado; la diferencia
   es un caso de DevCtxEngine (D6) que depende del ruido de los vectores, no del ranking.
4. **Los vectores de un mismo texto difieren entre corridas de indexado** (coseno 0,957-1,000 para
   chunks de `content_hash` idéntico, mismo modelo y mismo motor —`Cargo.lock` sin cambios—):
   dependen de con qué otros textos entran al lote (probable cuantización dinámica del ONNX
   `quint8` por lote; hipótesis no verificada). Comparar dos índices "desde cero" mezcla ese ruido
   con el del código; el reuso de vectores de TASK-002 lo evita en los upgrades. Explica la caída
   del caso 2 de backend-b en el híbrido del índice nuevo (1 → 7-8) que no aparece en el actualizado.
5. **El upgrade en sitio crece la base +52-58 % en bloques usados** (backend-a 504 → 786, frontend
   293 → 463; fichero 153 → 296 MiB y 83 → 171 MiB), contra +9-13 % desde cero: bloques que DuckDB
   no reutiliza (lo que DD-21 dejó "vigilado"). No es una fila de §6 (que mide desde cero), pero
   es lo que verá cada usuario que actualice.
6. **El reuso de vectores exige la misma ruta**: el registro del índice va por `repo_path`; una
   copia del repo (con su `.devctx`) en otra carpeta re-embebe todo (medido al primer intento con
   una copia). Se suma al pendiente post-009 de `project.path` absoluto.
7. **VmHWM de backend-a +237 MiB** (797 → 1 034; upgrade 1 083): fuera de la fila (que es de
   frontend), por encima de los +200 MB del presupuesto si se aplicara a ese repo.
8. **`mcp.version` del candidato dice `0.9.0`**: subir la versión antes del tag.
9. La métrica `from_test_coherente` del arnés compara `from_test` con `is_test` del símbolo
   fuente; desde TASK-010 una ocurrencia dentro de `mod tests` es de test aunque el símbolo
   archivo no lo sea (234 filas `imports`/`contains` en DevCtxEngine): la métrica debería
   limitarse a `calls` o mirar el rango de test.
10. El chunker agrupa funciones chicas de padres distintos en un `grouped` (ya en 0.9.0); con los
    constructores como símbolos, un `grouped` de una interfaz anidada absorbe el constructor de la
    clase siguiente (el rango del `grouped` pasa el fin de la interfaz, en backend-b). Cosmético, anotado.

### Pendiente de decisión del usuario

- Fila "calls sin decidir ≤ 15 %" en Rust (16,0 %): aceptar, o abrir una task de diseño.
- Fila de relevancia del brief: cumple en índice nuevo (+10,0 pp) y no en actualizado (+6,7 pp).
- Fila de tipos de arista: 5 en el repo Python (sin `implements`; `Clase()` como `calls`).
- `related` y hop 2: siguen apagados; ¿retirar `graph_hops: 2`?
- El crecimiento de la base en el upgrade (hallazgo 5): aceptar para 0.10.0 o compactar.

- **Commits de esta task:** `46f5df1` (arnés: archivo en el código vs solo citado), `e91a8b9`
  (arnés: destino calificado por módulo por nombre y archivo — espera revisión), `b209747`
  (`.gitignore` de `__pycache__/`), `fb1707c` (bug de `graph_edges` con `::`, test
  `rust_path_calls_stay_findable_by_0_9_0`, que fallaba antes: el target guardado era
  `crate::util::helper`), y el de docs y este Resultado.
- **Archivos tocados:** `scripts/graph-eval/{score.py,run.sh,README.md}`, `.gitignore`,
  `crates/devctx-index/src/{pipeline.rs,lib.rs}`, `docs/07-performance.md`,
  `docs/es/07-rendimiento.md`, `docs/13-keeping-the-index-fresh.md`,
  `docs/es/13-mantener-el-indice-al-dia.md`, `CHANGELOG.md`, este archivo y el master (§5, §13).
- **Verificado por:** gate de `fb1707c`: `cargo fmt --all -- --check`; `cargo clippy --workspace
  --all-targets --locked -- -D warnings` con y sin `--features gpu`; `TMPDIR=<sandbox>/tmp cargo
  test --workspace --locked --no-fail-fast`: 1 249 pasan, 0 fallan, 14 ignorados. Arnés de punta a
  punta con los dos binarios. Barrido de privacidad antes de cada commit.
- **Desviaciones:** (1) relevancia sobre dos variantes de índice (nuevo y actualizado) por el
  hallazgo 4; (2) los 5 repos se midieron en sus commits de TASK-001 (DevCtxEngine `b009f80`),
  y el gold de Rust en `a6c39db` con `graph_bench` (sin modelo); (3) el arnés pequeño corrió
  mientras indexaban los repos grandes (carga anotada; solo afecta latencias de consulta, que se
  tomaron de corridas con la máquina tranquila); (4) el frontend se repitió (par en orden inverso)
  por la diferencia del primer par; (5) la latencia de `build_context` se midió por el MCP (stdio
  → serve), no por `POST /context`.
- **No verificado:** el cross-check de Windows/macOS (Paso 7: hay que empujar `fix/plan-009-grafo`
  y anotar el run de `cross-check`); `memories_by_symbol` de campo; DD-18 de grupo; los tiempos con
  la máquina libre (siempre hubo otras sesiones); el +159 ms de `edge_level` bajo carga alta; GPU.
- **Riesgos abiertos / siguiente:** revisión de `fb1707c` y `e91a8b9`; decisiones de arriba;
  push + cross-check; subir la versión a 0.10.0; tag y release (con el OK del usuario).
