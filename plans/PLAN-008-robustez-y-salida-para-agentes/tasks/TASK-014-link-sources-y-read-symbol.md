# TASK-014 — `link_sources` correcto y sugerencias en `read_symbol`

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-006
- **Estado:** `pending`

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

- [ ] **Paso 1 — `link_sources`.** En el fallback de `linked_response`, si el `files` de la memoria
      contiene el sujeto (comparando ruta normalizada y sufijo), marcar `files-field`; solo lo demás
      es `inference`. Investigar por qué la junction no tenía la fila (¿rama?, ¿ruta relativa vs
      absoluta?, ¿memoria central?) y anotarlo en el Resultado.
- [ ] **Paso 2 — Sugerencias.** `read_symbol` sin definiciones → `suggestions`: hasta 5 símbolos por
      prefijo/sufijo/distancia de edición sobre `vectors.symbol` de la rama elegida (TASK-006).
- [ ] **Paso 3 — Externo.** Si el nombre aparece como target en `graph_edges` pero nunca como
      definición ni source → `"external": true` + `"called_from": N` sitios (con `get_references`
      como siguiente paso sugerido).

## Criterios de aceptación

- [ ] Test: memoria con `files: "src/a.rs"` sin fila de junction → `memories_by_file("src/a.rs")` la
      trae con `link_sources: "files-field"`.
- [ ] Test: `read_symbol("AuthServce")` → `suggestions` contiene `AuthService`.
- [ ] Test: `read_symbol("assertEquals")` en un repo con tests → `external: true`.

## Riesgos

Distancia de edición sobre todos los símbolos puede ser cara en repos grandes: limitar a candidatos
por prefijo/sufijo en SQL y rankear en memoria.

## Resultado

<!-- Contrato: PLAN-008 §11 -->
