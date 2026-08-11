> 🌐 [English version](../02-architecture.md)

# Arquitectura

DevCtxEngine es un único binario en Rust sobre DuckDB. Todo — indexado,
embeddings, búsqueda, el servidor MCP, la API HTTP, el TUI — vive en un mismo
proceso, sin más dependencia en tiempo de ejecución que git.

---

## 1. Forma

Un workspace de Cargo con crates enfocados, cada uno dueño de una preocupación:

| Crate | De qué es dueño |
|---|---|
| `devctx-core` | Tipos compartidos, esquema de config, resolución de rutas, fusión por rango |
| `devctx-store` | DuckDB: vectores, grafo de llamadas, rutas, memorias, estado de índice, registro |
| `devctx-parse` | tree-sitter: símbolos, imports, aristas de llamada, rutas de frameworks |
| `devctx-chunk` | Chunking semántico: fichero / clase / función / bloque, y ventanas de memoria |
| `devctx-embed` | Embeddings: ONNX local vía fastembed, u OpenAI / Voyage / custom |
| `devctx-rerank` | Reranking con cross-encoder, con alternativa no-op |
| `devctx-index` | El pipeline: seleccionar → parsear → chunkear → embeber → almacenar |
| `devctx-search` | Recuperación vectorial / por palabras clave / híbrida, y reranking |
| `devctx-memory` | remember (dedup, revisión) y recall (mezcla intro + chunk) |
| `devctx-summarize` | Extractivo por defecto; OpenAI o flan-t5 local como opción |
| `devctx-central` | El store central: registro de proyectos, memorias globales, cliente del daemon |
| `devctx-mcp` | Servidor MCP (stdio) y las implementaciones de herramientas que todo reutiliza |
| `devctx-api` | API HTTP (axum) sobre esas mismas implementaciones, más el daemon |
| `devctx-tui` | Interfaz de terminal (ratatui) |
| `devctx-cli` | El binario `devctx` |

La dirección de dependencias es estricta: `core` abajo, `cli` arriba, nada
apuntando hacia abajo de vuelta. `devctx-mcp` contiene los cuerpos de las
herramientas (`do_search`, `do_recall`, …) y `devctx-api` llama a esas mismas
funciones, así que el servidor MCP, la API HTTP y el CLI no pueden divergir: hay
una implementación de cada operación, no tres.

## 2. Un escritor por base de datos

DuckDB permite un único proceso lector-escritor por fichero. Esa restricción da
forma a todo el runtime.

```
   sesión MCP       comando CLI       TUI        panel web
        |                |             |             |
        +--------+-------+------+------+------+------+
                          |  HTTP (loopback)
                 devctx serve  ...........  posee index.duckdb
                          |                  mantiene el modelo caliente
                 .devctx/state/index.duckdb
```

El primer comando que necesita la base levanta un servidor en segundo plano, lo
anuncia en `.devctx/state/serve.json` y enruta a él. Todos los comandos
posteriores — desde cualquier proceso — encuentran ese fichero y enrutan también.
Nadie pelea nunca por el bloqueo, varias sesiones de agente pueden compartir un
proyecto, y puedes consultar mientras corre un indexado. El servidor se apaga
tras 15 minutos de inactividad.

Cuando no se puede levantar ningún servidor, los comandos abren el store
directamente. Es lo correcto para un comando solitario y mantiene la herramienta
usable en entornos restringidos; `DEVCTX_NO_AUTOSERVE=1` lo fuerza.

El **store central** sigue el mismo patrón con una diferencia: es un singleton,
compartido por todos los proyectos, así que `devctx serve --central` es el único
escritor y un segundo se rechaza en vez de competir. Ver
[El store central](12-store-central.md).

## 3. Indexado

```
  árbol de trabajo ─► seleccionar ─► parsear ─► chunkear ─► embeber ─► almacenar
                          │            │           │           │           │
                     diff de git,  símbolos de  fichero/    ONNX o     vectores +
                     sin trackear,  tree-sitter  clase/      API        grafo +
                     o rutas        + aristas    función                rutas +
                     explícitas                  /bloque                file_state
```

**La selección** es la única parte que consulta git: el diff desde el último
commit indexado, más los ficheros sin trackear que git no ignora. Una ejecución
completa lista todo el árbol de trabajo; una lista explícita de rutas se salta
git por completo, que es lo que necesita un vigilante de ficheros — un guardado
no mueve ningún commit, así que un diff de commits saldría vacío.

**El salto** ocurre por fichero, mediante hash de contenido. Reindexar un fichero
sin cambios cuesta una lectura y un hash, no un embedding. Es lo que hace que el
hook post-commit y el watcher sean lo bastante baratos como para correr
constantemente.

**El estado** vive en dos tablas indexadas por `(repo_path, branch)`:
`index_state` (último commit indexado, modelo y su dimensión) y `file_state`
(hash por fichero, lenguaje, cuentas de símbolos y chunks). Un cambio de modelo se
detecta aquí y fuerza un reindexado completo en vez de mezclar vectores
incompatibles.

