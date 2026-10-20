// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Bundling Homebrew into atpkg with aterm (design §5, deliverable 1)
//! and `Brewfile` handling over abrew state.

use std::path::Path;

use super::fetch;
use super::install::InstallError;
use super::prefix::{Kind, Prefix, json_escape, publish_directory};

/// The pinned Homebrew/brew release abrew bundles by default, and the
/// SHA-256 of its `archive/refs/tags/<tag>.tar.gz`. A `--version`
/// override must supply its own `--sha256` (fail-closed, design §5).
/// Bump both together; `abrew bundle status` shows what stands.
pub const HOMEBREW_BUNDLE_VERSION: &str = "4.5.8";
pub const HOMEBREW_BUNDLE_SHA256: &str =
    "6a5a18e6c5bea214983ec102dea3126cbc2d162540f8d93387fa9730d1f03be3";

/// The ownership receipt moves with the bundle, so publication cannot leave a
/// new executable paired with an old external record.
const BUNDLE_RECEIPT: &str = ".abrew-bundle.json";

fn bundle_record(directory: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(directory)
        .ok()?
        .file_type()
        .is_dir()
    {
        return None;
    }
    let bytes =
        crate::metadata_io::read_bounded_regular(&directory.join(BUNDLE_RECEIPT), 64 * 1024)
            .ok()?;
    let value: aterm_json::Value = aterm_json::from_slice(&bytes).ok()?;
    let word = |key| value.get(key).and_then(aterm_json::Value::as_str);
    if word("installed_by") != Some("abrew")
        || !word("version").is_some_and(super::valid_component)
        || !word("sha256").is_some_and(valid_sha256)
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn runnable_bundle(directory: &Path) -> bool {
    if ![directory.to_path_buf(), directory.join("bin")]
        .iter()
        .all(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
        })
    {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(directory.join("bin/brew")) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    false
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Lay verified Homebrew with its receipt in a private stage, then publish the
/// directory. Forced replacement exchanges two owned trees atomically.
pub fn install_bundle(
    prefix: &Prefix,
    version: Option<&str>,
    sha256: Option<&str>,
    force: bool,
) -> Result<(), InstallError> {
    let tag = version.unwrap_or(HOMEBREW_BUNDLE_VERSION);
    let expected = match (version, sha256) {
        (None, None) => HOMEBREW_BUNDLE_SHA256,
        (Some(_), Some(hash)) => hash,
        _ => {
            return Err(InstallError::Failed(
                "abrew: bundle overrides need both --version and --sha256".into(),
            ));
        }
    };
    if !super::valid_component(tag) || !valid_sha256(expected) {
        return Err(InstallError::Failed(
            "abrew: invalid bundle version or SHA-256".into(),
        ));
    }
    prefix.ensure()?;
    let dest = prefix.homebrew_bundle();
    let occupied = match std::fs::symlink_metadata(&dest) {
        Ok(_) => {
            if bundle_record(&dest).is_none() {
                return Err(InstallError::Failed(format!(
                    "abrew: refusing to replace unowned bundle path {}",
                    dest.display()
                )));
            }
            if !force {
                if let Some(record) = bundle_record(&dest) {
                    let laid = aterm_json::from_slice::<aterm_json::Value>(record.as_bytes())
                        .ok()
                        .and_then(|v| {
                            v.get("version")
                                .and_then(aterm_json::Value::as_str)
                                .map(str::to_owned)
                        });
                    if laid.as_deref() != Some(tag) {
                        return Err(InstallError::Failed(format!(
                            "abrew: bundled Homebrew {} is laid, not {tag}; use bundle --force --version {tag} --sha256 <sha> to replace it",
                            laid.as_deref().unwrap_or("unknown")
                        )));
                    }
                }
                if !runnable_bundle(&dest) {
                    return Err(InstallError::Failed("abrew: bundled executable is missing or invalid; use bundle --force to repair".into()));
                }
                println!(
                    "abrew: Homebrew bundle already laid at {} (use --force to re-lay)",
                    dest.display()
                );
                return Ok(());
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("abrew: cannot inspect bundle: {error}").into()),
    };
    let url = format!("https://github.com/Homebrew/brew/archive/refs/tags/{tag}.tar.gz");
    let tarball = prefix.cache().join(format!("homebrew-brew-{tag}.tar.gz"));
    if fetch::verify_sha256(&tarball, expected).is_err() {
        println!("==> Bundling Homebrew {tag} into {}", dest.display());
        fetch::curl_to_file(&url, &tarball, &[])?;
    }
    fetch::verify_sha256(&tarball, expected)?;
    let staging = aterm_tempfile::Builder::new()
        .prefix(".abrew-bundle-")
        .tempdir_in(&prefix.root)
        .map_err(|error| format!("abrew: cannot stage bundle: {error}"))?;
    let unpacked = staging.path().join("unpacked");
    fetch::untar_homebrew_bundle(&tarball, &unpacked)?;
    let inner = unpacked.join(format!("brew-{tag}"));
    if !runnable_bundle(&inner) {
        return Err(InstallError::Failed("abrew: bundle tarball needs real root/bin directories and an executable regular bin/brew".into()));
    }
    let record = format!(
        "{{\n  \"installed_by\": \"abrew\",\n  \"version\": \"{}\",\n  \"sha256\": \"{}\",\n  \"source_url\": \"{}\"\n}}\n",
        json_escape(tag),
        json_escape(expected),
        json_escape(&url)
    );
    use std::io::Write as _;
    let mut receipt = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(inner.join(BUNDLE_RECEIPT))
        .map_err(|error| format!("abrew: cannot stage bundle receipt: {error}"))?;
    receipt
        .write_all(record.as_bytes())
        .map_err(|error| format!("abrew: cannot stage bundle receipt: {error}"))?;
    publish_directory(&inner, &dest, occupied).map_err(|error| {
        format!("abrew: cannot publish bundle; existing destination retained: {error}")
    })?;
    println!("🍺  Homebrew {tag} bundled at {}", dest.display());
    Ok(())
}

/// The bundled brew abrew may execute: only a tree carrying a valid
/// abrew bundle receipt with a runnable `bin/brew`. A hand-planted
/// executable without the receipt is never the fallback.
pub(super) fn bundled_brew(prefix: &Prefix) -> Option<std::path::PathBuf> {
    let directory = prefix.homebrew_bundle();
    bundle_record(&directory)?;
    if !runnable_bundle(&directory) {
        return None;
    }
    Some(directory.join("bin/brew"))
}

/// `abrew bundle status`. Unknown legacy/foreign trees are reported separately.
pub fn bundle_status(prefix: &Prefix) {
    let directory = prefix.homebrew_bundle();
    if let Some(record) = bundle_record(&directory) {
        if !runnable_bundle(&directory) {
            println!("bundled: invalid executable (use abrew bundle --force to repair)");
            return;
        }
        println!("bundled: {}", directory.join("bin/brew").display());
        print!("{record}");
    } else if directory.symlink_metadata().is_ok() {
        println!(
            "bundled: unowned path {} (left untouched)",
            directory.display()
        );
    } else {
        println!("bundled: none (lay one with: abrew bundle)");
    }
}

/// Remove only a bundle carrying this engine's embedded ownership receipt.
pub fn bundle_remove(prefix: &Prefix) -> Result<(), InstallError> {
    prefix.ensure()?;
    let dest = prefix.homebrew_bundle();
    match std::fs::symlink_metadata(&dest) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("abrew: cannot inspect bundle: {error}").into()),
        Ok(_) if bundle_record(&dest).is_none() => {
            return Err(InstallError::Failed(format!(
                "abrew: refusing to remove unowned bundle path {}",
                dest.display()
            )));
        }
        Ok(_) => {}
    }
    std::fs::remove_dir_all(&dest)
        .map_err(|error| format!("abrew: cannot remove bundle: {error}"))?;
    println!("abrew: Homebrew bundle removed");
    Ok(())
}

/// `abrew bundle dump`: a Brewfile describing abrew's installed set.
#[must_use]
pub fn brewfile_dump(prefix: &Prefix) -> String {
    let mut out = String::new();
    for t in super::tap::list(prefix) {
        out.push_str(&format!("tap \"{}\"\n", t.name()));
    }
    for i in prefix.installed(Kind::Formula) {
        out.push_str(&format!("brew \"{}\"\n", i.name));
    }
    for i in prefix.installed(Kind::Cask) {
        out.push_str(&format!("cask \"{}\"\n", i.name));
    }
    out
}

/// One Brewfile line's instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrewfileEntry {
    Tap(String),
    Brew(String),
    Cask(String),
}

