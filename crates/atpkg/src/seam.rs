// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rustup toolchain seam management.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::Layout;

/// Compiled-in allowlist of rustup toolchain names atpkg owns.
pub const SEAM_NAMES: &[&str] = &["trust", "trust-dev"];

/// Default seam attached when nothing is recorded yet.
pub const DEFAULT_SEAM: &str = "trust";

/// Store program a rustup seam points into.
pub const SEAM_PROGRAM: &str = "trust";

/// Fix printed whenever attach refuses an entry atpkg does not own.
pub use crate::toolchain::DETACH_FIX;

const RECORD_PREFIX: &str = "rustup:";

/// `seams` prefix for a refused re-assertion (`refused:rustup:<name>: <why>`).
pub const REFUSED_PREFIX: &str = "refused:";

/// `seams` prefix for a stale foreign link replaced by re-assertion.
pub const REPLACED_PREFIX: &str = "replaced:";

/// `seams` prefix for a replacement in flight (`replacing:rustup:<name>: <target>`).
pub const REPLACING_PREFIX: &str = "replacing:";

/// Whether `name` is on the allowlist.
#[must_use]
/// Whether `name` is on [`SEAM_NAMES`].
pub fn name_allowed(name: &str) -> bool {
    SEAM_NAMES.contains(&name)
}

/// The `status.toml` spelling of a seam: `rustup:<name>`.
#[must_use]
/// `status.toml` key for a seam (`rustup:<name>`).
pub fn record_key(name: &str) -> String {
    let mut k = String::from(RECORD_PREFIX);
    k.push_str(name);
    k
}

struct Head<'a>(&'a str);

impl fmt::Display for Head<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.0.strip_prefix(RECORD_PREFIX).unwrap_or(self.0);
        write!(f, "rustup `{name}`")
    }
}

/// Rustup's home as this process sees it: `$RUSTUP_HOME`, else `<home>/.rustup`.
#[must_use]
/// Rustup's home as this process sees it (`$RUSTUP_HOME`, else `<home>/.rustup`).
pub fn rustup_home() -> Option<PathBuf> {
    rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
    )
}

/// Pure core of [`rustup_home`] with explicit environment and home inputs.
pub use aterm_toolchain::rustup::rustup_home_with;

/// `<rustup_home>/toolchains` — present iff rustup is installed.
#[must_use]
/// `<rustup_home>/toolchains` directory path.
pub fn toolchains_dir(rustup_home: &Path) -> PathBuf {
    rustup_home.join("toolchains")
}

/// `<rustup_home>/toolchains/<name>` — the seam entry.
#[must_use]
/// `<rustup_home>/toolchains/<name>` seam entry path.
pub fn seam_path(rustup_home: &Path, name: &str) -> PathBuf {
    toolchains_dir(rustup_home).join(name)
}

/// `<prefix>/store/trust` — the store tree a seam laid before the view existed resolves into.
#[must_use]
/// `<prefix>/store/trust` root recognized as atpkg-owned.
pub fn owned_root(layout: &Layout) -> PathBuf {
    layout.prefix.join("store").join(SEAM_PROGRAM)
}

/// `<prefix>/store/trust/current` — the build every view mirrors.
#[must_use]
/// `<prefix>/store/trust/current` build path mirrored by views.
pub fn store_current(layout: &Layout) -> PathBuf {
    layout.program_current(SEAM_PROGRAM)
}

/// `<prefix>/rustup/` — every view sits under it.
#[must_use]
/// `<prefix>/rustup/` root containing all views.
pub fn views_root(layout: &Layout) -> PathBuf {
    layout.prefix.join(aterm_toolchain::layout::VIEWS_DIR)
}

/// `<prefix>/rustup/<name>/` — the view the seam `name` targets.
#[must_use]
/// `<prefix>/rustup/<name>/` view directory for seam `name`.
pub fn view_dir(layout: &Layout, name: &str) -> PathBuf {
    views_root(layout).join(name)
}

/// What the seam `name` targets: its view.
#[must_use]
/// Target path (`<prefix>/rustup/<name>/`) for seam `name`.
pub fn seam_target(layout: &Layout, name: &str) -> PathBuf {
    view_dir(layout, name)
}

/// Stock binary names and the Trust tools they clone in a view.
pub const STOCK_NAMES: &[(&str, &str)] = &[
    ("rustc", "trustc"),
    ("cargo", "targo"),
    ("rustdoc", "trustdoc"),
];

const VIEW_DIRS: &[&str] = &["lib", "libexec", "share", "etc"];

fn mirror_tree(layout: &Layout, src: &Path, dst: &Path) -> io::Result<()> {
    layout.ensure_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to)?;
            #[cfg(windows)]
            if std::fs::metadata(&from)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                std::os::windows::fs::symlink_dir(&target, &to)?;
            } else {
                std::os::windows::fs::symlink_file(&target, &to)?;
            }
        } else if kind.is_dir() {
            mirror_tree(layout, &from, &to)?;
        } else if kind.is_file() {
            crate::clone::clone_file(&from, &to)?;
        }
    }
    Ok(())
}

/// What [`refresh_view`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of [`refresh_view`].
pub struct Refreshed {
    /// Numbered build named by `store/trust/current`.
    pub build: PathBuf,
    /// Count of tools laid under their own names.
    pub tools: usize,
    /// Stock names laid and the Trust tool each clones.
    pub stock: Vec<(&'static str, &'static str)>,
    /// Whether the view's `bin/` changed.
    pub changed: bool,
    /// Reason the view was left as-is when it did not match `build` ([`Deferred`]).
    pub deferred: Option<Deferred>,
}

/// Why [`refresh_view`] left an out-of-date view as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Why [`refresh_view`] left an out-of-date view as-is.
pub enum Deferred {
    /// Live process executable running from the view.
    InUse(PathBuf),
    /// Process table could not be read to rule out a running build.
    Unknown,
    /// Active lease holders on the view ([`crate::lease`]).
    Leased(crate::lease::Holders),
}

impl Deferred {
    const fn once(&self) -> &'static str {
        match self {
            Deferred::InUse(_) => "once that process has exited",
            Deferred::Unknown => "once the process table can be read",
            Deferred::Leased(_) => "once that run has ended",
        }
    }
}

impl fmt::Display for Deferred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Deferred::InUse(exe) => write!(f, "{} is running from it", exe.display()),
            Deferred::Unknown => f.write_str(
                "the process table could not be read, so a build running from it cannot be \
                 ruled out",
            ),
            Deferred::Leased(holders) => write!(f, "it is {}", holders.clause()),
        }
    }
}

/// How closely [`view_matches`] holds a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Verification depth for [`view_matches`] and [`crate::compat`].
pub enum Depth {
    /// Checks `bin/` name-to-file mappings and stock tool clones.
    Shallow,
    /// Checks [`Depth::Shallow`] plus every entry under mirrored directories.
    Deep,
}

struct BinPlan {
    /// Name in the view -> the store file it must be a clone of.
    entries: std::collections::BTreeMap<std::ffi::OsString, PathBuf>,
    /// Regular files in the build's `bin/`.
    tools: usize,
    /// Stock names laid, each with its Trust tool.
    stock: Vec<(&'static str, &'static str)>,
}

impl BinPlan {
    fn refreshed(self, build: PathBuf, changed: bool, deferred: Option<Deferred>) -> Refreshed {
        Refreshed {
            build,
            tools: self.tools,
            stock: self.stock,
            changed,
            deferred,
        }
    }
}

fn bin_plan(src_bin: &Path) -> io::Result<BinPlan> {
    let mut entries = std::collections::BTreeMap::new();
    let mut tools = 0usize;
    for entry in std::fs::read_dir(src_bin)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        entries.insert(entry.file_name(), entry.path());
        tools += 1;
    }
    let mut stock = Vec::new();
    for (public, trust) in STOCK_NAMES {
        let Some(trust_tool) = entries.get(std::ffi::OsStr::new(trust)).cloned() else {
            continue;
        };
        entries.insert(std::ffi::OsString::from(public), trust_tool);
        stock.push((*public, *trust));
    }
    Ok(BinPlan {
        entries,
        tools,
        stock,
    })
}

/// Lay a view's `bin/` at `dst_bin` (created here, empty) from the build's `bin/`.
/// # Errors
/// The build's `bin/` cannot be listed, or a clone cannot be made (the store on another volume: it
/// refuses rather than byte-copy the toolchain).
fn lay_bin(layout: &Layout, src_bin: &Path, dst_bin: &Path) -> io::Result<BinPlan> {
    let plan = bin_plan(src_bin)?;
    layout.ensure_dir(dst_bin)?;
    for (name, src) in &plan.entries {
        crate::clone::clone_file(src, &dst_bin.join(name))?;
    }
    Ok(plan)
}

/// Lays a complete view of `build` at `dest` ([`lay_view_dirs`] then `bin/`).
/// # Errors
/// Returns an error if `dest` exists or laying directories or binaries fails.
#[cfg(unix)]
pub(crate) fn lay_view(layout: &Layout, build: &Path, dest: &Path) -> io::Result<()> {
    lay_view_dirs(layout, build, dest)?;
    lay_bin(layout, &build.join("bin"), &dest.join("bin"))?;
    Ok(())
}

