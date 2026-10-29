// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native `Brewfile` handling and explicit cleanup of legacy Homebrew bundles.

use std::path::Path;

use super::install::InstallError;
use super::prefix::{Kind, Prefix};

/// The embedded ownership receipt written by older abrew versions.
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

/// Inspect a legacy Homebrew bundle without executing or changing it.
pub fn bundle_status(prefix: &Prefix) {
    let directory = prefix.homebrew_bundle();
    if let Some(record) = bundle_record(&directory) {
        if !runnable_bundle(&directory) {
            println!(
                "legacy bundled Homebrew: invalid executable at {} (unused; remove with: abrew bundle remove)",
                directory.display()
            );
        } else {
            println!(
                "legacy bundled Homebrew: {} (unused; remove with: abrew bundle remove)",
                directory.join("bin/brew").display()
            );
        }
        print!("{record}");
    } else if directory.symlink_metadata().is_ok() {
        println!(
            "legacy bundled Homebrew: unowned path {} (left untouched)",
            directory.display()
        );
    } else {
        println!("legacy bundled Homebrew: none");
    }
}

/// Explicitly remove only a legacy bundle carrying abrew's embedded ownership receipt.
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
    println!("abrew: legacy Homebrew bundle removed");
    Ok(())
}

/// `abrew bundle dump`.
#[must_use]
pub fn brewfile_dump(prefix: &Prefix) -> String {
    let mut out = String::new();
    for t in super::tap::list(prefix) {
        out.push_str(&format!("tap \"{}\"\n", t.name()));
    }
    for i in prefix.installed(Kind::Formula) {
        let keg = prefix.keg(&i.name, &i.version);
        if super::prefix::receipt_word(&keg, "installed_on_request").as_deref() == Some("false") {
            continue;
        }
        out.push_str(&format!("brew \"{}\"\n", prefix.specified_name(&i)));
    }
    for i in prefix.installed(Kind::Cask) {
        out.push_str(&format!("cask \"{}\"\n", prefix.specified_name(&i)));
    }
    out
}

/// Normalize an official core formula or cask operand in a Brewfile (`name`,
/// `homebrew/[homebrew-]core/name` for `brew`, or `homebrew/[homebrew-]cask/name`
/// for `cask`). Third-party tap packages require tap recipes and are not native.
fn parse_official_package_entry(key: &str, name: &str) -> Option<String> {
    if super::valid_component(name) {
        return Some(name.to_owned());
    }
    let (tap_part, pkg) = name.rsplit_once('/')?;
    if !super::valid_component(pkg) {
        return None;
    }
    let (user, repo) = super::tap::parse_tap_name(tap_part)?;
    match (key, user.as_str(), repo.as_str()) {
        ("brew", "homebrew", "core") | ("cask", "homebrew", "cask") => Some(pkg.to_owned()),
        _ => None,
    }
}

fn normalize_qualified_name(full: &str) -> Option<String> {
    let (tap_part, pkg) = full.rsplit_once('/')?;
    if !super::valid_component(pkg) {
        return None;
    }
    let (user, repo) = super::tap::parse_tap_name(tap_part)?;
    Some(format!("{user}/{repo}/{pkg}"))
}

/// Whether `entry` is already satisfied by `prefix`.
#[must_use]
pub fn entry_installed(prefix: &Prefix, entry: &BrewfileEntry) -> bool {
    match entry {
        BrewfileEntry::Tap(n) => super::tap::parse_tap_name(n)
            .map(|(u, r)| format!("{u}/{r}"))
            .is_some_and(|want| super::tap::list(prefix).iter().any(|t| t.name() == want)),
        BrewfileEntry::Brew(n) => package_installed(prefix, Kind::Formula, n, false),
        BrewfileEntry::Cask(n) => package_installed(prefix, Kind::Cask, n, true),
    }
}

fn package_installed(prefix: &Prefix, kind: Kind, requested: &str, canonical_cask: bool) -> bool {
    let (key, official_tap) = match kind {
        Kind::Formula => ("brew", "homebrew/core"),
        Kind::Cask => ("cask", "homebrew/cask"),
    };
    let Some(raw_short) = parse_official_package_entry(key, requested) else {
        return false;
    };
    let short = if canonical_cask {
        super::api::canonical_cask_token(&raw_short)
    } else {
        raw_short.as_str()
    };
    let Some(inst) = prefix.installed_one(kind, short) else {
        return false;
    };
    let want = format!("{official_tap}/{short}");
    inst.versions.iter().any(|version| {
        let one = super::prefix::Installed {
            name: short.to_owned(),
            version: version.clone(),
            kind,
            versions: Vec::new(),
        };
        normalize_qualified_name(&prefix.full_name(&one)).as_deref() == Some(want.as_str())
    })
}

/// One Brewfile line's instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrewfileEntry {
    Tap(String),
    Brew(String),
    Cask(String),
}

