# Búsqueda semántica de código

> 🇬🇧 [Read in English](../../03-core-concepts/search.md)

Encontrar código describiendo lo que hace, no adivinando cómo se llama.

---

## Qué es

`devctx search` responde una pregunta en prosa — *"dónde decidimos que un token
expiró"* — con los fragmentos de código que más probablemente contengan la
respuesta. Hay tres estrategias de recuperación, y una es la predeterminada:

```bash
devctx search "manejo de token expirado"          # vectorial (default)
devctx search "token expirado" --keyword          # BM25
devctx search "token expirado" --hybrid           # ambas, fusionadas
```

Los agentes llegan a lo mismo por la herramienta MCP `search`.

## Por qué existe

Grep encuentra la cadena que escribiste. Eso funciona cuando ya conocés el
vocabulario del repositorio y falla justo cuando no — un proyecto nuevo, un
subsistema que nunca abriste, un concepto que tres equipos escribieron de tres
formas distintas (`isExpired`, `checkTTL`, `validateWindow`).

La búsqueda vectorial encuentra *significado*, así que llega a la tercera forma
partiendo de la primera. La búsqueda por palabra clave encuentra el token
literal, que es lo que querés para un mensaje de error o un identificador que
copiaste de un stack trace. Ninguna es estrictamente mejor, y por eso están las
dos — y por eso existe la híbrida.

## Cómo funciona

### El pipeline

Indexar convierte archivos en fragmentos embebibles:

```
git diff → parse (tree-sitter) → chunk → embed → store (DuckDB)
```

Cada etapa vive en su propio crate: `devctx-parse`, `devctx-chunk`,
`devctx-embed`, `devctx-store`. Todo corre en proceso — no hay sidecar ni salto
de red, salvo que configures un proveedor de embeddings por API.

### Niveles de chunk

El chunker nunca parte un símbolo por la mitad. Emite fragmentos en cinco
niveles, y cuáles produce un archivo depende de lo que tenga adentro:

| Nivel | Qué contiene |
|---|---|
| `file` | Un fragmento resumen: la ruta más los símbolos declarados |
| `class` | Un contenedor — `class`, `struct`, `enum`, `trait`, `interface` — con su firma y miembros |
| `doc` | La prosa de un símbolo documentado, cuando el comentario dice algo que el nombre no |
| `function` | Un invocable, entero |
| `block` | Una porción de una función demasiado grande para embeber como un solo fragmento |

Dos comportamientos vale la pena conocerlos porque cambian lo que recibís:

- **Los símbolos chicos se agrupan.** Todo lo que baja de `min_chunk_tokens`
  (64) se fusiona con sus vecinos en un fragmento en vez de embeberse solo — un
  archivo de getters de una línea produce un puñado de fragmentos, no doscientos.
- **Un comentario que solo repite el nombre no genera fragmento.**
  `/// El nombre.` arriba de `fn nombre()` no aporta nada que el fragmento de la
  función no tenga.

Valores por defecto, de `ChunkConfig`:

| Ajuste | Default | Significado |
|---|---|---|
| `max_chunk_tokens` | 512 | Cota superior antes de partir una función en bloques |
| `min_chunk_tokens` | 64 | Por debajo de esto, los símbolos se agrupan |
| `large_function_threshold` | 1024 | Por encima, la función se parte en fragmentos de bloque |

Los tokens se estiman a ~4 caracteres por token. Es una heurística, no un
tokenizador.

### Headers de contexto

A un fragmento `function` o `block` se le antepone una línea de miga de pan
antes de embeberlo:

```
# auth/middleware.rs > AuthMiddleware > authenticate
```

Sin ella, el cuerpo de un método se lee como código anónimo. Con ella, el
embedding carga dónde vive el código, así que una consulta que nombra el módulo
o el tipo puede encontrar un cuerpo que no menciona ninguno de los dos.

### Hash de contenido

Cada fragmento lleva `content_hash` — sha256 de su texto, truncado a 16
caracteres hexadecimales. Eso es lo que hace barato re-indexar: un fragmento con
hash sin cambios no se vuelve a embeber. También es lo que hace barato indexar
varias ramas, ya que las ramas comparten la abrumadora mayoría del contenido de
sus archivos.

## Los tres modos

### Vectorial — el default

La consulta se embebe con el mismo modelo que embebió el código, y el store
devuelve los vecinos más cercanos por distancia coseno (HNSW cuando el índice
está construido, si no un escaneo).

### Palabra clave — BM25

