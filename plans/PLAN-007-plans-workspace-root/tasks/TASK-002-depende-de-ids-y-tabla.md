# TASK-002 — Parser: ids con sufijo, `Depende de` por extracción, tabla con columna de estado

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** TASK-001
- **Estado:** `done`

---

## Objetivo

Que el grafo de dependencias deje de inventar aristas y de avisar por cada palabra de prosa, que dos
tasks distintas no colapsen en un mismo id, y que la tabla del plan se lea solo de la tabla que
realmente lleva el estado.

## Contexto verificado

- `crates/devctx-core/src/plans.rs:225` — `parse_depends_on`: warning por cada token no reconocido
  (153 en revfa: `'el'`, `'A)**'`, `'**DD-T9**'`); `normalize_task_id` (`plans.rs:283`) convierte
  un `0` pelado en `TASK-000` (`Fase 0`) y `digits_of` (`plans.rs:273`) concatena
  `TASK-033/034/035` en `TASK-33034035`.
- `plans.rs:673` — `extract_task_id_from_filename` descarta el sufijo de letra: 22 archivos
  (`TASK-007a/b/c`, `TASK-001b`, …) colapsan sobre el id base. En PLAN-119, `TASK-001b` depende de
  `TASK-001` y se reporta como **auto-ciclo** `TASK-001 -> TASK-001`. No reconoce `TASK-R01-…`
  (PLAN-078, 10 archivos), `TASK-DB-001-…` (PLAN-051).
- Duplicados reales: `PLAN-047-firma-async-cert-pipeline/tasks/` tiene `TASK-001-DESIGN.md` y
  `TASK-001-verify-….md` (y lo mismo para 002, 011, 015). Hoy entran las dos como tasks.
- Claves de dependencia en revfa: `Dependencies:` 634, `Depende de:` 413, `Depends on:` 20,
  `Bloqueada por:` 7, `Dependencias:` 6; en front matter `depends_on: [TASK-002]` y `Dependencies: none`.
- Formas reales del valor: `` TASK-001 (`done`), Paso 0b … `` · `TASK-009, TASK-012 … TASK-018` ·
  `` `PLAN-120` TASK-031 (pares duplicados…) `` · `Fase 0 (y coordinar orden con TASK-006, ver Riesgos)` ·
  `ninguna (puede arrancar en paralelo con TASK-002 …)` · `none` · `ALL previous tasks` · `todas`.
- `plans.rs:435` — `parse_table_status` toma **cualquier** fila cuyo primer celda sea un id y lee la
  **última** celda: en PLAN-129 levanta una tabla de rollback (`tabla dice unknown(Revertir el commit)`).
  Encabezados reales con estado: `| Task | Qué | Especialista | Modelo | Depende de | Estado |`,
  `| ID | Título | Dependencias | Estimación | Estado |`, `| Task | Status |`. Filas tachadas:
  `| ~~TASK-011~~ | ~~identidad uuidv7~~ | — | — | — | `skipped` — revisión 2 |`.
- `plans.rs:477` — `pick_plan_doc` no filtra por prefijo `PLAN-<n>`: PLAN-013 eligió
  `BRIEF-CLAUDE-DESIGN-v5.1.md`, PLAN-027 `AUDIT-id-superior.md`, PLAN-076 `LINEAMIENTOS-SEGURIDAD-CROSSTAB.md`.

## Archivos

- **Modificar:** `crates/devctx-core/src/plans.rs`
- **Modificar:** `crates/devctx-mcp/src/state.rs` (exponer `external_deps` en el detalle y en el grafo)

## Pasos

- [x] **Paso 1 — Ids.** Un único normalizador para nombre de archivo y referencias:
      `TASK-<n>[letra]` → `TASK-NNN[letra]` (3 dígitos, letra en minúscula); `TASK-R01` → `TASK-R01`;
      `TASK-DB-001` → `TASK-DB-001`. `T032-001` sigue sin reconocerse (warning). Id repetido en el
      mismo plan → se queda el primero en orden, warning `id duplicado: <archivo>`.
- [x] **Paso 2 — Claves de dependencia** de PLAN-007 DD-7, en el encabezado (con el mismo
      `parse_field_line` y el split por ` · ` de TASK-001) y `depends_on`/`dependencies` en front matter.
- [x] **Paso 3 — Extracción.** Marca de "ninguna" al inicio → vacío (regla de PLAN-005, intacta;
      agregar `none`, `[]`, `–`). Si no: referencias cruzadas `PLAN-<n>` + `TASK-<id>` (con `/`,
      espacio o backticks entre medio) → `external_deps`; rangos `..`/`…`/`...` → expandidos (tope 50);
      el resto, solo tokens `TASK-<id>`. Números pelados solo si el valor entero es una lista de
      números. Auto-dependencia → descartada sin warning.
