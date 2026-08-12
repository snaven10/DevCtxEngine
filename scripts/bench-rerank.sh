#!/usr/bin/env bash
#
# Compare reranker models on your own repository and your own queries.
#
# Reranking is the most expensive stage of a search by two orders of magnitude,
# and which model is worth that cost depends on your corpus and your machine —
# so measure rather than take anyone's table for it.
#
#   ./scripts/bench-rerank.sh                 # run in a repo with a devctx index
#   QUERIES_FILE=my-queries.txt ./scripts/bench-rerank.sh
#
# Each model is timed on every query with a warm server, and its top hit is
# shown so you can judge the ordering, not just the latency. Your
# `.devctx/config.yaml` is restored on exit, including on Ctrl-C.

set -uo pipefail

DEVCTX="${DEVCTX:-devctx}"
MODELS_DIR="${DEVCTX_MODELS:-$HOME/.local/share/devctx/models}"
CONFIG=".devctx/config.yaml"
LIMIT="${LIMIT:-3}"

if [[ ! -f "$CONFIG" ]]; then
  echo "No $CONFIG here — run this from a repository you have indexed." >&2
  exit 1
fi

# --- what to compare -------------------------------------------------------
# "label|model key|model_dir"   — an empty model_dir means a fastembed built-in.
CANDIDATES=(
  "sin rerank|off|"
  "bge-base (built-in, 1.1 GB)|bge-base|"
  "bge-v2-m3 (built-in, multilingual)|bge-v2-m3|"
  "jina-turbo (built-in, English)|jina-turbo|"
  "jina-v2 (built-in, multilingual)|jina-v2|"
  "ms-marco-MultiBERT-L-12 (162 MB)|custom|$MODELS_DIR/ms-marco-MultiBERT-L-12"
  "ms-marco-TinyBERT-L-2 (4.8 MB)|custom|$MODELS_DIR/ms-marco-TinyBERT-L-2-v2"
)

# --- queries ---------------------------------------------------------------
# Use questions you would really ask; that is the only benchmark that matters.
if [[ -n "${QUERIES_FILE:-}" && -r "${QUERIES_FILE}" ]]; then
  mapfile -t QUERIES < "$QUERIES_FILE"
else
  QUERIES=(
    "where do we run the indexing pipeline"
    "how are memories deduplicated"
    "what happens when two processes open the database"
    "reciprocal rank fusion of ranked lists"
    "cómo se configura el modelo de embeddings"
    "instalar el hook que reindexa tras cada commit"
  )
fi

BACKUP="$(mktemp)"
cp "$CONFIG" "$BACKUP"
cleanup() {
  cp "$BACKUP" "$CONFIG"
  rm -f "$BACKUP"
  "$DEVCTX" serve --stop >/dev/null 2>&1
  echo "· $CONFIG restored"
}
trap cleanup EXIT INT TERM

set_reranking() {
  python3 - "$1" "$2" "$CONFIG" <<'PY'
import sys, re
key, model_dir, path = sys.argv[1], sys.argv[2], sys.argv[3]
block = "reranking:\n  enabled: %s\n  model: %s\n" % (
    "false" if key == "off" else "true",
    "bge-base" if key == "off" else key,
)
if model_dir:
    block += "  model_dir: %s\n" % model_dir
s = open(path).read()
s, n = re.subn(r"reranking:\n(?:  \S.*\n)+", block, s, count=1)
if not n:
    s += "\n" + block
open(path, "w").write(s)
PY
}

top_hit() {
  python3 -c "
import sys, json
try:
    hits = json.load(sys.stdin)
    print(f\"{hits[0]['file']}:{hits[0]['start_line']}\" if hits else '(no results)')
except Exception:
    print('(error)')"
}

now_ms() { echo $(( $(date +%s%N) / 1000000 )); }

printf '%s queries · limit %s · %s\n\n' "${#QUERIES[@]}" "$LIMIT" "$(pwd)"

for entry in "${CANDIDATES[@]}"; do
  IFS='|' read -r label key dir <<< "$entry"
  if [[ -n "$dir" && ! -d "$dir" ]]; then
    printf '### %s — skipped, no such directory: %s\n\n' "$label" "$dir"
    continue
  fi

  set_reranking "$key" "$dir"
  "$DEVCTX" serve --stop >/dev/null 2>&1

  # Warm the server, the embedder and the reranker before timing anything.
  local_start=$(now_ms)
  "$DEVCTX" search "warmup" --limit "$LIMIT" >/dev/null 2>&1
  warm=$(( $(now_ms) - local_start ))

  printf '### %s   (first call, everything cold: %s ms)\n' "$label" "$warm"
  total=0
  for q in "${QUERIES[@]}"; do
    [[ -z "$q" ]] && continue
    s=$(now_ms)
    out=$("$DEVCTX" search "$q" --limit "$LIMIT" --format json 2>/dev/null)
    ms=$(( $(now_ms) - s ))
    total=$(( total + ms ))
    printf '  %7s ms  %-44s  %s\n' "$ms" "$(printf '%s' "$out" | top_hit)" "$q"
  done
  printf '  %7s ms  (mean)\n\n' "$(( total / ${#QUERIES[@]} ))"
done
