# Análisis de Infraestructura DevAI
## Comparativa de Consumo y Costos: MCP (Retrieval Filtrado) vs. Modo Directo (Volcado Bruto)

Este documento analiza el impacto del Model Context Protocol (MCP) de DevAI sobre el consumo de
tokens y el costo de una tarea real ejecutada con agentes de desarrollo. La medición se hizo con un
A/B controlado: **la misma tarea de diagnóstico**, resuelta dos veces, variando únicamente si el agente
tenía acceso a las herramientas de DevAI (retrieval vectorial + memoria) o estaba forzado a `grep`/`read`.

> **Metodología**: ambas sesiones partieron de contexto limpio, mismo modelo, misma tarea de
> análisis/consulta (**0 líneas de código cambiadas** en ambas → sin varianza de implementación).
> Cifras tomadas de `/cost` al cierre de cada sesión.

---

## 1. Resumen Ejecutivo

| Métrica General | Con DevAI MCP (filtrado) | Sin MCP (volcado directo) | Impacto |
|---|---|---|---|
| **Costo total** | **$1.19 USD** | **$4.14 USD** | **+247.9%** sin MCP · **ahorro 71.3%** con MCP |
| Volumen total de tokens | ~0.67 M | ~8.27 M | **~12× más** sin MCP |
| Cache read total | 543.5 k | 7.56 M | ~14× más sin MCP |
| Tokens de salida (output) | ~6.0 k | ~53.2 k | ~8.9× más sin MCP |
| Duración API | 11 min 36 s | 11 min 28 s | Prácticamente idéntica |
| Wall time (real) | 20 min 48 s | 12 min 56 s | +7 min 52 s con MCP *(latencia local, ver §4)* |
| Líneas de código cambiadas | 0 | 0 | Tarea de análisis en ambos casos |

**Titular:** en una tarea de diagnóstico, DevAI MCP redujo el costo un **71.3%** y movió **~12× menos
volumen de tokens**, a cambio de mayor *wall time* atribuible a latencia de indexación local (no al API).

---

## 2. Desglose Técnico por Modelo

| Configuración | Tokens Entrada | Tokens Salida | Cache Read | Cache Write | Costo Parcial |
|---|---|---|---|---|---|
| **Con MCP** · claude-haiku-4-5 | 594 | 19 | 0 | 0 | $0.0007 |
| **Con MCP** · claude-opus-4-8 | 14.0 k | 6.0 k | 543.5 k | 112.2 k | $1.1900 |
| **Sin MCP** · claude-haiku-4-5 | 6.1 k | 25.5 k | **7.5 M** | 688.5 k | $1.7400 |
| **Sin MCP** · claude-opus-4-8 | 14.1 k | **27.7 k** | 61.9 k | 256.1 k | $2.4000 |

---

## 3. Los Dos Drivers del Ahorro

El sobrecosto del modo directo **no** proviene de un solo factor. Hay dos, y conviene documentarlos por
separado:

### 3.1 Driver A — Cache read: re-inyección de repositorios completos
Sin un filtro intermedio, el agente vuelca y re-lee porciones grandes del repositorio en cada turno. El
caché de prompt los relee una y otra vez: **7.5 M de tokens de cache read solo en Haiku**, frente a
**543.5 k con MCP** (~14× menos). El MCP actúa como indexador inteligente: pre-filtra con búsqueda
vectorial + memoria y entrega a Opus un contexto **limpio y acotado**.

### 3.2 Driver B — Output blowup: respuestas más largas y redundantes
Menos evidente pero igual de relevante: **sin MCP, Opus generó 27.7 k tokens de salida vs 6.0 k con MCP
(4.6×)**. Sintetizar a partir de código crudo volcado induce al modelo a divagar y repetir. Y el **output de
Opus es el componente NO cacheable más caro** — por eso el Opus-sin-MCP costó $2.40, impulsado por su
output, no por su caché. **Contexto limpio → respuestas más cortas y precisas → menos output caro.**

> En conjunto: el modo directo paga de más en **dos frentes** — cache reads masivos *y* output inflado.

---

## 4. El Trade-off de Wall Time (es local, no del API)

El modo MCP fue ~8 min más lento en *wall time* (20:48 vs 12:56), pero **el tiempo de API fue
prácticamente idéntico** (11:36 vs 11:28). Es decir: el modelo **no “pensó más”**. Los minutos extra son
**latencia local de DevAI**: el servicio ML calculando embeddings en CPU (hardware sin GPU dedicada) más
los round-trips del protocolo.

**Implicación práctica:** ese sobrecosto de tiempo es *CPU-bound y mitigable* —
- con GPU dedicada, o
- con un modelo de embeddings más liviano (p. ej. `ml-minilm` 384d en vez de `ml-mpnet` 768d).

