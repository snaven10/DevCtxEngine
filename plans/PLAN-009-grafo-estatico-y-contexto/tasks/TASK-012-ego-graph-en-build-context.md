# TASK-012 — Ego-graph en `build_context` y evaluación de la selección de grupo

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-009, TASK-010
- **Estado:** `done`

---

## Objetivo

Que `build_context`, además del código que la búsqueda encontró, traiga lo que lo rodea en el grafo
—callers, callees e implementaciones directas de los mejores hits— como **firmas** y con un tope de
presupuesto, para cubrir el código relacionado que no comparte vocabulario con la pregunta
(RepoGraph: +2-2.7 pts en SWE-bench Lite). Y medir si la selección de miembro de grupo gana algo con
el grafo antes de tocarla. Implementa DD-15 y DD-18.

## Contexto verificado

- `do_build_context` (`crates/devctx-mcp/src/state.rs:4114-4171`): memorias (`recall_fused`, 5) →
  `search_items` híbrido con 30 hits → `compose_context`.
- `compose_context` (`state.rs:4205-4303`, puro y testeable): sección de memorias con tope
  `CTX_MEMORY_SHARE_PCT = 35` (l.4175), sección `## Code` con tope por chunk
  (`CTX_CODE_CAP_DIV`, l.4182), "Recorded against this code" para los primeros 5 archivos, y
  `[devctx] omitted: N item(s)…` (l.4295-4300) + `close_context` (l.5493).
- Request MCP: `BuildContextReq` (`crates/devctx-mcp/src/lib.rs:256`); CLI `devctx context`
  (`--max-tokens`, `--no-memories`).
- Selección de grupo: `pick_group_member_within` (`state.rs:5381-5452`), `score_member`
  (`state.rs:5317`), desempate por `defines` (`/symbol/<name>`, `state.rs:5122-5128`),
  `PICK_IDENT_TOKENS = 2` (l.4526), `PICK_MARGIN = 0.03` (l.4524), `member_scores` sobre
  `raw_score` (l.4557-4572). Calibrado en campo 7/7 (PLAN-008 TASK-016).
- Tests existentes de `compose_context` (p. ej. `build_context_skips_a_chunk_that_does_not_fit_and_keeps_going`,
  `state.rs:7633`) y de selección de grupo (`group_pick_*`, `state.rs:7878+`).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_build_context`: vecinos de los primeros 5
  hits de código por lotes con la expansión de TASK-009; `compose_context`: sección
  `## Related (signatures)`), `crates/devctx-mcp/src/lib.rs` (`BuildContextReq`: `related: bool`,
  `graph_hops: u8`), `crates/devctx-mcp/src/backend.rs` y `crates/devctx-api/src/lib.rs` (pasar los
  parámetros), `crates/devctx-cli/src/main.rs` (`--no-related`, `--hops`).

## Pasos

- [x] **Paso 1 — tests que fallan** (sobre `compose_context`, puro): (a) la sección de firmas nunca
      supera 20 % del presupuesto; (b) va después de `## Code` y antes de "Recorded against this
      code"; (c) lo que no entra suma al `omitted` existente; (d) nunca un cuerpo, solo firma +
      relación + `archivo:línea`; (e) con `related: false` la salida es idéntica a la de 0.9.0.
- [x] **Paso 2 — vecinos.** Para los primeros 5 hits de código con símbolo (DD-4): `calls`/
      `instantiates` en ambas direcciones e `inherits`/`implements` hacia arriba, `≥ medium`, sin
      tests ni externos, por `rank`, hasta 4 por hit, sin repetir lo que ya está en `## Code`. Una
      consulta por lote, no por hit.
- [x] **Paso 3 — parámetros** `related` (default según Q-4) y `graph_hops` (1, máx. 2).
- [x] **Paso 4 — medir** con el arnés: relevancia del brief (archivo/símbolo esperado presente) con y
      sin `related`, tokens usados, latencia p50. Decidir el default con esos números.
- [x] **Paso 5 — selección de grupo (DD-18).** Variante experimental: bonus por `rank` del símbolo
      que el miembro define. Correr los 7 casos de PLAN-008 TASK-016 con y sin la variante. Solo se
      adopta si mejora sin romper ninguno; si no, se deja documentado en el Resultado y no se cambia
      la fórmula.

## Criterios de aceptación

- [x] Tests (a)-(e) verdes; tests existentes de `compose_context` y `group_pick_*` verdes sin
      modificar.
