//! Publish — bundle + sign + write to local registry.
//!
//! Output layout (mirrors what a real HTTPS registry would serve, just on
//! disk):
//!
//! ```text
//! commonsc/registry/
//! ├── index.json                            ← list of all published entries
//! └── bundles/
//!     └── <publisher>/<algo>/<version>/
//!         ├── manifest.json                 ← signed, content-addressed
//!         └── bundle.tar.zst                ← the artifact
//! ```
//!
//! Files inside the bundle: everything under `<project>/` except the
//! `manifest.template.json` (which is the *input* to publish), the `fixtures/`
//! directory (test data, not shipped), and the `README.md`. The bundle is the
//! runnable artifact — Python module + supporting files.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use serde_json::{json, Map, Value};

const ARTIFACT_MEDIA_TYPE: &str = "application/vnd.commonsc.pyodide-bundle.tar+zstd";
const MARKETPLACE_KEY_ID: &str = "commonsc-marketplace-2026-01";

pub struct Entry {
    pub manifest: ManifestSummary,
    pub registry_dir: PathBuf,
}

pub struct ManifestSummary {
    pub id: String,
    pub version: String,
}

fn safe_path_component(s: &str, label: &str) -> Result<()> {
    if s.is_empty()
        || s.starts_with('.')
        || s.contains('/')
        || s.contains('\\')
        || s.contains('\0')
    {
        return Err(anyhow!("{label} {s:?} is not a safe path component"));
    }
    Ok(())
}

/// Everything needed to sign + write a manifest, derived once so the local
/// publish, the client-side remote signature, and the server-side co-sign all
/// agree on the exact canonical bytes. `manifest` has the artifact set but not
/// the checksum or signatures; `canonical` is what every signature is over.
pub struct Prepared {
    pub manifest: Value,
    pub canonical: Vec<u8>,
    pub bundle: Vec<u8>,
    pub id: String,
    pub version: String,
    pub publisher_handle: String,
    pub publisher_key_id: String,
}

/// Build the runtime bundle and complete the manifest's artifact fields. Shared
/// by every path that needs the canonical bytes. Deterministic: the same
/// project yields the same bundle hash and canonical bytes on any machine, which
/// is what lets the publisher sign client-side and the server verify.
pub fn prepare(project: &Path) -> Result<Prepared> {
    let manifest_template = crate::manifest::read_template(project)?;
    let id = crate::manifest::id(&manifest_template)?.to_string();
    let version = crate::manifest::version(&manifest_template)?.to_string();
    let publisher_handle = crate::manifest::publisher_handle(&manifest_template)?.to_string();
    let publisher_key_id = crate::manifest::publisher_key_id(&manifest_template)?.to_string();
    safe_path_component(&publisher_handle, "publisher_handle")?;
    safe_path_component(id.splitn(2, '/').nth(1).unwrap_or_default(), "algo_handle")?;

    let bundle = build_artifact(project)?;
    let sha = hex::encode(Sha256::digest(&bundle));
    let mut manifest = manifest_template;
    crate::manifest::set_artifact(&mut manifest, ARTIFACT_MEDIA_TYPE, bundle.len() as u64, &sha);
    let canonical = crate::manifest::canonical_with_blanks(&manifest);

    Ok(Prepared { manifest, canonical, bundle, id, version, publisher_handle, publisher_key_id })
}

/// Stamp the checksum + both signatures onto a prepared manifest and write the
/// bundle, manifest, and index entry into the registry.
fn finalize_and_write(
    mut prep: Prepared,
    registry_root: &Path,
    publisher_sig: &str,
    marketplace_key_id: &str,
    marketplace_sig: &str,
) -> Result<Entry> {
    let algo_handle = prep
        .id
        .splitn(2, '/')
        .nth(1)
        .ok_or_else(|| anyhow!("manifest.id {} is not in publisher/algo form", prep.id))?
        .to_string();
    let bundle_dir = registry_root
        .join("bundles")
        .join(&prep.publisher_handle)
        .join(&algo_handle)
        .join(&prep.version);
    fs::create_dir_all(&bundle_dir)
        .with_context(|| format!("creating bundle dir at {}", bundle_dir.display()))?;

    fs::write(bundle_dir.join("bundle.tar.zst"), &prep.bundle)
        .with_context(|| format!("writing artifact in {}", bundle_dir.display()))?;

    let checksum = hex::encode(Sha256::digest(&prep.canonical));
    crate::manifest::set_checksum(&mut prep.manifest, &checksum);
    crate::manifest::set_signatures(
        &mut prep.manifest,
        &prep.publisher_key_id,
        publisher_sig,
        marketplace_key_id,
        marketplace_sig,
    );

    let manifest_pretty = serde_json::to_string_pretty(&prep.manifest)? + "\n";
    fs::write(bundle_dir.join("manifest.json"), &manifest_pretty)
        .with_context(|| format!("writing manifest in {}", bundle_dir.display()))?;

    update_index(registry_root, &prep.manifest, &bundle_dir)?;
    Ok(Entry {
        manifest: ManifestSummary { id: prep.id, version: prep.version },
        registry_dir: bundle_dir,
    })
}

