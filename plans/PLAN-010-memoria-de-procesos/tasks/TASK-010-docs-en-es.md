# TASK-010 — Docs EN + ES

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-002, TASK-008, TASK-009
- **Estado:** `pending`

---

## Objetivo

Que un usuario pueda entender y ajustar el consumo de memoria de devctx sin leer código: qué pesa
cada proceso, qué perillas hay, cómo leer `status.memory` y cuándo usar `provider: central`.

## Contexto verificado

- Las docs del repo existen en EN y ES (precedente: PLAN-008 TASK-015). Re-verificar al arrancar
  la ubicación exacta (`docs/`, `README*.md`, `CHANGELOG.md`) y el formato de la sección de
  configuración.
- Claves nuevas de este plan: `storage.memory_limit`, `storage.threads` (TASK-002);
  `embeddings.{intra_threads, inter_threads, batch_size, cpu_arena, memory_pattern, max_length}` y
  equivalentes en `reranking` (TASK-007); `embeddings.provider: central`,
  `embeddings.central_fallback` (TASK-009); env `DEVCTX_DB_MEMORY_LIMIT`, `DEVCTX_DB_THREADS`,
  `DEVCTX_EMBED_*`, `DEVCTX_MODEL_IDLE_SECS`.

## Archivos

- **Modificar:** docs de configuración EN/ES, `CHANGELOG.md`, comentarios de `config.rs` que
  alimentan el YAML de ejemplo si lo hay.

## Pasos

- [ ] **Paso 1 — "Memoria y procesos"** (EN/ES): modelo de procesos (central + un serve por
      proyecto), qué cuesta cada uno (números de TASK-011), liberación por inactividad.
- [ ] **Paso 2 — referencia de claves** con default, env equivalente y precedencia.
- [ ] **Paso 3 — `status.memory`** con un ejemplo real anotado.
- [ ] **Paso 4 — `provider: central`**: cuándo conviene, cuándo no (indexación grande), qué pasa si
      el central cae.
- [ ] **Paso 5 — `max_length`** y el aviso de reindex.
- [ ] **Paso 6 — CHANGELOG** con la nota "sin reindex" y la salida de fastembed.

## Criterios de aceptación

- [ ] Cada clave nueva aparece en EN y ES con default y env.
- [ ] Los números citados son los de TASK-011 (no los de la planificación).

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
