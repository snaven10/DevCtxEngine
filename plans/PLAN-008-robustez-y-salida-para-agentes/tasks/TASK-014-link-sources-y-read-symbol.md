# TASK-014 — `link_sources` correcto y sugerencias en `read_symbol`

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-006
- **Estado:** `done`

---

## Objetivo

Que una memoria cuyo campo `files` nombra el archivo nunca se reporte como `inference`, y que un
`read_symbol` sin resultado sugiera candidatos y diga si el nombre parece externo (JDK/librería).

## Contexto verificado

- `crates/devctx-mcp/src/state.rs:3278-3360` — `linked_response`: si la junction no trae nada, cae a
  `memories_mentioning` y marca **todo** como `"inference"` (`:3320`, `:3330`), aunque `m.files`
  contenga el archivo consultado.
- `crates/devctx-store/src/memory_refs.rs:14-49` — semántica de `files-field`/`content-mention`;
  `:229` emite `files-field` al escribir.
- `state.rs:3120-3139` — `do_read_symbol` devuelve `definitions: []` sin más;
  `crates/devctx-store/src/store.rs:658-…` — `symbol_definitions` exacto → `%.name`/`%::name`.
- Inventario: ~70 % de los targets del grafo son externos (JDK/librerías); "externo" hoy es una
  heurística solo en `do_graph` (target que nunca es source).

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs`
- **Modificar:** `crates/devctx-store/src/store.rs` (consulta de sugerencias)

## Pasos

- [x] **Paso 1 — `link_sources`.** En el fallback de `linked_response`, si el `files` de la memoria
      contiene el sujeto (comparando ruta normalizada y sufijo), marcar `files-field`; solo lo demás
      es `inference`. Investigar por qué la junction no tenía la fila (¿rama?, ¿ruta relativa vs
      absoluta?, ¿memoria central?) y anotarlo en el Resultado.
- [x] **Paso 2 — Sugerencias.** `read_symbol` sin definiciones → `suggestions`: hasta 5 símbolos por
      prefijo/sufijo/distancia de edición sobre `vectors.symbol` de la rama elegida (TASK-006).
- [x] **Paso 3 — Externo.** Si el nombre aparece como target en `graph_edges` pero nunca como
      definición ni source → `"external": true` + `"called_from": N` sitios (con `get_references`
      como siguiente paso sugerido).

## Criterios de aceptación

- [x] Test: memoria con `files: "src/a.rs"` sin fila de junction → `memories_by_file("src/a.rs")` la
      trae con `link_sources: "files-field"`.
- [x] Test: `read_symbol("AuthServce")` → `suggestions` contiene `AuthService`.
- [x] Test: `read_symbol("assertEquals")` en un repo con tests → `external: true`.

## Riesgos

Distancia de edición sobre todos los símbolos puede ser cara en repos grandes: limitar a candidatos
por prefijo/sufijo en SQL y rankear en memoria.

## Resultado

1. **Estado final:** `done`.
2. **Repro antes/después:** antes, `linked_response` marcaba todo lo del fallback de texto como `"inference"` aunque `m.files` nombrara el archivo; `read_symbol("AuthServce")` devolvía `definitions: []` sin más. Después: `files-field` / `suggestions` / `external`. Cubierto por los tests de abajo (los tests nuevos no compilan/fallan en el padre: faltan `fallback_source`, `symbol_suggestions`, `external_call_sites`).
3. **Causa raíz:** `state.rs` `linked_response` (fallback, antes `:3320`/`:3330`) hardcodeaba `"inference"`. La fila de junction falta cuando la memoria se guardó sin grafo/índice, se importó/migró, o el llamante usa otra grafía de la ruta (el lookup `memory_ids_for_file` es por igualdad exacta: `a.rs` vs `src/a.rs`). El fallback no mira ramas ni memoria central como causa; sí la ruta exacta. No se pudo reproducir con datos reales (sin acceso a stores del usuario).
4. **Archivos/símbolos:** `devctx-store`: `Store::symbol_suggestions(repo, branch, name, n) -> Result<Vec<String>>` (store.rs), `Store::external_call_sites(repo, branch, name) -> Result<Option<usize>>` (graph.rs). `devctx-mcp/state.rs`: `linked_response(.., file_subject: &[String], ..)`, `text_fallback_local`, `fallback_source`, `not_found_hints`.
5. **Tests:** `a_memory_naming_the_file_is_files_field_even_without_a_junction_row`, `fallback_source_compares_normalized_paths_and_suffixes`, `read_symbol_not_found_suggests_and_marks_external`, `suggestions_rank_exact_case_then_substring_then_edit_distance`, `edit_distance_basics`. `cargo test --workspace` con `TMPDIR=/var/tmp DEVCTX_MODEL_CACHE=/var/tmp/devctx-test-model-cache`: verde (38 suites); fmt y clippy (con y sin `--features gpu`) sin warnings.
6. **Contrato JSON:** `read_symbol` sin definiciones agrega `suggestions: [..]` (hasta 5: formas calificadas del grafo y luego nombres cercanos por prefijo/sufijo/contiene/Levenshtein) y, si el nombre es solo target de llamadas, `external: true`, `called_from: N`, `next_step`. `branch_fallback` intacto. `memories_by_file`: en el fallback, `link_sources: "files-field"` si `files` nombra el archivo (ruta normalizada + sufijo); si todas lo son, `matched_by` pasa a `"junction"`.
7. **No verificado:** rendimiento de `symbol_suggestions` en repos grandes (tope 2000 candidatos por SQL); no se probó la rama central (daemon) del fallback; `memories_by_symbol` no marca `files-field` (su sujeto no es un archivo).
8. **Números:** n/a.

### Fixup E (review)

- **I4:** `external: true` falso para funciones chicas agrupadas (símbolo `a, b, c`, `symbol_type: grouped`):
  `Store::symbol_definitions` tiene un tercer paso que busca el nombre como elemento exacto de la lista
  (sin el sufijo `+n`) o en el encabezado por función del texto (`# file > name` / `# file > Parent > name`),
  así `read_symbol` devuelve el chunk agrupado como definición. El `next_step` de `external` se suavizó: "no
  indexed definition was found on this branch — likely a library or runtime function, though it may live in a
  file the index excludes or on another branch". Test (falla en el padre):
  `a_grouped_small_function_is_found_not_external` (incluye que `get_a` no matchee por substring).
