// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-build EXEC ROOTS for the Trust bundles whose own tippy refuses them.
//!
//! # The defect
//!
//! Trust bundles 8571, 8589, 8590 and 8595 — every build an index pinned from before the
//! Trust-names-only distribution (trust `dist.rs`, 2026-09-14) — ship `bin/rustc` as a
//! separately signed COPY of `bin/trustc`. Their tippy (before trust `e79c1142a5`)
//! accepts a sibling `rustc` only when it is the same file as the selected `trustc` or
//! byte-identical to it, and macOS makes the copy neither: an ad-hoc code signature
//! bakes the file's own name into itself, so the two differ in 2,428 bytes on 8595 (2,430
//! on 8590), every one inside `LC_CODE_SIGNATURE`. So `tippy`, `targo tippy` and
//! `targo-tippy` all stop at "tippy: setup error: rustc-compatible sibling `…/bin/rustc`
//! is not the selected Trust compiler `…/bin/trustc`", on every machine running those
//! builds, while `aterm pkg doctor` said healthy (measured 2026-09-15; `tippy --version`
//! exits 0 on the broken store, so no version probe can see it). The store is
//! content-addressed (`aterm pkg verify` hashes it against the signed `tree_root`), so
//! nothing may be relinked inside a build.
//!
//! # The exec root
//!
//! For such a build atpkg lays `<prefix>/compat/trust/<build>/` by the construction the
//! rustup view uses ([`crate::seam::lay_view`]): `bin/` holds a copy-on-write CLONE
//! ([`crate::clone`]) of each regular file of the build's `bin/`, each stock name
//! (`rustc`, `cargo`, `rustdoc`) a clone of its Trust tool — so `rustc` holds `trustc`'s
//! bytes, which is the one thing that build's tippy asks — and `lib/`, `libexec/`,
//! `share/` and `etc/` cloned the same way, every regular file a clone and every symlink
//! recreated. Nothing in the store is written: not a byte, not a link count, not a
//! ctime. A [`ROOT_MARKER`] is written last, before the root is committed by
//! `rename(2)`, so a root without one is never routed. Run from that tree, `tippy`,
//! `targo tippy`, `targo-tippy`, `tippy-driver` and `$(trustc --print sysroot)/bin/tippy`
//! all lint (measured 2026-09-16 on bundle 8595 itself: its tippy caught a planted
//! `useless_vec` from the clone, and refused the same crate from the store).
//!
//! It used to be a HARD-LINK mirror of the store's own inodes. Every lay, rebuild and
//! retire then moved a live store inode's link count and ctime, which tippy pins, and a
//! link from a tracked process tagged the store — the owner, 2026-09-16: *"doing that with
//! hardlinks sounds like bugs and indeed: bugs"*. A root laid that way has no marker and
//! is not a clone, so it is never routed and the next `repair` rebuilds it.
//!
//! `lib/` is mirrored, not symlinked, for the reason the view's is: a frontend finds its
//! sysroot through the real path of the driver dylib it loaded, so a symlinked `lib/`
//! answered the STORE to `trustc --print sysroot`, and the sysroot spelling of tippy ran
//! beside the store's copy again (exit 1, measured).
//!
//! EVERY tool of an affected build runs from its root, not only the tippy family:
//! `targo tippy` looks for `targo-tippy` only beside `targo` itself (targo's
//! `search_directories`), so `targo` must run from the root, and `trustc` must too for
//! the sysroot spelling. One sysroot for the whole toolchain also costs no rebuilds:
//! alternating the store's and the root's `targo` over one target dir stayed `Fresh`
//! both ways (measured 2026-09-15, with a path dependency and a build script).
//!
//! # How a shim reaches it
//!
//! Through [`route_for_shim`], which the one shim renderer
//! (`platform::shim_executable_to_env`) consults for every shim it writes: when a
//! complete root stands for the build a shim's target lies in, the shim gains the guard
//! line of `platform::sh_shim_content_routed` ahead of its unchanged store `exec`. The
//! guard re-checks at every exec, with `test` builtins only, that the root's file is a
//! regular executable file and not a symlink, that the store file it stands for still
//! exists, and that the root's [`ROOT_MARKER`] stands — so a root half-way through a
//! rebuild, retired, or never finished runs the store path, as today. The BYTES are proved
//! where they can be afforded: when the root is laid, and at [`Depth::Deep`] by `repair`
//! and doctor.
//! Every reader of a shim keeps reading the store target.
//!
//! # Rules
//!
//! * A root is offered only where [`needs_root`] holds — exactly tippy's own refusal
//!   condition, so a Trust-names-only bundle, a pack whose `rustc` is a hard link of
//!   `trustc`, a Linux bundle with byte-identical copies and a bundle without tippy get
//!   NO root and shims byte-identical to today's. The mechanism retires itself with the
//!   builds: once no index pins an affected build, this module, the guard renderer and
//!   doctor's clause for it can be deleted.
//! * Built in a dot-temp beside its name and committed by `rename(2)`; clones only (a
//!   clone that cannot be made — the store on another volume — refuses, and the shims
//!   stay plain; a toolchain is never byte-copied in its place).
//! * A root that already matches its build is NEVER touched ([`ensure_root`] returns
//!   [`Ensured::Present`] after `lstat`s and [`needs_root`]'s byte compare, writing
//!   nothing). tippy pins its executable's and siblings' link count and ctime, and a
//!   tippy running FROM the root pins the root's own files, so rebuilding a root under it
//!   aborts it — the rule [`crate::seam::refresh_view`] follows for the same reason. What
//!   still replaces a root's files is rare and one-time: rebuilding one that no longer
//!   matches (including every root laid as hard links before clones), and removing one
//!   with its build. The store's own files are never touched by any of it.
//! * `lstat` everywhere: a symlink at a root's name is UNLINKED, never followed; nothing
//!   is removed or laid under a `compat` or `compat/trust` that is not a real directory;
//!   and a committed root is renamed aside before it is removed, so no shim can route into
//!   a half-deleted tree.
//! * Only a PROVEN answer removes anything: a build whose `bin/rustc` or `bin/trustc`
//!   cannot be read keeps whatever root stands for it ([`needs_root`]'s errors).
//! * A root lives exactly as long as its build: `store::discard_build` — the one function
//!   every discard path goes through — takes it with the tree ([`discard_root_of`]),
//!   `ops::uninstall` takes `compat/<program>` ([`remove_program`]) and `uninstall --all`
//!   takes `compat/` ([`remove_all`]); gc ends with [`sweep`], which removes a root whose
//!   build is gone or no longer needs one, and dot debris a crashed run left.
//!
//! # Who lays it
//!
//! `activate::install_tools_env` ensures the root (at [`Depth::Shallow`]) before it renders
//! a single shim for a trust build, so every install, flip, repair, seed and restore lays
//! it ahead of the shims that name the build; `flow::rollback_member` — which re-points the
//! shims at the prior build for `atpkg rollback`, a flip that fails and a group abort, and
//! renders them through its own `platform::install_shim_env` — ensures the prior build's
//! root the same way first. And because an up-to-date
//! program is never re-laid, the verbs that must heal a machine already on an affected
//! build run [`reconcile`]: the update pass after its gc, the default-set lane after its
//! alias reconcile, `seed` beside the rustup seam, a trust `rollback`, and `repair` at
//! [`Depth::Deep`] (a root it cannot lay is exit 1). The alias and agents reconciles
//! compare a shim's BYTES with the render, not only its target and environment, so an
//! `alab-` alias laid plain before a root stood is re-laid. `atpkg run` (`aterm <tool>`)
//! and the `clippy` reroute exec a tool without its shim, so they ask `ops::exec_path`,
//! which answers the root's file exactly when [`route_for_shim`] does.
//!
//! # Who reports it
//!
//! `aterm pkg doctor` (its check 5f), through [`inspect`] and [`strays`], which read and
//! never write or execute. On the active trust build, when it [`needs_root`]: a root that
//! matches the build at [`Depth::Deep`] with every trust shim routed is a note and leaves
//! the report healthy; no root, a root that differs, or a shim that does not route is a
//! warn that withholds "healthy", naming the inodes and the one fix, `aterm pkg repair`
//! — except behind a link or a file at `compat` or `compat/trust` ([`Blocked`]), where
//! repair refuses too and the warn names that path and says to remove it first.
//! A root no build needs is a warn naming `aterm pkg gc`. A build that needs no root gets
//! no line at all; one whose `bin/rustc` or `bin/trustc` cannot be read is a warn that
//! withholds "healthy" and names the read that failed.
//!
//! # Not decided here
//!
//! A root laid by a provenance-TRACKED process (the pass the GUI spawns) would be TAGGED:
//! every file such a process creates carries com.apple.provenance, and a toolchain run
//! from tagged files tags what it writes. (The hard-link form tagged the STORE itself —
//! measured 2026-09-15, the 0.86.0 rustup view re-tagged every executable of `trust/8595`
//! on the app's first pass; a clone never writes the store.) So a tracked process lays
//! roots through the view's untracked launchd lane ([`ensure_root_with`],
//! `crate::seam::run_view_job`) and keeps a standing root rather than lay one from this
//! process when that lane cannot run. Not covered: `cargo +trust clippy` (the bundle ships no `cargo-clippy`), and every
//! consumer that runs `store/trust/current/bin` by absolute path (aterm-verify's store
//! discovery, `tools/bootstrap-publisher.sh`, the release gates).
//!
//! Unix only: on Windows a byte-identical copy passes tippy, so every function here answers
//! "no root" and lays nothing. No Linux bundle needs a root either — its copies are
//! byte-identical, so [`needs_root`] answers `None` there.

use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::seam::Depth;

/// `<prefix>/compat` — the top-level directory exec roots live under. Outside `store/`
/// on purpose: nothing may be laid inside the content-addressed store namespace.
pub const COMPAT_DIR: &str = "compat";

/// `<prefix>/compat`.
#[must_use]
pub fn compat_dir(layout: &Layout) -> PathBuf {
    layout.prefix.join(COMPAT_DIR)
}

/// `<prefix>/compat/trust` — every trust exec root is a child of it.
#[must_use]
pub fn roots_dir(layout: &Layout) -> PathBuf {
    compat_dir(layout).join(crate::seam::SEAM_PROGRAM)
}

/// `<prefix>/compat/trust/<build>` — the exec root of trust build `build`, spelled the
/// way [`Layout::build_dir`] spells the build.
#[must_use]
pub fn root_dir(layout: &Layout, build: u64) -> PathBuf {
    roots_dir(layout).join(build.to_string())
}

/// The file written LAST into a root before it is committed by `rename(2)`: a root without
/// it — half-way through a lay, laid as hard links before clones, or planted — is never
/// routed ([`route_for_shim`], and the shim's own guard, `platform::sh_shim_content_routed`)
/// and never reads as matching ([`root_first_mismatch`]).
pub const ROOT_MARKER: &str = ".atpkg-root";

/// The marker's contents: a version line, so a future construction can tell its own roots
/// from these.
const ROOT_MARKER_BODY: &str = "atpkg exec root v1 (copy-on-write clones)\n";

/// `<root>/.atpkg-root`.
#[must_use]
pub fn root_marker(root: &Path) -> PathBuf {
    root.join(ROOT_MARKER)
}

/// Whether `root`'s marker is a regular file, by `lstat` (a symlink is not one).
#[cfg(unix)]
fn marker_stands(root: &Path) -> bool {
    std::fs::symlink_metadata(root_marker(root)).is_ok_and(|m| m.is_file())
}

/// The first path at which the root at `root` stops being `build_dir` at `depth` — its
/// [`ROOT_MARKER`] when that is not a regular file, otherwise the view construction's own
/// check ([`crate::seam::first_mismatch`], which at [`Depth::Deep`] also reads the stock
/// names' bytes) — or `None` when it matches.
#[must_use]
pub fn root_first_mismatch(build_dir: &Path, root: &Path, depth: Depth) -> Option<PathBuf> {
    #[cfg(unix)]
    if crate::seam::is_real_dir(root) && !marker_stands(root) {
        return Some(root_marker(root));
    }
    crate::seam::first_mismatch(build_dir, root, depth)
}

/// Whether the root at `root` IS `build_dir` at `depth` ([`root_first_mismatch`] found
/// nothing).
#[must_use]
pub fn root_matches(build_dir: &Path, root: &Path, depth: Depth) -> bool {
    root_first_mismatch(build_dir, root, depth).is_none()
}

/// The one line every lane prints when it lays or rebuilds the exec root of trust build
/// `build` ([`Ensured::Built`]): `activate::install_tools_env` (install, the flip,
/// `repair`'s re-lay) and the pass-level reconcile (`cli::reconcile_exec_roots`). One
/// sentence in one place, so the two cannot drift.
#[must_use]
pub fn laid_line(layout: &Layout, build: u64) -> String {
    format!(
        "atpkg: exec root: trust build {build} ships a bin/rustc that is a separate file from \
         bin/trustc with different bytes, which its tippy refuses — laid {} (copy-on-write \
         clones, where rustc holds trustc's bytes; the store is untouched)",
        root_dir(layout, build).display()
    )
}

/// `<prefix>/compat` or `<prefix>/compat/trust` standing as something other than a real
/// directory — a symbolic link, or a file — which is where every trust exec root lives.
/// atpkg never follows or writes through what stands there (a reviewer showed a removal
/// following such a link out of the prefix, 2026-09-15), and no update, repair or gc removes
/// it — only `uninstall` takes `compat` — so no exec root is laid and no shim routes until a
/// person moves it. No retry can fix it — `aterm pkg repair` refused on every run, while the
/// lines around it said repair would — so every message about it names the path and that
/// one manual step: [`ensure_root`]'s error (what the passes print) and doctor's (5f) warn
/// both speak through this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// The first of `<prefix>/compat`, `<prefix>/compat/trust` that is not a real directory.
    pub at: PathBuf,
    /// Whether it is a symbolic link (`lstat`); otherwise it is some other non-directory.
    pub symlink: bool,
    /// The exec root that cannot be laid behind it, `<prefix>/compat/trust/<n>`.
    pub root: PathBuf,
}

impl Blocked {
    /// What stands in the way, and what atpkg therefore does not do.
    #[must_use]
    pub fn what(&self) -> String {
        if self.symlink {
            format!(
                "{} is a symbolic link, not a directory — atpkg lays nothing through a link \
                 there, and no update, repair or gc removes it, so no exec root is laid at {} \
                 and no shim routes through one",
                self.at.display(),
                self.root.display()
            )
        } else {
            format!(
                "{} is not a directory — no update, repair or gc removes it, so no exec root is \
                 laid at {} and no shim routes through one",
                self.at.display(),
                self.root.display()
            )
        }
    }

    /// The one fix: the manual step no verb takes, then the verb that lays the root.
    #[must_use]
    pub fn fix(&self) -> &'static str {
        if self.symlink {
            "remove that link (only the link: whatever it names is left as it is), then \
             `aterm pkg repair` lays the exec root"
        } else {
            "move that file out of the way, then `aterm pkg repair` lays the exec root"
        }
    }
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; fix: {}", self.what(), self.fix())
    }
}

impl std::error::Error for Blocked {}

/// The [`Blocked`] in the way of trust build `build`'s exec root, by `lstat` of
/// `<prefix>/compat` and then `<prefix>/compat/trust`: `None` when each is a real
/// directory or absent (absent is laid).
#[must_use]
pub fn blocked(layout: &Layout, build: u64) -> Option<Blocked> {
    [compat_dir(layout), roots_dir(layout)]
        .into_iter()
        .find_map(|at| {
            let meta = std::fs::symlink_metadata(&at).ok()?;
            (!meta.is_dir()).then(|| Blocked {
                symlink: meta.file_type().is_symlink(),
                at,
                root: root_dir(layout, build),
            })
        })
}

/// The [`Blocked`] an [`ensure_root`] error carries, if that is why it failed.
#[must_use]
pub fn blocked_by(e: &io::Error) -> Option<&Blocked> {
    e.get_ref()?.downcast_ref::<Blocked>()
}

