// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DMG packaging (release spec §6 `dmg.rs`): `hdiutil create` UDZO with the
//! `/Applications` symlink (the pretty create-dmg layout was deliberately
//! dropped — spec decision 20), then sha256 the DMG bytes in-process via
//! `sha2` so the digest written into the manifest is provably the digest of
//! the file we just produced.
//!
//! Port of `apps/aterm-mac/make-dmg.sh`, hdiutil branch only. The signed .app
//! goes in AS-IS: run this AFTER `sign::sign_app` (the DMG freezes the app's
//! bytes), and hand the result to `sign::sign_and_notarize_dmg` next.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The packaged artifact: the release asset path, its exact byte digest (→
/// manifest `sha256`, cask pin, publish self-check) and size (transcript).
pub struct DmgOut {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Package `<app>` into `<out_dir>/aterm-<short>.dmg`.
///
/// The artifact name MUST stay `aterm-{short}.dmg` — it is the exact asset
/// name written into the manifest's `dmg`/`url` fields, and every installed
/// v0.25 client resolves its download by that name (a mismatch 404s the whole
/// fleet's update).
pub fn create(app: &Path, out_dir: &Path, short_version: &str) -> Result<DmgOut, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let dmg = out_dir.join(format!("aterm-{short_version}.dmg"));
    // Volume name matches make-dmg.sh ("aterm 0.2.0") — what Finder shows when
    // the image is mounted.
    let volname = format!("aterm {short_version}");
    let _ = std::fs::remove_file(&dmg);

    // Stage the .app + an /Applications symlink in a scratch dir, so the
    // mounted image offers the standard drag-to-install gesture even without
    // create-dmg's decorated window. The stage lives under dist/ (same
    // filesystem, already Spotlight-excluded via .metadata_never_index) and is
    // removed on every exit path.
    let stage = out_dir.join(format!(".dmg-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let result = build_in_stage(app, &stage, &volname, &dmg);
    let _ = std::fs::remove_dir_all(&stage); // cleanup on success AND failure
    result?;

    let size_bytes = std::fs::metadata(&dmg)
        .map_err(|e| format!("stat {}: {e}", dmg.display()))?
        .len();
    // In-process digest of the final bytes (spec §6): this exact string feeds
    // the manifest, the cask pin and the post-publish byte-identity check.
    let sha256 = sha256_file(&dmg)?;
    println!(
        "==> done: {} ({:.1} MB)",
        dmg.display(),
        size_bytes as f64 / 1_000_000.0
    );
    println!("    sha256: {sha256}");
    Ok(DmgOut {
        path: dmg,
        sha256,
        size_bytes,
    })
}

#[cfg(unix)]
fn build_in_stage(app: &Path, stage: &Path, volname: &str, dmg: &Path) -> Result<(), String> {
    // `cp -R` (not a hand-rolled walk): preserves symlinks, extended
    // attributes and the codesign seal exactly — the .app inside the DMG must
    // be byte-identical to the one we just signed.
    run_quiet(
        Command::new("cp").arg("-R").arg(app).arg(stage),
        "cp -R app into DMG stage",
    )?;
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))
        .map_err(|e| format!("symlink /Applications: {e}"))?;

    println!("==> hdiutil create {} (UDZO)", dmg.display());
    run_quiet(
        Command::new("hdiutil")
            .arg("create")
            .args(["-volname", volname])
            .arg("-srcfolder")
            .arg(stage)
            .args(["-fs", "HFS+", "-format", "UDZO", "-ov"])
            .arg(dmg),
        "hdiutil create",
    )
}

/// DMG assembly is a macOS operation end-to-end (`hdiutil`, HFS+, the
/// /Applications symlink); refuse plainly elsewhere.
#[cfg(not(unix))]
fn build_in_stage(_app: &Path, _stage: &Path, _volname: &str, _dmg: &Path) -> Result<(), String> {
    Err("DMG creation requires macOS (hdiutil); build releases on a Mac".into())
}

/// Streaming in-process SHA-256 → lowercase hex. Public: the publish
/// self-check re-hashes assets through the same code path.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn run_quiet(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("spawn {what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
