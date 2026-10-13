# TASK-018 — Compactar la base una vez tras el upgrade 0.9.0 → 0.10 (y `devctx compact`)

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** —
- **Estado:** `pending`

(La motivó la medición de TASK-016, que ya está escrita; no espera trabajo de esa task. Las
mediciones finales y el cierre del plan son TASK-019, que depende de esta.)

---

## Objetivo

Que actualizar un índice 0.9.0 a 0.10 no deje el archivo de la base en ~2×. La 0.10.0 lleva un paso
que **reescribe la base una vez** después del `index --full` que migra un índice 0.9.0 (copia a un
archivo nuevo, verificación, reemplazo atómico), más un comando explícito `devctx compact`.

**Meta y base de comparación (una sola, usada en la compuerta, los criterios y la medición):**
`B0` = el índice 0.9.0 **justo antes** del `--full` en sitio, medido sobre una copia tomada en ese
momento (bloques usados y bytes del archivo). Tras upgrade + compactación: usados **≤ 1,30 × B0** y
bytes **≤ 1,30 × B0**. Sin perder una fila ni una memoria, y con la base original utilizable ante
cualquier fallo.

Decisión del usuario del 2026-10-13 ("compactar al actualizar", master §12), tras el hallazgo 5 de
TASK-016. Cambia DD-21 (que decía "medir y no tocar").

**Relación con PLAN-010 TASK-012.** Mismo mecanismo, dos problemas:

| | PLAN-009 TASK-018 (esta) | PLAN-010 TASK-012 |
|---|---|---|
| Qué resuelve | el salto único del upgrade 0.9.0 → 0.10 | el crecimiento con el uso normal (reindexados que reescriben) |
| Cuándo | bloquea la 0.10.0 | no bloquea la 0.10.0 |
| Disparo | automático por migración + `devctx compact` | a decidir tras medir la curva de N ciclos |
| Entrega | el mecanismo seguro de copia + reemplazo | lo reusa; decide si hace falta un disparo periódico y su clave de config |

## Decisiones de diseño (revisión del 2026-10-13)

| # | Decisión |
|---|---|
| D1 | Esta task no depende de TASK-016. La "etapa B" (mediciones finales, cierre de §6, cross-check, informe) es **TASK-019**, que depende de esta |
| D2 | La nota de compactación es **por base**, pero se **rearma** con cada migración de una rama pre-grafo **que ya existía**, y siempre sujeta al umbral |
| D3 | Disparo automático solo desde `devctx index --full` / `devctx reindex --full`, y solo si: se anuncia antes de parar el serve, el serve cierra limpio, no hay corrida en vuelo y hay TTY. Si no, queda `owed` |
| D4 | Apagado en 0.10.0: solo `DEVCTX_NO_AUTO_COMPACT=1`. La clave de config llega con el disparo periódico de PLAN-010 TASK-012 |
| D5 | Sin copia de la base entera, pero con red: antes del `rename`, lo no regenerable (`memories`, `memory_symbol_references`, `sessions`) se exporta a un sidecar junto a la base, que se borra tras la próxima corrida exitosa. `--keep-original` es opcional en `devctx compact` |
| D6 | Se acepta reconstruir HNSW y FTS dos veces en el upgrade (el `--full` y la compactación) en la 0.10.0; se mide |

## Contexto verificado

Líneas contra `7aac2e5` (2026-10-13). Re-verificar al arrancar.

**La medición (TASK-016, Resultado, hallazgo 5 y Paso 2).** `--full` del candidato en sitio sobre el
índice 0.9.0, bloques de 256 KiB de `pragma_database_size()`:

| Repo | usados antes → después | archivo antes → después | usados, candidato desde cero | usados, 0.9.0 desde cero |
|---|---|---|---|---|
| backend-a | 504 → 786 (+56 %) | 153 → 296 MiB | 523 | 479 |
| frontend | 293 → 463 (+58 %) | 83 → 171 MiB | 316 | 279 |
| DevCtxEngine | 114 → 173 (+52 %) | — | 116 | 106 |
| backend-b | 85 → 124 (+46 %) | — | 89 | 76 |
| legacy-migration | 30 → 36 | — | 26 | 25 |

"Antes" es mayor que "0.9.0 desde cero" porque esos índices ya se habían abierto con el candidato y
usado (nota ¹ de TASK-016). De la tabla salen (cuenta, **no medición**): backend-a queda con ~398
bloques libres de ~1 184 (34 %) y frontend con ~221 de ~684 (32 %); DevCtxEngine en TASK-003 quedó
en 167 usados / 75 libres (31 %). Los *usados* también crecen: 786 contra 523 del mismo contenido
desde cero. Sacar solo los libres deja a backend-a en ~196 MiB (+28 % en bytes, +56 % en usados).
La hipótesis —**no verificada**— es que copiar fila por fila deja los usados cerca de 523. El
Paso 0 la confirma o la tumba antes de escribir código de producto.

**Motor.** `Cargo.lock`: `duckdb` y `libduckdb-sys` `1.10505.0` = **DuckDB v1.5.5**
(`pragma_version.cpp` del tarball). `v0.9.0` usa la misma versión (`git show v0.9.0:Cargo.lock`).

**Qué dice DuckDB** (fuentes al final; "leído" = código del tarball, **no ejecutado**):

- `VACUUM` no recupera espacio; `CHECKPOINT` libera tras borrar; para compactar la doc recomienda
  `ATTACH` + `COPY FROM DATABASE`.
- El checkpoint solo **trunca los bloques libres del final** (`SingleFileBlockManager::Truncate`:
  corta mientras `block_id + 1 == max_block`). Coincide con el 167/75 que no se mueve de TASK-003.