- [x] Arnés: relevancia de `build_context` ≥ base 0.9.0 + 10 pp (criterio global de §6), o el
      default queda en `related: false` con la tabla que lo justifica.
- [x] `build_context` p50 ≤ +150 ms sobre la base.
- [x] Tabla de los 7 casos de grupo con y sin variante en el Resultado.

## Riesgos

- Las firmas desplazan código útil del presupuesto: tope de 20 % y solo después de `## Code`.
- Vecinos de baja calidad en lenguajes poco resueltos (JS sin tipos): el filtro `≥ medium` los deja
  afuera.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. La sección `## Related (signatures)` queda **apagada por defecto**
  (`related: false`, Q-4): subió la relevancia del brief 3,8 pp, no los 10 que pide §6. La
  variante de selección de grupo (DD-18) no cambió ningún caso: **no se adopta**.
- **Resumen:** `build_context` puede sumar, entre `## Code` y "Recorded against this code", las
  firmas de los vecinos en el grafo de sus primeros 5 hits de código (DD-15), leídos en un lote;
  `related` y `graph_hops` en MCP, `POST /context`, `devctx context` (`--related`,
  `--no-related`, `--hops`) y `Backend` local y remoto. Primer commit: el MINOR de TASK-014
  (`clean_signature` con `leading_annotations` del parser).
- **Commits** (rama `feat/plan-009-grafo`, sin push): 4594c63 (`clean_signature`), 18c1b02
  (vecinos en el store), 3822474 (sección, parámetros, cliente, medición de grupo), 193507c
  (`DEVCTX_EVAL_CONTEXT_ARGS` del arnés) y el de esta documentación.
- **Números** (sandbox `mktemp -d -p /var/tmp`; copias sin `.devctx` verificadas con `find`;
  índices **con modelo** `ml-granite`, de a uno: DevCtxEngine 2 199 s, backend-b 1 108 s,
  backend-a y frontend —los tiempos de estos dos no sirven: el reloj de la máquina saltó durante
  la corrida—; frontend 17 761 símbolos, 14 812 chunks). Arnés de TASK-001 (`run.sh` paso
  `relevance`, brief de 4 096 tokens sin memorias), mismo binario, sin / con `--related` / con
  `--related --hops 2`. legacy-migration (4 casos) no se indexó: no estaba en la autorización.

  | Repo (casos) | brief: archivo sin · con · hop 2 | brief: símbolo sin · con | tokens est. medios sin · con · hop 2 |
  |---|---|---|---|
  | DevCtxEngine (10) | 9/10 · 9/10 · 9/10 | 4/4 · 4/4 | 4 038 · 4 039 · 4 069 |
  | backend-b (2) | 1/2 · **2/2** · 2/2 | 2/2 · 2/2 | 4 095 · 4 105 · 4 090 |
  | backend-a (8) | 7/8 · 7/8 · 7/8 | 7/8 · 7/8 | 4 035 · 4 062 · 4 073 |
  | frontend (6) | 4/6 · 4/6 · 4/6 | 5/6 · 5/6 | 4 078 · 4 074 · 4 055 |
  | **Total (26)** | **21/26 = 80,8 % · 22/26 = 84,6 % · 22/26** | 18/20 · 18/20 | |
  | inglés (15) | 15/15 · 15/15 · 15/15 | 11/11 · 11/11 | |
  | español (11) | 6/11 · **7/11** · 7/11 | 7/9 · 7/9 | |

  Base 0.9.0 (TASK-001, 30 casos): archivo 24/30 = 80,0 %, símbolo 17/24. Con la sección: +3,8 pp
  sobre esta misma rama sin ella; **ningún caso pasa de acierto a fallo**. El único caso que
  gana (backend-b, español) encuentra el archivo esperado en una línea `called by` de la sección,
  no en el código. Hop 2 no suma nada a hop 1. Por eso el default queda apagado.
