//! Local validate — the dev-kit subset of the four canonical gates.
//!
//! For this iteration we run the cheapest checks: manifest shape (against the
//! published JSON Schema with the post-publish fields treated as optional),
//! presence of the declared entrypoint module file, and shape-check the
//! fixture against the genomic-io VariantSet schema. The full gate runner —
//! actually executing the algorithm in a sandbox and validating its output —
//! lands when the host crate is wired (see crates/host/).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use jsonschema::{JSONSchema, SchemaResolver, SchemaResolverError};
use serde_json::{json, Value};
use url::Url;

const MANIFEST_SCHEMA: &str = include_str!("../../../product/schemas/manifest.schema.json");
const GENOMIC_IO_SCHEMA: &str = include_str!("../../../product/schemas/genomic-io.schema.json");
const RESULT_SCHEMA: &str = include_str!("../../../product/schemas/result.schema.json");

pub struct Report {
    pub checks: Vec<Check>,
    pub outcome: Outcome,
}

pub struct Check {
    pub id: &'static str,
    pub title: String,
    pub status: Status,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
}

impl Outcome {
    pub fn is_fail(self) -> bool {
        matches!(self, Outcome::Fail)
    }
}

impl Report {
    pub fn print(&self) {
        for c in &self.checks {
            let tag = match c.status {
                Status::Pass => "OK  ",
                Status::Fail => "FAIL",
            };
            println!("[{tag}] {} — {}", c.id, c.title);
            if let Some(d) = &c.detail {
                for line in d.lines() {
                    println!("       {line}");
                }
            }
        }
        match self.outcome {
            Outcome::Pass => println!("\nvalidate: pass"),
            Outcome::Fail => println!("\nvalidate: FAIL"),
        }
    }
}

