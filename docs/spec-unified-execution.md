# Spec: Unified algorithm execution — `devkit run`, an execution gate, and in‑app bundle testing

Status: draft · Owner: DruidaLabs · Date: 2026-06

## Problem

Nobody — human or agent — can actually *run* a candidate algorithm against real
data before it ships.

- The contribution pipeline never executes code. `devkit validate` and the
  server's `POST /algorithms/validate` are **static only**: JSON‑Schema check,
  entrypoint‑file‑exists, fixture‑shape. `crates/devkit/src/validate.rs:8`
  says execution "lands when the host crate is wired"; devkit does **not** depend
  on the host crate. The first time a submitted algorithm executes is on a
  *user's machine*.
- The desktop app is consumer‑only. There is no authoring/test/sideload screen;
  the Library installs signed bundles and runs pre‑existing ones. A user who
  follows the homepage's "Build an algorithm →" finds no on‑ramp in the app.
- `llms.txt` *instructs* agents to "test your code against the fixtures before
  submitting" but ships no command to do it.

The capability already exists on exactly one side: the consumer runtime. The
Tauri command `run_v2_algorithm` (`CommonSense/src-tauri/src/algorithm.rs`)
executes **any** bundle bytes via
`commonsc_host::sidecar::run_one_with_config_events(...)` — it verifies the
sha256 against the bytes it was handed, not against an allowlist. The runtime is
real, sandboxed (Pyodide under Deno, no net/fs/subprocess), and now emits
progress events (v0.1.4). We just need to point three more consumers at it.

## Goal

**One execution path, three consumers.** Make
`commonsc_host::sidecar::run_one_with_config_events` the single way an algorithm
runs, reachable from:

1. the **devkit CLI** (`devkit run`) — local author/agent loop;
2. the **marketplace** validate/publish gates — every submission is executed in
   the sandbox before a human can approve it;
3. the **desktop app** — a clearly‑marked developer surface to test a local
   bundle against your own sample.

Closing this gap also lets us tell the truth: today the homepage and `llms.txt`
promise a trust/test story the code doesn't back.

## Principles / non‑goals

- **Not** a code editor in the app. Authoring stays in the devkit; the app only
  *runs* a bundle you already built.
- **Not** a bypass of human review. Execution is a gate *before* the queue, never
  an auto‑publish.
- **Reuse, don't fork.** No second runtime, no second result renderer, no second
  progress system.
- **Honest by default.** Ship the contract‑honesty fixes regardless of the
  feature work; they're cheap and currently misleading.

## Shared foundation (prerequisite for 1 & 2)

`commonsc_host` becomes a normal dependency of `commonsc-devkit`. Two gaps in the
host must be closed first because both the CLI and the server will execute
untrusted code with it:

- **Wall‑clock timeout.** `Sidecar::run*` blocks on `read_line` with no deadline
  (`crates/host/src/sidecar.rs`). Add a hard timeout (tier `wallSecondsMax`, 30s)
  that kills the child and returns `SidecarError::Timeout`. Without this the
  execution gate is a DoS vector on the server.
- **Memory ceiling.** Pyodide alone won't enforce `memoryMiBMax: 512`. On the
  server, run the gate in a memory‑capped worker (cgroup/container/ulimit), not
  bare. Locally, best‑effort.

Deno is the new runtime requirement for devkit and the marketplace box. The app
already bundles `deno` (externalBin) so the in‑app surface gets it free; the CLI
requires `deno` on PATH (fall back to `SidecarConfig::default()`); the server
adds deno to its deploy.

## Deliverable 1 — `devkit run`

New subcommand:

```
commonsc-devkit run <project> \
  [--fixture fixtures/input.json] \   # default: the project's bundled fixture(s)
  [--entrypoint module:function] \    # default: read from manifest.template.json
  [--json]                            # machine-readable output for agents
```

Behavior:

1. Build the bundle exactly as `publish` does — reuse the tar+zstd logic in
   `crates/devkit/src/publish.rs` (factor it into a shared `build_bundle()`).
2. Compute its sha256; load the fixture VariantSet (validate it against
   `genomic-io.schema.json#/$defs/VariantSet` first — fail fast with a clear
   message if the fixture is malformed).
