// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Signed, leased build prerequisites. A shim observation identifies a candidate;
//! the selected signed channel and package plus a fresh tree fold admit it.
//! Neither PATH nor local status records authorize an executable.

use std::path::{Path, PathBuf};

use aterm_uds::host::upstream::{self, PinnedTool};

use crate::flow::Fetcher;
use crate::lease::{Lease, Subject};
use crate::lookup::{Record, Status};
use crate::sig::{Anchor, BuildFloor, TrustedIndex};
use crate::store::Layout;

mod named;
pub use named::configured_build;
pub use named::configured_tool;
#[cfg(unix)]
pub mod handoff;

/// Stable native-runner admission capability; older clients refuse the query.
pub const LEASE_CAPABILITY: &str = "aterm-signed-prerequisite-leases-v1";
/// Explicit named-build support, independent of the original selected-tool contract.
pub const NAMED_LEASE_CAPABILITY: &str = "aterm-signed-named-prerequisite-leases-v1";

const FRESHNESS_TRANSPORT_PREFIX: &str = "signed channel freshness transport: ";

/// The transport detail from a prerequisite admission that could not establish the live
/// channel head. Callers report this as COULD NOT RUN and must not suggest reinstalling an
/// already-present package.
#[must_use]
pub fn freshness_transport(error: &str) -> Option<&str> {
    error.strip_prefix(FRESHNESS_TRANSPORT_PREFIX)
}

/// Admit prerequisites from the same configured source, prefix, trust anchor,
/// freshness cache and durable floors the package manager uses. Payloads must
/// already be installed; this operation never accepts unmanaged PATH tools.
///
/// # Errors
/// Invalid configuration, unavailable package source, or any admission failure.
pub fn configured(wants: &[Prerequisite], who: &str) -> Result<HeldSet, String> {
    configured_with(wants, who, false).map(|(held, _)| held)
}

/// Install one prerequisite through the normal signed package flow, then admit
/// its selected bytes and retain its lease. The boolean says it was current.
/// No workspace cache or PATH copy can satisfy this operation.
///
/// # Errors
/// Unsupported hosts, invalid package identity, install or admission failures.
pub fn provision(want: Prerequisite, who: &str) -> Result<(HeldSet, bool), String> {
    configured_with(&[want], who, true)
}

fn configured_with(
    wants: &[Prerequisite],
    who: &str,
    install: bool,
) -> Result<(HeldSet, bool), String> {
    let config = crate::config::cached();
    if config.unreadable {
        return Err("the package configuration is unreadable".into());
    }
    let layout =
        crate::store::resolve_from(config).ok_or("the managed package prefix is unavailable")?;
    admitted_in(&layout, wants, who, install)
}

/// Admit already installed prerequisites from an explicitly selected signed store.
/// Signature, freshness, tree verification and lease policy are identical to [`configured`].
///
/// # Errors
/// An unsupported host, unavailable signed source or failed package admission.
pub fn configured_in(
    layout: &Layout,
    wants: &[Prerequisite],
    who: &str,
) -> Result<HeldSet, String> {
    admitted_in(layout, wants, who, false).map(|(held, _)| held)
}

/// Authenticate one installed executable from the caller's selected store,
/// retaining its signed payload and original lease without installing anything.
/// Unlike a command namespace this does not create replacement shim paths.
///
/// # Errors
/// Signed source, freshness, payload, selection or original-lease validation fails.
pub fn selected_in(
    layout: &Layout,
    want: Prerequisite,
    who: &str,
    lease_wait: std::time::Duration,
) -> Result<HeldTool, String> {
    let fetcher = crate::source::resolve_fetcher(layout, None);
    let session = Session::resolve(
        fetcher.as_ref(),
        layout,
        &crate::source::effective_anchor(layout),
        crate::source::build_floor(layout),
        crate::flow::now_unix(),
        crate::cli::current_triple(),
    )
    .map_err(admission_error)?;
    let session = session.with_description(who);
    let held = session.acquire_with_lease_wait(want, lease_wait)?;
    crate::source::advance_floors(
        layout,
        session.index.index_build,
        session.index.roster_seq(),
    );
    Ok(held)
}

