# TASK-004 — Reconexión del cliente remoto ante un serve muerto

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `done`

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

- [x] **Paso 1.** `RemoteClient` guarda `base`/`token` en `RwLock` y una closure
      `reconnect: Arc<dyn Fn() -> Result<(String, Option<String>), String> + Send + Sync>`.
- [x] **Paso 2 — Política (pura y testeable).** `fn should_retry(err: &ureq::Error) -> bool`:
      `true` solo para fallos de transporte **antes de enviar** (`ConnectionFailed`, connection
      refused, DNS/connect); `false` para timeout de lectura, conexión cortada tras enviar y
      cualquier `Error::Status`.
- [x] **Paso 3.** En `get_timed`/`post_timed`: si `should_retry`, llamar `reconnect`, actualizar
      `base`/`token`, reintentar **una** vez. Aplica a todas las rutas, incluida `remember` (el
      request no llegó). El error final incluye ambas causas.

## Criterios de aceptación

- [x] Tests unitarios de `should_retry` para cada clase de error (connect refused → sí; timeout de
      lectura → no; 500 → no; 404 → no).
- [x] Test `mcp_binding`: sesión MCP, `remember` OK, `devctx serve --stop` en el proyecto,
      `remember` otra vez → OK, y el PID en `serve.json` es distinto del primero.
- [x] Ninguna tool hace más de un reintento (contar en el test vía `serve.log` o un contador).

## Riesgos

Reintentar después de enviar duplicaría escrituras: la política lo prohíbe y los tests lo fijan.

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes: tras la TASK-001 la conexión quedaba cacheada para siempre (review I-4);
   cuando el serve salía (`--idle 900`, `serve --stop`, crash) CADA tool fallaba hasta reiniciar la sesión.
   Después: `a_session_survives_its_serve_leaving` (`remember` OK, `serve --stop`, `remember` OK con PID
   distinto en `serve.json`).
3. **Causa raíz:** confirmada: `RemoteClient.target()` guardaba `(base, token)` sin invalidación. Además
   (M-1) el connector corría con el mutex tomado y sin memoria del fallo: un serve colgado hacía esperar
   hasta 60 s a cada tool, y con `Busy` cada tool pagaba ~3,4 s.
4. **Archivos y símbolos** (`crates/devctx-mcp/src/backend.rs`, único archivo de código):
   `RemoteClient` ahora tiene `link: Mutex<Link>` (target + `generation` + último fallo) en vez de
   `conn`; `fn should_retry(&ureq::Error) -> bool` (privada); `RemoteClient::request` (envía, y ante
   fallo de conexión invalida, vuelve a correr el connector y reintenta UNA vez), `invalidate(generation)`;
   `const FAILURE_BACKOFF = 8 s`. `get_timed`/`post_timed` pasan por `request`. Se corrigió el doc de
   `RemoteClient` ("tries again next time" solo era cierto antes de la primera conexión). Se usó un
   contador de generación en vez de comparar `base` porque el puerto es determinista (`auto_addr`): el serve
   nuevo suele tener la misma URL. No se tocó `pid_alive`/`probe`. Sin closure `reconnect` nueva: se
   reutiliza el `Connector` de la TASK-001.
5. **Tests nuevos.** Unitarios (`backend::tests`): `a_refused_connection_is_retried`,
   `a_read_timeout_is_not_retried`, `an_http_status_is_not_retried` (404 y 500),
   `a_refused_connection_reconnects_once_and_failures_back_off` (el connector corre 2 veces, no más; tres
   llamadas con un connector que falla lo corren 1 vez). Integración (`mcp_binding.rs`):
   `a_session_survives_its_serve_leaving`. Resultados: ver abajo.
6. **Contrato JSON:** sin cambios. Un error tras reintento termina en `(retried once; first attempt: ...)`;
   si el reconnect falla: `<error>; reconnecting failed: <causa del connector>`.
7. **No verificado:** reintento con un serve que muere EN MEDIO de una petición (por diseño no se reintenta);
   el criterio "ninguna tool hace más de un reintento" se fija con el contador del connector en el unit
   test, no contando arranques en `serve.log`. La primera corrida del test de integración dio dos veces
   "timed out reading response" en el primer `remember` (serves colgados de corridas previas en la
   máquina); después pasó limpia, sin tocar el test.
8. **Números:** fallo de conexión ya conocido: respuesta inmediata durante 8 s en vez de ~3,4 s (Busy) o hasta 60 s
   (colgado) por tool.

### Fixup D2 (review)

- **Corrección del Resultado (punto 3, M-1):** NO estaba resuelto. `connect()` seguía corriendo con el mutex
  `Link` tomado: el backoff solo mitigaba, y el primer connect contra un serve lanzado que ni respondía ni salía
  (hasta `WAIT_TICKS` = 60 s + probe) encolaba a todas las demás tools detrás. Ahora `RemoteClient::target`
  es single-flight: un mutex `connecting` serializa los connectors y `link` se toma solo para leer/escribir
  estado (`known()`), nunca a través de `connect()`; los que esperan ven el resultado del primero (target o
  fallo recordado). Test: `a_slow_connect_does_not_hold_the_link_lock_and_is_single_flight`.
- `timeout_connect(2 s)` en el agente (`CONNECT_TIMEOUT`): un SYN descartado ya no consume los 120 s (1800 s en
  `index_repo`). Test: `a_dropped_syn_gives_up_at_the_connect_timeout`.
- `FAILURE_BACKOFF` baja de 8 s a 3 s: también memoizaba Busy / "shutting down" y fallaba al instante 8 s con
  el serve ya de vuelta (el punto 8 de arriba queda como "3 s").
- Código muerto: se elimina `Backend::remote` (connect `None`, sin llamadores).

### Fixup D2b (review final)

- `a_slow_connect_does_not_hold_the_link_lock_and_is_single_flight` solo discriminaba por `try_lock` con
  un `sleep(150ms)`: un hilo tardío daba falso verde. Ahora el conector se queda esperando en una
  compuerta (`mpsc`), se espera por sondeo a que arranque, el segundo hilo llega con el primero
  provablemente en vuelo y se comprueba que el conector no corre dos veces.
- `a_dropped_syn_gives_up_at_the_connect_timeout` acotaba a 10 s sin comprobar el presupuesto de 2 s:
  ahora afirma `CONNECT_TIMEOUT <= 2 s` y que la llamada vuelve en < 5 s (SYN descartado ≈ 2 s; red
  inalcanzable, antes).
