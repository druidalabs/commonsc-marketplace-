#!/usr/bin/env python3
"""Distil the full trait_evidence.jsonl into a compact, served evidence index.

The full file (one rich card per trait, ~21MB) is the local research artefact.
This produces evidence_index.jsonl: the small subset the marketplace serves at
GET /evidence, with the bulky per-study arrays dropped and three things added:

  - pmid      : a real PubMed id for the trait's most-associated study, joined
                from the source studies TSV (100% PMID coverage). The authoring
                agent cites THIS instead of guessing — and the references gate
                resolves it, so fabricated citations can't sneak through.
  - study_url : the GWAS Catalog study page, as a resolvable fallback citation.
  - medical   : the Option-A guardrail flag. GWAS Catalog is mostly disease
                GWAS, but CommonSense is trait/wellness/curiosity ONLY. We flag
                disease/clinical traits so the authoring agent refuses or
                reframes them instead of shipping a medical-claim test. This is
                a heuristic, not a clinical judgement — the agent's own
                no-medical-claims rule is the real backstop.

    python3 build_evidence_index.py [trait_evidence.jsonl] \
        [--tsv gwas-catalog-...studies....tsv] [-o evidence_index.jsonl]
"""
from __future__ import annotations

import argparse
import csv
import json
import sys

# Source TSV column indices (0-based), from the v1.0.3.1 studies layout.
T_PMID, T_DATE, T_ASSOC, T_MAPPED, T_ACCESSION = 1, 3, 11, 12, 14

MEDICAL_URI_MARKERS = ("MONDO", "Orphanet", "/HP_", "DOID", "/MP_", "/NCIT_")
MEDICAL_KEYWORDS = (
    "disease", "disorder", "cancer", "carcinoma", "tumor", "tumour", "neoplasm",
    "diabetes", "syndrome", "deficiency", "infection", "sclerosis", "arthritis",
    "failure", "ischemic", "ischaemic", "leukemia", "leukaemia", "lymphoma",
    "malignan", "schizophren", "depress", "bipolar", "alzheimer", "parkinson",
    "epilep", "asthma", "hypertension", "coronary", "cardiovascular", "stroke",
    "mortality", "dementia", "psychosis", "anorexia", "obesity", "addiction",
    "dependence", "autism", "adhd", "migraine", "osteoporosis", "glaucoma",
    "cirrhosis", "hepatitis", "nephropathy", "retinopathy", "neuropathy",
    "fibrosis", "anemia", "anaemia", "thrombosis", "embolism", "aneurysm",
    "ulcer", "colitis", "crohn", "psoriasis", "eczema", "dermatitis", "allergy",
    "sepsis", "pneumonia", "covid", "cholesterol", "triglyceride", "ldl", "hdl",
    # Clinical measurements, biomarkers, pharmacogenomics and risk traits are
    # health/medical too — CommonSense is trait/wellness/curiosity only, so
    # these are out, even though they aren't "diseases" by name.
    "measurement", "levels", " level", "biomarker", "response to", "drug",
    "pharmacogenom", "risk", "count", "intake", "medication", "blood pressure",
    "heart rate", "metabolite", "protein quantification", "serum", "plasma",
)


def is_medical(card: dict) -> bool:
    uri = card.get("trait_uri") or ""
    if any(m in uri for m in MEDICAL_URI_MARKERS):
        return True
    name = (card.get("trait") or "").lower()
    return any(k in name for k in MEDICAL_KEYWORDS)


def build_pmid_map(tsv_path: str) -> dict:
    """trait term -> PubMed id of its highest-association-count study (the most
    informative primary citation). Keyed by the same comma-split MAPPED_TRAIT
    terms the distiller uses, so it joins cleanly onto the cards."""
    best: dict[str, tuple[int, str, str]] = {}  # trait -> (assoc, date, pmid)
    csv.field_size_limit(10 * 1024 * 1024)
    with open(tsv_path, newline="") as f:
        reader = csv.reader(f, delimiter="\t")
        next(reader, None)  # header
        for row in reader:
            if len(row) <= T_ACCESSION:
                continue
            pmid = row[T_PMID].strip()
            if not pmid.isdigit():
                continue
            try:
                assoc = int(float(row[T_ASSOC] or 0))
            except (ValueError, TypeError):
                assoc = 0
            date = (row[T_DATE] or "").strip()
            for trait in (t.strip() for t in (row[T_MAPPED] or "").split(",")):
                if not trait:
                    continue
                cur = best.get(trait)
                if cur is None or (assoc, date) > (cur[0], cur[1]):
                    best[trait] = (assoc, date, pmid)
    return {trait: v[2] for trait, v in best.items()}


def compact(card: dict, pmid: str | None) -> dict:
    accs = card.get("example_study_accessions") or []
    primary = accs[0] if accs else None
    return {
        "trait": card.get("trait"),
        "trait_uri": card.get("trait_uri"),
        "tier": card.get("evidence_tier"),
        "portability": card.get("portability"),
        "summary_stats": bool(card.get("summary_stats_available")),
        "n_studies": card.get("n_studies"),
        "pmid": pmid,
        "study": primary,
        "study_url": f"https://www.ebi.ac.uk/gwas/studies/{primary}" if primary else None,
        "caveats": card.get("caveats") or [],
        "medical": is_medical(card),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("input", nargs="?", default="trait_evidence.jsonl")
    ap.add_argument("--tsv", default="gwas-catalog-v1.0.3.1-studies-r2026-06-22.tsv")
    ap.add_argument("-o", "--output", default="evidence_index.jsonl")
    args = ap.parse_args()

    pmid_map: dict[str, str] = {}
    try:
        pmid_map = build_pmid_map(args.tsv)
        print(f"joined PMIDs for {len(pmid_map)} traits from {args.tsv}")
    except FileNotFoundError:
        print(f"warning: {args.tsv} not found — cards will have no pmid", file=sys.stderr)

    n = wellness = medical = with_pmid = 0
    with open(args.input) as fin, open(args.output, "w") as fout:
        for line in fin:
            line = line.strip()
            if not line:
                continue
            card = json.loads(line)
            c = compact(card, pmid_map.get(card.get("trait")))
            fout.write(json.dumps(c, separators=(",", ":")) + "\n")
            n += 1
            medical += c["medical"]
            wellness += not c["medical"]
            with_pmid += c["pmid"] is not None

    print(f"wrote {args.output}: {n} cards "
          f"({wellness} wellness-eligible, {medical} medical-flagged, {with_pmid} with PMID)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
