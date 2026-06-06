# Deploying the CommonSense marketplace to Linode

End-to-end walkthrough for putting `commonsc-marketplace` behind nginx on the
existing Linode VPS, served at `api.commonsc.io`, alongside the apex website
at `commonsc.io`.

```
              ┌──────────────────────────────────────────────┐
              │              Linode VPS (Ubuntu)             │
              │                                              │
   Internet → │  nginx  ┬─→  /srv/website            :80/443 │
              │         │    (existing site)                 │
              │         │                                    │
              │         └─→  127.0.0.1:8787                  │
              │              commonsc-marketplace            │
              │              (systemd service)               │
              │              reads /srv/commonsc/            │
              │                                              │
              └──────────────────────────────────────────────┘
                          ▲
                          │ TLS via Let's Encrypt
                          │
              DNS at name.com:
                A   api.commonsc.io  → <LINODE_IP>
                A   commonsc.io      → <LINODE_IP>  (already set)
```

## 1 · DNS at name.com

Add a new A record for the subdomain:

| Type | Host | Answer          | TTL |
|------|------|-----------------|-----|
| A    | api  | `<LINODE_IPv4>` | 300 |

Optional but recommended: an AAAA record if the Linode has IPv6.

`dig api.commonsc.io +short` should return the Linode IP within a few minutes
of saving.

## 2 · Prepare the Linode

These steps need root (or `sudo`). Run them once per box.

### 2.1 · Create a non-root user for the marketplace

```sh
sudo adduser --system --group --home /srv/commonsc --shell /bin/bash commonsc
```

`/srv/commonsc` is the workspace path the systemd unit assumes.

### 2.2 · Install the Rust toolchain (for the `commonsc` user)

```sh
sudo -u commonsc -i bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
```

This installs rustup + the stable toolchain into `/srv/commonsc/.cargo/`. The
systemd unit and deploy script both expect `~/.cargo/bin/cargo` at that
location.

### 2.3 · Build dependencies

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev git curl
```

`build-essential` covers gcc/ld; `libssl-dev` is for any indirect TLS deps
in transitive crates. None of our direct deps need OpenSSL, but `cargo` will
sometimes pull rust-openssl through a transitive path.

### 2.4 · Clone the workspace

```sh
sudo -u commonsc -i git clone https://github.com/<owner>/commonsc.git /srv/commonsc
```

Replace `<owner>` with your GitHub handle. The repo's root is the cargo
workspace — the binary lives at `crates/marketplace/`.

### 2.5 · First build

```sh
sudo -u commonsc -i bash -c 'cd /srv/commonsc && ~/.cargo/bin/cargo build --release -p commonsc-marketplace'
```

First-time build takes ~5 minutes. Subsequent builds are incremental and
finish in under a minute.

After this completes, `/srv/commonsc/target/release/commonsc-marketplace`
exists and the systemd unit can launch it.

## 3 · Install the systemd service

```sh
sudo cp /srv/commonsc/deploy/systemd/commonsc-marketplace.service \
    /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable --now commonsc-marketplace
sudo systemctl status commonsc-marketplace
```

The service binds to `127.0.0.1:8787` — only nginx (on the same host) can
reach it. Confirm with `curl http://127.0.0.1:8787/health` → `ok`.

Tail logs with `journalctl -u commonsc-marketplace -f`.

## 4 · Wire nginx

### 4.1 · Subdomain (api.commonsc.io)

```sh
sudo cp /srv/commonsc/deploy/nginx/api.commonsc.io.conf \
    /etc/nginx/sites-available/api.commonsc.io
sudo ln -s /etc/nginx/sites-available/api.commonsc.io \
    /etc/nginx/sites-enabled/api.commonsc.io

sudo nginx -t            # syntax check
sudo systemctl reload nginx
```

At this point `http://api.commonsc.io/health` should return `ok` from any
machine. (Plain HTTP for now; TLS in the next step.)

### 4.2 · Apex routes (/.well-known/commonsc.json, /llms.txt)

Open your existing `/etc/nginx/sites-available/commonsc.io` server block.
Inside the `server { … }` stanza that handles `commonsc.io` (the one that
serves the website), paste the two `location` blocks from
`/srv/commonsc/deploy/nginx/apex.conf.snippet`. Reload:

```sh
sudo nginx -t
sudo systemctl reload nginx
```

These let agents fetch the canonical
`https://commonsc.io/.well-known/commonsc.json` URL — the apex is where the
`/.well-known/` convention lives. Behind the scenes nginx proxies just those
two paths to the marketplace; everything else on `commonsc.io` stays at the
website.

