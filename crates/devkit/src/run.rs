//! `devkit run` — execute an algorithm bundle locally against a fixture.
//!
//! This is the "test before you submit" step the agent contract tells authors
//! to do but never shipped. It builds the *runtime* bundle (exactly what a
//! consumer installs), runs it through the shared Deno + Pyodide sidecar
//! (`commonsc_host`), and checks the returned envelope against
//! `result.schema.json#/$defs/Result` — so a result that wouldn't render in the
//! app fails here, on the author's machine, instead of on a user's.
//!
//! Requires `deno` on PATH and the host crate's in-repo sidecar (Pyodide). The
//! run is wall-clock bounded by the host's Tier-1 default (30s).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use commonsc_host::sidecar::{HostEvent, SidecarConfig, SidecarError};
use sha2::{Digest, Sha256};
use serde_json::{json, Value};

pub struct RunOptions {
    /// Project directory (the one with `manifest.template.json`).
    pub project: PathBuf,
    /// Fixture VariantSet to run against. Defaults to `fixtures/input.json`.
    pub fixture: Option<PathBuf>,
    /// Override the wall-clock limit (seconds). Defaults to the Tier-1 ceiling.
    pub timeout_secs: Option<u64>,
    /// Emit a machine-readable JSON verdict instead of the human report.
    pub json: bool,
}

pub struct RunOutcome {
    pub passed: bool,
    pub summary: String,
    pub result: Option<Value>,
    pub errors: Vec<String>,
    pub elapsed_ms: u128,
    json: bool,
}

impl RunOutcome {
    pub fn print(&self) {
        if self.json {
            let doc = json!({
                "passed": self.passed,
                "summary": self.summary,
                "elapsedMs": self.elapsed_ms,
                "errors": self.errors,
                "result": self.result,
            });
            println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
            return;
        }
        println!("ran in {} ms", self.elapsed_ms);
        if self.passed {
            println!("[OK] {}", self.summary);
            if let Some(r) = &self.result {
                println!("{}", serde_json::to_string_pretty(r).unwrap_or_default());
            }
        } else {
            println!("[FAIL] {}", self.summary);
            for e in &self.errors {
                println!("       {e}");
            }
        }
    }
}

/// Verdict from the execution gate.
pub enum GateOutcome {
    Pass,
    Fail(String),
    /// Deno wasn't available, so the algorithm couldn't be run here. Treated as
    /// non-failing: static-only validation still works offline, and the server
    /// (where Deno is installed) enforces it for real.
    Skipped(String),
}

/// Execute a project's bundle against its fixture in a hardened sandbox and
/// judge it: must not throw, must finish within the wall-clock limit, and must
/// return an envelope conforming to `result.schema.json`. The sidecar runs with
/// a scrubbed environment (`clean_env`) so untrusted code can't read host
/// secrets. This is the marketplace's execution gate.
pub fn execution_gate(project: &Path) -> GateOutcome {
    let manifest = match crate::manifest::read_template(project) {
        Ok(m) => m,
        Err(e) => return GateOutcome::Fail(format!("reading manifest: {e}")),
    };
    let entry = |field: &str| {
        manifest
            .get("entrypoint")
            .and_then(|e| e.get(field))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let (module, function) = match (entry("module"), entry("function")) {
        (Some(m), Some(f)) => (m, f),
        _ => return GateOutcome::Fail("manifest.entrypoint.module/function missing".into()),
    };
    let fixture = project.join("fixtures/input.json");
    let variant_set: serde_json::Value = match std::fs::read_to_string(&fixture)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => return GateOutcome::Fail(format!("missing or unparseable {}", fixture.display())),
    };
    let bundle = match crate::publish::build_runtime_bundle(project) {
        Ok(b) => b,
        Err(e) => return GateOutcome::Fail(format!("building bundle: {e}")),
    };
    let sha = hex::encode(Sha256::digest(&bundle));

    let mut cfg = bundled_sidecar_config();
    cfg.clean_env = true; // untrusted code — don't expose host env (e.g. signing keys)

    match commonsc_host::sidecar::run_one_with_config_events(
        cfg, &bundle, &sha, &module, &function, variant_set, &mut |_| {},
    ) {
        Ok(value) => match crate::validate::result_envelope_errors(&value) {
            Ok(errs) if errs.is_empty() => GateOutcome::Pass,
            Ok(errs) => GateOutcome::Fail(format!("result does not conform: {}", errs.join("; "))),
            Err(e) => GateOutcome::Fail(format!("checking result envelope: {e}")),
        },
        Err(SidecarError::Spawn(_)) => GateOutcome::Skipped("Deno not on PATH".into()),
        Err(SidecarError::Algorithm(m)) => GateOutcome::Fail(format!("algorithm raised: {m}")),
        Err(SidecarError::Timeout { seconds }) => {
            GateOutcome::Fail(format!("exceeded the {seconds}s wall-clock limit"))
        }
        Err(e) => GateOutcome::Fail(format!("sandbox error: {e}")),
    }
}

