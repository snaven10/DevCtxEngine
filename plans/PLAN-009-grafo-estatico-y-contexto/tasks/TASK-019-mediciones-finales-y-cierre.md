# TASK-019 — Mediciones finales con compactación, cierre de §6, cross-check e informe

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** — (orquestador, con aprobación del usuario; la sesión revisora mide)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-018
- **Estado:** `done`

---

## Objetivo

Cerrar el plan sobre el binario **con** la compactación de TASK-018: re-medir lo que la compactación
cambia (tamaño, tiempo y RAM del upgrade, downgrade), completar la tabla de §6, correr el
cross-check de Windows/macOS y escribir el cierre (§13). Es la "etapa B" que se separó de TASK-016
por decisión de la revisión del 2026-10-13: TASK-016 queda con lo ya medido sobre el candidato sin
compactación.

## Contexto verificado

- TASK-016 (Resultado) midió las filas de §6 con el candidato `e0b8937` + `fb1707c`, sin
  compactación. Su hallazgo 5 (el upgrade en sitio crece +46-58 % en bloques usados y deja el
  archivo en ~2×) motivó TASK-018. Sus mediciones que **no** dependen del tamaño ni del camino del
  upgrade siguen valiendo y no se repiten.
- Lo que cambia con TASK-018 y hay que volver a medir: la fila "Tamaño del DB" para el upgrade en
  sitio, el tiempo total del upgrade que ve el usuario (ahora incluye parar el serve y compactar),
  el pico de RAM y de disco de ese recorrido, y el downgrade sobre una base compactada.
- Base de comparación del tamaño: `B0` de TASK-018 (el índice 0.9.0 justo antes del `--full` en
  sitio, sobre una copia), en bloques usados y en bytes.
- Método, sandbox y arnés: los de TASK-016 (`scripts/graph-eval/`, casos privados en
  `$DEVCTX_EVAL_CASES`, repos solo con alias).
- Cross-check: job `cross-check` en pushes a `fix/**` (`.github/workflows/rust.yml`); era el Paso 7
  de TASK-016.
- Decisión del usuario del 2026-10-13 (master §12): las filas 1, 3 y 15 de §6 son limitaciones
  conocidas de 0.10.0.

## Archivos

- **Modificar:** este archivo (`## Resultado`), el master (§6 si hay que anotar el número medido,
  §13 Cierre), `docs/07-performance.md` + `docs/es/07-rendimiento.md` y `CHANGELOG.md` con las
  cifras finales (si TASK-018 dejó las suyas, se reemplazan por estas).

## Pasos

- [x] **Paso 1 — aprobación.** Confirmar con el usuario los repos (mínimo backend-a y frontend;
      DevCtxEngine de control) y que el binario a medir es el candidato final (versión ya subida a
      0.10.0). Antes de medir, y asignado a esta task por la revisión de TASK-018:
      - **el bump a 0.10.0** del workspace: hoy la nota de compactación escribe `done:0.9.0:…`;
      - en el `CHANGELOG.md`, como *Known limitations*, las filas 1, 3 y 15 de §6 (decisión del
        usuario del 2026-10-13);
      - el **contrato de downgrade**, en el CHANGELOG y en `docs/13` + ES: las rutas de módulo quedan
        como en 0.9.0 y las llamadas por tipo como `Tipo.metodo`; 0.9.0 las sigue encontrando por
        nombre y recorre un nivel más;
      - el **known issue de HNSW**: reconstruir el índice cambia el top-k aproximado (el exacto no;
        medido en TASK-018).
- [x] **Paso 2 — upgrade con compactación.** Por repo, sobre una copia del índice 0.9.0 real:
      medir `B0`; `devctx index --full` por el serve con el disparo automático; registrar bloques
      usados/libres y bytes tras el `--full` y tras compactar, segundos de cada parte y total,
      `VmHWM` del proceso que compacta y disco temporal máximo. Veredicto contra ≤ 1,30 × `B0`.
- [x] **Paso 3 — lo que no debe cambiar.** Sobre la base compactada: arnés híbrido y vectorial y el
      brief (los 30 casos) iguales a los del índice actualizado sin compactar de TASK-016 (misma
      base de vectores: se espera igualdad salvo empates); `impact("get"/"map")` p95; HNSW y FTS
      usados por la consulta.
