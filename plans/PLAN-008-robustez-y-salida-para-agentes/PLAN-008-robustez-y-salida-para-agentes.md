# PLAN-008 — Robustez del ciclo de vida y salida útil para agentes

**Fecha:** 2026-10-03
**Fase:** 2 (Ejecución) — aprobado por el usuario el 2026-10-03. P0 en `feat/plan-008-p0`.
**Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), sale de `main` (v0.8.2 + `3069e51`)
**Origen:** prueba de campo de devctx 0.8.2 en el workspace REVFA (13 repos, `group: revfa`) y en
este mismo repo. Dos sesiones evaluaron la herramienta y llegaron a consenso sobre los bugs, sus
causas y el orden. Insumos: `research-state-of-art.md` y `devctx-graph-inventory.md` (scratchpad de
la sesión 695c60fd…, 2026-10-03).
**Release recomendado:** `0.8.3` con la parte P0 (TASK-001…009) apenas esté verde, y `0.9.0` con la
parte P1 (TASK-010…016). Ver §10.

---

## 1. Qué resuelve

Dos familias de problemas:

- **P0 — robustez.** Hoy un proceso `devctx mcp` viejo puede dejar **todas** las tools de índice
  inutilizables en un repo hasta que alguien lo mata a mano, un cliente queda pegado a un servidor
  muerto, un hook de git envenena el entorno del servidor, y varias tools contestan `[]` sin decir
  por qué. Son fallas silenciosas o auto-perpetuantes: el usuario no puede saber qué pasó.
- **P1 — salida para agentes.** Las tools devuelven demasiado (sin `limit` ni paginación), no
  aceptan `project` donde hace falta, y el ranking/armado de contexto mete ruido (tests, javadocs,
  chunks repetidos).

Lo que **no** resuelve: el grafo de código (tabla de símbolos, aristas tipadas, PageRank). Eso es
PLAN-009, que arranca después de este.

### 1.1 Números medidos en campo (2026-10-02/03, devctx 0.8.2)

| Síntoma | Medido |
|---|---|
| Procesos `devctx mcp` viejos (binario `(deleted)`) reteniendo el DuckDB de un proyecto | 3, sobre `REVFA_BackEnd`, `REVFA_Calidad_MicroService`, `revfa_plantillas` → `search`, `read_symbol`, `get_references`, `impact_analysis`, `search_routes`, `remember` fallan en los tres |
| Espera de `remote::ensure` antes de rendirse | 60 s fijos (200 × 300 ms), aunque el hijo murió en el primer segundo |
| `devctx search` CLI contra un DB bloqueado | 63 s hasta fallar, sin decir qué PID lo tiene |
| `read_symbol("AuthService")`, `get_references("authHttpInterceptor")`, `search_routes` en REVFA_FrontEnd sobre `feat/plan-124-…` | `[]` sin aviso, mientras `search` sí devuelve resultados |
| `recall` MCP con `scope: all`/`local` en el grupo revfa | falla: `None of the 13 repositories … First failure: No such file or directory (os error 2)`, sin ruta. CLI desde el repo devuelve 4 memorias; MCP global/group, 1 |
| `search` "how are memories linked to symbols" (DevCtxEngine) | el mismo chunk 3-5 veces |
| `build_context` en sesión de grupo para una tarea de FrontEnd | usa el repo default (`tickets-srv`), trunca 5 memorias ("exceeded its share of the output budget"), prioriza javadocs largos |
| `search_routes` sin `path` | 50 rutas, ~6k tokens |
| `plan_status` sin filtro en `~/revfa` | 130 planes, ~11k tokens |
| `memories_by_symbol`/`memories_by_file` | ~2.5k tokens por memoria (contenido completo) |
| `REVFA_REGISTRO_EXTERIOR` indexado 2026-08-19, antes del normalizador de receptores (`9ef3f4d`, 2026-08-24) | `devctx status` dice `up_to_date: true` y el índice tiene nodos basura de Mutiny |
| Suite de tests | deja vivo un `target/debug/devctx serve --idle 900` con cwd en un tmp borrado |

