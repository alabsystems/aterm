// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared authenticated, leased compiler selection. Source authority and bounded
//! version/signed-index probes belong to the caller; selection never installs.
use crate::trust_lock;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Caller-owned version probe constrained by the selection's absolute deadline.
pub type VersionProbe<'a> =
    dyn Fn(&Path, Option<Instant>) -> std::result::Result<String, String> + 'a;

/// Captured explicit selection inputs, never populated from ambient environment here.
pub struct Inputs {
    /// Canonical managed store selected by the caller.
    pub prefix: PathBuf,
    /// Project configuration directory.
    pub repo: PathBuf,
    /// Captured command search path, never ambient host PATH.
    pub path: OsString,
    /// Captured Cargo configuration home.
    pub cargo_home: PathBuf,
    /// Compiler-moving environment entries to refuse.
    pub overrides: Vec<(String, String)>,
}
/// Caller policy; native host resolution supplies an absolute deadline and zero lease wait.
/// None preserves existing publisher whole-operation timing; its version probes stay bounded.
#[derive(Clone, Copy)]
pub struct Options {
    /// Host callers supply one absolute deadline for all phases and callbacks.
    pub deadline: Option<Instant>,
    /// Zero returns typed Busy without waiting on a reclaiming build.
    pub lease_wait: Duration,
}
impl Options {
    fn check(self) -> Result<()> {
        if self.deadline.is_some_and(|at| Instant::now() >= at) {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    }
    fn wait(self) -> Duration {
        self.deadline.map_or(self.lease_wait, |at| {
            self.lease_wait
                .min(at.saturating_duration_since(Instant::now()))
        })
    }
}
#[derive(Debug)]
pub enum Error {
    /// Authentication or selection policy refused.
    Refused(String),
    /// Required filesystem or native operation failed.
    CannotRun(String),
    /// The selected build is being reclaimed; no compiler probe ran.
    Busy(crate::lease::Subject),
    /// The caller deadline expired; selection confers no execution authority.
    Deadline,
}
impl Error {
    fn refused(message: impl Into<String>) -> Self {
        Self::Refused(message.into())
    }
    fn cannot_run(message: impl Into<String>) -> Self {
        Self::CannotRun(message.into())
    }
    fn io(what: &str, error: &std::io::Error) -> Self {
        Self::CannotRun(format!("{what}: {error}"))
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) | Self::CannotRun(m) => f.write_str(m),
            Self::Busy(subject) => write!(
                f,
                "lease selected Trust build: {}",
                crate::lease::LeaseError::Busy(subject.clone())
            ),
            Self::Deadline => f.write_str("project compiler selection deadline expired"),
        }
    }
}
impl std::error::Error for Error {}
/// Result of authenticated compiler selection.
pub type Result<T> = std::result::Result<T, Error>;

/// Authenticated physical payload and paired execution root protected by one native lease.
pub struct Selected {
    /// Signed, physical store payload root.
    payload: PathBuf,
    /// Validated physical execution root (which may be a native compat copy).
    execution: PathBuf,
    /// Authenticated managed build.
    build: u64,
    /// Its signed tree root.
    tree_root: String,
    /// Execution compiler content, matched to the signed payload's verified lineage.
    compiler: crate::lineage::Compiler,
    /// Managed layout retained with the original native lease.
    layout: crate::Layout,
    _lease: crate::lease::Lease,
}

impl Selected {
    /// Named path of the original held SDK lease, as a read-only observation.
    /// The path grants no acquisition, recovery or settlement authority.
    #[must_use]
    pub fn lease_path(&self) -> &Path {
        self._lease.path()
    }

    /// Duplicate the actual original SDK GC lease for explicit child custody.
    /// No SDK bytes are authenticated or made immutable by this operation.
    /// # Errors
    /// The original lease changed or its descriptor could not be duplicated.
    #[cfg(unix)]
    pub fn lease_descriptor(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        self._lease.try_clone_descriptor()
    }

