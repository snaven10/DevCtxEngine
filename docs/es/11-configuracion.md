> 🌐 [English version](../11-configuration.md)

# Configuración

Hay dos archivos de configuración. El del proyecto describe un repositorio; el
central describe esta máquina.

---

## 1. Config del proyecto — `.devctx/config.yaml`

Lo escribe `devctx init` (o `devctx projects add --init`) en la raíz del
repositorio. Se encuentra subiendo directorios desde donde estés, así que
funciona desde cualquier subdirectorio.

```yaml
project:
  name: miproy                 # el nombre por el que los agentes se refieren a él
  path: /home/tu/code/miproy   # raíz absoluta del repositorio
  group: ''                    # repos de un mismo producto que comparten memorias

state_dir: ''                  # vacío => .devctx/state/ dentro del repo
language: en                   # en | es — idioma de la UI y los resúmenes

embeddings:
  provider: local              # local | openai | voyage | custom
  model: minilm-l6             # clave del registro; ver docs/09
  model_dir: ''                # directorio de un modelo ONNX propio
  offline: auto                # auto | "true" | "false"

storage:
  db_path: ''                  # vacío => {state_dir}/index.duckdb
  hnsw: true                   # índice vectorial aproximado (requiere la extensión VSS)
  metric: cosine               # cosine | ip — ip exige vectores normalizados
  fts: false                   # índice BM25 de palabras clave (requiere la extensión FTS)

indexing:
  exclude: []                  # patrones estilo .gitignore; ver docs/13
  branches: []                 # ramas rastreadas; vacío => la que esté en checkout

reranking:
  enabled: false               # opt-in; ver docs/08 ADR-15 para las mediciones
  model: bge-base              # bge-base | bge-v2-m3 | jina-turbo | custom
  model_dir: ''                # tu propio cross-encoder ONNX
  pool: 100                    # candidatos que ve el cross-encoder

summarization:
  provider: extractive         # extractive | openai | noop
  require_local: true          # bloquea providers no locales
  target_tokens: 200
  model: gpt-4o-mini           # para providers de API
```

**Dónde acaba la base de datos.** Manda `storage.db_path`; luego
`{state_dir}/index.duckdb`; luego `.devctx/state/index.duckdb` bajo la ruta del
proyecto. `devctx init` deja ambos vacíos, así que el índice vive dentro del
repositorio — y escribe `.devctx/.gitignore` con `state/` para que no se
commitee. La config que hay al lado sí merece la pena trackearla.

**Cambiar el modelo de embedding** cambia el ancho de los vectores, que queda
fijado al crear la base de datos. El indexado detecta el desajuste y reindexa
desde cero en vez de corromper el store.

## 2. Config central — `~/.config/devctx/config.yaml`