## 2. Bugs: reproducción y causa raíz (verificado en `main`, 2026-10-03)

Todas las referencias `archivo:línea` se re-verificaron contra el árbol actual. Las que cambiaron
respecto del informe de campo están marcadas *(movida)*.

### B1 (alta) — el MCP retiene el DuckDB del proyecto

**Repro.** Abrir una sesión MCP en un repo; reinstalar el binario (`install -m755 …`); en otra
terminal `devctx serve --stop` o esperar el `--idle 900`. La próxima tool de la sesión vieja que
necesite backend cae a `Backend::Local`. Desde ese momento cualquier `devctx search`/MCP nuevo en
ese repo falla con `Could not set lock … held by …/devctx (deleted) PID N`.

**Causa.**
- `crates/devctx-cli/src/main.rs:1563-1577` (`mcp_backend`): si `remote::ensure` devuelve `None`,
  llama `remote::reclaim_db` (**mata** el serve anunciado, `remote.rs:197-221`) y luego
  `devctx_mcp::backend_for(cfg, None)` → `Backend::Local(AppState::build)` (`lib.rs:1266-1281`).
- `AppState::build` abre `Store::open` (`state.rs:205`) y lo guarda en
  `primary: Arc<Mutex<Store>>` (`state.rs:167-171`) **por toda la vida del proceso**; no hay camino
  de liberación.
- El backend queda cacheado en `Binding::Project` (`lib.rs:376`), `Binding::Group.default`
  (`lib.rs:384`), en `hinted` (`lib.rs:410`, poblado en `lib.rs:492-512`) y vía `use_project`
  (`lib.rs:904`).
- `remote::ensure` (`remote.rs:129-148`) espera 60 s fijos y **no detecta la muerte del hijo**
  (el cliente central sí: `crates/devctx-central/src/client.rs:148-156`, con `serve.log`); el
  stderr del serve va a `/dev/null` (`remote.rs:157-159`).
- **Nuevo hallazgo:** `spawn_server` usa `std::env::current_exe()` (`remote.rs:153`). En Linux, con
  el binario reemplazado, eso devuelve `"/home/…/devctx (deleted)"` y el `spawn` falla con ENOENT:
  **un MCP viejo nunca puede levantar un serve** → cae directo a `Local`. Es el disparador más
  probable en campo.
- **Nuevo hallazgo:** `cmd_serve` escribe `serve.json` **antes** de abrir el store
  (`main.rs:1253` vs `run_blocking` en `main.rs:1258`) y hace `reclaim_db` al arrancar
  (`main.rs:1247`): un serve auto-lanzado mata a un serve sano que solo tardó >400 ms en contestar
  `/health` (`discover`, `remote.rs:259-279`), p. ej. porque estaba indexando.
- Auto-perpetuante: cada serve nuevo muere en `AppState::build` (lock del MCP) y borra su
  `serve.json` (`main.rs:1261`).

### B1c (media) — `devctx search` contra un DB bloqueado tarda 63 s en fallar

Mismo camino: `ensure` lanza un serve que muere al instante, espera 60 s, y el fallback local
falla con el error de lock. Debe fallar en ~1-2 s nombrando el PID que retiene el archivo y cómo
liberarlo (`devctx serve --stop`, o cerrar la sesión MCP del PID N).

### B2 (alta) — cliente pegado a un serve muerto

**Repro.** Sesión MCP enrutada; `devctx serve --stop` (o idle 900 s); la próxima tool falla con
un error de transporte para siempre.

**Causa.** `RemoteClient { base, token }` inmutable (`backend.rs:27-30`), creado una sola vez
(`Backend::remote`, `backend.rs:138`, desde `lib.rs:1266-1281`). `read()` (`backend.rs:97-115`)
convierte el error de transporte en `String` sin revalidar ni reconectar.

### B3 (alta) — el serve hereda `GIT_DIR`/`GIT_INDEX_FILE` de un hook