    /// Cheap original lease identity check; not SDK byte authentication.
    /// # Errors
    /// The named/held lease changed, became unsafe, or cannot be validated.
    pub fn check_lease(&self) -> std::io::Result<()> {
        self._lease.check_current()
    }

    /// Physical signed payload protected by the held lease.
    #[must_use]
    pub fn payload(&self) -> &Path {
        &self.payload
    }

    /// Validated executable root, which may be a compat view of the payload.
    #[must_use]
    pub fn execution(&self) -> &Path {
        &self.execution
    }

    /// The authenticated managed build, independent of global current changes.
    #[must_use]
    pub const fn build(&self) -> u64 {
        self.build
    }

    /// Fresh signed tree root verified against the payload before its first probe.
    #[must_use]
    pub fn tree_root(&self) -> &str {
        &self.tree_root
    }

    /// Immutable admitted execution identity and payload lineage proof.
    #[must_use]
    pub const fn compiler(&self) -> &crate::lineage::Compiler {
        &self.compiler
    }

    /// Exact execution Targo, independent of a later cwd's missing lock.
    #[must_use]
    pub fn targo(&self) -> PathBuf {
        self.execution.join("bin/targo")
    }

    /// Recheck payload content and any compat mapping while its native lease is still held.
    /// # Errors
    /// A modified signed payload or execution root.
    pub fn recheck(&self) -> Result<()> {
        if crate::lookup::payload_root(&self.layout, "trust", self.build).as_ref()
            != Some(&self.payload)
        {
            return Err(Error::refused(
                "selected compiler payload ownership changed",
            ));
        }
        let modes = crate::store::declared_modes(&self.payload)
            .map_err(|e| Error::io("read compiler declared modes", &e))?;
        let root = crate::tree::tree_root_declared(&self.payload, &modes)
            .map_err(|e| Error::io("hash selected compiler", &e))?;
        if root != self.tree_root {
            return Err(Error::refused("selected signed compiler payload changed"));
        }
        #[cfg(not(unix))]
        if self.execution != self.payload {
            return Err(Error::refused("compiler compatibility roots require Unix"));
        }
        #[cfg(unix)]
        if self.execution != self.payload
            && crate::compat::blocked(&self.layout, self.build).is_some()
        {
            return Err(Error::refused("selected compiler compat ownership changed"));
        }
        #[cfg(unix)]
        if self.execution != self.payload
            && !crate::compat::root_matches(
                &trust_lock::build_dir(&self.layout.prefix, self.build),
                &self.execution,
                crate::seam::Depth::Deep,
            )
        {
            return Err(Error::refused(
                "selected compiler execution root no longer matches its signed payload",
            ));
        }
        Ok(())
    }
}

