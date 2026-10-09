# TASK-015 — Docs EN + ES y guía de reindex

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-002, TASK-011, TASK-012, TASK-013, TASK-014
- **Estado:** `pending`

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

- [ ] **Paso 1 — grafo.** `symbol-graph.md`/`grafo-de-simbolos.md`: tablas `symbols`/`edges`, kinds,
      `confidence`/`resolution`, `external`/`from_test`, qué resuelve y qué no por lenguaje (con la
      tabla de reglas de DD-7), y que la precisión no es de compilador.
- [ ] **Paso 2 — tools.** `repo_map`, `traverse`, `skeleton`: parámetros, ejemplos, cuándo usar cada
      una frente a `search`/`read_symbol`/`impact_analysis`/`get_references`; cambios en `impact`
      (defaults, campos nuevos) y en `build_context` (`## Related (signatures)`, `related`,
      `graph_hops`).
- [ ] **Paso 3 — ranking.** `search.md`/`busqueda.md`: la centralidad como tercera lista de la RRF,
      solo en híbrido, `search.centrality_weight`.
- [ ] **Paso 4 — reindex.** `13-keeping-the-index-fresh.md`/`13-mantener-el-indice-al-dia.md` y
      `AGENTS.md`: `EXTRACTOR_VERSION` 2, `extractor_stale`, `devctx index --full` por repo y rama,
      costo medido en TASK-002/TASK-016, qué contestan las tools mientras tanto (camino viejo y
      error de las nuevas), downgrade.
- [ ] **Paso 5 — decisiones y rendimiento.** `08-design-decisions.md` (por qué tree-sitter + reglas y
      no stack-graphs/LSP/SCIP todavía) y `07-performance.md` (números de TASK-016).
- [ ] **Paso 6 — CHANGELOG** con la sección del release (Q-1 del master).

## Criterios de aceptación

- [ ] Cada doc tocado en inglés tiene su par en español con el mismo contenido.
- [ ] Ningún ejemplo de la doc nombra código de ACME (repo público): ejemplos sobre este repo.
- [ ] Los números de costo y latencia citados son los medidos (TASK-002, TASK-016), con fecha.

## Riesgos

- Docs que prometen precisión: cada afirmación de "resuelve X" debe tener un fixture que la pruebe.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
