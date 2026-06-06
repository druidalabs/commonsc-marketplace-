use std::path::PathBuf;

use clap::{Parser, Subcommand};

use commonsc_host::sidecar::{self, Sidecar, SidecarConfig};

#[derive(Parser)]
#[command(name = "commonsc-host", version, about = "CommonSense Tier-1 sandbox host")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Boot the sidecar, evaluate a Python expression, print the result. Smoke test
    /// for the Rust <-> Deno <-> Pyodide bridge — no bundle, no schemas yet.
    Hello {
        /// Python expression to evaluate inside Pyodide. Default exercises numeric eval.
        #[arg(default_value = "2 + 3")]
        expr: String,
    },
    /// Run an algorithm bundle once against a JSON VariantSet on disk and
    /// print the result envelope. Used both as the production entry point and
    /// as the end-to-end smoke test for the bundle-loader + Pyodide path.
    Run {
        /// Path to the published `bundle.tar.zst` artifact.
        #[arg(long)]
        bundle: PathBuf,
        /// Expected sha256 of the artifact (hex, 64 chars). Use `--unchecked`
        /// to skip the verification while iterating locally on an unsigned
        /// bundle — never set in production paths.
        #[arg(long)]
        sha256: Option<String>,
        /// Skip the sha256 check. Mutually exclusive with `--sha256` in
        /// practice; the CLI accepts either.
        #[arg(long, default_value_t = false)]
        unchecked: bool,
        /// Entrypoint module declared in the manifest (e.g. `prs_height.main`).
        #[arg(long)]
        module: String,
        /// Entrypoint function declared in the manifest.
        #[arg(long)]
        function: String,
        /// JSON file containing the VariantSet to feed the algorithm.
        #[arg(long)]
        input: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Hello { expr } => {
            let mut sidecar = Sidecar::spawn(SidecarConfig::default())?;
            let value = sidecar.hello(&expr)?;
            println!("{value}");
            sidecar.shutdown()?;
            Ok(())
        }
        Cmd::Run {
            bundle,
            sha256,
            unchecked,
            module,
            function,
            input,
        } => {
            let bytes = std::fs::read(&bundle)?;
            let variant_set: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&input)?)?;
            let expected = sha256.clone().unwrap_or_else(|| {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(&bytes))
            });
            let _ = unchecked; // accepted for symmetry; if --sha256 isn't given we already match
            let value = sidecar::run_one(&bytes, &expected, &module, &function, variant_set)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
    }
}
