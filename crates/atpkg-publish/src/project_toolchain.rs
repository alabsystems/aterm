// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One authenticated, leased compiler selection for a project or an exact package source.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use atpkg::trust_lock::{self, TrustLock};

use crate::artifact::tools::{self, Tools};
use crate::ctx::Ctx;
use crate::error::{Error, Result};

/// The explicit authority whose compiler is selected. A missing committed lock means the
/// authenticated managed default; the default project runner requires a working lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authority {
    /// Native adopt's not-yet-committed lock, or an ordinary pinned working tree.
    Working { path: PathBuf, lock: TrustLock },
    /// The lock read from the package's admitted full source commit, never checkout HEAD.
    Committed {
        repo: PathBuf,
        source: String,
        lock: Option<TrustLock>,
    },
}

impl Authority {
    /// Read a required lock at the root the caller names (never a parent's lock).
    /// # Errors
    /// A missing, nonregular, unreadable or malformed lock.
    pub fn working(repo: &Path) -> Result<Self> {
        let repo = std::fs::canonicalize(repo)
            .map_err(|e| Error::io("resolve project lock authority", &e))?;
        let path = repo.join(trust_lock::LOCK_FILE);
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return Err(Error::refused(
                "the project requires a regular trust.lock at its root",
            ));
        }
        let lock = trust_lock::read(&path).map_err(Error::refused)?;
        Ok(Self::Working { path, lock })
    }

    /// Read only the exact source's root lock after native local-source admission.
    /// # Errors
    /// Source admission, malformed full SHA, non-blob lock, incomplete objects or malformed lock.
    pub fn committed(repo: &Path, source: &str) -> Result<Self> {
        if !agh::path::is_sha(source) {
            return Err(Error::refused(
                "package source must be a full lowercase Git object id",
            ));
        }
        let repo = crate::git::SourceRepo::open(repo)?;
        if repo.query(&["cat-file", "-t", source])? != "commit\n" {
            return Err(Error::refused("package source object must be a commit"));
        }
        let listing = repo.query(&["ls-tree", "-z", source, "--", trust_lock::LOCK_FILE])?;
        let lock = if listing.is_empty() {
            None
        } else {
            let row = listing
                .strip_suffix('\0')
                .ok_or_else(|| Error::refused("unframed source lock"))?;
            let (head, name) = row
                .split_once('\t')
                .ok_or_else(|| Error::refused("malformed source lock entry"))?;
            let words: Vec<_> = head.split_whitespace().collect();
            if name != trust_lock::LOCK_FILE
                || words.len() != 3
                || words[0] != "100644"
                || words[1] != "blob"
            {
                return Err(Error::refused(
                    "the committed trust.lock must be a regular source blob",
                ));
            }
            let text = repo.query(&["cat-file", "blob", words[2]])?;
            Some(trust_lock::parse(&text).map_err(Error::refused)?)
        };
        Ok(Self::Committed {
            repo: repo.root().to_path_buf(),
            source: source.to_string(),
            lock,
        })
    }

    fn recheck(&self, controller_repo: &Path) -> Result<()> {
        let root = std::fs::canonicalize(controller_repo)
            .map_err(|e| Error::io("resolve controller authority", &e))?;
        let observed = match self {
            Self::Working { path, .. } if *path == root.join(trust_lock::LOCK_FILE) => {
                Self::working(&root)?
            }
            Self::Committed { repo, source, .. } if *repo == root => {
                Self::committed(&root, source)?
            }
            _ => {
                return Err(Error::refused(
                    "controller repository differs from its compiler authority",
                ));
            }
        };
        if observed != *self {
            return Err(Error::refused("controller compiler authority changed"));
        }
        Ok(())
    }

    fn lock(&self) -> Option<&TrustLock> {
        match self {
            Self::Working { lock, .. } => Some(lock),
            Self::Committed { lock, .. } => lock.as_ref(),
        }
    }
}

