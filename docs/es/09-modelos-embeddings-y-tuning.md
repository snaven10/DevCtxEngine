# Modelos de embeddings, summarizer y tuning

Guía práctica de los modelos disponibles, las estrategias de presupuesto de
tokens, qué configuración conviene según el hardware, y los comportamientos
verificados empíricamente (pruebas del 2026-05-28).

> **Contexto**: esta instalación migró de `minilm-l6` (384 dims, entrenado en
> inglés) a **`ml-mpnet`** (768 dims, multilingüe) para mejorar el recall y el
> resumen de memorias escritas en **español**. La documentación de abajo explica
> por qué, cómo, y cómo ajustarlo a otros equipos.

---

## 1. Modelos de embeddings disponibles

El registro vive en `ml/devai_ml/embeddings/local.py` (`MODEL_REGISTRY`). Todos
corren localmente vía `sentence-transformers`. Se selecciona con la clave en
`embeddings.model` del `config.yaml`, o con `devai model use <clave>`.

| Clave | Modelo | Dims | Tamaño | Velocidad | Idioma | Fuerte en |
|-------|--------|------|--------|-----------|--------|-----------|
| `minilm-l6` | all-MiniLM-L6-v2 | 384 | 22 MB | muy rápida | 🇬🇧 inglés | máquinas con pocos recursos, código/texto en inglés |
| `minilm-l12` | all-MiniLM-L12-v2 | 384 | 33 MB | rápida | 🇬🇧 inglés | algo más de precisión que L6, sigue liviano |
| `bge-small` | BAAI/bge-small-en-v1.5 | 384 | 33 MB | rápida | 🇬🇧 inglés | mejor recuperación que MiniLM en inglés |
| `bge-base` | BAAI/bge-base-en-v1.5 | 768 | 110 MB | media | 🇬🇧 inglés | máxima precisión en inglés, repos grandes |
| **`ml-minilm`** | paraphrase-multilingual-MiniLM-L12-v2 | 384 | 470 MB | rápida | 🌍 50+ idiomas | **español rápido**, equipos chicos con contenido multilingüe |
| **`ml-mpnet`** | paraphrase-multilingual-mpnet-base-v2 | 768 | 1.1 GB | media | 🌍 50+ idiomas | **máxima calidad en español**, equipos con CPU decente o GPU |

### Cuál elegir

- **Contenido en español/mixto** → `ml-minilm` (rápido) o `ml-mpnet` (mejor calidad).
  Ambos NO requieren prefijos `query:`/`passage:` — son drop-in con el `encode()` actual.
- **Solo inglés** → `bge-base` (mejor) o `minilm-l6` (más liviano).
- **Evitar los `e5`**: rinden por debajo de su potencial acá porque el provider
  no agrega los prefijos `query:`/`passage:` que esos modelos necesitan.

> ⚠️ **Cambiar de modelo cambia la dimensión del vector** (384 ↔ 768). El vector
> store es incompatible entre dimensiones → **obliga a re-indexar todo**. Ver §6.

---

## 2. El pipeline de respuesta: rerank → presupuesto de tokens

Cuando llamás `recall` o `search`, el flujo es:

```
vector search (top_k_fetch)  →  reranker  →  token budget (fit)  →  respuesta
```

1. **Reranker** (`DEVAI_RERANK_*`): por defecto `flashrank` (ms-marco-MiniLM-L-12-v2).
   Reordena por relevancia y recorta a `limit`. **Nota: flashrank es un modelo en
   INGLÉS** — reordena bien pero da scores más bajos en consultas cross-lingual
   (ej. query en inglés contra memoria en español: rankea #1 correcto pero con
   score ~0.37 en vez de ~0.99). Mejora futura opcional: reranker multilingüe.

2. **Token budget** (`DEVAI_TOKEN_*` + `DEVAI_SUMMARIZER_*`): ajusta el contenido
   para no exceder `DEVAI_MAX_OUTPUT_TOKENS`. Aquí se decide drop/resumen/truncado.

### La fórmula del presupuesto por item

```
presupuesto_por_item = max(DEVAI_MAX_OUTPUT_TOKENS / limit, 128)
```

Cada memoria que **cabe** en su tajada se devuelve **verbatim**; la que se pasa,
se procesa según la estrategia. Con `MAX_OUTPUT_TOKENS=8000`:

| `limit` | tajada/item | efecto |
|---------|-------------|--------|
| 4 | 2000 tok | casi todo verbatim |
| 8 | 1000 tok | medianas verbatim, grandes resumidas |
| 12 | 666 tok | muchas resumidas |
| 18 | 444 tok | casi todas resumidas |

**Regla práctica**: una memoria sale verbatim ⟺ `tamaño_memoria ≤ 8000 / limit`.
Una memoria de 600 tok es verbatim hasta `limit ≤ 13`; una de 2000 tok, hasta `limit ≤ 4`.

---

## 3. Estrategias de presupuesto (`DEVAI_TOKEN_STRATEGY`)

