# PLAN-007 — Planes en la raíz del workspace, y un parser que aguanta planes reales

**Fecha:** 2026-10-01
**Fase:** 2 (Ejecución) — aprobado por el usuario el 2026-10-01. Rama `feature/plans-workspace-root` (sale de
`main` = v0.7.0, `f979582`, árbol limpio)
**Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
**Origen:** En el setup real del usuario (`/home/you/acme`), `plan_status` por MCP y la pestaña
"Plans" del dashboard no ven los planes, y aunque los vieran mostrarían basura: 1592 warnings.

---

## 1. Qué resuelve

Dos problemas independientes que se tapan uno al otro. Arreglar solo el primero entrega un dashboard
que lee el directorio correcto y muestra 94 planes en 0 % — peor que no mostrar nada, porque parece
información.

### 1.1 Problema 1 — la raíz equivocada

`/home/you/acme` es un **workspace**: no es proyecto devctx (no tiene `.devctx/`), contiene
13 repos registrados con `group: acme`, y los planes viven en la raíz del workspace:
`/home/you/acme/plans` (136 entradas; 128 directorios `PLAN-<n>*`, 1174 archivos de task).
**Ningún sub-repo tiene `plans/`.**

| Superficie | Raíz que usa hoy | Resultado en `~/acme` |
|---|---|---|
| CLI `devctx plan-status` | `plan_status_root()` (`crates/devctx-cli/src/main.rs:2013`): raíz del proyecto si el cwd está en uno, si no el cwd | Funciona desde `~/acme`. **Falla desde dentro de un miembro** (`~/acme/backend-a`): lee `backend-a/plans`, que no existe |
| Tool MCP `plan_status` | `backend_for` (`crates/devctx-mcp/src/lib.rs:492`) → en `Binding::Group` (`lib.rs:371`) cae al miembro `default` (`lib.rs:516-521`) → `Backend::plan_status` (`crates/devctx-mcp/src/backend.rs:227`) → `do_plan_status(state)` usa `state.root` (`crates/devctx-mcp/src/state.rs:720-721`) o, en `Remote`, `GET /plans/status` al daemon del miembro, cuya raíz también es la del miembro | `{"plans":[]}` — lee `<miembro>/plans` |
| Dashboard `devctx web` | `cmd_web` (`main.rs:1318`) llama `load_project()` (`main.rs:3429`), que exige `.devctx/` | **Ni arranca** en `~/acme` ("No DevCtxEngine project found") |
| Rutas `/plans/status`, `/plans/graph` | `crates/devctx-api/src/lib.rs:59-60` → `do_plan_status`/`do_plan_graph` (`state.rs:720`, `state.rs:905`) sobre `state.root` | La raíz del miembro |
| `memories_by_file` → `plan_tasks` | `plan_tasks_for_file(&state.root, …)` (`state.rs:3126`, def. `state.rs:3134`) | Siempre vacío en un miembro del workspace |
| `serve --central` | `crates/devctx-api/src/central.rs:40-54`: no sirve dashboard ni rutas `/plans/*` | No aplica |

Lo bueno: `plan_status_value(root, plan)` (`state.rs:729`) y `plan_graph_value(root, plan)`
(`state.rs:911`) **ya reciben la raíz explícita** — la costura existe; lo que falta es decidir *qué*
raíz pasarles. `AppState.root` es privado (`state.rs:163`, calculado en `state.rs:196`).

### 1.2 Problema 2 — el parser se ahoga con planes reales

Medido el 2026-10-01 con `devctx 0.7.0` (`~/.local/bin/devctx`), `devctx plan-status --format json`
desde `~/acme`, más `plan-status PLAN-N --format json` por cada plan para clasificar warnings:

| Métrica (baseline) | Valor |
|---|---|
| Planes detectados | 128 |
| Tasks contadas (suma de `total`) | 1160 (de 1174 archivos: 14 con nombre no reconocido) |
| Planes con al menos un warning | 113 |
| Warnings del parser (suma de `warnings_count`) | **1592** |
| Warnings por plan incl. análisis (missing + ciclos) | 1618 |
| Tasks `done` | 176 |
| Planes con tasks y **0 done** | 94 |
| Títulos de plan que empiezan con `:` | 76 |

Clasificación de los 1618:

| Warnings | Categoría | Causa raíz |
|---|---|---|
| 720 | `falta el campo Estado` | El campo se llama `**Status:**` en inglés (647 tasks), está en front matter YAML (`status: done`), en una sección `## Estado` con el valor en la línea siguiente, o en una línea con varios campos separados por `·` (`**Specialist**: x · **Status**: done`). El parser solo reconoce la clave `estado` (`crates/devctx-core/src/plans.rs:379`) y solo con bold (`parse_field_line`, `plans.rs:152`) |
| 646 | `tabla dice X, archivo dice Y` | Consecuencia de lo anterior (archivo = pending por defecto), y `parse_table_status` (`plans.rs:435`) lee **cualquier** fila que empiece con un id de task y toma la **última celda** — en PLAN-129 levantó una tabla de rollback: `tabla dice unknown(Revertir el commit)` |
| 153 | `Depende de: token no reconocido` | `parse_depends_on` (`plans.rs:225`) avisa por cada palabra de prosa (`'el'`, `'A)**'`, `'**DD-T9**'`) y además **inventa** dependencias: `Fase 0` → `TASK-000`, `TASK-033/034/035` → `TASK-33034035` |
| 50 | `Estado no reconocido` | `Status::parse` (`plans.rs:30`) mira solo el primer token: `✅ **\`done\`** — …`, `🟢 \`pending\``, `sigue \`in_progress\`.`, `Pendiente`, `PENDING`, `completed`, `\`skipped\`` |
| 14 | `nombre de archivo de task no reconocido` | `TASK-R01-…` (PLAN-078, 10), `TASK-DB-001-…` (PLAN-051), `T032-00N-…` (PLAN-032, 3) |
| 13 | `depende de TASK-N, que no existe` | Las dependencias inventadas de arriba y referencias cruzadas `` `PLAN-120` TASK-031 `` |
| 13 | `ciclo de dependencias` | `TASK-001b` se normaliza a `TASK-001` (`extract_task_id_from_filename`, `plans.rs:673`): **22 archivos con sufijo de letra colapsan sobre el id base**, y `TASK-001b → TASK-001` se lee como auto-ciclo |
| 9 | `múltiples documentos de plan` | `pick_plan_doc` (`plans.rs:477`) no filtra por prefijo `PLAN-<n>`: en PLAN-013 eligió `BRIEF-CLAUDE-DESIGN-v5.1.md` |

Nota: PLAN-129 **no** necesita renombre. Su directorio es `PLAN-129-tickets-microservicio` y
`PLAN-129-tickets-microservicio.md` coincide con el nombre del dir, que `pick_plan_doc` prefiere.

### 1.3 Proyección medida del parser tolerante

Antes de escribir este plan se prototipó la regla de DD-4…DD-7 en un script desechable (fuera del
repo) y se corrió contra el corpus de `~/acme/plans`:

| Métrica | Baseline | Prototipo |
|---|---|---|
| Warnings del parser (sin ciclos, sin análisis) | 1592 | **126** |
| Planes con warnings | 113 | 31 |
| Tasks resueltas (`done` + `skipped`) | 176 | 600 |
| Planes con tasks y 0 resueltas | 94 | 32 |

Lo que queda en esos 126 es **legítimo** y debe seguir avisándose (§6).

## 2. Hallazgos verificados (2026-10-01, rama `feature/plans-workspace-root`)

Todas las referencias `archivo:línea` de §1 se re-verificaron en esta rama (HEAD `f979582`). Además:

- `DevctxServer` guarda `cwd` (`lib.rs:406`, se fija en `lib.rs:448`). En modo grupo el binding
  nace del descenso en `cmd_mcp` (`main.rs:1363`, rama `Resolution::Group` alrededor de
  `main.rs:1416-1443`) sobre ese mismo cwd — **el cwd del MCP es la raíz del workspace**.
- `Binding::Group` no guarda la raíz desde la que se resolvió; `projects_under`/`resolve_under`
  (`state.rs:1342`, `state.rs:1462`) sí la reciben.
- `fit_plan_status_budget` (`state.rs:866`) es privada: el MCP no puede hoy calcular un
  `plan_status` en proceso con presupuesto sin pasar por un backend.
- El registry de esta máquina: los 13 miembros de `acme` son hijos directos de `/home/you/acme`.
- Test que este plan puede romper sin querer: `table_output_on_the_real_repo_plans_fits_the_hook_budget`
  (`crates/devctx-cli/tests/plan_status_cli.rs:124`) exige que `devctx plan-status` sobre los planes
  **de este repo** quepa en 600 bytes. **Medido: sin PLAN-007 son 485 bytes; con PLAN-007 (que pasa
  a ser el plan activo y lista 6 bloqueadas) son 723.** Ese test falla en cuanto este plan entra al
  repo, antes de tocar una línea de código.

