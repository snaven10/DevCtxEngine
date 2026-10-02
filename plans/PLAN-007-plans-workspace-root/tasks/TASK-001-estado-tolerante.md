# TASK-001 — Parser: `Estado` tolerante, `Status::Skipped` y títulos

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Que el estado de una task se encuentre donde los planes reales lo escriben y se lea aunque venga
con emoji, bold, backticks o prosa de cola. Es la causa de 720 + ~600 de los 1592 warnings medidos
en `~/revfa` (PLAN-007 §1.2).

## Contexto verificado

- `crates/devctx-core/src/plans.rs:30` — `Status::parse` mira solo `first_token` (`plans.rs:127`).
  Tiene un brazo muerto (`"en" if lowered == "en"`, `plans.rs:35-38`) que se va con el rediseño.
- `plans.rs:152` — `parse_field_line` exige `**` al inicio: no acepta `Status: x` sin bold ni una
  línea con varios campos separados por ` · `.
- `plans.rs:180` — `header_block`: entre el H1 y el primer `## `. Sin H1, hoy toma el texto entero.
- `plans.rs:368` — `parse_task_doc` solo reconoce la clave `estado` (`plans.rs:379`).
- `plans.rs:649` — `h1_title_strip_plan_prefix` solo quita `—`/`-`: 76 títulos de revfa quedan
  como `": Actualizar configuración…"`; PLAN-008 tiene H1 `PLAN-1:` con dir `PLAN-008`.
- `plans.rs:663` — `status_label`; `plans.rs:690` — `analyze` (`Status::Done => continue`).
- Variantes reales de revfa (cuenta · texto), para los fixtures:
  237 `**Status:** done` · 232 `**Status:** pending` · 159 `` - **Estado:** `pending` `` ·
  32 `**Status:** completed` · 28 `**Status:** DONE` · 12 `Status: pending` (sin bold) ·
  11 `**Status**: pending` · 10 `**Status:** ✅ Completed — …` · 9 `**Status:** implemented — …` ·
  8 `**Estado:** Pendiente` · 8 `**Estado**: PENDING` · 6 `` - **Estado:** sigue `pending`. `` ·
  5 `` - **Estado:** `skipped` `` · 4 `` - **Estado:** ✅ **`done`** — ejecutada… `` ·
  3 `` **Estado: `done`.** `` · 3 `` 🟢 `pending` — **DESBLOQUEADA…** `` ·
  `` 🟨 **`in_progress`** (revisión 13 …) `` · `**Plan:** PLAN-111 · **Estado:** ⬜ PENDIENTE · **Depende de:** —` ·
  `**Specialist**: x · **Proyecto**: y · **Status**: done` (PLAN-072) · front matter `status: done`
  (25 archivos con `---` inicial, p. ej. `PLAN-002/tasks/TASK-001-backend-dto-service-endpoint.md`,
  `PLAN-083-…/tasks/TASK-001-…md`) · sección `## Estado` con `` `completed` `` en la línea siguiente
  (`PLAN-009/tasks/TASK-001-auth-ms-no-reemplazar-jwt.md`).
- Lo que **no** es un estado y debe ignorarse: `- **Estado final:** …` (en `## Resultado`),
  `Estado objetivo:`.

## Archivos

- **Modificar:** `crates/devctx-core/src/plans.rs`

## Pasos

- [x] **Paso 1 — Fixtures primero.** Un test por variante de la lista de arriba (texto copiado tal
      cual), más: `Estado final: done` en Resultado no pisa `Estado: pending`; `Estado objetivo:`
      no es estado; prosa sin palabra clave → `Unknown` + warning.
- [x] **Paso 2 — Línea de campo.** `parse_field_line` acepta bold con `:` dentro o fuera, `**Campo: valor**`,
      y sin bold (`Campo: valor` al inicio de línea, con o sin viñeta). La línea se parte por ` · `
      antes de buscar campos. La clave se compara exacta tras normalizar (minúscula, sin tildes).
