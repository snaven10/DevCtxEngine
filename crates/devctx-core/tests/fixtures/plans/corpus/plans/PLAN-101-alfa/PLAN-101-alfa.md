# PLAN-101 — Alfa: migración de catálogos

Plan de fixture. Texto recortado de planes reales: la columna de estado
es la tercera y hay una tabla de rollback con el id en la primera columna.

| Task | Descripción | Estado |
|---|---|---|
| TASK-001 | alcance y base | `done` |
| TASK-001b | extra con front matter | `done` |
| TASK-002 | API | `pending` |
| TASK-003 | UI | `pending` |
| ~~TASK-004~~ | descartada | |

## Rollback

| Paso | Acción |
|---|---|
| TASK-002 | hecho el respaldo previo, restaurar con `pg_restore` |
| TASK-003 | revertir el despliegue |