- **Latencia** (`POST /context` sobre el serve caliente, los casos de cada repo × 5 rondas
  intercaladas sin / con / hop 2; 4 096 tokens, sin memorias), p50 / p95 en ms:

  | Repo | sin | con | hop 2 | Δ p50 con |
  |---|---|---|---|---|
  | DevCtxEngine | 92 / 120 | 116 / 148 | 145 / 182 | +24 |
  | backend-b | 78 / 95 | 87 / 123 | 96 / 122 | +9 |
  | backend-a | 142 / 170 | 179 / 231 | 238 / 281 | +37 |
  | frontend | 132 / 164 | 186 / 236 | 229 / 268 | +54 |

  Máquina con carga 14-16. **Meta ≤ +150 ms: cumplida.** Medido antes con carga 30-47 (otro
  índice corriendo): DevCtxEngine 232 → 333, backend-b 186 → 243, backend-a 284 → 443 (+159, por
  encima). Sobre una copia de backend-a (210 567 aristas): el nivel hacia arriba de `edge_level`
  60 ms, hacia abajo 39, el índice de dispatch 19 (el `IN` sobre ids es barato; el filtro por
  `repo`/`branch` y el join con `symbols` son el costo).
- **Selección de grupo (DD-18)**, `group_pick_rank_variant_eval` (`score_member` y
  `choose_member` reales sobre 3 miembros con serve vivo; la variante suma `PICK_MARGIN ×
  percentil del rank` de la definición al miembro que define un identificador):

  | Caso | esperado | hoy | variante |
  |---|---|---|---|
  | 1 | backend-a | backend-a | backend-a |
  | 2 | backend-a | backend-a | backend-a |
  | 3 | backend-a | frontend (ambiguous) | frontend (ambiguous) |
  | 4 | backend-b | backend-a (ambiguous) | backend-a (ambiguous) |
  | 5 | backend-b | backend-b (ambiguous) | backend-b (ambiguous) |
  | 6 | frontend | frontend (defines un identificador) | frontend |
  | 7 | frontend | frontend | frontend |

  5/7 hoy y con la variante; solo el caso 6 tiene un `defines`, y ya ganaba: la variante actuó en
  1 de 7 casos, así que es **no evaluable** con estos casos (revisión, m10); la fórmula no cambia
  (DD-18).
- **Hallazgos de §2:** H7 (el grafo no participa del contexto): ahora puede, a pedido. Sin
  cambios en H1-H6.
- **Archivos tocados:** `crates/devctx-store/src/{related.rs (nuevo), lib.rs}`,
  `crates/devctx-mcp/src/{state.rs, state/related.rs (nuevo), backend.rs, lib.rs, repo_map.rs}`,
  `crates/devctx-api/src/lib.rs`, `crates/devctx-cli/src/{main.rs, remote.rs}`,
  `crates/devctx-parse/src/{parser.rs, lib.rs}`, `crates/devctx-index/src/lib.rs`,
  `scripts/graph-eval/{run.sh, README.md}`, `CHANGELOG.md`, docs EN/ES del constructor de
  contexto, el design (DD-15, DD-18) y el master. Públicos nuevos: `devctx_store::{ChunkRef,
  RelatedNeighbor, Relation, RELATED_PER_SEED, RELATED_MAX_HOPS}`, `Store::{chunk_symbols,
  related_neighbors}`, `devctx_parse::leading_annotations` (y reexportada por `devctx_index`),
  `devctx_mcp::state::{ContextOpts, DEFAULT_RELATED, DEFAULT_GRAPH_HOPS, RELATED_HEADING,
  RELATED_NOTE, do_build_context_with}`, `devctx_mcp::backend::{note_unapplied_related,
  RELATED_OLD_SERVER_NOTE}`. **Cambiadas:** `Backend::build_context` y
  `RemoteClient::build_context` (CLI) toman `&ContextOpts`. Sin schema, sin `languages/*.json`,
  sin config nueva, sin `EXTRACTOR_VERSION`/`LINK_VERSION`: un índice de TASK-010 en adelante
  sirve sin reindexar.
