# TASK-005 — Reranker sobre el motor propio

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-004
- **Estado:** `pending`

---

## Objetivo

`LocalReranker` corre sobre el motor de TASK-004 (cross-encoder: pares consulta/candidato → logit),
con los mismos scores que hoy dentro de la tolerancia del arnés, lotes en serie y sin
`available_parallelism` hilos por lote. Es condición para poder sacar fastembed del árbol (TASK-008).

## Contexto verificado

- `crates/devctx-rerank/src/local.rs`: `find_onnx` (`:45-66`), `read_tokenizer_files` (`:69-81`),
  `BATCH = 16` con la justificación del pico de 5.7 GB (`:138-150`), `LocalReranker::load`
  (`:167-178`), `build` (`:182-207`, builtin con caché y `guard_load`), `build_user_defined`
  (`:210-…`, `std::fs::read` del ONNX en `:213`).
- fastembed reranker: `fastembed-4.9.1/src/reranking/impl.rs` — `available_parallelism` (`:65,109`),
  `commit_from_file`/`commit_from_memory` (`:91,117-118`), `par_chunks(batch_size)` (`:138`).
- Features del crate: `local = ["dep:fastembed"]`, `gpu` con `ort/cuda`
  (`crates/devctx-rerank/Cargo.toml:12-21`).

## Archivos

- **Modificar:** `crates/devctx-rerank/src/local.rs`, `crates/devctx-rerank/Cargo.toml`
  (dependencia del motor compartido en vez de fastembed).

## Pasos

- [ ] **Paso 1 — replicar `TextRerank::rerank`.** Leer `fastembed-4.9.1/src/reranking/impl.rs`
      completo: encoding de pares (`encode_batch` con tuplas), salida usada (logits `[:, 0]`), si
      aplica sigmoide o no, orden y `return_documents`. Portar igual.
- [ ] **Paso 2 — motor compartido.** Usar el builder/tokenizer de TASK-004 (misma config de
      padding/truncación del par). Lotes de `BATCH` en serie.
- [ ] **Paso 3 — builtins y user-defined.** Mismas rutas de caché y de `model_dir`; `CudaFallback`
      intacto.
- [ ] **Paso 4 — equivalencia.** Arnés de TASK-003 (`|Δ| ≤ 1e-3`, mismo orden).

## Criterios de aceptación

- [ ] `cargo test -p devctx-rerank` verde; arnés de scores pasa con el peor caso anotado.
- [ ] Sin `rayon`/`par_chunks` en el camino del reranker (grep).
- [ ] `search` con rerank activado devuelve el mismo orden que antes sobre las consultas del arnés.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