3. Call `run_one_with_config(cfg, &bytes, &sha, module, function, variant_set)`.
4. Validate the returned envelope against `result.schema.json#/$defs/Result`,
   including the **closed block‑kind set** (`rows`, `score`, `distribution`,
   `table`, `callout`, `bars`).
5. Print the envelope (pretty, or `--json`). Exit non‑zero on: build failure,
   fixture invalid, sandbox throw, timeout, or envelope‑invalid — each with a
   distinct message and, where applicable, a `remediation.action` code matching
   the validate gate set.

This is the canonical "test before you submit" command the agent contract should
point to (replaces the unfulfilled instruction in `llms.txt` step 4).

## Deliverable 2 — execution gate in validate (devkit + marketplace)

Add a fourth gate to `commonsc_devkit::validate::run()`:

- **Gate `execution`** — runs the entrypoint against every bundled fixture via
  the host and asserts:
  - it does not throw;
  - it finishes within `wallSecondsMax`;
  - the result conforms to `result.schema.json` and uses only closed block kinds;
  - (determinism) two runs of the same fixture produce identical output — folds
    in the existing "seed your RNG" determinism requirement.

Wiring:

- **Server (authoritative).** `validate_handler` and `publish_handler`
  (`crates/marketplace/src/main.rs:97-98`) call the **execution worker**
  (see "Execution worker — settled") rather than running submitter code in the
  API process. A submission cannot reach the human queue without having executed
  cleanly in the sandbox. If the worker is unreachable, validate/publish **fail
  closed** — the submission stays un‑gated, never auto‑passed.
- **Local (best‑effort).** `devkit validate` runs `execution` when `deno` is
  present; otherwise it emits a `skipped` gate result ("deno not found — server
  will run this") rather than failing, so static validation still works offline.

Schema changes:

- `product/schemas/gate-result.schema.json`: add the `execution` gate and extend
  the **closed** `Remediation.action` enum with codes like
  `fix-entrypoint-throws`, `reduce-runtime`, `fix-result-envelope`,
  `seed-randomness`. (The enum is the agent's instruction set — keep it tight and
  documented.)
- Bump `discovery.schemaVersion` only if the gate‑result shape changes in a
  breaking way; adding gates/actions is additive.

## Deliverable 3 — in‑app "Test a local bundle"

A developer surface in the desktop app, hidden from the default consumer flow.

Rust:

- New Tauri command `run_local_bundle(path, sampleVariantSet, runId)` in
  `src-tauri/src/algorithm.rs`. Reads bytes from a local `bundle.tar.zst` (or
  builds from a project dir), computes the sha, runs via the same
  `run_one_with_config_events` path, emits the same `algo://event` progress
  stream. **Distinct from `run_v2_algorithm`**: no registry fetch, and the
  frontend signature gate (`verifyManifest`) is deliberately skipped — it's your
  own code.

UI:

- Entry point: a small **"Developer"** affordance (footer link or a setting),
  not a top‑level tab — keeps the consumer surface clean.
- Dev screen: pick a local bundle/folder → choose the active profile's sample →
  Run → reuse the v0.1.4 progress toasts → render the envelope through the
  existing `ui/src/blocks.tsx` renderer → show envelope‑validation status,
  wall‑clock time, and any thrown error verbatim.
- **Guardrails (important):** the screen is visibly labelled "Local test — not
  signed, not installed." Results are ephemeral: they are **not** written to the
  vault as a profile result, so this can never be a covert sideload path for paid
  content. No persistence, no catalog entry, no signature claim.

This gives a human the on‑ramp the homepage promises and an agent driving the app
a way to test on real data — both through the runtime we already ship.

## Contract‑honesty fixes (ship regardless, cheap)

These are currently misleading and independent of the feature work:

1. **`/algorithms/init` 404.** Discovery advertises `POST /algorithms/init`; the
   router (`crates/marketplace/src/main.rs:93-108`) doesn't serve it. Either
   implement it or remove it from `discovery/.well-known/commonsc.json` and the
   `llms.txt` step‑3 scaffold path. Recommend: **remove** for now; scaffolding is
   `devkit init` + the examples repo.
2. **Auth — now enforced for real (see "Auth — settled").** The contract's
   `authentication` block and the homepage's "signed twice, verified before any
   bytes run" become *true* in Phase B; no wording downgrade. The deterministic
   dev signer in `crates/devkit/src/signing.rs` is retired except for an offline
   `--dev` mode the server never accepts.
3. **Result block‑kind drift.** `llms.txt` lists `rows, score, table,
   distribution, callout` but the renderer also supports `bars` (added with the
   QC report). Add `bars` to `llms.txt` and confirm `result.schema.json` matches.
4. **Examples URL.** `llms.txt`/discovery point to `github.com/commonsc/algorithms`;
   the real org is `druidalabs`. Fix or create the repo the contract names.

## Security / pessimism

- Executing submitter code on the marketplace box is a new attack surface. Pyodide
  removes net/fs/subprocess, and Deno perms are locked, but **resource
  exhaustion** is the live risk → wall‑clock timeout + memory cap + ephemeral
  isolation are mandatory before the server runs deliverable 2, not optional.
- The in‑app dev path runs arbitrary local bundles with the user's own sample.
  That's acceptable (it's their machine, their code, their data, no egress), but
  the "not signed / ephemeral" framing must be unmissable so it can't be confused
  with installing a vetted algorithm.
