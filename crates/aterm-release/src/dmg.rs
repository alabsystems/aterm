// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging (release spec §6 `dmg.rs`). Two containers carry the SAME
//! signed, lean `aterm.app`:
//!
//! * the **DMG** — `hdiutil create` UDZO with the `/Applications` symlink (the
//!   pretty create-dmg layout was deliberately dropped — spec decision 20). This
//!   is the human download, under the fleet-pinned bare `aterm-<v>.dmg` name.
//! * the **zip** — `ditto -c -k` with no metadata ([`ARCHIVE_NO_METADATA`]). This is
//!   what the in-app updater stages from, because `hdiutil attach` needs a live bootstrap
//!   context (DiskImages registers with the `com.apple.hdiejectd` XPC service)
//!   and the survivor of a seamless overlap update is an orphan whose launchd
//!   job has exited — every attach from there fails ENXIO. `ditto` speaks to no
//!   XPC service, so it works from any process context.
//!
//! Both digests are computed in-process via `aterm-digest`, so the digest written into
//! the manifest is provably the digest of the file we just produced.
//!
//! Port of the retired `apps/aterm-mac/make-dmg.sh` (deleted with the shell
//! pipeline; not in this tree), hdiutil branch only. The signed .app
//! goes in AS-IS: run this AFTER `sign::sign_app` (both containers freeze the
//! app's bytes), and hand the DMG to `sign::sign_and_notarize_dmg` next.
//!
//! ONE lean self-provisioning macOS download: the bundle carries no toolchain
//! seed, so both containers image it exactly as signed, with nothing subtracted.
//!
//! BYTES ONLY, as far as a writer decides it. The bundle needs no file metadata: its
//! one executable is a Mach-O whose signature is inside it, the rest is sealed by
//! `_CodeSignature/CodeResources`, and the self-check's `codesign --verify --deep
//! --strict` on the extracted zip proves it. So no writer here copies any:
//! [`ARCHIVE_NO_METADATA`] stores no extended attribute, resource fork, ACL or
//! quarantine bit in the zip, and the DMG stage is a `cp -RX` copy. MEASURED on macOS
//! 26 (APFS): `ditto -c -k --keepParent` stores each attribute as a `._` AppleDouble
//! entry and `--sequesterRsrc` as `__MACOSX/`, while [`ARCHIVE_NO_METADATA`] stores
//! neither; and a `ditto --norsrc --noextattr` COPY of a tagged bundle still carries
//! its attributes (it clones), where `cp -RX` carries none.
//!
//! What no writer decides: macOS stamps `com.apple.provenance` on every file a
//! provenance-TRACKED process creates (one whose executable, or an ancestor's, or a
//! library it loads, carries the tag — a tracked shell, or a tagged toolchain: `targo`
//! is the cutter's parent and `trustc` writes its binary, and atpkg no longer clears a
//! tag a tracked writer laid in the store). The zip is unaffected — an archive entry is
//! bytes. The DMG is not: a tracked cutter's stage, and every file its `hdiutil` writes
//! into the image, carry the tag. MEASURED 2026-10-09 from a tracked parent: 23 of 23
//! files of a Developer ID `aterm.app` in the image; the same from a clean stage, from
//! a `ditto --noextattr` stage and through `diskutil image create`, while `hdiutil
//! makehybrid` drops the tag but stamps `com.apple.FinderInfo`, which `codesign
//! --strict` refuses. The tagged app passes `codesign --verify --deep --strict` and
//! `spctl -a -t exec`, and the updater installs from the zip. Apple's notary accepts
//! such a DMG as it does an untagged one: MEASURED 2026-10-09, a DMG whose app's files
//! all carried the tag, signed, packed and stapled from a tracked process, was Accepted
//! (`Ready for distribution`, no issues), stapled, and passed `spctl` as `Notarized
//! Developer ID`, the DMG and the app inside alike (docs/RELEASING.md names the
//! submissions). Nothing here reads the attribute: the cut ships the DMG it made.
//!
//! `hdiutil create` runs in the operator's shell, which may have outlived the aterm
//! that forked it (a seamless update) and so hold the bootstrap context of a launchd
//! job that is gone — the context in which the updater's survivor saw `hdiutil attach`
//! fail ENXIO. MEASURED 2026-10-09 on m3 from such a shell (its login shell's parent
//! exited at an update; no application job of that aterm remained in `gui/501`):
//! `hdiutil create -srcfolder` and `hdiutil attach -readonly -nobrowse` both
//! succeeded.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `ditto`'s switches for a PKZip archive of bytes alone: no resource forks or HFS
/// metadata, no extended attributes, no ACLs, no quarantine (the set `DITTONORSRC`
/// stands for, spelled out so nothing in the environment can turn one back on).
pub const ARCHIVE_NO_METADATA: [&str; 4] = ["--norsrc", "--noextattr", "--noacl", "--noqtn"];

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
/// fleet's update). The BYTES under the name are the lean app.
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
    // filesystem, so the moves below are renames) and is removed on every exit
    // path. Its NAME begins with a dot, which is what keeps it out of Spotlight
    // (MEASURED: a `.metadata_never_index` marker is inert; crates/atpkg/src/noindex.rs).
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
/// `ditto -c -k` [`ARCHIVE_NO_METADATA`] `--keepParent` archives the signed bundle
/// as bytes: the `_CodeSignature` layout and every symlink survive (so the extracted
/// app still verifies), `--keepParent` puts `aterm.app` at the archive root (the
/// client extracts and expects exactly that name), and no `._` or `__MACOSX/` entry
/// is written ([`assert_zip_has_no_metadata`] holds the archive to that).
///
/// The artifact name MUST stay `aterm-{short}-mac.zip`: it is the exact asset
/// name written into the manifest's `zip` field, and the client re-derives that
/// same string from the release tag and refuses anything else.
///
/// Archives the bundle AS SIGNED (and, on the active Apple tier, as stapled), so
/// the bytes `ditto` reads are exactly the bytes the DMG carries.
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
    assert_zip_has_no_metadata(&zip)?;

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
            .args(["-c", "-k"])
            .args(ARCHIVE_NO_METADATA)
            .arg("--keepParent")
            .arg(app)
            .arg(zip),
        "ditto -c -k app into updater zip",
    )
}

