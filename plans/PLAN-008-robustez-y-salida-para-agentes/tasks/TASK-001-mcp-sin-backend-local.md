# TASK-001 — El MCP nunca abre el DuckDB: backend remoto perezoso, `ensure` que detecta muerte, `serve.log`, exe `(deleted)`

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: opus para el diseño del backend perezoso, sonnet para el resto)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** —
- **Estado:** `pending`

---

## Objetivo

Que un proceso `devctx mcp` no pueda retener nunca el archivo DuckDB de un proyecto (PLAN-008 B1,
D1), y que cuando no hay servidor el error llegue en ~1-2 s con la causa real en vez de 60 s de
espera seguidos de un `Backend::Local` que bloquea a todos los demás.

## Contexto verificado

- `crates/devctx-cli/src/main.rs:1563-1577` — `mcp_backend`: `remote::ensure` → `None` →
  `remote::reclaim_db(&cfg)` (mata el serve anunciado) → `devctx_mcp::backend_for(cfg, None)`.
- `crates/devctx-mcp/src/lib.rs:1266-1281` — `backend_for(cfg, None)` → `Backend::local(AppState::build(cfg)?)`.
- `crates/devctx-mcp/src/state.rs:205` — `Store::open`; `state.rs:167-171` — `primary: Arc<Mutex<Store>>`
  vive lo que el proceso.
- Caches del backend: `Binding::Project` (`lib.rs:376`), `Binding::Group.default` (`lib.rs:384`),
  `hinted` (`lib.rs:410`, `lib.rs:492-512`), `use_project` (`lib.rs:904`). Todos guardan
  `Arc<Backend>` y por eso no pueden "cambiar" de local a remoto.
- `crates/devctx-cli/src/remote.rs:129-148` — `ensure`: 200 × 300 ms sin mirar si el hijo murió.
- `remote.rs:152-168` — `spawn_server`: stderr a `/dev/null` (`:157-159`), `current_exe()` (`:153`).
- `remote.rs:259-279` — `discover`: health-check de 400 ms; un serve ocupado se da por muerto.
- `main.rs:1242-1262` — `cmd_serve`: `reclaim_db` (`:1247`) **antes** de escribir `serve.json`
  (`:1253`) y de abrir el store (`run_blocking`, `:1258`).
- Patrón a copiar: `crates/devctx-central/src/client.rs:128-157` (`ensure` con `exited`),
  `:170-208` (`serve.log` y lectura de la causa).
- Los 24 métodos de `Backend` tienen brazo `Remote` (`rg -c "Backend::Remote\(" backend.rs` = 24).
- En Linux, `std::env::current_exe()` lee `/proc/self/exe`; con el binario reemplazado devuelve
  `"<ruta> (deleted)"` y `Command::new` falla con ENOENT.

## Archivos

- **Modificar:** `crates/devctx-cli/src/remote.rs` (`ensure`, `spawn_server`, `discover`, helper de log)
- **Modificar:** `crates/devctx-cli/src/main.rs` (`mcp_backend`, `cmd_serve`)
- **Modificar:** `crates/devctx-mcp/src/backend.rs` (variante remota perezosa)
- **Modificar:** `crates/devctx-mcp/src/lib.rs` (`backend_for` del MCP sin rama local)
- **Crear o modificar:** helper `self_exe()` en `crates/devctx-core` (lo reusa TASK-008)
- **Tests:** `crates/devctx-cli/tests/mcp_binding.rs`

## Pasos

- [ ] **Paso 1 — `self_exe()`.** `devctx_core::self_exe() -> io::Result<PathBuf>`: `current_exe()`;
      si la ruta termina en ` (deleted)` o no existe, usar la ruta sin el sufijo si existe (el binario
      instalado nuevo); si tampoco, `/proc/self/exe` (sigue ejecutando el viejo). Test unitario sobre
      la función pura de normalización de la cadena.
- [ ] **Paso 2 — `serve.log` del proyecto.** `spawn_server` manda stderr a
      `.devctx/state/serve.log` (append, truncado a un tamaño razonable), usa `self_exe()`, y devuelve
      un handle para saber si el hijo salió (hilo con `wait()`, como `client.rs`).
- [ ] **Paso 3 — `ensure` con causa.** Devuelve `Result<Remote, EnsureError>` (o equivalente) en vez
      de `Option`: corta en cuanto el hijo sale y adjunta la última línea útil de `serve.log`
      (lock + PID, puerto tomado, config inválida). Mantener una función `ensure_opt` para los
      callers CLI que hoy usan `Option` hasta TASK-002.
- [ ] **Paso 4 — `discover` tolerante.** Si `serve.json` existe y su PID está vivo, un `/health` lento
      no lo convierte en muerto: reintentar con timeout mayor (p. ej. 3 s) y, si sigue sin contestar,
      devolver "ocupado" — nunca spawnear otro ni matarlo. "PID vivo" significa que el proceso existe
      **y** su cmdline (`/proc/<pid>/cmdline`) es un `devctx serve`: los PIDs se reutilizan, y un PID
      reciclado por otro programa no puede contar como serve vivo.
- [ ] **Paso 5 — `cmd_serve`.** Abrir el store (o al menos tomar el lock) **antes** de escribir
      `serve.json`; si falla, escribir la causa a stderr (→ `serve.log`) y salir con código ≠ 0.
      `reclaim_db` al arrancar solo cuando el serve fue lanzado a mano (no auto-spawn), o eliminarlo
      del arranque: decidir y justificar en el Resultado.
- [ ] **Paso 6 — Backend remoto perezoso.** Nueva forma de `Backend::Remote` que guarda el
      `ProjectConfig`/identidad y una closure de conexión; la conexión se intenta en la primera
      llamada y se reintenta en las siguientes si no la hay. Sin servidor → error
      `"no devctx server for <proyecto>: <causa del serve.log>. Probá `devctx serve` en <ruta>"`.
- [ ] **Paso 7 — `mcp_backend` sin local.** Quitar `reclaim_db` y la rama `None → backend_for(cfg,
      None)`; el MCP siempre construye el backend perezoso. `devctx_mcp::backend_for(cfg, None)`
      queda solo para quien de verdad es dueño (tests internos / web), o se elimina si nadie más lo usa.

## Criterios de aceptación

- [ ] Test `mcp_binding`: **puerto tomado por un listener mudo** (el test abre un `TcpListener` en el
      puerto de `auto_addr` del proyecto o escribe un `serve.json` que apunta a él) → la tool devuelve
      error explícito; a continuación el test abre el DB (p. ej. `devctx status` con
      `DEVCTX_NO_AUTOSERVE=1`) sin error de lock.
- [ ] Test `mcp_binding`: **lock ajeno** (el test lanza `devctx serve --addr <libre>` a mano y borra su
      `serve.json`) → la tool vuelve en < 2 s con un error que contiene el PID del dueño.
- [ ] Con el binario reemplazado bajo un MCP vivo (simulable en test copiando el binario a un tmp,
      lanzando el MCP desde la copia y borrándola), el MCP sigue pudiendo lanzar un serve.
- [ ] `rg -n "reclaim_db" crates/devctx-cli/src/main.rs` no muestra el camino del MCP.
- [ ] Ningún test existente de `mcp_binding`/`mcp_tools` cambia de comportamiento salvo los que
      dependían del fallback local (listarlos en el Resultado).

## Riesgos

Sin fallback local, un serve que no arranca deja las tools caídas en esa sesión; el error tiene que
ser accionable (causa + comando). `serve.log` puede crecer: truncar.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