/// Store and environment inputs, captured once before a compiler can run.
pub struct Inputs {
    /// Managed store prefix.
    pub prefix: PathBuf,
    /// Working tree whose compiler-moving Cargo configuration is checked.
    pub repo: PathBuf,
    /// PATH behind the selected compiler.
    pub path: OsString,
    /// Cargo's configuration home.
    pub cargo_home: PathBuf,
    /// Compiler override environment, including the existing rustc mover vocabulary.
    pub overrides: Vec<(String, String)>,
}

impl Inputs {
    /// Capture the current environment for `repo`, without selecting a compiler.
    #[must_use]
    pub fn process(ctx: &Ctx, repo: &Path) -> Self {
        let keys = atpkg::toolchain::rustc_movers::RUSTC_VARS
            .iter()
            .copied()
            .chain([
                "TRUST_STAGE2_BIN",
                "ALAB_CARGO",
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "RUSTDOC",
                "CARGO_BUILD_RUSTDOC",
            ]);
        Self {
            prefix: ctx.pkg_prefix.clone(),
            repo: repo.to_path_buf(),
            path: std::env::var_os("PATH").unwrap_or_default(),
            cargo_home: std::env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| ctx.home.join(".cargo")),
            overrides: keys
                .filter_map(|key| {
                    std::env::var(key)
                        .ok()
                        .filter(|v| !v.is_empty())
                        .map(|v| (key.to_string(), v))
                })
                .collect(),
        }
    }
}

/// The payload and its execution root stay protected until the last child is reaped.
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
    compiler: atpkg::lineage::Compiler,
    /// The explicit source authority used for this selection.
    authority: Authority,
    layout: atpkg::Layout,
    _lease: atpkg::lease::Lease,
}

