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
    body::to_bytes,
    extract::{FromRequest, Multipart, Path as AxumPath, Request, State},
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

mod admin;

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
pub(crate) struct AppState {
    pub(crate) workspace: PathBuf,
    pub(crate) submissions_lock: Arc<Mutex<()>>,
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
        .route("/publisher/register", post(publisher_register))
        .route("/algorithms/validate", post(validate_handler))
        .route("/algorithms/publish", post(publish_handler))
        .route("/algorithms/:submission_id/status", get(status_handler))
        .route("/admin", get(|| async { axum::response::Redirect::to("/admin/") }))
        .route("/admin/", get(admin::index))
        .route("/admin/submissions/:submission_id", get(admin::detail))
        .route("/admin/submissions/:submission_id/approve", post(admin::approve))
        .route("/admin/submissions/:submission_id/reject", post(admin::reject))
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
    tracing::info!("  POST /publisher/register              register a new publisher (returns keyId)");
    tracing::info!("  POST /algorithms/validate             run canonical gates, return gate-result");
    tracing::info!("  POST /algorithms/publish              queue a submission for review");
    tracing::info!("  GET  /algorithms/:id/status           submission status");
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

// ── Publisher register ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    /// Human-readable display name shown in the catalog. 1-80 chars.
    name: String,
    /// Email, URL, or other contact handle. 1-200 chars.
    contact: String,
    /// Publisher's ed25519 public key, base64 (standard alphabet). Exactly 32 bytes decoded.
    pubkey: String,
    /// Requested namespace. Lowercase kebab-case, 1-40 chars. If absent we
    /// derive one from `name`.
    handle: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterResponse {
    handle: String,
    #[serde(rename = "keyId")]
    key_id: String,
}

async fn publisher_register(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, ApiError> {
    // Field validation. Bound everything tightly so we don't accept absurd
    // payloads that bloat the publishers/ directory.
    if req.name.is_empty() || req.name.len() > 80 {
        return Err(ApiError::client("name must be 1-80 chars".into()));
    }
    if req.contact.is_empty() || req.contact.len() > 200 {
        return Err(ApiError::client("contact must be 1-200 chars".into()));
    }

    use base64::Engine as _;
    let pubkey_bytes = base64::engine::general_purpose::STANDARD
        .decode(req.pubkey.trim())
        .map_err(|e| ApiError::client(format!("pubkey base64 decode failed: {e}")))?;
    if pubkey_bytes.len() != 32 {
        return Err(ApiError::client(format!(
            "pubkey must be exactly 32 bytes when decoded (got {})",
            pubkey_bytes.len()
        )));
    }

    let handle = req.handle.unwrap_or_else(|| slugify_handle(&req.name));
    if !is_valid_handle(&handle) {
        return Err(ApiError::client(format!(
            "invalid handle `{handle}`; expected lowercase kebab-case, 1-40 chars"
        )));
    }

    let publishers_dir = state.workspace.join("publishers");
    std::fs::create_dir_all(&publishers_dir).map_err(|e| ApiError::server(e.to_string()))?;
    let publisher_file = publishers_dir.join(format!("{handle}.json"));
    if publisher_file.exists() {
        return Err(ApiError::client(format!(
            "handle `{handle}` is already registered. Pick a different one with --handle."
        )));
    }

    let key_id = format!("{handle}-2026-01");
    let record = json!({
        "handle": handle,
        "name": req.name,
        "contact": req.contact,
        "pubkey": req.pubkey,
        "keyId": key_id,
        "registeredAt": now_millis(),
    });
    let body = serde_json::to_string_pretty(&record)
        .map_err(|e| ApiError::server(e.to_string()))?
        + "\n";
    {
        let _guard = state.submissions_lock.lock().await;
        std::fs::write(&publisher_file, body).map_err(|e| ApiError::server(e.to_string()))?;
    }

    Ok(Json(RegisterResponse { handle, key_id }))
}

fn slugify_handle(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for c in s.chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

fn is_valid_handle(s: &str) -> bool {
    if s.is_empty() || s.len() > 40 {
        return false;
    }
    let bytes = s.as_bytes();
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

// ── Validate ──────────────────────────────────────────────────────────────

async fn validate_handler(req: Request) -> Result<Json<Value>, ApiError> {
    let (_archive, project_dir) = extract_bundle(req).await?;
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
    req: Request,
) -> Result<Json<Value>, ApiError> {
    let (raw_archive, project_dir) = extract_bundle(req).await?;
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

// ── Bundle extraction (raw or multipart) ──────────────────────────────────

const MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;

/// Pull a tar.zst bundle off an incoming request. Supports two body shapes:
///
/// - **Raw** — `Content-Type: application/zstd` or `application/octet-stream`,
///   body is the bundle bytes. Preferred for CLI/SDK uploads.
/// - **Multipart** — `Content-Type: multipart/form-data`, single field named
///   `bundle`. Backward-compatible with the original curl examples.
///
/// Returns the raw archive bytes (so the publish path can persist them) plus
/// a TempDir holding the unpacked project. The TempDir is cleaned up when it
/// drops, so callers must use it before returning.
async fn extract_bundle(req: Request) -> Result<(Vec<u8>, TempDir), ApiError> {
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let bytes: Vec<u8> = if content_type.starts_with("multipart/form-data") {
        let mut multipart = Multipart::from_request(req, &())
            .await
            .map_err(|e| ApiError::client(format!("invalid multipart upload: {e}")))?;
        let mut found = None;
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| ApiError::client(format!("reading multipart: {e}")))?
        {
            if field.name() == Some("bundle") {
                let raw = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::client(format!("reading bundle field: {e}")))?;
                found = Some(raw.to_vec());
                break;
            }
        }
        found.ok_or_else(|| {
            ApiError::client(
                "multipart upload missing required `bundle` field (expect a tar.zst of the project)"
                    .into(),
            )
        })?
    } else {
        // Raw body. We don't strictly require `application/zstd` because
        // many CLI tools default to `application/octet-stream` for binary
        // uploads; both decode the same way.
        let body = req.into_body();
        to_bytes(body, MAX_BUNDLE_BYTES)
            .await
            .map_err(|e| ApiError::client(format!("reading body: {e}")))?
            .to_vec()
    };

    if bytes.is_empty() {
        return Err(ApiError::client("request body was empty".into()));
    }
    let dir = TempDir::new().map_err(|e| ApiError::server(e.to_string()))?;
    unpack_tar_zst(&bytes, dir.path()).map_err(ApiError::client)?;
    Ok((bytes, dir))
}

/// Return the directory that actually holds `manifest.template.json`. We
/// accept two tar layouts: project files at the archive root, or wrapped in
/// a single top-level directory (the common `tar -cf foo.tar dir/` shape).
/// Anything else (no template, or template buried deeper than one level) is
/// a malformed upload.
pub(crate) fn locate_project_root(extracted: &Path) -> std::result::Result<PathBuf, ApiError> {
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

pub(crate) fn unpack_tar_zst(bytes: &[u8], dest: &Path) -> std::result::Result<(), String> {
    let decompressed =
        zstd::stream::decode_all(bytes).map_err(|e| format!("zstd decode: {e}"))?;
    let mut archive = tar::Archive::new(decompressed.as_slice());
    archive
        .unpack(dest)
        .map_err(|e| format!("tar unpack: {e}"))?;
    Ok(())
}

pub(crate) fn report_to_json(report: &commonsc_devkit::validate::Report) -> Value {
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
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
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