No es un costo inherente a la arquitectura MCP, sino del entorno de ejecución.

---

## 5. Influencia del Modelo de Embeddings (este benchmark usó el más pesado)

Este A/B se ejecutó con **`ml-mpnet` (paraphrase-multilingual-mpnet-base-v2, 768 dim)** — el modelo de
embeddings **más pesado y de mayor calidad** disponible en la instalación. Es clave para interpretar bien
los resultados, porque cada métrica reacciona distinto al peso del modelo:

- **El ahorro de costo/tokens (≈71%) es prácticamente independiente del modelo.** Proviene del *filtrado*
  (el retrieval devuelve fragmentos acotados en lugar de volcar repos), no del peso del modelo. Un modelo
  más liviano filtra igual → el ahorro se mantendría en el mismo orden de magnitud.
- **El wall time SÍ cambiaría — a favor.** El penalty de tiempo (§4) viene del cómputo de embeddings en
  CPU. Un modelo más liviano es mucho más rápido:
  - `ml-minilm` (384 dim, multilingüe): ~5× más rápido que `ml-mpnet` en CPU.
  - `minilm-l6` (384 dim, inglés): aún más rápido (22 MB vs 1.1 GB).
  → El gap de ~8 min de wall time **se reduciría sustancialmente** con cualquiera de los dos.
- **El precio a pagar: precisión de retrieval.** `ml-mpnet` da el mejor ranking, sobre todo en **español**.
  Un modelo más liviano puede traer resultados algo menos relevantes → ocasionalmente el agente hace una
  búsqueda extra o lee un poco más, lo que **erosiona marginalmente** el ahorro de tokens, sin cambiar el
  orden de magnitud.

| Modelo | Dim | Velocidad (CPU) | Calidad retrieval (ES) | Cuándo conviene |
|---|---|---|---|---|
| `ml-mpnet` *(usado en este benchmark)* | 768 | lenta (~225 ms/embed) | **mejor** | Máxima precisión en español; equipo con CPU decente o GPU |
| `ml-minilm` | 384 | ~5× más rápida | buena | **Balance** velocidad/calidad para español en equipos modestos |
| `minilm-l6` | 384 | la más rápida | menor (inglés) | Prioridad velocidad / contenido en inglés |

> **Lectura honesta:** los números de este informe son el **escenario de mayor calidad y mayor wall time**.
> Con un modelo más liviano tendrías **el mismo ahorro de costo (~71%) con bastante menos penalidad de
> tiempo**, a cambio de algo de precisión de retrieval. En otras palabras: el ahorro económico es robusto;
> el costo de tiempo es ajustable según el modelo que elijas.

---

## 6. Alcance y Caveat de Dominio

El ahorro del 71% corresponde a una tarea de **diagnóstico / comprensión** — el escenario donde el volcado
bruto es, en su mayoría, redundancia. La brecha se **estrecha** en tareas donde genuinamente hay que tocar
cada archivo (p. ej. un refactor masivo), porque ahí el contenido se lee igual con o sin MCP.

> Regla operativa: **MCP rinde más cuanto más “buscar la aguja en el pajar” sea la tarea**, y menos cuando
> la tarea es “tocar todo el pajar”.

---

## 7. Conclusiones y Recomendación

**Hallazgos clave:**
- **Costo:** ahorro del **71.3%** ($1.19 vs $4.14) en la tarea de diagnóstico medida.
- **Volumen:** **~12× menos** tokens totales movidos; **~14×** menos cache read; **~8.9×** menos output.
- **Doble ahorro:** el MCP recorta tanto la *re-lectura de contexto* (cache) como la *verbosidad de la
  respuesta* (output caro de Opus).
- **Costo de tiempo:** mayor *wall time*, pero por **latencia de indexación local (CPU)**, no por el API —
  mitigable con GPU o un modelo de embeddings más liviano.
- **Modelo usado:** este benchmark corrió con el modelo **más pesado** (`ml-mpnet`, 768 dim) → es el peor
  caso en *wall time* y el mejor en precisión. Con `ml-minilm` el ahorro de costo se mantendría y el tiempo
  bajaría notablemente (ver §5).

**Recomendación:** para tareas de diagnóstico y comprensión de código, el uso de DevAI MCP es **altamente
recomendable**: el ahorro financiero (≈71%) y la reducción de volumen de tokens (≈12×) superan con holgura
el costo en tiempo de espera, que además es optimizable a nivel de hardware/modelo. Para refactors que
requieren leer la base completa, evaluar caso por caso.

---

*A/B ejecutado el 2026-05-29 · tarea de diagnóstico sobre un workspace multi-repo real · modelo de embeddings
`ml-mpnet` (768 dim, el más pesado disponible) · cifras de `/cost` de Claude Code.*