/// Lays [`lay_view`] without `bin/` for [`crate::compat::ensure_root`].
/// # Errors
/// Returns an error if `dest` exists or mirroring directories fails.
#[cfg(unix)]
pub(crate) fn lay_view_dirs(layout: &Layout, build: &Path, dest: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; a view is laid only at a free name",
                dest.display()
            ),
        ));
    }
    layout.ensure_dir(dest)?;
    for dir in VIEW_DIRS {
        let src = build.join(dir);
        if src.is_dir() {
            mirror_tree(layout, &src, &dest.join(dir))?;
        }
    }
    Ok(())
}

/// Whether the view at `view` still is `build` at `depth` ([`first_mismatch`] finds nothing).
#[must_use]
/// Whether `view` matches `build` at `depth` ([`first_mismatch`] is `None`).
pub(crate) fn view_matches(build: &Path, view: &Path, depth: Depth) -> bool {
    first_mismatch(build, view, depth).is_none()
}

/// The first path at which the view at `view` stops being `build` at `depth`.
#[must_use]
/// First path where `view` mismatches `build` at `depth`, or `None` if it matches.
pub(crate) fn first_mismatch(build: &Path, view: &Path, depth: Depth) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if !is_real_dir(&view_tree(view)) {
            return Some(view.to_path_buf());
        }
        bin_mismatch(build, view)
            .or_else(|| {
                VIEW_DIRS
                    .iter()
                    .find_map(|dir| dir_mismatch(build, view, dir, depth))
            })
            .or_else(|| match depth {
                Depth::Deep => stock_bytes_mismatch(build, view),
                Depth::Shallow => None,
            })
    }
    #[cfg(not(unix))]
    {
        let _ = (build, depth);
        Some(view.to_path_buf())
    }
}

/// [`first_mismatch`] over `bin/` alone.
#[cfg(unix)]
/// [`first_mismatch`] over `bin/` alone for a real directory `view`.
pub(crate) fn bin_mismatch(build: &Path, view: &Path) -> Option<PathBuf> {
    let view_bin = view.join("bin");
    if !is_real_dir(&view_bin) {
        return Some(view_bin);
    }
    let Ok(plan) = bin_plan(&build.join("bin")) else {
        return Some(build.join("bin"));
    };
    for (name, src) in &plan.entries {
        let at = view_bin.join(name);
        if !crate::clone::is_clone_of(src, &at) {
            return Some(at);
        }
    }
    let Ok(listing) = std::fs::read_dir(&view_bin) else {
        return Some(view_bin);
    };
    for entry in listing {
        match entry {
            Ok(e) if plan.entries.contains_key(&e.file_name()) => {}
            Ok(e) => return Some(e.path()),
            Err(_) => return Some(view_bin),
        }
    }
    None
}

/// [`Depth::Deep`]'s byte check over the stock names.
#[cfg(unix)]
/// [`Depth::Deep`] byte check over [`STOCK_NAMES`] in `view`.
pub(crate) fn stock_bytes_mismatch(build: &Path, view: &Path) -> Option<PathBuf> {
    for (public, trust) in STOCK_NAMES {
        let tool = build.join("bin").join(trust);
        if !std::fs::symlink_metadata(&tool).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let at = view.join("bin").join(public);
        if !matches!(crate::clone::same_bytes(&tool, &at), Ok(true)) {
            return Some(at);
        }
    }
    None
}

/// Elsewhere than Unix the view is not compared (see [`bin_mismatch`]).
#[cfg(not(unix))]
/// Non-Unix stub for [`stock_bytes_mismatch`] (see [`bin_mismatch`]).
pub(crate) fn stock_bytes_mismatch(_build: &Path, _view: &Path) -> Option<PathBuf> {
    None
}

/// Elsewhere than Unix the view is not compared: `bin/` always reads as differing.
#[cfg(not(unix))]
/// Non-Unix stub for [`bin_mismatch`] (`bin/` always reports a mismatch).
pub(crate) fn bin_mismatch(_build: &Path, view: &Path) -> Option<PathBuf> {
    Some(view.join("bin"))
}

/// Elsewhere than Unix the view is not compared: every directory reads as differing.
#[cfg(not(unix))]
fn dir_mismatch(_build: &Path, view: &Path, dir: &str, _depth: Depth) -> Option<PathBuf> {
    Some(view.join(dir))
}

/// The half of [`first_mismatch`] over one of [`VIEW_DIRS`].
#[cfg(unix)]
fn dir_mismatch(build: &Path, view: &Path, dir: &str, depth: Depth) -> Option<PathBuf> {
    let src = build.join(dir);
    let at = view.join(dir);
    if !src.is_dir() {
        return std::fs::symlink_metadata(&at).is_ok().then_some(at);
    }
    if !is_real_dir(&at) {
        return Some(at);
    }
    match depth {
        Depth::Shallow if dir == "lib" => top_files_mismatch(&src, &at),
        Depth::Shallow => None,
        Depth::Deep => tree_mismatch(&src, &at),
    }
}

/// Whether `lstat` reports a real directory, excluding symlinks and Windows
/// reparse points such as junctions.
pub(crate) fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !crate::platform::is_reparse(&m))
}

/// The directory that holds the view at `view`: `<name>.noindex` when `view` is its link
/// ([`aterm_toolchain::layout`]'s Spotlight layout), else `view` itself. Any other link at
/// the view's name is itself, which no reader takes for a view.
pub(crate) fn view_tree(view: &Path) -> PathBuf {
    aterm_toolchain::layout::aside_of(view)
        .filter(|_| aterm_toolchain::layout::is_linked_aside(view))
        .unwrap_or_else(|| view.to_path_buf())
}

/// [`Depth::Shallow`] over `lib/`: every regular file directly in `src` a clone of it in `dst`.
#[cfg(unix)]
fn top_files_mismatch(src: &Path, dst: &Path) -> Option<PathBuf> {
    let Ok(listing) = std::fs::read_dir(src) else {
        return Some(src.to_path_buf());
    };
    for entry in listing {
        let Ok(entry) = entry else {
            return Some(src.to_path_buf());
        };
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if !crate::clone::is_clone_of(&from, &to) {
                return Some(to);
            }
        }
    }
    None
}

/// [`Depth::Deep`] over one mirrored directory.
#[cfg(unix)]
fn tree_mismatch(src: &Path, dst: &Path) -> Option<PathBuf> {
    let Ok(listing) = std::fs::read_dir(src) else {
        return Some(src.to_path_buf());
    };
    let mut expected = std::collections::BTreeSet::new();
    for entry in listing {
        let Ok(entry) = entry else {
            return Some(src.to_path_buf());
        };
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            return Some(from);
        };
        if kind.is_symlink() {
            let same = matches!(
                (std::fs::read_link(&from), std::fs::read_link(&to)),
                (Ok(a), Ok(b)) if a == b
            );
            if !same {
                return Some(to);
            }
        } else if kind.is_dir() {
            if !is_real_dir(&to) {
                return Some(to);
            }
            if let Some(found) = tree_mismatch(&from, &to) {
                return Some(found);
            }
        } else if kind.is_file() {
            if !crate::clone::is_clone_of(&from, &to) {
                return Some(to);
            }
        } else {
            continue;
        }
        expected.insert(entry.file_name());
    }
    let Ok(listing) = std::fs::read_dir(dst) else {
        return Some(dst.to_path_buf());
    };
    for entry in listing {
        match entry {
            Ok(e) if expected.contains(&e.file_name()) => {}
            Ok(e) => return Some(e.path()),
            Err(_) => return Some(dst.to_path_buf()),
        }
    }
    None
}

/// What a view cannot be laid without, as [`build_lacks`] finds it in the store's build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildLack {
    /// Nothing holds a tree at its name: removed, or its link names an aside that is gone.
    Tree,
    /// Something other than its own tree stands at its name, such as a link to another tree.
    OwnTree,
    /// It has no `bin/`, or has lost a tool its ready record names.
    Bin,
    /// It has no `lib/rustlib`, the sysroot its `trustc` runs on.
    Lib,
}

impl BuildLack {
    fn what(self) -> &'static str {
        match self {
            Self::Tree => "is gone",
            Self::OwnTree => "is not a tree of its own",
            Self::Bin => "has lost its bin/ or a tool in it",
            Self::Lib => "has no lib/rustlib",
        }
    }

    /// What lays the build again. A build without its tree or a recorded tool reads as not
    /// installed, so an install stages it. One that keeps them reads as complete
    /// ([`crate::store::build_is_complete`] sees only `bin/`), so until the channel's pin
    /// moves an update or an install leaves it as current, and only a reinstall stages it.
    /// An uninstall also retires the machine's toolset adoption, which `--default-set` records
    /// again and a plain `install trust` does not.
    fn remedy(self) -> &'static str {
        match self {
            Self::Tree | Self::Bin => "`aterm pkg install trust` lays it again",
            Self::OwnTree | Self::Lib => {
                "an update or an install leaves a build that reads as installed: `aterm pkg \
                 uninstall trust`, which removes every Trust build and rustup's `trust` until \
                 the install finishes, then `aterm pkg install --default-set`, lays it again"
            }
        }
    }
}

