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
  este binario (o uno que no dejó registro). Las tools de grafo agregan un
  `warning` a cada respuesta y el arreglo es `devctx index --full`; las
  ejecuciones incrementales nunca lo limpian. **Tras actualizar desde 0.8.2 o
  anterior, todo índice está así una vez**, y hasta que reindexes, una rama
  *nueva* re-embebe todos sus ficheros en vez de copiarlos de una ya indexada —
  correcto, pero lento.

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
