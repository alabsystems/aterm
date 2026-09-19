// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The seam owner (Lockstep, slice S1): atpkg OWNS the rustup toolchain seam.
//!
//! `rust-toolchain.toml` pins a rustup toolchain called `trust`, and rustup finds it
//! at `<rustup_home>/toolchains/trust`. That entry used to be a hand-made `rustup
//! toolchain link` naming wherever the operator last built the compiler, and it
//! dangled whenever the tree under it moved. This module makes the entry a symlink
//! atpkg lays and re-asserts: `<rustup_home>/toolchains/<name>` ->
//! `<prefix>/rustup/<name>`, atpkg's VIEW of the live trust build.
//!
//! WHY A VIEW AND NOT THE STORE. rustup's proxies and cargo's doctest phase resolve
//! the STOCK names — `rustc`, `cargo`, `rustdoc` — and the Trust distribution ships
//! only Trust's names: `trustc`, `targo`, `trustdoc` (trust `dist.rs`, 2026-09-14).
//! Until then the bundle carried a second COPY of each under the stock name, and one
//! compiler as two files needed a proof that they were the same compiler wherever one
//! of them was handed authority — a proof macOS makes impossible, because an ad-hoc
//! code signature bakes the file's own name into itself (measured on bundle 8595:
//! 2,428 differing bytes, every one inside the signature). tippy refused a correct
//! toolchain on that proof. The store is also content-addressed (`aterm pkg verify`),
//! so nothing may be laid inside a build after it is staged. The view answers both:
//! `<prefix>/rustup/<name>/bin/` holds a copy-on-write CLONE ([`crate::clone`]) of each
//! tool in `store/trust/current/bin/` under its own name, plus each stock name as a clone
//! of its Trust tool ([`STOCK_NAMES`]) — byte-identical to it, nothing to authenticate,
//! nothing of the store written: not a byte, not a link count, not a ctime — and `lib/`,
//! `libexec/`, `share/`, `etc/` cloned the same way, every regular file a clone, every
//! symlink recreated, so the view is a complete sysroot and every frontend read from it
//! answers the VIEW as its sysroot. It used to be a HARD-LINK mirror of the store's own
//! inodes, and every link and unlink moved a live store inode (the owner, 2026-09-16:
//! *"doing that with hardlinks sounds like bugs and indeed: bugs"* — the defects are in
//! [`crate::clone`]'s doc). The untracked lane still builds it ([`refresh_view`]): a file
//! a provenance-tracked process creates is tagged, and a toolchain run from tagged files
//! tags what it writes. That last clause is why they are mirrored and not symlinked:
//! rustc finds its sysroot through the real path of the driver dylib it loaded, so a
//! symlinked `lib/` made `rustc --print sysroot` answer the STORE, and every script
//! that finds the frontends beside that answer (`$(rustc --print sysroot)/bin/targo`,
//! the clean repo's ruled spelling) ran the store's tippy beside the store's copy —
//! measured 2026-09-15, the day the symlink form shipped. It is CHECKED by every
//! attach and every re-assertion and REBUILT only when it no longer matches
//! ([`refresh_view`], [`view_matches`]), so it follows `current` across updates and
//! rollbacks without touching a live view file when nothing moved — tippy pins its
//! own executable's and its siblings' link count and ctime, and the rebuild-every-call
//! form aborted every tippy in flight at each repair or update pass (measured
//! 2026-09-15).
//!
//! The installed bundles 8571, 8589, 8590 and 8595 predate the Trust-names-only
//! distribution and still ship `bin/rustc` (and `bin/cargo`) as separate copies, which
//! their own tippy refuses. The view presents no copy, but the view is rustup's, and it
//! exists only where the seam attaches. PATH is covered for those builds by
//! [`crate::compat`]: a per-build exec root laid by the same construction
//! ([`lay_view`]) and reached through a guard line in the `bin/` shims.
//!
//! WHEN TRUST IS DEV-LINKED, THE VIEW PRESENTS THE CHECKOUT (2026-09-16). `aterm pkg
//! link trust <checkout>` puts the checkout's tools on PATH and every other surface
//! reads the link as the newer decision that outranks the store — `which`, `list`,
//! doctor — but the seam kept building its view from `store/trust/current`, so
//! `cargo +trust`, `rustup run trust` and every repo pinning `channel = "trust"` ran
//! the INSTALLED compiler while `targo` on PATH ran the checkout's: two compilers
//! under one name, doctor calling the seam healthy (2026-09-15 audit). Now
//! [`view_source`] asks the link marker first: a dev-linked trust whose checkout is a
//! sysroot (a `bin/` and a `lib/`) is what the view presents — `bin/` one EXEC STUB
//! per tool (the `bin/` shim body, [`crate::platform::install_shim_to`]: a `/bin/sh`
//! line that `exec`s `<checkout>/bin/<tool>`) plus the stock names as stubs to their
//! Trust tools, and the mirrored directories as directory SYMLINKS to the checkout's.
//! Stubs, not symlinks, for `bin/`: targo and tippy refuse to run when the executable
//! the OS reports for them is a symlink (measured — see that function's doc; on macOS
//! `current_exe()` names the link, not its target), and a stub's `exec` makes the
//! process image the checkout's plain file at its real path, beside its real siblings.
//! Not hard links: a dev tree is rebuilt in place, and a hard link to a file the build
//! replaces goes stale silently, while a stub names the path and runs whatever stands
//! there. The checkout is not the sealed store, so nothing about content-addressing
//! applies, and `rustc --print sysroot` through a linked `lib/` answers the CHECKOUT,
//! which is the truth for every script that finds the frontends beside that answer.
//! `link` and `unlink` re-assert the seam themselves, so the view follows the decision
//! the moment it is made, and `unlink` returns the store's clone view. A dev-linked
//! trust whose checkout is not a sysroot is refused and recorded, the view left as it
//! stands — rustup cannot present a cargo project's `target/release`.
//!
//! The rules, all fail-closed:
//!
//! * Names come from a compiled-in allowlist ([`SEAM_NAMES`]); `trust` is the
//!   default. No other name is ever created or removed.
//! * If `<rustup_home>/toolchains` does not exist, rustup is not installed: attach is
//!   a no-op that says so. `~/.rustup` is NEVER created.
//! * An ABSENT entry is created atomically (temp symlink + `rename(2)` — the same
//!   primitive as `store/<p>/current`, [`crate::activate::atomic_symlink`]) after the
//!   view is refreshed. An existing symlink that resolves into `<prefix>/rustup/` or
//!   `<prefix>/store/trust/` is atpkg's: ADOPTED untouched when it names the view,
//!   RE-POINTED at the view when it names the store (`current`, or a numbered build —
//!   the layouts from before the view existed). Anything else — a real directory, a
//!   regular file, a symlink elsewhere — is REFUSED with the one fix, [`DETACH_FIX`].
//!   Nothing here ever follows an existing link.
//! * A view that already matches its build is left untouched. One that does not has
//!   each part that differs — a mirrored directory, or `bin/` — built beside the live one
//!   and swapped in by `rename(2)`, and the parts that still match are not re-laid. A
//!   clone that cannot be made (the store on another volume) REFUSES the attach: atpkg
//!   never byte-copies a toolchain in place of a clone.
//! * A successful attach is RECORDED in `status.toml` as `seams = ["rustup:trust"]`
//!   (load, modify, save through the atomic writer — other fields are never clobbered).
//! * Detach removes the entry only when it is a symlink resolving into the prefix (or
//!   `--force`, which also moves a real directory ASIDE rather than deleting it: a
//!   tree atpkg did not lay is never recursively removed), removes the view atpkg
//!   owned, and drops the record.
//! * The seam is RE-ASSERTED after every successful activation of `trust`, after a
//!   rollback of `trust`, and at the end of the unattended `update` pass, so a deleted
//!   or dangling entry heals on the next pass and the view follows the build. Every
//!   recorded seam is re-asserted, plus a FIRST attach of `trust` when rustup is present
//!   and the entry is absent (creating a name nothing else owns is safe by construction).
//! * `uninstall --all` detaches every recorded seam with the toolset it removes
//!   ([`detach_recorded`], from the CLI edge, before the build trees go).
//!
//! The library entry points take the rustup home as DATA ([`attach`], [`detach`],
//! [`status`], [`reassert`]); only the CLI edge reads `RUSTUP_HOME` / `HOME`
//! ([`rustup_home`], [`arm_from_env`]). flow.rs — whose tests activate a program
//! named `trust` inside temp layouts — calls [`reassert_if_armed`], which does nothing
//! unless the real process edge armed it, so no unit test can reach a developer's
//! live `~/.rustup`.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::Layout;

/// The compiled-in allowlist of rustup toolchain names atpkg will own.
pub const SEAM_NAMES: &[&str] = &["trust", "trust-dev"];

/// The seam every re-assertion attaches when nothing is recorded yet.
pub const DEFAULT_SEAM: &str = "trust";

/// The ONE store program a rustup seam ever points into.
pub const SEAM_PROGRAM: &str = "trust";

/// The one fix printed whenever attach refuses an entry atpkg does not own.
///
/// This read `aterm pkg seam detach --force` until 2026-09-01. There is no
/// `seam` verb: `VERBS` (`cli.rs:33-56`) lists twenty-two names and `seam` is
/// not among them, nor does the string occur anywhere else in `crates/atpkg`.
/// So the ONE remedy printed on this refusal named a command that exits 2 —
/// and the refusal is about an entry under `~/.rustup` that aterm deliberately
/// will not touch, so the action has to be the user's anyway. Both commands
/// below are real: `rustup toolchain uninstall` is rustup's own, and `repair`
/// is in `VERBS`.
pub const DETACH_FIX: &str =
    "remove that entry yourself (e.g. `rustup toolchain uninstall trust`), then `aterm pkg repair`";

/// The record-key prefix in `status.toml`'s `seams` list (`rustup:<name>`).
const RECORD_PREFIX: &str = "rustup:";

/// The `seams` spelling of a REFUSED re-assertion: `refused:rustup:<name>: <why>`.
///
/// Until 2026-09-10 a refusal was only `println!`d by the pass verbs — and the 6-hourly
/// pass the GUI spawns discards stdout, so a machine whose `~/.rustup/toolchains/trust`
/// pointed at a from-source dev build (m21, since Jul 19) re-printed the refusal into
/// the void every six hours while `status.toml` said `seams = []` and `doctor` said
/// healthy. Recorded here it reaches `aterm pkg status` and Settings. The prefix is
/// chosen so [`recorded_names`] — which strips [`RECORD_PREFIX`] — can never read a
/// refusal as a seam to re-assert.
pub const REFUSED_PREFIX: &str = "refused:";

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

/// Rustup's home as this process sees it: `$RUSTUP_HOME`, else `<home>/.rustup`.
/// `None` only when neither is resolvable (HOME unset on Unix).
#[must_use]
pub fn rustup_home() -> Option<PathBuf> {
    rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
    )
}

/// Pure core of [`rustup_home`], with the two inputs as data.
#[must_use]
pub fn rustup_home_with(env_rustup_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(v) = env_rustup_home
        && !v.is_empty()
    {
        return Some(PathBuf::from(v));
    }
    home.map(|h| h.join(".rustup"))
}

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
    layout.prefix.join("rustup")
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

/// The stock names rustup's proxies and cargo's doctest phase resolve, each laid in a
/// view as a clone of the Trust-named tool beside it. The distribution ships only
/// the Trust names (trust `dist.rs`, 2026-09-14); these exist for rustup alone, and
/// nothing inside the toolchain resolves them — `targo` asks for `trustc` and
/// `trustdoc` by name, and tippy runs `trustc`.
pub const STOCK_NAMES: &[(&str, &str)] = &[
    ("rustc", "trustc"),
    ("cargo", "targo"),
    ("rustdoc", "trustdoc"),
];