/// Build the sidecar config, preferring assets shipped *next to the binary* —
/// how the distributed tarball lays them out (`commonsc-devkit` + `sidecar/` +
/// optional `deno`). Falls back to [`SidecarConfig::default`] (the in-repo
/// sidecar) for `cargo run` during development.
fn bundled_sidecar_config() -> SidecarConfig {
    let mut cfg = SidecarConfig::default();
    if let Ok(exe) = std::env::current_exe() {
        // current_exe() may hand back the symlink the installer created (macOS
        // doesn't resolve it); canonicalize so we find the sidecar next to the
        // *real* binary inside the unpacked tarball dir.
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(dir) = exe.parent() {
            let script = dir.join("sidecar").join("run.ts");
            if script.exists() {
                cfg.script = script;
            }
            // A `deno` shipped alongside the binary takes precedence over PATH,
            // so a fully self-contained tarball can include it later.
            let deno = dir.join(if cfg!(windows) { "deno.exe" } else { "deno" });
            if deno.exists() {
                cfg.deno = deno;
            }
        }
    }
    // Honour an explicit Deno cache dir. The marketplace box runs under
    // systemd `ProtectHome=true`, so Deno's default `$HOME/.cache` is
    // unwritable; pointing this at a ReadWritePaths data dir lets the execution
    // gate boot there. Set on cfg (not the child env) so `clean_env` keeps it.
    if let Ok(dir) = std::env::var("COMMONSC_DENO_DIR") {
        if !dir.is_empty() {
            cfg.deno_dir = Some(PathBuf::from(dir));
        }
    }
    cfg
}

pub fn run(opts: RunOptions) -> Result<RunOutcome> {
    let project = &opts.project;
    let manifest = crate::manifest::read_template(project)
        .context("reading manifest.template.json")?;

    let entrypoint = |field: &str| -> Result<String> {
        manifest
            .get("entrypoint")
            .and_then(|e| e.get(field))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("manifest.entrypoint.{field} missing or not a string"))
    };
    let module = entrypoint("module")?;
    let function = entrypoint("function")?;

    // Load the fixture VariantSet (the host accepts it verbatim; the static
    // `validate` gate is where its shape is schema-checked).
    let fixture_path = opts
        .fixture
        .clone()
        .unwrap_or_else(|| project.join("fixtures/input.json"));
    let raw = std::fs::read_to_string(&fixture_path)
        .with_context(|| format!("reading fixture {}", fixture_path.display()))?;
    let variant_set: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing fixture {}", fixture_path.display()))?;

    // Build the exact runtime artifact a consumer would install, and address it
    // by content hash the way the host expects.
    let bundle = crate::publish::build_runtime_bundle(project)
        .context("building runtime bundle")?;
    let sha = hex::encode(Sha256::digest(&bundle));

    let mut on_event = |ev: HostEvent| {
        if let HostEvent::Progress { percent, label } = ev {
            eprintln!("  · {} ({:.0}%)", label.unwrap_or_default(), percent * 100.0);
        }
    };

    let mut cfg = bundled_sidecar_config();
    if let Some(secs) = opts.timeout_secs {
        cfg.wall_timeout = Some(Duration::from_secs(secs));
    }

    let started = Instant::now();
    let run_res = commonsc_host::sidecar::run_one_with_config_events(
        cfg,
        &bundle,
        &sha,
        &module,
        &function,
        variant_set,
        &mut on_event,
    );
    let elapsed_ms = started.elapsed().as_millis();

    match run_res {
        Ok(value) => {
            let errors = crate::validate::result_envelope_errors(&value)?;
            let passed = errors.is_empty();
            let summary = if passed {
                value
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(ran, no summary field)")
                    .to_string()
            } else {
                "result does not conform to result.schema.json#/$defs/Result".to_string()
            };
            Ok(RunOutcome { passed, summary, result: Some(value), errors, elapsed_ms, json: opts.json })
        }
        // Algorithm-level failures are a failing test, not a devkit error —
        // report them and let the caller exit non-zero. Infrastructure failures
        // (no deno, broken pipe) are genuine errors and propagate.
        Err(SidecarError::Algorithm(msg)) => Ok(RunOutcome {
            passed: false,
            summary: "algorithm raised an error".to_string(),
            result: None,
            errors: vec![msg],
            elapsed_ms,
            json: opts.json,
        }),
        Err(SidecarError::Timeout { seconds }) => Ok(RunOutcome {
            passed: false,
            summary: format!("timed out after {seconds}s (Tier-1 wall-clock limit)"),
            result: None,
            errors: vec![format!("the algorithm did not finish within {seconds}s")],
            elapsed_ms,
            json: opts.json,
        }),
        Err(SidecarError::Spawn(e)) => Err(anyhow!(
            "could not start the sandbox — is `deno` on PATH? ({e})"
        )),
        Err(SidecarError::BundleHashMismatch { .. }) => {
            Err(anyhow!("internal: freshly-built bundle failed its own hash check"))
        }
        Err(e) => Err(anyhow!("sandbox run failed: {e}")),
    }
}
