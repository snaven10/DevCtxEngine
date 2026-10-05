# TASK-003 — Arnés de equivalencia: fixtures y vectores/scores dorados generados con fastembed

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** — (arranca en paralelo con TASK-001; **debe** cerrarse con fastembed aún en el árbol)
- **Estado:** `pending`

---

## Objetivo

Congelar, con el código actual (fastembed 4.9.1), los vectores y scores que producen los modelos
locales sobre un set de textos representativo, y dejar un test que compare cualquier motor contra
esos dorados con la tolerancia de PLAN-010 DD-3. Es la red que permite reemplazar fastembed sin
reindexar.

## Contexto verificado

- Proveedor actual: `LocalProvider` (`crates/devctx-embed/src/local.rs:35-97`), `embed` recorta a
  `max_chars` (`:76-79`), llama `TextEmbedding::embed(…, Some(batch_size))` y aplica `l2_normalize`
  (`:84-86`). Granite se carga como user-defined con `Pooling::Mean` (`local.rs:199-233`); builtins
  vía `InitOptions` + caché `model_cache_dir()` (`local.rs:162-185`).
- Registro de modelos (`crates/devctx-embed/src/registry.rs:24-83`): builtins Mean (MiniLM,
  paraphrase) y CLS (BGE) según `fastembed-4.9.1/src/text_embedding/impl.rs:~150-195`; ninguno con
  cuantización dinámica (`impl.rs:210-224`).
- Reranker: `LocalReranker` (`crates/devctx-rerank/src/local.rs:152-178`), `BATCH = 16` (`:150`),
  user-defined por `model_dir` o builtin.
- Test ignorado que ya descarga MiniLM: `minilm_embeds_and_normalizes` (`local.rs:296-313`) — la
  guardia de caché de modelos de tests (PLAN-008 TASK-017 ítem 8) aplica: los dorados se generan con
  un comando explícito, no en `cargo test` normal.

## Archivos

- **Crear:** `crates/devctx-embed/tests/fixtures/equivalence/texts.json` (textos + metadatos),
  `crates/devctx-embed/tests/fixtures/equivalence/<modelo>.f32` + `manifest.json` (modelo, versión
  de fastembed/ort, batch, `max_length`, sha256 del `.onnx`), `crates/devctx-embed/tests/equivalence.rs`
  (comparador + generador detrás de `#[ignore]`/env `DEVCTX_GEN_GOLDENS=1`), análogos en
  `crates/devctx-rerank/tests/`.
- **Modificar:** — (no toca código de producción).

## Pasos

- [ ] **Paso 1 — set de textos (~200).** Chunks de código reales de este repo (Rust, TS, Java, SQL,
      YAML), memorias en ES y EN, consultas cortas, textos > 512 tokens (para truncación), vacío,
      un solo carácter, unicode/emoji, mezcla de idiomas. Para el reranker: ~20 consultas × 10
      candidatos.
- [ ] **Paso 2 — generador.** Usa `LocalProvider`/`LocalReranker` actuales tal cual (mismo
      `batch_size`, mismo `max_chars`). Dos pasadas: batch 8 y batch 1, para medir el ruido inherente
      al padding `BatchLongest` (DD-3) y guardarlo en el manifiesto.
- [ ] **Paso 3 — dorados.** `minilm-l6` (Mean), `bge-small` (CLS) siempre; `ml-granite` si hay
      `model_dir` (con sha256 del ONNX en el manifiesto para detectar otro archivo). Reranker:
      el builtin por defecto **sólo si** su descarga es razonable; si no, un cross-encoder chico
      user-defined documentado en el manifiesto.
- [ ] **Paso 4 — comparador.** Función `assert_equivalent(provider, goldens, tol)` reutilizable por
      TASK-004/005: coseno ≥ 0.9999 por vector, reporta el peor caso; para reranker `|Δ| ≤ 1e-3` y
      orden idéntico. Hoy corre contra el propio fastembed (debe dar ~1.0: valida el arnés).
- [ ] **Paso 5 — consultas de búsqueda.** 10 consultas sobre un DB de fixtures en memoria
      (`Store::open_in_memory`) con los vectores dorados; guardar el top-10 esperado.
- [ ] **Paso 6 — tamaño.** Los `.f32` no deben pasar de ~2 MB en total (200 × 384 × 4 B ≈ 300 KB por
      modelo de 384); si hace falta, f16 con tolerancia ajustada y documentada.

## Criterios de aceptación

- [ ] `DEVCTX_GEN_GOLDENS=1 cargo test -p devctx-embed --test equivalence -- --ignored` regenera los
      dorados de forma determinista (dos corridas → mismos bytes, o diferencia ≤ ruido medido).
- [ ] El comparador contra fastembed da coseno mínimo ≥ 0.99999 (sanity del arnés).
- [ ] Manifiesto con versión de fastembed (4.9.1), ort (2.0.0-rc.9), ORT nativo (1.20.0), batch,
      `max_length`, y el ruido batch-8 vs batch-1 medido.
- [ ] Los tests que necesitan modelos están `#[ignore]` o detrás de env y respetan la guardia de
      caché de modelos.

## Riesgos

- Descargas de builtins en CI: los dorados se generan local y se versionan; CI sólo compara si el
  modelo está disponible (si no, `skip` explícito, no verde falso).

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