/// The directories beside `bin/` a sysroot is read through, CLONED into the view
/// (file by file, not a directory symlink — see the module doc) so a tool run from it
/// finds its own `lib/rustlib`, `libexec/` helpers and `share/` docs, and reports the
/// view as its sysroot.
const VIEW_DIRS: &[&str] = &["lib", "libexec", "share", "etc"];

/// Mirror `src` at `dst`: directories created, regular files CLONED
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
    /// Tools linked under their own names.
    pub tools: usize,
    /// Stock names laid, each with the Trust tool it is a link to.
    pub stock: Vec<(&'static str, &'static str)>,
    /// Whether the view's `bin/` changed (a rebuild that produced the same set is silent).
    pub changed: bool,
}

/// How closely [`view_matches`] holds a view — or a [`crate::compat`] exec root, which is
/// the same construction under another name — against the build it was laid from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// `bin/`'s whole name -> file map (every tool a clone of the store's, every stock
    /// name a clone of its Trust tool, nothing extra), the mirrored directories present
    /// as real directories, and the regular files directly in `lib/` — the driver dylibs
    /// every frontend loads — clones of the store's. A few dozen `lstat`s: cheap enough
    /// for every update pass.
    Shallow,
    /// Everything [`Depth::Shallow`] checks, and every entry under the mirrored
    /// directories: each regular file a clone of the store's, each symlink the same target,
    /// each directory a real one, nothing extra. A few thousand `lstat`s — a perl walk
    /// of the same shape over bundle 8595 (4,112 files) took 0.108 s, measured
    /// 2026-09-15 — for `repair`, and for every re-assertion of a rustup view.
    Deep,
}

/// What a view's `bin/` must hold for a build's `bin/` at `src_bin`, read with `lstat`
/// only: every regular file under its own name, then each of [`STOCK_NAMES`] whose Trust
/// tool is a regular file there — mapped to that Trust tool, REPLACING whatever the
/// bundle shipped under the stock name (bundles 8571/8589/8590/8595 ship a separate,
/// separately signed copy). A symlink in the build's `bin/` is not an entry: every Trust
/// frontend refuses a symlinked sibling, so a view never presents one.
struct BinPlan {
    /// Name in the view -> the store file it must be a clone of.
    entries: std::collections::BTreeMap<std::ffi::OsString, PathBuf>,
    /// Regular files in the build's `bin/`.
    tools: usize,
    /// Stock names laid, each with its Trust tool.
    stock: Vec<(&'static str, &'static str)>,
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

/// Lay a view's `bin/` at `dst_bin` (created here; it must not hold entries yet) from the
/// build's `bin/` at `src_bin`: one clone per [`BinPlan`] entry, so each tool is a clone
/// of the store's file and each stock name a clone of its Trust tool — byte-identical to
/// it, which is what bundle 8595's tippy asks of a `rustc` beside its `trustc`. A
/// stock-name copy the bundle shipped is never cloned at all. Returns the tool count and
/// the stock names laid.
///
/// # Errors
/// The build's `bin/` cannot be listed, or a clone cannot be made — the store on another
/// volume, and it refuses rather than byte-copying the toolchain.
fn lay_bin(
    layout: &Layout,
    src_bin: &Path,
    dst_bin: &Path,
) -> io::Result<(usize, Vec<(&'static str, &'static str)>)> {
    let plan = bin_plan(src_bin)?;
    layout.ensure_dir(dst_bin)?;
    for (name, src) in &plan.entries {
        crate::clone::clone_file(src, &dst_bin.join(name))?;
    }
    Ok((plan.tools, plan.stock))
}

/// Lay a complete view of `build` at `dest`, which must not exist: each of [`VIEW_DIRS`]
/// the build has MIRRORED by [`mirror_tree`] ([`lay_view_dirs`]), then `bin/` by
/// [`lay_bin`]. The one construction a rustup view and a [`crate::compat`] exec root
/// share; the caller owns the name it lays at (a dot-temp it renames into place).
///
/// `bin/` LAST, on purpose: a lay that fails under `lib/` (an unreadable entry, a clone
/// refused) returns before a single tool was laid, so a half-built tree never presents a
/// runnable frontend. (Under the hard-link construction this order was also what kept a
/// failing retry from moving the store inodes tippy pins; a clone moves none.)
///
/// # Errors
/// `dest` already exists, or anything [`lay_bin`] and [`mirror_tree`] refuse. A partial
/// tree is left at `dest` for the caller to remove.
#[cfg(unix)]
pub(crate) fn lay_view(layout: &Layout, build: &Path, dest: &Path) -> io::Result<()> {
    lay_view_dirs(layout, build, dest)?;
    lay_bin(layout, &build.join("bin"), &dest.join("bin"))?;
    Ok(())
}

/// [`lay_view`] without its `bin/`: `dest` (which must not exist) created, and each of
/// [`VIEW_DIRS`] the build has mirrored into it. What [`crate::compat::ensure_root`] lays
/// when a standing root's `bin/` already IS the build's, and it moves that live `bin/`
/// into the new tree by `rename(2)` instead of linking every tool again.
///
/// # Errors
/// `dest` already exists, or anything [`mirror_tree`] refuses. A partial tree is left at
/// `dest` for the caller to remove.
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

/// Whether the view at `view` still IS `build` at `depth` — [`first_mismatch`] found
/// nothing. `stat` only: it never links, unlinks, creates or chmods anything, which is
/// the whole point (see [`refresh_view`]).
#[must_use]
pub(crate) fn view_matches(build: &Path, view: &Path, depth: Depth) -> bool {
    first_mismatch(build, view, depth).is_none()
}

/// The first path at which the view at `view` stops being `build` at `depth`, or `None`
/// when it matches — the path a report names. Top-level entries beside `bin/` and the
/// mirrored directories are not compared: a `.bin.old-<pid>` a crashed rebuild left is
/// debris, not a mismatch, and counting it would rebuild the view on every call forever
/// (a rebuild removes only its own pid's debris). Elsewhere than Unix the view is not
/// compared, so every view reads as mismatched and is rebuilt, as it always was.
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

/// The half of [`first_mismatch`] over `bin/` alone, for a `view` already known to be a
/// real directory: `<view>/bin` a real directory holding exactly the [`BinPlan`] of the
/// build's `bin/`, every entry a clone of its store file. `bin/` is judged apart from the
/// mirrored directories so a rebuild can leave a `bin/` that still matches alone — its
/// entries are the files tippy pins. A hard link to the store (the construction before
/// clones) is NOT a match, so a view laid that way is rebuilt once.
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
/// presents must hold its Trust tool's BYTES. The attribute identity cannot see the one
/// wrong answer that matters — bundle 8595's separately signed `bin/rustc` has `trustc`'s
/// length, mode and time and 2,428 different bytes, and it is exactly the file the view
/// exists not to present (its tippy refuses it). Three files, about 38 MB on 8595, read
/// only at [`Depth::Deep`].
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

/// The half of [`first_mismatch`] over ONE of [`VIEW_DIRS`], `dir`, at `depth`: absent in
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

/// `lstat` says a real directory — a symlink to one is not.
///
/// NOT Unix-gated, and that is load-bearing: [`crate::compat::ensure_root_with`] asks it on
/// every target (its untracked-lane `Allow` arm, which decides whether a root already STANDS)
/// and carries no `cfg` of its own, so a `#[cfg(unix)]` here left `atpkg` failing to compile
/// for `x86_64-pc-windows-msvc` with E0425 — the `win` cell's own triple, measured
/// 2026-09-16. The body means the same sentence on every target: `symlink_metadata` does not
/// follow a Windows reparse point either, and a junction or a directory symlink answers
/// `is_symlink()`, never `is_dir()`. A `#[cfg(not(unix))]` twin returning `false` would have
/// compiled and been WRONG — it makes the "the root that stands is kept" branch unreachable
/// off Unix. `crates/atpkg/tests/platform_cfg_parity.rs` is the standing guard for the class.
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

/// The hidden verb the launchd job execs when this process is provenance-tracked —
/// machinery, not vocabulary: unlisted in help and `VERBS`, dispatched on the raw argv
/// before the store lock (its parent HOLDS that lock), served by name through the
/// `aterm` front door like its two siblings.
pub const HIDDEN_VERB: &str = "__refresh-view";

/// First line of a spec file — a version stamp, so a stale copy of this binary never
/// misreads a newer spec. `v2` (2026-09-16): the job lays CLONES; a helper binary from
/// before that would lay hard links the clone identity rejects on every pass, so it must
/// refuse the spec instead.
const SPEC_HEADER: &str = "atpkg-view-spec v2";

/// What the view helper is asked to lay: the rustup view for a seam name, or a trust
/// build's EXEC ROOT ([`crate::compat`]) — the same clone construction, so the same lane:
/// the files it creates must not carry a tracked process's provenance tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewJob {
    /// `<prefix>/rustup/<name>` from `store/trust/current`.
    Seam { name: String },
    /// `<prefix>/compat/trust/<build>` from the build at this directory.
    Root { build_dir: PathBuf },
}

/// Render the spec the helper reads: the store prefix and the job, hex-encoded (the
/// prefix is under `Library/Application Support`, which has a space).
#[must_use]
pub fn encode_spec(prefix: &Path, job: &ViewJob) -> String {
    let mut out = String::from(SPEC_HEADER);
    out.push('\n');
    out.push_str("prefix=");
    out.push_str(&crate::tree::hex(crate::call1(
        crate::platform::os_str_bytes,
        prefix.as_os_str(),
    )));
    match job {
        ViewJob::Seam { name } => {
            out.push_str("\nname=");
            out.push_str(&crate::tree::hex(name.as_bytes()));
        }
        ViewJob::Root { build_dir } => {
            out.push_str("\nroot=");
            out.push_str(&crate::tree::hex(crate::call1(
                crate::platform::os_str_bytes,
                build_dir.as_os_str(),
            )));
        }
    }
    out.push('\n');
    out
}

/// Parse a spec rendered by [`encode_spec`]. A foreign header, an unknown key, a bad
/// hex digit, a relative prefix or a name off the allowlist refuses the whole spec —
/// the helper must never build a view for a prefix it half-understood.
pub fn decode_spec(text: &str) -> Result<(PathBuf, ViewJob), String> {
    let mut lines = text.lines();
    if lines.next() != Some(SPEC_HEADER) {
        return Err(String::from("spec header missing or of another version"));
    }
    let (mut prefix, mut name, mut root) = (None, None, None);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("malformed spec line: {line:?}"));
        };
        match key {
            "prefix" => {
                prefix = Some(crate::stage_helper::path_of(crate::stage_helper::unhex(
                    value,
                )?));
            }
            "name" => {
                name = Some(
                    String::from_utf8(crate::stage_helper::unhex(value)?)
                        .map_err(|_| String::from("seam name is not UTF-8"))?,
                );
            }
            "root" => {
                root = Some(crate::stage_helper::path_of(crate::stage_helper::unhex(
                    value,
                )?));
            }
            other => return Err(format!("unknown spec key: {other:?}")),
        }
    }
    let prefix = prefix.ok_or_else(|| String::from("spec names no prefix"))?;
    if !prefix.is_absolute() {
        return Err(String::from("spec prefix must be absolute"));
    }
    match (name, root) {
        (Some(name), None) => {
            if !name_allowed(&name) {
                return Err(format!("seam name {name:?} is not on the allowlist"));
            }
            Ok((prefix, ViewJob::Seam { name }))
        }
        (None, Some(build_dir)) => {
            if !build_dir.is_absolute() {
                return Err(String::from("spec root must be absolute"));
            }
            Ok((prefix, ViewJob::Root { build_dir }))
        }
        (Some(_), Some(_)) => Err(String::from("spec names both a seam and a root")),
        (None, None) => Err(String::from("spec names neither a seam nor a root")),
    }
}