**Repro.** Commit en un worktree enlazado con hooks de devctx instalados; borrar el worktree; toda
tool que llame git en ese repo falla con `not a git repository: …/.git/worktrees/DevCtxEngine-postponed`
(confirmado en `/proc/<pid>/environ` del serve).

**Causa.** El bloque que escribe el hook (`crates/devctx-cli/src/hooks.rs:63-76`, `block`) corre
`("<exe>" index >/dev/null 2>&1 &)` con el entorno que git exporta a los hooks (`GIT_DIR`,
`GIT_INDEX_FILE`, `GIT_AUTHOR_*`, …). `devctx index` auto-lanza un serve (`spawn_server`,
`remote.rs:152-168`) que los hereda. Todos los `git` internos los respetan:
`crates/devctx-index/src/git.rs:223` (`run`, usado por `GitRepo::open` en `git.rs:43`),
`crates/devctx-core/src/config.rs:453,481`, `crates/devctx-index/src/lib.rs:53`,
`crates/devctx-cli/src/main.rs:3141`, `crates/devctx-api/src/central.rs:188`,
`crates/devctx-cli/src/hooks.rs:37`.

Agravante: `config.rs:440-467` (`main_worktree`) resuelve un worktree enlazado al proyecto del
worktree principal; con `GIT_DIR` heredado, `git -C <principal>` mezcla el `HEAD` del worktree con
los archivos del principal.

### B4 (alta, silencioso) — el grafo filtra por la rama actual sin fallback

**Repro.** REVFA_FrontEnd en `feat/plan-124-…` (no está en `indexing.branches`, sin filas):
`read_symbol`, `get_references`, `search_routes` → `[]`; `search` → resultados.

**Causa.** `search` usa `search_branch` (`state.rs:485-506`): rama actual si tiene filas, si no la
default, si no sin filtro. Las tools de grafo usan `state.repo_branch()` (`state.rs:352`) a secas:
`do_impact` (`state.rs:2894`), `do_read_symbol` (`state.rs:3120`), `do_references`
(`state.rs:3396`), `do_search_routes` (`state.rs:3426`), `do_routes_for_handler`
(`state.rs:3440`). `do_index_status` (`state.rs:704-728`) devuelve `indexed:false` y nadie más lo
dice.

### B5 (media) — `recall` MCP con `scope: all`/`local` falla sin ruta

**Causa (hipótesis fuerte, a confirmar con el repro de TASK-008).** `recall_one_local`
(`state.rs:1923`) re-ejecuta el binario con `std::env::current_exe()` (`state.rs:1928`); en un MCP
cuyo binario fue reemplazado eso es `"…/devctx (deleted)"` → ENOENT = `No such file or directory
(os error 2)`. Explica que la CLI recién lanzada funcione y el MCP viejo no. El mismo patrón está en
`search_one` (`state.rs:1691`), `do_search_project` (`state.rs:2079`) y `move_to_project`
(`state.rs:2817`). El mensaje agregado (`state.rs:1905-1919`) no incluye la ruta ni el comando.

### B6 (media) — chunks repetidos y anclaje por identificador exacto

`devctx_search::reciprocal_rank_fusion` deduplica solo por id de punto
(`crates/devctx-search/src/lib.rs:121-122`); el mismo `(file, start_line, end_line)` presente en
varias ramas sale N veces cuando `search_branch` cae a "sin filtro" (hipótesis a confirmar). El
modo keyword con `do_memories_by_symbol` no ancla la definición real (`state.rs:3147`).

### B7 (media) — `build_context` sin `project`, ranking pobre, corte temprano

`do_build_context` (`state.rs:2972-3093`) *(movida, el informe decía ~3030 para la búsqueda)*:
no recibe `project` (`BuildContextReq`, `lib.rs:232-239`); en grupo usa el miembro default;
`do_search(state, query, 30, None, SearchMode::Vector, false)` (`state.rs:3023`) solo vectorial
sin rerank; corta en el primer chunk que no cabe (`break`, `state.rs:3044`); las memorias de
`do_recall_scoped` llegan ya truncadas con "exceeded its share of the output budget"
(`state.rs:2493`, `2506`, `2564`) — esos son los 5 truncados observados.

