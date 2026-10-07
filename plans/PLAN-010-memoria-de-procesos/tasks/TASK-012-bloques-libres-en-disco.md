# TASK-012 — Bloques libres que crecen con las reescrituras: medir varias corridas y evaluar recuperar espacio

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama a definir (se ejecuta después de PLAN-009)
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Saber si el archivo DuckDB de un repo crece sin techo con el uso normal (reindexados que reescriben
archivos) y, si es así, tener una forma medida y segura de recuperar ese espacio. Agregada a PLAN-010
por decisión del usuario (2026-10-07). No bloquea la 0.10.0.

## Contexto verificado

- La decisión vigente (PLAN-009, DD-21) es "medir y no tocar": no se compacta y `graph_edges` se sigue
  escribiendo. El presupuesto de tamaño de PLAN-009 se mide en **bloques usados**, no en tamaño de
  archivo.
- Mediciones de PLAN-009 TASK-003 (follow-ups A-D) sobre DevCtxEngine, en bloques usados/libres de
  `PRAGMA database_size`:
  - índice desde cero con el binario equivalente a 0.9.0: 111/0;
  - `--full` con el binario nuevo (tablas `symbols`/`edges`): 167/75;
  - segundo `--full` sin cambios y un incremental sin cambios: 167/75 y 166/76 (no escriben, los
    libres no bajan);
  - un `--full` que reescribió 40 archivos: **178/95** — los libres crecen con las reescrituras.
- Esa última cifra cumple la condición que se había puesto para reabrir el tema.
- DuckDB no achica el archivo con `VACUUM`; recuperar espacio exige reescribir la base (p. ej.
  `EXPORT`/`IMPORT DATABASE` o `ATTACH` + `COPY FROM DATABASE` a un archivo nuevo y reemplazo
  atómico), con el riesgo conocido sobre índices HNSW (extensión VSS) y sobre el WAL.
- Instrumentación disponible: el bloque `memory` de `status` (tamaño de la base y del HNSW) y
  `scripts/memprobe.sh` (TASK-001).

## Archivos

- **Crear:** script de medición de N corridas (bajo `scripts/`), si no alcanza con `run.sh` del arnés
  de PLAN-009.
- **Modificar (solo si la evaluación lo justifica y el usuario lo aprueba):** `crates/devctx-store`
  (compactación), `crates/devctx-cli` (comando o flag), docs EN + ES.

## Pasos

- [ ] **Paso 1 — Medir la curva.** En un sandbox, sobre DevCtxEngine y un repo Java mediano (clon de
      solo lectura): indexar desde cero y después N ciclos (≥10) de "modificar K archivos +
      incremental" y algunos `--full` que reescriben. Registrar por ciclo bloques usados, libres,
      tamaño de archivo y tamaño del WAL. ¿Los libres se reutilizan en ciclos posteriores
      (meseta) o crecen sin techo?
- [ ] **Paso 2 — Identificar qué los genera.** Atribuir el crecimiento a tablas o índices (`vectors`,
      `symbols`, `edges`, `graph_edges`, FTS, HNSW): medir con y sin cada uno donde sea posible
      (p. ej. con `storage.hnsw` apagado, o sin reconstruir FTS).
- [ ] **Paso 3 — Evaluar opciones con números.** Al menos: (a) no hacer nada, si hay meseta;
      (b) compactar bajo pedido (`devctx index --compact` o comando de mantenimiento) reescribiendo
      la base a un archivo nuevo con reemplazo atómico; (c) compactar automáticamente cuando
      libres/usados supere un umbral; (d) reducir la fuente del crecimiento (p. ej. dejar de escribir
      `graph_edges` cuando ya no haga falta para el downgrade). Para cada una: espacio recuperado,
      tiempo, riesgo sobre HNSW/WAL y si el servidor tiene que estar parado.
- [ ] **Paso 4 — Recomendar y pedir decisión.** Presentar la recomendación al usuario. Implementar
      solo lo que apruebe, con tests (la compactación no pierde filas, conserva HNSW/FTS o los marca
      como pendientes, es atómica ante un corte).

## Criterios de aceptación

- [ ] Curva de N ≥ 10 ciclos registrada para los dos repos, con bloques usados/libres, tamaño de
      archivo y WAL por ciclo.
- [ ] Atribución del crecimiento a tablas o índices, con el método usado.
- [ ] Tabla de opciones con números y una recomendación; decisión del usuario registrada en el master.
- [ ] Si se implementa algo: tests que muestren que no se pierden filas y que un corte a mitad deja la
      base original intacta; gate verde y cross-check de CI.

## Riesgos

- Reescribir la base con un índice HNSW requiere la extensión VSS cargada antes del esquema (bug
  arreglado en PLAN-009 TASK-003): probar con y sin VSS.
- Una compactación con el servidor corriendo pelea por el lock exclusivo de DuckDB: tiene que parar el
  servidor o hacerse dentro de él.

## Resultado
<!-- SE LLENA AL CERRAR (estado done/skipped). Vacío mientras esté pending. -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
