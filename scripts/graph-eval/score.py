#!/usr/bin/env python3
"""Scorer del arnés de evaluación del grafo y del contexto (PLAN-009 TASK-001, DD-1).

Solo biblioteca estándar. Lo invoca run.sh; también sirve a mano.

  score.py retrieval --manifest M.tsv --dir D [--mode vector|hybrid ...]
      Hit@5, Hit@10, MRR por modo (search) y presencia en el brief (context), por caso y
      agregado, global y por idioma.
  score.py gold --gold G.txt --edges E.tsv --symbols S.tsv --repo NAME [--tol N]
      Precisión de resolución contra los gold edges, contando por "nombre calificado igual
      al esperado".
  score.py pct N [N ...]            p50 / p95 / min / max de una lista de números.

Todas las salidas son tablas markdown, listas para pegar en un `## Resultado`.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import re
import sys
from collections import defaultdict
from pathlib import Path


# --------------------------------------------------------------------------- utilidades

def pct_value(vals: list[float], p: float) -> float:
    """Percentil por rango más cercano (nearest-rank): con 5 corridas p95 es el máximo."""
    s = sorted(vals)
    k = max(1, math.ceil(p / 100.0 * len(s)))
    return s[k - 1]


def read_cases_manifest(path: Path) -> list[dict]:
    rows = []
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip():
                continue
            idx, lang, query, files, symbol, repo = (line.split("\t") + [""] * 6)[:6]
            rows.append({
                "idx": idx,
                "lang": lang or "en",
                "query": query,
                "files": [f.strip() for f in files.split(",") if f.strip()],
                "symbol": "" if symbol in ("", "-") else symbol,
                "repo": repo,
            })
    return rows


def load_hits(path: Path) -> list[dict] | None:
    """`search --format json`: 0.9.0 devuelve `{"results": [...]}`; versiones y modos con
    arreglo pelado o con `hits` también se aceptan."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if isinstance(data, dict):
        data = data.get("results", data.get("hits", []))
    return data if isinstance(data, list) else None


def position(hits: list[dict] | None, wanted: list[str]) -> int | None:
    """Posición (1-based) del primer hit cuyo archivo contiene alguno de los esperados."""
    if hits is None:
        return None
    for i, h in enumerate(hits, 1):
        f = str(h.get("file", ""))
        if any(w in f for w in wanted):
            return i
    return 0  # presente la respuesta, pero el archivo no apareció


def brief_presence(text: str, files: list[str], symbol: str) -> tuple[bool, bool, int]:
    """(archivo esperado en el brief, símbolo esperado en el brief, tokens estimados).

    El brief de `devctx context` marca cada bloque de código con `// <archivo>:<línea>`; no
    informa los tokens que usó, así que se estiman como caracteres / 4 (el mismo orden de
    magnitud que el presupuesto). El archivo se busca como substring en cualquier parte del
    texto y el símbolo como palabra completa.
    """
    has_file = any(f in text for f in files)
    has_sym = bool(symbol) and re.search(r"(?<![A-Za-z0-9_])" + re.escape(symbol) + r"(?![A-Za-z0-9_])", text) is not None
    return has_file, has_sym, len(text) // 4


def mrr_of(positions: list[int | None]) -> float:
    vals = [(1.0 / p if p else 0.0) for p in positions if p is not None]
    return sum(vals) / len(vals) if vals else 0.0


def pct(n: int, d: int) -> str:
    return f"{100.0 * n / d:.1f} %" if d else "n/a"


# --------------------------------------------------------------------------- retrieval