/// What keeps a view from being laid from the store build `build`, or `None`: the build must
/// stand ([`crate::store::build_stands`]), hold a `bin/` with every tool its ready record names,
/// and ship `lib/rustlib`, without which its `trustc` has no sysroot.
///
/// Asked of the build alone, before anything of the view moves, because a lay drops each part
/// its build lacks: right for a whole build with no `etc/`, ruinous for one that lost what it
/// runs on. A view already stripped still hears why. A part is asked as the lay asks it,
/// following a link.
///
/// # Errors
/// A stat failed other than by the path being absent, which says nothing of the build.
fn build_lacks(build: &Path) -> io::Result<Option<BuildLack>> {
    if !crate::store::build_stands(build)? {
        return Ok(Some(if crate::store::tree_absent(build) {
            BuildLack::Tree
        } else {
            BuildLack::OwnTree
        }));
    }
    let missing = |part: &Path| match std::fs::metadata(build.join(part)) {
        Ok(meta) => Ok(!meta.is_dir()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(true)
        }
        Err(e) => Err(e),
    };
    if missing(Path::new("bin"))? || crate::store::lost_a_recorded_tool(build) {
        return Ok(Some(BuildLack::Bin));
    }
    if missing(&Path::new("lib").join("rustlib"))? {
        return Ok(Some(BuildLack::Lib));
    }
    Ok(None)
}

/// Builds or refreshes the view for `name` from `store/trust/current`.
/// # Errors
/// Returns an error if `store/trust/current` is unreadable, its build lacks what the view needs
/// ([`build_lacks`], the view left as it stands), or the view cannot be written or cloned.
pub fn refresh_view(layout: &Layout, name: &str) -> io::Result<Refreshed> {
    refresh_view_with(layout, name, &crate::gc::process_table)
}

/// [`refresh_view`] with injected process enumeration (`running`, [`crate::gc::running_from`]).
pub(crate) fn refresh_view_with(
    layout: &Layout,
    name: &str,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> io::Result<Refreshed> {
    settle_view_aside(layout, name);
    sweep_view_debris(&view_dir(layout, name));
    #[cfg(unix)]
    if let ViewSource::Linked(checkout) = view_source(layout) {
        return refresh_linked_view(layout, name, &checkout, running);
    }
    let current = store_current(layout);
    let build = verified_store_current_build(layout, &current)?;
    let src_bin = build.join("bin");
    let view = view_dir(layout, name);
    if let Some(lacks) = build_lacks(&build)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            Refusal::NoBuild { build, lacks }.to_string(),
        ));
    }
    layout.ensure_dir(&views_root(layout))?;
    // A view that is not laid aside yet is laid whole, aside, when it is laid at all.
    let lay_whole =
        crate::store::lays_aside(&build) && !aterm_toolchain::layout::is_linked_aside(&view);
    let absent = std::fs::symlink_metadata(&view).is_err();
    if !(lay_whole && absent) {
        layout.ensure_dir(&view_tree(&view))?;
    }
    if !absent && view_matches(&build, &view, Depth::Deep) {
        return Ok(bin_plan(&src_bin)?.refreshed(build, false, None));
    }
    if let Some(deferred) = relay_blocked(layout, name, &view, running) {
        return Ok(bin_plan(&src_bin)?.refreshed(build, false, Some(deferred)));
    }
    #[cfg(unix)]
    if lay_whole {
        return lay_view_aside(layout, &build, &view);
    }
    let pid = std::process::id().to_string();
    for dir in VIEW_DIRS {
        let src = build.join(dir);
        let at = view.join(dir);
        let old = view.join(format!(".{dir}.old-{pid}"));
        if dir_mismatch(&build, &view, dir, Depth::Deep).is_none() {
            continue;
        }
        let _ = std::fs::remove_dir_all(&old);
        if !src.is_dir() {
            // Not in this build: drop whatever an older build left under the name.
            if std::fs::symlink_metadata(&at).is_ok() {
                std::fs::rename(&at, &old)?;
                let _ = std::fs::remove_dir_all(&old);
                crate::platform::remove_link(&old);
            }
            continue;
        }
        let staged = view.join(format!(".{dir}.tmp-{pid}"));
        let _ = std::fs::remove_dir_all(&staged);
        mirror_tree(layout, &src, &staged)?;
        swap_in(&staged, &at, &old)?;
    }
    let live = view.join("bin");
    let stock_differed = stock_bytes_mismatch(&build, &view).is_some();
    if bin_mismatch(&build, &view).is_none() && !stock_differed {
        return Ok(bin_plan(&src_bin)?.refreshed(build, false, None));
    }
    let staged = view.join(format!(".bin.tmp-{pid}"));
    let _ = std::fs::remove_dir_all(&staged);
    let plan = lay_bin(layout, &src_bin, &staged)?;
    let changed = stock_differed || !same_bin(&live, &staged);
    let old = view.join(format!(".bin.old-{pid}"));
    let _ = std::fs::remove_dir_all(&old);
    swap_in(&staged, &live, &old)?;
    Ok(plan.refreshed(build, changed, None))
}

/// Lay the whole view of `build` at `<view>.noindex` and put it in place behind the link
/// `view` -> `<view>.noindex` ([`aterm_toolchain::layout`]): where no view stands yet, or over
/// a plain view this refresh replaces anyway, which nothing runs from ([`relay_blocked`]).
/// The view is laid at `.<name>.lay-<pid>` and renamed whole to its aside, so an aside is
/// always complete and nothing runs from one half laid; the plain view goes only once the
/// link stands, and a crash anywhere leaves what [`settle_view_aside`] puts in order.
#[cfg(unix)]
fn lay_view_aside(layout: &Layout, build: &Path, view: &Path) -> io::Result<Refreshed> {
    let (Some(aside), Some(name)) = (
        aterm_toolchain::layout::aside_of(view),
        view.file_name().and_then(|n| n.to_str()),
    ) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no view", view.display()),
        ));
    };
    let plan = bin_plan(&build.join("bin"))?;
    // No link names whatever is at the aside's name: no view is laid aside yet.
    if std::fs::symlink_metadata(&aside).is_ok() {
        crate::store::remove_tree(&aside)?;
    }
    let staged = view.with_file_name(format!(".{name}{LAYING}{}", std::process::id()));
    if std::fs::symlink_metadata(&staged).is_ok() {
        crate::store::remove_tree(&staged)?;
    }
    if let Err(e) = lay_view(layout, build, &staged).and_then(|()| std::fs::rename(&staged, &aside))
    {
        let _ = crate::store::remove_tree(&staged);
        return Err(e);
    }
    let plain = std::fs::symlink_metadata(view).is_ok();
    let changed = !plain
        || stock_bytes_mismatch(build, view).is_some()
        || !same_bin(&view.join("bin"), &aside.join("bin"));
    let parked = parked_view(view, name);
    if plain {
        let _ = std::fs::remove_dir_all(&parked);
        crate::platform::remove_link(&parked);
        if let Err(e) = std::fs::rename(view, &parked) {
            let _ = crate::store::remove_tree(&aside);
            return Err(e);
        }
    }
    if let Err(e) = crate::store::link_aside(view) {
        if plain {
            let _ = std::fs::rename(&parked, view);
        }
        let _ = crate::store::remove_tree(&aside);
        return Err(e);
    }
    // Accepted, failed or not: the new view's link stands, and a view a power cut takes
    // back is put in order by settle_view_aside and laid again from its build.
    if let Some(root) = view.parent() {
        let _linked = crate::store::sync_dir_ordered(root);
    }
    if plain {
        let _ = std::fs::remove_dir_all(&parked);
        crate::platform::remove_link(&parked);
    }
    Ok(plan.refreshed(build.to_path_buf(), changed, None))
}

/// `.<name>.old-<pid>` beside the view `view`: where [`lay_view_aside`] parks the plain view it
/// replaces until the new one's link stands.
#[cfg(unix)]
fn parked_view(view: &Path, name: &str) -> PathBuf {
    view.with_file_name(format!(".{name}{PARKED}{}", std::process::id()))
}

/// The infix of `.<name>.lay-<pid>`, where [`lay_view_aside`] lays a view before renaming it
/// whole to `<name>.noindex`.
const LAYING: &str = ".lay-";

/// The infix of `.<name>.old-<pid>` ([`parked_view`]).
const PARKED: &str = ".old-";

/// Put the Spotlight layout of the view `name` in order after a crash
/// ([`lay_view_aside`]): an aside (always complete) with no link at the view's name gets it,
/// an aside beside a plain view (a lay cut short before the move) goes, and so do a view half
/// laid and a plain view parked by an earlier lay.
fn settle_view_aside(layout: &Layout, name: &str) {
    let view = view_dir(layout, name);
    crate::store::relink_aside(&view);
    if let Some(aside) = aterm_toolchain::layout::aside_of(&view)
        && std::fs::symlink_metadata(&view).is_ok_and(|m| m.is_dir())
        && std::fs::symlink_metadata(&aside).is_ok()
    {
        let _ = crate::store::remove_tree(&aside);
    }
    let Ok(entries) = std::fs::read_dir(views_root(layout)) else {
        return;
    };
    let stems = [format!(".{name}{LAYING}"), format!(".{name}{PARKED}")];
    for entry in entries.flatten() {
        let debris = entry.file_name().to_str().is_some_and(|n| {
            stems.iter().any(|stem| {
                n.strip_prefix(stem.as_str())
                    .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            })
        });
        if debris {
            let path = entry.path();
            let _ = std::fs::remove_dir_all(&path);
            crate::platform::remove_link(&path);
        }
    }
}

fn swap_in(staged: &Path, at: &Path, old: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(at).is_ok() {
        std::fs::rename(at, old)?;
    }
    std::fs::rename(staged, at)?;
    let _ = std::fs::remove_dir_all(old);
    crate::platform::remove_link(old);
    Ok(())
}

