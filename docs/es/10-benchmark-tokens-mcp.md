# Benchmark de payload de recuperación

> 🇬🇧 [Read in English](../10-mcp-token-benchmark.md)

Cuántos tokens cuesta *llegar a* una respuesta, medido de tres formas sobre las
mismas preguntas.

---

## Qué mide esto, y qué no

Esto mide el **payload de recuperación**: el tamaño de lo que cae en el contexto
de un modelo antes de que empiece a razonar. Ese es el mecanismo por el cual la
recuperación filtrada ahorra dinero, y es directamente medible y reproducible.

**No** mide el costo de una sesión de punta a punta. Eso depende del modelo, la
tarea, cuántos turnos toma el agente y cómo se comporta el caché — nada de lo
cual esta página puede controlar, y todo lo cual haría los números
irreproducibles. Si querés costo de sesión, medí el tuyo con el reporte de costo
de tu cliente.

Los conteos de tokens de abajo usan la misma heurística de ~4 caracteres por
token que el código usa internamente. Es una estimación, aplicada idénticamente
a las tres columnas, así que las *proporciones* se sostienen aunque los números
absolutos se corran.

## Método

Tres preguntas sobre este repositorio, cada una respondida de tres formas:

1. **grep-y-leer** — el enfoque ingenuo: buscar palabras clave probables, abrir
   cada archivo que coincida. Medido como el tamaño total de todos los archivos
   Rust que coinciden.
2. **`build_context`** — un solo brief con presupuesto, `--max-tokens 4096`.
3. **`search --limit 5`** — solo los fragmentos rankeados, como JSON.

Reproducilo con los comandos de la última sección.

## Resultados

Medido en este repositorio: 128 archivos, 2,333 fragmentos, `ml-granite`.

### "¿Cómo combina la fusión por rango recíproco los dos recuperadores?"

| Enfoque | Payload | Tokens est. | vs. grep |
|---|---|---|---|
| grep-y-leer (29 archivos) | 542,591 chars | ~135,600 | — |
| `build_context` | 15,518 chars | ~3,900 | **35× menos** |
| `search --limit 5` | 3,236 chars | ~800 | **168× menos** |

### "¿Por qué se hace checkpoint del WAL antes de que salga el servidor?"

| Enfoque | Payload | Tokens est. | vs. grep |
|---|---|---|---|
| grep-y-leer (56 archivos) | 939,148 chars | ~234,800 | — |
| `build_context` | 16,228 chars | ~4,100 | **58× menos** |
| `search --limit 5` | 4,153 chars | ~1,000 | **226× menos** |

### "¿Cómo se deduplican las memorias al guardarlas?"

| Enfoque | Payload | Tokens est. | vs. grep |
|---|---|---|---|
| grep-y-leer (22 archivos) | 518,519 chars | ~129,600 | — |
| `build_context` | 15,903 chars | ~4,000 | **33× menos** |
| `search --limit 5` | 6,517 chars | ~1,600 | **80× menos** |

## Cómo leer estos números

**La columna de grep es el villano honesto.** Su costo lo maneja cuántos
archivos coinciden con una palabra clave, no cuánto de ellos es relevante. La
pregunta del WAL es el peor caso justamente porque "checkpoint" y "wal" aparecen
en tests, comentarios y módulos no relacionados — 56 archivos, casi un megabyte,
para responder una pregunta cuya respuesta es un solo comentario de
documentación.

**`build_context` es plano.** Unos 4,000 tokens sin importar la pregunta, porque
eso es lo que pediste. Este es el punto de un presupuesto: el costo es un
parámetro que ponés, no un resultado que descubrís. Además reporta lo que no
entró, así que un costo plano no esconde una respuesta truncada.

**`search` es el más barato pero responde una pregunta más angosta.** Devuelve
código rankeado y nada más — sin decisiones previas, sin memorias registradas
contra esos archivos. Para "dónde está el código", es exactamente lo correcto.
Para "qué debería saber antes de cambiar esto", no.

**La brecha se ensancha con el tamaño del repositorio.** `build_context` está
acotado por su presupuesto; grep-y-leer crece con la cantidad de coincidencias
de palabra clave. En un repositorio diez veces más grande, la columna uno crece
y la dos no.

## El caveat que importa

Un agente real no lee los 29 archivos. Lee unos pocos, adivina, lee unos pocos
más. Así que la columna de grep es una cota superior de una estrategia, no una
predicción de lo que gastaría un agente en particular.

Lo que sí muestra correctamente es la *forma* del problema: con búsqueda por
palabra clave, el costo de encontrar una respuesta escala con qué tan comunes
son las palabras, y el agente no tiene forma de saber de antemano cuál de los 29
archivos es el bueno. La recuperación filtrada reemplaza esa búsqueda por un
payload acotado y rankeado.

## Reproducilo

```bash
# 1. cota superior de grep-y-leer
rg -l -i 'wal|checkpoint|ART' crates/ --type rust > /tmp/hits.txt
wc -l < /tmp/hits.txt                    # archivos que un agente podría abrir
xargs wc -c < /tmp/hits.txt | tail -1    # caracteres totales

# 2. un brief con presupuesto
devctx context "why is the WAL checkpointed before the server exits" \
  --max-tokens 4096 | wc -c

# 3. solo fragmentos rankeados
devctx search "why is the WAL checkpointed before the server exits" \
  --limit 5 --format json | wc -c
```

Dividí los caracteres entre 4 para la estimación de tokens. Corrélo contra tu
propio repositorio — lo que transfiere son las proporciones, no los números
absolutos.
