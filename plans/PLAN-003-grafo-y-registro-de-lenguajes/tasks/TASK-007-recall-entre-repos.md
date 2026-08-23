# TASK-007 — El recall entre repositorios devolvía cero, en dos capas distintas

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/grafo-y-registro-de-lenguajes`
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Que `recall` devuelva las memorias locales que existen: las del propio
repositorio, y las de los repositorios hermanos del grupo.

## Contexto verificado (2026-08-23)

Apareció buscando si alguna memoria vieja repetía la afirmación falsa de
TASK-005. El `recall` devolvió los 11 proyectos REVFA en `failed_projects`.

**Son dos bugs encadenados, ninguno de PLAN-003.**

### Bug 1 — el fan-out invoca un flag que no existe

`crates/devctx-mcp/src/state.rs:1433`, `recall_one_local` lanza el binario con:

```
recall <query> --limit N --scope local --format json
```

y el subcomando `recall` **no tiene `--format`**. Reproducido a mano:

```
$ devctx recall "prueba" --limit 3 --scope local --format json
error: unexpected argument '--format' found
```

Falla igual con el binario anterior (`devctx.bak-pre-plan003`), y `git log -S`
lo ubica en `16c5acb`. **El fan-out de grupo nunca funcionó**: todo `recall` con
scope `all` o `group` respondía solo con memorias globales, y los 11 hermanos
caían en `failed_projects` — un campo que casi nadie lee.

### Bug 2 — el CLI parsea una forma que el servidor no devuelve

`local_recall` (`crates/devctx-cli/src/main.rs`) hacía:

```rust
let raw = r.recall(query, limit)?;
return Ok(parsed.as_array().cloned().unwrap_or_default());
```

pero `POST /recall` → `do_recall_scoped` responde
`{"memories": [...], "omitted_for_budget": {...}}`. `as_array()` sobre un objeto
es `None`, así que **devolvía vacío siempre que hubiera un servidor corriendo**,
que es el caso normal.

Medido en REVFA_BackEnd: `memory-stats` reporta **16 memorias**, y
`recall "solicitud" --scope local` respondía `No memories.`

Y de paso, `remote::recall` no mandaba `scope`; el endpoint asume `all`. Aunque
la forma se hubiera parseado bien, habría traído los tres tiers y `cmd_recall`
los habría fusionado por segunda vez con los suyos.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs` — flag `--format` en `Recall`, `memories_of`, `local_recall`
- **Modificar:** `crates/devctx-cli/src/remote.rs` — `scope: "local"` explícito

## Pasos

- [x] **Paso 1 — `--format` en el subcomando `Recall`.** Reutilizar el
      `OutputFormat` que ya existe (`Table`/`Json`), no inventar otro enum.
- [x] **Paso 2 — Salida JSON siempre un objeto**, `{"memories": [...]}`, incluso
      vacío. El `No memories.` de antes habría llegado al fan-out como error de
      parseo y se habría reportado como "ese miembro está roto".
- [x] **Paso 3 — `memories_of`**: acepta el objeto o un array pelado. Ninguna de
      las dos formas se asume — este mismo supuesto fue el bug.
- [x] **Paso 4 — `scope: "local"` explícito** en `remote::recall`.
- [x] **Paso 5 — Verificar el fan-out de grupo end-to-end** con el binario
      instalado: `failed_projects` vacío y memorias de más de un repositorio.
- [x] **Paso 6 — Commit.**

## Criterios de aceptación

- [x] `devctx recall "solicitud" --scope local` en REVFA_BackEnd devuelve
      memorias en vez de `No memories.`
- [x] `--format json` imprime un objeto válido, también sin resultados.
- [x] Un `recall` de grupo devuelve `failed_projects` vacío y trae memorias de
      más de un repositorio.

## Riesgos

**El fan-out lanza `current_exe()`**, así que un proceso `devctx mcp` viejo
sigue usando su propio binario hasta que el cliente reconecte. Verificar contra
un proceso recién arrancado, no contra el que ya estaba.

**Nadie se enteró de esto durante meses** porque el modo de falla es un campo
`failed_projects` en un JSON que se lee salteado, y porque "no hay memorias
sobre eso" es una respuesta plausible. Vale la pena preguntarse si `recall`
debería **fallar ruidosamente** cuando todos los miembros caen, en vez de
devolver un resultado parcial que se ve normal.

## Resultado

- **Estado final:** `done` (2026-08-23)

- **Resumen:** `recall` volvió a devolver memorias locales —propias y de los
  repositorios hermanos— tras arreglar dos supuestos sobre la forma de los datos
  que nadie había verificado contra la otra punta.

- **Archivos tocados:**
  - `crates/devctx-cli/src/main.rs` — flag `--format` en `Recall` (reutilizando
    el `OutputFormat` que ya existía), salida JSON siempre objeto, helper
    `memories_of`, y `local_recall` parsando la forma correcta.
  - `crates/devctx-cli/src/remote.rs` — `scope: "local"` explícito en `recall`.

- **Verificado por:**

  | Comprobación | Antes | Después |
  |---|---|---|
  | `recall "solicitud" --scope local` en REVFA_BackEnd (16 memorias) | `No memories.` | 3 memorias, con título y tipo |
  | `recall ... --format json` | `error: unexpected argument '--format'` | `{"memories":[...]}` |
  | `--format json` sin resultados | (no existía) | `{"memories":[]}` — JSON válido |
  | Fan-out de grupo, proceso `devctx mcp` recién arrancado desde `~/revfa` | 11 proyectos en `failed_projects`, 0 locales | **`failed_projects` vacío**, 8 memorias de 3 repos |

  El fan-out se probó contra un proceso MCP **nuevo**, no contra los que ya
  estaban corriendo: `recall_one_local` lanza `current_exe()`, así que un
  proceso viejo sigue usando su propio binario.

  `cargo build --tests -p devctx-cli` compila limpio.

- **Desviaciones:**
  1. **Eran dos bugs, no uno.** La task se abrió por el `--format` inexistente.
     Verificándolo apareció el segundo, más grave: `local_recall` leía
     `parsed.as_array()` sobre un objeto, así que devolvía vacío **siempre que
     hubiera un servidor corriendo**, que es el caso normal. El primero afectaba
     al fan-out de grupo; el segundo, a **todo** `recall` local.
  2. **Tercer arreglo no planeado:** `remote::recall` no mandaba `scope` y el
     endpoint asume `all`. Aun con la forma bien parseada, habría traído los
     tres tiers y `cmd_recall` los habría fusionado por segunda vez con los
     suyos.
  3. **Sin test automatizado.** Los dos bugs viven en el borde entre procesos
     (CLI ↔ servidor HTTP, MCP ↔ subproceso CLI), que es justo donde no hay
     cobertura. Se verificó a mano, de punta a punta. **Queda como deuda**, y es
     la deuda que permitió que esto durara meses.

- **Riesgos abiertos / siguiente:**
  - **3 de las 8 memorias del fan-out volvieron con `repo` vacío** — son las del
    miembro por defecto, que entran por el tier local y no pasan por el
    `map.entry("repo")` del fan-out. Cosmético, pero una memoria local sin su
    repositorio dice menos de lo que debería.
  - **El modo de falla sigue siendo silencioso.** `failed_projects` es un campo
    dentro de un JSON que se lee salteado, y "no hay memorias sobre eso" es una
    respuesta plausible. Decidir si `recall` debe fallar ruidosamente cuando
    **todos** los miembros caen, en vez de devolver un parcial que se ve normal.
  - Un test de integración que ejercite CLI ↔ servidor y MCP ↔ subproceso.
