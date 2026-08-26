# PLAN-003 — Verificación con datos reales

**Fecha:** 2026-08-24
**Sujeto:** REVFA_BackEnd, rama `development`, 1,500 archivos
**Binario:** compilado de `c36a87e`, verificado por **md5** (`5a5f1ede…`) contra
el proceso vivo — no por `--version`, que dice `0.5.0` tanto en el release
publicado como en este build.

---

## 1. Nodos del grafo que eran expresiones

`devctx impact OficinaService.actualizar`, mismo símbolo antes y después:

| | Callees | Nombres malformados |
|---|---|---|
| **Antes** | 45 | **5** |
| **Después** | 39 | **0** |

Los cinco que desaparecieron:

```
Oficina.findByCodigo(codigo).flatMap
.persist(oficina).replaceWith
Oficina.<Oficina>findById(idOficina).flatMap
.persist(oficina).replaceWith(
() -> OficinaDTO.from(oficina)).invoke
```

En su lugar quedaron `flatMap`, `replaceWith` e `invoke` — el nombre pelado, que
es la respuesta honesta cuando el receptor no se puede nombrar, y que desde
TASK-001 encuentra sus propias aristas.

La caída de 45 a 39 es la consolidación esperada: cuatro expresiones distintas
sobre `Oficina` colapsan en los nombres reales.

## 2. Muestra ciega — 10 métodos Java

Elegidos por **hash md5 del nombre** sobre 3,046 candidatos, no a dedo. El
patrón de grep **acepta la llamada sin receptor**: el que exigía un punto perdió
las llamadas intra-clase y produjo un "160% de cobertura" que era falso.

| Método | Grafo | Archivos | |
|---|---|---|---|
| inicioDia | 33 | 2 | ok |
| obtenerEstadoProcesamiento | 0 | 1 | ambos ~cero |
| findAllActiveWithJoins | 2 | 2 | ok |
| getNombreRegimenMatrimonial | 2 | 2 | ok |
| CamposNormalizadosDTO | 0 | 1 | ambos ~cero |
| searchAndMapForAdminNui | 39 | 4 | ok |
| setConfiguracionDependencia | 1 | 2 | ok |
| leerNuiIns | 1 | 1 | ok |
| seccionesSinSolicitud | 3 | 2 | ok |
| getTipoCertificacion | 3 | 2 | ok |

**Grafos vacíos con llamadas reales: 0 de 10.** Ese era el defecto.

**Las unidades no son las mismas** y el reporte no las presenta como si lo
fueran: el grafo cuenta símbolos llamadores transitivos y la columna de archivos
cuenta ficheros donde aparece la cadena. Que `inicioDia` dé 33 contra 2 no es un
error — es el radio transitivo contra dos archivos. Lo concluyente es la
ausencia de ceros con llamadas reales.

Los dos casos en cero son consistentes: una declaración con un solo archivo y
sin llamadores, y un constructor de DTO usado como tipo.

## 3. Salud del índice

```
files 1500 · chunks 17495 · symbols 11260 · up_to_date true
```

Tras un `--full` de 2h 35m. El WAL desapareció al cerrar, así que el CHECKPOINT
final corrió: la base abre limpia.

## 4. Qué NO se verificó

- ~~**Rust y Python sin reindexar.**~~ **HECHO el 2026-08-26** — ver §7.
- **Ningún constructor Java concreto identificado en el índice.** El test
  unitario prueba que ahora produce arista —fallaba con `edges: []` antes del
  cambio— pero no se nombró un constructor real de REVFA_BackEnd en el grafo.
- **El tiempo de `impact` no se volvió a medir** tras el reindexado.
- **El archivo creció de 90 MB a 1.23 GB** y no volvió a bajar con los
  checkpoints. Sin explicar. Ver riesgos.

## 5. Incidentes de la corrida

**Rompí el canal de progreso.** El `index --full` cortó con
`timed out reading response` a los 4 s. Diagnostiqué un servidor zombi —tenía
una hora de vida con `--idle 900`— y le mandé `SIGTERM`. Ese SIGTERM cerró el
listener HTTP y borró el archivo de descubrimiento: el indexado siguió otras dos
horas siendo **invisible** para la barra, para `devctx status` y para el MCP.

