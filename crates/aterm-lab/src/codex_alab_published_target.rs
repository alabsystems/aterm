// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preserve signed package authority across the proof's private handoff.

use std::io;
use std::path::{Path, PathBuf};

use aterm_agent::harness::native_act::{Image, Process};
use atpkg::prerequisite::{HeldTool, Prerequisite};
use atpkg::signed_artifact_cache::{SignedArtifactCache, SignedArtifactKey};

use super::Stdio;

pub(super) struct Packages {
    original: Option<Package>,
    replacement: Package,
    index_documents: Vec<(String, Vec<u8>)>,
}

struct Package {
    source: PathBuf,
    held: HeldTool,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    exposes: Vec<String>,
}

pub(super) struct Staged {
    original: Option<Payload>,
    replacement: Payload,
    pub(super) registry: PathBuf,
}

pub(super) struct Payload {
    held: HeldTool,
    exposes: Vec<String>,
}

impl Packages {
    /// Check the same ABI the real Source constructor requires on both signed leases
    /// before starting the rig. Successful preflight supplies no admission.
    pub(super) fn check_source_constructor(&self) -> Result<(), String> {
        let original = self
            .original
            .as_ref()
            .ok_or("missing the signed original")?;
        let mut missing = Vec::new();
        for (role, package) in [("original", original), ("replacement", &self.replacement)] {
            package.held.validate()?;
            let supported = aterm_agent::harness::codex_alab::source_credential_capable(
                package.held.physical_path(),
            )
            .map_err(|error| format!("could not check the signed {role} Source ABI: {error}"))?;
            if !supported {
                missing.push(format!("{role} {}", package.held.build()));
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "missing the native Source constructor ABI in signed Codex build(s) {}",
                missing.join(", ")
            ))
        }
    }

    pub(super) fn prepare(original: Option<&Path>, replacement: &Path) -> Result<Self, String> {
        let config = atpkg::config::cached();
        if config.unreadable {
            return Err("the package configuration is unreadable".into());
        }
        let layout = atpkg::store::resolve_from(config)
            .ok_or("the managed package prefix is unavailable")?;
        let replacement = installed_build(&layout, replacement)?;
        let original = original
            .map(|path| installed_build(&layout, path))
            .transpose()?;
        if let Some(original) = original {
            check_successor(original, replacement)?;
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
        let row = selected
            .index
            .program("codex")
            .ok_or("the signed index names no Codex package")?;
        let candidate = candidates
            .get(selected.candidate)
            .ok_or("the admitted index has no source document")?;
        let index_documents = vec![
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
        ];
        let package = |build| {
            if !atpkg::gate::current_build_ok(&channel, "codex", Some(build)) {
                return Err(format!(
                    "Codex build {build} is withdrawn or below its signed floor"
                ));
            }
            let source = layout.build_dir("codex", build);
            let held = atpkg::prerequisite::configured_tool(
                Prerequisite::Codex,
                build,
                "codex-alab proof: retain a signed handoff payload while copying it",
            )?;
            if source.canonicalize().map_err(|e| e.to_string())? != held.root() {
                return Err(
                    "the admitted handoff payload differs from the requested package".into(),
                );
            }
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
                || !manifest.exposes.iter().any(|name| name == "codex")
            {
                return Err("the signed handoff documents differ from the admitted payload".into());
            }
            Ok(Package {
                source,
                held,
                manifest: raw,
                signature,
                exposes: manifest.exposes,
            })
        };
        let replacement = package(replacement)?;
        let original = original.map(package).transpose()?;
        Ok(Self {
            original,
            replacement,
            index_documents,
        })
    }

    pub(super) fn stage(self, scratch: &Path, home: &Path) -> Result<Staged, String> {
        let layout = private_layout(home);
        let registry = scratch.join("signed-registry");
        std::fs::create_dir(&registry).map_err(|e| e.to_string())?;
        for (name, bytes) in self.index_documents {
            crate::util::write_fresh(&registry.join(name), bytes).map_err(|e| e.to_string())?;
        }
        let fetcher = atpkg::DirFetcher::new(registry.clone());
        let session = atpkg::prerequisite::Session::resolve(
            &fetcher,
            &layout,
            &atpkg::Anchor::pinned(0),
            atpkg::BuildFloor::none(),
            crate::util::now_secs(),
            aterm_toolchain::rustup::NATIVE_TRIPLE,
        )
        .map_err(|e| format!("the private handoff registry was refused: {e}"))?
        .with_description("codex-alab proof: hold the signed private handoff payloads");
        let original = self
            .original
            .map(|package| package.stage(&layout, &registry, &session, &scratch.join("pkg-a")))
            .transpose()?;
        let replacement =
            self.replacement
                .stage(&layout, &registry, &session, &scratch.join("pkg-b"))?;
        // A signed original starts selected. The development path still keeps B installed
        // underneath the temporary link for A.
        original.as_ref().unwrap_or(&replacement).activate(home)?;
        Ok(Staged {
            original,
            replacement,
            registry,
        })
    }
}

