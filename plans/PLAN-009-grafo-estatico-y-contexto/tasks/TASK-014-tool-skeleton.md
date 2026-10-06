# TASK-014 — Tool `skeleton` por archivo

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-004
- **Estado:** `pending`

---

## Objetivo

Una tool nueva `skeleton(file)` que devuelve la estructura de un archivo —símbolos con firma, kind y
rango, anidados por contenedor, sin cuerpos— leída de la tabla `symbols`. Sirve para **localizar**
barato (Agentless, `get_symbols_overview` de Serena); la evidencia dice que no reemplaza al código
como contexto de edición, y la descripción del tool lo dice. Implementa DD-17.

## Contexto verificado

- Tras TASK-003/004, `symbols` tiene por archivo: `kind`, `name`, `qualified`, `parent_id`,
  `signature` (normalizada, tope 200 caracteres), rangos; el símbolo de archivo es la raíz.
- Hoy lo más parecido es el chunk `file` (`crates/devctx-chunk/src/chunker.rs:206-222`: imports
  crudos + lista plana `- nombre (kind)`), que no tiene firmas ni jerarquía.
- `read_file` (`do_read_file`, `crates/devctx-mcp/src/state.rs:922`) resuelve rutas relativas al
  repo; reusar esa resolución para el parámetro `file`.
- Patrón de alta de tool: ver TASK-011.

## Archivos

- **Modificar:** `crates/devctx-store/src/symbols.rs` (`file_symbols(repo, branch, file)` ordenado),
  `crates/devctx-mcp/src/state.rs` (`do_skeleton`, render), `crates/devctx-mcp/src/backend.rs`,
  `crates/devctx-mcp/src/lib.rs` (`#[tool]`), `crates/devctx-api/src/lib.rs` (`GET /skeleton`),
  `crates/devctx-cli/src/main.rs` (`devctx skeleton <archivo>`).

## Pasos

- [ ] **Paso 1 — tests que fallan.** (a) archivo Java con clase, clase interna, constructor y métodos
      → árbol con indentación por `parent_id`, firmas y `[l1-l2]`; (b) TS con arrow-const exportado;
      (c) archivo no indexado → error con sugerencias de rutas cercanas (`file_index`); (d) rama v1
      → error que pide `devctx index --full`; (e) nunca aparece una línea de cuerpo.
- [ ] **Paso 2 — render** textual (va al contexto del modelo): una línea de imports resumidos
      (cantidad + los primeros), luego el árbol; tope por `DEVCTX_MAX_OUTPUT_TOKENS` con
      `[devctx] N símbolos más`.
- [ ] **Paso 3 — exposición** MCP/API/CLI; descripción: "para ubicar dónde está algo; para editar,
      `read_symbol`/`read_file`".
- [ ] **Paso 4 — medir** tokens de `skeleton` vs `read_file` en 5 archivos grandes de los repos del
      arnés.

## Criterios de aceptación

- [ ] Tests (a)-(e) verdes.
- [ ] Tabla de tokens skeleton vs archivo completo en el Resultado (esperado: ≤ 15 % en archivos de
      > 300 líneas).

## Riesgos

- Firmas multilínea mal cortadas (anotaciones Java, decoradores TS): normalización de DD-17 y
  fixtures específicos.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