Verifiqué que estaba trabajando (CPU y WAL) **después** de mandar la señal. El
orden correcto era al revés, y por eso el usuario perdió la visibilidad.

**Estimé mal el tiempo restante.** Dije "15 a 45 minutos" comparando la
velocidad de escritura del ciclo 2 contra la del ciclo 1 — pero el ciclo 1
incluía cargar el modelo de embeddings. Comparé una fase con arranque contra una
sin arranque y leí la diferencia como progreso. Tardó una hora más.

**El verificador salió mal a la primera**, otra vez: contó 44 nombres
malformados sobre 42 callees, porque el patrón `^ {8,}` matchea la indentación
normal. Los números de arriba salen de la segunda versión, sobre los nombres ya
extraídos.

## 6. Riesgos abiertos

- **`index.duckdb` pasó de 90 MB a 1.23 GB** y no bajó tras tres checkpoints.
  Puede ser espacio preasignado que DuckDB reusa; **no está comprobado**.
- **`devctx repair` no puede reparar el caso para el que existe** (abre la base,
  y abrirla es lo que replaya el WAL y falla). Hay un
  `index.duckdb.wal.CORRUPTO-1907` en este mismo directorio, dejado por otra
  sesión: no es un caso aislado.
- **`--idle 900` no mata los servidores.** Confirmado con uno de una hora.
- **El CLI reporta un timeout como si el indexado hubiera muerto**, cuando el
  servidor sigue trabajando. Miente en la dirección cara: invita a reintentar o
  a matar el proceso.


---

## 7. Paso 7 — Rust y Python, medido (2026-08-26)

El punto que quedó abierto arriba. El refactor movió los siete lenguajes a JSON,
y los tests unitarios sólo prueban que las queries compilan y matchean en casos
pequeños; que el comportamiento sobre código real no cambiara era otra cosa.

### Python — la comparación que vale

`REVFAConversorPlantilla` tenía su índice de hacía días, construido con el parser
**anterior**. Un `--full` lo rehizo entero con el nuevo:

| | Archivos | Chunks | Símbolos |
|---|---|---|---|
| Antes (parser viejo) | 21 | 177 | 117 |
| Después (parser nuevo) | 21 | 177 | **117** |

Idéntico. El traslado de las queries de Python al JSON no perdió ni agregó una
sola captura.

### Rust — medido contra el binario anterior

El índice de DevCtxEngine ya se había rehecho con el parser nuevo vía el hook, así
que no servía de "antes". Se comparó directamente el binario **0.4.1**
(`~/.local/bin/devctx.bak-pre-plan003`, previo al refactor) contra el **0.6.0**,
sobre los mismos dos archivos `.rs`:

| Binario | Chunks | Símbolos |
|---|---|---|
| 0.4.1, pre-refactor | 80 | 49 |
| 0.6.0, post-refactor | 80 | **49** |

Idéntico también. **No hay regresión de parser en Rust.**

### Lo que sí apareció, y no es del parser

Reindexar DevCtxEngine con `--full` dio **1,580** símbolos donde el índice
incremental decía **1,732** — mismo commit (`d81f196`), mismo código, 152 de
diferencia. Los chunks no se movieron (2,529 en ambos).

Como el parser produce lo mismo en ambas versiones (arriba), esto **no es del
refactor**: es que un índice construido incrementalmente y uno construido de una
vez no coinciden. El repositorio tiene cinco ramas indexadas y el `--full`
reportó `files_copied: 161`, así que la sospecha es que el incremental deja filas
de versiones anteriores, o que el conteo cruza ramas.

**Sin diagnosticar, a propósito.** Hay un dato duro —incremental 1,732 contra
full 1,580 sobre el mismo commit— y ninguna explicación verificada. Escribir un
arreglo sobre la sospecha es exactamente lo que costó un día con el test flaky.