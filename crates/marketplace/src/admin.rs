//! Reviewer admin UI for the submission queue.
//!
//! All endpoints are mounted under `/admin/` and are intended to be reached
//! through nginx with HTTP basic auth in front (single-tenant — just the
//! CommonSense team). The pages are server-rendered HTML so the marketplace
//! binary remains a single deployable; no JS build step, no SPA bundle.
//!
//! Approve flow runs `commonsc_devkit::publish::run` against the submitted
//! project, which writes the signed bundle into `registry/` and updates the
//! catalog index. Reject just marks the submission and records a reason.
//!
//! Per brief §11: no auto-publish, ever. Every catalog entry passes through
//! this surface.

use std::fs;
use std::path::{Path, PathBuf};

use axum::{
    extract::{Form, Path as AxumPath, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::TempDir;

use crate::{locate_project_root, AppState, ApiError};

/// Stored alongside the submission record once a reviewer acts on it.
/// Records when the action happened and the reviewer's note (for rejects).
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct ReviewDecision {
    pub decided_at: i64,
    pub reason: Option<String>,
}

/// Extends the SubmissionRecord shape in `main.rs` with optional decision
/// metadata. Stored as a JSON sidecar (`.review.json`) so we don't have to
/// rev the original record schema each time the workflow grows.
fn decision_path(workspace: &Path, submission_id: &str) -> PathBuf {
    workspace.join(format!("submissions/{submission_id}.review.json"))
}

fn submission_record_path(workspace: &Path, submission_id: &str) -> PathBuf {
    workspace.join(format!("submissions/{submission_id}.json"))
}

#[derive(Debug, Deserialize)]
pub struct StoredSubmission {
    pub submission_id: String,
    pub manifest_id: String,
    pub manifest_version: String,
    pub status: String,
    pub submitted_at: i64,
    pub bundle_sha256: String,
    pub project_archive: String,
}

fn list_submissions(workspace: &Path) -> Vec<StoredSubmission> {
    let dir = workspace.join("submissions");
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // Skip the .review.json sidecars (they end in ".review.json" so the
        // extension is still "json" — filter by full filename).
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.ends_with(".review.json"))
            .unwrap_or(false)
        {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(rec) = serde_json::from_str::<StoredSubmission>(&text) {
                out.push(rec);
            }
        }
    }
    out.sort_by(|a, b| b.submitted_at.cmp(&a.submitted_at));
    out
}