fn admitted_in(
    layout: &Layout,
    wants: &[Prerequisite],
    who: &str,
    install: bool,
) -> Result<(HeldSet, bool), String> {
    let triple = crate::cli::current_triple();
    for &want in wants {
        if !want.supports(triple) {
            return Err(format!("{} is unavailable on {triple}", want.program()));
        }
    }
    let fetcher = crate::source::resolve_fetcher(layout, None);
    let session = Session::resolve(
        fetcher.as_ref(),
        layout,
        &crate::source::effective_anchor(layout),
        crate::source::build_floor(layout),
        crate::flow::now_unix(),
        triple,
    )
    .map_err(admission_error)?;
    let session = session.with_description(who);
    let mut current = true;
    if install {
        for &want in wants {
            current &= session.install(want)?;
        }
    }
    let held = session.acquire_all(wants)?;
    crate::source::advance_floors(
        layout,
        session.index.index_build,
        session.index.roster_seq(),
    );
    Ok((held, current))
}

fn admission_error(error: crate::FlowError) -> String {
    let provenance = crate::flow::last_resolve();
    if let Some(provenance) = provenance
        && provenance.transport_failed
    {
        let why = provenance.why.unwrap_or_else(|| error.to_string());
        return format!("{FRESHNESS_TRANSPORT_PREFIX}{why}");
    }
    format!("signed prerequisite index: {error}")
}

/// A signed prerequisite and its public package command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prerequisite {
    /// Secret scanning.
    Gitleaks,
    /// The publication engine's pinned Python interpreter.
    Python,
    /// Ed25519 source signing, separate from Apple's certificate tooling.
    OpenSslSigning,
    /// Lean's project builder. The global `lean` name still means Clean.
    Lake,
    /// ALab's separately packaged release-signing helper.
    AtpkgKeys,
    /// AY's maintained solver and campaign executable.
    Ay,
    /// Codex's retained process-update target.
    Codex,
}

impl Prerequisite {
    /// The upstream recipe, when this prerequisite wraps an upstream tool.
    /// First-party versions and targets come from the signed package manifest.
    #[must_use]
    pub const fn recipe(self) -> Option<&'static PinnedTool> {
        match self {
            Self::Gitleaks => Some(&upstream::GITLEAKS),
            Self::Python => Some(&upstream::CPYTHON),
            Self::OpenSslSigning => Some(&upstream::OPENSSL_SIGNING),
            Self::Lake => Some(&upstream::LEAN4),
            Self::AtpkgKeys | Self::Ay | Self::Codex => None,
        }
    }

    /// Owning package in the signed index and manifest.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::Gitleaks => upstream::GITLEAKS.name,
            Self::Python => upstream::CPYTHON.name,
            Self::OpenSslSigning => upstream::OPENSSL_SIGNING.name,
            Self::Lake => upstream::LEAN4.name,
            Self::AtpkgKeys => "atpkg-keys",
            Self::Ay => "ay",
            Self::Codex => "codex",
        }
    }

    /// The exposed package executable, preserving its invocation name.
    #[must_use]
    pub const fn executable(self) -> &'static str {
        match self {
            Self::Gitleaks => "gitleaks",
            Self::Python => "python3.14",
            Self::OpenSslSigning => "openssl-signing",
            Self::Lake => "lake",
            Self::AtpkgKeys => "atpkg-keys",
            Self::Ay => "ay",
            Self::Codex => "codex",
        }
    }

    /// Resolve only an exposed prerequisite name; upstream `lean` remains Clean.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        [
            Self::Gitleaks,
            Self::Python,
            Self::OpenSslSigning,
            Self::Lake,
            Self::AtpkgKeys,
            Self::Ay,
            Self::Codex,
        ]
        .into_iter()
        .find(|want| want.executable() == name)
    }

    /// Whether the shared recipe admits this target, before source discovery.
    #[must_use]
    pub fn supports(self, triple: &str) -> bool {
        let platform = match triple {
            "aarch64-apple-darwin" => "macos-aarch64",
            "x86_64-apple-darwin" => "macos-x86_64",
            "aarch64-unknown-linux-gnu" => "linux-aarch64",
            "x86_64-unknown-linux-gnu" => "linux-x86_64",
            _ => "unsupported",
        };
        self.recipe()
            .is_none_or(|recipe| recipe.hosts.contains(&platform))
    }
}