- Determinism check doubles runtime cost per fixture — fine at submission volume,
  budget for it in rate limits.

## Auth — settled (full real ed25519 + real marketplace co‑sign)

Both the publisher‑identity claim and the consumer‑facing "verified before it
runs" claim become real in Phase B. The deterministic dev signer is retired.

**What B must undo (discovered during B1).** Today the server *fabricates* the
publisher signature: `admin::approve` runs `publish::run`, which signs **both**
the publisher and marketplace signatures with forgeable dev keys — the
publisher's private key never participates — and remote publish uploads the raw
*project*, not a signed manifest. So B is a restructure, not a key swap: the
publisher must complete and sign the manifest **client-side** at publish time;
the server only *verifies* it and adds its *own* co-sign at approval.

B sub-slices:
- **B1 ✅ — crypto foundation.** Real `sign_with_secret` / `verify` (round-trip
  tested) in `signing.rs`; the dev signer is quarantined as `sign_dev` /
  `public_key_dev` (forgeable, local/offline only — the server must reject it).
  Local `publish::run` now signs the publisher sig with the registered key when
  credentials match the manifest's keyId.
- **B2 ✅ — client-side signing on remote publish.** `publish --remote` builds
  the runtime bundle, completes the manifest, and signs the canonical bytes with
  the registered private key (`sign_for_remote`), sending it as the
  `x-commonsc-key-id` + `x-commonsc-publisher-sig` headers. `init.rs` now reads
  the keyId from credentials. **Auth is the manifest signature** — the bearer
  JWT is deferred (request-level hardening, lower value given the signature
  already binds publisher identity to the artifact; revisit if replay becomes a
  concern).
- **B3 ✅ — server verification.** `publish_handler` looks up the publisher's
  registered pubkey (`publishers/{handle}.json`, keyId asserted) and verifies the
  manifest signature over the reproducibly-rebuilt canonical bytes
  (`verify_remote_signature`); **dev-signed / forged / wrong-key submissions are
  rejected (401)**. The verified signature is stored on the submission.
- **B4 ✅ — approve co-signs only.** `approve` reuses the stored, verified
  publisher signature (never re-fabricates it) and adds the marketplace co-sign
  via `marketplace_cosign` — a real server-held key from
  `COMMONSC_MARKETPLACE_PRIVATE_KEY` when set, else the dev key.
  Round-trip + forgery-rejection unit-tested against a real example manifest.
- **B5 — app verifies both signatures.** *Blocked on two decisions (below), not
  code.* Wire real ed25519 verification into `catalog.ts verifyManifest`
  (currently only checks the signature is plausibly base64), pinning the
  marketplace pubkey and verifying the publisher sig against a trusted source.
  Prerequisites:
  1. **Marketplace keypair provisioning** — generate the production marketplace
     ed25519 keypair, set the private half as `COMMONSC_MARKETPLACE_PRIVATE_KEY`
     on the API box, **re-publish the embedded registry** co-signed with it, and
     pin the public half in the app. Until then the embedded/dev registry is
     dev-co-signed; flipping strict verification first would break the live app.
  2. **Publisher keys trust source** — a `keys.json` (keyId→pubkey) served by the
     marketplace from `publishers/` and loaded by the app during catalog pull,
     so the app can verify *community* publisher signatures (first-party keys can
     be embedded). Use a JS ed25519 verifier (e.g. `@noble/ed25519`) — WebCrypto
     Ed25519 isn't available on the macOS 12/13 WKWebView the app still targets.

