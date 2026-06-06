//! CommonSense marketplace HTTP server.
//!
//! Static surface (discovery + schemas + catalog) plus the publisher-facing
//! `POST /algorithms/validate` and `POST /algorithms/publish` endpoints. Both
//! call into `commonsc_devkit`'s existing gate code so the local and remote
//! validate paths are the same logic with different fixtures, per brief §3.6.
//!
//! Submissions are persisted as JSON files under `<workspace>/submissions/`.
//! A submission stays `queued` until a reviewer (the admin UI lands in a
//! later milestone) approves it; only then is the bundle promoted into the
//! public registry. The brief §11 makes auto-publish absolute.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Multipart, Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

#[derive(Parser)]
#[command(name = "commonsc-marketplace", about = "CommonSense marketplace HTTP server")]
struct Cli {
    /// TCP port to listen on.
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Workspace root — the directory containing `discovery/`, `product/schemas/`,
    /// `registry/`, and (created on first publish) `submissions/`. Defaults
    /// to the parent of this binary's manifest dir.
    #[arg(long)]
    workspace: Option<PathBuf>,
}

/// Shared state passed to every handler. Only the submissions write needs
/// synchronisation; the rest of the layout is read-only.
#[derive(Clone)]
struct AppState {
    workspace: PathBuf,
    submissions_lock: Arc<Mutex<()>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "commonsc_marketplace=info,tower_http=warn".into()),
        )
        .compact()
        .init();

    let workspace = resolve_workspace(cli.workspace)?;
    let discovery_dir = workspace.join("discovery");
    let schemas_dir = workspace.join("product/schemas");
    let registry_dir = workspace.join("registry");
    let submissions_dir = workspace.join("submissions");

    sanity_check_path("discovery", &discovery_dir)?;
    sanity_check_path("schemas", &schemas_dir)?;
    sanity_check_path("registry", &registry_dir)?;
    std::fs::create_dir_all(&submissions_dir).context("creating submissions dir")?;

    let state = AppState {
        workspace: workspace.clone(),
        submissions_lock: Arc::new(Mutex::new(())),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(root_index))
        .route("/health", get(|| async { "ok" }))
        .route("/algorithms/validate", post(validate_handler))
        .route("/algorithms/publish", post(publish_handler))
        .route("/algorithms/:submission_id/status", get(status_handler))
        .nest_service(
            "/.well-known",
            ServeDir::new(discovery_dir.join(".well-known")),
        )
        .nest_service("/llms.txt", ServeDir::new(discovery_dir.join("llms.txt")))
        .nest_service("/schemas", ServeDir::new(schemas_dir))
        .nest_service("/registry", ServeDir::new(registry_dir))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = ([127, 0, 0, 1], cli.port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding to {addr}"))?;

    tracing::info!(
        "serving CommonSense marketplace on http://{} (workspace: {})",
        addr,
        workspace.display(),
    );
    print_routes();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn sanity_check_path(label: &str, path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!(
            "{label} directory not found at {}\n\nIs the --workspace flag pointing at the commonsc/ root?",
            path.display()
        );
    }
    Ok(())
}

fn resolve_workspace(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.canonicalize().unwrap_or(p));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("can't resolve workspace from {}", manifest_dir.display()))?;
    Ok(workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf()))
}

async fn root_index() -> Json<serde_json::Value> {
    Json(json!({
        "service": "CommonSense marketplace (development)",
        "discovery": "/.well-known/commonsc.json",
        "llms": "/llms.txt",
        "schemas": "/schemas/",
        "catalog": "/registry/index.json",
        "api": {
            "validate": "POST /algorithms/validate",
            "publish": "POST /algorithms/publish",
            "status": "GET /algorithms/{submissionId}/status"
        },
        "health": "/health"
    }))
}

