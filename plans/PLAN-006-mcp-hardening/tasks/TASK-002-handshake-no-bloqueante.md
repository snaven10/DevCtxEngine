# TASK-002 — El chequeo de actualización no bloquea el handshake de `devctx mcp`

- **Plan:** PLAN-006 — Endurecimiento del MCP
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-hardening`
- **Depende de:** — (independiente)
- **Estado:** `done`

---

## Objetivo

`devctx mcp` corría el chequeo de actualización (con timeout de red de 4s) antes de empezar a
servir, demorando el `initialize` de todo cliente MCP. Sacarlo del camino del handshake sin
perder el comportamiento para comandos interactivos de la CLI.

## Contexto verificado

- `crates/devctx-cli/src/main.rs`, `cmd_mcp` (línea ~1363, antes del cambio): llamaba
  `update_check::available(RELEASE_REPO, env!("CARGO_PKG_VERSION"))` en línea recta, antes de
  resolver el `binding` del proyecto y de que el servidor empiece a leer stdio.
- `crates/devctx-cli/src/update_check.rs:57-69` (`fetch_latest`): `ureq::AgentBuilder::new()
  .timeout(Duration::from_secs(4))` — ese es el peor caso que se sumaba al arranque, una vez por
  día (`CHECK_EVERY`, `update_check.rs:15`) o siempre que no hay caché.
- El resultado se guarda en la variable de entorno `DEVCTX_UPDATE_AVAILABLE`
  (`main.rs:1368` antes del cambio) y se lee, perezosamente, en `do_list_projects`
  (`crates/devctx-mcp/src/state.rs:2066`) — nada depende de tenerlo listo en el instante cero.
- El otro call site, en el comando interactivo (`main.rs:1058`), se deja intacto: un comando de
  CLI que termina y sale no tiene "después del handshake" al que mover el chequeo.

## Cambios

- `cmd_mcp`: el chequeo pasa a `std::thread::spawn(|| { … })`, seteando la misma variable de
  entorno cuando el hilo termina. El servidor arranca a servir sin esperarlo.

## Riesgos

- **Carrera con el primer `list_projects`.** Si ese hilo no terminó todavía, la variable no está
  seteada y la respuesta es la misma que "sin novedades" — idéntico al comportamiento actual antes
  de que el chequeo original terminara. No hay pérdida de información, solo un resultado que llega
  un poco más tarde.
- **`std::env::set_var` desde otro hilo.** No hay lector concurrente de esa variable en el camino
  crítico (solo se lee una vez por llamada a `do_list_projects`, que hace su propia lectura con
  `.ok()`), así que la única condición de carrera posible es "leer antes de que el hilo escriba",
  ya cubierta arriba.

## Verificación

Tiempo de respuesta a un `initialize` de JSON-RPC sobre stdin, antes y después del cambio —
números en el cierre del plan (PLAN-006 §6) y en el reporte final.

## Resultado

- **Estado final:** `done`
- **Resumen:** El chequeo de actualización de `devctx mcp` corre en un hilo aparte; el servidor
  empieza a servir sin esperarlo. El comando interactivo de CLI sigue chequeando en línea recta.
- **Archivos tocados:** `crates/devctx-cli/src/main.rs` (`cmd_mcp`).
- **Verificado por:** medición de `initialize` sobre stdin antes/después (ver cierre del plan).
- **Desviaciones:** ninguna respecto al enfoque planteado por el usuario ("saltarlo del todo en
  modo mcp, o correrlo después de servir") — se optó por la segunda opción porque conserva el
  chequeo diario sin agregar una rama de comportamiento nueva.
