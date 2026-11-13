// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preserve real package authority across the proof's private same-build handoff.

use std::io;
use std::path::{Path, PathBuf};

use atpkg::prerequisite::{HeldTool, Prerequisite};
use atpkg::signed_artifact_cache::{SignedArtifactCache, SignedArtifactKey};

use super::Stdio;

pub(super) struct Target {
    source: PathBuf,
    held: HeldTool,
    documents: Vec<(String, Vec<u8>)>,
    exposes: Vec<String>,
}

impl Target {
    pub(super) fn prepare(package: &Path) -> Result<Self, String> {
        let config = atpkg::config::cached();
        if config.unreadable {
            return Err("the package configuration is unreadable".into());
        }
        let layout = atpkg::store::resolve_from(config)
            .ok_or("the managed package prefix is unavailable")?;
        let package = package.canonicalize().map_err(|e| e.to_string())?;
        let build = atpkg::ops::list_installed_for(&layout, "codex")
            .into_iter()
            .find(|build| {
                layout
                    .build_dir("codex", *build)
                    .canonicalize()
                    .is_ok_and(|root| root == package)
            })
            .ok_or("the handoff target must be an installed signed Codex build")?;
        let source = layout.build_dir("codex", build);
        if !atpkg::store::build_is_complete(&source) {
            return Err("the installed handoff target is incomplete".into());
        }
        let held = atpkg::prerequisite::configured_tool(
            Prerequisite::Codex,
            build,
            "codex-alab proof: retain the original handoff payload while copying it",
        )?;
        if held.root().canonicalize().map_err(|e| e.to_string())? != package {
            return Err("the admitted handoff payload differs from the requested package".into());
        }

        let fetcher = atpkg::resolve_fetcher(&layout, None);
        let candidates = fetcher.index_candidates()?;
        let now = crate::util::now_secs();
        let anchor = atpkg::Anchor::pinned(atpkg::Floor::new(layout.roster_floor()).current());
        let floor = atpkg::BuildFloor {
            index_build: atpkg::Floor::new(layout.floor()).current(),
            roster_seq: atpkg::Floor::new(layout.floor_generation()).current(),
        };
        // This is a new private store's first contact. Its source snapshot carries the real
        // signatures and remains subject to the normal native admission after it is copied.
        let selected = atpkg::select_index(&anchor, candidates.clone(), floor, None, now)
            .selected
            .ok_or("no signed index can authorize the private handoff store")?;
        let until = aterm_types::rfc3339::parse_utc(&selected.index.valid_until)
            .ok_or("the signed index has no valid freshness deadline")?;
        atpkg::check_freshness(now, until)
            .map_err(|e| format!("the handoff index is not fresh: {e:?}"))?;
        let triple = aterm_toolchain::rustup::NATIVE_TRIPLE;
        let channel = selected
            .index
            .channel_for(atpkg::config::CHANNEL, triple)
            .ok_or("the signed stable channel is unavailable")?;
        if !atpkg::gate::current_build_ok(&channel, "codex", Some(build)) {
            return Err("the handoff target is withdrawn or below its signed floor".into());
        }
        let row = selected
            .index
            .program("codex")
            .ok_or("the signed index names no Codex package")?;
        let (raw, signature) = package_document(fetcher.as_ref(), &layout, &row.repo, build)?;
        let verified = selected
            .index
            .verify_pkg(raw.clone(), &signature)
            .map_err(|e| format!("the handoff package signature was refused: {e:?}"))?;
        let manifest = atpkg::parse_pkg(&verified)
            .map_err(|e| format!("the handoff package manifest was refused: {e:?}"))?;
        if !manifest.is_for("codex")
            || manifest.build_number != build
            || manifest.version != held.version()
            || !manifest
                .artifact_for(triple)
                .is_some_and(|artifact| artifact.tree_root == held.tree_root())
        {
            return Err("the signed handoff documents differ from the admitted payload".into());
        }
        let candidate = candidates
            .get(selected.candidate)
            .ok_or("the admitted index has no source document")?;
        let documents = vec![
            ("index.toml".into(), candidate.index_bytes.clone()),
            ("index.toml.sig".into(), candidate.sig.clone()),
            (
                aterm_update_core::roster::ROSTER_ASSET.into(),
                candidate.roster_bytes.clone(),
            ),
            (
                aterm_update_core::roster::ROSTER_SIG_ASSET.into(),
                candidate.roster_sig.clone(),
            ),
            (format!("pkg-codex-{build}.toml"), raw),
            (format!("pkg-codex-{build}.toml.sig"), signature),
        ];
        Ok(Self {
            source,
            held,
            documents,
            exposes: manifest.exposes,
        })
    }