### B8 (media) — índice viejo reportado como al día

`up_to_date` compara solo commits (`state.rs:725`). Un índice generado por un extractor anterior
(queries de `languages/*.json` o lógica de `devctx-parse` distintas) se reporta al día. **No es un
bug del extractor actual** (genera 0 targets basura); es falta de versión del índice. No existe
tabla de metadatos en `crates/devctx-store/src/schema.rs` (tablas en `:63-238`).

### B9 (baja) — la suite deja un serve vivo

`Tmp::drop` (`crates/devctx-cli/tests/mcp_binding.rs:93-132`) recorre dos niveles y llama
`serve --stop`; en `local_scope_refuses_to_guess_a_repository` (`localguard-solo`,
`mcp_binding.rs:700`) quedó un serve vivo con cwd borrado. Causa no confirmada; hipótesis: el
serve aún no escribió `serve.json` cuando corre `Drop` (escribe antes de abrir el store pero
después de `reclaim_db`), o `stop_server` lo borra mientras el proceso sigue. El serve no tiene
ninguna regla de "mi proyecto desapareció → salgo".

### D2 — ciclo de vida del MCP

- **EOF de stdin:** `serve_stdio_bound` (`lib.rs:1296-1302`) espera `service.waiting()`; rmcp
  termina el servicio al cerrar stdin, pero `run_stdio_bound` (`lib.rs:1314-1319`) suelta un runtime
  multi-thread cuyo `Drop` **espera las tareas `spawn_blocking`** en curso (un `index` o un `recall`
  federado). No hay test que fije que el proceso sale. *A verificar en TASK-003.*
- **Binario cambiado:** no hay detección (el único uso de versión es `Implementation::new(…,
  CARGO_PKG_VERSION)`, `lib.rs:1166`). Decidido: **no** salir; reportarlo.

## 3. Decisiones (acordadas entre las dos sesiones)

### D1 — El MCP nunca tiene `Backend::Local`

- El MCP solo enruta. `backend_for` del MCP devuelve un backend remoto **perezoso**: guarda el
  `ProjectConfig` y una closure de conexión (`remote::ensure`); cada llamada usa la conexión viva o
  reintenta `ensure`, y si no hay servidor devuelve un error explícito con la causa (del
  `serve.log`) en vez de abrir el DB.
- Verificado que es viable: los 24 métodos de `Backend` en `backend.rs` tienen brazo `Remote`, y
  `plan_status` en modo grupo/sin binding ya corre en proceso sin `Store` (PLAN-007 DD-4).
- `reclaim_db` sale del camino del MCP. Sigue en TUI/`serve`/`repair`/`web`, que son dueños
  interactivos y explícitos.
- `ensure` detecta la muerte del hijo y lee la causa de un `serve.log` del proyecto (mismo patrón
  que `devctx-central/src/client.rs`). `discover` tolera un serve ocupado: PID vivo + `serve.json`
  presente = vivo aunque `/health` tarde; nunca se lo mata por lento.
- El binario propio se resuelve con un helper que entiende el sufijo ` (deleted)`.

Alternativa rechazada: liberar el `Store` del `AppState` tras N segundos de inactividad. Mantiene
dos dueños posibles del archivo y la carrera vuelve en cuanto el MCP reabre.

### D2 — Ciclo de vida del MCP

Sale en EOF de stdin (con tope de espera para tareas bloqueantes y test que lo fije). **No** sale
cuando cambia su binario: `index_status` y `list_projects` reportan `mcp_version` vs versión del
binario instalado y sugieren reiniciar la sesión.

### D3 — Ruido en el ranking: penalizar, no excluir

