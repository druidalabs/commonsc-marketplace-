#!/usr/bin/env bash
# Daily aggregate download tally — privacy-preserving.
#
# Counts yesterday's successful DMG downloads from the nginx access log and
# appends ONLY aggregate numbers (date, count, unique-IP count) to a CSV. IPs
# are read transiently to compute the unique count but are never stored, so the
# running totals survive the 14-day log rotation without retaining any PII.
#
# Cron (runs just after midnight):
#   5 0 * * *  /srv/commonsc/deploy/analytics/tally-downloads.sh

set -euo pipefail

LOG="${NGINX_LOG:-/var/log/nginx/access.log}"
OUT="${TALLY_OUT:-/srv/commonsc/analytics/downloads.csv}"

mkdir -p "$(dirname "$OUT")"
[ -f "$OUT" ] || echo "date,downloads,unique_ips" > "$OUT"

# Yesterday in nginx log format (e.g. 18/Jun/2026) and ISO for the CSV.
day_log=$(date -d 'yesterday' +%d/%b/%Y 2>/dev/null || date -v-1d +%d/%b/%Y)
day_iso=$(date -d 'yesterday' +%F 2>/dev/null || date -v-1d +%F)

hits=$(grep -hF "[$day_log" "$LOG" 2>/dev/null \
       | grep -E 'GET /download/CommonSense-[^ ]+\.dmg' \
       | grep -E ' (200|206) ' || true)

count=$(printf '%s' "$hits" | grep -c . || true)
uniq=$(printf '%s\n' "$hits" | awk 'NF{print $1}' | sort -u | grep -c . || true)

echo "${day_iso},${count},${uniq}" >> "$OUT"
echo "tallied $day_iso: $count downloads, $uniq unique IPs → $OUT"
