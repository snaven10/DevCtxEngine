# Constructor de contexto

> 🇬🇧 [Read in English](../../03-core-concepts/context-builder.md)

Un solo brief con presupuesto para una pregunta: lo que ya se sabe, el código
que mejor rankea, y el conocimiento registrado contra exactamente ese código.

---

## Qué es

```bash
devctx context "cómo decidimos que un token expiró" --max-tokens 4096
```

Los agentes llaman la herramienta MCP `build_context`. La salida es **prosa, no
JSON** — está pensada para leerse directo al contexto de un modelo, y un sobre
JSON alrededor de código y prosa gasta presupuesto en puntuación.

## Por qué existe

Un agente frente a un área desconocida hace tres búsquedas, lee cuatro archivos
y corre un recall — gastando una tajada grande de su ventana en recuperación
antes de empezar a pensar. Peor: se lleva el *código* y se pierde el
razonamiento, porque nunca se le ocurrió preguntar qué ya se había decidido.

`build_context` hace ese ensamblado una sola vez, bajo un techo declarado, y
devuelve un solo artefacto.

## Las tres pasadas

El orden es el diseño. Cada pasada acota lo que la siguiente necesita decir.

### 1. Lo que ya se sabe

Un `recall` contra la pregunta, en todos los alcances, límite 5.

**Primero, porque es la parte que ninguna cantidad de lectura del código
recupera**, y porque es chica. El código te dice qué pasa; no te dice que la
alternativa obvia se probó y se abandonó.

Cada memoria se muestra como `[memory] <id> — <título>` más sus primeras líneas
(6 líneas o 500 caracteres), sin marca de truncado por ítem, y en conjunto las
memorias pueden usar como máximo el **35%** del presupuesto, para que un recall
largo no deje sin lugar al código. Una memoria no se repite como código cuando el
brief la cita literalmente.

### 2. Código

Una búsqueda híbrida de la pregunta (vector + palabras clave, sin reranker),
trayendo 30 resultados, con todo lo que hace [`search`](busqueda.md): tests, docs
y config rankean por debajo del código, los duplicados se colapsan y un
identificador de la pregunta pone su definición primero. Si el índice BM25 no está
construido, no se construye para el brief — el híbrido degrada a vector más
anclaje. Los filtros `kind` e `include_tests` aplican acá también.

La búsqueda es deliberadamente más profunda que lo que va a entrar: *el
presupuesto decide dónde parar, no el límite*. Un resultado cuyo texto una
memoria de arriba ya cita se saltea — no vale la pena pagarlo dos veces.

### 3. Registrado contra este código

Para los primeros 5 archivos que eligieron las pasadas 1 y 2, las memorias
vinculadas a esos archivos por la unión memoria↔grafo.

Esta es la pasada que justifica todo el diseño. Son memorias adheridas a
*exactamente este código* — conocimiento que un recall semántico sobre la
redacción de la pregunta nunca habría sacado a flote, porque la memoria y la
pregunta usan palabras distintas. Es para esto que existe la unión.

Cada una se etiqueta con la procedencia de su vínculo:

```
[memory · files-field · about crates/devctx-search/src/lib.rs] Rerank default
```

`files-field` y `content-mention` significan que algo conectó la memoria con
este código al escribirla. `inference` significa solo que las palabras calzan.
La etiqueta está ahí para que quien lea pueda ponderarla.

Los duplicados entre archivos se descartan por id de memoria. Solo las memorias
que entraron al brief aportan sus `files`.

## De qué repositorio

`build_context` acepta un **`project`** opcional — un nombre registrado o
cualquier ruta dentro de uno — que arma el brief desde ese repositorio solo para
esta llamada. En una sesión de grupo debe ser un *miembro del grupo*; cualquier
otra cosa se rechaza.

En una **sesión de grupo sin `project`** se acabó el comportamiento viejo (usar en
silencio el miembro por defecto del grupo). En su lugar el brief nombra de dónde
salió:

```
[devctx] context from acme-api (best match 0.71; 3 of 5 members scored; not scored: acme-web (no running server), acme-docs (different model))
[devctx] ambiguous: also acme-worker (0.69) — within 0.03 of acme-api (0.71); pass `project` to choose
```

Cómo se elige un miembro:

1. Solo se puntúan los miembros cuyo servidor **ya está corriendo**: se lee el
   `serve.json` de cada uno, se consulta `/health` (0,8 s) y se hace una búsqueda
   vectorial (10 resultados, sin reranker) por HTTP. **Nunca arranca un
   proceso.** Un miembro sin servidor queda `not scored (no running server)`; uno
   con otro modelo de embeddings (aun del mismo ancho) o cuyo checkout ya no existe
   queda `not scored` también, porque los cosenos de dos modelos no son
   comparables.