- Penalización (no exclusión) en la RRF para tests/specs/docs/SQL/README.
- Filtro duro explícito: `include_tests: bool` y `kind: code|test|doc|config` en `search` y
  `build_context`.
- Exclusión dura solo para vendor/generado. **Verificado:** hoy no hay ningún default —
  `Indexing.exclude` es `Vec` vacío por defecto (`crates/devctx-core/src/config.rs:176-187`) y el
  índice confía en `git ls-files` + `--exclude-standard` (`crates/devctx-index/src/git.rs:85-99`).
  Un `dist/` o `node_modules/` **versionado** entra al índice. Se agrega una lista por defecto
  (`node_modules/`, `target/`, `dist/`, `build/`, `vendor/`, `*.min.js`, `*.generated.*`) que se
  suma a la del usuario y se puede desactivar.

## 4. Fuera de alcance

- Todo el trabajo de grafo (símbolos con ID, aristas tipadas, confianza, PageRank, `traverse`,
  skeleton): PLAN-009.
- Migraciones de tablas existentes. B8 agrega una tabla nueva `index_meta(key, value)` con
  `CREATE TABLE IF NOT EXISTS`; ninguna tabla existente cambia.
- Auto-reinicio del MCP cuando cambia el binario (D2: solo se reporta).
- Reindexar automáticamente un índice viejo en segundo plano (B8: se avisa; `index` lo hace si se
  pide).
- Cambiar el modelo de embeddings o el reranker.
- Tocar archivos de `/home/snaven10/revfa` (la verificación es solo lectura, salvo reindexar
  `REVFA_REGISTRO_EXTERIOR` con aprobación).

## 5. Tasks y orden

| TASK | Título | Prioridad | Depende de | Estado |
|------|--------|-----------|------------|--------|
| TASK-001 | El MCP nunca abre el DuckDB: backend remoto perezoso, `ensure` que detecta muerte, `serve.log`, exe `(deleted)` | P0 | — | `done` |
| TASK-002 | CLI: fallo rápido ante un DB bloqueado, con PID y cómo liberarlo | P0 | TASK-001 | `done` |
| TASK-003 | Ciclo de vida del MCP: salida en EOF y aviso de versión desfasada | P0 | — | `done` |
| TASK-004 | Reconexión del cliente remoto ante un serve muerto | P0 | TASK-001 | `done` |
| TASK-005 | Entorno de git limpio en hooks, serve y subprocesos `git` | P0 | — | `done` |
| TASK-006 | Grafo honesto: fallback de rama como `search` y avisos explícitos | P0 | — | `done` |
| TASK-007 | Versión del extractor en `index_meta` y aviso de índice viejo | P0 | — | `done` |
| TASK-008 | `recall` federado: exe propio robusto y errores con ruta | P0 | TASK-001 | `done` |
| TASK-009 | Higiene de tests: ningún serve sobrevive a la suite (+ B10 SIGTERM, B11 idle) | P0 | TASK-001 | `done` |
| TASK-010 | `project`, `limit`, paginación y conteo de omitidos en las tools | P1 | TASK-004 | `done` |
| TASK-011 | Ranking: penalización de tests/docs, `kind`/`include_tests`, excludes por defecto | P1 | — | `done` |
| TASK-012 | Dedup de chunks y anclaje por identificador exacto | P1 | TASK-006 | `done` |
| TASK-013 | `build_context`: `project`, selección en grupo y presupuesto por relevancia | P1 | TASK-010, TASK-011, TASK-012 | `done` |
| TASK-014 | `link_sources` correcto y sugerencias en `read_symbol` | P1 | TASK-006 | `done` |
| TASK-015 | Docs EN + ES | P1 | TASK-013, TASK-014, TASK-007, TASK-005 | `done` |
| TASK-016 | Verificación de campo en `~/revfa` y este repo, antes/después | P1 | TASK-015, TASK-017 | `pending` |
| TASK-017 | Pendientes del review de P0: idle exit, `in_tx`, procown fuera de Linux, transacciones y tests | P1 | — | `done` |

