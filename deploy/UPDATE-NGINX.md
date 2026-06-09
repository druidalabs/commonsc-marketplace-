# Updating the nginx config after first deploy

The `deploy/nginx/api.commonsc.io.conf` in this repo is the **pre-certbot**
template. After your first `sudo certbot --nginx -d api.commonsc.io` it
rewrites the live file with TLS directives — and a plain `sudo cp` from
this template wipes those directives.

If you've changed something in `deploy/nginx/api.commonsc.io.conf` (added a
new location, tweaked proxy timeouts, etc.) and need to push it to the live
nginx, do it in one of two ways:

## Option A — `cp` + re-run certbot (simplest)

```sh
sudo cp /srv/commonsc/deploy/nginx/api.commonsc.io.conf \
    /etc/nginx/sites-available/api.commonsc.io
sudo certbot --nginx -d api.commonsc.io
sudo nginx -t
sudo systemctl reload nginx
```

certbot is idempotent. It re-adds the TLS lines, skips the cert renewal
because it's still valid, and reloads nginx. ~5 seconds total.

## Option B — edit the live file directly

Open `/etc/nginx/sites-available/api.commonsc.io` in `sudo nano` (or your
editor of choice) and copy just the new `location` block (or whichever
hunk you actually changed) over by hand. Leave the certbot-added
`listen 443 ssl;` and `ssl_certificate ...` lines alone.

```sh
sudo nano /etc/nginx/sites-available/api.commonsc.io
sudo nginx -t
sudo systemctl reload nginx
```

Slower but no risk of clobbering anything.

## Diagnostic

If `curl -v https://api.commonsc.io/...` shows a cert for the wrong domain
(e.g. `casadebarro.druidalabs.com`), nginx has no `:443` server block
matching `api.commonsc.io`. Fix: re-run `sudo certbot --nginx -d api.commonsc.io`.
