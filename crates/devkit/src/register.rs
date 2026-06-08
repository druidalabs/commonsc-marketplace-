//! `commonsc-devkit register` — register a publisher account.
//!
//! Generates an ed25519 keypair locally, POSTs the *public* key to the
//! marketplace's `POST /publisher/register` endpoint, and writes the
//! credentials (private key, keyId, contact details) to a 0600 file at
//! `~/.commonsc/credentials.json`. The private key never leaves the machine.
//!
//! Later, when publishing through the live API, the dev-kit signs each
//! upload with this private key; the marketplace verifies against the public
//! key it stored on registration. For v0 the publish path still uses the
//! deterministic dev keys — this command lays the foundation for production
//! signing without changing the existing publish flow today.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

pub struct RegisterOptions {
    pub name: String,
    pub contact: String,
    pub handle: Option<String>,
    pub api: String,
    pub config: Option<PathBuf>,
    pub force: bool,
}

pub struct RegisterSummary {
    pub handle: String,
    pub key_id: String,
    pub config_path: PathBuf,
}

impl RegisterSummary {
    pub fn print(&self) {
        println!("registered publisher `{}`", self.handle);
        println!("  keyId:  {}", self.key_id);
        println!("  saved:  {}", self.config_path.display());
        println!();
        println!("Your private key is in the saved file at mode 0600. Don't share it.");
        println!("Future `commonsc-devkit publish` calls against the live API will sign with it.");
    }
}

#[derive(Serialize)]
struct RegisterRequest<'a> {
    name: &'a str,
    contact: &'a str,
    pubkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<&'a str>,
}

#[derive(Deserialize)]
struct RegisterResponse {
    handle: String,
    #[serde(rename = "keyId")]
    key_id: String,
}

/// On-disk credentials. Stored at `~/.commonsc/credentials.json` (or the
/// path supplied via `--config`). chmod 600 — your private key lives here.
#[derive(Serialize, Deserialize)]
pub struct StoredCredentials {
    pub handle: String,
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub name: String,
    pub contact: String,
    #[serde(rename = "privateKey")]
    pub private_key: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub api: String,
    #[serde(rename = "registeredAt")]
    pub registered_at: i64,
}

pub fn run(opts: RegisterOptions) -> Result<RegisterSummary> {
    let config_path = opts
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(default_config_path)?;

    if config_path.exists() && !opts.force {
        return Err(anyhow!(
            "credentials already exist at {} — pass --force to overwrite",
            config_path.display()
        ));
    }

    // Generate the keypair locally. The seed comes from the OS RNG; this is
    // not deterministic like the dev signing keys.
    let signing = SigningKey::generate(&mut OsRng);
    let public = signing.verifying_key();
    let private_b64 =
        base64::engine::general_purpose::STANDARD.encode(signing.to_bytes());
    let public_b64 =
        base64::engine::general_purpose::STANDARD.encode(public.to_bytes());

    let api = opts.api.trim_end_matches('/').to_string();
    let url = format!("{api}/publisher/register");

    let req = RegisterRequest {
        name: &opts.name,
        contact: &opts.contact,
        pubkey: public_b64.clone(),
        handle: opts.handle.as_deref(),
    };

    let response = ureq::post(&url)
        .set("user-agent", concat!("commonsc-devkit/", env!("CARGO_PKG_VERSION")))
        .set("accept", "application/json")
        .send_json(serde_json::to_value(&req)?);

    let body: RegisterResponse = match response {
        Ok(resp) => resp
            .into_json()
            .context("decoding register response (server returned non-JSON?)")?,
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp
                .into_string()
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("server returned HTTP {code}: {body}"));
        }
        Err(e) => {
            return Err(anyhow!("network call to {url} failed: {e}"));
        }
    };

    let creds = StoredCredentials {
        handle: body.handle.clone(),
        key_id: body.key_id.clone(),
        name: opts.name,
        contact: opts.contact,
        private_key: private_b64,
        public_key: public_b64,
        api: api.clone(),
        registered_at: now_millis(),
    };

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body_str = serde_json::to_string_pretty(&creds)? + "\n";
    fs::write(&config_path, body_str)
        .with_context(|| format!("writing {}", config_path.display()))?;
    // chmod 0600 — the private key sits in here.
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", config_path.display()))?;

    Ok(RegisterSummary {
        handle: body.handle,
        key_id: body.key_id,
        config_path,
    })
}

/// Read credentials back from disk. Returns `Ok(None)` if no file exists at
/// the given path (or the default if none supplied). Callers that need
/// credentials and don't have them should print a helpful error pointing at
/// `commonsc-devkit register`.
#[allow(dead_code)]
pub fn load(config: Option<&Path>) -> Result<Option<StoredCredentials>> {
    let path = match config {
        Some(p) => p.to_path_buf(),
        None => default_config_path()?,
    };
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let creds: StoredCredentials = serde_json::from_str(&raw)
        .with_context(|| format!("parsing credentials at {}", path.display()))?;
    Ok(Some(creds))
}

fn default_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow!("can't resolve $HOME — pass --config to set an explicit path")
    })?;
    Ok(home.join(".commonsc").join("credentials.json"))
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
