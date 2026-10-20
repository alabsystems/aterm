// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Native casks install App/Binary artifacts from bounded zip/tar payloads.
//! Other artifacts, disk images and replacements need Homebrew's full recipe
//! and pass through before any native installation changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::fetch;
use super::formula::{Artifact, Cask};
use super::install::{
    InstallError, InstallLink, check_directory, link_target_points_into, publish_payload,
    same_path_identity,
};
use super::prefix::{Kind, Prefix, owned_receipt};

/// The pre-lock delegation check for one cask: every PassThrough the
/// install itself would raise before downloading.
pub(super) fn preflight(cask: &Cask) -> Result<(), InstallError> {
    if cask.disabled {
        return Err(InstallError::PassThrough(format!(
            "cask {} is disabled upstream",
            cask.token
        )));
    }
    if cask.artifacts.iter().any(|a| matches!(a, Artifact::Pkg(_))) {
        return Err(InstallError::PassThrough(format!(
            "cask {} ships a pkg installer (root work abrew never does itself)",
            cask.token
        )));
    }
    if !fetch::archive_supported(&download_name(cask)) || cask.artifacts.is_empty() {
        return Err(InstallError::PassThrough(format!(
            "cask {} needs its Homebrew extraction recipe",
            cask.token
        )));
    }
    Ok(())
}

