# TASK-003 — Resolución Java/TS/Go/Rust: `var`, campos, constructor, imports, scope

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** rust (a definir al detallar)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-002
- **Estado:** `pending`

---

## Objetivo

`type_map` con scope por función en vez de plano por archivo (`parser.rs:89-112`), `var` inferido de inicializadores simples (elimina los 481 `var.*`), campos inyectados por constructor incluso en clases internas (`PrepartidaResource.java:310`), imports para calificar targets, constantes en MAYÚSCULA (`LOG`) no son tipos (`parser.rs:327`), Rust `Foo::bar` usa el path como receptor, Go califica métodos por receptor (`container_kinds` vacío hoy).

*Stub de Fase 0: contexto verificado, pasos y criterios de aceptación se escriben al detallar
PLAN-009, después de cerrar PLAN-008.*

## Resultado

<!-- SE LLENA AL CERRAR -->