| Estrategia | Qué hace | Costo CPU | Pierde items | Recomendación |
|-----------|----------|-----------|--------------|---------------|
| `drop` | descarta items enteros desde el peor rankeado hasta caber | **cero** | **SÍ** ❌ | evitar para memorias — oculta resultados relevantes |
| `soft_truncate` | corta cada item grande en borde de oración (conserva el principio) | **cero** | no | bueno para equipos chicos / hojear |
| `hard_truncate` | corta en conteo exacto de chars | cero | no | rara vez |
| `summarize` | resume cada item grande con el summarizer | depende del summarizer | no | **recomendado** con `extractive` |

> **El bug original**: con `drop` + `MAX_OUTPUT_TOKENS=4000`, 1-2 memorias grandes
> llenaban el presupuesto y las demás se **botaban silenciosamente** →
> `items_dropped: 9` → uno concluía "esa memoria no existe" cuando sí existía.
> Cualquier estrategia ≠ `drop` mantiene `output_count == input_count`.

---

## 4. Summarizers (`DEVAI_SUMMARIZER_PROVIDER`)

| Provider | Tipo | Local | Veredicto |
|----------|------|-------|-----------|
| `noop` | ninguno | ✅ | con `strategy=summarize` cae a truncado — no sirve |
| **`extractive`** | extractivo (elige oraciones por similitud al query) | ✅ | **recomendado**: reusa el modelo de embeddings, no corrompe identificadores, encuentra contenido enterrado |
| `flan-t5` | abstractivo (genera texto) | ✅ | **NO usar para código/español**: corrompe identificadores (ej. un símbolo `getStatusById` sale `getStatuById`) y palabras en español (`Diseño`→`Diseo`), límite de 512 tokens de entrada, lento. Parcheado para transformers 5.x pero igual no recomendado |
| `openai` | abstractivo cloud | ❌ | bloqueado por `require_local=true` (fuga de datos) |

**`extractive` es la elección correcta** para una herramienta de memoria de código:
- Preserva identificadores **verbatim** (elige oraciones completas, no parte palabras).
- Es **query-focused**: trae las oraciones relevantes a lo que buscaste, aunque
  estén al final de una memoria larga.
- Reusa el modelo de embeddings ya cargado → no descarga nada extra.

---

## 5. Configuración recomendada por hardware

El factor de CPU más pesado es **el modelo de embeddings** (ml-mpnet 768d es ~5x
más lento que minilm-l6 en CPU). La estrategia de resumen es secundaria
(`extractive` agrega ~0.5-1 s por recall al embeber oraciones; `soft_truncate` es gratis).

### 🖥️ PC pequeña / sin GPU (o GPU débil), contenido en ESPAÑOL
```jsonc
DEVAI_EMBEDDING_MODEL    = "ml-minilm"        // 384d multilingüe, rápido
DEVAI_EMBEDDING_DEVICE   = "cpu"
DEVAI_TOKEN_STRATEGY     = "soft_truncate"    // cero CPU extra, no pierde items
DEVAI_MAX_OUTPUT_TOKENS  = "6000"
DEVAI_RERANK_PROVIDER    = "flashrank"
```
> **¿Desactivar drop y summarize en PC chica?** Sí a `drop` (pierde memorias,
> nunca conviene). En cuanto a `summarize`: en un equipo chico conviene
> `soft_truncate` en su lugar — mantiene TODAS las memorias y **no gasta CPU**
> extra (no embebe oraciones). Usá `summarize`+`extractive` solo si tolerás
> ~1 s más por recall a cambio de resúmenes query-focused.

### 🖥️ PC potente / con GPU, contenido en ESPAÑOL  (← esta instalación)
```jsonc
DEVAI_EMBEDDING_MODEL    = "ml-mpnet"         // 768d multilingüe, máxima calidad
DEVAI_EMBEDDING_DEVICE   = "cpu"              // o "cuda" si hay GPU buena
DEVAI_TOKEN_STRATEGY     = "summarize"
DEVAI_SUMMARIZER_PROVIDER= "extractive"
DEVAI_MAX_OUTPUT_TOKENS  = "8000"
```

### 🖥️ Contenido solo en INGLÉS
```jsonc
DEVAI_EMBEDDING_MODEL    = "bge-base"   // o "minilm-l6" si el equipo es chico
DEVAI_TOKEN_STRATEGY     = "summarize"
DEVAI_SUMMARIZER_PROVIDER= "extractive"
```

### Costo medido (CPU, sin GPU — laptop con GPU Maxwell vieja, solo CPU)
- `ml-mpnet`: ~225 ms por embed de memoria; ~27 chunks/seg en batch.
- Re-index de un repo grande (~1500 archivos, ~7000 chunks, 58k edges): ~2 h.
- Recall normal: ~1-2 s. (`minilm-l6` era ~5x más rápido.)

---

## 6. Comportamientos verificados (pruebas 2026-05-28)

Batería de pruebas empíricas sobre memorias reales con `ml-mpnet` + `extractive`:

| Prueba | Qué se midió | Resultado |
|--------|--------------|-----------|
| Contenido al FINAL | query apuntando al último párrafo | `summarize`/extractive **lo encuentra** ✅; `soft_truncate` lo pierde ❌ |
| Umbral verbatim | barrido de presupuesto | verbatim si `presupuesto ≥ tamaño memoria`; resume si menos |
| Presupuesto mínimo (60 tok) | compresión extrema | coherente, **identificadores intactos, cero corrupción** |
| 3 estrategias | drop/summarize/soft_truncate | drop = todo-o-nada; summarize = comprime lo relevante; soft = lineal |
| Multilingüe EN→ES | query en inglés, memoria en español | match #1 correcto (score ~0.37 por reranker inglés) |
| Código (`search`) | — | fuerza `drop` automáticamente — **el código nunca se resume** (evita corromper identificadores) |

**Conclusiones**:
- `extractive` trae contenido relevante aunque esté enterrado en una memoria larga
  → es la estrategia correcta para recall por consulta puntual.
- El cruce multilingüe funciona (query inglés ↔ contenido español) gracias a mpnet.
- `summarize`/`soft_truncate` nunca pierden memorias (`output_count == input_count`).

### Cheat sheet de uso

| Querés... | Configurá / usá |
|-----------|-----------------|
| Detalle exacto de algo puntual | `limit 3-5` → verbatim completo |
| Explorar un tema amplio | `limit 12-18` → muchos resultados al grano, 0 perdidos |
| Buscar en otro idioma | nada — `ml-mpnet`/`ml-minilm` ya lo bridgean |
| Que siempre traiga lo relevante aunque esté enterrado | `summarize` + `extractive` (ya activo) |

---

## 7. Gotchas al migrar de modelo (aprendidos en producción)

1. **`config.yaml` vence al env var.** El CLI Go (`devai index`) y el MCP leen
   `embeddings.model` del `config.yaml` y lo pasan a Python **sobrescribiendo**
   `DEVAI_EMBEDDING_MODEL`. **Cada repo tiene su propio `.devai/config.yaml`** +
   uno en la raíz del workspace + uno en `state/`. Cambiar solo el env NO basta:
   usar `devai model use <clave>` en CADA repo, o editar todos los `config.yaml`.
   (El default del template está en `cmd/devai/cmd/init.go` → ya apunta a `ml-mpnet`.)

2. **Wipear `vectors/` no basta — limpiar `file_state`.** El re-index chequea el
   hash por archivo en la tabla `file_state` (en `index.db`) y **salta** los que
   coinciden, aunque los vectores ya no existan. `--incremental=false` NO bypasea
   el chequeo. Hay que `DELETE FROM file_state` (y `index_state`) para forzar el
   re-embed. **`index.db` contiene las memorias y el grafo → NO borrarlo**, solo
   esas dos tablas. Las memorias se re-embeben con el script
   `reembed_memories.py` (no hay comando nativo).

3. **El idle watchdog (1800 s) mata el re-index largo.** `index_repo` es UNA sola
   llamada RPC; el watchdog mide "idle" como tiempo sin requests nuevos, no
   actividad de CPU. Un repo grande con `ml-mpnet` tarda > 30 min → el watchdog
   mata el ML service (`reading response: EOF`). Para re-indexar:
   `DEVAI_ML_IDLE_TIMEOUT_SEC=0`.

### Procedimiento completo de cambio de modelo
```bash
# 1. cambiar el modelo en TODOS los config.yaml
for r in repoA repoB ...; do (cd "$r" && devai model use ml-mpnet); done
# 2. apagar el MCP/ML service (liberar el LanceDB)
# 3. wipe del vector store (conserva index.db con memorias+grafo)
rm -rf "$DEVAI_STATE_DIR/vectors"
# 4. limpiar file_state + index_state en index.db (NO memories)
#    sqlite3 index.db "DELETE FROM file_state; DELETE FROM index_state;"
# 5. re-indexar cada repo con el watchdog apagado
for r in repoA repoB ...; do
  (cd "$r" && DEVAI_ML_IDLE_TIMEOUT_SEC=0 devai index --incremental=false)
done
# 6. re-embeber memorias con el modelo nuevo
DEVAI_EMBEDDING_MODEL=ml-mpnet python reembed_memories.py
# 7. reconectar el MCP
```

---

## 8. Dónde vive cada configuración

| Archivo | Lo lee | Para qué |
|---------|--------|----------|
| `<repo>/.devai/config.yaml` | CLI `devai index` (desde ese repo) | modelo + excludes al indexar ese repo |
| `<workspace>/.devai/config.yaml` | MCP (cwd = raíz) | modelo del servicio MCP |
| `<workspace>/.devai/state/config.yaml` | resolución de state compartido | state_dir compartido |
| `.mcp.json` (env del cliente) | MCP en runtime | strategy, summarizer, max_tokens, rerank, idle timeout |

**Todos deben tener el MISMO modelo** o reaparece el gotcha #1.