fn print_routes() {
    tracing::info!("  GET  /                                root index (JSON)");
    tracing::info!("  GET  /health                          liveness probe");
    tracing::info!("  GET  /.well-known/commonsc.json       discovery contract");
    tracing::info!("  GET  /llms.txt                        LLM-facing companion");
    tracing::info!("  GET  /schemas/<name>.schema.json      JSON schemas");
    tracing::info!("  GET  /registry/index.json             algorithm catalog");
    tracing::info!("  POST /algorithms/validate             run canonical gates, return gate-result");
    tracing::info!("  POST /algorithms/publish              queue a submission for review");
    tracing::info!("  GET  /algorithms/:id/status           submission status");
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

// ── Validate ──────────────────────────────────────────────────────────────

async fn validate_handler(mut multipart: Multipart) -> Result<Json<Value>, ApiError> {
    let project_dir = extract_project_from_multipart(&mut multipart).await?;
    let project_root = locate_project_root(project_dir.path())?;
    let report = commonsc_devkit::validate::run(&project_root)
        .map_err(|e| ApiError::server(format!("validate failed to run: {e}")))?;
    Ok(Json(report_to_json(&report)))
}

// ── Publish ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct SubmissionRecord {
    submission_id: String,
    manifest_id: String,
    manifest_version: String,
    status: SubmissionStatus,
    submitted_at: i64,
    bundle_sha256: String,
    /// Path (relative to workspace) to the saved project tar.zst awaiting
    /// review.
    project_archive: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum SubmissionStatus {
    Queued,
    InReview,
    Approved,
    Rejected,
}

async fn publish_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    let (project_dir, raw_archive) = extract_project_and_archive(&mut multipart).await?;
    let project_root = locate_project_root(project_dir.path())?;
    let report = commonsc_devkit::validate::run(&project_root)
        .map_err(|e| ApiError::server(format!("validate failed to run: {e}")))?;
    if report.outcome.is_fail() {
        return Ok(Json(json!({
            "status": "validation-failed",
            "gateResult": report_to_json(&report),
        })));
    }

    let manifest = commonsc_devkit::manifest::read_template(&project_root)
        .map_err(|e| ApiError::client(format!("reading manifest: {e}")))?;
    let manifest_id = commonsc_devkit::manifest::id(&manifest)
        .map_err(|e| ApiError::client(format!("manifest id: {e}")))?
        .to_string();
    let manifest_version = commonsc_devkit::manifest::version(&manifest)
        .map_err(|e| ApiError::client(format!("manifest version: {e}")))?
        .to_string();

    let submission_id = generate_submission_id();
    let bundle_sha = hex::encode(Sha256::digest(&raw_archive));

    // Persist the raw archive next to the submission record so the reviewer
    // (and future "approve" handler) can reconstruct the original upload
    // without depending on the temp dir we extracted into.
    let archive_rel = format!("submissions/{submission_id}.tar.zst");
    let archive_path = state.workspace.join(&archive_rel);
    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ApiError::server(e.to_string()))?;
    }
    std::fs::write(&archive_path, &raw_archive).map_err(|e| ApiError::server(e.to_string()))?;

    let record = SubmissionRecord {
        submission_id: submission_id.clone(),
        manifest_id: manifest_id.clone(),
        manifest_version,
        status: SubmissionStatus::Queued,
        submitted_at: now_millis(),
        bundle_sha256: bundle_sha,
        project_archive: archive_rel,
    };

    let record_path = state
        .workspace
        .join(format!("submissions/{submission_id}.json"));
    {
        let _guard = state.submissions_lock.lock().await;
        let body = serde_json::to_string_pretty(&record)
            .map_err(|e| ApiError::server(e.to_string()))?
            + "\n";
        std::fs::write(&record_path, body).map_err(|e| ApiError::server(e.to_string()))?;
    }

    Ok(Json(json!({
        "status": "queued",
        "submissionId": submission_id,
        "manifestId": manifest_id,
        "review": {
            "queue": "human-in-the-loop",
            "policy": "https://commonsc.io/review-policy"
        },
        "gateResult": report_to_json(&report),
    })))
}

// ── Status ────────────────────────────────────────────────────────────────