- `COPY FROM DATABASE` recrea todo el catálogo (`BindCopyDatabaseSchema`) y después hace un
  `INSERT … SELECT` por tabla (`BindCopyDatabaseData`): el HNSW existiría antes de la carga y el
  schema `fts_main_vectors` se copiaría como tablas y macros comunes. Ninguna fuente dice que
  funcione con VSS/FTS.
- VSS: persistencia experimental; el índice se serializa entero en cada checkpoint, tiene que caber
  en RAM, no cuenta para `memory_limit`, y el WAL no lo recupera bien.
- FTS: vive en `fts_main_<tabla>` y no se actualiza solo.

**El store.**

- `Store::open` (`crates/devctx-store/src/store.rs:188-214`): `Connection::open`, límites
  (`:327-336`), **extensiones antes del schema** (`:205-211`, el fallo fatal de TASK-003) e
  `init_schema` (`crates/devctx-store/src/schema.rs:12-50`), que **escribe** en una base existente
  (tablas nuevas, `ALTER`, vista, `drop_broken_project_indexes` `:135-144`, checkpoint).
- **Nadie mira un lock propio al abrir.** Abren la base por repo: `open_store` del CLI
  (`crates/devctx-cli/src/main.rs:4371-4381`, `Store::open` en `:4373`), `AppState::build`
  (`crates/devctx-mcp/src/state.rs:320`) —el serve y cualquier tool que lo relevanta— y la TUI
  (`crates/devctx-tui/src/lib.rs:148`). La única exclusión hoy es el lock de archivo de DuckDB.
- `Store::try_clone` (`store.rs:305-317`) comparte la instancia: el serve reparte clones y **no
  puede cerrar el archivo para reemplazarlo**.
- Índices derivados: `enable_hnsw` (`:381-398`, la métrica va en el **nombre**
  `idx_vectors_hnsw_{cosine|ip}`), `hnsw_metric` (`:402`), `drop_hnsw` (`:499-510`), `rebuild_fts`
  (`:515-527`), `has_fts` (`:535-541`), `drop_fts` (`:556-562`); `hnsw_estimated_bytes` (`:51`).
- `checkpoint`/`try_checkpoint`/`force_checkpoint` (`:580-618`); `checkpoint_is_safe`
  (`schema.rs:60-77`). Un WAL que sobrevive a su proceso rompe los ART al replayarse (`:564-582`).
- `Store::bloat_ratio` (`store.rs:627-640`) ya existe.
- **`devctx repair` deja restos que pueden ser la única copia de una tabla.**
  `Store::rebuild_indexes` (`store.rs:650-677`), por tabla: un batch `CREATE TABLE
  devctx_repair_<t> AS SELECT * FROM <t>; DROP TABLE <t>;` (`:664-667`), `init_schema` (`:668`) y
  otro batch `INSERT INTO <t> SELECT * FROM devctx_repair_<t>; DROP TABLE devctx_repair_<t>;`
  (`:669-672`). Un corte entre los dos deja `<t>` vacía (recreada por el próximo `open`) y los
  datos solo en `devctx_repair_<t>`. Además cada intento empieza con `DROP TABLE IF EXISTS
  devctx_repair_<t>` (`:662-663`): reintentar `repair` **destruye** ese resto. `table_names`
  (`:680-689`) los esconde.
- `index_meta` (`schema.rs:326-332`, PK `(repo_path, branch, key)`); accesores en
  `crates/devctx-store/src/state.rs:153-196`. Notas de toda la base con `("", "")`:
  `PENDING_HNSW_META_KEY` / `PENDING_FTS_META_KEY` (`crates/devctx-index/src/pipeline.rs:268-277`).
- `Store::extractor_stale` (`state.rs:212-227`) da `true` **si no hay sello** (`:219-224`): también
  en una rama que nunca se indexó. No sirve solo para reconocer una migración.
- Tablas (`schema.rs:146-401`): `vectors` (código **y** chunks de memorias, PK `id`), `graph_edges`,
  `routes`, `memories` (PK), `memory_symbol_references` (**sin PK**), `projects`, `sessions`,
  `index_state`, `file_state`, `index_meta`, `symbols` y `edges` (**sin PK ni UNIQUE**),
  `branch_lineage`; vista `live_edges`; columnas agregadas por `ALTER` en bases de desarrollo
  (`symbols.content_hash`, `edges.hint`, `schema.rs:24-31`): el orden físico puede diferir.
  **Las memorias locales no son regenerables.**

**El final de un index** (`pipeline.rs`): `InFlight` (`:265`), `prev_extractor_stale` (`:350-355`),
`full_reindex` (`:382`), sello del extractor (`:769-778`), reconstrucción de HNSW/FTS (`:797-849`),
checkpoint (`:854`) y el aviso de bloat (`:856-875`, `BLOAT_WARN_RATIO = 3.0` en `:884`), que manda
a `devctx repair`. Un upgrade deja ~1,5× (usados + libres sobre usados): **ese aviso no salta**.

**Parar el serve.** `remote::reclaim_db` (`crates/devctx-cli/src/remote.rs:779-810`) devuelve
`bool`. Por dentro `terminate_ours` (`:759-774`) llama `procown::terminate_patient`, que manda
SIGTERM, espera el checkpoint y **escala a SIGKILL**; `procown::Termination`
(`crates/devctx-core/src/procown.rs:704-716`) tiene `Gone` tanto para "salió por SIGTERM" como para
"lo mató SIGKILL": hoy no se distinguen, y un SIGKILL deja un WAL. En Windows devuelve
`Unsupported`: devctx no puede parar un serve ahí. `indexing_in_flight` (`remote.rs:695-707`).

**CLI.** `enum Command` (`main.rs:52`), `Repair` (`:105`), despacho (`:604-804`), `cmd_repair`
(`:4211-4243`). `cmd_index` (`:4014`): por el serve (`:4020-4040`) o directo (`:4041-4070`).
`cmd_reindex` (`:863`) va por el serve. Los hooks corren `devctx index` suelto
(`crates/devctx-cli/src/hooks.rs:63-78`).

