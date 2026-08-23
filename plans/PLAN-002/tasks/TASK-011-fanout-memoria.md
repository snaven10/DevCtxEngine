# TASK-011 — Fan-out de memoria: `recall` abarca las locales de todos los miembros

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** TASK-002
- **Estado:** `pending`

---

## Objetivo

Que estando vinculado a un grupo, `recall` encuentre lo que sabe el producto: las memorias
**grupales** (que ya encuentra) **y las locales de cada miembro** (que hoy no).

## Contexto verificado

Los tres tiers, según `docs/03-core-concepts/memory.md`:

| Scope | Se guarda en | Visible desde |
|---|---|---|
| `local` | store del propio proyecto | **solo ese repositorio** |
| `group` | central, `@group:<nombre>` | todo repo que comparta `project.group` |
| `global` | central, `@global` | todo proyecto de la máquina |

`recall` con `scope: all` busca el tier local **del store vinculado** más el central. En modo grupo
el store vinculado es uno solo, así que **las locales de los otros 10 miembros no se ven** — aunque
la sesión dice estar en el producto entero.

Ese es exactamente el agujero: el usuario pregunta "qué sabemos de X" desde la raíz del workspace y
recibe la respuesta de un repo más lo compartido, presentada como si fuera todo.

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs` (nueva `do_recall_group`)
- **Modificar:** `crates/devctx-mcp/src/lib.rs` (la tool `recall` despacha según el binding)

## Pasos

- [ ] **Paso 1 — `do_recall_group(members, query, limit, scope)`.** El tier central se consulta UNA
      vez (es compartido); los tiers locales, uno por miembro.
- [ ] **Paso 2 — No duplicar el central.** Consultarlo por miembro traería la misma fila N veces.
      Central una vez + locales N.
- [ ] **Paso 3 — Fusionar por RRF** y respetar `limit` sobre el resultado fusionado, igual que TASK-010.
- [ ] **Paso 4 — Cada memoria dice de qué proyecto vino**, en el mismo campo `repo` que ya devuelve.
      Verificar que las locales lo traigan poblado.
- [ ] **Paso 5 — Respetar `scope` explícito**: `scope: "local"` en modo grupo = las locales de todos
      los miembros (no un error: acá "local" describe el tier, no un destino de escritura como en
      TASK-009). `scope: "group"` = solo el central del grupo.
- [ ] **Paso 6 — Un miembro que falla no tumba el recall**; se reporta y el resto contesta.

## Criterios de aceptación

- [ ] En modo grupo, `recall` devuelve una memoria `local` guardada desde OTRO miembro.
- [ ] Las grupales se devuelven una sola vez, no N.
- [ ] `limit` acota el resultado fusionado, no cada miembro.
- [ ] Cada resultado identifica su proyecto de origen.
- [ ] En modo proyecto el comportamiento no cambia (sin regresión).

## Riesgos

- **Presupuesto**: `recall` ya recorta por presupuesto y devuelve `omitted_for_budget`. Con N stores
  hay más candidatos y ese recorte se vuelve más agresivo — verificar que lo omitido se siga
  reportando, porque callar lo que no entró es peor que no traerlo.
- **Latencia**: mismo tradeoff que TASK-010, y aquí duele más porque `recall` suele ir al inicio de
  una tarea.

## Resultado

<!-- SE LLENA AL CERRAR -->
