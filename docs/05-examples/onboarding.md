# Example: Onboarding to an Unfamiliar Codebase

> 🇪🇸 [Leer en español](../es/05-ejemplos/incorporacion.md)

Your first day in a repository nobody has time to explain to you.

---

## Step 0 — Index it

```bash
cd the-repository
devctx init
devctx index --full
```

`init` asks for the embedding model. **Answer it carefully** — it is the one
decision that cannot be changed later without re-indexing everything and
re-embedding every memory. If the code or its comments are not in English, pick
a multilingual model. See [Models and Tuning](../09-models-and-tuning.md).

Then check what you got:

```bash
devctx status
```

Files, chunks, symbols, model, freshness. A symbol count near zero in a large
repository means the code is in a language that is being indexed as text rather
than parsed — see the language table in
[Symbol Graph](../03-core-concepts/symbol-graph.md).

## Step 1 — Find the edges of the system

Start where the outside world touches it:

```bash
devctx routes
```

Routes tell you what the system *does* far faster than any file tree. Seven
frameworks are recognised — FastAPI, Flask, Express, NestJS, Spring, Quarkus,
Angular.

If it is not a web service, start from the entry points instead:

```
search("main entry point application startup")
```

## Step 2 — Ask the system what it is about

```
search("authentication")
search("database connection and transactions")
search("configuration loading")
search("background jobs and scheduling")
```

Four searches on the concepts every system has will map most of it. You are not
reading yet — you are learning which files exist and what they are called.

Note the **file** and **symbol** fields more than the code. At this stage you are
building a vocabulary, and the vocabulary is what makes every later query work.

## Step 3 — Find out what the team already knows

This is the step that distinguishes onboarding here from onboarding with grep:

```
memory_context()
```

The most recent memories, with no query — for exactly this situation, where you
do not yet know enough to ask a question. If the team has been recording
decisions, this is the fastest orientation available.

Then, on anything that looked important in step 2:

```
memories_by_file("src/payments/processor.rs")
```

The knowledge recorded against that file: why it is structured that way, what
bit someone last time.

## Step 4 — Go deep on one thing

Pick the subsystem you will actually work in and get a real brief:

```
build_context("how does authentication work end to end", max_tokens=8000)
```

Raise the budget for onboarding. The default 4096 is tuned for a focused
question; you are asking a broad one, and the brief tells you honestly when it
truncated:

```
[devctx] 7 further item(s) did not fit in 8000 tokens.
```

## Step 5 — Trace one path by hand

Understanding comes from following one request all the way through, not from
reading summaries.

```
routes_for_handler("login")     → which URL reaches this
read_symbol("login")            → the handler itself
impact_analysis("login")        → what it calls, transitively
```

One trace teaches you more about the conventions of a codebase than ten
searches.

## Step 6 — Write down what you learned

Your confusion today is data. It expires in about a week, when the codebase
starts feeling normal and you can no longer remember what was surprising.

```bash
devctx remember "Auth uses two token types: a short-lived access token validated
in middleware, and an opaque refresh token stored server-side. The middleware
does NOT hit the database — that is deliberate, for latency — so a revoked user
stays valid until the access token expires." \
  --type architecture \
  --topic auth-token-model \
  --files src/auth/middleware.rs,src/auth/tokens.rs
```

Write down specifically **what surprised you**. That is the part the code does
not say and the part the next newcomer will also trip on.

## The progression

| Phase | Question | Tool |
|---|---|---|
| 0 | What is here? | `init`, `index --full`, `status` |
| 1 | What does it do? | `routes`, `search` for entry points |
| 2 | What is it made of? | `search` on universal concepts |
| 3 | What does the team know? | `memory_context`, `memories_by_file` |
| 4 | How does my subsystem work? | `build_context` with a raised budget |
| 5 | How does one request flow? | `routes_for_handler` → `read_symbol` → `impact_analysis` |
| 6 | What did I learn? | `remember --files --topic` |

## Checklist for the first day

- [ ] `devctx init` — with the model chosen deliberately, not accepted blindly
- [ ] `devctx index --full`
- [ ] `devctx status` — symbol count sane for the repository's size?
- [ ] `devctx routes` — or entry points if it is not a service
- [ ] `memory_context` — is there existing team knowledge?
- [ ] One `build_context` on your subsystem
- [ ] One request traced end to end
- [ ] At least one memory saved, with `--files`
- [ ] `devctx hooks install` — so the index stays current without you thinking
      about it
