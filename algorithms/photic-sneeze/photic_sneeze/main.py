"""Photic sneeze reflex — single-variant interpretation at rs10427255.

The autosomal dominant compelling helio-ophthalmic outburst reflex (ACHOO):
about 18–35% of people sneeze in response to sudden bright light, especially
sunlight after coming out of a dark room. rs10427255 on chromosome 2 sits
near TRPV1 and is the best-replicated marker, though the mechanism (probably
crossed signals between the trigeminal and optic nerves) is still debated.
"""
from __future__ import annotations

import time
from typing import Any

ID = "commonsc/photic-sneeze"
RSID = "rs10427255"

GENOTYPES: dict[str, dict[str, Any]] = {
    "CC": {
        "summary": "CC — typical (no photic sneeze reflex)",
        "detail": "Both copies of the reference allele. Sudden bright light usually doesn't trigger a sneeze.",
        "tone": "moss",
    },
    "CT": {
        "summary": "CT — sometimes triggered",
        "detail": "One copy of the photic-sneeze-associated allele. Some heterozygotes report occasional bright-light sneezing, others none.",
        "tone": "moss",
    },
    "TT": {
        "summary": "TT — strong photic sneeze reflex",
        "detail": "Both copies of the photic-sneeze-associated allele. Stepping into bright sunlight (or even a sudden lamp) often triggers an involuntary sneeze.",
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
                    {"label": "Region", "value": "near TRPV1"},
                    {"label": "Genotype", "value": v["genotype"]},
                    {"label": "Chromosome", "value": f"chr{v['chrom']}"},
                ],
            },
            {
                "kind": "callout",
                "tone": "neutral",
                "title": "Trivia, not a clinical finding",
                "body": "The photic sneeze reflex is benign — Aristotle wrote about it 2300 years ago. Some pilots get screened for it because an involuntary sneeze on a sun-glare turn is a real cockpit hazard.",
            },
        ],
    }
