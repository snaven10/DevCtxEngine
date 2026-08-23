# TASK-001 — Expandir el nombre pelado a sus formas calificadas al consultar el grafo

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/grafo-y-registro-de-lenguajes`
- **Depende de:** —
- **Estado:** `done`

---

## Objetivo

Que `impact_analysis("actualizar")`, `get_references("actualizar")` y
`get_callers/callees` devuelvan las aristas que ya existen en `graph_edges` bajo
la forma `Clase.actualizar`, en vez de vacío. Sin reindexar y sin cambiar el
esquema.

## Contexto verificado (2026-08-23)

- `crates/devctx-store/src/graph.rs` consulta por igualdad exacta en cuatro
  lugares: `get_callers` (`WHERE target = ?`), `get_callees` (`WHERE source = ?`),
  `find_references` (`WHERE target = ?`) y `bfs` (que alimenta `impact_analysis`).
- `crates/devctx-parse/src/parser.rs:296` — `qualified_source` devuelve
  **siempre** `Clase.metodo` cuando hay contenedor. En Java todo método está en
  una clase, así que `get_callees` por nombre pelado no puede coincidir nunca.
- `crates/devctx-parse/src/parser.rs:312` — `qualified_target` califica solo si
  resuelve el receptor; si no, deja el nombre pelado.
- Medido contra REVFA_BackEnd/`development`: `actualizar` → 0/0;
  `OficinaService.actualizar` → 1 caller y 23 callees.
- El esquema (`crates/devctx-store/src/schema.rs:94`) tiene
  `idx_edges_source` e `idx_edges_target` sobre `(repo, branch, <col>)`.
  **Un `LIKE '%.' || ?` no usa esos índices** — ver Riesgos.

## Archivos

- **Modificar:** `crates/devctx-store/src/graph.rs`
- **Modificar:** `crates/devctx-cli/src/main.rs` (salida del comando `impact`, para reportar la expansión)
- **Modificar:** `crates/devctx-mcp/src/lib.rs` (mismo reporte en la salida JSON de `impact_analysis` / `get_references`)

## Pasos

- [x] **Paso 1 — Escribir el test que falla.** En `graph.rs`, con aristas
      sembradas `A.foo → B.bar` y `C.bar → D.baz`: `get_callers("bar")` debe
      devolver `A.foo`, y `get_callees("bar")` debe devolver `D.baz`.
      Hoy ambos devuelven vacío.
- [x] **Paso 2 — Añadir `resolve_symbol(repo, branch, name) -> Vec<String>`.**
      Si `name` contiene `.`, devolver `vec![name]` sin expandir. Si no,
      devolver las formas distintas que aparezcan como `source` **o** como
      `target` cumpliendo `= name OR LIKE '%.' || name`, ordenadas.
- [x] **Paso 3 — Enrutar las cuatro consultas por `resolve_symbol`.**
      `get_callers`, `get_callees`, `find_references` y el arranque de `bfs`
      pasan a aceptar el conjunto expandido (`IN (…)`) en vez de un solo valor.
      El interior de `bfs` **no** se expande: los nombres que ya salieron del
      grafo se usan tal cual.
- [x] **Paso 4 — Reportar la expansión.** Cuando un nombre pelado expandió a más
      de una declaración, la salida lo dice con los nombres. En el CLI como línea
      previa al reporte; en MCP como campo `resolved_symbols` del JSON.
- [x] **Paso 5 — Test de no-regresión de calificado.** `get_callers("B.bar")`
      no debe traer las aristas de `C.bar`.
- [x] **Paso 6 — Commit.** `fix(graph): resolve a bare symbol to its qualified forms`

## Criterios de aceptación

- [x] Con el índice **actual** de REVFA_BackEnd/`development`, sin reindexar:
      `devctx impact actualizar` devuelve callers y callees no vacíos.
- [x] Esa misma salida nombra a cuántas declaraciones expandió `actualizar`.
- [x] `devctx impact OficinaService.actualizar` devuelve **lo mismo que hoy**
      (1 caller, 23 callees): un nombre calificado no expande.
- [x] `devctx impact crearNotificacion` sigue devolviendo sus 8 callers directos.
- [x] Los tests de `graph.rs` pasan, incluido el de no-regresión del Paso 5.

## Riesgos

**El `LIKE '%.' || ?` no puede usar `idx_edges_source`/`idx_edges_target`** —
el comodín va al principio. Es un scan del subconjunto `(repo, branch)`. Para los
tamaños actuales (un repo, una rama) es aceptable; si `graph_edges` creciera a
millones de filas habría que materializar una columna `target_bare`. **Medir el
tiempo de `impact` sobre REVFA_BackEnd antes y después y anotarlo en `## Resultado`.**

