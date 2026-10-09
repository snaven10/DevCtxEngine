# graph-eval — arnés de evaluación del grafo y del contexto

PLAN-009 TASK-001 / DD-1. Mide con números si un cambio mejora (o rompe) tres cosas: el grafo de
código, la relevancia de `search` y `build_context`, y el costo de indexar. Se extiende
`scripts/bench-retrieval.sh` (que sigue ahí, sin tocar) con casos de formato más rico, el brief de
`context` y las métricas del grafo.

| Archivo | Para qué |
|---|---|
| `run.sh` | Orquesta todo contra repos **ya indexados** y escribe `report.md` (tabla markdown para pegar en un `## Resultado`). |
| `graph_metrics.sql` | Métricas del grafo (sección `legacy`: `graph_edges` de 0.9.0; sección `new`: `symbols`/`edges`, corre sola cuando existan). |
| `score.py` | Hit@k, MRR, presencia en el brief, precisión de gold edges, percentiles. Solo stdlib. |
| `cases-devctx.txt` | 10 casos de este repo. |
| `gold-devctx.txt` | 26 gold edges de Rust (este repo, contra a6c39db: 6 de TASK-001, 14 de TASK-007 y 6 de su revisión). |
| `impact-devctx.txt` | Lista fija de símbolos para la latencia de `impact`. |
| `gold-java-anon.txt` | Los gold edges de Java de TASK-005 **anonimizados** (alias opacos `ClassA.m1`, sin rutas ni números de línea), con el caso que muestrea cada uno y el resultado medido, y los casos de dispatch por interfaz de TASK-017 (nodos de `impact_analysis`): para auditar, no para correr. |
| `gold-ts-anon.txt` | Los gold edges de TypeScript de TASK-006, anonimizados igual (`FileA`, `ClassA.m1`, `f1`), con el caso y el resultado antes/después. |
| `gold-python-anon.txt` | Los gold edges de Python de TASK-007 (27 sitios de `legacy-migration`), anonimizados igual, con el caso y el resultado antes/después. |

## Uso rápido

```bash
cd <este repo>             # indexado: .devctx/config.yaml presente
scripts/graph-eval/run.sh  # sin DEVCTX_EVAL_CASES: solo DevCtxEngine
```

Requisitos: `bash` ≥ 5, `python3` y, para las métricas del grafo y los gold edges, el módulo
`duckdb` (si no está, se usa `uv run --with duckdb`; si tampoco, esas dos secciones se saltan y
el reporte lo dice). El módulo `duckdb` de Python debe ser **igual o más nuevo** que el del binario
(`Cargo.lock`, hoy 1.5.x).

Variables: ver el encabezado de `run.sh` (`DEVCTX`, `DEVCTX_EVAL_CASES`, `DEVCTX_EVAL_GOLD`,
`DEVCTX_EVAL_IMPACT`, `DEVCTX_EVAL_ROOT`, `DEVCTX_EVAL_OUT`, `DEVCTX_EVAL_LIMIT`, `DEVCTX_EVAL_RUNS`,
`DEVCTX_EVAL_PY`, `DEVCTX_EVAL_ONLY`, `DEVCTX_EVAL_BRANCH`).

### Casos fuera del repo (privacidad)

Este repo es público. Los casos de un repo privado (rutas, clases, paquetes de un cliente) **no**
se commitean: viven en una carpeta aparte y se cargan por variable.

```bash
export DEVCTX_EVAL_CASES=/ruta/privada/cases-x.txt
export DEVCTX_EVAL_GOLD=/ruta/privada/gold-x.txt
export DEVCTX_EVAL_IMPACT=/ruta/privada/impact-x.txt
export DEVCTX_EVAL_ROOT=/ruta/con/los/repos      # $ROOT/<repo> = checkout indexado de cada repo
scripts/graph-eval/run.sh
```

El campo `repo` de cada caso es el nombre de la carpeta bajo `$DEVCTX_EVAL_ROOT`.

## Formato de los casos

```
consulta | archivo(s) esperado(s), separados por coma | símbolo esperado (o -) | repo | idioma (en|es)
```

- El archivo esperado se compara por **substring** contra la ruta del resultado; con varios, basta uno.
- Hit@k = el archivo esperado aparece entre los primeros k de `search --no-rerank --limit 20`;
  MRR = media de 1/posición (0 si no aparece). Se miden `search` vectorial (sin flag) e híbrido (`--hybrid`).
- Brief = `devctx context --max-tokens 4096 --no-memories`: se mira si el archivo (y el símbolo, si hay)
  esperado está en el texto, y los tokens que dice usar.
- Reglas de oro: **los targets se escriben antes de medir** y no se editan después, salvo error de
  transcripción, que se anota en el `## Resultado` de la task. Al menos un tercio de los casos en
  español y un tercio "de comportamiento" (consulta que no nombra el identificador).

## Formato de los gold edges

```
repo | archivo:línea | destino calificado esperado | external? | archivo del destino (opcional)
```

Un sitio de llamada etiquetado a mano. `destino` es `Clase.metodo` (en Rust el `::` se escribe `.`),
o la función suelta sin calificar. `external` si el destino no está definido en el repo (JDK, Panache,
Mutiny, RxJS, builtins). Estados que cuenta `score.py gold` (por **nombre calificado igual al esperado**):

