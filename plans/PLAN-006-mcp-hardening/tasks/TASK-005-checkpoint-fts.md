# TASK-005 — Checkpoint tras crear/borrar el índice FTS (y HNSW) para que un crash no brickee el WAL

- **Plan:** PLAN-006 — Endurecimiento del MCP
- **Especialista:** — (sonnet, en la misma sesión)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-hardening`
- **Depende de:** — (independiente; nace de la nota de infraestructura de TASK-004)
- **Estado:** `done`

---

## Objetivo

TASK-004 dejó la base local del propio repo (`.devctx/state/index.duckdb`) con el WAL corrupto:

```
Failure while replaying WAL file ".../index.duckdb.wal": Cannot drop entry
"fts_main_vectors" because there are entries that depend on it. table
"terms"/"docs"/"dict" depends on schema "fts_main_vectors". Use DROP...CASCADE
```

Root cause verificada por el orquestador: `Store::rebuild_fts` y `Store::drop_fts`
(`crates/devctx-store/src/store.rs`) ejecutan DDL de FTS (`PRAGMA create_fts_index`/
`PRAGMA drop_fts_index`) y no hacen `CHECKPOINT` después. Si el proceso muere entre ese DDL y el
próximo checkpoint (un `kill -9`, un `pkill -f 'devctx serve'` a mitad de un rebuild — exactamente lo
que pasó en TASK-004), el WAL queda con DDL de índice a medio escribir que DuckDB no puede reproducir
al reabrir: la base queda inabrible para siempre (reindexar no alcanza, porque reindexar empieza
borrando, y borrar es lo que ya falla).

Alcance: checkpointear los cuatro caminos con la misma forma (`rebuild_fts`, `drop_fts`, `drop_hnsw`,
`enable_hnsw`) y verificar/corregir la afirmación de `open_past_a_broken_wal`
(`crates/devctx-cli/src/main.rs`) de que "las memorias se checkpointean al escribirse".

## Contexto verificado

- `Store::rebuild_fts` (`store.rs:279`) — `PRAGMA create_fts_index(...)`, sin checkpoint.
- `Store::drop_fts` (`store.rs:309`) — `PRAGMA drop_fts_index(...)`, sin checkpoint.
- `Store::drop_hnsw` (`store.rs:266`) — `DROP INDEX ...` (HNSW), sin checkpoint.
- `Store::enable_hnsw` (`store.rs:212`) — `CREATE INDEX ... USING HNSW`, sin checkpoint (y llama a
  `drop_hnsw` primero).
- `Store::rebuild_indexes` (`store.rs:366`) ya checkpointea al final (línea ~391), pero eso solo
  cubre la corrida completa de `devctx repair`: si el proceso muere a mitad de camino (entre el
  `drop_fts`/`drop_hnsw` y el checkpoint final), el problema es el mismo.
- `devctx-index/src/pipeline.rs::run` (líneas ~106-286) también hace su propio `drop_fts`/`drop_hnsw`/
  `enable_hnsw`/`rebuild_fts` y checkpointea al final de la función — mismo caso: un crash a mitad de
  la corrida (entre el rebuild de índices y el checkpoint final) deja el WAL corrupto igual. Al mover
  el checkpoint adentro de cada función de `Store`, esta corrida queda cubierta sin tocar
  `pipeline.rs`.
- `devctx-cli/src/main.rs::cmd_index` (líneas ~3220-3239) llama `store.enable_hnsw`/
  `store.rebuild_fts` **después** de que `index_run` ya devolvió (para el primer index, cuando el
  índice no existía antes y por eso `pipeline::run` no lo tocó) y **no** checkpointea después. Mismo
  bug, mismo arreglo.
- `devctx-mcp/src/state.rs::do_search` (línea ~367) construye el FTS índice de forma perezosa
  (`store.rebuild_fts()`) la primera vez que alguien pide `keyword`/`hybrid` search. Tampoco
  checkpointea. Es el camino más expuesto: corre en cualquier request de búsqueda, no solo en
  `devctx index`/`devctx repair`.
- `open_past_a_broken_wal` (`devctx-cli/src/main.rs:3274`) imprime "Memories are checkpointed when
  written and are not affected." al recuperarse de un WAL roto. Verificado siguiendo `remember`:
  `devctx-mcp::state::do_remember` → `devctx_memory::remember` (`crates/devctx-memory/src/lib.rs:141`)
  → `Store::upsert_memory` + `index_memory_vectors`. Ninguna de las dos llama a `checkpoint()`.
  `do_remember_shared`/`do_remember_unbound` (rutas central/group/global) pasan por
  `Central::remember` (`devctx-central/src/lib.rs:174`), que llama al mismo `devctx_memory::remember`.
  **La afirmación es falsa**: una memoria escrita justo antes de un crash se pierde igual que un
  índice, porque comparte el mismo WAL sin checkpointear. Se corrige el comentario y se agrega el
  checkpoint en el único punto de paso compartido por ambas rutas (`devctx_memory::remember`).

## Cambios

1. `Store::rebuild_fts`, `Store::drop_fts`, `Store::drop_hnsw`, `Store::enable_hnsw`
   (`crates/devctx-store/src/store.rs`) — checkpoint (`self.checkpoint()`, reutilizando la función
   existente) inmediatamente después de que el DDL correspondiente tiene éxito. Best-effort, igual
   que `checkpoint()` en sí (ver su propio doc comment: un checkpoint puede fallar legítimamente con
   otra transacción abierta, y eso no debe tumbar la operación) — mismo patrón que ya usa
   `pipeline.rs` al llamar `req.store.checkpoint()` en línea recta, sin manejo especial de error.
2. `devctx_memory::remember` (`crates/devctx-memory/src/lib.rs`) — `store.checkpoint()` después de
   cada escritura exitosa (la rama de duplicado y la rama de creación/revisión), antes de retornar
   `Ok`.
3. `open_past_a_broken_wal` (`crates/devctx-cli/src/main.rs`) — corrige el comentario: ya no dice que
   las memorias "están afectadas" o no; ahora es cierto que se checkpointean al escribirse (por (2)),
   así que el mensaje se mantiene pero deja de ser una afirmación no verificada.

## Verificación (TDD)

Test nuevo en `crates/devctx-store/src/store.rs` (`mod tests`):
`fts_drop_survives_a_crash_before_the_next_checkpoint`, con su productor de crash
`crash_producer_drop_fts` (mismo test binary, invocado como proceso hijo con una variable de entorno
que solo ese test mira; bajo `cargo test` normal es un no-op). El hijo abre un store en un archivo
temporal, inserta una fila, construye el FTS, checkpointea (estado estable), llama `drop_fts()` y
muere con `std::process::abort()` — a diferencia de `exit`, `abort` no corre destructores ni
`atexit` de C++, lo más cercano a un `kill -9` sin fork real. Se reproduce la mitad *drop*, que es la
que dejó el WAL inreproducible. El padre reabre esa misma base y confirma que abre. Si la extensión
FTS no está disponible, el hijo borra el archivo y sale normal, y el padre lo detecta ANTES de
aseverar el status del hijo.

Test análogo en `crates/devctx-memory/src/lib.rs` (`mod tests`):
`remember_survives_a_crash_right_after_writing` / `crash_producer_remember`, mismo patrón: el hijo
llama `remember(...)` y muere por `abort`; el padre reabre y confirma con `recall` que la memoria
sobrevive.

**Corrección 2026-09-28:** los dos `store.checkpoint()` de `remember` habían quedado comentados
(`// TEMP-DISABLED`) tras la verificación en rojo, mientras esta task decía `done` y
`open_past_a_broken_wal` afirmaba que las memorias se checkpointean. Se reactivaron antes del commit.

`cargo test --workspace` y `cargo clippy --workspace --all-targets` en verde al cierre (conteos en el
reporte de la sesión).

## Resultado

- **Estado final:** `done`
- **Archivos tocados:** `crates/devctx-store/src/store.rs`, `crates/devctx-memory/src/lib.rs`,
  `crates/devctx-cli/src/main.rs`.
- **Verificado por:** test de crash simulado (antes falla, después pasa) + `cargo test --workspace` +
  `cargo clippy --workspace --all-targets`.
