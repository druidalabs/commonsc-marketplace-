# Muscle fibre type (ACTN3)

`druidalabs/actn3-sprint` — scaffolded by `commonsc-devkit init`.

## What to edit

- `actn3_sprint/main.py` — fill the `GENOTYPES` table with real interpretations
  per genotype. Tone values (`moss`, `amber`, `rust`, `neutral`) drive the
  result card colour.
- `manifest.template.json` — confirm the blurb, category, and `supportedKinds`
  match what the algorithm actually does. Bump `version` for every change once
  you've published the first cut.
- `fixtures/input.json` — adjust the genotype on the placeholder variant so
  validation exercises an interpretation path you care about.

## Workflow

```sh
# Verify the scaffold passes the local gates (manifest schema, entrypoint
# import, fixture conforms to VariantSet).
cargo run -p commonsc-devkit -- validate <this-dir>

# Bundle, sign with the dev keys, and write into commonsc/registry/.
cargo run -p commonsc-devkit -- publish <this-dir>
```

After publish, the customer app picks up the new algorithm on the next catalog
pull. Bumping `version` and re-publishing produces an "update available" pill
on the installed tile.