- [x] **Paso 3 — Fuentes del estado**, en orden: campo del encabezado con clave `estado`/`status`/`state`;
      front matter YAML (`---` … `---` al inicio; `status:`), leído a mano, sin `serde_yaml` para
      esto; sección `## Estado`/`## Status`, primera línea no vacía. El encabezado sin H1 empieza
      después del front matter y termina en el primer `## `.
- [x] **Paso 4 — Valor.** Primer span entre backticks que normalice a palabra clave; si no, primera
      palabra clave del texto sin emoji/`*`/puntuación. Tabla de sinónimos de PLAN-007 DD-6, como
      `const` en el módulo. `✅` sin palabra clave → `Done`.
- [x] **Paso 5 — `Status::Skipped`.** `is_done()` sigue siendo solo `Done`; nuevo `is_resolved()`
      (`Done | Skipped`) que usa `analyze` para dependencias y para no listar la task. `status_label`
      → `"skipped"`.
- [x] **Paso 6 — Títulos.** `h1_title_strip_plan_prefix` quita `PLAN-<n>` (cualquier número) seguido
      de `—`, `-`, `:` o espacio.
- [x] **Paso 7 — Consumidores de `Status`** en `crates/devctx-mcp/src/state.rs`: `plan_status_list`
      (`state.rs:758`) agrega `skipped` al listado; `plan_status_detail` y `plan_graph_value` pasan
      `"skipped"` como status de nodo. `crates/devctx-api/assets/index.html:329-332`: color para
      `node[status = "skipped"]` (gris tenue).

## Criterios de aceptación

- [x] Cada variante de "Contexto verificado" parsea al estado esperado, sin warning.
- [x] `Estado final` / `Estado objetivo` nunca deciden el estado.
- [x] Una task `skipped` satisface la dependencia de otra (la dependiente queda lista).
- [x] `": Actualizar…"` ya no aparece como título; PLAN-008 (`PLAN-1:`) sale limpio.
- [x] Los 18 tests existentes de `plans.rs` siguen verdes sin tocarlos (salvo el brazo muerto).
- [x] `crates/devctx-core/Cargo.toml` sin dependencias nuevas.

## Riesgos

Sinónimos que aparecen en prosa (`implemented — pending deploy`): gana el backtick, luego la
primera palabra clave. Si un sinónimo produce falsos `done` en el corpus (TASK-008), se saca de la
lista; no se agrega heurística encima.

## Resultado

- **Estado final:** `done` (implementado; sin compilar)
- **Resumen:** `Status::parse` ahora busca palabra clave (backtick exacto primero, luego la primera del texto sin emoji/puntuación; sinónimos ES+EN de DD-6 con las decisiones Q-4/Q-5: `implemented`/`cerrada`/`merged`/`hecho` = done, `ready-for-approval` sigue Unknown). Nuevo `Status::Skipped` + `is_resolved()` (lo usa `analyze`, `active_plan_id` y el orden de `plan_tasks`). `parse_field_line` acepta bold con `:` dentro/fuera, `**Campo: valor**` y sin bold; `field_segments` parte por ` · `. Fuentes del estado: campo de encabezado → front matter → sección `## Estado`; `header_block` salta el front matter. `h1_title_strip_plan_prefix` quita cualquier `PLAN-<n>` y `—`/`–`/`-`/`:`. Estado faltante sigue pending + warning (Q-2: no se llena desde la tabla). `state.rs`: `skipped` en el listado y el detalle, nodo `skipped` en el grafo; `index.html`: color gris para `skipped`.
- **Archivos tocados:** `crates/devctx-core/src/plans.rs`, `crates/devctx-mcp/src/state.rs`, `crates/devctx-api/assets/index.html`
- **Verificado por:** `cargo test -p devctx-core` (62 unit + 6 plans_corpus), `-p devctx-mcp` (41 unit + 4 plans_root), `-p devctx-cli --test plan_status_cli --test mcp_binding --test mcp_tools` (9 + 20 + 8, 1 ignored preexistente) — todo verde (2026-10-01); `cargo clippy -p devctx-core -p devctx-mcp -p devctx-cli --all-targets` sin warnings.
