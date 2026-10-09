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

El `reranking.pool` por defecto es 100. Cuando no corre ningún reranker, el
recuperador trae un pool corto de 20 en su lugar — una búsqueda más profunda se
tiraría sin reordenamiento. `--no-rerank` desactiva el reranking para una
búsqueda sin importar la configuración.

Rerankers incluidos: `bge-base` (default), `bge-v2-m3` (multilingüe),
`jina-turbo` (el más rápido). Poné `reranking.model_dir` para cargar tu propio
cross-encoder ONNX — vale la pena, porque fastembed no trae ninguno liviano y
los incluidos pasan todos del gigabyte.

## Modelos de embedding

`devctx models` lista lo disponible. El default que viene es `minilm-l6` (384
dimensiones, inglés). Para código que no está en inglés, `ml-granite` (384,
multilingüe) midió mejor en CPU.

El asterisco en `devctx models` marca lo que *esta máquina* está configurada
para darle a los proyectos nuevos, que podés cambiar en la config central — no
lo que viene de fábrica.

**Elegí antes del primer índice.** Cambiar el modelo después significa
re-indexar cada archivo y re-embeber cada memoria, porque los vectores de dos
modelos no viven en el mismo espacio.

Si tu código o tus comentarios no están en inglés, elegí un modelo multilingüe.
Los modelos en inglés van a embeber español con toda felicidad — solo que mal.

## Ranking: qué sube y qué se hunde

La recuperación encuentra candidatos; cuatro pasos deciden qué ve primero el
agente.

### Tipo de archivo y penalización

Cada resultado se clasifica por su ruta (y lenguaje) en uno de cuatro **tipos**,
evaluados en este orden; gana el primero que calza:

| Tipo | Qué calza |
|---|---|
| `test` | `test/`, `tests/`, `__tests__`, `__mocks__`, `spec/`, `e2e/`, `testdata`, `fixtures`; `*_test.*`, `test_*`, `*.test.*`, `*.spec.*`, `*.cy.*`, `conftest.py`, y `*Test` / `*Tests` / `*IT` en Java/Kotlin/Scala/Groovy/C#/PHP/Swift (`Contest.java` es código) |
| `doc` | `*.md`, `*.mdx`, `*.rst`, `*.adoc`, `*.txt`, `docs/`, `README*`, `CHANGELOG*`, `LICENSE*`, `CONTRIBUTING*` |
| `config` | `*.sql`, `*.yaml`, `*.yml`, `*.json`, `*.toml`, `*.xml`, `*.properties`, `*.ini`, `*.cfg`, `*.conf`, `requirements*.txt` |
| `code` | todo lo demás; las memorias no tienen archivo y cuentan como código |

Tests, docs y config se **degradan, no se excluyen**. El factor de la config
(`search.penalty.test|doc|config`, `0.6` cada uno por defecto) se convierte en
un número de posiciones — `round((1 − factor) × 10)`, así que `0.6` ubica un
resultado como si estuviera cuatro puestos más abajo — porque los scores no
tienen una escala común entre modos (cosenos, fracciones RRF, logits del
cross-encoder). Los empates se los lleva el resultado sin penalizar, los scores
se mantienen monótonos (un resultado degradado nunca muestra un score mayor que
el de arriba; `raw_score` conserva el número propio del recuperador) y la
degradación se topa en `(limit − 1) / 2` posiciones para que un `limit` chico no
saque de la respuesta al mejor resultado. `1.0` apaga la penalización de un
tipo; `0.0` lo manda al final.

```yaml
# .devctx/config.yaml
search:
  penalty:
    test: 0.6
    doc: 0.6
    config: 0.6
```

### Filtros duros: `kind` e `include_tests`

Cuando degradar no alcanza, filtrá:

```bash
devctx search "token refresh" --kind code        # solo código
devctx search "token refresh" --kind test        # solo tests
devctx search "token refresh" --no-tests         # todo menos tests
```

En la tool MCP: `kind: "code" | "test" | "doc" | "config"` e
`include_tests: false`. Un `kind` explícito gana sobre `include_tests`; un `kind`
inválido falla con ``` `kind` must be one of code, test, doc, config ```. Ambos
aplican también a `search_project`, a la búsqueda de grupo y a `build_context`.

### Deduplicación

El mismo fragmento se devuelve una sola vez. Un resultado se descarta cuando otro,
mejor rankeado, tiene el mismo id, el mismo `(archivo, inicio, fin)` — el mismo
rango indexado en dos ramas — o un rango *contenido* en un fragmento de
contenido del mismo archivo. Los fragmentos resumen (`file`, `class`, `doc`) nunca
se tragan el código que tienen adentro: una función detrás del fragmento resumen
de su archivo sigue siendo un resultado. La deduplicación corre antes del corte a
`limit`, así que lo que descarta no se informa como omitido.

### Anclaje por identificador

