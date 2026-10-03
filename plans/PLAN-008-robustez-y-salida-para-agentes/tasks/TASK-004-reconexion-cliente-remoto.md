# TASK-004 — Reconexión del cliente remoto ante un serve muerto

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Que una sesión MCP sobreviva a que su serve salga (`--idle 900`, `serve --stop`, crash): en el
primer fallo de conexión **antes de enviar**, redescubrir/relanzar el serve y reintentar una sola
vez (PLAN-008 B2).

## Contexto verificado

- `crates/devctx-mcp/src/backend.rs:27-30` — `RemoteClient { base, token }` inmutable.
- `backend.rs:138` — `Backend::remote`, creado una vez desde `lib.rs:1266-1281`.
- `backend.rs:59-94` — `get_timed`/`post_timed`; `backend.rs:97-115` — `read()`: un
  `ureq::Error::Transport` se vuelve `String` sin distinguir connect vs. read.
- TASK-001 deja una closure de conexión en el backend perezoso: esta task la reusa.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/backend.rs`

## Pasos

- [ ] **Paso 1.** `RemoteClient` guarda `base`/`token` en `RwLock` y una closure
      `reconnect: Arc<dyn Fn() -> Result<(String, Option<String>), String> + Send + Sync>`.
- [ ] **Paso 2 — Política (pura y testeable).** `fn should_retry(err: &ureq::Error) -> bool`:
      `true` solo para fallos de transporte **antes de enviar** (`ConnectionFailed`, connection
      refused, DNS/connect); `false` para timeout de lectura, conexión cortada tras enviar y
      cualquier `Error::Status`.
- [ ] **Paso 3.** En `get_timed`/`post_timed`: si `should_retry`, llamar `reconnect`, actualizar
      `base`/`token`, reintentar **una** vez. Aplica a todas las rutas, incluida `remember` (el
      request no llegó). El error final incluye ambas causas.

## Criterios de aceptación

- [ ] Tests unitarios de `should_retry` para cada clase de error (connect refused → sí; timeout de
      lectura → no; 500 → no; 404 → no).
- [ ] Test `mcp_binding`: sesión MCP, `remember` OK, `devctx serve --stop` en el proyecto,
      `remember` otra vez → OK, y el PID en `serve.json` es distinto del primero.
- [ ] Ninguna tool hace más de un reintento (contar en el test vía `serve.log` o un contador).

## Riesgos

Reintentar después de enviar duplicaría escrituras: la política lo prohíbe y los tests lo fijan.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
