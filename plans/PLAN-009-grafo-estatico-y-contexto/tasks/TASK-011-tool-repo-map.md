# TASK-011 — Tool `repo_map` con PageRank personalizado bajo presupuesto

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama local `feat/plan-009-grafo`, remota `fix/plan-009-grafo`
- **Depende de:** TASK-010
- **Estado:** `pending`

---

## Objetivo

Una tool nueva `repo_map` que, con ~1k tokens, le da al agente un mapa del repo: las firmas de los
símbolos más relevantes para su foco (archivos, símbolos o una consulta), agrupadas por archivo y
ordenadas por PageRank personalizado, sin cuerpos. Es la orientación inicial barata que hoy no
existe (Aider: archivo correcto en el 70.3 % de SWE-bench Lite atribuido al repo-map). Implementa
DD-12 (parte personalizada) y DD-14.

## Contexto verificado

- Patrón para agregar una tool (verificado en las existentes): función `do_*` en
  `crates/devctx-mcp/src/state.rs` → método en `Backend` con brazo local y remoto
  (`crates/devctx-mcp/src/backend.rs`, p. ej. `read_symbol` l.640, `build_context` l.648) → ruta en
  `crates/devctx-api/src/lib.rs` (`router`, l.43-77) → `#[tool(...)]` en
  `crates/devctx-mcp/src/lib.rs` (p. ej. `build_context`, l.1247-1251) → subcomando opcional en
  `crates/devctx-cli/src/main.rs` (enum `Command`, l.52+).
- `identifier_tokens` para sacar identificadores de una consulta (`crates/devctx-search/src/lib.rs:600-641`).
- Estimación de tokens del producto: chars/4 (`CHARS_PER_TOKEN`, `state.rs:946`;
  `devctx_chunk::estimate_tokens`, `crates/devctx-chunk/src/chunk.rs:77-79`).
- Contrato de salida textual con cierre `[devctx] …` como `compose_context`
  (`state.rs:4295-4301`, `close_context`).
- Caché por proceso: `AppState` ya cachea modelos con liberación por inactividad
  (`release_idle_models`, `state.rs:350`).
- Resolución de proyecto y rama: `graph_branch` (`state.rs:884`) con `branch_fallback`; parámetro
  `project` como en las tools de PLAN-008 TASK-010.

## Archivos

- **Crear:** `crates/devctx-mcp/src/repo_map.rs` (selección y render; puro y testeable),
  `crates/devctx-index/src/pagerank.rs` (ya existe de TASK-010: agregar personalización).
- **Modificar:** `crates/devctx-mcp/src/state.rs` (`do_repo_map`, caché de adyacencia por
  `(repo, branch, generación del índice)` invalidada al terminar un `index`),
  `crates/devctx-mcp/src/backend.rs`, `crates/devctx-mcp/src/lib.rs`, `crates/devctx-api/src/lib.rs`
  (`GET /repo-map`), `crates/devctx-cli/src/main.rs` (`devctx map [--budget N] [--focus …]`),
  `crates/devctx-cli/src/help_map.rs` si lista comandos.

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) render nunca supera `budget × 1.15` tokens (propiedad sobre
      grafos aleatorios chicos); (b) con `focus_files = [X]`, los símbolos que X usa aparecen antes
      que los globalmente centrales; (c) sin foco = orden de `rank` global; (d) tests y externos
      nunca aparecen; (e) jerarquía por `parent_id` en el render; (f) rama v1 → error que pide
      `devctx index --full`.
- [ ] **Paso 2 — personalización** (DD-14): `focus_symbols` e identificadores de `query` × 10,
      `focus_files` × 50 como referenciadores, prefijo `_` × 0.1, nombres definidos en > 5 archivos
      × 0.1; misma matriz ponderada de DD-12.
- [ ] **Paso 3 — selección** por búsqueda binaria de cuántas firmas entran (tolerancia 15 %);
      archivos por rank agregado, dentro por línea.
- [ ] **Paso 4 — caché de adyacencia** en `AppState`; se suelta con la política de inactividad y se
      invalida con cada `index` terminado.
- [ ] **Paso 5 — exposición** MCP/API/CLI; descripción del tool clara ("para orientarse; para leer
      código usá `read_symbol`/`read_file`").
- [ ] **Paso 6 — medir** con el arnés: casos con foco → archivo esperado presente en el mapa; tokens;
      latencia p95 en backend-a (fría y caliente); memoria de la caché.

## Criterios de aceptación

- [ ] Tests (a)-(f) verdes.
- [ ] Arnés: archivo esperado presente en ≥ 70 % de los casos con foco; tokens ≤ 1024 × 1.15 con el
      default.
- [ ] p95 ≤ 500 ms caliente en backend-a; primera carga < 1 s; memoria de la caché anotada.

## Riesgos

- El mapa sin foco en un repo grande puede no decir nada útil: se documenta que rinde con foco
  (archivos abiertos, símbolos de la tarea) y el agente lo pide así.
- Firmas largas (Java con anotaciones): tope de 200 caracteres (DD-17) y anotaciones fuera.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
