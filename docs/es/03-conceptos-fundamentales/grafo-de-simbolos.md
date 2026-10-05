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

El grafo responde la pregunta estructural sin ranking: una arista existe o no
existe. Pero **su ausencia no prueba nada** — ver los límites más abajo, porque
esa distinción es la que evita un refactor roto.

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
la cosa misma; usá `search` cuando querés código *sobre una idea*. `limit`
(default 5) acota las definiciones — un nombre puede estar definido muchas veces.

**Un fallo se explica, no solo viene vacío.** Cuando no se encuentra ninguna
definición, `read_symbol` agrega:

- `suggestions` — hasta 5 nombres para probar: las formas calificadas que el grafo
  de llamadas conoce de un nombre pelado (`charge` → `Card.charge`), y luego los
  nombres definidos más cercanos por prefijo, sufijo, contención o distancia de
  edición (`AuthServce` → `AuthService`);
- `external: true`, `called_from: N` y un `next_step` cuando el nombre solo aparece
  como *destino* de llamadas — lo más probable es una función de librería o del
  runtime, aunque puede vivir en un archivo que el índice excluye o en otra rama.
  Un nombre calificado (`serde_json::from_str`, `Panache.withTransaction`) se busca
  tal cual y después por su último segmento, porque el grafo suele guardar solo el
  callee pelado. Un externo no recibe `suggestions` difusas (`with_context` no es
  un typo de `build_context`), solo las formas calificadas del mismo nombre.

Las funciones chicas que el chunker agrupó en un solo fragmento (`a, b, c`) se
encuentran dentro de él, así que no se reportan como externas por error.

## De qué rama responde

El grafo es por rama. Cuando la rama en checkout no tiene filas indexadas,
`read_symbol`, `get_references`, `impact_analysis`, `search_routes`,
`routes_for_handler` (y `search`, `build_context`, `memories_by_symbol`) responden
desde la rama por defecto o la indexada más recientemente y agregan
`branch_fallback: {current, used, why}`; sin ningún índice fallan con un error
explícito en vez de `[]`. Ver
[Búsqueda → Conciencia de ramas](busqueda.md#conciencia-de-ramas).

## Límites que conviene conocer

**Los nombres del grafo son calificados.** El `source` de una arista se escribe
`Clase.metodo` siempre que la función que llama esté dentro de un contenedor; su
`target` se escribe así cuando el sitio de llamada tenía un receptor con tipo
resoluble, y pelado cuando no. Una pregunta hecha con un nombre pelado se expande
a cada forma calificada que lo lleva, y la respuesta reporta qué declaraciones
fundió. Veintiún métodos llamados `actualizar` es un número corriente para un
repositorio Quarkus — un radio de impacto que los junta sin decirlo sería peor
que uno vacío, así que leé esa línea.

Preguntá con el nombre calificado cuando querés decir un método y nada más. Es
la única forma de acotarlo, y por eso un nombre calificado nunca se expande.

> Esto corrige una afirmación anterior de estas páginas: que la cobertura era
> "binaria por símbolo, sin nada que distinguiera los casos". No lo era. Un
> `actualizar` pelado devolvía cero mientras `OficinaService.actualizar` devolvía
> un llamador y veintitrés llamados: las aristas estuvieron ahí todo el tiempo,
> bajo una llave con la que nadie preguntaba.

**Un resultado vacío sigue significando "no encontré", nunca "no hay".** Dos de las
razones antes eran silenciosas y ya no lo son: una rama sin filas
(`branch_fallback`) y un índice construido por un extractor más viejo (`warning`
en cada respuesta de grafo, `extractor_stale` en `index_status` — corré `devctx
index --full`). Las de abajo siguen sin anunciarse. Ante un vacío, cruzá con
`search --keyword` antes de renombrar o borrar.

**La resolución de llamadas es por nombre**, informada por imports y ligaduras
de tipo donde la gramática lo soporta.

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
