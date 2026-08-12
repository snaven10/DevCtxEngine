# TASK-002 — Endpoint `GET /index/progress`

**Plan**: PLAN-001
**Modelo sugerido**: haiku
**Depende de**: TASK-001
**Estado**: done

## Objetivo

Publicar el estado de progreso por HTTP, sin que consultarlo compita con el indexado que
está midiendo.

## Archivos afectados

- `crates/devctx-api/src/lib.rs` — ruta y handler.

## Pasos

1. Registrar la ruta en `router`, junto a `/status`:
   ```rust
   .route("/index/progress", get(index_progress))
   ```
2. Escribir el handler **sin** pasar por el helper `run`:
   ```rust
   async fn index_progress(State(api): State<Api>) -> Response {
       match do_index_progress(&api.state) {
           Ok(body) => json_ok(body),
           Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e),
       }
   }
   ```
   `run` despacha a `spawn_blocking`, y ese pool es justamente donde está corriendo el
   indexado: la consulta quedaría encolada detrás de lo que quiere medir. Este handler solo
   copia cuatro campos bajo un lock, así que corre en el executor async sin bloquearlo.
3. Dejar el endpoint detrás de la capa de auth existente, como el resto salvo `/health`.

## Criterios de aceptación

- [ ] `curl :PUERTO/index/progress` devuelve el JSON de `IndexProgress`.
- [ ] Responde en menos de 100 ms **mientras** hay un indexado en curso.
- [ ] Con token configurado, sin `Authorization` responde 401.

## Notas / gotchas

- El comentario que justifica no usar `run` tiene que quedar en el código: es exactamente el
  tipo de "simplificación" que alguien revierte después por consistencia con los demás
  handlers, reintroduciendo el bug.
