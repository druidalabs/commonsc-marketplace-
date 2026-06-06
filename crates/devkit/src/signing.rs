//! Dev signing keys.
//!
//! For the local dev/demo flow we derive ed25519 keys deterministically from a
//! hardcoded seed per `keyId`. **These are not real production keys.** Anyone
//! reading this file can forge signatures from them — that's the point: it's
//! the cheapest way to exercise the verify-on-customer path end-to-end without
//! a key-management story.
//!
//! When the marketplace gets a real signing pipeline, this module gets replaced
//! by one that fetches keys from a vault / HSM. The verify side on the customer
//! app doesn't change — it always checks a pubkey listed in
//! `commonsc.io/catalog/keys.json`.

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// Generate a `[u8; 32]` seed from a stable label, so the same `keyId` always
/// resolves to the same keypair across machines and runs.
fn seed_for(label: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"commonsc-devkit/v0/dev-key/");
    hasher.update(label.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Derive a deterministic signing key from any keyId on demand. In dev/demo
/// mode every keyId is valid — the seed comes from the keyId string itself,
/// so contributor accounts get a consistent signing identity without us
/// having to maintain a static registration table. Production swaps this for
/// a KMS lookup with real key-management; the verify side on the customer is
/// unchanged.
fn key_for(key_id: &str) -> SigningKey {
    SigningKey::from_bytes(&seed_for(key_id))
}

pub fn sign(key_id: &str, bytes: &[u8]) -> Option<String> {
    let key = key_for(key_id);
    let sig = key.sign(bytes);
    Some(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()))
}

#[allow(dead_code)]
pub fn public_key(key_id: &str) -> Option<VerifyingKey> {
    Some(key_for(key_id).verifying_key())
}
