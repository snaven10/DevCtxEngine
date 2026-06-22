# Install & Config Overhaul — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate install/config docs into a single source of truth, fix the highest-impact configuration defaults, and document the multi-repo central-store topology as a reproducible recipe.

**Architecture:** Three workstreams on branch `docs/install-config-overhaul`. Code defaults (WS2) land first so the docs (WS1/WS3) describe the new behavior, not the old. Go changes are TDD'd; doc changes are verified with `rg`/grep checks and a linear read-through.

**Tech Stack:** Go 1.24 (cmd/devai), Python 3.12 (ml/devai_ml), POSIX shell (scripts/install.sh), Markdown docs (bilingual EN/ES).

## Global Constraints

- Spec: `docs/proposals/install-config-overhaul.md` — every task implements part of it.
- **Source of truth:** `docs/11-configuration.md` is authoritative for configuration. README summarizes + points; it never re-hosts a full table that can diverge.
- **EN/ES parity:** any change to `docs/NN-*.md` or README config content is mirrored in `docs/es/NN-*.md`.
- **Do not build/release without the user asking** (project rule). `go test` is allowed for verification; `make build`, `oc`, releases are not run unless the user authorizes.
- Conventional commits. No "Co-Authored-By".
- Default embedding model is `minilm-l6` (384d) — `ml/devai_ml/config.py:100`, `cmd/devai/cmd/init.go`, `scripts/install.sh:31`. Do not change it.
- The recommended CPU multilingual model is `ml-granite` (ONNX int8, 384d).
- Go cmd tests live in `cmd/devai/cmd/*_test.go` (pattern: `mcp_configure_test.go`). Run with `go test ./cmd/...`.

---

## Group A — WS2: Configuration defaults (Go + scripts)

### Task 1: `devai init` stops writing a per-repo `state_dir`

Footgun F. The generated `.devai/config.yaml` currently hardcodes an absolute `state_dir: <repo>/.devai/state`, which forces a per-repo store and breaks centralization (index writes here, MCP reads the central store). Omit `state_dir` so it resolves to the XDG central default.

**Files:**
- Modify: `cmd/devai/cmd/init.go` (the `config := fmt.Sprintf(...)` template, ~line 60, and the trailing `fmt.Printf("  State: ...")` lines)
- Test: `cmd/devai/cmd/init_test.go` (create)

**Interfaces:**
- Produces: a generated config whose body contains no top-level `state_dir:` key. Later doc tasks (Task 11) rely on this new behavior.

- [ ] **Step 1: Write the failing test**

Create `cmd/devai/cmd/init_test.go`. The init logic builds the config via `fmt.Sprintf`; extract the template into a testable helper first (Step 3 does the extraction). The test asserts the generated config omits `state_dir` but keeps `project.name`/`project.path`:

```go
package cmd

import "strings"
import "testing"

func TestInitConfigTemplate_OmitsStateDir(t *testing.T) {
	cfg := renderInitConfig("MyProj", "/home/u/repo")
	if strings.Contains(cfg, "state_dir:") {
		t.Fatalf("generated config must not pin a per-repo state_dir; got:\n%s", cfg)
	}
	if !strings.Contains(cfg, "name: MyProj") {
		t.Fatalf("config should contain project name; got:\n%s", cfg)
	}
	if !strings.Contains(cfg, "path: /home/u/repo") {
		t.Fatalf("config should contain project path; got:\n%s", cfg)
	}
	if !strings.Contains(cfg, "model: minilm-l6") {
		t.Fatalf("config should keep default model; got:\n%s", cfg)
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `go test ./cmd/devai/cmd/ -run TestInitConfigTemplate -v`
Expected: FAIL — `renderInitConfig` undefined.

- [ ] **Step 3: Extract the template into `renderInitConfig` and drop `state_dir`**

In `cmd/devai/cmd/init.go`, replace the inline `config := fmt.Sprintf(...)` block with a call to a new helper, and define the helper without the `state_dir:` line:

```go
// renderInitConfig builds the default .devai/config.yaml body. It intentionally
// omits `state_dir` so the store resolves to the central XDG default
// (~/.local/share/devai/state) unless the user opts into a per-repo or shared
// path. See docs/12-multi-repo-central-store.md.
func renderInitConfig(name, absPath string) string {
	return fmt.Sprintf(`# DevAI project configuration
project:
  name: %s
  path: %s

# state_dir: ~/.local/share/devai/state  # default (central). Uncomment to override.
language: es  # en | es

embeddings:
  provider: local
  model: minilm-l6
  # offline: auto  # auto=use cache when available, true=always offline, false=always check HF Hub

storage:
  mode: local
  # qdrant_url: localhost:6334
  # qdrant_api_key: ""

indexing:
  exclude:
    - "node_modules/**"
    - "vendor/**"
    - ".git/**"
    - "__pycache__/**"
    - "dist/**"
    - "build/**"
    - "*.min.js"
    - "*.lock"
`, name, absPath)
}
```

In the `runInit` body, replace `config := fmt.Sprintf(\`...\`, name, absPath, stateDir)` with `config := renderInitConfig(name, absPath)`. Keep the `os.MkdirAll(stateDir, ...)` line as-is (creating the dir is harmless) but change the final summary print from the hardcoded per-repo state to the resolved default:

```go
	fmt.Printf("Initialized devai for %s\n", absPath)
	fmt.Printf("  Config: %s\n", configPath)
	fmt.Printf("  Store:  central default (~/.local/share/devai/state) — set state_dir or DEVAI_STATE_DIR to override\n")
```

- [ ] **Step 4: Run test to verify it passes**

Run: `go test ./cmd/devai/cmd/ -run TestInitConfigTemplate -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add cmd/devai/cmd/init.go cmd/devai/cmd/init_test.go
git commit -m "fix(init): omit per-repo state_dir so store resolves to central default"
```

---

### Task 2: Auto-index hook injects the active model and the OOM guard

Footguns D + E. `hookBlock` only injects `DEVAI_STATE_DIR`. If the repo config's model differs from the store, each commit reindexes with the wrong model; and there is no `DEVAI_EMBED_MAX_CHARS` guard. Capture the active model + the embed cap at install time and inject them into the block (mirrors the user's hand-rolled hooks).

**Files:**
- Modify: `cmd/devai/cmd/hooks.go` (`hookBlock`, ~line 54; `runHooksInstall` model resolution, ~line 86)
- Test: `cmd/devai/cmd/hooks_test.go` (create)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `hookBlock(devaiBinary, stateDir, model, maxChars string) string` — note the **new signature** (two extra params). Any other caller of `hookBlock` must be updated.

- [ ] **Step 1: Write the failing test**

Create `cmd/devai/cmd/hooks_test.go`:

```go
package cmd

import "strings"
import "testing"

func TestHookBlock_InjectsModelAndEmbedCap(t *testing.T) {
	block := hookBlock("/bin/devai", "/central/state", "ml-granite", "2048")
	for _, want := range []string{
		`DEVAI_STATE_DIR="/central/state"`,
		`DEVAI_EMBEDDING_MODEL="ml-granite"`,
		`DEVAI_EMBED_MAX_CHARS="2048"`,
		"index --incremental",
		hookBeginMarker,
		hookEndMarker,
	} {
		if !strings.Contains(block, want) {
			t.Fatalf("hook block missing %q; got:\n%s", want, block)
		}
	}
}

func TestHookBlock_OmitsModelWhenUnset(t *testing.T) {
	block := hookBlock("/bin/devai", "/central/state", "", "2048")
	if strings.Contains(block, "DEVAI_EMBEDDING_MODEL=") {
		t.Fatalf("model env must be omitted when empty; got:\n%s", block)
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `go test ./cmd/devai/cmd/ -run TestHookBlock -v`
Expected: FAIL — too few arguments to `hookBlock`.

- [ ] **Step 3: Update `hookBlock` to take and inject model + maxChars**

Replace `hookBlock` in `cmd/devai/cmd/hooks.go`:

```go
// hookBlock builds the delimited block injected into the post-commit hook.
//
// It cd's to the repo top-level first so `devai index` resolves the real repo
// name (it sends repo_path="."), pins the store + model + embed cap so the
// auto-index matches the MCP server's store exactly, and backgrounds the run so
// the commit is never blocked. model may be empty (then it is omitted).
func hookBlock(devaiBinary, stateDir, model, maxChars string) string {
	env := fmt.Sprintf(`DEVAI_STATE_DIR=%q`, stateDir)
	if model != "" {
		env += fmt.Sprintf(` DEVAI_EMBEDDING_MODEL=%q`, model)
	}
	if maxChars != "" {
		env += fmt.Sprintf(` DEVAI_EMBED_MAX_CHARS=%q`, maxChars)
	}
	return fmt.Sprintf(`%s
# Auto-index after each commit. Managed by 'devai hooks install/uninstall' — do not edit by hand.
( cd "$(git rev-parse --show-toplevel)" && %s %q index --incremental ) >/dev/null 2>&1 &
%s`, hookBeginMarker, env, devaiBinary, hookEndMarker)
}
```

- [ ] **Step 4: Resolve model + maxChars in `runHooksInstall` and pass them**

In `runHooksInstall`, after the existing `stateDir` resolution block, add resolution for model and embed cap from the environment (the installer/MCP context), then update the `hookBlock` call and the warning:

```go
	if stateDir == "" {
		stateDir = filepath.Join(absPath, ".devai", "state")
		fmt.Println("  ⚠  DEVAI_STATE_DIR not set — hook will use a per-repo store.")
		fmt.Println("     For a central store, set DEVAI_STATE_DIR before installing the hook.")
	}

	model := os.Getenv("DEVAI_EMBEDDING_MODEL")
	maxChars := os.Getenv("DEVAI_EMBED_MAX_CHARS")
	if maxChars == "" {
		maxChars = "2048" // conservative OOM guard for the background indexer
	}
```

Then change the block construction:

```go
	block := hookBlock(devaiBinary, stateDir, model, maxChars)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `go test ./cmd/devai/cmd/ -run TestHookBlock -v`
Expected: PASS (both).

- [ ] **Step 6: Build-check the package compiles (no full binary)**

Run: `go vet ./cmd/devai/cmd/`
Expected: no errors (confirms no other caller of `hookBlock` left with the old arity).

- [ ] **Step 7: Commit**

```bash
git add cmd/devai/cmd/hooks.go cmd/devai/cmd/hooks_test.go
git commit -m "fix(hooks): inject active model + embed cap into auto-index hook block"
```

---

### Task 3: `install.sh` fails loudly when the ML wheel is missing

Footgun A. A missing `devai_ml` wheel currently `warn`s and continues; ML features then fail silently. Make it abort unless the user explicitly opted out.

**Files:**
- Modify: `scripts/install.sh` (the `else warn "devai_ml wheel not found..."` branch, ~line 413; add `--allow-no-ml` flag parsing near the other flags)

**Interfaces:** none (shell).

- [ ] **Step 1: Add the opt-out flag default**

Near the global defaults in `scripts/install.sh` (where `INSTALL_HOOKS`, `MODEL`, etc. are declared, ~line 31), add:

```sh
ALLOW_NO_ML=false
```

And in the argument parser (where `--gpu`, `--no-hooks`, etc. are handled), add a case:

```sh
        --allow-no-ml) ALLOW_NO_ML=true; shift ;;
```

- [ ] **Step 2: Make the missing-wheel branch fail by default**

Replace the `else` branch at ~line 413:

```sh
    else
        if [[ "${ALLOW_NO_ML}" == true ]]; then
            warn "devai_ml wheel not found — continuing without ML (search/index will not work)."
        else
            die "devai_ml wheel not found in release assets. ML features (embeddings, search, indexing) will not work. Re-run with --allow-no-ml to install anyway."
        fi
    fi
```

- [ ] **Step 3: Verify the script still parses**

Run: `bash -n scripts/install.sh`
Expected: no syntax errors (exit 0).