/// Parse a Brewfile's `tap`/`brew`/`cask "name"` lines.
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
            if key == "tap" {
                let (user, repo) = super::tap::parse_tap_name(name)?;
                Some(format!("{user}/{repo}"))
            } else {
                parse_official_package_entry(key, name)
            }
        };
        if let Some(tap) = quoted("tap") {
            entries.push(BrewfileEntry::Tap(tap));
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
    std::fs::read_to_string(&p).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound && path.is_none() {
            "Error: No Brewfile found".to_string()
        } else {
            format!("abrew: cannot read {}: {e}", p.display())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn dump_keeps_on_request_and_qualified_names_only() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let mk = |name: &str, version: &str, fields: &[(&str, &str)]| {
            let dir = p.keg(name, version);
            std::fs::create_dir_all(&dir).unwrap();
            let mut all: Vec<(&str, &str)> = vec![
                ("installed_by", "abrew"),
                ("name", name),
                ("version", version),
            ];
            all.extend_from_slice(fields);
            p.write_receipt(&dir, &all).unwrap();
        };
        mk("wget", "1.25", &[("installed_on_request", "true")]);
        mk("dep", "2.0", &[("installed_on_request", "false")]);
        mk(
            "taptool",
            "1.0",
            &[
                ("installed_on_request", "true"),
                ("full_name", "someone/tap/taptool"),
            ],
        );
        mk(
            "badreceipt",
            "1.0",
            &[
                ("installed_on_request", "true"),
                ("full_name", "evil\" \ntap \"bad/repo"),
            ],
        );
        mk("legacy", "0.9", &[]);
        let text = brewfile_dump(&p);
        assert!(text.contains("brew \"wget\"\n"), "{text}");
        assert!(text.contains("brew \"someone/tap/taptool\"\n"), "{text}");
        assert!(text.contains("brew \"badreceipt\"\n"), "{text}");
        assert!(!text.contains("evil"), "{text}");
        assert!(text.contains("brew \"legacy\"\n"), "{text}");
        assert!(!text.contains("dep"), "dependency leaked into dump: {text}");

        let (entries, skipped) = parse_brewfile(&text);
        assert_eq!(
            skipped,
            ["brew \"someone/tap/taptool\""],
            "third-party tap formulae remain unsupported rows: {skipped:?}"
        );
        for entry in &entries {
            assert!(
                entry_installed(&p, entry),
                "dumped core entry {entry:?} must be recognized as installed"
            );
        }
        assert!(
            !entry_installed(&p, &BrewfileEntry::Brew("taptool".into())),
            "third-party tap keg must not satisfy a core formula entry"
        );
        assert!(entry_installed(
            &p,
            &BrewfileEntry::Brew("homebrew/core/wget".into())
        ));
    }

    #[test]
    fn brewfile_parse() {
        let (entries, skipped) = parse_brewfile(
            "tap \"me/tools\"\nbrew \"wget\"\nbrew \"homebrew/homebrew-core/curl\"\ncask \"homebrew/cask/firefox\"\nbrew \"someone/homebrew-tap/taptool\"\nmas \"Xcode\", id: 1\n",
        );
        assert_eq!(
            entries,
            [
                BrewfileEntry::Tap("me/tools".into()),
                BrewfileEntry::Brew("wget".into()),
                BrewfileEntry::Brew("curl".into()),
                BrewfileEntry::Cask("firefox".into()),
            ]
        );
        assert_eq!(skipped.len(), 2);
        let (entries, skipped) = parse_brewfile(
            "brew 'openssl@3' # note\ntap \"me/homebrew-tools\"\nbrew \"wget\", args: [\"HEAD\"]\nbrew \"wget\" if true\nbrew\"wget\"\ncask \"../outside\"\nbrew \"two/parts\"\ncask \"homebrew/core/firefox\"\ntap \"../foo\"\nbrew 'wget\"\n",
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
            8,
            "unsupported syntax is never partially executed"
        );
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod legacy_tests {
    use super::*;

    fn legacy_receipt(directory: &Path) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(
            directory.join(BUNDLE_RECEIPT),
            format!(
                r#"{{"installed_by":"abrew","version":"old","sha256":"{}"}}"#,
                "0".repeat(64)
            ),
        )
        .unwrap();
    }

    #[test]
    fn legacy_status_preserves_state_until_explicit_owned_removal() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        prefix.ensure().unwrap();
        let directory = prefix.homebrew_bundle();
        legacy_receipt(&directory);
        let legacy_data = directory.join("legacy-data");
        std::fs::write(&legacy_data, b"legacy").unwrap();
        let owner_data = prefix.root.join("owner-work");
        std::fs::write(&owner_data, b"keep").unwrap();

        assert!(bundle_record(&directory).is_some());
        assert!(
            !runnable_bundle(&directory),
            "a broken legacy bundle is removable"
        );
        bundle_status(&prefix);
        assert_eq!(std::fs::read(&legacy_data).unwrap(), b"legacy");
        bundle_remove(&prefix).unwrap();
        assert!(!directory.exists());
        assert_eq!(std::fs::read(&owner_data).unwrap(), b"keep");
        bundle_remove(&prefix).unwrap();
    }

    #[test]
    fn bundle_never_removes_foreign_trees_or_follows_receipt_links() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        let directory = prefix.homebrew_bundle();
        std::fs::create_dir(&directory).unwrap();
        let foreign = directory.join("owner-work");
        std::fs::write(&foreign, b"keep").unwrap();
        assert!(bundle_remove(&prefix).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        let record = temp.path().join("foreign-record");
        std::fs::write(
            &record,
            format!(
                r#"{{"installed_by":"abrew","version":"old","sha256":"{}"}}"#,
                "0".repeat(64)
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(&record, directory.join(BUNDLE_RECEIPT)).unwrap();
        assert!(bundle_record(&directory).is_none());
        assert!(bundle_remove(&prefix).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        assert!(std::fs::read(&record).unwrap().starts_with(b"{"));
    }

    #[test]
    fn legacy_removal_does_not_follow_a_linked_bundle_directory() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&temp.path().join("prefix"));
        prefix.ensure().unwrap();
        let outside = temp.path().join("outside");
        legacy_receipt(&outside);
        let owner_data = outside.join("owner-work");
        std::fs::write(&owner_data, b"keep").unwrap();
        let directory = prefix.homebrew_bundle();
        std::os::unix::fs::symlink(&outside, &directory).unwrap();

        assert!(bundle_record(&directory).is_none());
        assert!(bundle_remove(&prefix).is_err());
        assert_eq!(std::fs::read(&owner_data).unwrap(), b"keep");
        assert!(
            directory
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