fn relay_blocked(
    layout: &Layout,
    name: &str,
    view: &Path,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> Option<Deferred> {
    let laid = std::iter::once("bin")
        .chain(VIEW_DIRS.iter().copied())
        .any(|part| std::fs::symlink_metadata(view.join(part)).is_ok());
    if !laid {
        return None;
    }
    if let Some(subject) = crate::lease::Subject::view(name) {
        let holders = crate::lease::holders(&layout.prefix, &subject);
        if holders.in_use() {
            return Some(Deferred::Leased(holders));
        }
    }
    match crate::gc::running_from(view, running) {
        Some(None) => None,
        Some(Some(exe)) => Some(Deferred::InUse(exe)),
        None => Some(Deferred::Unknown),
    }
}

fn is_view_debris(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((stem, rest)) = rest.split_once('.') else {
        return false;
    };
    if stem != "bin" && !VIEW_DIRS.contains(&stem) {
        return false;
    }
    let Some(pid) = rest
        .strip_prefix("tmp-")
        .or_else(|| rest.strip_prefix("old-"))
    else {
        return false;
    };
    !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
}

fn sweep_view_debris(view: &Path) {
    let Ok(entries) = std::fs::read_dir(view) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(is_view_debris) {
            continue;
        }
        let path = entry.path();
        let _ = std::fs::remove_dir_all(&path);
        crate::platform::remove_link(&path);
    }
}

/// What the view presents: the store's live build, or a dev-linked checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Source presented by a view.
pub enum ViewSource {
    /// Installed build at `store/trust/current`.
    Store,
    /// Dev-linked checkout with a valid sysroot (`bin/` and `lib/`).
    Linked(PathBuf),
    /// Dev-linked checkout without a valid sysroot.
    LinkedNoSysroot(PathBuf),
}

/// Which source the view presents now.
#[must_use]
/// Returns the current [`ViewSource`] for `layout`.
pub fn view_source(layout: &Layout) -> ViewSource {
    if !cfg!(unix) {
        return ViewSource::Store;
    }
    let Some(checkout) = crate::linkmode::linked_checkout(layout, SEAM_PROGRAM) else {
        return ViewSource::Store;
    };
    let inside_prefix = normalize(&checkout).starts_with(normalize(&layout.prefix));
    if !inside_prefix && checkout.join("bin").is_dir() && checkout.join("lib").is_dir() {
        ViewSource::Linked(checkout)
    } else {
        ViewSource::LinkedNoSysroot(checkout)
    }
}

/// Whether the view at `view` already presents `checkout` as [`refresh_linked_view`] lays it.
#[cfg(unix)]
fn linked_view_matches(checkout: &Path, view: &Path, plan: &BinPlan) -> bool {
    for dir in VIEW_DIRS {
        let src = checkout.join(dir);
        let at = view.join(dir);
        if src.is_dir() {
            if !std::fs::read_link(&at).is_ok_and(|t| t == src) {
                return false;
            }
        } else if std::fs::symlink_metadata(&at).is_ok() {
            return false;
        }
    }
    let bin = view.join("bin");
    is_real_dir(&bin) && bin_holds_exactly(&bin, plan)
}

/// The stubs a linked view's `bin/` at `dst_bin` holds for `plan`.
#[cfg(unix)]
fn linked_stubs(dst_bin: &Path, plan: &BinPlan) -> io::Result<Vec<crate::lay::Executable>> {
    plan.entries
        .iter()
        .map(|(entry, target)| {
            crate::platform::shim_executable_to_env(
                &dst_bin.join(entry),
                target,
                &crate::shim_env::ShimEnv::NONE,
            )
        })
        .collect()
}

/// Whether `bin` holds exactly the stubs of `plan`.
#[cfg(unix)]
fn bin_holds_exactly(bin: &Path, plan: &BinPlan) -> bool {
    let Ok(want) = linked_stubs(bin, plan) else {
        return false;
    };
    let Ok(listing) = std::fs::read_dir(bin) else {
        return false;
    };
    let mut seen = 0usize;
    for entry in listing.flatten() {
        let path = entry.path();
        let Some(stub) = want.iter().find(|e| e.path == path) else {
            return false;
        };
        let have = crate::metadata_io::read_bounded_regular(&path, crate::platform::MAX_SHIM_BYTES);
        if !have.is_ok_and(|bytes| bytes == stub.body) {
            return false;
        }
        seen += 1;
    }
    seen == want.len()
}

/// Move `at` (a directory, a link, anything) aside under `view` and remove it.
#[cfg(unix)]
fn take_down(view: &Path, at: &Path, stem: &str, pid: &str) -> io::Result<()> {
    if std::fs::symlink_metadata(at).is_err() {
        return Ok(());
    }
    let old = view.join(format!(".{stem}.old-{pid}"));
    let _ = std::fs::remove_dir_all(&old);
    crate::platform::remove_link(&old);
    std::fs::rename(at, &old)?;
    let _ = std::fs::remove_dir_all(&old);
    crate::platform::remove_link(&old);
    Ok(())
}

