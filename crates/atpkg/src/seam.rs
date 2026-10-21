// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! atpkg owns the rustup toolchain seam. `rust-toolchain.toml` pins a toolchain called
//! `trust`, which rustup finds at `<rustup_home>/toolchains/trust`; a hand-made
//! `rustup toolchain link` dangles whenever the tree under it moves, so the entry is a
//! symlink atpkg lays and re-asserts: `<rustup_home>/toolchains/<name>` ->
//! `<prefix>/rustup/<name>`, atpkg's view of the live trust build.
//!
//! # Why a view and not the store
//!
//! rustup's proxies and cargo's doctest phase resolve the stock names (`rustc`, `cargo`,
//! `rustdoc`); the Trust distribution ships only `trustc`, `targo` and `trustdoc`. A second
//! copy under a stock name differs inside its ad-hoc code signature on macOS (the signature
//! bakes in the file's name), so tippy refuses it, and the content-addressed store takes no
//! files after staging. The view's `bin/` holds a copy-on-write clone ([`crate::clone`]) of
//! each store tool under its own name plus each stock name as a clone of its Trust tool
//! ([`STOCK_NAMES`]). `lib/`, `libexec/`, `share/` and `etc/` are mirrored file by file, not
//! symlinked: rustc finds its sysroot through the real path of the driver dylib it loaded,
//! so a symlinked `lib/` makes `rustc --print sysroot` answer the store. The view is checked
//! on every attach and rebuilt only when it no longer matches ([`refresh_view`]): tippy pins
//! its own and its siblings' link count and ctime, so a needless rebuild fails every tippy in
//! flight. For older bundles that still ship a separately signed `bin/rustc`, PATH reaches
//! [`crate::compat`]'s per-build exec roots, laid by the same construction ([`lay_view`]).
//!
//! When trust is dev-linked the view presents the checkout instead ([`view_source`]): the
//! link outranks the store everywhere else, and presenting the store here would put two
//! compilers under one name. A checkout that is a sysroot (`bin/` and `lib/`) gets one exec
//! stub per tool plus the stock names, and its directories as symlinks. Stubs, because targo
//! and tippy refuse an executable the OS reports as a symlink; not hard links, because a dev
//! tree is rebuilt in place. A checkout that is not a sysroot is refused and recorded.
//! `link` and `unlink` re-assert the seam themselves.
//!
//! # Rules (all fail-closed)
//!
//! * Only names on [`SEAM_NAMES`] are created or removed. Without `<rustup_home>/toolchains`
//!   rustup is not installed and attach is a no-op; `~/.rustup` is never created.
//! * An absent entry is created atomically ([`crate::activate::atomic_symlink`]) after the
//!   view is refreshed. A link into `<prefix>/rustup/` or `<prefix>/store/trust/` is
//!   atpkg's: adopted when it names the view, re-pointed when it names the store (the
//!   pre-view layouts). Anything else is refused with [`DETACH_FIX`]; an existing link is
//!   never followed. [`repair`] also re-points a foreign link that names nothing or a
//!   toolchain older than the store's ([`stale_against_store`]).
//! * The unattended re-assertion heals one foreign shape by itself: a link to a live build
//!   tree older than the store's ([`unattended_heal`]). The target it replaced is recorded
//!   ([`REPLACING_PREFIX`], then [`REPLACED_PREFIX`]) so `doctor` prints the `ln -sfn` that
//!   puts it back, the package log carries it ([`crate::packages_log::Event::Seam`]), and a
//!   link put back is left by every later unattended pass ([`Refusal::PutBack`]).
//! * A view that differs is rebuilt part by part (a mirrored directory, or `bin/`), each
//!   built beside the live one and swapped in by `rename(2)`. A clone that cannot be made
//!   (the store on another volume) refuses the attach; a toolchain is never byte-copied.
//! * Neither the view nor an entry due to be re-pointed changes while a live process runs
//!   from it, a run holds a lease on it ([`crate::lease`]), or the process table cannot be
//!   read ([`Deferred`], [`Attached::LeftEntry`]): `targo` and rustup's proxies spawn the
//!   compiler by unresolved path, so a swap would switch compilers mid-build. The
//!   re-assertion says so and names `aterm pkg repair`. An entry naming the very build the
//!   view presents is re-pointed regardless: the view is clones of it, so no compiler switches.
//! * A successful attach is recorded in `status.toml` as `seams = ["rustup:trust"]` through
//!   the atomic writer. Detach removes only a link resolving into the prefix (`--force` also
//!   unlinks a foreign link, or moves a real directory aside, never deleting it), removes
//!   the view and drops the record.
//! * The seam is re-asserted after every activation or rollback of `trust` and at the end of
//!   the unattended `update` pass ([`reassert`]). `uninstall --all` detaches every recorded
//!   seam ([`detach_recorded`]).
//!
//! The library entry points take the rustup home as data; only the CLI edge reads
//! `RUSTUP_HOME`/`HOME` ([`rustup_home`]) and re-asserts (`reassert_rustup_seam` in cli.rs).
//! flow.rs never reads a rustup home, so no unit test that activates `trust` in a temp
//! layout can reach a developer's `~/.rustup`.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::Layout;

/// The compiled-in allowlist of rustup toolchain names atpkg will own.
pub const SEAM_NAMES: &[&str] = &["trust", "trust-dev"];

/// The seam every re-assertion attaches when nothing is recorded yet.
pub const DEFAULT_SEAM: &str = "trust";

/// The one store program a rustup seam ever points into.
pub const SEAM_PROGRAM: &str = "trust";

/// The one fix printed whenever attach refuses an entry atpkg does not own, spelled in the
/// std-only [`crate::toolchain`] so its directory remedy carries the same words.
pub use crate::toolchain::DETACH_FIX;

/// The record-key prefix in `status.toml`'s `seams` list (`rustup:<name>`).
const RECORD_PREFIX: &str = "rustup:";

/// The `seams` spelling of a refused re-assertion: `refused:rustup:<name>: <why>`. Recorded
/// because the six-hourly pass the GUI spawns discards stdout, so `aterm pkg status`, `doctor`
/// and Settings can show it; [`recorded_names`] never reads it as a seam.
pub const REFUSED_PREFIX: &str = "refused:";

/// The `seams` spelling of a stale foreign link a re-assertion replaced:
/// `replaced:rustup:<name>: <target as written>` ([`replaced_from`]): the undo's record, and
/// the hold that keeps a link put back on that tree. [`recorded_names`] never reads it as a
/// seam, and an older atpkg sharing the store keeps it through its rewrites. Written only once
/// the entry names the view; before that it is [`REPLACING_PREFIX`]'s.
pub const REPLACED_PREFIX: &str = "replaced:";

/// A replacement in flight: `replacing:rustup:<name>: <target as written>`, written before the
/// entry is swapped so the way back is on disk first. [`record`] turns it into
/// [`REPLACED_PREFIX`]'s record once the entry names the view; a pass that finds the entry
/// naming anything else drops it ([`attach_reporting`]), so a heal cut off before its rename
/// is never settled as a false undo and hold.
pub const REPLACING_PREFIX: &str = "replacing:";

/// Whether `name` is on the allowlist.
#[must_use]
pub fn name_allowed(name: &str) -> bool {
    SEAM_NAMES.contains(&name)
}

/// The `status.toml` spelling of a seam: `rustup:<name>`.
#[must_use]
pub fn record_key(name: &str) -> String {
    let mut k = String::from(RECORD_PREFIX);
    k.push_str(name);
    k
}

/// A record key ([`record_key`]) as a person reads it: rustup `trust`, never
/// `rustup:trust`.
struct Head<'a>(&'a str);

impl fmt::Display for Head<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.0.strip_prefix(RECORD_PREFIX).unwrap_or(self.0);
        write!(f, "rustup `{name}`")
    }
}

/// Rustup's home as this process sees it: `$RUSTUP_HOME`, else `<home>/.rustup`.
/// `None` only when neither is resolvable (HOME unset on Unix).
#[must_use]
pub fn rustup_home() -> Option<PathBuf> {
    rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
    )
}

/// Pure core of [`rustup_home`], with the two inputs as data
/// (`aterm_toolchain::rustup`, the one rule).
pub use aterm_toolchain::rustup::rustup_home_with;

/// `<rustup_home>/toolchains` — present iff rustup is installed.
#[must_use]
pub fn toolchains_dir(rustup_home: &Path) -> PathBuf {
    rustup_home.join("toolchains")
}

/// `<rustup_home>/toolchains/<name>` — the seam entry.
#[must_use]
pub fn seam_path(rustup_home: &Path, name: &str) -> PathBuf {
    toolchains_dir(rustup_home).join(name)
}

/// `<prefix>/store/trust` — the store tree a seam laid before the view existed
/// resolves into; recognised as atpkg's so it can be re-pointed, never adopted.
#[must_use]
pub fn owned_root(layout: &Layout) -> PathBuf {
    layout.prefix.join("store").join(SEAM_PROGRAM)
}

/// `<prefix>/store/trust/current` — the build every view mirrors.
#[must_use]
pub fn store_current(layout: &Layout) -> PathBuf {
    layout.program_current(SEAM_PROGRAM)
}

/// `<prefix>/rustup/` — every view sits under it.
#[must_use]
pub fn views_root(layout: &Layout) -> PathBuf {
    layout.prefix.join(aterm_toolchain::layout::VIEWS_DIR)
}

/// `<prefix>/rustup/<name>/` — the view the seam `name` targets.
#[must_use]
pub fn view_dir(layout: &Layout, name: &str) -> PathBuf {
    views_root(layout).join(name)
}

/// What the seam `name` targets: its view.
#[must_use]
pub fn seam_target(layout: &Layout, name: &str) -> PathBuf {
    view_dir(layout, name)
}

/// The stock names rustup's proxies and cargo's doctest phase resolve, each laid in a view as
/// a clone of its Trust tool. Nothing inside the toolchain resolves them: `targo` asks for
/// `trustc` and `trustdoc` by name, and tippy runs `trustc`.
pub const STOCK_NAMES: &[(&str, &str)] = &[
    ("rustc", "trustc"),
    ("cargo", "targo"),
    ("rustdoc", "trustdoc"),
];

/// The directories beside `bin/` a sysroot is read through, cloned into the view
/// (file by file, not a directory symlink — see the module doc) so a tool run from it
/// finds its own `lib/rustlib`, `libexec/` helpers and `share/` docs, and reports the
/// view as its sysroot.
const VIEW_DIRS: &[&str] = &["lib", "libexec", "share", "etc"];

/// Mirror `src` at `dst`: directories created, regular files cloned
/// ([`crate::clone::clone_file`]), symlinks recreated with the same target, anything else
/// skipped. `dst` must not exist.
fn mirror_tree(layout: &Layout, src: &Path, dst: &Path) -> io::Result<()> {
    layout.ensure_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)?;
        let kind = meta.file_type();
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
pub struct Refreshed {
    /// The numbered build `store/trust/current` named.
    pub build: PathBuf,
    /// Tools laid under their own names.
    pub tools: usize,
    /// Stock names laid, each with the Trust tool it is a clone of.
    pub stock: Vec<(&'static str, &'static str)>,
    /// Whether the view's `bin/` changed (a rebuild that produced the same set is silent).
    pub changed: bool,
    /// `Some` when the view did not match `build` and was left as it stands ([`Deferred`]).
    /// It still presents what it had; `tools` and `stock` are `build`'s plan, not yet laid.
    pub deferred: Option<Deferred>,
}

/// Why [`refresh_view`] left an out-of-date view as it stands.
///
/// `targo` spawns the `trustc` beside its own unresolved `current_exe()` for every crate, and
/// a `cargo` rustup proxies into the view does the same, so re-laying `bin/` and `lib/` under
/// a running build hands its remaining crates to another compiler (`error[E0514]: found crate
/// … compiled by an incompatible version of rustc`). gc's in-use guard cannot see this: the
/// view is clones, which reclaiming the store build never touches.
///
/// Not covered here: a `targo` started as `store/trust/current/bin/targo`, which the
/// `current` flip alone switches. Callers resolve a `current` path before running it (the
/// census guard `store-current`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deferred {
    /// A live process runs from the view — the executable the kernel names for it.
    InUse(PathBuf),
    /// The process table could not be read, so a build running from the view cannot be
    /// ruled out; unknown is priced as in use (a stale view is one pass late, a swapped
    /// one fails a build).
    Unknown,
    /// A run holds a lease on the view ([`crate::lease`]) — a command under `aterm pkg
    /// lease` that has steps still to run, though no process runs from it between two of
    /// them — or its leases could not be read, which is priced the same.
    Leased(crate::lease::Holders),
}

impl Deferred {
    /// When the view is laid after all — the clause every line that names one ends on.
    fn once(&self) -> &'static str {
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

/// How closely [`view_matches`] holds a view — or a [`crate::compat`] exec root, which is
/// the same construction under another name — against the build it was laid from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// `bin/`'s whole name -> file map (every tool a clone of the store's, every stock name
    /// a clone of its Trust tool, nothing extra), the mirrored directories present as real
    /// directories, and the regular files directly in `lib/` — the driver dylibs every
    /// frontend loads — clones of the store's. A few dozen `lstat`s.
    Shallow,
    /// Everything [`Depth::Shallow`] checks, and every entry under the mirrored directories:
    /// each regular file a clone, each symlink the same target, each directory real, nothing
    /// extra. A few thousand `lstat`s; for `repair` and every re-assertion of a rustup view.
    Deep,
}

/// What a view's `bin/` must hold for a build's `bin/` at `src_bin`, read with `lstat`
/// only: every regular file under its own name, then each of [`STOCK_NAMES`] whose Trust
/// tool is a regular file there, mapped to that tool over any separately signed copy the
/// bundle ships under the stock name. A symlink is not an entry: every Trust frontend
/// refuses a symlinked sibling.
struct BinPlan {
    /// Name in the view -> the store file it must be a clone of.
    entries: std::collections::BTreeMap<std::ffi::OsString, PathBuf>,
    /// Regular files in the build's `bin/`.
    tools: usize,
    /// Stock names laid, each with its Trust tool.
    stock: Vec<(&'static str, &'static str)>,
}

impl BinPlan {
    /// What [`refresh_view`] reports for `build` when this is its `bin/` plan.
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
        let src = entry.path();
        if !std::fs::symlink_metadata(&src)?.is_file() {
            continue;
        }
        entries.insert(entry.file_name(), src);
        tools += 1;
    }
    let mut stock = Vec::new();
    for (public, trust) in STOCK_NAMES {
        let trust_tool = src_bin.join(trust);
        if !std::fs::symlink_metadata(&trust_tool).is_ok_and(|m| m.is_file()) {
            continue;
        }
        // Inserted over any copy the bundle shipped under the stock name: the view
        // presents the Trust tool itself under it.
        entries.insert(std::ffi::OsString::from(public), trust_tool);
        stock.push((*public, *trust));
    }
    Ok(BinPlan {
        entries,
        tools,
        stock,
    })
}

/// Lay a view's `bin/` at `dst_bin` (created here, empty) from the build's `bin/`: one clone
/// per [`BinPlan`] entry, so each stock name is byte-identical to its Trust tool and a
/// stock-name copy the bundle shipped is never cloned. Returns the plan it laid.
///
/// # Errors
/// The build's `bin/` cannot be listed, or a clone cannot be made (the store on another
/// volume: it refuses rather than byte-copy the toolchain).
fn lay_bin(layout: &Layout, src_bin: &Path, dst_bin: &Path) -> io::Result<BinPlan> {
    let plan = bin_plan(src_bin)?;
    layout.ensure_dir(dst_bin)?;
    for (name, src) in &plan.entries {
        crate::clone::clone_file(src, &dst_bin.join(name))?;
    }
    Ok(plan)
}

/// Lay a complete view of `build` at `dest`, which must not exist: the [`VIEW_DIRS`] mirrored
/// ([`lay_view_dirs`]), then `bin/` ([`lay_bin`]). The one construction a rustup view and a
/// [`crate::compat`] exec root share; the caller lays at a dot-temp it renames into place.
/// `bin/` goes last, so a lay that fails earlier never presents a runnable frontend.
///
/// # Errors
/// `dest` exists, or [`lay_bin`] or [`mirror_tree`] refuses. A partial tree is left at
/// `dest` for the caller to remove.
#[cfg(unix)]
pub(crate) fn lay_view(layout: &Layout, build: &Path, dest: &Path) -> io::Result<()> {
    lay_view_dirs(layout, build, dest)?;
    lay_bin(layout, &build.join("bin"), &dest.join("bin"))?;
    Ok(())
}

/// [`lay_view`] without `bin/`: `dest` (which must not exist) created and the build's
/// [`VIEW_DIRS`] mirrored into it, for [`crate::compat::ensure_root`] when a standing root's
/// `bin/` already matches and is moved across by `rename(2)`.
///
/// # Errors
/// `dest` exists, or [`mirror_tree`] refuses. A partial tree is left at `dest` for the
/// caller to remove.
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
/// `stat` only: it never links, unlinks, creates or chmods anything (see [`refresh_view`]).
#[must_use]
pub(crate) fn view_matches(build: &Path, view: &Path, depth: Depth) -> bool {
    first_mismatch(build, view, depth).is_none()
}

