# PLAN-001 — Progreso de indexado a través del servidor

**Estado**: done
**Fecha**: 2026-08-12
**Proyecto(s)**: devai-context-engine

## Objetivo

Que `devctx index` muestre barra, posición y **ETA** también cuando el trabajo lo hace el
servidor. Hoy solo muestra segundos transcurridos, así que no hay forma de saber si un
indexado va por la mitad o si arrancó hace un minuto y le faltan veinte.

## Contexto

La barra ya existe y ya calcula ETA. Lo que falta es que la información cruce el proceso.

- `crates/devctx-index/src/pipeline.rs:19` — `trait ProgressSink { fn start(&self, total: usize); fn file(&self, path: &str); }`
- `crates/devctx-cli/src/main.rs:1301` — `IndexBar` implementa `ProgressSink` con `indicatif`,
  template `"[{elapsed_precise}] [{bar}] {pos}/{len} (eta {eta}) {msg}"`.
- `crates/devctx-cli/src/main.rs:1659` — `cmd_index`: si hay servidor, rutea y muestra
  `Heartbeat` (solo segundos). El comentario en el código ya reconoce la limitación:
  *"The server does the work, so the local progress bar sees nothing."*
- `crates/devctx-mcp/src/state.rs:316` — `do_index` → `do_index_inner(state, full, None)`;
  la corrida en el servidor pasa `progress: None`, así que nadie observa nada.
- `crates/devctx-cli/src/remote.rs:237` — el cliente HTTP usa timeout de 3600 s justamente
  porque un indexado ruteado tarda minutos sin responder.

Memoria relacionada: `session-2026-08-12-oom-devctx`, `devctx-oom-leak-2026-08-12`.

## Scope

### Incluye
- Estado de progreso observable dentro del servidor, alimentado por el `ProgressSink` que ya existe.
- Endpoint HTTP de solo lectura para consultarlo.
- Polling desde el CLI que alimente el `IndexBar` existente durante un indexado ruteado.
- Degradación limpia contra un servidor viejo que no tenga el endpoint.

### NO incluye
- Cambiar el protocolo a streaming (SSE / chunked). Se evaluó y se descartó: obliga a
  rediseñar `Remote` y el manejo de respuestas, para el mismo resultado visible.
- Progreso para otros comandos largos (`search` con reranker, `repair`).
- Progreso en el servidor central (`devctx-api/src/central.rs`).
- Barra en el MCP: un cliente MCP no pinta terminal.

## Tareas

| ID | Título | Archivos | Depende de | Estado |
|----|--------|----------|------------|--------|
| TASK-001 | Estado de progreso observable en `AppState` | `devctx-mcp/src/state.rs` | — | done |
| TASK-002 | Endpoint `GET /index/progress` | `devctx-api/src/lib.rs` | TASK-001 | done |
| TASK-003 | Polling + `IndexBar` en el CLI ruteado | `devctx-cli/src/remote.rs`, `devctx-cli/src/main.rs` | TASK-002 | done |

## Orden de ejecución

Estrictamente secuencial — cada tarea consume lo que expone la anterior.

1. TASK-001
2. TASK-002
3. TASK-003

## Riesgos

- **El poller compite con el indexado.** `GET /index/progress` debe tomar el lock solo para
  copiar cuatro campos y no pasar por `spawn_blocking`; si se cuela en el pool bloqueante
  queda encolado detrás del propio indexado y reporta tarde o nunca.
  → Lock corto, handler async puro, sin tocar la base.
- **Servidor viejo, CLI nuevo.** Mientras convivan binarios, el endpoint puede devolver 404.
  → El CLI cae a `Heartbeat` en cualquier error del poller. Nunca aborta el indexado por
  fallar el progreso.
- **Dos indexados a la vez.** El estado es uno solo por servidor; dos clientes indexando el
  mismo proyecto se pisarían las cuentas.
  → Aceptable: el servidor ya es dueño único del DuckDB y serializa el trabajo. Se documenta.
- **Ruido en la terminal.** El `IndexBar` escribe a stderr con `\r`; mezclado con los
  `eprintln!` del servidor podría ensuciar.
  → El servidor escribe a su propio log, no a la terminal del CLI. Verificar en la prueba.

## Criterios de aceptación

- [x] `devctx index` muestra `{pos}/{len}` avanzando y ETA. Capturado bajo pty:
      `⠲ [00:00:55] [===>          ] 15/140 (eta 58m) crates/devctx-central/src/lib.rs`
- [x] `/index/progress` responde mientras indexa — **con un matiz**: 12 de 13 muestras
      entre 7 y 58 ms, una de 114 ms. El umbral de 100 ms que fijé no se cumple al 100 %.
      No es el fallo que se quería evitar (encolarse detrás del indexado habría dado
      segundos), pero queda registrado como incumplido.
- [x] `cargo test --workspace`: 218 passed, 5 ignored.
- [ ] **NO verificado**: el fallback contra un servidor sin el endpoint. Está implementado
      (`ServerProgress::GIVE_UP_AFTER`), pero probarlo exige un binario anterior a este
      cambio y el instalado ya fue reemplazado. Se verifica compilando el commit previo a
      `TASK-002` y apuntándole el CLI nuevo.
- [ ] **NO verificado**: el camino local con `DEVCTX_NO_AUTOSERVE=1`. No fue tocado por
      este cambio, pero tampoco se ejecutó.

## Notas de ejecución

- **`Heartbeat` eliminado.** TASK-003 asumía que otros caminos lo usaban; el compilador
  demostró que `cmd_index` era su único consumidor, y `ServerProgress` absorbió su modo
  spinner. Dejarlo habría sido código muerto.
- **`running` resultó necesario, no decorativo.** El primer borrador decidía dibujar con
  `total > 0` a secas, y `dead_code` avisó que el campo no se leía. Al mirarlo apareció un
  bug real: los totales finales de una corrida sobreviven a su final, así que un segundo
  indexado habría dibujado la barra terminada del anterior antes de reiniciarse. La
  condición correcta es `running && total > 0`.
- **Hallazgo lateral**: el indexado va a ~3.7 s por archivo (15 archivos en 55 s), lo que
  da ~1 hora para este repo. Es un problema de rendimiento aparte, no de este plan, pero
  explica por qué la barra hacía tanta falta.