/// Resolve/authenticate once, with no install or current/shim/default mutation.
/// The caller binds signed-head authority and version probes to its explicit inputs and deadline.
/// # Errors
/// Missing/malformed lock, overrides, missing lease/build, signed content or lineage mismatch,
/// invalid compat root, or an explicit compiler other than this authority's selection.
pub fn select(
    inputs: &Inputs,
    lock: Option<&trust_lock::TrustLock>,
    explicit: Option<&Path>,
    options: Options,
    signed_root: &dyn Fn(u64, Option<Instant>) -> std::result::Result<String, String>,
    probe: &VersionProbe<'_>,
) -> Result<Selected> {
    options.check()?;
    if let Some((key, _)) = inputs.overrides.first() {
        return Err(Error::refused(format!(
            "{key} overrides project compiler selection; remove it"
        )));
    }
    if crate::toolchain::linked_tool_bin(&inputs.prefix, "targo").is_some() {
        return Err(Error::refused(
            "a dev-linked Targo overrides managed compiler selection",
        ));
    }
    let (build, requested_payload) = requested(inputs, lock)?;
    options.check()?;
    let subject = crate::lease::Subject::build(trust_lock::PROGRAM, build)
        .ok_or_else(|| Error::refused("invalid managed Trust build"))?;
    let lease = crate::lease::take(
        &inputs.prefix,
        &subject,
        "authenticated project compiler",
        options.wait(),
    );
    options.check()?;
    let lease = lease.map_err(|e| match e {
        crate::lease::LeaseError::Busy(subject) => Error::Busy(subject),
        error => Error::cannot_run(format!("lease selected Trust build: {error}")),
    })?;
    options.check()?;
    let layout = crate::Layout {
        prefix: inputs.prefix.clone(),
    };
    let payload = crate::lookup::payload_root(&layout, "trust", build)
        .ok_or_else(|| Error::refused("selected compiler payload has unowned path indirection"))?;
    if std::fs::canonicalize(requested_payload).ok().as_ref() != Some(&payload) {
        return Err(Error::refused(
            "selected compiler path does not name its owned payload",
        ));
    }
    if !crate::store::build_is_complete(&trust_lock::build_dir(&inputs.prefix, build)) {
        return Err(Error::refused("selected Trust build is incomplete"));
    }
    let (tree_root, payload_compiler) =
        authenticated_payload(&payload, lock, build, signed_root, probe, options)?;
    let execution = execution_root(&payload, &layout, build)?;
    options.check()?;
    if let Some(explicit) = explicit {
        let actual = std::fs::canonicalize(explicit)
            .map_err(|e| Error::io("resolve explicit compiler", &e))?;
        if actual != execution.join("bin") {
            return Err(Error::refused(
                "explicit compiler does not match the source lock or authenticated managed default",
            ));
        }
    }
    let movers = crate::toolchain::rustc_movers::rustc_movers(
        Some(&inputs.repo),
        &|_| None,
        Some(&inputs.cargo_home),
    );
    if !movers.is_empty() {
        return Err(Error::refused(
            "Cargo configuration overrides the selected project compiler",
        ));
    }
    let compiler = paired_compiler(payload_compiler, &payload, &execution, probe, options)?;
    options.check()?;
    let selected = Selected {
        payload,
        execution,
        build,
        tree_root,
        compiler,
        layout,
        _lease: lease,
    };
    selected.recheck()?;
    options.check()?;
    Ok(selected)
}

fn requested(inputs: &Inputs, lock: Option<&trust_lock::TrustLock>) -> Result<(u64, PathBuf)> {
    Ok(if let Some(lock) = lock {
        (
            lock.build,
            trust_lock::build_dir(&inputs.prefix, lock.build),
        )
    } else {
        let root = std::fs::canonicalize(inputs.prefix.join("store/trust/current"))
            .map_err(|e| Error::io("resolve the managed default compiler", &e))?;
        let name = root.file_name().and_then(OsStr::to_str).unwrap_or("");
        let build = name
            .strip_suffix(".noindex")
            .unwrap_or(name)
            .parse::<u64>()
            .map_err(|_| {
                Error::refused("the managed default does not name a versioned Trust build")
            })?;
        let expected = std::fs::canonicalize(trust_lock::build_dir(&inputs.prefix, build))
            .map_err(|e| Error::io("resolve the default Trust build", &e))?;
        if expected != root {
            return Err(Error::refused("managed default escapes its Trust build"));
        }
        (build, root)
    })
}

#[cfg(not(unix))]
fn execution_root(payload: &Path, _layout: &crate::Layout, _build: u64) -> Result<PathBuf> {
    Ok(payload.to_path_buf())
}

#[cfg(unix)]
fn execution_root(payload: &Path, layout: &crate::Layout, build: u64) -> Result<PathBuf> {
    Ok(
        match crate::compat::needs_root(payload)
            .map_err(|e| Error::io("inspect compiler execution layout", &e))?
        {
            None => payload.to_path_buf(),
            Some(_) => {
                if let Some(blocked) = crate::compat::blocked(layout, build) {
                    return Err(Error::refused(format!(
                        "selected compiler compat path: {blocked}"
                    )));
                }
                let root = crate::compat::root_dir(layout, build);
                if !crate::compat::root_matches(
                    &trust_lock::build_dir(&layout.prefix, build),
                    &root,
                    crate::seam::Depth::Deep,
                ) {
                    return Err(Error::refused(
                        "required compiler compat root is missing or differs; repair it before running",
                    ));
                }
                root
            }
        },
    )
}

