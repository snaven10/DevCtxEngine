# Ejemplo: depurar en un código grande

> 🇬🇧 [Read in English](../../05-examples/debugging.md)

Un flujo trabajado para un bug en código que no escribiste.

**Escenario:** los pagos a veces se cobran dos veces. Tenés un ticket, un
fragmento de stack trace que menciona `processPayment`, y cero familiaridad con
el módulo.

---

## Paso 0 — Verificá que el índice esté al día

```bash
devctx index_status
```

Una búsqueda sobre un índice viejo devuelve "no se encontró nada", que se ve
exactamente igual que "este código no existe". Descartalo primero — son treinta
segundos y te ahorra una hora.

## Paso 1 — Preguntá qué se sabe ya

Antes de leer una sola línea de código:

```
recall("doble cobro pago idempotencia")
```

Si alguien ya se topó con esto, la respuesta está acá y terminaste en una
llamada. Si no, aprendiste que el área no está documentada, lo que también sirve
— significa que vos vas a ser quien escriba la memoria al final.

## Paso 2 — Orientate con un solo brief con presupuesto

```
build_context("dónde se cobra un pago y cómo se maneja un reintento")
```

Devuelve tres cosas en un artefacto: memorias recuperadas sobre el área, el
código que mejor rankea, y — la parte que buscar a mano nunca alcanza — memorias
registradas contra exactamente los archivos que volvieron.

Esa tercera sección es donde aparecería "hicimos los reintentos no idempotentes
a propósito porque la pasarela deduplica", aunque nada de tu consulta usara esas
palabras.

## Paso 3 — Leé la definición real

Una vez que sabés el nombre del símbolo:

```
read_symbol("processPayment")
```

Devuelve la definición, su archivo, rango de líneas, tipo y código. Ojo que
devuelve **todas** las definiciones que coincidan con el nombre — si existen
dos, esa ambigüedad suele ser el bug.

Usá `read_symbol` cuando sabés el nombre. Usá `search` cuando solo sabés qué
*hace* el código.

## Paso 4 — Encontrá cada llamador

```
get_references("processPayment")
```

Cada sitio de llamada en el código indexado. Para un bug de doble cobro esta es
la llamada de mayor valor: dos llamadores donde esperabas uno es una explicación
completa.

## Paso 5 — Medí el radio de impacto antes de tocar nada

```
impact_analysis("processPayment")
```

Llamadores y llamados transitivos. Los llamadores te dicen qué podría romper un
cambio; los llamados te dicen de qué depende esto para funcionar.

**Leé la primera línea.** El grafo se indexa por nombre calificado, así que un
`processPayment` pelado se expande a cada `Clase.processPayment` y el reporte
dice cuáles fundió. Varias declaraciones significa que el radio es la unión de
todas — preguntá con el nombre calificado para acotarlo.

**El vacío sigue sin ser prueba.** Un reporte limpio significa "no encontré",
nunca "no hay": el despacho dinámico no deja arista, solo 7 lenguajes las
producen, y un índice desactualizado se ve idéntico a código ausente.

## Paso 6 — Antes de concluir que el código está mal

```
memories_by_symbol("processPayment")
```

El error más caro disponible para vos en este momento es "arreglar" una decisión
deliberada. El grafo de llamadas no te puede avisar de eso. Solo esto puede.

Revisá `link_sources` en cada resultado:

- `files-field` / `content-mention` — alguien conectó esta memoria con este
  código a propósito. Pesala.
- `inference` — las palabras casualmente coincidieron. Pesala menos.

## Paso 7 — Registrá el hallazgo

El fix va en el diff. La *razón* no, y eso es lo que necesita el próximo.

```bash
devctx remember "El doble cobro venía del envoltorio de reintentos llamando a
processPayment después de que la pasarela ya había confirmado. La pasarela es
idempotente por id de solicitud, pero el envoltorio generaba un id nuevo en cada
intento." \
  --type bug \
  --topic payments-double-charge \
  --files src/payments/processor.rs,src/payments/retry.rs
```

Tres cosas hacen que esta memoria sea útil y no decorativa:

- **`--files`** — es lo que la vuelve alcanzable desde `processPayment` después,
  vía `memories_by_symbol`. Sin eso, solo la encuentra una búsqueda de texto, y
  solo si adivinás la redacción.
- **`--topic`** — si el entendimiento se revisa, la revisión reemplaza esta
  entrada en vez de contradecirla.
- **La causa raíz, no el síntoma.** "Arreglado el doble cobro" no vale nada. La
  oración sobre el id nuevo en cada intento es todo el valor.

## El flujo completo

```
index_status                      → ¿está al día el índice?
recall                            → ¿alguien ya resolvió esto?
build_context                     → orientarse: conocido + código + memorias ligadas
read_symbol                       → la definición misma
get_references                    → quién lo llama
impact_analysis                   → hasta dónde llegaría un cambio
memories_by_symbol                → por qué está escrito así
remember --files --topic          → lo que el próximo necesita
```

## Qué te compró esto

La versión ingenua de esta tarea es: grep de `processPayment`, abrir cuatro
archivos, leer hasta que algo parezca mal, adivinar.

La diferencia no es la velocidad. Es que los pasos 1 y 6 sacan a flote
razonamiento que no existe en ninguna parte del código — y que el paso 7
significa que el próximo no repite nada de esto.