/// Lay the view for a dev-linked `checkout`.
#[cfg(unix)]
fn refresh_linked_view(
    layout: &Layout,
    name: &str,
    checkout: &Path,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> io::Result<Refreshed> {
    let view = view_dir(layout, name);
    layout.ensure_dir(&views_root(layout))?;
    layout.ensure_dir(&view_tree(&view))?;
    let src_bin = checkout.join("bin");
    let plan = bin_plan(&src_bin)?;
    if linked_view_matches(checkout, &view, &plan) {
        return Ok(plan.refreshed(checkout.to_path_buf(), false, None));
    }
    if let Some(deferred) = relay_blocked(layout, name, &view, running) {
        return Ok(plan.refreshed(checkout.to_path_buf(), false, Some(deferred)));
    }
    let pid = std::process::id().to_string();
    let mut changed = false;
    for dir in VIEW_DIRS {
        let src = checkout.join(dir);
        let at = view.join(dir);
        if src.is_dir() {
            if std::fs::read_link(&at).is_ok_and(|t| t == src) {
                continue;
            }
            take_down(&view, &at, dir, &pid)?;
            crate::activate::atomic_symlink(&src, &at)?;
        } else {
            if std::fs::symlink_metadata(&at).is_err() {
                continue;
            }
            take_down(&view, &at, dir, &pid)?;
        }
        changed = true;
    }
    let live = view.join("bin");
    if is_real_dir(&live) && bin_holds_exactly(&live, &plan) {
        return Ok(plan.refreshed(checkout.to_path_buf(), changed, None));
    }
    let staged = view.join(format!(".bin.tmp-{pid}"));
    let _ = std::fs::remove_dir_all(&staged);
    crate::platform::remove_link(&staged);
    layout.ensure_dir(&staged)?;
    linked_stubs(&staged, &plan)?
        .iter()
        .try_for_each(crate::lay::write_in_process)?;
    {
        take_down(&view, &live, "bin", &pid)?;
        std::fs::rename(&staged, &live)?;
        changed = true;
    }
    Ok(plan.refreshed(checkout.to_path_buf(), changed, None))
}

fn same_bin(live: &Path, staged: &Path) -> bool {
    #[cfg(unix)]
    {
        type Attrs = (u64, std::fs::Permissions, Option<std::time::SystemTime>);
        let map = |dir: &Path| -> Option<std::collections::BTreeMap<std::ffi::OsString, Attrs>> {
            let mut out = std::collections::BTreeMap::new();
            for entry in std::fs::read_dir(dir).ok()? {
                let entry = entry.ok()?;
                let meta = entry.metadata().ok()?;
                out.insert(
                    entry.file_name(),
                    (meta.len(), meta.permissions(), meta.modified().ok()),
                );
            }
            Some(out)
        };
        matches!((map(live), map(staged)), (Some(a), Some(b)) if a == b)
    }
    #[cfg(not(unix))]
    {
        let _ = (live, staged);
        false
    }
}

/// What sits at the seam path, by `lstat` — never following a link.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Filesystem entry kind at the seam path by `lstat`.
pub enum Entry {
    /// Nothing at the path.
    Absent,
    /// Symlink (or Windows junction) with its raw target.
    Link(PathBuf),
    /// Real directory.
    Dir,
    /// Regular file.
    File,
    /// Any other filesystem entry kind.
    Other,
}

impl Entry {
    /// Formats what was found at the entry for refusal diagnostics.
    pub(crate) fn describe(&self, layout: &Layout) -> String {
        match self {
            Entry::Absent => "absent".to_string(),
            Entry::Link(raw) => format!(
                "a symlink to {} outside {} and {}",
                raw.display(),
                views_root(layout).display(),
                owned_root(layout).display()
            ),
            Entry::Dir => "a real directory".to_string(),
            Entry::File => "a regular file".to_string(),
            Entry::Other => "neither a symlink nor a directory".to_string(),
        }
    }
}

fn inspect(path: &Path) -> io::Result<Entry> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Entry::Absent),
        Err(e) => return Err(e),
    };
    if crate::platform::is_reparse(&meta) {
        return std::fs::read_link(path).map(Entry::Link);
    }
    if meta.is_dir() {
        Ok(Entry::Dir)
    } else if meta.is_file() {
        Ok(Entry::File)
    } else {
        Ok(Entry::Other)
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(crate) fn absolute_target(raw: &Path, link: &Path) -> PathBuf {
    if raw.is_absolute() {
        return normalize(raw);
    }
    let base = link.parent().unwrap_or_else(|| Path::new(""));
    normalize(&base.join(raw))
}

fn resolves_into_prefix(layout: &Layout, abs: &Path) -> bool {
    [views_root(layout), owned_root(layout)].iter().any(|root| {
        let root = normalize(root);
        if abs.starts_with(&root) {
            return true;
        }
        match (std::fs::canonicalize(abs), std::fs::canonicalize(&root)) {
            (Ok(a), Ok(r)) => a.starts_with(&r),
            _ => false,
        }
    })
}

fn targets_view(layout: &Layout, name: &str, abs: &Path) -> bool {
    abs == normalize(&seam_target(layout, name))
}

/// Everything the verbs decide on, gathered by one `lstat` + `readlink`.
#[derive(Debug, Clone, PartialEq, Eq)]
/// `lstat` and `readlink` inspection of a seam entry.
pub struct Probe {
    /// `<rustup_home>/toolchains/<name>`.
    pub path: PathBuf,
    /// Entry kind at `path`.
    pub entry: Entry,
    /// Absolute normalized link target, if `entry` is a link.
    pub target: Option<PathBuf>,
    /// Whether the link target lies inside `<prefix>/rustup/` or `<prefix>/store/trust/`.
    pub in_prefix: bool,
    /// Whether the link target is `<prefix>/rustup/<name>`.
    pub targets_view: bool,
}

/// Inspects the seam entry for `name` without modifying the filesystem.
/// # Errors
/// Returns the `lstat`/`readlink` error when the entry exists but cannot be read.
pub fn probe(layout: &Layout, rustup_home: &Path, name: &str) -> io::Result<Probe> {
    let path = seam_path(rustup_home, name);
    let entry = inspect(&path)?;
    let target = match &entry {
        Entry::Link(raw) => Some(absolute_target(raw, &path)),
        _ => None,
    };
    let in_prefix = target
        .as_deref()
        .is_some_and(|t| resolves_into_prefix(layout, t));
    let targets_view = target
        .as_deref()
        .is_some_and(|t| targets_view(layout, name, t));
    Ok(Probe {
        path,
        entry,
        target,
        in_prefix,
        targets_view,
    })
}

/// What an `attach` did.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of an `attach` operation.
pub enum Attached {
    /// `<rustup_home>/toolchains` is absent; rustup is not installed.
    NoRustup { key: String, toolchains: PathBuf },
    /// Absent entry was created as a link to the view.
    Created {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// Existing link to the view was adopted unchanged.
    Adopted {
        key: String,
        path: PathBuf,
        target: PathBuf,
        view_build: Option<PathBuf>,
    },
    /// Store link was repointed to the view.
    Repointed {
        key: String,
        path: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    /// Stale foreign link was replaced with a link to the view.
    ReplacedStale {
        key: String,
        path: PathBuf,
        from: PathBuf,
        to: PathBuf,
        stale: Stale,
    },
    /// Entry due for repointing was left untouched because a process runs from it.
    LeftEntry {
        key: String,
        path: PathBuf,
        from: PathBuf,
        to: PathBuf,
        why: Deferred,
    },
}

impl Attached {
    /// Whether the seam is now recorded and live.
    #[cfg(test)]
    #[must_use]
    /// Whether the seam is now recorded and live.
    pub(crate) fn is_live(&self) -> bool {
        !matches!(self, Attached::NoRustup { .. } | Attached::LeftEntry { .. })
    }

    /// Whether this attach changed the filesystem (what a silent pass reports).
    #[must_use]
    /// Whether this attach modified the filesystem.
    pub const fn changed(&self) -> bool {
        matches!(
            self,
            Attached::Created { .. }
                | Attached::Repointed { .. }
                | Attached::ReplacedStale { .. }
                | Attached::Adopted {
                    view_build: Some(_),
                    ..
                }
        )
    }
}

impl Attached {
    fn key(&self) -> &str {
        match self {
            Attached::NoRustup { key, .. }
            | Attached::Created { key, .. }
            | Attached::Adopted { key, .. }
            | Attached::Repointed { key, .. }
            | Attached::ReplacedStale { key, .. }
            | Attached::LeftEntry { key, .. } => key,
        }
    }
}

impl fmt::Display for Attached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", Head(self.key()))?;
        match self {
            Attached::NoRustup { key: _, toolchains } => write!(
                f,
                "no rustup ({} absent) — nothing to attach",
                toolchains.display()
            ),
            Attached::Created {
                key: _,
                path,
                target,
            } => write!(f, "attached {} -> {}", path.display(), target.display()),
            Attached::Adopted {
                key: _,
                path,
                target,
                view_build: None,
            } => write!(
                f,
                "adopted {} (already -> {})",
                path.display(),
                target.display()
            ),
            Attached::Adopted {
                key: _,
                path,
                target,
                view_build: Some(build),
            } => write!(
                f,
                "{} -> {} now presents {}",
                path.display(),
                target.display(),
                build.display()
            ),
            Attached::Repointed {
                key: _,
                path,
                from,
                to,
            } => write!(
                f,
                "re-pointed {} -> {} (was {}, inside the store)",
                path.display(),
                to.display(),
                from.display()
            ),
            Attached::ReplacedStale {
                key: _,
                path,
                from,
                to,
                stale,
            } => {
                write!(
                    f,
                    "re-pointed {} -> {} (was {}, {stale})",
                    path.display(),
                    to.display(),
                    from.display()
                )?;
                if matches!(stale, Stale::Older { .. }) {
                    write!(f, " — {}", put_back_hint(from, path))?;
                }
                Ok(())
            }
            Attached::LeftEntry {
                key: _,
                path,
                from,
                to,
                why,
            } => {
                let once = why.once();
                write!(
                    f,
                    "{} was left pointing at {}, not {} — {why}; `aterm pkg repair` re-points \
                     it {once}; so does the next pass",
                    path.display(),
                    from.display(),
                    to.display()
                )
            }
        }
    }
}

/// Why a foreign link's toolchain is stale beside the store's trust build.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Reason a foreign link's toolchain is stale relative to the store's trust build.
pub enum Stale {
    /// Link target does not exist.
    Dangling,
    /// Foreign toolchain `commit-date` is older than the store build's.
    Older { its: String, store: String },
}

impl fmt::Display for Stale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stale::Dangling => f.write_str("which no longer exists"),
            Stale::Older { its, store } => write!(
                f,
                "a toolchain from {its}, older than the store's {store}; it is left where it is"
            ),
        }
    }
}

/// Whether the toolchain at `sysroot` is stale beside the store's current trust build.
#[must_use]
/// Checks whether the toolchain at `sysroot` is missing or older than the store build.
pub fn stale_against_store(layout: &Layout, sysroot: &Path) -> Option<Stale> {
    if view_source(layout) != ViewSource::Store {
        return None;
    }
    let store = verified_store_current_build(layout, &store_current(layout)).ok()?;
    std::fs::metadata(&store).ok()?;
    match std::fs::metadata(sysroot) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(Stale::Dangling),
        Err(_) => return None,
        Ok(_) => {}
    }
    let its = commit_date(sysroot)?;
    let store = commit_date(&store)?;
    (its < store).then_some(Stale::Older { its, store })
}

fn commit_date(sysroot: &Path) -> Option<String> {
    let bin = sysroot.join("bin");
    ["trustc", "rustc"].iter().find_map(|name| {
        let out =
            crate::doctor::output_bounded(aterm_uds::spawnfd::command(bin.join(name)).arg("-vV"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let date = text
            .lines()
            .find_map(|l| l.strip_prefix("commit-date: "))?
            .trim();
        let digits = date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
        (out.status.success() && date.len() == 10 && digits).then(|| date.to_string())
    })
}

/// Whether `sysroot` is a live rustc build tree's stage sysroot.
#[must_use]
/// Whether `sysroot` is a stage sysroot inside a live `rustc` build tree.
pub fn live_build_tree(sysroot: &Path) -> bool {
    stage_shaped(&normalize(sysroot))
        || std::fs::canonicalize(sysroot).is_ok_and(|real| stage_shaped(&real))
}

fn stage_shaped(path: &Path) -> bool {
    let mut tail = path.components().rev().map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let (Some(Some(stage)), Some(Some(host)), Some(Some(build))) =
        (tail.next(), tail.next(), tail.next())
    else {
        return false;
    };
    build == "build"
        && matches!(stage, "stage0" | "stage1" | "stage2")
        && (host == "host" || target_triple(host))
}

fn target_triple(s: &str) -> bool {
    s.split('-').count() >= 3
        && s.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        })
}

/// What an unattended re-assertion does with a foreign link ([`unattended_heal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Heal {
    /// Re-point it: an older live build tree.
    Stale(Stale),
    /// Leave it, and say it was put back ([`Refusal::PutBack`]).
    PutBack,
    /// Leave it, refused as foreign as ever.
    No,
}

fn unattended_heal(layout: &Layout, name: &str, entry: &Path, target: &Path) -> Heal {
    if name != DEFAULT_SEAM || target.to_str().is_none() || !live_build_tree(target) {
        return Heal::No;
    }
    let Some(stale @ Stale::Older { .. }) = stale_against_store(layout, target) else {
        return Heal::No;
    };
    if replaced_from(layout, name)
        .is_some_and(|from| same_tree(&absolute_target(&from, entry), target))
    {
        return Heal::PutBack;
    }
    Heal::Stale(stale)
}

