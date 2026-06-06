"""Earwax type — single-variant interpretation at rs17822931 (ABCC11).

The T allele is a loss-of-function variant that produces dry, flaky cerumen
instead of the wet, sticky form. Striking geographic clines: ~95% wet in
European/African populations, ~95% dry in East Asian populations. ABCC11
also affects underarm odour because the same transporter is active in
apocrine sweat.
"""
from __future__ import annotations

import time
from typing import Any

ID = "commonsc/earwax-type"
RSID = "rs17822931"

GENOTYPES: dict[str, dict[str, Any]] = {
    "CC": {
        "summary": "CC — wet earwax",
        "detail": "Both copies of the wild-type ABCC11 allele. Cerumen is yellow, sticky, and slightly oily. Common in European and African populations.",
        "tone": "moss",
    },
    "CT": {
        "summary": "CT — wet earwax",
        "detail": "One functional ABCC11 copy is enough to produce wet cerumen. Functionally indistinguishable from CC for this trait.",
        "tone": "moss",
    },
    "TT": {
        "summary": "TT — dry earwax",
        "detail": "Both copies non-functional. Cerumen is dry, flaky, and pale. Common in East Asian populations; the same genotype also reduces apocrine sweat odour.",
        "tone": "moss",
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
                    {"label": "Gene", "value": "ABCC11"},
                    {"label": "Genotype", "value": v["genotype"]},
                    {"label": "Chromosome", "value": f"chr{v['chrom']}"},
                ],
            },
            {
                "kind": "callout",
                "tone": "neutral",
                "title": "Related: underarm odour",
                "body": "ABCC11 also affects apocrine sweat composition. People with TT (dry earwax) tend to have noticeably reduced underarm odour, which is why deodorant use varies markedly by population.",
            },
        ],
    }