    fn layout(home: &Path) -> atpkg::Layout {
        atpkg::Layout {
            prefix: atpkg::store::default_prefix(home),
        }
    }

    fn root(&self, home: &Path) -> PathBuf {
        Self::layout(home).build_dir("codex", self.held.build())
    }

    pub(super) fn matches(&self, home: &Path, package: &Path) -> Result<bool, String> {
        Ok(self.root(home).canonicalize().map_err(|e| e.to_string())?
            == package.canonicalize().map_err(|e| e.to_string())?)
    }

    pub(super) fn activate(&self, home: &Path) -> Result<(), String> {
        let layout = Self::layout(home);
        let root = self.root(home);
        atpkg::activate_build(&layout, &root).map_err(|e| e.to_string())?;
        let refused = atpkg::activate::install_shims_env(
            &layout,
            &root,
            &self.exposes,
            atpkg::Aliases::Off,
            self.held.environment(),
        )
        .map_err(|e| e.to_string())?;
        if !refused.is_empty() {
            return Err(format!(
                "the signed handoff exposes invalid commands: {refused:?}"
            ));
        }
        Ok(())
    }

    pub(super) fn linked(&self, home: &Path) -> Result<bool, String> {
        atpkg::linked_checkout_checked(&Self::layout(home), "codex")
            .map(|checkout| checkout.is_some())
            .map_err(|e| e.to_string())
    }

    pub(super) fn stage(&self, scratch: &Path, home: &Path) -> Result<PathBuf, String> {
        self.held.validate()?;
        let root = self.root(home);
        let parent = root.parent().ok_or("the private build has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        clone_package(self.held.root(), &root)?;
        copy_records(&self.source, &root).map_err(|e| e.to_string())?;
        self.held.validate()?;
        if !atpkg::store::build_is_complete(&root) {
            return Err("the private handoff copy is incomplete".into());
        }
        let registry = scratch.join("signed-registry");
        std::fs::create_dir(&registry).map_err(|e| e.to_string())?;
        for (name, bytes) in &self.documents {
            crate::util::write_fresh(&registry.join(name), bytes).map_err(|e| e.to_string())?;
        }
        std::os::unix::fs::symlink(&root, scratch.join("pkg-b")).map_err(|e| e.to_string())?;
        // Keep B's installed selection available while the ordinary development link selects
        // A. The proof rebuilds B's managed invocation paths after unlinking A.
        self.activate(home)?;
        Ok(registry)
    }
}

fn package_document(
    fetcher: &dyn atpkg::Fetcher,
    layout: &atpkg::Layout,
    repo: &str,
    build: u64,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    if let Some(source) = fetcher.package_source_id(repo, "codex") {
        let cache = SignedArtifactCache::for_layout(layout);
        let key = SignedArtifactKey::package(&source, repo, "codex", build);
        if let Some(pair) = cache.load(
            &key,
            usize::try_from(atpkg::net::MANIFEST_CAP).unwrap_or(usize::MAX),
            usize::try_from(atpkg::net::SIG_CAP).unwrap_or(usize::MAX),
        ) {
            return Ok(pair);
        }
    }
    fetcher.pkg_manifest(repo, "codex", build)
}

pub(super) fn clone_package(from: &Path, to: &Path) -> Result<(), String> {
    let copied = aterm_uds::spawnfd::command("/bin/cp")
        .arg("-cR")
        .arg(from)
        .arg(to)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if copied.success() {
        Ok(())
    } else {
        Err(format!(
            "cannot copy {} to {}: {copied}",
            from.display(),
            to.display()
        ))
    }
}

fn copy_records(source: &Path, target: &Path) -> io::Result<()> {
    let source_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("the source build has no UTF-8 name"))?;
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("the target build has no UTF-8 name"))?;
    let source_ready = atpkg::store::ready_marker_path(source)
        .ok_or_else(|| io::Error::other("the source has no readiness path"))?;
    let target_ready = atpkg::store::ready_marker_path(target)
        .ok_or_else(|| io::Error::other("the target has no readiness path"))?;
    copy_record(&source_ready, &target_ready)?;
    for suffix in atpkg::store::STAGE_SIDECAR_SUFFIXES {
        copy_record(
            &source.with_file_name(format!("{source_name}{suffix}")),
            &target.with_file_name(format!("{target_name}{suffix}")),
        )?;
    }
    Ok(())
}

fn copy_record(source: &Path, target: &Path) -> io::Result<()> {
    match crate::util::read_artifact(source) {
        Ok(bytes) => crate::util::write_fresh(target, bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "codex_alab_published_target_tests.rs"]
mod tests;
