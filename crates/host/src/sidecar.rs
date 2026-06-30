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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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

    #[error("algorithm exceeded the {seconds}s wall-clock limit and was killed")]
    Timeout { seconds: u64 },

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
    /// Hard wall-clock limit for a single run. When the algorithm outlives it,
    /// the sidecar is SIGKILLed and the run returns [`SidecarError::Timeout`].
    /// Defaults to the Tier-1 ceiling (30s). `None` disables the limit (the run
    /// can block forever — only sensible for trusted local debugging).
    pub wall_timeout: Option<Duration>,
    /// When true, the sidecar is spawned with a scrubbed environment (only
    /// PATH/HOME/TMPDIR + the Deno cache vars), so algorithm code running under
    /// `--allow-env` can't read host secrets (e.g. the marketplace signing key)
    /// out of the process environment. Set it whenever you execute untrusted
    /// code — i.e. the marketplace's execution gate. Default false (the desktop
    /// app runs the user's own chosen algorithms).
    pub clean_env: bool,
}

/// An event surfaced by the sidecar (or synthesised by the host) while a run is
/// in flight. Forwarded to a caller-supplied sink so the desktop app can turn
/// them into user-visible progress; non-consumers pass a no-op.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Progress { percent: f32, label: Option<String> },
    Log { level: String, message: String },
}