(La columna `Estado` va última: el parser de `plan_status` lee la columna con encabezado `Estado`.)

**Paralelizables:**
- TASK-001, TASK-003, TASK-005, TASK-006, TASK-007 y TASK-011 arrancan juntas: tocan zonas
  distintas (`remote.rs`/`lib.rs` del MCP · fin de `lib.rs` y `do_index_status` · `hooks.rs` y los
  `Command::new("git")` · tools de grafo en `state.rs` · `schema.rs`/pipeline · `devctx-search` y
  `config.rs`). TASK-006 y TASK-007 tocan ambas `do_index_status`: la segunda en entrar rebasea.
- Tras TASK-001: TASK-002, TASK-004, TASK-008 y TASK-009.
- TASK-010 espera a TASK-004 porque las dos cambian firmas de `Backend` en `backend.rs`; no es
  dependencia lógica sino de conflicto.

## 6. Criterios de aceptación globales (medidos en TASK-016)

| Criterio | Antes | Meta |
|---|---|---|
| Con un `devctx mcp` viejo vivo (binario reemplazado), otro proceso obtiene error de lock | sí, en 3 repos | **nunca** |
| Tiempo hasta error cuando el serve no puede arrancar (lock ajeno) | 60-63 s | ≤ 2 s, con causa y PID |
| Tool tras `devctx serve --stop` en mitad de la sesión | error permanente | OK con PID nuevo, incluido `remember` |
| Serve hijo de un hook con `GIT_DIR` de un worktree borrado | rompe git en el repo | no hereda `GIT_*` |
| `read_symbol`/`get_references`/`search_routes` en rama no indexada | `[]` silencioso | resultados de la rama de fallback + campo `branch_fallback` |
| `status` sobre un índice de extractor viejo | `up_to_date: true` | `extractor_stale: true` + aviso |
| `recall scope:all` en revfa desde MCP | falla | devuelve las del CLI (≥ 4 para la consulta del informe) |
| `search` "how are memories linked to symbols" | mismo chunk 3-5× | 0 duplicados `(file, start, end)` |
| `search_routes` sin `path` | ~6k tokens | ≤ 1.5k con `limit` default + `omitted` |
| `plan_status` sin filtro en revfa | ~11k tokens | ≤ 3k con `active_only`/`limit` + `omitted` |
| `build_context` en grupo sin `project` | default silencioso | repo por mejor score, nombrado, o error que pide `project` |
| Suite de tests | deja un serve vivo | 0 procesos `devctx serve` con cwd en tmp tras la suite |

## 7. Riesgos

- **Sin fallback local el MCP depende del serve.** Si el serve no puede arrancar (modelo roto,
  disco lleno) las tools fallan donde antes "funcionaban" abriendo el DB. Se acepta: ese
  "funcionar" es lo que bloquea a todos los demás procesos. Mitigación: el error trae la causa del
  `serve.log` y la acción concreta.
- **Reintentos que duplican escrituras.** Reintentar `remember` tras un error de conexión podría
  guardar dos veces si el request llegó. Mitigación: solo se reintenta en fallo **antes** de enviar
  (connection refused / connect error), nunca en timeout de lectura ni con status HTTP.
- **Excludes por defecto** pueden sacar del índice algo que un repo sí quiere (`build/` como
  paquete fuente). Mitigación: opt-out explícito en config y se documenta; el cambio se nota en el
  siguiente `index` (archivos podados en el resumen).
- **Penalización de tests** puede empeorar búsquedas que buscan tests. Mitigación: `kind: test` /
  `include_tests` y la penalización es un factor, no un filtro.
- **`index_meta` nuevo en DBs existentes.** Ausente = "versión desconocida" → aviso de reindexar
  una vez. Molesto pero honesto; se documenta.
- **Procesos viejos en la máquina.** Los MCP 0.8.2 vivos siguen reteniendo locks hasta que se
  cierren sus sesiones: el arreglo protege a partir de la próxima sesión. TASK-016 lo dice.
