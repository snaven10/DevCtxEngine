# TASK-015 — Docs EN + ES y guía de reindex

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-002, TASK-011, TASK-012, TASK-013, TASK-014
- **Estado:** `done`

---

## Objetivo

Que la documentación (inglés y español) describa el grafo nuevo, las tools nuevas, las señales de
ranking y cómo actualizar: por qué hace falta `devctx index --full`, cuánto cuesta (con el reuso de
vectores de TASK-002), qué pasa mientras tanto y cómo volver atrás. Cerrar con el CHANGELOG.

## Contexto verificado

- Docs existentes del área: `docs/03-core-concepts/symbol-graph.md`,
  `docs/03-core-concepts/context-builder.md`, `docs/03-core-concepts/search.md`,
  `docs/03-core-concepts/mcp-integration.md`, `docs/04-agent-workflow.md`,
  `docs/13-keeping-the-index-fresh.md`, `docs/08-design-decisions.md`, `docs/07-performance.md`,
  `docs/11-configuration.md`; en español bajo `docs/es/` con los mismos números
  (`grafo-de-simbolos.md`, `constructor-de-contexto.md`, `busqueda.md`, `integracion-mcp.md`,
  `04-flujo-de-trabajo-del-agente.md`, `13-mantener-el-indice-al-dia.md`, `08-decisiones-de-diseno.md`,
  `07-rendimiento.md`, `11-configuracion.md`).
- `MEMORY-PROTOCOL.md` y `AGENTS.md` §6 (costo de indexado) en la raíz; `CHANGELOG.md`.
- Mecanismo de aviso de índice viejo (PLAN-008 TASK-007) ya documentado en
  `docs/13-keeping-the-index-fresh.md` (y su par en español).

## Archivos

- **Modificar:** los docs listados arriba (EN y ES), `AGENTS.md` §6 (costo con reuso de vectores),
  `CHANGELOG.md`, `README.md` si lista tools.

## Pasos

- [x] **Paso 1 — grafo.** `symbol-graph.md`/`grafo-de-simbolos.md`: tablas `symbols`/`edges`, kinds,
      `confidence`/`resolution`, `external`/`from_test`, qué resuelve y qué no por lenguaje (con la
      tabla de reglas de DD-7), y que la precisión no es de compilador.
- [x] **Paso 2 — tools.** `repo_map`, `traverse`, `skeleton`: parámetros, ejemplos, cuándo usar cada
      una frente a `search`/`read_symbol`/`impact_analysis`/`get_references`; cambios en `impact`
      (defaults, campos nuevos) y en `build_context` (`## Related (signatures)`, `related`,
      `graph_hops`).
- [x] **Paso 3 — ranking.** `search.md`/`busqueda.md`: la centralidad como tercera lista de la RRF,
      solo en híbrido, `search.centrality_weight`.
- [x] **Paso 4 — reindex.** `13-keeping-the-index-fresh.md`/`13-mantener-el-indice-al-dia.md` y
      `AGENTS.md`: `EXTRACTOR_VERSION` 2, `extractor_stale`, `devctx index --full` por repo y rama,
      costo medido en TASK-002/TASK-016, qué contestan las tools mientras tanto (camino viejo y
      error de las nuevas), downgrade.
- [x] **Paso 5 — decisiones y rendimiento.** `08-design-decisions.md` (por qué tree-sitter + reglas y
      no stack-graphs/LSP/SCIP todavía) y `07-performance.md` (números de TASK-016).
- [x] **Paso 6 — CHANGELOG** con la sección del release (Q-1 del master).

## Criterios de aceptación

- [x] Cada doc tocado en inglés tiene su par en español con el mismo contenido.
- [x] Ningún ejemplo de la doc nombra código de ACME (repo público): ejemplos sobre este repo.
- [x] Los números de costo y latencia citados son los medidos (TASK-002…014, TASK-017), con fecha;
      los de TASK-016 quedan marcados como pendientes.

## Riesgos

- Docs que prometen precisión: cada afirmación de "resuelve X" debe tener un fixture que la pruebe.

## Resultado

<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. Solo docs; nada de código, nada compilado ni testeado (no hizo falta:
  cada comportamiento se verificó leyendo el código o el test que lo fija).