- [x] **Paso 4 — Warning único** `Depende de sin TASK reconocible '<valor recortado a 60>'` cuando no
      hay ids, ni externos, ni marca de "ninguna". Nunca un warning por token.
- [x] **Paso 5 — `Task.external_deps: Vec<String>`** (serializado). `analyze` los ignora: no bloquean
      ni van a `missing`. `plan_status_detail` los lista; `plan_graph_value` no dibuja aristas para ellos.
- [x] **Paso 6 — Tabla.** Estado por tabla: una fila de encabezado con celda `Estado`/`Status`/`State`
      fija la columna; las filas de datos (hasta la próxima línea que no empiece con `|`) leen esa
      columna con el `Status::parse` tolerante de TASK-001. Tabla sin esa columna → ignorada. Id
      tachado sin estado legible → `Skipped`. Celda sin palabra clave → no entra al mapa (no es un
      desacuerdo, es prosa).
- [x] **Paso 7 — Documento del plan.** `<dir>.md`; si no, candidatos `PLAN-<n>*.md` sin `-design.md`;
      si no hay, cualquier `.md` sin `-design.md`. Warning de "múltiples" solo dentro del nivel elegido.

## Criterios de aceptación

- [x] Fixtures con cada valor real de "Contexto verificado": `Fase 0` → warning único y sin
      `TASK-000`; `TASK-033/034/035` → `TASK-033, TASK-034, TASK-035`; `TASK-012 … TASK-018` → 7 ids;
      `` `PLAN-120` TASK-031 `` → `external_deps`, no `missing`.
- [x] `— (paralela a TASK-001)` sigue dando vacío (test existente de PLAN-005 intacto).
- [x] `TASK-001b` y `TASK-001` son tasks distintas; `TASK-001b → TASK-001` no es ciclo.
- [x] Una tabla de rollback con ids en la primera columna y sin columna `Estado` no genera warnings.
- [x] En un dir con `BRIEF-X.md` y `PLAN-013-algo.md` (sin `<dir>.md`), se elige `PLAN-013-algo.md`.
- [x] Sin dependencias nuevas en `devctx-core`.

## Riesgos

Extraer ids de prosa puede crear aristas que el autor no quiso (`coordinar orden con TASK-006`). Se
acepta y se documenta (TASK-007): antes era warning y ninguna arista; una arista plausible visible
en el dashboard es más fácil de corregir en el markdown que un warning entre 1592.

## Resultado

- **Estado final:** `done` (implementado; sin compilar)
- **Resumen:** Un solo `parse_id_at` para nombres de archivo, referencias y tabla: `TASK-<n>[letra]` conserva el sufijo, acepta `TASK-R01` y `TASK-DB-001`; `T032-001` sigue sin reconocerse. Id repetido en el plan → se queda el primero y avisa `id duplicado`. `parse_depends_on` extrae solo tokens `TASK-<id>` (rangos `..`/`...`/`…` con tope 50, listas `033/034/035`, números pelados solo si todo el valor es una lista), marcas de ninguna (`—`, `–`, `-`, `ninguna`, `none`, `n/a`, `[]`) con prioridad sobre la prosa, auto-dependencias descartadas, y un único warning `Depende de sin TASK reconocible '<60 chars>'`. Referencias cruzadas `PLAN-<n>/TASK-<id>` van a `Task.external_deps` (si el `PLAN-<n>` es el propio plan, es local). `analyze` las ignora. Claves `depende de`/`depends on`/`dependencies`/`dependencias`/`bloqueada por`/`blocked by` y `depends_on` en front matter (incluida lista en bloque). Tabla: solo tablas con columna `Estado`/`Status`/`State`, valor leído de esa columna, id tachado sin estado legible → `Skipped`, celda sin palabra clave no entra. `pick_plan_doc`: `<dir>.md`, luego `PLAN-<n>*.md` sin `-design.md`, luego cualquier `.md`. `state.rs`: `external_deps` en el detalle (solo si hay) y en los nodos del grafo (sin aristas).
- **Archivos tocados:** `crates/devctx-core/src/plans.rs`, `crates/devctx-mcp/src/state.rs`
- **Verificado por:** `cargo test -p devctx-core` (62 unit + 6 plans_corpus), `-p devctx-mcp` (41 unit + 4 plans_root), `-p devctx-cli --test plan_status_cli --test mcp_binding --test mcp_tools` (9 + 20 + 8, 1 ignored preexistente) — todo verde (2026-10-01); `cargo clippy -p devctx-core -p devctx-mcp -p devctx-cli --all-targets` sin warnings.
