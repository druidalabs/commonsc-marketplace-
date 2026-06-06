#!/usr/bin/env bash
# Update + restart the CommonSense marketplace on a Linode VPS.
#
# Run as the `commonsc` user from /srv/commonsc:
#     ./deploy/deploy.sh
#
# What it does:
#   1. Pulls the latest from GitHub (origin/main by default).
#   2. Rebuilds the marketplace binary in release mode.
#   3. Asks systemd to restart the service.
#   4. Confirms the new binary is responding.
#
# Safe to run repeatedly. If the build fails, the running service stays up
# on the previous binary because `cargo build` writes to a new artifact only
# when it succeeds.

set -euo pipefail

ROOT="${COMMONSC_ROOT:-/srv/commonsc}"
BRANCH="${COMMONSC_BRANCH:-main}"
SERVICE="${COMMONSC_SERVICE:-commonsc-marketplace}"
HEALTH_URL="${COMMONSC_HEALTH_URL:-http://127.0.0.1:8787/health}"

log() { printf '[%(%H:%M:%S)T] %s\n' -1 "$*"; }

cd "$ROOT"

log "pulling origin/$BRANCH"
git fetch --quiet origin "$BRANCH"
git reset --hard "origin/$BRANCH"

log "building commonsc-marketplace (release)"
~/.cargo/bin/cargo build --release -p commonsc-marketplace

log "restarting $SERVICE"
sudo /bin/systemctl restart "$SERVICE"

# Give systemd a moment to come up before we probe.
sleep 1

log "probing $HEALTH_URL"
HTTP_CODE=$(curl --silent --output /dev/null --write-out '%{http_code}' "$HEALTH_URL" || echo "000")
if [[ "$HTTP_CODE" != "200" ]]; then
    log "WARN: health probe returned $HTTP_CODE — check journalctl -u $SERVICE"
    exit 1
fi

log "OK — service responded with 200 from $HEALTH_URL"
log "current commit: $(git rev-parse --short HEAD)"
