#!/bin/sh
# Indexing progress for every DevCtxEngine project currently working.
#
# Drop this anywhere and point a status line at it. It prints one line while a
# run is in flight and nothing at all otherwise, so it can be appended to an
# existing status line without taking up room the rest of the time.
#
# Printed into the Claude Code status line, which re-runs on every render — so
# this has to cost almost nothing when nothing is indexing, which is nearly
# always. Two things make that true: the project list is cached, and each
# server is asked with a timeout short enough that an unreachable one costs a
# fifth of a second rather than a hang.
#
# Silence is the normal output. It prints only while a run is in flight.

CACHE="${TMPDIR:-/tmp}/devctx-statusline-projects"
CACHE_TTL=300   # seconds; a project list changes when someone runs `init`
DEVCTX="${DEVCTX_BIN:-$HOME/.local/bin/devctx}"

[ -x "$DEVCTX" ] || exit 0

# Refresh the cached project paths at most once every CACHE_TTL. `projects
# list` reaches the central daemon, which is far too heavy to do per render.
stale=1
if [ -f "$CACHE" ]; then
  now=$(date +%s)
  then_=$(date -r "$CACHE" +%s 2>/dev/null || echo 0)
  [ $((now - then_)) -lt "$CACHE_TTL" ] && stale=0
fi
if [ "$stale" = 1 ]; then
  # Never block a render on this: if the daemon is slow or down, keep the old
  # list (or none) rather than stalling the prompt.
  timeout 3 "$DEVCTX" projects list --format json 2>/dev/null \
    | sed -n 's/.*"path" *: *"\([^"]*\)".*/\1/p' > "$CACHE.tmp" 2>/dev/null
  [ -s "$CACHE.tmp" ] && mv "$CACHE.tmp" "$CACHE" || rm -f "$CACHE.tmp"
fi
[ -f "$CACHE" ] || exit 0

out=""
while IFS= read -r proj; do
  [ -n "$proj" ] || continue
  sf="$proj/.devctx/state/serve.json"
  [ -f "$sf" ] || continue
  addr=$(sed -n 's/.*"addr" *: *"\([^"]*\)".*/\1/p' "$sf")
  [ -n "$addr" ] || continue

  p=$(curl -s -m 0.25 "http://$addr/index/progress" 2>/dev/null) || continue
  case "$p" in *'"running":true'*) ;; *) continue ;; esac

  done_=$(printf '%s' "$p" | sed -n 's/.*"done" *: *\([0-9]*\).*/\1/p')
  total=$(printf '%s' "$p" | sed -n 's/.*"total" *: *\([0-9]*\).*/\1/p')
  name=$(basename "$proj")
  if [ -n "$total" ] && [ "$total" -gt 0 ] 2>/dev/null; then
    pct=$(( done_ * 100 / total ))
    # A short bar reads faster than a number at a glance, which is the whole
    # job of a status line; the count stays for when the glance is not enough.
    filled=$(( pct / 10 ))
    bar=""
    i=0
    while [ $i -lt 10 ]; do
      if [ $i -lt $filled ]; then bar="${bar}█"; else bar="${bar}░"; fi
      i=$((i + 1))
    done
    out="${out}${out:+  }${name} ${bar} ${pct}% (${done_}/${total})"
  else
    out="${out}${out:+  }${name}"
  fi
done < "$CACHE"

# The label goes in front once rather than beside each repository: with two or
# three indexing at the same time, repeating the verb is what pushes the line
# past the width anyone reads.
[ -n "$out" ] && printf '⚙ indexando %s' "$out"
