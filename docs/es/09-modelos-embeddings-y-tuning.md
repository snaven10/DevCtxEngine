# Modelos y ajuste

> 🇬🇧 [Read in English](../09-models-and-tuning.md)

Elegir un modelo de embeddings, y el puñado de perillas que cambian el
comportamiento en vez de la decoración.

Para el esquema completo de configuración y todas las variables de entorno, ver
[Configuración](11-configuracion.md). Esta página trata de *qué* valores elegir
y por qué.

---

## La única decisión difícil de deshacer

**Elegí el modelo de embeddings antes de tu primer índice.**

Los vectores de dos modelos no viven en el mismo espacio, así que cambiar el
modelo significa re-indexar cada archivo *y* re-embeber cada memoria. Todo lo
demás en esta página se puede cambiar después.

```bash
devctx models
```

| Modelo | Dims | Idiomas | Notas |
|---|---|---|---|
| `minilm-l6` | 384 | Inglés | El más chico y rápido; el respaldo incorporado |
| `minilm-l12` | 384 | Inglés | Apenas mejor que L6, sigue liviano |
| `bge-small` | 384 | Inglés | Mejor recuperación en inglés que MiniLM |
| `bge-base` | 768 | Inglés | La mejor precisión en inglés; 768 de ancho, o sea store más grande |
| `ml-minilm` | 384 | 50+ | Multilingüe rápido, sin archivos que bajar |
| `ml-mpnet` | 768 | 50+ | 768 de ancho; **tope de entrada de 128 tokens** |
| `ml-granite` | 384 | multilingüe | **Recomendado para código no inglés.** El mejor multilingüe en CPU: máxima recuperación, indexado más rápido |
| `ml-granite-lg` | 768 | multilingüe | Hermano de 768; `ml-granite` lo iguala en CPU |

### Cómo elegir

**¿Tu código o tus comentarios no están en inglés?** Elegí un modelo
multilingüe. Los modelos en inglés van a embeber español con toda felicidad —
solo que mal. Esta es la elección equivocada más común, y falla en silencio: la
búsqueda sigue devolviendo resultados, solo que peores de lo que deberían, y
nada te avisa.

**Si no**, 384 dimensiones es el default correcto. Indexa más rápido y ocupa
menos, y `ml-granite` midió al menos tan bien como su hermano de 768 en CPU. Ir
a 768 cuando midieras que lo necesitás, no antes.

Cuidado con el **tope de 128 tokens** de `ml-mpnet` — los fragmentos más largos
se truncan antes de embeberse, lo que descarta en silencio la cola de cada
función de más de unos 500 caracteres.

### Conseguir los archivos

La columna `FILES` de `devctx models` dice qué necesita cada uno:

- `automatic` — se descarga solo en el primer uso.
- `download` — necesita `devctx models --download <modelo>` una vez.
- `ready` — ya está en esta máquina.

Las descargas caen en un caché compartido, así que un segundo proyecto que use
el mismo modelo no baja nada.

## Proveedores

```yaml
embeddings:
  provider: local        # local (default) | openai | voyage | custom
  model: ml-granite        # clave del registro; de fábrica es minilm-l6
  model_dir: ""          # un directorio con tu propio modelo ONNX
  offline: auto          # auto (default) | true | false
```

**`local`** corre fastembed en proceso. Sin red una vez que los archivos del
modelo están presentes, y ningún dato sale de la máquina.

**`openai` / `voyage`** llaman una API. Tu código va a un tercero — una decisión
real, no de rendimiento.

**`custom`** carga un modelo ONNX que vos proveés. Apuntá `model_dir` a un
directorio con el archivo ONNX más `tokenizer.json` y `config.json`. Nombres
aceptados, en orden: `onnx/model_quint8_avx2.onnx`, `onnx/model.onnx`,
`model_quint8_avx2.onnx`, `model.onnx`.

Un proveedor custom no tiene entrada en el registro, así que su ancho hay que
declararlo con `DEVCTX_EMBED_DIMENSION` (384 por defecto si no está — y un valor
equivocado acá corrompe el store, así que ponelo).

El `model_dir` de la configuración le gana a la variable de entorno
`DEVCTX_MODEL_DIR`. Ponerlo en la configuración evita tener que exportar nada en
la shell.

## Memoria y batching

Dos variables de entorno, ambas relevantes solo cuando la máquina está justa:

| Variable | Default | Efecto |
|---|---|---|
| `DEVCTX_EMBED_MAX_CHARS` | 4096 | Caracteres por texto que se le pasa al encoder. `0` desactiva el tope. |
| `DEVCTX_EMBED_BATCH_SIZE` | 32 | Textos por lote del encoder. |

