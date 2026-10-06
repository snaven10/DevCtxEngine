# PLAN-010 — Memoria de procesos: modelo, ONNX Runtime y DuckDB acotados y medidos

**Fecha:** 2026-10-05
**Fase:** 2 (Ejecución) — aprobado 2026-10-05; TASK-001 hecha (línea base en 0.9.0); el resto (TASK-002…011) se ejecuta después de PLAN-009 (decisión 2026-10-06)
**Diseño:** [`PLAN-010-design.md`](./PLAN-010-design.md)
**Fase anterior:** hotfix 0.8.5 (D1 lotes de embeddings en serie + batch 8 por defecto; D2 liberación
por inactividad del embedder del central + corrección del comentario falso). Lo ejecuta otra sesión
fuera de este plan; PLAN-010 lo da por hecho y **no lo re-planifica**.
**Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`). Sale de `main` una vez
publicado 0.8.5 (y de lo que haya en `main` de PLAN-008 P1 en ese momento). Rama sugerida:
`feat/plan-010-memoria`.
**Origen:** investigación verificada de consumo de memoria del 2026-10-05 (devctx 0.8.4, máquina
con `nproc=20`, WSL2), medida con `/proc/<pid>/status` sobre el central y 14 serves vivos.
**Release:** `0.11.0` (Q-4, decidido 2026-10-06): se ejecuta completo DESPUÉS de PLAN-009 (`0.10.0`).

Todas las referencias `archivo:línea` son contra `main` = v0.8.4 (`47eea79`). El hotfix 0.8.5 mueve
líneas de `devctx-embed/src/local.rs`, `devctx-central/src/lib.rs` y `devctx-api/src/central.rs`:
cada task las re-verifica al arrancar.

---

## 1. Qué resuelve

devctx corre **un proceso por proyecto** (serve) más un central, y cada uno que toca el modelo de
embeddings carga su propia copia privada del modelo y de ONNX Runtime, con hilos y lotes
dimensionados como si fuera dueño de la máquina. Resultado: un central de 1.8 GB que nunca suelta el
modelo, serves que en reposo pesan ~500 MB, y picos de indexación de 6.5 GB y 18.2 GB que en una
laptop de 16 GB invitan al OOM killer (que mata la sesión del usuario, no a devctx).

El hotfix 0.8.5 ataca las dos causas más gruesas (lotes en paralelo y el central que nunca libera).
Este plan ataca lo estructural:

1. **Saber qué ocupa qué.** Hoy no hay forma de ver, desde devctx, si el modelo está cargado, cuánto
   es ONNX Runtime, cuánto es DuckDB y cuánto el índice HNSW. La hipótesis principal de los ~500 MB
   en reposo **no está verificada** (TASK-001).
2. **Dejar de depender de fastembed** para el camino local: sus decisiones (rayon sobre los lotes,
   `intra_threads = available_parallelism`, pesos copiados a memoria anónima, sin control de arena ni
   memory pattern) son las que no se pueden corregir desde afuera. Se reemplaza por `ort` +
   `tokenizers` directos, **reproduciendo los vectores actuales** para no forzar un reindex.
3. **Compartir los pesos entre procesos** (mmap / formato ORT), para que N serves con el modelo
   cargado no paguen N copias.
4. **Acotar DuckDB de forma configurable y medida** (hoy está acotado por env, sin config ni
   verificación de que el `SET` se aplicó).
5. **Opcional:** un modo `embeddings.provider: central` donde los serves piden embeddings al central
   en vez de cargar el modelo, evaluado con números después del reemplazo.

### 1.1 Números medidos (2026-10-05, devctx 0.8.4, `/proc/<pid>/status`)

| Proceso | RSS | VmHWM | Notas |
|---|---|---|---|
| central (`serve --central`) | **1.8 GB** | 1.9 GB | 1.77 GB `RssAnon`; modelo cargado por un `remember`/`recall` y nunca liberado |
| serve `REVFA_FrontEnd` | 601 MB | **6.5 GB** | `DEVCTX_EMBED_BATCH_SIZE=8`; DB de 2.3 GB |
| serve `DevCtxEngine` | 508 MB | **18.2 GB** | batch 32 (default), índice completo |
| 12 serves sin modelo cargado | 12-43 MB c/u | — | lo que cuesta un serve "vacío" |
| zombies `<defunct>` | 6 | — | hijos `devctx` bajo procesos `devctx mcp` viejos (ver §2, H7) |

Modelo en uso: `ml-granite` = `granite-embedding-97m-multilingual-r2`, ONNX int8 (~98 MB),
384 dims, ModernBERT de 12 capas, `max_length` 512; `tokenizer.json` de 25 MB.

## 2. Hallazgos y causa raíz (verificado contra `main`, 2026-10-05)

### H1 — fastembed corre los lotes en paralelo con todos los hilos *(lo cubre 0.8.5 · D1)*

`fastembed-4.9.1/src/text_embedding/impl.rs:295`: `texts.par_chunks(batch_size)` con rayon → hasta
`nproc` (=20) lotes a la vez, cada `session.run` con `with_intra_threads(available_parallelism)`
(`impl.rs:104-109`). La atención de ModernBERT cuesta ≈ 12 heads × 512² × 4 B por texto y por capa
→ picos de GB. Mismo patrón en el reranker (`fastembed-4.9.1/src/reranking/impl.rs:109,138`).
El hotfix 0.8.5 serializa los lotes en `LocalProvider::embed` y baja el batch por defecto a 8; los
hilos intra-op siguen en 20 porque fastembed no deja cambiarlos → TASK-004/TASK-007.

### H2 — los pesos no están mapeados: memoria anónima privada por proceso

`crates/devctx-embed/src/local.rs:223` hace `std::fs::read(&onnx_path)` y arma un
`UserDefinedEmbeddingModel` (`local.rs:227`); fastembed lo pasa a
`Session::builder()…commit_from_memory(&model.onnx_file)` (`impl.rs:105-110`), que copia el
protobuf dentro de ORT. Cada proceso tiene su copia en `RssAnon`; el page cache no se comparte.
Idem reranker: `crates/devctx-rerank/src/local.rs:213`. Los modelos builtin usan
`commit_from_file` (`impl.rs:77-81`), que tampoco mapea para `.onnx`.

### H3 — el central carga el embedder y no lo suelta *(lo cubre 0.8.5 · D2)*

`crates/devctx-central/src/lib.rs:70` (`embedder: Mutex<Option<Arc<…>>>`) y `:163-172`
(`Central::embedder`, carga perezosa en el primer `remember`/`recall`, sin liberación). Los serves sí
liberan a los 300 s con `malloc_trim` (`crates/devctx-mcp/src/state.rs:350-373`,
`crates/devctx-api/src/lib.rs:83,161-175`). El comentario de `crates/devctx-api/src/central.rs:10-12`
("It deliberately loads no model") es falso.

### H4 — **corrección al informe**: DuckDB SÍ está acotado desde v0.1.0, pero solo por env y sin verificación

El informe de campo decía "no hay `SET memory_limit`/`threads` en ningún lado". **No es así**:
`crates/devctx-store/src/store.rs:29-44` define `DEFAULT_MEMORY_LIMIT = "2GB"` y
`DEFAULT_THREADS = 4`, y `Store::apply_resource_limits` (`store.rs:221-230`) los aplica en
`Store::open` (`store.rs:121`) y `open_in_memory` (`store.rs:147`), con override por
`DEVCTX_DB_MEMORY_LIMIT` / `DEVCTX_DB_THREADS` (commit `72b1a5c`). Lo que falta:

- el resultado del `SET` se descarta (`let _ = …`, `store.rs:227`): si falla, nadie se entera;
- no hay perilla en `config.yaml` (`Storage`, `crates/devctx-core/src/config.rs:150-173`) ni en la
  config del central;
- 2 GB × (central + N serves) sigue siendo más de lo que la máquina tiene, y **nadie midió** si 2 GB
  es necesario para el HNSW de un store grande;
- el índice HNSW de VSS (`store.rs:244`, `hnsw_enable_experimental_persistence`; creación en
  `Store::enable_hnsw`, `store.rs:267-284`) vive entero en RAM y, según la documentación de VSS, su
  memoria no pasa por el buffer manager → `memory_limit` no lo acota. *A verificar en TASK-001.*

Esto convierte la "D3" del informe en ajustar defaults con números, exponerlos en config, y
reportar los valores efectivos (TASK-002), no en "agregar el límite".

### H5 — ort rc.9 tiene lo que hace falta; fastembed no lo expone

`ort-2.0.0-rc.9/src/session/builder/impl_commit.rs:137-140` (`commit_from_memory_directly`: config
`session.use_ort_model_bytes_directly` y `session.use_ort_model_bytes_for_initializers`, sólo para
formato `.ort`); `impl_options.rs:49,61,109` (`with_intra_threads`, `with_inter_threads`,
`with_memory_pattern`); `impl_options.rs:92` (`with_optimized_model_path`, que permite generar el
`.ort`); `impl_config_keys.rs:10` (`with_prepacking`). La arena de CPU no tiene wrapper en rc.9,
pero `ort::api()` (`lib.rs:152`) da la `OrtApi` cruda (`DisableCpuMemArena`). ORT nativo: 1.20.0
(`ort-sys-2.0.0-rc.9/build.rs:7`).

### H6 — el modo "embeddings remotos" ya tiene media pieza

`crates/devctx-embed/src/api.rs:149-189` (`CustomProvider`) ya hace `POST {endpoint}/embed` y parsea
`{"vectors": …}`; OpenAI y Voyage tienen URL fija (`api.rs:10-11`). `Central::shares_vector_space`
(`crates/devctx-central/src/lib.rs:158-160`) existe y **nadie la llama**. En el central, `remember` y
`recall` embeben **con el `Mutex<Central>` tomado** (`crates/devctx-api/src/central.rs:495-515`,
helper `run` en `:688-696`): un `/embed` montado igual serializaría todo el registro detrás de cada
inferencia.

### H7 — los zombies ya están arreglados desde 0.8.3; quedan los de MCPs viejos

`spawn_server` (`crates/devctx-cli/src/remote.rs:422-477`) cosecha al hijo con un hilo que hace
`child.wait()` (`:461-470`, entró en `e7a18e5`, v0.8.3, PLAN-008 TASK-001); el spawn del central
hace lo mismo (`crates/devctx-central/src/client.rs:255-269`). Los 6 `<defunct>` observados cuelgan
de procesos `devctx mcp` anteriores (binario `(deleted)`), que no tienen ese código. PLAN-008
TASK-017 no los menciona, y no hace falta: desaparecen al cerrar esas sesiones. TASK-001 confirma
la versión del padre de cada zombie; si alguno cuelga de un binario ≥ 0.8.3, se abre bug aparte.

## 3. Decisiones

Resumen; el detalle y los tradeoffs están en [`PLAN-010-design.md`](./PLAN-010-design.md).

| Id | Decisión |
|---|---|
| DD-1 | Medir primero. Nada se optimiza sin la línea base post-0.8.5 de TASK-001 |
| DD-2 | Reemplazar fastembed por `ort` + `tokenizers` propios en `devctx-embed` **y** `devctx-rerank` |
| DD-3 | Fidelidad: mismo pipeline que fastembed, mismo `ort =2.0.0-rc.9`; coseno ≥ 0.9999 contra vectores dorados → sin reindex |
| DD-4 | `max_length` configurable, default 512 sin cambios; cambiarlo marca el índice como desfasado |
| DD-5 | Pesos compartidos vía formato ORT + mmap, condicionado a un spike que mida `RssFile` vs `RssAnon` |
| DD-6 | Perillas de ORT (hilos intra/inter, arena, memory pattern, batch) en config, defaults medidos |
| DD-7 | DuckDB: `storage.memory_limit`/`storage.threads` en config, precedencia env > config > default, valores efectivos reportados |
| DD-8 | `embeddings.provider: central` opt-in, apagado por defecto; sin embeber bajo el mutex; versión y modelo en cada respuesta |
| DD-9 | Opción B (un solo proceso multi-proyecto) **descartada** |
| DD-10 | Instrumentación permanente (no un script de un día): bloque `memory` en `status` |

## 4. Fuera de alcance

- **Lo del hotfix 0.8.5** (lotes en serie, batch 8 por defecto, liberación del embedder del central,
  comentario de `central.rs`). Este plan parte de ahí.
- **Opción B**: un único proceso workspace con todos los proyectos. Descartada con el usuario
  (DD-9): rehace el ciclo de vida que PLAN-008 acaba de estabilizar.
- **Cambiar de modelo** (otro embedder, cuantización distinta, dimensiones distintas) o de reranker.
- **Cambiar `max_length` por defecto** (DD-4: se hace configurable, el default queda en 512).
- **Actualizar `ort`** a rc.10/2.0 final ni ONNX Runtime > 1.20 (cambia numéricos; plan aparte).
- **Reindexar** cualquier proyecto: el criterio es justamente no tener que hacerlo.
- **Zombies de MCPs ≤ 0.8.2** (H7): arreglados en 0.8.3; desaparecen al cerrar esas sesiones.
- Ítems de PLAN-008 TASK-017 (idle exit con listener abierto, `in_tx`, kqueue…): siguen allá.
- Tocar `/home/snaven10/revfa` más allá de lectura y medición (copias del DB para las pruebas de
  HNSW se hacen en el scratchpad).

## 5. Tasks y orden

| Task | Qué | Especialista | Depende de | Estado |
|------|-----|--------------|------------|--------|
| TASK-001 | Instrumentación de memoria (`status.memory`, logs de carga/liberación) y línea base post-0.8.5 | general-purpose (Rust) | — | `pending` |
| TASK-002 | DuckDB: `memory_limit`/`threads` en config, valores efectivos, defaults medidos con REVFA_FrontEnd | general-purpose (Rust) | TASK-001 | `pending` |
| TASK-003 | Arnés de equivalencia: fixtures + vectores y scores dorados generados con fastembed | general-purpose (Rust) | — | `pending` |
| TASK-004 | Motor ONNX propio para embeddings (`ort` + `tokenizers`), builtins y user-defined, CUDA y stall guard | general-purpose (Rust) | TASK-003 | `pending` |
| TASK-005 | Reranker sobre el motor propio | general-purpose (Rust) | TASK-004 | `pending` |
| TASK-006 | Pesos compartidos entre procesos: spike ORT/mmap e implementación | general-purpose (Rust) | TASK-001, TASK-004 | `pending` |
| TASK-007 | Perillas de runtime ONNX en config (hilos, arena, memory pattern, batch, `max_length`) con defaults medidos | general-purpose (Rust) | TASK-001, TASK-004 | `pending` |
| TASK-008 | Retirar fastembed del árbol y cross-check Windows/macOS | general-purpose (Rust) | TASK-005, TASK-006, TASK-007 | `pending` |
| TASK-009 | Modo opcional `embeddings.provider: central` (`/embed`, `RemoteProvider`, `base_url`) | general-purpose (Rust) | TASK-008 | `pending` |
| TASK-010 | Docs EN + ES | general-purpose (Rust) | TASK-002, TASK-008, TASK-009 | `pending` |
| TASK-011 | Verificación de campo antes/después contra §6 | — (orquestador, con aprobación del usuario) | TASK-010 | `pending` |

(La columna `Estado` va última: el parser de `plan_status` lee la columna con encabezado `Estado`.)

**Paralelizables:**
- **TASK-001 y TASK-003 arrancan juntas** apenas 0.8.5 está en `main`: una toca `devctx-api`,
  `devctx-mcp/state.rs`, `devctx-store` (lectura de métricas); la otra sólo agrega tests y fixtures
  en `devctx-embed`/`devctx-rerank` **con fastembed todavía en el árbol** (es la única ventana en la
  que se pueden generar los dorados).
- Tras TASK-001: **TASK-002** (store/config) en paralelo con la cadena de TASK-004.
- Tras TASK-004: **TASK-005, TASK-006 y TASK-007 en paralelo**. 006 y 007 tocan ambas el builder de
  sesión del motor nuevo: la segunda en entrar rebasea (conflicto de zona, no lógico).
- TASK-009 espera a TASK-008 porque el `/embed` del central debe correr el motor final, y porque la
  decisión de hacerlo default (Q-3) depende de los números de TASK-006/007.

**Esquema de ejecución (igual que PLAN-008):** una sesión ejecuta, otra revisa cada lote en modo
solo lectura antes de seguir. Compilar y correr tests queda autorizado sólo cuando el usuario apruebe
el plan. **Antes de taggear, cross-check obligatorio** (Windows y macOS `cargo check`, con y sin
`--features gpu` donde aplique) empujando una rama `fix/**` que dispare el job de CI — lección de
v0.8.3, que quedó como tag sin release por un `cfg(unix)` faltante.

## 6. Criterios de aceptación globales (medidos en TASK-011)

"Antes" = 0.8.4 medido el 2026-10-05; TASK-001 re-mide sobre 0.8.5 y esa pasa a ser la línea base
de comparación (la columna se completa ahí).

| Criterio | Antes (0.8.4) | Meta |
|---|---|---|
| RSS del central en reposo tras el idle de modelos | 1.8 GB; **0.9.0: 105 MB** (303 MB cargado) | ≤ 150 MB (sin modelo) |
| RSS de un serve con modelo cargado, en reposo (DevCtxEngine) | 508 MB; **0.9.0: 300-315 MB** (pico de carga `VmHWM` ~710) | ≤ 250 MB, con el reparto modelo/ORT/DuckDB/HNSW explicado por `status.memory` |
| Pico de indexación completa de DevCtxEngine (`VmHWM` del serve) | 18.2 GB; **0.9.0: 897 MiB** | ≤ 1.5 GB |
| Pico de indexación incremental en REVFA_FrontEnd | 6.5 GB; **0.9.0: 753 MiB** (clon git, 149 archivos) | ≤ 1.5 GB |
| 3 serves con modelo cargado: páginas de pesos compartidas | 0 (todo `RssAnon`); **0.9.0: 3 serves = 679 MiB anon, Pss 742** | pesos en `RssFile`/`Pss` repartido; `RssAnon` por serve baja ≥ el tamaño del modelo (~98 MB) |
| Embeddings motor nuevo vs fastembed, sobre el set de fixtures | — | coseno ≥ 0.9999 por vector; top-10 de `search` idéntico en el set de consultas |
| Scores del reranker vs fastembed | — | `|Δ| ≤ 1e-3` y mismo orden en el set de fixtures |
| Reindex requerido tras actualizar | — | **ninguno** (`extractor_stale`/aviso de modelo en `false`) |
| Latencia de `search` (p50 y p95, 20 consultas, serve caliente) | **0.9.0: p50 30-37 ms, p95 40-70 ms** (CLI, serve caliente) | ≤ +10 % |
| Throughput de indexación (chunks/s, DevCtxEngine completo) | **0.9.0: 8.9 chunks/s** (DevCtxEngine, 4 677 chunks; 7.9 en REVFA_FrontEnd) | ≥ −15 % (se acepta algo más lento a cambio del pico) |
| DuckDB acotado y HNSW creado en REVFA_FrontEnd (2.3 GB) con el default elegido | 2 GB, sin medir | `CREATE INDEX … USING HNSW` termina sin OOM; `memory_limit` efectivo visible en `status` |
| `status` informa memoria | no; **TASK-001: sí** (`status.memory`) | bloque `memory`: modelo cargado sí/no, RSS anon/file, DuckDB usado/límite, vectores HNSW |
| `provider: central` (si se activa) | — | serve con `provider: central` en reposo ≤ 60 MB; `search` OK; error explícito con central caído |

## 7. Riesgos

- **Divergencia numérica del motor propio.** Padding `BatchLongest`, tokens especiales agregados a
  mano por fastembed (`fastembed-4.9.1/src/common.rs:112-131`), el `epsilon` de su `normalize`
  (`common.rs:135-140`) y el orden de reducción en el mean pooling pueden mover el último bit.
  Mitigación: TASK-003 genera los dorados **antes** de tocar nada; tolerancia DD-3; si no se alcanza,
  TASK-004 queda `blocked` y no se retira fastembed.
- **Prepacking de MLAS anula parte del ahorro del mmap.** Los MatMul int8 se re-empaquetan en
  memoria privada aunque los pesos vengan mapeados. Mitigación: el spike de TASK-006 mide con y sin
  `with_prepacking(false)` y decide con números (DD-5); si no hay ganancia, se documenta y se
  cierra `skipped` con el motivo.
- **`.ort` generado con optimización `Level3` es dependiente del hardware.** Mitigación: generarlo
  por máquina en la caché de modelos (no distribuirlo), con clave por versión de ORT + hash del
  `.onnx` + nivel de optimización; regenerar si no coincide (DD-5).
- **HNSW fuera del `memory_limit`.** Bajar el límite puede no bajar el pico de crear el índice y sí
  provocar spill del resto. Mitigación: TASK-002 mide en una copia de REVFA_FrontEnd antes de fijar
  defaults.
- **Descarga de builtins sin fastembed.** Hay que reusar el layout de caché de hf-hub para no
  re-descargar (`models--Org--repo/snapshots/…` bajo `model_cache_dir`) y mantener
  `devctx_core::modelload::guard_load`. Mitigación: TASK-004 lo prueba con la caché real existente
  (lectura) y con una vacía en tmp.
- **CUDA.** El camino `--features gpu` + `CudaFallback` (`crates/devctx-core/src/fallback.rs`) debe
  sobrevivir; no hay GPU en la máquina de desarrollo. Mitigación: compila con y sin `gpu` en el gate
  y en el cross-check; el comportamiento queda declarado como no verificado si no hay hardware.
- **`provider: central` crea un punto único de falla.** Mitigación: opt-in, error explícito y
  reintento perezoso (DD-8); nunca es default en este plan.
- **Trabajo en paralelo con PLAN-008 P1** en `state.rs`/`devctx-api`. Mitigación: este plan arranca
  después de 0.8.5 y rebasea sobre lo que haya mergeado de P1; las zonas compartidas son pocas
  (`status`).

## 8. Verificación

- Gate por lote: `cargo fmt --check`, `cargo clippy --workspace --all-targets` con y sin
  `--features gpu`, `cargo test --workspace`. Cada task agrega sus tests (detalle en cada TASK).
- Tests clave:
  1. equivalencia de embeddings contra dorados (`ml-granite` si hay `model_dir`, `minilm-l6` y
     `bge-small` para cubrir Mean y CLS) — TASK-003/TASK-004;
  2. equivalencia de scores del reranker — TASK-003/TASK-005;
  3. `status.memory` presente y con campos tipados (sin números fijos) — TASK-001;
  4. `memory_limit`/`threads` desde config, env gana, valor efectivo reportado — TASK-002;
  5. `/embed` del central: rechaza modelo/dimensión distintos, no toma el mutex del registro
     durante la inferencia (test con un embedder falso lento + `projects list` concurrente) — TASK-009.
- Mediciones con la metodología de TASK-001 (script en `scripts/`, salida en el `## Resultado` de
  cada task).
- Campo: TASK-011, antes/después contra §6.
- `devctx plan-status PLAN-010` desde la raíz de este repo: a correr por el orquestador al aprobar.

Nada de esto se compiló ni se testeó: es un plan.

## 9. Rollback

Cada task es un commit convencional propio. Puntos sensibles:
- **TASK-004/005 (motor propio)** quedan detrás de la misma API (`LocalProvider`, `LocalReranker`) y,
  hasta TASK-008, fastembed sigue en el árbol: revertir el commit devuelve el camino viejo sin tocar
  datos. Los vectores son equivalentes (DD-3), así que índices escritos con el motor nuevo siguen
  siendo válidos con el viejo.
- **TASK-006** agrega un `.ort` en la caché de modelos: revertir lo deja huérfano e inocuo (borrable).
- **TASK-002/007** sólo agregan claves de config con default; revertir ignora las claves (serde
  `default`), no rompe configs existentes.
- **TASK-008** es la única irreversible "en la práctica" (saca la dependencia): sólo entra cuando
  003-007 están `done` y el campo intermedio está verde.
- **TASK-009** es opt-in: con `provider: local` (default) el código nuevo no se ejecuta.
Ninguna task migra ni reescribe datos.

## 10. Release

Opciones (Q-4):
- **`0.10.0`**: cambia el motor de inferencia, agrega claves de config y un endpoint nuevo en el
  central; si P1 ya salió como 0.9.0, esto es el siguiente minor.
- **`0.9.x`**: si se considera "interno" (mismos vectores, mismas tools). Defendible para TASK-001/002
  solas, menos para el cambio de motor.
Recomendación: un `0.9.x`/`0.10.0-rc` intermedio tras TASK-007 (motor nuevo con fastembed aún como
red de seguridad) no aporta: el usuario no puede elegir motor. Se sugiere **un solo `0.10.0`** al
cerrar TASK-011, con TASK-009 incluida pero apagada.

**Decidido (2026-10-06):** como PLAN-009 sale primero como `0.10.0`, este plan sale como **`0.11.0`**
(un solo release al cerrar TASK-011). TASK-001 (instrumentación) se mergea antes a `main` sin release
y viaja con `0.10.0`.

## 11. Contrato de resultado (lo que cada task reporta al cerrar)

Cada TASK llena su `## Resultado` con:

1. **Estado final** (`done` / `blocked` / `skipped`) y una línea de por qué si no es `done`.
2. **Números antes/después** con la metodología de TASK-001 (RSS, `RssAnon`, `RssFile`, `VmHWM`,
   latencias), o "no medible en esta task" con el motivo.
3. **Hipótesis confirmadas o refutadas** de §2 / del diseño (sobre todo H4-HNSW y la de los ~500 MB).
4. **Archivos tocados** y símbolos públicos nuevos o cambiados (firma), claves de config nuevas.
5. **Tests**: nombres, comando con que se corrieron y resultado.
6. **Contrato JSON**: campos agregados/cambiados en `status`, `/embed`, etc.
7. **Lo que NO se verificó**, explícito (p. ej. CUDA sin hardware, macOS sólo compilado).

## 12. Preguntas abiertas (para el usuario)

- **Q-1 — Defaults de DuckDB.** Hoy 2 GB / 4 hilos. Propuesta a confirmar con los números de
  TASK-002: `memory_limit` 1 GB para serves y 512 MB para el central, `threads` 2. ¿OK fijarlos por
  medición, o preferís valores conservadores fijos ya?
- **Q-2 — `max_length`.** ¿Se deja en 512 (sin reindex, DD-4) o se evalúa 256 para chunks de código
  sabiendo que cambia los vectores de todo chunk > 256 tokens y exige reindex? La propuesta es
  dejarlo configurable y en 512.
- **Q-3 — `provider: central` como default.** ¿Queda siempre opt-in, o se re-evalúa hacerlo default
  si TASK-009 muestra latencia aceptable y el central estable?
- **Q-4 — Número de release.** `0.10.0` vs `0.9.x` (§10). → **Resuelta: `0.11.0`** (ver abajo).

### Decisiones del usuario (2026-10-06), con la línea base de TASK-001 en mano

- **Ejecutar el plan completo** (TASK-002…011), **después de PLAN-009**. Orden general: TASK-001 se
  mergea a `main` sin release → PLAN-009 → `0.10.0` → resto de PLAN-010 → `0.11.0`.
- **Lo que la línea base cambia del foco:** tres metas de §6 **ya se cumplen en 0.9.0** gracias al
  hotfix 0.8.5 — pico de `index --full` (897 MiB), pico incremental (753 MiB) y central en reposo
  (105 MiB). El foco real del resto del plan es:
  - el **costo por serve con el modelo cargado**: 300-315 MiB, de los que ~225 MiB son modelo + ORT
    (meta 250 MiB), que con tres serves suma 679 MiB de `RssAnon` (pesos anónimos, no compartidos);
  - el **transitorio de carga del modelo**: pico ~710 MiB en la primera búsqueda de un serve fresco
    contra ~300 MiB estables.
  TASK-006 (pesos compartidos vía mmap) y TASK-009 (`provider: central`) son las que actúan sobre lo
  primero; TASK-004/007 (motor ort propio y perillas) sobre lo segundo.
- Q-1, Q-2 y Q-3 siguen abiertas y se deciden con los números de sus tasks.

## 13. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
- **Cerrado:**
- **Shipeó:**
- **Verificación:**
- **Quedó afuera:**
- **Continúa en:**
