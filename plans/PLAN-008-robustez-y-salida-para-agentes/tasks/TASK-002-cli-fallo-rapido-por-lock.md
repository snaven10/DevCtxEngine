# TASK-002 — CLI: fallo rápido ante un DB bloqueado, con PID y cómo liberarlo

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

`devctx search` (y todo comando CLI que pase por `remote::ensure`) contra un DB retenido por otro
proceso falla en ≤ 2 s, nombrando el PID dueño y la acción para liberarlo (PLAN-008 B1c). Hoy tarda
63 s.

## Contexto verificado

- Callers CLI de `remote::ensure` en `crates/devctx-cli/src/main.rs`: `:725`, `:954` (TUI), `:1564`
  (MCP, lo cubre TASK-001), `:1682`, `:1859`, `:1953`, `:2020`, `:2056`, `:2068`, `:2313`.
- Tras `ensure → None`, los comandos abren el store local (`open_store`) y DuckDB devuelve
  `Could not set lock on file … Conflicting lock is held in <exe> (PID N)`.
- El texto del error de DuckDB ya trae el PID; falta cortar la espera (TASK-001 paso 3) y
  reformularlo.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs` (callers de `ensure`, apertura local)
- **Modificar:** `crates/devctx-cli/src/remote.rs` (si hace falta exponer la causa)

## Pasos

- [ ] **Paso 1.** Migrar los callers CLI al `ensure` con causa de TASK-001. Si `ensure` falla por
      lock, no intentar la apertura local: devolver el error directo.
- [ ] **Paso 2.** Mensaje: `the index of <proyecto> is held by PID N (<cmdline corto>, started <hora>).
      If it is a `devctx mcp` from an old session, close that session; otherwise `devctx serve --stop`.`
      El cmdline se lee de `/proc/N/cmdline` cuando existe; si el binario es `(deleted)` decirlo.
- [ ] **Paso 3.** `DEVCTX_NO_AUTOSERVE=1` + lock: mismo mensaje, sin espera.

## Criterios de aceptación

- [ ] Test de integración (CLI): lock ajeno (serve a mano sin `serve.json`) → `devctx search x`
      termina en < 2 s, código ≠ 0, stderr contiene el PID y `serve --stop`.
- [ ] Sin lock, el comportamiento y la salida de `devctx search` no cambian (tests existentes verdes).

## Riesgos

Leer `/proc` es Linux-only: en otras plataformas el mensaje omite el cmdline, nunca falla por eso.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
