# Traffic & download analytics (privacy-preserving)

The privacy policy promises **no client-side analytics, no third-party
JavaScript, and no cookies**. So we measure server-side, from nginx's existing
access logs — visitors get nothing new, and the stated posture is unchanged.
(Don't add Google Analytics / Plausible / a tracking pixel — it would break the
privacy promise and require a policy change.)

## Quick counts from the log

Downloads (each DMG GET; 206 = resumed/ranged):
```sh
grep -E 'GET /download/CommonSense-[^ ]+\.dmg' /var/log/nginx/access.log \
  | grep -E ' (200|206) ' | wc -l
```

Roughly-unique downloaders (by IP):
```sh
grep -E 'GET /download/.*\.dmg .* (200|206) ' /var/log/nginx/access.log \
  | awk '{print $1}' | sort -u | wc -l
```

Top pages:
```sh
awk '{print $7}' /var/log/nginx/access.log | sort | uniq -c | sort -rn | head -20
```

## A dashboard: GoAccess

GoAccess turns the nginx log into a live HTML dashboard — server-side, no client
JS, no cookies:
```sh
sudo apt-get install -y goaccess
# one-off static report:
goaccess /var/log/nginx/access.log --log-format=COMBINED -o /srv/commonsc/analytics/report.html
# or real-time (writes a self-updating page):
goaccess /var/log/nginx/access.log --log-format=COMBINED --real-time-html \
  -o /srv/commonsc/analytics/report.html
```
Serve `report.html` behind the existing admin auth (or keep it local and view
over SSH). It gives visitors, requests, top URLs, referrers, status codes.

## Persisting totals past the 14-day log rotation

The privacy policy rotates logs every 14 days. To keep **aggregate** numbers
long-term without retaining raw logs or IPs, run `tally-downloads.sh` daily via
cron — it stores only `date,downloads,unique_ips` (counts, never IPs):
```cron
5 0 * * *  /srv/commonsc/deploy/analytics/tally-downloads.sh
```
Output accumulates in `/srv/commonsc/analytics/downloads.csv`.