- **Tests** (gate completo antes de cada commit de código, en una copia del árbol con su propio
  `target`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings` con y sin `--features gpu`, `TMPDIR=<sandbox>/tmp cargo test --workspace --locked
  --no-fail-fast`: 1 214, 1 223 y 1 237 pasan, 0 fallan; el tercero falló una vez en clippy por
  `type_complexity` antes de commitear, corregido con un alias):
  - *Fallaban antes del cambio* (escritos y corridos primero): los 9 de `related::tests` del
    store (stubs vacíos); `an_annotation_with_a_bracket_in_a_literal_is_stripped` (la anotación
    quedaba delante); (a) `related_keeps_to_its_share_and_counts_what_does_not_fit`, (b)
    `related_goes_between_the_code_and_the_linked_memories`, (d)
    `related_lines_are_a_signature_a_relation_and_a_place`,
    `related_reads_the_hits_the_brief_shows`, `no_related_section_says_why` (con el stub que
    ignoraba la sección); `a_hit_shows_its_caller_and_its_callee`,
    `nothing_is_read_unless_the_section_is_on`, `an_old_index_says_why_there_is_no_section` (con
    `gather` vacío); `related_on_an_old_server_says_it_was_not_applied` (sin la nota);
    `the_first_five_code_hits_are_the_seeds` con el test en línea (era semilla);
    `related_takes_its_room_from_the_code` y `a_neighbor_of_a_hit_pushed_out_goes_with_it` (con
    la sección leída después del código; el segundo, además, contó mal las líneas que se van con
    su hit).
  - (e) `without_related_the_brief_is_the_old_one_byte_for_byte`: golden capturado del
    `compose_context` de 0.9.0 antes de tocarlo.
  - *Verificados por mutación* (revertir la regla, `touch`, correr): filtro de confianza, de
    tests/`file`, semillas excluidas, "ya mostrado", "tomado por otra semilla", orden por rank,
    desempate por nombre (el fixture se cambió para que el archivo no ordene igual), cadena de
    dispatch `high`/`medium`, chunk `doc`; tope del 20 %, suma al `omitted`, aplanado de la
    firma, semillas de lo que el brief muestra, tope de 5 semillas, solo código, `related:false`
    sin lecturas, aviso de índice viejo, ubicación entre código y memorias ligadas, re-acomodo
    del código, devolución del lugar, retiro de las líneas de un hit que se fue y su conteo.
    Todos fallan con la mutación.
  - Tests existentes de `compose_context`, `group_pick_*`, `build_context` y `mcp_tools`:
    **verdes sin tocar ningún assert** (`compose_context` queda como referencia de 0.9.0 en
    tests, con la misma firma).
- **Contrato JSON / texto:** `build_context` (MCP) y `POST /context` aceptan `related` y
  `graph_hops` (aditivo); `graph_hops` fuera de 1..=2 es un error. El brief solo cambia con la
  sección pedida: `## Related (signatures)` o una línea `[devctx] related: …`.
- **Desviaciones:**
  1. La sección **toma su lugar del código** (hasta el 20 %) en vez de usar lo que el código deja:
     así como estaba en el diseño casi nunca entraba (DD-15).
  2. Un símbolo de test dentro de un archivo de código no es semilla.
  3. `inherits`/`implements` "hacia arriba" incluye, para una semilla método, el método del
     supertipo que sobrescribe (`overrides`, por el índice de dispatch de TASK-017).
  4. Los 7 casos de grupo no son los de PLAN-008 TASK-016 (no quedaron registrados): 7 del arnés
     elegidos antes de medir, con 3 miembros.
  5. 26 de los 30 casos (sin legacy-migration).
  6. La medición de grupo quedó como test ignorado (`group_pick_rank_variant_eval`) en
     `state.rs`; la variante no está en el código de producción.
- **No verificado:** legacy-migration; macOS/Windows (solo CI); el grupo real de ACME (más
  miembros) y las 7 consultas originales; la latencia con la máquina libre (carga 14-16 en la
  medición buena).
- **Riesgos abiertos / siguiente:** (1) TASK-016 puede volver a medir el default con los 30
  casos; la sección ayuda en español (6/11 → 7/11) y no toca inglés. (2) El costo de
  `edge_level` en backend-a (60 ms el nivel hacia arriba) es del filtro por rama y el join con
  `symbols`; mejorarlo ayuda también a `impact` y `traverse`. (3) Hop 2 no aportó: se puede
  retirar si TASK-016 tampoco lo justifica.
- **Revisión (APROBADA CON CAMBIOS, sin BLOCKER), resuelta en 40d01c6 encima de 19f95c8:**
  - *M1 (el total podía pasarse del presupuesto) y M2 (el acomodo greedy no es monótono: un grupo
    podía quedar sin el código de su hit):* el acomodo ahora itera hasta un punto fijo. El código
    se acomoda en lo que deja la sección; se retienen solo las líneas cuyo hit sigue en ese código
    y cuyo vecino ese código no muestra (*m3*); el render nunca supera el anterior (no readmite lo
    que cortó), así que la sección solo achica y el ciclo termina; con el texto final el código
    queda acomodado para ese largo. *m1:* si la sección sacara al mejor hit, no hay sección
    (`not shown: N neighbor(s) would push out the best hit`). *m2:* la línea de nota entra en el
    presupuesto (el código le hace lugar) salvo que eso sacara al mejor hit; ahí cierra el brief
    sin presupuesto, como `omitted`. NIT: `none fitted` (pasan el 20 %) y `none shown` (sus hits
    ya no entran) se distinguen.
  - *Re-medido con el binario corregido* (mismos 26 casos): archivo en el brief 21/26 sin, 22/26
    con y con hop 2, igual que antes por repo e idioma. Ningún brief pasa de 16 384 bytes antes
    de la línea `omitted` (máximo 16 383). El 4 105 de backend-b era la estimación del arnés
    (caracteres / 4 de toda la salida, con la línea `omitted` y la de `branch_fallback`, que no
    están presupuestadas), no un desborde del brief; el desborde de M1 sí era posible (lo muestra
    el test de tabla, que fallaba). **Archivo en el código contra archivo solo citado:** sin 21 / 0,
    con 21 / 1, hop 2 21 / 1: el +1 es solo una ruta en una línea `called by`.
  - *Tests que fallaban antes del fix:*
    `related_never_passes_the_budget_and_never_shows_a_line_without_its_hit` (tabla determinista
    de 400 briefs: presupuesto y líneas sin hit), `a_greedy_layout_never_leaves_a_group_without_its_hit`
    (el ejemplo de M2), `related_never_pushes_out_the_best_hit` (m1, 170 tokens),
    `a_neighbor_the_final_code_shows_is_not_repeated` (m3). Con el fix escrito y verificados por
    mutación: `the_related_note_is_part_of_the_budget` (m2: falla sin el lugar para la nota),
    `a_section_that_shrinks_gives_the_room_back` (falla con una sola pasada), el guardia del mejor
    hit, el retain por hit y por "ya mostrado". La regla "el render solo achica" no tiene un test
    que la distinga: con readmisión el ciclo igual respeta el presupuesto y las líneas; lo que
    asegura es que termina.
  - *m7:* `nothing_is_read_unless_the_section_is_on` llama con `ContextOpts::default()` y exige 0
    lecturas y nada de la sección (verificado: con `DEFAULT_RELATED = true` falla). Se usa el
    contador `GATHERS` del MCP: `LEVEL_STATEMENTS` es `cfg(test)` del crate del store y no se ve
    desde `devctx-mcp`; `GATHERS` cuenta la única entrada al grafo.
  - *m8:* segundo golden de 0.9.0 (`without_related_a_quoted_hit_and_a_long_doc_are_the_old_brief`:
    un hit que una memoria cita y un javadoc de 12 líneas), capturado ejecutando `compose_context`
    en un checkout de cb350bb.
  - *NITs:* test de una anotación sin cerrar después de otras (la firma queda como está
    guardada); el doc-comment huérfano de `group_pick_chooses_the_member_that_matches` vuelve a
    su test; `devctx context --hops` sin `--related` avisa por stderr; test propio del filtro de
    externos (`an_external_neighbor_is_left_out`, verificado por mutación).
  - *Gate de 40d01c6:* fmt, clippy con y sin `gpu`, `cargo test --workspace`: 1 245 pasan, 0
    fallan.
  - **Para TASK-016 (anotado, no hecho):**
    - *m4:* hop 2 se consulta siempre que se pide y no aportó nada (+30-60 ms): expandir solo las
      semillas con menos de `per_seed` vecinos, o retirarlo.
    - *m5:* `dispatch_index` lee todas las aristas de supertipo en cada brief (19 ms en
      backend-a): cachearlo por generación del índice.
    - *m6:* `do_build_context_with` descarta el `Reader` de la búsqueda, abre un segundo store y
      `gather` repite `has_symbol_graph`.
    - *m9:* `note_unapplied_related` detecta la sección por substring; buscar al inicio de línea.
    - *m10:* la variante de DD-18 actuó en 1 de 7 casos: **no evaluable**, no "no mejora". Hallazgo:
      la selección de hoy acertó 5/7 en estos casos (los dos fallos marcados `ambiguous`) contra
      el 7/7 de la calibración de PLAN-008; las consultas no son las mismas ni el grupo.
    - *Perf:* `edge_level` filtra por `repo`/`branch` y hace el join con `symbols` sin índices:
      leer las aristas por id y resolver los extremos después, `UNION ALL` de las dos
      direcciones, `ListParam` con `prepare_cached`. El +159 ms medido bajo carga incumple la meta.
    - *Evaluación:* sumar al arnés la métrica "archivo en el código" contra "archivo solo citado";
      el +3,8 pp es un caso de 26, por una ruta en una línea `called by`.
