//! Manifest read/write helpers.
//!
//! Manifests are stored as JSON in two stages: the author writes
//! `manifest.template.json` (everything except the artifact-dependent fields);
//! publish completes it to a full `manifest.json` by adding `artifact`,
//! `checksum`, and `signatures`. Round-trip is through `serde_json::Value` so
//! we don't lose unknown fields the schema may add over time.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

pub fn read_template(project: &Path) -> Result<Value> {
    let path = project.join("manifest.template.json");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading manifest template at {}", path.display()))?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing manifest template at {}", path.display()))?;
    if !value.is_object() {
        return Err(anyhow!("manifest.template.json must be a JSON object"));
    }
    Ok(value)
}

pub fn id(manifest: &Value) -> Result<&str> {
    manifest
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing required string field `id`"))
}

pub fn version(manifest: &Value) -> Result<&str> {
    manifest
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing required string field `version`"))
}

pub fn name(manifest: &Value) -> Result<&str> {
    manifest
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing required string field `name`"))
}

pub fn publisher_handle(manifest: &Value) -> Result<&str> {
    manifest
        .get("publisher")
        .and_then(|p| p.get("handle"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing publisher.handle"))
}

pub fn publisher_key_id(manifest: &Value) -> Result<&str> {
    manifest
        .get("publisher")
        .and_then(|p| p.get("keyId"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest missing publisher.keyId"))
}

pub fn set_artifact(manifest: &mut Value, media_type: &str, size: u64, sha256: &str) {
    let obj = manifest.as_object_mut().expect("manifest must be object");
    let mut artifact = Map::new();
    artifact.insert("mediaType".to_string(), Value::String(media_type.to_string()));
    artifact.insert(
        "size".to_string(),
        Value::Number(serde_json::Number::from(size)),
    );
    artifact.insert("sha256".to_string(), Value::String(sha256.to_string()));
    obj.insert("artifact".to_string(), Value::Object(artifact));
}

pub fn set_checksum(manifest: &mut Value, hex: &str) {
    let obj = manifest.as_object_mut().expect("manifest must be object");
    obj.insert("checksum".to_string(), Value::String(hex.to_string()));
}

pub fn set_signatures(
    manifest: &mut Value,
    publisher_key_id: &str,
    publisher_sig_b64: &str,
    marketplace_key_id: &str,
    marketplace_sig_b64: &str,
) {
    let mk_sig = |key_id: &str, value: &str| {
        let mut m = Map::new();
        m.insert("alg".to_string(), Value::String("ed25519".to_string()));
        m.insert("keyId".to_string(), Value::String(key_id.to_string()));
        m.insert("value".to_string(), Value::String(value.to_string()));
        Value::Object(m)
    };
    let mut sigs = Map::new();
    sigs.insert(
        "publisher".to_string(),
        mk_sig(publisher_key_id, publisher_sig_b64),
    );
    sigs.insert(
        "marketplace".to_string(),
        mk_sig(marketplace_key_id, marketplace_sig_b64),
    );
    let obj = manifest.as_object_mut().expect("manifest must be object");
    obj.insert("signatures".to_string(), Value::Object(sigs));
}

/// Produce the canonical byte string for hashing or signing: the manifest with
/// `checksum` and `signatures` blanked out, encoded with sorted keys (see
/// `canonical`). Matches the customer app's verification path.
pub fn canonical_with_blanks(manifest: &Value) -> Vec<u8> {
    let mut clone = manifest.clone();
    if let Some(obj) = clone.as_object_mut() {
        obj.insert("checksum".to_string(), Value::String(String::new()));
        obj.insert("signatures".to_string(), Value::Object(Map::new()));
    }
    crate::canonical::bytes(&clone)
}
