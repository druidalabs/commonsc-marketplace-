"""Eye colour — single-variant interpretation at rs12913832 (HERC2 intron 86).

The G allele up-regulates OCA2 (brown pigment), the A allele suppresses it.
About 75% of variation in blue vs brown eye colour in European-descent
populations is explained by this single SNP — unusual for a complex trait.

Other variants (TYR, SLC24A4, etc.) push the result toward hazel or green
when the HERC2 genotype is ambiguous; this single-SNP read should be taken
as an indication, not a diagnosis.
"""
from __future__ import annotations

import time
from typing import Any

ID = "commonsc/eye-colour"
RSID = "rs12913832"

GENOTYPES: dict[str, dict[str, Any]] = {
    "AA": {
        "summary": "AA — likely blue eyes",
        "detail": "Both copies of the OCA2-suppressing allele. ~80% of AA homozygotes have light irises.",
        "tone": "moss",
    },
    "AG": {
        "summary": "AG — mixed / hazel / green",
        "detail": "One suppressing allele. Eye colour is most variable in heterozygotes — anything from light blue through green to brown.",
        "tone": "moss",
    },
    "GG": {
        "summary": "GG — likely brown eyes",
        "detail": "Both copies up-regulate OCA2. Iris is almost always brown, occasionally darker hazel.",
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
                    {"label": "Gene", "value": "HERC2 / OCA2"},
                    {"label": "Genotype", "value": v["genotype"]},
                    {"label": "Chromosome", "value": f"chr{v['chrom']}"},
                ],
            },
            {
                "kind": "callout",
                "tone": "amber",
                "title": "One SNP is a sketch, not a portrait",
                "body": "Eye colour is influenced by many genes. This rs12913832 read explains roughly three-quarters of blue-vs-brown variation in Europeans but won't capture hazel or green outcomes reliably.",
            },
        ],
    }
