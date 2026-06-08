use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use commonsc_devkit::{init, publish, register, validate};

// Module implementations live under `src/lib.rs` so the marketplace HTTP
// service can call the same validate/publish gate code without duplicating it.

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
        /// Catalog category. Reasonable defaults are `trait` or `wellness`.
        #[arg(long, default_value = "trait")]
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
    /// Bundle the project, sign with the dev publisher + marketplace keys,
    /// and write the entry to the local registry at `commonsc/registry/`.
    Publish {
        project: PathBuf,
        /// Override the registry directory. Defaults to `<workspace>/registry/`
        /// resolved from the directory layout.
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
        Cmd::Publish { project, registry } => {
            let entry = publish::run(&project, registry.as_deref())
                .with_context(|| format!("publish failed for {}", project.display()))?;
            println!(
                "published {}@{} → {}",
                entry.manifest.id, entry.manifest.version, entry.registry_dir.display()
            );
            Ok(())
        }
    }
}
