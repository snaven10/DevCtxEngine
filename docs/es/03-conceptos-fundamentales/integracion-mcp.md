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

Parseo, chunking, embeddings y reranking son Rust, en el mismo binario — no hay
sidecar ni un segundo runtime. Pero **`devctx mcp` nunca abre la base de
datos.** Es un cliente delgado y perezoso de `devctx serve`, el único proceso
que posee el archivo DuckDB del proyecto; ver
[Ciclo de vida](#ciclo-de-vida-y-fiabilidad).

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

## Ciclo de vida y fiabilidad

DuckDB permite un escritor por archivo. Si cada sesión MCP abriera la base por
su cuenta, una sesión vieja bastaría para dejar afuera a todos los demás
procesos del repositorio hasta que alguien la matara a mano. Por eso el MCP no
posee nada:

- **Perezoso.** El MCP se conecta al servidor del proyecto en la primera
  llamada que lo necesita, y lo levanta en segundo plano si no hay ninguno. No
  retiene ningún `Store` entre llamadas y nunca cae a abrir el archivo. Si no
  alcanza ningún servidor, la tool falla con `no devctx server for <proyecto>:
  <causa>`, y la causa sale de `serve.log`, junto a la base.
- **Falla rápido ante un lock.** Si otro proceso retiene la base, `devctx` nombra
  el PID, su línea de comando y su antigüedad, y el remedio, en menos de dos
  segundos: `the index of <proyecto> is held by PID N (…). If it is a devctx mcp
  from an old session, close that session; otherwise devctx serve --stop.`
- **Se reconecta.** Tras `devctx serve --stop`, una salida por inactividad o un
  crash, la siguiente llamada levanta un servidor nuevo y sigue — `remember`
  incluido. Una llamada se reintenta **solo cuando es seguro que nunca llegó al
  servidor** (conexión rechazada o imposible de establecer) o cuando el servidor
  respondió `503` con el encabezado `X-Devctx-Exiting` (estaba saliendo y no
  procesó nada). Un timeout de lectura, una conexión cortada tras enviar o
  cualquier otro status HTTP nunca se reintenta, así que una escritura jamás se
  hace dos veces. Tras una reconexión fallida, el fallo se recuerda 3 segundos.
- **Sale cuando sale su cliente.** Cerrar stdin termina el proceso: en unos seis
  segundos aun con una tool en vuelo (rmcp drena respuestas hasta 5 s y luego el
  runtime tiene 1 s).
- **Reporta un binario desactualizado en vez de morir.** `index_status` y
  `list_projects` llevan
  `"mcp": {"version": "0.9.0", "binary_replaced": false}`. Si reinstalás
  `devctx` con una sesión abierta, `binary_replaced` pasa a `true` y el objeto
  suma `installed_version` y un `hint`: *reiniciá la sesión de IA para tomarlo*.
  El proceso viejo sigue funcionando con el código con el que arrancó — nada se
  reinicia solo, así que **reiniciá tus sesiones de Claude / Cursor después de
  actualizar.**

### El servidor con el que habla

`devctx serve` (auto-lanzado con `--idle 900`) es el dueño de la base de un
proyecto. Con qué podés contar:

| | Comportamiento |
|---|---|
| **Salida por inactividad** | Tras la ventana sin requests congela las escrituras, hace checkpoint del WAL y se va. Un índice en curso cuenta como actividad solo mientras sigue avanzando de archivo (`DEVCTX_INDEX_STALL_SECS`, 900 por defecto); uno atascado no mantiene vivo al servidor. |
| **Cierre ordenado** | SIGTERM cancela un índice en curso entre archivos (nada queda a medias: cada archivo es una transacción y el commit registrado no avanza), hace checkpoint y sale. `DEVCTX_SHUTDOWN_GRACE_SECS` solo puede acortar los 3 s de gracia. |
| **Mientras sale** | Los requests reciben `503` + `X-Devctx-Exiting: 1` *antes* de procesar nada, así que un cliente puede repetirlos sin riesgo contra el siguiente servidor. `serve.json` se mantiene hasta que termina el checkpoint final, y así no arranca un segundo servidor contra el lock. |
| **Proyecto desaparecido** | Si se borra el directorio del proyecto, el servidor sale solo (`DEVCTX_VANISH_POLL_MS`, 10 s por defecto, tres sondeos seguidos de "no existe"). |
| **Carga de modelos** | Una descarga de modelo sin progreso durante 120 s (`DEVCTX_MODEL_STALL_SECS`; `0` la desactiva) falla con un error que pide reiniciar el servidor, en vez de colgarse. |
| **Memoria** | El embedding corre un lote a la vez, de 8 textos (`DEVCTX_EMBED_BATCH_SIZE`); un modelo sin uso se descarga a los 300 s (`DEVCTX_MODEL_IDLE_SECS`). |

`devctx serve --stop` manda SIGTERM y espera a que el proceso **salga** de
verdad, no solo a que cambie su `cmdline`: retiene el SIGKILL mientras un índice
se cancela (hasta 65 s) o corre el checkpoint final. Solo señala a un proceso
del que puede probar que es el servidor devctx de este proyecto:

| Plataforma | Cómo se verifica la propiedad |
|---|---|
| Linux | `pidfd` (sin carrera de reutilización de PID) más `/proc/<pid>/{exe,cmdline,stat}`; el start time guardado en `serve.json` debe coincidir |
| macOS | `ps -ww -o lstart=,command=` (UTC) y `lsof` para el directorio de trabajo; la espera de salida usa `kqueue` |
| Windows | **No soportado**: `--stop` no puede verificar ni señalar el proceso y lo dice; no borra `serve.json` |

Un PID que no se puede verificar nunca se señala y su `serve.json` se conserva;
`serve --stop` sale entonces con código distinto de cero y las instrucciones
manuales (`kill <pid>` y borrar el archivo).

## Las herramientas

24 herramientas, agrupadas por lo que responden.

### Código

| Herramienta | Responde |
|---|---|
| `search` | *¿Dónde está el código sobre X?* Modos: `vector` (default), `keyword` (BM25), `hybrid` (RRF). Filtros: `language`, `kind`, `include_tests`; ver [Búsqueda](busqueda.md) |
| `read_file` | El archivo, opcionalmente un rango de líneas inclusivo desde 1 |
| `read_symbol` | La definición de un símbolo, su código, archivo, rango y tipo — cuando sabés el nombre. Si no la encuentra responde con `suggestions` y, para llamadas a librerías, `external: true` |
| `get_references` | Cada sitio de llamada de un símbolo, uno por ocurrencia, con `confidence` y `via`; `min_confidence: "low"` agrega las ambiguas, `kinds` otras relaciones (ver [Grafo de símbolos](grafo-de-simbolos.md#get_referencessímbolo--quién-llama-a-esto)) |
| `impact_analysis` | Llamadores transitivos (radio de impacto) y llamados, por profundidad, cada uno con `confidence` y `via`; por defecto solo llamadas high/medium, sin tests, sin llamados de librerías, 200 por dirección — lo que eso deja afuera se cuenta; las llamadas por una interfaz, clase abstracta o trait se siguen como `via: "dispatch"` (nunca `high`). `min_confidence`, `include_tests`, `include_external`, `max_nodes`, `dispatch` (ver [Grafo de símbolos](grafo-de-simbolos.md#impact_analysissímbolo--radio-de-impacto)) |
| `summarize` | Texto reducido a ~`max_tokens`, extractivo por defecto para que sobrevivan los identificadores |

La distinción entre `search` y `read_symbol` conviene interiorizarla:
`read_symbol` cuando sabés el nombre y querés la cosa misma, `search` cuando
querés código *sobre una idea*.

### Rutas

| Herramienta | Responde |
|---|---|
| `search_routes` | Rutas HTTP por método y/o subcadena de path, 20 por página |
| `routes_for_handler` | Las rutas que sirve un símbolo manejador |

Frameworks reconocidos: FastAPI, Flask, Express, NestJS, Spring, Quarkus,
Angular.

### Memoria

| Herramienta | Responde |
|---|---|
| `remember` | Guardar decisión/insight/nota/bug, deduplicado por tema o contenido |
| `recall` | Memorias relevantes a una consulta, en todos los niveles, etiquetadas con su origen |
| `memory_context` | Las memorias más recientes, *sin consulta* — para recuperarse tras un reset, cuando todavía no sabés qué preguntar |
| `memories_by_symbol` | Por qué este símbolo es como es — lo que el grafo de llamadas no puede responder. 5 por página, `content` recortado a 600 caracteres (`full: true` lo levanta) |
| `memories_by_file` | Lo mismo, para un archivo — más `plan_tasks`, las tasks de plan (de `plans/`) que lo mencionan |
| `memory_refs` | La inversa: dado un id de memoria, los símbolos y archivos que le conciernen |
| `memory_stats` | Conteos, total y por tipo |
| `memory_forget` | Borrar una permanentemente. No reversible. |
| `memory_move` | Mover entre niveles, o a otro proyecto. El id cambia. |
| `build_context` | Un brief con presupuesto: lo conocido + código + lo registrado contra ese código. Acepta `project`; en un grupo elige el miembro que mejor calza (ver [Constructor de contexto](constructor-de-contexto.md)) |

### Planes

| Herramienta | Responde |
|---|---|
| `plan_status` | Progreso de los planes en `plans/` (markdown, fuente de verdad): sin argumento lista planes — 25 por página, el más nuevo primero, `active_only: true` deja solo los que tienen tasks sin resolver — y nombra el activo; con `plan`, las tasks listas/en curso/bloqueadas de ese plan |

Los planes viven como markdown escrito a mano en git, nunca se copian a la
base: `plan_status` lee `plans/PLAN-*/` en cada llamada — del proyecto, o de la raíz
del workspace cuando los repositorios de un producto viven bajo un directorio
que guarda el `plans/` compartido (el `plans_root` del resultado dice cuál y
por qué; el orden está en [el flujo de trabajo del agente](../04-flujo-de-trabajo-del-agente.md#de-dónde-se-lee-plans)).
El formato de las tasks es tolerante — `Status:`/`Estado:`, sinónimos, un
estado `skipped` — y también está documentado ahí. La tabla de progreso
de un plan y los archivos de sus tasks pueden discrepar (ambos se escriben a
mano) — cuando pasa, el archivo de la task manda y la discrepancia se reporta
como warning, nunca se elige en silencio. Llamala justo después de compactar
para recuperar qué task está lista para arrancar.

### Proyectos e indexado

| Herramienta | Responde |
|---|---|
| `list_projects` | Cada repositorio rastreado: nombre, ruta, modelo, frescura del índice |
| `use_project` | Vincular esta sesión a un proyecto |
| `search_project` | Buscar en *otro* proyecto registrado por nombre |
| `index_repo` | Indexar: git diff → parse → chunk → embed → store |
| `index_status` | Último commit indexado y conteos para este repo y rama, si está al día (`up_to_date`), si el extractor es más viejo que este binario (`extractor_stale`) y el bloque de versión `mcp` |

`search_project` es para cuando la respuesta vive en un repositorio distinto del
que estás trabajando — la pregunta de backend que te salta editando el frontend.

## Formas de retorno

La mayoría devuelve **JSON**. Dos no:

- `build_context` devuelve **prosa**, porque el resultado está pensado para
  leerse directo al contexto de un modelo y un sobre JSON gastaría presupuesto
  en puntuación.
- `summarize` devuelve texto.

### Las tools de listas siempre responden con un objeto

`search`, `search_project` y el `search` de grupo responden
`{"results": [...]}`; `search_routes` y `routes_for_handler` responden
`{"routes": [...]}`. **Nunca un arreglo pelado** — las versiones anteriores
respondían un arreglo cuando no había nada que decir y un objeto en caso
contrario, y eso obligaba a cada cliente a adivinar. Junto a las filas puede
aparecer cualquiera de estos:

| Campo | Significado |
|---|---|
| `omitted` | `{count, reason, next_offset?}` — lo que quedó afuera. `reason` es `limit` (el tamaño de página) o `budget` (el presupuesto de salida; gana si cortaron los dos). Ausente significa que no se dejó nada afuera. (`omitted_for_budget`, que además nombra lo descartado, se mantiene un release más.) |
| `total`, `next_offset` | `search_routes` y `plan_status`: filas que coincidían antes de paginar, y el `offset` de la página siguiente (ausente en la última) |
| `branch_fallback` | `{current, used, why}` — la respuesta salió de otra rama; ver [Búsqueda](busqueda.md#conciencia-de-ramas) |
| `warning` | El índice lo produjo un extractor más viejo; reindexá con `devctx index --full` |

Los resultados de búsqueda llevan `score`, `file`, `start_line`, `end_line`,
`symbol`, `symbol_type`, `level`, `language`, `kind` (`code` / `test` / `doc` /
`config`), `text`, más `raw_score` (solo cuando una etapa posterior reescribió
`score`: el score propio del recuperador) y `anchored: true` (la definición de
un identificador nombrado en la consulta). `devctx search --format json`
imprime el mismo objeto.

### Paginación

| Herramienta | Página por defecto | Parámetros |
|---|---|---|
| `search_routes` | 20 rutas | `limit`, `offset` |
| `plan_status` (listado) | 25 planes, el más nuevo primero | `limit`, `offset`, `active_only` |
| `memories_by_symbol`, `memories_by_file` | 5 memorias | `limit`, `offset`, `full` |
| `search` | top-k según `limit` (10); no pagina | `limit` |

Pasá el `next_offset` de la respuesta anterior como `offset` para continuar. Una
memoria cuyo `content` se recortó lleva `content_truncated: true` y
`content_chars`; su `id` trae el resto (`full: true`).

### `project`

Las tools que actúan sobre código aceptan un `project` opcional (nombre
registrado, o cualquier ruta dentro de uno) que resuelve esa llamada y nunca
cambia la vinculación — ahora también `build_context`, `memories_by_symbol` y
`memories_by_file`. En una sesión de grupo, `build_context` además exige que sea
miembro del grupo.

Los resultados de memoria-por-código (`memories_by_symbol`, `memories_by_file`,
`memory_refs`) siempre llevan `link_sources`, y está ahí a propósito:
`files-field` y `content-mention` significan que algo conectó esa memoria con
ese código al escribirla, mientras que `inference` significa solo que las
palabras coinciden. Quien evalúe cuánto confiar en un vínculo necesita esa
distinción. `files-field` también se reporta cuando falta la fila de unión pero
los `files` de la memoria nombran el archivo.

## Descubrimiento

En `tools/list` el servidor devuelve las 24 definiciones con parámetros en JSON
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
archivo, así que un servidor de larga vida posee la base de datos y el CLI — y el
MCP — rutean a través de él en vez de pelear por el lock.

## Actualizar a 0.9

- **Reiniciá tus sesiones de IA.** Un `devctx mcp` en marcha conserva el código
  con el que arrancó; te lo avisa en `mcp.binary_replaced`.
- **Las versiones mezcladas son un estado de transición, no una
  configuración.** Un MCP 0.8.4 contra un servidor 0.9 no puede enviar los
  parámetros nuevos (`kind`, `include_tests`, `offset`, `full`, `project` en
  `build_context`) y retransmite las formas de objeto nuevas a un cliente que
  quizá todavía espera arreglos. En el sentido inverso, un MCP 0.9 envuelve en
  el objeto nuevo el arreglo pelado que responde un servidor viejo, pero ese
  servidor ignora `kind` e `include_tests`. Detené los servidores viejos con
  `devctx serve --stop` después de actualizar.
- **Cambiaron las formas JSON** (objetos, no arreglos; `results`, no `hits`).
  Ver el [changelog](../../../CHANGELOG.md).
- **El primer `devctx index` tras actualizar** reconcilia los excludes por
  defecto nuevos — ver [Mantener el índice al día](../13-mantener-el-indice-al-dia.md).