De toda la máquina. Se escribe con los valores por defecto la primera vez que
algo toca el store central. Referencia completa en
[El store central §6](12-store-central.md#6-configuración).

```yaml
memory:
  provider: local
  model: minilm-l6       # fija el espacio vectorial de la memoria global — una
                         # restricción, no un default: no puede variar por proyecto
defaults:                # lo que `projects add --init` escribe en un proyecto nuevo
  embeddings:
    provider: local
    model: minilm-l6
  reranking:
    enabled: false      # opt-in; ver ADR-15 para las mediciones
    model: bge-base
reindex:
  every_seconds: 0       # barrido en segundo plano; 0 = apagado
```

**Precedencia** para todo lo que ambos archivos pueden expresar:

```
.devctx/config.yaml  ›  defaults centrales  ›  defaults del binario
```

Los `defaults` centrales son un punto de partida, copiado a la config de un
proyecto al crearlo. Editarlos después no cambia los proyectos existentes — edita
la config del proyecto y luego `devctx projects refresh <nombre>` para actualizar
la copia del registro.

## 3. Variables de entorno

| Variable | Efecto |
|---|---|
| `DEVCTX_HOME` | Reubica el store central *y* la config bajo un único directorio. Sobre todo para tests y CI. |
| `DEVCTX_MODEL_CACHE` | Dónde se cachean los modelos descargados. Por defecto: `{dir de datos}/models`. |
| `DEVCTX_NO_AUTOSERVE` | Nunca levantar un servidor automáticamente; los comandos abren el store directamente. |
| `DEVCTX_API_TOKEN` | Token Bearer que exigen `serve` / `api` en todas las rutas salvo `/health`. |
| `DEVCTX_MODEL_DIR` | Directorio de un modelo ONNX propio. `embeddings.model_dir` manda sobre ella. |
| `DEVCTX_EMBED_ENDPOINT` | URL base para el provider de embeddings `custom`. |
| `DEVCTX_EMBED_DIMENSION` | Ancho de vector para el provider `custom`, que no está en el registro. |
| `DEVCTX_EMBED_MAX_CHARS` | Caracteres por texto que se le pasa al encoder. Default `4096`; `0` lo desactiva. Bajalo (ej. `2048`) en una máquina justa — ataca el relleno del lote, que es de donde viene el pico de memoria. |
| `DEVCTX_EMBED_BATCH_SIZE` | Textos por lote del encoder. Default `32`. |
| `DEVCTX_DB_MEMORY_LIMIT` | Presupuesto de memoria de DuckDB por proceso, cualquier literal de tamaño de DuckDB. Default `2GB`. |
| `DEVCTX_DB_THREADS` | Hilos de trabajo de DuckDB. Default `4`. |
| `DEVCTX_MODEL_IDLE_SECS` | Cuánto se mantiene cargado un modelo sin uso. Default `300`; `0` lo mantiene mientras viva el proceso. |
| `DEVCTX_MAX_OUTPUT_TOKENS` | Tope de un `read_file` completo sin rango de líneas. Default `8000`; `0` lo desactiva. |
| `DEVCTX_NO_UPDATE_CHECK` | Optar por no hacer la verificación de releases en segundo plano. |
| `DEVCTX_LANG` | Idioma del resumen agrupado de `--help` (`en` / `es`). |
| `OPENAI_API_KEY` / `VOYAGE_API_KEY` | Credenciales de los providers de embeddings por API. |

Se respetan `$XDG_DATA_HOME` y `$XDG_CONFIG_HOME` si están definidas.

### Por qué existen los límites de la base

DuckDB por defecto toma el 80% de la memoria del sistema. Eso es correcto para
un proceso en una máquina y está mal acá, porque cada proyecto tiene su propio
store: tres servidores en una laptop de 16 GB son 38 GB de intención, y el OOM
killer del kernel llega mucho antes de que DuckDB sienta presión.

Peor: ese killer no elige al proceso glotón — elige el `oom_score` más alto, que
en una sesión systemd suele ser los servicios de escritorio del propio usuario.
El síntoma visible es un panel muerto y ventanas cerradas, no una consulta
lenta. Un presupuesto modesto por proceso cuesta un volcado a disco en las
consultas más grandes y compra una máquina que sigue usable.

`DEVCTX_UPDATE_AVAILABLE` también existe, pero la pone el CLI *para* sus propios
subprocesos, no vos.


## 4. Registrarlo en un cliente de IA

```bash
devctx mcp configure --client claude-code --scope project
devctx mcp configure --client cursor --scope global
devctx mcp configure --client claude-desktop --scope global
devctx mcp configure --client claude-code --remove
devctx mcp configure --client cursor --show      # imprime sin escribir
```

| Cliente | Ámbito proyecto | Ámbito global |
|---|---|---|
| `claude-code` | `.mcp.json` | `~/.claude.json` |
| `cursor` | `.cursor/mcp.json` | `~/.cursor/mcp.json` |
| `claude-desktop` | — | `claude_desktop_config.json` |

La entrada se escribe dentro de `mcpServers` junto a lo que ya hubiera.
`--env CLAVE=VALOR` (repetible) añade entradas de entorno.

Los archivos de ámbito proyecto caen dentro del repositorio — decide si los
quieres commitear antes de hacerlo.

## 5. Modo servidor

DuckDB permite un único proceso escritor por archivo, así que `devctx serve` pasa
a ser el dueño exclusivo del store de un proyecto y el resto de comandos enrutan
a él por HTTP. Se levanta solo en el primer uso y se apaga tras 15 minutos de
inactividad.

```bash
devctx serve                 # primer plano, este proyecto
devctx serve --stop
devctx serve --central       # el store central en su lugar; ver docs/12
DEVCTX_NO_AUTOSERVE=1 devctx search "…"    # abrir el store directamente
```

Como el servidor tiene el código cargado, **un binario recompilado no surte
efecto hasta reiniciar el servidor que está corriendo** — haz `devctx serve
--stop` antes de probar un cambio.

## 6. Resumen rápido

| Quieres… | Haz |
|---|---|
| Cambiar el modelo de un proyecto | Edita `embeddings.model` y luego `devctx index --full` |
| Dejar archivos fuera del índice | `.gitignore`, o `indexing.exclude` para los que git trackea |
| Sacar el índice del repositorio | Define `state_dir` (o `storage.db_path`) |
| Mover los modelos a otro disco | `DEVCTX_MODEL_CACHE` |
| Ver con qué está configurado un proyecto | `devctx projects show <nombre>` |
| Compartir una lección entre proyectos | `devctx remember … --scope global` |
