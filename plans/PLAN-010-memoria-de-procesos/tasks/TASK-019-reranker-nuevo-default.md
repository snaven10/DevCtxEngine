# TASK-019 — Reranker: adoptar el elegido como default sobre el motor propio (catálogo, descarga, pool y latencia medidos)

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-005, TASK-018
- **Estado:** `pending`

---

## Objetivo

Que el reranker elegido en TASK-018 sea el de la key por defecto, que se descargue solo (como un
builtin) sin que el usuario arme un `model_dir`, que `pool` tenga el default medido y que la
latencia real en el motor propio (TASK-005: lotes en serie, hilos acotados) quede medida. Decidir
con esos números si `reranking.enabled` sigue en `false`. PLAN-010 DD-16.

## Contexto verificado

- Tras TASK-005 el reranker corre en el motor propio con el builder/tokenizer de TASK-004; tras
  TASK-007, con `intra_threads`/`batch_size` configurables en `reranking`.
- Default de la key hoy: `bge-base` (`crates/devctx-core/src/config.rs:452`,
  `crates/devctx-rerank/src/local.rs:26`); pool 100 (`config.rs:448`); `enabled: false` (`:443`).
  `SearchReq.rerank` es `true` por defecto en la tool, pero no hace nada si `enabled: false`.
- `jina-v2` (`JINARerankerV2BaseMultiligual`) es **CC-BY-NC-4.0** y hoy se ofrece como key
  (`local.rs:266`) y en `docs/09-models-and-tuning.md`.
- Descarga de modelos: `devctx models` / `devctx models --download <key>` para embeddings
  (AGENTS.md §2) con la caché compartida `model_cache_dir()` y `guard_load`.

## Archivos

- **Modificar:** `crates/devctx-rerank/src/{local.rs,lib.rs}` (o el catálogo del motor propio:
  key nueva → repo HF, archivo ONNX, revisión fija, sha256), `crates/devctx-core/src/config.rs`
  (default de `model` y `pool`), `crates/devctx-cli/src/models.rs` (`devctx models` lista
  rerankers y descarga), `docs/09-models-and-tuning.md`, `docs/07-performance.md`,
  `docs/08-design-decisions.md` (ADR-15 actualizado) en EN/ES vía TASK-010.

## Pasos

- [ ] **Paso 1 — catálogo.** Key corta para el ganador (p. ej. `mmarco-ml` o `gte-ml`) con repo HF,
      archivo (`onnx/model_quint8_avx2.onnx` o `onnx/model_int8.onnx`, según lo que midió TASK-018),
      **revisión de HF fija** y sha256; descarga por `hf-hub` a `model_cache_dir()` dentro de
      `guard_load`, igual que los builtins de embeddings del motor propio. Respeta `offline`.
- [ ] **Paso 2 — `devctx models`.** Lista también los rerankers con licencia, tamaño, idiomas y si
      ya están en caché; `--download <key>` los baja.
- [ ] **Paso 3 — latencia real.** Con el motor propio: p50/p95 por búsqueda (serve caliente, carga
      aparte) a pool 20/30/50 con `intra_threads` 4 y 8; `VmHWM`; costo de la primera carga.
      Contrastar con lo medido en TASK-018 sobre fastembed.
- [ ] **Paso 4 — defaults.** `model` = ganador; `pool` = el de TASK-018 (esperado 20-30, no 100: el
      pool multiplica todo el costo); el ganador debe seguir cumpliendo la regla de elegibilidad de TASK-018
      sobre el motor propio (re-correr el arnés con candidatos congelados).
- [ ] **Paso 5 — `enabled`.** Propuesta a confirmar con el usuario (Q-7): pasa a `true` solo si
      p95 ≤ 2 s al pool elegido con `intra_threads` por defecto **y** se mantiene la ganancia; si no,
      queda `false` y la doc dice cuándo prenderlo.
- [ ] **Paso 6 — compatibilidad.** Las keys viejas (`bge-base`, `bge-v2-m3`, `jina-turbo`) siguen
      funcionando; `jina-v2` se mantiene solo si el usuario lo pide explícitamente, con aviso de
      licencia no comercial en `devctx models` y en la doc (Q-8). Una config que no nombra `model`
      pasa al default nuevo: anotarlo en el CHANGELOG.
- [ ] **Paso 7 — arnés de equivalencia.** Dorados del reranker nuevo generados con la referencia de
      Python (onnxruntime, mismo ONNX) y comparados con el motor propio (`|Δ| ≤ 1e-3`, mismo orden),
      igual que TASK-005 con los viejos.

## Criterios de aceptación

- [ ] Una instalación limpia con `reranking.enabled: true` y sin `model`/`model_dir` descarga el
      reranker nuevo y reranquea (test ignorado con red + verificación manual con caché vacía).
- [ ] Latencia p50/p95 y `VmHWM` medidos en el Resultado; con los defaults, p95 ≤ 2 s si
      `enabled: true` por defecto, o documentado como opt-in si no.
- [ ] Calidad sobre el set de TASK-014 con el motor propio: le gana a "sin rerank" con la regla por
      task de PLAN-010 DD-12, como en TASK-018 Paso 5.
- [ ] `devctx models` muestra licencia; ninguna key por defecto es NC o gated.
- [ ] Gate del plan verde.

## Riesgos

- **El ONNX comunitario desaparece o cambia** (si gana gte): revisión fija + sha256; si falla la
  verificación, error claro con la key alternativa.
- **Prender el rerank por defecto cambia la latencia que ve todo el mundo:** por eso es una pregunta
  al usuario (Q-7), no una decisión de la task.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