fn paired_compiler(
    payload_compiler: crate::lineage::Compiler,
    payload: &Path,
    execution: &Path,
    probe: &VersionProbe<'_>,
    options: Options,
) -> Result<crate::lineage::Compiler> {
    let result = if execution == payload {
        payload_compiler
    } else {
        // Compat roots intentionally contain executable views, not a second LINEAGE.
        // Their bytes/mapping were checked deeply against the signed, proven payload.
        options.check()?;
        let admission = crate::lineage::admit_content_with(&execution.join("bin"), &|p| {
            probe_version(probe, p, options)
        });
        options.check()?;
        let content = admission.map_err(|why| Error::refused(why.join("\n")))?;
        let proven = &payload_compiler.content;
        if content.version != proven.version
            || content.trustc_sha256 != proven.trustc_sha256
            || content.targo_sha256 != proven.targo_sha256
            || content.driver_sha256 != proven.driver_sha256
        {
            return Err(Error::refused(
                "execution compiler bytes differ from the proven signed payload",
            ));
        }
        crate::lineage::Compiler {
            content,
            lineage: payload_compiler.lineage,
        }
    };
    options.check()?;
    Ok(result)
}

#[cfg(test)]
#[path = "project_compiler/tests.rs"]
mod tests;

fn authenticated_payload(
    payload: &Path,
    lock: Option<&trust_lock::TrustLock>,
    build: u64,
    signed_root: &dyn Fn(u64, Option<Instant>) -> std::result::Result<String, String>,
    probe: &VersionProbe<'_>,
    options: Options,
) -> Result<(String, crate::lineage::Compiler)> {
    options.check()?;
    let signed = signed_root(build, options.deadline);
    options.check()?;
    let tree_root = signed.map_err(Error::refused)?;
    if lock.is_some_and(|lock| lock.tree_root != tree_root) {
        return Err(Error::refused(
            "source lock and fresh signed compiler tree disagree",
        ));
    }
    let modes = crate::store::declared_modes(payload)
        .map_err(|e| Error::io("read selected compiler modes", &e))?;
    if crate::tree::tree_root_declared(payload, &modes)
        .map_err(|e| Error::io("authenticate selected compiler tree", &e))?
        != tree_root
    {
        return Err(Error::refused(
            "selected compiler payload differs from its signed tree; no compiler ran",
        ));
    }
    options.check()?;
    let bin = payload.join("bin");
    let admission =
        crate::lineage::check_compiler_with(&bin, crate::lineage::LineagePolicy::Require, &|p| {
            probe_version(probe, p, options)
        });
    options.check()?;
    let payload_compiler = admission.map_err(|why| Error::refused(why.join("\n")))?;
    if let Some(lock) = lock {
        if payload_compiler.content.version.commit != lock.compiler_commit {
            return Err(Error::refused("source lock names another compiler commit"));
        }
        let lineage = crate::lineage::LineageSummary::read(payload).map_err(Error::refused)?;
        if lineage.compiler_commit != lock.compiler_commit || lock.receipt != lineage.receipt {
            return Err(Error::refused(
                "source lock names another compiler lineage receipt",
            ));
        }
    }
    Ok((tree_root, payload_compiler))
}

fn probe_version(
    probe: &VersionProbe<'_>,
    path: &Path,
    options: Options,
) -> std::result::Result<String, String> {
    options.check().map_err(|error| error.to_string())?;
    let result = probe(path, options.deadline);
    options.check().map_err(|error| error.to_string())?;
    result
}
