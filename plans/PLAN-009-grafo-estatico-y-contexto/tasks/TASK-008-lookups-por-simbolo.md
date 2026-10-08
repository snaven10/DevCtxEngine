# TASK-008 — Lookups por símbolo: `read_symbol`, anclaje, `get_references`, `external`, sugerencias, vista web

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-005
- **Estado:** `pending`

---

## Objetivo

Que todas las lecturas que hoy buscan por nombre o sufijo sobre `vectors.symbol`/`graph_edges`
pasen a la tabla `symbols` y a `edges` por id, **conservando sus contratos JSON** (`anchored`,
`suggestions`, `external`, `called_from`, `next_step`, `resolved_symbols`, `branch_fallback`,
`omitted`), y cerrar los pendientes 1 y 2 de PLAN-008 y el hallazgo B2 de su campo. En ramas sin
reindexar, el camino viejo con el aviso de índice viejo. Implementa DD-10 y la parte de lectura de
DD-19.

## Contexto verificado

- `Store::symbol_definitions` (`crates/devctx-store/src/store.rs:865-930`): exacto → `ends_with`
  `.`/`::` → regex sobre `grouped`. Usado por `do_read_symbol` (`crates/devctx-mcp/src/state.rs:5546-5570`)
  y el CLI (`crates/devctx-cli/src/main.rs:2191`).
- `Store::symbol_matches` (`store.rs:995-1032`): el anclaje de `anchor_identifiers`
  (`crates/devctx-search/src/lib.rs:664-704`), con `rank_definitions` (l.715-728), `ANCHOR_MAX` 3
  (l.360), `ANCHOR_FETCH` 48 (l.365), `ANCHOR_TOKENS` 3 (l.369).
- `is_anchored_definition` (`state.rs:675-682`) marca `anchored` por sufijo.
- `Store::symbol_suggestions` (`store.rs:940-987`), `not_found_hints` (`state.rs:5577-5620`),
  `Store::external_call_sites` (`crates/devctx-store/src/graph.rs:227-258`, fallback por último
  segmento en l.254-257), `Store::resolve_symbol` (`graph.rs:183-210`), `Store::find_references`
  (`graph.rs:304-332`), `do_references` (`state.rs:6091-6125`).
- `split_file_symbol` (`state.rs:5622-5631`): acepta cualquier "extensión" alfanumérica de ≤ 5
  caracteres → `Foo.Bar::baz` se lee como archivo `Foo.Bar` (pendiente 1 de PLAN-008).
- Vista web: `do_graph` (`state.rs:1873+`) usa `graph_edges` y `graph_defined_symbols`
  (`graph.rs:153-160`, "definido" = aparece como source).
- Selección de grupo: `Srv::defines` hace `GET /symbol/<name>?limit=1` (`state.rs:5122-5128`), que
  termina en `symbol_definitions`.
- Hallazgos de campo de PLAN-008 TASK-016: B2 (`external` falla con calificados), B3 (sugerencias
  ruidosas para externos), B4 (`memories_by_symbol("links.rs::memories_by_symbol")` cae a
  `inference`).
- TUI: `crates/devctx-tui/src/lib.rs:230` llama `impact_analysis` (no cambia de firma acá).

## Archivos

- **Modificar:** `crates/devctx-store/src/store.rs` (`symbol_definitions`, `symbol_matches`,
  `symbol_suggestions` sobre `symbols` + chunks por rango, DD-4), `crates/devctx-store/src/graph.rs`
  (`resolve_symbol`, `find_references`, `external_call_sites`, `graph_defined_symbols`, `graph_edges`
  de la vista web sobre `edges`; camino viejo si la rama no tiene filas en `symbols`),
  `crates/devctx-mcp/src/state.rs` (`is_anchored_definition` por id, `not_found_hints`,
  `split_file_symbol`, `do_references` con `confidence`, `do_graph`), `crates/devctx-search/src/lib.rs`
  (los hits anclados llevan su id), `crates/devctx-core/src/types.rs` si `VectorMetadata` necesita
  transportar `symbol_id` en memoria (no en el schema de `vectors`).

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) `split_file_symbol("Foo.Bar::baz")` → no es archivo; con
      `links.rs::memories_by_symbol` sí (pendiente 1, B4); (b) `Foo::new` inexistente con un `new`
      hoja definido en el repo → **no** `external` (pendiente 2); (c) `serde_json::from_str` y
      `Panache.withTransaction` → `external: true` con `called_from` (B2); (d) `with_context`
      externo → sin sugerencias de near-miss (B3, regla ya existente que debe sobrevivir); (e)
      Python: `get_references("helper")` con dos llamadas desde el mismo método → dos referencias;
      (f) `read_symbol` de un arrow-const TS → definición.
- [ ] **Paso 2 — lecturas nuevas** por `symbols`/`edges` con las mismas firmas públicas y el mismo
      orden de salida (archivo, línea). Chunks del símbolo por rango (DD-4).
- [ ] **Paso 3 — contratos.** Los tests existentes de anclaje, sugerencias, `external`,
      `resolved_symbols` y `branch_fallback` se corren **sin modificar**; campos nuevos aditivos:
      `sym` (16 hex), `confidence` en referencias, `kind` del símbolo.
- [ ] **Paso 4 — camino viejo.** Si la rama no tiene filas en `symbols` (índice v1), cada lectura usa
      la implementación actual y la respuesta ya trae `extractor_stale`/warning (mecanismo de
      PLAN-008; test `an_index_without_extractor_record_warns_in_every_graph_answer` sigue verde).
- [ ] **Paso 5 — vista web** (`do_graph`) sobre `edges`, con `external`/`from_test` reales en vez de
      la heurística "nunca es source".
- [ ] **Paso 6 — medir** con el arnés: anclaje (definición primero, 8 consultas de PLAN-008),
      `read_symbol`/`get_references` en los casos de campo, latencia de `read_symbol` (p95).

## Criterios de aceptación

- [ ] Tests (a)-(f) verdes; tests de contrato de PLAN-008 verdes sin tocarlos.
- [ ] Anclaje: definición primero en ≥ 7/8 de las consultas de PLAN-008 TASK-016 (igual o mejor).
- [ ] `read_symbol("tokenInterceptor")` en frontend reindexado → definición en
      el archivo del interceptor de la librería de auth (o en fixture equivalente si el
      reindex de FrontEnd no está aprobado todavía; se dice).
- [ ] Rama v1 sin reindexar: respuestas iguales a 0.9.0 + aviso.

## Riesgos

- El orden de `symbol_matches` decide qué se ancla: mantener `rank_definitions` como único juez.
- `grouped` deja de ser necesario para encontrar funciones chicas (DD-4); no borrar la tercera
  pasada del camino viejo.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
