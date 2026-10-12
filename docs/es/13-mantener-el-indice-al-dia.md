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
extractor — así que cada fila es un paso, no 0.9.0 → 0.10.0 de una vez:

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
solo en este patrón, así que el fichero no vuelve a achicarse.

El paso de un índice 0.9.0 real con el binario final 0.10.0 se mide en la
verificación de campo antes del release (PLAN-009 TASK-016); sus cifras se
agregan acá.

### Qué responden las herramientas mientras tanto

Nada se rompe mientras una rama espera su `--full`:

| Herramienta | Responde |
|---|---|
| `index_status` | `extractor_stale: true` y un `hint` que dice que corras `devctx index --full` |
| `read_symbol`, `get_references`, `search` (`keyword`/`hybrid`), la vista web del grafo | Como 0.9, por nombre sobre `graph_edges`, con un `warning` |
| `impact_analysis` | El recorrido de 0.9 — por nombre, sin confianza, sin filtros, sin tope — con el `warning`, que agrega, si se pasaron filtros, que no se aplicaron |
| `build_context` con `related: true` | Sin sección, y `[devctx] related: not shown: …` |
| `traverse`, `repo_map`, `skeleton` | **Un error**: `… reads the symbol graph, and this branch's index predates it (an older extractor, or no symbol rows): reindex with devctx index --full (vectors are reused)`. No tienen una respuesta de 0.9 a la que volver |
| búsqueda híbrida con `search.centrality_weight` > 0 | Solo vector + palabra clave |
| una rama nueva indexada desde una vieja | Re-embebe sus ficheros en vez de copiarlos |

Una rama indexada por 0.10 cuyo lenguaje no tiene extracción de símbolos (solo
ficheros de texto) tampoco tiene filas de símbolos: las herramientas viejas
responden a la manera de 0.9 con un `warning` propio, y `skeleton` manda a
`read_file` en vez de pedir un reindexado que no cambiaría nada.

### Volver a 0.9.0

Instalá el binario 0.9.0, corré `devctx serve --stop` y reiniciá tus sesiones
de IA. Después:

- Las tablas de 0.10 (`symbols`, `edges`, la vista `live_edges`, claves extra
  de `index_meta`) quedan en la base, sin leerse e inocuas. Nada se migra ni se
  reescribe.
- `graph_edges` se mantuvo al día en su formato de 0.9, así que `read_symbol`,
  `get_references` e `impact_analysis` de 0.9.0 responden desde ahí.
  Verificado con un build cuyo camino de lectura es el de 0.9.0 — la misma
  respuesta de `impact`, byte a byte, que el binario 0.10 sobre la misma base
  (2026-10-06); la verificación de campo lo repite con el binario publicado.
- 0.9.0 ve un sello que no escribió e informa `extractor_stale` con su propio
  aviso. Funciona igual; su `--full` volvería a embeber cada chunk (0.9.0 no
  reusa vectores), y no hace falta para que responda.
- **Volver a 0.10** después de que 0.9.0 indexó ficheros: esos ficheros no
  tienen filas de símbolos, así que `graph_out_of_step` marca la rama como vieja
  y pide `devctx index --full` otra vez — con los vectores reusados.
- El fichero conserva su tamaño: los bloques libres no se recuperan.

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
