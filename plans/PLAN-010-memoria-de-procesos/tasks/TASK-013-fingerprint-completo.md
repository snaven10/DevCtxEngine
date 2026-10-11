# TASK-013 — Fingerprint de embeddings completo: pooling, `max_length` y hash del ONNX, también para memorias

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** — (después de PLAN-009, como todo el plan; no depende del motor nuevo)
- **Estado:** `pending`

---

## Objetivo

Que la huella de embeddings describa **todo** lo que decide el número que sale para un texto, para
que ningún cambio de TASK-004, TASK-007 o TASK-017 pueda mezclar vectores de dos espacios en
silencio: ni en el índice de código, ni en las memorias (locales y del central), ni al importar un
JSONL. Es la red de seguridad del bloque de calidad. PLAN-010 DD-11. Actualizar a un binario con
esta task **no** re-embebe ningún índice ni rompe `remember`/`recall` (§6, "reindex requerido tras
actualizar: ninguno").

## Contexto verificado (2026-10-11, rama `feat/plan-009-grafo`; re-verificar líneas al arrancar)

- `embedding_fingerprint` (`crates/devctx-embed/src/lib.rs:106`) arma, para `local`,
  `local|{model}|{dimension}|{LOCAL_ENGINE}|max_chars={n}|l2`. **No incluye** pooling, `max_length`
  ni qué archivo ONNX se cargó. `LOCAL_ENGINE` sale de `Cargo.lock` en `build.rs`
  (`fastembed-<v>/ort-<v>`); su comentario ya dice que el motor de PLAN-010 debe cambiar ese string
  hasta demostrar vectores iguales.
- Se guarda por rama en `index_meta` con la clave `EMBED_FP_META_KEY = "embedding_fingerprint"`
  (`crates/devctx-store/src/state.rs:30`). `index_run` lo compara
  (`crates/devctx-index/src/pipeline.rs:329-348`): si difiere, marca la rama `transition`, apaga el
  reuso y **fuerza un `--full`** (`model_changed` anula `can_incremental`, `pipeline.rs:372-374`).
  Un índice sin huella (anterior a fingerprints) se acepta si modelo y ancho coinciden y se sella.
- Lo calculan `crates/devctx-mcp/src/state.rs:1276` (index en el serve) y
  `crates/devctx-cli/src/main.rs:3969` (camino directo).
- Pooling: los user-defined (`ml-granite`, `ml-granite-lg`) se cargan con `Pooling::Mean` fijo
  (`crates/devctx-embed/src/local.rs:252`); los builtins usan el default de fastembed por modelo.
- `max_length`: solo los builtins con `max_input_tokens` reciben `with_max_length`
  (`local.rs:201`); los user-defined quedan en `DEFAULT_MAX_LENGTH = 512` de fastembed 4.9.1
  recortado por `model_max_length` (Granite: 32 768 en `tokenizer_config.json`).
- **Memorias: no tienen huella.** El central solo rechaza un cambio de **dimensión**
  (`CentralError::DimensionMismatch`, `crates/devctx-central/src/error.rs:22`; test
  `changing_the_memory_model_is_refused_not_corrupted`, `crates/devctx-central/src/lib.rs:717`).
  `shares_vector_space` (`lib.rs:184`) compara solo `provider` y `model`. La tabla `memories`
  (`crates/devctx-store/src/schema.rs:211`) no guarda con qué se embebió cada una. Un cambio de
  pooling con el mismo modelo y 384 dims pasaría todas esas guardas.
- `memories import` reusa el embedding del JSONL si `e.model == model && e.dim == dim`
  (`crates/devctx-cli/src/main.rs`, `cmd_memories_import`, ~`:3178`): con otro pooling reusaría
  vectores Mean en un store CLS sin aviso.

## Archivos

- **Modificar:** `crates/devctx-embed/src/lib.rs` (`embedding_fingerprint` v2 + parser de la v1,
  `EmbedSettings` con `pooling`/`max_length` efectivos), `crates/devctx-embed/src/registry.rs`
  (`LocalModelSpec` gana `pooling` explícito por modelo), `crates/devctx-embed/src/local.rs` (expone
  pooling, `max_length` efectivo y sha256 del ONNX cargado), `crates/devctx-index/src/pipeline.rs`
  (equivalencia v1↔v2 al comparar), `crates/devctx-store/src/state.rs` (meta de memorias),
  `crates/devctx-central/src/{lib.rs,error.rs}` (huella del espacio de memorias),
  `crates/devctx-cli/src/main.rs` (`memories export/import` con huella), `crates/devctx-mcp/src/state.rs`
  (`status` informa la huella y el desfase).

## Pasos

- [ ] **Paso 1 — huella v2.** Formato con versión y campos con nombre, p. ej.
      `v2|local|ml-granite|384|eng=fe-compat/1|pool=mean|maxlen=512|onnx=<sha256[:16]>|max_chars=4096|l2`.
      **`eng=` es una clase numérica y se escribe `fe-compat/1` desde el primer día**, nunca el string
      crudo de `LOCAL_ENGINE`. Mapa explícito y versionado en el código (tabla `ENGINE_CLASSES`):
      `fastembed-4.9.1/ort-2.0.0-rc.9` → `fe-compat/1`. Un `LOCAL_ENGINE` que no está en el mapa (p. ej.
      tras un `cargo update` de fastembed u ort) da una clase nueva `unmapped:<LOCAL_ENGINE>` y no
      reusa nada, igual que hoy. Así TASK-004 no tiene que volver a re-sellar: si su motor pasa el
      arnés, se agrega al mapa con la misma clase y la huella no cambia.
      Para builtins, `onnx=` es el `model_code/model_file` + revisión de la caché (o el sha si es
      barato); para HTTP queda como hoy. El sha256 del ONNX se calcula una vez por carga y se cachea
      junto al archivo (`<onnx>.sha256` con mtime+tamaño) para no leer 98 MB en cada `index`.
- [ ] **Paso 2 — pooling y `max_length` explícitos.** `LocalModelSpec.pooling` (`Mean`/`Cls`) con el
      valor **que se usa hoy** (Granite = `Mean`, aunque su modelo es CLS: el cambio es TASK-017). El
      `max_length` efectivo = `min(config/def 512, model_max_length, max_input_tokens)`, el mismo
      cálculo que hace fastembed. Nada cambia de valor en esta task: solo se nombra.
- [ ] **Paso 3 — migración de índices existentes (sin re-embeber).** Una huella v1 guardada se
      considera **igual** a la v2 activa si y solo si: mismo provider/modelo/ancho/`max_chars`, el motor
      de la v1 traducido por el mapa del Paso 1 da la misma clase que el `eng=` activo,
      y la v2 activa tiene `pool` y `maxlen` iguales a los valores implícitos de la v1 (los del Paso 2)
      — o sea, si el setup no cambió. El hash del ONNX no existe en la v1: se acepta (mismo criterio
      que el índice "antes de fingerprints": hubo un solo archivo por clave) y la corrida **re-sella**
      con la v2. Avisar una sola vez en `status`/log: "huella v1 actualizada a v2 sin re-embeber".
      Si cualquier campo difiere, comportamiento de hoy: `transition` + `--full`.
- [ ] **Paso 4 — ONNX distinto bajo el mismo nombre.** Si el sha del ONNX de una v2 guardada no
      coincide con el cargado (otro archivo en `model_dir`, otra cuantización), es un setup distinto:
      no hay reuso. `status` lo dice con los dos hashes. Test con dos ONNX falsos.
- [ ] **Paso 5 — memorias, store por store.** Guardar la huella del espacio de memorias:
      en el central (`index_meta` o tabla de config del central, clave `memory_embedding_fingerprint`)
      y en cada store de repo para las memorias `local`. Al abrir: si falta, sellar con la activa
      (memorias anteriores = un solo setup posible, el de hoy); si difiere, **no** mezclar: `remember`
      y `recall` fallan con un error que dice qué cambió y el comando de migración (lo implementa
      TASK-017; hasta entonces el mensaje dice "volvé a la config anterior"). `shares_vector_space`
      compara huellas, no nombres.
- [ ] **Paso 6 — export/import.** El JSONL de `memories export` agrega `embedding.fingerprint`. El
      import reusa el vector solo si la huella es **igual**; un archivo viejo (sin huella) se reusa solo
      si modelo y ancho coinciden **y** la huella activa es la equivalente a la v1 (Paso 3); si no,
      re-embebe y lo dice en el resumen.
- [ ] **Paso 7 — motor nuevo.** Dejar documentado (comentario + PLAN-010 DD-11) que TASK-004 solo
      **agrega una entrada** al mapa del Paso 1 (su motor → `fe-compat/1`) cuando el arnés de
      TASK-003 pasa con la tolerancia de DD-3; la huella guardada no cambia y no hay re-sellado. Si
      el arnés no pasa, TASK-004 queda `blocked` (ver su Paso 7) y no se toca el mapa. Esta task no
      cambia el motor.

## Criterios de aceptación

- [ ] Tests unitarios: la huella cambia con `pool`, `maxlen`, `onnx`; no cambia con `device`;
      `fastembed-4.9.1/ort-2.0.0-rc.9` produce `eng=fe-compat/1`, y un motor fuera del mapa, otra clase.
- [ ] Test de migración: un store con huella v1 de hoy + config sin cambios → la corrida siguiente
      es **incremental**, reusa vectores y re-sella v2 (cero embeddings nuevos para archivos sin cambio).
- [ ] Test: misma config pero `pool` distinto → `transition` + `--full`, y nada se reusa.
- [ ] Test del central: memorias con huella A, config con huella B (mismo modelo y ancho) →
      `remember`/`recall` fallan con mensaje accionable; con dimensión distinta sigue el error de hoy.
- [ ] Test de import: JSONL con huella distinta → `recomputed`, nunca `reused`.
- [ ] Sobre los índices reales de DevCtxEngine y de un repo del grupo (copias en el scratchpad):
      el primer `index` con el binario nuevo no re-embebe nada (log de chunks embebidos = solo los
      cambiados).
- [ ] Gate del plan (fmt, clippy con y sin `gpu`, `cargo test --workspace`).

## Riesgos

- **Re-embeber todo por un detalle de formato.** Es justo lo que el Paso 3 evita; el test de
  migración es obligatorio y se corre sobre una copia de un índice real, no solo en memoria.
- **Hash del ONNX caro.** 98 MB (granite) a 1.2 GB (rerankers): cachear por mtime+tamaño; si el
  costo medido pasa de 1 s en frío, usar sha de los primeros/últimos MB + tamaño y documentarlo.
- **Memorias de otro repo del grupo.** El tier `group` vive en el central: un solo sello para todo el
  grupo; no hay huellas por repo dentro del central.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
