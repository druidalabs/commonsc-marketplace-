# CI/CD workflows

Two deploy pipelines, both running on free GitHub-hosted `ubuntu-latest`
runners (unlimited on this public repo). The actual builds run **on the Linode
box**, not the runner, so minute usage is negligible.

| Workflow | Trigger | What it does |
|---|---|---|
| `deploy-marketplace.yml` | push to `main` touching `crates/`, `Cargo.*`, `deploy/`, `registry/` | SSH in, run `deploy/deploy.sh` (pull → build → restart → health-check) |
| `deploy-website.yml` | push to `main` touching `website/` | rsync `website/` to nginx's document root |

Both can also be run manually from the **Actions** tab (`workflow_dispatch`).

## One-time setup

### 1 · Create a dedicated deploy keypair (no passphrase)

```bash
ssh-keygen -t ed25519 -f commonsc_deploy -N "" -C "github-actions-deploy"
```

Add the **public** half to the deploy user's authorized keys on the box:

```bash
# on the Linode box, as the deploy user (e.g. commonsc)
cat commonsc_deploy.pub >> ~/.ssh/authorized_keys
```

### 2 · Add repo secrets

Settings → Secrets and variables → Actions → **Secrets**:

| Secret | Value |
|---|---|
| `LINODE_HOST` | box hostname or IP (e.g. `api.commonsc.io`) |
| `LINODE_USER` | deploy user (e.g. `commonsc`) |
| `LINODE_DEPLOY_KEY` | contents of the **private** key file `commonsc_deploy` |

Then delete the local private key — GitHub holds the only copy it needs.

### 3 · Add the website root variable

Settings → Secrets and variables → Actions → **Variables**:

| Variable | Value |
|---|---|
| `WEBSITE_ROOT` | absolute path nginx serves `commonsc.io` from |

Confirm the path before the first run:

```bash
grep -R "root " /etc/nginx/sites-available/commonsc.io
```

The README diagram uses `/srv/website`; the legacy `website/deploy.sh` used
`/var/www/commonsc`. Use whatever nginx actually points at, and make sure the
deploy user can write there (`sudo chown -R <user> <root>` or add group-write).

### 4 · Server permissions (marketplace only)

`deploy/deploy.sh` restarts the service via `sudo`. The deploy user needs a
passwordless sudoers entry for exactly that command (already configured by the
manual deploy in `deploy/README.md`):

```
commonsc ALL=(root) NOPASSWD: /bin/systemctl restart commonsc-marketplace
```

## Verifying

Push a no-op change under `website/` (or trigger manually) and watch the run in
the Actions tab. The marketplace job fails the build if the post-deploy
`/health` probe doesn't return 200, so a red run means the service did **not**
flip to the new binary — the previous one keeps serving.