/// The one line the helper answers with: `<changed> <tools> <hex build> <stock pairs>`.
fn encode_refreshed(r: &Refreshed) -> String {
    let stock: Vec<String> = r.stock.iter().map(|(p, t)| format!("{p}:{t}")).collect();
    format!(
        "{} {} {} {}",
        u8::from(r.changed),
        r.tools,
        crate::tree::hex(crate::call1(
            crate::platform::os_str_bytes,
            r.build.as_os_str()
        )),
        stock.join(",")
    )
}

/// [`encode_refreshed`], read back. A stock pair the allowlist does not name is a
/// refusal: the parent hands out `&'static` names, never ones a result file spelled.
fn decode_refreshed(body: &str) -> Result<Refreshed, String> {
    let mut fields = body.split(' ');
    let changed = match fields.next() {
        Some("0") => false,
        Some("1") => true,
        other => return Err(format!("malformed view result: changed={other:?}")),
    };
    let tools = fields
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| String::from("malformed view result: tools"))?;
    let build = crate::stage_helper::path_of(crate::stage_helper::unhex(
        fields
            .next()
            .ok_or_else(|| String::from("malformed view result: build"))?,
    )?);
    let mut stock = Vec::new();
    for pair in fields
        .next()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
    {
        let Some((public, trust)) = pair.split_once(':') else {
            return Err(format!("malformed view result: stock pair {pair:?}"));
        };
        let Some(&(public, trust)) = STOCK_NAMES
            .iter()
            .find(|(p, t)| *p == public && *t == trust)
        else {
            return Err(format!("view result names an unknown stock pair {pair:?}"));
        };
        stock.push((public, trust));
    }
    Ok(Refreshed {
        build,
        tools,
        stock,
        changed,
    })
}

/// The hidden verb's body: `__refresh-view <spec-file>`. Reads the spec, builds the
/// view IN THIS (untracked) PROCESS — which is the whole point — and answers in
/// `<spec-dir>/result`: `ok\n<line>\n` ([`encode_refreshed`]) or `err\n<message>\n`,
/// temp + rename so the parent never reads a half-written answer. Touches nothing
/// else: no config, no store lock, no status.
pub fn run_helper(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    use std::process::ExitCode;
    crate::stage_helper::arm_parent_watchdog();
    let Some(spec_path) = args.first().map(PathBuf::from) else {
        eprintln!("atpkg {HIDDEN_VERB}: usage: {HIDDEN_VERB} <spec-file>");
        return ExitCode::from(2);
    };
    let result_path = spec_path.with_file_name("result");
    let outcome = (|| -> Result<String, String> {
        let text = std::fs::read_to_string(&spec_path)
            .map_err(|e| format!("read spec {}: {e}", spec_path.display()))?;
        let (prefix, job) = decode_spec(&text)?;
        let layout = Layout { prefix };
        match job {
            ViewJob::Seam { name } => {
                let refreshed =
                    refresh_view_in_process(&layout, &name).map_err(|e| e.to_string())?;
                Ok(encode_refreshed(&refreshed))
            }
            ViewJob::Root { build_dir } => {
                let ensured =
                    crate::compat::ensure_root_in_process(&layout, &build_dir, Depth::Deep)
                        .map_err(|e| e.to_string())?;
                Ok(format!("root {}", crate::compat::ensured_word(ensured)))
            }
        }
    })();
    let body = match &outcome {
        Ok(line) => format!("ok\n{line}\n"),
        Err(why) => format!("err\n{why}\n"),
    };
    let tmp = result_path.with_file_name("result.tmp");
    if std::fs::write(&tmp, body).is_err() || std::fs::rename(&tmp, &result_path).is_err() {
        eprintln!(
            "atpkg {HIDDEN_VERB}: cannot write {}",
            result_path.display()
        );
        return ExitCode::from(1);
    }
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Build, or rebuild, the view for `name` from `store/trust/current` — through the
/// UNTRACKED launchd lane when this process is provenance-tracked.
///
/// A FILE A TRACKED PROCESS CREATES IS TAGGED. macOS tags what a provenance-tracked
/// process writes (`crate::provenance`, measured 2026-09-12), and a toolchain run from
/// tagged files tags what it writes in turn. The view's first form was a hard-link mirror
/// built in-process by the app's own `atpkg` (0.86.0, 2026-09-15): a hard link counts as
/// a write, so it re-tagged every executable of the clean `trust/8595` bundle ITSELF on
/// the app's first pass (ctime 04:47:16 on all 21 files; `aterm pkg doctor`: "carries
/// com.apple.provenance on 21 of 21 file(s)"). A clone never writes the store, but the
/// clone's own new files would carry the tag just the same. So the view is
/// built the way the bundle is staged and the shims are laid: by a launchd job
/// running this binary (in place, from a clean copy, or from a copy of its whole
/// bundle — [`crate::stage_helper::plan_helper`]), with the outcome MEASURED on a
/// file that was clean before the job. A tracked process with no lane, or one whose
/// lane fails, does what [`crate::lay::tracked_policy`] says: builds in-process and
/// says so (the default — a view that follows the build beats one that does not, and
/// `aterm pkg doctor` names the tag), or refuses under `ATPKG_REFUSE_TRACKED_INSTALL=1`.
/// An untracked process — every pass from an untagged app, every test harness —
/// builds in place exactly as before.
///
/// # Errors
/// [`refresh_view_in_process`]'s, and a refused lane under the refuse policy.
pub fn refresh_view(layout: &Layout, name: &str) -> io::Result<Refreshed> {
    let scratch = views_root(layout);
    layout.ensure_dir(&scratch)?;
    // MEASURED, once here: a probe file written into the views root and read back.
    let tracked = cfg!(target_os = "macos") && crate::provenance::process_is_tracked(&scratch);
    refresh_view_with(
        layout,
        name,
        tracked,
        &crate::lay::lane_for_this_binary(),
        crate::lay::tracked_policy(),
    )
}

/// [`refresh_view`] with the lane's three inputs explicit — whether this process is
/// tracked, which binary would serve the lane, and the policy when it cannot — so
/// every arm is provable from a test that is not itself in a position to be tracked.
pub fn refresh_view_with(
    layout: &Layout,
    name: &str,
    tracked: bool,
    lane: &crate::lay::Lane,
    policy: crate::lay::TrackedPolicy,
) -> io::Result<Refreshed> {
    let helper = match (tracked, lane) {
        (true, crate::lay::Lane::Helper(exe)) => exe,
        // Untracked, or a binary with no lane (a test harness — a fact about the
        // binary, not a failure): in place, as always.
        _ => return refresh_view_in_process(layout, name),
    };
    // A DEV-LINKED VIEW IS STUBS AND LINKS INTO THE CHECKOUT: laying it writes, renames
    // and links nothing in the store. This view lane still serves it while a store
    // build stands — the clone view it takes down is that build's, and the lane's
    // witness is a file of that build — and cannot when none does (no witness); the
    // stubs themselves then go through the shim lane inside `lay_executables`, one
    // untracked job for all of them, exactly as a tracked pass lays every shim.
    let linked = matches!(view_source(layout), ViewSource::Linked(_));
    if linked && std::fs::symlink_metadata(store_current(layout)).is_err() {
        return refresh_view_in_process(layout, name);
    }
    match refresh_view_untracked(helper, layout, name) {
        Ok(refreshed) => Ok(refreshed),
        Err(why) => match policy {
            crate::lay::TrackedPolicy::Refuse => Err(io::Error::other(
                crate::lay::tracked_refusal("refresh the rustup view", &why),
            )),
            crate::lay::TrackedPolicy::Allow if linked => {
                eprintln!(
                    "atpkg: note — this process is provenance-tracked and the untracked lane \
                     could not refresh the rustup view ({why}); the dev-linked view is laid \
                     from this process — links into the checkout, its stubs through the shim \
                     lane, no file of the store written or linked"
                );
                refresh_view_in_process(layout, name)
            }
            crate::lay::TrackedPolicy::Allow => {
                // KEEP THE VIEW THAT IS THERE (audit 2026-09-14): an in-process rebuild
                // lays every file of the view from this tracked process — and so TAGS
                // them, and everything the toolchain then writes — to gain a view one
                // build fresher. A stale view runs the previous compiler until the next pass;
                // a tagged store cannot cut a release until it is re-seeded. Only when
                // there is NO view at all — rustup's `trust` would dangle, the message
                // that reads as a blocked machine — is the in-process build worth its
                // tag, and it says so.
                if let Some(existing) = existing_view(layout, name) {
                    eprintln!(
                        "atpkg: note — this process is provenance-tracked and the untracked lane \
                         could not refresh the rustup view ({why}); the view keeps presenting \
                         {} rather than lay it — and tag it — from this process; \
                         the next pass refreshes it",
                        existing.build.display()
                    );
                    return Ok(existing);
                }
                eprintln!(
                    "atpkg: note — this process is provenance-tracked and the untracked lane \
                     could not refresh the rustup view ({why}); there is no view yet, so it is \
                     built in-process, and every file of the view it clones WILL \
                     carry com.apple.provenance — `aterm pkg doctor` names what that breaks; \
                     `aterm pkg uninstall trust && aterm pkg install trust` re-seeds it clean \
                     once the lane runs"
                );
                refresh_view_in_process(layout, name)
            }
        },
    }
}

/// The view as it stands — what a lane that could not refresh it leaves in place: the
/// build its `bin/` was cloned from (read off the view's `trustc` against the builds in
/// the store), its tool count and the stock names present. `None` when there is
/// no `bin/` with a Trust tool in it.
fn existing_view(layout: &Layout, name: &str) -> Option<Refreshed> {
    let bin = view_dir(layout, name).join("bin");
    let entries: Vec<std::ffi::OsString> = std::fs::read_dir(&bin)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name())
        .collect();
    if !entries.iter().any(|n| n == "trustc") {
        return None;
    }
    let stock: Vec<(&'static str, &'static str)> = STOCK_NAMES
        .iter()
        .copied()
        .filter(|(public, trust)| {
            entries.iter().any(|n| n == public) && entries.iter().any(|n| n == trust)
        })
        .collect();
    let tools = entries.len().saturating_sub(stock.len());
    let build = build_of_view(layout, &bin.join("trustc"))?;
    Some(Refreshed {
        build,
        tools,
        stock,
        changed: false,
    })
}

/// Which `store/trust/<build>` a view's `trustc` was laid from — the build whose own
/// `bin/trustc` it has the length, mode and modification time of: a clone of it, or the
/// hard link a view laid before clones still is. `None` when no build matches (a view
/// from a build gc reclaimed).
#[cfg(unix)]
fn build_of_view(layout: &Layout, view_trustc: &Path) -> Option<PathBuf> {
    let store = layout.prefix.join("store").join("trust");
    std::fs::read_dir(&store)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .find(|p| crate::clone::same_attributes(&p.join("bin").join("trustc"), view_trustc))
}

#[cfg(not(unix))]
fn build_of_view(_layout: &Layout, _view_trustc: &Path) -> Option<PathBuf> {
    None
}