**`status`.** Bloque `memory`: `State::memory_json` (`crates/devctx-mcp/src/state.rs:449-451`);
`hint` de índice viejo en `:1601-1608`.

**Docs que hoy dicen lo contrario:** `docs/13-keeping-the-index-fresh.md:357`,
`docs/08-design-decisions.md:319`, `docs/07-performance.md:174`, sus pares en `docs/es/`, y
`CHANGELOG.md` (0.10.0).

## Archivos

- **Crear:** `crates/devctx-store/src/compact.rs`; tests en ese módulo y de integración en
  `crates/devctx-cli/tests/`; el script de medición del Paso 0 bajo `scripts/`.
- **Modificar:** `crates/devctx-store/src/{lib.rs,store.rs,error.rs}` (`Store::open` respeta el
  lock de compactación); `crates/devctx-core/src/procown.rs` (distinguir SIGTERM de SIGKILL);
  `crates/devctx-index/src/pipeline.rs` (nota `owed`, aviso de bloat);
  `crates/devctx-cli/src/{main.rs,remote.rs}`; `crates/devctx-mcp/src/state.rs` (`compaction` en
  `status`/`index_status`); `crates/devctx-tui` (mensaje del lock); docs EN + ES, `AGENTS.md`,
  `CHANGELOG.md`; `PLAN-009-design.md` (DD-21) y el master.

## Protocolo (Paso 2)

Nombres: `<db>` = la ruta **canónica** de `index.duckdb` (si es un symlink se resuelve y todos los
temporales van junto al archivo real, en el mismo filesystem); `<tmp>` = `<db>.compact.tmp`;
`<lock>` = `<db>.compact.lock`; `<side>` = `<db>.pre-compact.duckdb`.

**Invariantes** (cada una con test):

