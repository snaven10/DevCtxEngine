# TASK-001 — Parser `devctx_core::plans`: del markdown al grafo de tasks

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plan-status`
- **Depende de:** — (primera del plan)
- **Estado:** `pending`

---

## Objetivo

Una función pura que, dado el texto de un plan y de sus tasks, devuelva la estructura que todo lo
demás consume: plan, tasks, estado normalizado, dependencias y archivos mencionados. Más una capa
fina que lee `plans/` del disco. Es el contrato del plan: las otras siete tasks lo presentan.

## Contexto verificado

- `crates/devctx-core/src/lib.rs:3-4` — el crate es liviano a propósito; deps: `anyhow`,
  `thiserror`, `serde`, `serde_yaml`. **No agregar `regex`**: el formato es por línea y se parsea a mano.
- Las variaciones reales están en PLAN-005 §2.2. Las que muerden:
  - `plans/PLAN-002/tasks/TASK-002-enum-binding.md:6` — `- **Depende de:** — (paralela a TASK-001)`:
    empieza con guion y **menciona un TASK que no es dependencia**.
  - `plans/PLAN-001/tasks/TASK-001-observable-progress-in-appstate.md:3-6` — `**Estado**: done`,
    sin viñeta, dos puntos fuera del bold, sin backticks.
  - Las tasks de PLAN-002/003 repiten `- **Estado final:**` dentro de `## Resultado` (p. ej.
    `PLAN-002/tasks/TASK-001-projects-under-path.md`, sección Resultado).
  - `plans/PLAN-003-grafo-y-registro-de-lenguajes/` tiene `PLAN-003-design.md` **y**
    `PLAN-003-grafo-y-registro-de-lenguajes.md`, los dos con H1 `# PLAN-003 —`.
  - `plans/PLAN-004-mcp-integration/` no tiene `tasks/`.

## Archivos

- **Crear:** `crates/devctx-core/src/plans.rs`
- **Modificar:** `crates/devctx-core/src/lib.rs` (`pub mod plans;`)

## Pasos

- [ ] **Paso 1 — Modelo.** `Plan { id, title, dir, doc_path, tasks: Vec<Task>, table_status:
      BTreeMap<String, Status>, mtime, warnings }` y `Task { id, title, plan_id, status: Status,
      depends_on: Vec<String>, files: Vec<FileRef>, path }`. `Status = Pending | InProgress | Done |
      Blocked | Unknown(String)`. `FileRef { path, line: Option<u32> }`. Todo `Serialize`.
- [ ] **Paso 2 — Bloque de encabezado.** Solo se leen campos entre el H1 y el primer `## `.
      Así `Estado final:` de `## Resultado` nunca compite con el `Estado` real.
- [ ] **Paso 3 — Línea de campo tolerante.** Aceptar `- **Campo:** valor`, `**Campo**: valor`,
      `**Campo:** valor`, con o sin viñeta. Nombre de campo sin tildes/mayúsculas para comparar.
- [ ] **Paso 4 — `Estado` normalizado.** Primer token del valor, sin backticks, en minúscula:
      `pending`→Pending, `in_progress`/`in-progress`/`en progreso`→InProgress, `done`→Done,
      `blocked`/`bloqueada`→Blocked; lo demás → `Unknown(texto)` + warning. Campo ausente →
      Pending + warning. La cola (`— con un riesgo…`) se ignora.
- [ ] **Paso 5 — `Depende de`.** Si el valor, ya sin espacios, **empieza con `—`, `-`, `ninguna`,
      `n/a`** → vacío, aunque después mencione un TASK. Si no: tokens `TASK-\d+`; rango
      `TASK-001..004` se expande; números pelados `001, 002` se aceptan como `TASK-001`… Normalizar a
      3 dígitos. Lo irreconocible → warning, no dependencia inventada.
- [ ] **Paso 6 — Archivos mencionados.** Tokens entre backticks simples, **fuera de bloques
      ```` ``` ````**, que contengan `/` o terminen en una extensión conocida (`rs md toml yaml json
      html js ts sh`), con `:línea` opcional. Excluir los que empiezan con `..`, `http`, o contienen
      espacios. Deduplicar por ruta.
- [ ] **Paso 7 — Id y título.** Task: id del nombre de archivo (`TASK-\d+`), título = H1 sin el
      prefijo `TASK-00N — `. Plan: id = prefijo `PLAN-\d+` del **directorio**.
- [ ] **Paso 8 — Documento del plan.** Preferir `<nombre-del-dir>.md`; si no existe, el único
      `PLAN-N-*.md` que no termine en `-design.md`; si hay varios, warning y el primero en orden.
- [ ] **Paso 9 — Tabla del plan.** Filas `| TASK-00N | … | \`estado\` |` → `table_status` (última
      celda que normalice a un Status). Solo para advertir: si difiere del archivo, warning
      `"TASK-010: tabla dice done, archivo dice pending"`. **Nunca pisa el estado del archivo.**
- [ ] **Paso 10 — Carga desde disco.** `load_plans(root: &Path) -> Vec<Plan>`: dirs `plans/PLAN-*`,
      `mtime` = el mayor de sus `.md`. Plan sin `tasks/` → `tasks` vacío, sin error. Un archivo
      ilegible → warning en el plan, no aborta. Sin `plans/` → `Vec` vacío.
- [ ] **Paso 11 — Análisis del grafo** (puro, sobre `Plan`): `analyze(&Plan) -> Analysis { ready,
      in_progress, blocked: Vec<(task, Vec<dep_pendiente>)>, missing: Vec<(task, dep)>, cycles:
      Vec<Vec<task>> }`. Lista = no-done, no Blocked explícito, todas sus deps Done. Dep que no existe
      en el plan → `missing` y cuenta como no satisfecha. Ciclos por DFS con colores.

## Criterios de aceptación

- [ ] Tests unitarios en `plans.rs` sobre **strings fixture** copiadas de los encabezados reales
      (una por variación de PLAN-005 §2.2). Ningún test lee `plans/` del repo.
- [ ] `— (paralela a TASK-001)` → `depends_on` vacío.
- [ ] Una task con `Estado: pending` y `Estado final: done` en Resultado → Pending.
- [ ] En el dir de PLAN-003 se elige `PLAN-003-grafo-y-registro-de-lenguajes.md`, no `-design.md`.
- [ ] Tabla `done` vs archivo `pending` → estado Pending + un warning.
- [ ] Ciclo `A→B→A` detectado; dep `TASK-099` inexistente → `missing`, y la task no queda lista.
- [ ] Un ` `crates/x/y.rs:42` ` dentro de un bloque ```` ``` ```` NO aparece en `files`.
- [ ] `devctx-core/Cargo.toml` sin dependencias nuevas.

## Riesgos

Formato libre escrito a mano: habrá variaciones que hoy no existen. La regla es **warning, no
adivinanza** — un estado inventado es exactamente lo que este plan quiere evitar.

## Resultado

<!-- SE LLENA AL CERRAR -->