Búsqueda de texto completo sobre el texto de los fragmentos, servida por la
extensión FTS de DuckDB. Exacta, rápida, y la elección correcta para mensajes de
error e identificadores.

### Híbrida — fusión por rango recíproco

Corren los dos recuperadores y se fusionan sus listas ordenadas:

```
score(item) = Σ  1 / (k + rango)     k = 60, el rango empieza en 1
```

Un ítem bien rankeado por cualquiera de los dos aparece; un ítem bien rankeado
por ambos aparece más arriba. RRF fusiona *rangos*, no puntajes, así que no
necesita calibrar entre dos sistemas cuyos números significan cosas distintas.

Si el índice FTS no fue construido, la híbrida degrada en silencio a solo
vectorial en vez de fallar.

## Reranking

Un cross-encoder puede reordenar el pool de candidatos antes de truncarlo a
`--limit`. **Está apagado por defecto, y ese default lo puso la medición, no el
gusto.** En este repositorio:

| Configuración | Latencia | Memoria residente |
|---|---|---|
| Sin reranking | 30 ms | 406 MB |
| El cross-encoder más barato | 8.6 s | 2.4 GB |
| `bge-reranker-base` | 30 s | 3.4 GB |

Y el único modelo medido contra todo el banco de pruebas empeoró los
resultados — bajó una respuesta correcta del primer puesto al vigésimo primero.

Dos cosas que entender antes de encenderlo:

- **El pool es el techo.** Un reranker reordena lo que le entregan y nada más.
  Una respuesta rankeada por debajo de `reranking.pool` le es invisible, por
  bueno que sea el modelo.
- **El pool también es todo el costo.** El cross-encoder es la etapa más lenta
  por dos órdenes de magnitud, y el tamaño del pool lo multiplica. Pool profundo
  con modelo chico y rápido, o pool corto con modelo grande. Profundo *y* grande
  es inusable.

El pool por defecto es 20. `--no-rerank` lo desactiva para una búsqueda sin
importar la configuración.

Rerankers incluidos: `bge-base` (default), `bge-v2-m3` (multilingüe),
`jina-turbo` (el más rápido). Poné `reranking.model_dir` para cargar tu propio
cross-encoder ONNX — vale la pena, porque fastembed no trae ninguno liviano y
los incluidos pasan todos del gigabyte.

## Modelos de embedding

`devctx models` lista lo disponible. El default actual para proyectos nuevos es
`ml-granite` (384 dimensiones, multilingüe, la mejor recuperación en CPU).

**Elegí antes del primer índice.** Cambiar el modelo después significa
re-indexar cada archivo y re-embeber cada memoria, porque los vectores de dos
modelos no viven en el mismo espacio.

Si tu código o tus comentarios no están en inglés, elegí un modelo multilingüe.
Los modelos en inglés van a embeber español con toda felicidad — solo que mal.

## Conciencia de ramas

Los fragmentos se guardan por `(repo, rama)`. Una búsqueda devuelve resultados
de la rama en la que estás, así que un símbolo borrado en tu rama no aparece
desde `main`.

Las ramas que querés indexadas se declaran en la configuración bajo
`indexing.branches`, y `devctx index --branch <nombre>` indexa una en concreto.
Como la copia la maneja `content_hash`, indexar una segunda rama copia en vez de
re-embeber todo lo que las dos comparten — medido en 95–96% de los archivos
sobre tres repositorios reales.

Indexar es independiente del worktree: corrélo desde cualquiera y actualiza el
mismo índice.

## Filtros

`--language <lang>` restringe a un lenguaje. `--limit` limita resultados
(default 10). `--format json` emite un arreglo JSON en vez de la tabla.

## Ejemplo trabajado

```bash
$ devctx search "cómo decidimos que un token expiró" --limit 3
```

1. La consulta se embebe (vector de 384 dimensiones, `ml-granite`).
2. El store devuelve los 20 fragmentos más cercanos para este repo y rama.
3. Con reranking apagado, se devuelven los 3 primeros en el orden del
   recuperador.

El primer resultado suele ser un fragmento `function` cuyo header de contexto
nombra el tipo y el módulo — que es cómo una consulta que dice "token" encuentra
un método llamado `sigue_valido`.

## Modelo mental

Grep es un índice de **cadenas**. Esto es un índice de **significado**, con un
índice de cadenas al lado y una forma de fusionar los dos.

Usá búsqueda vectorial cuando sabés qué *hace* el código. Usá palabra clave
cuando sabés cómo se *llama*. Usá híbrida cuando no estás seguro — que, en un
repositorio desconocido, es la mayoría del tiempo.