/// The line the lanes that lay a root beside their own shims (`activate::install_tools_env`,
/// `flow::rollback_member`) print when [`ensure_root`] fails for trust build `build`. A
/// [`Blocked`] names its path and the manual fix — no retry lays a root there; any other
/// failure keeps the promise the next `aterm pkg repair` can keep.
#[must_use]
pub fn not_laid_line(layout: &Layout, build: u64, e: &io::Error) -> String {
    match blocked_by(e) {
        Some(blocked) => format!(
            "atpkg: trust build {build}: {} — this build's trust shims run the store path, where \
             its tippy refuses to start; fix: {}",
            blocked.what(),
            blocked.fix()
        ),
        None => format!(
            "atpkg: trust build {build}: exec root {} not put in order ({e}) — a shim routes \
             only through a whole root already standing there, and otherwise runs the store \
             path, where this build's tippy refuses to start; `aterm pkg repair` retries",
            root_dir(layout, build).display()
        ),
    }
}

/// What [`ensure_root`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensured {
    /// The build needs no root; any root left for it was removed.
    Plain,
    /// A root matching the build already stood; nothing was written.
    Present,
    /// A root was laid, or a mismatched one replaced.
    Built,
}

/// What a [`reconcile`] (or a bare [`sweep`]) did — the lines a pass prints only when
/// something changed or failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Builds whose root was laid or replaced.
    pub built: Vec<u64>,
    /// Shims re-laid because their bytes differed from the render — routed through a
    /// root, or back to plain where no root stands.
    pub routed: Vec<PathBuf>,
    /// Roots and debris removed.
    pub swept: Vec<PathBuf>,
    /// What could not be done, one sentence each; the shims involved run the store path.
    pub errors: Vec<String>,
}

impl Report {
    /// Whether anything was written or refused.
    #[must_use]
    pub fn changed(&self) -> bool {
        !(self.built.is_empty()
            && self.routed.is_empty()
            && self.swept.is_empty()
            && self.errors.is_empty())
    }
}

/// Whether the build at `build_dir` needs an exec root: `Ok(Some(differing_bytes))`
/// exactly when its tippy would refuse it, `Ok(None)` when it provably would not. All of
/// these must hold, each read with `lstat` (the byte compare opens both files without
/// following a link):
///
/// * `bin/rustc` exists, in any form;
/// * `bin/trustc` is a regular file;
/// * `bin/tippy` or `bin/targo-tippy` is a regular file — no tippy, nothing to refuse;
/// * `bin/rustc` is not `trustc`'s `(dev, ino)`;
/// * `bin/rustc` is not a byte-identical copy of the same length.
///
/// That is tippy's acceptance rule before trust `e79c1142a5` (`checked_compiler_pair`:
/// the same file, or identical bytes), turned around. It decides only whether a root is
/// OFFERED — a root holds nothing but the build's own inodes, so it is not a security
/// boundary. The count is the byte positions that differ plus the length difference, or
/// `trustc`'s whole length when `rustc` is not a regular file (no byte of it can be shown
/// to be `trustc`'s).
///
/// # Errors
/// A read that FAILS — an `lstat` refused for any reason but the name being absent, a
/// file that will not open or read — is an error, never `Ok(None)`: an unproven "needs
/// none" must not retire a root. Every caller that removes something acts on `Ok(None)`
/// alone — [`ensure_root`] leaves whatever stands and returns the error, and [`sweep`]
/// keeps the root — because a single failed read had swept a needed root, un-routed every
/// shim and moved every store inode twice on the pass that re-laid it (a reviewer measured
/// it with the store `bin/rustc` at mode `0111`, 2026-09-15).
pub fn needs_root(build_dir: &Path) -> io::Result<Option<u64>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let bin = build_dir.join("bin");
        // `lstat`, where an absent name (or a `bin` that is not a directory) is the answer
        // "not there" and every other failure is the question unanswered.
        let entry = |name: &str| match std::fs::symlink_metadata(bin.join(name)) {
            Ok(m) => Ok(Some(m)),
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                Ok(None)
            }
            Err(e) => Err(io::Error::new(
                e.kind(),
                format!("cannot lstat {} ({e})", bin.join(name).display()),
            )),
        };
        let Some(rustc) = entry("rustc")? else {
            return Ok(None);
        };
        let Some(trustc) = entry("trustc")?.filter(std::fs::Metadata::is_file) else {
            return Ok(None);
        };
        let has_tippy = entry("tippy")?.is_some_and(|m| m.is_file())
            || entry("targo-tippy")?.is_some_and(|m| m.is_file());
        if !has_tippy || (rustc.dev(), rustc.ino()) == (trustc.dev(), trustc.ino()) {
            return Ok(None);
        }
        if !rustc.is_file() {
            return Ok(Some(trustc.len()));
        }
        let differing = differing_bytes(
            &bin.join("rustc"),
            rustc.len(),
            &bin.join("trustc"),
            trustc.len(),
        )
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "cannot compare {} with {} ({e})",
                    bin.join("rustc").display(),
                    bin.join("trustc").display()
                ),
            )
        })?;
        Ok((differing != 0).then_some(differing))
    }
    #[cfg(not(unix))]
    {
        let _ = build_dir;
        Ok(None)
    }
}

/// The byte positions at which two files of the given lengths differ, plus the length
/// difference. Streams 64 KiB at a time; opened with `O_NOFOLLOW`.
#[cfg(unix)]
fn differing_bytes(a: &Path, a_len: u64, b: &Path, b_len: u64) -> io::Result<u64> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    const CHUNK: usize = 64 * 1024;
    let open = |p: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(p)
    };
    let (mut fa, mut fb) = (open(a)?, open(b)?);
    let common = a_len.min(b_len);
    let mut differing = a_len.max(b_len) - common;
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut left = common;
    while left > 0 {
        let n = usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK));
        fa.read_exact(&mut ba[..n])?;
        fb.read_exact(&mut bb[..n])?;
        let mismatched = ba[..n].iter().zip(&bb[..n]).filter(|(x, y)| x != y).count();
        differing += mismatched as u64;
        left -= n as u64;
    }
    Ok(differing)
}

/// The trust build number `build_dir` is, when it is exactly
/// `<prefix>/store/trust/<n>` of this layout — the only shape [`ensure_root`] accepts,
/// and what `activate::install_tools_env` asks before it offers one.
pub(crate) fn trust_build_of(layout: &Layout, build_dir: &Path) -> Option<u64> {
    let (program, n) = crate::ops::store_build_of(&layout.prefix, build_dir)?;
    (program == crate::seam::SEAM_PROGRAM && build_dir == layout.build_dir(&program, n))
        .then_some(n)
}

/// The trust build a shim TARGET is a tool of: `Some(n)` exactly when `target` is
/// `<prefix>/store/trust/<n>/bin/<file>` of this layout.
fn trust_tool_build(layout: &Layout, target: &Path) -> Option<u64> {
    let (program, n) = crate::ops::store_build_of(&layout.prefix, target)?;
    (program == crate::seam::SEAM_PROGRAM
        && target.parent() == Some(layout.build_dir(&program, n).join("bin").as_path()))
    .then_some(n)
}

/// Make the exec root of the trust build at `build_dir` what the build needs, at `depth`.
///
/// * The build provably needs no root ([`needs_root`] is `Ok(None)`): any root left for it
///   is removed — renamed aside first, a symlink unlinked — and the answer is
///   [`Ensured::Plain`]. Only under a `<prefix>/compat` and `compat/trust` that are both
///   real directories: behind a symlink at either name nothing is atpkg's, and a root-named
///   directory there is left whole (a reviewer showed the removal following such a link
///   out of the prefix, 2026-09-15).
/// * `compat` or `compat/trust` exists and is not a real directory: refused with a
///   [`Blocked`] ([`blocked_by`] recovers it), which names that path and the manual fix,
///   before anything is compared — a root reached through a link is not one
///   [`route_for_shim`] routes to, so answering `Present` over it had left every shim
///   plain with no error; and the refusal had read "update directory is a symlink;
///   refusing" beside advice to run `aterm pkg repair`, which refuses the same way.
/// * A root stands and [`root_matches`] holds at `depth`: nothing is written at all, not
///   even a directory mode — [`Ensured::Present`].
/// * Otherwise the root is laid at `.<n>.tmp-<pid>`, any standing root is renamed to
///   `.<n>.old-<pid>`, the temp renamed into place and the old one removed —
///   [`Ensured::Built`]. When the standing root's `bin/` still matches the build
///   ([`crate::seam::bin_mismatch`] and the stock names' bytes) and only a directory beside
///   it differs, the temp gets the mirrored directories alone
///   ([`crate::seam::lay_view_dirs`]) and the live `bin/` is MOVED into it by `rename(2)` —
///   so a `.DS_Store` under `share/` never replaces a `bin/` file a tippy running from the
///   root pins. The [`ROOT_MARKER`] is written into the temp last, before the swap. Between
///   the renames the root, or its `bin/`, is briefly absent, and a shim exec'd in that
///   instant runs the store path, as today.
///
/// # Errors
/// `build_dir` is not a trust build of this layout, [`needs_root`] could not read the
/// build, `compat` or `compat/trust` is not a directory atpkg may write (a [`Blocked`]
/// when something other than a real directory stands at either name), the root cannot
/// be removed, or it cannot be laid — a clone refused (the store on another volume), a
/// directory unreadable, the marker unwritable. On an error while laying or swapping a root, whatever stood at the
/// root is left where it was (a lay's temp tree is removed). The one exception is the
/// retire of a build that needs none: its root is renamed to `.<n>.old-<pid>` FIRST, so
/// when that tree then will not delete the error comes back with the name already empty
/// and the `.<n>.old-<pid>` debris left for [`sweep`] — never a half-deleted tree at the
/// name. Nothing is ever byte-copied, and a shim rendered afterwards routes only if
/// [`route_for_shim`] still finds a complete root.
pub fn ensure_root(layout: &Layout, build_dir: &Path, depth: Depth) -> io::Result<Ensured> {
    // MEASURED, once per process: a probe file written into the temp dir and read back
    // — never under `compat/`, which this function must not create for a build that
    // needs no root and must refuse, untouched, when a link stands at its name.
    let tracked =
        cfg!(target_os = "macos") && crate::provenance::process_is_tracked(&std::env::temp_dir());
    ensure_root_with(
        layout,
        build_dir,
        depth,
        tracked,
        &crate::lay::lane_for_this_binary(),
        crate::lay::tracked_policy(),
    )
}

/// [`ensure_root`] with the untracked lane's three inputs explicit. The root is one clone
/// per file of the build — the rustup view's construction — and every file a
/// provenance-TRACKED process creates is tagged, so a tracked process hands the laying to
/// the launchd job the view uses ([`crate::seam::run_view_job`], a `ViewJob::Root`),
/// measured on the root's clone of a store file clean before the job. When the lane cannot
/// run: a root that already STANDS is kept as it is (one build behind at worst, refreshed
/// next pass) rather than lay a tagged one from this process; with no root at all the
/// in-process build is worth its tag under [`crate::lay::TrackedPolicy::Allow`] (tippy
/// would refuse the store otherwise) and says so, and is refused under `Refuse`.
pub fn ensure_root_with(
    layout: &Layout,
    build_dir: &Path,
    depth: Depth,
    tracked: bool,
    lane: &crate::lay::Lane,
    policy: crate::lay::TrackedPolicy,
) -> io::Result<Ensured> {
    let helper = match (tracked, lane) {
        (true, crate::lay::Lane::Helper(exe)) => exe,
        _ => return ensure_root_in_process(layout, build_dir, depth),
    };
    // Nothing to LAY — no lane needed — when the build ships one compiler as one file,
    // when a link or a file stands where the root would go (refused, untouched, by the
    // in-process body's own rule), or when the standing root already matches at the
    // asked depth. Every one of those reads and never links, so the process may do it.
    if let Some(n) = trust_build_of(layout, build_dir)
        && (matches!(needs_root(build_dir), Ok(None))
            || blocked(layout, n).is_some()
            || root_matches(build_dir, &root_dir(layout, n), depth))
    {
        return ensure_root_in_process(layout, build_dir, depth);
    }
    match crate::seam::run_view_job(
        helper,
        layout,
        &crate::seam::ViewJob::Root {
            build_dir: build_dir.to_path_buf(),
        },
        build_dir,
    ) {
        Ok(line) => Ok(ensured_from_word(
            line.strip_prefix("root ").unwrap_or(&line),
        )),
        Err(why) => match policy {
            crate::lay::TrackedPolicy::Refuse => Err(io::Error::other(
                crate::lay::tracked_refusal("lay the trust exec root", &why),
            )),
            crate::lay::TrackedPolicy::Allow => {
                let standing = trust_build_of(layout, build_dir)
                    .is_some_and(|n| crate::seam::is_real_dir(&root_dir(layout, n).join("bin")));
                if standing {
                    eprintln!(
                        "atpkg: note — this process is provenance-tracked and the untracked lane \
                         could not lay the trust exec root ({why}); the root that stands is kept \
                         rather than lay a tagged one from this process; the next pass \
                         refreshes it"
                    );
                    return Ok(Ensured::Present);
                }
                eprintln!(
                    "atpkg: note — this process is provenance-tracked and the untracked lane \
                     could not lay the trust exec root ({why}); there is no root yet, so it is \
                     laid in-process, and every file of the root it clones WILL carry \
                     com.apple.provenance (the store is not touched) — `aterm pkg repair` \
                     re-lays it clean once the lane runs"
                );
                ensure_root_in_process(layout, build_dir, depth)
            }
        },
    }
}

/// The word the view helper answers for an [`Ensured`], and back.
#[must_use]
pub fn ensured_word(e: Ensured) -> &'static str {
    match e {
        Ensured::Plain => "plain",
        Ensured::Present => "present",
        Ensured::Built => "built",
    }
}

fn ensured_from_word(word: &str) -> Ensured {
    match word.trim() {
        "plain" => Ensured::Plain,
        "built" => Ensured::Built,
        _ => Ensured::Present,
    }
}

/// [`ensure_root`]'s body IN THIS PROCESS — what an untracked process runs directly and
/// the launchd helper runs for a tracked one.
pub fn ensure_root_in_process(
    layout: &Layout,
    build_dir: &Path,
    depth: Depth,
) -> io::Result<Ensured> {
    let Some(n) = trust_build_of(layout, build_dir) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is not a trust build of {}",
                build_dir.display(),
                layout.prefix.display()
            ),
        ));
    };
    #[cfg(unix)]
    {
        let root = root_dir(layout, n);
        let dir = roots_dir(layout);
        let pid = std::process::id();
        let aside = dir.join(format!(".{n}.old-{pid}"));
        match needs_root(build_dir) {
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "cannot tell whether trust build {n} needs an exec root: {e}; whatever \
                         stands at {} is left as it is",
                        root.display()
                    ),
                ));
            }
            Ok(None) => {
                if walkable_roots_dir(layout).is_some() {
                    retire(&root, &aside)?;
                }
                return Ok(Ensured::Plain);
            }
            Ok(Some(_)) => {}
        }
        // A link or a file at either name: refused with the path and the manual fix, before
        // anything is compared or created — no retry could lay a root there.
        if let Some(blocked) = blocked(layout, n) {
            return Err(io::Error::other(blocked));
        }
        // Both write nothing over directories already right (`ensure_private_dir` chmods
        // only a mode that differs), and both refuse a symlink at their name — one planted
        // between the check above and here.
        layout.ensure_dir(&compat_dir(layout))?;
        layout.ensure_dir(&dir)?;
        if root_matches(build_dir, &root, depth) {
            return Ok(Ensured::Present);
        }
        let tmp = dir.join(format!(".{n}.tmp-{pid}"));
        remove_entry(&tmp)?;
        remove_entry(&aside)?;
        let standing = std::fs::symlink_metadata(&root).is_ok();
        let carry_bin = crate::seam::is_real_dir(&root)
            && crate::seam::bin_mismatch(build_dir, &root).is_none()
            && crate::seam::stock_bytes_mismatch(build_dir, &root).is_none();
        let laid = if carry_bin {
            crate::seam::lay_view_dirs(layout, build_dir, &tmp)
        } else {
            crate::seam::lay_view(layout, build_dir, &tmp)
        };
        if let Err(e) = laid {
            let _ = remove_entry(&tmp);
            return Err(e);
        }
        let (root_bin, tmp_bin) = (root.join("bin"), tmp.join("bin"));
        if carry_bin && let Err(e) = std::fs::rename(&root_bin, &tmp_bin) {
            let _ = remove_entry(&tmp);
            return Err(e);
        }
        // Put a carried `bin/` back into the standing root after a failed swap.
        let restore_bin = || {
            if carry_bin {
                let _ = std::fs::rename(&tmp_bin, &root_bin);
            }
        };
        // The marker LAST, so no name ever holds a routable root that is not whole.
        if let Err(e) = std::fs::write(root_marker(&tmp), ROOT_MARKER_BODY) {
            restore_bin();
            let _ = remove_entry(&tmp);
            return Err(io::Error::new(
                e.kind(),
                format!("cannot write {} ({e})", root_marker(&tmp).display()),
            ));
        }
        if standing && let Err(e) = std::fs::rename(&root, &aside) {
            restore_bin();
            let _ = remove_entry(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, &root) {
            if standing {
                let _ = std::fs::rename(&aside, &root);
            }
            restore_bin();
            let _ = remove_entry(&tmp);
            return Err(e);
        }
        // Committed. The old tree is debris now; if it will not go, `sweep` takes it.
        let _ = remove_entry(&aside);
        Ok(Ensured::Built)
    }
    #[cfg(not(unix))]
    {
        let _ = (n, depth);
        Ok(Ensured::Plain)
    }
}