- [ ] **Step 4: Verify the flag is wired**

Run: `grep -n -- '--allow-no-ml\|ALLOW_NO_ML' scripts/install.sh`
Expected: 3 hits (default, parser case, branch).

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh
git commit -m "fix(install): fail loudly on missing ML wheel; add --allow-no-ml opt-out"
```

---

### Task 4: `configure_client` injects the embed cap into the MCP client config

Footgun G. The installer injects `DEVAI_STATE_DIR` + `DEVAI_EMBEDDING_MODEL` into the client JSON but not the OOM guard. Add it so it's visible and tunable.

**Files:**
- Modify: `scripts/install.sh` (`configure_client`, the `env_flags=(...)` line, ~line 469)

- [ ] **Step 1: Add the embed cap to `env_flags`**

In `configure_client`, change:

```sh
    local env_flags=(--env "DEVAI_STATE_DIR=${STATE_DIR}" --env "DEVAI_EMBEDDING_MODEL=${MODEL}")
```

to:

```sh
    local env_flags=(
        --env "DEVAI_STATE_DIR=${STATE_DIR}"
        --env "DEVAI_EMBEDDING_MODEL=${MODEL}"
        --env "DEVAI_EMBED_MAX_CHARS=2048"
    )
```

- [ ] **Step 2: Verify the script still parses**

Run: `bash -n scripts/install.sh`
Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "fix(install): inject DEVAI_EMBED_MAX_CHARS into MCP client config"
```

---

### Task 5: `devai index` aborts on model/store mismatch instead of corrupting (conservative footgun C)

Footgun C, conservative variant. Rather than changing config resolution order (higher risk), detect when the resolved embedding model's dimension differs from the existing store's recorded dimension and abort with a clear message — turning silent corruption into an actionable error. If no store exists yet, proceed normally.

**Files:**
- Modify: the index command entrypoint (`cmd/devai/cmd/index.go` or wherever `index` resolves config + opens the store — locate with `grep -rn "func runIndex\|index --incremental\|LoadConfigFromCWD" cmd/`)
- Reuse: `resolvedStorageConfig()` in `cmd/devai/cmd/server.go:79` if the index path shares it.

**Interfaces:**
- Consumes: the resolved `*config.ProjectConfig` (has the embedding model) and the store's recorded dimension (LanceDB schema or a `meta` row).

> **Implementer note:** This task is exploratory — locate the exact mismatch-check seam first. If the store does not expose its recorded model/dimension cheaply from Go (it may live behind the Python ML RPC), implement the guard at the point where the dimension is already known (the upsert path returns a PyArrow schema error today). Prefer the smallest change that converts the silent/cryptic failure into: `die("store was built with <Ndim> dims (model X); config requests <Mdim> dims (model Y). Reindex from scratch or set the matching model. See docs/12.")`. If no cheap seam exists, **defer this task** and document the mismatch as a known gotcha in Task 11 instead (the spec lists C as the riskiest, deferral-eligible item).

- [ ] **Step 1: Locate the seam**

Run: `grep -rn "LoadConfigFromCWD\|resolvedStorageConfig\|dimension" cmd/devai/cmd/ internal/storage/`
Expected: identify where index opens the store with a dimension.

- [ ] **Step 2: Decide — implement guard or defer**

If a cheap dimension comparison exists: implement the `die(...)` guard described above and add a Go test that feeds a mismatched config + a fake store-dimension and asserts the error. If not: mark this task deferred in the plan checkbox, and ensure Task 11 documents the mismatch gotcha. Either way, record the decision in the commit/PR description.

- [ ] **Step 3 (if implemented): Commit**

```bash
git add cmd/devai/cmd/index.go cmd/devai/cmd/index_test.go
git commit -m "fix(index): abort on model/store dimension mismatch instead of corrupting"
```

---

## Group B — WS1: Documentation consolidation

> For all doc tasks: "test" = the listed `rg`/grep verification + a manual read-through. No build.

### Task 6: Merge README install into one path; fix the install flag table

**Files:**
- Modify: `README.md` (§Install ~L27–52, §Installation ~L133–160)

