# TASK-003 — `memory_forget` 404: rutas faltantes en el daemon por-proyecto

- **Plan:** PLAN-006 — Endurecimiento del MCP
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-hardening`
- **Depende de:** — (independiente)
- **Estado:** `done`

---

## Objetivo

El usuario reportó `memory_forget` devolviendo 404 sobre una memoria `group` recién creada, que
`recall` sí encontraba (dos intentos, verificado 2026-08-19). Rastrear la causa real — la hipótesis
de partida era un filtro de scope/proyecto en el delete que las memorias `group` no llevan — y
corregirla, o documentar por qué no reproduce.

## Rastreo

La hipótesis de scope **no se sostiene**:

- `devctx_store::Store::forget_memory` (`crates/devctx-store/src/memory.rs:196-212`): `DELETE …
  WHERE id = ?`. Sin `project`, sin `repo`. Cualquier id se borra desde cualquier store que lo
  tenga, sin importar el scope con el que se guardó.
- El handler del daemon central que expone esto por HTTP, `forget_memory`
  (`crates/devctx-api/src/central.rs:512-518`), llama a exactamente esa misma función, sin scope
  agregado.

La causa real está un nivel más arriba, en el **enrutamiento**, no en el store:

- `crates/devctx-mcp/src/backend.rs:372-377` — `Backend::Remote::memory_forget` hace `r.post(
  "/memory/forget", …)`. `Backend::Remote::memory_move` (línea 380-385) hace lo mismo contra
  `/memory/move`.
- `Backend::Remote` es el camino normal de `devctx mcp`: `mcp_backend`
  (`crates/devctx-cli/src/main.rs:1469-1483`) llama `remote::ensure(&cfg)` primero y solo cae a
  `Backend::Local` si no hay (ni se puede levantar) un daemon por-proyecto — existe precisamente
  para no competir por el único escritor de DuckDB entre varias sesiones MCP.
- El router de ese daemon, `crates/devctx-api/src/lib.rs` función `router()`, **nunca registró
  `/memory/forget` ni `/memory/move`** — ni la ruta ni el `use` de `do_memory_forget`/
  `do_memory_move` estaban en este archivo (sí existen en `devctx-mcp/src/state.rs`, y sí las usa
  `Backend::Local` directamente).

Con la ruta ausente, la petición nunca llega a `do_memory_forget`: cae en el fallback de axum, que
devuelve **404 literal, textual "Not Found"**, no el mensaje de error de la aplicación
("`{id}` is not in this project, and the shared store is unreachable"). Eso explica exactamente lo
reportado: un 404 sin más contexto, sobre una llamada que a todas luces debería haber funcionado.

**No es específico de `group`.** El bug pega sobre cualquier `memory_forget`/`memory_move`
enrutado por `Backend::Remote`, sea la memoria local o compartida — simplemente que las de scope
`group`/`global` necesariamente pasan por red en algún punto (`do_memory_forget` cae a `central()`
cuando no están en el store local), así que son las que más se notan cuando algo de la capa HTTP
falla. Con una memoria `local` que sí vive en el store del proyecto, el mismo `Backend::Remote`
también habría devuelto 404, por la misma razón: la petición nunca sale de axum.

## Cambios

- `crates/devctx-api/src/lib.rs`:
  - `use` agrega `do_memory_forget`, `do_memory_move` (ya existían en `devctx-mcp::state`, no se
    tocaron).
  - Dos rutas nuevas: `.route("/memory/forget", post(memory_forget))` y `.route("/memory/move",
    post(memory_move))`.
  - Dos handlers nuevos, mismo patrón que `memory_refs`/`read_file`: `MemoryForgetBody { id }` y
    `MemoryMoveBody { id, to }` deserializados del cuerpo JSON, delegando a
    `do_memory_forget`/`do_memory_move`.

## Test

`crates/devctx-api/src/lib.rs`, `mod tests` (nuevo): `memory_forget_route_exists_and_forgets_a_real_memory`.
No necesita el modelo de embeddings — siembra una `devctx_store::Memory` directo con
`store.upsert_memory()` (esa función solo toca la tabla `memories`, no el índice vectorial), arma
un `AppState`/`Router` reales apuntando a una base temporal, y llama `POST /memory/forget` a través
de `tower::ServiceExt::oneshot` — HTTP real contra el router real, no una llamada directa a la
función. Se verificó que el test efectivamente detecta la regresión: comentando la línea de la
ruta, el test falla (`resp.status()` es 404, no 200); restaurada la ruta, pasa.

No hizo falta un test `#[ignore]`d por embeddings: la causa no tiene nada que ver con el modelo de
embeddings, así que no hacía falta reproducir con uno.

## Criterios de aceptación

- [x] `POST /memory/forget` y `POST /memory/move` responden a través del router de
      `crates/devctx-api/src/lib.rs` (antes: 404 de axum).
- [x] `store.forget_memory`/`do_memory_forget` sin tocar — el rastreo confirmó que ya eran
      correctos.
- [x] Test de regresión que falla si la ruta vuelve a faltar.

## Resultado

- **Estado final:** `done`
- **Resumen:** el 404 no era un problema de scope de memorias `group`; eran dos rutas HTTP
  (`/memory/forget`, `/memory/move`) que nunca se registraron en el router por-proyecto de
  `devctx-api`, así que toda sesión MCP enrutada por un daemon (el modo normal) caía en el 404
  genérico de axum antes de llegar al código de la aplicación.
- **Archivos tocados:** `crates/devctx-api/src/lib.rs`, `crates/devctx-api/Cargo.toml`
  (`tower` como dev-dependency, para el test de integración con `oneshot`).
- **Verificado por:** `memory_forget_route_exists_and_forgets_a_real_memory` (nuevo,
  `cargo test -p devctx-api --lib`), confirmado que reproduce la falla al comentar la ruta.
- **Desviaciones:** el plan original suponía un problema de scope en el store; el rastreo lo
  descartó con evidencia de código (§ Rastreo) antes de tocar nada ahí.
