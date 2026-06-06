# CommonSense shared schemas

These four JSON Schemas are the spine of CommonSense. Every other component — the desktop
app, the dev-kit CLI, the server pipeline, the autonomous-agent API, the catalog renderer —
validates against them. If two components disagree about what a manifest, an input, an
output, or a gate report looks like, those components have a bug; the schemas are the
arbiter.

They live here, outside any one runtime, so Rust, TypeScript, and Python can all generate
types from the same source.

## The four schemas

| File | What it describes | Produced by | Consumed by |
|---|---|---|---|
| `manifest.schema.json` | A publishable algorithm bundle: id, tier, entrypoint, capabilities, lockfile, signatures | Publisher (via dev-kit) | Marketplace, host loader |
| `genomic-io.schema.json` | Input the host streams into a sandboxed algorithm — variants, reference build, file kinds | Host parser | Algorithm code, contract gate |
| `result.schema.json` | The single result envelope every algorithm returns. The renderer is a closed set. | Algorithm code | Desktop app UI, contract gate |
| `gate-result.schema.json` | Output of running the four canonical compliance gates against a bundle | Dev-kit `validate`, server pipeline | Contributors (humans + agents), marketplace |

## How they fit together

```
                       publisher
                          │
                          ▼
                ┌──────────────────┐
                │  manifest.json   │  ◀── manifest.schema.json
                │  + artifact      │
                └──────────────────┘
                          │
                          ▼
              ┌──────────────────────┐
              │  dev-kit / server    │  ── emits ──▶  gate-result.json
              │  4 canonical gates   │                (gate-result.schema.json)
              └──────────────────────┘
                          │  pass + signed
                          ▼
                  ┌───────────────┐
                  │  marketplace  │
                  └───────────────┘
                          │
                          ▼
                ┌──────────────────┐
                │  customer host   │
                │  verifies sigs   │
                │  + loads bundle  │
                └──────────────────┘
                  │              │
   parses sample  ▼              ▼  sandboxed algorithm
        ┌──────────────┐    ┌──────────────┐
        │ VariantSet   │───▶│  compute()   │
        │ (genomic-io) │    │              │
        └──────────────┘    └──────┬───────┘
                                   │
                                   ▼
                            ┌──────────────┐
                            │   Result     │  ◀── result.schema.json
                            └──────────────┘
                                   │
                                   ▼
                            desktop renderer
```

Three invariants worth highlighting:

1. **The host parses; the algorithm consumes.** Algorithms never see raw file bytes — they
   receive validated `VariantSet` records. The host is the only thing with access to both
   data and network (brief §3.1).
2. **Capabilities are declared in the manifest and the schema is closed.** A plugin cannot
   exfiltrate data because the API to do so does not exist in its sandbox (brief §3.2). New
   capabilities require a schema change, which requires review.
3. **The result envelope is a closed set.** Algorithms compose `rows`, `score`, `table`,
   `distribution`, `callout` — that is the entire visual vocabulary. The renderer never has
   to evaluate algorithm-provided code or markup. This is also the answer to "how do we
   standardise rendering results across algorithms": every algorithm returns the same shape;
   the desktop app has one renderer.

## Examples

`examples/` contains one worked instance of each schema, modelled on the polygenic-height-
score reference algorithm we are building to dogfood the publisher route:

- `manifest.example.json` — what `druidalabs/prs-height@0.1.0` looks like on the wire.
- `variant-set.example.json` — what the host streams into the algorithm.
- `result.example.json` — what the algorithm returns. Exercises every block kind.
- `gate-result.example.json` — what `commonsc validate` prints when one gate fails and
  another warns, showing the structured `remediation` an autonomous agent loops on.

## Versioning

Every schema declares a `schemaVersion` (or, for the manifest, a top-level `"2"` constant).
Bumped on breaking changes only. The customer host refuses unknown majors; non-breaking
additions (new optional fields, new block kinds, new remediation actions) do not bump it.
Add tests alongside any change.

## Open decisions parked here

These will be revisited as we build outward; flagged per brief §12.

- **Lockfile format for non-Pyodide tiers.** Locked to Pyodide today (`interpreter.name`
  enum). WebR and container tiers will introduce their own shapes — the cleanest path is a
  discriminated union on `interpreter.name`, but we defer that until we have a real second
  tier in hand.
- **Signature transport.** We co-sign the manifest, but the artifact's bytes are covered
  only via the manifest's `artifact.sha256`. Acceptable, but means the artifact is not
  independently verifiable without the manifest. Revisit if we ever serve artifacts from a
  registry the marketplace doesn't control.
- **Result block extensibility.** The `Block.oneOf` is closed on purpose (one renderer for
  all algorithms). If a future algorithm genuinely needs a new visualisation, we add a
  block kind here, not in algorithm code.
