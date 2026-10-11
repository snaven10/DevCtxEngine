# TASK-018 — Reranker: selección medida entre finalistas (calidad ES/EN y latencia en CPU)

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (scripts + config; Rust solo si hace falta para medir)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-014
- **Estado:** `pending`

---

## Objetivo

Elegir **con evidencia** el reranker que reemplaza al actual, o concluir que ninguno vale la pena.
Esta task solo mide y decide; la adopción como default (catálogo, descarga, pool) es TASK-019, sobre
el motor propio. Se puede correr apenas exista el set de TASK-014, porque los finalistas son
cross-encoders clásicos que fastembed 4.9.1 ya carga por `reranking.model_dir`. PLAN-010 DD-16.

## Contexto verificado (código)

- Reranker local: `crates/devctx-rerank/src/local.rs`. Builtins de fastembed 4.9.1 mapeados en
  `model_for` (`:261-266`): `bge-base` (default, `crates/devctx-core/src/config.rs:452`), `bge-v2-m3`,
  `jina-turbo`, `jina-v2`. User-defined por `model_dir` (`build_user_defined`, `:210-225`;
  `find_onnx` acepta nombres libres). `BATCH = 16` (`:150`). Pool default 100
  (`config.rs:448`). `enabled: false` por defecto (ADR-15).
- Contrato de fastembed para un user-defined (docs.rs `fastembed/4.9.1/src/reranking/impl.rs`):
  entradas `input_ids`/`attention_mask` i64 (+ `token_type_ids` si el grafo lo declara), pares
  `(query, doc)` por `encode_batch`, lee la salida **`logits`** columna 0. Cualquier cross-encoder
  clásico exportado con Optimum entra; un reranker LLM (logits sí/no) **no**.
- Mediciones propias publicadas en `docs/07-performance.md` y ADR-15 (repo de 2 333 chunks, pool 100):
  sin rerank ~30 ms / 406 MB; "el cross-encoder más barato" ~8.6 s / 2.4 GB; `bge-reranker-base`
  ~30 s / 3.4 GB; y el único modelo medido sobre todo el bench **empeoró** el orden (una respuesta
  de 1.º a 21.º). AGENTS.md §7: `bge-v2-m3` ~180 s por búsqueda con pool 100 (≈ 1.8 s/par).
- `scripts/bench-rerank.sh` compara modelos con el serve caliente, pero sin casos etiquetados.
- **Sospecha de latencia inflada (sin verificar):** el reranker sigue en fastembed con
  `par_chunks(batch)` sobre rayon y `intra_threads = available_parallelism` por lote
  (`fastembed-4.9.1/src/reranking/impl.rs:65,109,138`; master H1): con pool 100 y batch 16 corren
  hasta 7 lotes a la vez × 20 hilos cada uno. El hotfix 0.8.5 serializó solo los embeddings. El blog
  de Ettin (CPU i7-13700K, 512 tokens) publica `bge-reranker-base` a 19.2 pares/s, o sea ~5 s para
  100 pares, contra los ~30 s nuestros: un factor 6-9× que encaja con sobre-suscripción de hilos o
  con contar la carga del modelo. **Por eso las latencias absolutas se miden de nuevo en TASK-019
  sobre el motor propio**; aquí cuentan sobre todo las relativas.

## Investigación de candidatos (web, 2026-10-11)

Fuentes: tarjetas de HF de cada modelo; tabla de evaluación de la tarjeta de
`Qwen/Qwen3-Reranker-0.6B` (corridas del fabricante sobre top-100 de Qwen3-Embedding-0.6B); paper
mGTE (arXiv 2407.19669, tabla 5); paper LAMAR (arXiv 2607.22042, tabla 4, MIRACL por idioma); blog
`huggingface.co/blog/ettin-reranker` (pares/s en CPU y MTEB(eng,v2)); docs.rs de fastembed 4.9.1.
**V** = verificado en la fuente (las celdas de la tarjeta de Qwen3 y la carpeta ONNX de mmarco se
re-verificaron a mano); **E** = estimación propia, escalada desde los pares/s de Ettin y los
parámetros sin embeddings, anclada en lo que medimos (hereda el factor 6-9× de arriba).

