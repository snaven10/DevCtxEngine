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

La memoria residente la domina el modelo cargado. La cifra de ~406 MB de arriba
es un modelo de embeddings de 384 dimensiones más el store; habilitar un
cross-encoder agrega gigabytes, que es la razón real de que venga apagado.

**Solo-CPU es el caso asumido.** Nada de acá requiere GPU.

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
