//! `commonsc-devkit evidence <trait>` — look up GWAS Catalog evidence before
//! you build a test.
//!
//! Hits the marketplace's GET /evidence endpoint and prints, per matching
//! trait: the evidence tier, ancestry portability, a REAL PubMed id to cite,
//! and caveats — so a contributor can pick a well-evidenced, in-scope trait and
//! drop a citation into manifest.references that the publish gate will resolve.
//! It's the same evidence layer the in-app AI author uses.

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct EvidenceResponse {
    #[serde(default)]
    count: usize,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    results: Vec<Card>,
}

#[derive(Deserialize)]
struct Card {
    #[serde(rename = "trait")]
    name: String,
    tier: Option<String>,
    portability: Option<String>,
    pmid: Option<String>,
    study_url: Option<String>,
    #[serde(default)]
    caveats: Vec<String>,
    #[serde(default)]
    medical: bool,
    n_studies: Option<u32>,
}

pub fn run(query: &str, api: &str, limit: usize, json: bool) -> Result<()> {
    let url = format!(
        "{}/evidence?q={}&limit={}",
        api.trim_end_matches('/'),
        urlencode(query),
        limit
    );
    let body = ureq::get(&url)
        .call()
        .with_context(|| format!("querying {url}"))?
        .into_string()
        .context("reading /evidence response")?;

    if json {
        println!("{body}");
        return Ok(());
    }

    let parsed: EvidenceResponse =
        serde_json::from_str(&body).context("parsing /evidence response")?;
    if parsed.results.is_empty() {
        println!("No evidence found for \"{query}\". Try a broader term (e.g. the trait, not the gene).");
        return Ok(());
    }

    println!("{} match(es) for \"{}\":\n", parsed.count, query);
    for c in &parsed.results {
        let flag = if c.medical {
            "   ⚠ flagged medical — out of scope (CommonSense is trait/wellness/curiosity only)"
        } else {
            ""
        };
        println!("• {}{}", c.name, flag);
        println!(
            "    tier {}   ·   {}   ·   {} studies",
            c.tier.as_deref().unwrap_or("?"),
            c.portability.as_deref().unwrap_or("ancestry unknown"),
            c.n_studies.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
        );
        if let Some(p) = &c.pmid {
            println!("    cite → {{ \"type\": \"pubmed\", \"id\": \"{p}\" }}");
        } else if let Some(u) = &c.study_url {
            println!("    cite → {{ \"type\": \"url\", \"id\": \"{u}\" }}");
        }
        for cav in &c.caveats {
            println!("    caveat: {cav}");
        }
        println!();
    }
    if let Some(note) = parsed.note {
        println!("{note}");
    }
    Ok(())
}

/// Minimal application/x-www-form-urlencoded encoding of the query, over UTF-8
/// bytes (avoids pulling a URL crate for one query param).
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