fn resolve_registry(project: &Path, registry_override: Option<&Path>) -> Result<PathBuf> {
    let root = registry_override
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| default_registry_root(project))?;
    fs::create_dir_all(&root)
        .with_context(|| format!("creating registry root at {}", root.display()))?;
    Ok(root)
}

/// Local publish to a registry directory. Signs the publisher slot with the
/// registered key when local credentials match the manifest's keyId, else with
/// the forgeable dev key (offline authoring). The marketplace co-sign uses the
/// real key from `COMMONSC_MARKETPLACE_PRIVATE_KEY` when set (so we can publish
/// the embedded registry for real), else the dev key.
pub fn run(project: &Path, registry_override: Option<&Path>) -> Result<Entry> {
    let report = crate::validate::run(project)?;
    if report.outcome.is_fail() {
        report.print();
        return Err(anyhow!("validate failed; publish aborted"));
    }
    let registry_root = resolve_registry(project, registry_override)?;
    let prep = prepare(project)?;

    let publisher_sig = match crate::register::load(None)? {
        Some(creds) if creds.key_id == prep.publisher_key_id => {
            crate::signing::sign_with_secret(&creds.private_key, &prep.canonical)?
        }
        _ => crate::signing::sign_dev(&prep.publisher_key_id, &prep.canonical),
    };
    let marketplace_sig = marketplace_cosign(&prep.canonical)?;
    finalize_and_write(prep, &registry_root, &publisher_sig, MARKETPLACE_KEY_ID, &marketplace_sig)
}

/// Compute the marketplace co-signature over `canonical`. Uses the real
/// server-held key from `COMMONSC_MARKETPLACE_PRIVATE_KEY` (base64 of the
/// 32-byte seed) when present; otherwise the forgeable dev key, so local and
/// CI flows keep working without the production secret.
pub fn marketplace_cosign(canonical: &[u8]) -> Result<String> {
    match std::env::var("COMMONSC_MARKETPLACE_PRIVATE_KEY") {
        Ok(secret) if !secret.trim().is_empty() => {
            crate::signing::sign_with_secret(secret.trim(), canonical)
        }
        _ => Ok(crate::signing::sign_dev(MARKETPLACE_KEY_ID, canonical)),
    }
}

/// Client-side: sign the manifest with the publisher's registered private key,
/// for upload to the live marketplace. Requires credentials and that they match
/// the manifest's keyId.
pub struct RemoteSignature {
    pub key_id: String,
    pub handle: String,
    pub signature_b64: String,
}

pub fn sign_for_remote(project: &Path) -> Result<RemoteSignature> {
    let prep = prepare(project)?;
    let creds = crate::register::load(None)?
        .ok_or_else(|| anyhow!("not registered — run `commonsc-devkit register` first"))?;
    if creds.key_id != prep.publisher_key_id {
        return Err(anyhow!(
            "manifest publisher.keyId ({}) doesn't match your credentials ({}). \
             Re-scaffold with your handle or fix manifest.publisher.",
            prep.publisher_key_id,
            creds.key_id
        ));
    }
    let signature_b64 = crate::signing::sign_with_secret(&creds.private_key, &prep.canonical)?;
    Ok(RemoteSignature { key_id: creds.key_id, handle: creds.handle, signature_b64 })
}

/// Server-side (publish): verify a client-provided publisher signature over the
/// project's canonical manifest against the publisher's registered public key.
/// This is the auth gate — a forged or dev-signed manifest fails here.
pub fn verify_remote_signature(
    project: &Path,
    publisher_key_id: &str,
    public_key_b64: &str,
    sig_b64: &str,
) -> Result<()> {
    let prep = prepare(project)?;
    if prep.publisher_key_id != publisher_key_id {
        return Err(anyhow!(
            "manifest publisher.keyId ({}) doesn't match the asserted keyId ({})",
            prep.publisher_key_id,
            publisher_key_id
        ));
    }
    if !crate::signing::verify(public_key_b64, &prep.canonical, sig_b64) {
        return Err(anyhow!(
            "publisher signature does not verify against the registered key for {publisher_key_id}"
        ));
    }
    Ok(())
}

