# Flujo de trabajo del agente

> Volver al [README](../../README.md)
> 🇬🇧 [Read in English](../04-agent-workflow.md)

Cómo debería un agente usar realmente estas herramientas durante una tarea: cuál
responde qué pregunta, y en qué orden.

Para memoria en concreto — cuándo guardar, qué poner — ver
[MEMORY-PROTOCOL.md](../../MEMORY-PROTOCOL.md). Para la configuración inicial,
ver [AGENTS.md](../../AGENTS.md).

---

## El problema que esto resuelve

Un agente soltado en un repositorio desconocido tiene una ventana de contexto y
ninguna idea de qué hay adentro. El enfoque ingenuo — leer archivos hasta que
algo parezca relevante — gasta la ventana en recuperación y aun así se pierde el
razonamiento, porque el razonamiento no está en el código.

Lo que el código no te puede decir: que el enfoque obvio se probó y se abandonó,
que esta función es crítica para un llamador tres módulos más allá, que la rama
rara existe por un incidente en producción.

## Elegir herramienta

| La pregunta que tenés | La herramienta |
|---|---|
| ¿Dónde está el código sobre X? | `search` |
| Sé el nombre — mostrame la cosa | `read_symbol` |
| ¿Qué llama a esto? | `get_references` |
| ¿Qué se rompe si lo cambio? | `impact_analysis` |
| ¿Por qué está escrito así? | `memories_by_symbol` / `memories_by_file` |
| ¿Qué sabemos de X? | `recall` |
| ¿Qué debería leer antes de responder? | `build_context` |
| ¿Qué ruta HTTP sirve esto? | `search_routes` / `routes_for_handler` |
| La respuesta está en otro repositorio | `search_project` |
| Acabo de perder mi contexto | `memory_context` |

Las dos filas que la gente saltea son las caras de saltear: `impact_analysis`
antes de cambiar algo público, y `memories_by_symbol` antes de suponer que el
código está mal.

## El bucle

### Empezá con `build_context`

Si vas a hacer trabajo real en un área que no conocés, una llamada reemplaza las
primeras tres o cuatro:

```
build_context("cómo decidimos que un token expiró", max_tokens=4096)
```

Devuelve, en un solo brief con presupuesto: qué se decidió ya sobre esta área,
el código que mejor rankea, y las memorias registradas contra exactamente esos
archivos. Esa última parte es la que la recuperación manual nunca alcanza — una
memoria cuyas palabras no coinciden con tu pregunta pero cuyos *archivos* sí.

Te dice qué no entró, así sabés si subir el presupuesto o acotar la pregunta.

### Después acotá

`build_context` orienta. No reemplaza leer. Una vez que sabés qué símbolos
importan:

```
read_symbol("verify_token")      → la definición
get_references("verify_token")   → cada sitio de llamada
impact_analysis("verify_token")  → el radio de impacto, transitivo
```

**El grafo es binario por símbolo.** Medido en un repositorio Java/Quarkus,
`crearNotificacion` devolvió 8 aristas para 8 sitios de llamada mientras
`actualizar` y `cambiarEstado` devolvieron cero teniendo llamadores reales — y
nada dice de antemano en qué grupo está el tuyo.

O sea: **las aristas son confiables; el vacío no.** Un reporte limpio significa
"no encontré", nunca "no hay". Cruzá un vacío con `search --keyword` antes de
tocar nada.

### Antes de decidir que el código está mal

Corré `memories_by_symbol` sobre él. El error más caro que comete un agente es
"arreglar" una decisión deliberada, y el grafo de llamadas no puede avisarte —
solo la memoria puede.

Leé `link_sources` en el resultado. `files-field` y `content-mention` significan
que alguien conectó esa memoria con ese código a propósito. `inference`
significa solo que coincidieron las palabras, y merece menos peso.

### Cuando terminás

Registrá lo que la próxima sesión va a necesitar. La vara es: **¿alguien
volvería a deducir esto, a un costo, si no estuviera escrito?**

Fixes de bugs con causa raíz, decisiones con razonamiento, detalles
traicioneros, convenciones. No: lo que el diff ya dice.

Pasá siempre `files`. Una memoria sin eso solo se encuentra por texto, lo que
exige saber ya qué preguntar. Con eso, la memoria llega a cualquiera que caiga
sobre ese código. Detalles en
[MEMORY-PROTOCOL.md](../../MEMORY-PROTOCOL.md).

## Trabajar entre repositorios

`list_projects` muestra qué rastrea esta máquina. `search_project` busca en otro
por nombre sin salir de tu sesión — la pregunta de backend que te salta editando
el frontend.

Si una lección resulta aplicar más allá de un repositorio, `memory_move` la
promueve a `group` o `global` en vez de obligarte a reescribirla.

## Cuando no hay nada vinculado

Un servidor MCP registrado globalmente arranca en el directorio desde el que se
lanzó el cliente, que con frecuencia no es ningún repositorio. Las herramientas
entonces reportan que no hay proyecto vinculado.

```
list_projects        → qué existe
use_project <nombre> → vincular esta sesión
```

Este es un estado normal, no una instalación rota.

## Cuando el índice está viejo

`index_status` reporta el último commit indexado y si el índice está al día. Si
está atrasado, `index_repo` lo pone al día de forma incremental — solo lo que
cambió.

El índice refleja el **árbol de trabajo**, no el último commit, así que el
código sin commitear es buscable. Lo que git ignora no lo es, que es la razón
habitual de que un archivo que ves no aparezca.

## Antipatrones

**Leer archivos para encontrar cosas.** Para eso está `search`. Leer es para
después de saber qué archivo.

**Saltear `impact_analysis` porque el cambio parece chico.** El tamaño del diff
no tiene relación con el tamaño del radio de impacto.

**Confiar en una búsqueda que no devolvió nada.** Verificá `index_status`
primero — un resultado vacío de un índice viejo o sin construir se ve idéntico a
un resultado vacío de código que no existe.

**Guardar una memoria sin `files`.** Cuesta un campo y determina si la memoria
se vuelve a encontrar alguna vez desde el código.

**Suponer que el reranking ayudaría.** Está apagado por defecto porque se midió:
dos órdenes de magnitud más lento, y el único modelo evaluado contra la suite
empeoró los resultados. Ver [ADR-15](08-decisiones-de-diseno.md).
