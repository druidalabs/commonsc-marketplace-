"""
Polygenic risk score for height.

Reads a VariantSet (input shape conforms to genomic-io.schema.json#/$defs/VariantSet)
and returns a Result envelope (output conforms to result.schema.json). Each weight
contributes `beta * n_effect_alleles` to the running score; the final number is
roughly a z-score against a typical European reference population.

This algorithm is intentionally tiny — eight common height-associated SNPs — so it
fits cleanly in the Tier-1 sandbox: pure-stdlib, no compiled deps, no IO outside
the host-supplied variant stream. Real PRS over 600+ variants ports the same way;
the only thing that changes is the size of `WEIGHTS`.
"""
from __future__ import annotations

import math
import time
from typing import Any

# Eight commonly-chipped height-associated variants. Effect sizes are
# illustrative but in the right ballpark — not for clinical use. The build is
# GRCh38; manifest declares `referenceBuild` so a sample with the wrong build
# would be refused upstream (no silent answer).
WEIGHTS: list[dict[str, Any]] = [
    {"rsid": "rs143384",   "chrom": "20", "effect_allele": "G", "beta":  0.041, "freq": 0.612},
    {"rsid": "rs2871865",  "chrom": "12", "effect_allele": "A", "beta": -0.038, "freq": 0.281},
    {"rsid": "rs7639425",  "chrom": "3",  "effect_allele": "A", "beta":  0.033, "freq": 0.443},
    {"rsid": "rs6060369",  "chrom": "20", "effect_allele": "T", "beta": -0.029, "freq": 0.367},
    {"rsid": "rs2230754",  "chrom": "10", "effect_allele": "C", "beta":  0.027, "freq": 0.518},
    {"rsid": "rs6440003",  "chrom": "3",  "effect_allele": "A", "beta":  0.025, "freq": 0.339},
    {"rsid": "rs1208",     "chrom": "8",  "effect_allele": "A", "beta": -0.022, "freq": 0.245},
    {"rsid": "rs10946808", "chrom": "6",  "effect_allele": "G", "beta":  0.034, "freq": 0.295},
]


def _count_effect_alleles(genotype: str, effect_allele: str) -> int:
    """Number of effect alleles (0, 1, or 2) in a diploid call like 'CT'."""
    return sum(1 for base in genotype if base == effect_allele)


def _population_sigma(weights: list[dict[str, Any]]) -> float:
    """Additive standard deviation of the score across a population in
    Hardy-Weinberg equilibrium, derived from the variant frequencies. Used to
    place the user on a population curve. Floored so we never divide by zero."""
    var = sum((w["beta"] ** 2) * 2.0 * w["freq"] * (1.0 - w["freq"]) for w in weights)
    return math.sqrt(var) or 1e-6


def _normal_bins(sigma: float, n: int = 21) -> list[dict[str, Any]]:
    """Pre-binned normal population curve centred at the median (0), spanning
    ±3.5σ. The host renders this as a histogram with the user's value marked;
    binning happens here because the UI never bins."""
    lo, hi = -3.5 * sigma, 3.5 * sigma
    width = (hi - lo) / n
    bins: list[dict[str, Any]] = []
    for i in range(n):
        a = lo + i * width
        b = a + width
        mid = (a + b) / 2.0
        density = math.exp(-(mid * mid) / (2.0 * sigma * sigma))
        bins.append({"from": round(a, 4), "to": round(b, 4), "count": int(round(density * 1000))})
    return bins