## 4. Almacenamiento

Un fichero DuckDB por proyecto lo guarda todo:

| Tabla | Contiene |
|---|---|
| `vectors` | Embeddings de chunks (`FLOAT[n]`) y sus metadatos — la única tabla atada a la dimensión |
| `graph_edges` | Aristas de llamada e import, para el análisis de impacto |
| `routes` | Rutas HTTP extraídas del framework y sus handlers |
| `memories` | Decisiones, ideas y notas guardadas |
| `index_state`, `file_state` | Contabilidad incremental |
| `projects` | El registro — solo se puebla en el store central |

La búsqueda vectorial es `array_cosine_distance` sobre `FLOAT[n]`, una función
nativa de DuckDB que no necesita extensión. Dos extensiones opcionales añaden un
índice HNSW (VSS) para búsqueda aproximada y un índice BM25 (FTS) para palabras
clave; ambas degradan a fuerza bruta cuando no están disponibles, en vez de
fallar.

El ancho de la columna de vectores queda fijado al crear la tabla, y por eso
cambiar de modelo de embedding implica reindexar, y por eso el store central se
niega a abrir si su modelo de memoria ya no coincide con lo que hay en disco.

## 5. Recuperación

Tres modos, un solo camino:

- **Vectorial** — embeber la consulta, búsqueda coseno, rerank.
- **Palabras clave** — BM25 sobre el texto del chunk, sin necesidad de modelo.
- **Híbrida** — ambas, fusionadas por rango recíproco.

La fusión es por **rango**, nunca por score, y lo hace el mismo helper en todas
partes (`devctx_core::fuse_by_rank`). Los scores de una similitud vectorial y de
un peso BM25 no son comparables; tampoco lo son dos scores vectoriales de modelos
distintos, que es lo que convierte la fusión por rango en la primitiva correcta
para mezclar las memorias de un proyecto con las globales compartidas.

El reranking pasa un cross-encoder sobre los mejores candidatos. Es la etapa más
lenta, así que el TUI la omite por capacidad de respuesta y `--no-rerank` la
desactiva.

## 6. Memoria

Una memoria se guarda dos veces: como vector de introducción que cubre título más
contenido, y como ventanas deslizantes del cuerpo si es larga. El recall mezcla
las dos — `α·intro + (1-α)·mejor_chunk` — de modo que una memoria larga se
encuentra por cualquiera de sus partes sin que una corta quede ahogada.

La deduplicación es por hash de contenido normalizado, o por clave de tema si se
da, así que guardar lo mismo dos veces incrementa un contador en vez de añadir una
fila.

El alcance decide el destino: las memorias `local` se quedan en el proyecto, las
`global` van al store central donde todos los proyectos pueden recuperarlas. La
identidad global excluye deliberadamente al proyecto que la aportó, así que la
misma lección aprendida en dos repositorios converge en una sola memoria — con el
origen conservado como procedencia.

## 7. Interfaces

| Superficie | Transporte | Notas |
|---|---|---|
| CLI | — | Enruta al servidor; cae a acceso directo |
| MCP | JSON-RPC por stdio | Lo que usan los agentes; también enruta al servidor |
| API HTTP | axum, loopback | Token Bearer opcional; el panel se sirve desde ahí |
| TUI | ratatui | Cuatro vistas; el trabajo largo va en un hilo aparte |
| Web | HTTP | Página autocontenida, con dependencias empaquetadas |

Todas llegan a las mismas funciones `do_*`. Añadir una operación es escribirla una
vez y exponerla, no implementarla por superficie.

## 8. Dónde vive cada cosa

```
<repo>/.devctx/
  config.yaml          config del proyecto (merece la pena commitearla)
  .gitignore           mantiene state/ fuera de git
  state/
    index.duckdb       el índice de este proyecto
    serve.json         el servidor en marcha, si lo hay

~/.local/share/devctx/
  central.duckdb       registro + memorias globales
  serve.json           el daemon central
  models/              modelos descargados, compartidos por todos los proyectos

~/.config/devctx/
  config.yaml          config central
```

## 9. Lo que deliberadamente no hay

**Ningún servicio de red.** Todo escucha en loopback y existe para arbitrar un
bloqueo de fichero, no para servir a clientes remotos.

**Ningún daemon en segundo plano por defecto.** Los servidores se levantan bajo
demanda y se apagan solos. El planificador central que reindexa por temporizador
es opt-in.

**Ningún store de vectores compartido entre proyectos.** Un diseño anterior
apuntaba todos los repositorios a una misma base. Con stores por proyecto,
reindexar uno nunca bloquea a otro, cada uno puede usar un modelo distinto, y
ninguna búsqueda necesita un filtro por repo para ser legible. Solo se comparte lo
que no tiene un único dueño: el registro y las memorias globales.

**Nada de Python.** Las versiones anteriores levantaban un sidecar de ML en Python
por JSON-RPC. Parseo, chunking, embeddings y reranking están ahora en proceso; el
único programa externo que se invoca es `git`.
