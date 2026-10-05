# TASK-013 — `build_context`: `project`, selección en grupo y presupuesto por relevancia

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet; opus si la selección en grupo se complica)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-010, TASK-011, TASK-012
- **Estado:** `done`

---

## Objetivo

Que `build_context` arme contexto del repo correcto y con el código antes que la prosa, sin cortar
en el primer chunk grande ni traer memorias truncadas (PLAN-008 B7).

## Contexto verificado

- `crates/devctx-mcp/src/lib.rs:232-239` — `BuildContextReq { query, max_tokens, include_memories }`,
  sin `project`; tool en `lib.rs:1018`.
- `crates/devctx-mcp/src/state.rs:2972-3093` — `do_build_context`: recall de 5 memorias en todos los
  tiers (`:2996`), `do_search(…, 30, None, SearchMode::Vector, false)` (`:3023`), corte con `break`
  en el primer chunk que no cabe (`:3044`), memorias por archivo de los 5 primeros archivos.
- Truncados: `do_recall_scoped` recorta memorias con "exceeded its share of the output budget"
  (`state.rs:2493`, `:2506`, `:2564`) antes de que `build_context` las vea.
- Grupo: `DevctxServer::backend_for` (`lib.rs:492-512`) devuelve `Binding::Group.default` sin hint.
- Campo: sesión de grupo revfa → tarea de FrontEnd respondida desde `tickets-srv`.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`,
  `crates/devctx-mcp/src/backend.rs`, `crates/devctx-api/src/lib.rs`

## Pasos

- [x] **Paso 1 — `project`.** Parámetro `project` como en las demás tools (acepta un repo distinto del
      bindeado en sesiones de grupo).
- [x] **Paso 2 — Grupo sin `project`.** Correr la búsqueda en cada miembro (reusar la federación de
      `search` en grupo) y elegir el repo con mejor score top-k; nombrarlo en la salida
      (`[devctx] context from <repo> (best match among N members)`). Si el margen entre el primero y
      el segundo es chico, devolver error pidiendo `project` con los candidatos. Nunca default
      silencioso.
- [x] **Paso 3 — Ranking.** Búsqueda `hybrid` con las penalizaciones de TASK-011 y dedup de TASK-012;
      `kind`/`include_tests` pasan derecho.
- [x] **Paso 4 — Presupuesto.** Reemplazar `break` por "saltar y seguir" (un chunk grande no impide
      los siguientes que sí caben); dentro de un chunk de código, recortar comentarios de
      documentación iniciales largos antes que el cuerpo; memorias como título + primeras líneas con
      `id` (sin la marca de truncado por cada una) y una sola línea final con el conteo de omitidos.

## Criterios de aceptación

- [x] Test: grupo de 2 miembros, consulta que solo matchea en el no-default → contexto del correcto,
      nombrado.
- [x] Test: consulta ambigua entre miembros → error que lista candidatos y pide `project`.
- [x] Test: primer hit más grande que el presupuesto → los siguientes que caben aparecen.
- [x] Test: ningún "exceeded its share" en la salida de `build_context`; sí un conteo final.
- [ ] En el Resultado: la consulta de campo (FrontEnd en revfa) antes/después. -> diferido a TASK-016 (medición de campo).

## Riesgos

La selección en grupo cuesta N búsquedas (una por miembro): mismo costo que `search` en grupo hoy;
medirlo y reportarlo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después:** antes, en sesión de grupo `build_context` usaba `Binding::Group.default` (sin `project`), buscaba en `Vector` con `do_search` (que recorta cada fila a 1/30 del presupuesto con "exceeded its share") y cortaba con `break` en el primer chunk que no cabía. Ahora: ver tests; la consulta de campo FrontEnd/revfa queda para TASK-016.
3. **Causa raíz:** confirmada. Además: `do_recall_scoped` y `do_search` presupuestaban por su cuenta (marca "truncated" por ítem) antes de que `build_context` viera los datos; ahora `build_context` toma los datos sin presupuestar (`recall_fused`, `search_items`) y reparte su propio `max_tokens`.
4. **Archivos:** `devctx-mcp/src/{state,lib,backend}.rs`, `devctx-api/src/lib.rs`, `devctx-cli/src/{remote,main}.rs`. Nuevos/cambiados: `do_build_context(state, query, max_tokens, include_memories, &KindSel)`, `Backend::build_context(.., &KindSel)`, `RemoteClient::build_context(.., &KindSel)`, privados `recall_fused`, `search_items`, `group_targets`, `fan_out_search` (extraídos de `do_recall_scoped`/`do_search`/`do_search_group`, sin duplicar), `compose_context`, `memory_brief`, `trim_leading_doc`, `cap_lines`, `member_score`, `choose_member`, pub `pick_group_member(members, query, &KindSel) -> Result<(String, usize), String>`; `DevctxServer::context_backend`.
5. **Tests** (`cargo test -p devctx-mcp --lib -- build_context group_pick`, 6/6 ok): `build_context_skips_a_chunk_that_does_not_fit_and_keeps_going` (en el padre el `break` perdía los chunks siguientes), `build_context_trims_a_long_leading_doc_before_the_body`, `build_context_memories_keep_to_a_share_and_never_stamp_truncation`, `group_pick_chooses_the_member_that_matches`, `group_pick_refuses_a_close_call_and_names_candidates`. En el padre las funciones no existen (no compilan); la conducta que fijan es la descrita en el punto 2. Gate completo: fmt, clippy (también `--features gpu`) y `cargo test --workspace`.
6. **Contrato:** `build_context` (MCP) acepta `project`, `kind`, `include_tests`; `POST /context` acepta `kind`, `include_tests`. Salida prosa: primera línea `[devctx] context from <repo>` **solo en sesión de grupo o cuando la llamada trae `project`** (en un proyecto bindeado sin `project` no hay línea; corregido en Fixup G); en grupo sin `project`, `<repo> (best match S; k of M members scored[; failed: …][; not scored: …])` y, si fue reñido, una segunda línea `[devctx] ambiguous: also …`; cierre `[devctx] omitted: N item(s), reason: budget (T tokens)...` (una sola vez); `[devctx] branch_fallback: ...` se mantiene. Memorias: `[memory] <id> — <título>` + primeras líneas (6 líneas / 500 chars), sin marca de truncado por ítem.
7. **No verificado:** consulta de campo en revfa (TASK-016); el margen de selección `PICK_MARGIN=0.03` sobre media top-3 de coseno es heurístico, sin calibrar en campo; no hay test de integración end-to-end del grupo (la selección se prueba sobre `choose_member`/`member_score`, la fan-out reusa `search_one` ya cubierto).
8. **Parámetros/números:** búsqueda `hybrid` limit 30, rerank off; memorias ≤35% del presupuesto; un chunk ≤ max(presupuesto/3, 600 chars); doc inicial >6 líneas se deja en 3 + "N doc lines trimmed"; selección: top-3 por miembro (vector), margen 0.03. Costo de selección: N búsquedas (una por miembro, lotes de 4) más la de contexto; igual que `search` en grupo; no medido en tiempo.

### Fixup F (review)

- Sin cambios en `build_context`. Efecto indirecto: `search_ranked` ya no multiplica scores por 0.6 sino que
  degrada por posiciones y deja los scores monótonos (`min(propio, anterior)`), así que `member_score` (media
  top-3) deja de castigar ×0.6 al miembro cuyo mejor hit es un doc/test/config; y el pool del reranker se corta a
  `max(pool, 2×limit)` también con filtro duro (TASK-011 Fixup F, I-1). Los tests de `build_context`/`group_pick`
  pasan sin cambios.

### Fixup G (review)

Contrato corregido (punto 6): la línea `[devctx] context from <repo>` aparece solo en grupo o con `project`.

- **1 — default silencioso por otra vía:** `choose_member(outcomes, default)` (`devctx-mcp/src/state.rs`) chequea
  `best ≤ 0` antes que cualquier caso de un solo miembro y cuenta TODOS los miembros: la etiqueta dice
  `k of M members scored; failed: X (motivo); not scored: Y (no running server | different model … | checkout
  missing)`, también en los errores. Ya no hay `r.ok()` ni `_skipped`/`_missing` descartados. Tests:
  `group_pick_accounts_for_every_member_and_never_picks_a_zero` (en el padre `[(only,_)]` devolvía el único con
  score 0 y "among 1"), `pick_group_member_scores_only_warm_members_and_names_the_rest` (e2e con servidores HTTP
  falsos: uno OK, uno débil, uno que responde 500, uno colgado, uno frío, uno de otro modelo).
- **2 — escalas:** (a) la selección pide `/search` con `rerank:false` (nunca logits, sin CPU del cross-encoder);
  (b) `ProjectRow.embed_model` (del registro) y `group_targets` excluye miembros de otro modelo aunque tengan la
  misma dimensión (`minilm-l6` y `ml-granite` son 384) → "not scored: different model". Test
  `group_targets_exclude_another_model_of_the_same_width`; (c) decidido y documentado en `group_targets`: misma
  dimensión + mismo modelo ⇒ cosenos del retriever comparables; logits, scores clampados, BM25 y RRF no. Borrado el
  doc que afirmaba lo contrario sobre `FANOUT_CONCURRENCY`.
- **Coordinador (raw_score y penalización entre miembros):** `SearchResult.raw_score: Option<f32>`
  (`devctx-core/src/types.rs`; `rewrite_score`/`retriever_score`) guarda el score del retriever cuando la
  penalización lo clampa o el reranker lo reemplaza; los hits JSON lo exponen (`raw_score`, solo si difiere) junto
  con `kind` y `language`; `devctx_core::hit_raw_score` lo lee. `do_search_group` (fusión por score o RRF) reordena
  cada miembro por `raw_score`, fusiona y aplica la penalización UNA vez sobre la lista fusionada
  (`fuse_member_hits` + `devctx_search::demoted_order`; penalty del config del primer miembro). `member_scores`
  devuelve `(code, all)`: media top-3 de `raw_score` sobre hits no penalizados, y sobre todos; decide `code`
  salvo que ningún miembro tenga código que matchee (pregunta de docs o `kind` explícito). Se piden 10 hits por
  miembro (`PICK_FETCH`). Tests: `member_scores_count_code_on_the_raw_score` (docs-site 0.82 vs api 0.75 → api),
  `group_fusion_demotes_once_over_the_merged_list`.
- **3 — costo:** la selección solo puntúa miembros con servidor YA vivo: lee su `serve.json` (al lado de la DB,
  según su config), `/health` con 0.8 s y `/search` por HTTP directo; nunca lanza procesos ni servers. Frío → "not
  scored (no running server)"; `/health` sin respuesta → failed. **Decisión:** si nadie pudo puntuarse y nada
  falló (todos fríos/otro modelo/ausentes), responde el miembro default del grupo con la etiqueta "the group's
  default member" y aviso `not chosen by relevance … pass project`; si hubo fallos, error con la lista. Caché por
  sesión (`DevctxServer.picks`) con clave query normalizada (minúsculas, espacios colapsados) + `kind` +
  `include_tests`, TTL 10 min, solo elecciones por relevancia; la etiqueta dice `cached choice`. Identificadores
  (`identifier_tokens`, 2 primeros): `/symbol/<id>?limit=1` en cada miembro puntuado; quien lo define suma
  `PICK_MARGIN` (rompe empates, no da vuelta una ventaja clara; la etiqueta dice ``defines `X` ``). Tests:
  `pick_group_member_with_every_member_cold_uses_the_default` (sin `serve.json` nuevo, < 2 s),
  `group_pick_falls_back_to_the_default_only_when_nothing_failed`,
  `group_pick_prefers_the_member_that_defines_the_symbol_on_a_tie`.
- **4 — usabilidad:** margen < `PICK_MARGIN` → responde igual desde el mejor con `[devctx] ambiguous: also X
  (0.49), Y (0.48) — within 0.03 of … pass project`; error solo si nada matcheó o nadie respondió. Test
  `group_pick_answers_a_close_call_with_a_warning`. Calibración del margen: TASK-016.
- **5:** `compose_context` agrega los `files` de una memoria a `memory_files` solo si la memoria entró. Test
  `a_memory_that_did_not_fit_does_not_hide_its_files_code` (falla en el padre).
- **6:** `memory_brief` corta por `char_indices` una primera línea > 500 chars. Test
  `a_memory_with_a_giant_first_line_keeps_its_start` (en el padre cuerpo vacío + "…").
- **7:** `cap_lines` corta por chars una línea única > cap (`… (line cut at N of M chars; K more lines)`). Test
  `a_single_giant_line_is_cut_by_chars`.
- **8/9:** se borró `is_doc_line`. `trim_leading_doc(text, CommentStyle)` solo recorta hits `kind: code` y solo un
  bloque que empieza el chunk y es comentario en su lenguaje: `/* … */` (conserva el `*/`, marcador ` * …` dentro
  del comentario), `//`/`///`/`//!` (marcador con el mismo prefijo), `#` solo en lenguajes de `#`, `--` en SQL/Lua/
  Haskell; nunca `#if`/`#pragma`/`#import`/`#[attr]`, ni `*p = …`; docstrings de Python se dejan enteros. El
  marcador de `cap_lines` usa el comentario del lenguaje (`//`, `#`, `--`, o ninguno en Markdown/JSON). Tests:
  `a_trimmed_doc_stays_well_formed_in_its_language`, `markdown_lists_and_headings_are_not_trimmed`.