/// Build the view through an untracked launchd job running `helper` on
/// [`HIDDEN_VERB`], then MEASURE: the view's clone of a file of the live build's `bin/`
/// that is clean must be clean too (a tag on it would be the tag the lane exists to
/// prevent). `Err(reason)` is "the
/// lane could not do it"; the caller's policy decides what happens next.
#[cfg(target_os = "macos")]
fn refresh_view_untracked(helper: &Path, layout: &Layout, name: &str) -> Result<Refreshed, String> {
    let current = store_current(layout);
    let build = std::fs::read_link(&current)
        .map(|raw| absolute_target(&raw, &current))
        .map_err(|e| format!("read {}: {e}", current.display()))?;
    let line = run_view_job(
        helper,
        layout,
        &ViewJob::Seam {
            name: name.to_string(),
        },
        &build,
    )?;
    decode_refreshed(&line)
}

/// Run one [`ViewJob`] through the untracked launchd lane and MEASURE: the laid clone of a
/// clean file of `build`'s `bin/` must not have gained com.apple.provenance (a tag on it
/// would be the tag the lane exists to prevent). Returns the helper's one result line. `Err(reason)` is "the lane could not
/// do it"; the caller's policy decides what happens next.
#[cfg(target_os = "macos")]
pub(crate) fn run_view_job(
    helper: &Path,
    layout: &Layout,
    job: &ViewJob,
    build: &Path,
) -> Result<String, String> {
    if !crate::stage_helper::exe_serves_hidden_verb(helper) {
        return Err(format!(
            "{} is not an atpkg/aterm binary, so it would not serve {HIDDEN_VERB}",
            helper.display()
        ));
    }
    // The witness is chosen BEFORE the job: a store file that is clean, whose clone in
    // the laid tree must be clean after it. A clone copies its source's extended
    // attributes, so a store file that is already tagged (a bundle seeded in-process
    // before the lanes existed) proves nothing either way; when none is clean the
    // measurement is skipped — there is nothing left to protect. A clone that was
    // already tagged before this job (a view an earlier in-process pass laid) is not this
    // job's doing either, so the witness asks only for a tag the job ADDED.
    let dest_bin = match job {
        ViewJob::Seam { name } => view_dir(layout, name),
        ViewJob::Root { .. } => crate::compat::trust_build_of(layout, build).map_or_else(
            || build.to_path_buf(),
            |n| crate::compat::root_dir(layout, n),
        ),
    }
    .join("bin");
    let witness = std::fs::read_dir(build.join("bin"))
        .map_err(|e| format!("list {}: {e}", build.join("bin").display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .find(|p| !crate::provenance::carries_provenance(p))
        .and_then(|p| p.file_name().map(|n| dest_bin.join(n)))
        .map(|clone| {
            let tagged_before = crate::provenance::carries_provenance(&clone);
            (clone, tagged_before)
        });
    let scratch = crate::stage_helper::lanes_scratch().unwrap_or_else(|| views_root(layout));
    layout
        .ensure_dir(&scratch)
        .map_err(|e| format!("create {}: {e}", scratch.display()))?;
    let mut handle = crate::stage_helper::Job::prepare(&scratch, "view-helper")?;
    std::fs::write(&handle.spec, encode_spec(&layout.prefix, job))
        .map_err(|e| format!("write spec: {e}"))?;
    handle.submit(helper, HIDDEN_VERB)?;
    let outcome = (|| -> Result<Result<String, String>, String> {
        handle.wait_for_result()?;
        handle.read_result()
    })();
    // The label removed and a still-running helper stopped BEFORE the view is
    // inspected or the in-process fallback touches it.
    drop(handle);
    let line = match outcome {
        Ok(Ok(line)) => line,
        Ok(Err(refusal)) => return Err(format!("the untracked helper refused: {refusal}")),
        Err(why) => return Err(why),
    };
    if let Some((witness, tagged_before)) = witness
        && !tagged_before
        && crate::provenance::carries_provenance(&witness)
    {
        return Err(format!(
            "the untracked lane laid the clones, but {} now carries com.apple.provenance",
            witness.display()
        ));
    }
    Ok(line)
}

/// See the macOS body — there is no launchd and no tag anywhere else.
#[cfg(not(target_os = "macos"))]
pub(crate) fn run_view_job(
    _helper: &Path,
    _layout: &Layout,
    _job: &ViewJob,
    _build: &Path,
) -> Result<String, String> {
    Err(String::from("the untracked lane exists on macOS only"))
}

/// See the macOS body — there is no launchd and no tag anywhere else.
#[cfg(not(target_os = "macos"))]
fn refresh_view_untracked(
    _helper: &Path,
    _layout: &Layout,
    _name: &str,
) -> Result<Refreshed, String> {
    Err(String::from("the untracked lane exists on macOS only"))
}

/// Build, or rebuild, the view for `name` from `store/trust/current` — or, when the view
/// already IS that build ([`view_matches`] at [`Depth::Deep`]), do nothing at all.
///
/// # Why a matching view is left untouched
///
/// This used to rebuild on every call: hard-link every tool into `.bin.tmp-<pid>`,
/// compare, delete the temp when nothing changed, and re-mirror and swap each of `lib/`,
/// `libexec/`, `share/` and `etc/` regardless. Each link and unlink moved the `st_nlink`
/// and `st_ctime` of a live store inode, and tippy's identity guard snapshots exactly
/// those for its own executable and its siblings (trust `feb929a7ff~1`,
/// `path_identity.rs`): a link+unlink of `tippy-driver` during a tippy run aborted it
/// with "changed identity, length, or contents", measured 2026-09-15. Clones no longer
/// touch the store at all, but a tippy running FROM the view pins the view's own files
/// the same way, so a rebuild still aborts it. A matching view therefore costs only
/// `lstat`s, and even the view's own directories are not re-created or chmodded.
///
/// The two [`Layout::ensure_dir`] calls still run FIRST, ahead of that check, as they did
/// before it existed: they are where the fail-closed rules live — a symlink at
/// `<prefix>/rustup` or at the view's name is refused, and the directory must be ours and
/// not group- or other-writable — so a matching tree behind a planted link is refused
/// exactly as a mismatched one is (a reviewer measured the early return accepting it,
/// 2026-09-15). They cost the no-churn rule nothing: `ensure_private_dir` chmods only a
/// directory whose mode is not already `0700`, because a `chmod(2)` to the SAME mode still
/// moves the directory's `st_ctime` on APFS (measured 2026-09-15).
///
/// # A rebuild
///
/// Only the parts that stopped matching are laid again, each beside its live copy and
/// swapped in by `rename(2)`: each mirrored directory that differs at [`Depth::Deep`] (a
/// `.DS_Store` Finder left under `share/` re-mirrors `share/` alone), then `bin/`
/// ([`lay_bin`]) only when [`bin_mismatch`] or the stock-name byte check says it differs
/// — so a rebuild over a directory beside `bin/` never replaces a `bin/` file a running
/// tippy pins, and a mirror that fails returns before `bin/` is touched. A build whose `bin/` holds a symlink does not get that entry:
/// every Trust frontend refuses a symlinked sibling, so the view never presents one.
///
/// # Errors
/// `store/trust/current` is not a link atpkg can read, `<prefix>/rustup` or the view is
/// not a directory atpkg may write, the build's `bin/` cannot be listed, or a clone
/// cannot be made — the last is the store on another volume, and it refuses rather than
/// byte-copying the toolchain.
pub fn refresh_view_in_process(layout: &Layout, name: &str) -> io::Result<Refreshed> {
    // FIRST, before every early return below: the debris a KILLED rebuild left under
    // another pid. It is not a mismatch (`first_mismatch` ignores it on purpose), so a
    // view that already matches — the common case, and the one that returns two lines
    // down — is exactly where an abandoned sysroot's clones would otherwise sit forever.
    sweep_view_debris(&view_dir(layout, name));
    #[cfg(unix)]
    if let ViewSource::Linked(checkout) = view_source(layout) {
        return refresh_linked_view(layout, name, &checkout);
    }
    let current = store_current(layout);
    let build = std::fs::read_link(&current).map(|raw| absolute_target(&raw, &current))?;
    let src_bin = build.join("bin");
    let view = view_dir(layout, name);
    layout.ensure_dir(&views_root(layout))?;
    layout.ensure_dir(&view)?;
    if view_matches(&build, &view, Depth::Deep) {
        let plan = bin_plan(&src_bin)?;
        return Ok(Refreshed {
            build,
            tools: plan.tools,
            stock: plan.stock,
            changed: false,
        });
    }
    let pid = crate::dec_u64(u64::from(std::process::id()));
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
        // A directory SYMLINK from the form that shipped before mirroring is a link,
        // not a tree: `rename` moves it as a link and `remove_link` drops it.
        if std::fs::symlink_metadata(&at).is_ok() {
            std::fs::rename(&at, &old)?;
        }
        std::fs::rename(&staged, &at)?;
        let _ = std::fs::remove_dir_all(&old);
        crate::platform::remove_link(&old);
    }
    let live = view.join("bin");
    let stock_differed = stock_bytes_mismatch(&build, &view).is_some();
    if bin_mismatch(&build, &view).is_none() && !stock_differed {
        let plan = bin_plan(&src_bin)?;
        return Ok(Refreshed {
            build,
            tools: plan.tools,
            stock: plan.stock,
            changed: false,
        });
    }
    let staged = view.join(format!(".bin.tmp-{pid}"));
    let _ = std::fs::remove_dir_all(&staged);
    let (tools, stock) = lay_bin(layout, &src_bin, &staged)?;
    // A live `bin/` that did not match is ALWAYS replaced — even one presenting the same
    // name -> file map (the hard links a view held before clones, which `bin_mismatch`
    // rejects precisely so they are retired). `changed` says only whether what the view
    // presents is different, so a migration that swaps like for like stays silent — and
    // a stock name whose BYTES were not its Trust tool's is a change, attributes or not.
    let changed = stock_differed || !same_bin(&live, &staged);
    let old = view.join(format!(".bin.old-{pid}"));
    let _ = std::fs::remove_dir_all(&old);
    if std::fs::symlink_metadata(&live).is_ok() {
        std::fs::rename(&live, &old)?;
    }
    std::fs::rename(&staged, &live)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(Refreshed {
        build,
        tools,
        stock,
        changed,
    })
}

/// Whether `name` is the dot-named scratch a view rebuild makes for itself:
/// `.<stem>.tmp-<pid>` or `.<stem>.old-<pid>`, where `<stem>` is `bin` or one of
/// [`VIEW_DIRS`] and `<pid>` a non-empty run of ASCII digits — the PRODUCER's exact
/// shape, the one [`refresh_view_in_process`] and [`take_down`] render.
///
/// As narrow as [`crate::store::stage_scratch_of`], and for its reason: this authorizes
/// an unguarded `remove_dir_all` inside a directory a user can also put things in
/// (`~/.rustup/toolchains/<name>/`), so "looks like something we made" is not a good
/// enough test — `.lib.old-notes/` is not ours to delete.
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

/// Remove the staging debris a KILLED rebuild left in `view` — every [`is_view_debris`]
/// entry, whatever shape it is (a mirrored tree, a directory link, a file).
///
/// WHY IT IS NEEDED. A rebuild stages into `.<stem>.tmp-<pid>` and retires what stood at
/// the name through `.<stem>.old-<pid>`, removing both as it goes — but only its OWN
/// pid's, and only on the path it completes. A pass killed between those renames (a `^C`,
/// the window's apply deadline, a power loss) parks a whole superseded sysroot under a
/// name no later pass ever looks at: [`first_mismatch`] ignores top-level entries beside
/// `bin/` deliberately (counting debris would rebuild the view forever), so the tree is
/// not a mismatch, the view reads as current, and the clones it holds keep a superseded
/// build's blocks allocated for as long as the prefix lives — one more per crash.
///
/// WHY IT IS SAFE HERE. A view is laid under the store-wide writer lock, as every verb
/// that lays or discards a build is ([`crate::lock::try_lock_store`]; the untracked lane's
/// helper runs inside its parent's hold), so debris found here is owned by a process that
/// no longer exists — the argument [`crate::compat::sweep`] makes for the identical shape
/// under `compat/trust`. Best-effort throughout: what will not go is left for the next
/// pass, exactly as a failed in-pass removal already is.
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
    /// Trust is dev-linked but the checkout is not one atpkg can present — not a sysroot
    /// (a cargo project's `target/release`, a tree that lost its `lib/`), or a tree
    /// inside atpkg's own prefix (the view itself, say — stubs into which would exec
    /// themselves forever, and laying them would take the store's view down). [`attach`]
    /// refuses it, naming the path.
    LinkedNoSysroot(PathBuf),
}

