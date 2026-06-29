#!/usr/bin/env python3
"""
Distil the GWAS Catalog studies metadata file into a compact, per-trait
evidence-strength index for AI agents that help build DNA-test / risk-score
tooling.

Input  : the 25-column "studies" download (TSV), e.g.
         gwas-catalog-v1.0.3.1-studies-r2026-06-22.tsv
Output : <prefix>.jsonl  one self-contained evidence card per trait (agent-facing)
         <prefix>.csv     same data flattened (human-readable digest)

Why this shape: the studies file is an INDEX of evidence, not the evidence
itself. Aggregating it per ontology-mapped trait lets an agent answer, up front
and without re-deriving anything:
  - is there credible, well-powered evidence for this trait?
  - which ancestries does that evidence cover (portability risk)?
  - are full summary statistics available, and where?
  - how many independent studies back it, how big, and how recent?

The evidence tier here is a TRANSPARENT TRIAGE HEURISTIC, not a statement of
clinical validity. It tells an agent where to look first, not what to trust
medically. Thresholds are explicit below so you can tune them.

Runs fully locally. No network required. Needs only pandas + stdlib.
"""

import argparse
import json
import re
from collections import defaultdict

import pandas as pd

# --- exact column names from the v1.0.3.1 header --------------------------
C_MAPPED      = "MAPPED_TRAIT"
C_MAPPED_URI  = "MAPPED_TRAIT_URI"
C_DISEASE     = "DISEASE/TRAIT"
C_INIT_N      = "INITIAL SAMPLE SIZE"
C_REPL_N      = "REPLICATION SAMPLE SIZE"
C_ACCESSION   = "STUDY ACCESSION"
C_ASSOC       = "ASSOCIATION COUNT"
C_FULL_SS     = "FULL SUMMARY STATISTICS"
C_SS_LOC      = "SUMMARY STATS LOCATION"
C_GENO        = "GENOTYPING TECHNOLOGY"
C_DATE        = "DATE"

# Broad ancestry buckets keyed to terms that appear in the free-text sample
# fields. The catalog mostly uses these standardised descriptors; the handful
# of nationalities below catch common cases. This is heuristic and extensible:
# add terms as you see them in your data. Anything unmatched -> "Other".
ANCESTRY_TERMS = {
    "European": ["european", "white", "caucasian", "finnish", "german",
                 "british", "icelandic", "sardinian", "estonian"],
    "African": ["african", "yoruba", "luhya", "gambian", "sub-saharan"],
    "African American": ["african american", "african-american", "afro-caribbean"],
    "East Asian": ["east asian", "japanese", "han chinese", "chinese", "korean"],
    "South Asian": ["south asian", "indian", "bangladeshi", "pakistani", "tamil"],
    "Hispanic/Latin American": ["hispanic", "latin american", "latino", "latina"],
    "Greater Middle Eastern": ["middle eastern", "arab", "iranian", "turkish"],
    "Oceanian": ["oceanian", "papuan", "melanesian"],
    "Native American": ["native american", "amerindian", "indigenous american"],
}


def extract_sample_size(text: str) -> int:
    """Sum every integer-looking token in a sample-size description.

    The free-text fields read like '8,956 European ancestry cases, 12,000
    controls'. Summing all numbers gives a rough cohort size. Approximate by
    design -- good enough to rank power, not a precise N.
    """
    if not text:
        return 0
    return sum(int(tok.replace(",", "")) for tok in re.findall(r"\d[\d,]*", text))


def detect_ancestries(text: str) -> set:
    """Keyword-match broad ancestry buckets out of free-text sample prose."""
    if not text:
        return set()
    low = text.lower()
    found = {bucket for bucket, kws in ANCESTRY_TERMS.items()
             if any(k in low for k in kws)}
    return found


def portability(ancestries: set) -> tuple:
    """Return (flag, human note) describing cross-ancestry transfer risk."""
    if not ancestries:
        return "unknown", "Ancestry not parseable from sample text; verify manually."
    if ancestries == {"European"}:
        return "european_only", ("Evidence is European-ancestry only; a score "
                                 "built on it may transfer poorly to other groups.")
    if "European" in ancestries and len(ancestries) > 1:
        return "multi_ancestry", "Includes European plus other ancestries."
    return "non_european", "Evidence is non-European; check the target population matches."


def evidence_tier(n_studies: int, max_n: int, has_ss: bool, n_ancestries: int) -> str:
    """Transparent triage heuristic. Tune the thresholds to your needs."""
    score = 0
    if max_n >= 100_000:
        score += 2
    elif max_n >= 10_000:
        score += 1
    if n_studies >= 5:
        score += 2
    elif n_studies >= 2:
        score += 1
    if has_ss:
        score += 1
    if n_ancestries >= 2:
        score += 1
    if score >= 5:
        return "strong"
    if score >= 3:
        return "moderate"
    return "limited"


def split_aligned(traits: str, uris: str):
    """MAPPED_TRAIT can hold several comma-separated terms; pair each with its
    URI by position. Falls back gracefully if counts disagree."""
    t = [x.strip() for x in (traits or "").split(",") if x.strip()]
    u = [x.strip() for x in (uris or "").split(",") if x.strip()]
    if len(u) != len(t):
        u = u + [""] * (len(t) - len(u))  # pad; never raise on ragged data
    return list(zip(t, u))