- **10:** `search_items(.., build_fts)`: `build_context` no construye el índice BM25 si falta (hybrid degrada a
  vector + anclaje); `search` sí, como antes.
- **11:** en grupo, `group_context_target` rechaza un `project` que no resuelve o que resuelve a un proyecto que no
  es miembro (antes `resolve_hint` aceptaba cualquiera registrado). Test
  `group_context_target_rejects_unknown_and_foreign_projects`. `context_backend` delega en esa función pura.
- Nit: `is_anchored_definition` (extraído de `search_items`) y test
  `anchored_marks_only_the_identifiers_anchoring_looked_up` (el 4.º identificador no se marca).
- Algoritmo final de selección (grupo sin `project`): targets = presentes con mismo modelo+dimensión que la
  mayoría → por cada uno, en lotes de 4: `serve.json` + `/health` (0.8 s) → `/search` vector, sin rerank, 10
  hits, `kind`/`include_tests` → `(code, all)` sobre `raw_score` → +`PICK_MARGIN` si define un identificador de la
  query → mejor efectivo; `≤ 0` error; margen < 0.03 aviso; sin puntuados: default (si nada falló) o error.
- Gate: `cargo fmt --check`; `clippy --workspace --all-targets` con y sin `--features gpu`, 0 warnings;
  `TMPDIR=/var/tmp DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache cargo test --workspace` verde (694 tests,
  sin flakes); sin `target/debug/devctx serve` residuales.