impl Staged {
    /// A shipped host cannot select the rig's private registry. Exercise that host's actual
    /// signed-package admission in the same network jail before attributing a failure to Codex.
    pub(super) fn check_fixture_host(
        &self,
        aterm: &Path,
        jail: &str,
        env: &[(String, String)],
        scratch: &Path,
    ) -> Result<(), String> {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        command
            .args(["-p", jail])
            .arg(aterm)
            .args(["pkg", "lease", "--tool"])
            .arg(format!("codex={}", self.replacement.build()))
            .args(["--", "/usr/bin/true"])
            .current_dir(scratch)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        let answer = aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
            .map_err(|_| "the fixture host did not complete signed-package admission under the private network policy")?;
        if answer.status.success() {
            return Ok(());
        }
        let why = if String::from_utf8_lossy(&answer.stderr)
            .contains("could not reach the toolchain index")
        {
            "the fixture host could not reach the private signed package index"
        } else {
            "the fixture host could not admit the staged signed successor"
        };
        Err(format!(
            "{why}; pass --aterm pointing to a debug or dev-seams build with private registry support"
        ))
    }

    pub(super) fn original(&self) -> Option<&Payload> {
        self.original.as_ref()
    }

    pub(super) fn payload(&self, package: &Path) -> Result<Option<&Payload>, String> {
        let root = package.canonicalize().map_err(|e| e.to_string())?;
        Ok(self
            .original
            .iter()
            .chain([&self.replacement])
            .find(|payload| payload.held.root() == root))
    }
}

impl Payload {
    pub(super) fn activate(&self, home: &Path) -> Result<(), String> {
        self.held.validate()?;
        let layout = private_layout(home);
        let root = self.held.root();
        atpkg::activate_build(&layout, root).map_err(|e| e.to_string())?;
        let refused = atpkg::activate::install_shims_env(
            &layout,
            root,
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
        let selected = atpkg::lookup::read(Some(&layout), "codex");
        if !selected.managed
            || selected.build != Some(self.held.build())
            || selected.physical_executable.as_deref() != Some(self.held.physical_path())
        {
            return Err("the private Codex selection differs from its signed image".into());
        }
        Ok(())
    }

    pub(super) fn build(&self) -> u64 {
        self.held.build()
    }

    pub(super) fn observe_process(&self, pid: i32) -> Option<Process> {
        self.held.validate().ok()?;
        let image = Image::at(self.held.physical_path())?;
        observe_image(pid, &image)
    }
}

impl Package {
    fn stage(
        self,
        layout: &atpkg::Layout,
        registry: &Path,
        session: &atpkg::prerequisite::Session<'_>,
        alias: &Path,
    ) -> Result<Payload, String> {
        let build = self.held.build();
        self.held.validate()?;
        let root = layout.build_dir("codex", build);
        let parent = root.parent().ok_or("the private build has no parent")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        clone_package(self.held.root(), &root)?;
        copy_records(&self.source, &root).map_err(|e| e.to_string())?;
        self.held.validate()?;
        let name = format!("pkg-codex-{build}.toml");
        crate::util::write_fresh(&registry.join(&name), self.manifest)
            .map_err(|e| e.to_string())?;
        crate::util::write_fresh(&registry.join(format!("{name}.sig")), self.signature)
            .map_err(|e| e.to_string())?;
        let held = session.acquire_build(Prerequisite::Codex, build)?;
        if held.tree_root() != self.held.tree_root()
            || held.version() != self.held.version()
            || held.environment() != self.held.environment()
        {
            return Err("the private handoff payload differs from the admitted original".into());
        }
        std::os::unix::fs::symlink(&root, alias).map_err(|e| e.to_string())?;
        Ok(Payload {
            held,
            exposes: self.exposes,
        })
    }
}

pub(super) fn linked(home: &Path) -> Result<bool, String> {
    atpkg::linked_checkout_checked(&private_layout(home), "codex")
        .map(|checkout| checkout.is_some())
        .map_err(|e| e.to_string())
}

fn private_layout(home: &Path) -> atpkg::Layout {
    atpkg::Layout {
        prefix: atpkg::store::default_prefix(home),
    }
}

fn check_successor(old: u64, new: u64) -> Result<(), String> {
    if new <= old {
        return Err(format!(
            "cross-release handoff requires a newer signed Codex build: source {old}, selected {new}; omit --signed-original for a development-link smoke"
        ));
    }
    Ok(())
}

fn installed_build(layout: &atpkg::Layout, package: &Path) -> Result<u64, String> {
    let package = package.canonicalize().map_err(|e| e.to_string())?;
    atpkg::ops::list_installed_for(layout, "codex")
        .into_iter()
        .find(|build| {
            layout
                .build_dir("codex", *build)
                .canonicalize()
                .is_ok_and(|root| root == package)
        })
        .ok_or_else(|| {
            format!(
                "the handoff package must be an installed signed Codex build: {}",
                package.display()
            )
        })
}

fn observe_image(pid: i32, image: &Image) -> Option<Process> {
    Process::observe(u32::try_from(pid).ok()?).filter(|process| &process.image == image)
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
