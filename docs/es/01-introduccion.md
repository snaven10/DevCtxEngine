> 🌐 [English version](../01-introduction.md)
> Volver al [README](README.md)

# Introducción

---

## ¿Qué es DevCtxEngine?

DevCtxEngine es un **motor de contexto para agentes de IA**. Le da a los
asistentes de código — Claude Code, Cursor, agentes propios — una comprensión
semántica y estructurada de tu codebase, en vez de obligarlos a trabajar por la
mirilla de lecturas de fichero sueltas.

No es un buscador, ni un linter, ni otro indexador al que haya que cuidar. Es la
capa entre tu código y tu agente que convierte ficheros fuente en conocimiento
navegable, consultable y persistente — y que conserva ese conocimiento entre
sesiones y entre proyectos.

**Es a los agentes de IA lo que un IDE es a las personas.** Un IDE te da búsqueda
en todo el proyecto, ir-a-definición, buscar-referencias y estado persistente del
workspace. Sin eso estás haciendo `cat` en un terminal, que es justo lo que hacen
los agentes por defecto.

---

## El problema

- **Visión de mirilla.** Los agentes ven un fichero a la vez. No pueden sostener
  un módulo en memoria de trabajo, y menos seguir una cadena de llamadas entre
  paquetes.
- **Sin conciencia estructural.** `grep` encuentra texto. No sabe que `handleAuth`
  es un método de `AuthMiddleware` al que llaman tres ficheros de rutas.
- **Amnesia.** Cada sesión empieza de cero. El agente que dedicó veinte minutos a
  entender tu flujo de autenticación ayer no recuerda nada hoy — y un agente que
  trabaja en un segundo repositorio nunca aprende lo que le enseñó el primero.
- **Desperdicio de contexto.** Sin recuperación dirigida, los agentes vuelcan
  ficheros enteros en la ventana de contexto. La mitad de los tokens se va en
  código irrelevante; lo importante se trunca.

---

## Capacidades

**Búsqueda semántica.** Pregunta en lenguaje natural y recibe código ordenado por
relevancia, troceado por fronteras de símbolo y no por ventanas arbitrarias de
líneas. Vectorial, por palabras clave (BM25) o híbrida.

**Grafo de símbolos.** Aristas de llamada e import extraídas del AST, de modo que
"qué se rompe si cambio esto" es una consulta y no una conjetura.

**Memoria persistente.** Decisiones, ideas y trampas que sobreviven a las
sesiones, deduplicadas para que guardar lo mismo dos veces no acumule ruido. Una
memoria es privada de su proyecto o **global** — compartida con todos los
proyectos de la máquina, así que una lección aprendida una vez está disponible en
todas partes.

**Conciencia entre proyectos.** Un registro de cada repositorio que has
configurado, para que un agente que trabaja en uno sepa que existen los demás,
pueda buscar en su código y recordar lo que se aprendió allí.

**Rutas de frameworks.** Rutas HTTP y sus handlers, extraídas para Spring,
Quarkus, Nest, Express y otros.

**Integración MCP.** Todo ello expuesto como herramientas que un agente puede
llamar, más una API HTTP, una interfaz de terminal y un panel web sobre el mismo
motor.

---

## Inicio rápido

### Instalar

```bash
cargo build --release
cp target/release/devctx ~/.local/bin/
```

### Preparar un repositorio

```bash
cd ~/code/miproyecto
devctx init                        # escribe .devctx/config.yaml y registra el proyecto
devctx index                       # la primera vez descarga el modelo de embeddings
```

### Buscar

```bash
devctx search "dónde validamos el token de autenticación"
devctx search "lógica de reintentos" --hybrid --limit 5
devctx impact handleAuth           # llamadores y llamados transitivos
```

### Recordar

```bash
devctx remember "las sesiones caducan a las 24h, ver auth/session.rs" --type decision
devctx remember "verifica siempre la firma de los webhooks" --scope global
devctx recall "cuánto duran las sesiones"
```

### Mantenerlo al día

```bash
devctx hooks install               # reindexa tras cada commit
devctx watch                       # o reindexa los ficheros según los guardas
```

### Conectar un agente

```bash
devctx mcp configure --client claude-code --scope project
```

---

## Cómo funciona, en treinta segundos

`devctx index` le pregunta a git qué cambió, parsea esos ficheros con tree-sitter
en símbolos y aristas de llamada, los trocea por fronteras de símbolo, embebe cada
trozo con un modelo ONNX local, y guarda vectores, grafo y hashes por fichero en
un único fichero DuckDB dentro del repositorio. Los ficheros sin cambios se saltan
por hash, así que reindexar sale lo bastante barato como para hacerlo tras cada
commit.

Una consulta embebe tu pregunta, encuentra los trozos más cercanos, opcionalmente
los reordena con un cross-encoder, y devuelve resultados con fichero, rango de
líneas y símbolo.

Como DuckDB permite un único proceso escritor, el primer comando que necesita la
base arranca un pequeño servidor que la posee; todo lo demás — otras invocaciones
del CLI, sesiones de agente, el TUI, el panel — enruta a ese servidor en vez de
pelear por el bloqueo, y el modelo se queda cargado entre llamadas.

Lo que merece la pena compartir entre proyectos — el registro y las memorias
globales — vive en un store central fuera de cualquier repositorio. Todo lo demás
se queda con su proyecto.

---

## Mapa de la documentación

| Lee esto | Para |
|---|---|
| [Arquitectura](02-arquitectura.md) | Cómo encajan las piezas y por qué |
| [Configuración](11-configuracion.md) | Los dos ficheros de config, variables de entorno, clientes MCP |
| [El store central](12-store-central.md) | Registro, memorias globales, el daemon |
| [Mantener el índice al día](13-mantener-el-indice-al-dia.md) | Hooks, watch, reindex, exclusiones |
| [Flujo de trabajo del agente](04-flujo-de-trabajo-del-agente.md) | Cómo debería usar un agente las herramientas |
| [Modelos y tuning](09-modelos-embeddings-y-tuning.md) | Elegir un modelo de embeddings |
| [Decisiones de diseño](08-decisiones-de-diseno.md) | Compromisos, con el razonamiento |