/// Which source the view presents NOW — the link marker outranks the store, as it does
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

/// Whether the view at `view` already presents `checkout` as [`refresh_linked_view`]
/// lays it: `bin/` a real directory holding exactly the stubs [`linked_stubs`] would lay
/// — byte for byte, so a hand-edited body or one an older atpkg rendered is re-laid,
/// not kept forever on the strength of its `exec` line — and each of [`VIEW_DIRS`] a
/// directory link to the checkout's (or absent when the checkout has none). A link
/// planted at `bin/` is not a match, whatever stands behind it — the store path's rule.
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
/// [`crate::lay::Executable`] per entry, the `bin/` shim body that execs the checkout's
/// file ([`crate::platform::shim_executable_to_env`], with no exported environment — a
/// dev checkout's target never routes through an exec root, so no guard line is
/// rendered).
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
/// store's clone view taken down this way removes only the view's own files (a view laid
/// as hard links before clones would unlink the store inodes' extra names: their link
/// counts drop and their ctimes move, their bytes never do).
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

/// Lay the view for a dev-linked `checkout` (see the module doc): the mirrored
/// directories as links to the checkout's, `bin/` staged beside the live one as one
/// exec stub per plan entry — laid in ONE [`crate::lay::lay_executables`] call, so a
/// provenance-tracked process uses one untracked job for all of them, as every
/// shim-laying pass does — and swapped in by `rename(2)` when it differs. Writes,
/// renames and links no file of the store: a store view standing there is taken down
/// ([`take_down`]), never followed.
#[cfg(unix)]
fn refresh_linked_view(layout: &Layout, name: &str, checkout: &Path) -> io::Result<Refreshed> {
    let view = view_dir(layout, name);
    layout.ensure_dir(&views_root(layout))?;
    layout.ensure_dir(&view)?;
    let src_bin = checkout.join("bin");
    let plan = bin_plan(&src_bin)?;
    if linked_view_matches(checkout, &view, &plan) {
        return Ok(Refreshed {
            build: checkout.to_path_buf(),
            tools: plan.tools,
            stock: plan.stock,
            changed: false,
        });
    }
    let pid = crate::dec_u64(u64::from(std::process::id()));
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
        return Ok(Refreshed {
            build: checkout.to_path_buf(),
            tools: plan.tools,
            stock: plan.stock,
            changed,
        });
    }
    let staged = view.join(format!(".bin.tmp-{pid}"));
    let _ = std::fs::remove_dir_all(&staged);
    crate::platform::remove_link(&staged);
    layout.ensure_dir(&staged)?;
    crate::lay::lay_executables(&linked_stubs(&staged, &plan)?)?;
    {
        take_down(&view, &live, "bin", &pid)?;
        std::fs::rename(&staged, &live)?;
        changed = true;
    }
    Ok(Refreshed {
        build: checkout.to_path_buf(),
        tools: plan.tools,
        stock: plan.stock,
        changed,
    })
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
    /// A symlink (or Windows junction) with this RAW target, as written.
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

/// The bare noun for what sits at a seam path, with no layout to compare against — the
/// half of [`Entry::describe`] a caller that already names the path needs.
fn entry_noun(entry: &Entry) -> &'static str {
    match entry {
        Entry::Absent => "absent",
        Entry::Link(_) => "a symlink",
        Entry::Dir => "a real directory",
        Entry::File => "a regular file",
        Entry::Other => "neither a symlink nor a directory",
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

/// Whether an absolute target IS the view for `name` — lexically only, on purpose: a
/// link into the store (`current`, or a numbered build) is atpkg's and gets RE-POINTED
/// here, so it must not read as already right.
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

/// What an [`attach`] did.
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
    /// The entry already linked to the view; left byte-for-byte alone. `view_build` is
    /// the build the view now presents WHEN this attach rebuilt it (2026-09-15: the view
    /// moving to a new build under an unchanged entry was reported as an unchanged
    /// adoption, so an update pass said nothing about the compiler it had just switched
    /// rustup to), `None` when the view was already that build.
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
}

impl Attached {
    /// Whether the seam is now recorded and live (everything but no-rustup).
    #[must_use]
    pub fn is_live(&self) -> bool {
        !matches!(self, Attached::NoRustup { .. })
    }

    /// Whether this attach CHANGED the filesystem (what a silent pass reports).
    #[must_use]
    pub fn changed(&self) -> bool {
        matches!(
            self,
            Attached::Created { .. }
                | Attached::Repointed { .. }
                | Attached::Adopted {
                    view_build: Some(_),
                    ..
                }
        )
    }
}

impl fmt::Display for Attached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Attached::NoRustup { key, toolchains } => write!(
                f,
                "{key}: no rustup ({} absent) — nothing to attach",
                toolchains.display()
            ),
            Attached::Created { key, path, target } => write!(
                f,
                "{key}: attached {} -> {}",
                path.display(),
                target.display()
            ),
            Attached::Adopted {
                key,
                path,
                target,
                view_build: None,
            } => write!(
                f,
                "{key}: adopted {} (already -> {})",
                path.display(),
                target.display()
            ),
            Attached::Adopted {
                key,
                path,
                target,
                view_build: Some(build),
            } => write!(
                f,
                "{key}: {} -> {} now presents {}",
                path.display(),
                target.display(),
                build.display()
            ),
            Attached::Repointed {
                key,
                path,
                from,
                to,
            } => write!(
                f,
                "{key}: re-pointed {} -> {} (was {}, inside the store)",
                path.display(),
                to.display(),
                from.display()
            ),
        }
    }
}

/// Why an [`attach`] or [`detach`] did not happen. Every variant is fail-closed:
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
                "trust is not installed ({} absent) — nothing to point the seam at",
                target.display()
            ),
            Refusal::LinkedNoSysroot { checkout } => write!(
                f,
                "trust is dev-linked to {}, which is not a sysroot atpkg can present (no bin/ \
                 and lib/, or a tree inside atpkg's own prefix) — the seam is left as it \
                 stands; `aterm pkg unlink trust` returns it to the installed build",
                checkout.display()
            ),
            Refusal::Foreign { path, what } => write!(
                f,
                "{} is {what} — not aterm's seam; refusing to touch it (fix: {DETACH_FIX})",
                path.display()
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
pub fn attach(layout: &Layout, rustup_home: &Path, name: &str) -> Result<Attached, Refusal> {
    if !name_allowed(name) {
        return Err(Refusal::BadName(name.to_string()));
    }
    let key = record_key(name);
    let toolchains = toolchains_dir(rustup_home);
    if !toolchains.is_dir() {
        return Ok(Attached::NoRustup { key, toolchains });
    }
    let target = seam_target(layout, name);
    let p = probe(layout, rustup_home, name)?;
    // Foreign entries are refused BEFORE the view is touched, so a refusal changes
    // nothing on disk, as the contract says.
    if let Entry::Link(raw) = &p.entry
        && !p.in_prefix
    {
        return Err(Refusal::Foreign {
            what: Entry::Link(raw.clone()).describe(layout),
            path: p.path,
        });
    }
    if !matches!(p.entry, Entry::Absent | Entry::Link(_)) {
        return Err(Refusal::Foreign {
            what: p.entry.describe(layout),
            path: p.path,
        });
    }
    // THE LINK OUTRANKS THE STORE: a dev-linked trust is presented whether or not a
    // store build stands; one whose checkout rustup could not present is refused
    // before the view is touched; otherwise the store's build must be there.
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
    let refreshed = refresh_view(layout, name)?;
    match p.entry {
        Entry::Absent => {
            crate::activate::atomic_symlink(&target, &p.path)?;
            record(layout, name)?;
            Ok(Attached::Created {
                key,
                path: p.path,
                target,
            })
        }
        Entry::Link(raw) => {
            if p.targets_view {
                record(layout, name)?;
                return Ok(Attached::Adopted {
                    key,
                    path: p.path,
                    target,
                    view_build: refreshed.changed.then_some(refreshed.build),
                });
            }
            // A link into the store — `current`, or a numbered build — from before the
            // view existed: `rename(2)` a fresh link over it. The existing link is
            // replaced, never followed.
            crate::activate::atomic_symlink(&target, &p.path)?;
            record(layout, name)?;
            Ok(Attached::Repointed {
                key,
                path: p.path,
                from: raw,
                to: target,
            })
        }
        // Every other shape was refused above.
        other => Err(Refusal::Foreign {
            what: other.describe(layout),
            path: p.path,
        }),
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
    /// `--force`: a link ELSEWHERE was unlinked (its target is named, so it is
    /// recoverable by hand).
    ForcedUnlinked {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// `--force`: a real directory / file was MOVED aside, never deleted.
    ForcedDisplaced {
        key: String,
        path: PathBuf,
        moved_to: PathBuf,
    },
}

impl fmt::Display for Detached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Detached::NoRustup { key, toolchains } => write!(
                f,
                "{key}: no rustup ({} absent) — record cleared",
                toolchains.display()
            ),
            Detached::AlreadyAbsent { key, path } => {
                write!(
                    f,
                    "{key}: {} already absent — record cleared",
                    path.display()
                )
            }
            Detached::Removed { key, path, target } => write!(
                f,
                "{key}: detached {} (was -> {})",
                path.display(),
                target.display()
            ),
            Detached::ForcedUnlinked { key, path, target } => write!(
                f,
                "{key}: --force unlinked {} (was -> {}, not aterm's)",
                path.display(),
                target.display()
            ),
            Detached::ForcedDisplaced {
                key,
                path,
                moved_to,
            } => write!(
                f,
                "{key}: --force moved {} aside to {} (not deleted)",
                path.display(),
                moved_to.display()
            ),
        }
    }
}

fn displaced_name(path: &Path, name: &str) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut file = String::from(name);
    file.push_str(".displaced-by-aterm-");
    file.push_str(&crate::dec_u64(secs));
    file.push('-');
    file.push_str(&crate::dec_u64(u64::from(std::process::id())));
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
            // The view is atpkg's own, laid for this seam alone; it goes with the entry.
            // The store it mirrored loses the view's extra names for its files and
            // nothing else; `remove_dir_all` unlinks a directory symlink (a dev-linked
            // view's `lib/`) rather than descend into it.
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

/// The recorded rustup seam NAMES that are on the allowlist — what a re-assertion or
/// a whole-set detach walks. A key that is not `rustup:<allowed>` is ignored, never
/// acted on.
#[must_use]
pub fn recorded_names(layout: &Layout) -> Vec<String> {
    recorded_keys(layout)
        .iter()
        .filter_map(|k| k.strip_prefix(RECORD_PREFIX))
        .filter(|n| name_allowed(n))
        .map(str::to_string)
        .collect()
}

/// Add `rustup:<name>` to `status.toml`'s `seams` (load, modify, save). Idempotent.
///
/// Seeded through [`crate::status::seed_for_rewrite`]: the save rewrites the WHOLE
/// record, so an existing one this process cannot read is an error to report, never a
/// file to replace — replacing it would drop the very `seams` list a later `uninstall
/// --all` walks to detach the rustup toolchain.
fn record(layout: &Layout, name: &str) -> io::Result<()> {
    let key = record_key(name);
    let mut s = crate::status::seed_for_rewrite(layout)?;
    if s.seams.contains(&key) {
        return Ok(());
    }
    s.seams.push(key);
    s.seams.sort();
    s.seams.dedup();
    crate::status::write(layout, &s)
}

