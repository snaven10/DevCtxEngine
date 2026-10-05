# TASK-016 — Verificación de campo en `~/revfa` y este repo, antes/después

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** — (orquestador, con el binario nuevo instalado y aprobación del usuario)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-015
- **Estado:** `pending`

---

## Objetivo

Reproducir cada bug de PLAN-008 §2 con el binario 0.8.2 y re-medirlo con el nuevo, contra los
criterios de PLAN-008 §6. Solo lectura sobre `/home/snaven10/revfa`, salvo reindexar
`REVFA_REGISTRO_EXTERIOR` (B8) con aprobación.

## Contexto verificado

- "A rebuilt binary does not take effect until the server restarts" (`AGENTS.md`): `devctx serve
  --stop` en cada repo antes de medir; las sesiones MCP 0.8.2 vivas siguen reteniendo locks hasta
  cerrarse (PLAN-008 §7).
- Baselines de PLAN-008 §1.1.

## Pasos

- [ ] **B1/B1c.** MCP de prueba lanzado desde una copia del binario que se borra; `serve --stop`;
      tool → error explícito; desde otra terminal `devctx search` en el mismo repo → OK (antes: lock).
      Tiempo hasta error con lock ajeno (antes 63 s).
- [ ] **B2.** Sesión MCP, `serve --stop`, `remember` → OK con PID nuevo.
- [ ] **B3.** Commit en un worktree de prueba de este repo con hooks instalados, borrar el worktree,
      `read_symbol` → OK; `/proc/<serve>/environ` sin `GIT_DIR`.
- [ ] **B4.** REVFA_FrontEnd en una rama no indexada: `read_symbol("AuthService")`,
      `get_references("authHttpInterceptor")`, `search_routes` → resultados + `branch_fallback`.
- [ ] **B5.** `recall` MCP `scope: all` en revfa → ≥ lo que devuelve la CLI.
- [ ] **B6.** `search "how are memories linked to symbols"` en este repo → 0 duplicados.
- [ ] **B7.** `build_context` para una tarea de FrontEnd en sesión de grupo → repo correcto.
- [ ] **B8.** `devctx status` en `REVFA_REGISTRO_EXTERIOR` → `extractor_stale: true`; tras
      `index --full` (con aprobación) → `false` y 0 nodos basura de Mutiny.
- [ ] **B9.** `cargo test -p devctx-cli` y conteo de serves vivos después.
- [ ] **P1.** Tokens de `search_routes` sin `path` y `plan_status` sin filtro en revfa, antes/después.

## Criterios de aceptación

- [ ] Cada fila de PLAN-008 §6 con su número real; desviaciones explicadas.
- [ ] `git status` limpio en los repos de revfa tocados (solo lectura).

## Resultado

<!-- Contrato: PLAN-008 §11 -->
