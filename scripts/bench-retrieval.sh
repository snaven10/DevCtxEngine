#!/usr/bin/env bash
#
# Does the file that answers the question reach the top of the results?
#
# Reranking can only reorder what retrieval hands it, so this measures
# retrieval on its own: for each query, the position of the file that should
# answer it, in vector and in hybrid mode. "—" means it was not in the top N.
#
# Targets live in scripts/bench-queries.txt and are written before measuring.
# Each line is `query | file that answers it | language`, and the summary breaks
# results down by language: the corpus is written in English, so a Spanish
# question is the harder case and an average over both would hide it.
#
#   ./scripts/bench-retrieval.sh              # summary + per-query table
#   LIMIT=50 ./scripts/bench-retrieval.sh     # look deeper

set -uo pipefail
DEVCTX="${DEVCTX:-devctx}"
LIMIT="${LIMIT:-20}"
CASES="${CASES:-scripts/bench-queries.txt}"

[[ -f .devctx/config.yaml ]] || { echo "Run from an indexed repository." >&2; exit 1; }
[[ -f "$CASES" ]] || { echo "No $CASES" >&2; exit 1; }

pos() {
  python3 -c "
import sys, json
want = sys.argv[1]
try: hits = json.load(sys.stdin)
except Exception: print('err'); raise SystemExit
for i, h in enumerate(hits, 1):
    if want in h['file']:
        print(i); break
else: print('—')" "$1"
}

printf '%-52s %6s %6s\n' "query" "vector" "hybrid"
printf '%-52s %6s %6s\n' "$(printf '%.0s-' {1..52})" "------" "------"

declare -A found=() total=()
vh=0; hh=0; n=0
while IFS='|' read -r q want lang; do
  [[ -z "${q// }" || "${q:0:1}" == "#" ]] && continue
  lang="${lang:-en}"
  n=$((n+1)); total[$lang]=$(( ${total[$lang]:-0} + 1 ))
  v=$("$DEVCTX" search "$q" --limit "$LIMIT" --no-rerank --format json 2>/dev/null | pos "$want")
  h=$("$DEVCTX" search "$q" --limit "$LIMIT" --no-rerank --hybrid --format json 2>/dev/null | pos "$want")
  if [[ "$v" != "—" && "$v" != "err" ]]; then
    vh=$((vh+1)); found[$lang]=$(( ${found[$lang]:-0} + 1 ))
  fi
  [[ "$h" != "—" && "$h" != "err" ]] && hh=$((hh+1))
  printf '%-52s %6s %6s\n' "${q:0:52}" "$v" "$h"
done < "$CASES"

echo
printf 'found in top %s:  vector %s/%s   hybrid %s/%s\n' "$LIMIT" "$vh" "$n" "$hh" "$n"
# Per language, because an average hides the case the whole reranker argument
# was about: a query asked in Spanish against a corpus written in English.
for lang in "${!total[@]}"; do
  printf '  %s: %s/%s vector\n' "$lang" "${found[$lang]:-0}" "${total[$lang]}"
done
