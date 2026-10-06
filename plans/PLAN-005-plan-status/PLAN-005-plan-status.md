# PLAN-005 — `plan_status`: devctx lee los planes del repo en vez de que el modelo los recuerde

**Fecha:** 2026-09-11
**Fase:** 1 (Planificación) — pendiente de aprobación. Rama `feature/plan-status` (sale de `fix/mcp-integration`, 2 commits sin compilar)
**Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
**Origen:** Tras una compactación o un reinicio, el modelo pierde el hilo de qué task del plan activo sigue.

---

## 1. Qué resuelve

Los planes ya son un grafo. Cada task declara sus aristas y su estado en el encabezado — por ejemplo
`plans/PLAN-002/tasks/TASK-003-descenso-al-arrancar.md:6-7`:

```markdown
- **Depende de:** TASK-001, TASK-002
- **Estado:** `done`
```

Pero nadie lo lee como grafo. Después de compactar, la única forma de saber "qué task está lista para
arrancar" es que el modelo abra el plan, abra cada task, cruce las dependencias a mano… o que se
acuerde. No se acuerda: el hook `SessionStart` (`~/.claude/hooks/devctx-session-start.sh`) hoy
inyecta `devctx status` y, tras compactar, un `recall` de memorias recientes — **nada del plan en curso.**

La información está en disco, versionada en git. Falta una función que la lea y conteste.

## 2. Hallazgos verificados (2026-09-11)

Contra los 4 planes existentes en `plans/` y el código en `fix/mcp-integration` (HEAD `c7d0b82`).

### 2.1 La tabla del plan y el archivo de la task YA se contradicen

| Task | Tabla del plan dice | Archivo de la task dice |
|---|---|---|
| PLAN-002 TASK-010 | `done` (`PLAN-002-mcp-auto-bind-por-path.md:103`) | `pending` (`tasks/TASK-010-fanout-codigo.md:7`) |
| PLAN-002 TASK-011 | `done` (`PLAN-002-…md:104`) | `pending` (`tasks/TASK-011-fanout-memoria.md:7`) |
| PLAN-003 TASK-005 | `pending` (`PLAN-003-…lenguajes.md`, tabla §3) | `done` (`tasks/TASK-005-retractar-la-afirmacion-falsa.md:7`) |

Dos copias del mismo dato, escritas a mano, ya divergieron en 3 de 22 tasks. **Es el argumento
contra agregar una tercera copia en DuckDB** (§3), y define la regla: **el archivo de la task manda;
la tabla del plan solo se usa para advertir la divergencia.**

### 2.2 Variaciones de formato reales (el parser tiene que tolerarlas todas)

| Variación | Dónde |
|---|---|
| Encabezado `**Estado**: done` (dos puntos fuera del bold, sin backticks, sin viñeta) | PLAN-001 tasks (`TASK-001-observable-progress-in-appstate.md:3-6`) |
| Encabezado `- **Estado:** \`done\`` (viñeta, dos puntos dentro, backticks) | PLAN-002 y PLAN-003 tasks |
| PLAN-001 no tiene `Especialista` ni `Proyecto`; tiene `Modelo sugerido` | PLAN-001 tasks |
| `Estado` con texto de cola: `done — con un riesgo aceptado que después explotó…` | `PLAN-001-index-progress-through-the-server.md:3` |
| Un segundo `Estado final:` dentro de `## Resultado` | 14 tasks de PLAN-002/003 |
| `Depende de: — (primera del plan)` y **`— (paralela a TASK-001)`**: guion + un TASK que NO es dependencia | `PLAN-002/tasks/TASK-001…md:6`, `TASK-002-enum-binding.md:6` |
| Rango `TASK-001..004` y números pelados `001, 002` | Solo en tablas de plan (PLAN-003 §3, PLAN-002 §3), no en tasks |
| Directorio sin slug (`PLAN-001/`, `PLAN-002/`) y con slug (`PLAN-003-grafo-…/`) | `plans/` |
| Dos `PLAN-003-*.md` en el mismo dir, **ambos con H1 `# PLAN-003 —`** (`-design.md` y el plan) | `plans/PLAN-003-grafo-y-registro-de-lenguajes/` |
| Plan sin `tasks/` (tasks solo en la tabla, con ids `1`, `2`, `3`) | `plans/PLAN-004-mcp-integration/` |
| Valores de `Estado` observados: **solo `done` y `pending`** | Los 22 archivos de task |
| Referencias a archivo: rutas completas (`crates/devctx-mcp/src/state.rs`), con línea (`lang.rs:177`), peladas (`state.rs`, `lib.rs`) y relativas (`../VERIFICACION.md`) | tasks de PLAN-002/003 |