/// The first path at which the view at `view` stops being `build` at `depth`, or `None` when
/// it matches. Top-level entries beside `bin/` and the mirrored directories are not compared:
/// a crashed rebuild's `.bin.old-<pid>` is debris ([`sweep_view_debris`]), and counting it
/// would rebuild the view forever. Off Unix every view reads as mismatched and is rebuilt.
#[must_use]
pub(crate) fn first_mismatch(build: &Path, view: &Path, depth: Depth) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if !is_real_dir(view) {
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

/// [`first_mismatch`] over `bin/` alone, for a `view` known to be a real directory:
/// `<view>/bin` a real directory holding exactly the build's [`BinPlan`], each entry a clone
/// of its store file. Judged apart so a rebuild can keep a matching `bin/`, whose entries are
/// the files tippy pins. A hard link to the store is not a match.
#[cfg(unix)]
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

/// [`Depth::Deep`]'s byte check over the stock names: each of [`STOCK_NAMES`] the view
/// presents must hold its Trust tool's bytes. A bundle's separately signed `bin/rustc` can
/// share `trustc`'s length, mode and time with other bytes, and it is the file the view
/// exists not to present.
#[cfg(unix)]
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
pub(crate) fn stock_bytes_mismatch(_build: &Path, _view: &Path) -> Option<PathBuf> {
    None
}

/// Elsewhere than Unix the view is not compared: `bin/` always reads as differing.
#[cfg(not(unix))]
pub(crate) fn bin_mismatch(_build: &Path, view: &Path) -> Option<PathBuf> {
    Some(view.join("bin"))
}

/// Elsewhere than Unix the view is not compared: every directory reads as differing.
#[cfg(not(unix))]
fn dir_mismatch(_build: &Path, view: &Path, dir: &str, _depth: Depth) -> Option<PathBuf> {
    Some(view.join(dir))
}

/// The half of [`first_mismatch`] over one of [`VIEW_DIRS`], `dir`, at `depth`: absent in
/// the view when the build has none; a real directory otherwise, holding at
/// [`Depth::Shallow`] the top-level regular files of `lib/` as clones of the store's (and
/// nothing more is asked of the others), at [`Depth::Deep`] exactly the mirrored tree.
#[cfg(unix)]
fn dir_mismatch(build: &Path, view: &Path, dir: &str, depth: Depth) -> Option<PathBuf> {
    let src = build.join(dir);
    let at = view.join(dir);
    if !src.is_dir() {
        // Not in this build: nothing may sit under the name either.
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

/// `lstat` says a real directory; a symlink to one is not.
///
/// Unix-gated because every caller is. Widen the gate rather than add a `not(unix)` twin
/// returning `false`: `symlink_metadata` does not follow a Windows reparse point either. An
/// ungated caller fails to compile in the foreign cells (`verify cells`).
#[cfg(unix)]
pub(crate) fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// [`Depth::Shallow`] over `lib/`: every regular file directly in `src` a clone of it in
/// `dst`.
#[cfg(unix)]
fn top_files_mismatch(src: &Path, dst: &Path) -> Option<PathBuf> {
    let Ok(listing) = std::fs::read_dir(src) else {
        return Some(src.to_path_buf());
    };
    for entry in listing {
        let Ok(entry) = entry else {
            return Some(src.to_path_buf());
        };
        let from = entry.path();
        if std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
            let to = dst.join(entry.file_name());
            if !crate::clone::is_clone_of(&from, &to) {
                return Some(to);
            }
        }
    }
    None
}

/// [`Depth::Deep`] over one mirrored directory: exactly what [`mirror_tree`] lays —
/// regular files cloned from the store's, symlinks with equal targets, real directories
/// recursed — and nothing else, in either direction.
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
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            return Some(from);
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            let same = matches!(
                (std::fs::read_link(&from), std::fs::symlink_metadata(&to), std::fs::read_link(&to)),
                (Ok(a), Ok(m), Ok(b)) if m.file_type().is_symlink() && a == b
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
            // A socket or fifo: `mirror_tree` skips it, so the view holds nothing there.
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

/// Build or rebuild the view for `name` from `store/trust/current`, or do nothing when it
/// already matches ([`view_matches`] at [`Depth::Deep`]): tippy's identity guard snapshots the
/// `st_nlink` and `st_ctime` of its executable and siblings and fails when they move, so a
/// matching view costs only `lstat`s and not even a `chmod`.
///
/// The two [`Layout::ensure_dir`] calls run before that check, so a matching tree behind a
/// planted symlink or with a loose mode is refused or hardened like any other. They keep the
/// no-churn rule: `ensure_dir` chmods only a mode that differs (a same-mode `chmod(2)` still
/// moves `st_ctime`).
///
/// A rebuild re-lays only the parts that differ, each beside its live copy and swapped in by
/// `rename(2)`: each mirrored directory, then `bin/` only when it differs, so a rebuild beside
/// `bin/` never replaces a file a running tippy pins and a failed mirror returns before
/// `bin/`.
///
/// A view that differs but that a live process runs from, a run leases, or whose process
/// table cannot be read is left as it stands ([`Refreshed::deferred`], [`Deferred`]). The
/// table is read at most once, only after a mismatch, and never for a first lay.
///
/// # Errors
/// `store/trust/current` is not a readable link, `<prefix>/rustup` or the view is not a
/// directory atpkg may write, the build's `bin/` cannot be listed, or a clone cannot be made
/// (the store on another volume: a toolchain is never byte-copied).
pub fn refresh_view(layout: &Layout, name: &str) -> io::Result<Refreshed> {
    refresh_view_with(layout, name, &crate::gc::process_table)
}

/// [`refresh_view`] with the process enumeration injected (`running`, answering as
/// [`crate::gc::running_from`] takes it), so the in-use arm is provable from a test.
pub(crate) fn refresh_view_with(
    layout: &Layout,
    name: &str,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> io::Result<Refreshed> {
    // First, before every early return below: the debris a killed rebuild left under
    // another pid. It is not a mismatch (`first_mismatch` ignores it on purpose), so a view
    // that already matches is exactly where an abandoned sysroot's clones would sit forever.
    sweep_view_debris(&view_dir(layout, name));
    #[cfg(unix)]
    if let ViewSource::Linked(checkout) = view_source(layout) {
        return refresh_linked_view(layout, name, &checkout, running);
    }
    let current = store_current(layout);
    let build = std::fs::read_link(&current).map(|raw| absolute_target(&raw, &current))?;
    let src_bin = build.join("bin");
    let view = view_dir(layout, name);
    layout.ensure_dir(&views_root(layout))?;
    layout.ensure_dir(&view)?;
    if view_matches(&build, &view, Depth::Deep) {
        return Ok(bin_plan(&src_bin)?.refreshed(build, false, None));
    }
    // A build running from the view keeps it ([`Deferred`]): checked after the match, so a
    // current view reads no table, and before the first rename.
    if let Some(deferred) = relay_blocked(layout, name, &view, running) {
        return Ok(bin_plan(&src_bin)?.refreshed(build, false, Some(deferred)));
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
        // A directory symlink (the pre-mirroring form) is renamed as a link, and
        // `remove_link` drops it.
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
    // A `bin/` that did not match is replaced even when it presents the same name -> file map
    // (pre-clone hard links); `changed` says only whether what the view presents differs.
    let changed = stock_differed || !same_bin(&live, &staged);
    let old = view.join(format!(".bin.old-{pid}"));
    let _ = std::fs::remove_dir_all(&old);
    swap_in(&staged, &live, &old)?;
    Ok(plan.refreshed(build, changed, None))
}

/// Put `staged` in the view at `at`, renaming what stood there aside to `old` and removing it.
/// Between the two renames the part is missing, so a process starting from the view then
/// finds no `bin/` or `lib/` ([`relay_blocked`] does not cover that instant). A pass killed
/// between them leaves a mismatch the next pass re-lays; [`sweep_view_debris`] takes `old`.
fn swap_in(staged: &Path, at: &Path, old: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(at).is_ok() {
        std::fs::rename(at, old)?;
    }
    std::fs::rename(staged, at)?;
    let _ = std::fs::remove_dir_all(old);
    crate::platform::remove_link(old);
    Ok(())
}

/// Whether the out-of-date view must be left this pass: a run holds a lease on it, a live
/// process runs from under it (in either spelling), or the process table could not be read.
/// `None` also when nothing is laid in the view yet: a fresh view has nothing to run from,
/// and waiting there would leave rustup's entry naming an empty directory.
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
    // A leased view keeps it too: a command under `aterm pkg lease` runs steps from it for
    // hours, with no process between two steps. Read first: a few file tests, no walk.
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

/// Whether `name` is the scratch a view rebuild makes: `.<stem>.tmp-<pid>` or
/// `.<stem>.old-<pid>`, `<stem>` being `bin` or one of [`VIEW_DIRS`] and `<pid>` ASCII digits.
/// As narrow as [`crate::store::stage_scratch_of`]: it authorizes a `remove_dir_all` where a
/// user can also put things (`.lib.old-notes/` is not ours).
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

/// Remove the staging debris a killed rebuild left in `view`: every [`is_view_debris`] entry,
/// whatever its shape. [`first_mismatch`] ignores it, so a view that reads as current would
/// otherwise keep a superseded build's clones allocated. The caller holds the store lock.
/// Best-effort.
fn sweep_view_debris(view: &Path) {
    let Ok(entries) = std::fs::read_dir(view) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(is_view_debris) {
            continue;
        }
        let path = entry.path();
        // `remove_dir_all` first (the tree case), then the link/file case — the pair
        // `take_down` already uses, so a symlink at the name is unlinked, never followed.
        let _ = std::fs::remove_dir_all(&path);
        crate::platform::remove_link(&path);
    }
}

/// What the view presents: the store's live build, or a dev-linked checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewSource {
    /// `store/trust/current` — the installed build, one copy-on-write clone of it.
    Store,
    /// Trust is dev-linked and the recorded checkout is a sysroot (a `bin/` and a
    /// `lib/`): the view is exec stubs into its `bin/` and links to its directories.
    Linked(PathBuf),
    /// Trust is dev-linked but the checkout is not one atpkg can present — not a sysroot (a
    /// cargo project's `target/release`, a tree that lost its `lib/`), or a tree inside
    /// atpkg's own prefix, whose stubs would exec themselves forever. `attach` refuses it,
    /// naming the path.
    LinkedNoSysroot(PathBuf),
}

/// Which source the view presents now — the link marker outranks the store, as it does
/// for `which` and `list`. Read-only; a marker that cannot be read is no link.
#[must_use]
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

/// Whether the view at `view` already presents `checkout` as [`refresh_linked_view`] lays it:
/// `bin/` a real directory holding exactly the stubs [`linked_stubs`] renders, byte for byte,
/// and each of [`VIEW_DIRS`] a link to the checkout's, or absent when the checkout has none.
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

/// The stubs a linked view's `bin/` at `dst_bin` holds for `plan`: one
/// [`crate::lay::Executable`] per entry, the `bin/` shim body that execs the checkout's file
/// ([`crate::platform::shim_executable_to_env`], with no exported environment — a dev
/// checkout's target never routes through an exec root, so no guard line is rendered).
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

/// Whether `bin` holds exactly the stubs of `plan` — every entry a regular file whose
/// bytes are the rendered stub, nothing extra — read with `lstat` and bounded reads.
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
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            return false;
        }
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

/// Move `at` (a directory, a link, anything) aside under `view` and remove it — how
/// [`refresh_linked_view`] takes down what stood at a name before it lays its own. The
/// store's clone view taken down this way removes only the view's own files.
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

/// Lay the view for a dev-linked `checkout` (see the module doc): the mirrored directories as
/// links to the checkout's, and `bin/` staged as one exec stub per plan entry and swapped in
/// when it differs. A store view standing there is taken down ([`take_down`]), never
/// followed, and not while a build runs from it ([`Deferred`]).
#[cfg(unix)]
fn refresh_linked_view(
    layout: &Layout,
    name: &str,
    checkout: &Path,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
) -> io::Result<Refreshed> {
    let view = view_dir(layout, name);
    layout.ensure_dir(&views_root(layout))?;
    layout.ensure_dir(&view)?;
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

/// Whether two `bin/` directories present the same name -> file map, a file judged by its
/// length, permission bits and modification time (a clone keeps all three, so a staged
/// clone of what the live `bin/` already presents compares equal). Elsewhere than Unix a
/// rebuild always counts as a change.
fn same_bin(live: &Path, staged: &Path) -> bool {
    #[cfg(unix)]
    {
        type Attrs = (u64, std::fs::Permissions, Option<std::time::SystemTime>);
        let map = |dir: &Path| -> Option<std::collections::BTreeMap<std::ffi::OsString, Attrs>> {
            let mut out = std::collections::BTreeMap::new();
            for entry in std::fs::read_dir(dir).ok()? {
                let entry = entry.ok()?;
                let meta = std::fs::symlink_metadata(entry.path()).ok()?;
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
pub enum Entry {
    /// Nothing there.
    Absent,
    /// A symlink (or Windows junction) with this raw target, as written.
    Link(PathBuf),
    /// A real directory.
    Dir,
    /// A regular file.
    File,
    /// Something else (a socket, a fifo, …).
    Other,
}

impl Entry {
    /// The words a refusal uses for what was found.
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

/// Lexically fold `.` and `..` so two spellings of one path compare equal without
/// touching the filesystem (a dangling target has nothing to canonicalize).
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

/// A link's raw target made absolute (relative targets are relative to the link's
/// directory) and normalized.
fn absolute_target(raw: &Path, link: &Path) -> PathBuf {
    if raw.is_absolute() {
        return normalize(raw);
    }
    let base = link.parent().unwrap_or_else(|| Path::new(""));
    normalize(&base.join(raw))
}

/// Whether an absolute target lies inside `<prefix>/rustup/` or `<prefix>/store/trust/`
/// — the two trees a seam atpkg laid can name: lexically first, then by canonical path
/// when both sides resolve (so a prefix reached through a symlinked ancestor still
/// counts). A dangling target decides lexically alone.
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

/// Whether an absolute target is exactly the view for `name`, lexically only: a link into
/// the store (`current`, or a numbered build) is atpkg's and gets re-pointed, so it must not
/// read as already right.
fn targets_view(layout: &Layout, name: &str, abs: &Path) -> bool {
    abs == normalize(&seam_target(layout, name))
}

/// Everything the verbs decide on, gathered by one `lstat` + `readlink`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// `<rustup_home>/toolchains/<name>`.
    pub path: PathBuf,
    /// What is there.
    pub entry: Entry,
    /// For a link: its target made absolute and normalized.
    pub target: Option<PathBuf>,
    /// For a link: whether the target lies inside `<prefix>/rustup/` or `<prefix>/store/trust/`.
    pub in_prefix: bool,
    /// For a link: whether the target is exactly the view, `<prefix>/rustup/<name>`.
    pub targets_view: bool,
}

/// Inspect the seam entry for `name` without changing anything.
///
/// # Errors
/// The `lstat`/`readlink` failure, when the entry exists but cannot be read.
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
pub enum Attached {
    /// `<rustup_home>/toolchains` is absent: rustup is not installed, nothing done.
    NoRustup { key: String, toolchains: PathBuf },
    /// The entry was absent and is now a link to the view.
    Created {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// The entry already linked to the view; left byte-for-byte alone. `view_build` is the
    /// build the view now presents when this attach rebuilt it, so a pass reports the compiler
    /// it switched rustup to; `None` when the view was already that build.
    Adopted {
        key: String,
        path: PathBuf,
        target: PathBuf,
        view_build: Option<PathBuf>,
    },
    /// The entry linked into the store (`current`, or a numbered build); now the view.
    Repointed {
        key: String,
        path: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    /// The entry linked outside the store to a stale toolchain, now the view; the toolchain is
    /// untouched. An older one's target is recorded ([`replaced_from`]) and the line names the
    /// command that puts it back.
    ReplacedStale {
        key: String,
        path: PathBuf,
        from: PathBuf,
        to: PathBuf,
        stale: Stale,
    },
    /// The entry was due to be re-pointed (a pre-view store link, or a stale foreign link) and
    /// was left, because a live process runs from what it names or the table could not be read
    /// ([`Deferred`]). rustup's proxies spawn the compiler by the entry's unresolved path, so a
    /// re-point mid-build hands the rest of the build to another compiler (E0514). Nothing is
    /// recorded. Never for a link to the very build the view presents.
    LeftEntry {
        key: String,
        path: PathBuf,
        from: PathBuf,
        to: PathBuf,
        why: Deferred,
    },
}

impl Attached {
    /// Whether the seam is now recorded and live (everything but no-rustup, and an entry
    /// left behind — nothing was recorded for it).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_live(&self) -> bool {
        !matches!(self, Attached::NoRustup { .. } | Attached::LeftEntry { .. })
    }

    /// Whether this attach changed the filesystem (what a silent pass reports).
    #[must_use]
    pub fn changed(&self) -> bool {
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
                // The undo, where there is something to go back to: the line is what the
                // pass prints and a typed `repair` shows, so the way back is on it.
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

/// Why a foreign link's toolchain is stale beside the store's trust build — the one kind
/// of foreign entry [`repair`] re-points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stale {
    /// The link names nothing: every `cargo +trust` fails.
    Dangling,
    /// Older than the store's build, by the `commit-date` each compiler's `-vV` reports
    /// (`YYYY-MM-DD`): the toolchain's own, and the store's.
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

/// Whether the toolchain at `sysroot` is stale beside the store's current trust build: gone,
/// or older. `None` without a store build, while the view presents a dev-linked checkout
/// rather than the store ([`view_source`]), and when either compiler does not answer with a
/// date inside the probe bound — an unknown age is never read as stale. The one rule for
/// `doctor`'s (5e) line and for [`repair`], so doctor never names a fix repair refuses.
#[must_use]
pub fn stale_against_store(layout: &Layout, sysroot: &Path) -> Option<Stale> {
    if view_source(layout) != ViewSource::Store {
        return None;
    }
    let store = store_current(layout);
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

/// The `commit-date:` a sysroot's compiler reports — `bin/trustc -vV`, else `bin/rustc
/// -vV` (a stage2 from before the Trust names) — when it is a `YYYY-MM-DD` date.
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

/// Whether `sysroot` is a live rustc build tree's stage sysroot (`…/build/<triple>/stage0`,
/// `stage1` or `stage2`, or `build/host/…`), as written or as it resolves. `x.py build`
/// empties such a directory for every rebuild, so no channel should name one: a from-source
/// toolchain is reached sealed (`~/toolchains/trust-<rev>`), which never has this shape.
#[must_use]
pub fn live_build_tree(sysroot: &Path) -> bool {
    stage_shaped(&normalize(sysroot))
        || std::fs::canonicalize(sysroot).is_ok_and(|real| stage_shaped(&real))
}

/// [`live_build_tree`]'s lexical rule over one spelling: the last three components are
/// `build`, a target triple or `host`, and `stage0`/`stage1`/`stage2`.
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

/// Whether `s` is spelled like a rustc target triple: three or more non-empty `-`-separated
/// parts of ASCII letters, digits, `_` and `.` (`aarch64-apple-darwin`,
/// `x86_64-unknown-linux-gnu`, `thumbv7em-none-eabihf`).
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

/// Whether an unattended re-assertion re-points a foreign link. rust-analyzer, `cargo +trust`
/// and every rustup proxy run whatever the entry names, so the narrowest always-wrong case
/// heals by itself: `name` is [`DEFAULT_SEAM`] (`trust-dev` is by name the channel a person
/// points at a tree they are building), the target is a live build tree
/// ([`live_build_tree`]), and its compiler is older than the store's
/// ([`stale_against_store`]). An unreadable age, a newer tree, a dev-linked trust and a tree
/// that is gone (it may be mid-rebuild; that is [`repair`]'s) all leave it. A link naming
/// again the tree a re-assertion replaced, under any spelling ([`same_tree`]), was put back
/// and is left. The shape is read first, so nothing is spawned for any other link.
fn unattended_heal(layout: &Layout, name: &str, entry: &Path, target: &Path) -> Heal {
    // A target that is not UTF-8 could not be recorded as it is (`status.toml` is text), and
    // a way back recorded wrong is a hold that never matches: such a link is left.
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

/// Whether two absolute link targets name the same tree: equal as spelled, or the same real
/// path when both resolve. The put-back hold is on the tree: `rustup toolchain link trust .`
/// inside `build/host/stage2` writes its physical cwd (`…/build/<triple>/stage2/.`).
fn same_tree(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// The way back from a replaced stale link, as the pass line and `doctor` print it: the
/// `ln -sfn` that lays `from` again at `path`, each operand quoted by the shim's rule (a path
/// can hold a space or a `'`).
fn put_back_hint(from: &Path, path: &Path) -> String {
    format!(
        "to put it back: ln -sfn {} {} (no unattended pass re-points it again; `aterm pkg \
         repair` does)",
        crate::platform::sh_quote_str(&from.to_string_lossy()),
        crate::platform::sh_quote_str(&path.to_string_lossy())
    )
}

/// [`REPLACED_PREFIX`] + `rustup:<name>` + `: ` — what the record for exactly `name`
/// starts with (never `trust-dev`'s beside `trust`'s).
fn replaced_prefix(name: &str) -> String {
    let mut k = String::from(REPLACED_PREFIX);
    k.push_str(&record_key(name));
    k.push_str(": ");
    k
}

/// [`replaced_prefix`] for the replacement in flight ([`REPLACING_PREFIX`]).
fn replacing_prefix(name: &str) -> String {
    let mut k = String::from(REPLACING_PREFIX);
    k.push_str(&record_key(name));
    k.push_str(": ");
    k
}

/// The link target, as written, that a re-assertion last replaced at `name`, or `None`.
/// Laying it back is the whole undo ([`put_back_hint`]), and every unattended pass leaves a
/// link that names it again ([`Refusal::PutBack`]).
#[must_use]
pub fn replaced_from(layout: &Layout, name: &str) -> Option<PathBuf> {
    let prefix = replaced_prefix(name);
    recorded_keys(layout)
        .iter()
        .find_map(|k| k.strip_prefix(&prefix).map(PathBuf::from))
}

/// Record `from` as the replacement in flight at `name` ([`REPLACING_PREFIX`]); `None` drops
/// it. [`replaced_from`]'s record is untouched until [`record`] settles this one. One per
/// name: the latest replacement is the one to undo.
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
/// `None` when nothing is recorded or the entry names neither the view nor that link.
#[must_use]
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

/// Why an `attach` or [`detach`] did not happen. Every variant is fail-closed:
/// nothing on disk changed.
#[derive(Debug)]
pub enum Refusal {
    /// Not on [`SEAM_NAMES`].
    BadName(String),
    /// `<prefix>/store/trust/current` does not exist: there is nothing to build a view of.
    NotInstalled { target: PathBuf },
    /// Trust is dev-linked to a checkout that is not a sysroot (no `bin/` and `lib/`):
    /// rustup could present nothing from it. The view is left as it stands.
    LinkedNoSysroot { checkout: PathBuf },
    /// The entry exists and is not atpkg's — the one fix is [`DETACH_FIX`].
    Foreign { path: PathBuf, what: String },
    /// The entry names again the stale toolchain a re-assertion replaced ([`replaced_from`]):
    /// someone put it back, so the unattended pass leaves it. [`repair`] still re-points it.
    PutBack { path: PathBuf, target: PathBuf },
    /// A filesystem failure.
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

/// Attach the seam `name`: refresh the view, lay `<rustup_home>/toolchains/<name>` ->
/// `<prefix>/rustup/<name>` and record it. See the module doc for the rules.
///
/// # Errors
/// [`Refusal`] — the name is not allowed, trust is not installed, the entry is not
/// atpkg's, the view could not be built, or the filesystem failed. The rustup entry is
/// unchanged on any `Err`.
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

/// A view `attach` found out of date and left as it stands ([`Refreshed::deferred`]):
/// the build it was not re-laid onto, and why.
type LeftView = (PathBuf, Deferred);

/// How [`attach_reporting`] brings the view in line: [`refresh_view`] in every shipped
/// call, a test's own refresh — an injected process table, a failed re-lay — in a test.
type Refresh<'a> = &'a dyn Fn(&Layout, &str) -> io::Result<Refreshed>;

/// `attach`, also answering whether the view was left behind ([`LeftView`]). The entry is
/// laid, adopted or re-pointed either way: a view one pass stale still presents a compiler.
/// With `repair`, a foreign link to nothing or to an older toolchain is re-pointed too;
/// without it, a link to an older live build tree nobody put back ([`unattended_heal`]). No
/// existing link is re-pointed while a process runs from what it names or `running` cannot
/// be read ([`Attached::LeftEntry`]).
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
    // An in-flight replacement ([`REPLACING_PREFIX`]) is dropped unless the entry names the
    // view, the only evidence its swap happened; left, a later [`record`] would settle it as a
    // replacement aterm never made. A heal below writes it again.
    let replacing = replacing_prefix(name);
    if !p.targets_view
        && recorded_keys(layout)
            .iter()
            .any(|k| k.starts_with(&replacing))
    {
        set_replacing(layout, name, None)?;
    }
    // Foreign entries are refused before the view is touched, so a refusal changes nothing on
    // disk; the exceptions are `repair`'s stale links and the unattended heal.
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
    // The link outranks the store: a dev-linked trust is presented whether or not a store
    // build stands; one whose checkout rustup could not present is refused before the view
    // is touched; otherwise the store's build must be there.
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
            // A build running through the entry keeps it ([`Attached::LeftEntry`]). Checked
            // after the view is laid, to keep the window to the rename short; `running_from`
            // checks both spellings of what the entry names.
            let old = p
                .target
                .clone()
                .unwrap_or_else(|| absolute_target(&raw, &p.path));
            // A link to the very build the view presents (a pre-view `store/trust/current`, or
            // its number) switches no compiler when re-pointed, so it is not held for the
            // processes running from that build. A view left behind never qualifies.
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
            // Laying the view can take seconds: read the entry again, so a link a person
            // changed meanwhile is not renamed over or recorded as the way back.
            if std::fs::read_link(&p.path).ok().as_ref() != Some(&raw) {
                return Err(Refusal::Io(io::Error::other(format!(
                    "{} changed while this pass laid the view — left for the next pass",
                    p.path.display()
                ))));
            }
            // Re-point by `rename(2)` of a fresh link over the old one, never following it. An
            // older toolchain's link is recorded first as a replacement in flight
            // ([`REPLACING_PREFIX`]): a replacement that cannot be recorded is not made.
            // [`record`] settles it once the entry names the view; a failed swap drops it.
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
        // Every other shape was refused above.
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
/// ([`detach_recorded`]) did or declined at one seam. Its `Display` is the line the pass
/// prints; a caller decides by the variant, never by the words.
#[derive(Debug)]
pub enum SeamEvent {
    /// An attach that changed the filesystem ([`Attached::changed`]), or an entry it left
    /// behind ([`Attached::LeftEntry`]).
    Attached(Attached),
    /// A view left as it stands ([`Deferred`]): the view at the seam `name`, the build it
    /// was not re-laid onto, and why. Not a refusal: the seam is attached and presents a
    /// working compiler — the previous one.
    ViewLeft {
        name: String,
        view: PathBuf,
        build: PathBuf,
        why: Deferred,
    },
    /// An attach or a detach refused at the seam `name`.
    Refused { name: String, refusal: Refusal },
    /// What a detach did.
    Detached(Detached),
}

impl SeamEvent {
    /// The seam's name and the entry, when this is a foreign entry left alone
    /// ([`Refusal::Foreign`]) — what the pass's `foreign:` marker names for the window,
    /// whose one fix is [`DETACH_FIX`].
    #[must_use]
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
            // The view, the build it was not re-laid onto, why, what `cargo +<name>` runs
            // meanwhile, and `aterm pkg repair`, named because the next unattended pass can
            // be six hours away.
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
            // A filesystem failure names the entry it hit; every other refusal already says
            // which entry it is about.
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
pub enum Detached {
    /// rustup is not installed; only the record (if any) was dropped.
    NoRustup { key: String, toolchains: PathBuf },
    /// Nothing was at the path; only the record (if any) was dropped.
    AlreadyAbsent { key: String, path: PathBuf },
    /// An owned link (into the prefix) was unlinked.
    Removed {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// `--force`: a link elsewhere was unlinked (its target is named, so it is recoverable by
    /// hand).
    ForcedUnlinked {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// `--force`: a real directory / file was moved aside, never deleted.
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

/// Remove the seam entry at `path` (a link) and prove it is gone.
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

/// Detach the seam `name`: remove the entry when it is a symlink resolving into the
/// prefix (or `force`), and drop the record either way.
///
/// # Errors
/// [`Refusal`] — the name is not allowed, the entry is not atpkg's and `force` is
/// off, or the filesystem failed.
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
            // The view is this seam's alone and goes with the entry; `remove_dir_all` unlinks
            // a directory symlink (a dev-linked view's `lib/`) rather than descend into it.
            let _ = std::fs::remove_dir_all(view_dir(layout, name));
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
pub struct SeamStatus {
    /// `rustup:<name>`.
    pub key: String,
    /// Whether `status.toml` records it.
    pub recorded: bool,
    /// `<rustup_home>/toolchains/<name>`.
    pub path: PathBuf,
    /// `<rustup_home>/toolchains` exists.
    pub rustup_present: bool,
    /// What is there, or the read error.
    pub entry: Result<Entry, String>,
    /// For a link: its absolute target.
    pub target: Option<PathBuf>,
    /// For a link: inside `<prefix>/rustup/` or `<prefix>/store/trust/`.
    pub in_prefix: bool,
    /// For a link: exactly the view, `<prefix>/rustup/<name>`.
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

/// The `toolchains` dir a seam path sits in (its parent), for the no-rustup note.
fn toolchains_dir_of(seam: &Path) -> &Path {
    seam.parent().unwrap_or(seam)
}

/// Inspect the seam `name` and its record, changing nothing.
#[must_use]
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
pub fn recorded_keys(layout: &Layout) -> Vec<String> {
    crate::status::read(layout)
        .map(|s| s.seams)
        .unwrap_or_default()
}

/// The recorded rustup seam names on the allowlist, which a re-assertion or a whole-set
/// detach walks. A key that is not `rustup:<allowed>` is ignored, never acted on.
#[must_use]
pub fn recorded_names(layout: &Layout) -> Vec<String> {
    recorded_keys(layout)
        .iter()
        .filter_map(|k| k.strip_prefix(RECORD_PREFIX))
        .filter(|n| name_allowed(n))
        .map(str::to_string)
        .collect()
}

/// The `seams` keys the disk itself proves: every allowlisted rustup toolchain entry that
/// resolves into this prefix. What a record rebuilt after it was lost carries
/// ([`crate::status::rebuilt_from_store`]), so `uninstall --all` still detaches them.
#[must_use]
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

/// Add `rustup:<name>` to `status.toml`'s `seams`. Idempotent. Seeded through
/// [`crate::status::seed_for_rewrite`]: the save rewrites the whole record, whose `seams` list
/// is what `uninstall --all` walks.
///
/// It also settles a replacement in flight ([`REPLACING_PREFIX`]) into the way back and the
/// hold ([`replaced_from`]): every caller has just made the entry name the view, and a pass
/// that found it naming anything else has already dropped the in-flight record
/// ([`attach_reporting`]).
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

/// Drop `rustup:<name>` and its `refused:`, `replaced:` and `replacing:` entries from
/// `status.toml`'s `seams`, so no refusal or hold outlives the seam. Matched through the `: `
/// separator, so `trust` never takes `trust-dev`'s.
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
pub fn refused_key(name: &str) -> String {
    let mut k = String::from(REFUSED_PREFIX);
    k.push_str(&record_key(name));
    k
}

/// [`refused_key`] with the `: ` that separates it from the reason — what an entry for
/// exactly `name` starts with, and what an entry for a longer name (`trust-dev` beside
/// `trust`) never does.
fn refusal_prefix(name: &str) -> String {
    let mut k = refused_key(name);
    k.push_str(": ");
    k
}

/// Every recorded refusal, as `(name, why)` — what `status`/`doctor` print when the last
/// pass could not lay a seam. Empty when the last re-assertion of every seam succeeded.
#[cfg(all(test, unix))]
#[must_use]
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

/// Record that re-asserting `name` was refused with `why` (replacing any earlier
/// refusal for the same name). Never touches the `rustup:<name>` record itself.
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

/// Drop the recorded refusal for `name`, if any.
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

/// Re-assert every recorded seam, plus [`DEFAULT_SEAM`] when trust is installed or dev-linked
/// to a sysroot. Best-effort and quiet: the events name only what changed, what was refused,
/// and a view or entry left as it stands ([`Deferred`]). The one foreign entry it replaces is
/// a link to an older live build tree nobody put back ([`unattended_heal`]), recorded so it
/// can be undone ([`replaced_from`]).
#[must_use]
pub fn reassert(layout: &Layout, rustup_home: &Path) -> Vec<SeamEvent> {
    reassert_with(
        layout,
        rustup_home,
        &refresh_view,
        &crate::gc::process_table,
        false,
    )
}

/// [`reassert`] for `aterm pkg repair`, the pass a person asks for: a foreign link to
/// nothing, or to a toolchain older than the store's ([`stale_against_store`]), is
/// re-pointed at the view too, and the line names what it was, so it can be linked back.
#[must_use]
pub fn repair(layout: &Layout, rustup_home: &Path) -> Vec<SeamEvent> {
    reassert_with(
        layout,
        rustup_home,
        &refresh_view,
        &crate::gc::process_table,
        true,
    )
}

/// [`reassert`] with the view's refresh injected ([`Refresh`]), and the process table the
/// entry's in-use guard reads ([`Attached::LeftEntry`]), so what a pass records around a
/// view or an entry it left behind — or a view it failed to lay — is provable from a test.
fn reassert_with(
    layout: &Layout,
    rustup_home: &Path,
    refresh: Refresh<'_>,
    running: &dyn Fn() -> Option<Vec<PathBuf>>,
    repair: bool,
) -> Vec<SeamEvent> {
    let mut events = Vec::new();
    if !toolchains_dir(rustup_home).is_dir() {
        // No rustup: no re-assertion reaches a seam, so a view recorded left behind
        // ([`crate::quiet::ViewLeft`]) would keep the park's re-check waking for good.
        crate::quiet::clear_views_left(layout);
        return events;
    }
    let mut names: std::collections::BTreeSet<String> =
        recorded_names(layout).into_iter().collect();
    // The default seam joins whenever trust is installed, not only when its entry is absent:
    // `attach` is fail-closed on every shape, so a foreign entry is said and recorded each
    // pass. Also when trust is dev-linked to a sysroot with no store build: rustup's `trust`
    // is where a repo pinning the channel looks for it.
    if !names.contains(DEFAULT_SEAM)
        && (std::fs::symlink_metadata(store_current(layout)).is_ok()
            || matches!(view_source(layout), ViewSource::Linked(_)))
    {
        names.insert(DEFAULT_SEAM.to_string());
    }
    for name in names {
        match attach_reporting(layout, rustup_home, &name, refresh, running, repair) {
            Ok((a, left)) => {
                // Success clears the last recorded refusal, and so does a view left behind:
                // every recorded refusal is decided before the view is touched, so this pass
                // re-asked it, and a failed re-lay is `Io`, which is never recorded. An entry
                // left behind is the exception: it is still the link the last pass refused.
                if !matches!(a, Attached::LeftEntry { .. }) {
                    let _ = clear_refusal(layout, &name);
                }
                // An entry left behind is said: it changed nothing, and silence would read as
                // a seam already right.
                let said = a.changed() || matches!(a, Attached::LeftEntry { .. });
                // A replaced stale link is also logged: an unattended pass's lines reach no
                // one, and Settings ▸ Packages' Activity reads the package log.
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
                // A view left behind is said (it answered `changed: false`) and recorded, so
                // the park's re-check ([`crate::quiet::Due::View`]) lays it as soon as nothing
                // uses it; a view laid or current ends the record.
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
                // Recorded, not only returned: the GUI discards the pass's stdout. `Io` is a
                // transient; recording it would make one EIO a standing fault in Settings.
                if !matches!(refusal, Refusal::Io(_)) {
                    let _ = record_refusal(layout, &name, &refusal.to_string());
                    // A refusal is no wait: waking a pass cannot answer it, and the
                    // refusal's own record is what doctor shows.
                    crate::quiet::clear_view_left(layout, &name);
                }
                events.push(SeamEvent::Refused { name, refusal });
            }
        }
    }
    events
}

/// Detach every recorded seam (never `--force`): the whole-set removal's companion.
/// Returns one event per seam acted on or refused.
#[must_use]
pub fn detach_recorded(layout: &Layout, rustup_home: &Path) -> Vec<SeamEvent> {
    recorded_names(layout)
        .into_iter()
        .map(|name| match detach(layout, rustup_home, &name, false) {
            Ok(d) => SeamEvent::Detached(d),
            Err(refusal) => SeamEvent::Refused { name, refusal },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// A synthetic HOME-shaped tree: `prefix/` (the store) beside `rustup/`
    /// (a fake `RUSTUP_HOME`). Nothing here reads `$HOME` or `$RUSTUP_HOME`.
    struct Fixture {
        #[cfg_attr(
            not(unix),
            expect(
                dead_code,
                reason = "the tree's drop guard; only the unix tests read it"
            )
        )]
        root: aterm_tempfile::TempDir,
        layout: Layout,
        rustup: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = aterm_tempfile::Builder::new()
                .prefix(&format!("atpkg-seam-{label}-"))
                .tempdir()
                .unwrap();
            let prefix = root.path().join("prefix");
            std::fs::create_dir_all(&prefix).unwrap();
            let rustup = root.path().join("rustup");
            std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
            Fixture {
                root,
                layout: Layout { prefix },
                rustup,
            }
        }

        /// Lay `store/trust/<build>/` (Trust-named tools in `bin/` and no stock names, as the
        /// distribution ships, and a `lib/`) and `store/trust/current -> <build>`.
        fn install_trust(&self, build: u64) -> PathBuf {
            let dir = self.layout.build_dir("trust", build);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::create_dir_all(dir.join("lib").join("rustlib")).unwrap();
            std::fs::write(
                dir.join("lib").join("libtrust.dylib"),
                format!("driver of {build}"),
            )
            .unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                "../../etc",
                dir.join("lib").join("rustlib").join("etc-link"),
            )
            .unwrap();
            for tool in ["trustc", "targo", "trustdoc", "tippy"] {
                std::fs::write(
                    dir.join("bin").join(tool),
                    format!("{tool} of build {build}"),
                )
                .unwrap();
            }
            crate::activate::atomic_symlink(&dir, &store_current(&self.layout)).unwrap();
            dir
        }

        fn seam(&self, name: &str) -> PathBuf {
            seam_path(&self.rustup, name)
        }

        fn seams_recorded(&self) -> Vec<String> {
            recorded_keys(&self.layout)
        }
    }

    #[cfg(unix)]
    fn link(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    /// The refusal an event carries.
    #[cfg(unix)]
    fn refusal(e: &SeamEvent) -> Option<&Refusal> {
        match e {
            SeamEvent::Refused { refusal, .. } => Some(refusal),
            _ => None,
        }
    }

    /// The attach an event carries.
    #[cfg(unix)]
    fn attached(e: &SeamEvent) -> Option<&Attached> {
        match e {
            SeamEvent::Attached(a) => Some(a),
            _ => None,
        }
    }

    /// A foreign entry left alone, its one fix [`DETACH_FIX`].
    #[cfg(unix)]
    fn is_foreign(e: &SeamEvent) -> bool {
        matches!(refusal(e), Some(Refusal::Foreign { .. }))
    }

    /// A link put back after a heal, left as it stands ([`Refusal::PutBack`]).
    #[cfg(unix)]
    fn is_put_back(e: &SeamEvent) -> bool {
        matches!(refusal(e), Some(Refusal::PutBack { .. }))
    }

    /// The link an older toolchain's replacement re-pointed away from — the one whose way
    /// back the line prints ([`put_back_hint`]).
    #[cfg(unix)]
    fn replaced_older(e: &SeamEvent) -> Option<&Path> {
        match attached(e) {
            Some(Attached::ReplacedStale {
                from,
                stale: Stale::Older { .. },
                ..
            }) => Some(from),
            _ => None,
        }
    }

    #[cfg(unix)]
    /// Whether the view file `at` is a clone of the store file `store` holding its bytes —
    /// and not the store's own inode, which is what a hard link would be.
    fn cloned(store: &Path, at: &Path) -> bool {
        crate::clone::is_clone_of(store, at)
            && matches!(crate::clone::same_bytes(store, at), Ok(true))
    }

    #[test]
    fn allowlist_and_record_key() {
        assert!(name_allowed("trust"));
        assert!(name_allowed("trust-dev"));
        assert!(!name_allowed("stable"));
        assert!(!name_allowed(""));
        assert!(!name_allowed("../trust"));
        assert_eq!(record_key("trust"), "rustup:trust");
    }

    #[test]
    fn rustup_home_prefers_env_then_home() {
        let env = std::ffi::OsString::from("/x/rh");
        assert_eq!(
            rustup_home_with(Some(&env), Some(Path::new("/h"))),
            Some(PathBuf::from("/x/rh"))
        );
        // An empty env var is unset, as rustup treats it.
        let empty = std::ffi::OsString::new();
        assert_eq!(
            rustup_home_with(Some(&empty), Some(Path::new("/h"))),
            Some(PathBuf::from("/h/.rustup"))
        );
        assert_eq!(
            rustup_home_with(None, Some(Path::new("/h"))),
            Some(PathBuf::from("/h/.rustup"))
        );
        assert_eq!(rustup_home_with(None, None), None);
        assert_eq!(
            rustup_home_with(Some(&env), None),
            Some(PathBuf::from("/x/rh"))
        );
    }

    #[test]
    fn normalize_folds_dots() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize(Path::new("/a/b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn bad_name_is_refused_before_touching_disk() {
        let fx = Fixture::new("badname");
        fx.install_trust(6808);
        let err = attach(&fx.layout, &fx.rustup, "stable").unwrap_err();
        assert!(matches!(err, Refusal::BadName(_)), "{err}");
        assert!(std::fs::symlink_metadata(fx.seam("stable")).is_err());
        assert!(fx.seams_recorded().is_empty());
        let err = detach(&fx.layout, &fx.rustup, "stable", true).unwrap_err();
        assert!(matches!(err, Refusal::BadName(_)));
    }

    #[test]
    fn no_rustup_is_a_noop_that_never_creates_rustup_home() {
        let fx = Fixture::new("norustup");
        fx.install_trust(6808);
        std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::NoRustup { .. }), "{out}");
        assert!(!out.is_live() && !out.changed());
        assert!(!fx.rustup.join("toolchains").exists(), "never created");
        assert!(fx.seams_recorded().is_empty(), "nothing recorded");
        // The status line says so too, and reassert stays silent.
        let s = status(&fx.layout, &fx.rustup, "trust");
        assert!(!s.rustup_present);
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
        assert!(
            !fx.rustup.join("toolchains").exists(),
            "reassert never creates it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn absent_is_created_targeting_current_and_recorded() {
        let fx = Fixture::new("absent");
        fx.install_trust(6808);
        let before = status(&fx.layout, &fx.rustup, "trust");
        assert!(!before.recorded && before.entry == Ok(Entry::Absent));

        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::Created { .. }), "{out}");
        assert!(out.changed());
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust"),
            "the link names the view, never the store"
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        // No temp link left behind in toolchains/.
        let leftovers: Vec<_> = std::fs::read_dir(toolchains_dir(&fx.rustup))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp symlink remains");

        let after = status(&fx.layout, &fx.rustup, "trust");
        assert!(after.recorded && after.in_prefix && after.targets_view);

        // A second attach adopts: byte-identical link, still one record.
        let again = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(again, Attached::Adopted { .. }), "{again}");
        assert!(!again.changed());
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn attach_refuses_when_trust_is_not_installed() {
        let fx = Fixture::new("notinstalled");
        let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
        assert!(matches!(err, Refusal::NotInstalled { .. }), "{err}");
        assert!(
            std::fs::symlink_metadata(fx.seam("trust")).is_err(),
            "no dangling link laid"
        );
        assert!(fx.seams_recorded().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn existing_link_into_prefix_is_adopted_untouched() {
        let fx = Fixture::new("adopt");
        fx.install_trust(6808);
        refresh_view(&fx.layout, "trust").unwrap();
        link(&seam_target(&fx.layout, "trust"), &fx.seam("trust"));
        let ino_before = std::fs::symlink_metadata(fx.seam("trust")).unwrap();
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::Adopted { .. }), "{out}");
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        // Leave the bytes alone: the same inode, not a rewritten link.
        use std::os::unix::fs::MetadataExt;
        let ino_after = std::fs::symlink_metadata(fx.seam("trust")).unwrap();
        assert_eq!(ino_before.ino(), ino_after.ino(), "adopt rewrote the link");
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn relative_link_into_prefix_is_adopted() {
        let fx = Fixture::new("relative");
        fx.install_trust(6808);
        // `toolchains/trust -> ../../prefix/rustup/trust` — relative to the link.
        refresh_view(&fx.layout, "trust").unwrap();
        link(Path::new("../../prefix/rustup/trust"), &fx.seam("trust"));
        let p = probe(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(p.in_prefix && p.targets_view, "{p:?}");
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::Adopted { .. }), "{out}");
    }

    /// A rustup `trust` entry that is not atpkg's — a real directory, a link out of the
    /// prefix, a regular file — is refused with the one fix, fail-closed: left exactly
    /// as it stands, and nothing recorded.
    #[cfg(unix)]
    #[test]
    fn a_foreign_entry_is_refused_with_the_one_fix_and_left_alone() {
        let fx = Fixture::new("foreign");
        fx.install_trust(6808);
        let elsewhere = fx.root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let entry = fx.seam("trust");
        type Lay = fn(&Path, &Path);
        let shapes: [(&str, Lay); 3] = [
            ("a real directory", |entry, _| {
                std::fs::create_dir_all(entry.join("bin")).unwrap();
                std::fs::write(entry.join("bin").join("rustc"), b"x").unwrap();
            }),
            ("a link out of the prefix", |entry, elsewhere| {
                link(elsewhere, entry);
            }),
            ("a regular file", |entry, _| {
                std::fs::write(entry, b"not a toolchain").unwrap();
            }),
        ];
        for (label, lay) in shapes {
            lay(&entry, &elsewhere);
            let before = (
                std::fs::read_link(&entry).ok(),
                std::fs::read(&entry).ok(),
                std::fs::read(entry.join("bin").join("rustc")).ok(),
            );
            let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
            assert!(matches!(err, Refusal::Foreign { .. }), "{label}: {err}");
            assert!(err.to_string().contains(DETACH_FIX), "{label}: {err}");
            assert_eq!(
                (
                    std::fs::read_link(&entry).ok(),
                    std::fs::read(&entry).ok(),
                    std::fs::read(entry.join("bin").join("rustc")).ok(),
                ),
                before,
                "{label}: untouched"
            );
            assert!(fx.seams_recorded().is_empty(), "{label}");
            let s = status(&fx.layout, &fx.rustup, "trust");
            assert!(!s.in_prefix && !s.targets_view, "{label}: {s}");
            if std::fs::symlink_metadata(&entry).unwrap().is_dir() {
                std::fs::remove_dir_all(&entry).unwrap();
            } else {
                std::fs::remove_file(&entry).unwrap();
            }
        }
    }

    // A refused re-assertion is recorded as `refused:rustup:trust: <why>`, which
    // `recorded_names` never reads as a seam, until a re-assertion succeeds.
    #[cfg(unix)]
    #[test]
    fn a_refused_reassertion_is_recorded_until_one_succeeds() {
        let fx = Fixture::new("refusal-record");
        fx.install_trust(6808);
        let elsewhere = fx.root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust"));
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(is_foreign(&lines[0]), "{lines:?}");
        let refused = refusals(&fx.layout);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].0, "trust");
        assert_eq!(
            fx.seams_recorded(),
            vec![format!("refused:rustup:trust: {}", refused[0].1)]
        );
        // The refusal never masquerades as a recorded seam to re-assert.
        assert!(recorded_names(&fx.layout).is_empty());
        // Recording again with the same words is idempotent (one entry, no churn).
        let _ = reassert(&fx.layout, &fx.rustup);
        assert_eq!(fx.seams_recorded().len(), 1);
        // The disk was never touched: the foreign link still points elsewhere.
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
        // The user re-points the entry into the store; the next pass adopts it and
        // the refusal leaves the record, replaced by the seam itself.
        std::fs::remove_file(fx.seam("trust")).unwrap();
        refresh_view(&fx.layout, "trust").unwrap();
        link(&seam_target(&fx.layout, "trust"), &fx.seam("trust"));
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(lines.is_empty(), "an adoption is quiet: {lines:?}");
        assert!(refusals(&fx.layout).is_empty());
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        assert_eq!(refused_key("trust"), "refused:rustup:trust");
    }

    /// The unattended pass names the foreign entry it left alone
    /// ([`SeamEvent::foreign_entry`]), for the pass's `foreign:` marker, and touches nothing;
    /// once the entry is atpkg's, nothing is named.
    #[cfg(unix)]
    #[test]
    fn the_unattended_pass_names_the_foreign_entry_it_left_alone() {
        let fx = Fixture::new("foreignnamed");
        fx.install_trust(6808);
        let elsewhere = fx.root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust"));
        let said = reassert(&fx.layout, &fx.rustup);
        let foreign: Vec<_> = said.iter().filter_map(SeamEvent::foreign_entry).collect();
        assert_eq!(foreign, [("trust", fx.seam("trust").as_path())]);
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
        std::fs::remove_file(fx.seam("trust")).unwrap();
        let said = reassert(&fx.layout, &fx.rustup);
        assert!(said.iter().all(|e| e.foreign_entry().is_none()), "{said:?}");
    }

    /// A compiler at `<sysroot>/bin/<name>` answering `-vV` with the `commit-date:` in
    /// `<sysroot>/commit-date`: a hard link to one script per fixture, run once here unbounded,
    /// because macOS assesses a new executable on its first exec (~20 s under load, past the
    /// 5 s probe bound) and a link to an assessed file is not new.
    #[cfg(unix)]
    fn dated_compiler(fx: &Fixture, sysroot: &Path, name: &str, date: &str) {
        let script = fx.root.path().join("dated-compiler");
        if !script.is_file() {
            std::fs::write(
                &script,
                "#!/bin/sh\nd=$(cat \"$(dirname \"$0\")/../commit-date\")\n\
                 echo \"rustc 1.99.0-dev (0000000 $d)\"\necho \"commit-date: $d\"\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::process::Command::new(&script).output();
        }
        let bin = sysroot.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(name);
        let _ = std::fs::remove_file(&exe);
        std::fs::hard_link(&script, &exe).unwrap();
        std::fs::write(sysroot.join("commit-date"), date).unwrap();
    }

    /// An older sealed toolchain (`~/toolchains/trust-<rev>`): the unattended pass refuses it,
    /// `repair` re-points it, names it and records the way back, leaving the tree; likewise a
    /// link to a tree that is gone. A newer or undated toolchain, or any link while trust is
    /// dev-linked, stays refused under `repair` too.
    #[cfg(unix)]
    #[test]
    fn repair_repoints_a_link_to_an_older_or_missing_toolchain_and_nothing_else() {
        let fx = Fixture::new("stale-local");
        let build = fx.install_trust(9192);
        dated_compiler(&fx, &build, "trustc", "2026-09-17");
        let sealed = fx.root.path().join("toolchains").join("trust-0717abcdef");
        dated_compiler(&fx, &sealed, "rustc", "2026-07-17");
        link(&sealed, &fx.seam("trust"));
        assert_eq!(
            stale_against_store(&fx.layout, &sealed),
            Some(Stale::Older {
                its: "2026-07-17".into(),
                store: "2026-09-17".into()
            })
        );
        assert!(!live_build_tree(&sealed));
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(is_foreign(&lines[0]), "{lines:?}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), sealed);

        let lines = repair(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(
            replaced_older(&lines[0]),
            Some(sealed.as_path()),
            "{lines:?}"
        );
        // The way back is printed with the line: the command a person runs.
        assert!(
            lines[0].to_string().contains(&format!(
                "to put it back: ln -sfn '{}' '{}'",
                sealed.display(),
                fx.seam("trust").display()
            )),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        assert_eq!(
            fx.seams_recorded(),
            vec![
                format!("replaced:rustup:trust: {}", sealed.display()),
                "rustup:trust".to_string()
            ]
        );
        assert!(
            sealed.join("bin").join("rustc").is_file(),
            "the old tree is left"
        );
        assert!(
            repair(&fx.layout, &fx.rustup).is_empty(),
            "then it is ours: adopted"
        );

        for (date, why) in [("2026-09-18", "newer"), ("unknown", "undated")] {
            std::fs::remove_file(fx.seam("trust")).unwrap();
            dated_compiler(&fx, &sealed, "rustc", date);
            link(&sealed, &fx.seam("trust"));
            assert_eq!(stale_against_store(&fx.layout, &sealed), None, "{why}");
            let lines = repair(&fx.layout, &fx.rustup);
            assert!(is_foreign(&lines[0]), "{why}: {lines:?}");
            assert_eq!(
                std::fs::read_link(fx.seam("trust")).unwrap(),
                sealed,
                "{why}"
            );
        }

        // A link to a tree that is gone: `cargo +trust` fails outright, and repair relinks —
        // with no way back recorded, there being nothing to go back to.
        let gone = fx.root.path().join("toolchains").join("trust-deleted");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        link(&gone, &fx.seam("trust"));
        assert!(is_foreign(&reassert(&fx.layout, &fx.rustup)[0]));
        let lines = repair(&fx.layout, &fx.rustup);
        assert!(
            matches!(
                attached(&lines[0]),
                Some(Attached::ReplacedStale { from, stale: Stale::Dangling, .. }) if *from == gone
            ),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(sealed.clone()));

        // A dev-linked trust: the view presents the checkout, not the store, so no link is
        // stale against the store — doctor names no `repair` fix, and repair re-points
        // nothing.
        let checkout = sysroot_checkout(&fx, "checkout");
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc")],
        )
        .unwrap();
        dated_compiler(&fx, &sealed, "rustc", "2026-07-17");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        link(&sealed, &fx.seam("trust"));
        assert_eq!(stale_against_store(&fx.layout, &sealed), None);
        assert_eq!(stale_against_store(&fx.layout, &gone), None);
        assert!(is_foreign(&repair(&fx.layout, &fx.rustup)[0]));
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), sealed);
    }

    /// The store (build 9192, dated 2026-09-17) and a live build tree at `rel` whose `rustc`
    /// says `date`, with rustup's `trust` linked at it. Answers the tree.
    #[cfg(unix)]
    fn stale_tree_fixture(fx: &Fixture, rel: &str, date: &str) -> PathBuf {
        let build = fx.install_trust(9192);
        dated_compiler(fx, &build, "trustc", "2026-09-17");
        let tree = fx.root.path().join(rel);
        dated_compiler(fx, &tree, "rustc", date);
        link(&tree, &fx.seam("trust"));
        tree
    }

    /// [`reassert`] — the unattended pass, `repair` off — with the process table injected.
    #[cfg(unix)]
    fn unattended(fx: &Fixture, running: Option<Vec<PathBuf>>) -> Vec<SeamEvent> {
        reassert_with(
            &fx.layout,
            &fx.rustup,
            &refresh_view,
            &|| running.clone(),
            false,
        )
    }

    /// The shape the unattended heal is limited to: `…/build/<triple|host>/stage[0-2]`, as
    /// written or as it resolves, and nothing near it.
    #[cfg(unix)]
    #[test]
    fn live_build_tree_is_the_stage_sysroot_shape_and_nothing_near_it() {
        for yes in [
            "/h/trust/build/host/stage2",
            "/h/trust/build/aarch64-apple-darwin/stage2",
            "/w/build/x86_64-unknown-linux-gnu/stage1",
            "/w/build/thumbv7em-none-eabihf/stage0",
            "/w/build/host/stage2/",
            "/w/build/host/stage2/../stage2",
        ] {
            assert!(live_build_tree(Path::new(yes)), "{yes}");
        }
        for no in [
            "/h/toolchains/trust-0820abcdef",
            "/h/toolchains/trust-current",
            "/w/build/host/stage3",
            "/w/build/host/stage2-tools",
            "/w/build/stage2",
            "/w/build/aarch64/stage2",
            "/w/build/aarch64--darwin/stage2",
            "/w/built/host/stage2",
            "/w/build/host/stage2/bin",
            "/w/prefix/store/trust/9100",
            "stage2",
        ] {
            assert!(!live_build_tree(Path::new(no)), "{no}");
        }
        // As it resolves: a link named like a seal that is really a live tree is one (the
        // kernel runs the tree).
        let fx = Fixture::new("tree-shape");
        let tree = fx
            .root
            .path()
            .join("trust/build/aarch64-apple-darwin/stage2");
        std::fs::create_dir_all(&tree).unwrap();
        let alias = fx.root.path().join("toolchains").join("trust-alias");
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        link(&tree, &alias);
        assert!(live_build_tree(&alias));
    }

    /// The unattended pass re-points an older live build tree at the view, says so with the
    /// way back on the line, records what it replaced, leaves the tree alone and is quiet
    /// after. Both spellings of the tree: `host`, and a triple.
    #[cfg(unix)]
    #[test]
    fn the_unattended_pass_repoints_an_older_live_build_tree_and_records_the_way_back() {
        for (label, rel) in [
            ("heal-host", "trust/build/host/stage2"),
            ("heal-triple", "trust/build/aarch64-apple-darwin/stage1"),
        ] {
            let fx = Fixture::new(label);
            let tree = stale_tree_fixture(&fx, rel, "2026-08-20");
            // The heal is logged where Settings ▸ Packages' Activity reads it.
            let log = fx.root.path().join("logs");
            let _bound =
                crate::packages_log::test_bind::bind(log.clone(), fx.layout.prefix.clone());
            let lines = unattended(&fx, Some(vec![PathBuf::from("/usr/bin/true")]));
            assert_eq!(lines.len(), 1, "{label}: {lines:?}");
            let logged: Vec<_> = crate::packages_log::read_tail(&log, 10)
                .into_iter()
                .filter(|e| e.kind == crate::packages_log::kind::SEAM)
                .collect();
            assert!(
                logged.len() == 1
                    && logged[0].get("seam") == Some("rustup:trust")
                    && logged[0].get("from") == Some(tree.to_str().unwrap())
                    && logged[0]
                        .get("undo")
                        .is_some_and(|u| u.starts_with("to put it back: ln -sfn")),
                "{label}: {logged:?}"
            );
            assert_eq!(
                replaced_older(&lines[0]),
                Some(tree.as_path()),
                "{label}: {lines:?}"
            );
            assert_eq!(
                std::fs::read_link(fx.seam("trust")).unwrap(),
                seam_target(&fx.layout, "trust"),
                "{label}"
            );
            assert_eq!(
                fx.seams_recorded(),
                vec![
                    format!("replaced:rustup:trust: {}", tree.display()),
                    "rustup:trust".to_string()
                ],
                "{label}"
            );
            assert_eq!(replaced_from(&fx.layout, "trust"), Some(tree.clone()));
            // The record never reads as a seam to re-assert.
            assert_eq!(recorded_names(&fx.layout), vec!["trust".to_string()]);
            assert!(
                tree.join("bin").join("rustc").is_file(),
                "{label}: the tree is left"
            );
            // What doctor says of it: what it named and the way back.
            let note = replaced_note(&fx.layout, &fx.rustup, "trust").expect("a note");
            assert!(
                note.contains(&format!(
                    "ln -sfn '{}' '{}'",
                    tree.display(),
                    fx.seam("trust").display()
                )),
                "{label}: {note}"
            );
            // Then it is ours: adopted, and the pass says nothing.
            assert!(unattended(&fx, Some(Vec::new())).is_empty(), "{label}");
            // Detaching the seam takes the record with it: nothing is left to hold.
            detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
            assert!(fx.seams_recorded().is_empty(), "{label}");
            assert_eq!(replaced_from(&fx.layout, "trust"), None, "{label}");
        }
    }

    /// The unattended pass leaves every other foreign link, each with a staleness control
    /// where that is the point: an older seal, another prefix's older store build, a newer live
    /// tree, one of unknown age (no date, a silent driver, emptied mid-rebuild), one that is
    /// gone, an older live tree under a dev-link, and `trust-dev`.
    #[cfg(unix)]
    #[test]
    fn the_unattended_pass_leaves_every_other_foreign_link() {
        let fx = Fixture::new("heal-not");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        let refused = |fx: &Fixture, target: &Path, why: &str| {
            let _ = std::fs::remove_file(fx.seam("trust"));
            link(target, &fx.seam("trust"));
            let lines = unattended(fx, Some(Vec::new()));
            assert!(
                lines.len() == 1 && is_foreign(&lines[0]),
                "{why}: {lines:?}"
            );
            assert_eq!(
                std::fs::read_link(fx.seam("trust")).unwrap(),
                target,
                "{why}: nothing moved"
            );
            assert_eq!(replaced_from(&fx.layout, "trust"), None, "{why}");
            assert_eq!(refusals(&fx.layout).len(), 1, "{why}: refused as ever");
        };

        let sealed = fx.root.path().join("toolchains").join("trust-0820abcdef");
        dated_compiler(&fx, &sealed, "rustc", "2026-08-20");
        assert!(matches!(
            stale_against_store(&fx.layout, &sealed),
            Some(Stale::Older { .. })
        ));
        refused(&fx, &sealed, "an older seal");

        let other = fx
            .root
            .path()
            .join("other-prefix")
            .join("store")
            .join("trust")
            .join("9100");
        dated_compiler(&fx, &other, "trustc", "2026-08-20");
        assert!(matches!(
            stale_against_store(&fx.layout, &other),
            Some(Stale::Older { .. })
        ));
        refused(&fx, &other, "another prefix's older store build");

        dated_compiler(&fx, &stage2, "rustc", "2026-09-18");
        refused(&fx, &stage2, "a newer live tree");
        dated_compiler(&fx, &stage2, "rustc", "unknown");
        refused(&fx, &stage2, "a live tree with no date");
        // A driver that answers nothing inside the probe bound has an unknown age. The bound
        // is shortened for this leg; the elapsed check is a hang detector far under the wedge.
        let wedged = stage2.join("bin").join("rustc");
        std::fs::remove_file(&wedged).unwrap();
        std::fs::write(&wedged, "#!/bin/sh\nexec sleep 120\n").unwrap();
        std::fs::set_permissions(&wedged, std::fs::Permissions::from_mode(0o755)).unwrap();
        {
            let _probes = crate::doctor::ShorterProbes::of(std::time::Duration::from_millis(250));
            let started = std::time::Instant::now();
            assert_eq!(stale_against_store(&fx.layout, &stage2), None);
            refused(&fx, &stage2, "a live tree whose driver does not answer");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "the probe was cut off by its own bound, not the driver's two minutes: {:?}",
                started.elapsed()
            );
        }
        // `x.py build` empties the stage for the length of the rebuild.
        std::fs::remove_dir_all(&stage2).unwrap();
        std::fs::create_dir_all(&stage2).unwrap();
        refused(&fx, &stage2, "a live tree emptied mid-rebuild");
        // Gone altogether: `repair`'s to re-point, not the pass's.
        std::fs::remove_dir_all(&stage2).unwrap();
        assert_eq!(
            stale_against_store(&fx.layout, &stage2),
            Some(Stale::Dangling)
        );
        refused(&fx, &stage2, "a live tree that is gone");

        // A dev-linked trust: the view presents the checkout, so nothing is stale.
        dated_compiler(&fx, &stage2, "rustc", "2026-08-20");
        let checkout = sysroot_checkout(&fx, "checkout");
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc")],
        )
        .unwrap();
        refused(&fx, &stage2, "an older live tree under a dev-link");

        // `trust-dev`, a seam atpkg attached and a person then pointed at an older live tree:
        // by its name the channel for a tree being built, so it is not the pass's to heal.
        let fx = Fixture::new("heal-not-dev");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        record(&fx.layout, "trust-dev").unwrap();
        link(&stage2, &fx.seam("trust-dev"));
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines
                .iter()
                .any(|e| e.foreign_entry() == Some(("trust-dev", fx.seam("trust-dev").as_path()))),
            "{lines:?}"
        );
        assert_eq!(std::fs::read_link(fx.seam("trust-dev")).unwrap(), stage2);
        assert_eq!(replaced_from(&fx.layout, "trust-dev"), None);
    }

    /// An older numbered build of this store is the pre-view layout: re-pointed as ever, and
    /// never recorded as a replaced toolchain.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_store_build_is_never_recorded_as_replaced() {
        let fx = Fixture::new("heal-store");
        let old = fx.install_trust(6808);
        dated_compiler(&fx, &old, "trustc", "2026-08-20");
        let new = fx.install_trust(6809);
        dated_compiler(&fx, &new, "trustc", "2026-09-17");
        link(&old, &fx.seam("trust"));
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Repointed { .. })),
            "{lines:?}"
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        assert_eq!(replaced_note(&fx.layout, &fx.rustup, "trust"), None);
    }

    /// The heal's in-use deferral, as for `repair`: with a compiler running from under the tree
    /// (resolved through x.py's `host` link) or an unreadable table, the entry is left and
    /// nothing recorded; the first pass that finds nothing running heals it.
    #[cfg(unix)]
    #[test]
    fn the_unattended_heal_waits_out_a_build_running_through_the_tree() {
        let fx = Fixture::new("heal-in-use");
        let real = fx
            .root
            .path()
            .join("trust/build/aarch64-apple-darwin/stage2");
        dated_compiler(&fx, &real, "rustc", "2026-08-20");
        link(
            Path::new("aarch64-apple-darwin"),
            &fx.root.path().join("trust/build/host"),
        );
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        let exe = std::fs::canonicalize(stage2.join("bin").join("rustc")).unwrap();
        assert!(exe.starts_with(std::fs::canonicalize(&real).unwrap()));
        for (running, deferred) in [
            (Some(vec![exe.clone()]), Deferred::InUse(exe.clone())),
            (None, Deferred::Unknown),
        ] {
            let lines = unattended(&fx, running.clone());
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(
                matches!(
                    attached(&lines[0]),
                    Some(Attached::LeftEntry { from, why, .. }) if *from == stage2 && *why == deferred
                ),
                "{lines:?}"
            );
            assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
            assert!(fx.seams_recorded().is_empty(), "nothing recorded");
        }
        let lines = unattended(&fx, Some(vec![PathBuf::from("/usr/bin/true")]));
        assert!(
            lines.len() == 1 && replaced_older(&lines[0]).is_some(),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
    }

    /// The undo: after the printed `ln -sfn`, every unattended pass leaves the link (a recorded
    /// refusal of its own) and doctor's note says so; only `repair` re-points it. The hold
    /// names one target: another older live tree is healed as any other.
    #[cfg(unix)]
    #[test]
    fn a_link_put_back_after_the_heal_is_left_by_every_pass_until_repair() {
        let fx = Fixture::new("heal-undo");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        let healed = unattended(&fx, Some(Vec::new()));
        assert!(replaced_older(&healed[0]).is_some(), "{healed:?}");

        let out = std::process::Command::new("ln")
            .arg("-sfn")
            .arg(&stage2)
            .arg(fx.seam("trust"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);

        let mut said = String::new();
        for pass in 0..2 {
            let lines = unattended(&fx, Some(Vec::new()));
            assert!(
                lines.len() == 1
                    && matches!(
                        refusal(&lines[0]),
                        Some(Refusal::PutBack { target, .. }) if *target == stage2
                    ),
                "pass {pass}: {lines:?}"
            );
            said = lines[0].to_string();
            assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
        }
        let refused = refusals(&fx.layout);
        assert!(
            refused.len() == 1 && refused[0].1 == said,
            "the refusal recorded is the one said: {refused:?}"
        );
        let note = replaced_note(&fx.layout, &fx.rustup, "trust").expect("a note");
        assert!(
            note.contains(&format!("names {} again", stage2.display()))
                && note.contains("no unattended pass re-points it"),
            "{note}"
        );

        // A person asks again: repair re-points it, and the way back is still recorded.
        let lines = repair(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1 && replaced_older(&lines[0]).is_some(),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        assert!(refusals(&fx.layout).is_empty());
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(stage2.clone()));

        // The hold names one tree: a different older live tree is healed, and becomes
        // the way back.
        let other = fx.root.path().join("trust2/build/host/stage2");
        dated_compiler(&fx, &other, "rustc", "2026-08-21");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        link(&other, &fx.seam("trust"));
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines.len() == 1 && replaced_older(&lines[0]) == Some(other.as_path()),
            "{lines:?}"
        );
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(other));
    }

    /// The hold is on the tree, whatever the spelling: `rustup toolchain link trust .` inside
    /// `build/host/stage2` writes `build/<triple>/stage2/.` (rustup 1.29).
    #[cfg(unix)]
    #[test]
    fn a_link_put_back_with_rustups_own_spelling_is_held() {
        let fx = Fixture::new("heal-undo-spelling");
        let build = fx.install_trust(9192);
        dated_compiler(&fx, &build, "trustc", "2026-09-17");
        let root = std::fs::canonicalize(fx.root.path()).unwrap();
        let real = root.join("trust/build/aarch64-apple-darwin/stage2");
        dated_compiler(&fx, &real, "rustc", "2026-08-20");
        link(
            Path::new("aarch64-apple-darwin"),
            &root.join("trust/build/host"),
        );
        link(&root.join("trust/build/host/stage2"), &fx.seam("trust"));
        let healed = unattended(&fx, Some(Vec::new()));
        assert!(replaced_older(&healed[0]).is_some(), "{healed:?}");

        let rustups = real.join(".");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        link(&rustups, &fx.seam("trust"));
        let note = replaced_note(&fx.layout, &fx.rustup, "trust");
        assert!(
            note.as_deref()
                .is_some_and(|n| n.contains("no unattended pass re-points it")),
            "{note:?}"
        );
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(lines.len() == 1 && is_put_back(&lines[0]), "{lines:?}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), rustups);
    }

    /// A heal whose swap fails drops its in-flight record and leaves an earlier heal's way back
    /// as it was; read as the hold, that record would block the heal for good.
    #[cfg(unix)]
    #[test]
    fn a_heal_whose_swap_fails_leaves_the_record_as_it_stood() {
        let fx = Fixture::new("heal-swap-fails");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        // An earlier heal's way back, settled as a completed swap settles it.
        let earlier = fx.root.path().join("earlier/build/host/stage2");
        set_replacing(&fx.layout, "trust", Some(&earlier)).unwrap();
        record(&fx.layout, "trust").unwrap();
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(earlier.clone()));
        let toolchains = toolchains_dir(&fx.rustup);
        std::fs::set_permissions(&toolchains, std::fs::Permissions::from_mode(0o555)).unwrap();
        let lines = unattended(&fx, Some(Vec::new()));
        std::fs::set_permissions(&toolchains, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::Io(_))),
            "the swap was refused by the filesystem: {lines:?}"
        );
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(earlier));
        assert!(
            !fx.seams_recorded()
                .iter()
                .any(|k| k.starts_with(REPLACING_PREFIX)),
            "{:?}",
            fx.seams_recorded()
        );
        // And with the directory writable again, the next pass heals it.
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines.len() == 1 && replaced_older(&lines[0]).is_some(),
            "{lines:?}"
        );
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(stage2));
    }

    /// A heal cut off before its rename (only the in-flight record written) is finished by the
    /// next pass, with no doctor note in between; cut off after it, the next pass that adopts
    /// the entry settles the way back, and a link then put back is held.
    #[cfg(unix)]
    #[test]
    fn a_heal_cut_off_before_or_after_its_rename_is_finished_by_the_next_pass() {
        let fx = Fixture::new("heal-cut-off");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        // Before the rename: the heal's first write, and nothing else.
        set_replacing(&fx.layout, "trust", Some(&stage2)).unwrap();
        assert_eq!(replaced_from(&fx.layout, "trust"), None);
        assert_eq!(
            replaced_note(&fx.layout, &fx.rustup, "trust"),
            None,
            "nobody put anything back"
        );
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines.len() == 1 && replaced_older(&lines[0]).is_some(),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        assert_eq!(
            fx.seams_recorded(),
            vec![
                format!("replaced:rustup:trust: {}", stage2.display()),
                "rustup:trust".to_string()
            ]
        );

        // After the rename: the heal's first write and the swap, and not the settling one.
        let fx = Fixture::new("heal-cut-off-late");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        refresh_view(&fx.layout, "trust").unwrap();
        set_replacing(&fx.layout, "trust", Some(&stage2)).unwrap();
        crate::activate::atomic_symlink(&seam_target(&fx.layout, "trust"), &fx.seam("trust"))
            .unwrap();
        let _ = unattended(&fx, Some(Vec::new()));
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(stage2.clone()));
        assert!(
            replaced_note(&fx.layout, &fx.rustup, "trust")
                .is_some_and(|n| n.contains("was re-pointed at the store's view")),
        );
        crate::activate::atomic_symlink(&stage2, &fx.seam("trust")).unwrap();
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(lines.len() == 1 && is_put_back(&lines[0]), "{lines:?}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
    }

    /// An in-flight record left by a heal that never renamed is never settled later as a
    /// replacement: an entry laid afresh reports nothing replaced, and a later link to the
    /// tree, once older again, is healed rather than held.
    #[cfg(unix)]
    #[test]
    fn a_heal_cut_off_before_its_rename_and_never_finished_replaced_nothing() {
        let fx = Fixture::new("heal-leftover");
        let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        set_replacing(&fx.layout, "trust", Some(&stage2)).unwrap();
        dated_compiler(&fx, &stage2, "rustc", "2026-09-20");
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(is_foreign(&lines[0]), "{lines:?}");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            matches!(attached(&lines[0]), Some(Attached::Created { .. })),
            "{lines:?}"
        );
        assert_eq!(replaced_note(&fx.layout, &fx.rustup, "trust"), None);

        dated_compiler(&fx, &stage2, "rustc", "2026-08-20");
        std::fs::remove_file(fx.seam("trust")).unwrap();
        link(&stage2, &fx.seam("trust"));
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines.len() == 1 && replaced_older(&lines[0]).is_some(),
            "healed, not held: {lines:?}"
        );
    }

    /// The entry is read again before the swap: a link changed while the view was laid is left
    /// for the next pass, and nothing is recorded as replaced.
    #[cfg(unix)]
    #[test]
    fn a_link_changed_while_the_view_is_laid_is_left_for_the_next_pass() {
        let fx = Fixture::new("heal-raced");
        let _stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
        let seal = fx.root.path().join("toolchains").join("trust-0926abcdef");
        dated_compiler(&fx, &seal, "rustc", "2026-09-26");
        let entry = fx.seam("trust");
        let meanwhile = |layout: &Layout, name: &str| {
            let laid = refresh_view(layout, name);
            crate::activate::atomic_symlink(&seal, &entry).unwrap();
            laid
        };
        let lines = reassert_with(
            &fx.layout,
            &fx.rustup,
            &meanwhile,
            &|| Some(Vec::new()),
            false,
        );
        assert!(
            lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::Io(_))),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(&entry).unwrap(),
            seal,
            "their link stands"
        );
        assert_eq!(replaced_from(&fx.layout, "trust"), None);
        assert!(
            !fx.seams_recorded()
                .iter()
                .any(|k| k.starts_with(REPLACING_PREFIX) || k.starts_with(REFUSED_PREFIX)),
            "a transient, not a posture: {:?}",
            fx.seams_recorded()
        );
        // The next pass looks again: a newer seal is not the pass's to touch.
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(is_foreign(&lines[0]), "{lines:?}");
        assert_eq!(std::fs::read_link(&entry).unwrap(), seal);
    }

    /// The printed way back runs as printed: its operands are quoted by the shim's rule, so a
    /// path with a `'` in it is put back.
    #[cfg(unix)]
    #[test]
    fn the_printed_way_back_puts_back_a_path_with_a_quote_in_it() {
        let fx = Fixture::new("heal-quote");
        let stage2 = stale_tree_fixture(&fx, "o'neil's trust/build/host/stage2", "2026-08-20");
        let lines = unattended(&fx, Some(Vec::new()));
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = lines[0].to_string();
        let undo = line
            .split("to put it back: ")
            .nth(1)
            .and_then(|rest| rest.split(" (no unattended pass").next())
            .expect("the way back is on the line");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(undo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{undo}: {out:?}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(is_put_back(&lines[0]), "{lines:?}");
    }

    /// The same guard on the other re-point: a pre-view link to an older store build that a
    /// live build runs through is left, and re-pointed once nothing runs from it.
    #[cfg(unix)]
    #[test]
    fn a_pre_view_store_link_a_live_build_runs_through_is_left_until_it_exits() {
        let fx = Fixture::new("repoint-in-use");
        let old = fx.install_trust(6808);
        link(&old, &fx.seam("trust"));
        fx.install_trust(6809);
        let exe = std::fs::canonicalize(old.join("bin").join("trustc")).unwrap();
        let lines = reassert_with(
            &fx.layout,
            &fx.rustup,
            &refresh_view,
            &|| Some(vec![exe.clone()]),
            false,
        );
        assert!(
            lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::LeftEntry { .. })),
            "{lines:?}"
        );
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), old);
        let lines = reassert_with(
            &fx.layout,
            &fx.rustup,
            &refresh_view,
            &|| Some(Vec::new()),
            false,
        );
        assert!(
            lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Repointed { .. })),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
    }

    /// A pre-view link to the view's own build (`store/trust/current`, or its number) is
    /// re-pointed even while a process runs from that build or the table cannot be read: the
    /// view is clones of that build, so no compiler switches. The test above is the control.
    #[cfg(unix)]
    #[test]
    fn a_pre_view_link_to_the_views_own_build_is_repointed_under_a_running_build() {
        let fx = Fixture::new("repoint-same-build");
        let build = fx.install_trust(6809);
        let exe = std::fs::canonicalize(build.join("bin").join("targo")).unwrap();
        for spelling in [store_current(&fx.layout), build.clone()] {
            for running in [Some(vec![exe.clone()]), None] {
                let _ = std::fs::remove_file(fx.seam("trust"));
                link(&spelling, &fx.seam("trust"));
                let lines = reassert_with(
                    &fx.layout,
                    &fx.rustup,
                    &refresh_view,
                    &|| running.clone(),
                    false,
                );
                assert!(
                    lines.len() == 1
                        && matches!(attached(&lines[0]), Some(Attached::Repointed { .. })),
                    "{spelling:?} with {running:?}: {lines:?}"
                );
                assert_eq!(
                    std::fs::read_link(fx.seam("trust")).unwrap(),
                    seam_target(&fx.layout, "trust")
                );
                assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn detach_removes_only_owned_links() {
        let fx = Fixture::new("detach");
        fx.install_trust(6808);
        // Owned: attached by us — detach unlinks it and clears the record.
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
        assert!(matches!(out, Detached::Removed { .. }), "{out}");
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
        assert!(fx.seams_recorded().is_empty());
        // The store's own `current` is untouched — the rustup entry and the view went.
        assert!(std::fs::symlink_metadata(store_current(&fx.layout)).is_ok());
        assert!(std::fs::symlink_metadata(view_dir(&fx.layout, "trust")).is_err());

        // Absent: nothing to remove, exit clean.
        let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
        assert!(matches!(out, Detached::AlreadyAbsent { .. }), "{out}");

        // Foreign link: refused without --force, unlinked with it.
        let elsewhere = fx.root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust"));
        let err = detach(&fx.layout, &fx.rustup, "trust", false).unwrap_err();
        assert!(matches!(err, Refusal::Foreign { .. }), "{err}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
        let out = detach(&fx.layout, &fx.rustup, "trust", true).unwrap();
        assert!(matches!(out, Detached::ForcedUnlinked { .. }), "{out}");
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
        assert!(
            elsewhere.is_dir(),
            "the target of a foreign link is never touched"
        );

        // Foreign directory: refused without --force; moved aside (never deleted) with it.
        std::fs::create_dir_all(fx.seam("trust").join("bin")).unwrap();
        std::fs::write(fx.seam("trust").join("bin").join("rustc"), b"x").unwrap();
        let err = detach(&fx.layout, &fx.rustup, "trust", false).unwrap_err();
        assert!(matches!(err, Refusal::Foreign { .. }), "{err}");
        let out = detach(&fx.layout, &fx.rustup, "trust", true).unwrap();
        let moved_to = match &out {
            Detached::ForcedDisplaced { moved_to, .. } => moved_to.clone(),
            other => panic!("expected ForcedDisplaced, got {other}"),
        };
        assert!(
            std::fs::symlink_metadata(fx.seam("trust")).is_err(),
            "seam path is clear"
        );
        assert!(
            moved_to.join("bin").join("rustc").is_file(),
            "the foreign tree survives beside it: {}",
            moved_to.display()
        );
        assert!(moved_to.starts_with(toolchains_dir(&fx.rustup)));
        // And attach now succeeds — the fix the refusal named actually unblocks it.
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::Created { .. }), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn detach_with_no_rustup_clears_the_record() {
        let fx = Fixture::new("detach-norustup");
        fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
        let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
        assert!(matches!(out, Detached::NoRustup { .. }), "{out}");
        assert!(fx.seams_recorded().is_empty());
        assert!(!fx.rustup.join("toolchains").exists(), "never created");
    }

    #[cfg(unix)]
    #[test]
    fn record_never_clobbers_other_status_fields() {
        let fx = Fixture::new("record");
        fx.install_trust(6808);
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "trust".to_string(),
            crate::ProgramStatus {
                installed_build: Some(6808),
                state: "managed 6808 — pinned by index 9".into(),
                tree_root: "abc".into(),
            },
        );
        crate::status::write(
            &fx.layout,
            &crate::Status {
                schema: 1,
                updated_at: "2026-08-29T00:00:00Z".into(),
                enabled: true,
                index_source: "owner/repo".into(),
                outcome: "up to date".into(),
                programs,
                ..Default::default()
            },
        )
        .unwrap();
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let back = crate::status::read(&fx.layout).unwrap();
        assert_eq!(back.seams, vec!["rustup:trust".to_string()]);
        assert_eq!(back.updated_at, "2026-08-29T00:00:00Z");
        assert!(back.enabled);
        assert_eq!(back.index_source, "owner/repo");
        assert_eq!(back.outcome, "up to date");
        assert_eq!(back.programs["trust"].installed_build, Some(6808));
        assert_eq!(back.programs["trust"].tree_root, "abc");
        // Detach drops only the seam.
        detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
        let back = crate::status::read(&fx.layout).unwrap();
        assert!(back.seams.is_empty());
        assert_eq!(back.programs["trust"].installed_build, Some(6808));
    }

    /// An unreadable `status.toml` is kept aside as `status.toml.corrupt-<unix>` and healed,
    /// and the seam is recorded on the healed record.
    #[test]
    fn recording_a_seam_heals_an_unreadable_record() {
        let prefix = crate::testdir::dir("atpkg-seam-unreadable-");
        let layout = Layout {
            prefix: prefix.to_path_buf(),
        };
        let corrupt = "seams = [\"rustup:trust\"]\nthis is not valid toml {{{\n";
        std::fs::write(layout.status(), corrupt).unwrap();
        record(&layout, "trust").expect("the recorder heals and records");
        assert_eq!(recorded_keys(&layout), vec![record_key("trust")]);
        let aside = std::fs::read_dir(&layout.prefix)
            .unwrap()
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("status.toml.corrupt-")
            })
            .expect("the unreadable record is kept aside");
        assert_eq!(std::fs::read_to_string(aside.path()).unwrap(), corrupt);
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn trust_dev_is_a_second_independent_seam() {
        let fx = Fixture::new("trustdev");
        fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        attach(&fx.layout, &fx.rustup, "trust-dev").unwrap();
        assert_eq!(
            fx.seams_recorded(),
            vec!["rustup:trust".to_string(), "rustup:trust-dev".to_string()]
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust-dev")).unwrap(),
            seam_target(&fx.layout, "trust-dev")
        );
        detach(&fx.layout, &fx.rustup, "trust-dev", false).unwrap();
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        assert!(
            std::fs::symlink_metadata(fx.seam("trust")).is_ok(),
            "the other seam stays"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reassert_recreates_recorded_and_first_attaches_trust() {
        let fx = Fixture::new("reassert");
        fx.install_trust(6808);
        // Nothing recorded, rustup present, entry absent, trust installed ⇒ first attach.
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(attached(&lines[0]), Some(Attached::Created { .. })),
            "{lines:?}"
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        // Correct already ⇒ silent.
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
        // A user rm'd the link ⇒ recreated (it is recorded).
        std::fs::remove_file(fx.seam("trust")).unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_ok());
        // A recorded seam that went foreign is reported, never overwritten.
        std::fs::remove_file(fx.seam("trust")).unwrap();
        std::fs::create_dir_all(fx.seam("trust")).unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(is_foreign(&lines[0]), "{lines:?}");
        assert!(fx.seam("trust").is_dir(), "left alone");
    }

    #[cfg(unix)]
    #[test]
    fn reassert_first_attach_needs_trust_installed() {
        let fx = Fixture::new("reassert-notinstalled");
        // rustup present, entry absent, but no store/trust/current ⇒ nothing, silently.
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
        assert!(fx.seams_recorded().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn detach_recorded_walks_only_the_record() {
        let fx = Fixture::new("detach-recorded");
        fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        // An unrecorded foreign entry under another allowed name is NOT touched.
        let elsewhere = fx.root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust-dev"));
        let lines = detach_recorded(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(lines[0], SeamEvent::Detached(Detached::Removed { .. })),
            "{lines:?}"
        );
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
        assert_eq!(std::fs::read_link(fx.seam("trust-dev")).unwrap(), elsewhere);
        assert!(fx.seams_recorded().is_empty());
    }

    #[test]
    fn unknown_record_keys_are_ignored_never_acted_on() {
        let fx = Fixture::new("unknown-keys");
        crate::status::write(
            &fx.layout,
            &crate::Status {
                schema: 1,
                seams: vec![
                    "rustup:stable".into(),
                    "path:/usr/local/bin".into(),
                    "rustup:trust".into(),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(recorded_names(&fx.layout), vec!["trust".to_string()]);
    }

    /// The view: one clone per tool under its own name, each stock name a clone of its
    /// Trust tool — its bytes on a new inode, never the store's own — `lib/` a real
    /// directory of clones, and not one store file gaining a link.
    #[cfg(unix)]
    #[test]
    fn the_view_is_a_clone_of_the_build_with_the_stock_names_laid() {
        let fx = Fixture::new("view");
        let build = fx.install_trust(6808);
        let r = refresh_view(&fx.layout, "trust").unwrap();
        assert_eq!(r.build, build);
        assert_eq!(r.tools, 4);
        assert_eq!(
            r.stock,
            vec![
                ("rustc", "trustc"),
                ("cargo", "targo"),
                ("rustdoc", "trustdoc")
            ]
        );
        assert!(r.changed);
        let bin = view_dir(&fx.layout, "trust").join("bin");
        for (public, trust) in STOCK_NAMES {
            assert!(
                cloned(&build.join("bin").join(trust), &bin.join(public)),
                "{public} is a clone of the store's {trust}"
            );
        }
        assert!(cloned(&build.join("bin").join("tippy"), &bin.join("tippy")));
        // `lib/` is a real directory of clones, not a symlink, so a tool run from the view
        // resolves its sysroot to the view.
        let lib = view_dir(&fx.layout, "trust").join("lib");
        assert!(
            std::fs::symlink_metadata(&lib).unwrap().is_dir(),
            "a real directory"
        );
        assert!(cloned(
            &build.join("lib").join("libtrust.dylib"),
            &lib.join("libtrust.dylib")
        ));
        // Nothing of the store gained a link: a hard link would have.
        for tool in ["trustc", "targo", "trustdoc", "tippy"] {
            use std::os::unix::fs::MetadataExt as _;
            let nlink = std::fs::symlink_metadata(build.join("bin").join(tool))
                .unwrap()
                .nlink();
            assert_eq!(nlink, 1, "the store's {tool} has {nlink} links");
        }
        assert_eq!(
            std::fs::read_link(lib.join("rustlib").join("etc-link")).unwrap(),
            Path::new("../../etc")
        );
        // Idempotent: the same set is not a change.
        assert!(!refresh_view(&fx.layout, "trust").unwrap().changed);
    }

    /// A stock-name copy an older bundle shipped is not presented: the view's `rustc` is a
    /// clone of the store's `trustc`.
    #[cfg(unix)]
    #[test]
    fn a_stock_copy_the_bundle_shipped_is_replaced_by_a_clone_of_the_trust_tool() {
        let fx = Fixture::new("view-copy");
        let build = fx.install_trust(8595);
        std::fs::write(build.join("bin").join("rustc"), b"a second copy of trustc").unwrap();
        let r = refresh_view(&fx.layout, "trust").unwrap();
        assert_eq!(r.tools, 5);
        let bin = view_dir(&fx.layout, "trust").join("bin");
        assert!(cloned(
            &build.join("bin").join("trustc"),
            &bin.join("rustc")
        ));
        assert_eq!(
            std::fs::read(bin.join("rustc")).unwrap(),
            b"trustc of build 8595"
        );
    }

    /// The view follows `current`: after an update the stock names are clones of the new
    /// build's tools.
    #[cfg(unix)]
    #[test]
    fn the_view_follows_current_across_an_update() {
        let fx = Fixture::new("view-update");
        let old = fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let bin = view_dir(&fx.layout, "trust").join("bin");
        assert!(cloned(&old.join("bin").join("trustc"), &bin.join("rustc")));
        let new = fx.install_trust(6809);
        let lines = reassert(&fx.layout, &fx.rustup);
        // The view moved under an unchanged entry: said, in one line naming the build
        // it now presents.
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(
                attached(&lines[0]),
                Some(Attached::Adopted { view_build: Some(b), .. }) if *b == new
            ),
            "{lines:?}"
        );
        assert!(
            reassert(&fx.layout, &fx.rustup).is_empty(),
            "the same build again is a quiet adoption"
        );
        assert!(cloned(&new.join("bin").join("trustc"), &bin.join("rustc")));
        assert_eq!(
            std::fs::read(bin.join("cargo")).unwrap(),
            b"targo of build 6809"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust"),
            "the rustup entry never moved; only the view's contents did"
        );
    }

    /// A view a live process runs from is left as it stands, for the executable's path in
    /// either spelling and for an unreadable table. The table is read once per refresh, never
    /// for a first lay or a current view; a process in a sibling view or the store build
    /// holds nothing.
    #[cfg(unix)]
    #[test]
    fn a_view_a_live_process_runs_from_is_left_as_it_stands() {
        let fx = Fixture::new("view-in-use");
        fx.install_trust(9178);
        let first = refresh_view_with(&fx.layout, "trust", &|| -> Option<Vec<PathBuf>> {
            panic!("a first lay has nothing a process could run from, and reads no table")
        })
        .unwrap();
        assert!(first.changed && first.deferred.is_none(), "{first:?}");
        let view = view_dir(&fx.layout, "trust");
        let reads = &std::cell::Cell::new(0_u32);
        let table = |exes: Option<Vec<PathBuf>>| {
            move || {
                reads.set(reads.get() + 1);
                exes.clone()
            }
        };
        let current = refresh_view_with(&fx.layout, "trust", &table(Some(Vec::new()))).unwrap();
        assert!(!current.changed && current.deferred.is_none());
        assert_eq!(reads.get(), 0, "a current view reads no table");

        let new = fx.install_trust(9192);
        let exe = view.join("bin").join("targo");
        let resolved = std::fs::canonicalize(&exe).unwrap();
        for (running, why) in [
            (Some(vec![exe.clone()]), Deferred::InUse(exe.clone())),
            (
                Some(vec![resolved.clone()]),
                Deferred::InUse(resolved.clone()),
            ),
            (None, Deferred::Unknown),
        ] {
            reads.set(0);
            let r = refresh_view_with(&fx.layout, "trust", &table(running)).unwrap();
            assert_eq!(reads.get(), 1, "the table is read once per refresh");
            assert!(!r.changed, "{r:?}");
            assert_eq!(r.deferred, Some(why), "{r:?}");
            assert_eq!(r.build, new, "the build it was NOT re-laid onto");
            assert_eq!(
                std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
                "trustc of build 9178",
                "the running build's sibling compiler stays"
            );
            assert_eq!(
                std::fs::read_to_string(view.join("lib").join("libtrust.dylib")).unwrap(),
                "driver of 9178",
                "and so does the driver it loads"
            );
        }

        let elsewhere = vec![
            view_dir(&fx.layout, "trust-dev").join("bin").join("targo"),
            new.join("bin").join("targo"),
        ];
        let r = refresh_view_with(&fx.layout, "trust", &table(Some(elsewhere))).unwrap();
        assert!(r.changed && r.deferred.is_none(), "{r:?}");
        assert_eq!(
            std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
            "trustc of build 9192"
        );
        assert!(view_matches(&new, &view, Depth::Deep));
    }

    /// A view a command under `aterm pkg lease` holds is left as it stands (between two of
    /// its steps nothing runs from it); the line names the holder, and once the run ends
    /// the view follows `current`.
    #[cfg(unix)]
    #[test]
    fn a_leased_view_is_left_as_it_stands_until_the_run_ends() {
        let fx = Fixture::new("view-leased");
        fx.install_trust(9178);
        refresh_view(&fx.layout, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        let lease = crate::lease::take(
            &fx.layout.prefix,
            &crate::lease::Subject::view("trust").unwrap(),
            "targo (pid 7) — held by `aterm pkg lease` in /w",
            crate::lease::DEFAULT_WAIT,
        )
        .unwrap();
        let new = fx.install_trust(9192);
        let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
        let Some(Deferred::Leased(holders)) = &r.deferred else {
            panic!("a leased view is left: {r:?}");
        };
        assert!(holders.clause().contains("targo (pid 7)"), "{holders:?}");
        assert_eq!(
            std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
            "trustc of build 9178",
            "the run's compiler stays"
        );
        drop(lease);
        let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
        assert!(r.changed && r.deferred.is_none(), "{r:?}");
        assert!(view_matches(&new, &view, Depth::Deep));
    }

    /// A view left behind under a lease is recorded: the park's re-check finds nothing due
    /// while the lease stands and [`crate::quiet::Due::View`] once the run ends, and the pass
    /// it wakes lays the view and ends the record.
    #[cfg(unix)]
    #[test]
    fn a_view_a_flip_left_behind_is_recorded_and_due_once_its_run_ends() {
        let fx = Fixture::new("view-left-due");
        fx.install_trust(100);
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Created { .. })),
            "{lines:?}"
        );
        let view = view_dir(&fx.layout, "trust");
        let quiet = || Some(Vec::new());
        let t0 = 1_790_000_000;
        let lease = crate::lease::take(
            &fx.layout.prefix,
            &crate::lease::Subject::view("trust").unwrap(),
            "targo (pid 7) — held by `aterm pkg lease` in /w",
            crate::lease::DEFAULT_WAIT,
        )
        .unwrap();
        // The flip lands (a person's update) while the run holds the view.
        let new = fx.install_trust(200);
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines
                .iter()
                .any(|e| matches!(e, SeamEvent::ViewLeft { .. })),
            "{lines:?}"
        );
        let left = crate::quiet::views_left(&fx.layout);
        assert_eq!(
            left.views.get("trust").map(|v| v.build.clone()),
            Some(new.display().to_string()),
            "the view left behind is recorded: {left:?}"
        );
        assert_eq!(
            crate::quiet::due_with(&fx.layout, t0, &quiet),
            None,
            "nothing is due while the run holds the view"
        );
        drop(lease);
        let due = crate::quiet::due_with(&fx.layout, t0, &quiet);
        assert_eq!(
            due.map(|d| d.why),
            Some(crate::quiet::Due::View),
            "due the moment the run ends"
        );
        // …but not while a build runs from it.
        let exe = view.join("bin").join("targo");
        assert_eq!(
            crate::quiet::due_with(&fx.layout, t0, &|| Some(vec![exe.clone()])),
            None
        );
        // The pass the wake runs lays the view and ends the record.
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            !lines
                .iter()
                .any(|e| matches!(e, SeamEvent::ViewLeft { .. })),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
            "trustc of build 200"
        );
        assert!(crate::quiet::views_left(&fx.layout).views.is_empty());
        assert_eq!(crate::quiet::due_with(&fx.layout, t0, &quiet), None);
    }

    /// With `~/.rustup/toolchains` gone no re-assertion reaches a seam, so the one that finds
    /// no rustup ends the record of a view left behind. Control: with rustup present and the
    /// lease held, the record stands.
    #[cfg(unix)]
    #[test]
    fn a_view_left_behind_ends_when_rustup_is_gone() {
        let fx = Fixture::new("view-left-no-rustup");
        fx.install_trust(100);
        let _ = reassert(&fx.layout, &fx.rustup);
        let lease = crate::lease::take(
            &fx.layout.prefix,
            &crate::lease::Subject::view("trust").unwrap(),
            "targo (pid 7) — held by `aterm pkg lease` in /w",
            crate::lease::DEFAULT_WAIT,
        )
        .unwrap();
        fx.install_trust(200);
        let _ = reassert(&fx.layout, &fx.rustup);
        assert!(
            crate::quiet::views_left(&fx.layout)
                .views
                .contains_key("trust"),
            "the view is left behind and recorded"
        );
        let _ = reassert(&fx.layout, &fx.rustup);
        assert!(
            crate::quiet::views_left(&fx.layout)
                .views
                .contains_key("trust"),
            "the negative control: with rustup there, the record stands"
        );
        std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
        assert!(
            crate::quiet::views_left(&fx.layout).views.is_empty(),
            "rustup gone: nothing waits for a view"
        );
        assert!(!crate::quiet::view_record_path(&fx.layout).exists());
        drop(lease);
    }

    /// The dev-link arm takes a clone view down to lay its stubs; not while a build runs
    /// from that clone view. `link` switches rustup's compiler for the next build.
    #[cfg(unix)]
    #[test]
    fn a_dev_link_does_not_take_down_a_view_a_build_runs_from() {
        let fx = Fixture::new("dev-link-in-use");
        let store = fx.install_trust(6808);
        refresh_view(&fx.layout, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        let exe = view.join("bin").join("targo");
        let checkout = sysroot_checkout(&fx, "stage2");
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc"), PathBuf::from("bin/targo")],
        )
        .unwrap();
        let r = refresh_view_with(&fx.layout, "trust", &|| Some(vec![exe.clone()])).unwrap();
        assert_eq!(r.deferred, Some(Deferred::InUse(exe.clone())), "{r:?}");
        assert_eq!(r.build, checkout);
        assert!(cloned(
            &store.join("bin").join("trustc"),
            &view.join("bin").join("trustc")
        ));
        assert!(
            is_real_dir(&view.join("lib")),
            "the clone view's lib/ stays"
        );
        let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
        assert!(r.changed && r.deferred.is_none(), "{r:?}");
        assert_eq!(
            std::fs::read_link(view.join("lib")).unwrap(),
            checkout.join("lib")
        );
        crate::linkmode::unlink(&fx.layout, "trust").unwrap();
    }

    /// A pass that leaves the view behind still clears a refusal the disk has answered: every
    /// recorded refusal is decided before the view is touched, so the pass re-asked it. A
    /// failed re-lay is `Io`, which is never recorded.
    #[cfg(unix)]
    #[test]
    fn a_deferral_keeps_no_refusal_the_disk_answered_and_a_failed_relay_records_none() {
        let fx = Fixture::new("left-view-refusal");
        fx.install_trust(6808);
        assert_eq!(
            reassert(&fx.layout, &fx.rustup).len(),
            1,
            "the first attach"
        );
        let exe = view_dir(&fx.layout, "trust").join("bin").join("targo");
        // A stage2 mid-rebuild, its `lib/` not back yet: refused, and recorded.
        let checkout = sysroot_checkout(&fx, "stage2");
        let aside = fx.root.path().join("stage2-lib-aside");
        std::fs::rename(checkout.join("lib"), &aside).unwrap();
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc")],
        )
        .unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::LinkedNoSysroot { .. })),
            "{lines:?}"
        );
        assert_eq!(refusals(&fx.layout).len(), 1);

        // The rebuild finishes while a build still runs from the store's view.
        std::fs::rename(&aside, checkout.join("lib")).unwrap();
        let in_use = |l: &Layout, n: &str| refresh_view_with(l, n, &|| Some(vec![exe.clone()]));
        let lines = reassert_with(&fx.layout, &fx.rustup, &in_use, &|| Some(Vec::new()), false);
        assert!(
            lines.len() == 1
                && matches!(&lines[0], SeamEvent::ViewLeft { build, .. } if *build == checkout),
            "{lines:?}"
        );
        assert!(
            refusals(&fx.layout).is_empty(),
            "the checkout is a sysroot again, and the record says what the disk does: {:?}",
            refusals(&fx.layout)
        );

        // A re-lay that fails is said, and not recorded: `Io` is a hiccup, not a posture.
        let failed = |_: &Layout, _: &str| -> io::Result<Refreshed> {
            Err(io::Error::other("clonefile: operation not supported"))
        };
        let lines = reassert_with(&fx.layout, &fx.rustup, &failed, &|| Some(Vec::new()), false);
        assert!(
            lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::Io(_))),
            "{lines:?}"
        );
        assert!(
            refusals(&fx.layout).is_empty(),
            "{:?}",
            refusals(&fx.layout)
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        crate::linkmode::unlink(&fx.layout, "trust").unwrap();
    }

    /// End to end with a real process: a copy of this test binary stands in the store as
    /// `targo` and runs from the view ([`probe_parks_in_the_view_until_killed`]). An update
    /// leaves the view as it stands and says so; once the process is gone the next
    /// re-assertion lays the new build. The view's copy is a new file macOS assesses on its
    /// first exec, so nothing replaces or removes it before the probe wrote [`VIEW_PARKED`]
    /// (the kernel's path for it shows while it still waits for the verdict); a probe that
    /// never does leaves the fixture in place.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn reassert_leaves_a_view_a_live_build_runs_from_and_says_so() {
        if std::env::var_os(VIEW_PROBE_ENV).is_some() {
            return;
        }
        // Dropped (removed) only once the probe has run; never by an unwind before that.
        let fx = std::mem::ManuallyDrop::new(Fixture::new("view-live"));
        let old = fx.install_trust(100);
        let store_targo = old.join("bin").join("targo");
        std::fs::remove_file(&store_targo).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &store_targo)
            .expect("copy this test binary");
        std::fs::set_permissions(&store_targo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Created { .. })),
            "{lines:?}"
        );
        let view = view_dir(&fx.layout, "trust");
        // Under a translator the kernel names it as the executable of every process it
        // runs (Rosetta: `/mnt/lima-rosetta/rosetta`), so no process table shows a build
        // running from the view, the rest of this case's question.
        if let Some(translator) = aterm_uds::process::translator() {
            eprintln!(
                "NOT RUN under {translator}: seam::tests::reassert_leaves_a_view_a_live_build_\
                 runs_from_and_says_so: the probe's executable reads as the translator, never \
                 the view's targo; a CPU of this code's architecture decides it"
            );
            // Nothing ran from the fixture.
            drop(std::mem::ManuallyDrop::into_inner(fx));
            return;
        }

        /// Kills the probe however the test ends, so no stray outlives a failed assertion.
        struct Probe(aterm_uds::spawnfd::Child);
        impl Drop for Probe {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        // `spawnfd`'s: the probe holds its stdio and nothing else of this
        // process, not another test thread's pipe while it lives.
        let mut probe = Probe(
            aterm_uds::spawnfd::command(view.join("bin").join("targo"))
                .args(["--exact", "--nocapture", "--quiet", VIEW_PROBE])
                .env(VIEW_PROBE_ENV, "1")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("spawn the view's targo"),
        );
        // `spawn` does not promise the exec happened, and the kernel's path for the probe
        // shows before its first exec's assessment is done: its line is the proof it runs.
        aterm_uds::deadline::prints(
            &mut probe.0,
            VIEW_PARKED,
            aterm_uds::deadline::FIRST_EXEC_HANG,
        );
        let fx = std::mem::ManuallyDrop::into_inner(fx);
        assert_eq!(
            crate::gc::runs_from(&view),
            Some(true),
            "PRECONDITION: the process table shows the probe running from {}",
            view.display()
        );

        let new = fx.install_trust(200);
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(
                &lines[0],
                SeamEvent::ViewLeft { build, why: Deferred::InUse(_), .. } if *build == new
            ),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
            "trustc of build 100",
            "a live build's next compiler did not change under it"
        );
        assert!(cloned(&store_targo, &view.join("bin").join("targo")));

        let _ = probe.0.kill();
        let _ = probe.0.wait();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1
                && matches!(
                    attached(&lines[0]),
                    Some(Attached::Adopted { view_build: Some(b), .. }) if *b == new
                ),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
            "trustc of build 200"
        );
    }

    /// The env that puts the copy above into its parked mode, and the case it runs there.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const VIEW_PROBE_ENV: &str = "ATPKG_SEAM_VIEW_PROBE";
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const VIEW_PROBE: &str = "seam::tests::probe_parks_in_the_view_until_killed";
    /// The line the probe writes once it runs from the view.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const VIEW_PARKED: &str = "parked in the view";

    /// The copy's whole job: say [`VIEW_PARKED`], then run from the view until the parent
    /// kills it — or goes away, so a parent that dies first leaves no stray in the process
    /// table.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn probe_parks_in_the_view_until_killed() {
        if std::env::var_os(VIEW_PROBE_ENV).is_none() {
            return;
        }
        println!("{VIEW_PARKED}");
        // SAFETY: `getppid` takes no arguments, reads no memory we own and cannot fail.
        let spawner = unsafe { libc::getppid() };
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            // SAFETY: as above.
            if unsafe { libc::getppid() } != spawner {
                return;
            }
        }
    }

    /// A dev checkout shaped like a sysroot: Trust-named tools in `bin/`, a `lib/`
    /// with a driver, a `share/` — and no stock names, like the distribution.
    #[cfg(unix)]
    fn sysroot_checkout(fx: &Fixture, label: &str) -> PathBuf {
        let dir = fx.root.path().join(label);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("share")).unwrap();
        std::fs::write(
            dir.join("lib").join("libtrust.dylib"),
            "driver of the checkout",
        )
        .unwrap();
        for tool in ["trustc", "targo", "tippy"] {
            std::fs::write(dir.join("bin").join(tool), format!("{tool} of {label}")).unwrap();
        }
        dir
    }

    /// A dev-linked trust is what rustup's `trust` presents: exec stubs into the checkout's
    /// `bin/` (stock names to their Trust tools) and links to its directories. A tool rebuilt
    /// in place runs with no re-lay; `unlink` returns the store's clone view.
    #[cfg(unix)]
    #[test]
    fn a_dev_linked_trust_is_what_rustup_presents_until_it_is_unlinked() {
        let fx = Fixture::new("dev-link");
        let store = fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        let bin = view.join("bin");
        assert!(cloned(
            &store.join("bin").join("trustc"),
            &bin.join("rustc")
        ));
        assert_eq!(view_source(&fx.layout), ViewSource::Store);

        let checkout = sysroot_checkout(&fx, "stage2");
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc"), PathBuf::from("bin/targo")],
        )
        .unwrap();
        assert_eq!(
            view_source(&fx.layout),
            ViewSource::Linked(checkout.clone())
        );
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(
                attached(&lines[0]),
                Some(Attached::Adopted { view_build: Some(b), .. }) if *b == checkout
            ),
            "{lines:?}"
        );
        for (name, target) in [
            ("trustc", "trustc"),
            ("targo", "targo"),
            ("tippy", "tippy"),
            ("rustc", "trustc"),
            ("cargo", "targo"),
        ] {
            let stub = bin.join(name);
            assert!(
                std::fs::symlink_metadata(&stub).unwrap().is_file(),
                "{name} is a stub, not a link: targo and tippy refuse a symlinked executable"
            );
            assert_eq!(
                crate::platform::resolve_shim(&stub).unwrap(),
                checkout.join("bin").join(target),
                "{name} execs the checkout's tool"
            );
        }
        assert!(
            std::fs::read_dir(&bin).unwrap().count() == 5,
            "nothing extra in the view's bin"
        );
        assert_eq!(
            std::fs::read_link(view.join("lib")).unwrap(),
            checkout.join("lib")
        );
        assert_eq!(
            std::fs::read_link(view.join("share")).unwrap(),
            checkout.join("share")
        );
        assert!(
            std::fs::symlink_metadata(view.join("libexec")).is_err(),
            "a directory the checkout lacks is not presented"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust"),
            "the rustup entry never moved"
        );
        assert!(
            reassert(&fx.layout, &fx.rustup).is_empty(),
            "the same checkout again is a quiet adoption"
        );
        assert_eq!(
            view_source(&fx.layout),
            ViewSource::Linked(checkout.clone()),
            "doctor compares the view against the checkout's tools"
        );
        // The checkout is rebuilt in place — a new inode under the same name, as a
        // build's copy makes: the stub names the path, so rustup's `rustc` runs the new
        // file with no re-lay (a hard link would still name the old inode).
        let rebuilt = checkout.join("bin").join("trustc");
        std::fs::remove_file(&rebuilt).unwrap();
        std::fs::write(&rebuilt, "trustc, rebuilt").unwrap();
        assert_eq!(
            std::fs::read(crate::platform::resolve_shim(&bin.join("rustc")).unwrap()).unwrap(),
            b"trustc, rebuilt"
        );
        assert!(
            reassert(&fx.layout, &fx.rustup).is_empty(),
            "a rebuilt checkout needs no re-lay: the stubs name paths"
        );
        // The store was never written: the clone view taken down held its own inodes, and
        // each store file has its one name.
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(store.join("bin").join("trustc"))
                    .unwrap()
                    .nlink(),
                1
            );
            assert_eq!(
                std::fs::read(store.join("bin").join("trustc")).unwrap(),
                b"trustc of build 6808"
            );
        }

        // Unlinked: the store's clone view returns, and it is said.
        crate::linkmode::unlink(&fx.layout, "trust").unwrap();
        assert_eq!(view_source(&fx.layout), ViewSource::Store);
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(
                attached(&lines[0]),
                Some(Attached::Adopted { view_build: Some(b), .. }) if *b == store
            ),
            "{lines:?}"
        );
        assert!(cloned(
            &store.join("bin").join("trustc"),
            &bin.join("rustc")
        ));
        assert!(
            std::fs::symlink_metadata(view.join("lib"))
                .unwrap()
                .file_type()
                .is_dir(),
            "lib/ is a mirrored directory again, not a link"
        );
        assert!(
            std::fs::symlink_metadata(view.join("share")).is_err(),
            "the checkout's share/ went with it (the build ships none)"
        );
        assert!(
            std::fs::read_dir(&bin)
                .unwrap()
                .flatten()
                .all(|e| crate::platform::resolve_shim(&e.path()).is_none()),
            "no stub survives in the store's view: every entry is the store's own file"
        );
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    }

    /// A dev-link to a checkout that is not a sysroot is refused and recorded, the view left
    /// presenting the store; the refusal clears once the link goes.
    #[cfg(unix)]
    #[test]
    fn a_dev_link_to_a_tree_that_is_no_sysroot_is_refused_and_recorded() {
        let fx = Fixture::new("dev-link-nosysroot");
        let store = fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let bin = view_dir(&fx.layout, "trust").join("bin");
        let proj = fx.root.path().join("proj");
        std::fs::create_dir_all(proj.join("bin")).unwrap();
        std::fs::write(proj.join("bin").join("trustc"), "a lone binary").unwrap();
        crate::linkmode::link(&fx.layout, "trust", &proj, &[PathBuf::from("bin/trustc")]).unwrap();
        assert_eq!(
            view_source(&fx.layout),
            ViewSource::LinkedNoSysroot(proj.clone())
        );
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(
                refusal(&lines[0]),
                Some(Refusal::LinkedNoSysroot { checkout }) if *checkout == proj
            ),
            "{lines:?}"
        );
        assert_eq!(
            refusals(&fx.layout).len(),
            1,
            "recorded for Settings and doctor"
        );
        assert!(
            cloned(&store.join("bin").join("trustc"), &bin.join("rustc")),
            "the view still presents the store"
        );
        assert_eq!(
            view_source(&fx.layout),
            ViewSource::LinkedNoSysroot(proj.clone()),
            "…and doctor compares it against the store"
        );
        crate::linkmode::unlink(&fx.layout, "trust").unwrap();
        assert!(
            reassert(&fx.layout, &fx.rustup).is_empty(),
            "a quiet adoption"
        );
        assert!(
            refusals(&fx.layout).is_empty(),
            "the refusal cleared with the link"
        );
    }

    /// A checkout inside atpkg's own prefix (its stubs would exec themselves) is refused as no
    /// sysroot; a hand-edited stub is re-laid.
    #[cfg(unix)]
    #[test]
    fn a_checkout_inside_the_prefix_is_refused_and_an_edited_stub_is_relaid() {
        let fx = Fixture::new("dev-link-shapes");
        fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        // The view is a sysroot by shape (a bin/ and a lib/) — and inside the prefix.
        crate::linkmode::link(&fx.layout, "trust", &view, &[PathBuf::from("bin/trustc")]).unwrap();
        assert_eq!(
            view_source(&fx.layout),
            ViewSource::LinkedNoSysroot(view.clone())
        );
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1
                && matches!(
                    refusal(&lines[0]),
                    Some(Refusal::LinkedNoSysroot { checkout }) if *checkout == view
                ),
            "{lines:?}"
        );
        assert!(
            std::fs::symlink_metadata(view.join("lib"))
                .unwrap()
                .file_type()
                .is_dir(),
            "the store's view stands untouched"
        );
        crate::linkmode::unlink(&fx.layout, "trust").unwrap();

        let checkout = sysroot_checkout(&fx, "stage2");
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc")],
        )
        .unwrap();
        assert_eq!(reassert(&fx.layout, &fx.rustup).len(), 1);
        let stub = view.join("bin").join("rustc");
        let laid = std::fs::read(&stub).unwrap();
        std::fs::write(&stub, b"#!/bin/sh\nrm -rf something\nexec 'x'\n").unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1
                && matches!(
                    attached(&lines[0]),
                    Some(Attached::Adopted {
                        view_build: Some(_),
                        ..
                    })
                ),
            "an edited stub is a changed view: {lines:?}"
        );
        assert_eq!(std::fs::read(&stub).unwrap(), laid, "re-laid byte for byte");
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    }

    /// A dev-linked sysroot with no store build still gets the seam, pointing at a view of the
    /// checkout.
    #[cfg(unix)]
    #[test]
    fn a_dev_linked_trust_with_no_store_build_still_gets_the_seam() {
        let fx = Fixture::new("dev-link-nostore");
        let checkout = sysroot_checkout(&fx, "stage2");
        crate::linkmode::link(
            &fx.layout,
            "trust",
            &checkout,
            &[PathBuf::from("bin/trustc")],
        )
        .unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(attached(&lines[0]), Some(Attached::Created { .. })),
            "{lines:?}"
        );
        let bin = view_dir(&fx.layout, "trust").join("bin");
        assert_eq!(
            crate::platform::resolve_shim(&bin.join("rustc")).unwrap(),
            checkout.join("bin").join("trustc")
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);

        // The checkout stops being a sysroot: refused and recorded, and detaching takes the
        // refusal with the seam.
        std::fs::remove_dir_all(checkout.join("lib")).unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::LinkedNoSysroot { .. })),
            "{lines:?}"
        );
        assert_eq!(refusals(&fx.layout).len(), 1);
        crate::linkmode::unlink(&fx.layout, "trust").unwrap();
        detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
        assert!(
            refusals(&fx.layout).is_empty(),
            "the refusal went with the seam"
        );
        assert!(fx.seams_recorded().is_empty());
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
    }

    /// A symlinked tool in the bundle's `bin/` is not presented: every Trust frontend
    /// refuses a symlinked sibling, so the view would only move the refusal.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_tool_in_the_bundle_is_left_out_of_the_view() {
        let fx = Fixture::new("view-symlink");
        let build = fx.install_trust(6808);
        std::fs::remove_file(build.join("bin").join("trustc")).unwrap();
        link(Path::new("targo"), &build.join("bin").join("trustc"));
        let r = refresh_view(&fx.layout, "trust").unwrap();
        assert_eq!(r.tools, 3);
        assert_eq!(r.stock, vec![("cargo", "targo"), ("rustdoc", "trustdoc")]);
        let bin = view_dir(&fx.layout, "trust").join("bin");
        assert!(std::fs::symlink_metadata(bin.join("trustc")).is_err());
        assert!(std::fs::symlink_metadata(bin.join("rustc")).is_err());
    }

    /// `(path, st_nlink, st_ctime, st_ctime_nsec)` of every entry under `dir`, sorted —
    /// the part of an inode's identity tippy's guard snapshots and a link or unlink moves.
    #[cfg(unix)]
    fn inode_stamps(dir: &Path) -> Vec<(PathBuf, u64, i64, i64)> {
        use std::os::unix::fs::MetadataExt;
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                let m = std::fs::symlink_metadata(&path).unwrap();
                if m.is_dir() {
                    stack.push(path.clone());
                }
                out.push((path, m.nlink(), m.ctime(), m.ctime_nsec()));
            }
        }
        out.sort();
        out
    }

    /// A second refresh of a matching view makes no link or unlink call: tippy pins the
    /// `st_nlink` and `st_ctime` of its executable and siblings. The control proves the stamps
    /// see a single link+unlink.
    #[cfg(unix)]
    #[test]
    fn a_second_refresh_of_a_matching_view_touches_no_store_inode() {
        let fx = Fixture::new("view-nochurn");
        let build = fx.install_trust(8595);
        std::fs::write(build.join("bin").join("rustc"), b"a second copy of trustc").unwrap();
        std::fs::create_dir_all(build.join("libexec")).unwrap();
        std::fs::write(build.join("libexec").join("helper"), b"helper").unwrap();
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        assert!(view_matches(&build, &view, Depth::Deep));
        let store_before = inode_stamps(&build);
        let view_before = inode_stamps(&view);
        // Far past the clock's resolution, so a moved ctime cannot hide in one tick.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let again = refresh_view(&fx.layout, "trust").unwrap();
        assert!(!again.changed);
        assert_eq!(again.tools, 5);
        assert_eq!(
            again.stock,
            vec![
                ("rustc", "trustc"),
                ("cargo", "targo"),
                ("rustdoc", "trustdoc")
            ],
            "the early return reports what a rebuild would have"
        );
        // A whole re-assertion too: attach adopts, and the view is only checked.
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
        assert_eq!(inode_stamps(&build), store_before, "a store inode moved");
        assert_eq!(
            inode_stamps(&view),
            view_before,
            "the view's own entries moved"
        );
        // Control: one link+unlink of one store file is visible to the stamps.
        let probe = build.join("bin").join("tippy");
        std::fs::hard_link(&probe, fx.root.path().join("probe-link")).unwrap();
        std::fs::remove_file(fx.root.path().join("probe-link")).unwrap();
        assert_ne!(inode_stamps(&build), store_before, "the stamps are blind");
    }

    /// `view_matches` catches a stock name hard-linked to its Trust tool (both depths), one
    /// with the Trust tool's attributes but other bytes (deep only), and a file planted deep in
    /// `lib/` (deep only); the next refresh heals each.
    #[cfg(unix)]
    #[test]
    fn view_matches_rejects_a_linked_or_rebytten_stock_name_and_a_planted_file() {
        let fx = Fixture::new("view-matches");
        let build = fx.install_trust(8595);
        let view = view_dir(&fx.layout, "trust");
        assert!(!view_matches(&build, &view, Depth::Shallow), "no view yet");
        refresh_view(&fx.layout, "trust").unwrap();
        assert!(view_matches(&build, &view, Depth::Shallow));
        assert!(view_matches(&build, &view, Depth::Deep));

        let rustc = view.join("bin").join("rustc");
        let trustc = build.join("bin").join("trustc");
        std::fs::remove_file(&rustc).unwrap();
        std::fs::hard_link(&trustc, &rustc).unwrap();
        assert!(!view_matches(&build, &view, Depth::Shallow));
        assert_eq!(
            first_mismatch(&build, &view, Depth::Deep),
            Some(rustc.clone())
        );
        refresh_view(&fx.layout, "trust").unwrap();
        assert!(cloned(&trustc, &rustc));
        assert!(view_matches(&build, &view, Depth::Deep));

        // Other bytes behind the Trust tool's own length, mode and time.
        let mut bytes = std::fs::read(&trustc).unwrap();
        bytes[0] ^= 0x20;
        std::fs::remove_file(&rustc).unwrap();
        std::fs::write(&rustc, &bytes).unwrap();
        std::fs::set_permissions(&rustc, std::fs::metadata(&trustc).unwrap().permissions())
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&rustc)
            .unwrap()
            .set_modified(std::fs::metadata(&trustc).unwrap().modified().unwrap())
            .unwrap();
        assert!(
            view_matches(&build, &view, Depth::Shallow),
            "the attributes cannot see it"
        );
        assert_eq!(
            first_mismatch(&build, &view, Depth::Deep),
            Some(rustc.clone()),
            "the bytes can"
        );
        assert!(refresh_view(&fx.layout, "trust").unwrap().changed);
        assert!(cloned(&trustc, &rustc));
        assert!(view_matches(&build, &view, Depth::Deep));

        let planted = view.join("lib").join("rustlib").join("planted.dylib");
        std::fs::write(&planted, b"not the store's").unwrap();
        assert!(
            view_matches(&build, &view, Depth::Shallow),
            "shallow reads only lib/'s top level"
        );
        assert_eq!(
            first_mismatch(&build, &view, Depth::Deep),
            Some(planted.clone())
        );
        refresh_view(&fx.layout, "trust").unwrap();
        assert!(std::fs::symlink_metadata(&planted).is_err(), "re-mirrored");
        assert!(view_matches(&build, &view, Depth::Deep));

        let etc_link = view.join("lib").join("rustlib").join("etc-link");
        std::fs::remove_file(&etc_link).unwrap();
        link(Path::new("../../elsewhere"), &etc_link);
        assert_eq!(first_mismatch(&build, &view, Depth::Deep), Some(etc_link));
        refresh_view(&fx.layout, "trust").unwrap();

        let stray = view.join("bin").join("stray");
        std::fs::write(&stray, b"x").unwrap();
        assert_eq!(first_mismatch(&build, &view, Depth::Shallow), Some(stray));
        refresh_view(&fx.layout, "trust").unwrap();
        assert!(view_matches(&build, &view, Depth::Deep));

        // Debris beside bin/ is not a mismatch: counting it would rebuild forever.
        std::fs::create_dir_all(view.join(".bin.old-1")).unwrap();
        assert!(view_matches(&build, &view, Depth::Deep));
    }

    /// Debris a killed rebuild left beside `bin/` (which `first_mismatch` ignores) is swept by
    /// the next refresh, and only names of the producer's exact shape.
    #[cfg(unix)]
    #[test]
    fn a_killed_rebuilds_debris_is_swept_by_the_next_refresh() {
        let fx = Fixture::new("view-debris");
        let build = fx.install_trust(8595);
        refresh_view(&fx.layout, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        assert!(view_matches(&build, &view, Depth::Deep));

        // What a kill in a rebuild leaves: another pid's staged and retired trees, each
        // holding clones of a whole sysroot.
        let ours: Vec<PathBuf> = [
            ".bin.old-424242",
            ".bin.tmp-7",
            ".lib.old-424242",
            ".share.tmp-99",
        ]
        .iter()
        .map(|name| {
            let dir = view.join(name);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("bin").join("trustc"), b"a superseded build").unwrap();
            dir
        })
        .collect();
        // …beside names this producer can never render, which are not ours to delete.
        let theirs: Vec<PathBuf> = [
            ".bin.old-notapid",
            ".bin.old-",
            ".notes.tmp-1",
            "bin.old-1",
            ".bin.keep-1",
        ]
        .iter()
        .map(|name| {
            let dir = view.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        })
        .collect();

        // The view already matches, so this refresh lays nothing: the sweep is all it does.
        assert!(!refresh_view(&fx.layout, "trust").unwrap().changed);

        for gone in &ours {
            assert!(
                std::fs::symlink_metadata(gone).is_err(),
                "{} still holds a superseded build's clones",
                gone.display()
            );
        }
        for kept in &theirs {
            assert!(kept.is_dir(), "{} is not ours to delete", kept.display());
        }
        assert!(
            view_matches(&build, &view, Depth::Deep),
            "and the view it did not touch still stands"
        );
    }

    /// A `.DS_Store` under the view's `share/` is re-mirrored away without moving a `bin/` or
    /// store `lib/` inode; a failing mirror fails before `bin/` on every retry; a stray in the
    /// view's `bin/` does re-lay `bin/` (the control).
    #[cfg(unix)]
    #[test]
    fn a_rebuild_relays_only_the_part_that_differs() {
        let fx = Fixture::new("view-parts");
        let build = fx.install_trust(8595);
        std::fs::create_dir_all(build.join("share")).unwrap();
        std::fs::write(build.join("share").join("README"), b"docs").unwrap();
        refresh_view(&fx.layout, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        assert!(view_matches(&build, &view, Depth::Deep));
        let store_bin = inode_stamps(&build.join("bin"));
        let store_lib = inode_stamps(&build.join("lib"));
        let view_bin = inode_stamps(&view.join("bin"));
        std::thread::sleep(std::time::Duration::from_millis(20));

        let ds_store = view.join("share").join(".DS_Store");
        std::fs::write(&ds_store, b"Finder").unwrap();
        let refreshed = refresh_view(&fx.layout, "trust").unwrap();
        assert!(!refreshed.changed, "bin/ did not change");
        assert!(std::fs::symlink_metadata(&ds_store).is_err());
        assert!(view_matches(&build, &view, Depth::Deep));
        assert_eq!(inode_stamps(&build.join("bin")), store_bin);
        assert_eq!(inode_stamps(&view.join("bin")), view_bin);
        assert_eq!(
            inode_stamps(&build.join("lib")),
            store_lib,
            "a share/ mismatch re-mirrored lib/"
        );

        if crate::platform::our_uid() != 0 {
            let locked = build.join("lib").join("rustlib").join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let first = refresh_view(&fx.layout, "trust");
            let second = refresh_view(&fx.layout, "trust");
            let (store_after, view_after) = (
                inode_stamps(&build.join("bin")),
                inode_stamps(&view.join("bin")),
            );
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::remove_dir(&locked).unwrap();
            assert!(first.is_err() && second.is_err(), "{first:?} {second:?}");
            assert_eq!(
                store_after, store_bin,
                "a failing mirror moved a store bin/ inode"
            );
            assert_eq!(view_after, view_bin);
        }

        std::fs::write(view.join("bin").join("stray"), b"x").unwrap();
        assert!(refresh_view(&fx.layout, "trust").unwrap().changed);
        assert!(view_matches(&build, &view, Depth::Deep));
    }

    /// A matching view is accepted only where [`Layout::ensure_dir`] would accept it: behind a
    /// symlink at `<prefix>/rustup` the refresh is refused, and a view directory at `0777` is
    /// hardened to `0700`, with no store inode moved.
    #[cfg(unix)]
    #[test]
    fn a_matching_view_behind_a_link_or_a_loose_mode_is_not_accepted() {
        let fx = Fixture::new("view-failclosed");
        let build = fx.install_trust(8595);
        refresh_view(&fx.layout, "trust").unwrap();
        let view = view_dir(&fx.layout, "trust");
        let store = inode_stamps(&build);
        std::thread::sleep(std::time::Duration::from_millis(20));

        let views = views_root(&fx.layout);
        let aside = fx.root.path().join("views-aside");
        std::fs::rename(&views, &aside).unwrap();
        link(&aside, &views);
        assert!(
            view_matches(&build, &view, Depth::Deep),
            "fixture: whole behind the link"
        );
        let refused = refresh_view(&fx.layout, "trust");
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.to_string().contains("symlink")),
            "{refused:?}"
        );
        std::fs::remove_file(&views).unwrap();
        std::fs::rename(&aside, &views).unwrap();

        std::fs::set_permissions(&view, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!refresh_view(&fx.layout, "trust").unwrap().changed);
        let mode = std::fs::symlink_metadata(&view)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o700);
        assert_eq!(inode_stamps(&build), store, "a store inode moved");
    }
}