/// One operation's authenticated channel snapshot, reused for each prerequisite.
pub struct Session<'a> {
    fetcher: &'a dyn Fetcher,
    layout: &'a Layout,
    index: TrustedIndex,
    triple: &'a str,
    description: &'a str,
    prepared: std::cell::RefCell<Vec<(Prerequisite, Prepared)>>,
}

#[derive(Clone)]
struct Prepared {
    build: u64,
    package: crate::manifest::PkgManifest,
    signed: crate::flow::PackageBytes,
}

impl<'a> Session<'a> {
    /// Resolve one index through atpkg's normal signature, freshness, replay
    /// floor, succession and authenticated-cache admission.
    ///
    /// # Errors
    /// The existing verified index resolver's failures, or a cached fallback that cannot answer
    /// this operation's required live-freshness question.
    pub fn resolve(
        fetcher: &'a dyn Fetcher,
        layout: &'a Layout,
        anchor: &Anchor,
        floor: BuildFloor,
        now_unix: i64,
        triple: &'a str,
    ) -> Result<Self, crate::FlowError> {
        let index = crate::flow::resolve_verified_index(fetcher, layout, anchor, floor, now_unix)?;
        if let Some(provenance) = crate::flow::last_resolve()
            && !provenance.reached
        {
            if provenance.transport_failed {
                return Err(crate::FlowError::Unreachable(
                    provenance.why.unwrap_or_else(|| {
                        "the signed channel freshness read did not reach its source".to_string()
                    }),
                ));
            }
            return Err(crate::FlowError::NoIndex);
        }
        Ok(Self {
            fetcher,
            layout,
            index,
            triple,
            description: "signed build prerequisite",
            prepared: std::cell::RefCell::default(),
        })
    }

    /// Name the operation recorded in each held lease.
    #[must_use]
    pub fn with_description(mut self, description: &'a str) -> Self {
        self.description = description;
        self
    }

    fn package(&self, want: Prerequisite) -> Result<Prepared, String> {
        if let Some(prepared) = self
            .prepared
            .borrow()
            .iter()
            .find(|(known, _)| *known == want)
            .map(|(_, package)| package.clone())
        {
            return Ok(prepared);
        }
        let program = want.program();
        if !want.supports(self.triple) {
            return Err(format!("{program} is unavailable on {}", self.triple));
        }
        let channel = self
            .index
            .channel_for(crate::config::CHANNEL, self.triple)
            .ok_or("the signed stable channel is unavailable")?;
        let build = *channel
            .pin
            .get(program)
            .ok_or_else(|| format!("{program} has no signed channel pin"))?;
        let prepared = self.read_package(want, build, false)?;
        self.prepared.borrow_mut().push((want, prepared.clone()));
        Ok(prepared)
    }

