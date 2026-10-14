# TASK-019 — Mediciones finales con compactación, cierre de §6, cross-check e informe

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** — (orquestador, con aprobación del usuario; la sesión revisora mide)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-018
- **Estado:** `pending`

---

## Objetivo

Cerrar el plan sobre el binario **con** la compactación de TASK-018: re-medir lo que la compactación
cambia (tamaño, tiempo y RAM del upgrade, downgrade), completar la tabla de §6, correr el
cross-check de Windows/macOS y escribir el cierre (§13). Es la "etapa B" que se separó de TASK-016
por decisión de la revisión del 2026-10-13: TASK-016 queda con lo ya medido sobre el candidato sin
compactación.

## Contexto verificado

- TASK-016 (Resultado) midió las filas de §6 con el candidato `e0b8937` + `fb1707c`, sin
  compactación. Su hallazgo 5 (el upgrade en sitio crece +46-58 % en bloques usados y deja el
  archivo en ~2×) motivó TASK-018. Sus mediciones que **no** dependen del tamaño ni del camino del
  upgrade siguen valiendo y no se repiten.
- Lo que cambia con TASK-018 y hay que volver a medir: la fila "Tamaño del DB" para el upgrade en
  sitio, el tiempo total del upgrade que ve el usuario (ahora incluye parar el serve y compactar),
  el pico de RAM y de disco de ese recorrido, y el downgrade sobre una base compactada.
- Base de comparación del tamaño: `B0` de TASK-018 (el índice 0.9.0 justo antes del `--full` en
  sitio, sobre una copia), en bloques usados y en bytes.
- Método, sandbox y arnés: los de TASK-016 (`scripts/graph-eval/`, casos privados en
  `$DEVCTX_EVAL_CASES`, repos solo con alias).
- Cross-check: job `cross-check` en pushes a `fix/**` (`.github/workflows/rust.yml`); era el Paso 7
  de TASK-016.
- Decisión del usuario del 2026-10-13 (master §12): las filas 1, 3 y 15 de §6 son limitaciones
  conocidas de 0.10.0.

## Archivos

- **Modificar:** este archivo (`## Resultado`), el master (§6 si hay que anotar el número medido,
  §13 Cierre), `docs/07-performance.md` + `docs/es/07-rendimiento.md` y `CHANGELOG.md` con las
  cifras finales (si TASK-018 dejó las suyas, se reemplazan por estas).

## Pasos

- [ ] **Paso 1 — aprobación.** Confirmar con el usuario los repos (mínimo backend-a y frontend;
      DevCtxEngine de control) y que el binario a medir es el candidato final (versión ya subida a
      0.10.0). Antes de medir, y asignado a esta task por la revisión de TASK-018:
      - **el bump a 0.10.0** del workspace: hoy la nota de compactación escribe `done:0.9.0:…`;
      - en el `CHANGELOG.md`, como *Known limitations*, las filas 1, 3 y 15 de §6 (decisión del
        usuario del 2026-10-13);
      - el **contrato de downgrade**, en el CHANGELOG y en `docs/13` + ES: las rutas de módulo quedan
        como en 0.9.0 y las llamadas por tipo como `Tipo.metodo`; 0.9.0 las sigue encontrando por
        nombre y recorre un nivel más;
      - el **known issue de HNSW**: reconstruir el índice cambia el top-k aproximado (el exacto no;
        medido en TASK-018).
- [ ] **Paso 2 — upgrade con compactación.** Por repo, sobre una copia del índice 0.9.0 real:
      medir `B0`; `devctx index --full` por el serve con el disparo automático; registrar bloques
      usados/libres y bytes tras el `--full` y tras compactar, segundos de cada parte y total,
      `VmHWM` del proceso que compacta y disco temporal máximo. Veredicto contra ≤ 1,30 × `B0`.
- [ ] **Paso 3 — lo que no debe cambiar.** Sobre la base compactada: arnés híbrido y vectorial y el
      brief (los 30 casos) iguales a los del índice actualizado sin compactar de TASK-016 (misma
      base de vectores: se espera igualdad salvo empates); `impact("get"/"map")` p95; HNSW y FTS
      usados por la consulta.
- [ ] **Paso 4 — downgrade.** `v0.9.0` sobre la base compactada: los 60 casos del downgrade de
      TASK-016, mismas respuestas que sobre la no compactada; ningún llamador perdido.
- [ ] **Paso 5 — casos límite de campo.** Una base con dos ramas (frontend); una base con memorias
      locales reales (copia, solo lectura sobre el original): `recall` antes y después; un upgrade
      sin TTY y uno con `DEVCTX_NO_AUTO_COMPACT=1` (quedan `owed`, `status` lo dice) seguidos de
      `devctx compact`. Además (revisión de TASK-018): un **ensayo de restauración** desde el
      sidecar (`<db>.pre-compact.duckdb`, `devctx memories import`); una **sesión MCP activa**
      durante la compactación (el `remember` recibe "se está compactando" y entra al reintentar);
      y la **comprobación de lock de M-B** (el segundo `flock` falla) sobre el filesystem real.
- [ ] **Paso 6 — tabla de §6** completa con veredicto por fila: las filas medidas en TASK-016 se
      citan; la de tamaño lleva los dos números (desde cero y upgrade + compactación); las filas 1,
      3 y 15 van como limitaciones conocidas con su decisión.
- [ ] **Paso 7 — cross-check** Windows/macOS empujando una rama `fix/**`; anotar el run. Incluye lo
      que TASK-018 dejó para acá: el `rename` en Windows y el lock de sistema operativo en las tres
      plataformas. Si Windows no se puede confirmar: disparo automático apagado ahí y documentado.
- [ ] **Paso 8 — cierre del plan** (§13 del master) e informe: qué se verificó, qué no, y los
      pendientes que pasan al plan siguiente.

## Criterios de aceptación

- [ ] backend-a y frontend: usados y bytes tras upgrade + compactación ≤ 1,30 × `B0`, con tiempo,
      RAM y disco temporal anotados; o la fila declarada como no cumplida con causa y decisión del
      usuario.
- [ ] Búsqueda, brief e `impact` sobre la base compactada sin regresión contra TASK-016.
- [ ] Downgrade sobre la base compactada sin llamadores perdidos.
- [ ] Tabla de §6 completa; toda fila que no cumple tiene causa y decisión.
- [ ] Cross-check verde antes de taggear.
- [ ] `devctx plan-status PLAN-009` sin warnings al cerrar.

## Riesgos

- El reindex de los repos grandes puede no caber en una sesión: se mide lo aprobado y se declara el
  resto como no verificado.
- La carga de la máquina mueve los tiempos ±20 % (TASK-016): los tiempos se dan con la carga
  anotada; el tamaño no depende de ella.

## Resultado
<!-- SE LLENA AL CERRAR (estado done/skipped). Vacío mientras esté pending. Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
