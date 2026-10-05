# TASK-008 — Expansión ego-graph de k saltos como firmas

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** rust (a definir al detallar)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-007
- **Estado:** `pending`

---

## Objetivo

En `build_context`, por cada hit de código agregar sus callers/callees directos (k=1, configurable) como firmas, no cuerpos, dentro del presupuesto (RepoGraph: +2-2.7 pts en SWE-bench Lite). Respeta `confidence` y excluye `external`/`test` por defecto.

*Stub de Fase 0: contexto verificado, pasos y criterios de aceptación se escriben al detallar
PLAN-009, después de cerrar PLAN-008.*

## Resultado

<!-- SE LLENA AL CERRAR -->