/// Parse a Brewfile's `tap`/`brew`/`cask "name"` lines (the rest —
/// `mas`, options hashes, expressions — requires whole-command passthrough).
#[must_use]
pub fn parse_brewfile(text: &str) -> (Vec<BrewfileEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let quoted = |key: &str| -> Option<String> {
            let rest = line.strip_prefix(key)?;
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let rest = rest.get(quote.len_utf8()..)?;
            let end = rest.find(quote)?;
            let name = rest.get(..end)?;
            let tail = rest.get(end + quote.len_utf8()..)?.trim();
            if !tail.is_empty() && !tail.starts_with('#') {
                return None;
            }
            let valid = if key == "tap" {
                super::tap::parse_tap_name(name).is_some()
            } else {
                super::valid_component(name)
            };
            valid.then(|| name.to_owned())
        };
        if let Some((user, repo)) = quoted("tap").and_then(|n| super::tap::parse_tap_name(&n)) {
            entries.push(BrewfileEntry::Tap(format!("{user}/{repo}")));
        } else if let Some(n) = quoted("brew") {
            entries.push(BrewfileEntry::Brew(n));
        } else if let Some(n) = quoted("cask") {
            entries.push(BrewfileEntry::Cask(n));
        } else {
            skipped.push(line.to_string());
        }
    }
    (entries, skipped)
}

