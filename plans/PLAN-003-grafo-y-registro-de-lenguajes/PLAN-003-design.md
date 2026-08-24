# PLAN-003 — Diseño técnico

**Fecha:** 2026-08-23
**Master:** [`PLAN-003-grafo-y-registro-de-lenguajes.md`](./PLAN-003-grafo-y-registro-de-lenguajes.md)

---

## 1. El defecto, medido

`crates/devctx-parse/src/parser.rs` tiene dos funciones que no comparten
convención de nombre:

| Función | Línea | Qué produce |
|---|---|---|
| `qualified_source` | `parser.rs:296` | **siempre** `Clase.metodo` cuando hay contenedor |
| `qualified_target` | `parser.rs:312` | `Tipo.metodo` **solo si** resuelve el receptor; si no, el nombre pelado |

Y `crates/devctx-store/src/graph.rs` consulta por igualdad exacta:

```sql
SELECT DISTINCT source FROM graph_edges WHERE repo=? AND branch=? AND target=?   -- get_callers
SELECT DISTINCT target FROM graph_edges WHERE repo=? AND branch=? AND source=?   -- get_callees
```

### Consecuencia en Java

En Java **todo método vive en una clase**, así que el `source` de toda arista
Java está calificado sin excepción. Por lo tanto `get_callees("nombrePelado")`
**no puede coincidir con nada, nunca**. No es intermitente: es 100% de fallo
por construcción.

Del lado de `get_callers` depende del sitio de llamada:

```java
oficinaService.actualizar(id, request, usuario)  // campo tipado → "OficinaService.actualizar"
return crearNotificacion(dto);                   // intra-clase, sin this. → "crearNotificacion"
```

### Medición (2026-08-23, REVFA_BackEnd, rama `development`)

```
devctx impact actualizar                 → 0 callers, 0 callees
devctx impact OficinaService.actualizar  → 1 caller, 23 callees
devctx impact crearNotificacion          → 8 callers directos (todas llamadas intra-clase sin receptor)
```

**Las aristas siempre estuvieron ahí.** Lo que falla es la llave de búsqueda.

### Lo que esto retracta

Se documentó en 6 páginas, en `~/.claude/protocols/devctx-memory.md` y en
`~/.claude/CLAUDE.md` que *"la cobertura es binaria por símbolo y nada los
distingue de antemano"*. **Es falso.** Sí hay algo que los distingue, es
determinista, y se enuncia en una línea: si el sitio de llamada tenía receptor
con tipo resoluble, el target quedó calificado. Ver TASK-005.

---

## 2. Decisiones de diseño

### DD-1 — Resolver el nombre en la consulta, no reescribir el índice

Alternativas evaluadas:

| Opción | Costo | Reindexado | Descartada porque |
|---|---|---|---|
| **A. Expandir el símbolo a sus formas calificadas al consultar** | ~30 líneas en `graph.rs` | **no** | — **elegida** |
| B. Guardar target pelado *y* calificado (dos filas) | esquema + doble volumen | sí | duplica aristas y rompe `UNIQUE` |
| C. Resolver aristas a IDs de símbolo en indexado | pipeline cross-file + tabla `symbols` nueva | sí | **no existe tabla `symbols`**: los símbolos viven como columna `symbol` en `vectors` y ni siquiera guardan `parent`. Es un cambio de esquema, no un fix |

**A** funciona porque `graph_edges.source` ya contiene las formas calificadas.
El propio grafo es su índice de nombres:

```sql
SELECT DISTINCT source FROM graph_edges
 WHERE repo=? AND branch=? AND (source = ? OR source LIKE '%.' || ?)
```

**Regla de desempate:** si el usuario pasa un nombre **ya calificado**
(contiene `.`), se busca exacto y no se expande. La expansión aplica solo al
nombre pelado. Así `OficinaService.actualizar` sigue significando una cosa sola.

**El colapso de homónimos se reporta, no se esconde.** Si `actualizar` expande
a 7 declaraciones, la salida lo dice. Un grafo que fusiona 7 métodos distintos
en un nodo sin avisar es peor que uno vacío.

### DD-2 — El receptor se usa solo si es un identificador limpio

