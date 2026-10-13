# TASK-018 — Compactar la base una vez tras el upgrade 0.9.0 → 0.10 (y `devctx compact`)

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-016 (su etapa A, ya hecha: la medición que motivó esta task; la etapa B de TASK-016 se corre **después** de esta, sobre el binario con compactación — ver master §5)
- **Estado:** `pending`

---

## Objetivo

Que actualizar un índice 0.9.0 a 0.10 no deje el archivo de la base en ~2×. La 0.10.0 lleva un paso
que **reescribe la base una sola vez** después del primer `index --full` sobre un índice 0.9.0
(copia a un archivo nuevo, verificación, reemplazo atómico), más un comando explícito
`devctx compact`. Meta: tamaño tras el upgrade **≤ +30 %** sobre el índice 0.9.0 de partida, sin
perder una fila ni una memoria, y con la base original intacta ante cualquier fallo.

Decisión del usuario del 2026-10-13 ("compactar al actualizar", master §12), tras el hallazgo 5 de
TASK-016. Cambia DD-21 (que decía "medir y no tocar").

**Relación con PLAN-010 TASK-012.** Son dos problemas con el mismo mecanismo:

| | PLAN-009 TASK-018 (esta) | PLAN-010 TASK-012 |
|---|---|---|
| Qué resuelve | el salto único del upgrade 0.9.0 → 0.10 | el crecimiento con el uso normal (reindexados que reescriben) |
| Cuándo | bloquea la 0.10.0 | no bloquea la 0.10.0 |
| Disparo | automático una vez + `devctx compact` | a decidir tras medir la curva de N ciclos |
| Entrega | el mecanismo seguro de copia + reemplazo | reusa ese mecanismo; decide si hace falta un disparo periódico |

PLAN-010 TASK-012 sigue en pie: mide la curva y decide la política; ya no tiene que diseñar la
compactación (su Paso 3 opción (b) es `devctx compact`).

## Contexto verificado

Líneas contra `3712ae5` (2026-10-13). Re-verificar al arrancar.

**La medición (TASK-016, Resultado, hallazgo 5 y Paso 2).** `--full` del candidato en sitio sobre el
índice 0.9.0, bloques de 256 KiB de `pragma_database_size()`:

| Repo | usados antes → después | archivo antes → después | usados del candidato desde cero |
|---|---|---|---|
| backend-a | 504 → 786 (+56 %) | 153 → 296 MiB | 523 |
| frontend | 293 → 463 (+58 %) | 83 → 171 MiB | 316 |
| DevCtxEngine | 114 → 173 (+52 %) | — | 116 |
| backend-b | 85 → 124 (+46 %) | — | 89 |
| legacy-migration | 30 → 36 | — | 26 |

De ahí salen (cuenta, no medición): backend-a queda con ~398 bloques libres de ~1 184 (34 % del
archivo) y frontend con ~221 de ~684 (32 %); DevCtxEngine en TASK-003 quedó en 167 usados / 75
libres (31 %). **Ojo:** los *usados* también crecen (786 contra 523 del mismo contenido indexado
desde cero). Sacar solo los bloques libres deja a backend-a en ~196 MiB (+28 % sobre los 153 MiB de
partida, pero +56 % en usados). La hipótesis de esta task —**no verificada**— es que una reescritura
fila por fila deja los usados cerca de los de "desde cero" (~523 → ~131 MiB). El Paso 0 la confirma
o la tumba antes de escribir código de producto.

**Motor.** `Cargo.lock`: `duckdb` y `libduckdb-sys` `1.10505.0` = **DuckDB v1.5.5** (el README del
crate: `1.MAJOR_MINOR_PATCH.x`; `pragma_version.cpp` del tarball empaquetado dice `v1.5.5`).
`v0.9.0` usa **la misma** versión (`git show v0.9.0:Cargo.lock`): el formato de archivo que escribe
una compactación lo lee 0.9.0.

**Qué dice DuckDB sobre recuperar espacio** (fuentes al final; "leído en el código" = tarball de
`libduckdb-sys-1.10505.0`):

- `VACUUM` no recupera espacio; `CHECKPOINT` es lo que libera tras borrar; para compactar, la doc
  recomienda una copia nueva con `ATTACH` + `COPY FROM DATABASE db1 TO db2` (doc "Reclaiming space").
- El checkpoint solo **trunca los bloques libres del final del archivo**
  (`SingleFileBlockManager::Truncate`, `src/storage/single_file_block_manager.cpp`: recorre la free
  list desde el final y corta mientras `block_id + 1 == max_block`). Un bloque libre en el medio se
  queda. Coincide con lo medido en TASK-003 (un segundo `--full` y un incremental no cambian 167/75).
