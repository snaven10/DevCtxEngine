# TASK-007 — Resolvers Python, Rust y Go

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-005
- **Estado:** `pending`

---

## Objetivo

Que Python (workers de ACME), Rust (este repo) y Go (fixtures) tengan resolución por imports/módulos,
receptores tipados y externos marcados, con las llamadas a nivel de módulo de Python como aristas.
Implementa DD-7 (Python, Rust, Go).

## Contexto verificado

- Python: `types` solo `assignment` y `typed_parameter` con anotación (`languages/python.json`); sin
  `self.x = Foo()`; llamadas de módulo descartadas (`crates/devctx-parse/src/parser.rs:125-127`,
  TASK-003 ya les da fuente). Builtins como targets pelados (`len`, `print`). Repo de campo:
  legacy-migration (`src/pipeline.py`, `src/resolver.py`, scripts con `if __name__ == "__main__"`).
- Rust: `calls` con `scoped_identifier name:` → `Foo::bar` queda `bar` (`languages/rust.json`);
  `container_kinds: ["impl_item", "trait_item"]`; `impl Trait for T` no se registra; `types` solo
  `type_identifier` en `let`/params/campos. 49 `impl X for Y` en `crates/*/src/*.rs` de este repo.
- Go: `container_kinds: []` (`languages/go.json`), así que `(s *Server) Handle` no se califica;
  `selector_expression` con `operand` en `receiver_of` (`parser.rs:349`). No hay repo Go del usuario
  (Q-6 del master).

## Archivos

- **Crear:** `crates/devctx-parse/src/resolve/{python,rust,go}.rs`, fixtures multi-archivo en
  `crates/devctx-parse/tests/fixtures/{python,rust,go}/`.
- **Modificar:** `crates/devctx-parse/languages/{python,rust,go}.json`, `crates/devctx-index/src/link.rs`
  (módulos Python por ruta/paquete, `crate::`/`super::` de Rust leyendo los `Cargo.toml` del
  workspace para saber qué crates son del repo, `module` de `go.mod`).

## Pasos

- [ ] **Paso 1 — tests que fallan.** Python: dos llamadas a `helper()` desde el mismo método → dos
      aristas; llamada a nivel de módulo; `from .resolver import resolver_id` (relativo);
      `self.repo = Repo()` en `__init__` y `self.repo.save()`; `len(x)` → `external_known`. Rust:
      `Store::open(…)` → `Store.open` resuelto por `use`; `Self::helper()`; método de `impl Trait for
      T` → `T.m` + `implements`; `serde_json::from_str` → externo. Go: método con receptor; import
      del propio módulo vs `fmt`.
- [ ] **Paso 2 — Python.** Paquetes por carpeta (`__init__.py`) y raíz del repo; `import a.b as c`,
      `from x import y`, `from x import *` (exports si está en repo); atributos de instancia
      tipados por asignación en `__init__`; builtins/stdlib en `python.json`.
- [ ] **Paso 3 — Rust.** `use` (incluye grupos `{A, B}` y `self`), `mod`, `crate::`/`super::`;
      path como receptor en `Foo::bar`; `Self`; `impl Trait for T` → `implements`; crates del
      workspace = repo, el resto = externo (`std`, `core`, `alloc` y dependencias).
- [ ] **Paso 4 — Go (básico).** Receptor del método como contenedor; imports con prefijo del
      `module` de `go.mod` → repo; resto externo. Nada más (Q-6).
- [ ] **Paso 5 — medir.** DevCtxEngine con el arnés (gold edges Rust) y legacy-migration
      (86 archivos, barato) para Python.

## Criterios de aceptación

- [ ] Tests del paso 1 verdes.
- [ ] Gold edges Python y Rust: precisión ≥ 90 % en `high`, ≥ 75 % global.
- [ ] DevCtxEngine: `serde_json::from_str`, `tokio::spawn` quedan `external` (base para el
      criterio B2 que cierra TASK-008).

## Riesgos

- Python dinámico: lo no tipable cae a `medium`/`low`, nunca se inventa `high`.
- Macros de Rust (`format!`, `tokio::main`) no son llamadas: excluirlas de `references.call`.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