- [ ] **Step 1: Merge the two install sections into one "## Install"**

Keep a single section. Structure:
1. **Quick install (recommended):** the `curl … | bash` one-liner.
2. **From source (for contributors):** `make build` etc. — remove the word "recommended" from this subsection.
3. Replace the partial 3-flag table with one line: `> Full installer flag reference: [docs/11-configuration.md §2.4](docs/11-configuration.md).`

Delete the now-duplicate §Installation block (its from-source content moves into the merged section).

- [ ] **Step 2: Verify no contradictory "recommended" remains on from-source**

Run: `rg -n -i 'from source.*recommended|recommended.*from source' README.md`
Expected: 0 matches.

- [ ] **Step 3: Verify a single Install heading**

Run: `rg -n '^##+ Install' README.md`
Expected: exactly 1 match.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs(readme): merge install into one path; point to docs/11 for flags"
```

---

### Task 7: Fix README config section — pointers, correct values, ONNX models

**Files:**
- Modify: `README.md` (§Configuration ~L367–415, §Embedding Models ~L427–432)

- [ ] **Step 1: Reduce the env-var table to a quick reference + pointer**

Keep only the 5–7 most common vars (`DEVAI_STATE_DIR`, `DEVAI_EMBEDDING_MODEL`, `DEVAI_LOCAL_DB_PATH`, `DEVAI_EMBED_MAX_CHARS`, `DEVAI_TOKEN_STRATEGY`, `DEVAI_MAX_OUTPUT_TOKENS`). Above the table: `> These are the common ones. Full authoritative table (30+ vars): [docs/11-configuration.md §3](docs/11-configuration.md).`

- [ ] **Step 2: Fix the two wrong values**

- `DEVAI_STATE_DIR` default → `~/.local/share/devai/state` (not `.devai/state/`).
- `DEVAI_TOKEN_STRATEGY` values → `drop / soft_truncate / hard_truncate / summarize`.

- [ ] **Step 3: Replace the inline config.yaml schema with a pointer**

Replace the partial schema block with: `> Full config.yaml schema: [docs/11-configuration.md §1.1](docs/11-configuration.md).` (eliminates the divergent/incomplete copy).

- [ ] **Step 4: Add the ONNX models to the model table**

In §Embedding Models, add rows for `ml-granite` (ONNX int8, 384d, ~94MB, recommended for CPU multilingual) and `ml-granite-lg` (ONNX int8, 768d, ~299MB). Add: `> Picking a model for your hardware: [docs/09-models-and-tuning.md §1 & §5](docs/09-models-and-tuning.md).`

- [ ] **Step 5: Verify corrected values present**

Run: `rg -n 'hard_truncate|~/.local/share/devai/state|ml-granite' README.md`
Expected: at least 3 matches (one per fix).

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs(readme): config section -> pointers; fix state_dir/token-strategy; add granite models"
```

---

### Task 8: Add the v0.12 embed vars (and fix token-strategy values) in `docs/11`

**Files:**
- Modify: `docs/11-configuration.md` (§3 env-var table; the `DEVAI_TOKEN_STRATEGY` row)

- [ ] **Step 1: Add the two rows to the authoritative env table**