/// Refuse a zip that carries file metadata: any `__MACOSX/` entry (a sequestered
/// fork) or `._` entry (an AppleDouble attribute file), as `zipinfo -1` lists them.
#[cfg(unix)]
pub fn assert_zip_has_no_metadata(zip: &Path) -> Result<(), String> {
    let out = Command::new("/usr/bin/zipinfo")
        .arg("-1")
        .arg(zip)
        .output()
        .map_err(|e| format!("spawn zipinfo for {}: {e}", zip.display()))?;
    if !out.status.success() {
        return Err(format!(
            "zipinfo -1 {} failed: {}",
            zip.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let carriers: Vec<&str> = listing
        .lines()
        .filter(|entry| {
            entry.starts_with("__MACOSX/")
                || entry
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .is_some_and(|leaf| leaf.starts_with("._"))
        })
        .collect();
    if carriers.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} carries file metadata ({} entr{}: {}) — the updater zip is the bundle's bytes \
         and nothing else",
        zip.display(),
        carriers.len(),
        if carriers.len() == 1 { "y" } else { "ies" },
        carriers
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(not(unix))]
pub fn assert_zip_has_no_metadata(_zip: &Path) -> Result<(), String> {
    Err("checking a bundle zip requires macOS (zipinfo); build releases on a Mac".into())
}

/// Archiving a signed bundle without losing its seal is a macOS operation
/// (`ditto`, extended attributes); refuse plainly elsewhere.
#[cfg(not(unix))]
fn archive_app(_app: &Path, _zip: &Path) -> Result<(), String> {
    Err("bundle zip creation requires macOS (ditto); build releases on a Mac".into())
}

/// Copy the signed `app` into `stage` as bytes: `cp -RX` keeps every file's bytes,
/// mode and symlink (so the codesign seal holds) and copies no extended attribute or
/// resource fork. (What a tracked cutter writes carries `com.apple.provenance` all the
/// same — the kernel stamps it, not the copy; see the module docs.)
#[cfg(unix)]
fn copy_into_stage(app: &Path, stage: &Path) -> Result<(), String> {
    run_quiet(
        Command::new("/bin/cp").arg("-RX").arg(app).arg(stage),
        "cp -RX app into DMG stage",
    )
}

#[cfg(unix)]
fn build_in_stage(app: &Path, stage: &Path, volname: &str, dmg: &Path) -> Result<(), String> {
    copy_into_stage(app, stage)?;
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))
        .map_err(|e| format!("symlink /Applications: {e}"))?;

    // THE PLAIN IMAGE, AND ONLY IT. A decorated Finder window (AppleScript against
    // the mounted disk) failed on every launcher cut (`Finder got an error: Can't
    // get disk … (-1728)`). The drag gesture is still offered: the stage carries
    // the /Applications symlink beside the app.
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
    Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{archive_app, assert_zip_has_no_metadata, copy_into_stage};
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::process::Command;

    /// A bundle-shaped tree whose files carry an extended attribute, as a bundle laid
    /// from tagged inputs carries `com.apple.provenance`.
    fn tagged_app(root: &Path) -> std::path::PathBuf {
        let app = root.join("aterm.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/aterm"), b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink("aterm", app.join("Contents/MacOS/aterm-ctl")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
        for file in ["Contents/MacOS/aterm", "Contents/Info.plist"] {
            let out = Command::new("/usr/bin/xattr")
                .args(["-w", "user.t", "x"])
                .arg(app.join(file))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        app
    }

    fn attributes(path: &Path) -> String {
        let out = Command::new("/usr/bin/xattr")
            .arg("-lr")
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The attribute names on `path` and everything under it (`xattr -r` prints one
    /// `<path>: <name>` line per attribute).
    fn attribute_names(path: &Path) -> BTreeSet<String> {
        let out = Command::new("/usr/bin/xattr")
            .arg("-r")
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.rsplit_once(": ").map(|(_, name)| name.to_owned()))
            .collect()
    }

    #[test]
    fn the_updater_zip_and_the_dmg_stage_carry_no_file_metadata() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-dmg-")
            .tempdir()
            .unwrap();
        let app = tagged_app(dir.path());
        assert!(attributes(&app).contains("user.t"), "the fixture is tagged");

        let zip = dir.path().join("aterm-mac.zip");
        archive_app(&app, &zip).unwrap();
        assert_zip_has_no_metadata(&zip).unwrap();

        // What this process stamps on every file it writes: `com.apple.provenance` when
        // it is provenance-tracked (a gate run from under a tagged app), else nothing. No
        // copy can avoid that floor; anything above it came from the source.
        let control = dir.path().join("control");
        std::fs::write(&control, b"").unwrap();
        let stamped = attribute_names(&control);
        let stage = dir.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        copy_into_stage(&app, &stage).unwrap();
        let carried: Vec<String> = attribute_names(&stage)
            .difference(&stamped)
            .cloned()
            .collect();
        assert!(
            carried.is_empty(),
            "the stage copy carries the source's attributes {carried:?}: {}",
            attributes(&stage)
        );
        assert_eq!(
            std::fs::read_link(stage.join("aterm.app/Contents/MacOS/aterm-ctl")).unwrap(),
            Path::new("aterm")
        );

        // Negative control: the archive the cutter used to make keeps the attributes
        // in `__MACOSX/`, and the check refuses it, naming the entries.
        let sequestered = dir.path().join("sequestered.zip");
        let out = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(&app)
            .arg(&sequestered)
            .output()
            .unwrap();
        assert!(out.status.success());
        let err = assert_zip_has_no_metadata(&sequestered).unwrap_err();
        assert!(err.contains("__MACOSX/"), "{err}");
        // And the default archive stores them as `._` entries.
        let doubled = dir.path().join("doubled.zip");
        let out = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&app)
            .arg(&doubled)
            .output()
            .unwrap();
        assert!(out.status.success());
        let err = assert_zip_has_no_metadata(&doubled).unwrap_err();
        assert!(err.contains("._"), "{err}");
    }
}
