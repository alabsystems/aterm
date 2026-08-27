// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging (release spec §6 `dmg.rs`). Two containers carry the SAME
//! signed, lean `aterm.app`:
//!
//! * the **DMG** — `hdiutil create` UDZO with the `/Applications` symlink (the
//!   pretty create-dmg layout was deliberately dropped — spec decision 20). This
//!   is the human download, under the fleet-pinned bare `aterm-<v>.dmg` name.
//! * the **zip** — `ditto -c -k --sequesterRsrc --keepParent`. This is what the
//!   in-app updater stages from, because `hdiutil attach` needs a live bootstrap
//!   context (DiskImages registers with the `com.apple.hdiejectd` XPC service)
//!   and the survivor of a seamless overlap update is an orphan whose launchd
//!   job has exited — every attach from there fails ENXIO. `ditto` speaks to no
//!   XPC service, so it works from any process context.
//!
//! Both digests are computed in-process via `aterm-digest`, so the digest written into
//! the manifest is provably the digest of the file we just produced.
//!
//! Port of `apps/aterm-mac/make-dmg.sh`, hdiutil branch only. The signed .app
//! goes in AS-IS: run this AFTER `sign::sign_app` (both containers freeze the
//! app's bytes), and hand the DMG to `sign::sign_and_notarize_dmg` next.
//!
//! RETIRED 2026-08-26 (owner direction: ONE lean self-provisioning macOS
//! download): the batteries-included seed and with it the whole restage
//! family — the seed-stripping zip stage, the per-arch `aterm-<v>-x86_64.dmg`
//! filter (`create_arch_filtered`), the `aterm-<v>-lite.dmg` twin
//! (`create_lite`) and the post-restage codesign/Gatekeeper re-proof. The
//! bundle `bundle::assemble` hands over carries no seed, so both containers
//! image it exactly as signed, with nothing subtracted.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A packaged artifact (DMG or zip): the release asset path, its exact byte
/// digest (→ manifest `sha256`/`zip_sha256`, publish self-check) and size
/// (transcript).
pub struct Packaged {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Package `<app>` into `<out_dir>/aterm-<short>.dmg` — the app exactly as it
/// stands.
///
/// The artifact name MUST stay `aterm-{short}.dmg` — it is the exact asset
/// name written into the manifest's `dmg`/`url` fields, and every installed
/// client resolves its download by that name (a mismatch 404s the whole
/// fleet's update). The BYTES under the name are the lean app: nothing in the
/// deployed fleet ever required them to carry a toolchain seed, and since
/// 2026-08-26 nothing does.
pub fn create(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
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
    // the manifest and the post-publish byte-identity check.
    let sha256 = sha256_file(&dmg)?;
    println!(
        "==> done: {} ({:.1} MB)",
        dmg.display(),
        size_bytes as f64 / 1_000_000.0
    );
    println!("    sha256: {sha256}");
    Ok(Packaged {
        path: dmg,
        sha256,
        size_bytes,
    })
}

/// Package `<app>` into `<out_dir>/aterm-<short>-mac.zip` — the container the
/// in-app updater downloads and extracts.
///
/// `ditto -c -k --sequesterRsrc --keepParent` is the ONLY supported way to
/// archive a signed bundle: it preserves extended attributes and the
/// `_CodeSignature` layout (so the extracted app still verifies), `--keepParent`
/// puts `aterm.app` at the archive root (the client extracts and expects exactly
/// that name), and `--sequesterRsrc` keeps resource forks in the standard
/// `__MACOSX` sidecar rather than mangling them. `zip(1)` would silently drop
/// the metadata the signature seals.
///
/// The artifact name MUST stay `aterm-{short}-mac.zip`: it is the exact asset
/// name written into the manifest's `zip` field, and the client re-derives that
/// same string from the release tag and refuses anything else.
///
/// Archives the bundle AS SIGNED (and, on the active Apple tier, as stapled):
/// the seed-stripping restage that used to run here is retired with the seed
/// itself, so the bytes `ditto` reads are exactly the bytes the DMG carries.
pub fn create_zip(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let zip = out_dir.join(format!("aterm-{short_version}-mac.zip"));
    // `ditto -c -k` would overwrite an existing archive, but a stale one from an
    // earlier attempt must not survive a FAILED run and get hashed as this cut's
    // artifact — remove it first, exactly as `create` does for the DMG.
    let _ = std::fs::remove_file(&zip);

    println!("==> ditto -c -k {} (updater container)", zip.display());
    archive_app(app, &zip)?;

    let size_bytes = std::fs::metadata(&zip)
        .map_err(|e| format!("stat {}: {e}", zip.display()))?
        .len();
    let sha256 = sha256_file(&zip)?;
    println!(
        "==> done: {} ({:.1} MB)",
        zip.display(),
        size_bytes as f64 / 1_000_000.0
    );
    println!("    sha256: {sha256}");
    Ok(Packaged {
        path: zip,
        sha256,
        size_bytes,
    })
}

#[cfg(unix)]
fn archive_app(app: &Path, zip: &Path) -> Result<(), String> {
    run_quiet(
        Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(app)
            .arg(zip),
        "ditto -c -k app into updater zip",
    )
}

/// Archiving a signed bundle without losing its seal is a macOS operation
/// (`ditto`, extended attributes); refuse plainly elsewhere.
#[cfg(not(unix))]
fn archive_app(_app: &Path, _zip: &Path) -> Result<(), String> {
    Err("bundle zip creation requires macOS (ditto); build releases on a Mac".into())
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
    use aterm_digest::Sha256;
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