- `COPY FROM DATABASE` copia "schema, including constraints, indexes, sequences, macros and the data
  itself" (doc de `COPY`). En el código (`bind_copy_database.cpp`): primero recrea **todas** las
  entradas del catálogo en orden de export (`BindCopyDatabaseSchema`) y después hace un
  `INSERT … SELECT` por tabla de cada schema (`BindCopyDatabaseData`). Consecuencias leídas, **no
  ejecutadas**: (1) los índices existen antes de insertar, así que el HNSW se mantendría fila por
  fila (lo que `pipeline.rs` evita tirándolo antes de una carga); (2) el schema `fts_main_vectors`
  del FTS se copiaría como tablas y macros comunes. Ninguna fuente dice que eso funcione con VSS/FTS.
- VSS: la persistencia del HNSW es experimental (`hnsw_enable_experimental_persistence`); el índice
  se serializa entero en cada checkpoint, no lo maneja el buffer manager (tiene que caber en RAM y
  no cuenta para `memory_limit`), y el WAL no lo recupera bien (doc de VSS).
- FTS: el índice vive en el schema `fts_main_<tabla>` y no se actualiza solo (doc de FTS).

**El store.**

- `Store::open` (`crates/devctx-store/src/store.rs:188-214`): `Connection::open`, límites
  (`apply_resource_limits`, `:327-336`), **extensiones antes del schema** (`:205-211`, bug de
  TASK-003) e `init_schema` (`crates/devctx-store/src/schema.rs:12-50`).
- `Store::check_unlocked` (`store.rs:234-239`) abre y suelta para saber si otro proceso tiene el
  archivo. `Store::try_clone` (`:305-317`) comparte la instancia: el serve reparte clones a sus
  handlers, así que **dentro del serve no se puede cerrar el archivo para reemplazarlo**.
- Escritura: `Store::w()` (`:823-836`, respeta `freeze`), `in_transaction` (`:762-793`),
  `freeze` (`:850-863`).
- Índices derivados: `enable_hnsw` (`:381-398`, la métrica va en el **nombre**
  `idx_vectors_hnsw_{cosine|ip}`), `hnsw_metric` (`:402`), `drop_hnsw` (`:499-510`), `rebuild_fts`
  (`:515-527`), `has_fts` (`:535-541`), `drop_fts` (`:556-562`). Todos hacen checkpoint después de
  su DDL: un DDL de índice que queda en el WAL deja la base sin abrir (`:564-582`).
- `checkpoint`/`try_checkpoint`/`force_checkpoint` (`:580-618`) y `checkpoint_is_safe`
  (`schema.rs:60-77`: no hay checkpoint con HNSW si VSS no cargó).
- **Ya existe `Store::bloat_ratio`** (`store.rs:627-640`): `(usados + libres) / usados`.
- **Ya existe `devctx repair`**: `cmd_repair` (`crates/devctx-cli/src/main.rs:4211-4243`) para el
  serve con `remote::reclaim_db` (`crates/devctx-cli/src/remote.rs:779-810`), abre directo y llama
  `Store::rebuild_indexes` (`store.rs:650-677`), que reconstruye cada tabla **en sitio**
  (`CREATE TABLE tmp AS SELECT`, `DROP`, recrear, `INSERT`). No es atómico (un corte entre el `DROP`
  y el `INSERT` deja la tabla solo en `devctx_repair_<t>`), tira HNSW/FTS sin rehacerlos y depende de
  que los libres queden al final para que el archivo baje. Sirve para lo que fue hecho (ART rotos),
  no para esta task.
- `index_meta` (`schema.rs:326-332`, PK `(repo_path, branch, key)`); `get_index_meta` /
  `set_index_meta` / `delete_index_meta` (`crates/devctx-store/src/state.rs:153-196`). Las notas de
  toda la base usan `("", "")`: `PENDING_HNSW_META_KEY` / `PENDING_FTS_META_KEY`
  (`crates/devctx-index/src/pipeline.rs:268-277`).
- Tablas de la base por repo (`schema.rs:146-401`): `vectors` (código **y** chunks de memorias),
  `graph_edges`, `routes`, `memories`, `memory_symbol_references`, `projects` (vacía fuera del
  central), `sessions`, `index_state`, `file_state`, `index_meta`, `symbols`, `edges`,
  `branch_lineage`; vista `live_edges`. **Las memorias locales no son regenerables.**

**El final de un index** (`pipeline.rs`): `prev_extractor_stale` (`:350-355`), `full_reindex`
(`:382`), generación (`:734-744`), sello del extractor (`:769-778`), reconstrucción de HNSW/FTS por
la última corrida en vuelo (`:797-849`), checkpoint (`:854`) y **un aviso de bloat que ya existe**
(`:856-875`, `BLOAT_WARN_RATIO = 3.0` en `:884`) que manda a `devctx repair` y dice por qué no se
reescribe ahí: "it needs exclusive access to a database this process is serving".