def cmd_retrieval(a: argparse.Namespace) -> int:
    cases = read_cases_manifest(Path(a.manifest))
    d = Path(a.dir)
    modes = a.modes.split(",")
    per_case = []
    agg: dict[str, list] = defaultdict(list)
    for c in cases:
        row = {"case": c, "pos": {}, "brief": None}
        for m in modes:
            hits = load_hits(d / f"{c['idx']}.{m}.json")
            row["pos"][m] = position(hits, c["files"])
        ctx = d / f"{c['idx']}.context.txt"
        if ctx.exists():
            row["brief"] = brief_presence(ctx.read_text(encoding="utf-8", errors="replace"), c["files"], c["symbol"])
        per_case.append(row)

    def ranks(rows, m):
        return [r["pos"][m] for r in rows if r["pos"].get(m) is not None]

    def summary(rows, label):
        out = [f"| {label} | {len(rows)} |"]
        for m in modes:
            p = ranks(rows, m)
            n = len(p)
            h5 = sum(1 for x in p if x and x <= 5)
            h10 = sum(1 for x in p if x and x <= 10)
            out.append(f" {h5}/{n} | {h10}/{n} | {mrr_of(p):.3f} |")
        briefs = [r["brief"] for r in rows if r["brief"] is not None]
        nb = len(briefs)
        out.append(f" {sum(1 for b in briefs if b[0])}/{nb} |")
        withsym = [r["brief"] for r in rows if r["brief"] is not None and r["case"]["symbol"]]
        out.append(f" {sum(1 for b in withsym if b[1])}/{len(withsym)} |")
        toks = [b[2] for b in briefs if b[2]]
        out.append(f" {int(sum(toks) / len(toks)) if toks else 'n/a'} |")
        return "".join(out)

    hdr = "| grupo | casos |"
    sep = "|---|---|"
    for m in modes:
        hdr += f" {m} Hit@5 | {m} Hit@10 | {m} MRR |"
        sep += "---|---|---|"
    hdr += " brief: archivo | brief: símbolo | brief: tokens est. medios |"
    sep += "---|---|---|"
    print(hdr)
    print(sep)
    print(summary(per_case, "todos"))
    for lang in sorted({r["case"]["lang"] for r in per_case}):
        print(summary([r for r in per_case if r["case"]["lang"] == lang], lang))
    repos = sorted({r["case"]["repo"] for r in per_case})
    if len(repos) > 1:
        for rp in repos:
            print(summary([r for r in per_case if r["case"]["repo"] == rp], rp))
    print()
    print("Por caso (posición del archivo esperado; `—` = no apareció en el top pedido; "
          "`F`/`S` = archivo/símbolo esperado presente en el brief):")
    print()
    h2 = "| # | idioma | consulta |"
    s2 = "|---|---|---|"
    for m in modes:
        h2 += f" {m} |"
        s2 += "---|"
    h2 += " brief | tokens |"
    s2 += "---|---|"
    print(h2)
    print(s2)
    for r in per_case:
        c = r["case"]
        cells = []
        for m in modes:
            p = r["pos"].get(m)
            cells.append("err" if p is None else ("—" if p == 0 else str(p)))
        b = r["brief"]
        if b is None:
            bs, tk = "n/a", ""
        else:
            bs = ("F" if b[0] else "·") + ("S" if b[1] else ("·" if c["symbol"] else "-"))
            tk = str(b[2] or "")
        q = c["query"] if len(c["query"]) <= 60 else c["query"][:57] + "..."
        print(f"| {c['idx']} | {c['lang']} | {q} | " + " | ".join(cells) + f" | {bs} | {tk} |")
    return 0


# --------------------------------------------------------------------------- gold edges

_GENERICS = re.compile(r"<[^<>]*>")


def norm_target(t: str) -> str:
    """Forma comparable: `::` -> `.`, sin genéricos, sin `this.`/`self.`, sin `new `."""
    t = t.strip()
    while True:
        u = _GENERICS.sub("", t)
        if u == t:
            break
        t = u
    t = t.replace("::", ".")
    t = re.sub(r"^(this|self)\.", "", t)
    t = re.sub(r"^new\s+", "", t)
    return t


def leaf(t: str) -> str:
    return t.rsplit(".", 1)[-1]


def qual(t: str) -> str:
    return t.rsplit(".", 1)[0] if "." in t else ""


def lang_of(path: str) -> str:
    ext = path.rsplit(".", 1)[-1].lower() if "." in path else ""
    return {"java": "Java", "ts": "TS", "tsx": "TS", "js": "TS", "py": "Python", "rs": "Rust", "go": "Go"}.get(ext, ext or "?")


def load_tsv(path: Path) -> list[dict]:
    with path.open(encoding="utf-8", newline="") as fh:
        return list(csv.DictReader(fh, delimiter="\t"))


def read_gold(path: Path, repo: str | None) -> list[dict]:
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        f = [x.strip() for x in line.split("|")]
        f += [""] * (5 - len(f))
        r, loc, dst, ext, dfile = f[:5]
        if repo and r != repo:
            continue
        file, _, ln = loc.rpartition(":")
        out.append({"repo": r, "file": file, "line": int(ln), "dst": dst,
                    "external": ext.lower() == "external", "dst_file": dfile})
    return out