// ── HTML rendering helpers ────────────────────────────────────────────────

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · CommonSense reviewer</title>
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
    max-width: 980px;
    margin: 32px auto;
    padding: 0 20px;
    color: #231c12;
    background: #FBF6EC;
  }}
  h1, h2 {{ font-weight: 500; letter-spacing: -0.5px; margin-bottom: 0.4em; }}
  h1 a {{ color: inherit; text-decoration: none; }}
  .nav {{ font-family: ui-monospace, monospace; font-size: 12px; color: #8B7A60;
          letter-spacing: 0.4px; text-transform: uppercase; margin-bottom: 24px; }}
  .nav a {{ color: #3D5A3A; text-decoration: none; }}
  table {{ width: 100%; border-collapse: collapse; margin-top: 16px; }}
  th, td {{ text-align: left; padding: 10px 8px; border-bottom: 0.5px solid #d8cdb7; font-size: 13.5px; }}
  th {{ font-family: ui-monospace, monospace; font-size: 11px; color: #8B7A60;
        letter-spacing: 0.4px; text-transform: uppercase; }}
  tr a {{ color: #231c12; text-decoration: none; }}
  tr:hover {{ background: rgba(61,90,58,0.05); }}
  .status {{ font-family: ui-monospace, monospace; font-size: 11px; padding: 2px 8px;
             border-radius: 99px; letter-spacing: 0.4px; text-transform: uppercase; }}
  .status.queued    {{ background: rgba(168,118,42,0.10); color: #A8762A; }}
  .status.approved  {{ background: rgba(61,90,58,0.10);  color: #3D5A3A; }}
  .status.rejected  {{ background: rgba(142,74,38,0.10); color: #8E4A26; }}
  .status.in-review {{ background: rgba(168,118,42,0.10); color: #A8762A; }}
  pre {{ background: #f3eddd; padding: 14px; border-radius: 6px; overflow-x: auto;
         font-size: 12px; line-height: 1.5; }}
  .actions {{ display: flex; gap: 12px; margin-top: 24px; }}
  form {{ display: inline; }}
  button {{ font: inherit; padding: 10px 18px; border-radius: 6px; border: none;
            cursor: pointer; font-weight: 600; font-size: 13.5px; }}
  button.approve {{ background: #3D5A3A; color: #FBF6EC; }}
  button.reject  {{ background: transparent; color: #8E4A26;
                    box-shadow: inset 0 0 0 0.5px rgba(142,74,38,0.5); }}
  input[type=text] {{ font: inherit; padding: 9px 12px; border-radius: 6px;
                       border: 0.5px solid #d8cdb7; background: white; width: 380px; }}
  .meta {{ display: grid; grid-template-columns: max-content 1fr; gap: 4px 16px;
           font-family: ui-monospace, monospace; font-size: 12px; margin: 16px 0; }}
  .meta dt {{ color: #8B7A60; }}
  .meta dd {{ color: #231c12; margin: 0; }}
  .flash {{ background: rgba(61,90,58,0.08); border-left: 3px solid #3D5A3A;
            padding: 12px 16px; margin-bottom: 16px; font-size: 13.5px; }}
  .flash.error {{ background: rgba(142,74,38,0.08); border-left-color: #8E4A26; color: #8E4A26; }}
  .empty {{ padding: 48px 24px; text-align: center; color: #8B7A60; font-style: italic; }}
</style>
</head>
<body>
<div class="nav"><a href="/admin/">CommonSense reviewer</a> · {title}</div>
{body}
</body>
</html>"#,
        title = html_escape(title),
        body = body,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── Endpoints ─────────────────────────────────────────────────────────────

pub async fn index(State(state): State<AppState>) -> Html<String> {
    let submissions = list_submissions(&state.workspace);
    let body = if submissions.is_empty() {
        r#"<h1>Submission queue</h1>
<div class="empty">No submissions yet. When a publisher POSTs to <code>/algorithms/publish</code>, it lands here.</div>"#
            .to_string()
    } else {
        let rows: Vec<String> = submissions
            .iter()
            .map(|s| {
                format!(
                    r#"<tr>
  <td><a href="/admin/submissions/{id}">{id}</a></td>
  <td>{manifest_id}</td>
  <td>v{version}</td>
  <td><span class="status {status}">{status_label}</span></td>
  <td>{when}</td>
</tr>"#,
                    id = html_escape(&s.submission_id),
                    manifest_id = html_escape(&s.manifest_id),
                    version = html_escape(&s.manifest_version),
                    status = html_escape(&s.status),
                    status_label = html_escape(&s.status),
                    when = fmt_relative(s.submitted_at),
                )
            })
            .collect();
        format!(
            r#"<h1>Submission queue</h1>
<table>
<thead><tr>
  <th>Submission</th><th>Manifest</th><th>Version</th><th>Status</th><th>Submitted</th>
</tr></thead>
<tbody>
{rows}
</tbody>
</table>"#,
            rows = rows.join("\n"),
        )
    };
    Html(page("Queue", &body))
}

pub async fn detail(
    State(state): State<AppState>,
    AxumPath(submission_id): AxumPath<String>,
    flash: Option<axum::extract::Query<FlashParam>>,
) -> Response {
    if !is_safe_id(&submission_id) {
        return Html(page(
            "Not found",
            r#"<h1>Not found</h1><p>Invalid submission id.</p>"#,
        ))
        .into_response();
    }
    let record_path = submission_record_path(&state.workspace, &submission_id);
    let Ok(raw) = fs::read_to_string(&record_path) else {
        return Html(page(
            "Not found",
            r#"<h1>Not found</h1><p>No submission with that id.</p>"#,
        ))
        .into_response();
    };
    let Ok(record) = serde_json::from_str::<StoredSubmission>(&raw) else {
        return Html(page(
            "Corrupt record",
            r#"<h1>Corrupt record</h1><p>The submission record on disk failed to parse.</p>"#,
        ))
        .into_response();
    };

    // Re-validate from the saved archive — the gate result the publish
    // endpoint produced wasn't persisted, and validating fresh on view
    // means the reviewer sees what's actually about to be approved.
    let validation = revalidate(&state.workspace, &record.project_archive);
    let (gate_html, gate_passed) = match validation {
        Ok(report) => (
            format!(
                r#"<pre>{}</pre>"#,
                html_escape(&serde_json::to_string_pretty(&report).unwrap_or_default())
            ),
            report
                .get("outcome")
                .and_then(Value::as_str)
                .map(|s| s == "pass")
                .unwrap_or(false),
        ),
        Err(e) => (
            format!(
                r#"<p class="flash error">Couldn't re-validate this submission: {}</p>"#,
                html_escape(&e)
            ),
            false,
        ),
    };

    // Pull the decision sidecar if it exists, so re-visiting an
    // approved/rejected entry shows the decision context.
    let decision: Option<ReviewDecision> = fs::read_to_string(decision_path(&state.workspace, &submission_id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    let actions_html = match record.status.as_str() {
        "queued" | "in-review" if gate_passed => format!(
            r#"<div class="actions">
  <form method="post" action="/admin/submissions/{id}/approve">
    <button class="approve" type="submit">Approve &amp; promote to catalog</button>
  </form>
  <form method="post" action="/admin/submissions/{id}/reject">
    <input type="text" name="reason" placeholder="Reason (optional, recorded on the submission)">
    <button class="reject" type="submit">Reject</button>
  </form>
</div>"#,
            id = html_escape(&submission_id),
        ),
        "queued" | "in-review" => format!(
            r#"<p class="flash error">Gates didn't pass on re-validation — approve is disabled. The submitter needs to fix and re-publish.</p>
<div class="actions">
  <form method="post" action="/admin/submissions/{id}/reject">
    <input type="text" name="reason" placeholder="Reason (optional, recorded on the submission)">
    <button class="reject" type="submit">Reject</button>
  </form>
</div>"#,
            id = html_escape(&submission_id),
        ),
        other => {
            let reason = decision
                .as_ref()
                .and_then(|d| d.reason.as_deref())
                .unwrap_or("");
            let reason_html = if reason.is_empty() {
                String::new()
            } else {
                format!(r#"<p class="flash">Reason: {}</p>"#, html_escape(reason))
            };
            format!(
                r#"<p class="flash">Already {}. No further action.</p>{}"#,
                html_escape(other),
                reason_html
            )
        }
    };

    let flash_html = flash
        .and_then(|q| q.0.flash)
        .map(|f| format!(r#"<p class="flash">{}</p>"#, html_escape(&f)))
        .unwrap_or_default();

    let body = format!(
        r#"<h1>{manifest_id} <span style="color:#8B7A60;font-weight:400">v{version}</span></h1>
{flash}
<dl class="meta">
  <dt>Submission</dt> <dd>{id}</dd>
  <dt>Status</dt>     <dd><span class="status {status}">{status_label}</span></dd>
  <dt>Submitted</dt>  <dd>{when}</dd>
  <dt>Bundle sha256</dt> <dd>{sha}</dd>
  <dt>Archive</dt>    <dd>{archive}</dd>
</dl>
<h2>Gate result (re-validated now)</h2>
{gate_html}
{actions_html}"#,
        manifest_id = html_escape(&record.manifest_id),
        version = html_escape(&record.manifest_version),
        id = html_escape(&record.submission_id),
        status = html_escape(&record.status),
        status_label = html_escape(&record.status),
        when = fmt_absolute(record.submitted_at),
        sha = html_escape(&record.bundle_sha256),
        archive = html_escape(&record.project_archive),
        gate_html = gate_html,
        actions_html = actions_html,
        flash = flash_html,
    );
    Html(page(&format!("{} v{}", record.manifest_id, record.manifest_version), &body)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct FlashParam {
    pub flash: Option<String>,
}

pub async fn approve(
    State(state): State<AppState>,
    AxumPath(submission_id): AxumPath<String>,
) -> Response {
    if !is_safe_id(&submission_id) {
        return redirect_to_admin_with("invalid submission id");
    }
    let _guard = state.submissions_lock.lock().await;
    let record_path = submission_record_path(&state.workspace, &submission_id);
    let raw = match fs::read_to_string(&record_path) {
        Ok(s) => s,
        Err(_) => return redirect_to_admin_with("no submission with that id"),
    };
    let mut record_value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return redirect_to_admin_with("corrupt submission record"),
    };

    let archive_rel = record_value
        .get("project_archive")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let archive_path = state.workspace.join(&archive_rel);
    let bytes = match fs::read(&archive_path) {
        Ok(b) => b,
        Err(e) => return redirect_to_admin_with(&format!("can't read archive: {e}")),
    };

    let tmp = match TempDir::new() {
        Ok(d) => d,
        Err(e) => return redirect_to_admin_with(&format!("tempdir: {e}")),
    };
    if let Err(e) = crate::unpack_tar_zst(&bytes, tmp.path()) {
        return redirect_to_admin_with(&format!("unpack failed: {e}"));
    }
    let project_root = match locate_project_root(tmp.path()) {
        Ok(p) => p,
        Err(e) => return redirect_to_admin_with(&format!("locate project: {}", e.message)),
    };

    // Run the publish pipeline — bundles the project, signs with the dev
    // marketplace key, writes manifest.json + bundle.tar.zst into the
    // registry, and appends the index entry.
    let result = commonsc_devkit::publish::run(&project_root, None);
    match result {
        Ok(entry) => {
            // Update the submission record's status to approved.
            if let Some(obj) = record_value.as_object_mut() {
                obj.insert("status".to_string(), Value::String("approved".to_string()));
            }
            let _ = fs::write(
                &record_path,
                serde_json::to_string_pretty(&record_value).unwrap_or_default(),
            );
            // Record the decision sidecar.
            let decision = ReviewDecision {
                decided_at: now_millis(),
                reason: None,
            };
            let _ = fs::write(
                decision_path(&state.workspace, &submission_id),
                serde_json::to_string_pretty(&decision).unwrap_or_default(),
            );
            redirect_to_admin_with(&format!(
                "Approved {} v{} → promoted to catalog",
                entry.manifest.id, entry.manifest.version
            ))
        }
        Err(e) => redirect_to_admin_with(&format!("publish failed: {e}")),
    }
}

#[derive(Debug, Deserialize)]
pub struct RejectForm {
    pub reason: Option<String>,
}

pub async fn reject(
    State(state): State<AppState>,
    AxumPath(submission_id): AxumPath<String>,
    Form(form): Form<RejectForm>,
) -> Response {
    if !is_safe_id(&submission_id) {
        return redirect_to_admin_with("invalid submission id");
    }
    let _guard = state.submissions_lock.lock().await;
    let record_path = submission_record_path(&state.workspace, &submission_id);
    let raw = match fs::read_to_string(&record_path) {
        Ok(s) => s,
        Err(_) => return redirect_to_admin_with("no submission with that id"),
    };
    let mut record_value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return redirect_to_admin_with("corrupt submission record"),
    };
    if let Some(obj) = record_value.as_object_mut() {
        obj.insert("status".to_string(), Value::String("rejected".to_string()));
    }
    let _ = fs::write(
        &record_path,
        serde_json::to_string_pretty(&record_value).unwrap_or_default(),
    );
    let decision = ReviewDecision {
        decided_at: now_millis(),
        reason: form
            .reason
            .as_deref()
            .and_then(|r| if r.trim().is_empty() { None } else { Some(r.trim().to_string()) }),
    };
    let _ = fs::write(
        decision_path(&state.workspace, &submission_id),
        serde_json::to_string_pretty(&decision).unwrap_or_default(),
    );
    redirect_to_admin_with(&format!("Rejected {submission_id}"))
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn revalidate(workspace: &Path, archive_rel: &str) -> std::result::Result<Value, String> {
    let archive_path = workspace.join(archive_rel);
    let bytes = fs::read(&archive_path).map_err(|e| format!("read archive: {e}"))?;
    let tmp = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    crate::unpack_tar_zst(&bytes, tmp.path())?;
    let project_root = locate_project_root(tmp.path()).map_err(api_err_msg)?;
    let report = commonsc_devkit::validate::run(&project_root)
        .map_err(|e| format!("validate failed: {e}"))?;
    Ok(crate::report_to_json(&report))
}

fn api_err_msg(e: ApiError) -> String {
    format!("{:?}", e)
}

fn redirect_to_admin_with(flash: &str) -> Response {
    let encoded = urlencode(flash);
    Redirect::to(&format!("/admin/?flash={encoded}")).into_response()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn is_safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn fmt_absolute(millis: i64) -> String {
    // RFC 3339-ish using `time`'s default — but we don't have the `time`
    // crate as a dep. Fall back to seconds-since-epoch with a small offset
    // hint; rendered as an HTTP-style timestamp by the browser would need
    // JS. For v1, just print millis — the reviewer can hover or eyeball.
    format!("{millis}")
}

fn fmt_relative(millis: i64) -> String {
    let now = now_millis();
    let delta = now - millis;
    if delta < 0 {
        return "just now".to_string();
    }
    let s = delta / 1000;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}