## 3. Decisiones

### DD-1 — Opción 3: el workspace manda en modo grupo; el proyecto, si no

Decidido por el usuario. Rechazadas: `devctx init` en la raíz del workspace (rompe el binding de
grupo de toda sesión, y un índice ahí indexaría el monorepo entero) y symlinks.

### DD-2 — Un solo resolvedor de raíz, puro, en `devctx-core`

`devctx_core::plans::resolve_plans_root(project_root: Option<&Path>, workspace: Option<&Path>,
group_declared: bool) -> PlansRoot { root, source }`, sin I/O más allá de `is_dir`. Precedencia:

1. `workspace` (raíz del descenso de grupo, o cwd sin proyecto) si contiene `plans/`.
2. `project_root` si contiene `plans/`.
3. Si el proyecto declara `group:`: el ancestro más cercano de `project_root`, **estrictamente por
   debajo de `$HOME`**, que tenga `plans/` con al menos un `PLAN-<n>*`.
4. Si nada: `project_root` (o `workspace`) — mismo comportamiento que hoy, lista vacía.

`source` (`workspace` | `project` | `ancestor` | `none`) y `root` se agregan al JSON de
`plan_status` (campo nuevo `plans_root`), para que nadie vuelva a preguntarse de dónde salieron.

| Alternativa | Por qué no |
|---|---|
| Solo arreglar el MCP (`self.cwd` en modo grupo) | Deja el dashboard, el daemon y `plan_tasks` mirando la raíz del miembro; el CLI desde un miembro sigue fallando |
| Ancestro común de los miembros del grupo (registry) | Exige leer el registry/daemon central desde el CLI del hook, que hoy no toca store ni daemon a propósito (`main.rs:2022-2024`, doc de `cmd_plan_status`) |
| Walk-up sin condición | Un proyecto suelto bajo un dir con `plans/` ajenos los adoptaría. El paso 3 exige `group:` declarado y para antes de `$HOME` |
| Override por env (`DEVCTX_PLANS_ROOT`) | Hay que fijarlo en el proceso del daemon, no del cliente (misma trampa que `DEVCTX_MAX_OUTPUT_TOKENS`). Fuera de alcance hasta que haga falta |

Tradeoff aceptado: el paso 3 hace que un proyecto del grupo bindeado **solo** (no en modo grupo)
también vea los planes del workspace. Es lo que se quiere en acme (ningún miembro tiene `plans/`
propio); si un miembro tuviera los suyos, el paso 2 gana.

### DD-3 — `AppState` guarda `plans_root`; el daemon resuelve solo

`AppState` calcula `plans_root` al construirse (`state.rs:196`) con DD-2 (`workspace = None`,
`group_declared = !cfg.project.group.is_empty()`). `do_plan_status`, `do_plan_graph` y
`plan_tasks_for_file` usan `plans_root`, no `root`. Consecuencia: el daemon de cualquier miembro de
acme ya sirve los planes del workspace, y **`Backend::Remote` no necesita cambios**.

### DD-4 — El MCP en modo grupo o sin binding lee en proceso

En `Binding::Group` sin hint `project`, y en `Binding::None` cuando `cwd/plans` existe, la tool
`plan_status` calcula en el propio proceso MCP con `plan_status_value(root, plan)` + presupuesto
(`fit_plan_status_budget` pasa a `pub`), con `workspace = self.cwd`. No hace round-trip al daemon:
los planes son markdown, no tocan el store. Esto no depende de que el daemon del miembro esté en la
versión nueva (lección de PLAN-002: un proceso viejo sigue sirviendo su binario).

### DD-5 — `devctx web` desde un workspace usa el mismo descenso que `devctx mcp`

Sin flag nuevo. Si `load_project()` falla, `cmd_web` corre `resolve_under(cwd)` (extraído de
`cmd_mcp` a un helper compartido): `Single` → ese proyecto; `Group` → el miembro más recientemente
indexado (misma regla que el MCP); otro caso → el error de hoy, ahora con el porqué de
`why_unbound`. El dashboard se sirve con el `AppState` (o reusando el daemon) de ese miembro, y por
DD-3 su pestaña Plans lee el workspace. Las demás pestañas muestran el miembro default — se dice en
el banner de arranque.

