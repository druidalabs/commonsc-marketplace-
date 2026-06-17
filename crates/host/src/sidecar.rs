//! Spawns and drives the Deno + Pyodide sidecar.
//!
//! Bridge protocol: newline-delimited JSON over stdin/stdout. Every message has a
//! `type` field. Parent ➝ child messages are commands; child ➝ parent messages are
//! lifecycle events (`ready`), final results (`result`), or failures (`error`).
//! The child's stderr is left attached to the host's stderr so panics from inside
//! Deno or Pyodide show up immediately during development.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("failed to spawn deno (is it on PATH? `brew install deno`): {0}")]
    Spawn(#[source] std::io::Error),

    #[error("sidecar exited before it became ready")]
    EarlyExit,

    #[error("sidecar stdio pipe disappeared")]
    BrokenPipe,

    #[error("sidecar protocol error: {0}")]
    Protocol(String),

    #[error("algorithm reported an error: {0}")]
    Algorithm(String),

    #[error("bundle hash mismatch: declared {declared}, computed {computed}")]
    BundleHashMismatch { declared: String, computed: String },

    #[error("bundle unpack failed: {0}")]
    BundleUnpack(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// How the sidecar is launched.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// Path to the Deno binary. Defaults to `deno` (resolved via PATH). The
    /// packaged desktop app overrides this with the bundled sidecar binary.
    pub deno: PathBuf,
    /// Path to the sidecar entry script (TypeScript). Defaults to the in-repo copy
    /// alongside this crate, so `cargo run` works from a fresh checkout. The
    /// packaged app points this at the run.ts copied out of app resources.
    pub script: PathBuf,
    /// Where Deno keeps its cache (transpile + v8 code cache). `None` uses
    /// Deno's default (~/Library/Caches/deno). The packaged app sets this to a
    /// writable app-data dir so nothing depends on a system Deno install.
    pub deno_dir: Option<PathBuf>,
    /// Paths the algorithm is permitted to read from disk via Deno. `None`
    /// keeps the legacy broad `--allow-read`; `Some(paths)` narrows to that
    /// allowlist. The bundle's unpacked tempdir is the only thing in here for
    /// production runs; the cli `hello` debug path leaves it `None` because
    /// it doesn't unpack a bundle.
    pub allow_read: Option<Vec<PathBuf>>,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sidecar/run.ts");
        SidecarConfig {
            deno: PathBuf::from("deno"),
            script,
            deno_dir: None,
            allow_read: None,
        }
    }
}

pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Sidecar {
    /// Launch the sidecar with the tightest permission set Pyodide can boot under.
    ///
    /// What we grant, and why:
    /// - `--allow-read`: Pyodide's wheels and the WASM runtime live in Deno's npm
    ///   cache; reading is unavoidable.
    /// - `--allow-env`: Pyodide consults a small set of env vars during init.
    ///
    /// What we explicitly do NOT grant: `--allow-net`, `--allow-write` (outside
    /// scratch dirs handed in later), `--allow-run`, `--allow-ffi`. Deno's default
    /// is deny, so omission is enough — `--no-prompt` ensures we never silently
    /// upgrade to a permission via an interactive prompt in CI.
    pub fn spawn(cfg: SidecarConfig) -> Result<Self, SidecarError> {
        let allow_read_arg = match &cfg.allow_read {
            None => "--allow-read".to_string(),
            Some(paths) => {
                let joined = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                format!("--allow-read={joined}")
            }
        };
        let mut command = Command::new(&cfg.deno);
        command
            .arg("run")
            .arg("--no-prompt")
            .arg("--quiet")
            .arg(&allow_read_arg)
            .arg("--allow-env")
            .arg(&cfg.script)
            .env("DENO_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // Point Deno's cache at a writable dir when the caller supplies one —
        // the packaged app's resources are read-only, so the default cache
        // location is overridden to app-data.
        if let Some(dir) = &cfg.deno_dir {
            command.env("DENO_DIR", dir);
        }
        let mut child = command.spawn().map_err(SidecarError::Spawn)?;

        let stdin = child.stdin.take().ok_or(SidecarError::BrokenPipe)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(SidecarError::BrokenPipe)?);

        let mut sidecar = Sidecar { child, stdin, stdout };
        sidecar.wait_for_ready()?;
        Ok(sidecar)
    }

    fn wait_for_ready(&mut self) -> Result<(), SidecarError> {
        match self.read_event()? {
            Event::Ready => Ok(()),
            Event::Error { message } => Err(SidecarError::Algorithm(message)),
            other => Err(SidecarError::Protocol(format!(
                "expected `ready`, got {other:?}"
            ))),
        }
    }

    /// Evaluate a Python expression inside the sidecar's Pyodide and return the
    /// result rendered as JSON. Only used for the bring-up smoke test.
    pub fn hello(&mut self, expr: &str) -> Result<serde_json::Value, SidecarError> {
        self.send(&HostCommand::Hello { expr: expr.to_string() })?;
        loop {
            match self.read_event()? {
                Event::Result { value } => return Ok(value),
                Event::Error { message } => return Err(SidecarError::Algorithm(message)),
                // Future progress/log events get drained without surfacing here —
                // the hello path doesn't need them.
                Event::Ready | Event::Progress { .. } | Event::Log { .. } => continue,
            }
        }
    }

    /// Tell the already-booted sidecar to execute an algorithm. The bundle has
    /// already been unpacked to `bundle_dir` by the caller; the sidecar reads
    /// files from that directory into Pyodide's virtual FS, sys.path-inserts
    /// it, imports the entrypoint, and calls `function(variant_set)`.
    pub fn run(
        &mut self,
        bundle_dir: &Path,
        module: &str,
        function: &str,
        variant_set: serde_json::Value,
    ) -> Result<serde_json::Value, SidecarError> {
        self.send(&HostCommand::Run {
            bundle_dir: bundle_dir.to_string_lossy().to_string(),
            module: module.to_string(),
            function: function.to_string(),
            variant_set,
        })?;
        loop {
            match self.read_event()? {
                Event::Result { value } => return Ok(value),
                Event::Error { message } => return Err(SidecarError::Algorithm(message)),
                Event::Ready | Event::Progress { .. } | Event::Log { .. } => continue,
            }
        }
    }

    /// Ask the sidecar to exit cleanly; the child should reap promptly. If it
    /// doesn't, the OS-level kill on drop is the backstop.
    pub fn shutdown(mut self) -> Result<(), SidecarError> {
        let _ = self.send(&HostCommand::Shutdown);
        let _ = self.child.wait();
        Ok(())
    }

    fn send(&mut self, cmd: &HostCommand) -> Result<(), SidecarError> {
        let line = serde_json::to_string(cmd)?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_event(&mut self) -> Result<Event, SidecarError> {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line)?;
        if n == 0 {
            return Err(SidecarError::EarlyExit);
        }
        let event: Event = serde_json::from_str(line.trim_end())?;
        Ok(event)
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Best-effort kill; ignore errors because the process may already be gone.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum HostCommand {
    Hello {
        expr: String,
    },
    Run {
        #[serde(rename = "bundleDir")]
        bundle_dir: String,
        module: String,
        function: String,
        #[serde(rename = "variantSet")]
        variant_set: serde_json::Value,
    },
    Shutdown,
}

/// One-shot helper: spawn a sidecar, run one algorithm, shut it down. Verifies
/// the bundle hash, unpacks into a tempdir (which is cleaned on return), and
/// returns the algorithm's Result envelope. Used by the Tauri integration —
/// every customer-side run is a fresh Pyodide instance so module state can't
/// leak between runs.
pub fn run_one(
    bundle_bytes: &[u8],
    expected_sha256: &str,
    module: &str,
    function: &str,
    variant_set: serde_json::Value,
) -> Result<serde_json::Value, SidecarError> {
    run_one_with_config(
        SidecarConfig::default(),
        bundle_bytes,
        expected_sha256,
        module,
        function,
        variant_set,
    )
}

/// Like [`run_one`] but with an explicit [`SidecarConfig`] — the packaged
/// desktop app passes one pointing `deno`/`script`/`deno_dir` at bundled
/// resources instead of a system Deno install. `allow_read` is computed here
/// (bundle tempdir + the sidecar script's directory), overriding any the
/// caller set, so callers only need to supply the paths.
pub fn run_one_with_config(
    mut cfg: SidecarConfig,
    bundle_bytes: &[u8],
    expected_sha256: &str,
    module: &str,
    function: &str,
    variant_set: serde_json::Value,
) -> Result<serde_json::Value, SidecarError> {
    let computed = hex::encode(Sha256::digest(bundle_bytes));
    if !computed.eq_ignore_ascii_case(expected_sha256) {
        return Err(SidecarError::BundleHashMismatch {
            declared: expected_sha256.to_string(),
            computed,
        });
    }
    let dir = TempDir::new().map_err(SidecarError::Io)?;
    unpack_bundle(bundle_bytes, dir.path())?;

    // Two paths the sidecar legitimately needs to read:
    //   1. The bundle tempdir — the algorithm's own code + data.
    //   2. The sidecar's own directory — Pyodide loads its WASM and stdlib
    //      from `node_modules/` next to run.ts. Without read access here Deno
    //      refuses to bootstrap Pyodide.
    // Everything else on disk (/etc, $HOME, /Users) stays blocked.
    let mut allow = vec![dir.path().to_path_buf()];
    if let Some(parent) = cfg.script.parent() {
        allow.push(parent.to_path_buf());
    }
    cfg.allow_read = Some(allow);
    let mut sidecar = Sidecar::spawn(cfg)?;
    let result = sidecar.run(dir.path(), module, function, variant_set);
    let _ = sidecar.shutdown();
    result
}

fn unpack_bundle(bytes: &[u8], dest: &Path) -> Result<(), SidecarError> {
    let decompressed = zstd::stream::decode_all(bytes)
        .map_err(|e| SidecarError::BundleUnpack(format!("zstd decode: {e}")))?;
    let mut archive = tar::Archive::new(decompressed.as_slice());
    archive
        .unpack(dest)
        .map_err(|e| SidecarError::BundleUnpack(format!("tar unpack: {e}")))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Event {
    Ready,
    Result { value: serde_json::Value },
    // Progress and Log are part of the bridge protocol but not yet routed to a
    // consumer in this milestone; suppress unused-field noise rather than dropping
    // them and re-adding next iteration.
    Progress {
        #[allow(dead_code)] percent: f32,
        #[allow(dead_code)] label: Option<String>,
    },
    Log {
        #[allow(dead_code)] level: String,
        #[allow(dead_code)] message: String,
    },
    Error { message: String },
}