/// Install one cask. Unsupported recipes refuse before downloading or placing
/// artifacts. A missing `no_check` cache entry still has to be downloaded.
pub fn install(
    prefix: &Prefix,
    cask: &Cask,
    dry_run: bool,
    force: bool,
) -> Result<String, InstallError> {
    if cask.disabled {
        return Err(InstallError::PassThrough(format!(
            "cask {} is disabled upstream",
            cask.token
        )));
    }
    if cask.deprecated {
        eprintln!("abrew: warning: cask {} is deprecated upstream", cask.token);
    }
    if !super::valid_component(&cask.token) || !super::valid_component(&cask.version) {
        return Err(InstallError::PassThrough(
            "unsupported cask token/version".to_string(),
        ));
    }
    if let Some(inst) = prefix.installed_one(Kind::Cask, &cask.token) {
        if force {
            return Err(InstallError::PassThrough(format!(
                "replacement of cask {} needs brew",
                cask.token
            )));
        }
        println!("abrew: {} {} already installed", cask.token, inst.version);
        return Ok(inst.version);
    }
    let file = download_name(cask);
    if !fetch::archive_supported(&file) || cask.artifacts.is_empty() {
        return Err(InstallError::PassThrough(format!(
            "cask {} needs its Homebrew extraction recipe",
            cask.token
        )));
    }
    let artifacts = artifact_paths(prefix, cask)?;
    let staged = prefix.cask_dir(&cask.token, &cask.version);
    if std::fs::symlink_metadata(&staged).is_ok() {
        return Err(InstallError::Failed(format!(
            "abrew: unrecognized cask directory {}",
            staged.display()
        )));
    }
    for (_, dst, _) in &artifacts {
        if std::fs::symlink_metadata(dst).is_ok() {
            return Err(InstallError::Failed(format!(
                "abrew: refusing existing artifact {}",
                dst.display()
            )));
        }
    }
    if dry_run {
        println!("Would install cask {} {}", cask.token, cask.version);
        return Ok(cask.version.clone());
    }
    prefix.ensure()?;
    let dest = prefix.cache().join(file);
    let expected = cask.sha256.as_deref().unwrap_or("no_check");
    // A no_check payload is bound to neither version nor URL, so its
    // cache entry is never reused: always re-download, never receipt
    // stale bytes under the current version.
    let reusable = cask.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
    if !reusable {
        println!("==> Fetching cask {} ({})", cask.token, cask.version);
        fetch::curl_to_file(&cask.url, &dest, &[])?;
    }
    fetch::verify_sha256(&dest, expected)?;
    let work =
        aterm_tempfile::TempDir::new_in(prefix.caskroom()).map_err(|e| format!("abrew: {e}"))?;
    let payload = work.path().join("payload");
    fetch::untar(&dest, &payload).map_err(|e| {
        if e.contains("unsupported archive") {
            InstallError::PassThrough(format!(
                "cask {} container format needs Homebrew extraction",
                cask.token
            ))
        } else {
            InstallError::Failed(e)
        }
    })?;
    for (relative, _, app) in &artifacts {
        let src = payload.join(relative);
        let valid = if *app { src.is_dir() } else { src.is_file() };
        #[cfg(unix)]
        let valid = {
            use std::os::unix::fs::PermissionsExt;
            valid
                && (*app
                    || src
                        .metadata()
                        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
        };
        if !valid {
            return Err(InstallError::Failed(format!(
                "abrew: cask {} has no artifact {}",
                cask.token,
                relative.display()
            )));
        }
    }
    let links: Vec<InstallLink> = artifacts
        .iter()
        .map(|(relative, destination, directory)| InstallLink {
            source: staged.join(relative),
            destination: destination.clone(),
            directory: *directory,
        })
        .collect();
    // The receipt records exactly which destinations this install
    // placed; uninstall removes those (identity-checked), never a
    // name-based survey of a shared directory.
    let artifact_fields: Vec<(String, String)> = links
        .iter()
        .enumerate()
        .map(|(i, l)| {
            (
                format!("artifact_{i}"),
                l.destination.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let mut receipt_fields: Vec<(&str, &str)> = vec![
        ("token", &cask.token),
        ("version", &cask.version),
        ("installed_by", "abrew"),
    ];
    for (k, v) in &artifact_fields {
        receipt_fields.push((k, v));
    }
    prefix.write_receipt(&payload, &receipt_fields)?;
    publish_payload(prefix, &payload, &staged, &links)?;
    prefix.rebuild_state()?;
    Ok(cask.version.clone())
}

fn download_name(cask: &Cask) -> String {
    let base = cask
        .url
        .split('?')
        .next()
        .unwrap_or(&cask.url)
        .rsplit('/')
        .next()
        .unwrap_or("download");
    format!(
        "{}-{}",
        cask.token,
        base.replace("#{version}", &cask.version)
    )
}

fn artifact_paths(
    prefix: &Prefix,
    cask: &Cask,
) -> Result<Vec<(PathBuf, PathBuf, bool)>, InstallError> {
    let mut paths = Vec::new();
    let mut destinations = BTreeSet::new();
    for artifact in &cask.artifacts {
        let (name, dir, app) = match artifact {
            Artifact::App(name) => (name, prefix.applications(), true),
            Artifact::Binary(name) => (name, prefix.bin(), false),
            _ => {
                return Err(InstallError::PassThrough(format!(
                    "cask {} has an unsupported artifact recipe",
                    cask.token
                )));
            }
        };
        let path = Path::new(name);
        if name.is_empty()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(InstallError::PassThrough(
                "cask artifact needs a Homebrew path mapping".to_string(),
            ));
        }
        let base = path
            .file_name()
            .ok_or_else(|| "abrew: empty cask artifact".to_string())?;
        let dst = dir.join(base);
        if !destinations.insert(dst.clone()) {
            return Err(InstallError::PassThrough(
                "cask has overlapping artifact destinations".to_string(),
            ));
        }
        paths.push((path.to_path_buf(), dst, app));
    }
    Ok(paths)
}

/// Remove links into receipt-admitted versions and only those payloads.
/// Unknown sibling versions and user data (`zap`) remain untouched.
pub fn uninstall(prefix: &Prefix, token: &str, dry_run: bool) -> Result<(), InstallError> {
    uninstall_from(
        prefix,
        token,
        dry_run,
        &[
            prefix.applications(),
            prefix.bin(),
            PathBuf::from("/Applications"),
        ],
    )
}

// The production directory roster is fixed above. Captures supply only their own
// directories, so the preservation regression never enumerates global applications.
fn uninstall_from(
    prefix: &Prefix,
    token: &str,
    dry_run: bool,
    directories: &[PathBuf],
) -> Result<(), InstallError> {
    let Some(inst) = prefix.installed_one(Kind::Cask, token) else {
        return Err(InstallError::Failed(format!(
            "abrew: cask {token} is not installed"
        )));
    };
    if dry_run {
        println!("Would uninstall cask {token}");
        return Ok(());
    }
    UninstallPlan::prepare(prefix, token, &inst.versions, directories)?.apply(prefix)
}

struct UninstallPlan {
    token: String,
    base: PathBuf,
    payloads: BTreeMap<String, std::fs::Metadata>,
    parents: BTreeMap<PathBuf, std::fs::Metadata>,
    links: BTreeMap<PathBuf, (PathBuf, std::fs::Metadata)>,
}

impl UninstallPlan {
    fn prepare(
        prefix: &Prefix,
        token: &str,
        versions: &[String],
        directories: &[PathBuf],
    ) -> Result<Self, InstallError> {
        let base = prefix.caskroom().join(token);
        check_directory(&base, &prefix.root)?;
        let mut plan = Self {
            token: token.to_string(),
            parents: BTreeMap::from([(
                base.clone(),
                std::fs::symlink_metadata(&base).map_err(|e| format!("abrew: {e}"))?,
            )]),
            base,
            payloads: BTreeMap::new(),
            links: BTreeMap::new(),
        };
        for version in versions {
            let payload = plan.base.join(version);
            check_directory(&payload, &prefix.root)?;
            let identity =
                std::fs::symlink_metadata(&payload).map_err(|e| format!("abrew: {e}"))?;
            plan.payloads.insert(version.clone(), identity);
            plan.check_payload(prefix, version)?;
        }
        // Survey every artifact directory before removing any entry or payload.
        // Only NotFound proves a directory absent; a planned pathname is not authority.
        for dir in directories {
            if dir.starts_with(&prefix.root) {
                check_directory(dir, &prefix.root)?;
            }
            let identity = match std::fs::symlink_metadata(dir) {
                Ok(m) if m.file_type().is_dir() && !crate::platform::is_reparse(&m) => m,
                Ok(_) => {
                    return Err(format!(
                        "abrew: refusing nonphysical artifact directory {}",
                        dir.display()
                    )
                    .into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(format!(
                        "abrew: cannot inspect artifact directory {}: {e}",
                        dir.display()
                    )
                    .into());
                }
            };
            plan.parents.insert(dir.clone(), identity);
            for entry in std::fs::read_dir(dir).map_err(|e| format!("abrew: {e}"))? {
                let entry = entry.map_err(|e| format!("abrew: {e}"))?;
                let path = entry.path();
                let identity =
                    std::fs::symlink_metadata(&path).map_err(|e| format!("abrew: {e}"))?;
                if !identity.file_type().is_symlink() {
                    continue;
                }
                let target = std::fs::read_link(&path).map_err(|e| format!("abrew: {e}"))?;
                if plan.payloads.keys().any(|version| {
                    link_target_points_into(&path, &target, &plan.base.join(version))
                }) {
                    plan.links.insert(path, (target, identity));
                }
            }
        }
        // Narrow the survey to destinations the install receipt
        // recorded, when it recorded any (receipts from before the
        // field existed keep the identity-checked survey).
        let recorded: std::collections::BTreeSet<PathBuf> = plan
            .payloads
            .keys()
            .filter_map(|version| {
                let bytes = crate::metadata_io::read_bounded_regular(
                    &plan.base.join(version).join("INSTALL_RECEIPT.json"),
                    64 * 1024,
                )
                .ok()?;
                let value: aterm_json::Value = aterm_json::from_slice(&bytes).ok()?;
                let aterm_json::Value::Object(map) = value else {
                    return None;
                };
                Some(
                    map.iter()
                        .filter(|(k, _)| k.starts_with("artifact_"))
                        .filter_map(|(_, v)| v.as_str().map(PathBuf::from))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        if !recorded.is_empty() {
            plan.links.retain(|path, _| recorded.contains(path));
        }
        Ok(plan)
    }

    fn check_parent(&self, prefix: &Prefix, parent: &Path) -> Result<(), InstallError> {
        if parent.starts_with(&prefix.root) {
            check_directory(parent, &prefix.root)?;
        }
        if self
            .parents
            .get(parent)
            .is_some_and(|identity| same_path_identity(parent, identity))
        {
            Ok(())
        } else {
            Err(format!(
                "abrew: cask directory changed before removal: {}",
                parent.display()
            )
            .into())
        }
    }

    fn check_link(&self, prefix: &Prefix, path: &Path) -> Result<(), InstallError> {
        let parent = path
            .parent()
            .ok_or_else(|| "abrew: artifact has no parent".to_string())?;
        self.check_parent(prefix, parent)?;
        if self.links.get(path).is_some_and(|(target, identity)| {
            same_path_identity(path, identity)
                && std::fs::read_link(path).is_ok_and(|actual| &actual == target)
        }) {
            Ok(())
        } else {
            Err(format!(
                "abrew: cask link changed before removal: {}",
                path.display()
            )
            .into())
        }
    }

    fn check_payload(&self, prefix: &Prefix, version: &str) -> Result<(), InstallError> {
        self.check_parent(prefix, &self.base)?;
        let payload = self.base.join(version);
        check_directory(&payload, &prefix.root)?;
        if self
            .payloads
            .get(version)
            .is_some_and(|identity| same_path_identity(&payload, identity))
            && owned_receipt(&payload, Kind::Cask, &self.token, version)
        {
            Ok(())
        } else {
            Err(format!(
                "abrew: cask payload changed before removal: {}",
                payload.display()
            )
            .into())
        }
    }

    fn apply(self, prefix: &Prefix) -> Result<(), InstallError> {
        // Admit the whole plan first, then recheck each entry immediately before
        // removal. Independent filesystem writers are not an atomic transaction.
        for parent in self.parents.keys() {
            self.check_parent(prefix, parent)?;
        }
        for version in self.payloads.keys() {
            self.check_payload(prefix, version)?;
        }
        for link in self.links.keys() {
            self.check_link(prefix, link)?;
        }
        for link in self.links.keys() {
            self.check_link(prefix, link)?;
            std::fs::remove_file(link).map_err(|e| format!("abrew: {e}"))?;
        }
        for version in self.payloads.keys() {
            self.check_payload(prefix, version)?;
            std::fs::remove_dir_all(self.base.join(version)).map_err(|e| format!("abrew: {e}"))?;
        }
        self.check_parent(prefix, &self.base)?;
        let _ = std::fs::remove_dir(&self.base);
        prefix.rebuild_state()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::extract::fixtures::{ZipMember, zip_bytes};

    fn cask(url: String) -> Cask {
        Cask {
            token: "demo".to_string(),
            version: "1".to_string(),
            url,
            artifacts: vec![
                Artifact::Binary("tool".to_string()),
                Artifact::App("Demo.app".to_string()),
            ],
            ..Cask::default()
        }
    }

    #[test]
    #[cfg(unix)]
    fn no_check_download_installs_real_artifacts_and_uninstall_preserves_unknown_versions() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let archive = tmp.path().join("demo.zip");
        std::fs::write(
            &archive,
            zip_bytes(
                &[
                    ZipMember {
                        name: "tool",
                        mode: 0o100755,
                        data: b"#!/bin/sh\nprintf 'cask ready\\n'\n",
                        deflate: false,
                    },
                    ZipMember {
                        name: "Demo.app/Contents/Info.plist",
                        mode: 0o100644,
                        data: b"app",
                        deflate: false,
                    },
                ],
                false,
            ),
        )
        .unwrap();
        let cask = cask(format!("file://{}", archive.display()));
        assert!(cask.sha256.is_none());
        assert!(!prefix.cache().join(download_name(&cask)).exists());
        assert_eq!(install(&prefix, &cask, false, false).unwrap(), "1");
        let output = aterm_uds::spawnfd::command(prefix.bin().join("tool"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"cask ready\n");
        assert_eq!(
            std::fs::read(prefix.applications().join("Demo.app/Contents/Info.plist")).unwrap(),
            b"app"
        );
        assert!(matches!(
            install(&prefix, &cask, false, true),
            Err(InstallError::PassThrough(_))
        ));
        let unknown = prefix.cask_dir("demo", "personal");
        std::fs::create_dir_all(&unknown).unwrap();
        std::fs::write(unknown.join("notes"), b"keep").unwrap();
        super::super::prefix::symlink_file(&unknown.join("notes"), &prefix.bin().join("personal"))
            .unwrap();
        uninstall(&prefix, "demo", false).unwrap();
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
        assert!(std::fs::symlink_metadata(prefix.applications().join("Demo.app")).is_err());
        assert_eq!(
            std::fs::read(prefix.bin().join("personal")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn unsupported_casks_refuse_before_native_changes() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        for artifact in [
            Artifact::Pkg("x.pkg".to_string()),
            Artifact::Font("x.ttf".to_string()),
            Artifact::Manpage("x.1".to_string()),
            Artifact::Other("uninstall:x".to_string()),
        ] {
            let mut cask = cask("unused.zip".to_string());
            cask.artifacts.push(artifact);
            assert!(matches!(
                install(&prefix, &cask, false, false),
                Err(InstallError::PassThrough(_))
            ));
            assert!(!prefix.root.exists());
        }
        let cask = cask("unused.dmg".to_string());
        assert!(matches!(
            install(&prefix, &cask, false, false),
            Err(InstallError::PassThrough(_))
        ));
        assert!(!prefix.root.exists());
    }

    #[test]
    #[cfg(unix)]
    fn a_missing_artifact_leaves_no_receipt_or_links() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        let archive = tmp.path().join("demo.zip");
        std::fs::write(
            &archive,
            zip_bytes(
                &[ZipMember {
                    name: "other",
                    mode: 0o100644,
                    data: b"not the promised tool",
                    deflate: false,
                }],
                false,
            ),
        )
        .unwrap();
        let cask = cask(format!("file://{}", archive.display()));
        assert!(install(&prefix, &cask, false, false).is_err());
        assert!(prefix.installed(Kind::Cask).is_empty());
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
        // A promised Binary with no executable mode is not a successful install.
        std::fs::write(
            &archive,
            zip_bytes(
                &[
                    ZipMember {
                        name: "tool",
                        mode: 0o100644,
                        data: b"not executable",
                        deflate: false,
                    },
                    ZipMember {
                        name: "Demo.app/Contents/Info.plist",
                        mode: 0o100644,
                        data: b"app",
                        deflate: false,
                    },
                ],
                false,
            ),
        )
        .unwrap();
        std::fs::remove_file(prefix.cache().join(download_name(&cask))).unwrap();
        assert!(install(&prefix, &cask, false, false).is_err());
        assert!(prefix.installed(Kind::Cask).is_empty());
        assert!(!prefix.cask_dir("demo", "1").exists());
        assert!(std::fs::symlink_metadata(prefix.bin().join("tool")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn uninstall_rechecks_planned_links_parents_and_payloads() {
        use super::super::prefix::{symlink_dir, symlink_file};

        for change in [
            "foreign-file",
            "same-target-link",
            "physical-parent",
            "aliased-parent",
            "payload",
            "receipt",
        ] {
            let tmp = aterm_tempfile::TempDir::new().unwrap();
            let prefix = Prefix::at(&tmp.path().join("prefix"));
            prefix.ensure().unwrap();
            let payload = prefix.cask_dir("demo", "1");
            std::fs::create_dir_all(&payload).unwrap();
            std::fs::write(payload.join("tool"), b"owned payload").unwrap();
            prefix
                .write_receipt(
                    &payload,
                    &[
                        ("token", "demo"),
                        ("version", "1"),
                        ("installed_by", "abrew"),
                    ],
                )
                .unwrap();
            let earlier = prefix.applications().join("also-owned");
            symlink_file(&payload.join("tool"), &earlier).unwrap();
            // Stand in for /Applications without reading or changing the real directory.
            let applications = tmp.path().join("captured-applications");
            std::fs::create_dir(&applications).unwrap();
            let artifact = applications.join("tool");
            symlink_file(&payload.join("tool"), &artifact).unwrap();
            let plan = UninstallPlan::prepare(
                &prefix,
                "demo",
                &["1".to_string()],
                &[prefix.applications(), applications.clone()],
            )
            .unwrap();

            match change {
                "foreign-file" | "same-target-link" => {
                    // Keep the old inode alive, so this control cannot depend on reuse.
                    std::fs::rename(&artifact, tmp.path().join("held-link")).unwrap();
                    if change == "foreign-file" {
                        std::fs::write(&artifact, b"foreign artifact").unwrap();
                    } else {
                        symlink_file(&payload.join("tool"), &artifact).unwrap();
                    }
                }
                "physical-parent" | "aliased-parent" => {
                    std::fs::rename(&applications, tmp.path().join("held-parent")).unwrap();
                    if change == "physical-parent" {
                        std::fs::create_dir(&applications).unwrap();
                        std::fs::write(&artifact, b"foreign artifact").unwrap();
                    } else {
                        let outside = tmp.path().join("foreign-parent");
                        std::fs::create_dir(&outside).unwrap();
                        std::fs::write(outside.join("tool"), b"foreign artifact").unwrap();
                        symlink_dir(&outside, &applications).unwrap();
                    }
                }
                "payload" => {
                    let held = tmp.path().join("held-payload");
                    std::fs::rename(&payload, &held).unwrap();
                    std::fs::create_dir(&payload).unwrap();
                    std::fs::write(payload.join("tool"), b"foreign payload").unwrap();
                    // Even a copied valid receipt does not transfer the old inode's authority.
                    std::fs::copy(
                        held.join("INSTALL_RECEIPT.json"),
                        payload.join("INSTALL_RECEIPT.json"),
                    )
                    .unwrap();
                }
                "receipt" => {
                    std::fs::write(payload.join("INSTALL_RECEIPT.json"), b"foreign receipt")
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(plan.apply(&prefix).is_err(), "{change}");
            assert!(payload.is_dir(), "{change}");
            assert_eq!(
                std::fs::read_link(&earlier).unwrap(),
                payload.join("tool"),
                "{change}"
            );
            if matches!(
                change,
                "foreign-file" | "physical-parent" | "aliased-parent"
            ) {
                assert_eq!(
                    std::fs::read(&artifact).unwrap(),
                    b"foreign artifact",
                    "{change}"
                );
            } else {
                assert_eq!(
                    std::fs::read_link(&artifact).unwrap(),
                    payload.join("tool"),
                    "{change}"
                );
            }
            if change == "payload" {
                assert_eq!(
                    std::fs::read(payload.join("tool")).unwrap(),
                    b"foreign payload"
                );
            }
            if change == "receipt" {
                assert_eq!(
                    std::fs::read(payload.join("INSTALL_RECEIPT.json")).unwrap(),
                    b"foreign receipt"
                );
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn uninstall_refuses_aliased_artifact_directories_before_removing_anything() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let payload = prefix.cask_dir("demo", "1");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("tool"), b"owned payload").unwrap();
        prefix
            .write_receipt(
                &payload,
                &[
                    ("token", "demo"),
                    ("version", "1"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        let unknown = prefix.cask_dir("demo", "personal");
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("notes"), b"owner notes").unwrap();
        let artifact = prefix.applications().join("tool");
        super::super::prefix::symlink_file(&payload.join("tool"), &artifact).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let external_link = outside.join("keep");
        super::super::prefix::symlink_file(&payload.join("tool"), &external_link).unwrap();
        std::fs::write(outside.join("marker"), b"external bytes").unwrap();
        std::fs::remove_dir(prefix.bin()).unwrap();
        super::super::prefix::symlink_dir(&outside, &prefix.bin()).unwrap();
        let local_applications = tmp.path().join("captured-applications");
        std::fs::create_dir(&local_applications).unwrap();
        let directories = [prefix.applications(), prefix.bin(), local_applications];
        assert!(uninstall_from(&prefix, "demo", false, &directories).is_err());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
        assert_eq!(
            std::fs::read_link(&external_link).unwrap(),
            payload.join("tool")
        );
        assert_eq!(
            std::fs::read(outside.join("marker")).unwrap(),
            b"external bytes"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
        assert!(payload.join("INSTALL_RECEIPT.json").is_file());
        // A non-directory survey root also refuses before the earlier eligible link.
        std::fs::remove_file(prefix.bin()).unwrap();
        std::fs::write(prefix.bin(), b"foreign root file").unwrap();
        assert!(uninstall_from(&prefix, "demo", false, &directories).is_err());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"owned payload");
        assert_eq!(std::fs::read(prefix.bin()).unwrap(), b"foreign root file");
        // Ordinary physical roots remove only admitted payload links and versions.
        std::fs::remove_file(prefix.bin()).unwrap();
        std::fs::create_dir(prefix.bin()).unwrap();
        let foreign = prefix.bin().join("foreign");
        super::super::prefix::symlink_file(&outside.join("marker"), &foreign).unwrap();
        uninstall_from(&prefix, "demo", false, &directories).unwrap();
        assert!(std::fs::symlink_metadata(&artifact).is_err());
        assert!(!payload.exists());
        assert_eq!(
            std::fs::read(unknown.join("notes")).unwrap(),
            b"owner notes"
        );
        assert_eq!(std::fs::read(&foreign).unwrap(), b"external bytes");
        assert_eq!(
            std::fs::read_link(&external_link).unwrap(),
            payload.join("tool")
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 2);
    }
}