pub fn run(project: &Path) -> Result<Report> {
    let manifest = crate::manifest::read_template(project)
        .context("reading manifest.template.json")?;

    let mut checks = Vec::new();

    // ── Check 1 — manifest matches the published schema (publish-time
    //     fields treated as optional at validate time). ────────────────────
    let manifest_schema: Value = serde_json::from_str(MANIFEST_SCHEMA)
        .context("parsing embedded manifest.schema.json")?;
    let validate_schema = relax_required(&manifest_schema, &["artifact", "checksum", "signatures"]);
    let compiled = JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_resolver(LocalResolver::new())
        .compile(&validate_schema)
        .map_err(|e| anyhow!("compiling manifest schema: {e}"))?;
    let manifest_errors = collect_errors(&compiled, &manifest);
    if manifest_errors.is_empty() {
        checks.push(Check {
            id: "manifest-schema",
            title: "manifest.template.json conforms to manifest.schema.json".into(),
            status: Status::Pass,
            detail: None,
        });
    } else {
        checks.push(Check {
            id: "manifest-schema",
            title: "manifest.template.json conforms to manifest.schema.json".into(),
            status: Status::Fail,
            detail: Some(manifest_errors.join("\n")),
        });
    }

    // ── Check 1b — references are well-formed per type. The schema requires
    //     ≥1 reference; this catches malformed PMIDs/DOIs/URLs early, before
    //     the marketplace gate spends a network round-trip resolving them. ───
    {
        let mut ref_errors = Vec::new();
        if let Some(refs) = manifest.get("references").and_then(Value::as_array) {
            for (i, r) in refs.iter().enumerate() {
                let ty = r.get("type").and_then(Value::as_str).unwrap_or("");
                let id = r.get("id").and_then(Value::as_str).unwrap_or("").trim();
                if let Some(err) = reference_format_error(ty, id) {
                    ref_errors.push(format!("references[{i}]: {err}"));
                }
            }
        }
        checks.push(Check {
            id: "references-format",
            title: "references are well-formed (PMID/DOI/URL)".into(),
            status: if ref_errors.is_empty() { Status::Pass } else { Status::Fail },
            detail: if ref_errors.is_empty() { None } else { Some(ref_errors.join("\n")) },
        });
    }

    // ── Check 2 — entrypoint module file exists at the declared path. ─────
    let module = manifest
        .get("entrypoint")
        .and_then(|e| e.get("module"))
        .and_then(Value::as_str);
    if let Some(module) = module {
        let candidate = entrypoint_candidate(project, module);
        if candidate.iter().any(|p| p.exists()) {
            checks.push(Check {
                id: "entrypoint-present",
                title: format!("entrypoint module `{module}` resolves on disk"),
                status: Status::Pass,
                detail: None,
            });
        } else {
            checks.push(Check {
                id: "entrypoint-present",
                title: format!("entrypoint module `{module}` resolves on disk"),
                status: Status::Fail,
                detail: Some(format!(
                    "expected one of: {}",
                    candidate
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            });
        }
    } else {
        checks.push(Check {
            id: "entrypoint-present",
            title: "entrypoint module declared".into(),
            status: Status::Fail,
            detail: Some("manifest.entrypoint.module missing or not a string".into()),
        });
    }

    // ── Check 3 — fixture validates against the declared input schema. ────
    let fixture = project.join("fixtures/input.json");
    if fixture.exists() {
        let raw = std::fs::read_to_string(&fixture)?;
        let inst: Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing fixture {}", fixture.display()))?;
        // Compile a wrapper that $refs into the genomic-io doc by absolute URL;
        // the resolver hands back the embedded schema. This way inner $refs
        // inside VariantSet resolve cleanly through the same resolver.
        let wrapper = json!({
            "$ref": "https://commonsc.io/schemas/genomic-io.schema.json#/$defs/VariantSet"
        });
        let compiled = JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_resolver(LocalResolver::new())
            .compile(&wrapper)
            .map_err(|e| anyhow!("compiling VariantSet ref: {e}"))?;
        let fixture_errors = collect_errors(&compiled, &inst);
        if fixture_errors.is_empty() {
            checks.push(Check {
                id: "fixture-shape",
                title: "fixtures/input.json validates against VariantSet".into(),
                status: Status::Pass,
                detail: None,
            });
        } else {
            checks.push(Check {
                id: "fixture-shape",
                title: "fixtures/input.json validates against VariantSet".into(),
                status: Status::Fail,
                detail: Some(fixture_errors.join("\n")),
            });
        }
    } else {
        checks.push(Check {
            id: "fixture-shape",
            title: "fixtures/input.json present".into(),
            status: Status::Fail,
            detail: Some(format!("expected {}", fixture.display())),
        });
    }

    // ── Check 4 — the algorithm actually runs and returns a valid envelope. ─
    // The heavy gate: build the bundle, execute it against the fixture in the
    // hardened sandbox, and require a conforming result. Skipped (non-failing)
    // when Deno isn't present, so offline static validation still works; the
    // server enforces it where Deno is installed.
    match crate::run::execution_gate(project) {
        crate::run::GateOutcome::Pass => checks.push(Check {
            id: "execution",
            title: "algorithm runs in the sandbox and returns a valid result".into(),
            status: Status::Pass,
            detail: None,
        }),
        crate::run::GateOutcome::Skipped(why) => checks.push(Check {
            id: "execution",
            title: "algorithm execution".into(),
            status: Status::Pass,
            detail: Some(format!("skipped — {why}")),
        }),
        crate::run::GateOutcome::Fail(why) => checks.push(Check {
            id: "execution",
            title: "algorithm runs in the sandbox and returns a valid result".into(),
            status: Status::Fail,
            detail: Some(why),
        }),
    }

    let outcome = if checks.iter().any(|c| c.status == Status::Fail) {
        Outcome::Fail
    } else {
        Outcome::Pass
    };
    Ok(Report { checks, outcome })
}

/// Validate an algorithm's output envelope against
/// `result.schema.json#/$defs/Result`, resolving inner `$ref`s through the same
/// embedded-schema resolver the manifest/fixture gates use. Returns the list of
/// conformance errors (empty ⇒ valid). Used by `devkit run` to fail a local
/// test whose result wouldn't render in the app.
pub fn result_envelope_errors(result: &Value) -> Result<Vec<String>> {
    let wrapper = json!({
        "$ref": "https://commonsc.io/schemas/result.schema.json#/$defs/Result"
    });
    let compiled = JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_resolver(LocalResolver::new())
        .compile(&wrapper)
        .map_err(|e| anyhow!("compiling Result ref: {e}"))?;
    Ok(collect_errors(&compiled, result))
}

/// Validate one reference's `id` against its `type`. Returns Some(msg) if it's
/// malformed. Public so the marketplace gate applies the same shape rules
/// before it spends a network call resolving the citation.
pub fn reference_format_error(ty: &str, id: &str) -> Option<String> {
    if id.is_empty() {
        return Some("empty id".into());
    }
    match ty {
        "pubmed" => {
            if id.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                Some(format!("PMID must be digits, got `{id}`"))
            }
        }
        "doi" => {
            // DOIs start with a `10.<registrant>/<suffix>`.
            if id.starts_with("10.") && id.contains('/') {
                None
            } else {
                Some(format!("DOI must look like `10.xxxx/...`, got `{id}`"))
            }
        }
        "url" => {
            if id.starts_with("http://") || id.starts_with("https://") {
                None
            } else {
                Some(format!("url must be http(s), got `{id}`"))
            }
        }
        // snpedia / clinvar take a page/variation id; reachability is checked
        // at the gate, so only require it be non-empty (handled above).
        "snpedia" | "clinvar" => None,
        other => Some(format!("unknown reference type `{other}`")),
    }
}

/// Eagerly drain a JSONSchema validation into owned strings, freeing the
/// borrow on the instance before the caller's match arms run.
fn collect_errors(schema: &JSONSchema, instance: &Value) -> Vec<String> {
    match schema.validate(instance) {
        Ok(()) => Vec::new(),
        Err(errors) => errors
            .map(|e| format!("{} (at {})", e, e.instance_path))
            .collect(),
    }
}

/// Where a Python module name like `prs_height.main` might live on disk inside
/// the project: as `prs_height/main.py`, or as `prs_height/__init__.py` when
/// the module name is just `prs_height`.
fn entrypoint_candidate(project: &Path, module: &str) -> Vec<PathBuf> {
    let parts: Vec<&str> = module.split('.').collect();
    let joined: PathBuf = parts.iter().collect();
    vec![
        project.join(joined.with_extension("py")),
        project.join(&joined).join("__init__.py"),
    ]
}

/// Produce a shallow copy of the manifest schema with named fields removed
/// from the top-level `required` array. Used so the validate gate accepts
/// templates that haven't yet been completed by publish.
fn relax_required(schema: &Value, drop: &[&str]) -> Value {
    let mut clone = schema.clone();
    if let Some(obj) = clone.as_object_mut() {
        if let Some(Value::Array(required)) = obj.get("required").cloned() {
            let kept: Vec<Value> = required
                .into_iter()
                .filter(|v| match v.as_str() {
                    Some(s) => !drop.contains(&s),
                    None => true,
                })
                .collect();
            obj.insert("required".to_string(), Value::Array(kept));
        }
    }
    clone
}

/// Resolves the three schema URIs embedded in the binary so cross-`$ref`s
/// don't trigger HTTPS fetches. The customer app does the same trick at
/// runtime via its inlined schema bundle.
struct LocalResolver {
    manifest: Arc<Value>,
    genomic_io: Arc<Value>,
    result: Arc<Value>,
}

impl LocalResolver {
    fn new() -> Self {
        let parse = |s: &str| -> Arc<Value> {
            Arc::new(serde_json::from_str(s).expect("embedded schema parses"))
        };
        LocalResolver {
            manifest: parse(MANIFEST_SCHEMA),
            genomic_io: parse(GENOMIC_IO_SCHEMA),
            result: parse(RESULT_SCHEMA),
        }
    }
}

impl SchemaResolver for LocalResolver {
    fn resolve(
        &self,
        _root_schema: &Value,
        url: &Url,
        _original_reference: &str,
    ) -> Result<Arc<Value>, SchemaResolverError> {
        let path = url.path();
        let basename = path.rsplit('/').next().unwrap_or(path);
        match basename {
            "manifest.schema.json" => Ok(self.manifest.clone()),
            "genomic-io.schema.json" => Ok(self.genomic_io.clone()),
            "result.schema.json" => Ok(self.result.clone()),
            _ => Err(SchemaResolverError::msg(format!(
                "no local schema for {url}"
            ))),
        }
    }
}
