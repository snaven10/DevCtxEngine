# TASK-008 — `recall` federado: exe propio robusto y errores con ruta

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `done`

---

## Objetivo

Que `recall` por MCP con `scope: all`/`local` en un grupo devuelva lo mismo que la CLI, y que si un
miembro falla el error diga qué ruta y qué comando (PLAN-008 B5).

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:1923` — `recall_one_local` re-ejecuta el binario con
  `std::env::current_exe()` (`state.rs:1928`) y `current_dir(path)`.
- Mismo patrón: `search_one` (`state.rs:1691`), `do_search_project` (`state.rs:2079`),
  `move_to_project` (`state.rs:2817`).
- `state.rs:1905-1919` — mensajes "None of the {members} repositories…" / "INCOMPLETE: …" con
  `First failure: {first_error}` sin ruta.
- Hipótesis: en un MCP con binario reemplazado `current_exe()` = `"…/devctx (deleted)"` → ENOENT =
  `No such file or directory (os error 2)`. Alternativa a descartar: `path` de un miembro que ya
  no existe en disco (`current_dir` inexistente también da ENOENT).
- TASK-001 provee `devctx_core::self_exe()`.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs`

## Pasos

- [x] **Paso 1 — Repro.** Confirmar la causa: MCP lanzado desde una copia del binario que luego se
      borra → `recall scope:all` en un grupo de 2 miembros. Y por separado: miembro registrado cuya
      ruta no existe. Anotar cuál reproduce el mensaje de campo.
- [x] **Paso 2.** Los cuatro sitios usan `self_exe()` y el helper de entorno limpio de TASK-005 si ya
      está (si no, dejar el `TODO` con el id de task).
- [x] **Paso 3.** Cada fallo por miembro se formatea como `<nombre> (<ruta>): <error>`; si la ruta no
      existe, decirlo antes de intentar el spawn. `First failure` pasa a listar hasta 3 fallos.
- [x] **Paso 4.** Miembros cuya ruta no existe se saltan con aviso (`skipped_missing: [...]`) y no
      cuentan como "unreachable".

## Criterios de aceptación

- [x] Test (unitario sobre el formateo + integración con un grupo de 2): un miembro con ruta borrada
      → la respuesta trae las memorias del otro y `skipped_missing` con la ruta.
- [x] Test: MCP con binario borrado → `recall scope:all` funciona.
- [x] El test existente `assert!(err.contains("None of the 11"))` (`state.rs:3580`) se adapta sin
      perder lo que fija.

## Riesgos

Ninguno nuevo; cambia el texto de errores (contrato no estructurado).

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro:** sin repro manual contra el binario instalado; cubierto por
   `group_recall_works_from_an_mcp_whose_binary_was_deleted` (MCP lanzado desde una copia que se borra tras el handshake; `recall scope:all` en grupo de 2). Antes: `None of the 2 repositories ... No such file or directory (os error 2)`; después: devuelve las memorias de ambos miembros.
3. **Causa raíz confirmada:** `std::env::current_exe()` = `".../devctx (deleted)"` en los 4 sitios de fan-out de `state.rs` (ENOENT del spawn). La alternativa (ruta de miembro inexistente) también da ENOENT, pero el descenso del registro ya descarta rutas que no canonicalizan al enlazar; solo ocurre si el checkout desaparece con la sesión ya enlazada (ahora manejado con `skipped_missing`).
4. **Archivos/símbolos:** `devctx-mcp/src/state.rs`: nuevos `run_in_member(member, path, args) -> Result<Output, String>`, `child_failure`, `split_missing`; `search_one` y `recall_one_local` ganan el parámetro `member: &str`; `fan_out_warning` lista hasta 3 fallos (`Failures: ...`, antes `First failure:`). `self_exe()` también en `devctx-cli/src/hooks.rs` (con error si resuelve a `/proc/self/exe`), `devctx-cli/src/main.rs` (`mcp configure`), `devctx-api/src/central.rs`, `devctx-tui/src/lib.rs` (x2). Se dejan `current_exe()` a propósito en `lifecycle.rs` (detecta el sufijo `(deleted)`), `models.rs` (self-update, no re-ejecuta) y tests.
5. **Tests:** unitarios `the_refusal_lists_up_to_three_failures`, `a_missing_member_path_is_named_before_any_spawn` (y los 4 existentes de `fan_out_warning` intactos); integración en `mcp_binding.rs`: `group_recall_local_and_all_return_the_members_memories`, `group_recall_skips_a_member_whose_path_is_gone`, `group_recall_works_from_an_mcp_whose_binary_was_deleted`. `cargo test -p devctx-cli --test mcp_binding --test mcp_tools --test plan_status_cli --test search_shapes_cli --test git_env_cli` y `-p devctx-mcp -p devctx-core -p devctx-api -p devctx-tui`: verde.
6. **Contrato JSON:** `recall`/`search` de grupo: nuevo `skipped_missing: [{project, path}]`; `failed_projects[].error` ahora empieza por `<miembro> (<ruta>): `; el texto de `warning`/error pasa de `First failure:` a `Failures:` (hasta 3, `| ` separados, `(and N more)`). Miembros ausentes no cuentan como inalcanzables.
7. **M-6:** el hijo ya pasa por el serve (`ensure_cli` en la CLI); solo abre el DuckDB local si `DEVCTX_NO_AUTOSERVE` o fallo de spawn, tras `check_unlocked`; es seguro porque el MCP ya no sostiene ningún `Store`. El hijo recibe `clean_git_env` en todos los sitios. Documentado en el doc de `run_in_member`.
8. **No verificado:** el ejecutable realmente reinstalado en `~/.local/bin` (prohibido tocarlo); hook con `/proc/self/exe` sin test dedicado; `models.rs` self-update con binario borrado.

### Fixup D1 (review)

- Con TODOS los miembros ausentes en disco (`present` vacío, `skipped_missing` no) no había
  `warning` y la respuesta salía solo del tier compartido sin avisar. Nuevo `all_missing_warning`
  en `devctx-mcp/src/state.rs`, usado por el fan-out de `search` y de `recall` (solo cuando se
  consultó el tier local). Test: `all_members_missing_is_said_out_loud`.