Alternativa rechazada: un modo "solo planes" del dashboard sin proyecto. Duplica el arranque del
servidor por una pestaña y deja las otras rotas.

### DD-6 — `Estado` tolerante: buscar el estado, no adivinarlo

- **Dónde se busca**, en orden: (a) campo del encabezado con clave `estado`, `status` o `state` —
  con o sin bold, con los dos puntos dentro o fuera (`**Estado:**`, `**Estado**:`, `**Estado: x**`,
  `Status: x`), y partiendo la línea por ` · ` cuando trae varios campos; (b) front matter YAML
  (`status:`); (c) sección `## Estado` / `## Status`, primera línea no vacía. `Estado final`,
  `Estado objetivo` y similares **nunca** cuentan: la clave debe ser exacta.
- **Cómo se lee el valor**: primero el primer span entre backticks que sea una palabra clave; si no,
  la primera palabra clave del texto ya sin emoji, `*` ni puntuación. Sinónimos ES+EN sin tildes y en
  minúscula: `pending/pendiente/todo`; `in_progress/in-progress/en progreso/en curso/wip`;
  `done/completed/complete/completada/hecho/hecha/terminada/cerrada/implemented/implementada/ejecutada/merged`;
  `blocked/bloqueada`; `skipped/omitida/descartada/cancelled/cancelada/superseded`. `✅` sin palabra
  clave → `done`.
- **Nuevo `Status::Skipped`**: cuenta como resuelta para dependencias, no aparece en ready ni en
  blocked. El listado agrega `skipped` y el progreso del dashboard usa `done + skipped`. Cambio
  aditivo en el JSON (`{"kind":"Skipped"}`).
- Lo que no contiene ninguna palabra clave (`código aplicado en working tree, sin commit…`,
  `ready-for-approval`) sigue siendo `Unknown` + warning. **Warning, no adivinanza** (PLAN-005).
- Campo ausente → `pending` + warning, igual que hoy. *Abierto (Q-2)*: tomar el estado de la tabla
  del plan cuando el archivo no lo tiene. El prototipo mide que solo salva 4 warnings.

### DD-7 — `Depende de`, ids y tabla

- **Claves**: `depende de`, `depends on`, `dependencies`, `dependencias`, `bloqueada por`,
  `blocked by`, y `depends_on` en front matter.
- **Valor**: si arranca con `—`, `-`, `–`, `ninguna`, `none`, `n/a`, `[]` → ninguna (regla de
  PLAN-005, intacta). Si no, se extraen **solo** los tokens `TASK-<id>`; la prosa alrededor se ignora
  sin warning. Rangos `TASK-001..004` y `TASK-012 … TASK-018` se expanden. Números pelados solo si el
  valor entero es una lista de números. Una auto-dependencia se descarta.
- **Referencias cruzadas** (`PLAN-120/TASK-031`, `` `PLAN-120` TASK-031 ``) se reportan como
  `external_deps` y **no bloquean ni cuentan como missing**: resolverlas exigiría cargar el otro plan
  y entra en fase 2.
- Valor sin ningún id ni marca de "ninguna" (`Fase 0`, `todas`, `Waves 1-4 completas`) → warning
  único `Depende de sin TASK reconocible`, sin inventar `TASK-000`.
- **Ids de task**: `TASK-<n>[letra]` conserva el sufijo (`TASK-001b` ≠ `TASK-001`); se aceptan
  `TASK-R01` y `TASK-DB-001`. Dos archivos con el mismo id → warning `id duplicado` (PLAN-047 tiene
  `TASK-001-DESIGN.md` junto a `TASK-001-verify-….md`). `T032-001` sigue sin reconocerse (warning).
- **Tabla del plan**: solo filas de una tabla cuya fila de encabezado tenga una columna
  `Estado`/`Status`/`State`, y el valor se lee de **esa** columna, no de la última. Ids tachados
  (`~~TASK-032~~`) sin estado legible → `skipped`.
- **Documento del plan**: `pick_plan_doc` prefiere `<dir>.md`; si no, solo candidatos `PLAN-<n>*.md`
  sin `-design.md`; recién después cualquier `.md`.
- **Título**: `h1_title_strip_plan_prefix` también quita `:` y cualquier prefijo `PLAN-<n>` aunque
  el número del H1 no coincida con el del dir (PLAN-008 tiene H1 `PLAN-1:`).

Todo sigue en `devctx_core::plans`, **sin dependencias nuevas** y línea por línea (restricción de
PLAN-005 §2.3: nada de `regex`).

