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

    let outcome = if checks.iter().any(|c| c.status == Status::Fail) {
        Outcome::Fail
    } else {
        Outcome::Pass
    };
    Ok(Report { checks, outcome })
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