fn same_tree(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

fn put_back_hint(from: &Path, path: &Path) -> String {
    format!(
        "to put it back: ln -sfn {} {} (no unattended pass re-points it again; `aterm pkg \
         repair` does)",
        crate::platform::sh_quote_str(&from.to_string_lossy()),
        crate::platform::sh_quote_str(&path.to_string_lossy())
    )
}

fn replaced_prefix(name: &str) -> String {
    let mut k = String::from(REPLACED_PREFIX);
    k.push_str(&record_key(name));
    k.push_str(": ");
    k
}

fn replacing_prefix(name: &str) -> String {
    let mut k = String::from(REPLACING_PREFIX);
    k.push_str(&record_key(name));
    k.push_str(": ");
    k
}

/// The link target, as written, that a re-assertion last replaced at `name`, or `None`.
#[must_use]
/// Link target last replaced by a re-assertion at `name`.
pub fn replaced_from(layout: &Layout, name: &str) -> Option<PathBuf> {
    let prefix = replaced_prefix(name);
    recorded_keys(layout)
        .iter()
        .find_map(|k| k.strip_prefix(&prefix).map(PathBuf::from))
}

fn set_replacing(layout: &Layout, name: &str, from: Option<&Path>) -> io::Result<()> {
    let prefix = replacing_prefix(name);
    let entry = from.map(|from| format!("{prefix}{}", from.to_string_lossy()));
    let mut s = crate::status::seed_for_rewrite(layout)?;
    if s.seams
        .iter()
        .filter(|k| k.starts_with(&prefix))
        .eq(entry.iter())
    {
        return Ok(());
    }
    s.seams.retain(|k| !k.starts_with(&prefix));
    if let Some(entry) = entry {
        s.seams.push(entry);
        s.seams.sort();
    }
    crate::status::write(layout, &s)
}

/// What became of the stale link a re-assertion replaced at `name`, as `doctor` prints it.
#[must_use]
/// Diagnostic note for `doctor` describing a stale link replaced at `name`.
pub fn replaced_note(layout: &Layout, rustup_home: &Path, name: &str) -> Option<String> {
    let from = replaced_from(layout, name)?;
    let p = probe(layout, rustup_home, name).ok()?;
    let target = p.target.as_deref()?;
    if p.targets_view {
        Some(format!(
            "rustup `{name}` was re-pointed at the store's view ({}) by aterm: it named {}, a \
             toolchain older than the store's (that tree is left where it is); {}",
            target.display(),
            from.display(),
            put_back_hint(&from, &p.path)
        ))
    } else if same_tree(&absolute_target(&from, &p.path), target) {
        Some(format!(
            "rustup `{name}` names {} again — put back after aterm re-pointed it at the store, \
             so no unattended pass re-points it; `aterm pkg repair` does",
            from.display()
        ))
    } else {
        None
    }
}

/// Why an `attach` or [`detach`] did not happen.
#[derive(Debug)]
/// Fail-closed reason an `attach` or [`detach`] was refused.
pub enum Refusal {
    /// Name is not on [`SEAM_NAMES`].
    BadName(String),
    /// `<prefix>/store/trust/current` does not exist.
    NotInstalled { target: PathBuf },
    /// `current` names a `build` no view can be laid from ([`BuildLack`]). The view keeps the
    /// clones it was last laid with, which still run.
    NoBuild { build: PathBuf, lacks: BuildLack },
    /// Dev-linked checkout is not a valid sysroot.
    LinkedNoSysroot { checkout: PathBuf },
    /// Entry exists and is not owned by atpkg (fix: [`DETACH_FIX`]).
    Foreign { path: PathBuf, what: String },
    /// Entry points back to the stale target previously replaced ([`replaced_from`]).
    PutBack { path: PathBuf, target: PathBuf },
    /// Filesystem error.
    Io(io::Error),
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::Io(e)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::BadName(n) => write!(
                f,
                "{n:?} is not a seam atpkg owns (allowed: {})",
                SEAM_NAMES.join(", ")
            ),
            Refusal::NotInstalled { target } => write!(
                f,
                "trust is not installed ({} absent) — nothing to point rustup `trust` at",
                target.display()
            ),
            Refusal::NoBuild { build, lacks } => write!(
                f,
                "the store's current trust build {} {} — rustup `trust` is left as it stands; {}",
                build.display(),
                lacks.what(),
                lacks.remedy()
            ),
            Refusal::LinkedNoSysroot { checkout } => write!(
                f,
                "trust is dev-linked to {}, which is not a sysroot atpkg can present (no bin/ \
                 and lib/, or a tree inside atpkg's own prefix) — rustup `trust` is left as \
                 it stands; `aterm pkg unlink trust` returns it to the installed build",
                checkout.display()
            ),
            Refusal::Foreign { path, what } => write!(
                f,
                "{} is {what} — not aterm's link; left untouched (fix: {DETACH_FIX})",
                path.display()
            ),
            Refusal::PutBack { path, target } => write!(
                f,
                "{} names {} again, the stale toolchain aterm re-pointed it away from — put \
                 back since, so left as it stands; `aterm pkg repair` re-points it at the store",
                path.display(),
                target.display()
            ),
            Refusal::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Attaches seam `name` by refreshing its view, linking it into rustup, and recording it.
/// # Errors
/// Returns [`Refusal`] if `name` is disallowed, trust is missing, the entry is foreign, or I/O
/// fails.
#[cfg(test)]
pub(crate) fn attach(layout: &Layout, rustup_home: &Path, name: &str) -> Result<Attached, Refusal> {
    attach_reporting(
        layout,
        rustup_home,
        name,
        &refresh_view,
        &crate::gc::process_table,
        false,
    )
    .map(|(attached, _)| attached)
}

type LeftView = (PathBuf, Deferred);

type Refresh<'a> = &'a dyn Fn(&Layout, &str) -> io::Result<Refreshed>;

fn attach_reporting(
    layout: &Layout,
    rustup_home: &Path,
    name: &str,
    refresh: Refresh<'_>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
    repair: bool,
) -> Result<(Attached, Option<LeftView>), Refusal> {
    if !name_allowed(name) {
        return Err(Refusal::BadName(name.to_string()));
    }
    let key = record_key(name);
    let toolchains = toolchains_dir(rustup_home);
    if !toolchains.is_dir() {
        return Ok((Attached::NoRustup { key, toolchains }, None));
    }
    let target = seam_target(layout, name);
    let p = probe(layout, rustup_home, name)?;
    let replacing = replacing_prefix(name);
    if !p.targets_view
        && recorded_keys(layout)
            .iter()
            .any(|k| k.starts_with(&replacing))
    {
        set_replacing(layout, name, None)?;
    }
    let mut stale = None;
    if let Entry::Link(raw) = &p.entry
        && !p.in_prefix
    {
        if let Some(t) = p.target.as_deref() {
            stale = if repair {
                stale_against_store(layout, t)
            } else {
                match unattended_heal(layout, name, &p.path, t) {
                    Heal::Stale(stale) => Some(stale),
                    Heal::PutBack => {
                        return Err(Refusal::PutBack {
                            path: p.path,
                            target: raw.clone(),
                        });
                    }
                    Heal::No => None,
                }
            };
        }
        if stale.is_none() {
            return Err(Refusal::Foreign {
                what: Entry::Link(raw.clone()).describe(layout),
                path: p.path,
            });
        }
    }
    if !matches!(p.entry, Entry::Absent | Entry::Link(_)) {
        return Err(Refusal::Foreign {
            what: p.entry.describe(layout),
            path: p.path,
        });
    }
    match view_source(layout) {
        ViewSource::Linked(_) => {}
        ViewSource::LinkedNoSysroot(checkout) => {
            return Err(Refusal::LinkedNoSysroot { checkout });
        }
        ViewSource::Store => {
            let current = store_current(layout);
            if std::fs::symlink_metadata(&current).is_err() {
                return Err(Refusal::NotInstalled { target: current });
            }
            let build = verified_store_current_build(layout, &current)?;
            if let Some(lacks) = build_lacks(&build)? {
                return Err(Refusal::NoBuild { build, lacks });
            }
        }
    }
    let refreshed = refresh(layout, name)?;
    let left = refreshed
        .deferred
        .clone()
        .map(|why| (refreshed.build.clone(), why));
    let attached = match p.entry {
        Entry::Absent => {
            crate::activate::atomic_symlink(&target, &p.path)?;
            record(layout, name)?;
            Attached::Created {
                key,
                path: p.path,
                target,
            }
        }
        Entry::Link(_) if p.targets_view => {
            record(layout, name)?;
            Attached::Adopted {
                key,
                path: p.path,
                target,
                view_build: refreshed.changed.then_some(refreshed.build),
            }
        }
        Entry::Link(raw) => {
            let old = p
                .target
                .clone()
                .unwrap_or_else(|| absolute_target(&raw, &p.path));
            let same_build = left.is_none()
                && std::fs::canonicalize(&old)
                    .ok()
                    .zip(std::fs::canonicalize(&refreshed.build).ok())
                    .is_some_and(|(a, b)| a == b);
            let why = if same_build {
                None
            } else {
                match crate::gc::running_from(&old, running) {
                    Some(None) => None,
                    Some(Some(exe)) => Some(Deferred::InUse(exe)),
                    None => Some(Deferred::Unknown),
                }
            };
            if let Some(why) = why {
                return Ok((
                    Attached::LeftEntry {
                        key,
                        path: p.path,
                        from: raw,
                        to: target,
                        why,
                    },
                    left,
                ));
            }
            if std::fs::read_link(&p.path).ok().as_ref() != Some(&raw) {
                return Err(Refusal::Io(io::Error::other(format!(
                    "{} changed while this pass laid the view — left for the next pass",
                    p.path.display()
                ))));
            }
            let older = matches!(stale, Some(Stale::Older { .. }));
            if older {
                set_replacing(layout, name, Some(&raw))?;
            }
            if let Err(e) = crate::activate::atomic_symlink(&target, &p.path) {
                if older {
                    let _ = set_replacing(layout, name, None);
                }
                return Err(e.into());
            }
            record(layout, name)?;
            match stale {
                Some(stale) => Attached::ReplacedStale {
                    key,
                    path: p.path,
                    from: raw,
                    to: target,
                    stale,
                },
                None => Attached::Repointed {
                    key,
                    path: p.path,
                    from: raw,
                    to: target,
                },
            }
        }
        other => {
            return Err(Refusal::Foreign {
                what: other.describe(layout),
                path: p.path,
            });
        }
    };
    Ok((attached, left))
}

/// One thing a re-assertion ([`reassert`], [`repair`]) or the whole-set detach
/// ([`detach_recorded`]) did or declined at one seam.
#[derive(Debug)]
/// Single event from [`reassert`], [`repair`], or [`detach_recorded`].
pub enum SeamEvent {
    /// Attach that changed the filesystem ([`Attached::changed`]) or left an entry.
    Attached(Attached),
    /// View left as-is ([`Deferred`]) during refresh.
    ViewLeft {
        name: String,
        view: PathBuf,
        build: PathBuf,
        why: Deferred,
    },
    /// Attach or detach refused at `name`.
    Refused { name: String, refusal: Refusal },
    /// Outcome of a detach.
    Detached(Detached),
}

impl SeamEvent {
    /// The seam's name and the entry.
    #[must_use]
    /// Seam name and path when this event is a [`Refusal::Foreign`].
    pub fn foreign_entry(&self) -> Option<(&str, &Path)> {
        match self {
            SeamEvent::Refused {
                name,
                refusal: Refusal::Foreign { path, .. },
            } => Some((name, path)),
            _ => None,
        }
    }
}

impl fmt::Display for SeamEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SeamEvent::Attached(a) => fmt::Display::fmt(a, f),
            SeamEvent::ViewLeft {
                name,
                view,
                build,
                why,
            } => write!(
                f,
                "{}: {} was left as it stands, not re-laid onto {} — {why}; `cargo +{name}` \
                 runs the compiler it still holds; `aterm pkg repair` lays it {}; so does \
                 the next pass",
                Head(&record_key(name)),
                view.display(),
                build.display(),
                why.once()
            ),
            SeamEvent::Refused {
                name,
                refusal: refusal @ Refusal::Io(_),
            } => write!(f, "{}: {refusal}", Head(&record_key(name))),
            SeamEvent::Refused { refusal, .. } => fmt::Display::fmt(refusal, f),
            SeamEvent::Detached(d) => fmt::Display::fmt(d, f),
        }
    }
}

