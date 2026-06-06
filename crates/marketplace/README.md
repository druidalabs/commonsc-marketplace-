# commonsc-marketplace

The CommonSense marketplace HTTP server. Hosts the discovery contract, JSON
schemas, and the algorithm registry (manifests + signed bundles).

For v0 this is a static file server. The publish API (`POST /algorithms/...`)
lands in the next iteration; today the dev-kit writes directly to the
on-disk registry and this binary just serves it.

## Run

From the workspace root:

```sh
cargo run -p commonsc-marketplace
# → http://localhost:8787
```

CLI flags:

| Flag | Default | Notes |
|---|---|---|
| `--port` | `8787` | TCP port to listen on. |
| `--workspace` | `<inferred>` | Override the workspace root. Resolved from the binary's manifest dir if omitted. |

The server expects three on-disk directories under the workspace:

- `discovery/` — `.well-known/commonsc.json`, `llms.txt`
- `product/schemas/` — JSON Schemas
- `registry/` — `index.json` + `bundles/<publisher>/<algo>/<version>/`

It will refuse to start if any are missing.

## Routes

```
GET  /                                  root index (JSON)
GET  /health                            liveness probe
GET  /.well-known/commonsc.json         discovery contract — agent first stop
GET  /llms.txt                          LLM-facing companion
GET  /schemas/<name>.schema.json        JSON schemas
GET  /registry/index.json               algorithm catalog
GET  /registry/bundles/<id>/<v>/...     manifests + signed bundles
```

All responses include CORS `Access-Control-Allow-Origin: *` so the customer
app running on a different localhost port can fetch them without a proxy.

## Wiring to the customer app

In one terminal, run the marketplace. In another, run `cargo tauri dev` with
the registry-base override:

```sh
VITE_REGISTRY_BASE=http://localhost:8787 cargo tauri dev
```

The customer app's catalog pulls now route through the marketplace instead of
the vite-served `/registry/` symlink. When you deploy the marketplace behind
`catalog.commonsc.io`, point the same env var there.

If `VITE_REGISTRY_BASE` is unset the app falls back to the vite path, so the
default no-flag dev loop still works without the marketplace running.

## What's next

- `POST /publisher/register` — generate a publisher account
- `POST /algorithms/validate` — run the canonical gates over an uploaded
  bundle, return the same `gate-result.schema.json` the local dev-kit produces
- `POST /algorithms/publish` — accept a signed bundle into the review queue
- `GET  /algorithms/{id}/status` — poll review status
- Reviewer admin UI (separate static SPA, talks to the same service)

All endpoints are already declared in `/.well-known/commonsc.json`; this
binary just doesn't implement them yet.