def build_index(df: pd.DataFrame, max_examples: int) -> list:
    # accumulate per-trait state
    acc = defaultdict(lambda: {
        "uri": "", "studies": 0, "max_n": 0, "associations": 0,
        "ancestries": set(), "genotyping": set(), "ss_locations": [],
        "ss_count": 0, "accessions": [], "latest_date": "",
    })

    for _, row in df.iterrows():
        init_n = extract_sample_size(row.get(C_INIT_N, ""))
        repl_n = extract_sample_size(row.get(C_REPL_N, ""))
        study_n = init_n + repl_n
        ancestries = detect_ancestries(
            f"{row.get(C_INIT_N, '')} {row.get(C_REPL_N, '')}")
        has_ss = str(row.get(C_FULL_SS, "")).strip().lower() in {"yes", "true", "1"}
        ss_loc = (row.get(C_SS_LOC, "") or "").strip()
        accession = (row.get(C_ACCESSION, "") or "").strip()
        try:
            assoc = int(float(row.get(C_ASSOC, 0) or 0))
        except (ValueError, TypeError):
            assoc = 0
        date = (row.get(C_DATE, "") or "").strip()

        for trait, uri in split_aligned(row.get(C_MAPPED, ""), row.get(C_MAPPED_URI, "")):
            a = acc[trait]
            if uri and not a["uri"]:
                a["uri"] = uri
            a["studies"] += 1
            a["max_n"] = max(a["max_n"], study_n)
            a["associations"] += assoc
            a["ancestries"] |= ancestries
            tech = (row.get(C_GENO, "") or "").strip()
            if tech:
                a["genotyping"].add(tech)
            if has_ss:
                a["ss_count"] += 1
                if ss_loc and len(a["ss_locations"]) < max_examples:
                    a["ss_locations"].append(ss_loc)
            if accession and len(a["accessions"]) < max_examples:
                a["accessions"].append(accession)
            if date > a["latest_date"]:
                a["latest_date"] = date

    # finalise into cards
    cards = []
    for trait, a in acc.items():
        flag, note = portability(a["ancestries"])
        tier = evidence_tier(a["studies"], a["max_n"], a["ss_count"] > 0,
                             len(a["ancestries"]))
        caveats = []
        if flag in ("european_only", "non_european", "unknown"):
            caveats.append(note)
        if a["ss_count"] == 0:
            caveats.append("No full summary statistics; only curated top hits "
                           "are available, which limits polygenic scoring.")
        if a["studies"] == 1:
            caveats.append("Backed by a single study; no independent replication.")
        cards.append({
            "trait": trait,
            "trait_uri": a["uri"],
            "evidence_tier": tier,
            "n_studies": a["studies"],
            "max_sample_size_approx": a["max_n"],
            "total_associations": a["associations"],
            "ancestries": sorted(a["ancestries"]),
            "portability": flag,
            "summary_stats_available": a["ss_count"] > 0,
            "n_studies_with_sumstats": a["ss_count"],
            "sumstats_locations": a["ss_locations"],
            "genotyping_technologies": sorted(a["genotyping"]),
            "latest_study_date": a["latest_date"],
            "example_study_accessions": a["accessions"],
            "caveats": caveats,
        })

    # sort: strongest evidence first, then by power
    order = {"strong": 0, "moderate": 1, "limited": 2}
    cards.sort(key=lambda c: (order[c["evidence_tier"]], -c["max_sample_size_approx"]))
    return cards


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("input", help="path to the GWAS Catalog studies TSV")
    p.add_argument("-o", "--prefix", default="trait_evidence",
                   help="output file prefix (default: trait_evidence)")
    p.add_argument("--max-examples", type=int, default=10,
                   help="cap on accessions / sumstats locations stored per trait")
    p.add_argument("--min-studies", type=int, default=1,
                   help="drop traits backed by fewer than this many studies")
    args = p.parse_args()

    df = pd.read_csv(args.input, sep="\t", dtype=str, keep_default_na=False,
                     on_bad_lines="warn")
    df.columns = [c.strip() for c in df.columns]

    cards = [c for c in build_index(df, args.max_examples)
             if c["n_studies"] >= args.min_studies]

    with open(f"{args.prefix}.jsonl", "w", encoding="utf-8") as fh:
        for card in cards:
            fh.write(json.dumps(card, ensure_ascii=False) + "\n")

    flat = pd.DataFrame([{
        **{k: v for k, v in c.items() if not isinstance(v, list)},
        "ancestries": "; ".join(c["ancestries"]),
        "genotyping_technologies": "; ".join(c["genotyping_technologies"]),
        "example_study_accessions": "; ".join(c["example_study_accessions"]),
        "caveats": " ".join(c["caveats"]),
    } for c in cards])
    flat.to_csv(f"{args.prefix}.csv", index=False)

    tiers = pd.Series([c["evidence_tier"] for c in cards]).value_counts().to_dict()
    print(f"Wrote {len(cards)} trait cards -> {args.prefix}.jsonl / .csv")
    print(f"Evidence tiers: {tiers}")


if __name__ == "__main__":
    main()
