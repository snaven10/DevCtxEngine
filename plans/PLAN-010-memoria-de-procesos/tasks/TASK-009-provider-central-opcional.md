# TASK-009 — Modo opcional `embeddings.provider: central`

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-008
- **Estado:** `pending`

---

## Objetivo

Permitir (opt-in, apagado por defecto) que los serves no carguen el modelo y le pidan los embeddings
al central, que tiene uno solo para todos. Con contrato versionado, validación de modelo/dimensión,
sin embeber bajo el mutex del registro y con error explícito si el central no está. Medir si conviene.
PLAN-010 DD-8.

## Contexto verificado

- `CustomProvider` ya hace `POST {endpoint}/embed` y parsea `{"vectors": …}`
  (`crates/devctx-embed/src/api.rs:149-189`, `parse_vectors` `:57-69`); `OpenAiProvider`/`VoyageProvider`
  usan URLs fijas (`api.rs:10-11`). `create_provider` (`crates/devctx-embed/src/lib.rs:93-125`)
  despacha por `provider`; `dimension_for` (`lib.rs:78-90`) resuelve la dimensión sin cargar.
- `Central::shares_vector_space` (`crates/devctx-central/src/lib.rs:158-160`) existe sin llamadores;
  `Central::embedder` (`:163-172`, con la liberación por inactividad de 0.8.5).
- El central embebe con el lock tomado: handlers `remember`/`recall`
  (`crates/devctx-api/src/central.rs:495-560`) pasan por `run` (`:688-696`), que hace
  `central.lock()` dentro de `spawn_blocking` y llama `f(&guard)`.
- Descubrimiento y auto-spawn del central: `crates/devctx-central/src/client.rs` (`CentralClient`,
  spawn con cosecha `:225-270`, `CALL_TIMEOUT` 300 s `:353`).
- Versionado entre binarios: precedente `mcp_version` de PLAN-008 D2.

## Archivos

- **Crear:** `crates/devctx-embed/src/remote.rs` (`RemoteProvider`) o equivalente en
  `devctx-central` si la dependencia circular lo exige (`devctx-central` depende de `devctx-embed`).
- **Modificar:** `crates/devctx-api/src/central.rs` (ruta `POST /embed`, semáforo, `remember`/`recall`
  embebiendo fuera del lock), `crates/devctx-central/src/lib.rs` (método para clonar el embedder sin
  retener el lock; `embed_checked` que valida modelo/dim), `crates/devctx-embed/src/api.rs`
  (`base_url` para OpenAI/Voyage), `crates/devctx-embed/src/lib.rs` (`provider: central` en
  `create_provider`), `crates/devctx-core/src/config.rs` (`embeddings.central_fallback`).

## Pasos

- [ ] **Paso 1 — sacar la inferencia de debajo del lock.** En el central: lock → clonar `Arc` del
      embedder → soltar → embeber → lock → escribir. Aplica a `remember`/`recall` actuales y al
      `/embed` nuevo. Test: un embedder falso que tarda 2 s no bloquea un `GET /projects` concurrente.
- [ ] **Paso 2 — `POST /embed`.** Request `{texts, model, dimension, max_length}`; respuesta
      `{vectors, model, dimension, engine, version}`; 409 si no coincide (`shares_vector_space` +
      dimensión + `max_length`); semáforo configurable (default 2) y 503 + `Retry-After` con cola
      llena; límite de textos por request.
- [ ] **Paso 3 — `RemoteProvider`.** Descubre URL/token del central vía `CentralClient`
      (auto-spawn incluido), manda lotes de `batch_size`, valida `model`/`dimension` de cada
      respuesta, header `X-Devctx-Version` y rechazo de otra minor con mensaje accionable.
      `dimension()`/`model_name()` sin red (de la config).
- [ ] **Paso 4 — central caído.** Reintento perezoso (re-`ensure`) y, si no levanta, error con
      causa. `central_fallback: local` opcional carga el modelo local con aviso.
- [ ] **Paso 5 — `base_url`** para OpenAI/Voyage (config/env), default el actual.
- [ ] **Paso 6 — medición.** Escenarios: serve con `provider: central` en reposo (meta ≤ 60 MB);
      `search` p50/p95 vs local; indexación completa de DevCtxEngine (chunks/s) vs local; RSS del
      central con 3 serves usándolo. Si la indexación cae > 30 %, documentar el modo sólo para
      consulta (DD-8).

## Criterios de aceptación

- [ ] Tests: 409 ante modelo/dimensión distintos; 503 con cola llena; rechazo de versión; central
      caído → error explícito (sin carga local salvo `central_fallback: local`).
- [ ] Test de no-bloqueo del registro durante una inferencia lenta (paso 1).
- [ ] Con `provider: local` (default) no cambia ningún comportamiento (tests existentes verdes).
- [ ] Tabla de medición en el Resultado y recomendación para Q-3.

## Riesgos

- Vectores por JSON: ~384 floats × N por request; si pesa, considerar f32 LE en base64 o
  `application/octet-stream` (decidir con la medición, no antes).
- Embeddings de indexación compitiendo con `recall` interactivo en el mismo central: el semáforo con
  prioridad (consultas cortas primero) queda como mejora si la medición lo pide.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
