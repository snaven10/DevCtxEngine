# Ejemplo: refactorizar con seguridad

> 🇬🇧 [Read in English](../../05-examples/refactoring.md)

**Escenario:** `UserService.getUser()` devuelve el objeto de usuario completo en
todas partes, y tres endpoints solo necesitan el id y el email. Querés
separarlo. Se llama desde lugares que no leíste.

---

## La regla

**Corré `impact_analysis` antes de cambiar cualquier símbolo público.** No
después, no "si parece riesgoso". Antes.

El tamaño de un diff no tiene relación con el tamaño de su radio de impacto. Un
cambio de firma de una línea puede alcanzar cuarenta sitios de llamada; una
reescritura de doscientas líneas de un helper privado puede no alcanzar ninguno.

## Paso 1 — Mapeá el radio de impacto

```
impact_analysis("getUser")
```

Dos direcciones:

- **Llamadores (aguas arriba)** — todo lo que se podría romper. Este es el
  trabajo.
- **Llamados (aguas abajo)** — de qué depende `getUser`. Esto es lo que
  condiciona cómo podés separarlo.

### Leelo como superconjunto, no como prueba

Tres límites, todos los cuales sub-reportan o sobre-reportan de formas que acá
importan:

| Límite | Consecuencia para vos |
|---|---|
| La resolución es por nombre | El `getUser()` de otro tipo puede aparecer como el mismo nodo. Sobre-reporta. |
| El despacho dinámico no deja arista | Las llamadas por callbacks, reflexión o registros llaveados por strings son **invisibles**. Sub-reporta. |
| Solo 7 lenguajes producen aristas | En un repositorio políglota, parte no está en el grafo. Sub-reporta, en silencio. |

Los casos de sub-reporte son los peligrosos. Cruzá con una búsqueda por palabra
clave antes de confiar en un reporte limpio:

```bash
devctx search "getUser" --keyword
```

La búsqueda por palabra clave encuentra la cadena en archivos que el grafo nunca
parseó — plantillas, configuración, otro lenguaje. Este es exactamente el caso
para el que existe BM25.

## Paso 2 — Confirmá cada sitio de llamada

```
get_references("getUser")
```

Te da archivo y línea de cada llamada. Ahora leelas. `impact_analysis` te dice
*hasta dónde* llega el cambio; `get_references` te dice *qué mirar*.

## Paso 3 — Averiguá por qué está así

Antes de mejorar el diseño, verificá si el diseño es deliberado:

```
memories_by_symbol("getUser")
```

Este es el paso que previene la clase más cara de refactor: deshacer una
decisión que alguien tomó por una razón que ya no se ve. "Devuelve el objeto
completo porque el ORM carga perezoso y tres consultas parciales salían más
lentas que una completa" es exactamente el tipo de cosa que vive en una memoria
y en ningún otro lado.

Ponderá los resultados por `link_sources`: `files-field` y `content-mention` son
conexiones deliberadas, `inference` es una coincidencia de palabras.

## Paso 4 — Revisá la superficie

Si es alcanzable por HTTP, el radio de impacto incluye clientes que no ves:

```
routes_for_handler("getUser")
```

Un refactor interno que cambia la forma de una respuesta no es interno.

## Paso 5 — Refactorizá

Ahora sí podés trabajar. Sabés cada llamador, por qué existe la forma actual, y
si el cambio es visible desde afuera.

## Paso 6 — Verificá contra el índice nuevo

Re-indexá, después confirmá que el símbolo viejo realmente desapareció:

```bash
devctx index
devctx search "getUser" --keyword
```

El índice refleja el árbol de trabajo, así que esto toma tu cambio sin commitear
de inmediato. Un resultado que sobrevive es un sitio de llamada que se te
escapó — normalmente en un archivo que el grafo nunca parseó, que es la razón de
que esta verificación sea por palabra clave y no semántica.

## Paso 7 — Registrá la decisión

```bash
devctx remember "Separé getUser en getUser y getUserSummary. El objeto completo
se cargaba para tres endpoints que solo necesitaban id y email, y la carga
perezosa del ORM hacía de eso una segunda consulta por cada acceso a un campo.
Mantuve getUser en vez de cambiar su forma porque dos clientes externos dependen
de la respuesta." \
  --type decision \
  --topic user-service-getuser-split \
  --files src/services/user.rs,src/api/handlers/user.rs
```

Registrá la **alternativa rechazada** — "mantuve getUser en vez de cambiar su
forma, porque dos clientes externos dependen de ella". Dentro de seis meses,
alguien va a mirar ese par aparentemente redundante y va a querer fusionarlo de
nuevo. Esa oración es lo que lo detiene.

## El flujo

```
impact_analysis          → ¿hasta dónde llega esto?
search --keyword         → lo que el grafo no pudo ver
get_references           → los sitios de llamada exactos
memories_by_symbol       → ¿el diseño actual es deliberado?
routes_for_handler       → ¿es visible desde afuera?
[refactorizar]
index && search --keyword → ¿se me escapó algo?
remember --files --topic  → la decisión y la alternativa rechazada
```

## Qué sale mal sin esto

**Perder un llamador en un lenguaje que el grafo no parsea.** Compila bien, se
rompe en ejecución, y el reporte de impacto decía que el cambio estaba limpio.

**Deshacer una decisión deliberada.** El código parecía redundante. Era crítico
por una razón que nadie escribió — hasta ahora.

**Cambiar la forma de una respuesta pública.** Ningún llamador interno se rompió,
así que pareció seguro. Los clientes no estaban en el repositorio.
