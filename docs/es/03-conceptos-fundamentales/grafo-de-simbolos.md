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

Cada sitio de llamada de un símbolo en el código indexado — llamadas e
instanciaciones (`new Foo()`), una por ocurrencia, así que dos llamadas desde el
mismo método son dos referencias. La respuesta directa a
*"¿es seguro cambiar esto?"* a un salto. `símbolo` es un nombre pelado (`charge`),
uno calificado (`Card.charge`, `Card::charge`) o `archivo::nombre`
(`src/pay.rs::charge`).

En un índice hecho por 0.10 o posterior, cada referencia conserva `file`, `line` y
`source` y agrega:

| Campo | Significado |
|---|---|
| `confidence` | `high`, `medium` o `low`: qué tan seguro está el índice de que esta ocurrencia es *esta* definición |
| `via` | La relación: `calls` o `instantiates` por defecto; con `kinds`, también `references` (un uso de tipo), `imports`, `inherits`, `implements` |
| `sym` | El id (16 dígitos hex) de la definición referida, cuando resolvió a una |
| `external` / `test` / `undecided` | Presentes (y `true`) solo cuando la ocurrencia es una llamada a una librería, está en un archivo de test o no se pudo decidir |

**`min_confidence`** (`"high"`, `"medium"` —el default— o `"low"`) fija la
confianza más baja que se lista. Por defecto se listan `high` y `medium`, y la
`medium` va marcada por su campo; las referencias ambiguas y las llamadas que el
índice no pudo decidir quedan afuera y se cuentan en
`below_confidence: {count, min_confidence, hint}`, nunca se pierden en silencio.
Con `min_confidence: "low"` se listan, cada una marcada. Para un nombre sin
definición en el repositorio (una función de librería), la respuesta lista sus
sitios de llamada externos.

**`kinds`** agrega relaciones a las llamadas e instanciaciones del default:
cualquiera de `references` (usos de tipo), `imports`, `inherits`, `implements`, o
`all` (`GET /references/<símbolo>?kinds=references,imports` por HTTP). Un servidor
anterior a 0.10 ignora `min_confidence` y `kinds`; el cliente lo dice en `warning`.

### `impact_analysis(símbolo)` — radio de impacto

Llamadores *y* llamados transitivos. Los llamadores son el radio de impacto:
todo lo que se podría romper. Los llamados son de qué depende este símbolo para
funcionar.

Corrélo antes de refactorizar cualquier cosa pública. Esta es la operación que
la gente olvida que existe y después lamenta no haber corrido.

Cada lado es una lista de `{symbol, depth}` (profundidad 1 = directo), lo más
profundo al final. En un índice hecho por 0.10 o posterior el recorrido sigue el
grafo de símbolos **por id** — una consulta por nivel, sin importar el tamaño de
la frontera — y cada símbolo trae además `confidence` (de la llamada que lo
alcanzó), `via` (`calls` o `instantiates`), `sym`, `file` y `line` (de su
definición), y `test`, `external` o `undecided` cuando son verdaderos. Dentro de
una profundidad, el orden es por confianza, después por rank (cuando el índice lo
calcule) y después por nombre.

Lo que los defaults dejan afuera **se cuenta, nunca desaparece en silencio**, y no
se recorre:

| Parámetro (flag del CLI) | Default | Afuera por defecto, contado en |
|---|---|---|
| `min_confidence` (`--min-confidence`) | `medium` | símbolos alcanzados solo por llamadas de confianza `low` → `below_confidence: {count, min_confidence, hint}` (cuenta símbolos); llamadas al nombre que el índice no pudo decidir (`svc.update()` con un receptor sin tipo) → `undecided_calls: {count, hint}` (cuenta llamadas, ninguna desde un test ni desde un llamador ya listado); con `low` se listan los dos, los llamadores de las llamadas sin decidir marcados `undecided` y sin recorrerlos |
| `include_tests` (`--include-tests`) | `false` | símbolos de archivos de test → `excluded.tests` |
| `include_external` (`--include-external`) | `false` | llamados de librerías → `excluded.external` |
| `max_nodes` (`--max-nodes`) | `200` por dirección (`0` = sin tope) | los símbolos de la profundidad que lo desborda → `omitted: {count, reason: "limit"}` y `omitted_by_limit: {count, max_nodes, upstream/downstream: {count, depth}}`; no se lee ningún nivel más profundo |

Por HTTP: `GET /impact/<símbolo>?depth=3&min_confidence=low&include_tests=true&max_nodes=500`.
La respuesta dice en `filters` qué filtros aplicó. Un nombre pelado significa las
definiciones del repositorio con ese nombre (como en `get_references`), no toda
llamada escrita con ese nombre.
Los campos de 0.9 conservan su significado: `resolved_symbols` (un nombre pelado
que representaba varias declaraciones), `branch_fallback`, `warning`, y `omitted`
/ `omitted_for_budget` (la mitad del presupuesto de salida por dirección;
`omitted.count` suma lo que cortaron el presupuesto y `max_nodes`). Un nombre sin
definición en el repositorio responde como `read_symbol`: `external: true` con
`called_from` (su upstream son entonces los llamadores de sus sitios de llamada) o
`suggestions`. En un índice anterior a 0.10 el recorrido es el de 0.9 — por
nombre, sin confianza, sin filtros, sin tope — con el `warning` que lo dice (y, si
se pasaron filtros, que no se aplicaron); un servidor anterior a 0.10 ignora los
parámetros, y el cliente lo dice en `warning`.

### `read_symbol(nombre)` — la definición

Código, archivo, rango de líneas y tipo. Usalo cuando sabés el nombre y querés
la cosa misma; usá `search` cuando querés código *sobre una idea*. `limit`
(default 5) acota las definiciones — un nombre puede estar definido muchas veces.
El nombre puede ser pelado, calificado (`Card.charge`, `Card::charge`, con su
paquete o crate adelante) o `archivo::nombre`; `Foo.Bar::baz` es el miembro `baz`
de `Foo.Bar`, no un archivo.

En un índice hecho por 0.10 o posterior cada definición trae además `sym` (su id,
16 dígitos hex, el mismo en todas las ramas), `kind` (`function`, `method`,
`class`, `field`…) y `qualified`. Una definición a la que el chunker no le da
código propio (un campo, una constante) responde con su firma como `code`.

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
  un typo de `build_context`). En un índice 0.10 un externo no trae ninguna
  `suggestions`, como en 0.9; y en un índice 0.10 `external` sale de la evidencia del propio índice (una dependencia
  declarada, la plataforma), y el último segmento solo se prueba cuando el
  repositorio no define nada con ese nombre: un `Foo::new` que no existe no es
  "externo" porque se llame algún `new` — recibe `suggestions` (`Thing.new`). Un
  `archivo::nombre` que no encuentra nada en ese archivo tampoco es externo.

Las funciones chicas que el chunker agrupó en un solo fragmento (`a, b, c`) se
encuentran dentro de él, así que no se reportan como externas por error.

**Un índice hecho antes de 0.10** (o por otro extractor) no tiene tabla de
símbolos: `read_symbol`, `get_references`, el anclaje de `search` y la vista web
del grafo responden como 0.9, por nombre sobre el grafo de llamadas, y lo dicen en
`warning` — reindexá con `devctx index --full` para tener los campos de arriba.
`search` en `keyword`/`hybrid` también lo dice: su anclaje de identificadores fue
por nombre. Una lectura fallida de la tabla de símbolos trae su propio `warning`
(un error de lectura, no un motivo para reindexar).

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
> `actualizar` pelado devolvía cero mientras `OfficeService.actualizar` devolvía
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