## 4. Fuera de alcance

- Escribir estados desde devctx, o "arreglar" los markdown de acme: este plan **no toca
  `/home/you/acme`**.
- Resolver dependencias entre planes (`external_deps` solo se reporta).
- Planes cuyo directorio no tiene número (`plans/PLAN-CALIDAD-cierre`): siguen ignorados, como hoy.
- Override de raíz por variable de entorno o flag (`DEVCTX_PLANS_ROOT`, `--plans-root`).
- Cachear el parseo (128 planes / 1174 archivos por llamada: medir primero en TASK-008).
- Que el dashboard en modo workspace muestre el grafo/memorias de **todos** los miembros.

## 5. Tasks y orden

| TASK | Título | Depende de | Estado |
|------|--------|------------|--------|
| TASK-001 | Parser: `Estado` tolerante, `Status::Skipped` y títulos | — | `done` |
| TASK-002 | Parser: ids con sufijo, `Depende de` por extracción, tabla con columna de estado | TASK-001 | `done` |
| TASK-003 | Resolvedor `resolve_plans_root` y `AppState.plans_root` (daemon, CLI, `plan_tasks`) | — | `done` |
| TASK-004 | MCP: `plan_status` en proceso en modo grupo y sin binding | TASK-003 | `done` |
| TASK-005 | `devctx web` desde un workspace (descenso compartido con `devctx mcp`) | TASK-003 | `done` |
| TASK-006 | Tests de integración: workspace sin init + corpus de fixtures reales | TASK-002, TASK-004, TASK-005 | `done` |
| TASK-007 | Docs EN + ES: formato tolerante y raíz de planes | TASK-002, TASK-005 | `done` |
| TASK-008 | Verificación final contra `~/acme` con antes/después medido | TASK-006, TASK-007 | `done` |

(La columna `Estado` va última a propósito: el parser actual lee la última celda de la fila como
estado, y este documento tiene que parsear sin warnings **antes** de TASK-002.)

**Paralelizables:** TASK-001 y TASK-003 arrancan juntas (archivos distintos: `plans.rs` vs
`state.rs`/`main.rs`). TASK-002 va después de TASK-001 (mismo archivo). TASK-004 y TASK-005 salen
juntas tras TASK-003 (tocan `lib.rs` del MCP vs `main.rs` del CLI; el helper de descenso lo extrae
TASK-005 y TASK-004 no lo necesita).

## 6. Criterios de aceptación globales (medidos en TASK-008)

Sobre `~/acme/plans`, comparados con el baseline de §1.2:

| Métrica | Baseline | Meta |
|---|---|---|
| Warnings del parser (suma de `warnings_count`) | 1592 | **≤ 150** |
| Planes con warnings | 113 | ≤ 35 |
| Tasks resueltas (`done` + `skipped`) | 176 | ≥ 580 |
| Planes con tasks y 0 resueltas | 94 | ≤ 35 |
| Títulos que empiezan con `:` | 76 | 0 |
| Ids de task colapsados por sufijo de letra | 22 archivos | 0 |
| Dependencias inventadas (`TASK-000` ×7, `TASK-33034035` ×2) | 9 | 0 |

**Lo que legítimamente queda** (proyección del prototipo, a re-medir):

- ~42 tasks **sin ningún campo de estado** (PLAN-052 ×20, PLAN-078 ×9, PLAN-037 ×6, PLAN-080 ×4,
  PLAN-028, PLAN-047).
- ~45 desacuerdos **reales** tabla vs archivo (PLAN-128 ×11, PLAN-121 ×9, PLAN-123 ×7, PLAN-120 ×5,
  PLAN-132 ×4, PLAN-085 ×3, …): las tablas quedaron viejas; es justo lo que el warning existe para
  señalar.
- ~26 `Depende de` sin ningún id (`Fase 0`, `todas`, `ALL previous tasks`, `Waves 1-4 completas`,
  `` `DP-8` ``, un hash de commit, el nombre de una rama).
- 4 ids duplicados en PLAN-047 (documentos compañeros `TASK-NNN-DESIGN.md`, `-FINDINGS.md`,
  `-AUDIT.md`, `-CODE-REVIEW.md`).
- 3 nombres `T032-00N-*.md` (PLAN-032) y 3-4 directorios con varios `.md` candidatos a plan.
- 2 estados sin palabra clave (`ready-for-approval`, prosa).

Y en el workspace:

- `plan_status` vía MCP lanzado desde `~/acme` (binding de grupo) lista los 128 planes, con
  `plans_root.source = "workspace"`.