**Colapso de homónimos.** `actualizar` tiene muchas declaraciones distintas en
REVFA_BackEnd. Expandir las funde en un solo reporte. Por eso el Paso 4 no es
cosmético: sin él, el usuario lee un radio de impacto que mezcla siete servicios
y no tiene forma de saberlo.

## Resultado

- **Estado final:** `done` (2026-08-23)

- **Resumen:** un nombre pelado se expande a cada forma calificada que lo lleva,
  usando el propio `graph_edges` como índice de nombres. La expansión aplica a la
  pregunta, nunca al recorrido. Sin cambio de esquema y sin reindexar.

- **Archivos tocados:**
  - `crates/devctx-store/src/graph.rs` — `resolve_symbol`, `neighbours_of`, reescritura de `get_callers`/`get_callees`/`find_references`/`bfs`, borrado del helper `distinct`, 6 tests nuevos.
  - `crates/devctx-mcp/src/state.rs` — `merged_declarations`, y el campo `resolved_symbols` en `do_impact` y `do_references`.
  - `crates/devctx-cli/src/main.rs` — `print_expansion`, en la rama local y en la remota.

- **Verificado por:**
  - `cargo test --offline -p devctx-store --lib` → **52 pasan, 0 fallan**.
  - Binario release instalado en `~/.local/bin/devctx` (respaldo en `devctx.bak-pre-plan003`), servidor de REVFA_BackEnd reiniciado, **sin reindexar**:

    | Consulta | Antes | Después |
    |---|---|---|
    | `impact actualizar` | 0 callers, 0 callees | **21 declaraciones**, callers y callees poblados |
    | `impact OficinaService.actualizar` | 1 caller, 23 callees | **idéntico** — un nombre calificado no expande |
    | `impact crearNotificacion` | 8 callers | 2 declaraciones, más callers |
    | `get_references crearNotificacion` | (array pelado) | 12 referencias + `resolved_symbols` |

- **Medición del riesgo declarado (el scan de `ends_with`).** Aislado en cuatro corridas de 3:

    | Caso | Tiempo |
    |---|---|
    | Piso del round-trip (calificado inexistente, sin scan, sin BFS) | ~105 ms |
    | Pelado inexistente: **solo el scan** | ~233 ms |
    | Calificado real, BFS de 1 semilla | ~420 ms |
    | Pelado real, BFS de 21 semillas | ~2270 ms |

  **El scan cuesta ~130 ms, un 6% del total.** El 5x entre pelado y calificado
  **no es el scan**: es el BFS recorriendo 21 semillas en vez de una, que es
  trabajo pedido. La columna materializada `target_bare` que anticipaba la
  sección de Riesgos **no hace falta hoy** — y si algún día hiciera falta, el
  número a vigilar es ese 130 ms, no el total.

- **Desviaciones:**
  1. **`ends_with(source, '.' || ?)` en vez de `LIKE '%.' || ?`.** En `LIKE` el
     guión bajo es comodín de un carácter y los identificadores están llenos de
     guiones bajos: `find_by_id` habría matcheado también `findXbyXid`.
     `ends_with` existe en DuckDB 1.x — verificado al correr los tests.
  2. **`do_references` cambió de forma.** Devolvía un array JSON pelado, sin
     lugar donde reportar la expansión. Ahora devuelve
     `{ symbol, resolved_symbols?, references[] }`. Afecta a
     `GET /references/:symbol`. Nadie lo parsea estructuralmente dentro del repo
     — se pasa como string al modelo — pero es un cambio de contrato.
  3. **Semillas reportables.** El plan decía "el interior de `bfs` no se expande",
     y así quedó; pero la primera versión metía las semillas en `visited` antes
     de arrancar, y eso **borraba la arista por la que existe el fix**:
     en `OficinaResource.actualizar → OficinaService.actualizar` los dos extremos
     responden a `actualizar`, así que el caller quedaba suprimido por ser
     semilla. Se separó en dos conjuntos: `walked` (no recorrer dos veces) y
     `reported` (no reportar la pregunta misma). Lo atrapó el test
     `impact_seeds_from_every_form_of_a_bare_name`, que falló 1 de 11.

- **Riesgos abiertos / siguiente:**
  - **21 declaraciones de `actualizar` se funden en un radio de impacto.** La
    salida lo dice y las lista, que era el requisito, pero sigue siendo una
    respuesta que hay que leer con criterio.
  - Los targets basura de cadena fluida siguen ahí (`getNombre` pelado y
    calificado a la vez) — es TASK-002, y exige reindexar.
  - Se detuvo y relanzó el servidor de REVFA_BackEnd (pid 493405). Otra sesión
    con binding a ese proyecto puede haber quedado desconectada.
