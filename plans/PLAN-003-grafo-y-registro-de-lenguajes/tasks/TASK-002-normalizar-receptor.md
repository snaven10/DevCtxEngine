# TASK-002 — Aceptar el receptor como calificador solo si es un identificador limpio

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/grafo-y-registro-de-lenguajes`
- **Depende de:** —
- **Estado:** `pending`

---

## Objetivo

Que una llamada en cadena fluida deje de producir targets con la expresión
entera —saltos de línea y lambdas incluidos— y caiga al nombre pelado, que es la
respuesta honesta cuando el receptor no se puede nombrar.

## Contexto verificado (2026-08-23)

`crates/devctx-parse/src/parser.rs:339` — `receiver_of` toma
`parent.child_by_field_name(field)` y devuelve su `utf8_text` **crudo**. En una
cadena fluida el nodo `object` es toda la expresión previa. Targets reales que
devolvió `devctx impact OficinaService.actualizar` sobre REVFA_BackEnd:

```
Oficina.findByCodigo(codigo).flatMap
Oficina.<Oficina>findById(idOficina).flatMap
Oficina
        .persist(oficina).replaceWith(
            () -> OficinaDTO.from(oficina)).invoke
```

En Quarkus reactivo la cadena fluida es la norma, no el borde.

Segundo efecto, en la misma salida: `getNombre` aparece como nodo pelado **y**
como `OficinaRequestDTO.getNombre`. El mismo método son dos nodos.

`qualified_target` (`parser.rs:312`) ya tiene la rama correcta para caerse al
nombre pelado — solo hay que llegar a ella.

## Archivos

- **Modificar:** `crates/devctx-parse/src/parser.rs`

## Pasos

- [ ] **Paso 1 — Escribir los tests que fallan.** En `crates/devctx-parse/src/lib.rs`,
      con fuente Java:
      `Oficina.findByCodigo(c).flatMap(x -> y)` → el target de `flatMap` es
      `flatMap`, **no** `Oficina.findByCodigo(c).flatMap`.
      Y `this.repo.save(x)` → sigue resolviendo por `type_map` como hoy.
- [ ] **Paso 2 — Añadir `fn clean_receiver(text: &str) -> Option<&str>`.**
      Devuelve `Some` solo si el texto completo es un identificador o una cadena
      punteada de identificadores (`ident ( '.' ident )*`), sin espacios, sin
      saltos de línea, sin paréntesis, sin `<`. `None` en cualquier otro caso.
- [ ] **Paso 3 — Filtrar en `receiver_of`.** Pasar el texto por `clean_receiver`
      antes de devolverlo. Un receptor sucio se vuelve `None`, y `qualified_target`
      ya devuelve el nombre pelado en ese caso.
- [ ] **Paso 4 — Test de no-regresión.** Los casos que hoy pasan (`self.x`,
      `this.campo`, `Tipo` en mayúscula, campo con tipo en `type_map`) siguen
      calificando igual.
- [ ] **Paso 5 — Commit.** `fix(parse): ignore a fluent-chain receiver when qualifying a call target`

## Criterios de aceptación

- [ ] Ningún target contiene `(`, `)`, `<`, un espacio o un salto de línea.
- [ ] Los tests existentes de `devctx-parse` siguen verdes.
- [ ] Tras reindexar REVFA_BackEnd, `devctx impact OficinaService.actualizar`
      ya no lista ningún callee con paréntesis o multilínea.
- [ ] `getNombre` deja de aparecer duplicado como pelado y calificado en esa
      misma salida. *(Si sigue duplicado, decirlo en `## Resultado` con el porqué —
      puede haber sitios de llamada genuinamente sin receptor.)*

## Riesgos

**Requiere reindexar** para que se vea: cambia lo que se escribe en `graph_edges`,
no cómo se lee.

**Pérdida deliberada de precisión.** Un receptor como `getServicio().actualizar(x)`
hoy produce basura; después producirá `actualizar` pelado. Es menos específico,
pero es **cierto**, y con TASK-001 el nombre pelado ya encuentra sus aristas.

## Resultado
<!-- SE LLENA AL CERRAR -->
