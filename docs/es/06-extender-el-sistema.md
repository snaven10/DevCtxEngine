# Extender el sistema

> 🇬🇧 [Read in English](../06-extending-the-system.md)

Dónde agregar código para cada tipo de extensión, qué implementar y cómo se
conecta.

No hay sistema de plugins ni carga dinámica. Agregás código en el lugar correcto
y lo registrás. Ese es todo el modelo, y es deliberado: todo se distribuye en un
solo binario, así que una extensión no puede quedar instalada a medias.

---

## El mapa de crates

Quince crates, pero solo cinco son puntos de extensión:

| Crate | Agregá acá cuando querés |
|---|---|
| `devctx-parse` | Un lenguaje nuevo, o las rutas de un framework HTTP nuevo |
| `devctx-embed` | Un proveedor de embeddings nuevo |
| `devctx-rerank` | Un reranker nuevo |
| `devctx-mcp` | Una herramienta nueva expuesta a los agentes |
| `devctx-cli` | Un comando nuevo |

El resto — `store`, `index`, `chunk`, `search`, `memory`, `summarize`, `api`,
`tui`, `central`, `core` — son consumidores de esos.

## 1. Un lenguaje nuevo

Un lenguaje es un archivo JSON bajo `crates/devctx-parse/languages/`, embebido en
el binario al compilar, más su gramática. Para agregar uno:

**Agregá la gramática** a `crates/devctx-parse/Cargo.toml`:

```toml
tree-sitter-ruby = "0.23"
```

**Conectala** en `grammar_for` (`crates/devctx-parse/src/registry.rs`), la única
tabla escrita a mano que queda, y listá el JSON en `SOURCES`, al lado:

```rust
"ruby" => tree_sitter_ruby::LANGUAGE.into(),
```

**Escribí `languages/ruby.json`.** Las extensiones viven acá, y también las
consultas — estilo `tags.scm` — que son el trabajo real:

| Clave | Captura | Obligatoria |
|---|---|---|
| `definitions` | Cada definición como `@definition.<kind>` (`function`, `method`, `class`, `field`… — los kinds de `DEFINITION_KINDS`) con su `@name` | Sí |
| `references` | Llamadas (`@reference.call` + `@name`), instanciaciones (`@reference.new` + `@type`), usos de tipo (`@reference.type`) — las aristas del grafo | Sí |
| `imports` | `@import` con `@import.path`, `.name`, `.alias`, `.wildcard`, `.default`, `.namespace` | Sí |
| `inherits` | Supertipos dentro de una definición: `@inherit.extends`, `@inherit.implements` | No |
| `types`, `scopes` | Declaraciones que tipan un receptor (`@name`, `@type`, `@init`, `@bind.field|param|local…`) y los node kinds que abren un scope | No (sin ellas, un solo scope por archivo) |
| `package`, `type_names`, `function_kinds`, `container_kinds`, `scope_kinds`, `overloads` | La cláusula de paquete; qué nombra un tipo; qué nodos son invocables, contenedores o scopes con nombre; si los tipos de parámetro entran al id | En parte |
| `platform_prefixes`, `builtins`, `platform_modules` | Lo que es de la plataforma: la evidencia de `external` | No |

`every_definition_compiles` compila cada consulta y rechaza una captura o un node
kind desconocidos, así que la CI atrapa una consulta mal formada. Lo que no puede
atrapar es una consulta válida que no encuentra nada — la falla que una vez hizo
que el grafo de llamadas pareciera vacío durante meses.

**La resolución entre archivos** es Rust, no JSON: un `LangResolver` en
`crates/devctx-parse/src/resolve/` (`resolver_for` le asigna un lenguaje). Un
lenguaje sin uno recibe solo las reglas genéricas del link pass, así que sus
aristas quedan sin decidir más seguido. Agregá también el lenguaje a
`GRAPH_LANGUAGES` (`crates/devctx-core/src/symbol_id.rs`), que es como se
detecta un grafo desfasado.

**Fijalo.** `tests/extractor_golden.rs` dibuja todo lo que el índice guarda de un
fixture por lenguaje en `tests/golden/extractor.txt`. Un cambio en lo que escribe
el extractor tiene que subir `EXTRACTOR_VERSION` (`registry.rs`) y regenerar el
golden (`DEVCTX_UPDATE_GOLDEN=1 cargo test -p devctx-parse --test
extractor_golden`): los índices existentes informan entonces `extractor_stale`
hasta un `devctx index --full`. Un cambio en las reglas de enlace sube en cambio
`LINK_VERSION` (`crates/devctx-index/src/link.rs`), y la próxima ejecución
reenlaza sin reindexar.

**Probá con un archivo real**, no con un snippet. Las gramáticas discrepan de la
intuición sobre los nombres de nodo, y una consulta que anda con un ejemplo de
juguete a menudo falla con la forma que toma el código real.

### La alternativa barata

Si solo necesitás que el lenguaje sea *buscable*, no *grafeado*, agregalo a
`raw_text_language()`:

```rust
pub fn raw_text_language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        // ...
        "rb" => "ruby",
        _ => return None,
    })
}
```

Una línea, sin gramática, sin consultas. Los archivos se fragmentan con
solapamiento y se embeben, así que la búsqueda los encuentra — simplemente no
producen símbolos ni aristas. Para configuración, marcado y plantillas esta es
la respuesta correcta, no un compromiso.

## 2. Un framework de rutas nuevo

`crates/devctx-parse/src/routes.rs`, en `extract_routes`.

Se reconocen siete frameworks: FastAPI, Flask, Express, NestJS, Spring, Quarkus
(JAX-RS) y Angular.

