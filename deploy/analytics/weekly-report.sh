#!/usr/bin/env bash
# Weekly traction snapshot — six KPIs, all server-side, no client telemetry.
# Run on the box (the analytics user must be in the `adm` group to read logs):
#     /srv/commonsc/deploy/analytics/weekly-report.sh
# Window defaults to 7 days; override with DAYS=30.
#
# The app phones nothing home by design, so we measure the edges: discover →
# download → catalog-open (usage proxy) → install → contribute. Revenue is left
# to Stripe (its dashboard + notifications are the source of truth).

set -uo pipefail   # not -e: a missing source should print 0, not abort the report

WEB_LOG="${WEB_LOG:-/var/log/nginx/access.log}"
API_LOG="${API_LOG:-/var/log/nginx/api.commonsc.io.access.log}"
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

# Revenue is intentionally NOT tracked here — Stripe's dashboard + payment
# notifications are the source of truth (and the local payment records include
# old test-mode checkouts). This report covers only what Stripe doesn't.

# 4 — Supply: contributor submissions (files modified in the window + total)
if [ -d "$SUBMISSIONS" ]; then
  recent=$(find "$SUBMISSIONS" -type f -name '*.json' -mtime -"$DAYS" 2>/dev/null | grep -c .)
  total=$( find "$SUBMISSIONS" -type f -name '*.json' 2>/dev/null | grep -c .)
  echo "4. Contributor submissions  : $recent   (all-time $total)"
else
  echo "4. Contributor submissions  : (no submissions dir)"
fi

# 5 — Agent traction: hits to the agent-facing discovery surfaces
agent=$( win "$API_LOG" | n 'GET /(\.well-known/commonsc\.json|llms\.txt)')
echo "5. Agent discovery hits     : $agent"

echo "════════════════════════════════════════════════"
echo "Revenue → Stripe dashboard + payment notifications (not tracked here)."
echo "Signal order: paid (Stripe) > opens > downloads > visitors."
echo "Trend history: /srv/commonsc/analytics/downloads.csv"
