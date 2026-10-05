# TASK-006 — Pesos compartidos entre procesos: spike ORT/mmap e implementación

- **Plan:** PLAN-010 — Memoria de procesos
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-001, TASK-004
- **Estado:** `pending`

---

## Objetivo

Que N procesos con el mismo modelo cargado compartan las páginas de pesos por page cache en vez de
tener N copias privadas, **si y sólo si** el spike demuestra ganancia real con latencia aceptable.
PLAN-010 DD-5.

## Contexto verificado

- Hoy: `std::fs::read` del ONNX (`crates/devctx-embed/src/local.rs:223`) →
  `commit_from_memory` (fastembed `text_embedding/impl.rs:105-110`): copia a memoria anónima.
  Mismo patrón en el reranker (`crates/devctx-rerank/src/local.rs:213`).
- ort rc.9 (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ort-2.0.0-rc.9/src/`):
  `commit_from_memory_directly` (`session/builder/impl_commit.rs:137-145`, setea
  `session.use_ort_model_bytes_directly` y `session.use_ort_model_bytes_for_initializers`, devuelve
  `InMemorySession<'_>` atado al buffer), `commit_from_file` (`impl_commit.rs:65`),
  `with_optimized_model_path` (`impl_options.rs:92`), `with_prepacking` (`impl_config_keys.rs:10`,
  `session.disable_prepacking`), `with_external_initializer_file` (`impl_options.rs:163`).
- ONNX Runtime nativo 1.20.0 (`ort-sys-2.0.0-rc.9/build.rs:7`).
- Línea base de `RssAnon`/`RssFile`/`Pss` por serve: TASK-001 (escenario E4).

## Archivos

- **Crear:** `crates/devctx-embed/src/onnx/mapped.rs` (dueño del `Mmap` + sesión), caché `.ort`
  bajo `model_cache_dir()`.
- **Modificar:** `crates/devctx-embed/src/onnx/mod.rs` (camino de carga), `Cargo.toml`
  (`memmap2`, y `self_cell` u `ouroboros` si se elige esa vía).

## Pasos

- [ ] **Paso 1 — spike (código descartable en el scratchpad o rama aparte).** Binario mínimo que
      carga granite de cuatro formas: (A) `commit_from_file(.onnx)`; (B1) `.ort` + mmap +
      `commit_from_memory_directly` con prepacking; (B2) igual sin prepacking; (C) datos externos.
      Para cada una: 3 procesos simultáneos, `smaps_rollup` (`Pss`, `Shared_Clean`, `Private_Dirty`)
      y latencia de 100 embeds.
- [ ] **Paso 2 — generación del `.ort`.** Primera carga desde `.onnx` con
      `with_optimized_model_path(<cache>/<key>-<sha256[..12]>-ort1.20-L{n}.ort)` y
      `session.save_model_format = ORT`; probar `Level3` vs `Level2`. Equivalencia contra el arnés
      con el `.ort` cargado (los vectores no deben cambiar).
- [ ] **Paso 3 — decidir con números.** Ganancia mínima para seguir: `RssAnon` por proceso baja
      ≥ el tamaño del modelo (~98 MB granite) con latencia ≤ +10 %. Si no se cumple: cerrar
      `skipped` con la tabla del spike como Motivo.
- [ ] **Paso 4 — implementación.** Struct dueño de `Arc<Mmap>` + sesión (lifetime borrado de forma
      segura y documentada; drop: sesión antes que mmap). Regeneración del `.ort` si cambia el hash
      del `.onnx`, la versión de ORT o el nivel. Escritura atómica (tmp + rename) para que dos
      procesos que arrancan a la vez no lean un `.ort` a medio escribir.
- [ ] **Paso 5 — fallback.** Cualquier error en el camino `.ort` → carga clásica desde `.onnx` con
      aviso (nunca deja al proceso sin modelo). Con `device: cuda`, camino clásico.
- [ ] **Paso 6 — reranker.** Mismo mecanismo si el spike mostró ganancia para su tamaño.

## Criterios de aceptación

- [ ] Tabla del spike (A/B1/B2/C × RSS anon/file, Pss, latencia) en el Resultado.
- [ ] Si se implementa: con 3 serves con modelo cargado, `Pss` de los pesos se reparte (≈ 1/3 cada
      uno) y `RssAnon` por serve baja ≥ ~98 MB vs línea base E4.
- [ ] Arnés de equivalencia verde cargando desde `.ort`.
- [ ] Test: `.ort` corrupto o de otra versión → se regenera / cae al `.onnx` con aviso, sin panic.
- [ ] Test: dos cargas concurrentes en procesos distintos no se pisan el `.ort`.

## Riesgos

- Prepacking MLAS re-copia los MatMul int8: puede comerse la ganancia (por eso B1 vs B2).
- `.ort` con `Level3` depende del hardware: por máquina, nunca distribuido; si se copia la caché a
  otra máquina, la clave no garantiza compatibilidad → incluir en la clave algo del CPU (flags AVX)
  o usar `Level2` si la diferencia de latencia es despreciable.
- `unsafe` del lifetime borrado: acotarlo a un módulo con tests y comentario `SAFETY`.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
