#!/usr/bin/env bash
# Weekly traction snapshot — six KPIs, all server-side, no client telemetry.
# Run on the box (the analytics user must be in the `adm` group to read logs):
#     /srv/commonsc/deploy/analytics/weekly-report.sh
# Window defaults to 7 days; override with DAYS=30.
#
# The app phones nothing home by design, so we measure the edges: discover →
# download → catalog-open (usage proxy) → install → pay → contribute.

set -uo pipefail   # not -e: a missing source should print 0, not abort the report

WEB_LOG="${WEB_LOG:-/var/log/nginx/access.log}"
API_LOG="${API_LOG:-/var/log/nginx/api.commonsc.io.access.log}"
PAYMENTS="${PAYMENTS:-/srv/commonsc/payments}"
SUBMISSIONS="${SUBMISSIONS:-/srv/commonsc/submissions}"
DAYS="${DAYS:-7}"

# OR-regex of the last $DAYS days in nginx's "[dd/Mon/YYYY" access-log format.
days_re() {
  local i d out=""
  for i in $(seq 0 $((DAYS - 1))); do
    d=$(date -d "-$i day" +%d/%b/%Y 2>/dev/null || date -v-"${i}"d +%d/%b/%Y)
    out="${out:+$out|}\\[${d}"
  done
  printf '%s' "$out"
}
RE="$(days_re)"
win()  { grep -hE "$RE" "$1" 2>/dev/null; }                 # last-N-days lines of a log
n()    { grep -cE "$1" || true; }                            # count matches on stdin
pct()  { [ "$2" -gt 0 ] && awk "BEGIN{printf \"%d%%\", 100*$1/$2}" || echo "n/a"; }

echo "════════════════════════════════════════════════"
echo " CommonSense traction — last ${DAYS} days  ($(date +%Y-%m-%d))"
echo "════════════════════════════════════════════════"

# 1 — Acquisition: unique visitors (distinct IPs across both vhosts)
visitors=$( { win "$WEB_LOG"; win "$API_LOG"; } | awk '{print $1}' | sort -u | grep -c . )
echo "1. Unique visitors          : $visitors"

# 2 — Intent: downloads + download-page conversion
dls=$(   win "$WEB_LOG" | n 'GET /download/CommonSense-[^ ]+\.dmg .* (200|206) ')
views=$( win "$WEB_LOG" | n 'GET /download(\.html)?[ ?]')
echo "2. Downloads                : $dls   (page views $views → conv $(pct "$dls" "$views"))"

# 3 — Activation/retention: catalog opens (desktop fetches the index each load)
opens=$(    win "$API_LOG" | n 'GET /registry/index\.json')
installs=$( win "$API_LOG" | n 'GET /registry/bundles/.*\.tar\.zst')
echo "3. Catalog opens            : $opens   (community-algorithm installs $installs)"

# 4 — Market fit: paid purchases + revenue (from persisted Stripe records)
if [ -d "$PAYMENTS" ]; then
  python3 - "$PAYMENTS" "$DAYS" <<'PY'
import json, glob, os, sys, time
d, days = sys.argv[1], int(sys.argv[2])
cutoff = time.time() - days * 86400
tot = totrev = win = winrev = 0
for f in glob.glob(os.path.join(d, "*.json")):
    try: r = json.load(open(f))
    except Exception: continue
    if not r.get("paid"): continue
    amt = r.get("amount_minor", 0)
    tot += 1; totrev += amt
    if r.get("created_at", 0) >= cutoff:
        win += 1; winrev += amt
print(f"4. Paid purchases           : {win}   (~£{winrev/100:.2f})   | all-time {tot} (~£{totrev/100:.2f})")
PY
else
  echo "4. Paid purchases           : (no payments dir at $PAYMENTS)"
fi

# 5 — Supply: contributor submissions (files modified in the window + total)
if [ -d "$SUBMISSIONS" ]; then
  recent=$(find "$SUBMISSIONS" -type f -name '*.json' -mtime -"$DAYS" 2>/dev/null | grep -c .)
  total=$( find "$SUBMISSIONS" -type f -name '*.json' 2>/dev/null | grep -c .)
  echo "5. Contributor submissions  : $recent   (all-time $total)"
else
  echo "5. Contributor submissions  : (no submissions dir)"
fi

# 6 — Agent traction: hits to the agent-facing discovery surfaces
agent=$( win "$API_LOG" | n 'GET /(\.well-known/commonsc\.json|llms\.txt)')
echo "6. Agent discovery hits     : $agent"

echo "════════════════════════════════════════════════"
echo "Signal order: #4 paid > #3 opens > #2 downloads > #1 visitors."
echo "Trend history: /srv/commonsc/analytics/downloads.csv"
