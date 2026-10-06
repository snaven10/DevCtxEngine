# TASK-003 — Schema `symbols` + `edges`, ids estables, escritura por lotes y mantenimiento

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-001 (para el merge: la línea base se mide con 0.9.0 antes)
- **Estado:** `pending`

---

## Objetivo

Que el índice tenga una tabla de símbolos con identidad estable y una tabla de aristas tipadas con
una fila por ocurrencia, escritas en la misma transacción por archivo que hoy y mantenidas en
borrado, copia entre ramas, renombre y poda de rama. Con la extracción actual todavía (sin
resolución nueva), pero ya sin perder ocurrencias repetidas ni llamadas a nivel de módulo. Sube
`EXTRACTOR_VERSION` a 2. Implementa DD-2, DD-3, DD-4 y la parte de rollout de DD-19.

## Contexto verificado

- DDL relacional en `crates/devctx-store/src/schema.rs:109-271` (`RELATIONAL_DDL`), aplicado con
  `CREATE TABLE IF NOT EXISTS` por `init_schema` (l.12-25), que ya sabe hacer `CHECKPOINT` tras crear
  una tabla en un DB existente (l.15-22, patrón de `index_meta`).
- Advertencia ART: `schema.rs:163-168` y `:211-219` documentan índices que devolvieron 0 filas.
- Escritor actual: `Store::replace_file_edges` (`crates/devctx-store/src/graph.rs:64-96`) borra por
  archivo e inserta fila a fila con dedupe `(source,target,kind)` (l.75-79); `delete_file_edges`
  (l.99-105).
- Puntos de mantenimiento: `Indexer::index_file` (transacción, `crates/devctx-index/src/pipeline.rs:956-977`),
  `Indexer::delete_file` (`pipeline.rs:812-824`), `Store::copy_file_rows`
  (`crates/devctx-store/src/memory_refs.rs:333-419`, copia aristas en l.375-391),
  `Store::drop_branch` (`memory_refs.rs:427+`), `Store::rename_file` (`crates/devctx-store/src/store.rs:822-828`).
- `Symbol`/`ParsedFile` (`crates/devctx-parse/src/types.rs:4-65`) no tienen calificado, firma ni
  id; `parent` es un nombre. `qualified_source` arma `Clase.metodo` (`parser.rs:296-306`).
- Llamadas sin función envolvente se descartan (`parser.rs:125-127`).
- `EXTRACTOR_VERSION = 1` (`crates/devctx-parse/src/registry.rs:93`); FNV-1a en `registry.rs:96-100`;
  test `the_fingerprint_is_stable_between_calls` exige `v1-` (l.226-230) y hay que actualizarlo.
- Sello y aviso: `pipeline.rs:542-562`; `Store::extractor_stale`
  (`crates/devctx-store/src/state.rs:183-188`).

## Archivos

- **Crear:** `crates/devctx-store/src/symbols.rs` (tipos `StoredSymbol`, `StoredEdgeV2` o nombre
  equivalente; `replace_file_symbols`, `replace_file_graph`, lecturas básicas por archivo/id),
  `crates/devctx-parse/src/symbol_id.rs` (DD-3).
- **Modificar:** `crates/devctx-store/src/schema.rs` (DDL de DD-2), `crates/devctx-store/src/lib.rs`
  (exports), `crates/devctx-store/src/memory_refs.rs` (`copy_file_rows`, `drop_branch`),
  `crates/devctx-store/src/store.rs` (`rename_file`), `crates/devctx-index/src/pipeline.rs`
  (`index_file`, `delete_file`, `store_edges`), `crates/devctx-parse/src/types.rs` (campos nuevos:
  `qualified`, `id`, `parent_id`, `signature` provisoria = primera línea), `crates/devctx-parse/src/parser.rs`
  (símbolo de archivo, fuente de módulo), `crates/devctx-parse/src/registry.rs` (`EXTRACTOR_VERSION = 2`).

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) dos llamadas al mismo destino desde el mismo método → dos
      filas en `edges`; (b) llamada a nivel de módulo en Python → fila con `src` = símbolo de
      archivo; (c) id estable: mismo símbolo en dos ramas → mismo id; mover el cuerpo dentro del
      archivo → mismo id; sobrecarga Java → ids distintos.
- [ ] **Paso 2 — DDL.** `symbols` y `edges` según DD-2, sin PK/UNIQUE. Índices: ninguno al
      principio; medir `read_symbol`/`find_references` sobre la copia de backend-a y agregar solo
      los que hagan falta, cada uno con un test de igualdad que lo fije.
- [ ] **Paso 3 — ids.** `symbol_id(repo, file, kind_class, qualified, disambiguator)` con FNV-1a 64
      (DD-3); colisión dentro de un archivo → ordinal. Expuesto en JSON como 16 hex.
- [ ] **Paso 4 — símbolo de archivo y fuentes de módulo.** El parser emite un símbolo `file` por
      archivo y usa su id como `src` de las llamadas sin función envolvente (en vez de descartarlas).
- [ ] **Paso 5 — escritura por lotes.** `replace_file_symbols` / `replace_file_graph` borran el
      archivo y insertan con `Appender` (o `INSERT … VALUES` por lotes), dentro de la transacción de
      `index_file`. `graph_edges` se sigue escribiendo como hoy (DD-2), derivado de lo mismo.
- [ ] **Paso 6 — mantenimiento.** `delete_file`, `drop_branch`, `rename_file` (actualiza `file` y
      recalcula ids del archivo: el id lleva el archivo) y `copy_file_rows` (copia `symbols` tal cual
      y `edges` con `dst_id`/`confidence` en NULL para que los resuelva el link pass de TASK-005).
- [ ] **Paso 7 — `EXTRACTOR_VERSION = 2`** y test del fingerprint actualizado. `index_status` sobre
      un índice v1 dice `extractor_stale: true` (ya lo hace el mecanismo de PLAN-008; agregar el test
      con un índice v1 sembrado).
- [ ] **Paso 8 — medir** sobre backend-b y DevCtxEngine con el arnés: filas de
      `edges` vs `graph_edges`, tamaño del DB, tiempo de `index --full` (con TASK-002 si ya entró).

## Criterios de aceptación

- [ ] Los tres tests del paso 1 pasan; los de `graph.rs` existentes siguen verdes (lectura vieja).
- [ ] `index --full` deja `symbols` y `edges` poblados; `drop_branch`, `delete_file` y
      `copy_file_rows` no dejan filas huérfanas (test por cada uno).
- [ ] Un 0.9.0 que abre el DB resultante no falla (tablas nuevas ignoradas, `graph_edges` válido):
      test de compatibilidad leyendo `graph_edges` con las consultas de 0.9.0.
- [ ] Medido y anotado: crecimiento de filas (ocurrencias vs pares) y del DB; dentro de DD-21 o
      con la desviación explicada.

## Riesgos

- Volumen: una fila por ocurrencia puede duplicar las aristas; si el DB pasa +30 %, evaluar dejar
  `graph_edges` de escribir antes (Q del Resultado, no decisión unilateral).
- Índices ART: no agregar PK/UNIQUE; si un índice da resultados raros en test, sacarlo.
- `rename_file` cambia ids (el archivo es parte del id): documentarlo; memorias siguen por nombre.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
