# Memoria

> 🇬🇧 [Read in English](../../03-core-concepts/memory.md)

Conocimiento que sobrevive a la sesión que lo produjo — y que se puede volver a
encontrar desde el código del que habla.

---

## Qué es

Una memoria es una nota corta, tipada y con alcance: una decisión y su
razonamiento, la causa raíz de un bug, un detalle traicionero que le costó una
tarde a alguien.

```bash
devctx remember "El reranking queda apagado por defecto: medido 30ms → 8.6s" \
  --type decision \
  --topic search-rerank-default \
  --files crates/devctx-core/src/config.rs

devctx recall "por qué está apagado el reranking"
```

Los agentes usan las herramientas MCP `remember` y `recall`, más
`memories_by_symbol`, `memories_by_file`, `memory_refs`, `memory_context`,
`memory_stats`, `memory_forget` y `memory_move`.

## Por qué existe

El historial de chat es una pizarra: útil durante la reunión, borrada después.
Los comentarios en el código son notas adhesivas — anotan un punto pero no
pueden sostener una decisión que abarca un sistema. Ninguno sobrevive a la
sesión siguiente, así que la misma pregunta se vuelve a responder, y a veces se
responde *distinto*.

La memoria es el cuaderno: buscable por significado, deduplicada, y — la parte
que más importa — alcanzable desde el código.

## Los tres niveles

| Alcance | Guardado bajo | Visible desde |
|---|---|---|
| `local` | El store del propio proyecto | Solo este repositorio |
| `group` | Store central, `@group:<nombre>` | Todo repo que comparta `project.group` |
| `global` | Store central, `@global` | Todo proyecto de la máquina |

`--scope all` (el default de `recall`) busca en cada nivel que aplique y fusiona
los resultados por rango.

### Por qué las filas de grupo y globales se re-llavean

La identidad de una memoria se deriva de su `project` más el hash de su
contenido. Si una fila global conservara el proyecto que la aportó, la *misma*
lección aprendida en dos repositorios caería como dos filas — la deduplicación
fallando justo donde más importa.

Así que las filas globales llevan todas el proyecto reservado `@global`, y las
de grupo llevan `@group:<nombre>`. El repositorio que la aportó queda en el
campo `repo` como procedencia. El llaveo por grupo mantiene el conocimiento
compartido de cada producto en su propio espacio: la deduplicación sigue
colapsando la misma lección de dos repos hermanos, sin filtrarla a proyectos no
relacionados como sí haría `@global`.

## Deduplicación

La escritura pasa por un solo camino, y o inserta o revisa:

- **Con `--topic`** — upsert por clave de tema. Guardar de nuevo bajo
  `search-rerank-default` revisa esa memoria en vez de agregar una segunda. Así
  es como una memoria se mantiene vigente en lugar de acumular versiones
  contradictorias.
- **Sin `--topic`** — la identidad cae al hash de contenido sobre el texto
  normalizado (minúsculas, espacios colapsados). Guardar lo mismo dos veces no
  hace nada.

Usá clave de tema para todo lo que esperás revisar. Usá contenido pelado para
observaciones de una sola vez.

## La unión memoria↔grafo

Esta es la parte que distingue a esta memoria de un archivo de notas buscable.

**Pasá `--files`.** Es el campo de mayor apalancamiento:

```bash
devctx remember "..." --files crates/devctx-search/src/lib.rs
```

Con él, la memoria se vuelve encontrable desde cada símbolo de esos archivos —
`memories_by_symbol` responde *"¿qué se decidió sobre `search()`?"* antes de que
tengas las palabras para formular un `recall`. Sin él, la memoria solo se
encuentra por texto, lo que exige saber ya qué preguntar.

### Dónde vive la fila de unión

El grafo de llamadas es por repositorio y vive en el store del proyecto. Una
memoria global o de grupo vive en el central. Una memoria sobre el `charge()` de
este repositorio tiene que ser encontrable desde `charge()` sin importar cuál de
los dos guarde su texto.

Por eso la fila de unión va siempre al store del **proyecto** — al lado del
grafo al que apunta — llevando solo el id de la memoria. Resolver ese id busca
primero localmente y cae al store central. Copiar el texto de la memoria a cada
proyecto que la menciona haría que una edición en un lugar dejara copias rancias
en todos los demás.

### Procedencia del vínculo

Cada resultado lleva `link_sources`, y la distinción es estructural:

| Valor | Significado |
|---|---|
| `files-field` | El campo `files` de la memoria nombró este archivo. Estructural. |
| `content-mention` | La prosa de la memoria nombró este archivo, y el archivo está indexado. Estructural. |
| `inference` | Solo coinciden las palabras. Más débil. |

Los dos primeros significan que algo conectó esta memoria con este código al
momento de escribirla. `inference` significa solo que el texto casualmente
calza. Quien evalúe si confiar en un vínculo debería leer este campo.

### Barrido de memorias viejas

Las memorias escritas antes de que existiera la unión — migradas, importadas o
guardadas por un build anterior — no traen vínculos:

```bash
devctx memories backfill-links --dry-run
devctx memories backfill-links
```

Hay una pasada derivada del texto para memorias sin `files` del todo, que es
como la mitad de un corpus real. Arma una lista de *candidatos* con las rutas de
archivo nombradas en la prosa y sobre-empareja a propósito: el mismo patrón que
encuentra `apps/registry/src/app/components/firmar-registro.ts` también
encuentra `Shepherd.js`, que es una librería que nadie indexó, y `CLAUDE.md`,
que no es código. Cada candidato se verifica contra el índice antes de escribir
un vínculo. **El índice es lo que los distingue, nunca el patrón.**

## Recall

```bash
devctx recall "por qué está apagado el reranking" --limit 5 --scope all
```

La recuperación trae un pool más profundo que el límite (`limit × 8`, mínimo 40)
de cada nivel aplicable, luego fusiona las listas por rango y deduplica por id
de memoria. `--repo <nombre>` acota los resultados globales a un repositorio
contribuyente.

## Administrar memorias

```bash
devctx memory-stats                       # conteos de este proyecto
devctx memory-forget <id>                 # borrar una, viva donde viva
devctx memories export > memories.jsonl   # un objeto JSON por línea
devctx memories import memories.jsonl     # solo agrega, nunca pisa
devctx memory-purge <clave-proyecto>      # borrar todas las de una clave
```

`memory_move` (MCP) promueve una memoria entre niveles — una lección que resulta
aplicar más allá de un repositorio pasa a `group` o `global` sin reescribirla.

Borrar importa tanto como escribir. Una memoria que registra una causa raíz que
resultó equivocada es peor que no tener memoria, porque se va a recuperar con
confianza.

## Modelo mental

Tres preguntas, tres herramientas:

- *"¿Qué sabemos de X?"* → `recall`. Necesita que tengas las palabras.
- *"¿Qué se decidió sobre **esta** función?"* → `memories_by_symbol`. Funciona
  cuando estás parado sobre el código y todavía no tenés las palabras.
- *"¿Qué debería saber antes de responder esto?"* → `build_context`, que arma
  código y memorias en un solo brief con presupuesto.

La segunda es la razón por la que `--files` no es opcional en la práctica.
