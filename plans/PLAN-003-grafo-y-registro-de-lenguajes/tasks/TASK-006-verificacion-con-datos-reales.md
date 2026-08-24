# TASK-006 — Verificación con datos reales sobre REVFA_BackEnd

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (medición sobre `~/revfa/REVFA_BackEnd`, rama `development`)
- **Depende de:** TASK-001, TASK-002, TASK-003, TASK-004
- **Estado:** `pending`

---

## Objetivo

Demostrar con números que el grafo responde donde antes devolvía vacío, y que
nada de lo que ya funcionaba se rompió. Sin esto el PLAN no cierra.

## Contexto verificado (2026-08-23)

Línea base tomada **antes** de tocar nada, con el binario instalado
(`~/.local/bin/devctx`), sobre REVFA_BackEnd rama `development`:

| Consulta | callers | callees |
|---|---|---|
| `actualizar` | 0 | 0 |
| `OficinaService.actualizar` | 1 | 23 |
| `crearNotificacion` | 8 | — |

En los 23 callees de `OficinaService.actualizar` hay al menos 4 targets basura
(con paréntesis o multilínea) y `getNombre`/`getTelefono`/`getCodigo` duplicados
en forma pelada y calificada.

> **Recordatorio de `~/.claude/CLAUDE.md` §1: no se buildea ni se corren tests sin
> pedido explícito del usuario.** Esta task requiere compilar, instalar y
> reindexar — **pedirlo antes de ejecutarla**. Si no se autoriza, se marca
> `blocked` y se declara qué quedó sin verificar.

## Archivos

- **Crear:** `plans/PLAN-003-grafo-y-registro-de-lenguajes/VERIFICACION.md` con la tabla antes/después.

## Pasos

- [ ] **Paso 0 — Pedir autorización** para compilar, instalar y reindexar.
- [ ] **Paso 1 — Solo TASK-001, sin reindexar.** Compilar, instalar, y correr
      `devctx impact actualizar`. Es la prueba de que la corrección de consulta
      funciona **sobre el índice viejo**. Anotar callers/callees y a cuántas
      declaraciones expandió.
- [ ] **Paso 2 — Tiempo de `impact`** antes y después del `LIKE`, sobre el mismo
      símbolo. Es el riesgo declarado en TASK-001.
- [ ] **Paso 3 — Reindexar** REVFA_BackEnd/`development` con TASK-002+003+004
      dentro. Anotar conteo total de aristas antes y después.
- [ ] **Paso 4 — Basura y duplicados.** Confirmar que ningún callee de
      `OficinaService.actualizar` tiene `(`, `)`, `<`, espacio o salto de línea,
      y ver si `getNombre` sigue duplicado.
- [ ] **Paso 5 — Constructores.** Nombrar un constructor Java concreto de
      REVFA_BackEnd que ahora aparezca como `source`.
- [ ] **Paso 6 — Muestra ciega.** Elegir **10 métodos Java al azar** —no los tres
      de la línea base— y para cada uno comparar el conteo de `get_references`
      contra un `grep -c` de sus sitios de llamada. Reportar la tabla **completa,
      incluidos los que no coincidan**.
- [ ] **Paso 7 — No-regresión políglota.** Reindexar DevCtxEngine (Rust) y un
      repo Python; conteo de símbolos y aristas antes/después. No deben moverse.
- [ ] **Paso 8 — Escribir `VERIFICACION.md`** y el `## 5. Cierre` del master,
      diciendo también **qué no se verificó**.

## Criterios de aceptación

- [ ] `devctx impact actualizar` devuelve callers y callees no vacíos, y la
      diferencia con la línea base está escrita.
- [ ] La tabla del Paso 6 está completa. **Un resultado con discrepancias y
      reportado vale; uno "100%" sin la tabla, no.**
- [ ] Rust y Python no cambiaron sus conteos (o el cambio está explicado).
- [ ] `VERIFICACION.md` existe y dice qué quedó sin probar.

## Riesgos

**Medir mal y creerse el número.** Ya pasó en este proyecto: un `grep` que exigía
un punto perdió las llamadas intra-clase y produjo un "160% de cobertura" que era
falso. El Paso 6 se hace con un patrón que **acepte** la llamada sin receptor, y
si el conteo no cuadra se reporta el desacuerdo en vez de ajustar el patrón hasta
que cuadre.

**El índice de REVFA_BackEnd es el que usa el usuario a diario.** Reindexar es
seguro, pero avisar antes.

## Resultado
<!-- SE LLENA AL CERRAR -->