### 4.3 · TLS with certbot

```sh
sudo apt-get install -y certbot python3-certbot-nginx
sudo certbot --nginx -d api.commonsc.io
```

Certbot edits `api.commonsc.io.conf` in place to add the `ssl_certificate`
lines and a `listen 443 ssl;` line, plus a redirect from `:80` to `:443`. It
also installs a renewal timer.

If your existing apex `commonsc.io` block isn't already TLS-terminated, run
certbot for it too — the apex agent-contract URLs must be HTTPS for the
schema validators to accept them.

## 5 · Smoke-test from outside

From your laptop:

```sh
curl -s https://api.commonsc.io/health                        # → ok
curl -s https://api.commonsc.io/.well-known/commonsc.json     # JSON discovery doc
curl -s https://api.commonsc.io/registry/index.json           # algorithm catalog
curl -s https://commonsc.io/.well-known/commonsc.json         # same JSON via apex
curl -s https://commonsc.io/llms.txt                          # LLM-facing companion

# Validate endpoint round-trip
tar -cf - -C /path/to/algorithms eye-colour | zstd -q -o /tmp/eye-colour.tar.zst
curl -X POST -F "bundle=@/tmp/eye-colour.tar.zst" \
    https://api.commonsc.io/algorithms/validate | jq .
```

## 6 · Ongoing deploys

When you push new code (algorithms, devkit changes, marketplace updates) to
GitHub, ssh in and run the deploy script:

```sh
ssh commonsc@<linode-ip>
cd /srv/commonsc
./deploy/deploy.sh
```

The script pulls main, rebuilds the binary, restarts the systemd unit, and
probes `/health` to confirm the new binary is responding.

If you want a one-shot from your laptop:

```sh
ssh commonsc@<linode-ip> /srv/commonsc/deploy/deploy.sh
```

## 7 · Configure the customer app

In the CommonSense desktop app's build, set:

```sh
VITE_REGISTRY_BASE=https://api.commonsc.io cargo tauri build
```

The customer app's `loadCatalog` and bundle fetches resolve against this
base. Without it set, the app falls back to the in-app symlinked
`/registry/` (development default).

## 8 · What's not here yet (production todos)

- **Real signing keys.** The marketplace co-signs with a deterministic key
  derived from a hardcoded seed. Production swaps in AWS KMS (or an HSM)
  before any third-party publisher trusts the signatures. The verify side
  on the customer doesn't change.
- **Backups.** The marketplace's persistent state is just the `registry/`
  directory and `submissions/` queue. A nightly cron `tar` to S3 (or a
  Linode backup snapshot) is fine. Don't need a database yet.
- **Rate limiting.** No throttling on validate/publish — fine while
  the user pool is "us + a few testers." Add `limit_req_zone` in nginx
  when external traffic shows up.
- **WAF / DDoS shielding.** None. Fine for v1. Throw Cloudflare in front
  later if needed.
- **Multi-machine deploy.** Stateful single-VPS for now. The state surface
  (registry + submissions) is small enough to scale to a CDN + object
  store when traffic justifies.

## 9 · Troubleshooting

**`commonsc-marketplace` won't start, journal shows "discovery directory not found"**
The systemd unit assumes `/srv/commonsc/` is the cargo workspace root. If
you cloned to a different path, edit `WorkingDirectory=` and `--workspace`
in the unit, then `systemctl daemon-reload && systemctl restart`.

**nginx says "host not found in upstream"**
The `upstream commonsc_marketplace` block needs DNS-free addresses. The
config uses `127.0.0.1:8787` directly, so this shouldn't happen — but if
you change the upstream to a hostname, install `nginx-extras` or use the
`resolver` directive.

**certbot can't reach the domain**
DNS hasn't propagated yet. Wait a few minutes and `dig api.commonsc.io
+short` until it returns the Linode IP, then retry. If you're impatient,
`nslookup` against `8.8.8.8` directly bypasses your ISP cache.

**Build fails with "linker `cc` not found"**
You skipped step 2.3. Install `build-essential`.

**`./deploy.sh` says it can't restart the service**
The script uses `sudo /bin/systemctl restart`. The `commonsc` user needs a
sudoers entry to run that without a password:

```
# /etc/sudoers.d/commonsc-marketplace
commonsc ALL=(root) NOPASSWD: /bin/systemctl restart commonsc-marketplace, /bin/systemctl status commonsc-marketplace
```
