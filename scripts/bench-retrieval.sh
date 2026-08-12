#!/usr/bin/env bash
#
# Does the right answer reach the reranker at all?
#
# A reranker can only reorder what retrieval hands it. This measures, per query:
#
#   vector  — position of the expected file in vector-only results (top 100)
#   hybrid  — position in hybrid (vector + BM25) results (top 100)
#   then, for each reranker, its position after reranking the hybrid pool
#
# "—" means the file was not found at all in that pass. The expected file is a
# judgement call, listed beside each query so it can be argued with.
#
#   ./scripts/bench-retrieval.sh

set -uo pipefail

DEVCTX="${DEVCTX:-devctx}"
MODELS_DIR="${DEVCTX_MODELS:-$HOME/.local/share/devctx/models}"
CONFIG=".devctx/config.yaml"
POOL="${POOL:-20}"   # what the reranker sees; matches devctx-search's POOL

[[ -f "$CONFIG" ]] || { echo "Run this from an indexed repository." >&2; exit 1; }

# "query|expected file (substring)"
CASES=(
  "where do we run the indexing pipeline|devctx-index/src/pipeline.rs"
  "how are memories deduplicated|devctx-memory/src/lib.rs"
  "what happens when two processes open the database|devctx-cli/src/remote.rs"
  "reciprocal rank fusion of ranked lists|devctx-core/src/rank.rs"
  "cómo se configura el modelo de embeddings|devctx-core/src/config.rs"
  "instalar el hook que reindexa tras cada commit|devctx-cli/src/hooks.rs"
)

RERANKERS=(
  "bge-base|bge-base|"
  "bge-v2-m3|bge-v2-m3|"
  "jina-turbo|jina-turbo|"
  "jina-v2|jina-v2|"
  "MultiBERT-L-12|custom|$MODELS_DIR/ms-marco-MultiBERT-L-12"
  "TinyBERT-L-2|custom|$MODELS_DIR/ms-marco-TinyBERT-L-2-v2"
)

BACKUP="$(mktemp)"; cp "$CONFIG" "$BACKUP"
cleanup() { cp "$BACKUP" "$CONFIG"; rm -f "$BACKUP"; "$DEVCTX" serve --stop >/dev/null 2>&1; echo "· $CONFIG restored"; }
trap cleanup EXIT INT TERM

set_reranking() {
  python3 - "$1" "$2" "$CONFIG" <<'PY'
import sys, re
key, model_dir, path = sys.argv[1:4]
block = "reranking:\n  enabled: %s\n  model: %s\n" % (
    "false" if key == "off" else "true", "bge-base" if key == "off" else key)
if model_dir:
    block += "  model_dir: %s\n" % model_dir
s = open(path).read()
s, n = re.subn(r"reranking:\n(?:  \S.*\n)+", block, s, count=1)
open(path, "w").write(s if n else s + "\n" + block)
PY
}

# position of $2 in the JSON hit list on stdin, or "—"
position() {
  python3 -c "
import sys, json
want = sys.argv[1]
try:
    hits = json.load(sys.stdin)
except Exception:
    print('err'); sys.exit()
for i, h in enumerate(hits, 1):
    if want in h['file']:
        print(i); break
else:
    print('—')" "$1"
}

printf '%-46s %8s %8s' "query" "vector" "hybrid"
for r in "${RERANKERS[@]}"; do printf ' %14s' "${r%%|*}"; done
echo

# Retrieval passes need no model, so do them once up front.
declare -A VEC HYB
set_reranking "off" ""
"$DEVCTX" serve --stop >/dev/null 2>&1
"$DEVCTX" search "warmup" --limit 3 >/dev/null 2>&1
for case in "${CASES[@]}"; do
  q="${case%%|*}"; want="${case##*|}"
  VEC["$q"]=$("$DEVCTX" search "$q" --limit 100 --no-rerank --format json 2>/dev/null | position "$want")
  HYB["$q"]=$("$DEVCTX" search "$q" --limit 100 --no-rerank --hybrid --format json 2>/dev/null | position "$want")
done

# Then each reranker over the hybrid pool.
declare -A POS
for entry in "${RERANKERS[@]}"; do
  IFS='|' read -r label key dir <<< "$entry"
  [[ -n "$dir" && ! -d "$dir" ]] && { for c in "${CASES[@]}"; do POS["$label|${c%%|*}"]="skip"; done; continue; }
  set_reranking "$key" "$dir"
  "$DEVCTX" serve --stop >/dev/null 2>&1
  "$DEVCTX" search "warmup" --limit "$POOL" --hybrid >/dev/null 2>&1
  for case in "${CASES[@]}"; do
    q="${case%%|*}"; want="${case##*|}"
    POS["$label|$q"]=$("$DEVCTX" search "$q" --limit "$POOL" --hybrid --format json 2>/dev/null | position "$want")
  done
done

for case in "${CASES[@]}"; do
  q="${case%%|*}"
  printf '%-46s %8s %8s' "${q:0:46}" "${VEC[$q]}" "${HYB[$q]}"
  for entry in "${RERANKERS[@]}"; do
    label="${entry%%|*}"
    printf ' %14s' "${POS["$label|$q"]:-?}"
  done
  echo
done
echo
echo "vector/hybrid: position within the top 100 before reranking."
echo "reranker columns: position within the top $POOL after reranking the hybrid pool."
