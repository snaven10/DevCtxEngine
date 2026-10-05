# TASK-005 — Entorno de git limpio en hooks, serve y subprocesos `git`

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `done`

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

- [x] **Paso 1 — Hook.** El bloque pasa a
      `(unset GIT_DIR … ; "<exe>" index >/dev/null 2>&1 &) || true` (ver Resultado: `unset`, no `env -u`).
      `devctx hooks install` reescribe bloques viejos (ya reemplaza el bloque gestionado). Actualizar
      `the_block_runs_index_detached_and_never_fails_a_commit`.
- [x] **Paso 2 — Helper.** `clean_git_env` hace `env_remove` de la lista. Aplicarlo en todos los
      `Command::new("git")` y en todo spawn del binario propio (serve, central, re-ejecuciones del MCP).
- [x] **Paso 3 — Worktree.** Test que fija qué `HEAD` se indexa cuando el `post-commit` corre desde un
      worktree enlazado (ver PLAN-008 Q-2): con el entorno limpio, `devctx index` desde el worktree
      resuelve al proyecto principal e indexa el `HEAD` del principal. Documentarlo en el Resultado.

## Criterios de aceptación

- [x] Test de integración: `devctx index` con `GIT_DIR=/nonexistent/.git/worktrees/x` y
      `GIT_INDEX_FILE=/nonexistent/index` en el entorno → el serve hijo arranca; en Linux,
      `/proc/<pid del serve>/environ` no contiene `GIT_DIR` ni `GIT_INDEX_FILE`; y la primera
      `read_symbol`/`index_status` contra ese serve responde sin error de git.
- [x] Test unitario de `clean_git_env` (las variables no llegan a un hijo `env`).
- [x] Test del bloque del hook actualizado y verde.
- [x] Criterio explícito de `HEAD` desde worktree, con su test (paso 3).

## Riesgos

`env -u` no existe en algunos `sh` mínimos (busybox sí lo trae; macOS sí). Si `env` falta, el hook
falla en silencio (`|| true`) y la capa 2 sigue protegiendo.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes (verificado quitando el `clean_git_env` de `spawn_server`):
   `cargo test -p devctx-cli --test git_env_cli spawned` -> `server inherited GIT_DIR` (fallo). Después: verde.
   Con `GIT_DIR`/`GIT_INDEX_FILE`/`GIT_PREFIX`/`GIT_AUTHOR_NAME` inválidos en el entorno de `devctx index`,
   el `/proc/<pid>/environ` del serve hijo no contiene ninguna.
3. **Causa raíz:** confirmada (§2 B3): el serve auto-lanzado heredaba el entorno que git exporta al hook.
   Decisión de portabilidad: el hook usa `unset` (builtin POSIX, dentro del subshell) en vez de `env -u`;
   no depende del binario `env` (busybox/macOS/dash lo cumplen igual) y no puede fallar el commit.
4. **Archivos y símbolos.**
   - Nuevo `crates/devctx-core/src/gitenv.rs`: `pub fn clean_git_env(cmd: &mut Command) -> &mut Command`,
     `pub const GIT_REPO_ENV` (7 variables estructurales), `pub const GIT_IDENTITY_ENV` (`GIT_AUTHOR_*`,
     `GIT_COMMITTER_*`); reexportados desde `devctx_core`.
   - Aplicado en todo `Command::new("git")` de producción: `devctx-core/src/config.rs`
     (`main_worktree`, `detect_default_branch`), `devctx-index/src/git.rs` (`run`), `devctx-api/src/central.rs`,
     `devctx-cli/src/main.rs` (`is_git_repo`), `hooks.rs` (`hooks_dir`); y en todo spawn del propio binario:
     `remote.rs::spawn_server`, `devctx-central/src/client.rs::spawn`, `devctx-api/src/central.rs::index_project`,
     y las 4 re-ejecuciones de `devctx-mcp/src/state.rs` (siguen con `current_exe()`: es de TASK-008).
   - `hooks.rs::block`: `(unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES; "<exe>" index >/dev/null 2>&1 &) || true`.
   - Los `Command::new("git")` dentro de `#[cfg(test)]` (p. ej. `devctx-index/src/lib.rs`) no se tocaron.