**CLI.** `enum Command` (`main.rs:52`), `Repair` (`:105`), despacho (`:604-804`). `cmd_index`
(`:4014`): si hay serve (o se puede levantar) el trabajo lo hace **el serve** (`remote::ensure_cli`,
`:4020-4040`); si no (`DEVCTX_NO_AUTOSERVE`), abre el store y corre `index_run` (`:4041-4070`).
`cmd_reindex` (`:863`) va siempre por el serve. `open_store` (`:4371-4381`) traduce el error de lock
(`remote::is_lock_error`, `remote.rs:466-468`; `lock_message`, `:479`). Los hooks corren
`devctx index` suelto y en silencio (`crates/devctx-cli/src/hooks.rs:63-78`).

**`status`.** El bloque `memory` (PLAN-010 TASK-001): `State::memory_json`
(`crates/devctx-mcp/src/state.rs:449-451`) sobre `Store::memory_report` (`store.rs:420-473`), con
`database_size`. El aviso de índice viejo y su `hint` salen en `state.rs:1601-1608`.

**Docs que hoy dicen lo contrario:** `docs/13-keeping-the-index-fresh.md:357` ("The file keeps its
size: free blocks are not reclaimed"), `docs/08-design-decisions.md:319`, `docs/07-performance.md:174`,
sus pares en `docs/es/`, y `CHANGELOG.md` (0.10.0, "Upgrading from 0.9.x" y "Known issues").

## Archivos

- **Crear:** `crates/devctx-store/src/compact.rs` (copia, verificación, reemplazo, limpieza);
  tests en ese módulo y un test de integración en `crates/devctx-cli/tests/`;
  `scripts/graph-eval/` o `scripts/`: el script de medición del Paso 0 si no alcanza con el de TASK-016.
- **Modificar:** `crates/devctx-store/src/{lib.rs,store.rs,error.rs}`;
  `crates/devctx-index/src/pipeline.rs` (nota "debe compactación", aviso de bloat);
  `crates/devctx-cli/src/main.rs` (`Command::Compact`, post-paso de `cmd_index`/`cmd_reindex`,
  limpieza al abrir); `crates/devctx-cli/src/remote.rs` (si hace falta exponer "sin corrida en vuelo");
  `crates/devctx-mcp/src/state.rs` (`status` / `index_status`: `compaction`);
  docs EN + ES (07, 08, 11 si hay clave de config, 13), `AGENTS.md` §6, `CHANGELOG.md`;
  `PLAN-009-design.md` (DD-21) y el master (§5, §6, §12, §13).

## Pasos

- [ ] **Paso 0 — spike (decide el método; sin código de producto).** En un sandbox como el de
      TASK-016 (`mktemp -d` fuera de `/tmp`, `DEVCTX_HOME`/`HOME`/`XDG_*` propios, copias de los
      índices 0.9.0 actualizados en sitio de backend-a, frontend y DevCtxEngine), con el DuckDB del
      repo (un binario de prueba o un test `#[ignore]`, no el CLI de DuckDB de otra versión).
      Registrar para cada variante: bloques usados/libres, bytes del archivo, tiempo, `VmHWM`, pico
      de disco (archivo nuevo + WAL + `temp_directory`).
      - **0.a — ¿alcanza la meta?** Reescribir la base y comparar usados con "antes del upgrade" y
        con "candidato desde cero" (tabla de arriba). **Compuerta:** si los usados no bajan a
        ≤ 1,30 × los del índice 0.9.0 de partida, parar y llevar los números al usuario antes de
        seguir (la compactación recuperaría los libres pero no cumpliría §6).
      - **0.b — variantes de copia.**
        (A) *schema propio + `INSERT … SELECT`*: `Store::open(<tmp>)` (crea el schema con el DDL del
        binario), `ATTACH '<db>' AS src (READ_ONLY)`, `INSERT INTO t BY NAME SELECT * FROM src.t` por
        tabla, y después `enable_hnsw(<métrica de la fuente>)` + `rebuild_fts()` + checkpoint.
        (B) *`COPY FROM DATABASE src TO dst`* con VSS y FTS cargados y la persistencia experimental
        activa: ¿copia el HNSW?, ¿cuánto tarda con el índice ya creado?, ¿`fts_main_vectors` queda
        consultable?, ¿la métrica del nombre del índice se conserva?
        (C) *B sobre una fuente sin índices derivados*: no aplica si exige tocar la fuente (la fuente
        no se toca); anotar solo si B permite excluirlos.
        `EXPORT`/`IMPORT DATABASE` queda descartado de entrada (pasa por archivos intermedios: ~2×
        de disco y el array `FLOAT[n]` por texto o Parquet) salvo que A y B fallen.
      - **0.c — HNSW.** Tras la copia y un **reabrir en frío**: `duckdb_indexes()` lo lista,
        `EXPLAIN` de la consulta de `Store::search` usa el índice (no full scan), los k vecinos de
        10 consultas fijas coinciden con los de la fuente (mismo orden o mismo conjunto: anotar cuál).
        ¿Qué pasa si VSS no carga (offline)? (esperado: no se compacta; ver "Modos de fallo").
      - **0.d — FTS.** `has_fts()` y `keyword_search` de 10 consultas devuelven lo mismo que la fuente.
      - **0.e — WAL.** ¿`ATTACH … (READ_ONLY)` abre una fuente con `.wal` sin replayar? ¿Con un WAL
        que no replaya? ¿Queda `<tmp>.wal` tras `CHECKPOINT` + cerrar? ¿Cuánto crece el WAL del
        destino durante la copia (hace falta `checkpoint` por tabla o `wal_autocheckpoint`)?
      - **0.f — locks.** Con la fuente adjunta `READ_ONLY`: ¿otro proceso puede abrirla en
        escritura? (tiene que fallar con el error de lock y el PID). En Linux: ¿`rename(tmp, db)`
        con la fuente todavía adjunta deja el lock sobre el inodo viejo y el archivo nuevo libre?
        Recordar que un lock `fcntl` del proceso se suelta al cerrar **cualquier** descriptor de ese
        archivo: el protocolo no abre la fuente dos veces en el mismo proceso.
      - **0.g — verificación barata.** Costo de un checksum por tabla independiente del orden
        (p. ej. `count(*)` + `bit_xor(hash(...))` sobre todas las columnas; para `vectors`, con y sin
        la columna `vector`) en backend-a.
      - **0.h — downgrade.** El binario `v0.9.0` abre la base compactada y contesta
        `get_references`/`impact`/búsqueda vectorial igual que sobre la base sin compactar (los
        mismos 60 casos del downgrade de TASK-016).
      - Salida del spike: método elegido (A es el recomendado, ver abajo), umbral calibrado y
        la tabla de números en `## Resultado`.

- [ ] **Paso 1 — tests que fallan** (lista en "Tests"), antes del código.

- [ ] **Paso 2 — `compact.rs`: el protocolo.** Método recomendado hasta que el spike diga otra cosa:
      **variante A**. Razones: usa el mismo DDL que crea una base nueva (el resultado es idéntico a
      un índice "desde cero" del binario, incluidas PK/UNIQUE), carga con los índices derivados
      abajo y los construye una vez al final con el código que ya existe y ya está probado
      (`enable_hnsw`, `rebuild_fts`), y no depende de que `COPY FROM DATABASE` sepa copiar un índice
      de una extensión experimental ni el schema interno del FTS. Costo: copia **solo las tablas que
      el binario conoce** (por eso el paso 3 de abajo).

      `compact(db_path, opts) -> Result<CompactReport>`; `<db>` = `index.duckdb`:

      1. **Dueño único.** Lo llama solo un proceso que **no** es el serve y tras `reclaim_db`
         (o con `DEVCTX_NO_AUTOSERVE`). Escribe `<db>.compact.lock` (PID + start time, como
         `serve.json`); si existe y su dueño vive, sale con "ya hay una compactación en curso".
      2. **Limpieza de restos.** Borra `<db>.compact.tmp`, `<db>.compact.tmp.wal` y el
         `temp_directory` de una corrida anterior muerta (el lock del punto 1 dice que está muerta).
      3. **Inspección de la fuente** (abrirla normal, una vez): checkpoint (pliega el WAL; si
         `checkpoint_is_safe` dice que no, **abortar**: no se compacta sin VSS una base con HNSW);
         leer usados/libres, dimensión guardada (`stored_dimension`), métrica del HNSW
         (`hnsw_metric`), si hay FTS (`has_fts`), `PENDING_*` de `index_meta`, y la lista de
         tablas, vistas, schemas e índices. Si hay **una tabla o schema que el binario no conoce**
         (fuera de `fts_main_vectors` y restos `devctx_repair_*`), abortar con el nombre: no se
         descarta nada en silencio. Cerrar. Tras cerrar no debe quedar `<db>.wal` con contenido; si
         queda, abortar.
      4. **Disco.** Espacio libre del filesystem de `<db>` ≥ tamaño actual del archivo (el "~1×";
         el destino mide menos, el margen cubre WAL y temporales). Si no alcanza: no empezar, decir
         cuánto falta y dejar la nota "debe" (ver disparo).
      5. **Copia.** `Store::open(<db>.compact.tmp, dim guardada)`; `ATTACH '<db>' AS src (READ_ONLY)`
         (toma el lock de lectura: un serve que se levante ahora falla con el error de lock);
         por cada tabla, en una transacción, `INSERT INTO t BY NAME SELECT * FROM src.t` (columnas
         por nombre: una base de desarrollo tiene `content_hash`/`hint` agregadas por `ALTER`);
         checkpoint entre tablas grandes si 0.e lo pide. `index_meta` se copia tal cual y recién
         después se escribe la nota `done` (así viaja dentro de la copia).
      6. **Índices derivados** en el destino: `enable_hnsw(métrica)` si la fuente lo tenía o lo
         debía (`PENDING_HNSW`), `rebuild_fts()` si lo tenía o lo debía; si alguno devuelve `false`
         (extensión ausente) y la fuente lo tenía: abortar. `CHECKPOINT`; cerrar el destino
         (se cierra también `src`).
      7. **Verificación en frío** (reabrir `<tmp>` con `Store::open`): (a) HNSW presente con la
         misma métrica y consultable por el índice; (b) FTS consultable; (c) checkpoint y cerrar:
         **no** queda `<tmp>.wal`.
      8. **Verificación contra la fuente, bajo lock, justo antes del reemplazo.** Una conexión en
         memoria con `src` y `<tmp>` adjuntos `READ_ONLY`: por tabla, `count(*)` y checksum iguales
         (todas las tablas; `memories` y `memory_symbol_references` siempre con todas las columnas);
         el set de tablas coincide. Como se compara **ahora**, una escritura que se hubiera colado
         en la fuente entre el paso 6 y este hace fallar la verificación. Soltar `<tmp>` (`DETACH`).
      9. **Reemplazo.** `fsync(<tmp>)`; `rename(<tmp>, <db>)` **con `src` todavía adjunta** (Unix:
         el lock sigue sobre el inodo viejo, nadie pudo abrir la base entre la verificación y el
         rename); `fsync` del directorio; `DETACH src`; borrar `<db>.compact.lock`. En Windows ver
         "Interacciones".
      10. **Reporte:** bloques y bytes antes/después, segundos, tablas y filas, índices rehechos.

- [ ] **Paso 3 — el disparo automático.**
      - **Quién marca.** `index_run`, al sellar el extractor (`pipeline.rs:769-771`), cuando la
        corrida es `full_reindex` **y** la rama venía de antes del grafo (`prev_extractor_stale` y
        la rama no tenía filas en `symbols` al empezar: eso es un índice ≤ 0.9.x, sin depender del
        número de `EXTRACTOR_VERSION`). Escribe `index_meta("", "", "compaction") =
        "owed:upgrade-0.10"` — clave de toda la base, como `PENDING_HNSW_META_KEY` — salvo que ya
        valga `done:…`. Un índice creado por 0.10 desde cero nunca la escribe.
      - **Una sola vez.** Tras compactar, la clave pasa a
        `done:<versión>:<epoch>:<bloques antes>-><después>`. Con `done`, ninguna corrida vuelve a
        marcar `owed` (una segunda rama actualizada después **no** vuelve a disparar: ver
        "Preguntas abiertas"; le queda el aviso y `devctx compact`).
      - **Umbral (propuesto, a calibrar en 0.a):** se compacta si `libres ≥ 25 %` de los bloques
        totales (`bloat_ratio() ≥ 1,33`) **y** `libres ≥ 64` bloques (16 MiB). Por qué: los tres
        upgrades medidos quedan en 31-34 % (backend-a, frontend, DevCtxEngine), así que 25 % los
        agarra con margen; el piso de 16 MiB evita reescribir bases chicas (legacy-migration: 36
        bloques en total) donde no se gana nada. Si `owed` pero no se llega al umbral: se escribe
        `done:…:skipped-below-threshold` y no se vuelve a evaluar.
      - **Quién ejecuta: el CLI, nunca el serve.** Al terminar bien un `devctx index --full` (o cada
        proyecto de `devctx reindex --full`): leer la nota (del reporte del serve o, en la vía
        directa, del store); si `owed` y se pasa el umbral →
        vía directa: soltar el store y llamar `compact`; vía serve: comprobar que no hay otra
        corrida en vuelo (`/index/progress`), `remote::reclaim_db` (como `repair`), `compact`. El
        próximo comando o tool vuelve a levantar el serve solo.
      - **Lo que NO dispara:** incrementales, hooks (`devctx index` suelto tras un commit: no puede
        tardar minutos ni parar un serve en silencio), `devctx watch`, la tool MCP `index_repo` y
        cualquier corrida dentro del serve. En esos casos la nota queda `owed` y se ve: `status` /
        `index_status` llevan `compaction: { state: "owed", free_blocks, used_blocks, hint:
        "run devctx compact" }` (aditivo, junto al bloque `memory`), y la corrida imprime una línea.
      - **Apagarlo:** `DEVCTX_NO_AUTO_COMPACT=1` (o clave de config `storage.auto_compact: false`,
        a decidir en la task) deja la nota `owed` y solo avisa.
      - **Cancelación:** `compact` es interrumpible hasta el paso 9; un Ctrl-C borra los
        temporales y deja `owed`.

- [ ] **Paso 4 — `devctx compact`.** `Command::Compact { dry_run, force }` en `main.rs`, con el
      molde de `cmd_repair`: para el serve del proyecto (`reclaim_db`), corre `compact` y muestra el
      reporte. Sin umbral (el usuario lo pidió), salvo "nada que recuperar" (0 libres) sin `--force`.
      `--dry-run` muestra usados/libres, lo que se espera recuperar y si el disco alcanza. Vale para
      cualquier base, no solo tras el upgrade. Solo la base del proyecto; el store central queda
      fuera. El aviso de bloat de `pipeline.rs:856-875` pasa a recomendar `devctx compact`
      (`repair` sigue siendo para índices ART rotos).

- [ ] **Paso 5 — limpieza al arrancar.** En `open_store` del CLI y al arrancar `serve`, antes de
      abrir: si hay `<db>.compact.tmp*` y el `compact.lock` no tiene dueño vivo, borrarlos (una
      línea por stderr). Nunca tocar `<db>` ni `<db>.wal`.

- [ ] **Paso 6 — medición** (ver "Medición") con el binario final; tabla en `## Resultado`.

- [ ] **Paso 7 — docs EN + ES y CHANGELOG** (ver "Docs").

- [ ] **Paso 8 — gate y cierre.** `cargo fmt --check`, `clippy --all-targets --locked -D warnings`
      (con y sin `--features gpu`), `cargo test --workspace --locked`; cross-check de
      Windows/macOS (junto con el Paso 7 de TASK-016); barrido de privacidad; `## Resultado`.

## Modos de fallo y estado en que queda la base

En todos, `<db>` es la base original salvo en la última fila.

| Falla | Qué queda | Qué ve el usuario |
|---|---|---|
| Otro proceso tiene la base (serve que no se pudo parar, MCP viejo) | original intacta, nada creado | error de lock con PID (`lock_message`); nota `owed` |
| Disco insuficiente | original intacta, nada creado | cuánto hace falta y cuánto hay; nota `owed`; el index ya terminó bien (exit 0, con aviso) |
| VSS/FTS no cargan y la fuente tiene ese índice | original intacta | "no se compacta sin la extensión"; nota `owed` |
| Tabla o schema desconocido en la fuente | original intacta | el nombre; nota `owed` |
| Corte (kill, apagón) durante la copia o los índices | original intacta + `<db>.compact.tmp*` huérfanos | el próximo arranque los borra (Paso 5); nota `owed` |
| Error de DuckDB en la copia (OOM, I/O) | original intacta; temporales borrados | el error; nota `owed` |
| La verificación no coincide (conteo, checksum, HNSW o FTS no consultables) | original intacta; temporales borrados | qué tabla o índice falló; nota `owed`; **no se reintenta sola** (`owed:failed-verify`, solo `devctx compact`) |
| Corte entre el `rename` y el `fsync` del directorio | o la original o la copia verificada, las dos completas | nada; si quedó la original, la nota sigue `owed` |
| Falla el `rename` (Windows: archivo abierto por otro) | original intacta; temporal borrado | el error; nota `owed` |
| Tras el reemplazo | la copia verificada, con la nota `done` adentro | reporte antes/después |

Un fallo de la compactación automática **nunca** hace fallar el `devctx index` que la disparó: el
índice ya está bien; se avisa y se sigue.

## Interacciones

- **`serve`.** La compactación corre en el CLI con el serve parado; el serve nunca compacta
  (reparte clones del store y no puede cerrar el archivo). Durante la copia la fuente está adjunta
  en solo lectura: un serve o un MCP que quiera levantarse recibe el error de lock de siempre y
  reintenta o lo muestra. Tras el reemplazo, el primer uso levanta un serve nuevo sobre el archivo
  nuevo. Una sesión MCP abierta no tiene la base abierta (rutea al serve), así que no estorba.
- **Downgrade a 0.9.0.** Mismo DuckDB (v1.5.5) en los dos binarios y la copia lleva **todas** las
  tablas, `graph_edges` incluida: 0.9.0 la abre y conserva sus llamadores (se mide en 0.h; es la
  condición con que el usuario aceptó el downgrade). `symbols`/`edges` siguen huérfanas e inocuas
  para 0.9.0, como antes.
- **Windows.** No se puede renombrar sobre un archivo abierto: allí el paso 9 suelta `src` antes
  del `rename`. Si otro proceso abrió la base en ese hueco, el `rename` falla y queda la original
  (seguro por construcción) — **a confirmar en el cross-check**: que DuckDB no abra con
  `FILE_SHARE_DELETE`. Reintento corto ante un antivirus o un indexador que tenga el archivo un
  instante. Si no se puede confirmar antes del tag: en Windows el disparo automático queda apagado
  y solo existe `devctx compact`, documentado.
- **`repair`.** Sin cambios. `repair` arregla ART rotos en sitio; `compact` recupera espacio con
  copia verificada. (Rebasar `repair` sobre `compact.rs` es una mejora posible, fuera de alcance.)

## Tests

Primero fallan, después pasan. En `crates/devctx-store/src/compact.rs` salvo donde se indica.

1. **Corte a mitad de la copia.** Un proceso hijo (patrón de
   `fts_drop_survives_a_crash_before_the_next_checkpoint`, `store.rs:1845`) arranca `compact` con
   un punto de falla inyectado (`std::process::abort` tras copiar la 2.ª tabla, y otra variante
   tras construir el HNSW y antes del `rename`): el padre reabre `<db>`, los conteos por tabla son
   los originales, HNSW y FTS siguen consultables, y quedan `<db>.compact.tmp*`; un segundo arranque
   los borra y un `compact` posterior termina bien.
2. **Disco insuficiente.** Con la consulta de espacio libre inyectable (un `fn` o un trait en
   `CompactOptions`), devolver menos que el tamaño de la base: error tipado, ningún archivo creado,
   la base sin tocar, la nota sigue `owed`.
3. **Base con HNSW** (`#[ignore]` si VSS no está, como los tests de HNSW existentes): tras
   `compact`, `hnsw_metric()` da la misma métrica (probar `cosine` e `ip`), `search` devuelve los
   mismos ids que antes y `bloat_ratio()` baja.
4. **Base con memorias.** `memories`, `memory_symbol_references` y los chunks de memoria de
   `vectors`: idénticos fila por fila tras `compact` (comparación completa, no solo conteo);
   `recall` de una memoria conocida la encuentra.
5. **Base con FTS:** `has_fts()` y `keyword_search` dan lo mismo.
6. **La verificación protege:** con una fila borrada del destino antes de verificar (falla
   inyectada), `compact` devuelve error y `<db>` es la original.
7. **Lock:** con otro `Store` abierto sobre `<db>` en otro proceso, `compact` falla con el error de
   lock y no crea nada; dos `compact` a la vez: el segundo sale por `compact.lock`.
8. **Tabla desconocida:** una tabla extra en la fuente aborta con su nombre.
9. **Disparo** (`crates/devctx-index`): un `--full` sobre una rama sin `symbols` deja
   `compaction = owed:…`; un incremental, un `--full` sobre un índice 0.10 y un `--full` con
   `done` no la tocan; con libres bajo el umbral queda `done:…skipped…`.
10. **Una sola vez** (`crates/devctx-cli/tests/`, vía directa con `DEVCTX_NO_AUTOSERVE`): índice
    fixture de forma 0.9.0 → `index --full` → la base se compactó y la nota es `done`; un segundo
    `index --full` no compacta.
11. **`devctx compact --dry-run`** no crea ni cambia archivos.
12. **`status`** lleva `compaction` con `owed` y con `done`.

## Medición

Con el binario final, mismo sandbox y método de TASK-016 (un repo por vez, `nice`, carga anotada),
sobre los índices 0.9.0 reales de **backend-a** y **frontend** (y DevCtxEngine como control):

| Qué | Cómo |
|---|---|
| Tamaño: 0.9.0 → tras `--full` en sitio → tras compactar | bloques usados/libres y bytes del archivo (`pragma_database_size()` sobre una copia) |
| Contra la meta | % sobre el índice 0.9.0 de partida, en usados y en bytes: **≤ +30 %** |
| Tiempo | segundos de la compactación (reloj monotónico), aparte del `--full`; desglose copia / HNSW / FTS / verificación |
| Pico de RAM | `VmHWM` del proceso que compacta (`wait4`/`ru_maxrss`) |
| Disco temporal | máximo de `<tmp>` + `<tmp>.wal` + `temp_directory` durante la corrida |
| Recorrido completo | `devctx index --full` por el serve con el disparo automático: tiempo total que ve el usuario |
| Downgrade | `v0.9.0` sobre la base compactada: mismas respuestas que sobre la no compactada |

Estas son las cifras que la **etapa B de TASK-016** copia a su tabla de §6 (fila de tamaño) y a los
docs.

## Docs

- `docs/13-keeping-the-index-fresh.md` + `docs/es/13-mantener-el-indice-al-dia.md`: en *Upgrading
  to 0.10*, el paso de compactación (cuándo corre sola, que para el serve, cuánto disco pide,
  cuánto tarda, cómo apagarla, `devctx compact`); corregir "The file keeps its size" (`:357`).
- `docs/07-performance.md` + `docs/es/07-rendimiento.md`: tamaños antes/después y tiempo medidos.
- `docs/08-design-decisions.md` + ES: el ADR del rollout (`:319`) deja de decir que el espacio no
  se recupera; por qué copia + rename y no `VACUUM`/`CHECKPOINT`.
- `docs/11-configuration.md` + ES: la variable o clave que apaga el disparo, si se agrega.
- `AGENTS.md` §6 (bullet de "Upgrading from 0.9.x") y "Things that will bite you" (necesita ~1× de
  disco; para el serve).
- `CHANGELOG.md` 0.10.0: *Added* (`devctx compact`, compactación única tras el upgrade, campo
  `compaction` en `status`), *Upgrading from 0.9.x* y *Known issues* (lo que quede: Windows,
  ramas actualizadas después).
- `README.md`/ayuda del CLI: `compact` en la lista de comandos.

## Criterios de aceptación

- [ ] Paso 0 hecho y escrito: método elegido con números; la compuerta 0.a pasada o la decisión del
      usuario registrada.
- [ ] backend-a y frontend, tras `index --full` en sitio + compactación: archivo y bloques usados
      **≤ +30 %** sobre el índice 0.9.0 de partida. Tiempo, `VmHWM` y disco temporal medidos y en
      el Resultado.
- [ ] El disparo automático corre **una vez** por base (nota `done` en `index_meta`), solo tras un
      `--full` que migró un índice sin grafo y solo sobre el umbral; incrementales, hooks, `watch`
      y el serve no compactan.
- [ ] `devctx compact` y `--dry-run` existen y están documentados.
- [ ] Ningún modo de fallo de la tabla deja una base distinta de "la original" o "la copia
      verificada": tests 1, 2, 6, 7 y 8 verdes.
- [ ] Tests 3, 4 y 5: HNSW (dos métricas), memorias y FTS idénticos tras compactar.
- [ ] `v0.9.0` abre la base compactada y contesta como sobre la no compactada.
- [ ] Nunca dos procesos sobre la base: la compactación solo corre con el serve parado y la fuente
      bajo lock; test 7 verde.
- [ ] Docs EN/ES y CHANGELOG al día; ningún doc dice ya que el espacio no se recupera.
- [ ] Gate verde, cross-check de Windows/macOS verde (o el disparo automático apagado en Windows y
      documentado), `devctx plan-status PLAN-009` sin warnings, barrido de privacidad vacío.

## Riesgos

- **La reescritura no baja los usados** (queda en +56 %): la compuerta 0.a lo detecta antes de
  escribir código; salida: decisión del usuario (aceptar por bytes de archivo, o buscar qué ocupa
  los 263 bloques de más de backend-a).
- **HNSW experimental.** Se reconstruye en el destino en vez de copiarse, y se verifica en frío; sin
  VSS no se compacta. El índice tiene que caber en RAM (ya es así al indexar).
- **Costo doble de índices derivados:** el `--full` del upgrade ya rehízo HNSW y FTS
  (`pipeline.rs:797-849`) y la compactación los vuelve a construir. Se mide; si pesa, opción para
  después: que el `--full` de un upgrade deje los índices "debidos" y los construya la compactación.
- **Parar el serve desde `devctx index --full`** corta lo que otros clientes estuvieran haciendo
  (igual que `repair`). Mitigación: solo si no hay corrida en vuelo; mensaje explícito; apagable.
- **Pérdida de memorias** (no regenerables): verificación completa de `memories` y reemplazo solo
  tras verificar; la original nunca se modifica.
- **Tablas que el binario no conoce** (una base tocada por una versión futura): se aborta, no se
  descartan.
- **Windows:** el `rename` y el hueco sin lock; ver "Interacciones". Sin máquina Windows local: lo
  cubre el cross-check y, si no alcanza, el disparo apagado ahí.
- **Zona de conflicto con PLAN-010** (`store.rs`, `pipeline.rs`, `state.rs`): módulo nuevo
  `compact.rs` para tocar lo mínimo.

## Fuentes

- DuckDB, *Reclaiming space*: <https://duckdb.org/docs/current/operations_manual/footprint_of_duckdb/reclaiming_space>
- DuckDB, `COPY` (*COPY FROM DATABASE … TO*): <https://duckdb.org/docs/current/sql/statements/copy>
- DuckDB, extensión VSS: <https://duckdb.org/docs/current/core_extensions/vss>
- DuckDB, extensión FTS: <https://duckdb.org/docs/current/core_extensions/full_text_search>
- Código de DuckDB v1.5.5 empaquetado en `libduckdb-sys-1.10505.0` (`duckdb.tar.gz`):
  `src/storage/single_file_block_manager.cpp` (`Truncate`),
  `src/planner/binder/statement/bind_copy_database.cpp`.

Las páginas de la doc son las de `current`, no fijadas a v1.5.5: lo que dependa de la versión se
confirma en el Paso 0 con el motor del repo.

## Resultado
<!-- SE LLENA AL CERRAR (estado done/skipped). Vacío mientras esté pending. Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