/// Server-side (approval): write the bundle to the registry using a publisher
/// signature that has *already been verified* (never re-signed here — the
/// server has no publisher private key) and a fresh marketplace co-sign.
pub fn publish_with_signoff(
    project: &Path,
    registry_override: Option<&Path>,
    publisher_key_id: &str,
    publisher_sig_b64: &str,
) -> Result<Entry> {
    let registry_root = resolve_registry(project, registry_override)?;
    let prep = prepare(project)?;
    if prep.publisher_key_id != publisher_key_id {
        return Err(anyhow!(
            "manifest publisher.keyId ({}) doesn't match the verified submission keyId ({})",
            prep.publisher_key_id,
            publisher_key_id
        ));
    }
    let marketplace_sig = marketplace_cosign(&prep.canonical)?;
    finalize_and_write(prep, &registry_root, publisher_sig_b64, MARKETPLACE_KEY_ID, &marketplace_sig)
}

// ── Remote publish (POST to a live marketplace) ─────────────────────────
//
// Bundles the project (same tar.zst content as the local publish), then
// streams the bytes to `<api>/algorithms/publish` as a raw `application/zstd`
// body. The server side validates the bundle, runs the canonical gates, and
// queues the submission for human review. Approval (in the reviewer admin
// UI) is what promotes it to the public catalog — same human gate as the
// local-publish path, just one network hop further upstream.

pub struct RemoteSubmission {
    pub submission_id: String,
    pub status: String,
    pub manifest_id: String,
    pub manifest_version: String,
}

impl RemoteSubmission {
    pub fn print(&self, api: &str) {
        println!("submitted {}@{} → {} ({})", self.manifest_id, self.manifest_version, self.submission_id, self.status);
        println!("  status:  {}/algorithms/{}/status", api, self.submission_id);
        println!("  review:  marketplace queue (human gate); the bundle becomes installable once approved.");
    }
}

pub fn run_remote(project: &Path, api: &str) -> Result<RemoteSubmission> {
    // Same validate-first guarantee as local publish — refuse to upload a
    // bundle that won't pass our own gates. Cheap; saves the server cycles
    // and the publisher embarrassment.
    let report = crate::validate::run(project)?;
    if report.outcome.is_fail() {
        report.print();
        return Err(anyhow!("validate failed; remote publish aborted"));
    }

    let manifest = crate::manifest::read_template(project)?;
    let manifest_id = crate::manifest::id(&manifest)?.to_string();
    let manifest_version = crate::manifest::version(&manifest)?.to_string();

    // Remote publish wants the *project*, not the runtime bundle — the
    // server-side validate needs the manifest template and the fixture to
    // exercise the gates. The reviewer's eventual approve step builds the
    // slimmer runtime bundle from this archive (same `build_artifact` used
    // by local publish).
    // Sign the manifest with the registered private key *before* upload — the
    // server verifies this against the publisher's stored pubkey and rejects
    // anything it can't (forged, dev-signed, or wrong key). This is the auth
    // gate; the publisher private key never leaves the machine.
    let signed = sign_for_remote(project)?;

    let archive = build_project_archive(project)?;
    let url = format!("{}/algorithms/publish", api.trim_end_matches('/'));

    println!(
        "uploading {} bytes to {} (signed as {}) …",
        archive.len(),
        url,
        signed.key_id,
    );

    let response = ureq::post(&url)
        .set("content-type", "application/zstd")
        .set("accept", "application/json")
        .set(
            "user-agent",
            concat!("commonsc-devkit/", env!("CARGO_PKG_VERSION")),
        )
        .set("x-commonsc-key-id", &signed.key_id)
        .set("x-commonsc-publisher-sig", &signed.signature_b64)
        .send_bytes(&archive);

    let body: Value = match response {
        Ok(resp) => resp.into_json().context("decoding publish response")?,
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp
                .into_string()
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("server returned HTTP {code}: {text}"));
        }
        Err(e) => return Err(anyhow!("network call to {url} failed: {e}")),
    };

    let status = body
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    if status == "validation-failed" {
        // The server-side gates rejected it (shouldn't happen — local
        // validate passed — but the gate code may have evolved server-side).
        // Surface the structured remediation.
        let gate = body
            .get("gateResult")
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_default();
        return Err(anyhow!(
            "server validation failed:\n{gate}\n(local validate passed — the server may have stricter gates; report the mismatch.)"
        ));
    }

    let submission_id = body
        .get("submissionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("server response missing `submissionId`: {body}"))?
        .to_string();

    Ok(RemoteSubmission {
        submission_id,
        status,
        manifest_id,
        manifest_version,
    })
}

