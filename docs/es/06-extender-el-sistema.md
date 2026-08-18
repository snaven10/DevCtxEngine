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

Tres ediciones en `crates/devctx-parse/src/lang.rs`, más una dependencia.

**Agregá la gramática** a `crates/devctx-parse/Cargo.toml`:

```toml
tree-sitter-ruby = "0.23"
```

**Agregá la variante** y su gramática:

```rust
pub enum Lang {
    // ...
    Ruby,
}

pub fn grammar(self) -> Language {
    match self {
        // ...
        Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
    }
}
```

**Mapeá las extensiones:**

```rust
pub fn lang_for_extension(ext: &str) -> Option<Lang> {
    Some(match ext {
        // ...
        "rb" => Lang::Ruby,
        _ => return None,
    })
}
```

**Escribí las consultas.** Este es el trabajo real. Cada lenguaje necesita:

| Consulta | Propósito | Obligatoria |
|---|---|---|
| `symbol_query` | Captura declaraciones como `@function`, `@method`, `@class`, `@struct`, `@enum`, `@interface`, `@type` | Sí |
| `calls_query` | Captura expresiones de llamada — estas se vuelven las aristas del grafo | Sí |
| `import_query` | Imports, usados para resolver destinos de llamada | Sí |
| `type_bindings_query` | Variable → tipo, afina la resolución de llamadas a métodos | Opcional (`None`) |

Los nombres de captura importan: el chunker y el grafo se guían por ellos.

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
   mucho más eficiente por texto con lote de 32 que con 1.

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