- **I1 — lock propio, de sistema operativo.** `<lock>` es un archivo con un lock exclusivo del SO
  (no PID + start time: fuera de Linux no hay start time), tomado antes de tocar nada y soltado al
  final. `Store::open` de una base en archivo y el arranque del serve intentan ese lock en modo
  compartido y sin esperar; si está tomado **se niegan a abrir** con un error tipado ("la base se
  está compactando; reintentá"), que `open_store`, `AppState::build` y la TUI muestran tal cual.
  El compactador abre `<tmp>` por una entrada interna que no pasa por esa comprobación.
- **I2 — una sola conexión sostiene la fuente.** La conexión `H` adjunta `<db>` como `src`
  (`READ_ONLY`) antes de leer nada y **no la suelta hasta después del `rename`**. Todo lo que se lee
  de la fuente pasa por `H`. El proceso no abre `<db>` por ningún otro lado (un lock `fcntl` se
  suelta al cerrar *cualquier* descriptor del archivo). El lock de DuckDB que `H` mantiene es lo que
  frena a un binario que no conoce `<lock>` (0.9.0, un 0.10 viejo).
- **I3 — sin WAL ajeno.** Con `src` adjunta se afirma que `<db>.wal` no existe o mide 0 bytes:
  **justo después del `ATTACH` y justo antes del `rename`**. Si existe, se aborta. Un `rename`
  nunca deja un WAL viejo junto a un archivo nuevo.
- **I4 — la fuente no se escribe.** Desde I1 hasta el `rename`, nada escribe en `<db>`.
  (El único open normal de la fuente es la *preparación* opcional del punto 2, antes de adjuntarla
  y declarada.)
- **I5 — nunca se borra `<tmp>` si `<db>` no existe.** Ni en la limpieza al arrancar ni en ningún
  camino de error. Y **nunca "borrar y renombrar"**: el reemplazo es un `rename` sobre `<db>` o no es.

`compact(db_path, opts) -> Result<CompactReport>`:

1. **Previos, antes de parar nada** (los corre también quien dispara, antes de `reclaim_db`):
   `<db>` existe; canonicalizar; el directorio se puede escribir (no es un filesystem de solo
   lectura); espacio **disponible para el usuario** (no el libre total) ≥ tamaño actual del archivo;
   el dueño del archivo es el usuario efectivo (si no, abortar: el `rename` cambiaría el dueño);
   criterio de RAM: `hnsw_estimated_bytes` + `memory_limit` de DuckDB ≤ 80 % de la memoria
   disponible (umbral a calibrar con el `VmHWM` del Paso 0); VSS y FTS se pueden cargar si la base
   los usa. Cualquier "no": no se empieza y se dice qué falta.
2. **Lock y limpieza.** Tomar `<lock>` (I1); si está tomado: "ya hay una compactación en curso".
   Con el lock en mano, borrar `<tmp>`, `<tmp>.wal` y el directorio temporal de una corrida muerta
   (respetando I5). *Preparación* (solo si `<db>.wal` existe con contenido, y solo en `devctx
   compact`; el disparo automático no llega aquí con WAL, ver Paso 3): un `Store::open` normal por
   la entrada interna, `try_checkpoint`, cerrar. Es un open como el de cualquier comando (replay,
   `init_schema`); si falla o el WAL sigue ahí, abortar.
3. **Sostener la fuente.** `H` = conexión en memoria; **primero** `apply_resource_limits` y carga
   de VSS + FTS (como `Store::open`: sin VSS, adjuntar una base con HNSW es el fallo fatal de
   TASK-003), después `ATTACH '<db>' AS src (READ_ONLY)`. Afirmar I3.
4. **Inventario y compatibilidad.** Crear el destino: `ATTACH '<tmp>' AS dst`, `USE dst`,
   `init_schema(dim guardada en la fuente)`. El set de objetos conocidos **sale del catálogo de
   `dst` recién creado**, no de una lista a mano. Contra él, por `duckdb_tables()`,
   `duckdb_columns()`, `duckdb_views()`, `duckdb_sequences()`, `duckdb_functions()` (macros),
   `duckdb_indexes()` y `duckdb_schemas()` de `src`:
   - tabla de `src` que no está en `dst` → **abortar nombrándola**, salvo `devctx_repair_*`
     **vacía** (se ignora y no se copia). Una `devctx_repair_*` **con filas aborta nombrándola**:
     puede ser la única copia de esa tabla (un `repair` cortado); el mensaje dice que no se
     reintente `repair` y cómo recuperar las filas a mano;
   - por tabla, columnas y tipos: una columna de `src` que `dst` no tiene, o con otro tipo →
     abortar nombrando tabla y columna. Una columna de `dst` que `src` no tiene se llena con NULL
     (y se declara en el reporte);
   - vista, secuencia, macro, índice o schema de `src` que `dst` no tiene → abortar nombrándolo.
     Excepciones cerradas, que se reconstruyen o se descartan a propósito: el schema
     `fts_main_vectors` y sus objetos, `idx_vectors_hnsw*`, y los índices que
     `drop_broken_project_indexes` ya borra en cada open.
   Leer de `src`: métrica del HNSW, si hay FTS, `PENDING_HNSW`/`PENDING_FTS`, usados/libres.
5. **Copia.** Por tabla, en una transacción: `INSERT INTO dst.<t> BY NAME SELECT <columnas comunes>
   FROM src.<t>` (lista explícita). Checkpoint de `dst` entre tablas grandes si 0.e lo pide.
   En `dst.index_meta`: escribir `('','','compaction') = 'done:<versión>:<epoch>:<motivo>'` (sin
   tamaños: el "después" no se conoce todavía; van en el reporte) y, si los índices se van a
   construir, borrar `PENDING_HNSW`/`PENDING_FTS`. `CHECKPOINT dst`; `USE memory`; `DETACH dst`.
   `src` sigue adjunta.
6. **Índices derivados**, en otra conexión `B` = `Store` sobre `<tmp>` (entrada interna):
   `enable_hnsw(métrica)` si la fuente lo tenía o lo debía, `rebuild_fts()` ídem; si devuelven
   `false` y la fuente lo tenía → abortar. Checkpoint, cerrar. (Si el Paso 0 muestra que
   `init_schema` sobre un catálogo adjunto no sirve, el punto 4 crea `<tmp>` con `B` y lo cierra
   antes de adjuntarlo; `src` no se suelta en ningún caso.)
7. **Verificación en frío**, en otra conexión `V` = `Store` nuevo sobre `<tmp>`: HNSW presente, con
   la misma métrica y usado por la consulta; FTS consultable; una búsqueda vectorial y una por
   palabra devuelven filas; checkpoint y cerrar: no queda `<tmp>.wal`.
8. **Verificación contra la fuente**, en `H` (que nunca soltó `src`): `ATTACH '<tmp>' AS chk
   (READ_ONLY)` y, por **cada** tabla del inventario:
   - `count(*)` igual;
   - `sum(hash(<columnas explícitas, ordenadas por nombre>)::HUGEINT)` igual. `sum`, no
     `bit_xor`: `symbols`, `edges` y `memory_symbol_references` no tienen PK y dos filas
     duplicadas se anulan con xor;
   - `vectors`: la columna `vector` entra **siempre** para las filas de memoria (`memory_type IS
     NOT NULL`; re-verificar el predicado contra cómo se guardan los chunks de memoria) y para las
     de código según el costo medido en 0.g (como mínimo `id` + `content_hash` + el resto);
   - `memories` y `memory_symbol_references`, además: `src EXCEPT ALL chk` y `chk EXCEPT ALL src`
     dan **0 filas**;
   - **única exclusión, declarada:** en `index_meta`, exactamente la fila `('','','compaction')` y
     las filas `('','', PENDING_HNSW_META_KEY)` / `('','', PENDING_FTS_META_KEY)` cuando el punto 5
     las borró. Todo lo demás de `index_meta` (sellos de extractor, fingerprints de embedding,
     excludes, generación, notas de link) se compara.
   `DETACH chk`.
9. **Red de memorias (D5).** Desde `H`: `ATTACH '<side>.tmp'`, `CREATE TABLE … AS SELECT * FROM
   src.{memories, memory_symbol_references, sessions}`, checkpoint, `DETACH`, `fsync`, `rename` a
   `<side>`. Son KB-MB. Lo borra la próxima corrida de index que termine bien sobre la base nueva
   (o `devctx compact`); los docs dicen cómo restaurar desde él.
10. **Reemplazo.** Copiar el modo de `<db>` a `<tmp>`; `fsync(<tmp>)`; **afirmar I3 otra vez**;
    `rename(<tmp>, <db>)` con `src` adjunta (Unix: el lock sigue sobre el inodo viejo); `fsync` del
    directorio; `DETACH src`; soltar y borrar `<lock>`. Con `--keep-original`: antes del `rename`,
    un hard link de `<db>` a `<db>.pre-compact` (pide ~2× de disco; solo en `devctx compact`).
    Windows: ver "Interacciones".
11. **Reporte:** bloques y bytes antes/después (leídos de la base nueva), segundos por fase, filas
    por tabla, índices rehechos, columnas llenadas con NULL, ruta del sidecar.

Un Ctrl-C en cualquier punto hasta el `rename` borra los temporales (I5), suelta el lock y deja la
nota como estaba.

## Pasos

- [ ] **Paso 0 — spike (decide el método; sin código de producto).** Sandbox como el de TASK-016
      (`mktemp -d` fuera de `/tmp`, `DEVCTX_HOME`/`HOME`/`XDG_*` propios), copias de los índices
      0.9.0 (`B0`) y de los actualizados en sitio de backend-a, frontend y DevCtxEngine, con el
      DuckDB del repo (binario de prueba o test `#[ignore]`). Por variante: usados/libres, bytes,
      tiempo, `VmHWM`, pico de disco.
      - **0.a — ¿alcanza la meta? (compuerta).** Reescribir y comparar contra `B0`. Si usados o
        bytes quedan > 1,30 × `B0`: **parar** y llevar los números al usuario.
      - **0.a' — ¿de dónde salen los 263 bloques de backend-a (786 − 523)?** Sobre el índice
        actualizado y sobre el candidato desde cero: (i) por tabla, `count(*)` vivo contra filas
        almacenadas y bloques (`pragma_storage_info`), con `vectors.vector` y `vectors.text` aparte;
        (ii) filas vivas del actualizado contra el desde-cero, por tabla; (iii) bloques de HNSW, de
        índices ART y de metadata; (iv) dos `CHECKPOINT` más y re-medir. Si la causa son row groups
        a medio borrar, la copia fila por fila debe quedar cerca de 523; si es otra cosa (HNSW
        serializado dos veces, `graph_edges` más grande), se anota y cambia la expectativa.
      - **0.b — método de copia.** (A, recomendado) el protocolo de arriba; confirmar que
        `USE dst` + `init_schema` crea el schema en un catálogo adjunto y que `INSERT … BY NAME`
        hace lo que el punto 4 supone (columna que falta en la fuente → NULL; columna de más en la
        fuente → error). (B) `COPY FROM DATABASE src TO dst` con VSS/FTS cargados: ¿copia el HNSW?,
        ¿a qué costo?, ¿FTS consultable?, ¿conserva el nombre del índice? `EXPORT`/`IMPORT
        DATABASE` descartado salvo que A y B fallen.
      - **0.c — HNSW** tras reabrir en frío: listado en `duckdb_indexes()`, `EXPLAIN` lo usa, los k
        vecinos de 10 consultas coinciden con la fuente (anotar si mismo orden o mismo conjunto).
      - **0.d — FTS:** `has_fts()` y 10 `keyword_search` iguales a la fuente.
      - **0.e — WAL.** ¿`ATTACH … (READ_ONLY)` abre una fuente con `.wal`? ¿lo replaya, lo ignora o
        falla? ¿Queda `<tmp>.wal` tras checkpoint + cerrar? ¿Cuánto crece el WAL del destino?
        **¿DuckDB 1.5.5 detecta un WAL que no corresponde al archivo** (poner el `.wal` de una base
        junto a otra y abrir)? Lo que haga —replayarlo, apartarlo, negarse— se anota: I3 no depende
        de la respuesta, pero el mensaje de error y los docs sí.
      - **0.f — locks (obligatorio; si falla, el diseño no vale).** Con `src` adjunta `READ_ONLY`
        por `H`: otro proceso que abre `<db>` en escritura **falla** con el error de lock; un
        `Store` sobre `<tmp>` abierto y cerrado en el mismo proceso **no** suelta el lock de `src`;
        `rename(<tmp>, <db>)` con `src` adjunta deja el lock sobre el inodo viejo. Lock de SO sobre
        `<lock>`: con qué API (la de `std` si el MSRV la tiene, o una dependencia chica) en Linux,
        macOS y Windows.
      - **0.g — checksum.** `hash()` distingue NULL de `''`, acepta `FLOAT[n]` y da lo mismo en dos
        bases para la misma fila; costo de `sum(hash(...)::HUGEINT)` en backend-a con y sin
        `vector`; costo de `EXCEPT ALL` sobre `memories`.
      - **0.h — downgrade.** `v0.9.0` abre la base compactada y contesta igual que sobre la no
        compactada (los 60 casos del downgrade de TASK-016).
      - **0.i — calibrar** el umbral del disparo y el criterio de RAM con los números reales.

- [ ] **Paso 1 — tests que fallan** (lista en "Tests"), antes del código.

- [ ] **Paso 2 — `compact.rs` y el lock en `Store::open`:** el protocolo de arriba con sus cinco
      invariantes.

- [ ] **Paso 3 — el disparo automático.**
      - **Quién marca, y cuándo.** `index_run`, **al empezar** una corrida `full_reindex` sobre una
        rama que (a) **ya existía** —filas en `index_state` o `file_state` para ese
        `(repo_path, branch)` antes de la corrida— y (b) es **pre-grafo**: sin filas en `symbols`
        para esa rama y sin sello de extractor vigente. Escribe `index_meta('','','compaction') =
        'owed:upgrade'`, **pisando cualquier `done:*`** (rearme, D2). Una rama nueva sobre una base
        0.9.0 no cumple (a) y **no toca la nota**; un índice creado por 0.10 no cumple (b). Marcar
        al empezar hace que una corrida cancelada deje `owed` y que la que la termina se evalúe.
      - **Estados de la nota** (clave de toda la base): ausente · `owed:upgrade` ·
        `owed:failed-verify` · `done:<versión>:<epoch>:compacted` ·
        `done:<versión>:<epoch>:skipped-below-threshold`. Ningún `done` bloquea un `owed` posterior.
      - **Umbral (propuesto, se calibra en 0.i):** `libres ≥ 25 %` de los bloques totales
        (`bloat_ratio() ≥ 1,33`) **y** `libres ≥ 64` bloques (16 MiB). Los upgrades medidos dan
        31-34 % (cuenta derivada, a confirmar); el piso evita reescribir bases chicas. Con `owed` y
        bajo el umbral: `done:…:skipped-below-threshold`.
      - **Quién ejecuta: el CLI, nunca el serve**, y solo al terminar **sin cancelación** un
        `devctx index --full` o cada proyecto de `devctx reindex --full`. Condiciones, todas (D3):
        nota `owed:upgrade`; umbral; `DEVCTX_NO_AUTO_COMPACT` sin definir; **hay TTY**; los previos
        del punto 1 del protocolo pasan; no hay corrida en vuelo (`/index/progress`); se **anuncia**
        ("voy a parar el serve de este proyecto para compactar; ~N MiB, ~T s") antes de parar; y el
        serve **cierra limpio**: salió por SIGTERM y no dejó `<db>.wal`. Para eso
        `procown::Termination` distingue "salió tras SIGTERM" de "hubo que matarlo", y
        `reclaim_db` deja de devolver un `bool`. Si lo mató SIGKILL, no paró, no se pudo verificar
        (Windows: `Unsupported`) o quedó un WAL: **no se compacta**, queda `owed`, se dice por qué.
        En la vía directa (`DEVCTX_NO_AUTOSERVE`) no hay serve que parar: se suelta el store y
        sigue igual, con I3.
      - **Lo que NO dispara:** incrementales, hooks, `devctx watch`, la tool MCP `index_repo`, una
        corrida sin TTY, cualquier corrida dentro del serve. La nota queda y se ve.
      - **`owed:failed-verify` no se reintenta solo:** solo `devctx compact`.
      - **Visibilidad.** `status` / `index_status`: `compaction: { state, free_blocks, used_blocks,
        hint }` (aditivo), con `owed` y `owed:failed-verify` como estados **distintos** y `hint`
        propio cada uno. El aviso de bloat de `pipeline.rs:856-875` baja de 3,0× al mismo umbral
        del disparo y recomienda `devctx compact` (`repair` queda para ART rotos).

- [ ] **Paso 4 — `devctx compact`.** `Command::Compact { dry_run, force, keep_original }`. Corre los
      previos, anuncia, para el serve con la misma regla de cierre limpio (si lo mató SIGKILL hace
      la *preparación* del punto 2 en vez de abortar: el usuario lo pidió explícitamente), ejecuta
      el protocolo y muestra el reporte. Sin umbral, salvo "0 bloques libres" sin `--force`.
      `--dry-run`: usados/libres, lo que se espera recuperar, previos, nada escrito. Funciona con o
      sin la clave `compaction` y con `PENDING_*` presentes. Solo la base del proyecto.

- [ ] **Paso 5 — limpieza al arrancar.** En `open_store` y al arrancar `serve`, tras comprobar
      `<lock>` (I1): si hay `<tmp>*` sin compactación viva **y `<db>` existe**, borrarlos (una
      línea por stderr). Si `<db>` no existe, no se borra nada y se avisa (I5). Un `<lock>` sin
      dueño se borra. Nunca tocar `<db>`, `<db>.wal` ni `<side>`.

- [ ] **Paso 6 — medición propia** (tabla en "Medición") para el Resultado; la medición final del
      plan es TASK-019.

- [ ] **Paso 7 — docs EN + ES y CHANGELOG** (ver "Docs").

- [ ] **Paso 8 — gate y cierre.** `cargo fmt --check`, `clippy --all-targets --locked -D warnings`
      (con y sin `--features gpu`), `cargo test --workspace --locked`; barrido de privacidad;
      `## Resultado`. El cross-check de Windows/macOS es de TASK-019.

## Modos de fallo y estado en que queda la base

| Falla | Qué queda | Qué ve el usuario | Nota |
|---|---|---|---|
| Un previo no pasa (disco, solo lectura, dueño, RAM, extensión) | original; nada creado; **el serve no se paró** | qué falta y cuánto | `owed` |
| El serve no cerró limpio (SIGKILL, no paró, Windows) o hay corrida en vuelo | original | por qué no se compactó | `owed` |
| `<lock>` tomado | original | "ya hay una compactación" | sin cambio |
| Otro proceso tiene la base (binario viejo) | original | error de lock con PID | `owed` |
| `<db>.wal` con contenido tras el `ATTACH` o antes del `rename` (I3) | original **y su WAL, sin tocar**; temporales borrados | "hay escrituras sin plegar; corré `devctx compact`" | `owed` |
| Tabla, columna, vista, macro, secuencia, índice o schema desconocido | original; temporales borrados | el nombre | `owed` |
| `devctx_repair_<t>` con filas | original; temporales borrados | el nombre, y que **no** se reintente `repair` | `owed` |
| Corte (kill, apagón) antes del `rename` | original + `<tmp>*` y `<lock>` huérfanos | el próximo arranque los limpia | `owed` |
| Error de DuckDB (OOM, I/O) | original; temporales borrados | el error | `owed` |
| La verificación no coincide | original; temporales borrados | qué tabla o índice | `owed:failed-verify` |
| Corte entre el `rename` y el `fsync` del directorio | la original o la copia verificada, completas | nada | según cuál quedó |
| Corte entre el `rename` y el borrado de `<lock>` | la copia verificada + `<lock>` sin dueño | el próximo arranque lo borra | `done` |
| Falla el `rename` | original; `<tmp>` borrado (I5: `<db>` existe) | el error | `owed` |
| Éxito | la copia verificada + `<side>` | reporte | `done:…:compacted` |

Un fallo de la compactación automática **nunca** hace fallar el `devctx index` que la disparó.
En ninguna fila se escribe en la fuente; "la original" significa el archivo que había, con lo que
la *preparación* —un open normal— le haya plegado.

## Interacciones

- **`serve`.** La compactación corre en el CLI con el serve parado. Mientras dura, un serve o una
  tool que intente abrir la base recibe "se está compactando; reintentá" (I1), y un binario viejo
  el error de lock de DuckDB (I2). Tras el reemplazo, el primer uso levanta un serve nuevo.
- **Downgrade a 0.9.0.** Mismo DuckDB y la copia lleva todas las tablas, `graph_edges` incluida:
  0.9.0 la abre y conserva sus llamadores (0.h; es la condición con que el usuario aceptó el
  downgrade). 0.9.0 no conoce `<lock>`: lo cubre I2.
- **Windows.** No se puede renombrar sobre un archivo abierto, así que allí `src` se suelta justo
  antes del `rename`. Eso **no es seguro por construcción**: lo cierra I1 (ningún 0.10 abre con
  `<lock>` tomado) y, para un binario viejo, que el `rename` falle si alguien abrió el archivo
  (a confirmar en el cross-check de TASK-019). Prohibido el fallback "borrar `<db>` y renombrar"
  (I5). Además devctx no puede parar un serve en Windows (`Termination::Unsupported`): el disparo
  automático solo corre ahí por la vía directa, y `devctx compact` pide parar el serve a mano. Si
  el cross-check no lo confirma: disparo automático apagado en Windows, documentado.
- **`repair`.** Sin cambios en 0.10.0. **Bug anotado para después de la 0.10** (no es de esta
  task): `Store::rebuild_indexes` empieza con `DROP TABLE IF EXISTS devctx_repair_<t>`
  (`store.rs:662-663`) y destruye, al reintentar, el resto que puede ser la única copia de una
  tabla tras un corte; y no es atómico. Va al master como pendiente post-009.

## Tests

Primero fallan, después pasan. En `compact.rs` salvo donde se indica. Los que piden VSS van con
`#[ignore]` si no está, como los de HNSW existentes.

*Seguridad del reemplazo*
1. **Corte a mitad** (proceso hijo + `std::process::abort`, patrón de
   `fts_drop_survives_a_crash_before_the_next_checkpoint`, `store.rs:1845`) en cada fase: tras la
   2.ª tabla, tras el HNSW, tras la verificación, entre el `rename` y el borrado de `<lock>`. El
   padre reabre: base íntegra (la original o la copia, según la fase), y el arranque siguiente
   limpia.
2. **Ctrl-C en cada fase:** temporales borrados, lock suelto, nota sin cambio.
3. **WAL no vacío en cada punto:** antes de empezar (automático → `owed`; `devctx compact` → lo
   pliega), tras el `ATTACH`, y **uno que aparece entre la verificación y el `rename`** (falla
   inyectada): aborta, y la original y su WAL quedan byte a byte.
4. **Un open concurrente se niega:** con `<lock>` tomado, `Store::open`, `AppState::build` y el
   arranque del serve devuelven el error tipado; con el lock suelto abren.
5. **Lock de DuckDB:** otro proceso con la base abierta → `compact` no crea nada; dos `compact` a
   la vez → el segundo sale por `<lock>`.
6. **La verificación protege:** una fila borrada, una duplicada de más y un valor cambiado en el
   destino (fallas inyectadas) → error, original intacta, `owed:failed-verify`.
7. **Checksum:** dos filas duplicadas en una tabla sin PK se detectan (con `bit_xor` no);
   NULL contra `''`; mismas filas con las columnas en otro orden físico dan igual.
8. **`<tmp>` huérfano con `<db>` ausente: no se borra** (I5).
9. **Disco insuficiente, directorio de solo lectura, dueño distinto, RAM:** cada previo con su
   consulta inyectable; error tipado, nada creado, **el serve no se paró**.
10. **Symlink:** `<db>` es un symlink → el reemplazo ocurre junto al archivo real y el symlink
    sigue apuntando ahí. **Modo:** la base nueva conserva el modo de la original.

*Contenido*
11. **`devctx_repair_memories` con filas aborta** nombrándola; vacía, se ignora y no se copia.
12. **Desconocidos en la fuente:** una tabla, una columna, una vista y una macro de más abortan,
    cada una con su nombre. **`BY NAME`:** una columna que falta en la fuente llega NULL y se
    declara; el test fija ese comportamiento.
13. **HNSW** (`cosine` e `ip`): misma métrica, mismos ids en `search`, `bloat_ratio()` baja.
14. **FTS:** `has_fts()` y `keyword_search` iguales.
15. **Memorias:** `memories`, `memory_symbol_references`, `sessions` y los chunks de memoria de
    `vectors` idénticos; **`recall` semántico** de una memoria conocida la encuentra tras compactar.
16. **Memorias con cero vectores de código** (una base solo con memorias): compacta y conserva todo.
17. **Sidecar:** existe tras compactar, con las tres tablas completas; la siguiente corrida exitosa
    lo borra; una que falla, no.
18. **`compact` sin la clave `compaction`, y con `PENDING_HNSW`/`PENDING_FTS`:** termina, construye
    los índices debidos y las notas `PENDING_*` no quedan en la base nueva.
19. **`index_meta`:** sellos y fingerprints idénticos tras compactar; solo cambia la fila
    `('','','compaction')`.

*Disparo* (`crates/devctx-index` y `crates/devctx-cli/tests/`)
20. Un `--full` sobre una rama pre-grafo que ya existía deja `owed:upgrade`; un incremental, un
    `--full` sobre un índice 0.10 y **una rama nueva** no tocan la nota.
21. **Una rama nueva no quema el `done`:** base 0.9.0 con rama `main`; `--full` de una rama nueva
    (bajo el umbral) → nota intacta; después el `--full` de `main` → `owed` → compacta.
22. **`skipped-below-threshold` no bloquea:** tras un `skipped`, otra migración vuelve a `owed`.
23. **Corrida cancelada:** deja `owed`; la que la termina dispara.
24. **Dos ramas** pre-grafo: cada migración rearma; con el umbral cumplido se compacta dos veces,
    y sin él no.
25. **Una sola vez por migración** (vía directa, `DEVCTX_NO_AUTOSERVE`): fixture de forma 0.9.0 →
    `index --full` → compactada y `done`; un segundo `index --full` no compacta.
26. **Serve con SIGKILL deja `owed`** (`remote.rs`, con el servidor de prueba que ignora SIGTERM
    que ya usan los tests de `stop`); con salida limpia, compacta.
27. **Sin TTY** y con `DEVCTX_NO_AUTO_COMPACT=1`: queda `owed` y se avisa.
28. **`devctx compact --dry-run`** no crea ni cambia archivos; **`status`** muestra `owed`,
    `owed:failed-verify` y `done` como estados distintos.

## Medición

Con el binario de esta task, sandbox y método de TASK-016, sobre los índices 0.9.0 reales de
**backend-a** y **frontend** (DevCtxEngine de control):

| Qué | Cómo |
|---|---|
| Tamaño: `B0` → tras `--full` en sitio → tras compactar | usados/libres y bytes (`pragma_database_size()` sobre una copia) |
| Contra la meta | % sobre `B0`, en usados y en bytes: **≤ +30 %** |
| Tiempo | segundos de la compactación, aparte del `--full`; desglose copia / HNSW / FTS / verificación / sidecar |
| Pico de RAM | `VmHWM` del proceso que compacta |
| Disco temporal | máximo de `<tmp>` + `<tmp>.wal` + temporales de DuckDB + `<side>` |
| Recorrido completo | `devctx index --full` por el serve con el disparo: tiempo total que ve el usuario |
| Downgrade | `v0.9.0` sobre la base compactada: mismas respuestas |

## Docs

- `docs/13-keeping-the-index-fresh.md` + ES: en *Upgrading to 0.10*, la compactación (cuándo corre
  sola, que anuncia y para el serve, cuánto disco y RAM pide, cuánto tarda, cómo apagarla, cuándo
  queda `owed` y qué hacer, el sidecar y cómo restaurar desde él, `devctx compact`); corregir
  `:357`.
- `docs/07-performance.md` + ES: tamaños y tiempos medidos.
- `docs/08-design-decisions.md` + ES: el ADR (`:319`); por qué copia + `rename` y no
  `VACUUM`/`CHECKPOINT`; el lock.
- `docs/11-configuration.md` + ES: `DEVCTX_NO_AUTO_COMPACT`.
- `AGENTS.md` §6 y "Things that will bite you" (~1× de disco; para el serve; "se está compactando").
- `CHANGELOG.md` 0.10.0: *Added*, *Upgrading from 0.9.x*, *Known issues* (Windows; el bug de
  `repair`).
- Ayuda del CLI y `README.md`: `compact`.

## Criterios de aceptación

- [ ] Paso 0 escrito: compuerta 0.a pasada (o decisión del usuario), reparto de los 263 bloques,
      0.f confirmado, método y umbral con números.
- [ ] backend-a y frontend tras upgrade + compactación: usados y bytes **≤ 1,30 × `B0`**; tiempo,
      `VmHWM` y disco temporal en el Resultado.
- [ ] Las cinco invariantes I1-I5 tienen test verde (tests 3, 4, 5, 8 y el `rename` de 1).
- [ ] Ninguna fila de la tabla de fallos deja algo distinto de "la original" o "la copia
      verificada": tests 1-12 verdes.
- [ ] Memorias: tests 15, 16 y 17 verdes; la verificación de `memories` y
      `memory_symbol_references` usa `EXCEPT ALL` en los dos sentidos.
- [ ] Disparo: una vez por migración de una rama pre-grafo que ya existía; tests 20-27 verdes.
- [ ] `devctx compact`, `--dry-run` y `--keep-original` existen y están documentados.
- [ ] `v0.9.0` abre la base compactada y contesta como sobre la no compactada.
- [ ] Docs EN/ES y CHANGELOG al día; ningún doc dice ya que el espacio no se recupera.
- [ ] Gate verde, `devctx plan-status PLAN-009` sin warnings, barrido de privacidad vacío.

## Riesgos

- **La reescritura no baja los usados:** compuerta 0.a y reparto 0.a' antes de escribir producto.
- **El lock de lectura de DuckDB no frena a un escritor, o cerrar `<tmp>` suelta el de `src`:**
  0.f es obligatorio; si falla, se rediseña antes de implementar.
- **`Store::open` gana una comprobación en cada apertura** (I1): tiene que ser un `try_lock` sin
  espera y no fallar si el directorio es de solo lectura (sin `<lock>` no hay compactación).
- **HNSW experimental:** se reconstruye y se verifica en frío; sin VSS no se compacta.
- **Costo doble de HNSW/FTS** (D6): aceptado y medido.
- **Parar el serve** corta a otros clientes: se anuncia, exige cierre limpio y es apagable.
- **Windows:** sin máquina local; lo cubre el cross-check de TASK-019 o el disparo apagado ahí.
- **Zona de conflicto con PLAN-010** (`store.rs`, `pipeline.rs`, `state.rs`): módulo nuevo.

## Fuentes

- DuckDB, *Reclaiming space*: <https://duckdb.org/docs/current/operations_manual/footprint_of_duckdb/reclaiming_space>
- DuckDB, `COPY` (*COPY FROM DATABASE … TO*): <https://duckdb.org/docs/current/sql/statements/copy>
- DuckDB, extensión VSS: <https://duckdb.org/docs/current/core_extensions/vss>
- DuckDB, extensión FTS: <https://duckdb.org/docs/current/core_extensions/full_text_search>
- Código de DuckDB v1.5.5 en `libduckdb-sys-1.10505.0` (`duckdb.tar.gz`):
  `src/storage/single_file_block_manager.cpp` (`Truncate`),
  `src/planner/binder/statement/bind_copy_database.cpp`.

Las páginas son las de `current`, no fijadas a v1.5.5: lo que dependa de la versión se confirma en
el Paso 0 con el motor del repo.

## Resultado
<!-- SE LLENA AL CERRAR (estado done/skipped). Vacío mientras esté pending. Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
