# TASK-021 — Retirar `jina-v2` (CC-BY-NC) de los rerankers builtin, sin romper el search de quien lo tenga configurado

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda; decisión del usuario 2026-10-11, Q-8)
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Release:** `0.10.x`
- **Depende de:** — (después de PLAN-009)
- **Estado:** `pending`

---

## Objetivo

Que devctx deje de ofrecer `jina-v2` (`jinaai/jina-reranker-v2-base-multilingual`, licencia
CC-BY-NC-4.0) como reranker builtin. Una config que lo nombra **no hace fallar** ninguna búsqueda:
cae a "sin rerank" (el `NoopReranker` de hoy) y avisa en `status` y en el log que hay que elegir otro
modelo. Va en la `0.10.x`, sin depender del motor propio. PLAN-010 Q-8.

## Contexto verificado (2026-10-11, rama `feat/plan-009-grafo`; re-verificar líneas al arrancar)

- La key se mapea en `model_for` (`crates/devctx-rerank/src/local.rs:261-266`):
  `"jina-v2" => RerankerModel::JINARerankerV2BaseMultiligual`; también figura en el test de keys
  (`local.rs:277`) y en el doc de `RerankSettings.model` (`crates/devctx-rerank/src/lib.rs:25`).
- `create_reranker` (`crates/devctx-rerank/src/lib.rs:60-91`) ya devuelve `NoopReranker` cuando el
  rerank está apagado o sin la feature `local`: es el punto natural para el fallback.
- Llamadores: el serve (`crates/devctx-mcp/src/state.rs`, al construir el reranker) y el CLI
  (`crates/devctx-cli/src/main.rs:4281`). Hay que confirmar en los dos que un `Err` de
  `create_reranker` hoy no aborta el search; con este cambio `jina-v2` ni siquiera llega a `Err`.
- Docs que lo nombran: `docs/09-models-and-tuning.md` (sección Reranking, lista de keys),
  `crates/devctx-core/src/config.rs` (comentario de `Reranking.model`), `scripts/bench-rerank.sh`
  (lista `CANDIDATES`).

## Archivos

- **Modificar:** `crates/devctx-rerank/src/local.rs` (sacar el mapeo; constante `RETIRED_MODELS`
  con la key y el motivo), `crates/devctx-rerank/src/lib.rs` (`create_reranker`: key retirada →
  `NoopReranker` + aviso; doc de `model`), `crates/devctx-mcp/src/state.rs` (`status` /
  `index_status`: campo `reranking.warning`), `crates/devctx-core/src/config.rs` (comentario),
  `scripts/bench-rerank.sh`, `docs/09-models-and-tuning.md` (EN; la ES va en TASK-022).

## Pasos

- [ ] **Paso 1 — key retirada.** `RETIRED_MODELS = [("jina-v2", "CC-BY-NC-4.0: non-commercial
      license")]`. `model_for("jina-v2")` ya no existe como builtin.
- [ ] **Paso 2 — fallback sin error.** En `create_reranker`, si `settings.model` está en
      `RETIRED_MODELS` (y no hay `model_dir`, que sigue ganando como hoy): devolver `NoopReranker`
      y un aviso tipado (p. ej. `RerankNotice::Retired { key, reason }`) que el llamador guarda.
      El search sigue, con el orden del recuperador.
- [ ] **Paso 3 — aviso visible.** `eprintln!` una vez por proceso en el log del serve
      ("reranking.model `jina-v2` was retired (non-commercial license); reranking is off — pick
      another model, see `devctx models`"), y `status`/`index_status` con
      `reranking: { model: "jina-v2", active: false, warning: "…" }`. El resultado de `search` no
      cambia de forma (sin campo nuevo obligatorio).
- [ ] **Paso 4 — docs y scripts.** Sacar `jina-v2` de la lista de keys de la doc (EN) y del
      comentario de `config.rs`; nota en el CHANGELOG ("retirado por licencia; quien lo tenga
      configurado queda sin rerank hasta elegir otro"); sacarlo de `bench-rerank.sh`.

## Criterios de aceptación

- [ ] Test unitario: `create_reranker` con `model: "jina-v2"` y `enabled: true` devuelve un reranker
      identidad (orden de entrada) **sin error** y con el aviso de retiro.
- [ ] Test: `model: "jina-v2"` + `model_dir` apuntando a un directorio sigue cargando el user-defined
      (el directorio gana, como hoy).
- [ ] Test de integración (o del handler): un `search` con esa config responde resultados y
      `status` muestra `reranking.warning`.
- [ ] `git grep -n jina-v2` solo devuelve `RETIRED_MODELS`, sus tests, el CHANGELOG y `plans/`.
- [ ] Gate del plan (fmt, clippy con y sin `gpu`, `cargo test --workspace`).

## Riesgos

- **Quien lo usaba pierde el rerank sin enterarse.** Mitigación: aviso en log y en `status`, y nota
  en el CHANGELOG; el search no se rompe.
- **La caché de modelos conserva el ONNX descargado** (~1 GB): no se borra solo; la doc dice cómo
  borrarlo (`model_cache_dir()`).

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