/// Read a Brewfile path (`./Brewfile` by default, as brew's is).
pub fn read_brewfile(path: Option<&Path>) -> Result<String, String> {
    let p = path.map_or_else(|| std::path::PathBuf::from("Brewfile"), Path::to_path_buf);
    std::fs::read_to_string(&p).map_err(|e| format!("abrew: cannot read {}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brewfile_parse() {
        let (entries, skipped) = parse_brewfile(
            "tap \"me/tools\"\nbrew \"wget\"\ncask \"firefox\"\nmas \"Xcode\", id: 1\n",
        );
        assert_eq!(entries.len(), 3);
        assert_eq!(skipped.len(), 1);
        let (entries, skipped) = parse_brewfile(
            "brew 'openssl@3' # note\ntap \"me/homebrew-tools\"\nbrew \"wget\", args: [\"HEAD\"]\nbrew \"wget\" if true\nbrew\"wget\"\ncask \"../outside\"\ntap \"../foo\"\nbrew 'wget\"\n",
        );
        assert_eq!(
            entries,
            [
                BrewfileEntry::Brew("openssl@3".into()),
                BrewfileEntry::Tap("me/tools".into())
            ]
        );
        assert_eq!(
            skipped.len(),
            6,
            "unsupported syntax is never partially executed"
        );
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod install_tests {
    use super::*;

    fn cache_bundle(
        prefix: &Prefix,
        tag: &str,
        entries: &[(&str, u8, &str, &[u8], u32)],
    ) -> String {
        prefix.ensure().unwrap();
        let archive =
            crate::extract::fixtures::gzip_bytes(&crate::extract::fixtures::tar_bytes(entries));
        let hash = fetch::sha256_bytes(&archive);
        std::fs::write(
            prefix.cache().join(format!("homebrew-brew-{tag}.tar.gz")),
            archive,
        )
        .unwrap();
        hash
    }

    fn run_bundle(prefix: &Prefix) -> Vec<u8> {
        let output = aterm_uds::spawnfd::command(prefix.homebrew_bundle().join("bin/brew"))
            .output()
            .expect("the installed executable starts");
        assert!(output.status.success(), "{output:?}");
        output.stdout
    }

    #[test]
    fn bundle_publishes_complete_receipts_and_retains_the_previous_executable_on_refusal() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        let outside = temp.path().join("owner.pc");
        std::fs::write(&outside, b"keep").unwrap();
        let first = cache_bundle(
            &prefix,
            "first",
            &[
                (
                    "pax_global_header",
                    b'g',
                    "",
                    b"52 comment=0000000000000000000000000000000000000000\n",
                    0o644,
                ),
                (
                    "brew-first/bin/brew",
                    b'0',
                    "",
                    b"#!/bin/sh\nprintf first",
                    0o755,
                ),
                (
                    "brew-first/pkgconfig/fuse/demo.pc",
                    b'2',
                    outside.to_str().unwrap(),
                    b"",
                    0o777,
                ),
            ],
        );
        install_bundle(&prefix, Some("first"), Some(&first), false).unwrap();
        assert_eq!(run_bundle(&prefix), b"first");
        assert_eq!(
            std::fs::read_link(prefix.homebrew_bundle().join("pkgconfig/fuse/demo.pc")).unwrap(),
            outside
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        assert!(
            bundle_record(&prefix.homebrew_bundle())
                .unwrap()
                .contains(&first)
        );

        let bad = cache_bundle(
            &prefix,
            "bad",
            &[("brew-bad/not-brew", b'0', "", b"incomplete", 0o644)],
        );
        assert!(install_bundle(&prefix, Some("bad"), Some(&bad), true).is_err());
        assert_eq!(run_bundle(&prefix), b"first");
        let malicious = cache_bundle(
            &prefix,
            "escape",
            &[("../outside", b'0', "", b"escape", 0o644)],
        );
        assert!(install_bundle(&prefix, Some("escape"), Some(&malicious), true).is_err());
        assert!(!prefix.root.join("outside").exists());
        assert_eq!(run_bundle(&prefix), b"first");

        let unexecutable = cache_bundle(
            &prefix,
            "unexecutable",
            &[(
                "brew-unexecutable/bin/brew",
                b'0',
                "",
                b"not executable",
                0o644,
            )],
        );
        assert!(install_bundle(&prefix, Some("unexecutable"), Some(&unexecutable), true).is_err());
        assert_eq!(run_bundle(&prefix), b"first");

        let next = cache_bundle(
            &prefix,
            "next",
            &[(
                "brew-next/bin/brew",
                b'0',
                "",
                b"#!/bin/sh\nprintf next",
                0o755,
            )],
        );
        install_bundle(&prefix, Some("next"), Some(&next), true).unwrap();
        assert_eq!(run_bundle(&prefix), b"next");
        assert!(
            bundle_record(&prefix.homebrew_bundle())
                .unwrap()
                .contains(&next)
        );
        assert!(
            !prefix.root.join("homebrew-bundle.json").exists(),
            "one embedded receipt"
        );
        bundle_remove(&prefix).unwrap();
        assert!(!prefix.homebrew_bundle().exists());
    }

    #[test]
    fn first_publication_refuses_an_empty_foreign_destination() {
        use std::os::unix::fs::MetadataExt as _;
        let temp = aterm_tempfile::tempdir().unwrap();
        let staged = temp.path().join("staged");
        let dest = temp.path().join("dest");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("new"), b"new").unwrap();
        std::fs::create_dir(&dest).unwrap();
        let original = std::fs::metadata(&dest).unwrap().ino();
        assert!(publish_directory(&staged, &dest, false).is_err());
        assert_eq!(std::fs::metadata(&dest).unwrap().ino(), original);
        assert!(!dest.join("new").exists());
        assert_eq!(std::fs::read(staged.join("new")).unwrap(), b"new");
    }

    #[test]
    fn bundle_never_removes_foreign_trees_or_follows_receipt_links() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        let directory = prefix.homebrew_bundle();
        std::fs::create_dir(&directory).unwrap();
        let foreign = directory.join("owner-work");
        std::fs::write(&foreign, b"keep").unwrap();
        let expected = "0".repeat(64);
        assert!(install_bundle(&prefix, Some("new"), Some(&expected), true).is_err());
        assert!(bundle_remove(&prefix).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        let record = temp.path().join("foreign-record");
        std::fs::write(
            &record,
            format!(r#"{{"installed_by":"abrew","version":"old","sha256":"{expected}"}}"#),
        )
        .unwrap();
        std::os::unix::fs::symlink(&record, directory.join(BUNDLE_RECEIPT)).unwrap();
        assert!(bundle_record(&directory).is_none());
        assert!(bundle_remove(&prefix).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        assert!(std::fs::read(&record).unwrap().starts_with(b"{"));
    }
}