| estado | significa |
|---|---|
| correcto | alguna arista en esa línea (±2) tiene el destino esperado |
| error | hay arista con el mismo nombre de método pero calificado distinto (se resolvió a otro) |
| sin calificar | la arista existe pero el destino es el nombre pelado: no decide |
| sin arista | no hay arista en esa línea. El grafo 0.9.0 deduplica por (fuente, destino): la 2.ª ocurrencia de la misma llamada en un método no existe |

Para los `external`, "correcto" = el grafo no lo resolvió a una definición del repo **por nombre**
(0.9.0 no marca externos; es una cota, no una medida de la marca `external` de DD-9).

Las métricas del grafo sobre `graph_edges` usan la misma cota por nombre que el inventario 0.8.2
para "aristas internas" y "colisiones": el grafo viejo no tiene identidad de símbolo.

## Medir sin tocar nada vivo (sandbox)

`run.sh` no indexa, no borra y copia el DuckDB antes de abrirlo. Aun así, para medir un binario
nuevo **no** lo apuntes al HOME real si podés evitarlo:

```bash
SB=$(mktemp -d -p /var/tmp devctx-eval-XXXXXX)            # sandbox propio
export DEVCTX_HOME=$SB/home HOME=$SB/home DEVCTX_MODEL_CACHE=$SB/modelcache
mkdir -p $SB/home $SB/repos
# copiar (no mover) la config de máquina y el modelo al sandbox: <home>/config.yaml y <home>/models/<modelo>
git clone <repo> $SB/repos/<nombre> && (cd $SB/repos/<nombre> && devctx init --yes)
```

`DEVCTX_HOME` también separa el serve: el serve del sandbox es otro proceso y se apaga con
`devctx serve --stop` desde el repo del sandbox (nunca se mata por nombre de proceso).

### Fase de parse (DD-21)

`cargo build --release -p devctx-parse --example parse_bench` y
`target/release/examples/parse_bench <dir> --runs 7`: parsea cada archivo como el indexador
(`devctx_parse::parse`), mejor y mediana por corrida, y símbolos por kind. Usa solo la API que
tiene toda release, así que el mismo archivo compila contra un checkout viejo (`git archive
<ref>`) para comparar.

### Costo de indexado (se mide aparte, a mano)

`run.sh` no reindexa. Sobre una **copia** del repo (nunca el vivo):

```bash
cd $SB/repos/<nombre>
t0=$(date +%s.%N)
DEVCTX_NO_AUTOSERVE=1 /ruta/scripts/memprobe.sh --interval 0.5 -- devctx index --full   # VmHWM al final
echo "$(echo "$(date +%s.%N) - $t0" | bc) s"; du -sb .devctx/state       # tamaño del DB después
```

`DEVCTX_NO_AUTOSERVE=1` indexa por la vía directa (la que construye el HNSW). Tomá la medida con
la máquina quieta: el tiempo es dominado por el embedding y se infla con la carga.

### Grafo sin modelo (`graph_bench`, TASK-005)

Para medir el grafo de un repo grande sin pagar el embedding:

```bash
cargo build --release -p devctx-index --example graph_bench
target/release/examples/graph_bench <repo> <ruta>/index.duckdb [--touch <archivo del repo>]
```

Corre `pipeline::run` con vectores constantes: parse, `symbols`/`edges` y link pass reales (la
búsqueda sobre ese índice no significa nada). Imprime el resumen, el tiempo y el `VmHWM`; con
`--touch` agrega un salto de línea al archivo e indexa solo ese (costo incremental de un archivo).
El link pass imprime su línea (`· link pass (full|incremental): …, load … ms, write … ms`). Para el
arnés, una carpeta `$ROOT/<repo>/.devctx/config.yaml` con `storage.db_path` apuntando al DuckDB y
`DEVCTX=true DEVCTX_EVAL_STEPS=graph` (el paso `graph` no llama al binario). Usa solo API que
existe desde 0.9.0: el mismo archivo compila contra un checkout anterior para el "antes".

### Gold edges sobre la tabla `edges` (TASK-005 en adelante)

Si el índice tiene `symbols`/`edges`, `run.sh` exporta `edges_new.tsv` (llamadas e instanciaciones
con el `qualified` de su `dst_id`, `confidence`, `resolution`, `external`) y `score.py gold
--new-edges` agrega una segunda tabla: correcto = destino resuelto igual al esperado (o que termina
en `.esperado`, una clase interna) o marca `external` si el esperado es externo; error = resuelto a
otro o marcado al revés; sin decidir = ni destino ni marca; sin arista = no hay ocurrencia (p. ej.
descartada: las filas `resolution = 'discarded'` no se exportan). Un getter de Lombok cuenta como
correcto si apunta al campo (`Dto.getX` → `Dto.x`, la convención del link pass). Reporta precisión
global, precisión en `high` y cobertura (no `low`), y por sitio la `confidence`/`resolution`.
`graph_metrics.sql` deja las descartadas fuera de los porcentajes de `calls_resueltas` y las cuenta
aparte (`descartadas`).

## Para tasks posteriores

Cada task que cambie el grafo o el ranking corre este arnés antes y después, con el mismo binario
de comparación (0.9.0, la línea base de TASK-001) y los mismos casos. La sección `new` de
`graph_metrics.sql` ya trae las filas de §6 del master (`calls` con `dst_id`, `confidence`/
`resolution`, `external`, `from_test`, ocurrencias repetidas) y se activa sola cuando el índice
tiene las tablas `symbols` y `edges`.