La extracción de rutas es **por patrones, no por AST**, que es la razón por la
que las rutas Spring de Kotlin se encuentran aunque Kotlin no tenga gramática
conectada. Esa independencia es una característica: un detector de framework
funciona para cualquier lenguaje cuyos archivos lleguen al indexador.

Dos cosas que los tests existentes protegen, y que un detector nuevo debería
manejar:

- **Los prefijos componen.** Un prefijo a nivel de controlador más una ruta a
  nivel de método son una sola ruta, y los tests de Spring y NestJS verifican
  exactamente eso.
- **Las ventanas de lookahead deben ser seguras en bytes.** Hay un test nombrado
  por los acentos sobreviviendo un lookahead de JAX-RS, porque una ventana que
  corta a mitad de UTF-8 revienta con código real y nunca con fixtures ASCII.

## 3. Un proveedor de embeddings nuevo

Implementá `EmbeddingProvider` en `crates/devctx-embed`:

```rust
pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    fn dimension(&self) -> usize;
}
```

Tres obligaciones que no están en la firma:

1. **Normalizá tus vectores.** `l2_normalize` está provisto. Coseno y producto
   interno solo ordenan igual para vectores unitarios, y un proveedor que se
   saltea esto hace que `storage.metric: ip` ordene por magnitud — en silencio y
   mal.
2. **`dimension()` tiene que poder saberse sin cargar el modelo.** Los
   servidores usan `dimension_for(provider, model)` para decidir si siquiera
   hace falta un embedder; ese camino perezoso es la razón de que indexar no
   pague por un modelo que nunca usa.
3. **Agrupá en lotes.** `embed()` recibe muchos textos porque el encoder es
   mucho más eficiente por texto en lote (el default es 8) que de a uno.

Para un modelo local incorporado, agregá un `LocalModelSpec` a `LOCAL_MODELS` en
`registry.rs` — clave, dimensión, idiomas y la nota que muestra
`devctx models`.

Si solo querés usar tu propio modelo ONNX, no necesitás nada de esto: poné
`embeddings.provider: custom` y `model_dir`. Ver
[Modelos y ajuste](09-modelos-embeddings-y-tuning.md).

## 4. Un reranker nuevo

Implementá `Reranker` en `crates/devctx-rerank`:

```rust
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, candidates: &[String], top_k: usize) -> Result<Vec<Ranked>>;
    fn name(&self) -> &str;
    fn pool(&self) -> usize;
}
```

`pool()` es el interesante. Es simultáneamente el techo de todo lo que tu
reranker podría arreglar — reordena lo que le entregan y nada más — y todo su
costo, ya que multiplica la etapa más lenta del pipeline. El reranker no-op
responde cero, que es la respuesta honesta para algo que no reordena nada.

Antes de agregar uno, leé [ADR-15](08-decisiones-de-diseno.md): los
cross-encoders medidos acá costaron dos órdenes de magnitud en latencia, y el
único evaluado contra toda la suite empeoró los resultados. Un reranker nuevo
debería venir con números.

## 5. Una herramienta MCP nueva

`crates/devctx-mcp/src/lib.rs`, dentro del bloque `#[tool_router]`:

```rust
#[tool(description = "Qué responde esto, y cuándo usarlo \
                      en vez de la herramienta vecina.")]
async fn your_tool(&self, Parameters(req): Parameters<YourReq>) -> Result<String, ErrorData> {
    let backend = self.bound()?;
    run_blocking(move || do_your_thing(&backend, &req)).await
}
```

Convenciones que son estructurales:

- **`self.bound()?`** devuelve el backend o el error de "no hay proyecto
  vinculado". Usalo salvo que la herramienta genuinamente funcione sin proyecto
  (`list_projects`, `use_project`).
- **`run_blocking`** mantiene el trabajo síncrono del store fuera del executor
  async. Las llamadas al store son bloqueantes; el runtime no.
- **Devolvé JSON**, como string. `build_context` es la excepción deliberada,
  porque su salida se lee al contexto de un modelo en vez de parsearse.
- **Poné el *cuándo* en la descripción.** El cliente elige herramientas solo por
  la descripción. "Devuelve JSON" no es una razón para llamar algo; "usá esto
  cuando sabés el nombre y querés la cosa misma, usá `search` cuando querés
  código sobre una idea" sí lo es.

La implementación va en `state.rs`, donde viven las otras funciones `do_*`.

## 6. Un comando de CLI nuevo

`crates/devctx-cli/src/main.rs`: agregá una variante de `Commands` y su
manejador, después agregalo al resumen agrupado en `help_map.rs` para que
aparezca bajo el encabezado correcto en `devctx --help`.

Ese último paso es fácil de saltear y conviene no saltearlo — un comando que
falta en la ayuda agrupada es un comando que nadie encuentra. A `impact` le pasó
exactamente eso una vez.

## Testing

```bash
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

`--offline` es obligatorio: el build vendoriza sus dependencias, DuckDB
incluido.

Dos lecciones que este código pagó, que vale la pena heredar:

**Los fixtures son más limpios que la realidad.** El indexado multi-rama tenía
22 tests verdes sobre una funcionalidad rota en los tres caminos reales, porque
los fixtures tenían un índice por rama, sin servidor y sin índice HNSW — tres
formas en las que no eran el mundo.

**Afirmá el efecto, no la forma.** Un test que afirmaba que una consulta
devolvía `Some` pasaba mientras el pipeline no copiaba nada. Reemplazarlo por un
embedder contador — que mide cuántos textos se embebieron de verdad — lo cazó al
instante.
