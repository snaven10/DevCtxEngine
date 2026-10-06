# TASK-016 — Verificación de campo contra §6

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** — (orquestador, con aprobación del usuario; la sesión revisora mide)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-015
- **Estado:** `pending`

---

## Objetivo

Medir, con el binario candidato y el arnés de TASK-001, cada fila de los criterios globales del
master (§6) contra la línea base 0.9.0, reproducir los casos de campo y dejar asentado qué se
verificó y qué no, antes del cross-check y del tag.

## Contexto verificado

- Arnés: `scripts/graph-eval/` (TASK-001); casos de ACME en `$DEVCTX_EVAL_CASES` (fuera del repo).
- Línea base 0.9.0: columna "Antes" de §6, completada en TASK-001.
- Casos de campo: `DraftResource.java:310` (backend-b),
  `tokenInterceptor` (frontend), Python de legacy-migration, `impact("get"/"map")`
  en backend-a, B2/B3/B4 de PLAN-008 TASK-016, pendientes 1 y 2 de PLAN-008.
- Método de PLAN-008 TASK-016: sandbox aislado, binario nuevo compilado aparte, VIEJO = tag
  `v0.9.0`; tokens aproximados como chars/4; solo lectura sobre `~/acme` salvo reindex aprobado.
- CI: job `cross-check` en pushes a `fix/**` (`.github/workflows/rust.yml`).

## Archivos

- **Modificar:** este archivo (`## Resultado`) y el master (§13 Cierre) al cerrar el plan.

## Pasos

- [ ] **Paso 1 — aprobación.** Pedir al usuario qué repos reindexar (estimación de tiempo con el
      reuso de vectores medido en TASK-002): mínimo backend-b, legacy-migration,
      DevCtxEngine; idealmente backend-a y frontend.
- [ ] **Paso 2 — reindex** con el candidato (en copias o en sitio, según lo aprobado),
      cronometrando y midiendo tamaño y `VmHWM`.
- [ ] **Paso 3 — arnés completo** y tabla de §6 fila por fila (Antes / Meta / Medido / Veredicto).
- [ ] **Paso 4 — casos de campo** uno por uno con la salida real de la tool.
- [ ] **Paso 5 — contratos.** Respuestas de `search` (vector), `read_symbol`, `get_references`,
      `impact_analysis`, `build_context` de 0.9.0 vs candidato sobre los mismos casos: solo
      diferencias aditivas o mejoras justificadas.
- [ ] **Paso 6 — rama v1.** Un repo sin reindexar con el candidato: aviso de índice viejo, tools
      viejas contestan, tools nuevas piden `index --full`.
- [ ] **Paso 7 — cross-check** Windows/macOS empujando una rama `fix/**`; anotar el run.
- [ ] **Paso 8 — cierre del plan** (§13 del master) con lo verificado y lo no verificado.

## Criterios de aceptación

- [ ] Tabla de §6 completa con veredicto por fila; toda fila que no cumple tiene causa y decisión
      (fix, `skipped` con motivo, o aceptación explícita del usuario).
- [ ] Cross-check verde antes de taggear.
- [ ] `devctx plan-status PLAN-009` sin warnings al cerrar.

## Riesgos

- El reindex de los repos grandes puede no caber en una sesión: se mide lo aprobado y se declara el
  resto como no verificado, como hizo PLAN-008 TASK-016 con frontend.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
