# Rendimiento

> 🇬🇧 [Read in English](../07-performance.md)

Qué cuesta qué, cuáles cifras se midieron de verdad, y cómo medir las tuyas.

---

## Leé esto primero

Cada número de esta página se midió en una sola máquina de desarrollo — una caja
WSL2 solo-CPU — contra repositorios reales. **Son órdenes de magnitud, no
garantías.** Donde una cifra no se midió, esta página lo dice en vez de
estimarla.

Lo único que generaliza: **el embedding domina el indexado, y todo lo demás es
ruido al lado.** Optimizá la cantidad de fragmentos que embebés y ya optimizaste
el indexado.

## Indexado

### Qué maneja el costo

En orden descendente de impacto:

1. **Fragmentos embebidos.** Es todo el juego.
2. **Si el índice HNSW está presente durante la carga.** Ver abajo — pesa más de
   lo que la mayoría espera.
3. **Ancho del modelo.** Un modelo de 768 dimensiones es aproximadamente el
   doble de trabajo vectorial y el doble de almacenamiento que uno de 384.
4. Parseo y chunking. Reales, pero chicos.

### El efecto HNSW

DuckDB mantiene un índice HNSW en cada insert. Medido en un backend Java de
~1,300 archivos, misma máquina, misma forma de corrida:

| Índice durante la carga | Rendimiento |
|---|---|
| HNSW presente | ~7 archivos/min |
| HNSW eliminado, reconstruido después | ~58 archivos/min |

Una diferencia de 8×. Por eso el indexado tira los índices derivados y los
reconstruye al final, y por eso no deberías construir un índice HNSW y después
cargarle datos en masa.

### Incremental vs completo

El indexado incremental es la característica de rendimiento más importante,
porque el caso común es un puñado de archivos cambiados, no el repositorio.

```bash
devctx index              # incremental: solo lo que git dice que cambió
devctx index --full       # todo
devctx index --branch x   # una rama concreta
```

Un fragmento cuyo `content_hash` no cambió no se vuelve a embeber, así que una
corrida incremental sobre un commit que toca tres archivos cuesta tres archivos
de embedding, no el repositorio.

**El indexado lee el árbol de trabajo, no el último commit.** `--full` no
descarta el trabajo sin commitear.

### Multi-rama

Indexar una segunda rama es mucho más barato que indexar un repositorio, porque
el mismo hashing de contenido que abarata las corridas incrementales hace que
las ramas compartan filas. Medido en tres repositorios:

| Tamaño del repositorio | Archivos | Copiados en vez de embebidos |
|---|---|---|
| ~1,400 archivos (TypeScript) | 1,406 | 1,343 (96%) |
| ~1,300 archivos (Java) | 1,297 | 1,251 (96%) |
| ~150 archivos (Java) | 153 | 146 (95%) |

Así que el costo marginal de una segunda rama declarada ronda el 4–5% de un
índice completo, no el 100%.

## Búsqueda

Medido en este repositorio (128 archivos, 2,333 fragmentos, modelo de 384
dimensiones):

| Configuración | Latencia | Memoria residente |
|---|---|---|
| Búsqueda vectorial, sin reranking | ~30 ms | ~406 MB |
| Con el cross-encoder más barato | ~8.6 s | ~2.4 GB |
| Con `bge-reranker-base` | ~30 s | ~3.4 GB |

El reranking está apagado por defecto por esta tabla, y porque el único modelo
medido contra todo el banco empeoró los resultados — bajó una respuesta correcta
del primer puesto al vigésimo primero. Ver
[Decisiones de diseño ADR-15](08-decisiones-de-diseno.md).

La búsqueda por palabra clave (BM25) y la híbrida no se midieron por separado.
La híbrida corre los dos recuperadores, así que tratala como al menos el costo
del camino vectorial.

## El grafo de símbolos (0.10)

Medido mientras se construía 0.10, en octubre de 2026, en la misma máquina solo
CPU — muchas veces cargada por otro trabajo, así que leé las cifras como órdenes
de magnitud. Los repositorios: este (≈440 archivos en ese momento), dos
repositorios Java privados (1 250 y 216 archivos), un subconjunto de un
workspace Angular privado y, para `build_context`, ese workspace entero. Las
herramientas de grafo se midieron en caliente, cada llamada abriendo el store
como lo hace el servidor, 40 llamadas por caso.