def cmd_gold(a: argparse.Namespace) -> int:
    sites = read_gold(Path(a.gold), a.repo)
    if not sites:
        print(f"(sin gold edges para `{a.repo}`)")
        return 0
    edges = load_tsv(Path(a.edges))
    syms = load_tsv(Path(a.symbols))
    defined_leaf = {s["symbol"] for s in syms if s["symbol"] and s.get("symbol_type") in
                    ("method", "function", "constructor")}
    defined_types = {s["symbol"] for s in syms if s["symbol"] and s.get("symbol_type") in
                     ("class", "struct", "interface", "trait", "enum", "impl")}
    # Los métodos definidos aparecen como `Clase.metodo` en graph_edges.source.
    defined_qual = {e["source"] for e in edges}
    by_file: dict[str, list[dict]] = defaultdict(list)
    for e in edges:
        by_file[e["source_file"]].append(e)

    def internal(target: str) -> bool:
        t = norm_target(target)
        if "." in t:
            return t in defined_qual or (leaf(t) in defined_leaf and qual(t).rsplit(".", 1)[-1] in defined_types)
        return t in defined_leaf or t in defined_types

    def judge(site: dict, tol: int) -> tuple[str, str]:
        cand = [e for e in by_file.get(site["file"], []) if abs(int(e["line"]) - site["line"]) <= tol]
        want = norm_target(site["dst"])
        if site["external"]:
            same = [e for e in cand if leaf(norm_target(e["target"])) == leaf(want)]
            if not cand:
                return "missing", ""
            if not same:
                return "missing", "; ".join(e["target"] for e in cand[:3])
            bad = [e for e in same if internal(e["target"])]
            if bad:
                return "wrong", bad[0]["target"]
            return "correct", same[0]["target"]
        if not cand:
            return "missing", ""
        exact = [e for e in cand if norm_target(e["target"]) == want]
        if exact:
            return "correct", exact[0]["target"]
        same_leaf = [e for e in cand if leaf(norm_target(e["target"])) == leaf(want)]
        if not same_leaf:
            return "missing", "; ".join(e["target"] for e in cand[:3])
        # Mismo nombre de método pero otra forma: pelado = sin decidir; calificado distinto = error.
        qualified = [e for e in same_leaf if "." in norm_target(e["target"])]
        if qualified:
            return "wrong", qualified[0]["target"]
        return "unqualified", same_leaf[0]["target"]

    res = []
    for s in sites:
        st0, got0 = judge(s, 0)
        st, got = judge(s, a.tol)
        res.append((s, st0, st, got))

    def table(rows):
        n = len(rows)
        c = sum(1 for r in rows if r[2] == "correct")
        w = sum(1 for r in rows if r[2] == "wrong")
        u = sum(1 for r in rows if r[2] == "unqualified")
        m = sum(1 for r in rows if r[2] == "missing")
        c0 = sum(1 for r in rows if r[1] == "correct")
        return (n, c, w, u, m, c0)

    print(f"Gold edges de `{a.repo}`: {len(res)} sitios; línea exacta o ±{a.tol} (el grafo viejo guarda "
          "solo la primera ocurrencia de cada par fuente→destino).")
    print()
    print("| grupo | sitios | correcto | error (resuelve a otro) | sin calificar | sin arista | "
          "precisión | precisión sobre decididos | correcto a línea exacta |")
    print("|---|---|---|---|---|---|---|---|---|")
    groups = [("todos", res)]
    for lg in sorted({lang_of(r[0]["file"]) for r in res}):
        groups.append((lg, [r for r in res if lang_of(r[0]["file"]) == lg]))
    groups.append(("internos (esperado definido en el repo)", [r for r in res if not r[0]["external"]]))
    groups.append(("externos (esperado `external`)", [r for r in res if r[0]["external"]]))
    for label, rows in groups:
        n, c, w, u, m, c0 = table(rows)
        print(f"| {label} | {n} | {c} | {w} | {u} | {m} | {pct(c, n)} | {pct(c, c + w)} | {c0} |")
    print()
    print("Para sitios `external` \"correcto\" significa que el grafo NO lo resolvió a una definición "
          "del repo por nombre (el esquema 0.9.0 no marca externos; es una cota).")
    print()
    print("| archivo:línea | esperado | estado | arista en 0.9.0 |")
    print("|---|---|---|---|")
    for s, st0, st, got in res:
        exp = ("`external` " if s["external"] else "") + f"`{s['dst']}`"
        short = s["file"].rsplit("/", 1)[-1]
        print(f"| {short}:{s['line']} | {exp} | {st} | {('`' + got + '`') if got else '—'} |")
    return 0


# --------------------------------------------------------------------------- latencias

def cmd_pct(a: argparse.Namespace) -> int:
    vals = [float(x) for x in a.values]
    if not vals:
        print("n/a")
        return 1
    print(f"p50={pct_value(vals, 50):.3f} p95={pct_value(vals, 95):.3f} min={min(vals):.3f} "
          f"max={max(vals):.3f} n={len(vals)}")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("retrieval")
    r.add_argument("--manifest", required=True)
    r.add_argument("--dir", required=True)
    r.add_argument("--modes", default="vector,hybrid")
    r.set_defaults(fn=cmd_retrieval)
    g = sub.add_parser("gold")
    g.add_argument("--gold", required=True)
    g.add_argument("--edges", required=True)
    g.add_argument("--symbols", required=True)
    g.add_argument("--repo", required=True)
    g.add_argument("--tol", type=int, default=2)
    g.set_defaults(fn=cmd_gold)
    p = sub.add_parser("pct")
    p.add_argument("values", nargs="*")
    p.set_defaults(fn=cmd_pct)
    a = ap.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