/// Remove whatever sits at `path`, by `lstat`: nothing is fine, a symlink or a file is
/// unlinked (never followed), a directory removed recursively (std's `remove_dir_all`
/// does not follow symlinks inside it).
#[cfg(unix)]
fn remove_entry(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    }
}

/// Remove a committed root at `root`: a real directory is renamed to `aside` FIRST, so
/// its name vanishes in one step and no shim can route into a tree half-way through
/// deletion; anything else at the name is unlinked. `Ok(true)` when something was there.
#[cfg(unix)]
fn retire(root: &Path, aside: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => {
            remove_entry(aside)?;
            std::fs::rename(root, aside)?;
            std::fs::remove_dir_all(aside)?;
            Ok(true)
        }
        Ok(_) => std::fs::remove_file(root).map(|()| true),
    }
}

/// Whether `target`, in a build's `bin/` at `bin`, is a stock name ([`crate::seam::STOCK_NAMES`])
/// that is a separate file from its Trust tool — the bundle's own COPY (8595 ships
/// `rustc`, `cargo` and `rustdoc` that way). A root presents the stock name as the Trust
/// tool, so a shim forwarding to the copy is never routed and never counted unrouted. A
/// stock name that IS its Trust tool's file in the store (a pack that kept one hard link)
/// is not a copy. This asks about the STORE bundle's shape, so it compares the store's own
/// inodes.
#[cfg(unix)]
fn is_stock_copy(bin: &Path, target: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(file) = target.file_name() else {
        return false;
    };
    crate::seam::STOCK_NAMES.iter().any(|(public, trust)| {
        if file != std::ffi::OsStr::new(public) {
            return false;
        }
        match (
            std::fs::symlink_metadata(bin.join(trust)),
            std::fs::symlink_metadata(target),
        ) {
            (Ok(t), Ok(c)) => t.is_file() && (t.dev(), t.ino()) != (c.dev(), c.ino()),
            _ => false,
        }
    })
}

/// The root file a shim at `shim` forwarding to `target` should run instead, or `None`
/// for today's shim. `stat` only. `Some(<root>/bin/<file>)` exactly when:
///
/// * `target` is `<P>/store/trust/<n>/bin/<file>`, where `<P>` is the shim's own prefix
///   (`shim.parent().parent()`, which holds for `bin/` and `agents/` alike) — so another
///   program, a dev link into a checkout, a lookalike prefix, extra path components and
///   a non-canonical build spelling all answer `None`;
/// * `<P>/compat`, `<P>/compat/trust`, the root and its `bin/` are real directories, and
///   the root's [`ROOT_MARKER`] is a regular file;
/// * `<root>/bin/<file>` is a clone of `target` ([`crate::clone::is_clone_of`]: a regular
///   file with its length, mode and time, not its inode);
/// * `<root>/bin/rustc` is a clone of the store `trustc` — the property the root exists
///   for;
/// * `target` is not a stock-name COPY the bundle ships beside its Trust tool (`rustdoc`
///   as a separate file from `trustdoc`): the root presents that name as the Trust tool,
///   which is not what such a shim forwards to;
/// * `<root>/lib` is a real directory whenever the build has a `lib/`.
///
/// It does not re-walk the root or read bytes: [`ensure_root`] proves the root when it
/// lays it, and `repair` and doctor at [`Depth::Deep`]; the shim's guard re-checks the
/// file and the marker at every exec. It does not ask [`needs_root`] either — that reads
/// bytes, and a root stands only where it held.
#[must_use]
pub fn route_for_shim(shim: &Path, target: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let prefix = shim.parent()?.parent()?;
        let layout = Layout {
            prefix: prefix.to_path_buf(),
        };
        let n = trust_tool_build(&layout, target)?;
        let file = target.file_name()?;
        let build = layout.build_dir(crate::seam::SEAM_PROGRAM, n);
        let root = root_dir(&layout, n);
        let root_bin = root.join("bin");
        let chain = [
            compat_dir(&layout),
            roots_dir(&layout),
            root.clone(),
            root_bin.clone(),
        ];
        if !chain.iter().all(|d| crate::seam::is_real_dir(d)) || !marker_stands(&root) {
            return None;
        }
        if is_stock_copy(&build.join("bin"), target) {
            return None;
        }
        let routed = root_bin.join(file);
        if !crate::clone::is_clone_of(target, &routed)
            || !crate::clone::is_clone_of(
                &build.join("bin").join("trustc"),
                &root_bin.join("rustc"),
            )
        {
            return None;
        }
        if build.join("lib").is_dir() && !crate::seam::is_real_dir(&root.join("lib")) {
            return None;
        }
        Some(routed)
    }
    #[cfg(not(unix))]
    {
        let _ = (shim, target);
        None
    }
}

/// An entry under `<prefix>/compat/trust` that is not a root any build needs, and that the
/// next [`sweep`] (every `gc`) removes — what doctor names with `aterm pkg gc` as the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stray {
    /// A numeric name that is not a real directory — a symlink or a file. atpkg lays only
    /// directories there.
    NotADirectory,
    /// A root whose build is no longer in the store: its clones keep that build's reclaimed
    /// blocks allocated.
    BuildGone(u64),
    /// A root whose build provably needs none ([`needs_root`] read it and answered
    /// `Ok(None)`) — its tippy refuses nothing.
    NeedsNone(u64),
}

/// What an entry of `<prefix>/compat/trust` named `name` is — the ONE classification
/// [`sweep`] removes by and [`strays`] reports by, so doctor names exactly what gc takes.
#[cfg(unix)]
enum RootsEntry {
    /// Dot-named: `.<n>.tmp-<pid>`, `.<n>.old-<pid>`, or anything else dot-named.
    Debris,
    /// Not a root any build needs.
    Stray(Stray),
    /// The root of a build that needs one — or whose need could not be read, which is not
    /// proof that it needs none.
    Needed,
}

/// Classify one entry of `<prefix>/compat/trust` by its name and `lstat` (and, for a root
/// whose build stands, [`needs_root`]'s read of the build). `None` for a name that is not
/// atpkg's — neither dot-named nor a canonical decimal build number — which is left alone.
#[cfg(unix)]
fn classify(layout: &Layout, name: &str, path: &Path) -> Option<RootsEntry> {
    if name.starts_with('.') {
        return Some(RootsEntry::Debris);
    }
    let n = name.parse::<u64>().ok().filter(|n| n.to_string() == name)?;
    // Every question below is answered only by a PROVEN answer: an `lstat` that fails for
    // any reason but an absent name (a `compat/trust` or `store/trust` that cannot be
    // searched) leaves the entry alone, exactly as a build whose need cannot be read does
    // ([`needs_root`]'s errors). The reconcile that follows the sweep names the failed read
    // for every build a shim still points at.
    let build = layout.build_dir(crate::seam::SEAM_PROGRAM, n);
    Some(match (real_dir(path), real_dir(&build)) {
        (Err(_), _) => RootsEntry::Needed,
        (Ok(false), _) => RootsEntry::Stray(Stray::NotADirectory),
        (Ok(true), Ok(false)) => RootsEntry::Stray(Stray::BuildGone(n)),
        (Ok(true), Ok(true)) if matches!(needs_root(&build), Ok(None)) => {
            RootsEntry::Stray(Stray::NeedsNone(n))
        }
        (Ok(true), _) => RootsEntry::Needed,
    })
}

/// `lstat` of `path` as an ANSWER: `Ok(true)` for a real directory, `Ok(false)` for
/// provably none — the name absent, a component above it not a directory (`ENOTDIR`), or
/// something other than a directory (a symlink included) at the name — and `Err` when the
/// question went unanswered, a component above it that cannot be searched, say. What
/// [`crate::seam::is_real_dir`] folds into `false`, for the callers that remove by it.
#[cfg(unix)]
fn real_dir(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Ok(m.is_dir()),
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) =>
        {
            Ok(false)
        }
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("cannot lstat {} ({e})", path.display()),
        )),
    }
}

/// `<prefix>/compat/trust`, when it and `<prefix>/compat` are both real directories — the
/// only shape [`sweep`] and [`strays`] walk. A symlink at either name is not followed into
/// whatever it names.
#[cfg(unix)]
fn walkable_roots_dir(layout: &Layout) -> Option<PathBuf> {
    let dir = roots_dir(layout);
    (crate::seam::is_real_dir(&compat_dir(layout)) && crate::seam::is_real_dir(&dir)).then_some(dir)
}

/// Every entry of `<prefix>/compat/trust` the next [`sweep`] would remove as not a root any
/// build needs, with why, sorted by path. `lstat`s and [`needs_root`]'s reads only —
/// doctor's half of the sweep, which writes nothing. Dot-named debris is NOT reported:
/// doctor holds no store lock, so a `.<n>.tmp-<pid>` may be a root another atpkg is laying
/// this instant, and a warn about it would be wrong exactly when it is read.
#[must_use]
pub fn strays(layout: &Layout) -> Vec<(PathBuf, Stray)> {
    #[cfg(unix)]
    {
        let Some(dir) = walkable_roots_dir(layout) else {
            return Vec::new();
        };
        let Ok(listing) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<(PathBuf, Stray)> = listing
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = entry.file_name().to_str()?.to_string();
                match classify(layout, &name, &path)? {
                    RootsEntry::Stray(why) => Some((path, why)),
                    RootsEntry::Debris | RootsEntry::Needed => None,
                }
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    #[cfg(not(unix))]
    {
        let _ = layout;
        Vec::new()
    }
}

/// Remove, from `<prefix>/compat/trust`, every entry that is not a root a build needs:
/// dot-named debris (`.<n>.tmp-<pid>`, `.<n>.old-<pid>` a crashed run left, or anything
/// else dot-named), a symlink or file at a numeric name (unlinked, never followed), and a
/// root whose build is gone or whose build no longer [`needs_root`] (renamed aside, then
/// removed). Other names are not atpkg's and are left alone. A `compat/trust` or `compat`
/// that is not a real directory is not walked: [`ensure_root`] refuses to lay into it,
/// and says so.
///
/// # Why it is safe here
///
/// Callers hold the store lock, as every verb that lays or discards a build does — but
/// that lock speaks only for the processes that TAKE it, and the lane that lays a root is
/// not one of them. A provenance-tracked pass hands the lay to a LAUNCHD job
/// ([`ensure_root_with`] submits a [`crate::seam::ViewJob::Root`], whose helper runs
/// [`ensure_root_in_process`] in a process of launchd's, under the `view-helper` stem),
/// so the helper is launchd's child rather than the submitter's and holds no lock of its
/// own. A `kill -9` of that pass drops the store lock while its helper keeps laying into
/// the very `.<n>.tmp-<pid>` and `.<n>.old-<pid>` names this sweep removes as debris: the
/// tree came back within the helper's next write, and nothing under `compat/` is ever
/// named again unless that same build number is laid once more.
///
/// So the orphaned lane jobs are STOPPED, and waited out, before a single entry is read —
/// exactly as [`crate::store::sweep_stage_scratch`], `gc`'s partial sweep and
/// `seam`'s view-debris sweep do it. The set of labels stopped is unchanged and
/// unwidened: only this prefix, only these stems, and a job whose owner pid is ALIVE
/// (another pass in flight, or a pid since reused) is left alone.
#[must_use]
pub fn sweep(layout: &Layout) -> Report {
    #[cfg(unix)]
    {
        // STOP FIRST, DELETE SECOND: a launchd-parented lane helper outlives the pass that
        // submitted it, and the store lock that pass dropped says nothing about the helper.
        crate::stage_helper::stop_orphaned_lane_jobs();
        let mut report = Report::default();
        let Some(dir) = walkable_roots_dir(layout) else {
            return report;
        };
        let Ok(listing) = std::fs::read_dir(&dir) else {
            report
                .errors
                .push(format!("{} could not be listed", dir.display()));
            return report;
        };
        let pid = std::process::id();
        for entry in listing.flatten() {
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let removed = match classify(layout, &name, &path) {
                None | Some(RootsEntry::Needed) => continue,
                Some(RootsEntry::Debris | RootsEntry::Stray(Stray::NotADirectory)) => {
                    remove_entry(&path).map(|()| true)
                }
                Some(RootsEntry::Stray(Stray::BuildGone(n) | Stray::NeedsNone(n))) => {
                    retire(&path, &dir.join(format!(".{n}.old-{pid}")))
                }
            };
            match removed {
                Ok(true) => report.swept.push(path),
                Ok(false) => {}
                Err(e) => report.errors.push(format!(
                    "{} was not removed ({e}); it keeps a reclaimed build's files allocated",
                    path.display()
                )),
            }
        }
        report
    }
    #[cfg(not(unix))]
    {
        let _ = layout;
        Report::default()
    }
}

/// `(prefix, program, build)` when `build_dir` is spelled exactly
/// `<prefix>/store/<program>/<build>` — a canonical decimal build number, a program name
/// that is a plain component — and `None` for every other path, a stage scratch sibling
/// (`<n>.incoming-<pid>`, `<n>.superseded-<pid>`) included.
#[cfg(unix)]
fn store_build_parts(build_dir: &Path) -> Option<(&Path, &str, u64)> {
    let name = build_dir.file_name()?.to_str()?;
    let n = name.parse::<u64>().ok().filter(|n| n.to_string() == name)?;
    let program_dir = build_dir.parent()?;
    let program = program_dir.file_name()?.to_str()?;
    if program.starts_with('.') {
        return None;
    }
    let store = program_dir.parent()?;
    if store.file_name()? != "store" {
        return None;
    }
    Some((store.parent()?, program, n))
}

/// Remove the exec root of the build at `build_dir` — `<prefix>/compat/<program>/<n>` for
/// a `build_dir` of `<prefix>/store/<program>/<n>`, derived from that parent chain alone
/// and from nothing else, so any other path names no root. The half of
/// [`crate::store::discard_build`] that keeps a root from outliving its build: every
/// discard path (gc's supersede, its partial sweep, flow's abort) goes through that one
/// function, so each takes the root with the tree.
///
/// Best-effort and silent, like the rest of `discard_build`. `lstat` all the way down:
/// `<prefix>/compat` and `<prefix>/compat/<program>` must be real directories, or nothing
/// under them is atpkg's to remove; a real root is renamed aside first (`.<n>.old-<pid>`)
/// so no shim routes into a half-deleted tree, and a symlink at the name is unlinked,
/// never followed. What will not go is left for [`sweep`], which removes a root whose
/// build is gone — the clones a root holds keep a reclaimed build's blocks allocated
/// until then.
pub(crate) fn discard_root_of(build_dir: &Path) {
    #[cfg(unix)]
    {
        let Some((prefix, program, n)) = store_build_parts(build_dir) else {
            return;
        };
        let compat = prefix.join(COMPAT_DIR);
        let dir = compat.join(program);
        if !crate::seam::is_real_dir(&compat) || !crate::seam::is_real_dir(&dir) {
            return;
        }
        let aside = dir.join(format!(".{n}.old-{}", std::process::id()));
        let _ = retire(&dir.join(n.to_string()), &aside);
    }
    #[cfg(not(unix))]
    {
        let _ = build_dir;
    }
}

/// Remove `<prefix>/compat/<program>` whole — `ops::uninstall`'s half, run after the
/// program's shims are gone, so nothing routes into what is being removed. A
/// `<prefix>/compat` that is not a real directory holds nothing of this program's and is
/// left for [`remove_all`]; a symlink at `<prefix>/compat/<program>` is unlinked, never
/// followed.
///
/// # Errors
/// The directory could not be removed. The shims are already gone, and [`sweep`] takes
/// any trust root whose build no longer stands.
pub fn remove_program(layout: &Layout, program: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        let compat = compat_dir(layout);
        if !crate::seam::is_real_dir(&compat) {
            return Ok(());
        }
        remove_entry(&compat.join(program))
    }
    #[cfg(not(unix))]
    {
        let _ = (layout, program);
        Ok(())
    }
}

