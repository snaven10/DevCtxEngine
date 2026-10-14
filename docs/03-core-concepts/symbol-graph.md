# Symbol Graph

> 🇪🇸 [Leer en español](../es/03-conceptos-fundamentales/grafo-de-simbolos.md)

A graph over the indexed code's symbols: who calls, builds, imports, extends or
uses what — and what a change would reach.

---

## What it is

While indexing, tree-sitter parses each supported file and extracts its symbols
(classes, functions, methods, fields…) and every relation the file writes down:
calls, instantiations, imports, supertypes, type uses, and which symbol holds
which. At the end of every run a **link pass** resolves each relation to the
definition it reaches, across files, and records how sure it is and why.

```bash
devctx symbol authenticate                 # the definition and its code
devctx impact authenticate                 # transitive callers and callees
devctx traverse OrderDto --kinds references --dir in   # who uses a type
devctx map --query search_ranked           # a ranked map of the repository
devctx skeleton src/api/client.ts          # the structure of one file
```

Agents use `read_symbol`, `get_references`, `impact_analysis`, `traverse`,
`repo_map` and `skeleton`; which one when is [below](#which-tool-when).

## Why it exists

Semantic search answers *"where is the code about X?"*. It cannot answer *"what
breaks if I change this?"*, because that question is about structure, not
meaning — and the answer includes code that never mentions X.

The graph answers the structural question without ranking: an edge either
exists or it does not. But **its absence proves nothing** — see the limits
below, because that distinction is what stands between you and a broken
refactor.

## What is in the graph

This describes an index made by 0.10 or later. An index made before holds only
the call edges of 0.9 (`graph_edges`, below) and is answered the old way until
it is rebuilt: see [Keeping the index fresh → Upgrading to
0.10](../13-keeping-the-index-fresh.md#upgrading-to-010-the-symbol-graph).

### Symbols

The `symbols` table holds one row per definition, per branch:

| Field | Holds |
|---|---|
| `id` | A stable id, shown as `sym` (16 hex digits): a hash of repository, file, kind class, qualified name and a disambiguator (the parameter types of a Java method, the trait and type arguments of a Rust `impl`). No branch in it: the same method has the same `sym` on every branch. It changes when the file, the enclosing scope, the name or (Java) the parameter types change — that is another symbol for "who calls it" |
| `kind` | `file`, `module`, `class`, `interface`, `enum`, `record`, `struct`, `trait`, `impl`, `type`, `function`, `method`, `constructor`, `field`, `const` |
| `name`, `qualified` | `update` and `OrderService.update`: every enclosing scope, without the package (`Outer.Inner.m`, `tests.helper` in a Rust `mod tests`, `N.f` inside a TypeScript `namespace N`) |
| `parent_id` | The container — class, `impl`, module — or the file symbol at top level |
| `package` | Java or Go package, Python module, Rust module path, TypeScript module path |
| `signature` | The head of the definition, up to its body, on one line: at most 200 characters after its leading annotations, ending in `…` when cut |
| `exported` | Visible outside its package or module (`public`, `export`, `pub`, a capital in Go, no `_` in Python) |
| `rank`, `in_degree` | The branch's global PageRank and the incoming non-test calls of `high`/`medium` confidence ([Search → Centrality](search.md#centrality-hybrid-only-opt-in)) |
| `is_test` | Test code: a test file, or Rust `#[cfg(test)]`, `mod tests`, `#[test]` code inside a source file |

Every file also has a **file symbol** (`kind = file`): the source of the calls
made outside any function (a Python script, `export const x = f()`), of the
file's imports, and the root of what it contains.

### Edges

The `edges` table holds one row per **occurrence** — two calls from one method
to the same target are two rows — of seven relations:

| Kind | From → to | Comes from |
|---|---|---|
| `calls` | caller → callee | a call (a Python call to a class is a `calls` to the class) |
| `instantiates` | the code → the type built | `new Foo()` (Java, TS/JS), a Rust struct expression, a Go composite literal |
| `imports` | file symbol → what it imports | `import`, `use`, `from … import`, re-exports |
| `inherits` | type → supertype | `extends`, Python bases, a Rust supertrait, a Go embedded type |
| `implements` | type → interface or trait | `implements`, `impl Trait for T` |
| `references` | the code → the type it names | the type of a field, parameter, variable or return value |
| `contains` | container → member | the nesting itself, resolved at parse time |

Each edge carries:

| Field | Meaning |
|---|---|
| `dst_id` | The definition reached, when the link pass resolved one |
| `dst_name` | How the target was written and qualified (`OrderService.findPage`, `java.util.List.of`) — never an expression |
| `confidence` | `high`, `medium` or `low`: how sure the index is that the occurrence means that definition |
| `resolution` | Why: `self`, `local`, `param`, `field`, `ctor_inject`, `import`, `same_file`, `same_package`, `inherited`, `return_type` (typed by a declared return type along a chain), `unique_name`, `name_only`, `external_known`, `chain_external`, `structural` (`contains`) |
| `external` | The target is outside the repository, **with evidence**: the platform (JDK, Python standard library and builtins, Rust `std`/`core`/`alloc`, the Go standard library, DOM/Node globals) or a dependency a manifest declares (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, `setup.cfg`, `requirements*.txt`). "Not found" is not "external": a name with no definition and no evidence stays undecided, `low` |
| `from_test` | The occurrence is in test code (the same rule as `is_test`) |
| `hint` | What the file said about a call's receiver (`typed field OrderService`, `chain findByCode name Order`): the link pass re-resolves an edge from it without re-reading the source |

A call made on a receiver nothing types, to a name the repository does not
define (`.map()` on an untyped stream, a method of an untyped lambda
parameter), is **discarded**: it stays in the table with `resolution =
discarded`, out of every answer (tools read the `live_edges` view), and comes
back by itself when a file that defines the name is indexed. `index_repo`
reports how many there are (`edges_discarded`) and how many calls are still
undecided (`edges_unresolved`).

`graph_edges`, the call table of 0.9 (one row per caller → callee pair, names
instead of ids), is still written alongside, in its old format, so an index
made by 0.10 stays readable if you go back to 0.9.0. Nothing in 0.10 reads it
except the old path for a branch not yet rebuilt.

### How a reference is resolved

Per occurrence, in order; the first rule that answers decides `resolution` and
`confidence`:

| # | Rule | `resolution` | `confidence` |
|---|---|---|---|
| 1 | `this` / `self` / `Self` / `super` → the enclosing type (or its supertype) | `self`, `inherited` | `high` |
| 2 | The receiver is a local or parameter with a type in the lexical scope | `local`, `param` | `high` |
| 3 | The receiver is a field of the enclosing class (each inner class has its own) | `field` | `high` |
| 4 | A field assigned from a constructor parameter, a TypeScript parameter property, Angular `inject(T)` | `ctor_inject` | `high` |
| 5 | The receiver is a type, found through an import or the package (never because it starts with a capital) | `import`, `same_package` | `high` |
| 6 | No receiver: a member of the container or a supertype, then a function of the file, then an import | `self`, `inherited`, `same_file`, `import` | `high` |
| 7 | The one definition of that name in the repository (Java and TypeScript only; never across an import that cannot be followed) | `unique_name` | `medium` |
| 8 | Several candidates, nothing decides | `name_only` (no `dst_id`) | `low` |
| 9 | No candidate in the repository and the type or package is from outside (with evidence) | `external_known` | `high` |

A fluent chain (`a.b().c()`) types `c`'s receiver by the **declared return
type** of `b` when `b` resolved into the repository (`return_type`); after a
call into a library nothing types it: `c` is `chain_external` (`medium`) when
the repository has no `c`, and `name_only` when it does. Confidence never
grows along a chain, and anything that rests on a guess (`unique_name`, a type
defined twice, an import that could not be followed) is never `high`.

**This is not a compiler.** There is no type inference beyond declarations,
initializers (`var x = new T()`, `x := New()`, `let s = Store::open(p)?`),
for-each elements and declared return types; no overload resolution by
argument types (Java picks by arity); no flow analysis. `confidence` says how
much each edge can be trusted, and it was measured (see below).

### What it resolves, by language

Each row is backed by a test; the test names are in a comment in this page's
source.

| Language | Resolves | Does not |
|---|---|---|
| **Java** | Fields per inner class; `var` from its initializer; constants (`LOG`) as fields of their declared type, never as types; constructor injection (`this.x = x`) and `@Inject` fields; imports, same package, wildcard imports, `java.lang`; `new T()` as `instantiates`; supertypes, including statics a Panache entity inherits (external); for-each elements; static imports (JUnit, Mockito); `Object` methods on an untyped receiver (external, `medium`); Lombok `@Data`/`@Getter` accessors (to the field, `medium`); receivers through fields (`this.cfg.server.port()`); anonymous classes | The type of an untyped lambda parameter; overloads chosen by argument types (only by arity); generics beyond the element of a collection; what `Optional.get()`/`List.get()` return |
| **TypeScript / JavaScript** | Relative imports (`.js` written for `.ts`, `index` files); `paths` aliases and `baseUrl` of the root `tsconfig.base.json` / `tsconfig.json` (one `extends`); barrels up to 4 levels; named, default, namespace and package imports; packages external only with evidence (a `package.json` dependency, a Node builtin); Angular DI (constructor parameter properties, `inject(T)` from `@angular/core` in a field or a local); arrow functions bound to a `const` as symbols and sources of their calls; lexical scopes (parameters, `catch`, block functions shadow); declared return types along a chain; interface methods (and abstract methods) as symbols | CommonJS (`require`, `module.exports`), `export =`, dynamic `import()`, JSX components as calls, ambient `.d.ts` declarations; the `paths` of a per-app `tsconfig.json`; `namespace` blocks are not symbols (they only qualify their members); overload signatures are not symbols (only the implementation); `this` in an object literal's method or a class expression; Angular signals called as functions; receivers through several fields (`this.a.b.m()`, by name) |
| **Python** | Modules by path and package (`__init__.py`, the repository root, `src/`, a script's own directory); relative imports, `import *`, re-exports; module-level calls (from the file symbol); a call to a class (to the class); `self.x = …` in a method, typed from a typed `__init__` parameter; `x = Foo()` / `x = make()` by the class or the return annotation; inheritance and `super()`; nested functions; the standard library, builtins and declared distributions as external; `typing` forms (`Any`, `TypeVar` and unions type nothing, `type[X]` is `X`, `Self` the class) | Unannotated parameters; `self.a.b()` (Python declares no fields); decorators that change a type; a requirements file's `-r` includes |
| **Rust** | Crates by `Cargo.toml` (own crate, path dependencies, `workspace = true`); modules by path and inline `mod`; `use` (groups, aliases, globs) scoped to its module, function or block; `crate::`, `self::`, `super::`, `Self::`; `pub use` re-exports; a type's `impl`s in any file of its crate; `impl Trait for T`; every bound of a type parameter; fields by their declaration; `let x = T::new()?` by the return type (`?`/`unwrap()` unwrap `Result`/`Option`); `std`/`core`/`alloc`, the prelude and declared crates as external; `T::default()` from a derive (no `impl` defines it) holds a `T`, at `medium`; `#[cfg]` twins as `medium` | Macros are not calls, and items a macro defines are not symbols; modules declared with `#[path]` or a conditional `mod`; a blanket `impl<T: X> Y for T` belongs to no type (its methods are `T.m`); `let x = a.f()` with `f` outside the repository (a chain, typed no further) |
| **Go** | The `module` of `go.mod` (and a local `replace`) decides what is the repository's; the standard library and other modules as external; a method by its receiver type, also from another file of the package; fields by their declaration; `x := New()` by its declared result; interface methods as symbols; build-tag twins as `medium`; an embedded external type makes a missing method external (`medium`) | Dispatch through embedding (embedding promotes methods, it does not override them); a method promoted from an embedded repository type has no test; checked only with fixtures: no Go repository was measured |
| Kotlin and raw-text files | — | No symbols, no edges (their Spring routes are still extracted) |

<!-- Evidence for the "Resolves" column (crates/devctx-parse/tests unless noted):
Java: link_java.rs a_each_inner_class_types_its_own_field, b_var_is_never_a_type,
c_a_constant_is_a_field_with_its_type, d_constructor_injection, e_same_package_and_import,
f_fluent_chains, g_new_is_instantiates, h_an_undefined_panache_static_is_inherited_and_external,
i_for_each_variables, static_imports_object_methods_literals_and_lombok,
a_dotted_receiver_is_typed_through_its_fields, lambda_parameters_and_patterns_bind_their_names,
an_anonymous_class_scopes_its_bare_calls,
confidence_never_escalates, a_call_after_an_external_one_is_not_external_by_default;
facts_java.rs java_symbols_with_kind_parent_signature_and_visibility (@Inject, record, constructor).
TypeScript/JavaScript: link_typescript.rs constructor_injection_aliases_and_barrels,
inject_in_a_field_follows_a_renamed_import, an_exported_arrow_function_is_a_symbol_source_and_destination,
local_bindings_shadow_fields, imports_by_name_namespace_default_and_package,
a_package_needs_a_manifest_or_a_node_builtin, only_angulars_inject_injects,
this_in_object_literals_and_class_expressions_is_not_a_class, javascript_without_types,
a_tsconfig_with_comments_is_read, a_block_local_function_does_not_shadow_outside_its_block,
a_call_through_an_interface_reaches_its_declared_method; facts_typescript.rs
an_interface_method_is_a_symbol (overload signatures not symbols);
src/symbol_id.rs qualified_names_carry_every_enclosing_symbol (namespace N → N.f).
Python: link_python.rs two_calls_from_one_method_are_two_edges, module_level_calls_are_edges_of_the_file,
relative_star_and_reexported_imports, instance_attributes_are_typed_in_init,
builtins_stdlib_and_declared_packages_are_external, inheritance_and_nested_functions,
a_script_imports_its_siblings_and_the_package, a_local_from_a_method_call_is_a_chain,
typing_special_forms, local_bindings_shadow_attributes.
Rust: link_rust.rs paths_resolve_by_use_and_self, impls_in_other_files_and_trait_impls,
declared_crates_and_std_are_external, macros_are_not_calls, modules_and_functions, values_and_fields,
declared_dependencies_decide_which_crate, every_bound_is_tried, cfg_homonyms_are_medium,
a_use_in_a_function_is_scoped_to_it, a_use_in_a_block_is_scoped_to_it, owners_values_and_variants,
a_derived_default_is_its_type, an_external_supertype_is_medium_evidence;
facts_rust.rs only_a_required_test_cfg_marks_test_code;
devctx-mcp state::impact::tests dispatch_follows_a_rust_blanket_impl (blanket impl methods are T.m).
Go: link_go.rs methods_by_their_receiver, the_module_decides_what_is_the_repositorys,
a_module_needs_its_go_mod, an_external_embedding_is_medium_evidence, package_names_tags_and_shadows;
facts_go.rs go_symbols_imports_and_embedding (embedding is inherits).
Discarded calls: devctx-mcp state::lookup::tests discarded_calls_reach_no_answer;
link_java.rs f_fluent_chains (two discarded). -->

**Measured precision.** Call sites labelled by hand before measuring (the
"gold edges" of `scripts/graph-eval/`), October 2026: Java 20 of 25 right, and
19 of 19 of the `high` ones; TypeScript 20 of 20; Python 26 of 27; Rust 26 of
26 (22 of 22 `high`). Calls left undecided: 4–8 % in two Java repositories,
11 % in a subset of an Angular workspace, 2 % in a Python repository, 17.5 % in
this repository's Rust — mostly chains after a library call to a name the
repository also defines, undecided on purpose. Go was checked with fixtures
only. These are a few dozen sites per language: an order of magnitude, not a
guarantee.

**Known limitations of 0.10.0** (field verification, PLAN-009 TASK-016;
accepted by the maintainer on 2026-10-13):

- **Python records 5 of the 7 relations.** There is no `implements` (Python
  declares no interfaces), and a call to a class, `Klass()`, is recorded as
  `calls` to the class, not as `instantiates`. The Java, TypeScript and Rust
  repositories measured record all 7.
- **Rust leaves 16.0 % of its calls undecided** (no resolved target and no
  `external` mark), against a goal of at most 15 %; the Java, TypeScript and
  Python repositories measured 1.6–14.7 %. What is left is chains after a
  library call to a name the repository also defines, and receivers with no
  known type: undecided on purpose, because marking them external would hide
  callers from `impact_analysis`.

## Supported languages

**Full parsing — symbols and edges (7):**

| Language | Extensions |
|---|---|
| Python | `.py` `.pyi` |
| JavaScript | `.js` `.mjs` `.cjs` `.jsx` |
| TypeScript | `.ts` `.mts` `.cts` |
| TSX | `.tsx` |
| Go | `.go` |
| Java | `.java` |
| Rust | `.rs` |

**Indexed as raw text — searchable, but no symbols and no edges:**

`.html` `.htm` `.css` `.scss` `.sass` `.less` `.json` `.yaml` `.yml` `.xml`
`.md` `.markdown` `.sql` `.graphql` `.gql` `.proto` `.kt` `.kts`

These are chunked with overlap and embedded, so search finds them. They simply
do not appear in the graph.

Kotlin is the notable case: it has no tree-sitter grammar wired up, so it is
indexed as text — **but its Spring routes are still extracted**, because route
detection has a separate path.

Anything else is not indexed.

## Which tool when

| Question | Tool |
|---|---|
| Where is the code about an idea? | `search` (not the graph) |
| I know the name: show me the definition | `read_symbol` |
| Where is this called or built — file and line? | `get_references` |
| What breaks if I change it (callers and callees, transitively)? | `impact_analysis` |
| What does this class contain, what implements this interface, who imports or uses this type — hop by hop, along any relation? | `traverse` |
| I am new to this area: what are its central pieces? | `repo_map`, with a focus |
| Where in this file is the thing I want? | `skeleton`, then `read_symbol` / `read_file` on the range |

`get_references` is one hop and lists occurrences; `impact_analysis` walks
`calls` and `instantiates` (plus dispatch) to a depth, capped per direction;
`traverse` walks the relations you pick, and returns the edges too. `repo_map`
and `skeleton` give signatures, never code: they locate, they do not replace
reading.

## Operations

### `get_references(symbol)` — who calls this?

Every call site of a symbol across the indexed code — calls and instantiations
(`new Foo()`), one per occurrence, so two calls from the same method are two
references. The direct answer to *"is this
safe to change?"* at one hop. `symbol` is a bare name (`charge`), a qualified one
(`Card.charge`, `Card::charge`) or `file::name` (`src/pay.rs::charge`).

On an index made by 0.10 or later, each reference keeps `file`, `line` and
`source` and adds:

| Field | Meaning |
|---|---|
| `confidence` | `high`, `medium` or `low`: how sure the index is that this occurrence means *this* definition |
| `via` | The relation: `calls` or `instantiates` by default; with `kinds`, also `references` (a type use), `imports`, `inherits`, `implements` |
| `sym` | The id (16 hex digits) of the definition referenced, when it resolved to one |
| `external` / `test` / `undecided` | Present (and `true`) only when the occurrence is a library call, sits in a test file, or could not be decided |

**`min_confidence`** (`"high"`, `"medium"` — the default — or `"low"`) sets the
lowest confidence listed. By default `high` and `medium` are listed, `medium`
marked by its field; ambiguous references and calls the index could not decide
are left out and counted in `below_confidence: {count, min_confidence, hint}`,
never dropped in silence. Pass `min_confidence: "low"` to list them, each marked.
For a name with no definition in the repository (a library function), the answer
lists its external call sites.

**`kinds`** adds relations to the default calls and instantiations: any of
`references` (type uses), `imports`, `inherits`, `implements`, or `all`
(`GET /references/<symbol>?kinds=references,imports` over HTTP). A server older
than 0.10 ignores `min_confidence` and `kinds`; the client then says so in
`warning`.

### `impact_analysis(symbol)` — blast radius

Transitive callers *and* callees. Callers are the blast radius: everything that
could break. Callees are what this symbol depends on to work.

Run it before refactoring anything public. This is the operation people forget
exists and then regret not running.

Each side is a list of `{symbol, depth}` (depth 1 = direct), deepest last. On an
index made by 0.10 or later the walk follows the symbol graph **by id** — one
query per level, whatever the size of the frontier — and each symbol also
carries `confidence` (of the call that reached it), `via` (`calls`,
`instantiates` or `dispatch`), `sym`, `file` and `line` (of its definition), and
`test`, `external` or `undecided` when true. Within a depth, symbols are ordered by
confidence, then by rank (the global PageRank each index run computes; see
[Search](search.md#centrality-hybrid-only-opt-in)), then by name.

What the defaults leave out is **counted, never dropped in silence**, and not
walked:

| Parameter (CLI flag) | Default | Left out by default, counted in |
|---|---|---|
| `min_confidence` (`--min-confidence`) | `medium` | symbols reached only by calls of `low` confidence → `below_confidence: {count, min_confidence, hint}` (counted in symbols); calls to the name the index could not decide (`svc.update()` on an untyped receiver) → `undecided_calls: {count, hint}` (counted in calls, none from a test or from a caller already listed); `low` lists both, the callers of the undecided calls marked `undecided` and not walked |
| `include_tests` (`--include-tests`) | `false` | symbols of test files → `excluded.tests` |
| `include_external` (`--include-external`) | `false` | library callees → `excluded.external` |
| `max_nodes` (`--max-nodes`) | `200` per direction (`0` = no cap) | the symbols of the depth that overflows it → `omitted: {count, reason: "limit"}` and `omitted_by_limit: {count, max_nodes, upstream/downstream: {count, depth}}`; no deeper level is read |
| `dispatch` (`--no-dispatch`) | `true` | nothing: dispatch through interfaces, abstract classes and traits is followed (below); `false` follows direct calls only. A node with more override-equivalent methods than it is expanded into (16) → `omitted_by_limit.dispatch: {count, max_per_node}` only (it counts methods; `omitted_by_limit.count` and `omitted.count` keep counting nodes) |

**Dispatch.** A caller that holds a service by its interface (a field injected as
`IService`, a parameter typed by a trait) calls `IService.update`, never
`ServiceImpl.update`; the walk follows that call anyway, through the resolved
`inherits`/`implements` edges, at every depth (not only from the symbol asked
about):

- **upstream**, `ServiceImpl.update` reaches the callers of the methods it
  overrides in its supertypes (`IService.update`, `BaseService.update`), at the
  depth of a direct caller;
- **downstream**, an interface or abstract method reaches the methods that
  implement it, one level below it, and what those call.

A method is override-equivalent when it has the same name and an arity that can
match (defaults, optional and variadic parameters widen it; in TypeScript and
JavaScript the implementing method may take fewer parameters than the one it
implements); a private or static
method overrides nothing; among same-arity overloads in one supertype, the one
whose simple parameter types match exactly wins. The chain of supertypes is followed up to four levels
(`ServiceImpl` → `BaseService` → `IService` → its super-interface); an
override-equivalent method further away is counted in
`omitted_by_limit.dispatch` (its `count`, and apart in `beyond_depth`, with
`max_depth: 4`), not followed. A supertype method that many of the methods
walked implement (an interface with a thousand implementations, all of them
roots) is attributed to at most 16 of them, the surest chains first: its callers are all reached, and the
pairs left unattributed are counted in `omitted_by_limit.dispatch.origins_capped`
(with `max_origins`); a call builds at most 8 192 dispatch rows, the rest counted
in `rows_capped` (with `max_rows`; the answer then says `total_is_lower_bound`).
Such a node
has `via: "dispatch"` and `through` (the method it went through), and its
confidence is `min(medium, the edge it stands for)` — never `high`, and a `low`
call stays `low` (counted in `below_confidence` by default). To leave dispatch
out: `dispatch: false` (`--no-dispatch`, `?dispatch=false`), or
`min_confidence: "high"`, which also leaves out every `medium` call (and does
not read, nor count, any dispatch). Either way the answer says so, from the
filters alone: `dispatch: {"evaluated": false, "reason": "dispatch:false"}` or
`{"evaluated": false, "reason": "min_confidence high"}` (absent when dispatch was
followed). Java,
TypeScript (`implements`, abstract classes) and Rust traits are covered; Python
base classes on a best-effort basis; Go not at all (embedding promotes methods, it
does not override them, and an interface is satisfied without an edge to follow).

Over HTTP: `GET /impact/<symbol>?depth=3&min_confidence=low&include_tests=true&max_nodes=500&dispatch=false`.
The answer says which filters it applied in `filters`; an invalid filter
(`min_confidence=most`) is still a 500 with `error` over HTTP, not a 400 like
`traverse`, `repo_map` and `skeleton`. A bare name means the repository's
definitions of it (as in `get_references`), not every call written with that
name — at most 10 000 of them, a guard against a pathological name (past it the
rest are not seeds, and nothing reports the cut).
The fields of 0.9 keep their meaning: `resolved_symbols` (a bare name that stood
for several declarations), `branch_fallback`, `warning`, and `omitted` /
`omitted_for_budget` (half the output budget per direction; `omitted.count`
adds what the budget and `max_nodes` cut). A name with no definition in the
repository answers as `read_symbol` does: `external: true` with `called_from`
(its upstream is then the callers of its call sites) or `suggestions`. On an
index made before 0.10 the walk is 0.9's — by name, no confidence, no filters, no
cap — with the `warning` that says so (and, when filters were passed, that they
were not applied); a server older than 0.10 ignores the parameters, and the
client then says so in `warning`.

### `traverse(symbol)` — walk the graph yourself

Hop by hop, along the relations you pick: `calls`, `instantiates`, `imports`,
`inherits`, `implements`, `contains`, `references` (`kinds`, comma-separated or
`all`; default `calls`), `out` (default), `in` or `both` (`direction`), 1 to 4
levels (`depth`, default 1; more is refused). It is the walk of
`impact_analysis` — one query per level and direction over the whole frontier,
over live edges only, by id, with the same filters — with other relations, and
with the edges in the answer.

When to use which: `traverse` to explore structure (what a class contains, its
hierarchy, who imports or uses a type, a chain of calls hop by hop);
`impact_analysis` for the blast radius of a change (transitive callers and
callees, capped per direction); `get_references` for the call sites — file and
line — of one symbol.

| Question | Call |
|---|---|
| Callers, and their callers | `{symbol: "OrderService.place", kinds: "calls", direction: "in", depth: 2}` |
| Where a type is built | `{symbol: "Invoice", kinds: "instantiates", direction: "in"}` |
| What a file imports / who imports a type | `{symbol: "src/api/client.ts", kinds: "imports"}` / `{symbol: "OrderDto", kinds: "imports", direction: "in"}` |
| Subclasses | `{symbol: "BaseRepo", kinds: "inherits", direction: "in"}` |
| Implementations | `{symbol: "PaymentGateway", kinds: "implements", direction: "in"}` |
| Members of a class | `{symbol: "OrderService", kinds: "contains"}` |
| Uses of a type | `{symbol: "OrderDto", kinds: "references", direction: "in"}` |
| A class's supertypes, two levels up | `{symbol: "ServiceImpl", kinds: "inherits,implements", depth: 2}` |

`symbol` takes a bare, qualified or `file::name` symbol, or a file path (the
file itself: imports leave from it); when a symbol has that name, the symbol
wins over the path. A bare name with several definitions is
walked from all of them and listed in `candidates` (`{sym, symbol, kind, file,
line}`, within a quarter of the output budget — the page gets whatever they
leave of it — the rest named in
`omitted_for_budget.candidates`; up to 10 000 definitions, as `impact_analysis`,
and `candidates_truncated` past that); `sym` (the hex id a node or a candidate
carries) starts from exactly one.

The answer is `{root, candidates?, nodes, edges, total, next_offset?, omitted?,
partial?, total_is_lower_bound?, filters, below_confidence?, excluded?, omitted_by_limit?, dispatch?,
omitted_for_budget?, branch_fallback?}`:

- `nodes`: `{sym, symbol, kind, file, line, depth, confidence, via}` (`via` is
  the relation that reached it, or `dispatch`, with `through`), plus `test`,
  `external` or `undecided` when true; by depth, then confidence, rank and name.
  The name asked is never listed (as in `impact_analysis`); of the
  definitions a bare name stood for, one reached from another is.
- `edges`: every edge by which a listed node was reached at its depth, one per
  pair and relation — `{from, to, kind, confidence, line, occurrences?}`, `from`
  and `to` being `sym`s (the name, for an end with no definition), in the edge's
  own direction (`from` holds the occurrence).
- Filters as `impact_analysis`: `min_confidence` (`medium`), `include_tests`
  and `include_external` (`false`): what they leave out is counted
  (`below_confidence`, `excluded`) and not walked. With `calls`, dispatch
  through interfaces, abstract classes and traits is followed (`via:
  "dispatch"`, never `high`; `dispatch: false` turns it off, and
  `dispatch: {evaluated: false, reason}` says when it was not followed). With
  `calls` `in` (or `both`) from a name — not a `sym`, not `file::name` — the
  calls to that name the index could not decide are callers too, as in
  `impact_analysis`: counted in `undecided_calls: {count, hint}` (not from tests
  nor from a caller listed), and with `min_confidence: "low"` their callers are
  listed at depth 1, `undecided: true`, never walked, with an edge to the name
  (one that a decided edge reaches deeper is that decided node, walked). With
  `partial` the count can be too high, as in `impact_analysis` past its cap: a
  caller on a level that was not read is not "listed". A root that is a file
  (a path) has no undecided calls.
- Paging (the contract of `search_routes`): `limit` (50) nodes from `offset`;
  `total`, `next_offset`, and `omitted: {count, reason, next_offset}`. A hub —
  a DTO referenced from hundreds of files — is paged, not dumped: once the walk
  holds more nodes than the page needs, no deeper level is read, `partial:
  {from_depth}` says so, and `total` and `omitted.count` then count only what
  was read — `total_is_lower_bound: true` (the next page reads on; the CLI
  prints `>= N`).
- The output budget (`DEVCTX_MAX_OUTPUT_TOKENS`) halves between nodes and
  edges; what does not fit is named in `omitted_for_budget`.

There is no 0.9 path: on an index made before 0.10 `traverse` is an error that
asks for `devctx index --full`; on a current index with no symbols at all (no
language with symbol extraction) it says so and sends to `search` or
`read_file`, since a reindex would change nothing. Over HTTP: `POST /traverse`
with the parameters as its JSON body — an invalid parameter (a depth over 4, an
unknown relation) is a 400 with its message, any other error of the tool a 500
with `error`; a server older than the tool answers 404, which the client turns into "restart it: `devctx serve --stop`".
From the shell: `devctx traverse OrderDto --kinds references --dir in --depth 2`
(`--sym`, `--min-confidence`, `--include-tests`, `--include-external`,
`--no-dispatch`, `--limit`, `--offset`, `--json`).

### `repo_map` — a map to orient yourself

About a thousand tokens that tell an agent where things are: the signatures
(never bodies) of the symbols most relevant to its focus, grouped by file,
ranked by PageRank personalized to that focus, members under their containers.
It is for orientation at the start of a task; to read code use `read_symbol` or
`read_file`, and for one symbol's neighbours, `traverse`.

| Parameter | Meaning |
|---|---|
| `budget` | Tokens the map may take (default 1024; at least 256 is used, at most `DEVCTX_MAX_OUTPUT_TOKENS`; 0 is refused) |
| `focus_files` | Files you have open or are editing (a repo-relative path or its tail): what they use ranks first; they are not listed themselves |
| `focus_symbols` | Symbols the task is about: bare, `Type.member` or `file::name` |
| `query` | The task in words: its identifiers (`camelCase`, `snake_case`, `Type.member`) count as focus symbols, and so does a one-word query |

How it ranks (DD-12, DD-14): without a focus, by the global rank the link pass
writes (`symbols.rank`). With one, the same matrix is ranked again, personalized:
every focus file weighs 50 in the personalization vector, shared among its
symbols, every term of `focus_symbols` or `query` weighs 10, shared among its
definitions, and the edges into a named symbol count ×10;
edges into a `_name` or into a name defined in more than five files count ×0.1.
Only live edges count (a call the link pass discarded lends nothing). Test code
(a test file, or `#[cfg(test)]` code) and libraries never appear, nor do file
symbols or the focus files. How many signatures fit is a binary search over the
ranked list, stopping once the map is within 15 % under the budget (tokens are
characters / 4, as everywhere in the product). Files come in the order of their
symbols' summed rank, symbols by line, indented under their containers; `⋮`
says a file has more:

```
crates/devctx-search/src/lib.rs
│ pub enum SearchMode
│ pub struct RankOptions
│   impl RankOptions
│     pub fn keeps(&self, kind: PathKind) -> bool
│ pub fn search_ranked(store: &Store, query: &str, filter: &SearchFilter, limit: usize, …
│ pub fn identifier_tokens(query: &str) -> Vec<String>
│ pub fn reciprocal_rank_fusion(lists: &[Vec<SearchResult>], k: f32) -> Vec<SearchResult>
⋮
crates/devctx-core/src/types.rs
│ pub struct SearchResult
│   impl SearchResult
│     pub fn rewrite_score(&mut self, score: f32)
⋮
[devctx] focus: query identifier search_ranked (1 definition)
[devctx] repo map: 45 of 4267 symbols in 9 files, ranked by PageRank personalized to the focus; 4222 more did not fit the 1024-token budget (a focus or a larger budget shows others). To read code: read_symbol / read_file.
```

(An excerpt of `devctx map --query search_ranked` on this repository.)

The closing `[devctx]` lines say what the focus matched (`not found` for a name
or file that is nothing here, and that the ranking then stayed global) and how
many symbols did not fit. Without a focus a large repository's map is its
globally central code, which says little: give one.

The serve keeps the branch's adjacency in memory, per index generation, so a
map costs a personalized PageRank and a render; it is read again after every
`index` and released when unused as long as a model would be
(`DEVCTX_MODEL_IDLE_SECS`). `status` reports it under `memory.repo_map`. It
holds one branch: maps of two branches asked in turn rebuild it each time.
Lists in `GET /repo-map` and `devctx map --focus/--symbol` are comma-separated,
so a path with a comma can only be passed through the MCP tool. Tokens are
estimated as characters / 4 like everywhere else; the `│` and `⋮` of the layout
may cost a real tokenizer more than that.

There is no 0.9 path: on an index made before 0.10 `repo_map` is an error that
asks for `devctx index --full`; on a current index with no symbols at all it
says so and sends to `search` or `read_file`. Over HTTP: `GET
/repo-map?budget=…&focus_files=a,b&focus_symbols=…&query=…` (lists
comma-separated; an invalid parameter, such as a budget of 0, is a 400, any
other error of the tool a 500 with `error`); a server older than the tool answers 404, which the client
turns into "restart it: `devctx serve --stop`". From the shell: `devctx map
--focus src/api/client.ts --symbol OrderService.place --query "…" --budget 1500`.

### `skeleton(file)` — the structure of one file

The symbols of one file — classes, methods, functions, fields — with their
signatures and line ranges, nested by container, without bodies, and its imports
in one line. It is for **locating** where something is, at a fraction of the
file's tokens; it is not the code: to edit, read it with `read_symbol` or
`read_file` (a line range from the skeleton).

```
crates/devctx-store/src/skeleton.rs: 13 symbols
imports (6): duckdb::params, crate::error::Result, crate::store::Store, super::*, crate::StoredSymbol, crate::StoredSymbolEdge
pub struct FileImports [13-18]
  pub total: usize [15-15]
  pub first: Vec<String> [17-17]
impl Store [20-82]
  pub fn file_imports(&self, repo: &str, branch: &str, file: &str, limit: usize) -> Result<FileImports> [24-51]
  pub fn has_file_rows(&self, repo: &str, branch: &str, file: &str) -> Result<bool> [55-63]
  pub fn file_suggestions(&self, repo: &str, branch: &str, asked: &str, n: usize) -> Result<Vec<String>> [67-81]
pub fn rank_file_suggestions(asked: &str, files: &[String], n: usize) -> Vec<String> [92-134]
mod tests [137-264] [test]
  fn files(v: &[&str]) -> Vec<String> [141-143] [test]
  fn suggestions_rank_tail_then_name_then_near_miss() [148-193] [test]
  fn file_sym(file: &str) -> StoredSymbol [195-207] [test]
  fn imports_are_counted_and_only_the_first_read() [212-263] [test]
```

(`devctx skeleton crates/devctx-store/src/skeleton.rs` on this repository: 244 tokens for a file of 2 406.)

- `file` is a repo-relative path, as `read_file` takes it (an absolute path inside
  the repository, also through a symbolic link, and a leading `./` are
  accepted). A path with `..`, or an absolute one outside the repository, is
  refused before anything is looked up, with the same message whether it exists
  or not. `devctx skeleton` reads the path relative to the working directory.
- The tree follows `parent_id`: two spaces per level; siblings by line, byte, end
  (the longest first), kind, name and id: any order of the rows draws the same
  text. A Rust
  `impl` is a member of its type, as the extractor records it, so it is drawn
  under the type even when it is written further down: the range says where.
- The signature is the stored one (DD-17: the definition up to its body,
  whitespace collapsed; its head — after leading annotations, decorators and
  attributes — at most 200 characters, ending in `…` when cut; the leading
  annotations that fit whole in 200 more stay in front) shown without those
  annotations, a field's or constant's value, or a final `;`. An index made
  before extractor version 21 counted the annotations against the 200: a cut
  that left an annotation unclosed is drawn `kind name …`.
- `imports (N): …` names the first 8 distinct targets, by line; the rest are
  counted (`+M more`).
- Test code is **marked, not hidden**: a `#[cfg(test)]` module and its members
  end in `[test]`, and a test file says `test file` once in its first line. The
  file is what was asked for, so hiding part of it would answer a different
  question.
- The text stays within `DEVCTX_MAX_OUTPUT_TOKENS` (8000; `0` is no cap; at
  least 256 is used; when the fixed lines leave no room the imports line shrinks
  to its count). Symbols are dropped from the end, in tree order, and the
  closing line says how many and the lowest and highest line they cover: `[devctx] N more
  symbols (lines a-b) did not fit the B-token budget (DEVCTX_MAX_OUTPUT_TOKENS);
  read_file with start_line=a shows them`.
- A file the index does not hold is an error that says whether it exists on disk
  and names the nearest indexed paths (the same path tail, the same file name,
  a near miss of it, the same directory). A file indexed as text (a language
  without symbol extraction: SQL, YAML…) is an error that sends to `read_file`.
- Nothing is read from the file itself: the skeleton is the index's view of the
  last indexed version.
- What the extractor does not give, the skeleton does not show: TypeScript
  `namespace` blocks are not symbols (their members come out flat, at the top),
  TypeScript overload signatures are not extracted (only the implementation),
  and functions nested in Python functions are extracted but not covered by a
  fixture of this tool.

There is no 0.9 path: on an index made before 0.10 `skeleton` is an error that
asks for `devctx index --full`; on a current index with no symbols at all (no
language with symbol extraction) it says so and sends to `search` or
`read_file`. Over HTTP: `GET /skeleton?file=…` (an empty `file` is a 400, any
other error of the tool — a file the index does not hold included — a 500 with
`error`, never a 404); a server older than the tool answers 404, which the client
turns into "restart it: `devctx serve --stop`". From the shell: `devctx skeleton
src/api/client.ts`.

### `read_symbol(name)` — the definition

Code, file, line range and kind. Use this when you know the name and want the
thing itself; use `search` when you want code *about an idea*. `limit` (default
5) caps the definitions — a name can be defined many times. The name can be bare,
qualified (`Card.charge`, `Card::charge`, with its package or crate in front) or
`file::name`; `Foo.Bar::baz` is the member `baz` of `Foo.Bar`, not a file.

On an index made by 0.10 or later each definition also carries `sym` (its id, 16
hex digits, the same on every branch), `kind` (`function`, `method`, `class`,
`field`…) and `qualified`. A definition the chunker gives no code of its own (a
field, a constant) answers with its signature as `code`.

**A miss is explained, not just empty.** When no definition is found,
`read_symbol` adds:

- `suggestions` — up to 5 names to try: the qualified forms the call graph knows
  for a bare name (`charge` → `Card.charge`), then the closest defined names by
  prefix, suffix, containment or edit distance (`AuthServce` → `AuthService`);
- `external: true`, `called_from: N` and a `next_step` when the name is only ever
  a call *target* — most likely a library or runtime function, though it may live
  in a file the index excludes or on another branch. A qualified name
  (`serde_json::from_str`, `Panache.withTransaction`) is looked up as written and
  then by its last segment, since the graph often holds only the bare callee. An
  external gets no fuzzy `suggestions` (`with_context` is not a typo of
  `build_context`). On a 0.10 index an external answers no `suggestions` at all,
  as 0.9 did; and on a 0.10 index
  `external` comes from the index's own evidence (a declared dependency, the
  platform), and the last segment is only tried when the repository defines
  nothing by that name: `Foo::new` that does not exist is not "external" because
  some `new` is called — it gets `suggestions` (`Thing.new`). A `file::name` that
  matches nothing in that file is never external either.

Small functions that the chunker grouped into one chunk (`a, b, c`) are found
inside it, so they are not misreported as external.

**An index made before 0.10** (or by another extractor) has no symbol table:
`read_symbol`, `get_references`, the anchoring of `search` and the web graph view
answer as 0.9 did, by name over the call graph, and say so in `warning` — reindex
with `devctx index --full` to get the fields above. `search` in `keyword`/`hybrid`
says it too: its identifier anchoring went by name. A failed read of the symbol
table gets its own `warning` (a read error, not a reason to reindex).

## Which branch it answers from

The graph is per branch. When the checked-out branch has no indexed rows,
`read_symbol`, `get_references`, `impact_analysis`, `search_routes`,
`routes_for_handler` (and `search`, `build_context`, `memories_by_symbol`) answer
from the default or most recently indexed branch and add
`branch_fallback: {current, used, why}`; with no index at all they fail with an
explicit error instead of `[]`. See
[Search → Branch awareness](search.md#branch-awareness).

## An index made before 0.10

The graph above is written by `devctx index --full` with a 0.10 binary. Until
then a branch reports `extractor_stale: true` on `index_status`, and:

| Tool | Answers |
|---|---|
| `read_symbol`, `get_references`, `search` (`keyword`/`hybrid`), the web graph view | As 0.9 did, by name over `graph_edges`, with a `warning` that says so |
| `impact_analysis` | The 0.9 walk — by name, no confidence, no filters, no cap — with the `warning` (which adds, when filters were passed, that they were not applied) |
| `build_context` with `related: true` | The brief without the section, and `[devctx] related: not shown: …` |
| `traverse`, `repo_map`, `skeleton` | An error: `… reads the symbol graph, and this branch's index predates it (an older extractor): reindex with devctx index --full (vectors are reused)` |
| hybrid search with `search.centrality_weight` > 0 | Vector + keyword only: there are no ranks to add |

The upgrade, what it costs and how to go back are in
[Keeping the index fresh](../13-keeping-the-index-fresh.md#upgrading-to-010-the-symbol-graph).

## Limits worth knowing

**A bare name means the repository's definitions of it.** `get_references`,
`impact_analysis` and `traverse` resolve `update` to every symbol named
`update` and walk from all of them by id, not to every call written `.update(`
— a library's `.map()` is not a caller of your `map`. The answer lists what a
bare name stood for (`resolved_symbols`, `candidates`); a common name such as
`update` can stand for a few dozen methods, so read that line. Ask with the
qualified name (`OrderService.update`), with `file::name`, or with a `sym` when
you mean exactly one. A name with no
definition in the repository stands for its external call sites.

**Calls the index could not decide are counted, not hidden.** A call on an
untyped receiver to a name the repository has (`svc.update()` with `svc`
unknown) has no destination: it is never walked, it is counted in
`undecided_calls` / `below_confidence`, and `min_confidence: "low"` lists it,
marked `undecided`.

**An empty result still means "nothing found", never "nothing there".** Three
of the reasons used to be silent and no longer are: a branch with no rows
(`branch_fallback`), an index built by an older extractor (`warning` on every
graph answer, `extractor_stale` on `index_status` — run `devctx index --full`),
and what a filter left out (`below_confidence`, `excluded`, `omitted`). The
ones below still announce nothing. Cross-check an empty result with `search
--keyword` before renaming or deleting.

**Resolution is static and per language** — imports, declared types, scopes and
the rules in the table above — not a compiler's. A wrong `high` edge is rare in
the measured repositories; a missing one is not.

**Dynamic dispatch is half visible.** A call through an interface, an abstract
class or a trait is followed (`via: "dispatch"`, never `high`). A call through a
callback, a function value, reflection, a service looked up by name or a
string-keyed registry leaves no edge at all. The graph under-reports exactly
where a language is most dynamic.

**Only 7 languages produce edges.** In a polyglot repository the graph covers
part of it, and there is no warning that says which part. `devctx status` shows
the symbol count; a suspiciously low one usually means the code is in a
language that is being indexed as text.

## How it complements search

| Question | Tool |
|---|---|
| *Where is the code about authentication?* | `search` |
| *What calls `authenticate`?* | `get_references` |
| *What breaks if I change it?* | `impact_analysis` |
| *What surrounds it — its types, its hierarchy, its members?* | `traverse` |
| *Where do I start in this repository?* | `repo_map` |
| *Why is it written this way?* | `memories_by_symbol` |
| *What should I read before answering?* | `build_context` |

Search is fuzzy and ranked. The graph is structural, and says how sure it is of
each edge. The *why* row is the one people do not think to ask, and it is the
one the code cannot answer at all.

## Mental model

Search is a map of the territory: it shows you what is near what, by meaning.
The graph is the road network: it shows you what actually connects to what, and
therefore where traffic goes when you close a road.

You want the map to find the neighbourhood, and the road network before you dig
anything up.