5. **Tests nuevos:** `devctx-core` `gitenv::tests::cleaned_vars_do_not_reach_the_child`;
   `devctx-cli` `hooks::tests::the_block_unsets_git_repo_variables_before_running_index`
   (y `the_block_runs_index_detached_and_never_fails_a_commit` sigue verde);
   `crates/devctx-cli/tests/git_env_cli.rs`: `spawned_server_does_not_inherit_hook_git_variables`,
   `index_from_a_linked_worktree_indexes_the_main_worktree_head`. Resultados (`cargo test --no-fail-fast -p devctx-core -p devctx-index -p devctx-api -p devctx-central -p devctx-mcp -p devctx-cli`): 0 fallos; core 71, cli unit 53, `git_env_cli` 2, `mcp_binding` 26, `mcp_tools` 8 (1 ignored, previo), `plan_status_cli` 9, `projects_cli` 10, index 22, mcp 46, central 19+4; clippy `--all-targets` sin warnings; fmt aplicado.
   **HEAD desde un worktree enlazado (Q-2):** el `post-commit` de un worktree enlazado corre `devctx index`
   ahí; `main_worktree` (`--git-common-dir`) resuelve al proyecto principal y se indexa el `HEAD` del
   worktree principal, no el del enlazado. El commit del worktree entra al índice cuando llega a una rama
   trackeada. Fijado por el segundo test (`alpha_marker` de `main` se encuentra; `beta_marker`, commiteado
   solo en la rama del worktree, no).
6. **Contrato JSON:** sin cambios.
7. **No verificado:** el hook real disparado por `git commit` (se probó el texto del bloque y el entorno del
   serve, no un commit con hook instalado); `sh` mínimos distintos de bash/dash; Windows; hooks ya
   instalados (ver abajo). **Hooks viejos:** conservan `index` sin `unset` hasta correr `devctx hooks install`
   en cada repo (reescribe el bloque entre los marcadores `# >>> devctx (managed) >>>`); mientras tanto la
   capa 2 (el serve y los `git` de devctx limpian el entorno solos) los protege. No hay migración automática.
8. **Números:** el test de entorno tarda ~11 s (carga del modelo en el serve), el de worktree ~14 s.

9. **Fixup (review):**
   - **M-7:** `clean_git_env` ya no depende solo de una lista fija: quita todo `GIT_*` del entorno ambiente salvo una allowlist (`GIT_SSH*`, `GIT_TERMINAL_PROMPT`, `GIT_ASKPASS`, `GIT_EXEC_PATH`, `GIT_CONFIG_GLOBAL/SYSTEM/NOSYSTEM`, `GIT_TRACE*`; son de transporte/diagnóstico, no eligen repositorio). Cubre `GIT_CONFIG_PARAMETERS` (git lo exporta a los hooks con `git -c`), `GIT_CONFIG_COUNT/KEY_n/VALUE_n`, `GIT_NAMESPACE`, `GIT_QUARANTINE_PATH`, `GIT_CEILING_DIRECTORIES`. `GIT_REPO_ENV` (y por tanto el `unset` del hook) incorpora los nombres fijos nuevos; los `KEY_n/VALUE_n` solo los limpia devctx. Aplicado también a los dos spawns de `devctx-tui/src/lib.rs`. Test: `a_hook_like_environment_is_stripped_but_transport_settings_stay`.
   - **M-8:** los setups intencionales con `GIT_DIR`/`GIT_WORK_TREE` (p. ej. dotfiles con `--git-dir`) ya NO son respetados por los subprocesos de devctx: cada repo se localiza desde el directorio dado. Documentado en el comentario de `clean_git_env`.