Estas interactúan. Un solo fragmento muy largo rellena todo el lote hasta su
longitud, así que un lote grande *y* un tope de caracteres alto es lo que produce
el pico de memoria — no cualquiera de los dos por separado. En una máquina
limitada, bajar `DEVCTX_EMBED_MAX_CHARS` a 2048 suele ser lo más efectivo,
porque ataca el relleno en vez del conteo.

## Ajuste de almacenamiento

```yaml
storage:
  hnsw: true            # índice de vecinos aproximados
  metric: cosine        # cosine (default) | ip
  fts: false            # índice BM25, habilita `search --keyword`
```

**`hnsw`** viene encendido por medición: 84 ms → 49 ms en un store de 17k
vectores con recall@10 sin cambios. Apagarlo compra una búsqueda más lenta y
nada más.

**`metric: ip`** (producto interno) se salta el cálculo de norma que el coseno
paga en cada comparación, así que es mediblemente más barato. Pero **los dos
solo ordenan igual cuando los embeddings están normalizados a la unidad.** Los
proveedores locales normalizan; un proveedor de API o custom que no lo haga
ordenaría en silencio por magnitud en vez de por dirección. Por eso `ip` es
opt-in — el modo de falla son resultados equivocados, no un error.

**`fts`** construye el índice BM25 que necesitan `search --keyword` y la
búsqueda híbrida. Sin él, la híbrida degrada a solo vectorial en silencio.

Ambos índices se eliminan durante un indexado masivo y se reconstruyen después —
ver [Rendimiento](07-rendimiento.md).

## Reranking

```yaml
reranking:
  enabled: false        # apagado por defecto
  model: bge-base       # bge-base | bge-v2-m3 | jina-turbo | custom
  model_dir: ""
  pool: 100
```

Apagado por defecto porque se midió: 30 ms → 8.6 s en el mejor caso, 30 s con
`bge-reranker-base`, y el único modelo medido contra todo el banco empeoró los
resultados. Cifras completas en
[Decisiones de diseño ADR-15](08-decisiones-de-diseno.md).

Si lo encendés, las dos cosas que importan:

- **`pool` es el techo.** Un reranker reordena lo que le entregan. Una respuesta
  rankeada por debajo del pool le es invisible, por bueno que sea el modelo.
- **`pool` también es todo el costo.** Multiplica la etapa más lenta. Pool
  profundo con modelo chico, o pool corto con modelo grande — no las dos.

Los cross-encoders incluidos pasan todos del gigabyte, porque fastembed no trae
ninguno liviano. `model_dir` es la salida: apuntalo a una exportación ONNX de
algo como `ms-marco-MiniLM-L-12-v2`, un orden de magnitud más chico.

## Resumen (summarization)

```yaml
summarization:
  provider: extractive   # extractive (default) | openai | noop
  require_local: true    # guarda de privacidad: bloquea proveedores no locales
  target_tokens: 200
  model: gpt-4o-mini     # solo para proveedores de API
```

**`extractive`** selecciona oraciones del original en vez de generar texto.
Corre local, no cuesta nada, y — la razón de que sea el default para código —
**preserva los identificadores literalmente.** Un resumen generado parafrasea
`AuthMiddleware::authenticate` como "el método de autenticación", que es
justamente el token que habrías buscado.

`require_local: true` es una guarda, no una preferencia: bloquea de plano los
proveedores de API, para que un cambio de configuración no empiece calladamente
a mandar código a un tercero.

## Alcance del indexado

```yaml
indexing:
  exclude:
    - "**/node_modules/**"
    - "**/*.generated.ts"
  branches:
    - main
    - develop
```

**`exclude`** usa sintaxis `.gitignore` y el mismo matcher, así que un patrón se
comporta igual acá y allá — e igual sin importar si el archivo llega vía
`index`, el hook post-commit o `watch`. Es para código que git *sí* rastrea pero
que no vale la pena buscar. Lo que ya está git-ignorado no necesita regla.

**`branches`** es declarado, no inferido, y ese es todo el punto:

- Un repositorio con worktrees tiene varias ramas vivas a la vez, y nada de la
  que está en checkout dice cuáles de las otras importan.
- Adivinar una base desde el grafo de git se equivoca en el caso común — dos
  ramas de feature del mismo padre — y se equivoca *en silencio*, respondiendo
  búsquedas con el código de otra rama.
- Es lo que hace segura la poda. Esta lista define qué pertenece al índice, así
  que todo lo demás se puede descartar. Sin ella no hay forma de distinguir una
  rama viva de una fusionada y borrada hace seis semanas, y el índice solo
  crece.

La primera entrada es el default: lo que apunta `devctx index` sin `--branch`, y
a lo que cae la búsqueda cuando la rama en checkout no está indexada.

Una lista vacía significa "lo que esté en checkout", que es correcto para un
repositorio de una rama sin worktrees.