| Modelo | Licencia (V) | Params (V) | Idiomas (V) | Arquitectura | ONNX (V) | fastembed 4.9.1 | Benchmarks (V, fuente) | Latencia CPU pool 20/30/50 |
|---|---|---|---|---|---|---|---|---|
| `bge-reranker-v2-m3` (hoy en la config del usuario) | Apache-2.0 | 568M | multilingüe | cross-encoder XLM-R large | comunitario (`rozgo/…`, 2.27 GB `.data`) | builtin `BGERerankerV2M3` | Qwen3: MMTEB-R 58.36, **MTEB-Code 41.38**; LAMAR: MIRACL es 67.3 | **medido** ~1.8 s/par → 36/54/90 s |
| `bge-reranker-base` (default de la key) | MIT | 278M | zh/en (tarjeta) | cross-encoder XLM-R base | oficial `onnx/model.onnx` 1.11 GB | builtin `BGERerankerBase` | C-MTEB MMarco 35.46 (tarjeta); Ettin MTEB(eng) 0.489 | medido ~0.3 s/par (pool 100 = 30 s) → E 6/9/15 s |
| **`cross-encoder/mmarco-mMiniLMv2-L12-H384-v1`** | Apache-2.0 | ~118M (≈21M sin embeddings) | mMARCO, 15 idiomas, **incluye es** | cross-encoder XLM-R 12 capas × 384 | **oficial**: `onnx/model.onnx` 471 MB, `model_quint8_avx2.onnx` 119 MB (+ avx512/arm64), `tokenizer.json` en la raíz | user-defined (`model_dir`) | **no hay números publicados** de MIRACL ni de código | E ~0.125 s/par → 2.5/3.8/6.3 s (clase MiniLM-L12: Ettin 75.9 pares/s) |
| **`Alibaba-NLP/gte-multilingual-reranker-base`** | Apache-2.0 | 306M | 70+ idiomas | cross-encoder (arquitectura propia "NewModel", `trust_remote_code`; el grafo ONNX no lo necesita) | comunitario `onnx-community/gte-multilingual-reranker-base`: fp32 1.26 GB, fp16 629 MB, int8/uint8 341 MB; nombres de salida **sin verificar** | user-defined si la salida es `logits` | Qwen3: MMTEB-R 59.44, **MTEB-Code 54.18**; mGTE: MIRACL 68.5, BEIR 55.4; LAMAR: MIRACL es 63.3 | E 10/15/25 s (clase bge-base) |
| `ms-marco-MiniLM-L6-v2` / `-L12-v2` | Apache-2.0 (datos MS MARCO: términos no comerciales, solo aviso) | 22.7M / 33.4M | **solo inglés** | cross-encoder BERT | oficial (int8 23/34 MB) | user-defined | DL19 74.3 (tarjeta); MIRACL 0.322 (eval de terceros) | Ettin 143.9 / 75.9 pares/s → E 1.3/2/3.3 s y 2.5/3.7/6.2 s |
| `Horizon-Labs/multilingual-reranker-small` (publicado 2026-10-01) | Apache-2.0 | 141M (mmBERT-small) | 40+, es explícito | cross-encoder | oficial int8 268 MB | user-defined | **auto-reportado**: MIRACL 0.724, es 0.691; sin código | E 3.5-5/5-7.5/9-12.5 s; **entrenado a 384 tokens** (recorta chunks de 512) |
| `gte-reranker-modernbert-base` | Apache-2.0 | 149M | **solo inglés** | cross-encoder ModernBERT | oficial (int8 151 MB) | user-defined | CoIR 79.99 (tarjeta); MIRACL 0.473 (terceros) | Ettin 14.7 pares/s → E 14/20/34 s |
| `granite-embedding-reranker-english-r2` | Apache-2.0 | 149M | **solo inglés**; IBM no publicó reranker multilingüe | ModernBERT | sin ONNX oficial | — | BEIR 55.8 (tarjeta) | Ettin 14.5 pares/s → E igual al anterior |
| `mxbai-rerank-xsmall-v1` / `base-v1` | Apache-2.0 | 70.8M / ~184M | **solo inglés** | DeBERTa-v3 | oficial | user-defined | BEIR 43.9 / 46.9 | E 4-6 s / 15-25 s (pool 50) |
| `mxbai-rerank-base-v2` | Apache-2.0 | 0.5B (Qwen2.5) | 100+ | **LLM** (plantilla + cabeza propia) | solo comunitario | no | BEIR 55.57, CoIR/CosQA 31.73 | E 43/64/107 s |
| `Qwen3-Reranker-0.6B` | Apache-2.0 | 0.6B | 100+ | **LLM** (plantilla chat, softmax de logits "yes"/"no" en el último token) | comunitario (int8/q4, CausalLM) y conversión `-seq-cls` | no | MMTEB-R 66.36, **MTEB-Code 73.42**; LAMAR MIRACL es 66.9 | E 52/78/130 s (peor que hoy) |
| `jina-reranker-v2-base-multilingual` | **CC-BY-NC-4.0 → descartado** | 278M | multilingüe | — | oficial | builtin `jina-v2` (¡hoy se ofrece!) | — | — |
| `jina-reranker-v1-turbo-en` | Apache-2.0 | 37.8M | **solo inglés** | JinaBERT | oficial | builtin `jina-turbo` | BEIR 49.60 | rápido |
| Otros descartados | jina-reranker-v3 (NC), zerank-1-small (1.7B), Ettin (solo inglés), KaLM-Reranker (1B, sin ONNX), LAMAR (0.6B, mismo costo que v2-m3) | | | | | | | |