- **M4:** con ruta resuelta, el sujeto `files-field` es solo esa ruta (`file_subjects`): un `mod.rs` pelado ya no
  marca cualquier `*/mod.rs`.
- **M5:** `symbol_suggestions` rankea en SQL antes del tope (`levenshtein` sobre el último segmento, luego
  `jaro_winkler_similarity`, luego el nombre para desempatar; LIMIT 200), "contenido en la consulta" solo para
  símbolos de ≥ 3 caracteres, y excluye símbolos `grouped`. Test
  `the_closest_symbol_survives_a_flood_of_candidates` (20k candidatos; falla en el padre). Tarda ~20 s por el
  upsert de 20k filas.

### Fixup L (campo, B2/B3/B4)

- **B2** `Store::external_call_sites` busca el nombre calificado tal cual y, si falla, por su último segmento (`.` o
  `::`), y acepta destinos `…::name` además de `….name`; un último segmento definido en el repo no se toma por externo.
  `serde_json::from_str`, `tokio::spawn`, `Panache.withTransaction` -> `external: true`.
- **B3** con `external: true` no se ofrecen sugerencias difusas del repo (`with_context` ya no sugiere
  `build_context`); solo las formas calificadas del mismo nombre, y el campo se omite si no hay. Sin externo todo sigue
  igual. Test `qualified_externals_are_found_and_get_no_noisy_suggestions` (falla en el padre en el primer
  calificado).
- **B4** `memories_by_symbol("links.rs::memories_by_symbol")` parte `archivo::símbolo` (`split_file_symbol`,
  `symbol_query`): el símbolo se busca solo y el archivo es el sujeto contra el que se compara `files` -> `files-field`
  en vez de `inference`. Un módulo (`devctx_store::Store`) no se toma por archivo. Test
  `a_file_qualified_symbol_matches_files_field_against_its_file`.