- [x] **Paso 4 — downgrade.** `v0.9.0` sobre la base compactada: los 60 casos del downgrade de
      TASK-016, mismas respuestas que sobre la no compactada; ningún llamador perdido.
- [x] **Paso 5 — casos límite de campo.** Una base con dos ramas (frontend); una base con memorias
      locales reales (copia, solo lectura sobre el original): `recall` antes y después; un upgrade
      sin TTY y uno con `DEVCTX_NO_AUTO_COMPACT=1` (quedan `owed`, `status` lo dice) seguidos de
      `devctx compact`. Además (revisión de TASK-018): un **ensayo de restauración** desde el
      sidecar (`<db>.pre-compact.duckdb`, `devctx memories import`); una **sesión MCP activa**
      durante la compactación (el `remember` recibe "se está compactando" y entra al reintentar);
      y la **comprobación de lock de M-B** (el segundo `flock` falla) sobre el filesystem real.
- [x] **Paso 6 — tabla de §6** completa con veredicto por fila: las filas medidas en TASK-016 se
      citan; la de tamaño lleva los dos números (desde cero y upgrade + compactación); las filas 1,
      3 y 15 van como limitaciones conocidas con su decisión.
- [ ] **Paso 7 — cross-check** Windows/macOS empujando una rama `fix/**`; anotar el run. Incluye lo
      que TASK-018 dejó para acá: el `rename` en Windows y el lock de sistema operativo en las tres
      plataformas. Si Windows no se puede confirmar: disparo automático apagado ahí y documentado.
- [x] **Paso 8 — cierre del plan** (§13 del master) e informe: qué se verificó, qué no, y los
      pendientes que pasan al plan siguiente.

## Criterios de aceptación

- [x] backend-a y frontend: usados y bytes tras upgrade + compactación ≤ 1,30 × `B0`, con tiempo,
      RAM y disco temporal anotados; o la fila declarada como no cumplida con causa y decisión del
      usuario.
- [x] Búsqueda, brief e `impact` sobre la base compactada sin regresión contra TASK-016.
- [x] Downgrade sobre la base compactada sin llamadores perdidos.
- [x] Tabla de §6 completa; toda fila que no cumple tiene causa y decisión.
- [ ] Cross-check verde antes de taggear.
- [x] `devctx plan-status PLAN-009` sin warnings al cerrar.

## Riesgos

- El reindex de los repos grandes puede no caber en una sesión: se mide lo aprobado y se declara el
  resto como no verificado.
- La carga de la máquina mueve los tiempos ±20 % (TASK-016): los tiempos se dan con la carga
  anotada; el tamaño no depende de ella.

## Resultado
<!-- Contrato: PLAN-009 §11 -->
- **Estado final:** `done`. Lo que no cierra esta task: el **cross-check** de Windows/macOS (Paso 7:
  lo dispara el push del coordinador; el `rename` y el lock por modo de compartición en Windows
  siguen sin verificar en una máquina Windows) y el OK del usuario para el tag.
- **Método.** Sandbox `/var/tmp/<sandbox>` (`mktemp -d`), con `HOME`/`XDG_*`/`DEVCTX_HOME`
  propios; modelo `ml-granite` del sandbox de TASK-016 en solo lectura; todo en CPU con
  `nice -n 10`, un repo por vez; repos solo con alias. **v0.9.0** = el binario de TASK-016
  (`git archive v0.9.0`, sha256 `715d42c2…`). **B0** = índice construido **desde cero por v0.9.0**
  en una copia del repo en el commit de TASK-001 (los de TASK-016 ya estaban actualizados en
  sitio; el de frontend se reconstruyó también, porque el índice guarda `repo_path` y el reuso de
  vectores exige la misma ruta). El upgrade corre **en la ruta del B0**, por el serve, bajo una
  TTY real (`script`), así que la compactación automática se dispara de verdad. Tamaños con
  `pragma_database_size()` sobre una copia; tiempos con reloj monotónico; `VmHWM` por proceso
  (`/proc/<pid>/status`, CLI y serve). Carga (loadavg de 1 min) anotada por fila.
- **Binarios.** `fa91a08` (sha256 `4bffea93…`): primera corrida del Paso 2, que mostró
  llamadores perdidos (abajo). `dc0111f` (sha256 `99871577…936bf`): `graph_edges` = extracción de
  0.9.0 copiada; upgrade, EXCEPT ALL, llamadores, memorias. `d3258a9` (sha256 `72949784…6771e55`):
  referencias huérfanas arregladas; restauración del sidecar, MCP, /mnt/c, Paso 3.

