# TASK-011 — Verificación de campo antes/después contra PLAN-010 §6

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** — (orquestador, con el binario nuevo instalado y aprobación del usuario)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`) + lectura/medición en
  `/home/snaven10/revfa`
- **Depende de:** TASK-010
- **Estado:** `pending`

---

## Objetivo

Medir cada fila de PLAN-010 §6 con el binario nuevo, contra la línea base 0.8.5 de TASK-001, con la
misma metodología (`scripts/memprobe.sh`, escenarios E1-E6), y dejar el veredicto por fila. Es la
fase de verify del plan.

## Contexto verificado

- Línea base y metodología: TASK-001 (Resultado) y PLAN-010-design DD-1.
- "A rebuilt binary does not take effect until the server restarts" (`AGENTS.md`):
  `devctx serve --stop` en cada repo y del central antes de medir; las sesiones MCP viejas siguen
  vivas hasta cerrarse.
- Sin reindex: el criterio es que los índices existentes (DevCtxEngine, REVFA_FrontEnd y el resto
  del grupo `revfa`) sigan sirviendo sin `index --full`.

## Pasos

- [ ] **E1.** Central en reposo tras el idle de modelos → RSS (meta ≤ 150 MB).
- [ ] **E2.** DevCtxEngine `index --full` → `VmHWM` del serve (meta ≤ 1.5 GB) y chunks/s
      (≥ −15 %). **Nota:** `--full` re-embebe DevCtxEngine; es el único reindex permitido y es de
      este repo, no de revfa.
- [ ] **E3.** REVFA_FrontEnd indexación incremental (un commit trivial en una copia/worktree de
      prueba, o la próxima indexación real con aprobación) → `VmHWM` (meta ≤ 1.5 GB).
- [ ] **E4.** 3 serves con modelo cargado → `Pss`/`RssAnon`/`RssFile` (meta: pesos compartidos,
      si TASK-006 quedó `done`).
- [ ] **E5.** 20 `search` → p50/p95 (≤ +10 %) y top-10 igual al de 0.8.5 en las consultas del arnés
      contra índices reales.
- [ ] **E6.** `status.memory` en serve y central: los cuatro bloques con números coherentes con
      `/proc`.
- [ ] **Sin reindex.** `status` en los repos de revfa: sin aviso de reindex nuevo; `search`/`recall`
      responden.
- [ ] **DuckDB.** HNSW de REVFA_FrontEnd sigue presente y usable con el `memory_limit` nuevo
      (`status.memory.duckdb` y `hnsw`).
- [ ] **`provider: central`** (si TASK-009 `done`): un repo de prueba con el modo activo, RSS en
      reposo y `search` OK; central detenido → error explícito.
- [ ] **Zombies.** `ps` sin `<defunct>` hijos de procesos ≥ 0.8.3.

## Criterios de aceptación

- [ ] Cada fila de PLAN-010 §6 con su número real y veredicto; desviaciones explicadas.
- [ ] `git status` limpio en los repos de revfa (solo lectura).
- [ ] `## 13. Cierre` del master redactado con lo verificado **y lo no verificado** (CUDA, macOS en
      runtime, Windows en runtime).

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