- **Resumen:** las docs EN y ES describen el grafo de símbolos de 0.10 (tablas, kinds, confianza,
  resolución, externos con evidencia, descartes, reglas de DD-7 y qué resuelve y qué no por lenguaje),
  cuándo usar cada tool del grafo, la centralidad opt-in con su porqué, la guía de paso a 0.10
  (`--full` por repo y rama, costo medido con fecha, qué contesta cada tool mientras tanto,
  `extractor_stale` + `graph_out_of_step`, `LINK_VERSION`, downgrade a 0.9.0), tres ADR nuevos
  (19-21), el rendimiento medido de TASK-009…014/017 con un marcador para TASK-016, y el CHANGELOG
  consolidado en la sección 0.10.0 sin fecha ni tag. **Versiones reales verificadas en el código:**
  `EXTRACTOR_VERSION` = 21 (`crates/devctx-parse/src/registry.rs`), `LINK_VERSION` = 21
  (`crates/devctx-index/src/link.rs`); la spec decía 2.
- **Commits** (sobre `3242d8f`, sin push): `a22155d` docs(grafo), `2517400` docs(tools), `736f9da`
  docs(ranking), `1e0fe33` docs(reindex), `e6c4dd3` docs(decisiones), `76ef3c1` docs(changelog),
  `551d5e9` docs(grafo) introducción, `f060ee7` docs(tools) ejemplos, y el de este Resultado.
- **Pares EN/ES tocados** (mismos encabezados, tablas y bloques de código en cada par, contado):
  `01-introduction`/`01-introduccion`; `02-architecture`/`02-arquitectura`;
  `03-core-concepts/symbol-graph`/`grafo-de-simbolos`; `mcp-integration`/`integracion-mcp`;
  `search`/`busqueda`; `04-agent-workflow`/`04-flujo-de-trabajo-del-agente`;
  `05-examples/refactoring`/`refactorizacion`; `05-examples/debugging`/`depuracion`;
  `06-extending-the-system`/`06-extender-el-sistema`; `07-performance`/`07-rendimiento`;
  `08-design-decisions`/`08-decisiones-de-diseno`; `11-configuration`/`11-configuracion`;
  `13-keeping-the-index-fresh`/`13-mantener-el-indice-al-dia`. Solo EN (no tienen par): `AGENTS.md`
  (§6), `README.md`, `CHANGELOG.md`. Sin cambios: `context-builder` (ya completo desde TASK-012),
  `MEMORY-PROTOCOL.md` (nada del plan lo toca).
- **Respaldo de "resuelve X":** cada fila de la tabla por lenguaje de `symbol-graph` lleva en un
  comentario HTML los tests que la fijan (`crates/devctx-parse/tests/link_{java,typescript,python,
  rust,go}.rs`, `facts_*.rs`, `src/symbol_id.rs::qualified_names_carry_every_enclosing_symbol`,
  `devctx-mcp state::impact::tests::dispatch_follows_a_rust_blanket_impl`,
  `state::lookup::tests::discarded_calls_reach_no_answer`). Lo que no tiene fixture se dice así
  (Go: "no Go repository was measured"; método promovido por embebido: "has no test").
