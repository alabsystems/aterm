// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundle packaging into the release DMG (`aterm-<v>.dmg`) and the updater zip (`aterm-<v>-mac.zip`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// `ditto` switches for a PKZip archive without resource forks, extended attributes, ACLs, or quarantine.
pub const ARCHIVE_NO_METADATA: [&str; 4] = ["--norsrc", "--noextattr", "--noacl", "--noqtn"];

/// Packaged release artifact (DMG or zip) with its SHA-256 digest and byte size.
pub struct Packaged {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Packages `app` into `<out_dir>/aterm-<short_version>.dmg`.
pub fn create(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let dmg = out_dir.join(format!("aterm-{short_version}.dmg"));
    let volname = format!("aterm {short_version}");
    let _ = std::fs::remove_file(&dmg);

    let stage = out_dir.join(format!(".dmg-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    let result = build_in_stage(app, &stage, &volname, &dmg);
    let _ = std::fs::remove_dir_all(&stage); // cleanup on success AND failure
    result?;

    let size_bytes = std::fs::metadata(&dmg)
        .map_err(|e| format!("stat {}: {e}", dmg.display()))?
        .len();
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

/// Packages `app` into `<out_dir>/aterm-<short_version>-mac.zip` without file metadata.
pub fn create_zip(app: &Path, out_dir: &Path, short_version: &str) -> Result<Packaged, String> {
    if !app.is_dir() {
        return Err(format!(
            "{} not found — assemble the bundle first",
            app.display()
        ));
    }
    let zip = out_dir.join(format!("aterm-{short_version}-mac.zip"));
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

/// Refuses a zip containing `__MACOSX/` or `._` AppleDouble metadata entries.
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

#[cfg(not(unix))]
fn archive_app(_app: &Path, _zip: &Path) -> Result<(), String> {
    Err("bundle zip creation requires macOS (ditto); build releases on a Mac".into())
}

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

    println!("==> hdiutil create {} (UDZO)", dmg.display());
    run_quiet(
        Command::new(aterm_uds::os_path!("hdiutil"))
            .arg("create")
            .args(["-volname", volname])
            .arg("-srcfolder")
            .arg(stage)
            .args(["-fs", "HFS+", "-format", "UDZO", "-ov"])
            .arg(dmg),
        "hdiutil create",
    )
}

#[cfg(not(unix))]
fn build_in_stage(_app: &Path, _stage: &Path, _volname: &str, _dmg: &Path) -> Result<(), String> {
    Err("DMG creation requires macOS (hdiutil); build releases on a Mac".into())
}

/// Computes the streaming SHA-256 of `path` as lowercase hex.
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
