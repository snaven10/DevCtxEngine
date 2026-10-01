# TASK-005 — Hook `SessionStart` inyecta las próximas tasks del plan activo

- **Plan:** PLAN-005 — `plan_status`
- **Especialista:** — (lo hace el **orquestador**, en `~/.claude`; no es un cambio del repo)
- **Proyecto:** configuración global (`~/.claude/hooks/devctx-session-start.sh`)
- **Depende de:** TASK-004
- **Estado:** `done`

---

## Objetivo

Que tras `startup|resume|clear|compact` el modelo arranque sabiendo qué plan está activo y qué task
sigue, sin tener que acordarse de preguntar. Es el cierre del problema del PLAN §1.

## Contexto verificado

`~/.claude/hooks/devctx-session-start.sh` (leído 2026-09-11):
- Nunca falla: todo camino sale `exit 0`.
- `cd "$cwd"`, luego `timeout 5 "$DEVCTX" status … | head -c 600`.
- Bloque post-compactación: `timeout 15 "$DEVCTX" recall … | head -c 2500`, solo si hay proyecto.
- Filtra `newer devctx|started background server`.

## Archivos

- **Modificar:** `~/.claude/hooks/devctx-session-start.sh` (fuera del repo)

## Pasos

- [ ] **Paso 1 — Bloque nuevo**, después del `status` y en **todo** evento (no solo `compact`):
      ```bash
      plan="$(timeout 3 "$DEVCTX" plan-status 2>/dev/null \
        | grep -Ev 'newer devctx|started background server' | head -c 600)"
      if [ -n "$plan" ] && ! printf '%s' "$plan" | grep -q 'sin planes'; then
        echo; echo "## Plan activo (plan-status)"; printf '%s\n' "$plan"
        echo "- Detalle: \`plan_status(plan)\`. El .md de la task manda sobre la tabla del plan."
      fi
      ```
- [ ] **Paso 2 — No depende del proyecto devctx.** `plan-status` cae al cwd (TASK-004 Paso 2): el
      bloque corre aunque el `status` diga "No DevCtxEngine project found".
- [ ] **Paso 3 — Presupuesto total.** Hoy: 600 (status) + 2500 (recall). Suma 600 como máximo.

## Criterios de aceptación

- [ ] `echo '{"source":"compact","cwd":"/home/snaven10/personal/DevCtxEngine"}' | ~/.claude/hooks/devctx-session-start.sh`
      muestra la sección "Plan activo" con ≤ 600 chars.
- [ ] Con un binario viejo sin `plan-status`: la sección no aparece y el hook sale 0.
- [ ] En `/home/snaven10/revfa` (sin `plans/`): sin sección, sin error.

## Riesgos

Un binario viejo imprime el error de clap por stderr (descartado) y nada por stdout: se omite solo.

## Resultado

<!-- SE LLENA AL CERRAR — el orquestador marca `done` acá cuando edite el hook -->