- **Pendientes "para TASK-015" encontrados y dónde quedó cada uno:**
  1. TASK-013 (revisión, NIT): `traverse` da error con un índice viejo mientras impact y
     get_references degradan con `warning` → tabla "What the tools answer meanwhile" de
     `13-keeping-the-index-fresh` (y ES), sección "An index made before 0.10" de `symbol-graph`,
     paso 3 de "Upgrading from 0.9.x" del CHANGELOG.
  2. TASK-013 riesgo (1), integrar sus docs en la guía de reindex → ídem.
  3. Guía de `--full` (spec, TASK-002/003/004) → "Upgrading to 0.10: the symbol graph" en 13/ES,
     `AGENTS.md` §6, `mcp-integration` "Upgrading to 0.10", README, CHANGELOG.
  4. Lo que el extractor no da (TASK-014 MINOR 7: namespaces TS, overloads TS, Python anidado;
     TASK-006 riesgos: CommonJS, `export =`, `import()`, JSX, `.d.ts`, `paths` por app, signals;
     TASK-007 riesgos: `#[path]`, `mod` condicional, macros, blanket impl, Python dinámico, Go sin
     repo real; TASK-005: parámetros de lambda, sobrecargas por tipo) → columna "Does not" de la
     tabla por lenguaje de `symbol-graph` (el Python anidado sí está cubierto en el link:
     `inheritance_and_nested_functions`; la nota de `skeleton` sigue diciendo que su tool no tiene
     fixture de eso).
  5. Límites conocidos por lenguaje y precisión que no es de compilador → misma tabla, "This is not
     a compiler", "Measured precision" y ADR-19.
  6. Costo medido (TASK-002/003/004) con fecha → tabla "What it costs" de 13/ES; el de un índice
     0.9.0 real con el binario final queda marcado para TASK-016 en 13, 07 ("To be completed in
     TASK-016" / "Se completa en TASK-016") y en Known issues del CHANGELOG.
  7. TASK-011 riesgo (1), pedir foco en `repo_map` → `04-agent-workflow` ("repo_map (with a focus)")
     y `symbol-graph` (ya lo decía).
  8. Centralidad opt-in (TASK-010) → `search.md`/`busqueda.md` con los números de la calibración y
     el corte por idioma; ADR-21.
- **Contradicciones doc ↔ código encontradas (todas corregidas en la doc):**
  1. `mcp-integration` decía "all 25 definitions" en Discovery; el código registra 27 tools.
  2. `search.md` decía que `search` no lleva el aviso de extractor viejo; `do_search` lo agrega en
     `keyword`/`hybrid` (`Reader::search_warning`, desde TASK-008 M2).
  3. `11-configuration`: `DEVCTX_MAX_OUTPUT_TOKENS` figuraba como tope de `read_file` solamente; es
     el presupuesto de casi todas las tools (search, recall, references, impact, traverse,
     skeleton, máximo de repo_map…).
  4. `08` ADR-17: "las otras 22 tools devuelven JSON"; hoy son 27 y `repo_map`, `skeleton` y
     `summarize` también devuelven texto. El ES no tenía el ADR-18 (agregado).
  5. `06-extending` describía un `enum Lang` y las claves `symbol_query`/`calls_query`, retiradas
     (el registro es JSON + `grammar_for`, claves de TASK-004…007).
  6. `02-architecture`: la tabla de almacenamiento no tenía `symbols`/`edges`/`index_meta`.
  7. `05-examples/refactoring`: "module-level code has no source symbol, so the edge is dropped";
     desde TASK-003 la fuente es el símbolo de archivo. Y "dynamic dispatch leaves no edge": el
     dispatch por interfaz se sigue desde TASK-017.
  8. `symbol-graph` (y `04-agent-workflow`, `debugging`): "one edge kind: calls", "keyed by
     qualified name, a bare name expands to every Class.method": describían 0.9.
  9. `search.md`: "the first `devctx index` of the new version computes the ranks"; vale para un
     índice ya hecho por el extractor de 0.10, no para uno de 0.9 (necesita el `--full`).
- **Posible bug de código (no arreglado, reportado):** en una rama vigente **sin filas de
  símbolos** (repo solo de texto), `traverse` y `repo_map` responden `*_NEEDS_REINDEX` ("reindex
  with devctx index --full"), que no cambia nada: es el mismo bucle que TASK-014 cerró para
  `skeleton` con `SKELETON_NO_SYMBOLS` (`state/traverse.rs::traverse_on` y `state/repo_map.rs::check`
  tratan `Legacy::NoRows` igual que `Legacy::Stale`). La doc describe el comportamiento actual.
- **Privacidad:** antes de cada commit, `git grep` de los patrones privados (vacío) y barrido de
  las líneas `+` de los términos de la lista (vacío). Los ejemplos usan este repo o nombres
  genéricos (`OrderService`, `OrderDto`); los repos privados van como "a 1 250-file Java
  repository", "a subset of an Angular workspace". `OfficeService.actualizar`/Quarkus ya estaban en
  la doc pública antes de esta task; se conservaron.
- **Verificado por:** lectura del código y de los tests citados (versiones, textos de error,
  defaults de `get_references`/`impact`/`traverse`, `with_centrality`, `extractor_stale` y
  `graph_out_of_step`, camino de copia antes del parse en `index_file`, `cmd_reindex`); conteo de
  encabezados/tablas/bloques por par; los dos barridos de privacidad. Sin `cargo`.
- **Desviaciones:** (1) se tocaron también `01`, `02`, `05-examples`, `06` y `11` (y sus pares),
  que la spec no listaba, porque contradecían el código; (2) el CHANGELOG no conserva la historia
  de versiones intermedias del extractor (2…21) ni de las reglas de enlace: la sección 0.10.0
  describe el paso 0.9.0 → 0.10.0 (la historia queda en los Resultados de cada task y en git);
  (3) `07-performance` incluye TASK-017 (dispatch) junto a TASK-009…014.
- **Riesgos abiertos / siguiente:** TASK-016 completa los números marcados (indexado desde cero,
  tamaño, `VmHWM`, arnés de 30 casos con centralidad por idioma, upgrade de índices 0.9.0 reales con
  el binario final, downgrade con el binario 0.9.0 publicado) y decide si la centralidad o
  `related` cambian de default; si cambian, hay que actualizar `search`, `context-builder`, ADR-21
  y el CHANGELOG. La fecha y el tag de 0.10.0 esperan el OK del usuario.
