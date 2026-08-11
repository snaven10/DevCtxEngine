# El store central

> 🌐 [English version](../12-central-store.md)

Un registro de todos los proyectos que DevCtxEngine conoce, más la memoria que
merece la pena llevarse de uno a otro. Es lo que permite que un agente trabajando
en un repositorio sepa que existen los demás — y recuerde lo que se aprendió allí.

---

## 1. Qué vive dónde

Cada proyecto conserva su propia base de datos: vectores, grafo de llamadas,
rutas y sus memorias propias. Solo se mueve al store central lo que no tiene un
único dueño.

| | Store del proyecto | Store central |
|---|---|---|
| Vectores de código | sí | no |
| Grafo de llamadas y rutas | sí | no |
| Memorias `local` | sí | no |
| Memorias `global` | copia, para trabajar sin red | **fuente de verdad** |
| Registro de proyectos | no | sí |
| Config del proyecto | fuente de verdad (`.devctx/config.yaml`) | copia cacheada |

Mantener los vectores por proyecto es deliberado. Reindexar un repositorio nunca
toca ni bloquea a otro, cada uno puede usar un modelo de embedding distinto sin
corromper nada, y ninguna búsqueda necesita un filtro por repo para no ahogarse
en resultados ajenos.

```
   repo-a/            repo-b/            repo-c/
   .devctx/state/     .devctx/state/     .devctx/state/
   index.duckdb       index.duckdb       index.duckdb
        |                  |                  |
   devctx serve       devctx serve       devctx serve
        |                  |                  |
        +---------+--------+---------+--------+
                  |
        devctx serve --central          <- escritor único
                  |
        ~/.local/share/devctx/central.duckdb
          projects + memorias globales
```

## 2. Ubicaciones

| Qué | Por defecto | Se cambia con |
|---|---|---|
| Base de datos central | `~/.local/share/devctx/central.duckdb` | `DEVCTX_HOME` |
| Config central | `~/.config/devctx/config.yaml` | `DEVCTX_HOME` |
| Modelos descargados | `~/.local/share/devctx/models` | `DEVCTX_MODEL_CACHE` |

`DEVCTX_HOME` junta config y datos bajo un solo directorio — así es como los
tests y CI se mantienen lejos de tus directorios reales. Se respetan
`$XDG_DATA_HOME` y `$XDG_CONFIG_HOME` si están definidas.

La caché de modelos se comparte a propósito: los ficheros son idénticos los pida
quien los pida y pesan cientos de megabytes, así que se descarga una copia y se
reutiliza en todas partes.

## 3. El registro

```bash
devctx projects add .                    # registra el repositorio actual
devctx projects add ~/code/api --init    # crea su config desde los defaults centrales
devctx projects list                     # nombre · modelo · frescura · ruta
devctx projects show api                 # todo lo registrado de uno
devctx projects refresh api              # relee su .devctx/config.yaml
devctx projects rm api --deactivate      # lo oculta, conservando su historial
```

`devctx init` registra el repositorio que inicializa, así que `projects add` solo
hace falta para repositorios inicializados antes de que existiera el registro.

Cada fila guarda dónde está el repositorio, qué modelo de embedding usa, su
descripción y cómo de fresco está su índice. El indexado actualiza la frescura
por su cuenta.

**Los nombres son únicos.** Registrar un segundo repositorio con un nombre ya
tomado se rechaza en vez de repuntarlo en silencio; usa `--name` para elegir
otro. Volver a registrar la misma ruta actualiza la fila existente en lugar de
duplicarla, conservando su fecha de alta y sus estadísticas de índice.

## 4. Memorias globales

Una memoria es o `local` — solo este proyecto — o `global`, compartida con todos
los proyectos de la máquina.

```bash
devctx remember "verifica siempre la firma de los webhooks" --type insight --scope global
devctx recall "cómo valido un webhook"               # ambos alcances (por defecto)
devctx recall "..." --scope global                   # solo las compartidas
devctx recall "..." --scope global --repo api        # solo lo que aportó `api`
```

Dos propiedades que conviene conocer.

**Una lección converge.** Las memorias globales se indexan por contenido, no por
el proyecto que las aportó, así que la misma idea guardada desde dos repositorios
se convierte en una memoria con un contador de duplicados — no en dos filas. El
repositorio de origen se conserva como procedencia y es sobre lo que filtra
`--repo`.

