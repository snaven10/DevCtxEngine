# TASK-015 — Docs EN + ES

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** docs (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-013, TASK-014, TASK-007, TASK-005
- **Estado:** `done`

---

## Objetivo

Documentar en inglés y español lo que cambia para el usuario y para el agente.

## Contexto verificado

- Docs EN en `docs/` (p. ej. `docs/03-core-concepts/mcp-integration.md`, `search.md`,
  `context-builder.md`, `symbol-graph.md`, `docs/11-configuration.md`,
  `docs/13-keeping-the-index-fresh.md`); traducción en `docs/es/`.
- `AGENTS.md` ("Things that will bite you") y `MEMORY-PROTOCOL.md` (embebido en el binario,
  `crates/devctx-mcp/src/lib.rs`).
- `CHANGELOG` del release.

## Pasos

- [ ] **Paso 1 — Ciclo de vida.** MCP sin DB propio, `serve.log`, errores de lock con PID, reconexión,
      `mcp.binary_replaced` y "reiniciá la sesión" (mcp-integration + AGENTS.md).
- [ ] **Paso 2 — Hooks.** Bloque nuevo con `env -u`, reinstalar con `devctx hooks install`, y qué se
      indexa al commitear desde un worktree (`13-keeping-the-index-fresh`).
- [ ] **Paso 3 — Honestidad del índice.** `branch_fallback`, `extractor_stale`, `index --full`.
- [ ] **Paso 4 — Tools.** `project`/`limit`/`offset`/`omitted`, `active_only`, `kind`/
      `include_tests`, penalizaciones, `default_excludes` (`11-configuration`), `build_context` en
      grupo.
- [ ] **Paso 5 — CHANGELOG** de 0.8.3 y 0.9.0 (o el que decida PLAN-008 Q-1).

## Criterios de aceptación

- [ ] Cada campo JSON nuevo de PLAN-008 aparece en EN y ES.
- [ ] Los ejemplos de config parsean (copiar a un `config.yaml` de prueba y `devctx status`).

## Riesgos

Docs ES desfasadas respecto de EN: hacer ambas en el mismo commit.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después:** sin repro (docs). Antes: `search.md`/`busqueda.md` decían "arreglo JSON", `DEVCTX_EMBED_BATCH_SIZE` figuraba con default 32, nada documentaba el ciclo de vida del MCP, `branch_fallback`, `extractor_stale`, excludes por defecto ni el contrato de paginación.
3. **Causa raíz:** n/a.
4. **Archivos tocados** (EN + ES en el mismo commit):
   - Nuevo `CHANGELOG.md` (0.8.3 tag sin release, 0.8.4, 0.8.5, "Unreleased (0.9.0)" con Breaking / Added / Fixed / notas de actualización).
   - `docs/03-core-concepts/{mcp-integration,search,context-builder,symbol-graph,memory}.md` y `docs/es/03-conceptos-fundamentales/{integracion-mcp,busqueda,constructor-de-contexto,grafo-de-simbolos,memoria}.md`.
   - `docs/11-configuration.md`/`docs/es/11-configuracion.md`, `docs/13-keeping-the-index-fresh.md`/`docs/es/13-mantener-el-indice-al-dia.md`, `docs/12-central-store.md`/`docs/es/12-store-central.md`, `docs/07-performance.md`/`docs/es/07-rendimiento.md`.
   - `README.md`, `AGENTS.md` ("Things that will bite you"), `MEMORY-PROTOCOL.md` (un párrafo en "Building context").
   - Correcciones de pasada: `search --format json` es un objeto (EN y ES); batch por defecto 8 (no 32); "23 definiciones" -> 24 tools; el MCP ya no "corre enteramente en proceso"; fila `DEVCTX_DAEMON_TIMEOUT_SECS` que faltaba en ES.
5. **Tests:** no aplica. Verificación: (a) cada variable de entorno (`DEVCTX_EMBED_BATCH_SIZE`, `DEVCTX_MODEL_IDLE_SECS`, `DEVCTX_MODEL_STALL_SECS`, `DEVCTX_INDEX_STALL_SECS`, `DEVCTX_SHUTDOWN_GRACE_SECS`, `DEVCTX_VANISH_POLL_MS`), clave de config (`indexing.default_excludes`, `indexing.include_build`, `search.penalty.{test,doc,config}`), campo JSON y mensaje citado se buscó con grep en `crates/`; (b) el YAML de ejemplo de `11-configuration` (con `default_excludes`, `include_build`, `search.penalty` y `exclude: ["!/target/", "target/"]`) se copió a un `config.yaml` temporal y `target/debug/devctx status` (0.8.5) lo parseó (`DEVCTX_HOME` temporal, sin tocar `~/.local`); (c) un script comprobó los enlaces relativos y anclas de los `.md` tocados (0 rotos).
6. **Contrato JSON:** documentado en `mcp-integration.md` (§Return shapes, EN/ES): objetos `results`/`routes`, `omitted{count,reason,next_offset?}` (+ alias `omitted_for_budget`), `total`, `next_offset`, `branch_fallback`, `warning`, `raw_score`, `kind`, `language`, `anchored`; páginas (search_routes 20, plan_status 25 + `active_only`, memories_by_* 5/600 chars + `full`); `project`; `read_symbol` `suggestions`/`external`/`called_from`/`next_step`; `link_sources: files-field`; `mcp{version,binary_replaced,installed_version?,hint?}`; `extractor_stale`.
7. **No verificado:** que cada afirmación de plataforma (macOS `ps`/`lsof`/kqueue, Windows `Unsupported`) coincida con un binario real en esas plataformas (se documentó lo que dicen el código y las notas de TASK-001/017); el CI de `fix/**` se cruzó leyendo `.github/workflows/rust.yml` (dispara en `push` a `main` y `fix/**`), no ejecutándolo; la voz/tono ES se mantuvo por documento (voseo en los de conceptos y config; tuteo neutro en `13-mantener-el-indice-al-dia`), sin revisión de un hablante nativo. No se compiló nada.
8. **Discrepancias plan/código encontradas:** TASK-005 dice que el `unset` del hook cubre 7 variables, pero `GIT_REPO_ENV` (`crates/devctx-core/src/gitenv.rs:15-28`) tiene 12 desde el fixup M-7; los docs dicen "las doce". El plan §3 dice que `Indexing.exclude` era el único mecanismo; TASK-011 corrigió que existía `VENDOR_DIRS` (ya reflejado en el CHANGELOG sin mencionarlo). `init --download` y `models --download` (y las variables de estancamiento/gracia/vanish) son de P0 (0.8.3/0.8.4), no de 0.9.0: el CHANGELOG los ubica ahí.