- **No verificado:** latencia real en revfa (13 miembros) y el margen 0.03 sobre `code` (TASK-016); el caso
  "serve vivo pero modelo descargado de memoria" (la primera `/search` recarga el modelo, hasta 20 s de timeout).

### Fixup H (review)

- **I-1 — siempre el default, presentado "por relevancia" y cacheado:** `MemberOutcome` separa `Cold` (comparable
  sin servidor / `serve.json` rancio), `Busy` (no respondió a tiempo), `Failed` (respondió error) y `NotScored`
  (no comparable: otro modelo/dimensión, checkout ausente). Si quedan miembros comparables sin puntuar,
  `choose_member` responde desde el mejor puntuado con etiqueta `best match X among the k members that could be
  scored` y una línea `[devctx] compared only k of N comparable members: … could not be scored … Pass project`,
  ofreciendo los que la pregunta nombra por nombre o `description` del registro (`name_candidates`;
  `ProjectRow.description`). `by_relevance` = todos los comparables puntuados; `GroupPick::cacheable()` =
  `by_relevance && warning.is_none()` (ni parciales ni empates se cachean). Tests:
  `group_pick_accounts_for_every_member_and_never_picks_a_zero`, `a_pick_among_the_warm_few_warns_and_is_not_cached`
  (la sesión del review: solo el default caliente), `name_candidates_match_name_and_description_words`.