**Publisher identity (devkit + server).**
- `devkit register` already generates a real ed25519 keypair (OsRng) and stores
  it at `~/.commonsc/credentials.json`; the server stores the pubkey at
  `publishers/{handle}.json`. Keep both.
- Replace `crates/devkit/src/signing.rs` (deterministic dev keys) with signing
  using the publisher's *stored private key*. The manifest's publisher signature
  is produced with it; `manifest.publisher.keyId` is the registered keyId — drop
  the hardcoded `-2026-01` suffix in `init.rs:61` and read the real keyId from
  credentials.
- `devkit` mints a short‑lived bearer JWT (EdDSA): `iss=keyId`,
  `aud=api.commonsc.io`, `exp=+1h`, signed with the publisher private key. Sent
  on `algorithms/validate|publish|status`.
- Server gains auth middleware: resolve `iss` → `publishers/{keyId}.json` →
  verify the JWT signature against the stored pubkey; reject expired/forged. On
  publish, *additionally* verify the manifest's publisher signature against the
  same pubkey. Mismatches return a structured error + `remediation.action`.
- Keep a `--dev` offline signer (deterministic) for local `devkit run`/`validate`
  only; the server never accepts dev‑signed submissions.

**Marketplace co‑sign (consumer trust).**
- Generate a real marketplace ed25519 keypair. Private key held server‑side on
  the API box, injected as a deploy secret, readable only by the service user
  (file‑based now; KMS later, as `signing.rs` already anticipates). **The key
  never touches the execution worker.**
- At approval (`admin.rs approve` → `publish::run`) the marketplace co‑signs the
  canonical manifest with the real key.
- Publish the marketplace pubkey at a stable URL and **pin it in the app**,
  replacing the placeholder in `catalog.ts verifyManifest`. The app then verifies
  publisher sig + marketplace co‑sign before running (plumbing already exists).

## Execution worker — settled (separate, secret‑less)

New deployable `commonsc-exec-worker`: a tiny HTTP service on its **own VM**,
holding NO signing key, NO Stripe secrets, NO registry write access.

- Internal‑only endpoint `POST /execute` { bundle bytes, fixture(s), entrypoint,
  tier limits } → returns a `gate-result`‑shaped execution verdict. Reached over
  the private network with a shared internal token; not exposed publicly.
- **Container‑per‑run**: a fresh container (podman/docker) from an image bundling
  deno + pyodide + the host run path. Per run: memory cap (512 MiB), pids limit,
  `--network none`, wall‑clock timeout (30 s) enforced *inside* (host timeout)
  and *outside* (container kill), tempdir destroyed after.
- The marketplace validate/publish handlers call the worker; the API process
  never spawns submitter code. Worker unreachable ⇒ fail closed.
- **Blast radius**: a full sandbox escape on the worker yields only the submitted
  bundle + a synthetic fixture in an ephemeral container — no keys, no payments,
  no registry. This isolation is *required* precisely because Phase B puts a real
  signing key on the API box.
- Cost: one small VM + a container runtime.

## Presentation contract — settled (closed block set + one shared renderer)

Visualisation already has an abstraction: the Result envelope's `blocks` array,
restricted to a **closed set** of kinds (`rows`, `score`, `distribution`,
`table`, `callout`, `bars`) rendered by `CommonSense/ui/src/blocks.tsx` (SVG, no
chart deps). Algorithms emit *data shaped as blocks*; the host owns
*presentation*. We keep it closed — authors do **not** ship
matplotlib/plotly/SVG/HTML:

- **Security** — arbitrary HTML/JS/SVG in the one surface that renders genomic
  results is an XSS/exfiltration hole; a host‑drawn closed set is safe by
  construction.
- **Portability** — the same envelope must render in the desktop app, a future
  web view, a PDF export, and an agent's text summary. A PNG or `<script>` can't.
- **Cost** — matplotlib‑in‑Pyodide is tens of MB; Tier‑1 is 512 MiB / 30 s.
- **Consistency** — every report looks like CommonSense.

