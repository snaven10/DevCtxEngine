# TASK-004 — Motor ONNX propio para embeddings (`ort` + `tokenizers`)

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-003
- **Estado:** `pending`

---

## Objetivo

`LocalProvider` deja de usar fastembed y corre sobre un motor propio (`ort` + `tokenizers`) que
reproduce los vectores actuales dentro de la tolerancia de DD-3, con builtins y user-defined, CUDA
con `CudaFallback` y el stall guard de descargas. La API pública de `devctx-embed` no cambia.
PLAN-010 DD-2, DD-3.

## Contexto verificado

- **Pipeline a replicar** (fastembed 4.9.1, rutas relativas a
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/fastembed-4.9.1/src/`):
  - Tokenizer: `common.rs:52-133` — `PaddingStrategy::BatchLongest`, `pad_id` de
    `config.json["pad_token_id"]` (default 0), `pad_token` de `tokenizer_config.json`,
    `TruncationParams { max_length: min(max_length, model_max_length) }` (`model_max_length` leído
    como f64, `:87-91`), special tokens de `special_tokens_map.json` como `AddedToken` (`:112-131`).
    Builtins vía `load_tokenizer_hf_hub` (`common.rs:37-46`).
  - Inferencia: `text_embedding/impl.rs:295-358` — `encode_batch(inputs, true)`, `input_ids` y
    `attention_mask` `i64` `(batch, len)`, `token_type_ids` sólo si la sesión lo declara
    (`impl.rs:128-131`).
  - Salida: `text_embedding/output.rs:13-19` (precedencia `OnlyOne` → `last_hidden_state` →
    `sentence_embedding`) y `normalize` con `eps = 1e-12` (`common.rs:135-140`).
  - Pooling: `pooling.rs:18-27` (CLS) y `:34-76` (Mean con máscara, piso 1.0).
  - Sesión: `Level3`, `with_intra_threads(available_parallelism)` (`impl.rs:77-81,104-110`).
  - `DEFAULT_MAX_LENGTH = 512` (`text_embedding/mod.rs:8`).
- devctx: `LocalProvider::load` (`crates/devctx-embed/src/local.rs:46-68`), `init_on` con CUDA
  (`:103-148`), `load_builtin` (`:162-185`, caché `model_cache_dir()`, `guard_load`),
  `load_user_defined` (`:199-233`, candidatos ONNX `:27-32`), `read_tokenizer_files` (`:235-251`).
  `CudaFallback` en `crates/devctx-core/src/fallback.rs`; stall guard en
  `crates/devctx-core/src/modelload.rs` (`guard_load`, `is_stall_error`).
- Features: `local = ["dep:fastembed"]`, `gpu = ["local", "dep:ort", "ort/cuda"]`, `ort =
  "=2.0.0-rc.9"` (`crates/devctx-embed/Cargo.toml:10-28`).
- Catálogo de builtins (`model_code`/`model_file`): `fastembed-4.9.1/src/models/text_embedding.rs`
  (p. ej. MiniLM-L6 → `Qdrant/all-MiniLM-L6-v2-onnx` `model.onnx`; BGE-small → `Xenova/bge-small-en-v1.5`
  `onnx/model.onnx`).

## Archivos

- **Crear:** `crates/devctx-embed/src/onnx/mod.rs` (sesión + run), `onnx/tokenizer.rs`,
  `onnx/pooling.rs`, `onnx/catalog.rs` (builtin → `hf_repo`, archivo, pooling).
- **Modificar:** `crates/devctx-embed/src/local.rs` (usa el motor nuevo; fastembed queda como
  implementación alternativa detrás de un cfg/env de diagnóstico hasta TASK-008),
  `crates/devctx-embed/Cargo.toml` (`ort` con `ndarray` y `download-binaries`, `tokenizers`,
  `hf-hub`, `ndarray` — mismas versiones que resuelve hoy el lockfile vía fastembed),
  `crates/devctx-embed/src/registry.rs` (pooling explícito por modelo, sacar el acople a nombres de
  fastembed en `builtin`).

## Pasos

- [ ] **Paso 1 — dependencias.** Directas con las versiones que ya están en `Cargo.lock` (vía
      fastembed): `ort =2.0.0-rc.9`, `tokenizers 0.21.x`, `hf-hub`, `ndarray`. Sin subir nada (DD-3).
- [ ] **Paso 2 — tokenizer.** Port fiel de `load_tokenizer` (incluido el comportamiento ante
      `special_tokens_map.json` ausente: hoy `read_optional` devuelve vacío y fastembed falla al
      parsear JSON vacío → verificar qué pasa en la práctica y replicarlo o documentarlo).
- [ ] **Paso 3 — sesión.** Builder con `Level3`, execution providers por `init_on`, y por ahora
      los mismos hilos que hoy (los perillas son TASK-007). Carga desde archivo (el mmap es
      TASK-006).
- [ ] **Paso 4 — run + pooling + normalización.** Lotes en serie en el hilo llamador; precedencia
      de outputs; Mean/CLS; `normalize(eps)` y luego `l2_normalize` (orden de hoy).
- [ ] **Paso 5 — builtins.** Descarga con `hf-hub` a `model_cache_dir()` reusando el layout que ya
      dejó fastembed (sin re-descargar), envuelta en `modelload::guard_load`; offline respeta
      `EmbedSettings::offline`.
- [ ] **Paso 6 — CUDA.** `CudaFallback` + `init_on` intactos; `cuda_providers()` sin cambios.
- [ ] **Paso 7 — equivalencia.** Pasar el arnés de TASK-003 para `minilm-l6`, `bge-small` y
      `ml-granite`. Si no pasa: `blocked` con el peor coseno y el texto que lo produce.
- [ ] **Paso 8 — `index_meta`.** Registrar `embed_engine = ort-direct/1` como diagnóstico (no
      dispara reindex, DD-3).

## Criterios de aceptación

- [ ] `cargo test -p devctx-embed` verde; el arnés de equivalencia pasa (coseno ≥ 0.9999, top-10
      idéntico) para los tres modelos, con el peor caso anotado.
- [ ] Con la caché real de modelos existente, cargar `minilm-l6` no hace ninguna descarga (test o
      verificación manual con red cortada).
- [ ] `cargo clippy --workspace --all-targets` con y sin `--features gpu` sin warnings nuevos.
- [ ] `LocalProvider` mantiene firma pública y comportamiento de `max_chars`/`batch_size`.

## Riesgos

- Divergencia en tokens especiales o en `model_max_length` gigante (BGE: `1e30`, `common.rs:86`):
  replicar el cast f64→f32→usize tal cual.
- El layout de caché de hf-hub puede variar entre la versión que trae fastembed y la directa: fijar
  la misma versión.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
