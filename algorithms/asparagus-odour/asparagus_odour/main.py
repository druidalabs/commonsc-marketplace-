"""Asparagus odour detection — single-variant interpretation at rs4481887
(OR2M7, an olfactory receptor on chromosome 1).

After eating asparagus, most people excrete sulphurous metabolites (mainly
methanethiol). About a quarter of people can't smell them — usually called
"asparagus anosmia." rs4481887 sits in a cluster of olfactory-receptor genes
and is the best-replicated marker of detection ability.
"""
from __future__ import annotations

import time
from typing import Any

ID = "commonsc/asparagus-odour"
RSID = "rs4481887"

GENOTYPES: dict[str, dict[str, Any]] = {
    "AA": {
        "summary": "AA — likely detector",
        "detail": "Both copies of the detector allele. You probably smell the characteristic sulphur note from asparagus pee strongly.",
        "tone": "moss",
    },
    "AG": {
        "summary": "AG — intermediate",
        "detail": "One detector allele. Most heterozygotes detect the odour, but at lower intensity than AA. Some report inconsistent detection.",
        "tone": "moss",
    },
    "GG": {
        "summary": "GG — likely anosmic",
        "detail": "No copies of the detector allele. You may simply not perceive the sulphurous compounds, even though your body still produces them after eating asparagus.",
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
                    {"label": "Gene cluster", "value": "OR2M7 / OR2M5"},
                    {"label": "Genotype", "value": v["genotype"]},
                    {"label": "Chromosome", "value": f"chr{v['chrom']}"},
                ],
            },
        ],
    }
