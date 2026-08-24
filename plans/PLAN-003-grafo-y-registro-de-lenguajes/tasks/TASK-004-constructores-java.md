# TASK-004 — `constructor_declaration` y los kinds de Java completos

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/grafo-y-registro-de-lenguajes`
- **Depende de:** TASK-003
- **Estado:** `pending`

---

## Objetivo

Que una llamada hecha dentro de un constructor Java produzca una arista, en vez
de descartarse entera.

## Contexto verificado (2026-08-23)

`lang.rs:177` — `FUNCTION_KINDS` contiene `function_definition`,
`function_declaration`, `method_declaration`, `method_definition` y
`function_item`. **No contiene `constructor_declaration`.**

`parser.rs:283` — `enclosing_function_node` sube hasta encontrar un
`FUNCTION_KINDS`. Dentro de un constructor no encuentra ninguno, devuelve `None`,
`qualified_source` (`parser.rs:296`) devuelve `None`, y `extract_edges`
(`parser.rs:125`) hace `continue`: **la arista se pierde**.

En Quarkus la inyección por constructor no es un caso de borde.

> **Esto es evidencia de lectura de código, no una medición.** El Paso 1 lo
> convierte en medición antes de arreglarlo. Si el test pasa de entrada, la
> hipótesis era falsa y esta task se marca `skipped` con el motivo.

No se agregó antes porque `FUNCTION_KINDS` es global: meterle
`constructor_declaration` se lo mete también a Python y a Rust. TASK-003 quita
ese impedimento.

## Archivos

- **Modificar:** `crates/devctx-parse/languages/java.json`
- **Modificar:** `crates/devctx-parse/src/lib.rs` (tests)

## Pasos

- [ ] **Paso 1 — Escribir el test que falla.** Fuente Java con una clase cuyo
      constructor llama a un método; esperar una arista con source
      `Clase.Clase`. Correrlo y **confirmar que falla** antes de seguir.
- [ ] **Paso 2 — Agregar `constructor_declaration`** a `function_kinds` en
      `java.json`.
- [ ] **Paso 3 — Revisar el resto de kinds de Java.** `container_kinds` tiene
      `class_declaration`, `interface_declaration`, `enum_declaration`.
      Evaluar `record_declaration` y `annotation_type_declaration` contra
      tree-sitter-java 0.23 — **agregar solo lo que un test demuestre que hace
      falta**, no por completitud.
- [ ] **Paso 4 — Verificar el nombre del source.** Un constructor tiene el mismo
      nombre que su clase, así que `qualified_source` produce `Clase.Clase`.
      Decidir si se deja así o se normaliza a `Clase.<init>`, y **dejar la
      decisión escrita en `## Resultado`** — afecta cómo se busca después.
- [ ] **Paso 5 — Commit.** `fix(parse): count calls made inside a Java constructor`

## Criterios de aceptación

- [ ] El test del Paso 1 falla antes del Paso 2 y pasa después.
- [ ] Tras reindexar REVFA_BackEnd, un constructor con inyección que llama a algo
      aparece como `source` en `graph_edges`. Nombrar el archivo concreto en
      `## Resultado`.
- [ ] Python, Go y Rust no cambian su conteo de aristas.

## Riesgos

Aristas nuevas donde antes había cero — es la intención, pero cambia los
conteos de TASK-006. Medir Java **antes y después** por separado.

## Resultado
<!-- SE LLENA AL CERRAR -->
