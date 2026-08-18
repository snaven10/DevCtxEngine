# Retrieval Payload Benchmark

> 🇪🇸 [Leer en español](es/10-benchmark-tokens-mcp.md)

How many tokens it costs to *get to* an answer, measured three ways on the same
questions.

---

## What this measures, and what it does not

This measures **retrieval payload**: the size of what lands in a model's context
before it starts reasoning. That is the mechanism by which filtered retrieval
saves money, and it is directly measurable and reproducible.

It does **not** measure end-to-end session cost. That depends on the model, the
task, how many turns the agent takes and how caching behaves — none of which
this page can control, and all of which would make the numbers unreproducible.
If you want session cost, measure your own with your client's cost reporting.

Token counts below use the same ~4 characters per token heuristic the codebase
uses internally. It is an estimate, applied identically to all three columns, so
the *ratios* hold even where the absolute numbers drift.

## Method

Three questions about this repository, each answered three ways:

1. **grep-and-read** — the naive approach: search for likely keywords, open
   every file that matches. Measured as the total size of all matching Rust
   files.
2. **`build_context`** — one budgeted brief, `--max-tokens 4096`.
3. **`search --limit 5`** — just the ranked chunks, as JSON.

Reproduce it with the commands in the last section.

## Results

Measured on this repository: 128 files, 2,333 chunks, `ml-granite`.

### "How does reciprocal rank fusion combine the two retrievers?"

| Approach | Payload | Est. tokens | vs. grep |
|---|---|---|---|
| grep-and-read (29 files) | 542,591 chars | ~135,600 | — |
| `build_context` | 15,518 chars | ~3,900 | **35× less** |
| `search --limit 5` | 3,236 chars | ~800 | **168× less** |

### "Why is the WAL checkpointed before the server exits?"

| Approach | Payload | Est. tokens | vs. grep |
|---|---|---|---|
| grep-and-read (56 files) | 939,148 chars | ~234,800 | — |
| `build_context` | 16,228 chars | ~4,100 | **58× less** |
| `search --limit 5` | 4,153 chars | ~1,000 | **226× less** |

### "How are memories deduplicated when saving?"

| Approach | Payload | Est. tokens | vs. grep |
|---|---|---|---|
| grep-and-read (22 files) | 518,519 chars | ~129,600 | — |
| `build_context` | 15,903 chars | ~4,000 | **33× less** |
| `search --limit 5` | 6,517 chars | ~1,600 | **80× less** |

## Reading these numbers

**The grep column is the honest villain.** Its cost is driven by how many files
match a keyword, not by how much of them is relevant. The WAL question is the
worst case precisely because "checkpoint" and "wal" appear in tests, comments and
unrelated modules — 56 files, nearly a megabyte, to answer a question whose
answer is one doc comment.

**`build_context` is flat.** Roughly 4,000 tokens regardless of question,
because that is what you asked for. This is the point of a budget: cost is a
parameter you set, not an outcome you discover. It also reports what did not
fit, so a flat cost does not hide a truncated answer.

**`search` is cheapest but answers a narrower question.** It returns ranked code
and nothing else — no prior decisions, no memories recorded against those files.
For "where is the code", that is exactly right. For "what should I know before
changing this", it is not.

**The gap widens with repository size.** `build_context` is bounded by its
budget; grep-and-read grows with the number of keyword matches. On a repository
ten times this size, column one grows and column two does not.

## The caveat that matters

A real agent does not read all 29 files. It reads a few, guesses, reads a few
more. So the grep column is an upper bound on one strategy, not a prediction of
what any particular agent would spend.

What it does show correctly is the *shape* of the problem: with keyword search,
the cost of finding an answer scales with how common the words are, and the
agent has no way to know in advance which of the 29 files is the one. Filtered
retrieval replaces that search with a bounded, ranked payload.

## Reproduce it

```bash
# 1. grep-and-read upper bound
rg -l -i 'wal|checkpoint|ART' crates/ --type rust > /tmp/hits.txt
wc -l < /tmp/hits.txt                    # files an agent might open
xargs wc -c < /tmp/hits.txt | tail -1    # total characters

# 2. one budgeted brief
devctx context "why is the WAL checkpointed before the server exits" \
  --max-tokens 4096 | wc -c

# 3. ranked chunks only
devctx search "why is the WAL checkpointed before the server exits" \
  --limit 5 --format json | wc -c
```

Divide characters by 4 for the token estimate. Run it against your own
repository — the ratios are what transfer, not the absolute numbers.
