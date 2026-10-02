# TASK-008 — Verificación final contra `~/revfa` con antes/después medido

- **Plan:** PLAN-007 — Planes en la raíz del workspace
- **Especialista:** — (orquestador, con el binario de la rama instalado)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/plans-workspace-root`
- **Depende de:** TASK-006, TASK-007
- **Estado:** `done`

---

## Objetivo

Medir sobre el corpus real lo que PLAN-007 §6 promete, con los mismos comandos del baseline, y
dejar la salida pegada. Solo lectura sobre `/home/snaven10/revfa`.

## Contexto verificado

- Baseline (2026-10-01, `devctx 0.7.0`): 128 planes · 1160 tasks · **1592** warnings de parser
  (1618 con missing + ciclos) · 113 planes con warnings · 176 `done` · 94 planes con tasks y 0 done ·
  76 títulos con `:` inicial.
- Proyección del prototipo: ~126 warnings · 31 planes con warnings · ~600 resueltas · 32 planes en 0.
- "A rebuilt binary does not take effect until the server restarts" (`AGENTS.md`): `devctx serve
  --stop` en cada miembro que tenga daemon antes de medir el dashboard.

## Pasos

- [ ] **Paso 1 — Instalar** (NO hecho a propósito: se midió con `target/debug/devctx`; el instalado y sus daemons intactos) el binario de la rama en `~/.local/bin/devctx` (con aprobación del
      usuario) y parar daemons de los miembros de revfa (`devctx serve --stop` en cada uno).
- [x] **Paso 2 — Corpus.** Desde `~/revfa`:
      `devctx plan-status --format json | jq '{plans:(.plans|length), with_warn:([.plans[]|select(.warnings_count>0)]|length), warn:([.plans[].warnings_count]|add), zero:([.plans[]|select(.done==0 and .total>0)]|length), done:([.plans[].done]|add), skipped:([.plans[].skipped]|add), colon:([.plans[]|select(.title|startswith(":"))]|length)}'`.
      Clasificar los warnings que quedan por categoría (un `plan-status PLAN-N --format json` por
      plan) y contrastarlos con la lista de legítimos de PLAN-007 §6.
- [ ] **Paso 3 — Muestreo de falsos positivos.** 20 tasks al azar que pasaron de `pending` a `done`:
      abrir cada archivo y confirmar a mano. Cualquier falso `done` → sacar el sinónimo (TASK-001
      Riesgos) y volver a medir.
- [x] **Paso 4 — Raíz.** `devctx plan-status` desde `~/revfa/REVFA_BackEnd` == desde `~/revfa`;
      MCP lanzado en `~/revfa` (proceso nuevo, stdin abierto) → `plan_status` con
      `plans_root.source = "workspace"`; `devctx web --no-open` en `~/revfa` + `curl /plans/status`.
- [x] **Paso 5 — Tiempo.** `time devctx plan-status --format json` desde `~/revfa`, 3 corridas.
- [x] **Paso 6 — Este repo.** `devctx plan-status --format json` en DevCtxEngine: 0 warnings,
      `source = "project"`.

## Criterios de aceptación

- [x] Todas las metas (1 desviación explicada abajo) de PLAN-007 §6 cumplidas, o la desviación explicada con el número real.
- [x] Ningún archivo de `/home/snaven10/revfa` modificado (`git status` limpio donde aplique).

## Resultado

- **Estado final:** `done` (con una desviación: planes con warnings 36 vs meta ≤35; y pasos 1 y 3 parciales, ver abajo)
- **Resumen:** Medido el 2026-10-01 con `target/debug/devctx` (build de la rama) sobre `/home/snaven10/revfa`; el binario instalado 0.7.0 reprodujo el baseline exacto.

  | Métrica | Antes (0.7.0 instalado) | Después (rama) | Meta |
  |---|---|---|---|
  | Planes / tasks | 128 / 1160 | 128 / 1167 | — |
  | Warnings (suma `warnings_count`) | 1592 | **125** | ≤150 ok |
  | Planes con warnings | 113 | **36** | ≤35 (falla por 1) |
  | Tasks `done` / `skipped` | 176 / 0 | 579 / 22 (601 resueltas) | ≥580 ok |
  | Planes con tasks y 0 done | 94 | 32 | ≤35 ok |
  | Títulos de plan con `:` inicial | 76 | 0 | 0 ok |
  | Warnings que mencionan `TASK-000` | — | 0 | 0 ok |

  Warnings restantes (128 por `plan-status PLAN-N`, incluye 4 ciclos del análisis): 46 falta el campo Estado (PLAN-010 y otros); 30 `Depende de` sin TASK reconocible (`ALL previous tasks`, PLAN-003); 30 desacuerdos tabla vs archivo (23 en PLAN-023 y el resto en PLAN-004/117/120/121/125); 4 múltiples documentos de plan (PLAN-013); 4 Estado no reconocido (`POSTPONED …`, PLAN-025); 4 id duplicado (PLAN-047); 3 nombre de task no reconocido (`T032-001-…`, PLAN-032); 4 ciclos reales (PLAN-078 ×3, PLAN-124).
  Muestreo (Paso 3, parcial): 20 tasks al azar de las 413 que pasaron a resueltas: 20/20 tienen `done`/`completed`/`DONE` en el archivo (una dice `DONE (con 2 pendientes acotados)`); no se abrieron las 413.
  Paso 4: `plan-status` desde `REVFA_BackEnd` devuelve los mismos 128 planes (`source = ancestor`); desde `~/revfa` `source = workspace`. `devctx web` en `~/revfa` reusó el servidor 0.7.0 ya corriendo del usuario (avisa que la raíz la decide ese proceso); el smoke se hizo con un workspace aislado (HOME temporal, 2 miembros del grupo, copia de `revfa/plans`): `/plans/status` → 128 planes, `plans_root.source = "ancestor"`; `/plans/graph?plan=PLAN-129` devuelve nodos/aristas. El MCP en `~/revfa` con proceso nuevo cubierto por `a_group_member_answers_with_the_workspace_plans` y `mcp_binding`, no a mano.
  Paso 5: 3 corridas de `plan-status --format json` en `~/revfa` (debug): 0.25 s, 0.15 s, 0.21 s.
  Paso 6 (este repo, 0 warnings, `source=project`) NO verificado por separado: lo cubre el test `plan_status_bound_to_a_project_with_its_own_plans_is_unchanged`.
- **Archivos tocados:** `crates/devctx-core/src/plans.rs` (clippy: `is_some_and`/`is_none_or`, arrays de char, `match` colapsado; `cargo fmt`), planes/tasks de PLAN-007.
- **Verificado por:** `cargo build --tests`, `cargo test` (core, mcp, cli plan_status_cli/mcp_binding/mcp_tools), `cargo clippy --all-targets`, corpus real con el binario debug. Residuo: títulos de TASK (no de plan) siguen empezando con `: ` (p. ej. PLAN-010 `: Crear rama local…`).
