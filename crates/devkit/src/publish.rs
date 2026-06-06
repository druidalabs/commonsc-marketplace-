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

pub fn run(project: &Path, registry_override: Option<&Path>) -> Result<Entry> {
    // 1. Validate first — refuse to publish a bundle whose template doesn't pass.
    let report = crate::validate::run(project)?;
    if report.outcome.is_fail() {
        report.print();
        return Err(anyhow!("validate failed; publish aborted"));
    }

    // 2. Resolve registry path. Default is `<repo>/commonsc/registry/` where
    //    `<repo>` is the parent of the workspace root, discovered by walking up
    //    from the project until we find a Cargo workspace.
    let registry_root = registry_override
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| default_registry_root(project))?;
    fs::create_dir_all(&registry_root)
        .with_context(|| format!("creating registry root at {}", registry_root.display()))?;

    let manifest_template = crate::manifest::read_template(project)?;
    let id = crate::manifest::id(&manifest_template)?.to_string();
    let version = crate::manifest::version(&manifest_template)?.to_string();
    let publisher_handle = crate::manifest::publisher_handle(&manifest_template)?.to_string();
    let publisher_key_id = crate::manifest::publisher_key_id(&manifest_template)?.to_string();
    let algo_handle = id
        .splitn(2, '/')
        .nth(1)
        .ok_or_else(|| anyhow!("manifest.id {id} is not in publisher/algo form"))?
        .to_string();

    let bundle_dir = registry_root
        .join("bundles")
        .join(&publisher_handle)
        .join(&algo_handle)
        .join(&version);
    fs::create_dir_all(&bundle_dir)
        .with_context(|| format!("creating bundle dir at {}", bundle_dir.display()))?;

    // 3. Build the artifact. tar the project (minus excluded entries), pipe
    //    through zstd, write to disk.
    let artifact_path = bundle_dir.join("bundle.tar.zst");
    let artifact_bytes = build_artifact(project)?;
    fs::write(&artifact_path, &artifact_bytes)
        .with_context(|| format!("writing artifact at {}", artifact_path.display()))?;
    let artifact_sha = hex::encode(Sha256::digest(&artifact_bytes));

    // 4. Complete the manifest: artifact, checksum, signatures.
    let mut manifest = manifest_template;
    crate::manifest::set_artifact(
        &mut manifest,
        ARTIFACT_MEDIA_TYPE,
        artifact_bytes.len() as u64,
        &artifact_sha,
    );

    let canonical = crate::manifest::canonical_with_blanks(&manifest);
    let checksum = hex::encode(Sha256::digest(&canonical));
    crate::manifest::set_checksum(&mut manifest, &checksum);

    let publisher_sig = crate::signing::sign(&publisher_key_id, &canonical)
        .ok_or_else(|| anyhow!("no dev signing key for keyId {publisher_key_id}"))?;
    let marketplace_sig = crate::signing::sign(MARKETPLACE_KEY_ID, &canonical)
        .ok_or_else(|| anyhow!("no dev signing key for {MARKETPLACE_KEY_ID}"))?;
    crate::manifest::set_signatures(
        &mut manifest,
        &publisher_key_id,
        &publisher_sig,
        MARKETPLACE_KEY_ID,
        &marketplace_sig,
    );

    // 5. Write the final manifest.
    let manifest_path = bundle_dir.join("manifest.json");
    let manifest_pretty = serde_json::to_string_pretty(&manifest)? + "\n";
    fs::write(&manifest_path, &manifest_pretty)
        .with_context(|| format!("writing manifest at {}", manifest_path.display()))?;

    // 6. Update (or create) the registry index.
    update_index(&registry_root, &manifest, &bundle_dir)?;

    Ok(Entry {
        manifest: ManifestSummary { id, version },
        registry_dir: bundle_dir,
    })
}

fn build_artifact(project: &Path) -> Result<Vec<u8>> {
    let raw_tar = build_tar(project)?;
    let mut compressed = Vec::with_capacity(raw_tar.len() / 4);
    zstd::stream::copy_encode(&raw_tar[..], &mut compressed, 19)
        .context("zstd encode failed")?;
    Ok(compressed)
}

fn build_tar(project: &Path) -> Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::<u8>::new());
    // Walk the project, deterministic order so the bundle hash is reproducible
    // across machines and runs.
    let mut entries = collect_entries(project)?;
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

fn collect_entries(project: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_excluded(&name) {
                continue;
            }
            let abs = entry.path();
            let rel = abs.strip_prefix(root)?.to_path_buf();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(root, &abs, out)?;
            } else if ft.is_file() {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(project, project, &mut out)?;
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