### B0 (v0.9.0 desde cero, 2026-10-14/16)

| Repo | s · CPU s · VmHWM | Bloques usados / libres · bytes | Carga inicio → fin |
|---|---|---|---|
| DevCtxEngine | 1 078 · 14 049 · 820 MiB | 107 / 1 · 28 323 840 | 11,9 → 33,9 |
| backend-b | 977 · 12 728 · 703 MiB | 84 / 1 · 22 294 528 | 33,9 → 31,4 |
| legacy-migration | 153 · 1 739 · 815 MiB | 25 / 1 · 6 828 032 | 29,5 → 37,2 |
| backend-a | 3 598 · 57 366 · 795 MiB | 480 / 131 · 160 182 272 | 37,2 → 24,9 |
| frontend | 1 577 · 26 358 · 787 MiB | 281 / 52 · 87 306 240 | 5,6 → 24,2 |

### Paso 2 — upgrade con compactación automática (`dc0111f`, 2026-10-16, carga 8–13)

| Repo | B0 usados / bytes | Tras `--full` | Tras compactar | × B0 usados / bytes | Paso completo · hasta compactar · compactación | VmHWM CLI / serve | Disco extra | Carga |
|---|---|---|---|---|---|---|---|---|
| DevCtxEngine | 107 / 28 323 840 | 164 / 61 MiB | 114 / 29 896 704 | 1,065 / 1,056 | 41,2 · 38,8 · 2,5 s | 149 / 916 MiB | 31,6 MiB | 8,4 → 9,8 |
| backend-b | 84 / 22 294 528 | 111 / 41 MiB | 84 / 22 032 384 | 1,000 / 0,988 | 29,6 · 27,9 · 1,7 s | 141 / 744 MiB | 27,0 MiB | 9,8 → 10,7 |
| legacy-migration | 25 / 6 828 032 | 31 / 13 MiB | 27 / 7 090 176 | 1,080 / 1,038 | 3,2 · 2,6 · 0,6 s | 95 / 151 MiB | 7,0 MiB | 8,4 → 8,4 |
| backend-a | 480 / 160 182 272 | 753 / 286 MiB | 522 / 137 113 600 | 1,087 / 0,856 | 300,5 · 293,1 · 7,3 s | 370 / 1 050 MiB | 156,6 MiB | 12,0 → 7,9 |
| frontend | 281 / 87 306 240 | 450 / 171 MiB | 308 / 80 752 640 | 1,096 / 0,925 | 226,6 · 221,7 · 5,0 s | 276 / 926 MiB | 91,0 MiB | 10,7 → 12,9 |

Meta ≤ 1,30 × `B0` en usados y en bytes: **cumple en los cinco**. Reuso de vectores 97,6–100 %.
Fases de la compactación (backend-a): inventario 0,2 · copia 1,8 · HNSW 4,7 · verificación en
frío 0,2 · contra la fuente 0,3 · sidecar 0,0 · reemplazo 0,1 s (sin FTS: `fts: false`). El
camino sin TTY se midió aparte: con `DEVCTX_NO_AUTO_COMPACT=1` el `--full` deja la nota debida y
termina con la línea `devctx compact`; el `devctx compact` explícito compacta en 0,5–7 s.

### Downgrade: `graph_edges` y llamadores

- **Con `fa91a08`** (fuente con regla 0.9.0, target del extractor 0.10): `lost_callers.py` (v0.9.0
  sobre B0 contra v0.9.0 sobre la base compactada, `impact` a profundidad 1/2/3 por semilla del
  arnés) dio llamadores perdidos en backend-a (13), DevCtxEngine (387) y backend-b (6). Clasificados:
  la mayoría eran falsos positivos de 0.9.0 que 0.10 corrige o renombres por generics; **22
  distintos eran reales** (targets Java con el paquete completo; receptores que 0.9.0 tipaba con su
  mapa plano y 0.10 no). Bug → `dc0111f`: `graph_edges` sale del código de 0.9.0 copiado tal cual
  (`crates/devctx-parse/src/legacy_v090.rs`), sobre el árbol ya parseado; las gramáticas tienen las
  mismas versiones que en v0.9.0. Antes, `01dffb6` (clase anidada) y `fa91a08` (arrows y Go) habían
  corregido casos sueltos; los reemplaza un solo mecanismo.
