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
| ¿En qué estaba en este plan? ¿Qué sigue? | `plan_status` |

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

**El grafo se indexa por nombre calificado.** Un nombre pelado se expande a cada
`Clase.metodo` que lo lleva, y el reporte dice qué declaraciones fundió — en un
repositorio Quarkus, `actualizar` son veintiún métodos distintos. Preguntá con el
nombre calificado cuando querés decir uno solo.

**El vacío sigue sin ser prueba.** Un reporte limpio significa "no encontré",
nunca "no hay": el despacho dinámico no deja arista, solo 7 lenguajes las
producen, y un índice desactualizado se ve idéntico a código ausente. Cruzá un
vacío con `search --keyword` antes de tocar nada.

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

## Planes, y el formato markdown que espera `plan_status`

Un repositorio que usa `plans/PLAN-NNN[-slug]/` para sus planes de trabajo
tiene `plan_status` gratis: sin argumento lista todos los planes y nombra el
activo (el que tiene trabajo pendiente y cuyo `.md` cambió más reciente;
empate va al número de plan más alto); `plan: "PLAN-005"` (o `5`, o el nombre
del directorio) da las tasks listas/en curso/bloqueadas de ese plan. Llamala
justo después de compactar — es lo que le falta al contexto recuperado.

`plan_status` lee archivos `plans/PLAN-NNN[-slug]/tasks/TASK-NNN-*.md`. El
bloque de encabezado (entre el H1 y la primera sección `## `) necesita como
mínimo:

```markdown
# TASK-003 — un título corto

- **Plan:** PLAN-005 — título del plan
- **Depende de:** TASK-001, TASK-002
- **Estado:** `pending`
```

`Estado` toma uno de cinco valores, escritos como prefieras; el parser
acepta además estos sinónimos (mayúsculas o minúsculas, sin importar
acentos, en inglés o español):

| Estado | Palabras reconocidas |
|---|---|
| `pending` | `pending`, `pendiente`, `todo` |
| `in_progress` | `in_progress`, `in-progress`, `en progreso`, `en curso`, `in progress`, `wip` |
| `done` | `done`, `completed`, `hecho`/`hecha`, `terminada`, `cerrada`, `implemented`, `ejecutada`, `merged`, o un `✅` a secas |
| `blocked` | `blocked`, `bloqueada`/`bloqueado` |
| `skipped` | `skipped`, `omitida`, `descartada`, `cancelled`/`cancelada`, `superseded`, `postponed`/`pospuesta` |

`skipped` es para una task descartada a propósito: resuelve a las tasks que
dependen de ella (como `done`) pero se cuenta aparte — el CLI muestra
`done/total (+N skipped)` — porque no se hizo. El valor puede traer emoji,
negritas y prosa al final (`✅ **`done`** — ejecutada en la rama`): gana un
span entre backticks que sea exactamente una palabra clave; si no, la primera
palabra clave del texto. Un valor sin palabra clave queda desconocido y
genera un warning; el parser nunca adivina. `` `pending` `` entre backticks
sigue siendo la forma recomendada.

Dónde se busca el estado, en orden:

1. un campo del encabezado — `Estado`, `Status` o `State`, en cualquiera de
   `- **Estado:** x`, `**Estado**: x`, `**Estado: x**` o `Estado: x` plano;
   varios campos pueden compartir línea separados por ` · ` (`**Plan:**
   PLAN-005 · **Estado:** pending · **Depende de:** —`);
2. front matter YAML (`---` … `---` al tope del archivo) con `estado:`,
   `status:` o `state:`;
3. una sección `## Estado` (o `## Status`): su primera línea no vacía.

El nombre tiene que coincidir exacto, así que `Estado final` o `Estado
objetivo` no son el estado.

`Depende de` (también `Depends on`, `Dependencias`, `Dependencies`,
`Bloqueada por`, `Blocked by`; en el encabezado o el front matter) solo cuenta
tokens `TASK-<id>`: la prosa alrededor se ignora, y se expanden rangos
(`TASK-001..004`) y listas (`TASK-033/034`). Un `—`, `Ninguna`, `None`, `N/A`
o `[]` al inicio significa ninguna, aunque el texto después mencione una task
(`— (paralela a TASK-001)`). Una referencia a una task de otro plan
(`PLAN-120/TASK-031`) se guarda como dependencia externa: se reporta pero
nunca bloquea. Como los ids se sacan del texto, un id de task mencionado en
una frase de este campo sí se vuelve arista — dejá en el campo solo las
dependencias.

Los ids conservan un sufijo o prefijo de letra tal cual: `TASK-001b` no es
`TASK-001`, y `TASK-R01` es su propia task. Dos archivos con el mismo id (un
compañero `TASK-001-DESIGN.md` junto a `TASK-001-verify.md`) generan un
warning y solo el primero, en orden alfabético, es la task. Lo demás que el
parser no pueda ubicar se vuelve un warning, nunca una adivinanza.

La tabla de progreso propia de un plan (en el `.md` del plan, no en el archivo
de la task) solo se lee para chequear que coincide con los archivos de las
tasks, y solo cuenta una tabla con columna `Estado`/`Status` (una tabla de
rollback con ids en la primera columna se ignora). El documento del plan es el
archivo con prefijo `PLAN-`, prefiriendo `<nombre-del-dir>.md`. **El archivo de
la task siempre gana** — si la tabla de un plan dice
`done` y el archivo de la task dice `pending`, la task se reporta `pending`,
más un warning nombrando la discrepancia. Nunca edites un archivo de task para
que un warning desaparezca sin antes verificar cuál de los dos es correcto.

### De dónde se lee `plans/`

La salida de `plan_status` trae `plans_root: {path, source}`. La raíz se
resuelve en este orden:

1. **workspace** — la raíz del workspace (donde arrancó un descenso de grupo
   del MCP, o el directorio desde el que lanzaste si no es ningún proyecto),
   si tiene `plans/`;
2. **project** — la raíz propia del proyecto, si tiene `plans/`;
3. **ancestor** — solo cuando el proyecto declara `group:`: el ancestro más
   cercano con `plans/PLAN-*`, deteniéndose por debajo de `$HOME`;
4. si no, `source` es `none` y la lista de planes queda vacía.

El caso de uso: un producto cuyos repositorios son proyectos devctx separados
bajo un directorio que no es proyecto, con los planes en
`<workspace>/plans/` — pertenecen al producto, no a un repo. Con ese layout el
MCP en modo grupo (o sin vincular) lee `<workspace>/plans`; `devctx web`
lanzado desde la raíz del workspace funciona sin `.devctx/` ahí (el mismo
descenso de grupo que `devctx mcp`); y el CLI desde dentro de un repo miembro
que declara `group:` encuentra el `plans/` del workspace como ancestro. Un
daemon en marcha sigue sirviendo la raíz que resolvió al arrancar: tras el
cambio, corré `devctx serve --stop`.

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
