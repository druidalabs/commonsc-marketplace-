# druidalabs/prs-height

First dogfood algorithm for the CommonSense library. Open-source, Apache-2.0.

## What it does

Sums the weighted contribution of eight common height-associated variants. Returns a
result envelope conforming to `result.schema.json` — a score block, a rows block listing
inputs, a table of largest contributions, and a "not clinical" callout.

The eight-variant table is illustrative, not clinical. Real height PRS uses hundreds of
variants and ancestry-matched references; this exists as the smallest realistic
algorithm that exercises the full publish → install → run pipeline.

## Files

- `manifest.template.json` — what the author writes. The dev-kit fills `artifact`,
  `checksum`, and `signatures` at publish time, emitting the final `manifest.json`.
- `prs_height/main.py` — the entrypoint declared by the manifest
  (`module: "prs_height.main"`, `function: "compute"`).
- `weights.tsv` — the eight-SNP table mirrored from `main.py`'s `WEIGHTS` constant.
  Kept human-readable for review; the code path uses the inlined constant.
- `fixtures/input.json` — a synthetic `VariantSet` against `manifest.input.schemaRef`,
  used by `devkit validate` to smoke-test the entrypoint.

## How to publish

From the repo root:

```sh
cargo run -p commonsc-devkit -- validate commonsc/algorithms/prs-height/
cargo run -p commonsc-devkit -- publish  commonsc/algorithms/prs-height/
```

`validate` runs the local subset of the four canonical gates (manifest schema, fixture
shape, output schema conformance). `publish` bundles the directory into a tar.zst,
signs it with the dev publisher + marketplace keys, and writes the entry into
`commonsc/registry/`. The customer app reads from there on its next catalog pull.