impl Selected {
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
    pub const fn compiler(&self) -> &atpkg::lineage::Compiler {
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
        if atpkg::lookup::payload_root(&self.layout, "trust", self.build).as_ref()
            != Some(&self.payload)
        {
            return Err(Error::refused(
                "selected compiler payload ownership changed",
            ));
        }
        let modes = atpkg::store::declared_modes(&self.payload)
            .map_err(|e| Error::io("read compiler declared modes", &e))?;
        let root = atpkg::tree::tree_root_declared(&self.payload, &modes)
            .map_err(|e| Error::io("hash selected compiler", &e))?;
        if root != self.tree_root {
            return Err(Error::refused("selected signed compiler payload changed"));
        }
        if self.execution != self.payload
            && atpkg::compat::blocked(&self.layout, self.build).is_some()
        {
            return Err(Error::refused("selected compiler compat ownership changed"));
        }
        if self.execution != self.payload
            && !atpkg::compat::root_matches(
                &trust_lock::build_dir(&self.layout.prefix, self.build),
                &self.execution,
                atpkg::seam::Depth::Deep,
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
/// `signed_root` is the native signed-head authority (in production `cli::signed_tree_root`).
/// # Errors
/// Missing/malformed lock, overrides, missing lease/build, signed content or lineage mismatch,
/// invalid compat root, or an explicit compiler other than this authority's selection.
pub fn select(
    inputs: &Inputs,
    authority: Authority,
    explicit: Option<&Path>,
    signed_root: &dyn Fn(u64) -> std::result::Result<String, String>,
    tools: &dyn Tools,
) -> Result<Selected> {
    if let Some((key, _)) = inputs.overrides.first() {
        return Err(Error::refused(format!(
            "{key} overrides project compiler selection; remove it"
        )));
    }
    if atpkg::toolchain::linked_tool_bin(&inputs.prefix, "targo").is_some() {
        return Err(Error::refused(
            "a dev-linked Targo overrides managed compiler selection",
        ));
    }
    let (build, requested_payload) = if let Some(lock) = authority.lock() {
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
    };
    let subject = atpkg::lease::Subject::build(trust_lock::PROGRAM, build)
        .ok_or_else(|| Error::refused("invalid managed Trust build"))?;
    let lease = atpkg::lease::take(
        &inputs.prefix,
        &subject,
        "atpkg-publish project toolchain",
        atpkg::lease::DEFAULT_WAIT,
    )
    .map_err(|e| Error::cannot_run(format!("lease selected Trust build: {e}")))?;
    let layout = atpkg::Layout {
        prefix: inputs.prefix.clone(),
    };
    let payload = atpkg::lookup::payload_root(&layout, "trust", build)
        .ok_or_else(|| Error::refused("selected compiler payload has unowned path indirection"))?;
    if std::fs::canonicalize(requested_payload).ok().as_ref() != Some(&payload) {
        return Err(Error::refused(
            "selected compiler path does not name its owned payload",
        ));
    }
    if !atpkg::store::build_is_complete(&trust_lock::build_dir(&inputs.prefix, build)) {
        return Err(Error::refused("selected Trust build is incomplete"));
    }
    let tree_root = signed_root(build).map_err(Error::refused)?;
    if authority
        .lock()
        .is_some_and(|lock| lock.tree_root != tree_root)
    {
        return Err(Error::refused(
            "source lock and fresh signed compiler tree disagree",
        ));
    }
    let modes = atpkg::store::declared_modes(&payload)
        .map_err(|e| Error::io("read selected compiler modes", &e))?;
    if atpkg::tree::tree_root_declared(&payload, &modes)
        .map_err(|e| Error::io("authenticate selected compiler tree", &e))?
        != tree_root
    {
        return Err(Error::refused(
            "selected compiler payload differs from its signed tree; no compiler ran",
        ));
    }
    let bin = payload.join("bin");
    let payload_compiler =
        atpkg::lineage::check_compiler_with(&bin, atpkg::lineage::LineagePolicy::Require, &|p| {
            tools::version(tools, p)
        })
        .map_err(|why| Error::refused(why.join("\n")))?;
    if let Some(lock) = authority.lock() {
        if payload_compiler.content.version.commit != lock.compiler_commit {
            return Err(Error::refused("source lock names another compiler commit"));
        }
        let lineage = crate::adopt::Lineage::read(&payload)?;
        if lineage.compiler_commit != lock.compiler_commit || lock.receipt != lineage.receipt {
            return Err(Error::refused(
                "source lock names another compiler lineage receipt",
            ));
        }
    }
    let execution = match atpkg::compat::needs_root(&payload)
        .map_err(|e| Error::io("inspect compiler execution layout", &e))?
    {
        None => payload.clone(),
        Some(_) => {
            if let Some(blocked) = atpkg::compat::blocked(&layout, build) {
                return Err(Error::refused(format!(
                    "selected compiler compat path: {blocked}"
                )));
            }
            let root = atpkg::compat::root_dir(&layout, build);
            if !atpkg::compat::root_matches(
                &trust_lock::build_dir(&inputs.prefix, build),
                &root,
                atpkg::seam::Depth::Deep,
            ) {
                return Err(Error::refused(
                    "required compiler compat root is missing or differs; repair it before running",
                ));
            }
            root
        }
    };
    if let Some(explicit) = explicit {
        let actual = std::fs::canonicalize(explicit)
            .map_err(|e| Error::io("resolve explicit compiler", &e))?;
        if actual != execution.join("bin") {
            return Err(Error::refused(
                "explicit compiler does not match the source lock or authenticated managed default",
            ));
        }
    }
    let movers = atpkg::toolchain::rustc_movers::rustc_movers(
        Some(&inputs.repo),
        &|_| None,
        Some(&inputs.cargo_home),
    );
    if !movers.is_empty() {
        return Err(Error::refused(
            "Cargo configuration overrides the selected project compiler",
        ));
    }
    let compiler = if execution == payload {
        payload_compiler
    } else {
        // Compat roots intentionally contain executable views, not a second LINEAGE.
        // Their bytes/mapping were checked deeply against the signed, proven payload.
        let content = atpkg::lineage::admit_content_with(&execution.join("bin"), &|p| {
            tools::version(tools, p)
        })
        .map_err(|why| Error::refused(why.join("\n")))?;
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
        atpkg::lineage::Compiler {
            content,
            lineage: payload_compiler.lineage,
        }
    };
    let selected = Selected {
        payload,
        execution,
        build,
        tree_root,
        compiler,
        authority,
        layout,
        _lease: lease,
    };
    selected.recheck()?;
    Ok(selected)
}

/// CLI syntax for the pinned, leased child runner.
pub const SYNOPSIS: &str = "toolchain run --repo DIR [--source FULL_SHA] -- COMMAND [args…]";

/// Run a pinned project's controller while one authenticated native lease remains held.
/// # Errors
/// Usage, compiler admission, child start/failure, changed lock or selected payload.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let [verb, flag, repo, tail @ ..] = args else {
        return Err(Error::usage(SYNOPSIS));
    };
    if verb != "run" || flag != "--repo" {
        return Err(Error::usage(SYNOPSIS));
    }
    let (source, command, rest) = match tail {
        [separator, command, rest @ ..] if separator == "--" => (None, command, rest),
        [flag, source, separator, command, rest @ ..]
            if flag == "--source" && separator == "--" =>
        {
            (Some(source), command, rest)
        }
        _ => return Err(Error::usage(SYNOPSIS)),
    };
    let repo = std::fs::canonicalize(repo).map_err(|e| Error::io("resolve project", &e))?;
    let authority = match source {
        Some(source) => Authority::committed(&repo, source)?,
        None => Authority::working(&repo)?,
    };
    let inputs = Inputs::process(ctx, &repo);
    let selected = select(
        &inputs,
        authority.clone(),
        None,
        &|build| atpkg::cli::signed_tree_root("trust", build),
        &tools::HostTools,
    )?;
    let front =
        std::env::current_exe().map_err(|e| Error::io("resolve native compiler front", &e))?;
    run_controller(&selected, &inputs, &front, command.as_ref(), rest)
}

/// Execute a controller under the already selected lease, retaining exact tools across cwd changes.
/// # Errors
/// Child start/status, changed source authority, or changed signed payload/execution mapping.
pub fn run_controller(
    selected: &Selected,
    inputs: &Inputs,
    front: &Path,
    command: &OsStr,
    args: &[String],
) -> Result<()> {
    selected.authority.recheck(&inputs.repo)?;
    selected.recheck()?;
    let bin = selected.execution.join("bin");
    let path = std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&inputs.path)),
    )
    .map_err(|e| Error::cannot_run(format!("selected compiler PATH: {e}")))?;
    eprintln!(
        "toolchain: Trust {} tree {} payload {} execution {}",
        selected.build,
        selected.tree_root,
        selected.payload.display(),
        selected.execution.display()
    );
    let mut child = aterm_uds::spawnfd::transparent(command).into_command();
    child
        .args(args)
        .current_dir(&inputs.repo)
        .env("PATH", path)
        .env("ATPKG_PROJECT_TARGO", selected.targo())
        .env("ATPKG_PROJECT_TRUSTC", bin.join("trustc"))
        .env("ATPKG_PROJECT_TRUSTDOC", bin.join("trustdoc"))
        .env("ATPKG_PROJECT_FRONT", front)
        .env("ATPKG_PROJECT_PAYLOAD", &selected.payload)
        .env("ATPKG_PROJECT_EXECUTION", &selected.execution);
    let outcome =
        atpkg::build_admit::supervise_controller(&mut child, &mut |line| eprintln!("{line}"));
    // Both authorities are observed after the native group drains, including a
    // failed child. Neither an SDK failure nor a child status skips the source read.
    let authority = selected.authority.recheck(&inputs.repo);
    let payload = selected.recheck();
    authority?;
    payload?;
    let outcome = outcome.map_err(|e| Error::io("supervise project compiler controller", &e))?;
    let status = outcome.root;
    if !status.success() || outcome.signal.is_some() {
        #[cfg(unix)]
        let code = {
            use std::os::unix::process::ExitStatusExt as _;
            status
                .code()
                .filter(|code| *code != 0)
                .or_else(|| status.signal().map(|signal| 128 + signal))
                .or_else(|| outcome.signal.map(|signal| 128 + signal))
        };
        #[cfg(not(unix))]
        let code = status
            .code()
            .filter(|code| *code != 0)
            .map(|code| code & 0xff)
            .or_else(|| outcome.signal.map(|signal| 128 + signal));
        return Err(Error::ChildExit {
            code: code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1),
            message: format!("project compiler controller ended {status}"),
        });
    }
    Ok(())
}