- **Con `dc0111f`**: EXCEPT ALL de `graph_edges` (las 9 columnas) entre B0 y la base 0.10 tras
  upgrade + compactación: **0 / 0 en los cinco** (857 · 15 643 · 7 765 · 27 022 · 81 811 filas);
  igual por el camino sin TTY + `devctx compact` explícito. `lost_callers.py`: **0 perdidos y 0
  ganados en los cinco** (11 + 6 + 43 + 17 + 35 semillas, 336 consultas). Golden por lenguaje del
  binario v0.9.0 real en `devctx-parse/tests/legacy_v090.rs`.
- **Costo de la pasada 0.9.0** (bench aparte, release, mediana de 3): 0,2–3,0 s por repo, 16–25 %
  del parseo, alrededor del 1–2 % de un `--full`.

### Memorias (legacy-migration, en su ruta; `dc0111f` y `d3258a9`)

- Cuatro memorias locales creadas con **v0.9.0** (tres con `--files`, una con `--topic`). Conteos
  `memories` / `memory_symbol_references` / `sessions` / vectores de memoria: antes 4/8/0/4; tras
  upgrade + compactación automática 4/8/0/4; tras abrirla v0.9.0 (downgrade) 4/8/0/4; sidecar 4/8/0.
  EXCEPT ALL de `memories` (20 columnas), `memory_symbol_references` (7) y `sessions` (6) antes y
  después del upgrade: 0 / 0.
- Las mismas 9 llamadas MCP (`memories_by_symbol` ×5, `memories_by_file`, `recall` ×2,
  `memory_stats`) con v0.9.0 antes, 0.10 después y v0.9.0 en downgrade: respuestas iguales
  (`memory_stats` igual salvo el orden de tipos empatados).
- **Fila 13** (`memories_by_symbol` de `Clase.metodo`, `Clase::metodo`, `modulo.Clase::metodo`,
  `archivo::metodo`): los dos binarios devuelven la memoria correcta (`text-inference`, o
  `junction` para la forma de archivo); el parseo viejo de `Foo.Bar::baz` como archivo no tiene
  efecto observable con datos de campo (cae a la inferencia y encuentra lo mismo). Cumple, sin
  regresión; el caso de parseo lo cubren los tests de TASK-008.
- **Vuelta 0.9.0 → 0.10:** un commit nuevo indexado por v0.9.0 (incremental); 0.10 informa
  `extractor_stale: true` con el `hint` de `devctx index --full`; ese `--full` reusa 313/313 chunks,
  no vuelve a compactar (nota `done`: no es una rama pre-grafo) y deja `extractor_stale: false`;
  memorias intactas.

### Restauración del sidecar (`d3258a9`, sha256 `72949784…`)

Con `dc0111f` el ensayo encontró un bug preexistente (igual en 0.9.0): `forget_memory` (y
`purge_memories_for_project`) dejaban las referencias de la memoria borrada, y el restore las
agregaba otra vez: 4 memorias y **11** referencias en vez de 8. Arreglado en `d3258a9` (borrado
de referencias en una transacción; el restore borra las de las memorias que agrega antes de
insertarlas; tres tests). Ensayo rehecho desde el B0 intacto: sidecar 4/8/0; `memory-forget` →
3/5; `memories import <sidecar>` → `restored 1 · 3 already present · 1 embedded · 3 symbol
references` → **4/8**; un segundo import agrega 0 y 0; `recall` vuelve a traer la memoria. Las
huérfanas que ya existan no se limpian (candidata a migración explícita: PLAN-010 TASK-012).

### MCP durante la compactación, /mnt/c y lock (`d3258a9`)

- Sesión `devctx mcp` viva sobre una copia de backend-a; `devctx compact --force` en paralelo. En
  cuanto aparece `<db>.compact.tmp` (0,25 s), `remember` por el MCP: el primer intento, "connection
  refused" (el serve se está parando); los diez siguientes, "the index … is being compacted
  (`devctx compact`); nothing can open it until that finishes — retry in a moment"; el intento 13
  entra (8,3 s, la compactación terminó a los 5,5 s; el MCP recuerda un fallo de conexión 3 s). La
  memoria queda una sola vez (`memory_stats` total 1) y `recall` la encuentra.