**Las memorias locales no salen.** Nada marcado `local` es visible desde otro
proyecto. Ese es el modelo de privacidad, y es por lo que el valor por defecto de
`remember` es `local`: compartir es una decisión que tomas explícitamente.

Los resultados de ambos stores se fusionan por **rango**, nunca por score. Un
proyecto puede embeber con un modelo distinto al del store central, así que sus
similitudes no están en escalas comparables; la posición es lo único en lo que
coinciden, y una memoria que aparece en ambas listas sale premiada por ello.

## 5. El daemon

DuckDB permite un único proceso escritor por fichero. Las bases de los proyectos
no se comparten, así que sus servidores nunca compiten — pero el store central lo
comparten todos, y que dos procesos lo abran a la vez no degrada, falla:

```
$ devctx projects add ./a & devctx projects add ./b
Error: opening the central store
```

`devctx serve --central` es el escritor único. Todo lo demás llega al store
central a través de él.

```bash
devctx serve --central              # en primer plano, en un puerto derivado de DEVCTX_HOME
devctx serve --central --stop       # pararlo
```

Rara vez lo arrancas a mano: cualquier comando que necesite el store central
levanta uno en segundo plano y se apaga solo tras 15 minutos de inactividad.
`DEVCTX_NO_AUTOSERVE=1` desactiva eso, y entonces un comando suelto abre el store
directamente — correcto cuando no hay nada más corriendo, y la razón de que un
`projects list` solitario siga funcionando sin daemon alguno.

A diferencia del servidor de un proyecto, no carga ningún modelo: arrancar es
abrir una base de datos y nada más.

## 6. Configuración

`~/.config/devctx/config.yaml`, escrito con los valores por defecto en el primer
arranque:

```yaml
memory:
  provider: local
  model: minilm-l6       # fija el espacio vectorial global
defaults:                # lo que hereda `projects add --init`
  embeddings:
    provider: local
    model: minilm-l6
  reranking:
    enabled: true
    model: bge-base
reindex:
  every_seconds: 0       # 0 = apagado; ver §7
```

`memory.model` es una restricción, no un valor por defecto. Todas las memorias
globales viven en un mismo espacio vectorial, así que no puede variar por
proyecto — y cambiarlo con el store ya creado se rechaza al abrir, en lugar de
corromperlo:

```
central store at ~/.local/share/devctx/central.duckdb holds 384-dimensional
vectors but `memory.model` resolves to 768; changing the central memory model
requires re-creating the store
```

Cuando el modelo de un proyecto coincide con `memory.model` — el caso habitual —
se reutiliza el modelo ya cargado y la memoria global no cuesta memoria extra.

## 7. Reindexado en segundo plano

El daemon puede refrescar los proyectos registrados por temporizador:

```yaml
reindex:
  every_seconds: 900
```

Una pasada compara `git rev-parse HEAD` con el commit registrado de cada proyecto
sin abrir ninguna base de datos, así que los proyectos que no tienen nada que
hacer no cuestan nada. Está **apagado por defecto**: indexar en silencio todos
los repositorios que hayas registrado alguna vez sorprende, y en un portátil sale
caro.

Ver [Mantener el índice al día](13-mantener-el-indice-al-dia.md) para las
alternativas por repositorio (`hooks`, `watch`), que es lo que casi todo el mundo
quiere primero.

## 8. Alcanzarlo desde un agente

Por MCP, tres herramientas cubren esto:

| Herramienta | Para qué |
|---|---|
| `list_projects` | Descubrir qué repositorios existen, y cómo de fresco está cada índice |
| `recall` | `scope: local \| global \| all`, `repo:` para acotar; cada hit lleva su alcance |
| `search_project` | Buscar en el código de otro repositorio por nombre |

`search_project` federa: despierta exactamente el servidor que nombraste. El
recall de memorias no — las memorias globales ya están en el store central, así
que una pregunta entre proyectos es una consulta local y no N arranques en frío.

---

## Transición desde el esquema anterior a Rust

Las versiones previas compartían *todo* apuntando cada repositorio a un mismo
store con `DEVAI_STATE_DIR`. Ese ya no es el modelo y no conviene recrearlo:
forzaba un único modelo de embedding en todos los repositorios, hacía que cada
reindexado compitiera por un solo fichero, y dejaba toda búsqueda necesitando un
filtro por repo para ser legible.

Si tienes un montaje así, registra los repositorios (`devctx projects add`) y deja
que cada uno conserve su índice. `DEVCTX_HOME` sigue existiendo para reubicar los
directorios propios de DevCtxEngine, que es otra cosa.