/// Tar.zst the *whole* project (manifest template + fixtures + README + code)
/// for upload to a marketplace's validate/publish endpoint. Only OS cruft and
/// build caches are excluded. Different from `build_artifact`, which strips
/// dev-only files to produce the runtime bundle that lives in the registry.
fn build_project_archive(project: &Path) -> Result<Vec<u8>> {
    let raw_tar = build_tar_with_filter(project, |name| {
        matches!(name, ".DS_Store" | "__pycache__" | ".git" | "target" | "node_modules")
    })?;
    let mut compressed = Vec::with_capacity(raw_tar.len() / 4);
    zstd::stream::copy_encode(&raw_tar[..], &mut compressed, 19)
        .context("zstd encode failed")?;
    Ok(compressed)
}

fn build_artifact(project: &Path) -> Result<Vec<u8>> {
    let raw_tar = build_tar(project)?;
    let mut compressed = Vec::with_capacity(raw_tar.len() / 4);
    zstd::stream::copy_encode(&raw_tar[..], &mut compressed, 19)
        .context("zstd encode failed")?;
    Ok(compressed)
}

/// The runtime bundle bytes (tar.zst, dev-only files stripped) — exactly what
/// ships to a consumer. Exposed so `devkit run` executes the *same* artifact a
/// user would install, not the project tree.
pub fn build_runtime_bundle(project: &Path) -> Result<Vec<u8>> {
    build_artifact(project)
}

fn build_tar(project: &Path) -> Result<Vec<u8>> {
    // Runtime-bundle exclude list — `is_excluded` strips dev-only files.
    build_tar_with_filter(project, is_excluded)
}

fn build_tar_with_filter<F: Fn(&str) -> bool>(project: &Path, exclude: F) -> Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::<u8>::new());
    // Walk the project, deterministic order so the bundle hash is reproducible
    // across machines and runs.
    let mut entries = collect_entries_with_filter(project, &exclude)?;
    entries.sort();
    for relative in &entries {
        let abs = project.join(relative);
        let metadata = abs.metadata()?;
        if metadata.is_dir() {
            // Tar libs auto-create parent dirs for files, so we skip recording
            // empty directories — keeps the bundle minimal and order-stable.
            continue;
        }
        let mut file = fs::File::open(&abs)
            .with_context(|| format!("opening {} for bundling", abs.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(metadata.len());
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, relative, &mut file)
            .with_context(|| format!("tarring {}", abs.display()))?;
    }
    tar.into_inner().context("finalizing tar")
}

#[allow(dead_code)]
fn collect_entries(project: &Path) -> Result<Vec<PathBuf>> {
    collect_entries_with_filter(project, &is_excluded)
}

