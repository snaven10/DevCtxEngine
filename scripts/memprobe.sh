#!/usr/bin/env bash
# memprobe.sh — sample the memory of a process over time and summarize it.
# PLAN-010 DD-1: the measurement method every memory task uses. Linux only
# (/proc); bash + awk, nothing else.
#
# Usage:
#   memprobe.sh --pid PID  [--interval 0.2] [--duration SECS] [--at 5,30,120] [--out FILE]
#                 [--until-exit PID2]   # also stop when PID2 (e.g. the client) exits
#   memprobe.sh [opts] -- COMMAND [ARGS...]     # runs COMMAND, samples its PID
#
# Samples /proc/PID/status (VmRSS, RssAnon, RssFile, VmHWM) and
# /proc/PID/smaps_rollup (Pss) every --interval seconds (default 0.2) until the
# process exits or --duration elapses. --at lists seconds-from-start at which to
# also report the value ("rest" figures). --out keeps the raw TSV.
#
# Summary (MiB): max and final RSS/anon/file/Pss, the kernel's VmHWM at the end,
# and RSS at each --at second. A PID that is not yours to read (or gone) is an
# error, never a row of zeros.
set -u

interval=0.2; duration=0; at=""; out=""; pid=""; until_pid=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pid) pid="$2"; shift 2;;
    --interval) interval="$2"; shift 2;;
    --duration) duration="$2"; shift 2;;
    --at) at="$2"; shift 2;;
    --out) out="$2"; shift 2;;
    --until-exit) until_pid="$2"; shift 2;;
    --) shift; break;;
    -h|--help) sed -n '2,19p' "$0"; exit 0;;
    *) echo "memprobe: unknown option $1" >&2; exit 2;;
  esac
done

if [ "$(uname -s)" != "Linux" ]; then
  echo "memprobe: unsupported OS (needs /proc)" >&2; exit 3
fi

child=""
if [ $# -gt 0 ]; then
  "$@" &
  child=$!
  pid=$child
fi
if [ -z "$pid" ]; then
  echo "memprobe: give --pid PID or -- COMMAND" >&2; exit 2
fi
if [ ! -r "/proc/$pid/status" ]; then
  echo "memprobe: cannot read /proc/$pid/status" >&2; exit 1
fi

raw=$(mktemp -p "${TMPDIR:-/var/tmp}" memprobe.XXXXXX)
# Monotonic seconds: /proc/uptime does not jump when the wall clock is stepped.
mono() { read -r up _ </proc/uptime; echo "$up"; }
start=$(mono)
sample() {
  local st sm
  st=$(awk '/^VmRSS:/{r=$2} /^RssAnon:/{a=$2} /^RssFile:/{f=$2} /^VmHWM:/{h=$2} END{if(r=="")exit 1; print r"\t"a"\t"f"\t"h}' "/proc/$pid/status" 2>/dev/null) || return 1
  sm=$(awk '/^Pss:/{print $2; exit}' "/proc/$pid/smaps_rollup" 2>/dev/null)
  now=$(mono)
  printf '%s\t%s\t%s\n' "$(awk -v n="$now" -v s="$start" 'BEGIN{printf "%.2f", n-s}')" "$st" "${sm:-0}" >>"$raw"
}

while sample; do
  if [ -n "$until_pid" ] && [ ! -d "/proc/$until_pid" ]; then break; fi
  if [ "$duration" != "0" ] && awk -v n="$(mono)" -v s="$start" -v d="$duration" 'BEGIN{exit !(n-s>=d)}'; then
    break
  fi
  sleep "$interval"
done
[ -n "$child" ] && wait "$child" 2>/dev/null
status=$?

if [ ! -s "$raw" ]; then
  echo "memprobe: no samples (process gone before the first read?)" >&2
  rm -f "$raw"; exit 1
fi

# columns: t  rss_kb  anon_kb  file_kb  hwm_kb  pss_kb
awk -F'\t' -v at="$at" '
  BEGIN { n = split(at, A, ","); }
  { t[NR]=$1; rss[NR]=$2; an[NR]=$3; fi[NR]=$4; if ($5>hwm) hwm=$5; pss[NR]=$6 }
  { if ($2>mr) mr=$2; if ($3>ma) ma=$3; if ($4>mf) mf=$4; if ($6>mp) mp=$6 }
  END {
    m = 1024
    printf "samples        %d over %.1f s\n", NR, t[NR]
    printf "RSS   max/final  %.0f / %.0f MiB\n", mr/m, rss[NR]/m
    printf "anon  max/final  %.0f / %.0f MiB\n", ma/m, an[NR]/m
    printf "file  max/final  %.0f / %.0f MiB\n", mf/m, fi[NR]/m
    printf "Pss   max/final  %.0f / %.0f MiB\n", mp/m, pss[NR]/m
    printf "VmHWM (kernel)   %.0f MiB\n", hwm/m
    for (i = 1; i <= n; i++) {
      want = A[i] + 0; best = 0
      for (j = 1; j <= NR; j++) if (t[j] <= want) best = j
      if (best == 0) printf "RSS at %ss       (no sample yet)\n", A[i]
      else if (t[NR] < want) printf "RSS at %ss       (process ended at %.1f s)\n", A[i], t[NR]
      else printf "RSS at %ss       %.0f MiB\n", A[i], rss[best]/m
    }
  }' "$raw"

if [ -n "$out" ]; then mv "$raw" "$out"; else rm -f "$raw"; fi
exit "$status"
