# TASK-022 — Docs EN + ES y verificación de campo de la `0.10.x` (bloque de calidad sin cambio de vectores)

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda; release en dos partes, Q-9)
- **Especialista:** general-purpose (docs) + orquestador (campo, con aprobación del usuario)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`) + lectura/medición en los repos del grupo
- **Release:** `0.10.x`
- **Depende de:** TASK-013, TASK-014, TASK-015, TASK-016, TASK-018, TASK-021
- **Estado:** `pending`

---

## Objetivo

Que la `0.10.x` salga documentada y verificada en campo, igual que la `0.11.0` con TASK-010/011,
pero acotada a lo que trae: huella v2 (TASK-013), set de evaluación (TASK-014), FTS (TASK-015),
`queries` (TASK-016), la recomendación medida del reranker (TASK-018, sin cambiar el default) y el
retiro de `jina-v2` (TASK-021). La verificación de campo corre **solo con OK del usuario**.

## Contexto verificado

- TASK-010 (docs) y TASK-011 (campo) dependen del motor propio y caen en la `0.11.0` (§10): sin esta
  task, la `0.10.x` saldría sin docs ni verificación.
- Docs EN/ES en `docs/` (precedente: PLAN-008 TASK-015); re-verificar ubicación y formato al arrancar.
- "A rebuilt binary does not take effect until the server restarts" (AGENTS.md): `devctx serve
  --stop` en cada repo y en el central antes de medir.
- Arnés: `scripts/graph-eval/` con los casos del repo y los privados por `$DEVCTX_EVAL_CASES`.

## Archivos

- **Modificar:** docs EN y ES (configuración: `storage.fts`, `queries` en `search`/`recall`, huella
  de embeddings y avisos de `status`, reranker: estado de la recomendación y retiro de `jina-v2`),
  `CHANGELOG.md` (entrada de la `0.10.x`, con las notas de release de §10 del master).
- **Crear:** — (el Resultado de esta task guarda los números de campo).

## Pasos

### Parte A — docs (sin aprobación especial)

- [ ] **A1.** Referencia de `storage.fts` (stemmer, stopwords, `split_identifiers`, default elegido en
      TASK-015 y que cambiarlo reconstruye solo el FTS) en EN y ES.
- [ ] **A2.** `queries` en `search`/`recall`: cuándo mandarlo (consulta en otro idioma que el código),
      tope, cómo se fusiona (método elegido en TASK-016).
- [ ] **A3.** Huella de embeddings: qué es, por qué un cambio de modelo/pooling/ONNX re-embebe, que
      actualizar a la `0.10.x` **no** re-embebe (v1 → v2 re-sellada), y el aviso de `status`.
- [ ] **A4.** Reranker: `jina-v2` retirado (qué pasa si estaba configurado); la recomendación de
      TASK-018 como opción manual por `model_dir` hasta la `0.11.0`.
- [ ] **A5.** CHANGELOG con las notas de release de la `0.10.x` (§10), incluida la de **volver a la
      `0.10.0`**: dispara un `--full` por la huella v2, salvo que se borre la clave
      `embedding_fingerprint` de `index_meta` (comando exacto en §9 del master).

### Parte B — verificación de campo (con OK explícito del usuario antes de empezar)

- [ ] **B1. Sin re-embebido.** Binario nuevo instalado, servers parados; en cada repo del grupo y en
      DevCtxEngine, `devctx index` (incremental): cero chunks embebidos por archivos sin cambio y
      huella re-sellada a v2 (`index_meta`); `status` sin aviso de reindex.
- [ ] **B2. Memorias.** `recall` responde en un repo de cada tier (local, group, global); la huella
      del espacio de memorias quedó sellada; un `memories export` + `import --dry-run` reporta
      `reused`, no `recomputed`.
- [ ] **B3. FTS.** El primer `index` reconstruye solo el FTS con la config nueva (tiempo anotado);
      `search --hybrid` responde.
- [ ] **B4. Calidad.** Arnés de TASK-014 completo (3 corridas, mediana) contra la línea base de
      TASK-014: la `0.10.x` cumple la regla por task de PLAN-010 DD-12 en lo que trae (FTS y
      `queries`); se anota cuánto falta para la regla por bloque, que se cierra en TASK-011.
- [ ] **B5. `jina-v2`.** En una copia de config de prueba (no la real), `reranking.model: jina-v2` →
      `search` responde y `status` muestra el aviso.
- [ ] **B6. Rollback probado en una copia.** Sobre una copia de un índice en el scratchpad: volver a la
      `0.10.0` sin borrar la clave → `--full` (confirmar); borrando la clave → incremental sin
      re-embebido (confirmar).

## Criterios de aceptación

- [ ] Cada clave y parámetro nuevo de la `0.10.x` en EN y ES, con default.
- [ ] B1-B6 con su número real y veredicto en el Resultado; desviaciones explicadas.
- [ ] `git status` limpio en los repos del grupo (solo lectura); nada de datos de clientes en el
      repo (grep de privacidad del plan vacío).

## Riesgos

- **Medir sobre repos vivos.** Solo `index` incremental y `search`/`recall`; el único escrito es el
  re-sellado de huella y el FTS, que es justamente lo que se verifica. Nada de `--full`.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
