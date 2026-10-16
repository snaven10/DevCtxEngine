# Mantener el índice al día

> 🌐 [English version](../13-keeping-the-index-fresh.md)

Cuatro maneras de no tener que ejecutar `devctx index` a mano, en el orden en que
la mayoría las quiere.

---

## La regla

**El índice refleja el árbol de trabajo, menos lo que git ignora.** No el último
commit — el árbol de trabajo. Un fichero que has escrito pero no has hecho
`git add` es justo el código por el que más probablemente vas a preguntar, así
que se indexa como cualquier otro.

Dos consecuencias que conviene interiorizar:

- `index --full` no tira el trabajo sin commitear.
- Lo que git ignora nunca llega al índice, así que `.gitignore` es el primer
  sitio donde controlar qué se indexa.

## 1. Los hooks de git

La automatización más barata que funciona de verdad. Disparan exactamente cuando
el diff tiene algo nuevo que mirar, no necesitan ningún proceso vivo, y no
cuestan nada mientras no pasa nada.

```bash
devctx hooks install
devctx hooks status
devctx hooks uninstall
```

**Se instalan dos hooks, y el segundo es el que se le pasa a todo el mundo:**

| Hook | Dispara en |
|---|---|
| `post-commit` | tus propios commits |
| `post-merge` | merges **y pulls fast-forward** |

`post-commit` NO corre en un merge ni en un `git pull` — git usa `post-merge`
para los dos. Instalar solo `post-commit` deja el índice viejo justo después de
mergear un PR o de traerte el trabajo de otro, que es cuando más probable es que
le hagas una pregunta que ya no puede responder bien.

Sigue sin cubrirse: rebase, checkout y reset. Son lo bastante raros como para que
re-indexar a mano sea la respuesta honesta, en vez de un tercer y cuarto hook.
`devctx hooks status` te lo dice.

El cuerpo se escribe entre marcadores, así que un hook que ya tuvieras se amplía
en vez de reemplazarse, y quitar el nuestro deja el tuyo intacto:

```sh
#!/bin/sh
make lint                       # el tuyo, conservado

# >>> devctx (managed) >>>
(unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX …; "/home/tu/.local/bin/devctx" index >/dev/null 2>&1 &) || true
# <<< devctx (managed) <<<
```

(La línea real lista las doce variables que seleccionan un repositorio.) Va
desacoplado y con `|| true`: git no debe esperar al indexado, ni fallar por su
culpa. Volver a ejecutar `install` refresca el bloque en su sitio — y así es como
una instalación vieja, anterior a `post-merge` o al `unset`, se lo agrega.

### El entorno que git le pasa a un hook

Git ejecuta los hooks con `GIT_DIR`, `GIT_INDEX_FILE` y compañía apuntando al
repositorio — o *worktree enlazado* — que está haciendo el commit. `devctx index`
auto-lanza un servidor, y un servidor que hereda esas variables las sigue usando
toda su vida: borra el worktree y cada llamada a git de ese servidor falla con
`not a git repository: …/.git/worktrees/…`. Por eso hay dos capas:

1. **El hook** hace `unset` de las variables que seleccionan el repositorio dentro
   de su subshell (`unset` es un builtin POSIX, así que no depende de un binario
   `env` y no puede hacer fallar el commit).
2. **devctx mismo** las quita: cada subproceso `git` y cada re-ejecución del
   binario `devctx` elimina de su entorno todo `GIT_*` salvo el de transporte y
   diagnóstico (`GIT_SSH*`, `GIT_TERMINAL_PROMPT`, `GIT_ASKPASS`, `GIT_EXEC_PATH`,
   `GIT_CONFIG_GLOBAL/SYSTEM/NOSYSTEM`, `GIT_TRACE*`).

**Los hooks instalados por una versión anterior conservan el bloque viejo** hasta
que vuelvas a correr `devctx hooks install` en cada repositorio; la capa 2 los
protege mientras tanto. No hay migración automática. Un efecto colateral que
conviene saber: como devctx descarta `GIT_DIR` / `GIT_WORK_TREE`, las
configuraciones que dependen de ellas (un repositorio bare de dotfiles, por
ejemplo) no son respetadas por los subprocesos de devctx — cada repositorio se
localiza desde el directorio que se le da.

### Desde un worktree enlazado