### 2.3 El crate correcto para el parser es `devctx-core`

`crates/devctx-core/src/lib.rs:3-4`: *"This crate is dependency-light on purpose: every other crate
builds on top of it."* Sus deps son `anyhow`, `thiserror`, `serde`, `serde_yaml`
(`crates/devctx-core/Cargo.toml`). `devctx-mcp`, `devctx-api` y `devctx-cli` ya dependen de él.

`devctx-parse` tiene `regex` pero arrastra 7 gramáticas tree-sitter: no tiene sentido que el CLI del
hook o el parser de markdown dependan de eso. Un crate nuevo sería un miembro más del workspace para
~300 líneas sin dependencias. **Módulo nuevo `devctx_core::plans`, sin deps nuevas** (parseo a mano,
línea por línea; no hace falta `regex`).

### 2.4 Cómo se cablea una tool hoy

- Registro: `#[tool_router] impl DevctxServer` (`crates/devctx-mcp/src/lib.rs:558`), un
  `#[tool(description = …)]` por método. Ejemplo mínimo: `index_status` (`lib.rs:627-633`).
- Resolución de proyecto: `backend_for(hint)` (`lib.rs:480`) — hint `project` → ese repo; si no,
  el binding; grupo → miembro default anotado con `resolved_project` (`annotate`, `lib.rs:528`).
- Local vs daemon: `Backend::Local(s) => do_x(s, …)` / `Backend::Remote(r, _) => r.get("/x")`
  (patrón en `crates/devctx-mcp/src/backend.rs:419-438`). Por eso **toda tool nueva necesita también
  su ruta HTTP** en `crates/devctx-api/src/lib.rs:42-68`, o se rompe cuando hay daemon.
- Presupuesto de salida: `env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS)` con
  `CHARS_PER_TOKEN = 4` y default 8000 (`crates/devctx-mcp/src/state.rs:501-519`). Ambos son privados
  de `state.rs`: la función nueva vive ahí para reusarlos.
- `AppState.root` es privado (`state.rs:161`); las `do_*` de `state.rs` lo leen directo.

### 2.5 El dashboard ya tiene todo menos la vista

`/` sirve `assets/index.html`, `/assets/cytoscape.min.js` sirve cytoscape embebido
(`crates/devctx-api/src/lib.rs:43-44`). `loadGraph()` (`assets/index.html:172`) hace
`getJSON('/graph?…')` y espera `{nodes:[{data}], edges:[{data}]}`. Las pestañas (`index.html:102-105`,
`initTabs()` en `:301`) solo alternan paneles de la columna derecha; el grafo vive en `#cy`, al centro.

## 3. Decisión

**Los planes quedan en markdown, en git, como única fuente de verdad. devctx los LEE en cada
consulta; no los copia a ninguna tabla.**

- Una función pura `devctx_core::plans` (texto → estructura) + una capa fina que lee el disco.
- **Una** tool MCP nueva, `plan_status(plan?)`, con descripción < 200 caracteres.
- Un subcomando `devctx plan-status` que no toca store ni daemon (para el hook, rápido y sin locks).
- Una ruta `/plans/graph` y una pestaña en el dashboard.
- Los resultados de `memories_by_file` suman las tasks que mencionan ese archivo, calculadas al vuelo.

### Alternativa rechazada: tablas `plans` / `tasks` en DuckDB

| Por qué no |
|---|
| **Segunda fuente de verdad.** §2.1 muestra que dos copias escritas a mano ya divergen; una tercera copia sincronizada por un indexador diverge cada vez que alguien edita un `.md` sin reindexar. |
| **Precedente de corrupción del store central** (2026-08-13). *No verificado en código ni en `docs/`: no encontré registro escrito del incidente; se cita según lo acordado con el usuario.* Cada tabla nueva es más superficie de migración y de lock en un archivo de un solo escritor. |
| **Más tools**: CRUD de tasks = 3-5 tools nuevas, justo después de recortar 24% el costo de contexto de las descripciones (commit `c7d0b82`: 9729 → 7385 chars). |
| **No hay nada que indexar**: son ~25 archivos chicos; leerlos en cada llamada cuesta menos que la sincronización. |

## 4. Tasks y orden

