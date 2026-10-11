# TASK-015 — FTS: verificar la hipótesis del español, stemmer/stopwords configurables y partido de identificadores

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (Rust + SQL de DuckDB)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-014
- **Estado:** `pending`

---

## Objetivo

Averiguar con números **por qué** el híbrido empeora el español (Hit@5 6/13 contra 8/13 del vectorial
en PLAN-009 TASK-001) y, si la causa es el análisis de texto del BM25, corregirlo: idioma de
stemmer/stopwords configurable y partido de `camelCase`/`snake_case`. No toca vectores: solo se
reconstruye el índice FTS (barato). PLAN-010 DD-13.

## Contexto verificado

- `Store::rebuild_fts` (`crates/devctx-store/src/store.rs:515-526`):
  `PRAGMA create_fts_index('vectors', 'id', 'text', overwrite = 1)` sin más parámetros.
- Defaults de DuckDB FTS (doc oficial, `core_extensions/full_text_search`, leída 2026-10-11):
  `stemmer = 'porter'`, `stopwords = 'english'` (571 palabras), `ignore = '(\\.|[^a-z])+'`,
  `strip_accents = 1`, `lower = 1`. Stemmers disponibles: 26 idiomas incluido `spanish`, más
  `porter` y `none`. `stopwords` acepta el nombre de una tabla de una columna `VARCHAR` o `'none'`.
  **Un solo stemmer y una sola lista de stopwords por índice.** `match_bm25(id, q, fields, k=1.2,
  b=0.75, conjunctive=0)`.
- Consecuencias (hipótesis, **sin verificar**):
  - H-FTS-1: las stopwords del español (`de`, `la`, `que`, `el`, `en`, `se`, `una`, `cómo`→`como`)
    no se quitan y meten ruido en consultas ES, sobre todo en repos con comentarios en español.
  - H-FTS-2: porter sobre palabras españolas produce raíces malas pero **consistentes** en consulta y
    documento; puede no ser la causa principal.
  - H-FTS-3: `lower` va antes que `ignore`, así que `findUserById` queda como un único token
    `finduserbyid`: una consulta "find user by id" (o su traducción) no lo toca. `snake_case` sí se
    parte (el `_` cae en `[^a-z]`). Los dígitos se descartan.
  - H-FTS-4: la causa no es el FTS sino la fusión: RRF (`k = 60`, `crates/devctx-search/src/lib.rs:18`,
    `reciprocal_rank_fusion` `:1005`) le da el mismo peso a un BM25 que en ES no encuentra nada útil.
- Búsqueda BM25: `store.rs:695-725` (`fts_main_vectors.match_bm25(id, ?)`). Fallback a vectorial si
  falta el índice (ADR-16).

## Archivos

- **Modificar:** `crates/devctx-store/src/store.rs` (`rebuild_fts` con parámetros; tabla auxiliar
  si se elige el partido de identificadores), `crates/devctx-core/src/config.rs` (`storage.fts`),
  `crates/devctx-search/src/lib.rs` (preprocesado de la consulta igual al del documento; peso de la
  fusión si H-FTS-4 se confirma), `crates/devctx-index` (huella del FTS en `index_meta` para
  reconstruir cuando cambia la config), `scripts/graph-eval/` (modo `keyword` por idioma).

## Pasos

- [ ] **Paso 1 — verificar antes de cambiar.** Sobre copias de los índices del set de TASK-014
      (scratchpad): (a) Hit@5/MRR por idioma del modo `keyword` solo, no solo del híbrido; (b) para
      cada caso ES que el híbrido pierde frente al vectorial, la lista BM25 y qué términos matchean
      (`stem(...)` de la consulta y del chunk esperado); (c) cuántos casos ES tienen el término clave
      en un identificador camelCase del chunk esperado. Resultado: qué hipótesis H-FTS-1…4 explican
      cada caso perdido. **Si ninguna se confirma, la task cierra `skipped` con los datos.**
- [ ] **Paso 2 — matriz de configuraciones** (solo reconstruye FTS; cada celda es un
      `rebuild_fts` sobre la copia):
      | id | stemmer | stopwords | identificadores |
      |---|---|---|---|
      | F0 | porter | english | no (hoy) |
      | F1 | porter | english+spanish (tabla) | no |
      | F2 | none | english+spanish | no |
      | F3 | spanish | english+spanish | no |
      | F4 | mejor de F1-F3 | english+spanish | partidos |
      | F5 | dos índices (porter / spanish) fusionados por RRF | ídem | partidos |
      F5 solo si F1-F4 no alcanzan el umbral (cuesta el doble de FTS).
- [ ] **Paso 3 — partido de identificadores.** Tabla auxiliar `fts_docs(id, body)` que se rehace en
      `rebuild_fts`: `body = text || ' ' || <text con camelCase partido>` vía `regexp_replace` en SQL
      (`([a-z0-9])([A-Z])` → `\1 \2`, `([A-Z]+)([A-Z][a-z])` → `\1 \2`), y el índice FTS sobre ella.
      La consulta pasa por la misma función antes de `match_bm25`. Así no cambia el schema de
      `vectors`. Medir el tamaño extra (bloques usados) y el tiempo de `rebuild_fts`.
- [ ] **Paso 4 — si H-FTS-4 se confirma**, probar peso por idioma o por lista en la fusión (p. ej.
      RRF ponderado `w_bm25 ∈ {1, 0.5}`) y, como alternativa, híbrido solo si el BM25 tiene algún
      término con IDF alto. Se elige con números, no se agregan perillas que no ganan.
- [ ] **Paso 5 — config y reconstrucción.** `storage.fts: { stemmer, stopwords: [english, spanish],
      split_identifiers: true }` con el default ganador. La huella de la config FTS va a `index_meta`
      (`fts_fingerprint`); si cambia, el próximo `index` reconstruye solo el FTS (no re-embebe) y
      `status` lo informa. Stopwords en español: lista versionada en el repo (fuente con licencia
      compatible, p. ej. la de Snowball, BSD), cargada a una tabla al crear el índice.
- [ ] **Paso 6 — medir y decidir** con el arnés de TASK-014 (3 corridas, mediana).

## Criterios de aceptación

- [ ] Paso 1 en el Resultado: tabla caso a caso de los ES perdidos con la hipótesis que lo explica.
- [ ] Con el default elegido, **híbrido ES**: Hit@5 ≥ +10 pp **o** MRR ≥ +0.05 sobre la línea base
      de TASK-014, con IC 90 % sin cruzar 0; **híbrido EN** sin bajar más de 1 caso; brief sin
      regresión. Si solo se logra igualar al vectorial en ES, se documenta y se decide con el usuario.
- [ ] `rebuild_fts` con la config nueva ≤ +20 % de tiempo y ≤ +20 % de bloques del FTS en
      frontend (el índice más grande del grupo).
- [ ] Tests: `split_identifiers` parte `findUserById`, `HTTPServer2Config`, `user_id` igual en
      consulta y documento; cambiar `storage.fts` marca el FTS para reconstruir y no toca vectores;
      sin la extensión FTS, sigue el fallback de hoy.

## Riesgos

- **Mejorar ES y empeorar EN** con stopwords ES (p. ej. `a`, `no`, `se` como tokens en código).
  Mitigación: el criterio EN es duro; F1 vs F2 lo separa.
- **DDL de FTS y el WAL** (bug conocido: `fts_main_vectors` imposible de dropear al reabrir,
  `store.rs:1825-1830`): la tabla auxiliar sigue el mismo patrón de checkpoint tras DDL.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
