#!/usr/bin/env bash
# Measure AgentPulse idle memory and CPU on macOS.
#
# UNTESTED: written for M10 macOS testing and not yet run on a Mac.
# Caveat: WKWebView content processes (com.apple.WebKit.*) are launched by
# launchd, not as children of the app, so they are matched by name. Close other
# WebKit apps (Safari, Mail) first, or the numbers will include them.
#
# Usage: scripts/perf/measure.sh [process-name] [seconds]
set -euo pipefail
NAME="${1:-agents-pulse}"
SECONDS_TO_SAMPLE="${2:-60}"

pids() {
  local root
  root=$(pgrep -x "$NAME" | head -1) || { echo "process $NAME not running" >&2; exit 1; }
  echo "$root"
  pgrep -f "com.apple.WebKit" || true
}

sum_cpu_time() { # total CPU seconds across processes
  ps -o time= -p "$(pids | paste -sd, -)" | awk -F'[:.]' '{ s += ($1 * 60) + $2 } END { print s }'
}

c0=$(sum_cpu_time)
sleep "$SECONDS_TO_SAMPLE"
c1=$(sum_cpu_time)
rss_kb=$(ps -o rss= -p "$(pids | paste -sd, -)" | awk '{ s += $1 } END { print s }')

echo "processes:        $(pids | wc -l | tr -d ' ')"
echo "rss_mb:           $(awk -v k="$rss_kb" 'BEGIN { printf "%.1f", k / 1024 }')"
echo "cpu_pct_one_core: $(awk -v a="$c0" -v b="$c1" -v s="$SECONDS_TO_SAMPLE" 'BEGIN { printf "%.2f", 100 * (b - a) / s }')"
