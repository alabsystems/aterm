// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared staging fixtures and explicit mounted-volume acceptance.

use super::*;
use crate::manifest::{Artifact, Cost};
use crate::tree::file_sha256;

pub(super) fn artifact(sha256: &str, tree_root: &str) -> Artifact {
    Artifact {
        target: "aarch64-apple-darwin".into(),
        kind: "binary".into(),
        asset: "ay-18.tar.zst".into(),
        sha256: sha256.into(),
        sha512: String::new(),
        tree_root: tree_root.into(),
        size: 0,
        reloc: "self-contained".into(),
        cost: Cost {
            download_bytes: 0,
            disk_installed: 1 << 20,
            build_seconds: 0,
        },
        url: String::new(),
        payload: String::new(),
        entry: String::new(),
        strip_components: 0,
        links: std::collections::BTreeMap::new(),
        vendor: String::new(),
        protocol: "github-release".into(),
        provenance: None,
    }
}

/// Every stage-scratch sibling currently sitting beside `build_dir`.
pub(super) fn scratch_beside(build_dir: &Path) -> Vec<String> {
    let Some(parent) = build_dir.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.contains(".incoming-") || n.contains(".superseded-"))
        .collect();
    out.sort();
    out
}

/// An https artifact over `archive`, signed for ITS bytes and for `root`.
pub(super) fn vendor_artifact(archive: &Path, payload: &str, root: &str) -> Artifact {
    let mut a = artifact(&file_sha256(archive).unwrap(), root);
    a.kind = if payload == "dmg" {
        "app-bundle".into()
    } else {
        "binary".into()
    };
    a.protocol = "https".into();
    a.payload = payload.into();
    a.size = std::fs::metadata(archive).unwrap().len();
    a
}

/// Write `content` at `dir/rel` with `mode`, creating parents.
#[cfg(unix)]
pub(super) fn lay(dir: &Path, rel: &str, content: &[u8], mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
pub(super) fn lay_link(dir: &Path, rel: &str, target: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, p).unwrap();
}