/// Remove `<prefix>/compat` whole — `uninstall --all`'s half, on both of its branches (an
/// empty store can still carry a root an older run left). A symlink planted at the name is
/// unlinked, never followed.
///
/// # Errors
/// The directory could not be removed.
pub fn remove_all(layout: &Layout) -> io::Result<()> {
    #[cfg(unix)]
    {
        remove_entry(&compat_dir(layout))
    }
    #[cfg(not(unix))]
    {
        let _ = layout;
        Ok(())
    }
}

/// Bring every trust shim in `bin/` in line with the exec roots its build needs, and
/// write nothing when it already is:
///
/// 1. [`sweep`];
/// 2. group the `bin/` entries whose target resolves to `<prefix>/store/trust/<n>/bin`
///    by `n` — primaries, `alab-` aliases and env-carrying shims alike; a tombstone, a
///    pending stub, a dev link and another program's shim resolve elsewhere and are
///    never touched;
/// 3. [`ensure_root`] at `depth` for each `n` whose build is there;
/// 4. re-render each grouped shim through `platform::shim_executable_to_env` with the
///    environment it already exports, and keep those whose bytes differ — routed where a
///    root now stands, back to plain where none does;
/// 5. lay them in ONE [`crate::lay::lay_executables`] call, so a provenance-tracked pass
///    uses one untracked job, as every shim-laying pass does.
///
/// A second run finds nothing to do. This is what reaches the shims no install rewrites:
/// the pass short-circuits an up-to-date program before it lays a shim, and the alias
/// reconcile keeps an alias whose target and environment already match — so a plain
/// `alab-tippy` on an affected build would otherwise stay plain forever.
#[must_use]
pub fn reconcile(layout: &Layout, depth: Depth) -> Report {
    if cfg!(not(unix)) {
        // No root is laid off Unix: every shim is today's, and nothing is re-rendered.
        let _ = depth;
        return Report::default();
    }
    let mut report = sweep(layout);
    let groups = trust_shims(layout);
    let mut files = Vec::new();
    for (n, shims) in &groups {
        let build = layout.build_dir(crate::seam::SEAM_PROGRAM, *n);
        if std::fs::symlink_metadata(&build).is_ok_and(|m| m.is_dir()) {
            match ensure_root(layout, &build, depth) {
                Ok(Ensured::Built) => report.built.push(*n),
                Ok(Ensured::Plain | Ensured::Present) => {}
                Err(e) => report.errors.push(match blocked_by(&e) {
                    // The path and the manual fix, without the "not put in order" framing
                    // that reads as something a retry finishes.
                    Some(blocked) => format!(
                        "trust build {n}: {} — its trust shims render plain and run the store \
                         path; fix: {}",
                        blocked.what(),
                        blocked.fix()
                    ),
                    None => format!(
                        "trust build {n}: exec root {} not put in order ({e}); a shim routes \
                         only through a whole root already standing there, and otherwise runs \
                         the store path",
                        root_dir(layout, *n).display()
                    ),
                }),
            }
        }
        for (shim, target) in shims {
            let env = crate::platform::shim_env_of(shim);
            let want = match crate::platform::shim_executable_to_env(shim, target, &env) {
                Ok(want) => want,
                Err(e) => {
                    report
                        .errors
                        .push(format!("{} not rendered ({e})", shim.display()));
                    continue;
                }
            };
            let have =
                crate::metadata_io::read_bounded_regular(shim, crate::platform::MAX_SHIM_BYTES);
            if !have.is_ok_and(|bytes| bytes == want.body) {
                files.push(want);
            }
        }
    }
    if !files.is_empty() {
        match crate::lay::lay_executables(&files) {
            Ok(()) => report.routed.extend(files.into_iter().map(|f| f.path)),
            Err(e) => report.errors.push(format!(
                "{} trust shim(s) not re-laid ({e}); they run as they were",
                files.len()
            )),
        }
    }
    report
}

/// The `bin/` shims whose target is a tool of a trust build of this layout
/// (`<prefix>/store/trust/<n>/bin/<file>`), grouped by `n`, each group sorted by shim
/// path: primaries, `alab-` aliases and env-carrying shims alike. A tombstone, a pending
/// stub, a dev link and another program's shim resolve elsewhere and are not listed.
/// Reads only — what [`reconcile`] re-renders and [`inspect`] counts.
fn trust_shims(layout: &Layout) -> std::collections::BTreeMap<u64, Vec<(PathBuf, PathBuf)>> {
    let mut groups: std::collections::BTreeMap<u64, Vec<(PathBuf, PathBuf)>> =
        std::collections::BTreeMap::new();
    if let Ok(listing) = std::fs::read_dir(layout.bin_dir()) {
        for entry in listing.flatten() {
            let shim = entry.path();
            let Some(target) = crate::platform::resolve_shim(&shim) else {
                continue;
            };
            if let Some(n) = trust_tool_build(layout, &target) {
                groups.entry(n).or_default().push((shim, target));
            }
        }
    }
    for shims in groups.values_mut() {
        shims.sort();
    }
    groups
}

/// Where an affected build's exec root stands, as [`inspect`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootState {
    /// `<prefix>/compat` or `<prefix>/compat/trust` is not a real directory: no root can be
    /// laid or routed behind it, whatever stands there, until a person moves it.
    Blocked(Blocked),
    /// Nothing at `<prefix>/compat/trust/<n>`.
    Absent,
    /// Something stands there, and this is the first path at which it stops being the
    /// build at [`Depth::Deep`] — a file that is not a clone of the store's, a stock name
    /// without its Trust tool's bytes, an extra entry, a missing one, an absent
    /// [`ROOT_MARKER`] — or the root itself when it cannot be `lstat`ed.
    Differs(PathBuf),
    /// The root is a clone of the build, every file of it, with its marker.
    Matches,
}

/// What doctor reads about the exec root of one trust build that [`needs_root`]: the
/// evidence, where the root stands, and which shims route through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// [`needs_root`]'s count: byte positions at which `bin/rustc` and `bin/trustc`
    /// differ, plus the length difference.
    pub differing: u64,
    /// `bin/rustc`'s inode number (`lstat`).
    pub rustc_ino: u64,
    /// `bin/trustc`'s inode number (`lstat`).
    pub trustc_ino: u64,
    /// `<prefix>/store/trust/<n>`, the build the root must be.
    pub build_dir: PathBuf,
    /// `<prefix>/compat/trust/<n>`.
    pub root: PathBuf,
    /// Where the root stands.
    pub state: RootState,
    /// File names of the `bin/` shims whose target is a regular file in this build's
    /// `bin/`, sorted — less those forwarding to a stock-name COPY the bundle ships beside a
    /// Trust tool the root presents under that name, which [`route_for_shim`] never routes.
    /// A shim whose target is gone is a
    /// broken shim, named elsewhere.
    pub shims: Vec<String>,
    /// Those of [`Inspection::shims`] whose bytes are not the ROUTED render — the guard
    /// line through `<root>/bin/<file>` ahead of the store `exec`, with the environment the
    /// shim already exports. Such a shim runs the store path, whatever stands at the root.
    pub unrouted: Vec<String>,
}

