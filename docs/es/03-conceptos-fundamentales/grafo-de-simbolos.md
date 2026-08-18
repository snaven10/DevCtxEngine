# Grafo de símbolos

> 🇬🇧 [Read in English](../../03-core-concepts/symbol-graph.md)

Un grafo de llamadas sobre el código indexado: quién llama a qué, y hasta dónde
llegaría un cambio.

---

## Qué es

Durante el indexado, tree-sitter parsea cada archivo soportado y extrae dos
cosas: los símbolos que declara y las llamadas que hace. Las llamadas se
vuelven aristas.

```bash
devctx symbol authenticate          # la definición y su código
devctx impact authenticate          # llamadores y llamados transitivos
```

Los agentes usan `read_symbol`, `get_references` e `impact_analysis`.

## Por qué existe

La búsqueda semántica responde *"¿dónde está el código sobre X?"*. No puede
responder *"¿qué se rompe si cambio esto?"*, porque esa pregunta es de
estructura, no de significado — y la respuesta incluye código que nunca menciona
X.

El grafo responde la pregunta estructural exactamente, sin ranking y sin
aproximación. Un llamador existe o no existe.

## Qué hay realmente en el grafo

**Un solo tipo de arista: `calls`.**

Vale la pena decirlo sin rodeos, porque es fácil suponer otra cosa. El parser
también extrae imports y ligaduras de tipo, pero eso se usa para *resolver* los
destinos de las llamadas — no se guardan como aristas. No hay aristas
`inherits`, `implements` ni `references`.

Entonces: esto es un grafo de llamadas. No un grafo de dependencias, ni una
jerarquía de tipos.

Tipos de símbolo que reconocen las consultas:

`function` · `method` · `class` · `struct` · `enum` · `interface` · `type`

## Lenguajes soportados

**Parseo completo — símbolos y aristas de llamada (7):**

| Lenguaje | Extensiones |
|---|---|
| Python | `.py` `.pyi` |
| JavaScript | `.js` `.mjs` `.cjs` `.jsx` |
| TypeScript | `.ts` `.mts` `.cts` |
| TSX | `.tsx` |
| Go | `.go` |
| Java | `.java` |
| Rust | `.rs` |

**Indexados como texto crudo — buscables, pero sin símbolos ni aristas:**

`.html` `.htm` `.css` `.scss` `.sass` `.less` `.json` `.yaml` `.yml` `.xml`
`.md` `.markdown` `.sql` `.graphql` `.gql` `.proto` `.kt` `.kts`

Se fragmentan con solapamiento y se embeben, así que la búsqueda los encuentra.
Simplemente no aparecen en el grafo.

Kotlin es el caso notable: no tiene gramática de tree-sitter conectada, así que
se indexa como texto — **pero sus rutas de Spring sí se extraen**, porque la
detección de rutas tiene un camino aparte.

Cualquier otra cosa no se indexa.

## Almacenamiento

Las aristas viven en la base DuckDB del proyecto, en `graph_edges`:

| Columna | Contiene |
|---|---|
| `source` / `target` | Nombres de símbolo |
| `kind` | Siempre `calls` |
| `source_file` / `target_file` | Dónde vive cada lado |
| `line` | Dónde aparece la llamada |
| `repo` / `branch` | Alcance — el grafo es por rama, como todo lo demás |

La unicidad es `(source, target, kind, repo, branch, source_file)`, así que la
misma llamada desde dos archivos distintos son dos aristas, y re-indexar no
duplica.

## Operaciones

### `get_references(símbolo)` — ¿quién llama a esto?

Cada sitio de llamada de un símbolo en el código indexado. La respuesta directa
a *"¿es seguro cambiar esto?"* a un salto.

### `impact_analysis(símbolo)` — radio de impacto

Llamadores *y* llamados transitivos. Los llamadores son el radio de impacto:
todo lo que se podría romper. Los llamados son de qué depende este símbolo para
funcionar.

Corrélo antes de refactorizar cualquier cosa pública. Esta es la operación que
la gente olvida que existe y después lamenta no haber corrido.

### `read_symbol(nombre)` — la definición

Código, archivo, rango de líneas y tipo. Usalo cuando sabés el nombre y querés
la cosa misma; usá `search` cuando querés código *sobre una idea*.

## Límites que conviene conocer

**La resolución de llamadas es por nombre**, informada por imports y ligaduras
de tipo donde la gramática lo soporta. Dos métodos llamados `save()` en tipos
distintos pueden resolver al mismo nodo. Tratá los resultados de impacto como un
superconjunto a revisar, no como un conjunto exacto probado.

**El despacho dinámico es invisible.** Una llamada hecha por un callback, una
API de reflexión o un registro llaveado por strings no deja arista sintáctica de
llamada. El grafo va a sub-reportar justo donde un lenguaje es más dinámico.

**Solo 7 lenguajes producen aristas.** En un repositorio políglota, el grafo
cubre una parte, y no hay aviso que diga cuál. `devctx status` muestra el conteo
de símbolos; uno sospechosamente bajo suele significar que el código está en un
lenguaje que se está indexando como texto.

## Cómo complementa a la búsqueda

| Pregunta | Herramienta |
|---|---|
| *¿Dónde está el código sobre autenticación?* | `search` |
| *¿Qué llama a `authenticate`?* | `get_references` |
| *¿Qué se rompe si lo cambio?* | `impact_analysis` |
| *¿Por qué está escrito así?* | `memories_by_symbol` |
| *¿Qué debería leer antes de responder?* | `build_context` |

La búsqueda es difusa y rankeada. El grafo es exacto y sin ranking. La cuarta
fila es la que a nadie se le ocurre preguntar, y es la única que el código no
puede responder en absoluto.

## Modelo mental

La búsqueda es un mapa del territorio: te muestra qué está cerca de qué, por
significado. El grafo es la red vial: te muestra qué conecta realmente con qué,
y por lo tanto adónde se va el tráfico cuando cerrás una calle.

Querés el mapa para encontrar el barrio, y la red vial antes de romper el
pavimento.
