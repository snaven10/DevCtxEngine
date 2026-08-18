# Constructor de contexto

> 🇬🇧 [Read in English](../../03-core-concepts/context-builder.md)

Un solo brief con presupuesto para una pregunta: lo que ya se sabe, el código
que mejor rankea, y el conocimiento registrado contra exactamente ese código.

---

## Qué es

```bash
devctx context "cómo decidimos que un token expiró" --max-tokens 4096
```

Los agentes llaman la herramienta MCP `build_context`. La salida es **prosa, no
JSON** — está pensada para leerse directo al contexto de un modelo, y un sobre
JSON alrededor de código y prosa gasta presupuesto en puntuación.

## Por qué existe

Un agente frente a un área desconocida hace tres búsquedas, lee cuatro archivos
y corre un recall — gastando una tajada grande de su ventana en recuperación
antes de empezar a pensar. Peor: se lleva el *código* y se pierde el
razonamiento, porque nunca se le ocurrió preguntar qué ya se había decidido.

`build_context` hace ese ensamblado una sola vez, bajo un techo declarado, y
devuelve un solo artefacto.

## Las tres pasadas

El orden es el diseño. Cada pasada acota lo que la siguiente necesita decir.

### 1. Lo que ya se sabe

Un `recall` contra la pregunta, en todos los alcances, límite 5.

**Primero, porque es la parte que ninguna cantidad de lectura del código
recupera**, y porque es chica. El código te dice qué pasa; no te dice que la
alternativa obvia se probó y se abandonó.

Los archivos que nombran estas memorias se registran, y la pasada 2 los usa.

### 2. Código

Una búsqueda vectorial de la pregunta, trayendo 30 resultados.

**Los archivos que una memoria ya trajo se saltean** — no vale la pena pagarlos
dos veces. La búsqueda es deliberadamente más profunda que lo que va a entrar:
*el presupuesto decide dónde parar, no el límite*.

### 3. Registrado contra este código

Para los primeros 5 archivos que eligieron las pasadas 1 y 2, las memorias
vinculadas a esos archivos por la unión memoria↔grafo.

Esta es la pasada que justifica todo el diseño. Son memorias adheridas a
*exactamente este código* — conocimiento que un recall semántico sobre la
redacción de la pregunta nunca habría sacado a flote, porque la memoria y la
pregunta usan palabras distintas. Es para esto que existe la unión.

Cada una se etiqueta con la procedencia de su vínculo:

```
[memory · files-field · about crates/devctx-search/src/lib.rs] Rerank default
```

`files-field` y `content-mention` significan que algo conectó la memoria con
este código al escribirla. `inference` significa solo que las palabras calzan.
La etiqueta está ahí para que quien lea pueda ponderarla.

Los duplicados entre archivos se descartan por id de memoria.

## El presupuesto

`--max-tokens` (default 4096) es un **corte duro**, no una meta. Los tokens se
convierten a un presupuesto de caracteres con una razón fija, y cada ítem se
verifica contra el espacio restante antes de agregarse.

Dos comportamientos vale la pena conocerlos:

**Nada se descarta en silencio.** Lo que no entró se cuenta y se nombra al
final:

```
[devctx] 7 further item(s) did not fit in 4096 tokens.
Raise max_tokens, or narrow the query.
```

Un brief que truncara calladamente se leería como "esto es todo lo que hay", que
es lo único que jamás debe significar.

**Los encabezados viajan con su primer ítem.** Un encabezado de sección se emite
pegado a la primera entrada que entra, nunca solo. Un encabezado emitido por
separado puede sobrevivir a un presupuesto que sus ítems no sobrevivieron,
dejando una sección vacía — y una sección vacía se lee como "acá no hay nada",
que es justo el mensaje equivocado cuando la verdad es "no entró".

## Forma de la salida

```
## What is already known

[memory] El reranking queda apagado por defecto
Medido 30 ms → 8.6 s y 406 MB → 2.4 GB...

## Code

// crates/devctx-search/src/lib.rs:55
pub fn search(...)

## Recorded against this code

[memory · files-field · about crates/devctx-search/src/lib.rs] El pool es el techo
Un reranker reordena lo que le entregan y nada más...
```

Las secciones sin contenido no aparecen del todo.

## Opciones

| Flag | Default | Efecto |
|---|---|---|
| `--max-tokens` | 4096 | Techo duro para todo el brief |
| `--no-memories` | apagado | Solo código — saltea las pasadas 1 y 3 |

`--no-memories` es para cuando querés recuperación cruda sin la capa de opinión.

## Modelo mental

`search` responde *"¿dónde está el código?"*. `recall` responde *"¿qué
sabemos?"*. `build_context` responde la pregunta que el agente realmente tiene,
que es **"¿qué debería haber leído antes de responder esto?"** — y la responde
dentro de un techo que vos ponés, diciéndote con honestidad qué dejó afuera.