async fn status_handler(
    State(state): State<AppState>,
    AxumPath(submission_id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    if !is_safe_id(&submission_id) {
        return Err(ApiError::client("invalid submission id".into()));
    }
    let path = state
        .workspace
        .join(format!("submissions/{submission_id}.json"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Err(ApiError::not_found("no submission with that id".into())),
    };
    let record: SubmissionRecord = serde_json::from_str(&raw)
        .map_err(|e| ApiError::server(format!("corrupt submission record: {e}")))?;
    Ok(Json(json!({
        "submissionId": record.submission_id,
        "manifestId": record.manifest_id,
        "manifestVersion": record.manifest_version,
        "status": record.status,
        "submittedAt": record.submitted_at,
        "bundleSha256": record.bundle_sha256,
    })))
}

// ── Multipart + bundle extraction ─────────────────────────────────────────

/// Pull a single multipart field named `bundle` (tar.zst of the project) and
/// unpack it into a fresh temp dir. Returns the dir so callers can pass its
/// path to the gate code. The dir is cleaned up when the handle drops.
async fn extract_project_from_multipart(multipart: &mut Multipart) -> Result<TempDir, ApiError> {
    let (dir, _) = extract_project_and_archive(multipart).await?;
    Ok(dir)
}

/// Same as `extract_project_from_multipart`, but also returns the raw
/// archive bytes so the publish path can persist them for later reviewer
/// retrieval.
async fn extract_project_and_archive(
    multipart: &mut Multipart,
) -> Result<(TempDir, Vec<u8>), ApiError> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::client(format!("invalid multipart upload: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name != "bundle" {
            continue;
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|e| ApiError::client(format!("reading bundle field: {e}")))?
            .to_vec();
        let dir = TempDir::new().map_err(|e| ApiError::server(e.to_string()))?;
        unpack_tar_zst(&bytes, dir.path()).map_err(|e| ApiError::client(e))?;
        return Ok((dir, bytes));
    }
    Err(ApiError::client(
        "multipart upload missing required `bundle` field (expect a tar.zst of the project)".into(),
    ))
}

/// Return the directory that actually holds `manifest.template.json`. We
/// accept two tar layouts: project files at the archive root, or wrapped in
/// a single top-level directory (the common `tar -cf foo.tar dir/` shape).
/// Anything else (no template, or template buried deeper than one level) is
/// a malformed upload.
fn locate_project_root(extracted: &Path) -> std::result::Result<PathBuf, ApiError> {
    if extracted.join("manifest.template.json").is_file() {
        return Ok(extracted.to_path_buf());
    }
    let mut entries = match std::fs::read_dir(extracted) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .collect::<Vec<_>>(),
        Err(e) => return Err(ApiError::server(e.to_string())),
    };
    if entries.len() == 1 {
        let entry = entries.remove(0);
        let child = entry.path();
        if child.is_dir() && child.join("manifest.template.json").is_file() {
            return Ok(child);
        }
    }
    Err(ApiError::client(
        "couldn't find manifest.template.json — expected a tar.zst of a project directory (files at the archive root, or wrapped in a single top-level folder)".to_string(),
    ))
}

fn unpack_tar_zst(bytes: &[u8], dest: &Path) -> std::result::Result<(), String> {
    let decompressed =
        zstd::stream::decode_all(bytes).map_err(|e| format!("zstd decode: {e}"))?;
    let mut archive = tar::Archive::new(decompressed.as_slice());
    archive
        .unpack(dest)
        .map_err(|e| format!("tar unpack: {e}"))?;
    Ok(())
}

fn report_to_json(report: &commonsc_devkit::validate::Report) -> Value {
    let checks: Vec<Value> = report
        .checks
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "title": c.title,
                "status": match c.status {
                    commonsc_devkit::validate::Status::Pass => "pass",
                    commonsc_devkit::validate::Status::Fail => "fail",
                },
                "evidence": c.detail,
            })
        })
        .collect();
    json!({
        "schemaVersion": "1",
        "runAt": now_millis(),
        "runtime": { "kind": "server", "version": env!("CARGO_PKG_VERSION") },
        "outcome": match report.outcome {
            commonsc_devkit::validate::Outcome::Pass => "pass",
            commonsc_devkit::validate::Outcome::Fail => "fail",
        },
        "checks": checks,
    })
}

fn generate_submission_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(micros.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let h = hasher.finalize();
    format!("sub_{}", hex::encode(&h[..8]))
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// ── API error type ────────────────────────────────────────────────────────

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn client(message: String) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }
    fn server(message: String) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
    fn not_found(message: String) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "code": self.status.as_u16(),
                    "message": self.message,
                }
            })),
        )
            .into_response()
    }
}
