//! Ed25519 signing + verification.
//!
//! Two modes:
//!
//! - **Real** (`sign_with_secret` / `verify`) — signs with the publisher's
//!   registered private key (the one `commonsc-devkit register` generated and
//!   stored in `~/.commonsc/credentials.json`) and verifies against a stored
//!   public key. This is the production path: the publisher signs client-side,
//!   the marketplace verifies, and the customer app re-verifies before any bytes
//!   run.
//!
//! - **Dev** (`sign_dev` / `public_key_dev`) — derives a keypair
//!   deterministically from a `keyId` string. **Forgeable by anyone** — it
//!   exists only so the local-registry publish path and offline tests work
//!   without a registration round-trip. The server never accepts a dev-signed
//!   submission.
//!
//! The bytes being signed are always the manifest's canonical encoding with
//! `checksum` and `signatures` blanked (see `manifest::canonical_with_blanks`),
//! so all three verifiers — devkit, marketplace, app — check the same thing.

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// Decode a base64 string into a fixed-size array, or `None` on any mismatch.
fn decode_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    let v = B64.decode(s.trim()).ok()?;
    v.as_slice().try_into().ok()
}

/// Sign `bytes` with a real ed25519 private key — the base64 of the 32-byte
/// seed as stored in `credentials.json`. Returns the base64 signature.
pub fn sign_with_secret(private_key_b64: &str, bytes: &[u8]) -> Result<String> {
    let seed: [u8; 32] = decode_fixed(private_key_b64)
        .ok_or_else(|| anyhow!("private key must be base64 of exactly 32 bytes"))?;
    let key = SigningKey::from_bytes(&seed);
    Ok(B64.encode(key.sign(bytes).to_bytes()))
}

/// Verify a base64 ed25519 signature over `bytes` against a base64 32-byte
/// public key. Returns false on any decode/length/verify failure — never panics.
pub fn verify(public_key_b64: &str, bytes: &[u8], sig_b64: &str) -> bool {
    let Some(pk) = decode_fixed::<32>(public_key_b64) else {
        return false;
    };
    let Some(sig_bytes) = decode_fixed::<64>(sig_b64) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    vk.verify(bytes, &Signature::from_bytes(&sig_bytes)).is_ok()
}

// ── Dev (deterministic, forgeable) ──────────────────────────────────────────

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

fn key_for(key_id: &str) -> SigningKey {
    SigningKey::from_bytes(&seed_for(key_id))
}

/// Sign with the forgeable dev key derived from `key_id`. Local-registry /
/// offline use only — the live marketplace rejects dev-signed submissions.
pub fn sign_dev(key_id: &str, bytes: &[u8]) -> String {
    B64.encode(key_for(key_id).sign(bytes).to_bytes())
}

/// The dev public key for a `key_id`, base64-encoded — lets the local-registry
/// path and tests verify dev signatures the same way the real ones are checked.
pub fn public_key_dev(key_id: &str) -> String {
    B64.encode(key_for(key_id).verifying_key().to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn real_sign_verify_round_trips() {
        let sk = SigningKey::generate(&mut OsRng);
        let priv_b64 = B64.encode(sk.to_bytes());
        let pub_b64 = B64.encode(sk.verifying_key().to_bytes());
        let msg = b"canonical manifest bytes";

        let sig = sign_with_secret(&priv_b64, msg).expect("sign");
        assert!(verify(&pub_b64, msg, &sig), "valid signature must verify");
        assert!(!verify(&pub_b64, b"tampered", &sig), "wrong message must fail");

        // A different key's signature must not verify against this pubkey.
        let other = SigningKey::generate(&mut OsRng);
        let other_sig = sign_with_secret(&B64.encode(other.to_bytes()), msg).unwrap();
        assert!(!verify(&pub_b64, msg, &other_sig), "wrong key must fail");
    }

    #[test]
    fn dev_sign_verifies_with_dev_pubkey() {
        let msg = b"hello";
        let sig = sign_dev("alice-2026-01", msg);
        assert!(verify(&public_key_dev("alice-2026-01"), msg, &sig));
        assert!(!verify(&public_key_dev("bob-2026-01"), msg, &sig));
    }

    #[test]
    fn verify_rejects_garbage() {
        assert!(!verify("not-base64!!", b"x", "also-bad"));
        assert!(!verify("", b"x", ""));
    }
}