- `devctx plan-status` desde `~/acme/backend-a` devuelve lo mismo que desde `~/acme`
  (`source = "ancestor"`).
- `devctx web` arranca desde `~/acme` y `GET /plans/status` lista los 128 planes.
- Este repo (`DevCtxEngine`) sigue leyendo **sus** planes (`source = "project"`), sin cambios.

## 7. Riesgos

- **Sinónimos demasiado generosos.** `cerrada` o `implemented` pueden aparecer en prosa del valor
  (`implemented — pending deploy`). Mitigación: el backtick gana sobre la prosa, y la primera palabra
  clave gana sobre las siguientes; TASK-001 fija los casos con fixtures reales. Si una palabra genera
  falsos `done`, se saca de la lista — nunca se agrega una heurística encima.
- **Extraer ids de prosa puede crear aristas falsas.** `Fase 0 (y coordinar orden con TASK-006…)`
  pasa a depender de TASK-006. Se acepta: era 100 % warning y 0 % dependencia; ahora es una arista
  plausible, y el dashboard la muestra.
- **`Status::Skipped` cambia el contrato JSON** (variante nueva en `kind`) y el conteo de progreso.
  El único consumidor estructural es `assets/index.html` (colores por `status`, `index.html:329-332`)
  y el hook `~/.claude/hooks/devctx-session-start.sh` (fuera del repo). TASK-007 lo documenta.
- **El walk-up de DD-2 paso 3** podría adoptar un `plans/` ajeno en un ancestro. Acotado por
  `group:` declarado, por exigir `PLAN-<n>*` dentro, y por parar antes de `$HOME`.
- **Daemons viejos.** Un daemon de un miembro levantado con 0.7.0 sigue sirviendo la raíz del
  miembro hasta reiniciarse (`devctx serve --stop`). DD-4 hace que el MCP en modo grupo no dependa
  de eso; el dashboard sí.
- **Rendimiento**: leer 1174 archivos por llamada. No medido aún; TASK-008 lo mide con `time`. Si
  pasa de ~300 ms, cachear por mtime va a un plan aparte.
- **Test del presupuesto del hook** (`plan_status_cli.rs:124`, ≤ 600 bytes sobre los planes de este
  repo): medido 485 → 723 bytes con PLAN-007 presente. **Falla desde el primer commit de este plan.**
  El test ata un presupuesto a datos vivos del repo; TASK-006 lo pasa a un fixture propio (o al
  bloque que el hook realmente inyecta) — sin recortar planes. Ver Q-1.

### Decisiones del usuario (2026-10-01) — plan APROBADO para ejecución

- **Q-1 → fixture propio.** El test del presupuesto del hook mide contra un fixture fijo, no contra
  los planes vivos del repo.
- **Q-2 → no.** El estado faltante no se llena desde la tabla (recomendación aceptada por defecto).
- **Q-3 → sí, acotado por `group:`** (recomendación aceptada por defecto).
- **Q-4 → `implemented`, `cerrada`, `merged`, `done`/`hecho` cuentan como `done`.**
  `ready-for-approval` queda `Unknown`.
- **Q-5 → sí.** `skipped` resuelve dependencias.

## 8. Verificación

- `cargo test -p devctx-core plans` — fixtures por cada variante de §1.2 (TASK-001, TASK-002).
- `cargo test -p devctx-mcp` y `-p devctx-cli --test plan_status_cli --test mcp_tools --test
  mcp_binding` — workspace sin init (TASK-006).
- Corpus: `devctx plan-status --format json` desde `~/acme` antes y después, con los mismos `jq` de
  §1.2 (TASK-008).
- `devctx plan-status PLAN-007 --format json` desde la raíz de este repo: 0 warnings (ya verificado
  al escribir este plan con el binario 0.7.0).

Nada de esto se corrió todavía salvo la medición del baseline y el prototipo: es un plan. **No se
compiló ni se testeó.**

## 9. Rollback

Cada task es un commit convencional propio. El parser (TASK-001/002) y la raíz (TASK-003…005) son
independientes: se puede revertir uno sin el otro. Revertir TASK-003 devuelve `state.root` como raíz
de planes y deja TASK-004/005 sin efecto visible (siguen compilando: el resolvedor devuelve la raíz
del proyecto). No hay migración de datos: los planes nunca se persisten (PLAN-005 §3), así que no
queda nada que limpiar en ningún store.

## 10. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