/// Drop `rustup:<name>` from `status.toml`'s `seams` (no record ⇒ nothing to do), and
/// the `refused:rustup:<name>: <why>` entry standing beside it, if any: the record
/// follows the disk on detach as it does on attach, so a refusal the last pass recorded
/// (a dev-link whose checkout stopped being a sysroot) does not outlive the seam the
/// user then removed (2026-09-16). The refusal is matched through its `: ` separator,
/// so `trust` never takes `trust-dev`'s.
fn unrecord(layout: &Layout, name: &str) -> io::Result<()> {
    let key = record_key(name);
    let refused = refusal_prefix(name);
    let Some(mut s) = crate::status::read(layout) else {
        return Ok(());
    };
    let before = s.seams.len();
    s.seams.retain(|k| *k != key && !k.starts_with(&refused));
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
#[must_use]
pub fn refusals(layout: &Layout) -> Vec<(String, String)> {
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

/// Re-assert every recorded seam, plus a first attach of [`DEFAULT_SEAM`] when rustup
/// is present, the entry is absent and trust is installed. Best-effort and quiet:
/// the returned lines name only what CHANGED (created, re-pointed) and what was
/// refused — an adopted, already-correct seam says nothing, so the 6-hour pass does
/// not narrate a no-op.
#[must_use]
pub fn reassert(layout: &Layout, rustup_home: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    if !toolchains_dir(rustup_home).is_dir() {
        return lines;
    }
    let mut names: std::collections::BTreeSet<String> =
        recorded_names(layout).into_iter().collect();
    // The default seam joins whenever trust is INSTALLED — not only when its entry is
    // absent. `attach` is fail-closed on every shape the entry can take (absent ⇒
    // created, a link into the store ⇒ adopted or re-pointed, anything foreign ⇒
    // refused, nothing touched), so the only thing widening this changes is that a
    // foreign entry is now SAID and RECORDED each pass instead of silently skipped:
    // m21 ran seven weeks with `~/.rustup/toolchains/trust` pointing at a dev stage2,
    // `seams = []`, and every pass walking an empty name set (2026-09-10 audit).
    // …and when trust is DEV-LINKED to a sysroot with no store build at all: the link
    // is a decision to present that compiler, and rustup's `trust` is where a repo
    // pinning the channel looks for it.
    if !names.contains(DEFAULT_SEAM)
        && (std::fs::symlink_metadata(store_current(layout)).is_ok()
            || matches!(view_source(layout), ViewSource::Linked(_)))
    {
        names.insert(DEFAULT_SEAM.to_string());
    }
    for name in names {
        match attach(layout, rustup_home, &name) {
            Ok(a) => {
                // A seam that attaches (or was already right) clears the refusal the
                // last pass may have recorded — the record follows the disk.
                let _ = clear_refusal(layout, &name);
                if a.changed() {
                    lines.push(a.to_string());
                }
            }
            Err(e) => {
                let why = e.to_string();
                // A refusal is RECORDED, not only returned: the pass verbs print these
                // lines into a stdout the GUI discards. `Io` is a filesystem hiccup, not
                // a posture — recording it would make one transient EIO a standing
                // fault in Settings.
                if !matches!(e, Refusal::Io(_)) {
                    let _ = record_refusal(layout, &name, &why);
                }
                lines.push(why);
            }
        }
    }
    lines
}

/// WHAT THIS MACHINE ACTUALLY COMPILES WITH, when that is not the build atpkg manages —
/// one clause naming the dissent, or `None` when rustup's entry IS the managed seam, is
/// absent, or there is no rustup at all.
///
/// **THE FAILURE THIS ANSWERS.** `aterm pkg update trust` printed `atpkg: rustc up to date`
/// for two weeks while every `targo`, every `cargo +trust` and every repo pinning
/// `channel = "trust"` ran a hand-placed toolchain in the home directory that was 859
/// commits behind — measured on m3, 2026-09-17: pin 8595, `$HOME/trust` HEAD 9454. Both halves
/// of that sentence were true of what atpkg MANAGES and neither was true of what the
/// machine USES, and nothing in the update lane had ever been given the second question to
/// ask. `doctor` knew — [`crate::doctor`]'s seam check had reported the same entry, twice —
/// but a verdict a person only sees when they run a different verb is not a verdict the
/// verb they ran gave them.
///
/// **THE FACT ONLY, NEVER THE REMEDY.** The one-line `ln -sfn` re-point (and the
/// [`DETACH_FIX`] for the shapes a link cannot be laid over) is spelled in exactly one
/// place, `doctor`'s `seam_line`, and this clause points there rather than growing a second
/// copy that can drift from it. What belongs here is the thing the update lane alone is in
/// a position to say: the verdict it just printed is about a build this machine does not
/// run.
///
/// Pure over the probe, so the words are testable without a rustup — the same rule
/// `seam_line` follows.
#[must_use]
pub fn dissent_line(st: &SeamStatus) -> Option<String> {
    if !st.rustup_present {
        return None;
    }
    let names_what = |what: String| {
        // Manual concat (no `format!`): Trust-gate lowering workaround — see `lib.rs::dec_u64`.
        let mut s = String::from("rustup's `");
        s.push_str(DEFAULT_SEAM);
        s.push_str("` resolves to ");
        s.push_str(&what);
        s.push_str(", which atpkg does not manage — `cargo +");
        s.push_str(DEFAULT_SEAM);
        s.push_str("`, `rustup run ");
        s.push_str(DEFAULT_SEAM);
        s.push_str("` and every repo pinning `channel = \"");
        s.push_str(DEFAULT_SEAM);
        s.push_str(
            "\"` compile with THAT copy; `aterm pkg doctor` names the one-line \
                    re-point",
        );
        s
    };
    match &st.entry {
        // A link somewhere else entirely: the hand-placed toolchain, the case this exists for.
        Ok(Entry::Link(raw)) if !st.in_prefix => Some(names_what(raw.display().to_string())),
        // A real directory, a regular file, something that is not a symlink at all: whatever
        // it holds is what `+trust` runs, and no re-point can be laid over it.
        Ok(entry @ (Entry::Dir | Entry::File | Entry::Other)) => {
            let mut what = st.path.display().to_string();
            what.push_str(" (");
            what.push_str(entry_noun(entry));
            what.push(')');
            Some(names_what(what))
        }
        // A link INTO the prefix but not at the view is atpkg's own older layout: `repair`
        // re-points it and the tools it reaches are this build's either way. Not a dissent.
        Ok(Entry::Link(_) | Entry::Absent) => None,
        // Nobody looked. A bound on what this process may know is never reported as a fact
        // about the machine — but it is not silence either: say that the question could not
        // be answered, so a reader is not left with a bare "up to date" that was never
        // checked.
        Err(e) => {
            let mut s = String::from("rustup's `");
            s.push_str(DEFAULT_SEAM);
            s.push_str("` at ");
            s.push_str(&st.path.display().to_string());
            s.push_str(" could not be inspected (");
            s.push_str(e);
            s.push_str("), so whether this machine compiles with the build above is UNKNOWN");
            Some(s)
        }
    }
}

/// [`dissent_line`] for `program`, against the rustup home the REAL process edge armed —
/// `None` for any program but [`SEAM_PROGRAM`], and `None` in every unit test by
/// construction ([`arm_from_env`]), so no test can be made to depend on the developer's own
/// `~/.rustup`.
///
/// This is the accessor the update and install lanes call. Their tests reach
/// [`dissent_line`] directly, which is where the words are.
#[must_use]
pub fn dissent_if_armed(layout: &Layout, program: &str) -> Option<String> {
    if program != SEAM_PROGRAM {
        return None;
    }
    let home = armed()?;
    dissent_line(&status(layout, home, DEFAULT_SEAM))
}

/// Detach every recorded seam (never `--force`): the whole-set removal's companion.
/// Returns one line per seam acted on or refused.
#[must_use]
pub fn detach_recorded(layout: &Layout, rustup_home: &Path) -> Vec<String> {
    recorded_names(layout)
        .iter()
        .map(|name| match detach(layout, rustup_home, name, false) {
            Ok(d) => d.to_string(),
            Err(e) => e.to_string(),
        })
        .collect()
}

/// The rustup home the REAL process edge armed, if any. `None` inside the crate's
/// own test harness by construction — see [`arm_from_env`].
static ARMED: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Arm the in-flow re-assertions with this process's [`rustup_home`]. Called ONCE at
/// the CLI dispatch edge for store-mutating verbs. A deliberate no-op under
/// `cfg(test)`: flow.rs's tests activate a program named `trust` inside temp
/// layouts, and an armed hook there would lay a link in the developer's live
/// `~/.rustup/toolchains` pointing into a temp dir.
pub fn arm_from_env() {
    if cfg!(test) {
        return;
    }
    let _ = ARMED.set(rustup_home());
}

/// Whether [`arm_from_env`] armed a rustup home.
#[must_use]
pub fn armed() -> Option<&'static Path> {
    ARMED.get().and_then(|h| h.as_deref())
}

/// [`reassert`] against the armed rustup home; nothing when unarmed. The hook flow.rs
/// calls after an activation or rollback of `trust`. Silent: the lines are returned
/// for a caller that wants to print them, and flow.rs discards them (it is hermetic).
#[must_use]
pub fn reassert_if_armed(layout: &Layout) -> Vec<String> {
    match armed() {
        Some(h) => reassert(layout, h),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// A synthetic HOME-shaped tree: `prefix/` (the store) beside `rustup/`
    /// (a fake `RUSTUP_HOME`). Nothing here reads `$HOME` or `$RUSTUP_HOME`.
    struct Fixture {
        root: PathBuf,
        layout: Layout,
        rustup: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("atpkg-seam-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let prefix = root.join("prefix");
            std::fs::create_dir_all(&prefix).unwrap();
            let rustup = root.join("rustup");
            std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
            Fixture {
                root,
                layout: Layout { prefix },
                rustup,
            }
        }

        /// Lay `store/trust/<build>/` — the Trust-named tools in `bin/`, a `lib/` — and
        /// `store/trust/current -> <build>`. A build ships NO stock names: that is the
        /// distribution's shape since 2026-09-14, and the view's job.
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

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    fn link(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    /// `(dev, ino)` of a path, following nothing.
    #[cfg(unix)]
    /// Whether the view file `at` is a clone of the store file `store` holding its bytes —
    /// and not the store's own inode, which is what a hard link would be.
    fn cloned(store: &Path, at: &Path) -> bool {
        crate::clone::is_clone_of(store, at)
            && matches!(crate::clone::same_bytes(store, at), Ok(true))
    }

    /// The view spec round-trips byte for byte and refuses what it half-understands —
    /// a foreign header, an unknown key, a name off the allowlist, a relative prefix.
    #[test]
    fn the_view_spec_round_trips_and_refuses_what_it_half_understands() {
        let prefix = Path::new("/Users//x/Library/Application Support/aterm/pkg");
        let seam = ViewJob::Seam {
            name: String::from("trust"),
        };
        let text = encode_spec(prefix, &seam);
        assert!(text.starts_with(SPEC_HEADER), "{text}");
        assert_eq!(
            decode_spec(&text).unwrap(),
            (prefix.to_path_buf(), seam.clone())
        );
        let root = ViewJob::Root {
            build_dir: prefix.join("store/trust/8595"),
        };
        assert_eq!(
            decode_spec(&encode_spec(prefix, &root)).unwrap(),
            (prefix.to_path_buf(), root)
        );
        assert!(decode_spec("atpkg-view-spec v0\n").is_err());
        assert!(decode_spec(&text.replace("name=", "nom=")).is_err());
        assert!(
            decode_spec(&encode_spec(
                prefix,
                &ViewJob::Seam {
                    name: String::from("nightly")
                }
            ))
            .is_err(),
            "a name off the allowlist"
        );
        assert!(decode_spec(&encode_spec(Path::new("relative/prefix"), &seam)).is_err());
        assert!(
            decode_spec(&encode_spec(
                prefix,
                &ViewJob::Root {
                    build_dir: PathBuf::from("store/trust/8595")
                }
            ))
            .is_err(),
            "a relative root"
        );
    }

    /// The helper's one result line round-trips, its stock pairs resolved back to the
    /// allowlist's own statics; a pair the allowlist does not name is refused.
    #[test]
    fn the_view_result_round_trips_through_the_allowlist() {
        let r = Refreshed {
            build: PathBuf::from("/p/store/trust/8595"),
            tools: 21,
            stock: STOCK_NAMES.to_vec(),
            changed: true,
        };
        assert_eq!(decode_refreshed(&encode_refreshed(&r)).unwrap(), r);
        let none = Refreshed {
            build: PathBuf::from("/p/store/trust/8595"),
            tools: 0,
            stock: Vec::new(),
            changed: false,
        };
        assert_eq!(decode_refreshed(&encode_refreshed(&none)).unwrap(), none);
        assert!(decode_refreshed("1 21 2f70 rustc:nope").is_err());
        assert!(decode_refreshed("2 21 2f70 ").is_err());
    }

    /// The policy table for the view, with the lane's outcome forced: an untracked
    /// process builds in place whatever the lane; a tracked one with no lane for its
    /// binary builds in place; a tracked one whose lane FAILS refuses under `Refuse`
    /// and builds in-process under `Allow`, the default.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tracked_process_whose_view_lane_fails_refuses_under_refuse_and_builds_under_allow() {
        use crate::lay::{Lane, TrackedPolicy};
        let f = Fixture::new("view-policy");
        f.install_trust(8595);
        // A helper spelled right that answers nothing: `/usr/bin/true` named `atpkg`.
        let fake_dir = f.layout.prefix.join("fake");
        std::fs::create_dir_all(&fake_dir).unwrap();
        let fake = fake_dir.join("atpkg");
        std::fs::copy("/usr/bin/true", &fake).unwrap();
        let broken = Lane::Helper(fake);
        let none = Lane::Unavailable(String::from("a test harness"));
        let view_trustc = view_dir(&f.layout, "trust").join("bin").join("trustc");

        refresh_view_with(&f.layout, "trust", false, &broken, TrackedPolicy::Refuse).unwrap();
        assert!(
            view_trustc.is_file(),
            "untracked: in place, whatever the lane"
        );
        refresh_view_with(&f.layout, "trust", true, &none, TrackedPolicy::Refuse).unwrap();
        assert!(view_trustc.is_file(), "tracked, no lane: in place");
        let err = refresh_view_with(&f.layout, "trust", true, &broken, TrackedPolicy::Refuse)
            .expect_err("a tracked process with a broken lane must refuse under Refuse");
        let msg = err.to_string();
        assert!(msg.contains("provenance-tracked"), "{msg}");
        assert!(msg.contains("refresh the rustup view"), "{msg}");
        refresh_view_with(&f.layout, "trust", true, &broken, TrackedPolicy::Allow)
            .expect("the default builds in-process");
        assert!(view_trustc.is_file());
    }

    #[test]
    fn allowlist_and_record_key() {
        assert!(name_allowed("trust"));
        assert!(name_allowed("trust-dev"));
        assert!(!name_allowed("stable"));
        assert!(!name_allowed(""));
        assert!(!name_allowed("../trust"));
        assert_eq!(record_key("trust"), "rustup:trust");
        assert_eq!(DEFAULT_SEAM, "trust");
    }

    #[test]
    fn rustup_home_prefers_env_then_home() {
        let env = std::ffi::OsString::from("/x/rh");
        assert_eq!(
            rustup_home_with(Some(&env), Some(Path::new("/h"))),
            Some(PathBuf::from("/x/rh"))
        );
        // An EMPTY env var is unset, as rustup treats it.
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
        assert!(err.to_string().contains("trust, trust-dev"));
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
        assert!(out.to_string().contains("no rustup"));
        assert!(!fx.rustup.join("toolchains").exists(), "never created");
        assert!(fx.seams_recorded().is_empty(), "nothing recorded");
        // The status line says so too, and reassert stays silent.
        let s = status(&fx.layout, &fx.rustup, "trust");
        assert!(!s.rustup_present);
        assert!(s.to_string().contains("(no rustup:"), "{s}");
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
        assert!(before.to_string().contains("recorded=no"), "{before}");
        assert!(before.to_string().contains("target=(absent)"), "{before}");

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
        let line = after.to_string();
        assert!(
            line.starts_with("rustup:trust: recorded=yes path="),
            "{line}"
        );
        assert!(line.contains("in-prefix=yes targets-view=yes"), "{line}");
        assert!(!line.contains('\n'), "one line");

        // A second attach ADOPTS: byte-identical link, still one record.
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
        assert!(err.to_string().contains("trust is not installed"));
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
        assert!(out.to_string().contains("adopted"));
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

    #[cfg(unix)]
    #[test]
    fn numbered_build_link_is_repointed_to_the_view() {
        let fx = Fixture::new("repoint");
        let build = fx.install_trust(6808);
        link(&build, &fx.seam("trust"));
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        match &out {
            Attached::Repointed { from, to, .. } => {
                assert_eq!(from, &build);
                assert_eq!(to, &seam_target(&fx.layout, "trust"));
            }
            other => panic!("expected Repointed, got {other}"),
        }
        assert!(out.changed());
        assert!(out.to_string().contains("re-pointed"), "{out}");
        assert!(out.to_string().contains("inside the store"), "{out}");
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        // The build tree itself was never followed into or touched.
        assert!(build.join("bin").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn foreign_directory_is_refused_with_the_one_fix() {
        let fx = Fixture::new("foreigndir");
        fx.install_trust(6808);
        std::fs::create_dir_all(fx.seam("trust").join("bin")).unwrap();
        std::fs::write(fx.seam("trust").join("bin").join("rustc"), b"x").unwrap();
        let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
        assert!(matches!(err, Refusal::Foreign { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("a real directory"), "{msg}");
        assert!(msg.contains(DETACH_FIX), "{msg}");
        // Fail-closed: the directory and its contents are untouched, nothing recorded.
        assert!(fx.seam("trust").join("bin").join("rustc").is_file());
        assert!(fx.seams_recorded().is_empty());
        let s = status(&fx.layout, &fx.rustup, "trust");
        assert!(s.to_string().contains("target=(a real directory)"), "{s}");
    }

    // A refused re-assertion is RECORDED in status.toml's `seams` as
    // `refused:rustup:trust: <why>` — the spelling `recorded_names` can never read as a
    // seam — and cleared by the first re-assertion that succeeds. Measured need: m21's
    // `~/.rustup/toolchains/trust` pointed at a dev stage2 for seven weeks while the
    // 6-hourly pass printed the refusal into a discarded stdout and status said
    // `seams = []` (2026-09-10 audit).
    #[cfg(unix)]
    #[test]
    fn a_refused_reassertion_is_recorded_until_one_succeeds() {
        let fx = Fixture::new("refusal-record");
        fx.install_trust(6808);
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust"));
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(DETACH_FIX), "{lines:?}");
        let refused = refusals(&fx.layout);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].0, "trust");
        assert!(refused[0].1.contains("a symlink to"), "{refused:?}");
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

    /// WHAT `update` NOW ASKS THAT IT NEVER USED TO. `aterm pkg update trust` printed
    /// `atpkg: rustc up to date` for two weeks on m3 while `~/.rustup/toolchains/trust`
    /// pointed at a hand-placed toolchain 859 commits behind the pin. Every shape the entry
    /// can take, judged.
    #[cfg(unix)]
    #[test]
    fn dissent_names_a_foreign_entry_and_stays_silent_about_the_managed_one() {
        let fx = Fixture::new("dissent");
        fx.install_trust(8595);

        // ABSENT: nothing to dissent about — the store IS the only answer rustup has.
        assert_eq!(dissent_line(&status(&fx.layout, &fx.rustup, "trust")), None);

        // THE m3 SHAPE: a link to a hand-placed toolchain in the home directory.
        let elsewhere = fx.root.join("toolchains").join("trust-957012d3");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust"));
        let why = dissent_line(&status(&fx.layout, &fx.rustup, "trust"))
            .expect("a foreign link is a dissent");
        assert!(
            why.contains(&elsewhere.display().to_string()),
            "it NAMES what runs instead: {why}"
        );
        assert!(why.contains("cargo +trust"), "{why}");
        assert!(
            why.contains("aterm pkg doctor"),
            "the remedy has ONE spelling and it is doctor's: {why}"
        );

        // A REAL DIRECTORY: no re-point can be laid over it, and it is still a dissent.
        std::fs::remove_file(fx.seam("trust")).unwrap();
        std::fs::create_dir_all(fx.seam("trust")).unwrap();
        let why = dissent_line(&status(&fx.layout, &fx.rustup, "trust"))
            .expect("a real directory is a dissent");
        assert!(why.contains("a real directory"), "{why}");

        // THE MANAGED SEAM ITSELF: silent. A verb that narrates a healthy machine trains
        // its reader to skip the line that matters.
        std::fs::remove_dir_all(fx.seam("trust")).unwrap();
        refresh_view(&fx.layout, "trust").unwrap();
        link(&seam_target(&fx.layout, "trust"), &fx.seam("trust"));
        assert_eq!(dissent_line(&status(&fx.layout, &fx.rustup, "trust")), None);

        // A link INTO the store but not at the view — atpkg's own older layout. `repair`
        // re-points it; the compiler it reaches is this build either way, so not a dissent.
        std::fs::remove_file(fx.seam("trust")).unwrap();
        link(&store_current(&fx.layout), &fx.seam("trust"));
        assert_eq!(dissent_line(&status(&fx.layout, &fx.rustup, "trust")), None);
    }

    /// NO RUSTUP IS NOT A DISSENT, and an entry nobody could inspect is not silence.
    #[cfg(unix)]
    #[test]
    fn dissent_is_silent_without_rustup_and_says_so_when_it_cannot_look() {
        let fx = Fixture::new("dissent-norustup");
        fx.install_trust(8595);
        std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
        assert_eq!(
            dissent_line(&status(&fx.layout, &fx.rustup, "trust")),
            None,
            "a machine with no rustup compiles with what is on PATH; this check has no \
             opinion about it"
        );
        // An inspection that FAILED is reported as unknown, never as "fine": a bound on
        // what this process may know must not be written down as a fact about the machine.
        let st = SeamStatus {
            key: String::from("rustup:trust"),
            recorded: false,
            path: fx.seam("trust"),
            rustup_present: true,
            entry: Err(String::from("Permission denied (os error 13)")),
            target: None,
            in_prefix: false,
            targets_view: false,
        };
        let why = dissent_line(&st).expect("an unreadable entry is said, not skipped");
        assert!(why.contains("UNKNOWN"), "{why}");
        assert!(why.contains("Permission denied"), "{why}");
    }

    /// The armed accessor is inert under test — no unit test can be made to read the
    /// developer's own `~/.rustup` — and it answers only for the seam program.
    #[test]
    fn dissent_if_armed_is_inert_in_tests_and_only_ever_about_trust() {
        let fx = Fixture::new("dissent-armed");
        assert_eq!(dissent_if_armed(&fx.layout, "ay"), None);
        assert_eq!(dissent_if_armed(&fx.layout, SEAM_PROGRAM), None);
    }

    #[cfg(unix)]
    #[test]
    fn foreign_symlink_is_refused_and_left_alone() {
        let fx = Fixture::new("foreignlink");
        fx.install_trust(6808);
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust"));
        let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
        assert!(matches!(err, Refusal::Foreign { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("a symlink to"), "{msg}");
        assert!(msg.contains(DETACH_FIX), "{msg}");
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
        assert!(fx.seams_recorded().is_empty());
        let s = status(&fx.layout, &fx.rustup, "trust");
        assert!(
            s.to_string().contains("in-prefix=no targets-view=no"),
            "{s}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn foreign_regular_file_is_refused() {
        let fx = Fixture::new("foreignfile");
        fx.install_trust(6808);
        std::fs::write(fx.seam("trust"), b"not a toolchain").unwrap();
        let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
        assert!(err.to_string().contains("a regular file"), "{err}");
        assert!(err.to_string().contains(DETACH_FIX));
        assert_eq!(std::fs::read(fx.seam("trust")).unwrap(), b"not a toolchain");
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
        let elsewhere = fx.root.join("elsewhere");
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

        // Foreign directory: refused without --force; MOVED ASIDE (never deleted) with it.
        std::fs::create_dir_all(fx.seam("trust").join("bin")).unwrap();
        std::fs::write(fx.seam("trust").join("bin").join("rustc"), b"x").unwrap();
        let err = detach(&fx.layout, &fx.rustup, "trust", false).unwrap_err();
        assert!(err.to_string().contains("a real directory"), "{err}");
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
                seams: Vec::new(),
                last_success_at: String::new(),
                last_index_reached_at: String::new(),
                last_index_build: 0,
                index_build_changed_at: String::new(),
                programs,
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

    /// The seam recorder REBUILDS `status.toml` from what it read, so an unreadable
    /// record is an error to report, never a file to replace: replacing it would drop the
    /// `seams` list `uninstall --all` walks, stranding the rustup `trust` link the
    /// uninstall was supposed to detach.
    #[test]
    fn recording_a_seam_refuses_an_unreadable_record() {
        let prefix =
            std::env::temp_dir().join(format!("atpkg-seam-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        let layout = Layout { prefix };
        let corrupt = "seams = [\"rustup:trust\"]\nthis is not valid toml {{{\n";
        std::fs::write(layout.status(), corrupt).unwrap();
        assert!(record(&layout, "trust").is_err(), "the recorder declines");
        assert!(
            record_refusal(&layout, "trust", "rustup is absent").is_err(),
            "the refusal recorder declines too"
        );
        assert_eq!(
            std::fs::read_to_string(layout.status()).unwrap(),
            corrupt,
            "the record is left exactly as it was"
        );
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
        assert!(lines[0].contains("attached"), "{lines:?}");
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
        assert!(lines[0].contains(DETACH_FIX), "{lines:?}");
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
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        link(&elsewhere, &fx.seam("trust-dev"));
        let lines = detach_recorded(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("detached"), "{lines:?}");
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

    #[test]
    fn hook_is_unarmed_inside_the_test_harness() {
        // The flow.rs hook must be inert here: flow's own tests activate `trust` in
        // temp layouts, and an armed hook would write into the developer's ~/.rustup.
        arm_from_env();
        assert!(
            armed().is_none(),
            "arm_from_env must be a no-op under cfg(test)"
        );
        let fx = Fixture::new("unarmed");
        fx.install_trust(6808);
        assert!(reassert_if_armed(&fx.layout).is_empty());
        assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
        assert!(fx.seams_recorded().is_empty());
    }

    /// THE VIEW. One clone per tool under its own name, each stock name a clone of its
    /// Trust tool — its bytes on a new inode, never the store's own inode — `lib/` a real
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
        // `lib/` is a mirror, not a directory symlink: the file inside is a clone of the
        // store's, and the directory itself is real, so a tool run from the view
        // resolves its sysroot to the VIEW.
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

    /// A stock-name COPY an older bundle shipped is not what the view presents: the
    /// view's `rustc` is a clone of the store's `trustc`, whatever `bin/rustc` in the
    /// bundle holds.
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

    /// The view follows `current`: after an update the stock names are clones of the NEW
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
        // The view MOVED under an unchanged entry: said, in one line naming the build
        // it now presents (2026-09-15; it used to pass as a silent adoption).
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("now presents") && lines[0].ends_with("6809"),
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

    /// A dev checkout shaped like a sysroot: Trust-named tools in `bin/`, a `lib/`
    /// with a driver, a `share/` — and no stock names, like the distribution.
    #[cfg(unix)]
    fn sysroot_checkout(fx: &Fixture, label: &str) -> PathBuf {
        let dir = fx.root.join(label);
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

    /// THE LINK OUTRANKS THE STORE (2026-09-16). A dev-linked trust is what rustup's
    /// `trust` presents: the view becomes exec stubs into the checkout's `bin/` — each
    /// tool, each stock name to its Trust tool — and links to its directories, said in
    /// one line naming the checkout, under an entry that never moves; a tool the
    /// checkout rebuilds under the same name is what the stub runs a moment later, with
    /// no re-lay; a quiet pass says nothing; and `unlink` returns the store's clone view,
    /// said the same way.
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
            lines[0].contains("now presents") && lines[0].ends_with("stage2"),
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
            lines[0].contains("now presents") && lines[0].ends_with("6808"),
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

    /// A dev-linked trust whose checkout is NOT a sysroot (a cargo project's
    /// `target/release`, a tree that lost its `lib/`) is refused and RECORDED — the
    /// view left presenting the store — and the refusal clears once the link goes.
    #[cfg(unix)]
    #[test]
    fn a_dev_link_to_a_tree_that_is_no_sysroot_is_refused_and_recorded() {
        let fx = Fixture::new("dev-link-nosysroot");
        let store = fx.install_trust(6808);
        attach(&fx.layout, &fx.rustup, "trust").unwrap();
        let bin = view_dir(&fx.layout, "trust").join("bin");
        let proj = fx.root.join("proj");
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
            lines[0].contains("not a sysroot") && lines[0].contains("aterm pkg unlink trust"),
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

    /// Two shapes the linked lay must never accept (2026-09-16 review): a checkout INSIDE
    /// atpkg's own prefix — the view itself, say — whose stubs would exec themselves
    /// forever, and a stub whose body was edited by hand, which a target-only comparison
    /// would keep forever; the first is refused as no sysroot, the second re-laid.
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
            lines.len() == 1 && lines[0].contains("inside atpkg's own prefix"),
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
            lines.len() == 1 && lines[0].contains("now presents"),
            "an edited stub is a changed view: {lines:?}"
        );
        assert_eq!(std::fs::read(&stub).unwrap(), laid, "re-laid byte for byte");
        assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    }

    /// A dev-linked sysroot with NO store build gets the seam too: the link is a
    /// decision to present that compiler, and the seam is created pointing at a view
    /// of the checkout — the one case `attach_refuses_when_trust_is_not_installed`
    /// does not cover.
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
        assert!(lines[0].contains("attached"), "{lines:?}");
        let bin = view_dir(&fx.layout, "trust").join("bin");
        assert_eq!(
            crate::platform::resolve_shim(&bin.join("rustc")).unwrap(),
            checkout.join("bin").join("trustc")
        );
        assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);

        // The checkout stops being a sysroot: the next pass refuses and RECORDS it —
        // and the unlink the refusal names takes the record with the seam, so nothing
        // says "refused" about a seam that is gone (2026-09-16 review).
        std::fs::remove_dir_all(checkout.join("lib")).unwrap();
        let lines = reassert(&fx.layout, &fx.rustup);
        assert!(
            lines.len() == 1 && lines[0].contains("not a sysroot"),
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

    /// The layout from before the view: a seam pointing at `store/trust/current`. It is
    /// atpkg's, so it is re-pointed at the view — and said, since the change is real.
    #[cfg(unix)]
    #[test]
    fn a_legacy_link_to_store_current_is_repointed_to_the_view() {
        let fx = Fixture::new("legacy");
        fx.install_trust(6808);
        link(&store_current(&fx.layout), &fx.seam("trust"));
        let p = probe(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(p.in_prefix && !p.targets_view, "{p:?}");
        let lines = reassert(&fx.layout, &fx.rustup);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("re-pointed") && lines[0].contains("inside the store"),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust")
        );
        assert!(
            view_dir(&fx.layout, "trust")
                .join("bin")
                .join("rustc")
                .is_file()
        );
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

    /// THE NO-CHURN RULE. A second refresh of a view that already matches its build makes
    /// ZERO link or unlink calls: the `st_nlink` and `st_ctime` of every store inode are
    /// what they were. The rebuild-every-call form linked every tool into a temp `bin/`
    /// and re-mirrored `lib/` on each attach, and tippy — which pins exactly those two
    /// fields of its own executable and siblings — aborted mid-run at every re-assertion
    /// (a link+unlink of `tippy-driver` during a run, measured 2026-09-15). The control
    /// at the end proves the stamps see a single link+unlink, so the equality is not
    /// vacuous.
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
        std::fs::hard_link(&probe, fx.root.join("probe-link")).unwrap();
        std::fs::remove_file(fx.root.join("probe-link")).unwrap();
        assert_ne!(inode_stamps(&build), store_before, "the stamps are blind");
    }

    /// `view_matches` is exact about what a view presents: a stock name that is a HARD LINK
    /// to its Trust tool (the construction before clones) fails both depths; one with its
    /// Trust tool's length, mode and time but other bytes (the shape of the bundle's own
    /// separately signed copy, the very file tippy refuses) passes the attribute check and
    /// fails the deep byte check; an extra file deep in `lib/` fails only the deep walk; a
    /// changed symlink target and a stray `bin/` entry fail too. Each is healed by the next
    /// refresh, which then matches.
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

    /// A KILLED REBUILD'S DEBRIS IS THE NEXT PASS'S TO SWEEP. A rebuild stages into
    /// `.<stem>.tmp-<pid>` and retires the standing tree through `.<stem>.old-<pid>`,
    /// removing only its own pid's; a pass killed between the two renames parks a whole
    /// superseded sysroot under a name nothing later looks at — `first_mismatch` ignores
    /// top-level entries beside `bin/` on purpose — so the view reads as current while
    /// the clones keep a reclaimed build's blocks allocated. The next refresh removes
    /// them, and only them: a name the producer cannot render is not ours to delete.
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

    /// A REBUILD RE-LAYS ONLY WHAT DIFFERS. A `.DS_Store` planted under the view's `share/`
    /// (Finder, browsing `~/.rustup/toolchains/trust/share`) is re-mirrored away without a
    /// single link or unlink of a `bin/` inode — neither the store's nor the view's — or of a
    /// store `lib/` inode, and the refresh reports `bin/` unchanged. A mirror that FAILS (an unreadable directory in the
    /// build's `lib/`) fails before `bin/` on every retry, so the retries move no `bin/`
    /// inode either. The control: a stray inside the view's `bin/` does re-lay `bin/`.
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

    /// THE EARLY RETURN KEEPS THE FAIL-CLOSED RULES. A view that matches its build is
    /// accepted only where [`Layout::ensure_dir`] would accept it: behind a symlink planted
    /// at `<prefix>/rustup` the refresh is REFUSED, exactly as a mismatched tree there is,
    /// and a view directory whose mode drifted to `0777` is hardened back to `0700` — with
    /// no store inode moved by either.
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
        let aside = fx.root.join("views-aside");
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
