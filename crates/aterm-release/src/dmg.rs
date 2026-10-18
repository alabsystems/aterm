// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging: two containers carry the same signed, lean `aterm.app`.
//!
//! * the DMG (`hdiutil create` UDZO with an `/Applications` symlink), the human download,
//!   under the fleet-pinned name `aterm-<v>.dmg`;
//! * the zip (`ditto -c -k` [`ARCHIVE_NO_METADATA`]), what the in-app updater stages from.
//!   `hdiutil attach` needs a live bootstrap context (the `com.apple.hdiejectd` XPC
//!   service), and the survivor of a seamless overlap update is an orphan whose launchd job
//!   has exited, so every attach there fails ENXIO; `ditto` speaks to no XPC service. (An
//!   operator shell that outlived its aterm can still `hdiutil create` and `attach`.)
//!
//! Run after `sign::sign_app` and before `sign::sign_and_notarize_dmg`. Digests are computed
//! in-process (`aterm-digest`), so the manifest's digest is that of the file just produced.
//!
//! Neither container stores file metadata: the Mach-O carries its signature,
//! `_CodeSignature/CodeResources` seals the rest, and the self-check's `codesign --verify
//! --deep --strict` on the extracted zip proves it. The DMG stage is a `cp -RX` copy: a
//! `ditto --norsrc --noextattr` copy still clones attributes, and a default `ditto -c -k`
//! stores them as `._` entries.
//!
//! macOS stamps `com.apple.provenance` on every file a provenance-tracked process creates,
//! so the DMG's files carry it (every stage method tried tags; `hdiutil makehybrid` does not
//! but stamps `com.apple.FinderInfo`, which `codesign --strict` refuses). The tagged app
//! passes `codesign` and `spctl` and the notary accepts it (docs/RELEASING.md names the
//! submissions), so nothing here reads the attribute.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `ditto`'s switches for a PKZip archive of bytes alone: no resource forks or HFS
/// metadata, no extended attributes, no ACLs, no quarantine (the set `DITTONORSRC`
/// stands for, spelled out so nothing in the environment can turn one back on).
pub const ARCHIVE_NO_METADATA: [&str; 4] = ["--norsrc", "--noextattr", "--noacl", "--noqtn"];

/// A packaged artifact (DMG or zip): the release asset path, its byte digest (manifest
/// `sha256`/`zip_sha256`, publish self-check) and size.
pub struct Packaged {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Package `<app>`, exactly as it stands, into `<out_dir>/aterm-<short>.dmg`.
///
/// The name must stay `aterm-{short}.dmg`: it is the manifest's `dmg`/`url` asset, and
/// every installed client resolves its download by it (a mismatch 404s the fleet's update).
pub fn create(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let dmg = out_dir.join(format!("aterm-{short_version}.dmg"));
    // What Finder shows when the image is mounted ("aterm 0.2.0").
    let volname = format!("aterm {short_version}");
    let _ = std::fs::remove_file(&dmg);

    // Stage the app and an /Applications symlink (the drag-to-install gesture) under
    // dist/, on the same filesystem, removed on every exit path. The dot-name keeps the
    // stage out of Spotlight (MEASURED: a `.metadata_never_index` marker is inert, by
    // atpkg's since-deleted noindex probe).
    let stage = out_dir.join(format!(".dmg-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let result = build_in_stage(app, &stage, &volname, &dmg);
    let _ = std::fs::remove_dir_all(&stage); // cleanup on success AND failure
    result?;

    let size_bytes = std::fs::metadata(&dmg)
        .map_err(|e| format!("stat {}: {e}", dmg.display()))?
        .len();
    // This exact string feeds the manifest and the post-publish byte-identity check.
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

/// Package `<app>` into `<out_dir>/aterm-<short>-mac.zip`, the container the in-app
/// updater downloads and extracts.
///
/// `ditto -c -k` [`ARCHIVE_NO_METADATA`] `--keepParent` archives the bundle as signed
/// (and stapled, on the active Apple tier): the `_CodeSignature` layout and every symlink
/// survive, `aterm.app` sits at the archive root (the client expects exactly that name),
/// and no `._` or `__MACOSX/` entry is written ([`assert_zip_has_no_metadata`]).
///
/// The name must stay `aterm-{short}-mac.zip`: it is the manifest's `zip` field, and the
/// client re-derives it from the release tag and refuses anything else.
pub fn create_zip(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let zip = out_dir.join(format!("aterm-{short_version}-mac.zip"));
    // A stale archive from an earlier attempt must not survive a failed run and be hashed
    // as this cut's artifact.
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

/// Copy the signed `app` into `stage` as bytes: `cp -RX` keeps every file's bytes, mode
/// and symlink (so the seal holds) and copies no extended attribute or resource fork. A
/// tracked cutter's copy still carries `com.apple.provenance` (see the module docs).
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

    // A plain image: a decorated Finder window (AppleScript against the mounted disk)
    // fails with `Finder got an error: Can't get disk … (-1728)`. The /Applications
    // symlink beside the app still offers the drag gesture.
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

        // What this process stamps on every file it writes (`com.apple.provenance` when it
        // is provenance-tracked, else nothing): no copy avoids that floor, so anything
        // above it came from the source.
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

        // Negative control: `--sequesterRsrc` keeps the attributes in `__MACOSX/`, and the
        // check refuses it, naming the entries.
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