- **I-2 — latencia:** un deadline total de 2.5 s (`PICK_DEADLINE`) para toda la selección; un hilo por miembro
  (sin lotes en lockstep), resultados por canal con `recv_timeout(deadline + 150 ms)`; quien no respondió es `Busy`
  y su hilo termina solo (todos sus timeouts están acotados al mismo deadline). `/health` ≤ 0.8 s, `/symbol` ≤ 1 s
  y en paralelo con `/search`. Si nadie puntuó y solo hubo fríos/ocupados → default con aviso (antes error); un
  error real (500) sigue siendo error. Tests con servidores HTTP falsos (uno por conexión, con demoras):
  `a_slow_or_hung_member_costs_the_budget_once` (search de 10 s ×2, colgado, `/symbol` de 10 s → < 2.5 s con
  presupuesto 1.5 s), `a_busy_only_warm_member_falls_back_to_the_default`,
  `group_pick_falls_back_to_the_default_only_when_nothing_failed`.
- **M-1:** `context_backend` abre el miembro elegido (y el `project` explícito) por la ruta que ya tiene el binding
  (`member_backend` → `open_path`, compartido con `backend_for`), sin re-resolver el nombre contra el central.
  Test `a_group_member_is_opened_by_its_path` (`devctx-mcp/src/lib.rs`).
