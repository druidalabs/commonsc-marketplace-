# Traffic & download analytics

How we measure visits and downloads for commonsc.io — and why it's done the way
it is.

## Principle: server-side only

The [privacy policy](https://commonsc.io/privacy.html) promises **no
client-side analytics, no third-party JavaScript, and no cookies**. We keep that
promise by measuring entirely **server-side**, from nginx's own access logs.
Nothing is added to any page; visitors are tracked by nothing.

> ⚠️ Do **not** add Google Analytics, Plausible, a tracking pixel, or any
> client-side script. It would break the privacy promise and require a policy
> change. Everything below reads logs we already keep.

### What we can and can't see
- **Can:** request counts, which pages/files were hit, status codes, referrers,
  rough unique-visitor counts (by IP), and download counts — all from the log.
- **Don't keep:** no cross-session identity, no cookies, no per-user history.
  nginx logs rotate every **14 days** (per the privacy policy); only the
  aggregate totals below are kept longer, and those contain **no IPs**.

### Where the logs are
- Website (`commonsc.io`): `/var/log/nginx/access.log` (default vhost log).
- API (`api.commonsc.io`): `/var/log/nginx/api.commonsc.io.access.log`.

Downloads are GETs for the DMG on the **website** host, so most commands below
use `/var/log/nginx/access.log`.

---

## 1. Quick counts (one-liners)

**Total downloads** (`206` = resumed/ranged requests, count them too):
```sh
grep -E 'GET /download/CommonSense-[^ ]+\.dmg' /var/log/nginx/access.log \
  | grep -E ' (200|206) ' | wc -l
```

**Roughly-unique downloaders** (distinct IPs):
```sh
grep -E 'GET /download/.*\.dmg .* (200|206) ' /var/log/nginx/access.log \
  | awk '{print $1}' | sort -u | wc -l
```

**Downloads per version:**
```sh
grep -oE 'CommonSense-[0-9][^ ]*\.dmg' /var/log/nginx/access.log \
  | sort | uniq -c | sort -rn
```

**Top pages:**
```sh
awk '{print $7}' /var/log/nginx/access.log | sort | uniq -c | sort -rn | head -20
```

**Where visitors came from (referrers):**
```sh
awk -F'"' '{print $4}' /var/log/nginx/access.log | sort | uniq -c | sort -rn | head -20
```

**Visits to the download page:**
```sh
grep -E 'GET /download(\.html)? ' /var/log/nginx/access.log | wc -l
```

---

## 2. A dashboard with GoAccess

[GoAccess](https://goaccess.io) turns the nginx log into a full dashboard
(visitors, requests, top URLs, referrers, status codes, OS/browser) — entirely
server-side, no client JS, no cookies.

Install:
```sh
sudo apt-get install -y goaccess
```

One-off static HTML report:
```sh
goaccess /var/log/nginx/access.log --log-format=COMBINED \
  -o /srv/commonsc/analytics/report.html
```

Real-time self-updating page:
```sh
goaccess /var/log/nginx/access.log --log-format=COMBINED --real-time-html \
  -o /srv/commonsc/analytics/report.html
```

Terminal view (no file, just look):
```sh
goaccess /var/log/nginx/access.log --log-format=COMBINED
```

**Serving the report:** keep it private. Either view it over SSH, or expose
`/analytics/report.html` behind HTTP basic auth in the nginx vhost — never
publish it open. Don't serve it from a path search engines can reach.

---

## 3. Persisting totals past log rotation

Logs rotate every 14 days, so raw history is short-lived by design. To keep
**long-term aggregate totals** without retaining logs or IPs, run
`tally-downloads.sh` once a day. It reads yesterday's log, computes the download
count + unique-IP count, and appends **only those numbers** (no IPs) to a CSV.

Install the cron (runs just after midnight):
```sh
( crontab -l 2>/dev/null; echo '5 0 * * * /srv/commonsc/deploy/analytics/tally-downloads.sh' ) | crontab -
```

Output accumulates at `/srv/commonsc/analytics/downloads.csv`:
```
date,downloads,unique_ips
2026-06-18,42,37
2026-06-19,55,49
```

Read it back any time:
```sh
column -s, -t /srv/commonsc/analytics/downloads.csv          # pretty table
awk -F, 'NR>1{s+=$2} END{print "total downloads:", s}' /srv/commonsc/analytics/downloads.csv
```

Overrides (env): `NGINX_LOG` (default `/var/log/nginx/access.log`),
`TALLY_OUT` (default `/srv/commonsc/analytics/downloads.csv`).

---

## Files in this directory
- `README.md` — this guide.
- `tally-downloads.sh` — the daily aggregate tally (privacy-preserving; stores
  counts only, never IPs).