/// What a [`detach`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Outcome of [`detach`].
pub enum Detached {
    /// Rustup is not installed; only the record was dropped.
    NoRustup { key: String, toolchains: PathBuf },
    /// Entry was already absent; only the record was dropped.
    AlreadyAbsent { key: String, path: PathBuf },
    /// Owned link into the prefix was unlinked.
    Removed {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// Foreign link was unlinked under `--force`.
    ForcedUnlinked {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// Real directory or file was moved aside under `--force`.
    ForcedDisplaced {
        key: String,
        path: PathBuf,
        moved_to: PathBuf,
    },
}

impl Detached {
    fn key(&self) -> &str {
        match self {
            Detached::NoRustup { key, .. }
            | Detached::AlreadyAbsent { key, .. }
            | Detached::Removed { key, .. }
            | Detached::ForcedUnlinked { key, .. }
            | Detached::ForcedDisplaced { key, .. } => key,
        }
    }
}

impl fmt::Display for Detached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", Head(self.key()))?;
        match self {
            Detached::NoRustup { key: _, toolchains } => write!(
                f,
                "no rustup ({} absent) — record cleared",
                toolchains.display()
            ),
            Detached::AlreadyAbsent { key: _, path } => {
                write!(f, "{} already absent — record cleared", path.display())
            }
            Detached::Removed {
                key: _,
                path,
                target,
            } => write!(
                f,
                "detached {} (was -> {})",
                path.display(),
                target.display()
            ),
            Detached::ForcedUnlinked {
                key: _,
                path,
                target,
            } => write!(
                f,
                "--force unlinked {} (was -> {}, not aterm's)",
                path.display(),
                target.display()
            ),
            Detached::ForcedDisplaced {
                key: _,
                path,
                moved_to,
            } => write!(
                f,
                "--force moved {} aside to {} (not deleted)",
                path.display(),
                moved_to.display()
            ),
        }
    }
}

fn displaced_name(path: &Path, name: &str) -> PathBuf {
    let secs = aterm_time::unix_secs().unwrap_or(0);
    let mut file = String::from(name);
    file.push_str(".displaced-by-aterm-");
    file.push_str(&secs.to_string());
    file.push('-');
    file.push_str(&std::process::id().to_string());
    path.with_file_name(file)
}

fn unlink_checked(path: &Path) -> io::Result<()> {
    crate::platform::remove_link(path);
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(io::Error::other(format!(
            "{} still exists after unlink",
            path.display()
        ))),
    }
}

/// Detaches seam `name`, removing its owned link (or forcing removal) and dropping its record.
/// # Errors
/// Returns [`Refusal`] if `name` is disallowed, the entry is foreign without `force`, or I/O fails.
pub fn detach(
    layout: &Layout,
    rustup_home: &Path,
    name: &str,
    force: bool,
) -> Result<Detached, Refusal> {
    if !name_allowed(name) {
        return Err(Refusal::BadName(name.to_string()));
    }
    let key = record_key(name);
    let toolchains = toolchains_dir(rustup_home);
    if !toolchains.is_dir() {
        unrecord(layout, name)?;
        return Ok(Detached::NoRustup { key, toolchains });
    }
    let p = probe(layout, rustup_home, name)?;
    match p.entry {
        Entry::Absent => {
            unrecord(layout, name)?;
            Ok(Detached::AlreadyAbsent { key, path: p.path })
        }
        Entry::Link(raw) if p.in_prefix => {
            unlink_checked(&p.path)?;
            // The view whole: a view laid aside goes with its link.
            let _ = crate::store::remove_tree(&view_dir(layout, name));
            unrecord(layout, name)?;
            Ok(Detached::Removed {
                key,
                path: p.path,
                target: raw,
            })
        }
        Entry::Link(raw) => {
            if !force {
                return Err(Refusal::Foreign {
                    what: Entry::Link(raw).describe(layout),
                    path: p.path,
                });
            }
            unlink_checked(&p.path)?;
            unrecord(layout, name)?;
            Ok(Detached::ForcedUnlinked {
                key,
                path: p.path,
                target: raw,
            })
        }
        other => {
            if !force {
                return Err(Refusal::Foreign {
                    what: other.describe(layout),
                    path: p.path,
                });
            }
            let aside = displaced_name(&p.path, name);
            std::fs::rename(&p.path, &aside)?;
            unrecord(layout, name)?;
            Ok(Detached::ForcedDisplaced {
                key,
                path: p.path,
                moved_to: aside,
            })
        }
    }
}

/// The one-line report `seam status` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Single-seam status report for `seam status`.
pub struct SeamStatus {
    /// `rustup:<name>`.
    pub key: String,
    /// Whether `status.toml` records the seam.
    pub recorded: bool,
    /// `<rustup_home>/toolchains/<name>`.
    pub path: PathBuf,
    /// Whether `<rustup_home>/toolchains` exists.
    pub rustup_present: bool,
    /// Entry kind at `path`, or the read error.
    pub entry: Result<Entry, String>,
    /// Absolute link target, if `entry` is a link.
    pub target: Option<PathBuf>,
    /// Whether the link target lies inside `<prefix>/rustup/` or `<prefix>/store/trust/`.
    pub in_prefix: bool,
    /// Whether the link target is `<prefix>/rustup/<name>`.
    pub targets_view: bool,
}

impl fmt::Display for SeamStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let yn = |b: bool| if b { "yes" } else { "no" };
        let target = match (&self.entry, &self.target) {
            (Ok(Entry::Link(_)), Some(t)) => t.display().to_string(),
            (Ok(Entry::Dir), _) => "(a real directory)".to_string(),
            (Ok(Entry::File), _) => "(a regular file)".to_string(),
            (Ok(Entry::Other), _) => "(not a symlink)".to_string(),
            (Ok(Entry::Absent), _) => "(absent)".to_string(),
            (Err(e), _) => format!("(error: {e})"),
            (Ok(Entry::Link(_)), None) => "(absent)".to_string(),
        };
        write!(
            f,
            "{}: recorded={} path={} target={target} in-prefix={} targets-view={}",
            self.key,
            yn(self.recorded),
            self.path.display(),
            yn(self.in_prefix),
            yn(self.targets_view)
        )?;
        if !self.rustup_present {
            write!(
                f,
                " (no rustup: {} absent)",
                toolchains_dir_of(&self.path).display()
            )?;
        }
        Ok(())
    }
}

fn toolchains_dir_of(seam: &Path) -> &Path {
    seam.parent().unwrap_or(seam)
}