| Task | Qué | Depende de | Estado |
|------|-----|------------|--------|
| TASK-001 | Parser `devctx_core::plans` (puro + carga desde disco) con tests sobre fixtures | — | `done` |
| TASK-002 | `do_plan_status` en `state.rs`: listado, listas/bloqueadas, ciclos, presupuesto; `Backend` + ruta HTTP | TASK-001 | `done` |
| TASK-003 | Tool MCP `plan_status(plan?, project?)` | TASK-002 | `done` |
| TASK-004 | Subcomando `devctx plan-status [PLAN] [--format json]` | TASK-001 | `done` |
| TASK-005 | Hook `SessionStart` inyecta las próximas tasks del plan activo (fuera del repo) | TASK-004 | `done` (hook en `~/.claude/hooks/devctx-session-start.sh`, fuera del repo) |
| TASK-006 | `/plans/graph` + pestaña "Plans" en el dashboard | TASK-002 | `done` |
| TASK-007 | `memories_by_file` devuelve también `plan_tasks` | TASK-001 | `done` |
| TASK-008 | Tests de integración y docs (EN + ES) | TASK-003, TASK-004, TASK-006, TASK-007 | `done` |

**Paralelizables:** tras TASK-001, salen juntas TASK-002, TASK-004 y TASK-007 (tocan `state.rs`
en funciones distintas; 002 y 007 conviene hacerlas en serie o en el mismo agente para no chocar en
el archivo). TASK-003 y TASK-006 salen apenas esté TASK-002. TASK-005 la hace el orquestador en
`~/.claude`, no un especialista en el repo.

**TASK-001 es la que entrega el contrato**: todo lo demás es presentación sobre su salida. Si su
modelo de datos cambia, cambian las siete.

## 5. Riesgos

- **"Plan activo" por mtime.** Un `git clone`/`checkout` iguala los mtimes. Desempate por número de
  plan más alto (TASK-002). Si igual elige mal, el usuario lo nombra con `plan`.
- **Heurística de `Depende de`.** `— (paralela a TASK-001)` tiene que leerse como "ninguna". Si alguien
  escribe `Ninguna` o `N/A`, también. Lo que no se reconozca se reporta como warning, nunca se adivina.
- **Referencias peladas (`state.rs`, `lib.rs`).** `lib.rs` existe en 15 crates. Emparejarlas con
  cualquier `lib.rs` inunda `memories_by_file` de falsos positivos (TASK-007 define la regla).
- **Presupuesto**: un plan con 40 tasks listado entero rompe el tope. Se recorta con conteo de omitidas,
  igual que `omitted_for_budget` en recall (`state.rs:1966-1972`).
- **La rama base no compila todavía** (`c7d0b82`, "Sin compilar ni testear todavía"). Los errores de
  compilación de PLAN-004 aparecerán mezclados con los de este plan. Recomendación: compilar
  `fix/mcp-integration` antes de ejecutar TASK-002.
- **Plans sin `tasks/`** (PLAN-004): se listan con `total: 0` y `task_files: false`. No se parsea la
  tabla como fuente de tasks (ids `1`, `2`… no son `TASK-00N`).

## 6. Verificación

- `cargo test -p devctx-core plans` — fixtures con las 11 variaciones de §2.2 (TASK-001).
- Contra el repo real: `devctx plan-status --format json` sobre `plans/` debe listar PLAN-001..005,
  reportar los 3 desacuerdos de §2.1 como warnings, y marcar PLAN-005 como activo.
- `mcp_tools.rs`: `call_tool(…, "plan_status", …)` con y sin `plan` (TASK-008).
- Dashboard: abrir `devctx web`, pestaña Plans, PLAN-005 con TASK-001 gris (lista) y el resto rojo.
- El hook: `echo '{"source":"compact","cwd":"…/DevCtxEngine"}' | ~/.claude/hooks/devctx-session-start.sh | wc -c`
  — la sección del plan ≤ 600 chars.

Nada de esto se corrió: es un plan. **No se compiló ni se testeó.**

## 7. Fuera de alcance / fase 2

- **`memories_by_symbol` → tasks.** Las tasks mencionan archivos, no símbolos. Pasar de símbolo a
  archivo exige `symbol_definitions` y, con nombre pelado, la expansión de PLAN-003 (en backend-a
  `actualizar` son 21 declaraciones): el link resultante sería inferencia disfrazada de estructura.
  Fase 2, si hace falta, con `link_source: "inference"` explícito.
- **Escribir estados desde devctx** (`plan_set_status`). La edición sigue siendo del agente sobre el
  `.md`; una tool que escribe markdown es otra conversación.
- **Tasks solo-en-tabla** (PLAN-004) y dependencias entre planes (`PLAN-002/TASK-003`): no aparecen hoy.
- **Cachear el parseo.** Medir primero: ~25 archivos por llamada.

## 8. Cierre

<!-- SE LLENA AL CERRAR EL PLAN -->
