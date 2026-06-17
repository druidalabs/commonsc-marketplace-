# CI/CD workflows

One deploy pipeline, running on a free GitHub-hosted `ubuntu-latest` runner
(unlimited on this public repo). The actual build runs **on the Linode box**,
not the runner, so minute usage is negligible.

| Workflow | Trigger | What it does |
|---|---|---|
| `deploy-marketplace.yml` | push to `main` touching `crates/`, `Cargo.*`, `deploy/`, `registry/` | SSH in, run `deploy/deploy.sh` (pull → build → restart → health-check) |

Can also be run manually from the **Actions** tab (`workflow_dispatch`).

> **The website is not deployed from this repo.** It lives in its own repo,
> `druidalabs/commonsc.io`, and deploys via its own workflow there (SSH → `git
> pull` on the box). The marketplace and website share the same Linode box but
> are independent deploys. The desktop `.dmg` is built and shipped by
> `scripts/publish-dmg.sh` in the `druidalabs/CommonSense` repo.

## One-time setup

### 1 · Create a dedicated deploy keypair (no passphrase)

```bash
ssh-keygen -t ed25519 -f commonsc_deploy -N "" -C "github-actions-deploy"
```

Add the **public** half to the deploy user's authorized keys on the box:

```bash
# on the Linode box, as the deploy user
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

### 3 · Server permissions

`deploy/deploy.sh` restarts the service via `sudo`. The deploy user needs a
passwordless sudoers entry for exactly that command (already configured by the
manual deploy in `deploy/README.md`):

```
commonsc ALL=(root) NOPASSWD: /bin/systemctl restart commonsc-marketplace
```

## Verifying

Trigger the workflow manually (or push a change under `crates/`) and watch the
run in the Actions tab. The job fails the build if the post-deploy `/health`
probe doesn't return 200, so a red run means the service did **not** flip to the
new binary — the previous one keeps serving.
