# Integración MCP

> 🇬🇧 [Read in English](../../03-core-concepts/mcp-integration.md)

DevCtxEngine expone sus capacidades como herramientas sobre el
[Model Context Protocol](https://modelcontextprotocol.io/), así que cualquier
cliente MCP — Claude Code, Claude Desktop, Cursor — puede usarlas sin trabajo
por cliente.

---

## Configuración

```bash
devctx mcp configure                          # Claude Code, alcance de proyecto
devctx mcp configure --client cursor
devctx mcp configure --client claude-desktop --scope global
devctx mcp configure --remove
```

| Cliente | Escribe en |
|---|---|
| `claude-code` | `.mcp.json` (proyecto) o `~/.claude.json` (global) |
| `claude-desktop` | `claude_desktop_config.json` (solo global) |
| `cursor` | `.cursor/mcp.json` |

`--name` cambia la clave bajo `mcpServers` (default `devctx`).

## Transporte

**stdio.** El cliente lanza `devctx mcp` como proceso hijo y habla JSON-RPC 2.0
por stdin/stdout. Sin HTTP, sin puertos, sin autenticación — el límite de
confianza es el límite del proceso.

El servidor corre enteramente en proceso: parseo, chunking, embeddings y
reranking son Rust, en el mismo binario. No hay sidecar ni un segundo runtime.

## Vinculación al proyecto

**Un servidor MCP registrado globalmente arranca en el directorio desde el que
se lanzó el cliente.** Rara vez es el repositorio que querías, y muchas veces no
es ningún repositorio — así que el servidor resuelve uno, en este orden:

| | Regla | Cómo se ve |
|---|---|---|
| 1 | `--project <ruta>` | Nombraste una raíz. Nada la sobreescribe. |
| 2 | Buscar hacia arriba | El directorio de trabajo está dentro de un repositorio. |
| 3 | **Descender por el registry** | El directorio de trabajo *contiene* proyectos registrados: una raíz de workspace. |
| 4 | Sin vincular | Nada coincidió; el error dice qué encontró y por qué no alcanzó. |

La regla 3 es la que hace funcionar un workspace multi-repositorio. Arrancando
desde un directorio que contiene varios proyectos registrados, el servidor
vincula:

- **un proyecto**, si solo uno vive adentro;
- **el grupo entero**, si todos declaran el mismo `project.group`.

```
$ cd ~/trabajo/acme && devctx mcp
Bound to group ACME (11 projects, default acme-api) — resolved from /home/vos/trabajo/acme
```

Vinculado a un grupo, `remember` usa `scope: group` por defecto en vez de
`local`: la sesión está atada a un producto, y `local` enterraría la memoria en
el miembro que el descenso eligió. Un `scope` explícito siempre gana.

Los proyectos que comparten directorio pero no grupo dejan al servidor sin
vincular **a propósito** — elegir uno sería adivinar. Dales el mismo
`project.group` y el descenso se encarga.

### Trabajar entre repositorios

El directorio de trabajo del servidor queda fijo cuando arranca el proceso y no
cambia nunca, así que una vinculación resuelta al inicio no puede seguirte
mientras te movés entre repositorios. Para eso, las herramientas de código toman
un `project` opcional: el nombre de un proyecto registrado, o cualquier ruta
adentro de uno.

```
search(query: "política de reintentos", project: "acme-worker")
search(query: "política de reintentos", project: "~/trabajo/acme/acme-worker/src/main.rs")
read_file(path: "~/trabajo/acme/acme-web/src/app.ts")   # se resuelve de la ruta misma
```

Resuelve **solo esa llamada** y nunca cambia la vinculación de la sesión. Cuando
la respuesta vino de un proyecto inferido y no de uno que nombraste, el
resultado trae `resolved_project` para que sepas qué repositorio contestó.

`use_project` sigue existiendo, y ahora es lo que su nombre dice: un override
explícito que mueve la sesión, útil cuando un tramo largo de trabajo vive en un
solo lugar.

## Las herramientas

23 herramientas, agrupadas por lo que responden.

### Código

| Herramienta | Responde |
|---|---|
| `search` | *¿Dónde está el código sobre X?* Modos: `vector` (default), `keyword` (BM25), `hybrid` (RRF) |
| `read_file` | El archivo, opcionalmente un rango de líneas inclusivo desde 1 |
| `read_symbol` | La definición de un símbolo, su código, archivo, rango y tipo — cuando sabés el nombre |
| `get_references` | Todos los sitios de llamada de un símbolo |
| `impact_analysis` | Llamadores transitivos (radio de impacto) y llamados |
| `summarize` | Texto reducido a ~`max_tokens`, extractivo por defecto para que sobrevivan los identificadores |

La distinción entre `search` y `read_symbol` conviene interiorizarla:
`read_symbol` cuando sabés el nombre y querés la cosa misma, `search` cuando
querés código *sobre una idea*.

### Rutas

| Herramienta | Responde |
|---|---|
| `search_routes` | Rutas HTTP por método y/o subcadena de path |
| `routes_for_handler` | Las rutas que sirve un símbolo manejador |

Frameworks reconocidos: FastAPI, Flask, Express, NestJS, Spring, Quarkus,
Angular.

### Memoria

| Herramienta | Responde |
|---|---|
| `remember` | Guardar decisión/insight/nota/bug, deduplicado por tema o contenido |
| `recall` | Memorias relevantes a una consulta, en todos los niveles, etiquetadas con su origen |
| `memory_context` | Las memorias más recientes, *sin consulta* — para recuperarse tras un reset, cuando todavía no sabés qué preguntar |
| `memories_by_symbol` | Por qué este símbolo es como es — lo que el grafo de llamadas no puede responder |
| `memories_by_file` | Lo mismo, para un archivo |
| `memory_refs` | La inversa: dado un id de memoria, los símbolos y archivos que le conciernen |
| `memory_stats` | Conteos, total y por tipo |
| `memory_forget` | Borrar una permanentemente. No reversible. |
| `memory_move` | Mover entre niveles, o a otro proyecto. El id cambia. |
| `build_context` | Un brief con presupuesto: lo conocido + código + lo registrado contra ese código |

### Proyectos e indexado

| Herramienta | Responde |
|---|---|
| `list_projects` | Cada repositorio rastreado: nombre, ruta, modelo, frescura del índice |
| `use_project` | Vincular esta sesión a un proyecto |
| `search_project` | Buscar en *otro* proyecto registrado por nombre |
| `index_repo` | Indexar: git diff → parse → chunk → embed → store |
| `index_status` | Último commit indexado y conteos para este repo y rama, y si está al día |

`search_project` es para cuando la respuesta vive en un repositorio distinto del
que estás trabajando — la pregunta de backend que te salta editando el frontend.

## Formas de retorno

La mayoría devuelve **JSON**. Dos no:

- `build_context` devuelve **prosa**, porque el resultado está pensado para
  leerse directo al contexto de un modelo y un sobre JSON gastaría presupuesto
  en puntuación.
- `summarize` devuelve texto.

Los resultados de memoria-por-código (`memories_by_symbol`, `memories_by_file`,
`memory_refs`) siempre llevan `link_sources`, y está ahí a propósito:
`files-field` y `content-mention` significan que algo conectó esa memoria con
ese código al escribirla, mientras que `inference` significa solo que las
palabras coinciden. Quien evalúe cuánto confiar en un vínculo necesita esa
distinción.

## Descubrimiento

En `tools/list` el servidor devuelve las 23 definiciones con parámetros en JSON
Schema, declaradas de entrada. El cliente valida los argumentos antes de cada
llamada. No hay paso de descubrimiento en tiempo de ejecución.

## Otras interfaces

El mismo motor se alcanza de otras cuatro formas, todas leyendo el mismo store:

```bash
devctx tui        # UI interactiva de terminal: búsqueda, grafo, memorias
devctx web        # tablero en navegador: grafo de llamadas + memorias
devctx api        # API REST HTTP
devctx serve      # servidor de larga vida que posee la DB; los demás comandos rutean a él
```

`serve` importa para la concurrencia: DuckDB permite un solo escritor por
archivo, así que un servidor de larga vida posee la base de datos y el CLI rutea
a través de él en vez de pelear por el lock.
