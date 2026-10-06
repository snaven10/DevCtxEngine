# TASK-008 — Retirar fastembed del árbol y cross-check Windows/macOS

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-005, TASK-006, TASK-007
- **Estado:** `pending`

---

## Objetivo

Sacar `fastembed` de las dependencias y de los comentarios, dejando la feature `local` como
`ort` + `tokenizers` + `hf-hub`, y comprobar que el workspace compila en Linux, Windows y macOS (con y
sin `gpu`) antes de que nada se taggee.

## Contexto verificado

- Usos de fastembed en `main` (`git grep -n fastembed main`): `crates/devctx-embed/Cargo.toml:8,12,14,26-27`,
  `crates/devctx-embed/src/{lib.rs:3, local.rs:1-15,34,150,169,267, registry.rs:4-5,16,69}`,
  `crates/devctx-rerank/Cargo.toml:8,12-13,20-21`, `crates/devctx-rerank/src/{lib.rs:4, local.rs:1-19,68,134,140,148,184}`,
  `crates/devctx-cli/src/main.rs:4`, `crates/devctx-cli/src/models.rs:10`,
  `crates/devctx-core/src/config.rs:257`, `crates/devctx-core/src/modelload.rs:3`, y los
  comentarios "no fastembed/ort" de `devctx-index`, `devctx-memory`, `devctx-search`,
  `devctx-summarize` `Cargo.toml`.
- `.fastembed_cache` se trata como artefacto legacy en `crates/devctx-index/src/pipeline.rs:563-565`
  y `crates/devctx-index/src/lib.rs:864`: **se mantiene** (repos viejos pueden tenerlo).
- Lección de v0.8.3: tag sin release por un `cfg(unix)` faltante en Windows; desde 0.8.4 hay check
  cruzado en CI (`521aaa9`) que corre al empujar ramas `fix/**`.

## Archivos

- **Modificar:** los `Cargo.toml` de `devctx-embed` y `devctx-rerank` (features `local`/`gpu`),
  `Cargo.lock`, los comentarios listados arriba, el camino alternativo fastembed que TASK-004 dejó
  detrás de un cfg/env de diagnóstico (se borra).

## Pasos

- [ ] **Paso 1 — dependencias.** `local = ["dep:ort", "dep:tokenizers", "dep:hf-hub", "dep:ndarray", …]`;
      `gpu = ["local", "ort/cuda"]`. `ort` deja de ser "sólo para nombrar CUDAExecutionProvider".
- [ ] **Paso 2 — borrar el camino viejo** y los imports de fastembed; actualizar docs de módulo.
- [ ] **Paso 3 — `cargo tree -i fastembed`** vacío; revisar que `rayon` no quedó arrastrado sin uso.
- [ ] **Paso 4 — build sin `local`.** `cargo check -p devctx-embed --no-default-features` y lo mismo
      en `devctx-rerank` (API-only sigue compilando).
- [ ] **Paso 5 — cross-check.** Empujar una rama `fix/plan-010-crosscheck` (con aprobación del
      usuario) para disparar el job de Windows/macOS; esperar verde con y sin `gpu` donde aplique.
- [ ] **Paso 6 — tamaño del binario.** Anotar antes/después (`ls -l target/release/devctx`).

## Criterios de aceptación

- [ ] `git grep -n fastembed` sólo devuelve las menciones legacy de `.fastembed_cache` y notas
      históricas en `plans/`/CHANGELOG.
- [ ] Gate completo verde: fmt, clippy con y sin `--features gpu`, `cargo test --workspace`.
- [ ] Cross-check de CI Windows y macOS verde (link al run en el Resultado).
- [ ] Arnés de equivalencia (TASK-003) verde sin fastembed en el árbol (los dorados ya no se pueden
      regenerar: queda documentado en el manifiesto).

## Riesgos

- `ort/download-binaries` en Windows/macOS: hoy lo arrastra fastembed con sus features; al pasar a
  directa hay que replicar exactamente esas features o el build de CI falla al linkear.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