`receiver_of` (`parser.rs:339`) toma el texto crudo del nodo `object`. En una
cadena fluida —que en Quarkus reactivo es la norma— eso es **toda la expresión
anterior, saltos de línea incluidos**. Targets reales medidos hoy:

```
Oficina.findByCodigo(codigo).flatMap
Oficina
        .persist(oficina).replaceWith(
            () -> OficinaDTO.from(oficina)).invoke
```

Regla: el receptor se acepta como calificador **solo** si su texto es un
identificador simple o una cadena punteada de identificadores (`self.campo`,
`this.repo`). Cualquier otra cosa → se cae al nombre pelado, que es la
respuesta honesta.

Efecto colateral bueno: hoy `getNombre` existe como nodo pelado **y** como
`OficinaRequestDTO.getNombre`. Esto reduce la duplicación de nodos.

**Requiere reindexar** — cambia lo que se escribe en `graph_edges`.

### DD-3 — `function_kinds` / `container_kinds` pasan a ser por lenguaje

Hoy son dos `const` globales (`lang.rs:177` y `:186`) que comparten los 7
lenguajes. Por eso falta `constructor_declaration`: agregarlo a la lista global
se lo agrega también a Python y a Rust, donde no significa nada.

Consecuencia actual, evidente en código: una llamada dentro de un constructor
Java sube buscando un `FUNCTION_KINDS`, no encuentra ninguno, `qualified_source`
devuelve `None` y **la arista se descarta entera**. En Quarkus con inyección por
constructor eso no es un caso de borde.

### DD-4 — Registro de lenguajes en JSON embebido en el binario

Los JSON son **fuente del repo**, incrustados con `include_str!` al compilar.
No se leen archivos en runtime y no hay override de usuario.

Lo que **no** puede salir del binario: la gramática. Las 6 gramáticas entran
como crates de C compilado (`tree-sitter-java = "0.23"`, …) y `Lang::grammar()`
devuelve un `extern "C"` linkeado en build time. Sacarla exigiría o `libloading`
sobre un ABI inestable, o `tree-sitter-loader` con un compilador de C en la
máquina del usuario. Ninguna de las dos entra acá.

Queda entonces una tabla compilada de ~10 líneas `"java" → tree_sitter_java::LANGUAGE`,
y **todo lo demás es datos**:

```json
{
  "name": "java",
  "grammar": "java",
  "extensions": ["java"],
  "symbols":  "(class_declaration name: (identifier) @class) ...",
  "calls":    "(method_invocation name: (identifier) @callee)",
  "types":    "(field_declaration ...)",
  "imports":  "(import_declaration) @import",
  "function_kinds":  ["method_declaration", "constructor_declaration"],
  "container_kinds": ["class_declaration", "interface_declaration", "enum_declaration"]
}
```

**Costo de agregar un lenguaje:** de 9 ediciones regadas en 6 funciones a
1 archivo JSON + 1 dep en `Cargo.toml` + 1 línea en la tabla de gramáticas.
Y la definición de un lenguaje se lee de corrido en vez de saltar por el archivo.

**Lo que se paga:** una query mal escrita deja de ser error de compilación y
pasa a ser error de carga. Se acota así:

- `Query::new` de tree-sitter **rechaza nombres de nodo inexistentes** en la
  gramática — la validación es real, no cosmética.
- Un test recorre los 7 JSON y compila **cada** query contra **su** gramática.
  El CI atrapa lo mismo que hoy atrapa el compilador, antes de cualquier release.

**Lo que la validación NO atrapa:** una query sintácticamente válida que no
matchea nada. Ese es exactamente el modo de falla silencioso que nos mordió hoy,
y contra eso lo único que sirve es TASK-006.

---

## 3. Orden y por qué

```
TASK-001 (consulta)      → independiente, arregla HOY sin reindexar
TASK-002 (receptor)      → parse; requiere reindexar
TASK-003 (registro JSON) → parse; despues de 002 para no chocar en el mismo crate
TASK-004 (constructor)   → trivial una vez que 003 dio kinds por lenguaje
TASK-005 (docs)          → depende de 001 para redactar la verdad, no la retractación a medias
TASK-006 (verify)        → al final, con datos reales
```
