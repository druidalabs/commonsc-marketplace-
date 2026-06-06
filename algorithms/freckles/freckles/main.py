"""Freckle tendency — single-variant interpretation at rs1042602 (TYR).

rs1042602 (S192Y) is a non-synonymous SNP in tyrosinase, the rate-limiting
enzyme in melanin synthesis. The A allele encodes a less-active variant of
tyrosinase, leading to less even melanin distribution and a stronger
freckling pattern when the skin is sun-exposed. Often co-occurs with red
hair (MC1R variants), but only ~20% of freckled people are redheads.
"""
from __future__ import annotations

import time
from typing import Any

ID = "commonsc/freckles"
RSID = "rs1042602"

GENOTYPES: dict[str, dict[str, Any]] = {
    "CC": {
        "summary": "CC — typical freckling",
        "detail": "Both copies of the full-activity TYR allele. Melanin synthesis runs at typical rates; freckling is uncommon unless other variants (e.g. MC1R) intervene.",
        "tone": "moss",
    },
    "AC": {
        "summary": "AC — moderate freckling tendency",
        "detail": "One copy of the reduced-activity allele. Sun exposure is more likely to produce visible freckles than for CC homozygotes.",
        "tone": "moss",
    },
    "AA": {
        "summary": "AA — strong freckling tendency",
        "detail": "Both copies of the reduced-activity allele. Freckling is common, especially in childhood and on sun-exposed skin. Often co-occurs with fair skin and increased UV sensitivity.",
        "tone": "amber",
    },
}


def _norm(geno: str) -> str:
    if len(geno) != 2:
        return geno
    return "".join(sorted(geno))


def compute(variant_set: dict[str, Any]) -> dict[str, Any]:
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
                    {"label": "Gene", "value": "TYR (S192Y)"},
                    {"label": "Genotype", "value": v["genotype"]},
                    {"label": "Chromosome", "value": f"chr{v['chrom']}"},
                ],
            },
            {
                "kind": "callout",
                "tone": "amber",
                "title": "Sun behaviour matters more than genotype",
                "body": "Strong freckling tendency tracks with fair-skin UV sensitivity. Regardless of this genotype, the usual sunscreen + shade advice applies.",
            },
        ],
    }