    fn read_package(
        &self,
        want: Prerequisite,
        build: u64,
        named: bool,
    ) -> Result<Prepared, String> {
        let program = want.program();
        let channel = self
            .index
            .channel_for(crate::config::CHANNEL, self.triple)
            .ok_or("the signed stable channel is unavailable")?;
        if !crate::gate::current_build_ok(&channel, program, Some(build)) {
            return Err(format!(
                "{program} {build} is withdrawn or below its signed floor"
            ));
        }
        let row = self
            .index
            .program(program)
            .ok_or("the signed index has no prerequisite program")?;
        let source = self.fetcher.package_source_id(&row.repo, program);
        let cache = crate::signed_artifact_cache::SignedArtifactCache::for_layout(self.layout);
        let document_cap = usize::try_from(crate::net::MANIFEST_CAP).unwrap_or(usize::MAX);
        let signature_cap = usize::try_from(crate::net::SIG_CAP).unwrap_or(usize::MAX);
        if let Some(source) = source.as_deref() {
            let key = crate::signed_artifact_cache::SignedArtifactKey::package(
                source, &row.repo, program, build,
            );
            if let Some((raw, signature)) = cache.load(&key, document_cap, signature_cap)
                && let Ok(prepared) = self.prepare_package(want, build, named, raw, signature)
            {
                return Ok(prepared);
            }
        }
        let (raw, signature) = self
            .fetcher
            .pkg_manifest(&row.repo, program, build)
            .map_err(|why| format!("{program} build {build} signed manifest fetch: {why}"))?;
        let prepared = self.prepare_package(want, build, named, raw, signature)?;
        if let Some(source) = source.as_deref() {
            let key = crate::signed_artifact_cache::SignedArtifactKey::package(
                source, &row.repo, program, build,
            );
            if let Err(error) = cache.store_verified(
                &key,
                &prepared.signed.0,
                &prepared.signed.1,
                document_cap,
                signature_cap,
            ) {
                eprintln!(
                    "atpkg: the verified {program} {build} package document was not cached ({error})"
                );
            }
        }
        Ok(prepared)
    }