impl Default for SidecarConfig {
    fn default() -> Self {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sidecar/run.ts");
        SidecarConfig {
            deno: PathBuf::from("deno"),
            script,
            deno_dir: None,
            allow_read: None,
            wall_timeout: Some(Duration::from_secs(30)),
            clean_env: false,
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
        // Untrusted-code path: start from an empty environment and re-add only
        // what Deno/Pyodide need, so the algorithm can't read host secrets via
        // `--allow-env`. Done before the `.env(...)` calls below so those win.
        if cfg.clean_env {
            command.env_clear();
            for key in ["PATH", "HOME", "TMPDIR"] {
                if let Ok(val) = std::env::var(key) {
                    command.env(key, val);
                }
            }
        }
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
        self.run_with_events(bundle_dir, module, function, variant_set, None, &mut |_| {})
    }

    /// Like [`run`](Self::run) but forwards each in-flight `progress`/`log`
    /// event to `on_event` before blocking on the final result, and enforces an
    /// optional wall-clock `timeout`. The callback runs on the calling thread,
    /// between reads, so it must not block for long.
    ///
    /// Timeout enforcement: a watchdog thread SIGKILLs the sidecar once `timeout`
    /// elapses, which closes its stdout and unblocks the read loop; the
    /// resulting EOF is then reported as [`SidecarError::Timeout`] rather than
    /// `EarlyExit`. Off Unix the kill is a no-op (targets are macOS + Linux), so
    /// the limit is advisory there.
    pub fn run_with_events(
        &mut self,
        bundle_dir: &Path,
        module: &str,
        function: &str,
        variant_set: serde_json::Value,
        timeout: Option<Duration>,
        on_event: &mut dyn FnMut(HostEvent),
    ) -> Result<serde_json::Value, SidecarError> {
        self.send(&HostCommand::Run {
            bundle_dir: bundle_dir.to_string_lossy().to_string(),
            module: module.to_string(),
            function: function.to_string(),
            variant_set,
        })?;

        let timed_out = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watchdog = timeout.map(|limit| {
            let timed_out = Arc::clone(&timed_out);
            let done = Arc::clone(&done);
            let pid = self.child.id();
            thread::spawn(move || {
                let start = Instant::now();
                while start.elapsed() < limit {
                    if done.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                if !done.load(Ordering::Acquire) {
                    timed_out.store(true, Ordering::Release);
                    kill_pid(pid);
                }
            })
        });

        let result = loop {
            match self.read_event() {
                Ok(Event::Result { value }) => break Ok(value),
                Ok(Event::Error { message }) => break Err(SidecarError::Algorithm(message)),
                Ok(Event::Progress { percent, label }) => {
                    on_event(HostEvent::Progress { percent, label });
                }
                Ok(Event::Log { level, message }) => {
                    on_event(HostEvent::Log { level, message });
                }
                Ok(Event::Ready) => {}
                Err(e) => {
                    break Err(if timed_out.load(Ordering::Acquire) {
                        SidecarError::Timeout {
                            seconds: timeout.map(|d| d.as_secs()).unwrap_or(0),
                        }
                    } else {
                        e
                    });
                }
            }
        };

        // Signal the watchdog to stand down and reap it before returning, so a
        // late SIGKILL can't land on the next run's process.
        done.store(true, Ordering::Release);
        if let Some(w) = watchdog {
            let _ = w.join();
        }
        result
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
    cfg: SidecarConfig,
    bundle_bytes: &[u8],
    expected_sha256: &str,
    module: &str,
    function: &str,
    variant_set: serde_json::Value,
) -> Result<serde_json::Value, SidecarError> {
    run_one_with_config_events(
        cfg,
        bundle_bytes,
        expected_sha256,
        module,
        function,
        variant_set,
        &mut |_| {},
    )
}

/// Like [`run_one_with_config`] but forwards in-flight progress/log events to
/// `on_event`. The host also synthesises a few coarse milestones around the
/// stages the sidecar can't see (bundle verify, unpack, sandbox boot) so the
/// caller has something to show before the algorithm's own events start.
pub fn run_one_with_config_events(
    mut cfg: SidecarConfig,
    bundle_bytes: &[u8],
    expected_sha256: &str,
    module: &str,
    function: &str,
    variant_set: serde_json::Value,
    on_event: &mut dyn FnMut(HostEvent),
) -> Result<serde_json::Value, SidecarError> {
    on_event(HostEvent::Progress {
        percent: 0.05,
        label: Some("Verifying bundle".into()),
    });
    let computed = hex::encode(Sha256::digest(bundle_bytes));
    if !computed.eq_ignore_ascii_case(expected_sha256) {
        return Err(SidecarError::BundleHashMismatch {
            declared: expected_sha256.to_string(),
            computed,
        });
    }
    on_event(HostEvent::Progress {
        percent: 0.2,
        label: Some("Starting sandbox".into()),
    });
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
    // Capture before `cfg` is consumed by spawn.
    let timeout = cfg.wall_timeout;
    let mut sidecar = Sidecar::spawn(cfg)?;
    on_event(HostEvent::Progress {
        percent: 0.45,
        label: Some("Sandbox ready".into()),
    });
    let result =
        sidecar.run_with_events(dir.path(), module, function, variant_set, timeout, on_event);
    let _ = sidecar.shutdown();
    result
}

/// SIGKILL a child by pid from the watchdog thread. SIGKILL can't be caught, so
/// the child dies promptly and its stdout closes, unblocking the run loop's
/// blocking read.
#[cfg(unix)]
fn kill_pid(pid: u32) {
    // SAFETY: a bare kill(2) syscall with an integer pid we own. Worst case the
    // pid was already reaped and the call no-ops with ESRCH.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) {
    // No portable kill-by-pid off Unix. Targets are macOS + Linux; on other
    // platforms the wall-clock limit is advisory (the read stays blocked).
}

/// Run an algorithm straight from an unpacked project directory — no bundle
/// bytes, hash check, or unpack. For local developer testing (the desktop app's
/// "test a local bundle" surface): point the sandbox at a project dir and run
/// its entrypoint. Only that dir and the sidecar's own dir are readable.
pub fn run_dir_with_config_events(
    mut cfg: SidecarConfig,
    dir: &Path,
    module: &str,
    function: &str,
    variant_set: serde_json::Value,
    on_event: &mut dyn FnMut(HostEvent),
) -> Result<serde_json::Value, SidecarError> {
    let mut allow = vec![dir.to_path_buf()];
    if let Some(parent) = cfg.script.parent() {
        allow.push(parent.to_path_buf());
    }
    cfg.allow_read = Some(allow);
    let timeout = cfg.wall_timeout;
    let mut sidecar = Sidecar::spawn(cfg)?;
    let result = sidecar.run_with_events(dir, module, function, variant_set, timeout, on_event);
    let _ = sidecar.shutdown();
    result
}

fn unpack_bundle(bytes: &[u8], dest: &Path) -> Result<(), SidecarError> {
    let decompressed = zstd::stream::decode_all(bytes)
        .map_err(|e| SidecarError::BundleUnpack(format!("zstd decode: {e}")))?;
    let mut archive = tar::Archive::new(decompressed.as_slice());
    for entry in archive.entries().map_err(|e| SidecarError::BundleUnpack(format!("tar entries: {e}")))? {
        let mut entry = entry.map_err(|e| SidecarError::BundleUnpack(format!("tar entry read: {e}")))?;
        entry
            .unpack_in(dest)
            .map_err(|e| SidecarError::BundleUnpack(format!("tar unpack: {e}")))?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Event {
    Ready,
    Result { value: serde_json::Value },
    // Forwarded to the caller's event sink by `run_with_events`; the `hello`
    // and plain `run` paths drain them.
    Progress { percent: f32, label: Option<String> },
    Log { level: String, message: String },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Bundle unpacking ─────────────────────────────────────────────────────

    #[test]
    fn unpack_bundle_round_trip() {
        let content = b"test content\n";
        let mut tar_bytes = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, "hello.txt", &content[..])
                .unwrap();
        }
        let mut compressed = Vec::new();
        zstd::stream::copy_encode(&tar_bytes[..], &mut compressed, 3).unwrap();

        let dest = tempfile::tempdir().unwrap();
        unpack_bundle(&compressed, dest.path()).unwrap();
        let extracted = std::fs::read(dest.path().join("hello.txt")).unwrap();
        assert_eq!(extracted, content);
    }

    #[test]
    fn unpack_bundle_rejects_corrupt_zst() {
        let dest = tempfile::tempdir().unwrap();
        let err = unpack_bundle(b"not zstd data at all", dest.path()).unwrap_err();
        assert!(
            matches!(err, SidecarError::BundleUnpack(_)),
            "expected BundleUnpack, got {err:?}"
        );
    }

    #[test]
    fn unpack_bundle_rejects_garbage_tar() {
        let mut compressed = Vec::new();
        zstd::stream::copy_encode(&b"not a valid tar archive"[..], &mut compressed, 3).unwrap();
        let dest = tempfile::tempdir().unwrap();
        let err = unpack_bundle(&compressed, dest.path()).unwrap_err();
        assert!(
            matches!(err, SidecarError::BundleUnpack(_)),
            "expected BundleUnpack, got {err:?}"
        );
    }

    // ── Hash verification ────────────────────────────────────────────────────

    #[test]
    fn hash_mismatch_returns_error_before_spawn() {
        let bytes = b"some bundle bytes";
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        let err = run_one_with_config_events(
            SidecarConfig::default(),
            bytes,
            wrong_hash,
            "mod",
            "fn",
            json!({}),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, SidecarError::BundleHashMismatch { .. }),
            "expected BundleHashMismatch, got {err:?}"
        );
    }

    // ── Config defaults ──────────────────────────────────────────────────────

    #[test]
    fn config_default_deno_is_literal_deno() {
        let cfg = SidecarConfig::default();
        assert_eq!(cfg.deno, PathBuf::from("deno"));
    }

    #[test]
    fn config_default_script_resolves_to_sidecar_dir() {
        let cfg = SidecarConfig::default();
        assert!(
            cfg.script.ends_with("sidecar/run.ts"),
            "expected script to end with sidecar/run.ts, got {}",
            cfg.script.display()
        );
    }

    #[test]
    fn config_default_wall_timeout_is_30_seconds() {
        let cfg = SidecarConfig::default();
        assert_eq!(cfg.wall_timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn config_default_clean_env_is_false() {
        let cfg = SidecarConfig::default();
        assert!(!cfg.clean_env);
    }

    #[test]
    fn config_default_deno_dir_is_none() {
        let cfg = SidecarConfig::default();
        assert!(cfg.deno_dir.is_none());
    }

    #[test]
    fn config_default_allow_read_is_none() {
        let cfg = SidecarConfig::default();
        assert!(cfg.allow_read.is_none());
    }

    #[test]
    fn config_with_allow_read_formats_flag_correctly() {
        let cfg = SidecarConfig {
            allow_read: Some(vec![
                PathBuf::from("/tmp/bundle"),
                PathBuf::from("/opt/sidecar"),
            ]),
            ..SidecarConfig::default()
        };
        let flag = match &cfg.allow_read {
            Some(paths) => {
                let joined = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                format!("--allow-read={joined}")
            }
            None => "--allow-read".to_string(),
        };
        assert_eq!(flag, "--allow-read=/tmp/bundle,/opt/sidecar");
    }

    // ── Error Display formatting ─────────────────────────────────────────────

    #[test]
    fn error_display_spawn() {
        let err = SidecarError::Spawn(std::io::Error::new(std::io::ErrorKind::NotFound, "deno not found"));
        let msg = err.to_string();
        assert!(msg.contains("deno"));
        assert!(msg.contains("PATH"));
    }

    #[test]
    fn error_display_timeout() {
        let err = SidecarError::Timeout { seconds: 42 };
        assert_eq!(err.to_string(), "algorithm exceeded the 42s wall-clock limit and was killed");
    }

    #[test]
    fn error_display_bundle_hash_mismatch() {
        let err = SidecarError::BundleHashMismatch {
            declared: "abc".into(),
            computed: "def".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("abc"));
        assert!(msg.contains("def"));
    }

    #[test]
    fn error_display_algorithm() {
        let err = SidecarError::Algorithm("division by zero".into());
        assert_eq!(err.to_string(), "algorithm reported an error: division by zero");
    }

    #[test]
    fn error_display_broken_pipe() {
        let err = SidecarError::BrokenPipe;
        assert_eq!(err.to_string(), "sidecar stdio pipe disappeared");
    }

    #[test]
    fn error_display_early_exit() {
        let err = SidecarError::EarlyExit;
        assert_eq!(err.to_string(), "sidecar exited before it became ready");
    }

    // ── JSON protocol serialization ──────────────────────────────────────────

    #[test]
    fn host_command_hello_serializes() {
        let cmd = HostCommand::Hello { expr: "1 + 1".into() };
        let json = serde_json::to_string(&cmd).unwrap();
        let expected = json!({"type": "hello", "expr": "1 + 1"});
        assert_eq!(serde_json::from_str::<serde_json::Value>(&json).unwrap(), expected);
    }

    #[test]
    fn host_command_run_serializes() {
        let cmd = HostCommand::Run {
            bundle_dir: "/tmp/bundle".into(),
            module: "mod".into(),
            function: "fn".into(),
            variant_set: json!({"key": "value"}),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let expected = json!({
            "type": "run",
            "bundleDir": "/tmp/bundle",
            "module": "mod",
            "function": "fn",
            "variantSet": {"key": "value"}
        });
        assert_eq!(serde_json::from_str::<serde_json::Value>(&json).unwrap(), expected);
    }

    #[test]
    fn host_command_shutdown_serializes() {
        let cmd = HostCommand::Shutdown;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"type":"shutdown"}"#);
    }

    // ── JSON protocol deserialization ────────────────────────────────────────

    #[test]
    fn event_ready_deserializes() {
        let event: Event = serde_json::from_str(r#"{"type":"ready"}"#).unwrap();
        assert!(matches!(event, Event::Ready));
    }

    #[test]
    fn event_result_deserializes() {
        let event: Event =
            serde_json::from_str(r#"{"type":"result","value":{"ok":true}}"#).unwrap();
        assert!(matches!(event, Event::Result { .. }));
        if let Event::Result { value } = &event {
            assert_eq!(value, &json!({"ok": true}));
        }
    }

    #[test]
    fn event_error_deserializes() {
        let event: Event =
            serde_json::from_str(r#"{"type":"error","message":"oops"}"#).unwrap();
        assert!(matches!(event, Event::Error { .. }));
        if let Event::Error { message } = &event {
            assert_eq!(message, "oops");
        }
    }

    #[test]
    fn event_progress_deserializes() {
        let event: Event =
            serde_json::from_str(r#"{"type":"progress","percent":0.5,"label":"working"}"#).unwrap();
        assert!(matches!(event, Event::Progress { .. }));
        if let Event::Progress { percent, label } = &event {
            assert!((percent - 0.5).abs() < f32::EPSILON);
            assert_eq!(label.as_deref(), Some("working"));
        }
    }

    #[test]
    fn event_progress_without_label_deserializes() {
        let event: Event =
            serde_json::from_str(r#"{"type":"progress","percent":1.0}"#).unwrap();
        assert!(matches!(event, Event::Progress { .. }));
        if let Event::Progress { percent, label } = &event {
            assert!((percent - 1.0).abs() < f32::EPSILON);
            assert!(label.is_none());
        }
    }

    #[test]
    fn event_log_deserializes() {
        let event: Event =
            serde_json::from_str(r#"{"type":"log","level":"info","message":"hello"}"#).unwrap();
        assert!(matches!(event, Event::Log { .. }));
        if let Event::Log { level, message } = &event {
            assert_eq!(level, "info");
            assert_eq!(message, "hello");
        }
    }

    // ── HostEvent construction ───────────────────────────────────────────────

    #[test]
    fn host_event_progress_round_trips() {
        let e = HostEvent::Progress {
            percent: 0.75,
            label: Some("computing".into()),
        };
        match e {
            HostEvent::Progress { percent, label } => {
                assert!((percent - 0.75).abs() < f32::EPSILON);
                assert_eq!(label.unwrap(), "computing");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn host_event_log_round_trips() {
        let e = HostEvent::Log {
            level: "warn".into(),
            message: "low disk".into(),
        };
        match e {
            HostEvent::Log { level, message } => {
                assert_eq!(level, "warn");
                assert_eq!(message, "low disk");
            }
            _ => panic!("wrong variant"),
        }
    }

    // ── run_one_with_config_events progress callbacks ────────────────────────

    #[test]
    fn hash_mismatch_still_fires_start_progress() {
        let mut events = Vec::new();
        let bytes = b"whatever";
        let wrong_hash = "f".repeat(64);
        let _ = run_one_with_config_events(
            SidecarConfig::default(),
            bytes,
            &wrong_hash,
            "m",
            "f",
            json!({}),
            &mut |e| events.push(e),
        );
        assert!(!events.is_empty(), "expected at least one progress event");
        assert!(
            matches!(events.first(), Some(HostEvent::Progress { .. })),
            "first event should be progress"
        );
    }

    // ── SidecarError From impls ──────────────────────────────────────────────

    #[test]
    fn io_error_converts_to_sidecar_error() {
        let io = std::io::Error::new(std::io::ErrorKind::Other, "disk full");
        let err: SidecarError = io.into();
        assert!(matches!(err, SidecarError::Io(_)));
    }

    #[test]
    fn json_error_converts_to_sidecar_error() {
        let json: serde_json::Error = serde_json::from_str::<()>("invalid").unwrap_err();
        let err: SidecarError = json.into();
        assert!(matches!(err, SidecarError::Json(_)));
    }
}