/// `chmod +a <ace> <path>`: add an ACL entry, which std has no call for.
#[cfg(target_os = "macos")]
pub(super) fn add_acl(path: &Path, ace: &str) {
    let out = aterm_uds::spawnfd::command("/bin/chmod")
        .arg("+a")
        .arg(ace)
        .arg(path)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "chmod +a {ace:?} {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// Whether `path` carries an ACL entry: `ls -led` lists each one on a line of its own (`  0:
/// group:everyone allow write`) after the entry's.
#[cfg(target_os = "macos")]
pub(super) fn has_acl(path: &Path) -> bool {
    let out = aterm_uds::spawnfd::command("/bin/ls")
        .arg("-led")
        .arg(path)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ls -led {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .any(|line| line.trim_start().starts_with("0:"))
}

/// `Foo.app` laid at `src` as an image's root: an executable, two internal symlinks, one of them a
/// `..` climb to the stage root and back, and the modes and ACL AT3 is about, beside the usual
/// `Applications` link and a README.
#[cfg(target_os = "macos")]
pub(super) fn foo_app_image(
    d: &Path,
    src: &Path,
    image: &Path,
    make: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<Artifact> {
    use std::os::unix::fs::PermissionsExt as _;
    /// Resources -> Contents -> Foo.app -> the stage root, then back down to the binary.
    const UP: &str = "../../../Foo.app/Contents/MacOS/foo";
    /// The ACL entry the image's `acl` file carries, also used as its contents.
    const ACE: &str = "everyone allow write";
    /// `(path, contents, mode on the image, mode staged)`.
    const FILES: &[(&str, &[u8], u32, u32)] = &[
        (
            "Foo.app/Contents/MacOS/foo",
            b"#!/bin/sh\necho foo\n",
            0o755,
            0o755,
        ),
        ("Foo.app/Contents/Info.plist", b"<plist/>", 0o644, 0o644),
        ("Foo.app/Contents/Resources/secret", b"s", 0o600, 0o644),
        ("Foo.app/Contents/Resources/sealed", b"r", 0o400, 0o644),
        ("Foo.app/Contents/Resources/shared", b"w", 0o666, 0o644),
        (
            "Foo.app/Contents/Resources/acl",
            ACE.as_bytes(),
            0o644,
            0o644,
        ),
        (
            "Foo.app/Contents/Plugins/helper",
            b"#!/bin/sh\n",
            0o775,
            0o755,
        ),
    ];
    /// `(directory, mode on the image)`; every one is staged `0755`.
    const DIRS: &[(&str, u32)] = &[
        ("Foo.app/Contents/Plugins", 0o777),
        ("Foo.app/Contents", 0o775),
        ("Foo.app", 0o700),
    ];
    for (rel, body, mode, _) in FILES {
        lay(src, rel, body, *mode);
    }
    for (rel, mode) in DIRS {
        std::fs::set_permissions(src.join(rel), std::fs::Permissions::from_mode(*mode)).unwrap();
    }
    add_acl(&src.join("Foo.app/Contents/Resources/acl"), ACE);
    assert!(
        has_acl(&src.join("Foo.app/Contents/Resources/acl")),
        "the image's file carries the ACL under test"
    );
    lay_link(src, "Foo.app/Contents/current", "MacOS/foo");
    lay_link(src, "Foo.app/Contents/Resources/up", UP);
    lay(src, "README.txt", b"not copied", 0o644);
    lay_link(src, "Applications", "/Applications");

    let replica = d.join("replica");
    for (rel, body, _, staged) in FILES {
        lay(&replica, rel, body, *staged);
    }
    lay_link(&replica, "Foo.app/Contents/current", "MacOS/foo");
    lay_link(&replica, "Foo.app/Contents/Resources/up", UP);
    lay_link(&replica, "bin/foo", "../Foo.app/Contents/MacOS/foo");
    let expected = tree_root(&replica).unwrap();
    make(src, image)?;
    let mut art = vendor_artifact(image, "dmg", &expected);
    art.links
        .insert("foo".into(), "Foo.app/Contents/MacOS/foo".into());
    Ok(art)
}

/// Detaches every disk image attached from under `dir` when it goes out of scope —
/// on EVERY path out of a test: an early `return`, an assertion panic, or success.
#[cfg(all(debug_assertions, target_os = "macos"))]
struct DetachAll {
    prefixes: Vec<PathBuf>,
}

#[cfg(all(debug_assertions, target_os = "macos"))]
impl DetachAll {
    /// Guard everything attached from under `dir` — by the path as spelled and as
    /// resolved, since `/var/folders/…` and `/private/var/folders/…` are the same
    /// directory and `hdiutil` echoes whichever spelling it was handed.
    fn over(dir: &Path) -> Self {
        let mut prefixes = vec![dir.to_path_buf()];
        match dir.canonicalize() {
            Ok(c) if c != *dir => prefixes.push(c),
            _ => {}
        }
        Self { prefixes }
    }

    /// The device nodes of every image `hdiutil info` still lists from under the
    /// guarded directory. Never panics: it runs from `Drop`, which may itself be
    /// running because an assertion already failed.
    fn leaked(&self) -> std::io::Result<Vec<PathBuf>> {
        let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
            .arg("info")
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!("hdiutil info: {out:?}")));
        }
        Ok(parse_hdiutil_info(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .filter(|a| self.prefixes.iter().any(|p| a.image.starts_with(p)))
            .map(|a| a.node)
            .collect())
    }

    fn assert_clean(&self, what: &str) {
        let leaked = self
            .leaked()
            .expect("read the final disk-image attachment report");
        assert!(leaked.is_empty(), "{what}: still attached: {leaked:?}");
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
impl Drop for DetachAll {
    fn drop(&mut self) {
        for node in self.leaked().unwrap_or_default() {
            let _ = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
                .args(["detach", "-force", "-quiet"])
                .arg(&node)
                .status();
        }
    }
}

/// Create one image for this acceptance row. Setup is attempted once; an unavailable
/// disk-image service retains its actual error in COULD NOT RUN.
#[cfg(all(debug_assertions, target_os = "macos"))]
fn create_image(src: &Path, image: &Path) -> std::io::Result<()> {
    let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
        .arg("create")
        .arg("-srcfolder")
        .arg(src)
        .args(["-volname", "AtpkgAcceptance", "-format", "UDZO"])
        .arg(image)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("hdiutil create: {out:?}")))
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
fn prerequisites() -> std::io::Result<()> {
    for tool in ["/usr/bin/hdiutil", "/usr/bin/ditto"] {
        std::fs::metadata(tool)?;
    }
    Ok(())
}

/// Stage a real mounted image's single app, including link/mode/ACL and detach controls.
/// # Errors
/// Returns a setup failure when required tools or the disk-image service are unavailable.
#[cfg(all(debug_assertions, target_os = "macos"))]
pub fn a_real_image_stages_its_single_app(d: &Path) -> std::io::Result<()> {
    prerequisites()?;
    let leaks = DetachAll::over(d);
    let (src, dmg) = (d.join("src"), d.join("foo.dmg"));
    let art = foo_app_image(d, &src, &dmg, create_image)?;
    let build = d.join("store/foo/2026082601");
    verify_and_stage(&art, &dmg, &build, &StageHooks::NONE).unwrap();
    assert!(
        !has_acl(&build.join("Foo.app/Contents/Resources/acl")),
        "no ACL on the image reaches the store"
    );
    let mounts: Vec<_> = std::fs::read_dir(build.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".mnt"))
        .collect();
    assert!(mounts.is_empty(), "no mount point left behind: {mounts:?}");
    leaks.assert_clean("the image must be detached");
    Ok(())
}

/// Refuse a real image with no app and prove detach plus absence of build/scratch.
/// # Errors
/// Returns a setup failure when required tools or the disk-image service are unavailable.
#[cfg(all(debug_assertions, target_os = "macos"))]
pub fn a_real_image_with_no_app_is_refused_and_detached(d: &Path) -> std::io::Result<()> {
    prerequisites()?;
    let leaks = DetachAll::over(d);
    let src = d.join("src-none");
    lay(&src, "README.txt", b"r", 0o644);
    let dmg = d.join("none.dmg");
    create_image(&src, &dmg)?;
    let art = vendor_artifact(&dmg, "dmg", &tree_root(&src).unwrap());
    let build = d.join("store/none/1");
    let err = verify_and_stage(&art, &dmg, &build, &StageHooks::NONE).unwrap_err();
    assert!(matches!(err, StageError::DmgAppCount(0)), "{err:?}");
    assert!(!build.exists());
    assert!(scratch_beside(&build).is_empty());
    leaks.assert_clean("detached on the error path");
    Ok(())
}
