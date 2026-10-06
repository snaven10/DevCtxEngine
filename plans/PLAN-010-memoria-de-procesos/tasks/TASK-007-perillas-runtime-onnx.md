# TASK-007 — Perillas de runtime ONNX en config, con defaults medidos

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-001, TASK-004
- **Estado:** `pending`

---

## Objetivo

Que hilos intra/inter-op, arena de CPU, memory pattern, batch y `max_length` del motor ONNX sean
configurables (config + env) y que sus defaults salgan de medir el pico (`VmHWM`) y el throughput de
una indexación completa de DevCtxEngine. Meta: pico ≤ 1.5 GB (PLAN-010 §6). PLAN-010 DD-4, DD-6.

## Contexto verificado

- Hoy los hilos los fija fastembed: `with_intra_threads(available_parallelism())`
  (`fastembed-4.9.1/src/text_embedding/impl.rs:104-109`) → 20 en esta máquina. Tras 0.8.5 los lotes
  van en serie y el batch default es 8 (env `DEVCTX_EMBED_BATCH_SIZE`, `env_usize` en
  `crates/devctx-embed/src/local.rs:253-259`); `DEVCTX_EMBED_MAX_CHARS` 4096 (`local.rs:23`).
- ort rc.9: `with_intra_threads`/`with_inter_threads`/`with_parallel_execution`/`with_memory_pattern`
  (`ort-2.0.0-rc.9/src/session/builder/impl_options.rs:49,61,71,109`); arena de CPU sin wrapper,
  accesible por `ort::api()` (`ort-2.0.0-rc.9/src/lib.rs:152`, `DisableCpuMemArena`) o
  `OrtArenaCfg` + allocator del environment (`with_env_allocators`, `impl_config_keys.rs:18`).
- `Embeddings` (`crates/devctx-core/src/config.rs:85-104`): `provider`, `model`, `model_dir`,
  `offline`, `device`. `Reranking` (`config.rs:232-275`) con `pool` y `device`.
- `max_length`: default 512 (`fastembed-4.9.1/src/text_embedding/mod.rs:8`),
  `LocalModelSpec::max_input_tokens` (`crates/devctx-embed/src/registry.rs:20`, sólo `ml-mpnet` = 128).
- `index_meta` (PLAN-008 TASK-007) para guardar el `max_length` con que se indexó.

## Archivos

- **Modificar:** `crates/devctx-core/src/config.rs` (`Embeddings` y `Reranking`: `intra_threads`,
  `inter_threads`, `batch_size`, `cpu_arena`, `memory_pattern`, `max_length`), `crates/devctx-embed/src/lib.rs`
  (`EmbedSettings` los lleva), `crates/devctx-embed/src/onnx/mod.rs` (builder), `crates/devctx-rerank/src/*`,
  `crates/devctx-mcp/src/state.rs` / `do_index_status` (aviso si `max_length` ≠ el indexado).

## Pasos

- [ ] **Paso 1 — claves y precedencia.** env > config > default. Env: `DEVCTX_EMBED_THREADS`,
      `DEVCTX_EMBED_BATCH_SIZE` (ya existe), `DEVCTX_EMBED_ARENA`, `DEVCTX_EMBED_MEM_PATTERN`,
      `DEVCTX_EMBED_MAX_LENGTH`.
- [ ] **Paso 2 — builder.** Aplicar las perillas en el builder del motor; `inter_threads=1` +
      `parallel_execution=false` por defecto.
- [ ] **Paso 3 — matriz.** DevCtxEngine `index --full` (escenario E2 de TASK-001) con
      `intra_threads` ∈ {2, 4, 8, nproc} × `batch` ∈ {4, 8, 16} × arena on/off × memory pattern
      on/off (recortar combinaciones dominadas). Medir `VmHWM`, RSS en reposo post-índice, chunks/s,
      y `search` p50/p95 (E5).
- [ ] **Paso 4 — defaults.** Elegir el punto con `VmHWM` ≤ 1.5 GB y throughput ≥ −15 % vs línea
      base; documentar en comentarios y en el Resultado.
- [ ] **Paso 5 — `max_length`.** Configurable, default 512. Guardar el efectivo en `index_meta`
      (`embed_max_length`) al indexar; `status` avisa "reindex recomendado" si la config difiere.
      Medir (sin adoptarlo, Q-2) el pico con 256 para informar la decisión.
- [ ] **Paso 6 — liberación.** Verificar que soltar la sesión devuelve la arena (HM-3); si no,
      combinar con arena off o `kSameAsRequested`.

## Criterios de aceptación

- [ ] Tests de precedencia env > config > default para cada clave.
- [ ] Matriz completa en el Resultado; con los defaults elegidos, E2 da `VmHWM` ≤ 1.5 GB.
- [ ] Arnés de equivalencia verde con los defaults nuevos (hilos y arena no deben mover vectores más
      allá de la tolerancia).
- [ ] `status` muestra el aviso de `max_length` desfasado (test con `index_meta` sembrado).

## Riesgos

- Menos hilos = indexación más lenta: se acepta hasta −15 %; si no se llega a la meta de pico sin
  pasar ese umbral, se informa y se decide con el usuario.
- Cambiar `intra_threads` puede alterar el orden de reducción de sumas en ORT y mover el último bit:
  cubierto por la tolerancia del arnés.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