**`impact_analysis`** (2026-10-08 a 2026-10-10), p95 con los defaults
(llamadas high/medium, sin tests ni llamados de librerías, 200 por dirección,
dispatch encendido), contra el recorrido de 0.9 sobre el mismo índice:

| Caso | 0.10 | 0.9 |
|---|---|---|
| `get`, repositorio Java de 1 250 archivos | 40,7 ms | 4 165 ms (1 855 nombres recorridos) |
| `map`, mismo repositorio | 85,9 ms | 5 724 ms (2 571 nombres) |
| peor de 8 casos ahí, con dispatch, máquina tranquila | 117,9 ms | — |
| peor de todos los casos, con carga | 242 ms | — |
| `get` / `new`, este repositorio, antes de que existiera el dispatch | 43,5 / 61,1 ms | 805 / 1 550 ms |

Dos causas, medidas por separado: una consulta por nivel en vez de una por
nombre, y que un nombre pelado signifique las definiciones del repositorio en
vez de cada llamada escrita con él. Seguir el dispatch cuesta 2–15 ms, y hasta
70 ms donde una interfaz tiene varias implementaciones.

**`traverse`** (2026-10-09): p95 a profundidad 2 entre 16,8 y 47,5 ms en 16
casos sobre el repositorio Java de 1 250 archivos (calls, references, imports,
contains, implements/inherits, instantiates, todas las relaciones); un peor caso
sintético (3 000 definiciones de un nombre, 1 000 implementando una interfaz)
93–126 ms tras un arreglo que lo bajó de 2,2–3,5 s (2026-10-10).

**`repo_map`** (2026-10-09/10), presupuesto de 1 024 tokens: p50 / p95 en
caliente 20,9 / 34,3 ms en el repositorio Java de 1 250 archivos (22 801
símbolos; primera carga 104 ms para leer y armar la adyacencia cacheada,
≈7,4 MB), 19,2 / 26,7 ms en el subconjunto Angular, 12,0 / 16,5 ms en este
repositorio. Nunca pasa el presupuesto (el mapa más grande, 996 tokens); con un
archivo vecino como foco, el archivo esperado estuvo en el mapa en 16 de 17
casos.

**`skeleton`** (2026-10-11): 4,9–10,2 % de los tokens del archivo en 12 archivos
de 623–4 684 líneas de cuatro repositorios; p95 ≤ 26 ms.

**`build_context` con `related: true`** (2026-10-12), brief de 4 096 tokens, p50
sin → con la sección: este repositorio 92 → 116 ms, el Java de 216 archivos
78 → 87 ms, el de 1 250 archivos 142 → 179 ms, el workspace Angular 132 →
186 ms (carga de la máquina 14–16). Archivo esperado en el brief: 21 → 22 de 26
casos.

**Ranks** (2026-10-09): el PageRank global suma 21–165 ms a un link pass (de
3 000 a 22 876 símbolos), y la búsqueda híbrida con la centralidad encendida
0–6 ms de p50.

### Costo de indexado de 0.10 contra 0.9.0

