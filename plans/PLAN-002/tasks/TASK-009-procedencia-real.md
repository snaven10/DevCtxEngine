# TASK-009 — Procedencia real: `repo` = grupo en modo grupo; `local` exige proyecto

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-002
- **Estado:** `done`

> **Nota de origen.** Esta tarea figuraba en la tabla del PLAN-002 sin archivo
> propio: nunca se escribió. Se redacta el 2026-08-19 **a partir del código ya
> implementado**, no al revés. Lo que sigue describe lo que hace el código hoy,
> verificado leyéndolo, más los tests que faltaban.

---

## Objetivo

Que una memoria escrita desde un binding de grupo diga la verdad sobre de dónde
vino, y que `scope: local` no tenga forma de archivarse en ningún lado sin que
alguien haya elegido cuál.

## Por qué

Son dos preguntas distintas que es fácil confundir en una:

- **Dónde se escribe** una memoria — la responde `scope`.
- **A quién se le atribuye** — la responde `repo`.

Con un binding de grupo la primera es "el producto". La segunda **no puede ser
un miembro**: elegir uno inventa una procedencia que nadie declaró, y seis meses
después alguien lee que la decisión salió de `api` cuando salió del producto.

Y `scope: local` significa "el store de este repositorio". En un binding de
grupo no hay tal repositorio. Elegir uno archiva la memoria donde nadie la
eligió y donde nadie la va a buscar — el mismo modo de falla silencioso que este
plan existe para cerrar.

## Qué hace el código (verificado 2026-08-19)

**1. Procedencia** — `crates/devctx-mcp/src/lib.rs`, en `remember`:

```rust
let (backend, resolved) = self.backend_for(req.project.as_deref())?;
let provenance = match (&group_binding, &resolved) {
    (Some(group), None) => Some(group.clone()),
    _ => None,
};
```

El grupo se usa como procedencia **solo si no hubo `project`**. Un hint explícito
nombra el repositorio, y entonces ese repositorio *es* la procedencia real — el
override de grupo sería mentir en la otra dirección.

Baja por `backend.remember(..., provenance)` → `do_remember_shared(..., provenance)`
→ `state.rs:1693`, donde reemplaza el `repo` que saldría de git.

**2. La guarda de `local`** — mismo archivo, antes de escribir:

```
This session is bound to group `X`, not to one repository, so `scope: local`
has no store to write to. Either name the project with `project` (a registered
name, or a path inside it), or use `scope: group` to record this for the whole
product.
```

Falla en vez de adivinar, y el mensaje trae **las dos salidas**, no solo el
diagnóstico.

## Archivos

- **Ya implementado:** `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/backend.rs`, `crates/devctx-mcp/src/state.rs`
- **Modificado:** `crates/devctx-cli/tests/mcp_binding.rs` — los tests que faltaban

## Pasos

- [x] **Paso 1** — Procedencia de grupo cuando no hay `project`.
- [x] **Paso 2** — Un `project` explícito gana: la procedencia es ese repositorio.
- [x] **Paso 3** — `scope: local` sin `project` en modo grupo → error con las dos salidas.
- [x] **Paso 4** — Tests de los tres comportamientos.

## Criterios de aceptación

- [x] En modo grupo, `remember` sin `project` atribuye al grupo.
- [x] En modo grupo, `remember` con `project` atribuye a ese repositorio.
- [x] En modo grupo, `scope: local` sin `project` falla, y el error nombra las dos salidas.
- [x] En modo proyecto nada de esto cambia.
- [x] La suite sigue verde.

## Riesgos

`Backend::Remote` no reenvía `provenance` en su `POST /remember` — solo
`content/title/type/topic/tags/scope/files`. Con un backend remoto la
atribución de grupo se pierde en silencio. **No se arregló acá**: queda anotado
como límite conocido, porque tocarlo cambia el contrato HTTP y eso es otra
tarea.

## Resultado

**Estado final:** `done` (2026-08-19)

La implementación ya estaba; lo que faltaba era el archivo de tarea y los tests.
Se agregaron 2 tests a `mcp_binding.rs` cubriendo los tres comportamientos.

**Límite conocido, sin arreglar:** `Backend::Remote` pierde la procedencia. Un
binding de grupo contra un servidor remoto atribuye la memoria a lo que diga
git, no al grupo. Merece su propia tarea porque cambia el contrato del endpoint.