- **Cambio de contrato JSON** (campos nuevos `branch_fallback`, `extractor_stale`, `omitted`,
  `next_offset`, `mcp_version`): aditivo. El hook `~/.claude/hooks/devctx-session-start.sh` (fuera
  del repo) consume `plan_status`; `active_only` es opt-in, no cambia el default.

## 8. Verificación

- `cargo test -p devctx-mcp`, `-p devctx-cli --test mcp_binding --test mcp_tools --test
  plan_status_cli`, `-p devctx-search`, `-p devctx-store`, `-p devctx-index` — cada task agrega los
  suyos (detalle en cada TASK).
- Tests de integración acordados (en `crates/devctx-cli/tests/mcp_binding.rs`):
  1. puerto del proyecto tomado por un listener mudo → la tool falla con error explícito y, después,
     el test puede abrir el DB (nadie lo retiene) — TASK-001;
  2. lock ajeno (serve lanzado a mano sin `serve.json`) → `ensure` vuelve en < 2 s con la causa —
     TASK-001/TASK-002;
  3. `serve --stop` entre dos tools → `remember` OK con PID nuevo — TASK-004;
  4. tests unitarios de la política de reintento — TASK-004;
  5. `devctx index` con `GIT_DIR`/`GIT_INDEX_FILE` a un dir inexistente → el serve hijo no los
     hereda — TASK-005.
- Campo: TASK-016 reproduce cada bug con el binario 0.8.2 y lo re-mide con el nuevo.
- `devctx plan-status PLAN-008` desde la raíz de este repo: 0 warnings (verificado al escribir).

Nada de esto se compiló ni se testeó: es un plan.

## 9. Rollback

Cada task es un commit convencional propio y revertible por separado. TASK-001 es la única con
cambio de comportamiento grande (sin fallback local); revertirla devuelve el comportamiento 0.8.2
sin tocar datos. TASK-007 crea `index_meta`: revertir deja la tabla huérfana e inocua (nadie la
lee); no hace falta borrarla. TASK-011 cambia el set indexado (excludes): revertir y `devctx index`
vuelve a incluir lo excluido. Ninguna task migra ni reescribe datos existentes.

## 10. Release

**Recomendación: dos releases.**
- `0.8.3` = P0 (TASK-001…009). Son arreglos; lo único "nuevo" es la tabla aditiva `index_meta` y
  campos JSON aditivos. Urge: hoy tres repos de revfa tienen todas las tools de índice caídas.
- `0.9.0` = P1 (TASK-010…016). Parámetros nuevos en tools, cambios de ranking que alteran
  resultados y excludes por defecto que cambian el set indexado: en 0.x eso es minor.

Alternativa: un solo `0.9.0` al final. Más simple, pero deja el P0 esperando al P1. Ver Q-1.

## 11. Contrato de resultado (lo que cada task reporta al cerrar)

Cada TASK llena su `## Resultado` con:

1. **Estado final** (`done` / `blocked` / `skipped`) y una línea de por qué si no es `done`.
2. **Repro antes/después**: el comando o test exacto que reproducía el bug y su salida antes y
   después (o "sin repro manual; cubierto por `<test>`").
3. **Causa raíz confirmada** o corregida respecto de §2 (sobre todo B5, B6, B9, que son hipótesis).
4. **Archivos tocados** y símbolos públicos nuevos o cambiados (firma).
5. **Tests**: nombres de los tests nuevos y comando con el que se corrieron, con el resultado.
6. **Contrato JSON**: campos agregados/cambiados en la salida de cada tool.
7. **Lo que NO se verificó**, explícito.
8. **Números** cuando el criterio es medible (tiempos, tokens, conteos).

## 12. Decisiones del usuario (2026-10-03) — plan APROBADO

- **Q-1 → dos releases.** 0.8.3 = P0 (TASK-001…009); 0.9.0 = P1 (TASK-010…016).
- **Q-2 → se acepta y se documenta.** Un commit desde un worktree enlazado indexa el `HEAD` del
  worktree principal; el commit del worktree entra al índice cuando llega a una rama trackeada.
