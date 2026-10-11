# TASK-017 — Granite como fue entrenado: pooling CLS y `max_length` configurable para user-defined (cambio de vectores declarado)

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-010-memoria`
- **Depende de:** TASK-007, TASK-013, TASK-014
- **Estado:** `pending`

---

## Objetivo

Usar `ml-granite` (y `ml-granite-lg`) como los publica IBM: pooling **CLS** en vez de Mean, y sin
cortar en 512 tokens chunks que el modelo puede leer enteros. El modelo **no cambia** (sigue
`granite-embedding-97m-multilingual-r2`, mismo ONNX, 384 dims); cambia cómo se usa, y eso cambia los
vectores. Por eso es un **cambio declarado**: va después del motor propio (cuya equivalencia se midió
contra fastembed con Mean, TASK-003/004), con huella nueva (TASK-013), migración explícita y medición
antes/después por idioma. PLAN-010 DD-15.

## Contexto verificado

- `1_Pooling/config.json` de `ibm-granite/granite-embedding-97m-multilingual-r2` (HF, leído
  2026-10-11): `pooling_mode_cls_token: true`, `pooling_mode_mean_tokens: false`. Hallazgo del
  usuario: lo mismo en el 311M (`ml-granite-lg`) — re-verificar al arrancar.
- devctx los carga con `UserDefinedEmbeddingModel::new(..).with_pooling(Pooling::Mean)`
  (`crates/devctx-embed/src/local.rs:252`). Tras TASK-004 eso vive en el motor propio con pooling
  explícito por modelo (`LocalModelSpec.pooling`, TASK-013 Paso 2, hoy = Mean).
- `max_length`: user-defined sin `with_max_length` (`local.rs:201` solo para builtins) → 512 de
  fastembed. En el `model_dir` real: `config.json` `max_position_embeddings: 32768`,
  `tokenizer_config.json` `model_max_length: 32768`, ModernBERT con `local_attention: 128` y
  `global_attn_every_n_layers: 3` (la atención global, cuadrática, es 1 de cada 3 capas).
  `DEVCTX_EMBED_MAX_CHARS = 4096` recorta antes del tokenizer: el techo real es ~1000-1500 tokens de
  código, no 32k.
- TASK-007 deja `embeddings.max_length` configurable (default 512) y el aviso en `status` si difiere
  de lo indexado; Q-2 sigue abierta para el default. Esta task es la que trae los números para
  cerrarla **para Granite**.
- Memorias: el central (`memory.model: ml-granite`) y las locales usan el mismo modelo; `memory.model`
  no se puede cambiar con memorias adentro, pero aquí no cambia el modelo: cambia la huella, que desde
  TASK-013 sí está guardada y bloquea la mezcla.
- Costo de reindex de referencia: ~1 h por ~1400 archivos en 8 núcleos (AGENTS.md §6); los 15 repos
  registrados del usuario, ~4-4.5 h. Re-embeber memorias: ~45 por minuto (AGENTS.md §5); ~2000
  memorias ≈ 45 min.

## Archivos

- **Modificar:** `crates/devctx-embed/src/registry.rs` (`pooling` nativo de Granite = `Cls`, más un
  `legacy_pooling = Mean` para la fijación), `crates/devctx-embed/src/onnx/*` (CLS ya existe por los
  builtins BGE: solo se elige), `crates/devctx-core/src/config.rs` (`embeddings.pooling:
  auto|mean|cls`), `crates/devctx-embed/src/lib.rs` (resolución del pooling efectivo),
  `crates/devctx-mcp/src/state.rs` y `crates/devctx-cli/src/main.rs` (`status`, comando de
  migración), `crates/devctx-central/src/lib.rs` + `devctx-cli` (`memories reembed`), arnés de
  equivalencia (dorados CLS de referencia).

## Pasos

- [ ] **Paso 1 — pre-chequeos.** (a) El ONNX en uso expone `last_hidden_state` (no un
      `sentence_embedding` con Mean ya horneado): si solo trae el pooled, CLS no es posible sin
      re-exportar → `blocked` y se decide con el usuario. (b) La tarjeta del modelo no pide prefijo de
      instrucción para consulta/documento (verificar; si lo pide, entra como parte del cambio).
      (c) Estadística de truncación: % de chunks de cada repo del set con > 512 tokens según el
      tokenizer de Granite (con el `max_chars` actual).
- [ ] **Paso 2 — dorados de referencia CLS.** Generar vectores CLS de referencia con onnxruntime de
      Python sobre **el mismo** ONNX int8 y el mismo tokenizer (script en `scripts/`, fuera de
      `cargo test`), para ~50 textos del arnés de TASK-003; el motor propio con `pooling: cls` debe
      dar coseno ≥ 0.9999. Los dorados Mean de TASK-003 siguen valiendo para `pooling: mean`.
- [ ] **Paso 3 — matriz de calidad** (índices de prueba en el scratchpad, nunca los reales):
      pooling {mean, cls} × `max_length` {512, 1024, efectivo con `max_chars` 4096 (~2048)} sobre
      los repos del set de TASK-014. Medir relevancia por idioma (3 corridas, mediana), recall de
      memorias, tiempo de indexado (chunks/s), `VmHWM` del indexado y `search` p95.
- [ ] **Paso 4 — fijación al espacio guardado.** `embeddings.pooling: auto` (default) resuelve así:
      si la rama/el store de memorias tiene huella guardada, usa **el pooling de esa huella** (un
      índice viejo sigue en Mean y sus consultas también: nada se mezcla y nada se re-embebe solo);
      si no hay nada guardado (repo nuevo), usa el nativo del modelo. `status` muestra
      `embeddings.upgrade_available: "pooling mean→cls, max_length 512→N"` y el comando. Lo mismo
      para `max_length`. Así un hook de git o un `index` incremental **nunca** dispara las 4 h por su
      cuenta.
- [ ] **Paso 5 — migración explícita.** `devctx index --full --upgrade-embeddings` (por repo) adopta
      los nativos, re-embebe y re-sella; `devctx memories reembed --scope local|group|global`
      (servers parados, como `migrate`) re-embebe memorias con el setup nuevo, re-sella la huella del
      espacio de memorias y no pierde nada si se corta (se escribe a una tabla nueva y se reemplaza al
      final; idempotente). `memories export` antes de reembeber, siempre (lo hace el comando).
- [ ] **Paso 6 — decidir defaults** con la matriz: el par (pooling, `max_length`) que cumple la
      regla por task de PLAN-010 DD-12 con menor costo. Si CLS no gana, se deja Mean y se documenta por qué contra la tarjeta.
- [ ] **Paso 7 — guía de migración** (insumo de TASK-010): orden recomendado para un usuario con N
      repos y memorias: parar servers → `memories export` → `memories reembed` → `index --full
      --upgrade-embeddings` repo por repo (o en serie con un script); tiempo estimado por repo según
      su número de archivos.

## Criterios de aceptación

- [ ] Paso 1 y la estadística de truncación en el Resultado.
- [ ] Dorados CLS: coseno ≥ 0.9999 contra la referencia de Python, peor caso anotado.
- [ ] Con los defaults elegidos, sobre el set de TASK-014, se cumple la **regla por task** de PLAN-010 DD-12 (mejora puntual en ES, ninguna regresión con IC)
      (recall Hit@3 sin regresión); indexado ≤ +20 % de tiempo; `VmHWM` del indexado ≤ 1.5 GB (§6).
- [ ] Tests: `pooling: auto` sobre un store con huella Mean consulta en Mean; repo nuevo usa CLS;
      `--upgrade-embeddings` re-sella; `memories reembed` cortado a la mitad deja las memorias
      originales intactas; `recall` con huella distinta falla con el mensaje de TASK-013.
- [ ] Ningún índice real se re-embebe por actualizar el binario (verificado en TASK-011).

## Costo para el usuario (para decidir en TASK-011, con aprobación)

- **Reindex:** opcional y explícito por repo; los 15 repos ≈ 4-4.5 h de CPU (en serie). Sin él, cada
  repo sigue funcionando en Mean.
- **Memorias:** `memories reembed` ≈ 1 min por 45 memorias. Obligatorio solo si se quiere CLS en
  memorias; sin él, el central sigue en Mean (fijado por huella).
- **`max_length` > 512:** encarece cada chunk largo (medido en el Paso 3); se acepta solo dentro del
  +20 %.

## Riesgos

- **Mezcla de espacios** entre un repo migrado y el central sin migrar: no es mezcla, son stores
  distintos con huellas distintas; lo único que se pierde es `shares_vector_space` (el serve no puede
  reusar el embedder del central). Medir el costo de RAM de tener ambos (TASK-001 da el método).
- **Búsqueda degradada durante el `--full`**: la rama está en `transition` con vectores mezclados
  mientras corre. Documentarlo; recomendar migrar con el repo sin uso.
- **CLS empeora el inglés**: el criterio EN lo frena; en ese caso el default queda en Mean.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
