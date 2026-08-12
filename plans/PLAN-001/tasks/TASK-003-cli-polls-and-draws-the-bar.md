# TASK-003 — Polling y barra real en el CLI ruteado

**Plan**: PLAN-001
**Modelo sugerido**: sonnet
**Depende de**: TASK-002
**Estado**: done

## Objetivo

Que `devctx index` con servidor muestre la misma barra con ETA que ya muestra el camino local.

## Archivos afectados

- `crates/devctx-cli/src/remote.rs` — método `index_progress()` en `Remote`.
- `crates/devctx-cli/src/main.rs` — `cmd_index` cambia `Heartbeat` por el poller.

## Pasos

1. En `Remote`, agregar `pub fn index_progress(&self) -> Result<IndexProgress>` que haga
   `GET {base}/index/progress`. **Timeout corto y propio** (1–2 s), no el agente de 3600 s:
   una consulta de progreso que se cuelga no debe dejar la barra congelada sin explicación.
2. En `cmd_index`, reemplazar el `Heartbeat` por un hilo que:
   - cree un `IndexBar`,
   - consulte `index_progress()` cada ~200 ms,
   - la primera vez que vea `total > 0` llame `bar.start(total)`,
   - luego fije la posición a `done` (usar `set_position`, no `inc`: la fuente de verdad es
     el servidor y no queremos que se desincronice) y el mensaje al archivo actual,
   - termine cuando el hilo principal le avise, igual que hace `Heartbeat` hoy con su
     `AtomicBool`.
3. **Fallback**: si las primeras consultas fallan (404 de un servidor viejo, conexión caída),
   el hilo abandona el polling y sigue con el comportamiento actual de `Heartbeat`. El
   indexado nunca falla por culpa del progreso.
4. Conservar el `Heartbeat` en el árbol: sigue siendo el fallback y lo usan otros caminos.

## Criterios de aceptación

- [ ] `devctx index` ruteado muestra `{pos}/{len}` avanzando y ETA que baja.
- [ ] Contra un servidor sin el endpoint, indexa igual y muestra el spinner de siempre,
      sin error visible.
- [ ] La barra se limpia al terminar (`finish_and_clear`) y la salida final queda igual
      que hoy.
- [ ] `DEVCTX_NO_AUTOSERVE=1 devctx index` sigue idéntico.

## Notas / gotchas

- `IndexBar::file()` hace `inc(1)`, lo que sirve para el camino local donde cada llamada es
  un archivo. Para el poller hace falta fijar posición absoluta: agregar un método aparte
  (`set(done, file)`) en vez de reutilizar `file()`, o el conteo se dispara.
- Los 200 ms son para que la barra se sienta viva; con un indexado de minutos no hay razón
  para consultar más seguido.
- Probar contra un repo con suficientes archivos para que la barra alcance a dibujarse: este
  repo con `--full` sirve (109 archivos indexados en el último estado).
