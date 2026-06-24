"""Muscle fibre type — single-variant interpretation at rs1815739 (ACTN3 R577X).

ACTN3 encodes alpha-actinin-3, a structural protein in fast-twitch muscle
fibres. The T allele (the "X" / 577X variant) introduces a premature stop codon,
so TT individuals make no functional ACTN3 at all. The C allele ("R") is the
functional copy.

  CC (RR) — two functional copies. Over-represented among elite sprint/power
            athletes; fast-twitch fibres are well served.
  CT (RX) — one functional copy. The most common genotype; a mix.
  TT (XX) — no functional ACTN3. Common (~18% of Europeans) and perfectly
            healthy; slightly over-represented among elite endurance athletes.

This is a single, well-studied SNP, but athletic performance is overwhelmingly
training, environment, and polygenics. Treat this as trait curiosity — it does
not predict whether anyone is or could become an athlete.
"""
from __future__ import annotations

import time
from typing import Any

ID = "druidalabs/actn3-sprint"
RSID = "rs1815739"

GENOTYPES: dict[str, dict[str, Any]] = {
    "CC": {
        "summary": "CC — two functional ACTN3 copies (power-leaning)",
        "detail": "Both copies make alpha-actinin-3. This genotype is over-represented among elite sprint and power athletes.",
        "label": "RR · power",
        "tone": "moss",
    },
    "CT": {
        "summary": "CT — one functional ACTN3 copy (mixed)",
        "detail": "The most common genotype. One working copy of alpha-actinin-3; no strong lean either way.",
        "label": "RX · mixed",
        "tone": "moss",
    },
    "TT": {
        "summary": "TT — no functional ACTN3 (endurance-leaning)",
        "detail": "Both copies carry the 577X stop variant, so no functional alpha-actinin-3 is made — common and healthy, and slightly over-represented among elite endurance athletes.",
        "label": "XX · endurance",
        "tone": "moss",
    },
}


def _norm(geno: str) -> str:
    """Order alleles so CT and TC compare equal; leave non-SNP strings alone."""
    if len(geno) != 2:
        return geno
    return "".join(sorted(geno))


def compute(variant_set: dict[str, Any]) -> dict[str, Any]:
    """Single-variant lookup entry point.

    Receives a VariantSet (genomic-io.schema.json#/$defs/VariantSet) and
    returns a Result envelope (result.schema.json#/$defs/Result).
    """
    variants = {v["rsid"]: v for v in variant_set.get("variants", [])}
    v = variants.get(RSID)
    base = {
        "schemaVersion": "1",
        "algorithmId": ID,
        "algorithmVersion": "0.1.0",
        "computedAt": int(time.time() * 1000),
    }
    if v is None:
        return {
            **base,
            "summary": f"{RSID} was not called in this file",
            "tone": "amber",
            "unavailable": f"{RSID} not present in this sample.",
            "blocks": [],
        }
    geno = _norm(v["genotype"])
    interp = GENOTYPES.get(geno) or GENOTYPES.get(v["genotype"])
    if interp is None:
        return {
            **base,
            "summary": f"{v['genotype']} — unrecognised genotype",
            "tone": "amber",
            "blocks": [],
        }
    return {
        **base,
        "summary": interp["summary"],
        "detail": interp.get("detail"),
        "tone": interp.get("tone", "moss"),
        "blocks": [
            {
                "kind": "rows",
                "title": "Variant",
                "rows": [
                    {"label": "rsID", "value": RSID},
                    {"label": "Gene", "value": "ACTN3 (R577X)"},
                    {"label": "Genotype", "value": v["genotype"]},
                    {"label": "Type", "value": interp["label"], "tone": "moss"},
                ],
            },
            {
                "kind": "callout",
                "tone": "amber",
                "title": "A gene, not a verdict",
                "body": "ACTN3 is one well-studied SNP, but sprint vs endurance is overwhelmingly training, environment, and many other genes. This is trait curiosity — it doesn't predict athletic ability.",
            },
        ],
    }