    fn prepare_package(
        &self,
        want: Prerequisite,
        build: u64,
        named: bool,
        raw: Vec<u8>,
        signature: Vec<u8>,
    ) -> Result<Prepared, String> {
        let program = want.program();
        let verified = self
            .index
            .verify_pkg(raw.clone(), &signature)
            .map_err(|e| format!("{program} package signature: {e:?}"))?;
        let package = crate::manifest::parse_pkg(&verified)
            .map_err(|e| format!("{program} package manifest: {e:?}"))?;
        let recipe = match want.recipe() {
            Some(base) if named => Some(
                base.versioned(&package.version)
                    .ok_or("the named prerequisite version has no shared recipe")?,
            ),
            recipe => recipe,
        };
        let platform = match self.triple {
            "aarch64-apple-darwin" => "macos-aarch64",
            "x86_64-apple-darwin" => "macos-x86_64",
            "aarch64-unknown-linux-gnu" => "linux-aarch64",
            "x86_64-unknown-linux-gnu" => "linux-x86_64",
            _ => "unsupported",
        };
        if let Some(recipe) = recipe {
            if !recipe.hosts.contains(&platform) {
                return Err(format!(
                    "{} {} is unavailable on {}",
                    recipe.name, recipe.version, self.triple
                ));
            }
            if package.version != recipe.version {
                return Err(format!(
                    "{program} signed package identity differs from its shared recipe"
                ));
            }
        }
        if !package.is_for(program)
            || package.build_number != build
            || package.version.is_empty()
            || !package.exposes.iter().any(|e| e == want.executable())
        {
            return Err(format!(
                "{program} signed package identity differs from its shared recipe"
            ));
        }
        let artifact = package
            .artifacts
            .iter()
            .find(|a| a.target == self.triple)
            .ok_or_else(|| format!("{program} has no published build on {}", self.triple))?;
        if artifact.tree_root.len() != 64
            || !artifact
                .tree_root
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("the signed package has no valid immutable tree root".into());
        }
        let expected_env: Vec<String> = recipe
            .map_or(&[][..], |recipe| recipe.shim_env)
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect();
        if package.shim_env != expected_env {
            return Err(
                "the signed prerequisite environment differs from its shared recipe".into(),
            );
        }
        crate::shim_env::ShimEnv::admit(&package.shim_env)?;
        Ok(Prepared {
            build,
            package,
            signed: (raw, signature),
        })
    }

    fn install(&self, want: Prerequisite) -> Result<bool, String> {
        // Refuse a signed package for a different recipe before changing any
        // installed selection. Reuse this operation's authenticated index.
        let prepared = self.package(want)?;
        let req = crate::InstallRequest {
            channel: crate::config::CHANNEL,
            program: want.program(),
            triple: self.triple,
            installed: crate::active_builds(self.layout)
                .get(want.program())
                .copied(),
        };
        let report = crate::flow::install_collecting_assets_with_snapshot(
            self.fetcher,
            self.layout,
            &crate::source::effective_anchor(self.layout),
            &req,
            crate::source::build_floor(self.layout),
            crate::flow::now_unix(),
            Some(crate::flow::InstallSnapshot {
                index: self.index.clone(),
                package: Some(prepared.signed),
            }),
            &mut std::collections::BTreeMap::new(),
        )
        .map_err(|e| e.to_string())?;
        Ok(report.already_current)
    }

    /// Authenticate the selected package and installed bytes, retaining a lease
    /// until the returned tool is dropped. Callers keep it through child exit.
    ///
    /// # Errors
    /// Unsupported targets, missing or withdrawn pins, invalid signed package
    /// identity, unmanaged shims, missing leases, mutated bytes or selection races.
    pub fn acquire(&self, want: Prerequisite) -> Result<HeldTool, String> {
        self.acquire_with_lease_wait(want, crate::lease::DEFAULT_WAIT)
    }

    /// Authenticate an installed selection without waiting longer than the
    /// caller permits for its original lease. Zero refuses a reclaiming build
    /// before any executable is offered to the caller.
    ///
    /// # Errors
    /// The same authentication failures as [`Self::acquire`], or lease contention.
    pub fn acquire_with_lease_wait(
        &self,
        want: Prerequisite,
        lease_wait: std::time::Duration,
    ) -> Result<HeldTool, String> {
        let program = want.program();
        let prepared = self.package(want)?;
        let (build, package) = (prepared.build, prepared.package);
        let artifact = package
            .artifacts
            .iter()
            .find(|a| a.target == self.triple)
            .expect("package checked the target");
        let signed_env = crate::shim_env::ShimEnv::admit(&package.shim_env)?;
        let selected = crate::lookup::read(Some(self.layout), want.executable());
        admit_selection(&selected, want, build)?;
        let (lease, held) = hold_selection(
            self.layout,
            want,
            build,
            &selected,
            self.description,
            lease_wait,
        )?;
        let expected_env: std::collections::BTreeMap<_, _> =
            signed_env.entries().iter().cloned().collect();
        if held.shim_env != expected_env {
            return Err("the prerequisite environment differs from its signed manifest".into());
        }
        let root = held
            .store_root
            .as_deref()
            .ok_or("the prerequisite has no physical payload root")?;
        if held.physical_root.as_deref() != Some(root) {
            return Err(
                "the prerequisite execution root differs from its immutable payload".into(),
            );
        }
        let named = root.join("bin").join(want.executable());
        if held.executable.as_deref().and_then(Path::file_name)
            != Some(std::ffi::OsStr::new(want.executable()))
            || named.canonicalize().ok().as_ref() != held.physical_executable.as_ref()
        {
            return Err("the prerequisite shim selects another package executable".into());
        }
        let identity = crate::trust_lock::tree_identity(root)
            .ok_or("the prerequisite tree has no stable identity")?;
        let physical = held
            .physical_executable
            .as_deref()
            .ok_or("the prerequisite has no physical executable")?;
        let (image, metadata) =
            aterm_uds::regular_file::open(physical).map_err(|e| e.to_string())?;
        let image_identity = ImageIdentity::of(&metadata);
        let modes = crate::store::declared_modes(&self.layout.build_dir(program, build))
            .map_err(|e| e.to_string())?;
        let actual = crate::tree::tree_root_declared(root, &modes).map_err(|e| e.to_string())?;
        let after = crate::lookup::read(Some(self.layout), want.executable());
        if actual != artifact.tree_root
            || !same_selection(&held, &after)
            || crate::trust_lock::tree_identity(root).as_deref() != Some(&identity)
        {
            return Err(format!(
                "{program} prerequisite bytes or selection differ from the signed tree"
            ));
        }
        let tool = HeldTool {
            executable: held
                .executable
                .ok_or("the prerequisite executable is absent")?,
            physical: physical.to_path_buf(),
            image,
            image_identity,
            environment: signed_env,
            lease,
            program,
            build,
            tree_root: actual,
            root: root.to_path_buf(),
            version: package.version,
        };
        tool.validate()?;
        Ok(tool)
    }

    /// Acquire each requested tool once and expose only those exact held
    /// invocations to subprocesses. The directory never exposes `lean`.
    ///
    /// # Errors
    /// A tool cannot be admitted, or the private shim directory cannot be made.
    pub fn acquire_all(&self, wants: &[Prerequisite]) -> Result<HeldSet, String> {
        let mut tools = Vec::new();
        for &want in wants {
            if tools.iter().any(|(known, _)| *known == want) {
                continue;
            }
            tools.push((want, self.acquire(want)?));
        }
        let directory = aterm_tempfile::Builder::new()
            .prefix("atpkg-prerequisites-")
            .suffix(".noindex")
            .tempdir()
            .map_err(|e| e.to_string())?;
        for (want, tool) in &tools {
            let names: &[&str] = if *want == Prerequisite::Python {
                // Source transforms use the portable python3 spelling. This
                // alias belongs only to the held operation, never global PATH.
                &["python3.14", "python3"]
            } else {
                &[want.executable()]
            };
            for name in names {
                crate::platform::install_shim_to_env(
                    &directory.path().join(name),
                    tool.path(),
                    tool.environment(),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(HeldSet { directory, tools })
    }
}

/// An operation's fixed command namespace and all its signed build leases.
/// A channel update can replace global shims without changing these invocations.
#[derive(Debug)]
pub struct HeldSet {
    directory: aterm_tempfile::TempDir,
    tools: Vec<(Prerequisite, HeldTool)>,
}

impl HeldSet {
    /// The leased executable for an explicitly requested prerequisite.
    #[must_use]
    pub fn tool(&self, want: Prerequisite) -> Option<&HeldTool> {
        self.tools
            .iter()
            .find(|(known, _)| *known == want)
            .map(|(_, tool)| tool)
    }

    /// The directory that exposes this set's fixed command names: a temporary path of this
    /// acquisition alone, different in every process that acquires the same tools.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// What a child finds under [`HeldSet::directory`], without that path: each exposed
    /// command with the signed package, build and payload root it runs. Two acquisitions of
    /// the same signed builds have one identity.
    #[must_use]
    pub fn identity(&self) -> String {
        self.tools
            .iter()
            .map(|(want, tool)| {
                format!(
                    "{}={}@{}:{}",
                    want.executable(),
                    tool.program(),
                    tool.build(),
                    tool.tree_root()
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Validate every held executable and put only the fixed requested names
    /// before the child's remaining PATH. Retain this set through child exit.
    ///
    /// # Errors
    /// An admitted executable changed, or PATH cannot represent the directory.
    pub fn apply(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        remaining_path: &std::ffi::OsStr,
    ) -> Result<(), String> {
        command.env("PATH", self.child_path(remaining_path)?);
        Ok(())
    }

    /// A validated fixed namespace for a pass-through command builder. Retain
    /// this set while the returned PATH is in use by a child.
    ///
    /// # Errors
    /// As `apply`.
    pub fn child_path(
        &self,
        remaining_path: &std::ffi::OsStr,
    ) -> Result<std::ffi::OsString, String> {
        for (_, tool) in &self.tools {
            tool.validate()?;
        }
        let path =
            std::env::join_paths(std::iter::once(self.directory.path().to_path_buf()).chain(
                std::env::split_paths(remaining_path).filter(|path| path != self.directory.path()),
            ))
            .map_err(|e| e.to_string())?;
        Ok(path)
    }
}

fn hold_selection(
    layout: &Layout,
    want: Prerequisite,
    build: u64,
    selected: &Record,
    description: &str,
    wait: std::time::Duration,
) -> Result<(Lease, Record), String> {
    let subject = Subject::build(want.program(), build)
        .ok_or("the prerequisite lease identity is invalid")?;
    let lease = crate::lease::take(&layout.prefix, &subject, description, wait)
        .map_err(|e| format!("{} prerequisite lease: {e}", want.program()))?;
    let held = crate::lookup::read(Some(layout), want.executable());
    admit_selection(&held, want, build)?;
    if !same_selection(selected, &held) {
        return Err("the prerequisite shim changed while acquiring its lease".into());
    }
    Ok((lease, held))
}

fn admit_selection(record: &Record, want: Prerequisite, build: u64) -> Result<(), String> {
    if record.status != Status::Ready
        || !record.managed
        || record.package.as_deref() != Some(want.program())
        || record.build != Some(build)
    {
        return Err(format!(
            "{} needs signed atpkg {} {build}; install with `aterm pkg install {}`",
            want.executable(),
            want.program(),
            want.program()
        ));
    }
    Ok(())
}

fn same_selection(a: &Record, b: &Record) -> bool {
    a.status == b.status
        && a.managed == b.managed
        && a.package == b.package
        && a.build == b.build
        && a.executable == b.executable
        && a.physical_executable == b.physical_executable
        && a.physical_root == b.physical_root
        && a.store_root == b.store_root
        && a.shim_env == b.shim_env
}

/// A signed store executable and the lease keeping its immutable build alive.
#[derive(Debug)]
pub struct HeldTool {
    executable: PathBuf,
    physical: PathBuf,
    image: std::fs::File,
    image_identity: ImageIdentity,
    environment: crate::shim_env::ShimEnv,
    lease: Lease,
    /// Owning signed package.
    program: &'static str,
    /// Selected signed channel build.
    build: u64,
    /// Freshly folded signed payload root.
    tree_root: String,
    root: PathBuf,
    version: String,
}

impl HeldTool {
    /// Duplicate the original GC lease for explicit payload lifetime custody.
    /// The copy neither authenticates new bytes nor grants execution resources.
    ///
    /// # Errors
    /// Original lease custody changed or duplication failed.
    #[cfg(unix)]
    pub fn lease_descriptor(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        self.lease.try_clone_descriptor()
    }

    /// The already authenticated physical executable, while this guard is held.
    #[must_use]
    pub fn physical_path(&self) -> &Path {
        &self.physical
    }
    /// The signed package identity retained by this guard.
    #[must_use]
    pub const fn program(&self) -> &'static str {
        self.program
    }

    /// The selected signed build.
    #[must_use]
    pub const fn build(&self) -> u64 {
        self.build
    }

    /// The freshly folded signed root.
    #[must_use]
    pub fn tree_root(&self) -> &str {
        &self.tree_root
    }

    /// Exact signed package version, also checked against the shared upstream
    /// recipe when this prerequisite wraps an upstream tool.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Immutable physical payload root retained by this tool's lease.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The exposed invocation path, valid while this guard is retained.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.executable
    }

    /// Only environment admitted from the signed package, never a local sidecar.
    #[must_use]
    pub const fn environment(&self) -> &crate::shim_env::ShimEnv {
        &self.environment
    }

    /// Recheck actual lease custody, invocation and opened executable identity immediately before
    /// building a child. The store protocol keeps a leased build immutable;
    /// this also detects a replaced or edited executable outside that protocol.
    ///
    /// # Errors
    /// Lease custody was lost, or the invocation/image identity changed or is unreadable.
    pub fn validate(&self) -> Result<(), String> {
        self.lease
            .check_current()
            .map_err(|error| format!("the prerequisite lease lost custody: {error}"))?;
        let (_, current) =
            aterm_uds::regular_file::open(&self.physical).map_err(|e| e.to_string())?;
        if self.executable.canonicalize().ok().as_ref() != Some(&self.physical)
            || ImageIdentity::of(&current) != self.image_identity
            || ImageIdentity::of(&self.image.metadata().map_err(|e| e.to_string())?)
                != self.image_identity
        {
            return Err("the admitted prerequisite executable changed before execution".into());
        }
        Ok(())
    }

    /// A command using the admitted invocation and signed environment. Keep this
    /// tool guard until the child exits, including on interruption.
    ///
    /// # Errors
    /// As `validate`.
    pub fn command(&self) -> Result<aterm_uds::spawnfd::Command, String> {
        self.validate()?;
        let mut command = aterm_uds::spawnfd::command(&self.executable);
        for (name, value) in self.environment.entries() {
            command.env(name, value);
        }
        Ok(command)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ImageIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, u32, i64, i64),
}

impl ImageIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt as _;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.mode(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
