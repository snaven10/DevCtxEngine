# TASK-004 — TS arrow-const y arreglos de Python

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** rust (a definir al detallar)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-002
- **Estado:** `pending`

---

## Objetivo

Extraer `export const x = () =>` / `const x = function` como símbolos en TS/TSX/JS. Python: `get_references` no deduplica por caller (hoy pierde la segunda llamada desde el mismo método), `impact_analysis` incluye callers a nivel de módulo, y los builtins (`len`, `print`, `dict.get`…) se marcan `external`.

*Stub de Fase 0: contexto verificado, pasos y criterios de aceptación se escriben al detallar
PLAN-009, después de cerrar PLAN-008.*

## Resultado

<!-- SE LLENA AL CERRAR -->