Un `post-commit` disparado en un `git worktree` enlazado ejecuta `devctx index`
ahí, y devctx resuelve un worktree enlazado al proyecto de su worktree
**principal** (`--git-common-dir`). Así que indexa el `HEAD` del worktree
principal, no el commit que acabas de hacer en el enlazado. El commit del worktree
enlazado llega al índice cuando aterriza en una rama trackeada (ver
`indexing.branches`).

El uninstall trata cada hook por separado: un `post-commit` que además corre tu
linter conserva el linter, mientras que un `post-merge` que era solo nuestro se
elimina.

## 2. `devctx watch`

Cubre la única ventana que el hook no puede — el trabajo escrito pero sin
commitear.

```bash
devctx watch                  # hasta que lo interrumpas
devctx watch --debounce 5     # segundos a esperar tras el último cambio
```

Los guardados se agrupan antes de indexar: los editores escriben a ráfagas
(formatear al guardar, luego la escritura, luego un renombrado de temporal) y una
compilación toca cientos de ficheros a la vez. Tres segundos por defecto.

Qué ignora: todo lo de `.gitignore`, todo lo de `indexing.exclude`, los
directorios propios de DevCtxEngine, y los temporales que dejan los editores al
guardar (`~`, `.swp`, `.#foo`, `foo.rs___jb_tmp___`).

**Límites conocidos.**

- Un `git checkout` dispara miles de eventos de golpe contra el estado de índice
  de otra rama. Por ahora, para el watcher al cambiar de rama.
- En Linux cada directorio vigilado cuesta un watch de inotify. Un repositorio
  grande puede agotar el límite por usuario; el error dice cómo subirlo.

## 3. `devctx reindex`

Trabaja sobre el registro en vez de sobre un solo repositorio:

```bash
devctx reindex                       # este proyecto
devctx reindex --all                 # todos los proyectos activos registrados
devctx reindex --project api --project web
devctx reindex --all --full
```

Cada proyecto se indexa a través de su propio servidor, así que esto nunca toma
un segundo bloqueo sobre una base que otro proceso ya posee. Que uno falle no
detiene al resto; los fallos se recogen y se informan al final.

## 4. El planificador central