def compute(variant_set: dict[str, Any]) -> dict[str, Any]:
    """Entry point declared by the manifest. Pure: same input → same output."""
    variants_by_rsid = {v["rsid"]: v for v in variant_set.get("variants", [])}

    score = 0.0
    used: list[dict[str, Any]] = []
    missing: list[str] = []

    for w in WEIGHTS:
        v = variants_by_rsid.get(w["rsid"])
        if v is None:
            missing.append(w["rsid"])
            continue
        n = _count_effect_alleles(v["genotype"], w["effect_allele"])
        contribution = w["beta"] * n
        score += contribution
        used.append({"rsid": w["rsid"], "beta": w["beta"], "contribution": contribution,
                     "genotype": v["genotype"]})

    used.sort(key=lambda r: abs(r["contribution"]), reverse=True)

    if not used:
        return {
            "schemaVersion": "1",
            "algorithmId": "druidalabs/prs-height",
            "algorithmVersion": "0.1.0",
            "computedAt": int(time.time() * 1000),
            "summary": "None of the eight required variants were called in this sample.",
            "tone": "amber",
            "unavailable": "All required variants missing.",
            "blocks": [],
        }

    # Convert the raw weighted sum into a population z-score: subtract the
    # population mean (expected effect alleles = 2·freq) and divide by the
    # additive SD, both over the variants this sample actually had. Now 0 is
    # the median and the user sits honestly on a standard-normal curve.
    used_weights = [w for w in WEIGHTS if w["rsid"] in {r["rsid"] for r in used}]
    pop_mean = sum(w["beta"] * 2.0 * w["freq"] for w in used_weights)
    sigma = _population_sigma(used_weights)
    z = (score - pop_mean) / sigma

    # Plain-English tone bucketing on the z-score. Illustrative, not clinical.
    if z >= 0.5:
        tone, headline = "moss", f"Above median (+{z:.1f}σ)."
    elif z <= -0.5:
        tone, headline = "amber", f"Below median ({z:.1f}σ)."
    else:
        tone, headline = "neutral", f"Near median ({z:+.1f}σ)."

    blocks: list[dict[str, Any]] = [
        {
            "kind": "score",
            "title": "Polygenic score",
            "value": round(z, 2),
            "unit": "σ",
            "scale": {"min": -3.5, "max": 3.5},
            "bands": [
                {"at": -2.0, "label": "−2σ",    "tone": "amber"},
                {"at":  0.0, "label": "median", "tone": "neutral"},
                {"at":  2.0, "label": "+2σ",    "tone": "amber"},
            ],
            "interpretation": (
                "Eight variants only — illustrative, not clinical. "
                "Real height PRS uses hundreds of variants and an ancestry-matched reference."
            ),
        },
        {
            "kind": "distribution",
            "title": "Where you fall in the population",
            "bins": _normal_bins(1.0),
            "userValue": round(z, 2),
            "unit": "σ",
        },
        {
            "kind": "rows",
            "title": "Inputs",
            "rows": [
                {"label": "Variants used",   "value": f"{len(used)} of {len(WEIGHTS)} declared"},
                {"label": "Reference build", "value": "GRCh38"},
                {"label": "Missing variants", "value": str(len(missing)),
                 "tone": "amber" if missing else "moss"},
            ],
        },
        {
            "kind": "table",
            "title": "Largest contributions",
            "columns": [
                {"key": "rsid", "label": "Variant"},
                {"key": "beta", "label": "Effect", "align": "right"},
                {"key": "contribution", "label": "Contribution", "align": "right"},
                {"key": "geno", "label": "Your genotype"},
            ],
            "rows": [
                {
                    "rsid": r["rsid"],
                    "beta": f"{r['beta']:+.3f}",
                    "contribution": f"{r['contribution']:+.3f}",
                    "geno": r["genotype"],
                }
                for r in used[:5]
            ],
        },
        {
            "kind": "callout",
            "tone": "amber",
            "title": "Not a clinical result",
            "body": (
                "Height is highly polygenic and strongly environmental. This eight-SNP "
                "estimate captures only a small slice of the genetic signal and is not a "
                "prediction of any individual's height."
            ),
        },
    ]

    return {
        "schemaVersion": "1",
        "algorithmId": "druidalabs/prs-height",
        "algorithmVersion": "0.1.0",
        "computedAt": int(time.time() * 1000),
        "summary": headline,
        "detail": (
            f"Summed the effect sizes of {len(used)} height-associated variants present in "
            f"your sample, weighted by their reported betas, and compared the total against "
            f"a reference distribution centered at zero."
        ),
        "tone": tone,
        "blocks": blocks,
    }