- `devctx compact --dry-run` y `--force` sobre `/mnt/c` (v9fs, WSL): pasan las comprobaciones y
  compactan (0,9 s). Con otro proceso sosteniendo `flock` compartido sobre `<db>.compact.lock`,
  `compact --force` se niega ("the index is in use … retry"), en v9fs y en ext4; `flock`
  exclusivo/compartido no bloqueante desde un segundo proceso, rechazado (errno 11) en los dos.
  Esto prueba el lock entre procesos de WSL; el lock contra procesos de Windows es del cross-check.

### Paso 3 — lo que no debe cambiar (`d3258a9`, 2026-10-16, carga 2–9)

Arnés completo (30 casos, `relevance,impact`, 5 corridas de `impact`), 3 rondas sobre la base
**sin compactar** y 3 sobre la **compactada** (mismos vectores), en la ruta del B0:

| | vector Hit@5 · Hit@10 · MRR | híbrido Hit@5 · Hit@10 · MRR | brief archivo · en el código · símbolo |
|---|---|---|---|
| sin compactar | 22/30 · 24/30 · 0,575 | 23/30 · 24/30 · 0,644–0,647 | 26/30 · 26/30 · 20/24 |
| compactada | 22/30 · 24/30 · 0,575 | 23/30 · 24/30 · 0,647 | 26/30 · 26/30 · 20/24 |

Vectorial idéntico caso por caso en las 6 corridas. Híbrido: 3 casos se mueven una posición
(3↔4, 11↔12) **dentro de las rondas de la misma base** también: empates, no la compactación.
`impact` (p50 / p95, ms, mediana de 3 rondas): backend-a `get` 53/57 sin compactar, 52/60
compactada; `map` 113/132 y 91/108 — meta p95 ≤ 300 ms **cumple**; mismas líneas de salida que en
TASK-016 (8 · 27 · 229 · 22; DevCtxEngine 47 · 207 · 127 · 54 · 46). HNSW presente (`cosine`) en
las cinco bases; FTS apagado en estas configs.

Una primera pasada del arnés sobre copias en **otra ruta** dio `indexed: false` (el índice se
busca por `repo_path`): descartada; no es un fallo del producto, es cómo se mide.

### Tabla de §6 (cierre)

Columnas: medido · commit / sha256 del binario · carga (loadavg 1 min). "TASK-016" = candidato
`e0b8937` + `fb1707c` (sha256 `e3c56562…`), carga 4–42, ver su Resultado.

| Criterio | Medido | Binario · carga | Veredicto |
|---|---|---|---|
| Tipos de arista | 7 en 4 repos; 5 en el Python | TASK-016 | limitación conocida (decisión 2026-10-13) |
| Nodos basura | 0 / 0 / 0 en los 5 | TASK-016 | cumple |
| `calls` sin decidir | 1,6–14,7 %; Rust 16,0 % | TASK-016 | limitación conocida (decisión 2026-10-13) |
| Externos con evidencia | 20/20 muestra | TASK-016 | cumple |
| `from_test` | 0 diferencias en `calls` | TASK-016 | cumple |
| Gold (42 sitios) | 40/42, `high` 40/40 | TASK-016 | cumple |
| Ocurrencias repetidas | una fila por ocurrencia | TASK-016 | cumple |
| `DraftResource` | `BetaService.findPaginated` `high` | TASK-016 | cumple |
| `tokenInterceptor` | la definición | TASK-016 | cumple |
| `impact("get"/"map")` | p95 57/132 ms (sin compactar), 60/108 ms (compactada) | `d3258a9` (`72949784…`) · 2–9 | cumple |
| `external` calificado | `external` + `called_from` | TASK-016 | cumple |
| `Foo::new` | no externo, sugerencias | TASK-016 | cumple |
| `memories_by_symbol` | misma memoria correcta en v0.9.0 y 0.10 | `dc0111f` (`99871577…`) · 7–13 | cumple (parseo cubierto por tests) |
| `repo_map` | ≤ 1 024 tokens; foco 7/7 | TASK-016 | cumple |
| Relevancia del brief | 26/30 actualizado (+6,7 pp), 27/30 nuevo | `d3258a9` · 2–9 (actualizado); TASK-016 (nuevo) | limitación conocida (decisión 2026-10-13) |
| `search --hybrid` | 23/30, MRR 0,647 | `d3258a9` · 2–9 | cumple |
| `--mode vector` | idéntico sin compactar / compactada; idéntico a 0.9.0 en TASK-016 | `d3258a9` · 2–9 | cumple |
| Contratos JSON | solo aditivos | TASK-016 | cumple |
| `index --full` desde cero | −8,7 % en suma | TASK-016 | cumple |
| `--full` tras el extractor | 7,1–18,7 % | TASK-016 | cumple |
| Tamaño del DB | desde cero +4–17 % (TASK-016); upgrade + compactación ×1,00–1,10 usados, ×0,86–1,06 bytes | `dc0111f` · 8–13 | cumple |
| Pico de RAM (frontend) | +101/+139 MiB al indexar; upgrade 926 MiB serve | TASK-016; `dc0111f` · 11–13 | cumple |
| Índice viejo | `extractor_stale` + aviso | TASK-016; vuelta 0.9.0→0.10 con `dc0111f` | cumple |