- **Q-3 → sí, con opt-out (2026-10-04).** `build/` entra en los excludes por defecto junto con
  `node_modules`, `target` y `dist`; una opción de config permite volver a indexarlo.
  - **Precisión de Q-3 (2026-10-05).** `build/` y `target/` se anclan a la **raíz del repo**
    (`/build/`, `/target/`), no a cualquier profundidad: `src/x/build/Builder.java` o un paquete
    Java `target` son código. El primer reconcile registra cuántos archivos excluye cada patrón.
    `node_modules/`, `dist/`, `vendor/`, `third_party/`, `bower_components/` siguen a cualquier
    profundidad: en monorepos cada paquete emite su `dist/` y nadie llama `dist` a un directorio
    de fuentes. Quien quiera `target/` a cualquier profundidad (Maven multi-módulo con `target/`
    trackeado) lo añade en `indexing.exclude: ["target/"]`; quien quiera indexar el `/target/`
    raíz lo reincluye con `indexing.exclude: ["!/target/"]`.
- **Q-4 → OK.** `search_routes` 20, `plan_status` 25 planes, `memories_by_*` 5 recortadas a 600
  caracteres.
- Ejecución de P0 en la rama `feat/plan-008-p0`, con compilación y tests autorizados; la sesión
  `debug-devctx-mcp-process` revisa cada lote y hace la verificación de campo antes de 0.8.3.
- **P0 publicado como 0.8.4 (2026-10-04).** 0.8.3 quedó como tag sin release: el build de Windows
  falló (`errno_result` de procown sin `cfg(unix)`); se corrigió como fix-forward.
- **P1 aprobado para ejecución (2026-10-04)** en la rama `feat/plan-008-p1`, con compilación y
  tests autorizados, release 0.9.0. El usuario sumó **TASK-017** con los pendientes del review de P0.

## Pendientes para P1 (surgidos en la revisión final de P0)

- **m-2 (macOS):** `Handle::wait_exit` fuera de Linux sondea `!owns()` cada 50 ms; debería usar
  kqueue (`EVFILT_PROC` / `NOTE_EXIT`) para esperar la salida real, como el pidfd en Linux.
- **m-4:** la guardia de la caché de modelos solo comprueba `resolve_model_cache`; no cubre tests
  fuera de `devctx-cli/tests` (p. ej. el test `#[ignore]` de minilm usa el `model_cache_dir()` real),
  ni un `CARGO_TARGET_DIR` relativo, ni `build.target-dir` de `.cargo/config`.
- **D1b:** no hay test e2e de `serve --stop` contra un checkpoint final real de más de 6 s (el
  camino paciente de `terminate_patient` solo se prueba con procesos de mentira).
- **Idle exit con el listener abierto:** el idle watchdog del serve y del central congela y hace
  checkpoint mientras axum todavía acepta requests. `e4f0cc9` cerró la pérdida de datos (upserts
  atómicos y detrás del gate), pero la ventana sigue: cerrar el listener o responder 503 antes del
  freeze la elimina de raíz.
- **`in_tx` tras un panic:** si `f` hace panic dentro de `Store::in_transaction`, `in_tx` queda en
  `true` y las transacciones siguientes de esa conexión corren sin `BEGIN` propio. Resetearlo con un
  guard.
- **`ps_command_is_server` fuera de Linux** acepta `/usr/bin/vim /x/devctx serve` (un token
  `devctx*` en cualquier posición de los argumentos, no solo como argv0).
- **Borrado de rama en `memory_refs.rs` (~430)** sin transacción: un corte a mitad deja la rama a
  medio borrar (se repara en la siguiente poda).
- **Test `devctx_index_fails_when_the_server_cancels_its_run`** usa `wait_with_output()` sin timeout:
  si el CLI se cuelga, se cuelga la suite.
- **`init` interactivo con granite + offline** guarda un `model_dir` vacío en la config.

## 13. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
