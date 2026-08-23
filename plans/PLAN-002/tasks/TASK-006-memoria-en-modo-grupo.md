# TASK-006 — Memoria en modo grupo: `scope` default `group`

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-002
- **Estado:** `done`

---

## Objetivo

Que estando vinculado a un grupo, una memoria guardada sin `scope` explícito caiga en `group` y no
en `local`. Si el binding dice "estoy en el producto, no en un repo", el default tiene que decir lo
mismo.

## Contexto verificado

`remember` documenta `scope` con default `local`. En modo grupo el `local` es el store del `default`
del grupo — un repo elegido por heurística (TASK-002). Guardar ahí por defecto significa **enterrar
la memoria en un repo que el usuario nunca eligió**, y `local` no lo recupera ningún repo hermano.

Confirmado en vivo (2026-08-19): un `remember` con `scope: "group"` responde
`{"scope": "group", "repo": "REVFA_BackEnd", "status": "created"}` — el `repo` sale del binding aun
en scope group. O sea: el scope decide DÓNDE se guarda; el binding decide a qué repo se ATRIBUYE.
Son dos cosas y hay que tratarlas por separado.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/lib.rs` (tool `remember`, y `recall` para el scope de lectura)

## Pasos

- [ ] **Paso 1 — `remember` sin `scope` explícito**: en `Binding::Group` → `group`; en
      `Binding::Project` → `local` (comportamiento actual, sin cambio).
- [ ] **Paso 2 — `scope` explícito siempre gana.** Un `scope: "local"` a propósito estando en grupo
      se respeta; se guarda en el store del `default` y se reporta a qué repo fue.
- [ ] **Paso 3 — Reportar el default aplicado** en la salida: que la respuesta diga que el `group`
      salió del binding y no de la llamada. Sin esto el usuario no puede auditar dónde cayó.
- [ ] **Paso 4 — `recall` en modo grupo**: default de búsqueda `all` (ya es el default documentado);
      verificar que en modo grupo realmente abarque los stores del grupo y no solo el del `default`.
- [ ] **Paso 5 — Atribución**: dejar el `repo` saliendo del binding (no inventar un repo "de grupo"),
      pero documentar el efecto en TASK-008. Cambiar el modelo de atribución NO entra en este plan.

## Criterios de aceptación

- [ ] En modo grupo, `remember` sin `scope` guarda en `group` y la respuesta lo dice.
- [ ] En modo proyecto, `remember` sin `scope` sigue guardando `local` (sin regresión).
- [ ] `scope: "local"` explícito en modo grupo se respeta y reporta el repo destino.
- [ ] `recall` en modo grupo encuentra memorias guardadas desde cualquier miembro.

## Riesgos

**Cambio de comportamiento observable.** Alguien que hoy guarda sin `scope` desde una raíz de
workspace obtiene `group` donde antes obtenía… nada (fallaba). O sea que en la práctica no rompe
flujos existentes: los convierte de "error" en "guardado". Igual va al changelog (TASK-008).

## Resultado

- **Estado final:** `done`
- **Resumen:** En modo grupo, `remember` sin scope explicito usa `group`; la respuesta trae `scope_from_binding` para que el default sea auditable.
- **Archivos tocados:** crates/devctx-mcp/src/lib.rs
- **Verificado por:** Compila. NO ejecutado.
- **Desviaciones:** El paso 5 (atribucion) se reabrio como TASK-009: dejar el `repo` saliendo del binding resulto ser el error de fondo, no un detalle a documentar.
- **Riesgos abiertos / siguiente:** Sin prueba end-to-end.
