# TASK-003 — `lang.rs` pasa a ser un registro JSON embebido, con kinds por lenguaje

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/grafo-y-registro-de-lenguajes`
- **Depende de:** TASK-002
- **Estado:** `done`

---

## Objetivo

Que la definición completa de un lenguaje viva en **un archivo JSON que se lee de
corrido**, embebido en el binario al compilar, en vez de repartida en 9 lugares
de 6 funciones. Y que `function_kinds`/`container_kinds` dejen de ser listas
globales compartidas entre los 7 lenguajes.

## Contexto verificado (2026-08-23)

- `crates/devctx-parse/src/lang.rs` — agregar una variante hoy exige tocar:
  el `enum Lang`, `name()`, `grammar()`, `symbol_query()`, `calls_query()`,
  `type_bindings_query()`, `import_query()`, `const ALL`, `lang_for_extension()`,
  más `Cargo.toml`. Nueve ediciones, seis funciones.
- `lang.rs:177` `FUNCTION_KINDS` y `lang.rs:186` `CONTAINER_KINDS` son `const`
  globales que consumen los 7 lenguajes. `parser.rs` las importa directo
  (`use crate::lang::{Lang, CONTAINER_KINDS, FUNCTION_KINDS}`).
- **`Lang::` no se usa fuera de `devctx-parse`** — verificado con grep en
  `crates/`: solo `lang.rs` y los tests de `lib.rs`. El acoplamiento ya está
  contenido, así que este refactor no se derrama a otros crates.
- `crates/devctx-parse/Cargo.toml` linkea 6 crates de gramática
  (`tree-sitter-{python,javascript,typescript,go,java,rust}` 0.23) contra
  `tree-sitter = "0.25"`.
- `tree_sitter::Query::new` **rechaza nombres de nodo que no existen** en la
  gramática: la validación de las queries del JSON es real.

## Archivos

- **Crear:** `crates/devctx-parse/languages/{python,javascript,typescript,tsx,go,java,rust}.json`
- **Crear:** `crates/devctx-parse/src/registry.rs` — la struct `LangDef`, la carga y la validación
- **Modificar:** `crates/devctx-parse/src/lang.rs` — queda la tabla de gramáticas y el lookup por extensión
- **Modificar:** `crates/devctx-parse/src/parser.rs` — `LanguageParser` toma los kinds de su `LangDef`
- **Modificar:** `crates/devctx-parse/Cargo.toml` — `serde` / `serde_json`

## Pasos

- [x] **Paso 1 — Definir `LangDef`** en `registry.rs`, con `Deserialize`:
      `name`, `grammar`, `extensions`, `symbols`, `calls`, `types` (opcional),
      `imports`, `function_kinds`, `container_kinds`.
- [x] **Paso 2 — Escribir los 7 JSON** trasladando **literalmente** las queries y
      extensiones que hoy están en `lang.rs`. Los `function_kinds`/`container_kinds`
      de cada uno arrancan siendo el subconjunto de las listas globales que
      aplica a ese lenguaje. **Traslado 1:1: este paso no cambia comportamiento.**
- [x] **Paso 3 — Embeber y parsear.** `include_str!` por archivo, deserializados
      una vez en un `LazyLock<Vec<LangDef>>`. Un JSON inválido revienta ahí, y el
      test del Paso 6 lo atrapa en CI.
- [x] **Paso 4 — Tabla de gramáticas.** `fn grammar_for(name: &str) -> Option<Language>`
      con las 7 entradas. **Es lo único compilado que queda**, porque las
      gramáticas son símbolos C linkeados (ver DD-4).
- [x] **Paso 5 — Kinds por lenguaje.** `LanguageParser` guarda los kinds de su
      `LangDef`; `enclosing_function_node`, `enclosing_container` y
      `extract_symbols` los reciben en vez de leer las `const` globales.
      **Borrar `FUNCTION_KINDS` y `CONTAINER_KINDS`.**
- [x] **Paso 6 — Test de validación.** Recorre los 7 `LangDef`, resuelve su
      gramática y compila sus 4 queries con `Query::new`. Falla nombrando el
      lenguaje y la query. Un JSON sin gramática registrada también falla.
- [x] **Paso 7 — Documentar cómo se agrega un lenguaje** en
      `docs/03-core-concepts/symbol-graph.md` y su par en `docs/es/`: 1 JSON +
      1 dep + 1 línea en la tabla, y por qué la gramática no puede venir del JSON.
- [x] **Paso 8 — Commit.** `refactor(parse): define languages in embedded JSON`

## Criterios de aceptación

- [x] Toda la suite de `devctx-parse` pasa **sin tocar un solo test**: el traslado
      es 1:1 y los tests existentes son la prueba de que no cambió nada.
- [x] `FUNCTION_KINDS` y `CONTAINER_KINDS` ya no existen en el árbol.
- [x] El test del Paso 6 falla —y nombra el lenguaje— si se corrompe a propósito
      un nodo en `java.json`. **Comprobarlo de verdad, no asumirlo.**
- [x] Reindexar un repo pequeño produce el **mismo** conteo de símbolos y aristas
      que antes del cambio. Anotar ambos números en `## Resultado`.
- [x] `Lang::` sigue sin usarse fuera de `devctx-parse`.

## Riesgos

**La validación se mueve de compile-time a load-time.** Hoy una query mala no
compila; después falla al cargar. El test del Paso 6 lo cubre en CI, pero solo
si ese test existe y corre — por eso es criterio de aceptación comprobado, no
declarado.

**Lo que ninguna validación atrapa:** una query válida que no matchea nada. Es
el modo de falla que originó este PLAN. Contra eso solo sirve TASK-006.

**Traslado con dedos.** El riesgo real del Paso 2 es un carácter perdido al
copiar una query. Los tests existentes de `lib.rs` cubren los 7 lenguajes y son
la red — no los modifiques para que pasen.

## Resultado

- **Estado final:** `done` (2026-08-24)
- **Verificado por:** ver [`../VERIFICACION.md`](../VERIFICACION.md) — medición completa sobre REVFA_BackEnd, con lo que NO se verificó declarado.
