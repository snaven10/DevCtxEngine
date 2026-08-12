# TASK-001 — Estado de progreso observable en `AppState`

**Plan**: PLAN-001
**Modelo sugerido**: sonnet
**Depende de**: —
**Estado**: done

## Objetivo

Que una corrida de indexado dentro del servidor deje su avance en un lugar que otro request
pueda leer.

## Archivos afectados

- `crates/devctx-mcp/src/state.rs` — struct de progreso, `ProgressSink` que lo escribe, campo
  en `AppState`, cableado en `do_index_inner`, y un `do_index_progress` para el endpoint.

## Pasos

1. Definir el estado y su serialización:
   ```rust
   #[derive(Clone, Default, Serialize)]
   pub struct IndexProgress {
       pub running: bool,
       pub total: usize,
       pub done: usize,
       pub file: String,
   }
   ```
2. Implementar el sink que lo alimenta. `ProgressSink` toma `&self`, así que el estado va
   detrás de un `Mutex`:
   ```rust
   struct SharedProgress(Arc<Mutex<IndexProgress>>);

   impl ProgressSink for SharedProgress {
       fn start(&self, total: usize) { /* running = true, total, done = 0 */ }
       fn file(&self, path: &str)   { /* done += 1, file = path */ }
   }
   ```
   Recuperar de un lock envenenado con `PoisonError::into_inner`, igual que
   `AppState::checkpoint` (ver commit `f1824a1`): un contador de progreso jamás debe tumbar
   un indexado.
3. Agregar `index_progress: Arc<Mutex<IndexProgress>>` a `AppState` e inicializarlo en `build`.
4. En `do_index_inner`, pasar `progress: Some(&sink)` en vez de `None`, y al terminar (tanto
   en éxito como en error) marcar `running = false` conservando los contadores finales.
5. Exponer `pub fn do_index_progress(state: &AppState) -> Result<String, String>` que serialice
   el estado a JSON, siguiendo la forma de los demás `do_*`.

## Criterios de aceptación

- [ ] `do_index_progress` devuelve `running: false` antes de cualquier indexado.
- [ ] Durante un indexado, `done` crece y `file` cambia.
- [ ] Al terminar, `running` vuelve a `false` aunque el indexado haya fallado.
- [ ] Un lock envenenado no aborta el indexado.

## Notas / gotchas

- `ProgressSink::file` se llama **antes** de procesar cada archivo; `done` es "empezados",
  no "terminados". La barra igual sirve, pero no lo describas como completados.
- El `Mutex` se toma una vez por archivo. Es un lock de nanosegundos, no hace falta nada
  más elaborado.
- `do_index_paths` comparte `do_index_inner`, así que hereda el progreso gratis.