### Hallazgos

1. **Bug de downgrade, cerrado:** mezclar fuente 0.9.0 con target 0.10 perdía llamadores reales;
   `graph_edges` ahora es la extracción de 0.9.0 (`dc0111f`), con EXCEPT ALL 0/0 y 0 llamadores
   perdidos en los cinco.
2. **Referencias huérfanas de memorias** (preexistente desde 0.9.0), cerrado en `d3258a9`; la
   limpieza de las existentes queda como candidata en PLAN-010 TASK-012.
3. **Incidente de medición — hooks de git en las copias:** las copias de repos traían el
   `post-commit` de devctx que llama al binario instalado del usuario. Un `git commit` en una copia
   del sandbox lo disparó y levantó un serve de ese binario sobre la copia (con el HOME del
   sandbox); se paró con `serve --stop`, la corrida se descartó y se rehízo. Desde ahí todas las
   copias tienen `core.hooksPath` apuntando a una carpeta vacía. Nada se escribió en `~/.local`
   ni en `~/.config`. Para las próximas mediciones: desactivar los hooks al copiar.
4. **Test intermitente:** `search_shapes_cli::group_search_keeps_the_hits_of_a_member_answered_from_a_fallback_branch`
   falló 1 vez en el gate de `dc0111f` (`fallback not reported`, con la máquina cargada) y pasó
   5/5 en dos re-corridas seguidas y en los gates siguientes. Anotado; no se tocó.
5. Una copia en otra ruta se ve como no indexada (`repo_path`) y no reusa vectores (ya en el
   hallazgo 6 de TASK-016); las mediciones de esta task se hicieron en la ruta del B0.

- **Commits de esta task:** `5a6aa76` (0.10.0), `dbd1892` y `2db7062` (docs del Paso 1),
  `01dffb6`, `fa91a08`, `dc0111f` (`graph_edges`), `45650a5` (docs del downgrade), `d3258a9`
  (referencias de memorias), `cfda5df` (PLAN-010) y el de este cierre (docs).
- **Archivos tocados:** `Cargo.toml`, `Cargo.lock`, `crates/devctx-parse/src/{legacy_v090.rs,lib.rs,parser.rs,types.rs}`,
  `crates/devctx-parse/tests/{legacy_v090.rs,golden/legacy_v090.tsv,fixtures/legacy_v090/**}` y
  comentarios de otros tests, `crates/devctx-index/src/{pipeline.rs,lib.rs}`,
  `crates/devctx-store/src/{memory.rs,compact.rs,compact_tests.rs}`,
  `crates/devctx-cli/tests/compact_cli.rs`, `CHANGELOG.md`, docs 03 (symbol-graph, mcp-integration),
  07, 08, 13 (EN/ES), este archivo, el master y PLAN-010 TASK-012.
- **Verificado por:** gates completos (fmt, clippy con y sin `gpu`, `cargo test --workspace --locked
  -j 8 -- --test-threads=8`) en cada commit de código: 1 318 → 1 325 pasan, 0 fallan (salvo el
  intermitente de arriba, re-corrido); barrido de privacidad ampliado antes de cada commit.
- **No verificado:** cross-check de Windows/macOS (push del coordinador); lock y `rename` contra
  procesos de Windows; el caso de una base con **dos ramas** del Paso 5 (los B0 de esta task tienen
  una rama cada uno; la regla de la nota por rama la cubren los tests 20-24 de TASK-018); GPU;
  tiempos con la máquina libre. El Paso 4 no repitió los 60 casos de TASK-016 uno por uno: los
  reemplaza la comparación de la tabla entera (EXCEPT ALL) y de los llamadores de cada semilla a
  profundidad 1-3, que los incluye.