/// Read the exec root of trust build `build` for doctor: `None` when the build is provably
/// not in the store as a real directory or provably needs no root (every Trust-names-only
/// bundle: doctor prints nothing about it), `Some(Err(why))` when the build directory's
/// `lstat` failed for any reason but an absent name, or [`needs_root`] could not read the
/// build — doctor cannot vouch for PATH tippy then, and says so rather than print
/// nothing. `lstat`s, [`needs_root`]'s byte compare of
/// two ~330 KiB files on 8595, a [`Depth::Deep`] walk of the root ([`root_first_mismatch`],
/// ~0.1 s over 8595's 4,112 files, plus the stock names' ~38 MB of bytes) and a bounded read
/// of each trust shim — it writes
/// nothing, creates nothing, and runs no tool: a probe that EXECUTED tippy would answer
/// through tippy's ancestor-directory checks, which refuse intermittently on their own
/// (`$HOME` changing mid-run), so the verdict here is the inode facts tippy's own rule is
/// made of.
#[must_use]
pub fn inspect(layout: &Layout, build: u64) -> Option<Result<Inspection, String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let build_dir = layout.build_dir(crate::seam::SEAM_PROGRAM, build);
        match real_dir(&build_dir) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(e) => return Some(Err(e.to_string())),
        }
        let differing = match needs_root(&build_dir) {
            Ok(differing) => differing?,
            Err(e) => return Some(Err(e.to_string())),
        };
        let bin = build_dir.join("bin");
        let ino = |name: &str| {
            std::fs::symlink_metadata(bin.join(name))
                .map(|m| m.ino())
                .map_err(|e| format!("cannot lstat {} ({e})", bin.join(name).display()))
        };
        let (rustc_ino, trustc_ino) = match (ino("rustc"), ino("trustc")) {
            (Ok(r), Ok(t)) => (r, t),
            (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
        };
        let root = root_dir(layout, build);
        // The chain first: behind a link at `compat` or `compat/trust` a root is neither
        // absent nor different — it is out of atpkg's reach, and `aterm pkg repair` refuses
        // there exactly as every pass does.
        let state = match blocked(layout, build) {
            Some(blocked) => RootState::Blocked(blocked),
            None => match std::fs::symlink_metadata(&root) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => RootState::Absent,
                Err(_) => RootState::Differs(root.clone()),
                Ok(_) => root_first_mismatch(&build_dir, &root, Depth::Deep)
                    .map_or(RootState::Matches, RootState::Differs),
            },
        };
        let mut shims = Vec::new();
        let mut unrouted = Vec::new();
        for (shim, target) in trust_shims(layout).remove(&build).unwrap_or_default() {
            // A shim whose target is not a regular file of the build has nothing to route
            // to (no root carries it, and [`route_for_shim`] answers `None`): it is a broken
            // shim, which doctor's own scan names, not an unrouted one.
            if !std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_file()) {
                continue;
            }
            let (Some(name), Some(file)) = (shim.file_name(), target.file_name()) else {
                continue;
            };
            // A shim to the stock-name COPY the bundle ships (`rustdoc`, say — `rustc` and
            // `cargo` are refused as shim names) has nothing to route to either: the root
            // presents that name as the Trust tool, not the copy, so the writer's
            // [`route_for_shim`] answers `None` for it on every render. Counted, it read as
            // unrouted forever — a warn `aterm pkg repair` could never clear (a reviewer
            // showed it with a `bin/rustdoc` copy exposed, 2026-09-15). It runs the store
            // copy, which no tippy authenticates, exactly as before roots existed.
            //
            // Only a COPY ([`is_stock_copy`], the one predicate the writer asks too): a stock
            // name that IS its Trust tool's file in the store is that tool, so the writer
            // routes its shim and a plain one is unrouted like any other (a reviewer's probe
            // showed the name-only skip reading healthy while a reconcile still routed it,
            // 2026-09-15).
            if is_stock_copy(&bin, &target) {
                continue;
            }
            let name = name.to_string_lossy().into_owned();
            let want = crate::platform::sh_shim_content_routed(
                &target,
                &crate::platform::shim_env_of(&shim),
                Some(&root.join("bin").join(file)),
            );
            let routed =
                crate::metadata_io::read_bounded_regular(&shim, crate::platform::MAX_SHIM_BYTES)
                    .is_ok_and(|have| have == want.as_bytes());
            if !routed {
                unrouted.push(name.clone());
            }
            shims.push(name);
        }
        Some(Ok(Inspection {
            differing,
            rustc_ino,
            trustc_ino,
            build_dir,
            root,
            state,
            shims,
            unrouted,
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = (layout, build);
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::store::ToolName;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// A private temp prefix. Nothing here reads `$HOME`, the real prefix or `~/.rustup`.
    struct Fx {
        root: PathBuf,
        layout: Layout,
    }

    /// The shapes a trust build's `bin/` has come in.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Shape {
        /// 8571/8589/8590/8595: `rustc` and `cargo` separate copies that differ.
        Copy,
        /// Trust-names-only (trust `dist.rs`, 2026-09-14 on).
        NamesOnly,
        /// A pack whose extractor kept `rustc` a hard link of `trustc`.
        Linked,
        /// A byte-identical copy (the Linux shape: no signature bakes the name in).
        Identical,
        /// A differing copy, but no tippy and no targo-tippy to refuse it.
        CopyNoTippy,
        /// A differing copy with only `targo-tippy`.
        CopyTargoTippyOnly,
        /// A differing `rustc` beside no `trustc`.
        NoTrustc,
    }

    impl Fx {
        fn new(label: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("atpkg-compat-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            let prefix = root.join("pkg");
            std::fs::create_dir_all(prefix.join("bin")).unwrap();
            Fx {
                root,
                layout: Layout { prefix },
            }
        }

        /// Lay `store/trust/<n>` in `shape`, with a `lib/` (a driver dylib at the top, a
        /// nested rlib, a symlink), `libexec/`, `share/` and `etc/`, and point
        /// `store/trust/current` at it.
        fn build(&self, n: u64, shape: Shape) -> PathBuf {
            let dir = self.layout.build_dir("trust", n);
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let frontend = |name: &str| format!("frontend {n} / code signature: {name:_<6}");
            let mut tools = vec!["targo", "trustdoc", "tippy-driver"];
            match shape {
                Shape::CopyNoTippy => {}
                Shape::CopyTargoTippyOnly => tools.push("targo-tippy"),
                _ => tools.extend(["tippy", "targo-tippy"]),
            }
            for tool in tools {
                std::fs::write(bin.join(tool), format!("{tool} of build {n}")).unwrap();
            }
            if shape != Shape::NoTrustc {
                std::fs::write(bin.join("trustc"), frontend("trustc")).unwrap();
            }
            match shape {
                Shape::NamesOnly => {}
                Shape::Linked => std::fs::hard_link(bin.join("trustc"), bin.join("rustc")).unwrap(),
                Shape::Identical => std::fs::copy(bin.join("trustc"), bin.join("rustc"))
                    .map(|_| ())
                    .unwrap(),
                _ => {
                    std::fs::write(bin.join("rustc"), frontend("rustc")).unwrap();
                    std::fs::write(bin.join("cargo"), format!("cargo copy of build {n}")).unwrap();
                }
            }
            let rustlib = dir
                .join("lib")
                .join("rustlib")
                .join("aarch64-apple-darwin")
                .join("lib");
            std::fs::create_dir_all(&rustlib).unwrap();
            std::fs::write(
                dir.join("lib").join("librustc_driver-5cfd.dylib"),
                format!("driver {n}"),
            )
            .unwrap();
            std::fs::write(rustlib.join("libstd.rlib"), format!("std {n}")).unwrap();
            std::os::unix::fs::symlink(
                "../../etc",
                dir.join("lib").join("rustlib").join("etc-link"),
            )
            .unwrap();
            for (sub, file) in [
                ("libexec", "trust-helper"),
                ("share", "README"),
                ("etc", "trust.toml"),
            ] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
                std::fs::write(dir.join(sub).join(file), format!("{file} {n}")).unwrap();
            }
            crate::activate::atomic_symlink(&dir, &self.layout.program_current("trust")).unwrap();
            dir
        }

        fn tool(&self, n: u64, tool: &str) -> PathBuf {
            self.layout.build_dir("trust", n).join("bin").join(tool)
        }

        fn shim_path(&self, name: &str) -> PathBuf {
            self.layout.shim(&ToolName::new(name).unwrap())
        }

        /// Write today's PLAIN shim at `bin/<name>` forwarding to `target`, as an older
        /// client laid it — directly, so no route is decided.
        fn plain_shim(&self, name: &str, target: &Path, env: &crate::shim_env::ShimEnv) -> PathBuf {
            let shim = self.shim_path(name);
            std::fs::write(&shim, crate::platform::sh_shim_content_env(target, env)).unwrap();
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
            shim
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Whether `at` is a clone of the store file `store` holding its bytes — not the
    /// store's own inode, which a hard link would be.
    fn cloned(store: &Path, at: &Path) -> bool {
        crate::clone::is_clone_of(store, at)
            && matches!(crate::clone::same_bytes(store, at), Ok(true))
    }

    fn ident(path: &Path) -> (u64, u64) {
        let m = std::fs::symlink_metadata(path).unwrap();
        (m.dev(), m.ino())
    }

    /// `(path, st_nlink, st_ctime, st_ctime_nsec, st_mtime, st_mtime_nsec)` of `dir` and
    /// everything under it, sorted — what a link, an unlink, a create or a chmod moves.
    fn stamps(dir: &Path) -> Vec<(PathBuf, u64, i64, i64, i64, i64)> {
        let stamp = |p: &Path| {
            let m = std::fs::symlink_metadata(p).unwrap();
            (
                p.to_path_buf(),
                m.nlink(),
                m.ctime(),
                m.ctime_nsec(),
                m.mtime(),
                m.mtime_nsec(),
            )
        };
        let mut out = vec![stamp(dir)];
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                    stack.push(path.clone());
                }
                out.push(stamp(&path));
            }
        }
        out.sort();
        out
    }

    /// Make `path` unreadable, answering the permissions it HAD — restore with
    /// [`restore_mode`], NEVER with a literal. A file `std::fs::write` lays carries
    /// `0o666 & !umask`: `0o644` under the `022` most shells set, `0o664` under the `002` a
    /// Debian-style per-user-group login gives, and this fleet runs both. A literal restore
    /// therefore does not put the file back — it CHANGES the store file — and a view whose
    /// entries are identified by length, permission bits and mtime
    /// ([`crate::clone::is_clone_of`]) is then correctly re-laid. That reads exactly like the
    /// destructive production defect these tests pin, from a fixture that never staged it.
    #[must_use]
    fn unreadable(path: &Path) -> std::fs::Permissions {
        let had = std::fs::symlink_metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        had
    }

    /// Put back exactly what [`unreadable`] measured.
    fn restore_mode(path: &Path, had: &std::fs::Permissions) {
        std::fs::set_permissions(path, had.clone()).unwrap();
    }

    fn debris(layout: &Layout) -> Vec<String> {
        std::fs::read_dir(roots_dir(layout))
            .map(|l| {
                l.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Exactly tippy's refusal condition: a second file under `rustc` that is neither
    /// `trustc`'s inode nor its bytes, beside a tippy that would read it. Every shape that
    /// passes tippy — or has no tippy, or no compiler — needs nothing.
    #[test]
    fn needs_root_answers_exactly_the_builds_tippy_refuses() {
        let fx = Fx::new("needs");
        let expect =
            |a: &str, b: &str| a.bytes().zip(b.bytes()).filter(|(x, y)| x != y).count() as u64;
        let copy = fx.build(8595, Shape::Copy);
        assert_eq!(
            needs_root(&copy).unwrap(),
            Some(expect(
                "frontend 8595 / code signature: rustc_",
                "frontend 8595 / code signature: trustc"
            ))
        );
        assert_eq!(
            needs_root(&fx.build(8590, Shape::CopyTargoTippyOnly))
                .unwrap()
                .map(|d| d > 0),
            Some(true)
        );
        for (n, shape, why) in [
            (1, Shape::NamesOnly, "Trust names only"),
            (2, Shape::Linked, "rustc IS trustc"),
            (3, Shape::Identical, "a byte-identical copy passes tippy"),
            (4, Shape::CopyNoTippy, "no tippy to refuse it"),
            (5, Shape::NoTrustc, "no trustc to compare with"),
        ] {
            assert_eq!(needs_root(&fx.build(n, shape)).unwrap(), None, "{why}");
        }
        // A copy of another length counts the tail; a symlinked rustc counts trustc whole.
        std::fs::write(
            copy.join("bin").join("rustc"),
            b"frontend 8595 / code signature: rustc_!!",
        )
        .unwrap();
        assert_eq!(
            needs_root(&copy).unwrap(),
            Some(expect("rustc_", "trustc") + 2)
        );
        std::fs::remove_file(copy.join("bin").join("rustc")).unwrap();
        std::os::unix::fs::symlink("trustc", copy.join("bin").join("rustc")).unwrap();
        let trustc_len = std::fs::metadata(copy.join("bin").join("trustc"))
            .unwrap()
            .len();
        assert_eq!(needs_root(&copy).unwrap(), Some(trustc_len));
    }

    /// THE ROOT: every regular file of the build's `bin/` is a clone of the store's, each
    /// stock name a clone of its Trust tool (so `rustc` holds `trustc`'s bytes, and never the
    /// bundle copy's), `lib/`, `libexec/`, `share/`, `etc/` cloned with the symlink recreated,
    /// and the marker written. Laying it moves NOTHING in the store — no `st_nlink`,
    /// `st_ctime` or `st_mtime` — which the hard-link construction could not say. And THE
    /// NO-CHURN RULE: once it stands, a second ensure at either depth is `Present` and moves
    /// nothing anywhere — in the store, in the root, or on `compat/trust` itself.
    #[test]
    fn ensure_root_clones_every_file_and_then_touches_nothing() {
        let fx = Fx::new("ensure");
        let build = fx.build(8595, Shape::Copy);
        let store_at_first = stamps(&build);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        assert_eq!(
            stamps(&build),
            store_at_first,
            "laying the root moved a store inode"
        );
        let root = root_dir(&fx.layout, 8595);
        let rbin = root.join("bin");
        for tool in [
            "trustc",
            "targo",
            "trustdoc",
            "tippy",
            "targo-tippy",
            "tippy-driver",
        ] {
            assert!(cloned(&fx.tool(8595, tool), &rbin.join(tool)), "{tool}");
        }
        for (public, trust) in crate::seam::STOCK_NAMES {
            assert!(
                cloned(&fx.tool(8595, trust), &rbin.join(public)),
                "{public} holds {trust}'s bytes"
            );
        }
        assert_ne!(
            std::fs::read(rbin.join("rustc")).unwrap(),
            std::fs::read(fx.tool(8595, "rustc")).unwrap(),
            "never the copy"
        );
        let libstd = |base: &Path| {
            base.join("lib")
                .join("rustlib")
                .join("aarch64-apple-darwin")
                .join("lib")
                .join("libstd.rlib")
        };
        assert!(cloned(&libstd(&build), &libstd(&root)));
        assert!(root_marker(&root).is_file(), "the marker is laid");
        assert_eq!(
            std::fs::read_link(root.join("lib").join("rustlib").join("etc-link")).unwrap(),
            Path::new("../../etc")
        );
        for sub in ["libexec", "share", "etc"] {
            assert!(
                std::fs::symlink_metadata(root.join(sub)).unwrap().is_dir(),
                "{sub}"
            );
        }
        assert!(root_matches(&build, &root, Depth::Deep));
        assert!(debris(&fx.layout).is_empty(), "{:?}", debris(&fx.layout));

        let store_before = stamps(&build);
        let compat_before = stamps(&compat_dir(&fx.layout));
        std::thread::sleep(std::time::Duration::from_millis(20));
        for depth in [Depth::Shallow, Depth::Deep] {
            assert_eq!(
                ensure_root(&fx.layout, &build, depth).unwrap(),
                Ensured::Present
            );
        }
        assert_eq!(stamps(&build), store_before, "a store inode moved");
        assert_eq!(
            stamps(&compat_dir(&fx.layout)),
            compat_before,
            "the root moved"
        );
        // A reconcile over shims that already route writes nothing either.
        let shim = fx.plain_shim(
            "tippy",
            &fx.tool(8595, "tippy"),
            &crate::shim_env::ShimEnv::NONE,
        );
        assert_eq!(
            reconcile(&fx.layout, Depth::Deep).routed,
            vec![shim.clone()]
        );
        let shim_before = stamps(&fx.layout.bin_dir());
        let store_before = stamps(&build);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!reconcile(&fx.layout, Depth::Deep).changed());
        assert_eq!(stamps(&fx.layout.bin_dir()), shim_before);
        assert_eq!(stamps(&build), store_before);
    }

    /// A root that stopped matching is REBUILT: a tool replaced by other bytes, or by the
    /// store's own inode (the hard link a root held before clones), at either depth; a root
    /// whose marker is gone; a file planted deep in `lib/` at `Deep` only (a shallow pass
    /// leaves it, and writes nothing). Dot debris a crashed run left is swept.
    #[test]
    fn a_mismatched_root_is_rebuilt_and_debris_is_swept() {
        let fx = Fx::new("rebuild");
        let build = fx.build(8595, Shape::Copy);
        ensure_root(&fx.layout, &build, Depth::Shallow).unwrap();
        let root = root_dir(&fx.layout, 8595);
        let tippy = root.join("bin").join("tippy");
        std::fs::remove_file(&tippy).unwrap();
        std::fs::write(&tippy, b"not the store's tippy").unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        assert!(cloned(&fx.tool(8595, "tippy"), &tippy));

        std::fs::remove_file(&tippy).unwrap();
        std::fs::hard_link(fx.tool(8595, "tippy"), &tippy).unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built,
            "a hard link to the store is not a clone of it"
        );
        assert!(cloned(&fx.tool(8595, "tippy"), &tippy));
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                std::fs::metadata(fx.tool(8595, "tippy")).unwrap().nlink(),
                1,
                "the store's link came back"
            );
        }

        std::fs::remove_file(root_marker(&root)).unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built,
            "an unmarked root is not a root"
        );
        assert!(root_marker(&root).is_file());

        let planted = root.join("lib").join("rustlib").join("planted.dylib");
        std::fs::write(&planted, b"not the store's").unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Present
        );
        assert!(planted.is_file());
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Deep).unwrap(),
            Ensured::Built
        );
        assert!(std::fs::symlink_metadata(&planted).is_err());
        assert!(debris(&fx.layout).is_empty());

        let dir = roots_dir(&fx.layout);
        std::fs::create_dir_all(dir.join(".8595.tmp-1").join("bin")).unwrap();
        std::fs::create_dir_all(dir.join(".8595.old-2")).unwrap();
        std::fs::write(dir.join(".DS_Store"), b"").unwrap();
        std::fs::write(dir.join("notes"), b"not atpkg's").unwrap();
        let swept = sweep(&fx.layout);
        assert_eq!(swept.swept.len(), 3, "{swept:?}");
        assert!(swept.errors.is_empty());
        assert!(debris(&fx.layout).is_empty());
        assert!(
            dir.join("notes").is_file(),
            "a name atpkg never lays is left alone"
        );
        assert!(root.is_dir(), "a root its build needs stays");

        // A symlink planted at the root's name is replaced by a real root, never followed.
        let elsewhere = fx.root.join("elsewhere");
        std::fs::rename(&root, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &root).unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        assert!(std::fs::symlink_metadata(&root).unwrap().is_dir());
        assert!(
            elsewhere.join("bin").join("tippy").is_file(),
            "the target is untouched"
        );
        assert!(debris(&fx.layout).is_empty());
    }

    /// A lay that fails part-way — an unreadable directory in `lib/`, standing in for the
    /// EXDEV a single-volume test cannot produce — returns the error, leaves no committed
    /// root and no temp, and every shim rendered afterwards is today's plain shim. And it
    /// fails BEFORE `bin/`: across two failing attempts (every launch and every pass retries)
    /// no store `bin/` inode was linked or unlinked, so a tippy running from the store is
    /// not aborted by a root that cannot be laid.
    #[test]
    fn a_failed_lay_commits_nothing_and_the_shims_stay_plain() {
        if crate::platform::our_uid() == 0 {
            return; // root reads a 0000 directory; the failure cannot be staged
        }
        let fx = Fx::new("layfail");
        let build = fx.build(8595, Shape::Copy);
        let locked = build.join("lib").join("rustlib").join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let bin_before = stamps(&build.join("bin"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        let err = ensure_root(&fx.layout, &build, Depth::Shallow);
        let again = ensure_root(&fx.layout, &build, Depth::Deep);
        let bin_after = stamps(&build.join("bin"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(again.is_err(), "{again:?}");
        // A lay that failed on its own (not behind a link) keeps the promise a retry can
        // keep, and carries no [`Blocked`].
        let err = err.unwrap_err();
        assert_eq!(blocked_by(&err), None, "{err}");
        let line = not_laid_line(&fx.layout, 8595, &err);
        assert!(
            line.starts_with(&format!(
                "atpkg: trust build 8595: exec root {} not put in order (",
                root_dir(&fx.layout, 8595).display()
            )) && line.ends_with("; `aterm pkg repair` retries"),
            "{line}"
        );
        assert_eq!(
            bin_after, bin_before,
            "a failing lay moved a store bin/ inode"
        );
        assert!(std::fs::symlink_metadata(root_dir(&fx.layout, 8595)).is_err());
        assert!(debris(&fx.layout).is_empty(), "{:?}", debris(&fx.layout));
        let target = fx.tool(8595, "tippy");
        let shim = fx.shim_path("tippy");
        assert_eq!(route_for_shim(&shim, &target), None);
        let none = crate::shim_env::ShimEnv::NONE;
        assert_eq!(
            crate::platform::shim_executable_to_env(&shim, &target, &none)
                .unwrap()
                .body,
            crate::platform::sh_shim_content_env(&target, &none).into_bytes()
        );
    }

    /// A build that needs no root keeps none: a leftover root directory is removed, a
    /// symlink planted at the name is unlinked and its target left whole, and a file
    /// planted there is removed.
    #[test]
    fn a_plain_build_retires_whatever_sits_at_its_root() {
        let fx = Fx::new("plain");
        let build = fx.build(8596, Shape::NamesOnly);
        let root = root_dir(&fx.layout, 8596);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Plain
        );
        assert!(std::fs::symlink_metadata(&root).is_err());
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("bin")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &root).unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Plain
        );
        assert!(std::fs::symlink_metadata(&root).is_err());
        assert!(elsewhere.join("bin").is_dir(), "never followed");
        std::fs::write(&root, b"x").unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Plain
        );
        assert!(std::fs::symlink_metadata(&root).is_err());
        assert!(debris(&fx.layout).is_empty());
        // A leftover root that will not delete (a `0500` directory holding a file): the name
        // is renamed aside FIRST, so the error comes back with the root already gone from
        // its name and `.<n>.old-<pid>` left for the sweep — never a half-deleted tree at
        // the name a shim routes by.
        if crate::platform::our_uid() != 0 {
            let stuck = root.join("lib");
            std::fs::create_dir_all(&stuck).unwrap();
            std::fs::write(stuck.join("held"), b"x").unwrap();
            std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o500)).unwrap();
            let refused = ensure_root(&fx.layout, &build, Depth::Shallow);
            let aside = format!(".8596.old-{}", std::process::id());
            let left = (
                std::fs::symlink_metadata(&root).is_err(),
                debris(&fx.layout),
            );
            let aside_lib = roots_dir(&fx.layout).join(&aside).join("lib");
            std::fs::set_permissions(&aside_lib, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(refused.is_err(), "{refused:?}");
            assert_eq!(left, (true, vec![aside]));
            let swept = sweep(&fx.layout);
            assert!(swept.errors.is_empty(), "{swept:?}");
            assert!(debris(&fx.layout).is_empty());
        }
        // Not a trust build of this layout: refused, nothing laid.
        assert!(ensure_root(&fx.layout, &fx.root.join("elsewhere"), Depth::Shallow).is_err());
        assert!(ensure_root(&fx.layout, &build.join("bin"), Depth::Shallow).is_err());
    }

    /// `route_for_shim` answers only for a shim of THIS prefix forwarding to a tool of a
    /// trust build whose root stands whole and marked, and only with a clone of that tool —
    /// never the store's own inode.
    #[test]
    fn route_for_shim_is_anchored_and_exact() {
        let fx = Fx::new("route");
        let build = fx.build(8595, Shape::Copy);
        ensure_root(&fx.layout, &build, Depth::Shallow).unwrap();
        let root = root_dir(&fx.layout, 8595);
        let target = fx.tool(8595, "tippy");
        let shim = fx.shim_path("tippy");
        assert_eq!(
            route_for_shim(&shim, &target),
            Some(root.join("bin").join("tippy"))
        );
        assert_eq!(
            route_for_shim(&fx.layout.prefix.join("agents").join("tippy"), &target),
            Some(root.join("bin").join("tippy")),
            "agents/ shares the prefix"
        );
        assert_eq!(
            route_for_shim(&fx.layout.bin_dir().join("rustc"), &fx.tool(8595, "rustc")),
            None,
            "the stock COPY is never routed: the root's rustc is trustc, not it"
        );

        // Another program with a root-shaped tree: no.
        let ay = fx.layout.build_dir("ay", 18).join("bin");
        std::fs::create_dir_all(&ay).unwrap();
        std::fs::write(ay.join("ay"), b"ay").unwrap();
        let ay_root = compat_dir(&fx.layout).join("ay").join("18").join("bin");
        std::fs::create_dir_all(&ay_root).unwrap();
        std::fs::hard_link(ay.join("ay"), ay_root.join("ay")).unwrap();
        assert_eq!(route_for_shim(&fx.shim_path("ay"), &ay.join("ay")), None);
        // A dev link into a checkout that merely has a store-shaped tail: no.
        let dev = fx
            .root
            .join("src")
            .join("store")
            .join("trust")
            .join("8595")
            .join("bin");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("tippy"), b"dev").unwrap();
        assert_eq!(route_for_shim(&shim, &dev.join("tippy")), None);
        // A lookalike prefix — the same target, a shim in another prefix: no.
        let other = fx.root.join("pkg2").join("bin").join("tippy");
        assert_eq!(route_for_shim(&other, &target), None);
        // Extra components, another directory of the build, a non-canonical number: no.
        assert_eq!(
            route_for_shim(&shim, &build.join("bin").join("sub").join("tippy")),
            None
        );
        assert_eq!(
            route_for_shim(&shim, &build.join("lib").join("librustc_driver-5cfd.dylib")),
            None
        );
        let spelled = fx
            .layout
            .prefix
            .join("store")
            .join("trust")
            .join("08595")
            .join("bin")
            .join("tippy");
        assert_eq!(route_for_shim(&shim, &spelled), None);

        // The root's rustc is not trustc's clone: no. Restored: yes.
        let rustc = root.join("bin").join("rustc");
        std::fs::remove_file(&rustc).unwrap();
        std::fs::write(&rustc, b"not trustc").unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::remove_file(&rustc).unwrap();
        crate::clone::clone_file(&fx.tool(8595, "trustc"), &rustc).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
        // The routed file with other bytes, a symlink, or the store's own inode: no.
        let routed = root.join("bin").join("tippy");
        std::fs::remove_file(&routed).unwrap();
        std::fs::write(&routed, b"not the store's tippy").unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::remove_file(&routed).unwrap();
        std::os::unix::fs::symlink(&target, &routed).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::remove_file(&routed).unwrap();
        std::fs::hard_link(&target, &routed).unwrap();
        assert_eq!(
            route_for_shim(&shim, &target),
            None,
            "the hard link a root held before clones"
        );
        std::fs::remove_file(&routed).unwrap();
        crate::clone::clone_file(&target, &routed).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
        // No marker, or a symlink at the marker's name: no.
        let marker = root_marker(&root);
        std::fs::remove_file(&marker).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::os::unix::fs::symlink(&target, &marker).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(&marker, ROOT_MARKER_BODY).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
        // No lib/ in a root whose build has one: no.
        let lib_aside = fx.root.join("lib-aside");
        std::fs::rename(root.join("lib"), &lib_aside).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::rename(&lib_aside, root.join("lib")).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
        // The root itself a symlink to a whole root elsewhere: no.
        let moved = fx.root.join("moved-root");
        std::fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
    }

    /// THE PASS THAT REACHES EVERY SHIM. Plain shims an older client laid for an affected
    /// build — a primary, an `alab-` alias, one exporting an environment — are routed in
    /// one reconcile, the environment kept; a tombstone, a pending stub and another
    /// program's shim are untouched; a second reconcile writes nothing. When `current`
    /// moves to a build that needs no root, its shims are today's plain shims byte for
    /// byte; and when the affected build is gone, its root goes with the next sweep.
    #[test]
    fn reconcile_routes_every_trust_shim_once_and_retires_with_the_build() {
        let fx = Fx::new("reconcile");
        fx.build(8595, Shape::Copy);
        let none = crate::shim_env::ShimEnv::NONE;
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let mut trust_shims = Vec::new();
        for (name, tool, e) in [
            ("trustc", "trustc", &none),
            ("targo", "targo", &env),
            ("tippy", "tippy", &none),
            ("alab-tippy", "tippy", &none),
        ] {
            trust_shims.push((
                fx.plain_shim(name, &fx.tool(8595, tool), e),
                tool,
                e.clone(),
            ));
        }
        crate::platform::install_tombstone_shim(
            &fx.shim_path("trustdoc"),
            "atpkg: trustdoc was yanked",
        )
        .unwrap();
        crate::stub::write_pending_stub(&fx.layout, &ToolName::new("ay").unwrap()).unwrap();
        let ny = fx.layout.build_dir("ny", 3).join("bin");
        std::fs::create_dir_all(&ny).unwrap();
        std::fs::write(ny.join("ny"), b"ny").unwrap();
        fx.plain_shim("ny", &ny.join("ny"), &none);
        let untouched: Vec<(PathBuf, Vec<u8>)> = ["trustdoc", "ay", "ny"]
            .iter()
            .map(|n| (fx.shim_path(n), std::fs::read(fx.shim_path(n)).unwrap()))
            .collect();

        let report = reconcile(&fx.layout, Depth::Shallow);
        assert_eq!(report.built, vec![8595]);
        assert!(report.errors.is_empty(), "{report:?}");
        let mut routed = report.routed.clone();
        routed.sort();
        let mut expected: Vec<PathBuf> = trust_shims.iter().map(|(s, _, _)| s.clone()).collect();
        expected.sort();
        assert_eq!(routed, expected);
        let root_bin = root_dir(&fx.layout, 8595).join("bin");
        for (shim, tool, e) in &trust_shims {
            assert_eq!(
                std::fs::read_to_string(shim).unwrap(),
                crate::platform::sh_shim_content_routed(
                    &fx.tool(8595, tool),
                    e,
                    Some(&root_bin.join(tool))
                ),
                "{}",
                shim.display()
            );
            assert_eq!(crate::platform::shim_env_of(shim), *e);
            assert_eq!(
                crate::platform::resolve_shim(shim),
                Some(fx.tool(8595, tool))
            );
        }
        for (shim, bytes) in &untouched {
            assert_eq!(&std::fs::read(shim).unwrap(), bytes, "{}", shim.display());
        }

        let bin_before = stamps(&fx.layout.bin_dir());
        std::thread::sleep(std::time::Duration::from_millis(20));
        let again = reconcile(&fx.layout, Depth::Deep);
        assert!(!again.changed(), "{again:?}");
        assert_eq!(stamps(&fx.layout.bin_dir()), bin_before);

        // `current` moves to a Trust-names-only build and an older client re-lays the shims.
        fx.build(8596, Shape::NamesOnly);
        for (name, tool, e) in [
            ("trustc", "trustc", &none),
            ("targo", "targo", &env),
            ("tippy", "tippy", &none),
            ("alab-tippy", "tippy", &none),
        ] {
            fx.plain_shim(name, &fx.tool(8596, tool), e);
        }
        let report = reconcile(&fx.layout, Depth::Shallow);
        assert!(!report.changed(), "{report:?}");
        assert!(
            std::fs::symlink_metadata(root_dir(&fx.layout, 8596)).is_err(),
            "no root for a plain build"
        );
        for (name, tool, e) in [
            ("trustc", "trustc", &none),
            ("targo", "targo", &env),
            ("tippy", "tippy", &none),
            ("alab-tippy", "tippy", &none),
        ] {
            assert_eq!(
                std::fs::read_to_string(fx.shim_path(name)).unwrap(),
                crate::platform::sh_shim_content_env(&fx.tool(8596, tool), e)
            );
        }
        // The rollback build still stands and still needs its root: kept.
        assert!(root_dir(&fx.layout, 8595).is_dir());
        // A render through the one shim writer, for the rollback build, routes again.
        let body = crate::platform::shim_executable_to_env(
            &fx.shim_path("tippy"),
            &fx.tool(8595, "tippy"),
            &none,
        )
        .unwrap()
        .body;
        assert!(String::from_utf8(body).unwrap().contains("[ ! -h "));
        // gc takes 8595; its root goes with the next sweep.
        std::fs::remove_dir_all(fx.layout.build_dir("trust", 8595)).unwrap();
        let report = reconcile(&fx.layout, Depth::Shallow);
        assert_eq!(report.swept, vec![root_dir(&fx.layout, 8595)]);
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(debris(&fx.layout).is_empty());
    }

    /// THE INVARIANCE TABLE. Every question the store answers from a shim gets the same
    /// answer from the plain and the routed form of every shim: `which`, `active_builds`,
    /// gc's `live_builds` (and so `shim_claims`), `pending_stub_exists`, `resolve_shim` and
    /// `shim_env_of`, the alias reconcile's "already right" (with the stale-shim prune
    /// behind it — neither writes a byte over either form), and doctor's report, broken-
    /// shim scan included — all but its (5f)(c) exec-root line, the one report line meant to
    /// differ (a warn over plain shims, a note once they route), and its free-space line,
    /// which reads the live volume: any build on the same disk between the two reports moved
    /// it by a tenth of a GiB and failed this test on unchanged code (a reviewer measured 7
    /// failures in 12 runs beside a writer, 2026-09-15).
    #[test]
    fn every_shim_reader_answers_the_same_for_the_routed_form() {
        let fx = Fx::new("invariance");
        let build = fx.build(8595, Shape::Copy);
        let none = crate::shim_env::ShimEnv::NONE;
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let tools = ["trustc", "targo", "tippy"];
        for tool in tools {
            let e = if tool == "targo" { &env } else { &none };
            fx.plain_shim(tool, &fx.tool(8595, tool), e);
            fx.plain_shim(&format!("alab-{tool}"), &fx.tool(8595, tool), e);
        }
        // A shim whose target is gone: doctor's broken-shim scan must name it either way.
        fx.plain_shim("tippy-driver", &fx.tool(8595, "tippy-driver-gone"), &none);
        crate::platform::install_tombstone_shim(
            &fx.shim_path("trustdoc"),
            "atpkg: trustdoc was yanked",
        )
        .unwrap();
        crate::stub::write_pending_stub(&fx.layout, &ToolName::new("ay").unwrap()).unwrap();
        let home = fx.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let names = [
            "trustc",
            "targo",
            "tippy",
            "alab-trustc",
            "alab-targo",
            "alab-tippy",
            "tippy-driver",
            "trustdoc",
            "ay",
        ];
        let tool_names: Vec<ToolName> = tools.iter().map(|t| ToolName::new(t).unwrap()).collect();

        let answers = || {
            let bin: Vec<(PathBuf, Vec<u8>)> = {
                let mut v: Vec<_> = std::fs::read_dir(fx.layout.bin_dir())
                    .unwrap()
                    .flatten()
                    .map(|e| (e.path(), std::fs::read(e.path()).unwrap()))
                    .collect();
                v.sort();
                v
            };
            crate::activate::reconcile_aliases(
                &fx.layout,
                &build,
                &tool_names,
                crate::activate::Aliases::Alab,
            )
            .unwrap();
            let mut after: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(fx.layout.bin_dir())
                .unwrap()
                .flatten()
                .map(|e| (e.path(), std::fs::read(e.path()).unwrap()))
                .collect();
            after.sort();
            let already_right = after == bin;
            let per_name: Vec<_> = names
                .iter()
                .map(|n| {
                    (
                        crate::ops::which(&fx.layout, n),
                        crate::stub::pending_stub_exists(&fx.layout, n),
                        crate::platform::resolve_shim(&fx.shim_path(n)),
                        crate::platform::shim_env_of(&fx.shim_path(n)),
                    )
                })
                .collect();
            // What the program exposes and which tools its build has live — the inputs
            // repair re-lays from and the rollback re-points with.
            let exposes = crate::ops::installed_exposes(&fx.layout, "trust").map(|mut v| {
                v.sort();
                v
            });
            let mut live_tools = crate::ops::active_tools(&fx.layout, "trust", 8595);
            live_tools.sort();
            let path = std::env::join_paths([fx.layout.bin_dir()]).unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let healthy = crate::doctor::run_with(
                &fx.layout,
                Some(&home),
                Some(&path),
                0,
                None,
                None,
                "doctor",
                &crate::doctor::Probes::default(),
                &mut out,
                &mut err,
            );
            // Doctor's (5f)(c) line is the one report line MEANT to differ — a warn over
            // plain shims, a note once they route — so it is held apart from the rest; the
            // free-space line reads the live volume, which other writers move, so it is
            // dropped from both.
            let (exec_root, report): (Vec<&str>, Vec<&str>) = std::str::from_utf8(&out)
                .unwrap()
                .lines()
                .filter(|line| {
                    let free = (line.starts_with("doctor: ok — ") && line.ends_with(" free"))
                        || line.starts_with("doctor: warn — only ")
                        || line == &"doctor: warn — could not query free space";
                    !free
                })
                .partition(|line| line.contains(" — trust build 8595: "));
            (
                already_right,
                per_name,
                (exposes, live_tools),
                crate::ops::active_builds(&fx.layout),
                crate::gc::live_builds(&fx.layout),
                healthy,
                report.join("\n"),
                String::from_utf8(err).unwrap(),
                exec_root.join("\n"),
            )
        };

        let plain = answers();
        assert!(plain.0, "the alias reconcile rewrote a plain shim");
        assert!(
            plain.7.contains("broken bin shim"),
            "the scan sees the gone target:\n{}",
            plain.7
        );
        // With no root standing, `exec_path` is `which` for every name.
        for n in names {
            assert_eq!(
                crate::ops::exec_path(&fx.layout, n),
                crate::ops::which(&fx.layout, n),
                "{n}"
            );
        }
        let report = reconcile(&fx.layout, Depth::Shallow);
        assert_eq!(report.routed.len(), 6, "{report:?}");
        assert!(
            std::fs::read_to_string(fx.shim_path("alab-targo"))
                .unwrap()
                .contains("[ ! -h ")
        );
        let routed = answers();
        assert!(routed.0, "the alias reconcile rewrote a routed shim");
        assert!(
            plain
                .8
                .starts_with("doctor: warn — trust build 8595: PATH tippy cannot run — ")
                && routed.8.starts_with("doctor: note — trust build 8595: ")
                && !plain.8.contains('\n')
                && !routed.8.contains('\n'),
            "one exec-root line each, a warn over plain shims and a note once they route:\n{}\n{}",
            plain.8,
            routed.8
        );
        assert_eq!(
            (
                &plain.0, &plain.1, &plain.2, &plain.3, &plain.4, &plain.5, &plain.6, &plain.7
            ),
            (
                &routed.0, &routed.1, &routed.2, &routed.3, &routed.4, &routed.5, &routed.6,
                &routed.7
            )
        );
        // The one reader that is MEANT to differ: what `atpkg run` and the reroute exec.
        let root_bin = root_dir(&fx.layout, 8595).join("bin");
        for n in names {
            let expected = match crate::ops::which(&fx.layout, n) {
                Some(t) if t.parent() == Some(build.join("bin").as_path()) && t.is_file() => {
                    Some(root_bin.join(t.file_name().unwrap()))
                }
                other => other,
            };
            assert_eq!(crate::ops::exec_path(&fx.layout, n), expected, "{n}");
        }
    }

    /// A ROOT LIVES EXACTLY AS LONG AS ITS BUILD, through the one function every discard
    /// path goes through. `store::discard_build` takes `<prefix>/compat/<program>/<n>`
    /// with the tree — renamed aside, no debris — and derives it from the build's own
    /// `store/<program>/<n>` chain only: a stage scratch sibling, a path outside any
    /// `store/`, a symlink planted at the root's name (unlinked, its target whole) and a
    /// `compat/` that is itself a symlink (not walked at all) all leave foreign trees alone.
    #[test]
    fn discard_build_takes_the_root_with_its_build_and_follows_nothing() {
        let fx = Fx::new("discard");
        let build = fx.build(8595, Shape::Copy);
        fx.build(8590, Shape::Copy);
        for n in [8590, 8595] {
            ensure_root(&fx.layout, &fx.layout.build_dir("trust", n), Depth::Shallow).unwrap();
        }
        let root = root_dir(&fx.layout, 8595);

        // Names that are not a build: nothing happens to the root.
        for not_a_build in [
            fx.layout
                .prefix
                .join("store")
                .join("trust")
                .join("8595.incoming-1"),
            fx.layout.prefix.join("store").join("trust").join("08595"),
            fx.root.join("elsewhere").join("trust").join("8595"),
            fx.layout.prefix.join("stash").join("trust").join("8595"),
        ] {
            discard_root_of(&not_a_build);
            assert!(root.is_dir(), "{}", not_a_build.display());
        }

        crate::store::discard_build(&build);
        assert!(std::fs::symlink_metadata(&build).is_err(), "the tree went");
        assert!(
            std::fs::symlink_metadata(&root).is_err(),
            "and its root with it"
        );
        assert!(debris(&fx.layout).is_empty(), "{:?}", debris(&fx.layout));
        assert!(
            root_dir(&fx.layout, 8590).is_dir(),
            "another build's root stays"
        );

        // A symlink planted at a root's name is unlinked; what it pointed at is whole.
        let elsewhere = fx.root.join("elsewhere-root");
        std::fs::create_dir_all(elsewhere.join("bin")).unwrap();
        std::fs::write(elsewhere.join("bin").join("keep"), b"not atpkg's").unwrap();
        let planted = root_dir(&fx.layout, 8589);
        std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();
        crate::store::discard_build(&fx.layout.build_dir("trust", 8589));
        assert!(std::fs::symlink_metadata(&planted).is_err());
        assert!(
            elsewhere.join("bin").join("keep").is_file(),
            "never followed"
        );

        // `compat/` itself a symlink to a tree shaped like roots: nothing under it goes.
        let lookalike = fx.root.join("lookalike");
        std::fs::create_dir_all(lookalike.join("trust").join("8590").join("bin")).unwrap();
        let compat = compat_dir(&fx.layout);
        std::fs::rename(&compat, fx.root.join("compat-aside")).unwrap();
        std::os::unix::fs::symlink(&lookalike, &compat).unwrap();
        crate::store::discard_build(&fx.layout.build_dir("trust", 8590));
        assert!(
            lookalike.join("trust").join("8590").join("bin").is_dir(),
            "a symlinked compat/ is not walked"
        );
    }

    /// DOCTOR'S HALF OF THE SWEEP. [`strays`] names exactly what [`sweep`] then removes —
    /// never dot debris, which may be a temp another atpkg is laying — and neither walks
    /// through a `compat` that is a symlink: what it names is not atpkg's tree, whatever
    /// sits beneath it. [`inspect`] answers `None` for a build that needs no root, and
    /// counts a shim whose target is gone as broken, not unrouted.
    #[test]
    fn strays_name_what_sweep_takes_and_neither_walks_a_symlinked_compat() {
        let fx = Fx::new("strays");
        let affected = fx.build(8595, Shape::Copy);
        fx.build(8600, Shape::NamesOnly);
        assert_eq!(
            inspect(&fx.layout, 8600),
            None,
            "no root needed, nothing said"
        );
        assert_eq!(inspect(&fx.layout, 1), None, "no build, nothing said");
        let none = crate::shim_env::ShimEnv::NONE;
        fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
        fx.plain_shim("tippy-driver", &fx.tool(8595, "tippy-driver-gone"), &none);
        let i = inspect(&fx.layout, 8595).unwrap().unwrap();
        assert_eq!(i.state, RootState::Absent);
        assert_eq!(
            (i.shims.clone(), i.unrouted.clone()),
            (vec!["tippy".to_string()], vec!["tippy".to_string()])
        );

        assert_eq!(
            ensure_root(&fx.layout, &affected, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        let dir = roots_dir(&fx.layout);
        std::fs::create_dir_all(dir.join("7")).unwrap();
        std::fs::create_dir_all(dir.join("8600")).unwrap();
        std::fs::create_dir_all(dir.join(".8595.tmp-1")).unwrap();
        std::fs::create_dir_all(dir.join("not-a-build")).unwrap();
        assert_eq!(
            strays(&fx.layout),
            vec![
                (dir.join("7"), Stray::BuildGone(7)),
                (dir.join("8600"), Stray::NeedsNone(8600)),
            ]
        );
        let mut swept = sweep(&fx.layout).swept;
        swept.sort();
        assert_eq!(
            swept,
            vec![dir.join(".8595.tmp-1"), dir.join("7"), dir.join("8600")]
        );
        assert!(strays(&fx.layout).is_empty());
        assert!(dir.join("8595").is_dir() && dir.join("not-a-build").is_dir());

        // `compat` itself a symlink to a tree holding a stray: not walked by either.
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("trust").join("7")).unwrap();
        std::fs::remove_dir_all(compat_dir(&fx.layout)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, compat_dir(&fx.layout)).unwrap();
        assert!(strays(&fx.layout).is_empty());
        assert_eq!(sweep(&fx.layout), Report::default());
        assert!(
            elsewhere.join("trust").join("7").is_dir(),
            "nothing followed"
        );
    }

    /// GC TAKES THE ROOTS IT SHOULD, AND ONLY THOSE. Over a live trust 8595 with rollback
    /// 8590 and a superseded 8589 — all three affected, all three rooted — `gc::run`
    /// reclaims 8589 and its root in the same discard; the live and rollback roots stand
    /// and the shims still route. An orphan root (no build), a symlink planted at a
    /// numeric name (unlinked, its target whole) and dot debris are swept and REPORTED;
    /// a second run reports nothing.
    #[test]
    fn gc_reclaims_a_superseded_root_and_sweeps_orphans_without_following_links() {
        let fx = Fx::new("gc");
        for n in [8589, 8590, 8595] {
            let dir = fx.build(n, Shape::Copy);
            crate::store::mark_build_ready(&dir).unwrap();
            ensure_root(&fx.layout, &dir, Depth::Shallow).unwrap();
        }
        let none = crate::shim_env::ShimEnv::NONE;
        let shim = fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
        assert_eq!(
            reconcile(&fx.layout, Depth::Shallow).routed,
            vec![shim.clone()]
        );

        let dir = roots_dir(&fx.layout);
        std::fs::create_dir_all(dir.join("9000").join("bin")).unwrap();
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("bin")).unwrap();
        std::fs::write(elsewhere.join("bin").join("keep"), b"not atpkg's").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("9001")).unwrap();
        std::fs::create_dir_all(dir.join(".8595.tmp-77")).unwrap();

        let report = crate::gc::run(&fx.layout);
        assert_eq!(report.reclaimed, vec![("trust".to_string(), vec![8589])]);
        assert!(std::fs::symlink_metadata(root_dir(&fx.layout, 8589)).is_err());
        for n in [8590, 8595] {
            assert!(root_dir(&fx.layout, n).is_dir(), "{n}'s root stands");
        }
        let mut swept = report.swept_exec_roots.clone();
        swept.sort();
        assert_eq!(
            swept,
            vec![dir.join(".8595.tmp-77"), dir.join("9000"), dir.join("9001")],
            "{report:?}"
        );
        assert!(report.exec_root_errors.is_empty(), "{report:?}");
        assert!(
            elsewhere.join("bin").join("keep").is_file(),
            "never followed"
        );
        assert!(debris(&fx.layout).is_empty(), "{:?}", debris(&fx.layout));
        assert!(
            route_for_shim(&shim, &fx.tool(8595, "tippy")).is_some(),
            "the live build still routes"
        );
        let again = crate::gc::run(&fx.layout);
        assert!(again.swept_exec_roots.is_empty() && again.reclaimed.is_empty());
    }

    /// UNINSTALL TAKES `compat/`. `ops::uninstall("trust")` removes the shims, the store
    /// tree and `compat/trust` (another program's `compat/` entry stays); `remove_all` —
    /// `uninstall --all`'s half — removes `compat/` whole, and unlinks a symlink planted at
    /// its name without following it.
    #[test]
    fn uninstall_removes_the_programs_roots_and_uninstall_all_removes_compat() {
        let fx = Fx::new("uninstall");
        let build = fx.build(8595, Shape::Copy);
        ensure_root(&fx.layout, &build, Depth::Shallow).unwrap();
        fx.plain_shim(
            "tippy",
            &fx.tool(8595, "tippy"),
            &crate::shim_env::ShimEnv::NONE,
        );
        let other = compat_dir(&fx.layout).join("ny");
        std::fs::create_dir_all(&other).unwrap();

        crate::ops::uninstall(&fx.layout, "trust").unwrap();
        assert!(std::fs::symlink_metadata(fx.shim_path("tippy")).is_err());
        assert!(std::fs::symlink_metadata(fx.layout.prefix.join("store").join("trust")).is_err());
        assert!(std::fs::symlink_metadata(roots_dir(&fx.layout)).is_err());
        assert!(other.is_dir(), "only the uninstalled program's roots");

        remove_all(&fx.layout).unwrap();
        assert!(std::fs::symlink_metadata(compat_dir(&fx.layout)).is_err());
        remove_all(&fx.layout).unwrap();

        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("trust").join("8595")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, compat_dir(&fx.layout)).unwrap();
        remove_program(&fx.layout, "trust").unwrap();
        assert!(
            elsewhere.join("trust").join("8595").is_dir(),
            "a symlinked compat/ is not walked by the per-program removal"
        );
        remove_all(&fx.layout).unwrap();
        assert!(std::fs::symlink_metadata(compat_dir(&fx.layout)).is_err());
        assert!(
            elsewhere.join("trust").join("8595").is_dir(),
            "never followed"
        );
    }

    /// `ops::exec_path` — what `atpkg run` and the `clippy` reroute exec — is the root's
    /// file exactly when [`route_for_shim`] answers for the shim: the store path with no
    /// root, the root's clone once one stands, the store path again when the root's file
    /// holds other bytes or its marker is gone, and the store path for a shim of another
    /// program or a name with no shim at all.
    #[test]
    fn exec_path_takes_the_root_only_when_the_shim_routes() {
        let fx = Fx::new("exec-path");
        let build = fx.build(8595, Shape::Copy);
        let none = crate::shim_env::ShimEnv::NONE;
        fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
        let ny = fx.layout.build_dir("ny", 3).join("bin");
        std::fs::create_dir_all(&ny).unwrap();
        std::fs::write(ny.join("ny"), b"ny").unwrap();
        fx.plain_shim("ny", &ny.join("ny"), &none);

        assert_eq!(
            crate::ops::exec_path(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy"))
        );
        ensure_root(&fx.layout, &build, Depth::Shallow).unwrap();
        let routed = root_dir(&fx.layout, 8595).join("bin").join("tippy");
        assert_eq!(
            crate::ops::exec_path(&fx.layout, "tippy"),
            Some(routed.clone())
        );
        assert_eq!(
            crate::ops::which(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy")),
            "which keeps answering the store"
        );
        assert_eq!(crate::ops::exec_path(&fx.layout, "ny"), Some(ny.join("ny")));
        assert_eq!(crate::ops::exec_path(&fx.layout, "absent"), None);
        assert_eq!(crate::ops::exec_path(&fx.layout, "../tippy"), None);

        let root = root_dir(&fx.layout, 8595);
        std::fs::remove_file(root_marker(&root)).unwrap();
        assert_eq!(
            crate::ops::exec_path(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy")),
            "an unmarked root is never exec'd"
        );
        std::fs::write(root_marker(&root), ROOT_MARKER_BODY).unwrap();
        std::fs::remove_file(&routed).unwrap();
        std::fs::write(&routed, b"not the store's tippy").unwrap();
        assert_eq!(
            crate::ops::exec_path(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy")),
            "other bytes in the root are never exec'd"
        );
    }

    /// THE SHIM WRITERS LAY THE ROOT FIRST. `activate::install_tools` on an affected build
    /// lays `compat/trust/<n>` before it renders, so the shims it writes route on the first
    /// lay — no reconcile needed; the alias reconcile re-lays a plain `alab-` alias an older
    /// client left (target and environment already right, bytes not), then writes nothing
    /// on a second run. On a build that needs no root, `install_tools` puts NOTHING on disk
    /// under `compat/` and lays today's shims byte for byte.
    #[test]
    fn install_lays_the_root_before_the_shims_and_the_alias_reconcile_routes_old_aliases() {
        let fx = Fx::new("install");
        let build = fx.build(8595, Shape::Copy);
        let tools: Vec<ToolName> = ["trustc", "targo", "tippy"]
            .iter()
            .map(|t| ToolName::new(t).unwrap())
            .collect();
        crate::activate::install_tools(&fx.layout, &build, &tools, crate::activate::Aliases::Off)
            .unwrap();
        let root_bin = root_dir(&fx.layout, 8595).join("bin");
        let none = crate::shim_env::ShimEnv::NONE;
        for t in ["trustc", "targo", "tippy"] {
            assert_eq!(
                std::fs::read_to_string(fx.shim_path(t)).unwrap(),
                crate::platform::sh_shim_content_routed(
                    &fx.tool(8595, t),
                    &none,
                    Some(&root_bin.join(t))
                ),
                "{t}"
            );
        }
        // An older client's plain alias beside the routed primary.
        let alias = fx.plain_shim("alab-tippy", &fx.tool(8595, "tippy"), &none);
        crate::activate::reconcile_aliases(
            &fx.layout,
            &build,
            &tools,
            crate::activate::Aliases::Alab,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&alias).unwrap(),
            crate::platform::sh_shim_content_routed(
                &fx.tool(8595, "tippy"),
                &none,
                Some(&root_bin.join("tippy"))
            )
        );
        let before = stamps(&fx.layout.bin_dir());
        let store_before = stamps(&build);
        std::thread::sleep(std::time::Duration::from_millis(20));
        crate::activate::reconcile_aliases(
            &fx.layout,
            &build,
            &tools,
            crate::activate::Aliases::Alab,
        )
        .unwrap();
        assert_eq!(
            stamps(&fx.layout.bin_dir()),
            before,
            "a second run writes nothing"
        );
        assert_eq!(stamps(&build), store_before);

        // A Trust-names-only build: nothing under compat/, today's shims exactly.
        let fresh = Fx::new("install-plain");
        let plain = fresh.build(8596, Shape::NamesOnly);
        crate::activate::install_tools(
            &fresh.layout,
            &plain,
            &tools,
            crate::activate::Aliases::Alab,
        )
        .unwrap();
        assert!(std::fs::symlink_metadata(compat_dir(&fresh.layout)).is_err());
        for t in [
            "trustc",
            "targo",
            "tippy",
            "alab-trustc",
            "alab-targo",
            "alab-tippy",
        ] {
            let tool = t.trim_start_matches("alab-");
            assert_eq!(
                std::fs::read_to_string(fresh.shim_path(t)).unwrap(),
                crate::platform::sh_shim_content_env(&fresh.tool(8596, tool), &none),
                "{t}"
            );
        }
    }

    /// A root that cannot be laid costs the install nothing: `install_tools` still lays
    /// every shim (plain, running the store path as before), returns `Ok`, and leaves no
    /// committed root and no temp — the pass's reconcile then reports the refusal.
    #[test]
    fn install_over_a_root_that_cannot_be_laid_lays_plain_shims() {
        if crate::platform::our_uid() == 0 {
            return; // root reads a 0000 directory; the failure cannot be staged
        }
        let fx = Fx::new("install-fail");
        let build = fx.build(8595, Shape::Copy);
        let locked = build.join("lib").join("rustlib").join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let tools = [ToolName::new("tippy").unwrap()];
        let laid = crate::activate::install_tools(
            &fx.layout,
            &build,
            &tools,
            crate::activate::Aliases::Off,
        );
        let report = reconcile(&fx.layout, Depth::Shallow);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        laid.unwrap();
        assert_eq!(
            std::fs::read_to_string(fx.shim_path("tippy")).unwrap(),
            crate::platform::sh_shim_content_env(
                &fx.tool(8595, "tippy"),
                &crate::shim_env::ShimEnv::NONE
            )
        );
        assert!(std::fs::symlink_metadata(root_dir(&fx.layout, 8595)).is_err());
        assert!(debris(&fx.layout).is_empty(), "{:?}", debris(&fx.layout));
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert!(report.errors[0].contains("trust build 8595"), "{report:?}");
        assert!(report.routed.is_empty(), "{report:?}");
    }

    /// A READ THAT FAILS PROVES NOTHING. With the store `bin/rustc` unreadable under a
    /// standing, routed root, [`needs_root`] is an error — not "needs none" — and nothing
    /// acts on it destructively: the reconcile at either depth keeps the root, re-lays no
    /// shim, moves no stamp in the store, the root or `bin/`, and names the failed read;
    /// the sweep and doctor's strays keep the root; [`ensure_root`] refuses and leaves it;
    /// [`inspect`] answers `Err`; the shim still routes. Readable again, a reconcile finds
    /// nothing to do. The failure mode this pins swept the root, un-routed every shim and
    /// moved every store inode twice (a reviewer's probe, 2026-09-15).
    #[test]
    fn a_build_that_cannot_be_read_keeps_its_root_and_its_routes() {
        if crate::platform::our_uid() == 0 {
            return; // root reads a 0000 file; the failure cannot be staged
        }
        let fx = Fx::new("unread");
        let build = fx.build(8595, Shape::Copy);
        let none = crate::shim_env::ShimEnv::NONE;
        let shim = fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
        let laid = reconcile(&fx.layout, Depth::Deep);
        assert_eq!(
            (laid.built.clone(), laid.routed.clone()),
            (vec![8595], vec![shim.clone()])
        );
        let rustc = build.join("bin").join("rustc");
        let rustc_mode = unreadable(&rustc);
        assert!(needs_root(&build).is_err(), "{:?}", needs_root(&build));
        let before = (
            stamps(&build),
            stamps(&compat_dir(&fx.layout)),
            stamps(&fx.layout.bin_dir()),
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        for depth in [Depth::Shallow, Depth::Deep] {
            let report = reconcile(&fx.layout, depth);
            assert!(
                report.built.is_empty() && report.routed.is_empty() && report.swept.is_empty(),
                "{report:?}"
            );
            assert_eq!(report.errors.len(), 1, "{report:?}");
            assert!(
                report.errors[0].contains("cannot tell whether trust build 8595"),
                "{report:?}"
            );
        }
        assert!(sweep(&fx.layout).swept.is_empty());
        assert!(strays(&fx.layout).is_empty());
        assert!(ensure_root(&fx.layout, &build, Depth::Shallow).is_err());
        assert!(matches!(inspect(&fx.layout, 8595), Some(Err(_))));
        let after = (
            stamps(&build),
            stamps(&compat_dir(&fx.layout)),
            stamps(&fx.layout.bin_dir()),
        );
        restore_mode(&rustc, &rustc_mode);
        assert_eq!(after, before, "an unproven answer moved something");
        assert!(route_for_shim(&shim, &fx.tool(8595, "tippy")).is_some());
        let healed = reconcile(&fx.layout, Depth::Deep);
        assert!(!healed.changed(), "{healed:?}");
        // The error arm of `needs_root` itself: a `bin/trustc` that cannot be opened.
        let trustc = build.join("bin").join("trustc");
        let trustc_mode = unreadable(&trustc);
        let read = needs_root(&build);
        restore_mode(&trustc, &trustc_mode);
        assert!(read.is_err(), "{read:?}");
        // THE FIXTURE PROVES ITS OWN PRECONDITION. Both chmod round-trips above must leave
        // the store exactly as they found it: the view's `bin/rustc` and `bin/trustc` are
        // clones of the store's `bin/trustc`, and a clone is identified by length,
        // permission bits and mtime, so a restore that invented a mode instead of measuring
        // one leaves the root CORRECTLY stale — and every `!changed()` below would then read
        // as the destructive defect this test pins rather than as a broken fixture.
        assert_eq!(
            root_first_mismatch(&build, &root_dir(&fx.layout, 8595), Depth::Deep),
            None,
            "the fixture's chmod round-trip, not a reconcile, moved the store"
        );
        // A `store/trust` that cannot be searched: the build directory's own `lstat` fails,
        // which is no more proof that the build is GONE than a failed read is proof that it
        // needs none. The sweep and strays keep the root, inspect answers `Err`, nothing
        // under `compat/` moves, and once searchable again nothing is re-laid (the sweep
        // had classified the root `BuildGone`, removed it, and re-laid every store `bin/`
        // inode on the next pass — a reviewer's probe, 2026-09-15).
        let programs = build.parent().unwrap().to_path_buf();
        let mode = std::fs::symlink_metadata(&programs).unwrap().permissions();
        let before = stamps(&compat_dir(&fx.layout));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::set_permissions(&programs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let seen = strays(&fx.layout);
        let swept = sweep(&fx.layout);
        let inspected = inspect(&fx.layout, 8595);
        std::fs::set_permissions(&programs, mode).unwrap();
        assert!(seen.is_empty(), "{seen:?}");
        assert!(
            swept.swept.is_empty() && swept.errors.is_empty(),
            "{swept:?}"
        );
        assert!(matches!(inspected, Some(Err(_))), "{inspected:?}");
        assert_eq!(
            stamps(&compat_dir(&fx.layout)),
            before,
            "an unproven answer moved something"
        );
        let healed = reconcile(&fx.layout, Depth::Deep);
        assert!(!healed.changed(), "{healed:?}");
    }

    /// NOTHING UNDER A LINKED `compat` IS ATPKG'S. With `<prefix>/compat`, or
    /// `<prefix>/compat/trust`, a symlink to a directory that holds a root-named tree:
    ///
    /// * a build that needs no root answers `Plain` from [`ensure_root`] and from a
    ///   [`reconcile`] and removes NOTHING behind the link (the removal had followed it out
    ///   of the prefix — a reviewer's probe, 2026-09-15);
    /// * an affected build whose root stands whole behind the link is REFUSED — not
    ///   `Present` — and the reconcile says so, because [`route_for_shim`] (which requires
    ///   both to be real directories) answers `None` through it and [`inspect`] reads it as
    ///   [`RootState::Blocked`]; every refusal names the link and the manual fix, and the
    ///   tree behind it is left whole;
    /// * a plain FILE at `compat` is blocked the same way, worded as a file.
    #[test]
    fn nothing_is_removed_or_trusted_behind_a_linked_compat() {
        let none = crate::shim_env::ShimEnv::NONE;
        for linked in [COMPAT_DIR, "compat/trust"] {
            let fx = Fx::new("linked-plain");
            let build = fx.build(8596, Shape::NamesOnly);
            fx.plain_shim("tippy", &fx.tool(8596, "tippy"), &none);
            let elsewhere = fx.root.join("elsewhere");
            let victim = if linked == COMPAT_DIR {
                elsewhere.join("trust").join("8596")
            } else {
                elsewhere.join("8596")
            };
            std::fs::create_dir_all(&victim).unwrap();
            std::fs::write(victim.join("keep"), b"not atpkg's").unwrap();
            std::fs::create_dir_all(compat_dir(&fx.layout)).unwrap();
            let at = fx.layout.prefix.join(linked);
            let _ = std::fs::remove_dir(&at);
            std::os::unix::fs::symlink(&elsewhere, &at).unwrap();
            assert_eq!(
                ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
                Ensured::Plain,
                "{linked}"
            );
            assert!(
                victim.join("keep").is_file(),
                "{linked}: ensure_root followed"
            );
            let report = reconcile(&fx.layout, Depth::Deep);
            assert!(!report.changed(), "{linked}: {report:?}");
            assert!(
                victim.join("keep").is_file(),
                "{linked}: reconcile followed"
            );
        }

        for linked in [COMPAT_DIR, "compat/trust"] {
            let fx = Fx::new("linked-affected");
            let build = fx.build(8595, Shape::Copy);
            let shim = fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
            assert_eq!(reconcile(&fx.layout, Depth::Shallow).built, vec![8595]);
            let target = fx.tool(8595, "tippy");
            assert!(route_for_shim(&shim, &target).is_some());
            let at = fx.layout.prefix.join(linked);
            let aside = fx.root.join("aside");
            std::fs::rename(&at, &aside).unwrap();
            std::os::unix::fs::symlink(&aside, &at).unwrap();
            assert!(
                root_matches(&build, &root_dir(&fx.layout, 8595), Depth::Deep),
                "{linked}: fixture — a whole root is reachable through the link"
            );
            assert_eq!(route_for_shim(&shim, &target), None, "{linked}");
            let expected = Blocked {
                at: at.clone(),
                symlink: true,
                root: root_dir(&fx.layout, 8595),
            };
            assert_eq!(blocked(&fx.layout, 8595).as_ref(), Some(&expected));
            assert_eq!(
                inspect(&fx.layout, 8595).unwrap().unwrap().state,
                RootState::Blocked(expected.clone()),
                "{linked}"
            );
            // The refusal names the link and the manual fix — never "update directory is a
            // symlink; refusing" beside a promise that `aterm pkg repair` retries.
            let refused = ensure_root(&fx.layout, &build, Depth::Shallow).unwrap_err();
            assert_eq!(blocked_by(&refused), Some(&expected), "{linked}");
            let what = format!(
                "{} is a symbolic link, not a directory — atpkg lays nothing through a link \
                 there, and no update, repair or gc removes it, so no exec root is laid at {} \
                 and no shim routes through one",
                at.display(),
                root_dir(&fx.layout, 8595).display()
            );
            let fix = "fix: remove that link (only the link: whatever it names is left as it \
                       is), then `aterm pkg repair` lays the exec root";
            assert_eq!(refused.to_string(), format!("{what}; {fix}"), "{linked}");
            let line = not_laid_line(&fx.layout, 8595, &refused);
            assert_eq!(
                line,
                format!(
                    "atpkg: trust build 8595: {what} — this build's trust shims run the store \
                     path, where its tippy refuses to start; {fix}"
                ),
                "{linked}"
            );
            assert!(!line.contains("retries"), "{line}");
            let report = reconcile(&fx.layout, Depth::Deep);
            assert!(report.built.is_empty(), "{linked}: {report:?}");
            assert_eq!(
                report.errors,
                vec![format!(
                    "trust build 8595: {what} — its trust shims render plain and run the store \
                     path; {fix}"
                )],
                "{linked}: {report:?}"
            );
            let behind = if linked == COMPAT_DIR {
                aside.join("trust").join("8595")
            } else {
                aside.join("8595")
            };
            assert!(
                behind.join("bin").join("tippy").is_file(),
                "{linked}: the tree behind the link is whole"
            );
        }

        let fx = Fx::new("file-compat");
        let build = fx.build(8595, Shape::Copy);
        std::fs::write(compat_dir(&fx.layout), b"not a directory").unwrap();
        let refused = ensure_root(&fx.layout, &build, Depth::Shallow).unwrap_err();
        let expected = Blocked {
            at: compat_dir(&fx.layout),
            symlink: false,
            root: root_dir(&fx.layout, 8595),
        };
        assert_eq!(blocked_by(&refused), Some(&expected));
        assert_eq!(
            refused.to_string(),
            format!(
                "{} is not a directory — no update, repair or gc removes it, so no exec root is \
                 laid at {} and no shim routes through one; fix: move that file out of the way, \
                 then `aterm pkg repair` lays the exec root",
                compat_dir(&fx.layout).display(),
                root_dir(&fx.layout, 8595).display()
            )
        );
        assert!(
            compat_dir(&fx.layout).is_file(),
            "the file is left where it was"
        );
    }

    /// A STOCK-NAME COPY IS NOT AN UNROUTED SHIM. A build that ships `bin/rustdoc` as a copy
    /// beside `trustdoc`, exposed as a shim: the root presents `rustdoc` as `trustdoc`'s
    /// inode, so the writer never routes that shim, and doctor's [`inspect`] does not count
    /// it — the reconcile routes `tippy`, a second reconcile writes nothing, and the root
    /// reads as whole with nothing unrouted (the count had kept a warn `repair` could
    /// never clear).
    #[test]
    fn a_stock_name_copy_is_neither_routed_nor_counted_unrouted() {
        let fx = Fx::new("stock-copy");
        let build = fx.build(8595, Shape::Copy);
        std::fs::write(build.join("bin").join("rustdoc"), b"rustdoc copy of 8595").unwrap();
        let none = crate::shim_env::ShimEnv::NONE;
        let tippy = fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
        let rustdoc = fx.plain_shim("rustdoc", &fx.tool(8595, "rustdoc"), &none);
        let rustdoc_bytes = std::fs::read(&rustdoc).unwrap();
        assert_eq!(reconcile(&fx.layout, Depth::Deep).routed, vec![tippy]);
        assert!(!reconcile(&fx.layout, Depth::Deep).changed());
        assert_eq!(std::fs::read(&rustdoc).unwrap(), rustdoc_bytes);
        let i = inspect(&fx.layout, 8595).unwrap().unwrap();
        assert_eq!(i.state, RootState::Matches);
        assert_eq!(
            (i.shims, i.unrouted),
            (vec!["tippy".to_string()], Vec::<String>::new())
        );

        // A stock name that is a HARD LINK of its Trust tool is that tool's inode, which
        // the root presents under the stock name — so the writer routes its shim, and a
        // plain one is unrouted until a pass re-renders it. Skipped by name alone, doctor
        // read healthy while the next reconcile still routed it (a reviewer's probe,
        // 2026-09-15).
        let fx = Fx::new("stock-link");
        let build = fx.build(8595, Shape::Copy);
        let bin = build.join("bin");
        std::fs::hard_link(bin.join("trustdoc"), bin.join("rustdoc")).unwrap();
        let tippy = fx.plain_shim("tippy", &fx.tool(8595, "tippy"), &none);
        let rustdoc = fx.plain_shim("rustdoc", &fx.tool(8595, "rustdoc"), &none);
        let rustdoc_plain = std::fs::read(&rustdoc).unwrap();
        assert_eq!(
            reconcile(&fx.layout, Depth::Deep).routed,
            vec![rustdoc.clone(), tippy]
        );
        std::fs::write(&rustdoc, &rustdoc_plain).unwrap();
        let i = inspect(&fx.layout, 8595).unwrap().unwrap();
        assert_eq!(i.state, RootState::Matches);
        assert_eq!(
            (i.shims, i.unrouted),
            (
                vec!["rustdoc".to_string(), "tippy".to_string()],
                vec!["rustdoc".to_string()]
            )
        );
        assert_eq!(reconcile(&fx.layout, Depth::Deep).routed, vec![rustdoc]);
        let i = inspect(&fx.layout, 8595).unwrap().unwrap();
        assert!(i.unrouted.is_empty(), "{:?}", i.unrouted);
    }

    /// A REBUILD BESIDE `bin/` MOVES NO `bin/` INODE. When the standing root's `bin/` still
    /// matches the build and only a directory beside it differs — a `.DS_Store` under
    /// `share/` at `Deep`, a driver dylib at the top of `lib/` replaced by other bytes at
    /// `Shallow` — the root is rebuilt (`Built`, the stray gone, the dylib a clone of the
    /// store's again), its live `bin/` directory is MOVED into the new tree (the same
    /// directory inode, so a tippy running from the root keeps its files), and no store
    /// `bin/` file's link count or ctime moved. A mismatch inside `bin/` itself still lays a
    /// fresh `bin/`.
    #[test]
    fn a_rebuild_beside_bin_moves_no_bin_inode() {
        let fx = Fx::new("carry-bin");
        let build = fx.build(8595, Shape::Copy);
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        let root = root_dir(&fx.layout, 8595);
        let root_bin = ident(&root.join("bin"));
        let store_bin = stamps(&build.join("bin"));
        std::thread::sleep(std::time::Duration::from_millis(20));

        let ds_store = root.join("share").join(".DS_Store");
        std::fs::write(&ds_store, b"Finder").unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Present,
            "shallow asks nothing of share/"
        );
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Deep).unwrap(),
            Ensured::Built
        );
        assert!(std::fs::symlink_metadata(&ds_store).is_err());

        let driver = root.join("lib").join("librustc_driver-5cfd.dylib");
        std::fs::remove_file(&driver).unwrap();
        std::fs::write(&driver, b"not the store's driver").unwrap();
        assert!(
            !root_matches(&build, &root, Depth::Shallow),
            "shallow reads the driver dylibs at the top of lib/"
        );
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        assert!(cloned(
            &build.join("lib").join("librustc_driver-5cfd.dylib"),
            &driver
        ));

        assert!(root_matches(&build, &root, Depth::Deep));
        assert!(debris(&fx.layout).is_empty(), "{:?}", debris(&fx.layout));
        assert_eq!(
            ident(&root.join("bin")),
            root_bin,
            "bin/ was moved, not laid"
        );
        assert_eq!(
            stamps(&build.join("bin")),
            store_bin,
            "a store bin/ inode moved"
        );

        // Control: a mismatch inside bin/ lays bin/ afresh.
        std::fs::write(root.join("bin").join("stray"), b"x").unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        assert_ne!(ident(&root.join("bin")), root_bin);
        assert!(root_matches(&build, &root, Depth::Deep));
    }
}
