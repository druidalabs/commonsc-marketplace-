use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use commonsc_devkit::{init, publish, register, run, validate};

// Module implementations live under `src/lib.rs` so the marketplace HTTP
// service can call the same validate/publish gate code without duplicating it.

/// Resolve the API base URL for remote publish — explicit `--api` wins, then
/// the saved credentials file's `api` field, then the production default.
/// Surfaces a clear error if the credentials file is required but missing.
fn resolve_remote_api(api: Option<&str>, config: Option<&std::path::Path>) -> Result<String> {
    if let Some(a) = api {
        return Ok(a.trim_end_matches('/').to_string());
    }
    match register::load(config)? {
        Some(creds) => Ok(creds.api.trim_end_matches('/').to_string()),
        None => {
            // No credentials and no explicit --api. Default to production
            // and let the user opt into a different target via the flag if
            // they want.
            Ok("https://api.commonsc.io".to_string())
        }
    }
}

#[derive(Parser)]
#[command(name = "commonsc-devkit", version, about = "CommonSense contributor toolkit")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a new algorithm project. Creates the directory with a manifest
    /// template (pre-filled with the supplied id, name, category, and default
    /// requirements), a Python entrypoint package, a synthetic VariantSet
    /// fixture targeting the algorithm's `requiredRsid`, and a README.
    Init {
        /// Project directory to create. Must not already exist.
        dir: PathBuf,
        /// Algorithm id in `publisher/name` form (e.g. `commonsc/eye-colour`).
        #[arg(long)]
        id: String,
        /// Human-readable name. Defaults to the title-cased id slug.
        #[arg(long)]
        name: Option<String>,
        /// One-line blurb for the catalog card.
        #[arg(long, default_value = "TODO: one-line description of what this algorithm does.")]
        blurb: String,
        /// Atlas region for the catalog grouping. One of:
        /// appearance, senses, fuel, motion, wellness, quality, ancestry, risk, research.
        #[arg(long, default_value = "appearance")]
        category: String,
        /// rsID the algorithm centres on. Used to populate the fixture variant
        /// and the entrypoint's stub lookup. Single-variant is the common case;
        /// multi-variant algorithms can add more rsIDs to the fixture by hand.
        #[arg(long)]
        rsid: Option<String>,
        /// Publisher handle. Inferred from the id (`publisher/name`) by default.
        #[arg(long)]
        publisher_handle: Option<String>,
        /// Publisher display name.
        #[arg(long, default_value = "CommonSense Reference")]
        publisher_name: String,
    },
    /// Register as a publisher with the marketplace. Generates an ed25519
    /// keypair locally, POSTs the public key to /publisher/register, stores
    /// the credentials (with the private key, chmod 0600) under
    /// `~/.commonsc/credentials.json` for future publishes.
    Register {
        /// Human-readable display name shown alongside your published items.
        #[arg(long)]
        name: String,
        /// Email, URL, or other contact handle for users + reviewers.
        #[arg(long)]
        contact: String,
        /// Desired publisher namespace (lowercase kebab-case). If absent we
        /// derive one from `--name`.
        #[arg(long)]
        handle: Option<String>,
        /// Marketplace base URL. Defaults to the production endpoint.
        #[arg(long, default_value = "https://api.commonsc.io")]
        api: String,
        /// Override the credentials file path. Defaults to
        /// `~/.commonsc/credentials.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Overwrite existing credentials if a file is already there.
        #[arg(long)]
        force: bool,
    },
    /// Validate an algorithm project: schema-check the manifest, smoke-test the
    /// fixture against the entrypoint's declared input/output schemas, and
    /// report any structured remediation needed before `publish` will accept.
    Validate {
        /// Path to the algorithm project directory (the one containing
        /// `manifest.template.json` and the entrypoint module).
        project: PathBuf,
    },
    /// Emit `keys.json` — the publisher keyId→pubkey trust source the app uses
    /// to verify publisher signatures. Scans the registry's manifests; the
    /// marketplace co-signing key is pinned in the app, not listed here.
    Keys {
        #[arg(long, default_value = "registry")]
        registry: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Verify a published manifest's signatures. The marketplace co-sign is
    /// checked against `--marketplace-pubkey` (the pinned key); the publisher
    /// signature against `--publisher-pubkey` or, by default, the dev key
    /// derived from its keyId. Exits non-zero if either fails.
    Verify {
        /// Path to a completed manifest.json (with artifact + signatures).
        manifest: PathBuf,
        #[arg(long)]
        marketplace_pubkey: Option<String>,
        #[arg(long)]
        publisher_pubkey: Option<String>,
    },
    /// Generate a fresh ed25519 keypair, printed as base64 in the exact format
    /// the signer/verifier use (32-byte seed / 32-byte public key). Use it to
    /// mint the marketplace co-signing key. The private key is a secret — set it
    /// as COMMONSC_MARKETPLACE_PRIVATE_KEY; never commit it.
    Keygen,
    /// Build the runtime bundle and execute it locally against a fixture through
    /// the Deno + Pyodide sandbox, then check the result envelope conforms to
    /// the Result schema. Requires `deno` on PATH. Exits non-zero if the
    /// algorithm throws, times out, or returns a non-conforming result.
    Run {
        /// Path to the algorithm project directory.
        project: PathBuf,
        /// Fixture VariantSet to run against. Defaults to `fixtures/input.json`.
        #[arg(long)]
        fixture: Option<PathBuf>,
        /// Override the wall-clock limit (seconds). Defaults to the Tier-1 30s.
        #[arg(long)]
        timeout_secs: Option<u64>,
        /// Emit a machine-readable JSON verdict (for agents) instead of the
        /// human report.
        #[arg(long)]
        json: bool,
    },
    /// Bundle the project and either (a) write to a local registry, or (b)
    /// upload to a live marketplace's review queue.
    ///
    /// Default is local. Pass `--remote` to upload. The remote URL comes from
    /// `--api`, then your saved credentials at `~/.commonsc/credentials.json`,
    /// then the production default `https://api.commonsc.io`.
    Publish {
        project: PathBuf,
        /// Submit to a live marketplace instead of writing to a local registry.
        #[arg(long)]
        remote: bool,
        /// API base URL for remote publish. Defaults to credentials.api, then
        /// `https://api.commonsc.io`.
        #[arg(long)]
        api: Option<String>,
        /// Credentials file path. Only consulted in remote mode. Defaults to
        /// `~/.commonsc/credentials.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Override the local registry directory (local mode only). Defaults to
        /// `<workspace>/registry/`.
        #[arg(long)]
        registry: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Init {
            dir,
            id,
            name,
            blurb,
            category,
            rsid,
            publisher_handle,
            publisher_name,
        } => {
            let summary = init::run(init::InitOptions {
                dir,
                id,
                name,
                blurb,
                category,
                rsid,
                publisher_handle,
                publisher_name,
            })?;
            summary.print();
            Ok(())
        }
        Cmd::Register {
            name,
            contact,
            handle,
            api,
            config,
            force,
        } => {
            let summary = register::run(register::RegisterOptions {
                name,
                contact,
                handle,
                api,
                config,
                force,
            })?;
            summary.print();
            Ok(())
        }
        Cmd::Validate { project } => {
            let report = validate::run(&project).with_context(|| {
                format!("validate failed for {}", project.display())
            })?;
            report.print();
            if report.outcome.is_fail() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Keys { registry, out } => {
            let index_path = registry.join("index.json");
            let idx: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&index_path)
                    .with_context(|| format!("reading {}", index_path.display()))?,
            )?;
            let mut key_ids = std::collections::BTreeSet::new();
            for e in idx.get("entries").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                let Some(murl) = e.get("manifestUrl").and_then(|v| v.as_str()) else { continue };
                let mpath = registry.join(murl.trim_start_matches("./"));
                if let Ok(raw) = std::fs::read_to_string(&mpath) {
                    if let Ok(m) = serde_json::from_str::<serde_json::Value>(&raw) {
                        if let Some(kid) = m
                            .get("publisher")
                            .and_then(|p| p.get("keyId"))
                            .and_then(|v| v.as_str())
                        {
                            key_ids.insert(kid.to_string());
                        }
                    }
                }
            }
            let keys: Vec<serde_json::Value> = key_ids
                .iter()
                .map(|kid| {
                    serde_json::json!({
                        "keyId": kid,
                        "alg": "ed25519",
                        "publicKey": commonsc_devkit::signing::public_key_dev(kid),
                    })
                })
                .collect();
            let doc = serde_json::json!({ "schemaVersion": "1", "keys": keys });
            let out = out.unwrap_or_else(|| registry.join("keys.json"));
            std::fs::write(&out, serde_json::to_string_pretty(&doc)? + "\n")
                .with_context(|| format!("writing {}", out.display()))?;
            println!("wrote {} ({} publisher key(s))", out.display(), key_ids.len());
            Ok(())
        }
        Cmd::Verify { manifest, marketplace_pubkey, publisher_pubkey } => {
            let raw = std::fs::read_to_string(&manifest)
                .with_context(|| format!("reading {}", manifest.display()))?;
            let m: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", manifest.display()))?;
            let canonical = commonsc_devkit::manifest::canonical_with_blanks(&m);
            let sig_field = |role: &str, key: &str| -> String {
                m.get("signatures")
                    .and_then(|s| s.get(role))
                    .and_then(|x| x.get(key))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let mut all_ok = true;
            let mut check = |role: &str, pubkey: Option<String>| {
                let key_id = sig_field(role, "keyId");
                let value = sig_field(role, "value");
                match pubkey {
                    Some(pk) => {
                        let ok = commonsc_devkit::signing::verify(&pk, &canonical, &value);
                        println!("  {role:<12} keyId={key_id}  {}", if ok { "VERIFIED" } else { "FAILED" });
                        all_ok &= ok;
                    }
                    None => println!("  {role:<12} keyId={key_id}  (no pubkey — skipped)"),
                }
            };
            // Publisher defaults to the dev key derived from its keyId (embedded
            // first-party items are dev-signed); override with --publisher-pubkey.
            let pub_default = publisher_pubkey.clone().or_else(|| {
                let kid = sig_field("publisher", "keyId");
                (!kid.is_empty()).then(|| commonsc_devkit::signing::public_key_dev(&kid))
            });
            println!("verifying {}", manifest.display());
            check("publisher", pub_default);
            check("marketplace", marketplace_pubkey);
            if all_ok { Ok(()) } else { std::process::exit(1); }
        }
        Cmd::Keygen => {
            use base64::Engine as _;
            let sk = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
            let b64 = base64::engine::general_purpose::STANDARD;
            println!("PRIVATE (secret — set as COMMONSC_MARKETPLACE_PRIVATE_KEY, never commit):");
            println!("  {}", b64.encode(sk.to_bytes()));
            println!("PUBLIC (safe to share / pin in the app):");
            println!("  {}", b64.encode(sk.verifying_key().to_bytes()));
            Ok(())
        }
        Cmd::Run { project, fixture, timeout_secs, json } => {
            let outcome = run::run(run::RunOptions {
                project: project.clone(),
                fixture,
                timeout_secs,
                json,
            })
            .with_context(|| format!("run failed for {}", project.display()))?;
            outcome.print();
            if !outcome.passed {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Publish {
            project,
            remote,
            api,
            config,
            registry,
        } => {
            if remote {
                let api_url = resolve_remote_api(api.as_deref(), config.as_deref())?;
                let submission = publish::run_remote(&project, &api_url)
                    .with_context(|| format!("remote publish failed for {}", project.display()))?;
                submission.print(&api_url);
            } else {
                let entry = publish::run(&project, registry.as_deref()).with_context(|| {
                    format!("publish failed for {}", project.display())
                })?;
                println!(
                    "published {}@{} → {}",
                    entry.manifest.id, entry.manifest.version, entry.registry_dir.display()
                );
            }
            Ok(())
        }
    }
}
