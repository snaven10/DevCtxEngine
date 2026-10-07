#!/usr/bin/env bash
#
# Arnés de evaluación del grafo y del contexto (PLAN-009 TASK-001, DD-1).
#
# Por cada repo que aparece en los casos mide, contra su índice ya construido:
#   1. relevancia de `search` (híbrido y vectorial): Hit@5, Hit@10, MRR;
#   2. presencia del archivo/símbolo esperado en `devctx context` (el brief de 4096 tokens);
#   3. latencia de `impact` sobre una lista fija (5 corridas, p50/p95);
#   4. métricas del grafo (graph_metrics.sql) sobre una COPIA del DuckDB del proyecto;
#   5. precisión de resolución contra los gold edges del repo (score.py gold).
# Sale una tabla markdown lista para pegar en un `## Resultado` (stdout y $OUT/report.md).
#
# Solo consulta: no indexa, no borra, no toca el índice original (el DuckDB se COPIA; el
# serve vivo lo tiene abierto). Para medir con un binario o un HOME aparte, ver README.md.
#
# Variables (todas opcionales):
#   DEVCTX              binario a medir                         (default: devctx del PATH)
#   DEVCTX_EVAL_CASES   archivo de casos extra (repos privados, fuera del repo; Q-5)
#   DEVCTX_EVAL_GOLD    archivo de gold edges extra
#   DEVCTX_EVAL_IMPACT  archivo `repo|símbolo` extra para la lista de `impact`
#   DEVCTX_EVAL_ROOT    carpeta con los repos por nombre: $ROOT/<repo>
#                       (DevCtxEngine cae a este checkout si no está ahí)
#   DEVCTX_EVAL_OUT     carpeta de salida                       (default: mktemp -d)
#   DEVCTX_EVAL_LIMIT   resultados que se piden a search        (default: 20)
#   DEVCTX_EVAL_RUNS    corridas por símbolo de impact          (default: 5)
#   DEVCTX_EVAL_PY      intérprete python con el módulo duckdb  (default: python3, o `uv run --with duckdb`)
#   DEVCTX_EVAL_ONLY    lista de repos separados por coma: solo esos
#   DEVCTX_EVAL_BRANCH  rama a medir en el DuckDB si hay varias (default: la de más aristas)
#   DEVCTX_EVAL_STEPS   pasos a correr, separados por coma: relevance,impact,graph  (default: los tres;
#                       `graph` incluye las métricas SQL y los gold edges)

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
DEVCTX="${DEVCTX:-devctx}"
LIMIT="${DEVCTX_EVAL_LIMIT:-20}"
RUNS="${DEVCTX_EVAL_RUNS:-5}"
STEPS="${DEVCTX_EVAL_STEPS:-relevance,impact,graph}"
step() { [[ ",$STEPS," == *",$1,"* ]]; }
OUT="${DEVCTX_EVAL_OUT:-$(mktemp -d "${TMPDIR:-/tmp}/graph-eval.XXXXXX")}"
SCORE=(python3 "$HERE/score.py")

mkdir -p "$OUT"
log() { printf '[graph-eval] %s\n' "$*" >&2; }

command -v "$DEVCTX" >/dev/null 2>&1 || [[ -x "$DEVCTX" ]] || { log "no encuentro el binario: $DEVCTX"; exit 1; }

# --- intérprete con duckdb -------------------------------------------------------------
if [[ -n "${DEVCTX_EVAL_PY:-}" ]]; then
  read -r -a PYBIN <<<"$DEVCTX_EVAL_PY"
elif python3 -c 'import duckdb' >/dev/null 2>&1; then
  PYBIN=(python3)
elif command -v uv >/dev/null 2>&1; then
  PYBIN=(uv run --quiet --with duckdb python)
else
  PYBIN=()
  log "sin python3+duckdb ni uv: se salta graph_metrics.sql y los gold edges (instalar: uv pip install duckdb)"
fi

# --- casos ------------------------------------------------------------------------------
CASE_FILES=("$HERE/cases-devctx.txt")
[[ -n "${DEVCTX_EVAL_CASES:-}" ]] && CASE_FILES+=("$DEVCTX_EVAL_CASES")
GOLD_FILES=("$HERE/gold-devctx.txt")
[[ -n "${DEVCTX_EVAL_GOLD:-}" ]] && GOLD_FILES+=("$DEVCTX_EVAL_GOLD")
IMPACT_FILES=("$HERE/impact-devctx.txt")
[[ -n "${DEVCTX_EVAL_IMPACT:-}" ]] && IMPACT_FILES+=("$DEVCTX_EVAL_IMPACT")
for f in "${CASE_FILES[@]}" "${GOLD_FILES[@]}" "${IMPACT_FILES[@]}"; do
  [[ -f "$f" ]] || { log "no existe $f"; exit 1; }