- **M-2:** `/health` devuelve `{"status":"ok","root":…}` (`AppState::root`; test
  `health_reports_the_project_root` en devctx-api). `warm_server` compara ese root con la ruta del miembro; uno
  distinto → `Cold("stale serve.json …")`; un servidor viejo sin `root` solo vale si `procown::classify(pid,
  start_time, cwd_is(root))` es `Ours` (la regla de `remote::discover`). Test
  `a_stale_serve_json_does_not_score_another_repository`.
- **M-3:** `member_serve_file` usa el `config_path` del registro (o el convencional) y, si no se puede leer, el
  `db_path` del registro (`ProjectRow.config_path`/`db_path`). No existe un override por entorno del `db_path` en
  `ProjectConfig` (el único `with_env_override` es de `Device`), así que no hay nada más que aplicar. Test
  `warm_server_follows_the_registered_config_and_db_paths`.
- **M-4:** `keep_selected` filtra en el cliente por `path_kind` de cada hit según `kind`/`include_tests` (un
  servidor 0.8.4 ignora esos campos del body); idempotente con servidores nuevos. Test
  `an_unfiltered_answer_is_filtered_by_kind_here` (pedido `doc`: el código del viejo ya no gana).
- **M-5:** `ProjectConfig::load` avisa una sola vez por proceso por (archivo, mensaje) (`first_warning`, OnceLock +
  HashSet). Test `a_config_warning_is_said_once_per_file`.
- **M-6:** caché extraída a `PickCache` (`get(key, now)` con edad, `put` solo si `cacheable`, barre vencidos). Un
  acierto arma un encabezado nuevo `X (cached choice from Ns ago: best match … of all N comparable members compared
  then; pass project …)` sin la cobertura ni avisos viejos. Tests `pick_cache_key_folds_the_query_and_keeps_the_selection`,
  `pick_cache_keeps_only_full_choices_for_the_ttl`.
- **M-7:** `compose_context` ya no quita del brief el código de los archivos que una memoria nombra en `files`;
  solo omite un hit cuyo texto la memoria incluida ya cita literal. Test
  `a_memory_naming_a_file_does_not_drop_its_code`; `a_memory_that_did_not_fit_does_not_hide_its_files_code`
  actualizado (una memoria que entra tampoco esconde el código).
- **Nits:** `defines` usa `backend::urlencode` (ahora `pub(crate)`), test `defines_percent_encodes_the_symbol`; el
  bono de identificador solo suma a un score base > 0 y "nada matcheó" mira el base, test
  `group_pick_bonus_never_lifts_a_zero`; empate de mayoría en `group_targets` se desempata por modelo y dimensión,
  no por orden de filas, test `group_targets_break_a_majority_tie_by_name`. Resultados de los hilos se ordenan por
  nombre antes de elegir (determinismo).
- Cada test nuevo se verificó fallando con la mutación que revierte su fix (I-1, I-2 tiempo y busy, M-2, M-3, M-4,
  M-7, encoding, bono, empate).
- Gate: `cargo fmt --check`; `clippy --workspace --all-targets` con y sin `--features gpu`, `-D warnings`;
  `cargo test --workspace` verde (corrido por paquetes/binarios, cada uno < 10 min); tests de devctx-mcp repetidos
  3× sin flakes; sin `target/debug/devctx serve` residuales.
- **No verificado:** latencia real en revfa (13 miembros) con el deadline de 2.5 s — un miembro que recarga su
  modelo en la primera `/search` quedará `busy` esa vez (su búsqueda sigue en el server y lo deja caliente para la
  próxima); calibración del margen (TASK-016).

### Fixup L (campo, B1)

En frío el grupo respondía desde el default y, con "1 de 3" puntuados, seguía respondiendo desde el default aunque la
pregunta fuera de otro miembro. Decisión: se mantiene "nunca levantar un serve para seleccionar". La advertencia ahora
es accionable: `Likely (named by the question): X, Y — pass project=X` (candidatos de `name_candidates`, ya filtrados
por IDF, restringidos a los miembros sin puntuar) y el brief lleva la línea legible por máquina
`[devctx] selection: {"scored":1,"total":3,"candidates":["X","Y"]}` (`GroupPick.selection`, `state::Selection`; ausente
si se compararon todos los comparables). Test `a_partial_selection_offers_candidates_machine_readably`; los tres tests
que comprobaban "Named by the question" se actualizaron a la frase nueva.