En modo `keyword` e `hybrid`, BM25 premia a quien más *menciona* un identificador,
no a quien lo *define*. Por eso una consulta con tokens con forma de
identificador — `snake_case`, `camelCase` / `PascalCase`, `Foo.bar`, `foo::bar` —
busca sus definiciones en la tabla de símbolos y las pone primero, marcadas
`anchored: true`. Acotado a propósito: como máximo 3 identificadores por consulta,
como máximo la mitad de la respuesta (nunca menos de uno), y una copia de test o
un mock que coincida nunca le gana a la definición de producción. Los nombres de
archivo (`state.rs`, `README.md`), las versiones (`v0.8.4`) y las abreviaturas
(`e.g`) no son identificadores; una palabra común tampoco. Las definiciones
ancladas no se degradan, pero un filtro duro de `kind` igual las quita. En un
índice 0.10 las definiciones salen de la tabla de símbolos, una por definición
(una función grande partida en bloques fija su primer bloque), y un resultado
anclado trae además `sym`, el id de la definición que es.

## Qué deja afuera el índice

La búsqueda solo puede rankear lo que se indexó. Desde 0.9, las rutas vendorizadas
y generadas se excluyen **por defecto** aunque git las trackee — `node_modules/`,
`dist/`, `vendor/`, `third_party/`, `bower_components/`, `*.min.js`,
`*.generated.*`, y `/build/` y `/target/` en la raíz del repositorio. La lista, los
opt-outs y cómo se reconcilia un cambio están en
[Configuración](../11-configuracion.md#excludes-por-defecto) y en
[Mantener el índice al día](../13-mantener-el-indice-al-dia.md#controlar-qué-se-indexa).

## Conciencia de ramas

Los fragmentos se guardan por `(repo, rama)`. Una búsqueda devuelve resultados
de la rama en la que estás, así que un símbolo borrado en tu rama no aparece
desde `main`.

**Cuando tu rama no tiene filas**, la respuesta no se queda muda. `search` — y las
tools de grafo `read_symbol`, `get_references`, `impact_analysis`, `search_routes`
y `routes_for_handler` — caen, en este orden, a la rama por defecto configurada y
luego a la rama indexada más recientemente que tenga filas, y lo dicen:

```json
"branch_fallback": {
  "current": "feat/plan-124",
  "used": "main",
  "why": "current branch feat/plan-124 is not indexed; answering from branch main"
}
```

(`is indexed but empty` cuando la rama tiene registro pero no filas.) Sin ninguna
rama indexada la llamada falla con `<repo> has no index for any branch; run devctx
index`. `index_status` informa `indexed: false` con `indexed_branches` (la más
reciente primero) y una pista que nombra la rama que se usaría. El fallback es una
mentira por omisión si lo ignorás: el símbolo que estás por editar en tu rama puede
ser distinto del que describe la respuesta.

### Un extractor viejo

El índice registra qué extractor lo produjo (`index_meta`: una versión más un hash
de las definiciones de lenguaje). Cuando eso difiere del binario en ejecución — o
no hay registro, como en cualquier índice hecho con 0.8.2 o anterior —
`index_status` dice `extractor_stale: true` con una pista, y cada tool de grafo
suma `warning: "index generated by an older extractor …; reindex (devctx index
--full)"`. Las corridas incrementales nunca lo limpian: solo `devctx index --full`,
y una corrida completa ya no copia filas de una rama que a su vez está vieja.
Hasta entonces una rama **nueva** re-embebe todos los archivos en vez de copiar de
una ya indexada. `search` en sí no lleva ese aviso: es una recuperación, no una
extracción. Si cambiás las gramáticas o `routes.rs`, subí `EXTRACTOR_VERSION` a
mano: el hash cubre solo los JSON de lenguajes.

Las ramas que querés indexadas se declaran en la configuración bajo
`indexing.branches`, y `devctx index --branch <nombre>` indexa una en concreto.
Como la copia la maneja `content_hash`, indexar una segunda rama copia en vez de
re-embeber todo lo que las dos comparten — medido en 95–96% de los archivos
sobre tres repositorios reales.

Indexar es independiente del worktree: corrélo desde cualquiera y actualiza el
mismo índice. Qué `HEAD` lee desde un worktree enlazado está en
[Mantener el índice al día](../13-mantener-el-indice-al-dia.md#desde-un-worktree-enlazado).

## Filtros

`--language <lang>` restringe a un lenguaje. `--limit` limita resultados
(default 10). `--kind <code|test|doc|config>` y `--no-tests` filtran por tipo de
archivo (ver arriba). `--format json` emite un objeto, `{"results": [...]}`, en
vez de la tabla; cada resultado tiene `score`, `file`, `start_line`, `end_line`,
`symbol`, `symbol_type`, `level`, `language`, `kind`, `text` y, cuando
corresponde, `raw_score` y `anchored`. La tool MCP responde el mismo objeto, más
`branch_fallback`, `omitted` y `warning` cuando aplican — ver
[Integración MCP](integracion-mcp.md#formas-de-retorno).

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