fn collect_entries_with_filter<F: Fn(&str) -> bool>(
    project: &Path,
    exclude: &F,
) -> Result<Vec<PathBuf>> {
    fn walk<F: Fn(&str) -> bool>(
        root: &Path,
        dir: &Path,
        exclude: &F,
        out: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if exclude(&name) {
                continue;
            }
            let abs = entry.path();
            let rel = abs.strip_prefix(root)?.to_path_buf();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(root, &abs, exclude, out)?;
            } else if ft.is_file() {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(project, project, exclude, &mut out)?;
    Ok(out)
}

fn is_excluded(name: &str) -> bool {
    matches!(
        name,
        "manifest.template.json"
            | "fixtures"
            | "README.md"
            | ".DS_Store"
            | "__pycache__"
            | ".git"
    )
}

fn update_index(registry_root: &Path, manifest: &Value, bundle_dir: &Path) -> Result<()> {
    let index_path = registry_root.join("index.json");
    let mut index: Value = if index_path.exists() {
        let raw = fs::read_to_string(&index_path)?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parsing existing index at {}", index_path.display()))?
    } else {
        json!({
            "schemaVersion": "1",
            "generatedAt": null,
            "entries": []
        })
    };

    let id = manifest.get("id").and_then(Value::as_str).unwrap_or_default();
    let version = manifest.get("version").and_then(Value::as_str).unwrap_or_default();
    let relative_bundle = bundle_dir
        .strip_prefix(registry_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| bundle_dir.to_path_buf());
    let manifest_url = format!("./{}/manifest.json", relative_bundle.display());
    let bundle_url = format!("./{}/bundle.tar.zst", relative_bundle.display());

    let new_entry = {
        let mut m = Map::new();
        m.insert("id".to_string(), Value::String(id.to_string()));
        m.insert("version".to_string(), Value::String(version.to_string()));
        m.insert("manifestUrl".to_string(), Value::String(manifest_url));
        m.insert("bundleUrl".to_string(), Value::String(bundle_url));
        m.insert(
            "publishedAt".to_string(),
            Value::Number(serde_json::Number::from(now_millis())),
        );
        Value::Object(m)
    };

    if let Some(entries) = index.get_mut("entries").and_then(Value::as_array_mut) {
        // Replace any existing entry with the same id+version.
        entries.retain(|e| {
            e.get("id").and_then(Value::as_str) != Some(id)
                || e.get("version").and_then(Value::as_str) != Some(version)
        });
        entries.push(new_entry);
        // Keep deterministic order — sort by id, then version.
        entries.sort_by(|a, b| {
            let id_a = a.get("id").and_then(Value::as_str).unwrap_or("");
            let id_b = b.get("id").and_then(Value::as_str).unwrap_or("");
            id_a.cmp(id_b).then_with(|| {
                let v_a = a.get("version").and_then(Value::as_str).unwrap_or("");
                let v_b = b.get("version").and_then(Value::as_str).unwrap_or("");
                v_a.cmp(v_b)
            })
        });
    }
    if let Some(obj) = index.as_object_mut() {
        obj.insert(
            "generatedAt".to_string(),
            Value::Number(serde_json::Number::from(now_millis())),
        );
    }

    let pretty = serde_json::to_string_pretty(&index)? + "\n";
    let mut file = fs::File::create(&index_path)?;
    file.write_all(pretty.as_bytes())?;
    Ok(())
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn example_project() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../algorithms/eye-colour")
    }

    #[test]
    fn prepare_is_deterministic() {
        let p = example_project();
        let a = prepare(&p).expect("prepare a");
        let b = prepare(&p).expect("prepare b");
        assert_eq!(a.canonical, b.canonical, "canonical bytes must be reproducible");
        assert_eq!(a.bundle.len(), b.bundle.len(), "bundle must be reproducible");
    }

    #[test]
    fn publisher_signature_round_trips_and_rejects_forgeries() {
        let p = example_project();
        let prep = prepare(&p).expect("prepare");
        let sk = SigningKey::generate(&mut OsRng);
        let priv_b64 = B64.encode(sk.to_bytes());
        let pub_b64 = B64.encode(sk.verifying_key().to_bytes());

        // Client signs; server verifies via the exact call it makes.
        let sig = crate::signing::sign_with_secret(&priv_b64, &prep.canonical).unwrap();
        assert!(
            verify_remote_signature(&p, &prep.publisher_key_id, &pub_b64, &sig).is_ok(),
            "a real signature must verify against the registered key"
        );

        // A different key must be rejected.
        let other = SigningKey::generate(&mut OsRng);
        let other_pub = B64.encode(other.verifying_key().to_bytes());
        assert!(
            verify_remote_signature(&p, &prep.publisher_key_id, &other_pub, &sig).is_err(),
            "wrong key must be rejected"
        );

        // A forgeable dev signature must be rejected — the whole point of B.
        let dev_sig = crate::signing::sign_dev(&prep.publisher_key_id, &prep.canonical);
        assert!(
            verify_remote_signature(&p, &prep.publisher_key_id, &pub_b64, &dev_sig).is_err(),
            "dev-signed manifest must be rejected by the real-key verifier"
        );
    }
}

fn default_registry_root(project: &Path) -> Result<PathBuf> {
    let abs = fs::canonicalize(project)
        .with_context(|| format!("canonicalizing {}", project.display()))?;
    let mut cursor: &Path = abs.as_path();
    loop {
        if cursor.join("commonsc/registry").exists()
            || (cursor.file_name().and_then(|n| n.to_str()) == Some("commonsc")
                && cursor.join("registry").exists())
        {
            // Found existing registry — use it.
            if cursor.join("commonsc/registry").exists() {
                return Ok(cursor.join("commonsc/registry"));
            }
            return Ok(cursor.join("registry"));
        }
        if cursor.join("Cargo.toml").exists() && cursor.file_name().and_then(|n| n.to_str()) == Some("commonsc") {
            // Cargo workspace root for commonsc — co-locate registry here.
            return Ok(cursor.join("registry"));
        }
        match cursor.parent() {
            Some(p) => cursor = p,
            None => {
                return Err(anyhow!(
                    "couldn't locate the commonsc workspace root from {}; pass --registry",
                    project.display()
                ))
            }
        }
    }
}
