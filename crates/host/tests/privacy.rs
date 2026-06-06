//! Privacy boundary negative tests.
//!
//! Each test builds one of the `_naughty` fixtures into a Pyodide bundle in
//! process, runs it through `commonsc_host::sidecar::run_one`, and asserts
//! that the returned `Result` envelope reports `tone: moss` — i.e., that the
//! fixture's bad action was rejected and the algorithm caught the exception.
//! A `tone: rust` result indicates a boundary breach; the test fails loudly.
//!
//! Requires `deno` on PATH; tests are `#[ignore]`d so a missing deno doesn't
//! break `cargo test`. Run with: `cargo test -p commonsc-host --test privacy
//! -- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};

use commonsc_host::sidecar;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn naughty_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../algorithms/_naughty")
        .canonicalize()
        .expect("locate _naughty fixtures")
}

fn build_bundle(project: &Path) -> Vec<u8> {
    // Mirrors commonsc-devkit's bundle layout: tar (deterministic order, no
    // mtime), then zstd. Excludes match what publish would skip.
    let mut entries = Vec::new();
    collect_files(project, project, &mut entries).expect("walk project");
    entries.sort();

    let mut tar = tar::Builder::new(Vec::<u8>::new());
    for rel in &entries {
        let abs = project.join(rel);
        let meta = fs::metadata(&abs).expect("stat bundle file");
        let mut header = tar::Header::new_gnu();
        header.set_size(meta.len());
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        let mut file = fs::File::open(&abs).expect("open bundle file");
        tar.append_data(&mut header, rel, &mut file)
            .expect("append to tar");
    }
    let raw_tar = tar.into_inner().expect("finalize tar");

    let mut compressed = Vec::with_capacity(raw_tar.len() / 4);
    zstd::stream::copy_encode(&raw_tar[..], &mut compressed, 19).expect("zstd encode");
    compressed
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if matches!(s.as_ref(), "manifest.template.json" | "fixtures" | "README.md" | ".DS_Store" | "__pycache__" | ".git") {
            continue;
        }
        let abs = entry.path();
        let rel = abs.strip_prefix(root).unwrap().to_path_buf();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_files(root, &abs, out)?;
        } else if ft.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

fn run_fixture(fixture: &str) -> Value {
    let project = naughty_dir().join(fixture);
    assert!(project.is_dir(), "missing fixture dir: {}", project.display());
    let bundle = build_bundle(&project);
    let sha = hex::encode(Sha256::digest(&bundle));
    let variants = json!({
        "referenceBuild": "GRCh38",
        "fileKind": "23andme",
        "sampleId": "naughty-fixture",
        "variants": []
    });
    sidecar::run_one(&bundle, &sha, "naughty.main", "compute", variants)
        .unwrap_or_else(|e| panic!("infrastructure error running {fixture}: {e}"))
}

fn assert_boundary_held(fixture: &str) {
    let result = run_fixture(fixture);
    let tone = result.get("tone").and_then(Value::as_str).unwrap_or("");
    let summary = result.get("summary").and_then(Value::as_str).unwrap_or("");
    assert_eq!(
        tone, "moss",
        "{fixture} reported a boundary breach: {summary}\nfull result: {result:#}",
    );
}

#[test]
#[ignore = "requires deno on PATH"]
fn socket_open_is_blocked() {
    assert_boundary_held("socket-open");
}

#[test]
#[ignore = "requires deno on PATH"]
fn fs_escape_is_blocked() {
    assert_boundary_held("fs-escape");
}

#[test]
#[ignore = "requires deno on PATH"]
fn subprocess_spawn_is_blocked() {
    assert_boundary_held("subprocess-spawn");
}

#[test]
#[ignore = "requires deno on PATH"]
fn network_fetch_is_blocked() {
    assert_boundary_held("network-fetch");
}
