# TASK-008 — `recall` federado: exe propio robusto y errores con ruta

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `pending`

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

- [ ] **Paso 1 — Repro.** Confirmar la causa: MCP lanzado desde una copia del binario que luego se
      borra → `recall scope:all` en un grupo de 2 miembros. Y por separado: miembro registrado cuya
      ruta no existe. Anotar cuál reproduce el mensaje de campo.
- [ ] **Paso 2.** Los cuatro sitios usan `self_exe()` y el helper de entorno limpio de TASK-005 si ya
      está (si no, dejar el `TODO` con el id de task).
- [ ] **Paso 3.** Cada fallo por miembro se formatea como `<nombre> (<ruta>): <error>`; si la ruta no
      existe, decirlo antes de intentar el spawn. `First failure` pasa a listar hasta 3 fallos.
- [ ] **Paso 4.** Miembros cuya ruta no existe se saltan con aviso (`skipped_missing: [...]`) y no
      cuentan como "unreachable".

## Criterios de aceptación

- [ ] Test (unitario sobre el formateo + integración con un grupo de 2): un miembro con ruta borrada
      → la respuesta trae las memorias del otro y `skipped_missing` con la ruta.
- [ ] Test: MCP con binario borrado → `recall scope:all` funciona.
- [ ] El test existente `assert!(err.contains("None of the 11"))` (`state.rs:3580`) se adapta sin
      perder lo que fija.

## Riesgos

Ninguno nuevo; cambia el texto de errores (contrato no estructurado).

## Resultado

<!-- Contrato: PLAN-008 §11 -->