**Lectura clave de la tabla de Qwen3 (V):** con un recuperador denso fuerte, `bge-reranker-v2-m3`
saca 41.38 en MTEB-Code, **muy por debajo** del propio recuperador (75.41): en código, un reranker
entrenado en pasajes puede empeorar el orden. Coincide con lo que ya vimos (ADR-15). Por eso el
criterio es **"le gana a no reranquear"**, no "le gana al reranker actual".

## Finalistas

1. **`mmarco-mMiniLMv2-L12-H384-v1`** (velocidad): entrenado con español, ONNX oficial int8 con
   `tokenizer.json`, una salida `logits`: entra hoy por `model_dir` y mañana como builtin del motor
   propio sin trabajo extra. ~14× más barato que v2-m3 (E). Riesgo: calidad en código sin publicar.
2. **`gte-multilingual-reranker-base`** (calidad): el mejor número publicado en código entre los
   cross-encoders clásicos multilingües (54.18 contra 41.38 de v2-m3) a la mitad de parámetros.
   Riesgo: ONNX solo comunitario (verificar salida `logits` y `token_type_ids`), costo clase bge-base.

Controles en la misma corrida: **sin rerank** (la vara), `bge-v2-m3` (hoy) y `bge-reranker-base`
(default de la key). Comodín barato: `Horizon-Labs/multilingual-reranker-small` (si su ONNX entra).
Fuera: rerankers LLM (Qwen3, mxbai v2) mientras la inferencia sea solo CPU: cuestan ≥ v2-m3 y exigen
plantilla, selección del último token y logits de vocabulario en el motor propio (TASK-005 no lo
cubre). Se re-evalúan si algún día hay GPU usable.

## Archivos

- **Modificar:** `scripts/graph-eval/run.sh` + `score.py` (modo rerank: candidatos congelados,
  latencia por búsqueda separada de la carga), `scripts/bench-rerank.sh` (lista de candidatos:
  sacar `jina-v2` por licencia o marcarlo NC, agregar los finalistas).
- **Crear:** `scripts/rerank-eval/fetch.sh` (descarga los finalistas a un directorio del scratchpad
  o del cache de modelos con sha256 anotado).

## Pasos

- [ ] **Paso 1 — verificar ONNX de los finalistas.** Entradas/salidas (nombre `logits`, forma
      `[N,1]`), `token_type_ids`, `model_max_length`; que cargan por `model_dir` en fastembed 4.9.1.
      Para gte: si la salida no se llama `logits`, se mide con un script de Python (onnxruntime) y se
      anota que TASK-019 debe mapear el nombre.
- [ ] **Paso 2 — candidatos congelados.** Para cada caso de TASK-014, el top-50 vectorial (el modo
      que gane tras TASK-015, si ya cerró; si no, vectorial) guardado a disco, para que todos los
      rerankers vean exactamente los mismos candidatos.
- [ ] **Paso 3 — calidad.** Cada modelo × pool {20, 30, 50} × {fp32, int8 cuando exista}: Hit@1,
      Hit@5, MRR@10 por idioma y tipo, contra "sin rerank" sobre los mismos candidatos.
- [ ] **Paso 4 — latencia.** Por búsqueda, serve caliente (modelo ya cargado; la carga se mide
      aparte), p50/p95 de 20 búsquedas, `intra_threads` fijado (4 y 8) para que la comparación sea
      justa, y `VmHWM` del serve. Incluir `bge-v2-m3` para confirmar o refutar el ~1.8 s/par y la
      sospecha de sobre-suscripción (con `RAYON_NUM_THREADS=1` como prueba barata).
- [ ] **Paso 5 — decidir.** Regla: un modelo es elegible si, al pool elegido, **le gana a sin rerank**
      con la regla por task de PLAN-010 DD-12 (mejora puntual en ES, ninguna regresión con IC; EN sin
      bajar más de 1 caso), y además la mediana del MRR global es mayor que la de sin rerank: el
      costo del reranker solo se justifica si mejora algo. Entre elegibles: el de menor p95; empate de ±0.02 de
      MRR → el más chico. Si ninguno es elegible, se recomienda dejar `enabled: false` y cambiar solo
      el modelo por defecto al más barato que no empeore (para quien lo active), y se cierra así.

## Criterios de aceptación

- [ ] Tabla completa (modelo × pool × precisión) con calidad por idioma y p50/p95/`VmHWM`, y la
      fila "sin rerank".
- [ ] Recomendación con la regla del Paso 5, el pool elegido y la latencia esperada en el motor
      propio (a re-medir en TASK-019).
- [ ] La sospecha de sobre-suscripción confirmada o refutada con números.
- [ ] Nada de modelos NC ni gated en la recomendación; licencias anotadas.

## Riesgos

- **Set chico para rerankers:** 40+40 casos con pool 50 distinguen mejoras grandes, no finas. Por
  eso el regla exige que ninguna regresión sea significativa (IC) y se reporta también MRR global.
- **int8 vs fp32:** la cuantización puede mover el orden; se mide, no se asume.
- **ONNX comunitario de gte:** puede romperse o cambiar; TASK-019 fija la revisión (commit de HF) y el
  sha256.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
