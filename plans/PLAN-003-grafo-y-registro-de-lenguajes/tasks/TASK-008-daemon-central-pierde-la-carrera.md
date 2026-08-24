# TASK-008 — El test flaky era el daemon central perdiendo una carrera de 4 segundos

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/grafo-y-registro-de-lenguajes`
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Que la suite deje de fallar 1 de cada 3 corridas, arreglando la causa en vez de
marcar los tests como `#[ignore]`.

## Contexto verificado (2026-08-23)

La recomendación original era **ignorar los tres tests pesados**. Medir primero
la volvió innecesaria.

### La medición que cambió la decisión

Siete corridas de `cargo test -p devctx-cli --test mcp_binding`:

| | Resultado |
|---|---|
| Corridas | 7 |
| Fallos | **2** (≈29%) |
| Test que falla | **no siempre el mismo** — la primera vez `a_group_binding_attributes_the_memory_to_the_group`, después `scope_defaults_follow_the_binding` |
| Aislado | pasa siempre, en 39s |

Que no sea siempre el mismo test descartó "este test está mal" y apuntó a
contención: falla **el que pierde la carrera**.

### El error real

Atraparlo costó dos intentos por un error de método propio: el `grep` filtraba
`panicked at`, que trae el archivo y la línea pero **no el mensaje, que va en la
línea siguiente**. El dato estuvo en la primera corrida roja y se descartó dos
veces. Con `grep -A3`:

```
tool `remember` returned an error:
  "The project registry could not be read ... no central store daemon
   and one could not be started; run `devctx serve --central`"
```

### La causa

`crates/devctx-central/src/client.rs`, en `ensure`:

```rust
// No model to load here, so a healthy daemon appears in well under a second.
for _ in 0..40 {                       // 40 × 100 ms = 4 segundos
```

**Ese comentario es la coartada del bug.** Es cierto en una máquina ociosa. En
una corrida completa hay cuatro tests cargando cada uno un modelo de embeddings,
más los servidores de proyecto de fondo; bajo eso un proceso tarda segundos solo
en ser agendado. El daemon no llegaba a los 4 s y el llamador lo reportaba como
"no se pudo arrancar" — sin decir por qué, porque `ensure` devuelve `Option`.

Es **el cuarto defecto del mismo día con el mismo patrón**: un supuesto sobre el
borde entre dos procesos, escrito como afirmación, nunca verificado bajo carga.
Ver TASK-001 (grafo), TASK-007 (recall, dos capas) y PLAN-002 (procedencia).

## Archivos

- **Modificar:** `crates/devctx-central/src/client.rs`
- **Modificar:** `crates/devctx-mcp/src/state.rs` — el mensaje que ve quien llama

## Pasos

- [x] **Paso 1 — Medir antes de decidir.** 7 corridas, 2 fallos, tests distintos.
- [x] **Paso 2 — Capturar el error de verdad** (`grep -A3`, no `grep`).
- [x] **Paso 3 — Presupuesto de espera realista.** `TICK` 100 ms × `WAIT_TICKS`
      200 = 20 s. **No es una estimación del arranque** —el arranque es rápido—
      sino el margen para una máquina cargada. El loop sale apenas el daemon
      contesta, así que en una máquina ociosa cuesta lo mismo que antes.
- [x] **Paso 4 — Distinguir "todavía no" de "nunca va a llegar".** `spawn`
      devuelve un `Arc<Mutex<Option<ExitStatus>>>` que el hilo cosechador marca
      al salir; el loop lo consulta y corta al instante si el daemon murió, en
      vez de quemar los 20 s completos.
- [x] **Paso 5 — Decir POR QUÉ falló.** `spawn_failure_hint` lee las últimas
      líneas de `serve.log` —donde el daemon escribe la razón real antes de
      morir— y `central()` las agrega al mensaje. Esa línea no la leía nadie.
- [x] **Paso 6 — Verificar con 5 corridas seguidas.**

## Criterios de aceptación

- [x] Cinco corridas consecutivas de `mcp_binding` en verde: **11/11, 0 fallos**.
- [x] Ningún test marcado `#[ignore]` por esta causa.
- [x] `cargo fmt --all --check` limpio.
- [x] `cargo clippy --all-targets -- -D warnings` limpio.

## Riesgos

**Subir un timeout puede ser tapar el síntoma.** Acá no lo es sólo porque van
las otras dos mitades: el loop detecta la muerte del proceso y el fallo dice su
causa. Si el daemon vuelve a no arrancar, ahora el mensaje lo explica en vez de
mandar a alguien a reproducirlo.

**20 s es un margen, no una medición.** Nadie midió cuánto tarda realmente el
arranque bajo carga máxima. Si esto reaparece, el número a revisar es ése — y
ahora el log dirá si el problema fue otro.

## Resultado

**Estado final:** `done` (2026-08-23)

- **Resumen:** el flaky no era un test frágil sino el daemon central perdiendo
  una carrera contra un presupuesto de 4 segundos escrito sobre un supuesto.
  Arreglado en la causa; no se ignoró ningún test.

- **Verificado por:**

  | | Corridas | Fallos | Tiempo |
  |---|---|---|---|
  | Antes | 7 | **2** (29%) | 161–261 s |
  | Después | 5 | **0** | 119–199 s |

  Más rápidas, además: el daemon ya no quema 4 s de espera para después
  rendirse. `fmt` y `clippy --all-targets -D warnings` limpios.

- **Desviaciones:** la recomendación aprobada era `#[ignore]` en tres tests.
  **No se ejecutó**, porque medir mostró que no eran tres tests frágiles sino
  uno cualquiera perdiendo una carrera. Ignorarlos habría enterrado un defecto
  real del daemon que afecta a producción, no sólo a los tests: cualquier
  usuario en una máquina cargada recibía "no se pudo arrancar" sin explicación.

- **Riesgos abiertos / siguiente:**
  - `cargo fmt` tocó `hooks.rs` y `mcp/lib.rs`, que vienen de PLAN-002:
    **`main` estaba desformateado y el CI ya estaba rojo** antes de esta rama.
    Queda corregido acá, pero nadie estaba mirando el CI.
  - Los servidores de proyecto **no mueren con `--idle 900`**: se observaron
    cinco vivos a los 45 minutos. Defecto aparte, sin diagnosticar.
