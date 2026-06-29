//! GWAS Catalog evidence lookup — `GET /evidence?q=<trait>`.
//!
//! Serves the compact per-trait evidence index (algorithms/raw/evidence_index.jsonl,
//! built by build_evidence_index.py) so an authoring agent can front-load the
//! credibility judgement before writing a test: how strong is the evidence, does
//! it port across ancestries, and a REAL PubMed id to cite (which the references
//! gate then resolves — no fabricated citations).
//!
//! Option A guardrail: CommonSense is trait/wellness/curiosity only. Disease /
//! clinical / biomarker / pharmacogenomic traits carry `medical: true` and are
//! excluded by default (set `include_medical=true` to see them). The authoring
//! agent must still apply its own no-medical-claims judgement — the flag is a
//! heuristic hint, not the gate.

use std::path::Path;
use std::sync::Arc;

use axum::{extract::Query, extract::State, response::Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::AppState;

#[derive(Clone, Serialize, Deserialize)]
pub struct EvidenceCard {
    #[serde(rename = "trait")]
    pub name: String,
    #[serde(default)]
    pub trait_uri: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub portability: Option<String>,
    #[serde(default)]
    pub summary_stats: Option<bool>,
    #[serde(default)]
    pub n_studies: Option<u32>,
    #[serde(default)]
    pub pmid: Option<String>,
    #[serde(default)]
    pub study: Option<String>,
    #[serde(default)]
    pub study_url: Option<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub medical: bool,
}

/// Load the index from disk. Missing file ⇒ empty (the endpoint then returns no
/// results rather than failing the whole service).
pub fn load(path: &Path) -> Vec<EvidenceCard> {
    let Ok(text) = std::fs::read_to_string(path) else {
        tracing::warn!("evidence index not found at {} — /evidence will be empty", path.display());
        return Vec::new();
    };
    let cards: Vec<EvidenceCard> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    tracing::info!("loaded {} evidence cards from {}", cards.len(), path.display());
    cards
}

#[derive(Deserialize)]
pub struct EvidenceQuery {
    q: Option<String>,
    limit: Option<usize>,
    /// Default true — return matches with the `medical` flag and let the
    /// authoring agent judge (a keyword classifier can't reliably split
    /// wellness from medical, and hiding traits loses legitimate ones like
    /// caffeine metabolism). Set false to hard-exclude flagged traits.
    include_medical: Option<bool>,
}

/// Crude relevance score of a query against a trait name. 0 ⇒ no match.
fn score(name_lc: &str, q: &str, tokens: &[&str]) -> i32 {
    if name_lc == q {
        return 100;
    }
    if name_lc.contains(q) {
        return 60;
    }
    let hit = tokens.iter().filter(|t| name_lc.contains(*t)).count();
    if hit == 0 {
        return 0;
    }
    let mut s = (hit as i32) * 10;
    if hit == tokens.len() {
        s += 20; // all query words present, in any order
    }
    s
}

pub async fn handler(
    State(state): State<AppState>,
    Query(p): Query<EvidenceQuery>,
) -> Json<Value> {
    let q = p.q.unwrap_or_default().trim().to_lowercase();
    let limit = p.limit.unwrap_or(10).min(50);
    let include_medical = p.include_medical.unwrap_or(true);
    if q.is_empty() {
        return Json(json!({ "query": "", "count": 0, "results": [] }));
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(i32, &EvidenceCard)> = state
        .evidence
        .iter()
        .filter(|c| include_medical || !c.medical)
        .filter_map(|c| {
            let s = score(&c.name.to_lowercase(), &q, &tokens);
            (s > 0).then_some((s, c))
        })
        .collect();
    // Wellness-eligible first, then highest score, then shorter (more specific)
    // name — so the agent sees buildable traits at the top, medical ones flagged.
    scored.sort_by(|a, b| {
        a.1.medical
            .cmp(&b.1.medical)
            .then(b.0.cmp(&a.0))
            .then(a.1.name.len().cmp(&b.1.name.len()))
    });
    let results: Vec<&EvidenceCard> = scored.into_iter().take(limit).map(|(_, c)| c).collect();
    Json(json!({
        "query": q,
        "count": results.len(),
        "note": "Evidence triage, NOT clinical validity. `medical:true` flags disease/clinical/biomarker/pharmacogenomic traits — CommonSense is trait/wellness/curiosity only, so do not build those. The flag is a heuristic; apply your own judgement.",
        "results": results,
    }))
}

/// Convenience for `main` to wrap the loaded cards.
pub type Index = Arc<Vec<EvidenceCard>>;
