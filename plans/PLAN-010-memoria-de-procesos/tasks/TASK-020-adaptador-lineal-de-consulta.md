# TASK-020 — Adaptador lineal sobre la consulta (384×384) — pospuesta

- **Plan:** PLAN-010 — Memoria de procesos (bloque de calidad de búsqueda, decisión del usuario 2026-10-11)
- **Especialista:** general-purpose (Rust + script de entrenamiento)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-016, TASK-017
- **Estado:** `postponed` (pospuesta: ver "Por qué se pospone")

---

## Objetivo (si se reabre)

Aprender una matriz `W` de 384×384 que se aplica **solo al vector de la consulta** (`q' = norm(W·q)`)
para acercar consultas en español a chunks de código con identificadores en inglés. Los vectores
indexados no cambian, así que **no obliga a reindexar**: se puede prender, apagar o reentrenar sin
tocar los índices. PLAN-010 DD-17.

## Por qué se pospone

- **Entra en "sin fine-tuning".** Entrenar `W` es optimizar parámetros con pares
  (consulta, chunk relevante) y negativos: un fine-tuning chico (147 456 parámetros) sobre la salida
  del modelo. El usuario decidió el 2026-10-11 no hacer fine-tuning por ahora; este caso es el
  borde de esa regla y se interpreta del lado conservador.
- **Necesita datos que no tenemos.** Con ~80 casos etiquetados (TASK-014) no se entrena sin
  sobreajustar, y esos casos son el set de evaluación: no pueden ser también el de entrenamiento.
  Haría falta un set aparte de cientos o miles de pares (p. ej. sintéticos: docstring/comentario →
  función, en ES y EN), lo que es un proyecto en sí.
- **Las alternativas baratas van primero.** TASK-016 (traducción de la consulta por el propio agente)
  ataca la misma brecha ES→EN sin entrenar nada; TASK-017 (CLS) puede cerrarla en parte. Si después
  de ambas la brecha ES/EN sigue > 15 pp en Hit@5, se reabre esta task con el usuario.

## Contexto verificado

- La consulta se embebe con el mismo `LocalProvider` que los chunks; `l2_normalize` al final
  (`crates/devctx-embed/src/local.rs`). Un adaptador iría después de eso y antes de la búsqueda, y
  su identidad (hash de `W`) tendría que entrar en la huella **de consulta**, no en la de los
  vectores indexados (TASK-013).

## Pasos (si se reabre)

- [ ] Set de entrenamiento separado del de evaluación, sin datos de clientes en el repo.
- [ ] Entrenamiento fuera del binario (script de Python, `W` inicializada en identidad, pérdida
      contrastiva con negativos del mismo repo, regularización hacia la identidad).
- [ ] `W` versionada como archivo con sha256; `embeddings.query_adapter: <ruta>`; apagado por defecto.
- [ ] Medición con el arnés de TASK-014 y los umbrales de DD-12; EN no puede bajar.

## Criterios de aceptación (si se reabre)

- [ ] Decisión explícita del usuario de que esto no viola "sin fine-tuning", registrada en el master.
- [ ] Umbrales de DD-12 sobre el set de evaluación, que nunca se usó para entrenar.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-010 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