Todavía no medido en igualdad de condiciones: el indexado desde cero, el tamaño
de la base y el pico de memoria de 0.10.0 contra 0.9.0, el arnés de evaluación
con los 30 casos (con los pesos de centralidad y por idioma) y el paso de
índices 0.9.0 reales son la verificación de campo antes del release (PLAN-009
TASK-016). *Se completa en TASK-016.* Las cifras parciales medidas hasta ahora
— reuso de vectores, el link pass, el crecimiento de la base — están en
[Mantener el índice al día → Cuánto cuesta](13-mantener-el-indice-al-dia.md#cuánto-cuesta).

## Almacenamiento

Cifras reales del store de este repositorio:

| Cantidad | Valor |
|---|---|
| Archivos indexados | 128 |
| Fragmentos | 2,333 |
| Símbolos | 1,599 |
| Store en disco | 17 MB |

Lo que da aproximadamente **18 fragmentos por archivo** y **~7 KB por
fragmento** a 384 dimensiones. Un vector `f32` de 384 dimensiones son 1.5 KB de
eso; el resto es texto del fragmento, filas del grafo y estructuras de índice.

Duplicar el ancho del modelo duplica aproximadamente la porción vectorial. No
duplica el texto.

### Dónde vive

| Ruta | Guarda |
|---|---|
| `.devctx/` en el repositorio | El índice y la configuración de ese proyecto |
| `~/.local/share/devctx/` | Registro de proyectos, memorias globales y de grupo, archivos de modelos |

`.devctx/` debería estar en `.gitignore`. El directorio central también guarda
los archivos de modelos descargados, que suele ser la mayor parte de su tamaño —
verificá antes de suponer que tus memorias son grandes.

## Ajuste

### Excluí lo que nunca preguntarías

El ajuste de mayor apalancamiento, porque elimina fragmentos en vez de
abaratarlos.

```yaml
indexing:
  exclude:
    - "**/node_modules/**"
    - "**/target/**"
    - "**/dist/**"
    - "**/*.min.js"
    - "**/*.lock"
```

`.gitignore` se aplica primero y es la herramienta gruesa; `exclude` es para lo
que git sí rastrea pero vos nunca preguntás — código vendorizado, clientes
generados, fixtures.

**Caveat conocido:** cambiar `exclude` entre corridas no se refleja en el hash de
contenido, así que la deduplicación de copia por rama puede arrastrar filas que
las nuevas exclusiones habrían descartado. Corré `devctx index --full` después de
cambiarlo.

### Elegí el modelo una sola vez

Los modelos de 384 dimensiones indexan más rápido y ocupan menos. El default
para proyectos nuevos es `ml-granite` (384, multilingüe), que en CPU midió mejor
tanto en recuperación como en velocidad de indexado entre las opciones
multilingües.

Cambiar el modelo después de indexar significa re-indexar cada archivo *y*
re-embeber cada memoria, porque los vectores de dos modelos no son comparables.
Elegí antes del primer índice.

### Mantené el índice fresco barato

```bash
devctx hooks install     # re-indexa al commitear; no cuesta nada en reposo
devctx watch             # re-indexa al guardar; un proceso, pero inmediato
```

El hook es la automatización más barata que funciona. Ver
[Mantener el índice al día](13-mantener-el-indice-al-dia.md) para las cuatro
opciones.

## Uso de recursos

**Un solo proceso.** Parseo, chunking, embeddings y reranking son Rust en
proceso — no hay sidecar sosteniendo una segunda copia de nada, ni límite de
serialización entre etapas.

El embedding corre **un lote a la vez**, de 8 textos por defecto
(`DEVCTX_EMBED_BATCH_SIZE`): el pico de memoria escala con el lote, y el reparto
paralelo anterior llegaba a 6,5 GB con lote 8 y a 18,2 GB con lote 32. Un modelo
que nadie usó en 5 minutos se descarga (`DEVCTX_MODEL_IDLE_SECS`), tanto en los
servidores de proyecto como en el daemon central.

La memoria residente la domina el modelo cargado. La cifra de ~406 MB de arriba
es un modelo de embeddings de 384 dimensiones más el store; habilitar un
cross-encoder agrega gigabytes, que es la razón real de que venga apagado.

**Solo-CPU es el caso asumido.** Nada de acá requiere GPU, y todas las cifras de
esta página son de CPU. Un build con `--features gpu` puede correr el embedder y
el reranker locales en una GPU NVIDIA (`device: cuda`); es opcional, no está
medido acá, y se describe en
[Modelos y ajuste](09-modelos-embeddings-y-tuning.md#gpu-cuda).

**Red:** solo descargas de modelos en el primer uso, y solo para modelos cuyos
archivos no estén ya presentes. `devctx models` muestra cuáles aplican. Con un
proveedor de embeddings local, indexar y buscar no hacen llamadas de red.

## Medir lo tuyo

```bash
devctx status                  # archivos, fragmentos, símbolos, modelo, frescura
devctx projects list           # cada repositorio, tamaño y antigüedad del índice
time devctx index --full       # tu rendimiento de indexado
time devctx search "..."       # tu latencia de búsqueda
```

`devctx status` emite JSON, así que es scriptable. Si tus cifras difieren
salvajemente de esta página, las causas usuales son el ancho del modelo, un
índice HNSW presente durante una carga masiva, o un repositorio lleno de código
vendorizado que `exclude` debería estar eliminando.