Para repositorios en los que no estás sentado ahora mismo. Ver
[El store central §7](12-store-central.md#7-reindexado-en-segundo-plano). Apagado
por defecto.

---

## Controlar qué se indexa

Más allá de `.gitignore`, la config del proyecto acepta patrones para código que
git *sí* trackea pero que no merece la pena buscar:

```yaml
# .devctx/config.yaml
indexing:
  exclude:
    - vendor/
    - "*.generated.rs"
    - docs/terceros/**
```

Son patrones de `.gitignore`, no globs literales — `vendor/` cubre todo lo que
hay debajo, `*.generated.rs` casa a cualquier profundidad — así que un patrón se
comporta igual aquí que allí. Se aplican igual llegue el fichero como llegue:
`index`, el hook, `watch`, o una lista explícita de rutas.

Añadir un exclude **poda lo que ahora cubre**, de modo que la config es la verdad
completa y no solo se aplica a ficheros vistos después. Un patrón malformado se
descarta en vez de tumbar la ejecución.

### Excluido por defecto

Desde 0.9, aunque no escribas ni una línea, esto queda fuera del índice aunque git
lo trackee: `node_modules/`, `dist/`, `vendor/`, `third_party/`,
`bower_components/`, `*.min.js`, `*.generated.*` (a cualquier profundidad) y
`/build/`, `/target/` (solo en la raíz del repositorio, así que
`src/x/build/Builder.java` sigue siendo código). Se apagan con
`indexing.default_excludes: false`, o recuperas solo `/build/` con
`indexing.include_build: true` — ver
[Configuración](11-configuracion.md#excludes-por-defecto). Tu propia lista
`exclude` se aplica después, así que `"!vendor/"` lo reincluye.

### Cuando cambia el conjunto de excludes: reconciliación

El índice recuerda una huella del conjunto efectivo de excludes. Cuando difiere —
editaste `exclude`, cambiaste una opción o actualizaste a un release con defaults
nuevos — el siguiente `devctx index` sigue siendo **incremental** y reconcilia:

- los ficheros trackeados e indexados que el conjunto ahora excluye se eliminan
  (`files_pruned`);
- los ficheros trackeados que ya no excluye y que no están en el índice se añaden
  y se embeben;
- nada más se re-embebe, y el log dice cuántos ficheros excluyó cada patrón:
  `the exclude set changed: 4 indexed file(s) are now excluded (/build/: 3, *.min.js: 1)`.

La huella nueva se registra solo cuando la reconciliación termina, así que una
cancelada simplemente se rehace. Dos límites: una ejecución por lista explícita de
rutas (hook, `watch`) ni reconcilia ni sella; y la reconciliación actúa sobre la
rama que se indexa, así que una rama de *fallback* sigue mostrando los ficheros
recién excluidos hasta que esa rama se reindexe. Los ficheros que git trackea pero
el indexador nunca toma (binarios, lenguajes sin parser, demasiado grandes) se
vuelven a saltar en cada reconciliación y aparecen en `files_skipped`.

## Cómo se ve un índice viejo

Dos señales honestas, ambas en `index_status`:

- `up_to_date: false` — el commit registrado no es `HEAD`.
- `extractor_stale: true` — el índice lo produjo un parser/extractor más viejo que
  este binario (o uno que no dejó registro), o su grafo de símbolos está
  desfasado de sus ficheros (abajo). Las tools de grafo agregan un `warning` a
  cada respuesta y el arreglo es `devctx index --full`; las ejecuciones
  incrementales nunca lo limpian. **Tras actualizar desde 0.8.2 o anterior, y
  de nuevo tras pasar a 0.10, todo índice está así una vez**, y hasta que
  reindexes, una rama *nueva* re-embebe todos sus ficheros en vez de copiarlos
  de una ya indexada — correcto, pero lento.

`extractor_stale` es por rama, y es la suma de dos chequeos:

- **El sello.** Cada ejecución `--full` registra en `index_meta` el extractor
  que construyó la rama: `EXTRACTOR_VERSION` (21 en 0.10) y un hash de las
  definiciones de lenguaje. Un sello distinto, o ninguno, es viejo.
- **`graph_out_of_step`.** El sello solo puede mentir: un binario que no escribe
  el grafo de símbolos (0.9.0 tras un downgrade) puede indexar de forma
  incremental bajo un sello de 0.10. Así que la rama también está vieja cuando
  un fichero de un lenguaje parseado no tiene símbolo de archivo (lo agregó ese
  binario), cuando el fichero de un símbolo de archivo ya no está (lo borró), o
  cuando el hash de contenido de un símbolo de archivo no es el del fichero (lo
  modificó). Una rama en ese estado nunca es fuente de la copia entre ramas. La
  respuesta se cachea por rama hasta que algo la escribe, porque cada tool de
  grafo pregunta.

Las reglas de enlace tienen versión propia (`LINK_VERSION`, 21 en 0.10): cuando
cambia, el próximo `devctx index` — alcanza con uno incremental — reenlaza y
re-rankea la rama, sin re-parsear ni re-embeber nada, y nada se informa como
viejo.

## Pasar a 0.10: el grafo de símbolos

0.10 indexa un grafo de símbolos — símbolos con ids estables, siete clases de
relaciones, un link pass que las resuelve — que ningún índice de 0.9 tiene (ver
[Grafo de símbolos](03-conceptos-fundamentales/grafo-de-simbolos.md#qué-hay-en-el-grafo)).
Así que, tras actualizar, cada rama indexada informa `extractor_stale: true`
hasta que se reconstruye una vez.

### Qué correr

```bash
devctx serve --stop                    # un servidor en marcha conserva el código viejo
devctx index --full                    # este repositorio: la rama que indexa por defecto
devctx index --full --branch release   # cada otra rama que consultes
devctx reindex --all --full            # o cada proyecto registrado (su rama por defecto)
```

- **Por repositorio y por rama.** El sello es por rama, y una ejecución vuelve
  a sellar solo la rama que indexa. Una rama que nunca reconstruís sigue
  respondiendo a la manera de 0.9. Una vez reconstruida una rama, las otras
  copian de ella los ficheros que comparten en vez de volver a parsearlos.
- **Solo `--full` lo limpia.** Los hooks, `watch` y un `devctx index` común
  escriben las tablas nuevas de los ficheros que tocan, pero nunca vuelven a
  sellar la rama.
- **Reiniciá tus sesiones de IA** después de instalar el binario: un MCP
  conserva el código con el que arrancó (`index_status` dice
  `mcp.binary_replaced`).

### Cuánto cuesta

La ejecución completa vuelve a parsear cada fichero y corre el link pass, pero
**reusa el vector guardado de cada chunk cuyo texto no cambió** (mismo hash de
contenido, misma configuración de embedding), así que no vuelve a pagar el
embedder; cuando ningún chunk necesita embedding ni siquiera carga el modelo.
Unos pocos chunks sí cambian de texto en 0.10 y se re-embeben: los
constructores pasan a ser chunks propios, la lista de métodos de una clase los
suma, y el header de contexto de los métodos de un `impl` genérico pierde sus
argumentos de tipo (`Cache<T>` → `Cache`).

Medido en un sandbox, build release, `minilm-l6`, mientras se construía el
extractor — así que cada fila es un paso, no 0.9.0 → 0.10.0 de una vez (este
repositorio creció mientras tanto: 281 ficheros el 2026-10-06, unos 440 dos
días después):

| Fecha | Repositorio | Desde cero | `--full` tras cambiar el extractor | Chunks reusados |
|---|---|---|---|---|
| 2026-10-06 | este, 281 ficheros, 4 928 chunks | 278 s | 54,9 s (19,7 %) | todos |
| 2026-10-06 | este, sobre un índice con la extracción de 0.9.0 | 345 s | 31,4 s | 4 888 de 4 928 (40 re-embebidos) |
| 2026-10-06 | un repositorio Java, 216 ficheros, 3 253 chunks | 442 s | 36,9 s (8,3 %) | todos |
| 2026-10-07 | el mismo repositorio Java, con las 7 relaciones | — | 24,8 s | 3 204 de 3 284 |
| 2026-10-06 | un `--full` sin nada que cambiar: este / el Java | 240 s / 169 s | 4,4 s / 2,4 s (1,8 % / 1,4 %) | todos, sin cargar el modelo |

El link pass y los ranks encima de eso: 0,2–0,4 s en este repositorio, y en un
repositorio Java de 1 250 ficheros 1,9–2,6 s con la máquina tranquila y hasta
5,6 s con carga (los ranks son 69–165 ms de eso), medido entre el 2026-10-07 y
el 2026-10-09; mientras corre tiene en memoria los símbolos de la rama (+114 MiB
de pico en ese repositorio Java).

La base crece. Con el grafo completo de 0.10, un repositorio Java de 216
ficheros pasó de 79 a 112 bloques usados de 256 KiB, más 66 libres
(2026-10-07); una reescritura deja bloques libres que DuckDB no reutiliza por sí
solo en este patrón, y ni `VACUUM` ni un checkpoint los devuelven — la
compactación de abajo sí.

El paso de un índice 0.9.0 real con el candidato 0.10.0 (verificación de campo,
PLAN-009 TASK-016, 2026-10-12, `ml-granite` en CPU, en sitio sobre el índice
que hizo 0.9.0 en el mismo sandbox):

| Repositorio | 0.9.0 desde cero | `--full` de 0.10 encima | Chunks reusados | Bloques usados antes → después | Fichero |
|---|---|---|---|---|---|
| este, 277 ficheros en un commit anterior | 566 s | 53 s (9,4 %) | 4 737 de 4 806 | 114 → 173 | 28,5 → 61,3 MiB |
| un repositorio Java, 216 ficheros | 529 s | 38 s (7,1 %) | 3 204 de 3 284 | 85 → 124 | 21,3 → 43,8 MiB |
| un repositorio Python, 62 ficheros | 59 s | 4 s (7,1 %) | los 312 | 30 → 36 | 7,5 → 11,8 MiB |
| un repositorio Java grande, 1 625 ficheros | 3 415 s | 330 s (9,7 %) | 20 242 de 21 615 | 504 → 786 | 153 → 296 MiB |
| un workspace Angular, 1 581 ficheros | 1 466 s | 274 s (18,7 %) | 13 309 de 14 167 | 293 → 463 | 83 → 171 MiB |

Los chunks re-embebidos son los que cambiaron de texto (constructores y
funciones flecha exportadas que 0.9.0 no cortaba, un chunk de clase que ahora
lista sus métodos). La base actualizada es más grande que una hecha desde cero
por 0.10 (el Java grande: 523 bloques usados desde cero, 786 actualizada): la
reescritura borra y vuelve a escribir cada fila de `vectors`, y DuckDB no
reutiliza esos bloques — hasta la compactación. El reuso exige que el índice
siga donde se construyó: el registro va por la ruta del repositorio, así que un
repositorio copiado (otro directorio) vuelve a embeber todo.

### La compactación después del paso

Reescribir cada fila de vectores deja el fichero de un índice actualizado en
alrededor del doble de lo que guarda (tabla de arriba). Por eso, una vez,
después del `devctx index --full` que migra una rama indexada antes del grafo,
devctx **compacta** la base: la copia fila por fila a un fichero nuevo junto a
ella, reconstruye ahí los índices HNSW y BM25, verifica la copia contra la
original y la pone en su lugar con un `rename` atómico.

Medido con el binario final sobre los cinco repositorios de la verificación de
campo (PLAN-009 TASK-019, 2026-10-16, release `dc0111f`, cada índice 0.9.0
reconstruido por el binario 0.9.0 en el mismo commit, actualizado en sitio con
terminal, máquina compartida, carga 8–13):

| Repositorio | 0.9.0 antes del paso | Tras el `--full` | Tras la compactación | Contra 0.9.0 | Paso completo | Compactación | Pico de memoria (CLI / servidor) | Disco extra |
|---|---|---|---|---|---|---|---|---|
| este repositorio | 107 bloques usados, 27 MiB | 164, 61 MiB | 114, 29 MiB | +7 % usados, +6 % bytes | 41 s | 2,5 s | 149 / 916 MiB | 32 MiB |
| un repositorio Java | 84, 21 MiB | 111, 41 MiB | 84, 21 MiB | +0 % usados, −1 % bytes | 30 s | 1,7 s | 141 / 744 MiB | 27 MiB |
| un repositorio Python | 25, 6,5 MiB | 31, 13 MiB | 27, 6,8 MiB | +8 % usados, +4 % bytes | 3 s | 0,6 s | 95 / 151 MiB | 7 MiB |
| un repositorio Java grande | 480, 153 MiB | 753, 286 MiB | 522, 131 MiB | +9 % usados, −14 % bytes | 301 s | 7,3 s | 370 / 1 050 MiB | 157 MiB |
| un workspace Angular | 281, 83 MiB | 450, 171 MiB | 308, 77 MiB | +10 % usados, −8 % bytes | 227 s | 5,0 s | 276 / 926 MiB | 91 MiB |

"Paso completo" es lo que espera la persona: el `--full` por el servidor
(97,6–100 % de los chunks reusados), pararlo y la compactación. Reconstruir el
índice HNSW es alrededor del 60 % de la compactación y copiar las filas
alrededor del 25 %; estos índices no tienen índice BM25 (`fts: false`), cuya
reconstrucción suma alrededor de un cuarto más (TASK-018). La compactación
conserva `graph_edges` fila por fila (0 filas distintas, `EXCEPT ALL`, en los
cinco).

**Cuándo corre sola.** Solo al terminar un `devctx index --full` (o cada
proyecto de `devctx reindex --full`) que no se canceló y dejó la nota debida, y
solo:

- con terminal. Corrido desde un agente o un script nunca compacta: termina con
  la línea exacta `devctx compact`, para correrla después;
- sin `DEVCTX_NO_AUTO_COMPACT`;
- si pasan los previos: el fichero es tuyo, su directorio se puede escribir,
  hay tanto disco disponible para tu usuario como lo que mide el fichero, y
  memoria para el límite de DuckDB más el índice HNSW dentro del 80 % de lo
  disponible;
- sin otra corrida de indexado en vuelo en el servidor;
- y si el servidor que para sale limpio (por SIGTERM, con su write-ahead log
  plegado). Un servidor que hubo que matar, o un log que quedó, deja la
  compactación debida.

Lo anuncia antes de parar el servidor. Lo que pida el índice mientras tanto
recibe "se está compactando; reintentá", y el pedido siguiente levanta un
servidor nuevo. Las corridas incrementales, los hooks, `watch` y la tool MCP
`index_repo` nunca compactan: dejan la nota, y `status` la muestra.

**Debida.** `status` e `index_status` llevan un bloque `compaction` (`state`:
`none`, `owed`, `owed:failed-verify`, `done`; los bloques usados y libres; un
`hint`), y una compactación debida encabeza el `hint`, en mayúsculas. Corré
`devctx compact`. `owed:failed-verify` quiere decir que una copia no coincidió
con la original: se descartó la copia y quedó la original; solo `devctx
compact` lo reintenta.

**`devctx compact`.** Compacta el índice de este proyecto a pedido.
`--dry-run` muestra los bloques, lo que dejaría y los previos, y no escribe
nada; `--keep-original` conserva la original como `<índice>.pre-compact` (un
hard link: el disco de una segunda copia); sin bloques libres no hace nada salvo
con `--force`. Para el servidor de la misma forma, y si hubo que matarlo pliega
antes el write-ahead log en vez de rendirse. En Windows devctx no puede parar un
servidor: paralo vos y después corrélo.

**Qué quiere decir "contesta igual".** La copia guarda los mismos datos: cada
tabla comparada por cantidad de filas y por un checksum de cada columna de cada
fila, y las memorias, sus referencias a símbolos y las sesiones además fila por
fila, en los dos sentidos. Los vecinos más cercanos exactos (por fuerza bruta)
son idénticos. El índice HNSW se reconstruye, y un índice aproximado construido
dos veces no es el mismo grafo: su top-k puede cambiar en algunos resultados
(medido: 216 de 300 listas top-10 idénticas entre la original y la copia, 260
de 300 entre dos construcciones de la misma copia; recall@10 de alrededor de
0,96 en los tres). Los empates de `impact_analysis` pueden salir en otro orden.

**Seguridad.** Nada escribe en la original hasta el `rename`. Un fallo, un
Ctrl-C o un corte antes la dejan como estaba; un temporal que quede
(`<índice>.compact.tmp`) lo borra la siguiente apertura — nunca si falta el
índice mismo. El fichero conserva su modo; su grupo, sus ACL y sus atributos
extendidos no se copian. El fichero de lock `<índice>.compact.lock` queda junto
al índice a propósito: borrar un fichero de lock compite con quien lo acaba de
abrir. Un filesystem que no respeta los locks de fichero (algunos montajes de
red y FUSE) hace que se niegue.

**El sidecar de memorias.** Antes del `rename`, las memorias, sus referencias a
símbolos y las sesiones — lo que ningún reindexado reconstruye — se guardan en
`<índice>.pre-compact.duckdb`, sin vectores. La siguiente apertura lo borra
pasados 30 días, y la siguiente compactación lo reemplaza; si el viejo tenía
una memoria que el índice ya no tiene, se conserva como
`<índice>.pre-compact.<epoch>.duckdb`, que nada borra. Para restaurar desde él:

```bash
devctx serve --stop
devctx memories import .devctx/state/index.duckdb.pre-compact.duckdb
```

Agrega las memorias que le falten al índice — nunca pisa una — con sus
referencias y sesiones, y embebe cada una otra vez (el sidecar no tiene
vectores, así que esto carga el modelo); corrido otra vez tras un fallo, embebe
lo que la primera corrida dejó sin vector. Dos cosas a saber: acepta cualquier
fichero `.duckdb` que tenga esas tablas (el sidecar de otro índice incluido), y
devuelve una memoria que olvidaste a propósito después de la compactación —
olvidala otra vez si es el caso.

### Qué responden las herramientas mientras tanto

Nada se rompe mientras una rama espera su `--full`:

| Herramienta | Responde |
|---|---|
| `index_status` | `extractor_stale: true` y un `hint` que dice que corras `devctx index --full` |
| `read_symbol`, `get_references`, `search` (`keyword`/`hybrid`), la vista web del grafo | Como 0.9, por nombre sobre `graph_edges`, con un `warning` |
| `impact_analysis` | El recorrido de 0.9 — por nombre, sin confianza, sin filtros, sin tope — con el `warning`, que agrega, si se pasaron filtros, que no se aplicaron |
| `build_context` con `related: true` | Sin sección, y `[devctx] related: not shown: …` |
| `traverse`, `repo_map`, `skeleton` | **Un error**: `… reads the symbol graph, and this branch's index predates it (an older extractor): reindex with devctx index --full (vectors are reused)`. No tienen una respuesta de 0.9 a la que volver |
| búsqueda híbrida con `search.centrality_weight` > 0 | Solo vector + palabra clave |
| una rama nueva indexada desde una vieja | Re-embebe sus ficheros en vez de copiarlos |

Una rama indexada por 0.10 cuyo lenguaje no tiene extracción de símbolos (solo
ficheros de texto) tampoco tiene filas de símbolos: las herramientas viejas
responden a la manera de 0.9 con un `warning` propio, y `traverse`, `repo_map`
y `skeleton` responden un error que manda a `search` o `read_file` en vez de
pedir un reindexado que no cambiaría nada.

### Volver a 0.9.0

Instalá el binario 0.9.0, corré `devctx serve --stop` y reiniciá tus sesiones
de IA. Después:

- Las tablas de 0.10 (`symbols`, `edges`, la vista `live_edges`, claves extra
  de `index_meta`) quedan en la base, sin leerse e inocuas. Nada se migra ni se
  reescribe.
- **`graph_edges` es idéntico al que escribe 0.9.0, incluidos sus falsos
  positivos.** 0.10 lo llena con la extracción de 0.9.0 (su código, copiado
  tal cual en `crates/devctx-parse/src/legacy_v090.rs`), así que
  `read_symbol`, `get_references` e `impact_analysis` de un 0.9.0 tras el
  downgrade dan las respuestas que daba 0.9.0 antes del paso, para el mismo
  commit: los mismos nombres, los mismos llamadores a cualquier
  profundidad y los mismos errores (una llamada unida por nombre suelto a
  otra función, un tipo que no veía). El grafo de símbolos y sus mejoras
  viven solo en las tablas que 0.9.0 no lee. Verificado (PLAN-009 TASK-019,
  cinco repositorios, el mismo commit): la tabla que escribió 0.9.0 y la
  que escribió 0.10 tras el paso y la compactación difieren en 0 filas en
  los dos sentidos, comparando todas las columnas (`EXCEPT ALL`), y el
  binario 0.9.0 sobre las dos encuentra los mismos llamadores para cada
  semilla del arnés a profundidad 1, 2 y 3; un test fija las filas que
  escribe el binario 0.9.0 para los fixtures de todos los lenguajes.
- 0.9.0 ve un sello que no escribió e informa `extractor_stale` con su propio
  aviso. Funciona igual; su `--full` volvería a embeber cada chunk (0.9.0 no
  reusa vectores), y no hace falta para que responda.
- **Volver a 0.10** después de que 0.9.0 indexó ficheros: esos ficheros no
  tienen filas de símbolos, así que `graph_out_of_step` marca la rama como vieja
  y pide `devctx index --full` otra vez — con los vectores reusados.
- Un índice compactado se abre igual: mismo DuckDB, todas las tablas copiadas,
  `graph_edges` incluida. 0.9.0 no conoce el lock de compactación; mientras
  una compactación corre, el lock propio de DuckDB sobre la original lo deja
  afuera.

## Lo que nunca se indexa

Los directorios propios de DevCtxEngine se saltan sea cual sea su estado en git:
`.devctx/` (estado y config) y el heredado `.fastembed_cache/`. Sin ese
guardarraíl, un reindexado completo se tragaría su propia base de datos y la
caché de modelos descargados, y luego respondería preguntas con eso.

Los modelos ahora viven fuera de cualquier repositorio — ver
[El store central §2](12-store-central.md#2-ubicaciones). Un `.fastembed_cache/`
que haya quedado en un checkout antiguo ya no se usa y puedes borrarlo.

## Incremental, completo y rutas explícitas

| Ejecución | Selecciona | Poda |
|---|---|---|
| `devctx index` | diff desde el último commit indexado, más los ficheros sin trackear | no — salvo que reconcilia un conjunto de excludes cambiado |
| `devctx index --full` | todo el árbol de trabajo | sí — lo desaparecido, y lo recién excluido |
| rutas explícitas (`watch`) | exactamente los ficheros nombrados | solo esos, si se borraron |

Una ejecución por rutas explícitas deliberadamente **no** avanza el commit
registrado: cubrió trabajo sin commitear, así que mover el marcador haría que el
siguiente diff incremental se saltara commits cuyos otros ficheros nunca se
miraron.

Los borrados de ficheros sin trackear no se detectan de forma incremental — el
diff de commits no puede verlos. Un pase completo los limpia.