done

# repos en orden de aparición
mapfile -t REPOS < <(cat "${CASE_FILES[@]}" | grep -v '^[[:space:]]*#' | awk -F'|' 'NF>=5 { r=$4; gsub(/^[ \t]+|[ \t]+$/, "", r); if (r != "" && !(r in s)) { s[r]=1; print r } }')
if [[ -n "${DEVCTX_EVAL_ONLY:-}" ]]; then
  mapfile -t REPOS < <(printf '%s\n' "${REPOS[@]}" | grep -Fx -f <(tr ',' '\n' <<<"$DEVCTX_EVAL_ONLY"))
fi
[[ ${#REPOS[@]} -gt 0 ]] || { log "no hay repos que medir"; exit 1; }

repo_dir() {
  local r="$1"
  if [[ -n "${DEVCTX_EVAL_ROOT:-}" && -d "$DEVCTX_EVAL_ROOT/$r" ]]; then
    echo "$DEVCTX_EVAL_ROOT/$r"
  elif [[ "$r" == "DevCtxEngine" ]]; then
    echo "$REPO_ROOT"
  fi
}

# ms entre dos $EPOCHREALTIME
elapsed_ms() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%d", (b - a) * 1000 }'; }

db_path_of() { # dir -> ruta del index.duckdb según la config del proyecto
  local d="$1" cfg="$1/.devctx/config.yaml" v
  v="$(awk '/^storage:/{s=1;next} /^[^ ]/{s=0} s && $1=="db_path:" {print $2}' "$cfg" | tr -d "'\"")"
  if [[ -n "$v" ]]; then echo "$v"; return; fi
  v="$(awk '/^state_dir:/ {print $2}' "$cfg" | tr -d "'\"")"
  if [[ -n "$v" ]]; then echo "$v/index.duckdb"; return; fi
  echo "$d/.devctx/state/index.duckdb"
}

REPORT="$OUT/report.md"
: >"$REPORT"
emit() { printf '%s\n' "$*" | tee -a "$REPORT"; }

emit "# graph-eval — $(date -u +%Y-%m-%dT%H:%MZ)"
emit ""
emit "- binario: \`$("$DEVCTX" --version 2>&1 | head -1)\` (\`$DEVCTX\`)"
emit "- casos: ${CASE_FILES[*]##*/} | gold: ${GOLD_FILES[*]##*/} | search --limit $LIMIT | impact × $RUNS"
emit ""

for repo in "${REPOS[@]}"; do
  dir="$(repo_dir "$repo")"
  if [[ -z "$dir" ]]; then
    log "$repo: sin carpeta (definí DEVCTX_EVAL_ROOT=<carpeta con \$ROOT/$repo>); se salta"
    emit "## $repo"; emit ""; emit "_sin carpeta: no medido_"; emit ""
    continue
  fi
  if [[ ! -f "$dir/.devctx/config.yaml" ]]; then
    log "$repo: $dir no tiene .devctx/config.yaml (¿indexado?); se salta"
    emit "## $repo"; emit ""; emit "_sin índice en $dir: no medido_"; emit ""
    continue
  fi
  W="$OUT/$repo"; mkdir -p "$W"
  log "== $repo ($dir)"
  emit "## $repo"
  emit ""

  # --- calentar el serve (una llamada) y registrar el estado ---
  ( cd "$dir" && "$DEVCTX" status ) >"$W/status.txt" 2>&1 || true
  emit "<details><summary>devctx status</summary>"; emit ""; emit '```'; emit "$(head -40 "$W/status.txt")"; emit '```'; emit ""; emit "</details>"; emit ""

  # --- 1+2. relevancia: search híbrido/vectorial y brief de context ---
  : >"$W/manifest.tsv"; : >"$W/lat-vector.txt"; : >"$W/lat-hybrid.txt"; : >"$W/lat-context.txt"
  i=0
  while IFS='|' read -r q files sym crepo lang _rest; do
    step relevance || break
    q="${q#"${q%%[![:space:]]*}"}"; q="${q%"${q##*[![:space:]]}"}"
    [[ -z "$q" || "${q:0:1}" == "#" ]] && continue
    crepo="${crepo#"${crepo%%[![:space:]]*}"}"; crepo="${crepo%"${crepo##*[![:space:]]}"}"
    [[ "$crepo" == "$repo" ]] || continue
    files="$(tr -d ' ' <<<"$files")"; sym="$(tr -d ' ' <<<"$sym")"; lang="$(tr -d ' ' <<<"$lang")"
    i=$((i + 1))
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$i" "${lang:-en}" "$q" "$files" "${sym:--}" "$repo" >>"$W/manifest.tsv"
    ( cd "$dir" && exec </dev/null || exit 1
      t0=$EPOCHREALTIME
      "$DEVCTX" search "$q" --limit "$LIMIT" --no-rerank --format json >"$W/$i.vector.json" 2>"$W/$i.vector.err"
      t1=$EPOCHREALTIME
      "$DEVCTX" search "$q" --limit "$LIMIT" --no-rerank --hybrid --format json >"$W/$i.hybrid.json" 2>"$W/$i.hybrid.err"
      t2=$EPOCHREALTIME
      "$DEVCTX" context "$q" --max-tokens 4096 --no-memories >"$W/$i.context.txt" 2>"$W/$i.context.err"
      t3=$EPOCHREALTIME
      elapsed_ms "$t0" "$t1" >>"$W/lat-vector.txt"; echo >>"$W/lat-vector.txt"
      elapsed_ms "$t1" "$t2" >>"$W/lat-hybrid.txt"; echo >>"$W/lat-hybrid.txt"
      elapsed_ms "$t2" "$t3" >>"$W/lat-context.txt"; echo >>"$W/lat-context.txt"
    )
  done < <(cat "${CASE_FILES[@]}")

  if [[ $i -gt 0 ]]; then
    emit "### Relevancia ($i casos)"; emit ""
    "${SCORE[@]}" retrieval --manifest "$W/manifest.tsv" --dir "$W" | tee -a "$REPORT"
    emit ""
    for m in vector hybrid context; do
      mapfile -t lat < <(grep -v '^$' "$W/lat-$m.txt")
      emit "- latencia $m (ms por llamada, serve caliente): $("${SCORE[@]}" pct "${lat[@]}")"
    done
    emit ""
  fi

  # --- 3. latencia de impact ---
  if step impact && cat "${IMPACT_FILES[@]}" | grep -q "^[[:space:]]*${repo}[[:space:]]*|"; then
  emit "### \`impact\` (depth 3, $RUNS corridas, segundos)"; emit ""
  emit "| símbolo | p50 | p95 | min | max | líneas de salida |"
  emit "|---|---|---|---|---|---|"
  while IFS='|' read -r irepo isym _r; do
    irepo="${irepo#"${irepo%%[![:space:]]*}"}"; irepo="${irepo%"${irepo##*[![:space:]]}"}"
    [[ -z "$irepo" || "${irepo:0:1}" == "#" || "$irepo" != "$repo" ]] && continue
    isym="${isym#"${isym%%[![:space:]]*}"}"; isym="${isym%"${isym##*[![:space:]]}"}"
    ts=(); lines=0
    for ((k = 0; k < RUNS; k++)); do
      t0=$EPOCHREALTIME
      out="$(cd "$dir" && "$DEVCTX" impact "$isym" 2>&1 </dev/null)"
      t1=$EPOCHREALTIME
      lines=$(wc -l <<<"$out")
      ts+=("$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.3f", b - a }')")
    done
    read -r _ p50 p95 mn mx _ <<<"$("${SCORE[@]}" pct "${ts[@]}" | sed 's/[a-z0-9]*=/ /g; s/^/x/')"
    emit "| \`$isym\` | $p50 | $p95 | $mn | $mx | $lines |"
  done < <(cat "${IMPACT_FILES[@]}")
  emit ""
  fi

  # --- 4+5. métricas del grafo y gold edges, sobre una copia del DuckDB ---
  step graph || continue
  if [[ ${#PYBIN[@]} -eq 0 ]]; then
    emit "_sin duckdb: métricas del grafo y gold edges no medidos_"; emit ""; continue
  fi
  src_db="$(db_path_of "$dir")"
  if [[ ! -f "$src_db" ]]; then
    emit "_no existe $src_db: métricas del grafo no medidas_"; emit ""; continue
  fi
  mkdir -p "$W/db"
  cp -f "$src_db" "$W/db/index.duckdb"
  [[ -f "$src_db.wal" ]] && cp -f "$src_db.wal" "$W/db/index.duckdb.wal"
  emit "### Métricas del grafo (copia de \`$(basename "$src_db")\`, $(du -h "$src_db" | cut -f1))"; emit ""
  "${PYBIN[@]}" - "$W/db/index.duckdb" "$HERE/graph_metrics.sql" "$W" "$repo" "${DEVCTX_EVAL_BRANCH:-}" <<'PY' | tee -a "$REPORT"
import sys, re, csv
import duckdb

db, sqlfile, outdir, repo, branch = sys.argv[1:6]
con = duckdb.connect(db, read_only=False)  # es una copia: se puede filtrar sin riesgo
tables = {r[0] for r in con.execute("select table_name from information_schema.tables").fetchall()}

# Si el índice tiene varias (repo, rama), se mide una sola: la indicada, o la de más aristas.
pairs = con.execute("select repo, branch, count(*) from graph_edges group by 1, 2 order by 3 desc").fetchall()
note = ""
if len(pairs) > 1:
    pick = next((p for p in pairs if branch and p[1] == branch), pairs[0])
    note = f"_(el índice tiene {len(pairs)} pares repo/rama; se midió `{pick[0]}` @ `{pick[1]}`)_"
    for t in ("graph_edges", "vectors", "edges", "symbols"):
        if t in tables:
            con.execute(f"delete from {t} where not (repo = ? and branch = ?)", [pick[0], pick[1]])

def statements(text):
    section, name, buf = "legacy", None, []
    for line in text.splitlines():
        s = line.strip()
        m = re.match(r"--\s*@section\s+(\w+)", s)
        if m: section = m.group(1); continue
        m = re.match(r"--\s*@metric\s+(\S+)", s)
        if m: name = m.group(1); buf = []; continue
        if s.startswith("--") or not s: continue
        buf.append(line)
        if s.endswith(";"):
            yield section, name, "\n".join(buf).rstrip(";")
            buf = []

have_new = "symbols" in tables and "edges" in tables
rows = []
for section, name, sql in statements(open(sqlfile, encoding="utf-8").read()):
    if section == "new" and not have_new:
        continue
    try:
        for k, v in con.execute(sql).fetchall():
            rows.append((section, name, k, v))
    except Exception as e:  # una métrica rota no tumba el resto
        rows.append((section, name, "ERROR", str(e).splitlines()[0][:120]))

print()
if note: print(note); print()
print("| sección | métrica | clave | valor |")
print("|---|---|---|---|")
for section, name, k, v in rows:
    print(f"| {section} | {name} | {k} | {v} |")
if not have_new:
    print()
    print("_Sección `new` (symbols/edges): no aplica, el índice es del esquema 0.9.0._")

with open(f"{outdir}/metrics.tsv", "w", encoding="utf-8", newline="") as fh:
    w = csv.writer(fh, delimiter="\t"); w.writerow(["section", "metric", "key", "value"]); w.writerows(rows)
# Para score.py (que no lee DuckDB): aristas y símbolos definidos.
with open(f"{outdir}/edges.tsv", "w", encoding="utf-8", newline="") as fh:
    w = csv.writer(fh, delimiter="\t"); w.writerow(["source", "target", "kind", "source_file", "line"])
    for r in con.execute("select source, target, kind, source_file, line from graph_edges").fetchall():
        w.writerow([("" if x is None else str(x).replace("\t", " ").replace("\n", " ")) for x in r])
# Esquema nuevo (TASK-005 en adelante): cada ocurrencia con su destino resuelto, para
# `score.py gold --new-edges` (precisión por `confidence`/`resolution`).
if have_new:
    with open(f"{outdir}/edges_new.tsv", "w", encoding="utf-8", newline="") as fh:
        w = csv.writer(fh, delimiter="\t")
        w.writerow(["kind", "file", "line", "dst_name", "dst_qualified", "confidence", "resolution", "external"])
        for r in con.execute("""select e.kind, e.file, e.line, e.dst_name, s.qualified, e.confidence,
                                       e.resolution, coalesce(e.external, false)
                                from edges e left join symbols s
                                  on s.id = e.dst_id and s.repo = e.repo and s.branch = e.branch
                                where e.kind in ('calls', 'instantiates')
                                  and coalesce(e.resolution, '') <> 'discarded'""").fetchall():
            w.writerow([("" if x is None else str(x).replace("\t", " ").replace("\n", " ")) for x in r])
with open(f"{outdir}/symbols.tsv", "w", encoding="utf-8", newline="") as fh:
    w = csv.writer(fh, delimiter="\t"); w.writerow(["symbol", "symbol_type", "file"])
    for r in con.execute("select distinct symbol, symbol_type, file from vectors where coalesce(symbol,'') <> '' and coalesce(memory_type,'') = ''").fetchall():
        w.writerow([("" if x is None else str(x)) for x in r])
PY
  emit ""

  if [[ -s "$W/edges.tsv" ]]; then
    emit "### Gold edges"; emit ""
    for g in "${GOLD_FILES[@]}"; do
      if grep -q "^$repo|" "$g"; then
        newe=()
        [[ -s "$W/edges_new.tsv" ]] && newe=(--new-edges "$W/edges_new.tsv")
        "${SCORE[@]}" gold --gold "$g" --edges "$W/edges.tsv" --symbols "$W/symbols.tsv" --repo "$repo" "${newe[@]}" | tee -a "$REPORT"
        emit ""
      fi
    done
  fi
done

log "listo. Reporte: $REPORT"
