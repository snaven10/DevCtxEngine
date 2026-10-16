# TASK-011 — Verificación de campo antes/después contra PLAN-010 §6

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** — (orquestador, con el binario nuevo instalado y aprobación del usuario)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`) + lectura/medición en
  `/home/you/acme`
- **Depende de:** TASK-010
- **Nota (2026-10-11):** con el bloque de calidad (TASK-013…019) suma la verificación de calidad y la migración opcional de vectores (pasos al final).
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
- Sin reindex: el criterio es que los índices existentes (DevCtxEngine, frontend y el resto
  del grupo `acme`) sigan sirviendo sin `index --full`.

## Pasos

- [ ] **E1.** Central en reposo tras el idle de modelos → RSS (meta ≤ 150 MB).
- [ ] **E2.** DevCtxEngine `index --full` → `VmHWM` del serve (meta ≤ 1.5 GB) y chunks/s
      (≥ −15 %). **Nota:** `--full` re-embebe DevCtxEngine; es el único reindex permitido y es de
      este repo, no de acme.
- [ ] **E3.** frontend indexación incremental (un commit trivial en una copia/worktree de
      prueba, o la próxima indexación real con aprobación) → `VmHWM` (meta ≤ 1.5 GB).
- [ ] **E4.** 3 serves con modelo cargado → `Pss`/`RssAnon`/`RssFile` (meta: pesos compartidos,
      si TASK-006 quedó `done`).
- [ ] **E5.** 20 `search` → p50/p95 (≤ +10 %) y top-10 igual al de 0.8.5 en las consultas del arnés
      contra índices reales.
- [ ] **E6.** `status.memory` en serve y central: los cuatro bloques con números coherentes con
      `/proc`.
- [ ] **Sin reindex.** `status` en los repos de acme: sin aviso de reindex nuevo; `search`/`recall`
      responden.
- [ ] **DuckDB.** HNSW de frontend sigue presente y usable con el `memory_limit` nuevo
      (`status.memory.duckdb` y `hnsw`).
- [ ] **`provider: central`** (si TASK-009 `done`): un repo de prueba con el modo activo, RSS en
      reposo y `search` OK; central detenido → error explícito.
- [ ] **Zombies.** `ps` sin `<defunct>` hijos de procesos ≥ 0.8.3.
- [ ] **Calidad (TASK-013…019).** Arnés de TASK-014 completo (3 corridas, mediana) con el binario
      nuevo **antes** de migrar nada: FTS nuevo, `queries` y reranker nuevo contra la línea base de
      TASK-014; filas de calidad de §6 con su veredicto.
- [ ] **Huella.** En cada repo del grupo, el primer `index` con el binario nuevo es incremental y no
      re-embebe (huella v1 → v2 re-sellada); `status` muestra `upgrade_available` si aplica.
- [ ] **Migración declarada (solo con aprobación explícita del usuario, después de lo anterior).**
      `memories export` de los tres tiers → `memories reembed` → `index --full --upgrade-embeddings`
      repo por repo (15 repos, ~4-4.5 h). Re-correr el arnés después y anotar el antes/después por
      idioma. Si el usuario no la aprueba, se documenta y queda como paso del usuario.

## Criterios de aceptación

- [ ] Cada fila de PLAN-010 §6 con su número real y veredicto; desviaciones explicadas.
- [ ] `git status` limpio en los repos de acme (solo lectura).
- [ ] Sin vectores de dos espacios en ningún store: cada rama y cada store de memorias con una sola
      huella (consulta a `index_meta`).
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