2. El puntaje es el promedio de los 3 mejores scores crudos del recuperador entre
   los resultados de *código* del miembro (todos los resultados si la pregunta no
   calza con ningún código, o si `kind` es explícito). Un miembro que *define* un
   identificador nombrado en la pregunta suma +0,03 de desempate, nunca suficiente
   para dar vuelta una ventaja clara.
3. Toda la selección comparte un **deadline de 2,5 s**; un miembro que no responde
   a tiempo queda `busy`, no es un error.
4. Una ventaja menor que 0,03 igual responde desde el mejor miembro, más la línea
   `ambiguous` que nombra a los otros. Si algunos miembros comparables no pudieron
   puntuarse, la respuesta dice `compared only k of N comparable members` y ofrece
   los que la pregunta nombra (`Likely (named by the question): api — pass
   `project=api``) y agrega una línea legible por máquina,
   `[devctx] selection: {"scored":1,"total":3,"candidates":["api"]}`, para que un
   agente reintente con `project`. La selección nunca levanta un servidor para
   mejorar su propia elección: los miembros sin puntuar siguen así hasta que su
   servidor esté arriba. Si nadie pudo puntuarse y nada falló (todos fríos),
   responde desde el miembro por defecto del grupo y dice que *no se eligió por
   relevancia*. Un miembro que responde con un error es un error, listado.
5. Una elección que comparó **a todos** los miembros comparables con ventaja clara
   se guarda en caché **10 minutos**, por pregunta normalizada y `kind` /
   `include_tests`; su encabezado dice `cached choice`. Las elecciones parciales y
   las ambiguas nunca se cachean, así que un miembro que se calentó después tiene
   su turno.

Fuera de un grupo, un proyecto vinculado responde como siempre; la línea `context
from` aparece solo en una sesión de grupo o cuando se pasa `project`.
`branch_fallback` (ver [Búsqueda](busqueda.md#conciencia-de-ramas)) se reporta como
`[devctx] branch_fallback: …`.

## El presupuesto

`--max-tokens` (default 4096) es un **corte duro**, no una meta. Los tokens se
convierten a un presupuesto de caracteres con una razón fija, y cada ítem se
verifica contra el espacio restante antes de agregarse.

Dos comportamientos vale la pena conocerlos:

**Nada se descarta en silencio.** Lo que no entró se cuenta y se nombra al
final, una sola vez:

```
[devctx] omitted: 7 item(s), reason: budget (4096 tokens). Raise max_tokens, or narrow the query.
```

**Un fragmento que no entra ya no termina el brief.** Un ítem demasiado grande
para el espacio que queda se saltea y se prueba el siguiente, así que una función
grande no esconde a las chicas relevantes que vienen detrás. Cada fragmento de
código se topa en lo mayor entre un tercio del presupuesto y 600 caracteres; un
comentario de documentación inicial de más de 6 líneas se recorta a 3 más `N doc
lines trimmed`, y se mantiene bien formado en su lenguaje (el `*/` se conserva); y
una única línea enorme se corta por caracteres con una marca que dice cuánto quedó
afuera.

Un brief que truncara calladamente se leería como "esto es todo lo que hay", que
es lo único que jamás debe significar.

**Los encabezados viajan con su primer ítem.** Un encabezado de sección se emite
pegado a la primera entrada que entra, nunca solo. Un encabezado emitido por
separado puede sobrevivir a un presupuesto que sus ítems no sobrevivieron,
dejando una sección vacía — y una sección vacía se lee como "acá no hay nada",
que es justo el mensaje equivocado cuando la verdad es "no entró".

## Forma de la salida

```
## What is already known

[memory] El reranking queda apagado por defecto
Medido 30 ms → 8.6 s y 406 MB → 2.4 GB...

## Code

// crates/devctx-search/src/lib.rs:55
pub fn search(...)

## Recorded against this code

[memory · files-field · about crates/devctx-search/src/lib.rs] El pool es el techo
Un reranker reordena lo que le entregan y nada más...
```

Las secciones sin contenido no aparecen del todo.

## Opciones

| Flag | Default | Efecto |
|---|---|---|
| `--max-tokens` | 4096 | Techo duro para todo el brief |
| `--no-memories` | apagado | Solo código — saltea las pasadas 1 y 3 |

`--no-memories` es para cuando querés recuperación cruda sin la capa de opinión.

La tool MCP además acepta `project`, `kind` e `include_tests` (e
`include_memories` para `--no-memories`); el CLI no.

## Modelo mental

`search` responde *"¿dónde está el código?"*. `recall` responde *"¿qué
sabemos?"*. `build_context` responde la pregunta que el agente realmente tiene,
que es **"¿qué debería haber leído antes de responder esto?"** — y la responde
dentro de un techo que vos ponés, diciéndote con honestidad qué dejó afuera.
