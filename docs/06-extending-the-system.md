# Extending the System

> 🇪🇸 [Leer en español](es/06-extender-el-sistema.md)

Where to add code for each kind of extension, what to implement, and how it gets
wired in.

There is no plugin system and no dynamic loading. You add code in the right
place and register it. That is the whole model, and it is deliberate: everything
ships in one binary, so an extension cannot be half-installed.

---

## The crate map

Fifteen crates, but only five are extension points:

| Crate | Add here when you want |
|---|---|
| `devctx-parse` | A new language, or a new HTTP framework's routes |
| `devctx-embed` | A new embedding provider |
| `devctx-rerank` | A new reranker |
| `devctx-mcp` | A new tool exposed to agents |
| `devctx-cli` | A new command |

The rest — `store`, `index`, `chunk`, `search`, `memory`, `summarize`, `api`,
`tui`, `central`, `core` — are consumers of those.

## 1. A new language

Three edits in `crates/devctx-parse/src/lang.rs`, plus a dependency.

**Add the grammar** to `crates/devctx-parse/Cargo.toml`:

```toml
tree-sitter-ruby = "0.23"
```

**Add the variant** and its grammar:

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

**Map the extensions:**

```rust
pub fn lang_for_extension(ext: &str) -> Option<Lang> {
    Some(match ext {
        // ...
        "rb" => Lang::Ruby,
        _ => return None,
    })
}
```

**Write the queries.** This is the real work. Each language needs:

| Query | Purpose | Required |
|---|---|---|
| `symbol_query` | Captures declarations as `@function`, `@method`, `@class`, `@struct`, `@enum`, `@interface`, `@type` | Yes |
| `calls_query` | Captures call expressions — these become the graph's edges | Yes |
| `import_query` | Imports, used to resolve call targets | Yes |
| `type_bindings_query` | Variable → type, sharpening method-call resolution | Optional (`None`) |

The capture names matter: the chunker and the graph both key off them.

**Test with a real file**, not a snippet. Grammars disagree with intuition about
node names, and a query that matches a toy example often misses the shape real
code takes.

### The cheaper alternative

If you only need the language *searchable*, not *graphed*, add it to
`raw_text_language()` instead:

```rust
pub fn raw_text_language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        // ...
        "rb" => "ruby",
        _ => return None,
    })
}
```

One line, no grammar, no queries. The files get chunked with overlap and
embedded, so search finds them — they just produce no symbols and no edges. For
config, markup and templates this is the right answer, not a compromise.

## 2. A new route framework

`crates/devctx-parse/src/routes.rs`, in `extract_routes`.

Seven frameworks are recognised: FastAPI, Flask, Express, NestJS, Spring,
Quarkus (JAX-RS) and Angular.

Route extraction is **pattern-based, not AST-based**, which is why Kotlin's
Spring routes are found even though Kotlin has no grammar wired up. That
independence is a feature: a framework detector works for any language whose
files reach the indexer.

Two things the existing tests exist to protect, and that a new detector should
handle:

- **Prefixes compose.** A controller-level prefix plus a method-level path is
  one route, and the tests for Spring and NestJS check exactly that.
- **Lookahead windows must be byte-safe.** There is a test named for accents
  surviving a JAX-RS lookahead, because a window that slices mid-UTF-8 panics on
  real code and never on ASCII fixtures.

## 3. A new embedding provider

Implement `EmbeddingProvider` in `crates/devctx-embed`:

```rust
pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    fn dimension(&self) -> usize;
}
```

Three obligations that are not in the signature:

1. **Normalize your vectors.** `l2_normalize` is provided. Cosine and inner
   product only rank identically for unit vectors, and a provider that skips
   this makes `storage.metric: ip` rank by magnitude — silently, and wrongly.
2. **`dimension()` must be knowable without loading the model.** Servers use
   `dimension_for(provider, model)` to decide whether an embedder is even needed;
   that lazy path is why indexing does not pay for a model it never uses.
3. **Batch.** `embed()` receives many texts because the encoder is far more
   efficient per text at batch size 32 than at 1.

For a built-in local model, add a `LocalModelSpec` to `LOCAL_MODELS` in
`registry.rs` — key, dimension, languages, and the note shown by
`devctx models`.

If you just want to use your own ONNX model, you do not need any of this: set
`embeddings.provider: custom` and `model_dir`. See
[Models and Tuning](09-models-and-tuning.md).

## 4. A new reranker

Implement `Reranker` in `crates/devctx-rerank`:

```rust
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, candidates: &[String], top_k: usize) -> Result<Vec<Ranked>>;
    fn name(&self) -> &str;
    fn pool(&self) -> usize;
}
```

`pool()` is the interesting one. It is simultaneously the ceiling on everything
your reranker could fix — it reorders what it is handed and nothing else — and
the whole of its cost, since it multiplies the slowest stage in the pipeline.
The no-op reranker answers zero, which is the honest answer for something that
reorders nothing.

Before adding one, read [ADR-15](08-design-decisions.md): the cross-encoders
measured here cost two orders of magnitude in latency, and the one benchmarked
across the whole suite made results worse. A new reranker should come with
numbers.

## 5. A new MCP tool

`crates/devctx-mcp/src/lib.rs`, inside the `#[tool_router]` block:

```rust
#[tool(description = "What this answers, and when to reach for it \
                      instead of the neighbouring tool.")]
async fn your_tool(&self, Parameters(req): Parameters<YourReq>) -> Result<String, ErrorData> {
    let backend = self.bound()?;
    run_blocking(move || do_your_thing(&backend, &req)).await
}
```

Conventions that are load-bearing:

- **`self.bound()?`** returns the backend or the "no project bound" error. Use it
  unless the tool genuinely works without a project (`list_projects`,
  `use_project`).
- **`run_blocking`** keeps synchronous store work off the async executor. Store
  calls are blocking; the runtime is not.
- **Return JSON**, as a string. `build_context` is the deliberate exception,
  because its output is read into a model's context rather than parsed.
- **Put the *when* in the description.** The client picks tools by description
  alone. "Returns JSON" is not a reason to call something; "use this when you
  know the name and want the thing itself, use `search` when you want code about
  an idea" is.

The implementation goes in `state.rs`, where the other `do_*` functions live.

## 6. A new CLI command

`crates/devctx-cli/src/main.rs`: add a `Commands` variant and its handler, then
add it to the grouped summary in `help_map.rs` so it appears under the right
heading in `devctx --help`.

That last step is easy to skip and worth not skipping — a command missing from
the grouped help is a command nobody finds. `impact` was invisible that way once.

## Testing

```bash
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

`--offline` is required: the build vendors its dependencies, DuckDB included.

Two lessons this codebase paid for, worth inheriting:

**Fixtures are cleaner than reality.** Multi-branch indexing had 22 green tests
over a feature that was broken in all three real paths, because the fixtures had
one index per branch, no server, and no HNSW index — three ways in which they
were not the world.

**Assert the effect, not the shape.** A test asserting a query returned `Some`
passed while the pipeline copied nothing. Replacing it with a counting embedder
— which measures how many texts were actually embedded — caught it immediately.