/// Inspect the seam `name` and its record, changing nothing.
#[must_use]
/// Inspects seam `name` and its record without modifying anything.
pub fn status(layout: &Layout, rustup_home: &Path, name: &str) -> SeamStatus {
    let key = record_key(name);
    let recorded = recorded_keys(layout).contains(&key);
    let rustup_present = toolchains_dir(rustup_home).is_dir();
    match probe(layout, rustup_home, name) {
        Ok(p) => SeamStatus {
            key,
            recorded,
            path: p.path,
            rustup_present,
            entry: Ok(p.entry),
            target: p.target,
            in_prefix: p.in_prefix,
            targets_view: p.targets_view,
        },
        Err(e) => SeamStatus {
            key,
            recorded,
            path: seam_path(rustup_home, name),
            rustup_present,
            entry: Err(e.to_string()),
            target: None,
            in_prefix: false,
            targets_view: false,
        },
    }
}

/// Every `seams` key `status.toml` records (empty when there is no record).
#[must_use]
/// All `seams` keys recorded in `status.toml`.
pub fn recorded_keys(layout: &Layout) -> Vec<String> {
    crate::status::read(layout)
        .map(|s| s.seams)
        .unwrap_or_default()
}

/// The recorded rustup seam names on the allowlist.
#[must_use]
/// Recorded rustup seam names on [`SEAM_NAMES`].
pub fn recorded_names(layout: &Layout) -> Vec<String> {
    recorded_keys(layout)
        .iter()
        .filter_map(|k| k.strip_prefix(RECORD_PREFIX))
        .filter(|n| name_allowed(n))
        .map(str::to_string)
        .collect()
}

/// The `seams` keys the disk itself proves.
#[must_use]
/// Allowlisted rustup seam keys currently pointing into this prefix on disk.
pub fn attached_keys(layout: &Layout) -> Vec<String> {
    let Some(home) = rustup_home() else {
        return Vec::new();
    };
    SEAM_NAMES
        .iter()
        .filter(|name| status(layout, &home, name).in_prefix)
        .map(|name| record_key(name))
        .collect()
}

fn record(layout: &Layout, name: &str) -> io::Result<()> {
    let key = record_key(name);
    let (replacing, replaced) = (replacing_prefix(name), replaced_prefix(name));
    let mut s = crate::status::seed_for_rewrite(layout)?;
    let settled = s
        .seams
        .iter()
        .find_map(|k| k.strip_prefix(&replacing))
        .map(|from| format!("{replaced}{from}"));
    if s.seams.contains(&key) && settled.is_none() {
        return Ok(());
    }
    if let Some(settled) = settled {
        s.seams
            .retain(|k| !k.starts_with(&replacing) && !k.starts_with(&replaced));
        s.seams.push(settled);
    }
    s.seams.push(key);
    s.seams.sort();
    s.seams.dedup();
    crate::status::write(layout, &s)
}

fn unrecord(layout: &Layout, name: &str) -> io::Result<()> {
    let key = record_key(name);
    let refused = refusal_prefix(name);
    let replaced = replaced_prefix(name);
    let replacing = replacing_prefix(name);
    let Some(mut s) = crate::status::read(layout) else {
        return Ok(());
    };
    let before = s.seams.len();
    s.seams.retain(|k| {
        *k != key
            && !k.starts_with(&refused)
            && !k.starts_with(&replaced)
            && !k.starts_with(&replacing)
    });
    if s.seams.len() == before {
        return Ok(());
    }
    crate::status::write(layout, &s)
}

/// The `seams` key of a refusal for `name`: `refused:rustup:<name>`.
#[must_use]
/// `seams` refusal key for `name` (`refused:rustup:<name>`).
pub fn refused_key(name: &str) -> String {
    let mut k = String::from(REFUSED_PREFIX);
    k.push_str(&record_key(name));
    k
}

fn refusal_prefix(name: &str) -> String {
    let mut k = refused_key(name);
    k.push_str(": ");
    k
}

/// Every recorded refusal.
#[cfg(all(test, unix))]
#[must_use]
/// All recorded refusals as `(name, why)` pairs.
pub(crate) fn refusals(layout: &Layout) -> Vec<(String, String)> {
    recorded_keys(layout)
        .iter()
        .filter_map(|k| {
            let rest = k
                .strip_prefix(REFUSED_PREFIX)?
                .strip_prefix(RECORD_PREFIX)?;
            let (name, why) = rest.split_once(": ")?;
            Some((name.to_string(), why.to_string()))
        })
        .collect()
}

fn record_refusal(layout: &Layout, name: &str, why: &str) -> io::Result<()> {
    let key = refused_key(name);
    let mut entry = key.clone();
    entry.push_str(": ");
    entry.push_str(why);
    let mut s = crate::status::seed_for_rewrite(layout)?;
    if s.seams.contains(&entry) {
        return Ok(());
    }
    let mine = refusal_prefix(name);
    s.seams.retain(|k| !k.starts_with(&mine));
    s.seams.push(entry);
    s.seams.sort();
    crate::status::write(layout, &s)
}

fn clear_refusal(layout: &Layout, name: &str) -> io::Result<()> {
    let key = refusal_prefix(name);
    let Some(mut s) = crate::status::read(layout) else {
        return Ok(());
    };
    let before = s.seams.len();
    s.seams.retain(|k| !k.starts_with(&key));
    if s.seams.len() == before {
        return Ok(());
    }
    crate::status::write(layout, &s)
}

/// Re-assert every recorded seam.
#[must_use]
/// Re-asserts all recorded seams plus [`DEFAULT_SEAM`] when trust is available.
pub fn reassert(layout: &Layout, rustup_home: &Path) -> Vec<SeamEvent> {
    reassert_with(
        layout,
        rustup_home,
        &refresh_view,
        &crate::gc::process_table,
        false,
    )
}

/// [`reassert`] for `aterm pkg repair`.
#[must_use]
/// [`reassert`] for `aterm pkg repair`, also replacing stale foreign links once.
pub fn repair(layout: &Layout, rustup_home: &Path) -> Vec<SeamEvent> {
    reassert_with(
        layout,
        rustup_home,
        &refresh_view,
        &crate::gc::process_table,
        true,
    )
}

fn reassert_with(
    layout: &Layout,
    rustup_home: &Path,
    refresh: Refresh<'_>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
    repair: bool,
) -> Vec<SeamEvent> {
    let mut events = Vec::new();
    if !toolchains_dir(rustup_home).is_dir() {
        crate::quiet::clear_views_left(layout);
        return events;
    }
    let mut names: std::collections::BTreeSet<String> =
        recorded_names(layout).into_iter().collect();
    if !names.contains(DEFAULT_SEAM)
        && (std::fs::symlink_metadata(store_current(layout)).is_ok()
            || matches!(view_source(layout), ViewSource::Linked(_)))
    {
        names.insert(DEFAULT_SEAM.to_string());
    }
    for name in names {
        match attach_reporting(layout, rustup_home, &name, refresh, running, repair) {
            Ok((a, left)) => {
                if !matches!(a, Attached::LeftEntry { .. }) {
                    let _ = clear_refusal(layout, &name);
                }
                let said = a.changed() || matches!(a, Attached::LeftEntry { .. });
                if let Attached::ReplacedStale {
                    key,
                    path,
                    from,
                    to,
                    stale,
                } = &a
                {
                    let undo = match stale {
                        Stale::Older { .. } => put_back_hint(from, path),
                        Stale::Dangling => String::new(),
                    };
                    crate::packages_log::append(
                        layout,
                        &crate::packages_log::Event::Seam {
                            seam: key,
                            from: &from.display().to_string(),
                            to: &to.display().to_string(),
                            why: &stale.to_string(),
                            undo: &undo,
                        },
                    );
                }
                if said {
                    events.push(SeamEvent::Attached(a));
                }
                if let Some((build, why)) = left {
                    crate::quiet::note_view_left(
                        layout,
                        &name,
                        &build,
                        &why.to_string(),
                        crate::flow::now_unix(),
                    );
                    events.push(SeamEvent::ViewLeft {
                        view: view_dir(layout, &name),
                        name: name.clone(),
                        build,
                        why,
                    });
                } else {
                    crate::quiet::clear_view_left(layout, &name);
                }
            }
            Err(refusal) => {
                if !matches!(refusal, Refusal::Io(_)) {
                    let _ = record_refusal(layout, &name, &refusal.to_string());
                    crate::quiet::clear_view_left(layout, &name);
                }
                events.push(SeamEvent::Refused { name, refusal });
            }
        }
    }
    events
}

/// Detach every recorded seam (never `--force`): the whole-set removal's companion.
#[must_use]
/// Detaches every recorded seam without `--force`.
pub fn detach_recorded(layout: &Layout, rustup_home: &Path) -> Vec<SeamEvent> {
    recorded_names(layout)
        .into_iter()
        .map(|name| match detach(layout, rustup_home, &name, false) {
            Ok(d) => SeamEvent::Detached(d),
            Err(refusal) => SeamEvent::Refused { name, refusal },
        })
        .collect()
}

fn verified_store_current_build(layout: &Layout, current: &Path) -> io::Result<PathBuf> {
    let build = std::fs::read_link(current).map(|raw| absolute_target(&raw, current))?;
    if !crate::ops::store_build_of(&layout.prefix, &build)
        .is_some_and(|(owner, _)| owner == SEAM_PROGRAM)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} -> {} is outside store/{SEAM_PROGRAM}/<build>",
                current.display(),
                build.display()
            ),
        ));
    }
    Ok(build)
}

#[cfg(test)]
#[path = "seam_tests.rs"]
mod tests;