The vocabulary grows by *deliberate, reviewed additions* to the closed set
(`bars` was the first; `line`/`scatter`/`heatmap` are plausible future ones),
never by an open door.

Framing: this is the **third leg of the platform contract** — input = VariantSet
schema, compute = the Tier runtime, **output = the Block schema + one reference
renderer**. Today "output" is an app‑internal detail; promote it to a
first‑class, versioned part of the contract.

**The gap — authors render blind.** `blocks.tsx` lives only in the app; `devkit
run` validates envelope *shape* but shows no pixels; and `llms.txt` is stale
(lists five kinds, missing `bars`). An author can't see what users will see until
it's on a user's machine. Fixes, in priority order:

1. **Exact‑fidelity preview = the Phase D in‑app dev surface.** It renders a local
   bundle through the *same* `blocks.tsx` against a real sample, so "what you see
   is what users see" by definition. (Strongest reason to keep D in scope.)
2. **Static block gallery on commonsc.io** — every kind with example JSON beside
   its rendered output. No install, always‑on, agent‑readable; the reference an
   author/agent consults *before* writing code. Highest value‑per‑effort.
3. **One shared reference renderer** — extract `blocks.tsx` into a small versioned
   package reused by the app, the gallery, and a future `devkit preview`, so
   there is no drift between "the contract" and "what renders."
4. **Fix the stale vocabulary now** — add `bars` to `llms.txt`/the contract and
   make `result.schema.json#/$defs/Block` the single source of truth (folds into
   the Phase E honesty fixes).

## Phasing

- **Phase A — `devkit run` + host wall‑clock timeout** (keystone, no infra).
  **✅ shipped.** Unblocks the agent *and* human author loop; everything builds
  on the shared `build_runtime_bundle()` + host dependency. `devkit run` builds
  the runtime bundle, executes it through the shared sidecar against a fixture,
  validates the envelope against the Result schema, and exits non‑zero on
  throw/timeout/non‑conforming output. Host enforces a wall‑clock cap
  (`SidecarConfig.wall_timeout`, default 30 s) via a watchdog SIGKILL.
- **Phase B — real auth.** Marketplace keypair + app pin; devkit signs the
  manifest + JWT with the registered key; server auth middleware + signature
  verification. Crypto only, no new VM. The contract/homepage claims become true.
- **Phase C — execution worker VM + the `execution` gate** wired into
  validate/publish. Depends on A's shared run path.
- **Phase D — in‑app "Test a local bundle" surface.** Its own Tauri command;
  ship after A so the run path is proven in the CLI first.
- **Phase E — contract fixes** (`/algorithms/init` 404, `bars` block kind,
  examples URL). Cheap; do early. The auth wording becomes accurate in B.
- **Phase F — presentation contract.** Static block gallery on commonsc.io +
  extract `blocks.tsx` into a shared versioned reference renderer (app + gallery
  + future `devkit preview`). The `bars`/block‑list doc fix ships with E.

## Open decisions (remaining)

1. In‑app developer entry point: hidden footer link vs Settings toggle vs
   env/flag gate. (Discoverability vs. consumer‑surface cleanliness.)
2. Does `devkit run` rebuild every invocation, or cache by content hash for a
   fast iterate loop?

## Gaps beyond the phased work (not yet scheduled)

Surfaced in the contributor-UX review — bigger than open decisions, not yet on
the A–F track.

- **Contributor payout / revenue share.** A third party can publish a *paid*
  report (pricing in `registry/index.json`, Stripe checkout), but all revenue
  accrues to DruidaLabs — there is no payout to the author. "Marketplace" implies
  sellers get paid; today they don't. Needs a payee identity per publisher
  (Stripe Connect or equivalent), a split/ledger, payout scheduling, and
  tax/KYC. Must be decided before promoting third-party *paid* publishing; free
  contributions are unaffected.
- **Devkit distribution.** The toolkit is build-from-source Rust
  (`cargo build -p commonsc-devkit`), and `devkit run` additionally needs Deno.
  No released binary, no `npm`/`pipx`/`brew`, no hosted runner — the single
  biggest practical barrier for an agent (or human) landing cold. Needs prebuilt
  signed binaries per platform (GitHub Releases) + a one-line installer, deno
  vendored the way the desktop app bundles it, or a hosted "submit project, we
  build + run" endpoint so the local toolchain is optional.
