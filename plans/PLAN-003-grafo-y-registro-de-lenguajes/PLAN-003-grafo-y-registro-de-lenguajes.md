# PLAN-003 — El grafo de llamadas responde por nombre pelado, y los lenguajes se definen en JSON

**Fecha:** 2026-08-23
**Fase:** 2 (Ejecución)
**Diseño:** [`PLAN-003-design.md`](./PLAN-003-design.md)
**Proyectos:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama propuesta `feature/grafo-y-registro-de-lenguajes`
**Origen:** Reporte del usuario — "el grafo no une bien los símbolos, ¿es el soporte de Java el que está mal?"

## 1. Qué resuelve

Dos cosas que se descubrieron juntas y se arreglan juntas porque tocan los mismos archivos:

**El grafo devuelve vacío para nombres que sí tienen aristas.** `impact_analysis`,
`get_references` y `get_callers/callees` consultan por igualdad exacta contra un
campo que a veces está calificado (`Clase.metodo`) y a veces no. En Java el
`source` está calificado **siempre**, así que preguntar por el nombre pelado es
un fallo garantizado. Un reporte limpio se lee como "este cambio es seguro", y no
lo es.

**Agregar o afinar un lenguaje cuesta 9 ediciones regadas en 6 funciones.**
La definición de un lenguaje no se puede leer de corrido, y las listas
`FUNCTION_KINDS`/`CONTAINER_KINDS` son globales compartidas — razón por la cual
los constructores de Java no producen aristas y nadie lo notó.

**Por qué ahora:** el bug envenena la herramienta que se usa para decidir si un
refactor es seguro, y ya se documentó una explicación equivocada del síntoma que
hay que retractar.

## 2. Hallazgos que reorientan el plan

Verificado 2026-08-23 contra REVFA_BackEnd, rama `development`, con el binario instalado:

| Consulta | Resultado |
|---|---|
| `devctx impact actualizar` | **0** callers, **0** callees |
| `devctx impact OficinaService.actualizar` | **1** caller, **23** callees |
| `devctx impact crearNotificacion` | **8** callers directos |

**Java no está mal. El grafo no está roto.** Las aristas existen; la llave de
búsqueda no coincide. `crearNotificacion` funcionaba porque Java no exige `this.`
en llamadas intra-clase, así que su target quedó pelado. `actualizar` fallaba
porque se llama vía campo tipado (`oficinaService.actualizar`), así que su target
quedó calificado.

Esto **retracta** lo que se documentó en 6 páginas, en `~/.claude/protocols/devctx-memory.md`
y en `~/.claude/CLAUDE.md`: *"la cobertura es binaria por símbolo y nada los
distingue de antemano"*. Sí hay algo que los distingue y es determinista.

Dos defectos secundarios salieron en la misma corrida:

- **Receptores basura.** Targets reales medidos: `Oficina.findByCodigo(codigo).flatMap`
  y una expresión de 3 líneas con un lambda adentro. `receiver_of` toma el texto
  crudo del nodo `object`, que en una cadena fluida es toda la expresión previa.
- **Constructores Java invisibles.** `constructor_declaration` no está en
  `FUNCTION_KINDS` (`lang.rs:177`), así que toda llamada dentro de un constructor
  se descarta entera. *(Evidencia de lectura de código, no medida.)*

## 3. Tasks y orden

| Task | Qué | Especialista | Depende de | Estado |
|------|-----|--------------|------------|--------|
| TASK-001 | Expandir el nombre pelado a sus formas calificadas al consultar el grafo | — | — | `done` |
| TASK-002 | Aceptar el receptor como calificador solo si es un identificador limpio | — | — | `pending` |
| TASK-003 | `lang.rs` → registro JSON embebido, con kinds por lenguaje | — | TASK-002 | `pending` |
| TASK-004 | `constructor_declaration` y kinds Java completos | — | TASK-003 | `pending` |
| TASK-005 | Retractar la afirmación falsa en docs, protocolos y CLAUDE.md | — | TASK-001 | `pending` |
| TASK-006 | Verificación con datos reales sobre REVFA_BackEnd | — | TASK-001..004 | `pending` |
| TASK-007 | El recall entre repositorios devolvía cero, en dos capas | — | — | `done` |
| TASK-008 | El test flaky era el daemon central perdiendo una carrera de 4 s | — | — | `done` |

**Paralelizables:** TASK-001 y TASK-002 no se tocan (store vs parse). TASK-005
puede correr en paralelo con 002/003 una vez cerrada la 001. TASK-003 va después
de 002 para no chocar en `parser.rs`.

**TASK-001 es la que entrega valor sin reindexar.** 002, 003 y 004 cambian lo que
se escribe en el índice y exigen un reindexado antes de TASK-006.

## 4. Fuera de alcance

- **Resolver aristas a IDs de símbolo en tiempo de indexado.** Es lo correcto de
  verdad, pero no existe tabla `symbols` (los símbolos son una columna de
  `vectors`, sin `parent`) y el pipeline es por-archivo. Cambio de esquema, otro
  PLAN. No se toca hasta que TASK-006 demuestre que DD-1 no alcanza.
- **Gramáticas cargadas en runtime** (`.so` o compiladas al vuelo). Ver DD-4.
- **Agregar Kotlin u otro lenguaje nuevo.** Este PLAN baja el costo de agregarlo;
  agregarlo es otra tarea.
- **Los pendientes de PLAN-002 y del release** — push de `main`, decisión del test
  flaky, `Backend::Remote` y la procedencia, `Defaults` en `devctx-central`. Son
  otro hilo; mezclarlos obliga a tocar el árbol antes de decidir lo del flaky.
- **Despacho dinámico** (callbacks, reflexión, registros por string). Sigue siendo
  invisible al grafo y este PLAN no lo cambia.

## 5. Cierre
<!-- SE LLENA AL CERRAR EL PLAN -->