Add to §3:
- `DEVAI_EMBED_MAX_CHARS` — default `4096`. RAM guard: max chars fed to the encoder per text (not the model's context limit). Lower (e.g. 2048) on low-RAM machines to avoid OOM on minified/large non-code chunks.
- `DEVAI_EMBED_BATCH_SIZE` — default `16`. Texts per embedding batch. Lower (e.g. 8) to reduce peak RAM.

- [ ] **Step 2: Confirm token-strategy lists all four values in docs/11**

Run: `rg -n 'hard_truncate' docs/11-configuration.md`
Expected: ≥1 match (add it if missing).

- [ ] **Step 3: Verify the embed vars are documented**

Run: `rg -n 'DEVAI_EMBED_MAX_CHARS|DEVAI_EMBED_BATCH_SIZE' docs/11-configuration.md`
Expected: 2 matches.

- [ ] **Step 4: Commit**

```bash
git add docs/11-configuration.md
git commit -m "docs(config): document DEVAI_EMBED_MAX_CHARS/BATCH_SIZE in authoritative table"
```

---

### Task 9: Delete the rotten `DOCS.md`; break the nav cycle

**Files:**
- Delete: `DOCS.md`
- Modify: `docs/01-introduction.md` (Documentation Map ~L132 — ensure it points outward to README only as the entry, not in a loop); any README link to `DOCS.md`

- [ ] **Step 1: Confirm DOCS.md links are dead and it's unreferenced as the index**

Run: `rg -n 'DOCS\.md' README.md docs/`
Expected: note every reference (to fix in Step 3).

- [ ] **Step 2: Delete the file**

```bash
git rm DOCS.md
```

- [ ] **Step 3: Repoint any references to the real index**

Replace any `DOCS.md` link in README/docs with the README `## Documentation` section (the real index). In `docs/01-introduction.md`, make the Documentation Map's install entry point to `../README.md#install` only as the single entry (no return loop back into a README→docs/01→README ring — docs/01 is a leaf for "intro", README is the hub).

- [ ] **Step 4: Verify no dead DOCS.md references remain**

Run: `rg -n 'DOCS\.md' . -g '!docs/proposals/*'`
Expected: 0 matches.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: remove rotten DOCS.md (dead links, stale release/tool counts); fix nav"
```

---

### Task 10: EN/ES parity for the config/install changes

**Files:**
- Modify: `docs/es/11-configuracion.md`, `docs/es/01-introduccion.md`

- [ ] **Step 1: Mirror the embed-var rows into the Spanish config doc**

Add `DEVAI_EMBED_MAX_CHARS` / `DEVAI_EMBED_BATCH_SIZE` rows (Spanish descriptions) to `docs/es/11-configuracion.md` §3, and confirm `hard_truncate` is listed for the token strategy.

- [ ] **Step 2: Mirror the install-command fix**

Ensure the MCP configure command in `docs/es/01-introduccion.md` matches the canonical form used in README (resolve `--all` vs `claude` to the one the binary accepts — verify with `grep -n 'configure' cmd/devai/cmd/*.go`). Apply the same correction to the EN `docs/01-introduction.md` if it diverges.

- [ ] **Step 3: Verify parity**

Run: `rg -n 'DEVAI_EMBED_MAX_CHARS' docs/11-configuration.md docs/es/11-configuracion.md`
Expected: a match in both files.

- [ ] **Step 4: Commit**

```bash
git add docs/es/ docs/01-introduction.md
git commit -m "docs(es): mirror embed-var + MCP-command fixes; align configure command"
```

---

## Group C — WS3: Multi-repo central-store recipe

### Task 11: Author `docs/12-multi-repo-central-store.md` (+ ES mirror)

**Files:**
- Create: `docs/12-multi-repo-central-store.md`
- Create: `docs/es/12-multi-repo-store-central.md`
- Modify: `README.md` §Documentation (add the new doc to the index, "For Contributors" or a new "Multi-repo" bullet)

- [ ] **Step 1: Write the EN doc**

Sections (generalize the spec §1 Frente-3 topology — do NOT hardcode `/home/snaven10/...`; use `$WORKSPACE`):
1. **Mental model** — one central store, N repos feeding it via post-commit hooks, one MCP reading it. Include the ASCII topology diagram.
2. **Recipe** —
   - Pick the central store path (e.g. `$WORKSPACE/.devai/state` or `~/.local/share/devai/state`).
   - Per repo: `devai init` (now omits `state_dir` after Task 1) then set `DEVAI_STATE_DIR` for index/MCP, OR set `state_dir:` in each `config.yaml` to the shared path. Keep the **same model** across repos.
   - `DEVAI_STATE_DIR=<central> devai hooks install` in each repo — the hook now embeds the model + embed cap (Task 2).
   - Point the MCP client at the same `DEVAI_STATE_DIR` / `DEVAI_LOCAL_DB_PATH`.
3. **Worktree guards** — when a worktree shares the parent repo's hook via gitdir, add a `case "$(git rev-parse --show-toplevel)" in *<worktree-suffix>) exit 0 ;; esac` guard to avoid phantom indexing.
4. **Known gotchas** — model must match across repos (dimension mismatch corrupts the store; see Task 5 guard / footgun C); `DEVAI_EMBED_MAX_CHARS` prevents OOM on large chunks; never commit `.mcp.json` (credentials).
5. **Transition note** — after the Task 1/2 default changes, this recipe is mostly automatic; the doc covers both the manual and assisted paths.

- [ ] **Step 2: Write the ES mirror**

Translate Step 1 into `docs/es/12-multi-repo-store-central.md`. Add the cross-language link header both docs use.

- [ ] **Step 3: Add to the README documentation index**

Add a bullet under §Documentation: `- [Multi-repo Central Store](docs/12-multi-repo-central-store.md) — one shared index across many repos via hooks ([español](docs/es/12-multi-repo-store-central.md))`.

- [ ] **Step 4: Verify links resolve**

Run: `rg -n 'docs/12-multi-repo-central-store.md|docs/es/12-multi-repo-store-central.md' README.md && ls docs/12-multi-repo-central-store.md docs/es/12-multi-repo-store-central.md`
Expected: README references both; both files exist.

- [ ] **Step 5: Commit**

```bash
git add docs/12-multi-repo-central-store.md docs/es/12-multi-repo-store-central.md README.md
git commit -m "docs: add multi-repo central-store recipe (EN+ES)"
```

---

## Group D — Finalize

### Task 12: CHANGELOG entry + final read-through

**Files:**
- Modify: `CHANGELOG.md` (Unreleased section)

- [ ] **Step 1: Add the CHANGELOG entry**

Under `## [Unreleased]` (create if absent), list:
- `Changed: devai init no longer pins a per-repo state_dir (store resolves to central default).`
- `Changed: auto-index hook now embeds the active embedding model and DEVAI_EMBED_MAX_CHARS.`
- `Changed: install.sh fails loudly on missing ML wheel (use --allow-no-ml to override).`
- `Added: DEVAI_EMBED_MAX_CHARS injected into MCP client config by the installer.`
- `Added: docs/12 multi-repo central-store recipe (EN+ES).`
- `Docs: consolidated install/config into a single source of truth; removed rotten DOCS.md; fixed README/docs contradictions.`
- (If Task 5 implemented) `Fixed: devai index aborts on model/store dimension mismatch instead of corrupting.`

- [ ] **Step 2: Final linear read-through verification**

Run: `rg -n 'DOCS\.md' . -g '!docs/proposals/*' ; rg -n -i 'from source.*recommended' README.md`
Expected: both empty (no dead index, no contradiction).

Then read README top-to-bottom once: Install → Quick Start → Agent Setup → Configuration must flow without contradiction and without forcing a jump to another file for correct values.

- [ ] **Step 3: Run the full Go test suite**

Run: `go test ./cmd/... ./internal/...`
Expected: PASS (new init/hooks tests + existing config/router/mcp_configure tests).

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): record install/config overhaul"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** WS1 → Tasks 6–10; WS2 footguns F/D-E/A/G/C → Tasks 1/2/3/4/5; WS3 → Task 11; rollout/CHANGELOG → Task 12. All spec §3 items mapped.
- **Open decisions (spec §6) resolved:** pointers (Task 6/7), delete DOCS.md (Task 9), new docs/12 (Task 11), footgun C conservative+deferral-eligible (Task 5), single PR with separated commits (commit messages per task).
- **Type consistency:** `hookBlock` new 4-arg signature used consistently (Task 2 Steps 3–4, test in Step 1). `renderInitConfig(name, absPath)` consistent (Task 1). `--allow-no-ml`/`ALLOW_NO_ML` consistent (Task 3).
- **No placeholders:** every code step shows real before/after; doc steps give exact old→new values + verification greps. Task 5 is explicitly exploratory with a defined deferral path (per spec's risk note), not a vague placeholder.
