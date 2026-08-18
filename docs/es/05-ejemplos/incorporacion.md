# Ejemplo: incorporarse a un código desconocido

> 🇬🇧 [Read in English](../../05-examples/onboarding.md)

Tu primer día en un repositorio que nadie tiene tiempo de explicarte.

---

## Paso 0 — Indexalo

```bash
cd el-repositorio
devctx init
devctx index --full
```

`init` te pregunta por el modelo de embeddings. **Respondé con cuidado** — es la
única decisión que no se puede cambiar después sin re-indexar todo y re-embeber
cada memoria. Si el código o sus comentarios no están en inglés, elegí un modelo
multilingüe. Ver [Modelos y ajuste](../09-modelos-embeddings-y-tuning.md).

Después verificá qué obtuviste:

```bash
devctx status
```

Archivos, fragmentos, símbolos, modelo, frescura. Un conteo de símbolos cercano
a cero en un repositorio grande significa que el código está en un lenguaje que
se está indexando como texto en vez de parsearse — ver la tabla de lenguajes en
[Grafo de símbolos](../03-conceptos-fundamentales/grafo-de-simbolos.md).

## Paso 1 — Encontrá los bordes del sistema

Empezá por donde el mundo exterior lo toca:

```bash
devctx routes
```

Las rutas te dicen qué *hace* el sistema mucho más rápido que cualquier árbol de
archivos. Se reconocen siete frameworks — FastAPI, Flask, Express, NestJS,
Spring, Quarkus, Angular.

Si no es un servicio web, arrancá por los puntos de entrada:

```
search("punto de entrada principal arranque de la aplicación")
```

## Paso 2 — Preguntale al sistema de qué se trata

```
search("autenticación")
search("conexión a base de datos y transacciones")
search("carga de configuración")
search("trabajos en segundo plano y agendamiento")
```

Cuatro búsquedas sobre los conceptos que todo sistema tiene van a mapear la
mayor parte. Todavía no estás leyendo — estás aprendiendo qué archivos existen y
cómo se llaman.

Fijate más en los campos **file** y **symbol** que en el código. En esta etapa
estás construyendo vocabulario, y el vocabulario es lo que hace funcionar cada
consulta posterior.

## Paso 3 — Averiguá qué sabe ya el equipo

Este es el paso que distingue incorporarse acá de incorporarse con grep:

```
memory_context()
```

Las memorias más recientes, sin consulta — para exactamente esta situación,
donde todavía no sabés lo suficiente para preguntar. Si el equipo estuvo
registrando decisiones, esta es la orientación más rápida disponible.

Después, sobre cualquier cosa que se viera importante en el paso 2:

```
memories_by_file("src/payments/processor.rs")
```

El conocimiento registrado contra ese archivo: por qué está estructurado así,
qué mordió a alguien la última vez.

## Paso 4 — Profundizá en una sola cosa

Elegí el subsistema en el que realmente vas a trabajar y pedí un brief de
verdad:

```
build_context("cómo funciona la autenticación de punta a punta", max_tokens=8000)
```

Subí el presupuesto para incorporarte. El default de 4096 está afinado para una
pregunta enfocada; vos estás haciendo una amplia, y el brief te dice con
honestidad cuándo truncó:

```
[devctx] 7 further item(s) did not fit in 8000 tokens.
```

## Paso 5 — Trazá un camino a mano

El entendimiento viene de seguir una petición de punta a punta, no de leer
resúmenes.

```
routes_for_handler("login")     → qué URL llega acá
read_symbol("login")            → el manejador mismo
impact_analysis("login")        → qué llama, transitivamente
```

Una traza te enseña más sobre las convenciones de un código que diez búsquedas.

## Paso 6 — Escribí lo que aprendiste

Tu confusión de hoy es un dato. Vence en una semana, cuando el código empiece a
sentirse normal y ya no puedas recordar qué te sorprendía.

```bash
devctx remember "La autenticación usa dos tipos de token: uno de acceso de vida
corta validado en el middleware, y uno opaco de refresco guardado del lado del
servidor. El middleware NO pega a la base de datos — eso es deliberado, por
latencia — así que un usuario revocado sigue válido hasta que expira el token de
acceso." \
  --type architecture \
  --topic auth-token-model \
  --files src/auth/middleware.rs,src/auth/tokens.rs
```

Escribí específicamente **lo que te sorprendió**. Esa es la parte que el código
no dice y la parte con la que el próximo recién llegado también va a tropezar.

## La progresión

| Fase | Pregunta | Herramienta |
|---|---|---|
| 0 | ¿Qué hay acá? | `init`, `index --full`, `status` |
| 1 | ¿Qué hace? | `routes`, `search` de puntos de entrada |
| 2 | ¿De qué está hecho? | `search` sobre conceptos universales |
| 3 | ¿Qué sabe el equipo? | `memory_context`, `memories_by_file` |
| 4 | ¿Cómo funciona mi subsistema? | `build_context` con presupuesto subido |
| 5 | ¿Cómo fluye una petición? | `routes_for_handler` → `read_symbol` → `impact_analysis` |
| 6 | ¿Qué aprendí? | `remember --files --topic` |

## Checklist del primer día

- [ ] `devctx init` — con el modelo elegido a propósito, no aceptado a ciegas
- [ ] `devctx index --full`
- [ ] `devctx status` — ¿el conteo de símbolos tiene sentido para el tamaño del
      repositorio?
- [ ] `devctx routes` — o los puntos de entrada si no es un servicio
- [ ] `memory_context` — ¿hay conocimiento previo del equipo?
- [ ] Un `build_context` sobre tu subsistema
- [ ] Una petición trazada de punta a punta
- [ ] Al menos una memoria guardada, con `--files`
- [ ] `devctx hooks install` — para que el índice se mantenga al día sin que
      tengas que pensarlo
