# TASK-005 — Entorno de git limpio en hooks, serve y subprocesos `git`

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

---

## Objetivo

Que ninguna variable `GIT_*` exportada por git a un hook llegue al serve ni a los `git` que corre
devctx (PLAN-008 B3), en dos capas: el hook las limpia, y devctx las limpia igual por si el hook es
viejo o lo lanzó otra cosa.

## Contexto verificado

- `crates/devctx-cli/src/hooks.rs:63-76` — `block()`: `("<exe>" index >/dev/null 2>&1 &) || true`.
  Hooks gestionados: `post-commit`, `post-merge` (`hooks.rs:33`). Tests del bloque en
  `hooks.rs:229-274`.
- Spawns del serve: `crates/devctx-cli/src/remote.rs:152-168` (`spawn_server`),
  `crates/devctx-central/src/client.rs:210-…` (central).
- `Command::new("git")`: `crates/devctx-index/src/git.rs:223` (`run`; `GitRepo::open` en `:43`),
  `crates/devctx-core/src/config.rs:453` (`main_worktree`) y `:481` (`detect_default_branch`),
  `crates/devctx-index/src/lib.rs:53`, `crates/devctx-cli/src/main.rs:3141`,
  `crates/devctx-api/src/central.rs:188`, `crates/devctx-cli/src/hooks.rs:37`.
- Re-ejecuciones del binario desde el MCP: `crates/devctx-mcp/src/state.rs:1691`, `:1928`, `:2079`,
  `:2817`.
- Variables a quitar: `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_PREFIX`,
  `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`. Las `GIT_AUTHOR_*`/
  `GIT_COMMITTER_*` son inocuas para lectura pero se quitan también del serve (proceso de larga vida).

## Archivos

- **Modificar:** `crates/devctx-cli/src/hooks.rs` (bloque)
- **Crear:** helper `devctx_core::clean_git_env(&mut Command)` (o en un módulo compartido)
- **Modificar:** cada sitio de la lista de arriba

## Pasos

- [ ] **Paso 1 — Hook.** El bloque pasa a
      `(env -u GIT_DIR -u GIT_INDEX_FILE -u GIT_WORK_TREE -u GIT_PREFIX "<exe>" index >/dev/null 2>&1 &) || true`.
      `devctx hooks install` reescribe bloques viejos (ya reemplaza el bloque gestionado). Actualizar
      `the_block_runs_index_detached_and_never_fails_a_commit`.
- [ ] **Paso 2 — Helper.** `clean_git_env` hace `env_remove` de la lista. Aplicarlo en todos los
      `Command::new("git")` y en todo spawn del binario propio (serve, central, re-ejecuciones del MCP).
- [ ] **Paso 3 — Worktree.** Test que fija qué `HEAD` se indexa cuando el `post-commit` corre desde un
      worktree enlazado (ver PLAN-008 Q-2): con el entorno limpio, `devctx index` desde el worktree
      resuelve al proyecto principal e indexa el `HEAD` del principal. Documentarlo en el Resultado.

## Criterios de aceptación

- [ ] Test de integración: `devctx index` con `GIT_DIR=/nonexistent/.git/worktrees/x` y
      `GIT_INDEX_FILE=/nonexistent/index` en el entorno → el serve hijo arranca; en Linux,
      `/proc/<pid del serve>/environ` no contiene `GIT_DIR` ni `GIT_INDEX_FILE`; y la primera
      `read_symbol`/`index_status` contra ese serve responde sin error de git.
- [ ] Test unitario de `clean_git_env` (las variables no llegan a un hijo `env`).
- [ ] Test del bloque del hook actualizado y verde.
- [ ] Criterio explícito de `HEAD` desde worktree, con su test (paso 3).

## Riesgos

`env -u` no existe en algunos `sh` mínimos (busybox sí lo trae; macOS sí). Si `env` falta, el hook
falla en silencio (`|| true`) y la capa 2 sigue protegiendo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
