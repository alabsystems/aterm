// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-build exec roots for the Trust bundles whose own tippy refuses them.

use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::seam::Depth;

/// `<prefix>/compat`, where exec roots live.
pub use aterm_toolchain::layout::COMPAT_DIR;

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

/// `<prefix>/compat/trust/<build>`.
#[must_use]
pub fn root_dir(layout: &Layout, build: u64) -> PathBuf {
    roots_dir(layout).join(build.to_string())
}

/// The file written last into a root before `rename(2)` commits it.
pub const ROOT_MARKER: &str = ".atpkg-root";

/// The marker's contents: a version line, so a future construction can tell its own roots
/// from these.
#[cfg(unix)]
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

/// The first path at which the root at `root` stops being `build_dir` at `depth`.
#[must_use]
#[cfg(unix)]
pub fn root_first_mismatch(build_dir: &Path, root: &Path, depth: Depth) -> Option<PathBuf> {
    if crate::seam::is_real_dir(root) && !marker_stands(root) {
        return Some(root_marker(root));
    }
    crate::seam::first_mismatch(build_dir, root, depth)
}

/// Whether the root at `root` is `build_dir` at `depth` ([`root_first_mismatch`] found nothing).
#[must_use]
#[cfg(unix)]
pub fn root_matches(build_dir: &Path, root: &Path, depth: Depth) -> bool {
    root_first_mismatch(build_dir, root, depth).is_none()
}

/// The one line every lane prints when it lays or rebuilds the exec root of trust build `build`.
#[must_use]
pub fn laid_line(layout: &Layout, build: u64) -> String {
    format!(
        "atpkg: Trust build {build}: tippy now runs from {}",
        root_dir(layout, build).display()
    )
}

/// `<prefix>/compat` or `<prefix>/compat/trust` standing as something other than a real directory (a symlink or a file). atpkg never follows or writes through it (a removal could follow a link out of the prefix) and only `uninstall` removes `compat`.
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
    pub const fn fix(&self) -> &'static str {
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

/// The [`Blocked`] in the way of trust build `build`'s exec root, by `lstat` of `<prefix>/compat` and then `<prefix>/compat/trust`.
#[must_use]
#[cfg(unix)]
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

/// The line the lanes that lay a root beside their own shims (`activate::install_tools_env`, `flow::rollback_member`) print when [`ensure_root`] fails for trust build `build`.
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
            "atpkg: trust build {build}: exec root {} not put in order ({e}); a shim without a \
             whole root runs the store path, where tippy refuses to start; `aterm pkg repair` \
             retries",
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

/// What a [`reconcile`] (or a bare [`sweep`]) did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Builds whose root was laid or replaced.
    pub built: Vec<u64>,
    /// Shims re-laid because their bytes differed from the render.
    pub routed: Vec<PathBuf>,
    /// Roots and debris removed.
    pub swept: Vec<PathBuf>,
    /// What could not be done, one sentence each; the shims involved run the store path.
    pub errors: Vec<String>,
}

impl Report {
    /// Whether anything was written or refused.
    #[cfg(all(test, unix))]
    #[must_use]
    pub(crate) fn changed(&self) -> bool {
        !(self.built.is_empty()
            && self.routed.is_empty()
            && self.swept.is_empty()
            && self.errors.is_empty())
    }
}

/// Whether the build at `build_dir` needs an exec root.
/// # Errors
/// A read that fails (an `lstat` refused for any reason but absence, a file that will not open or read) is an error, never `Ok(None)`: callers remove a root only on `Ok(None)` ([`ensure_root`] leaves it, [`sweep`] keeps it), so one unreadable `bin/rustc` cannot retire a needed root.
pub fn needs_root(build_dir: &Path) -> io::Result<Option<u64>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let bin = build_dir.join("bin");
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
/// difference.
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

/// The trust build number `build_dir` is, when it is exactly `<prefix>/store/trust/<n>` of this layout.
pub(crate) fn trust_build_of(layout: &Layout, build_dir: &Path) -> Option<u64> {
    let (program, n) = crate::ops::store_build_of(&layout.prefix, build_dir)?;
    (program == crate::seam::SEAM_PROGRAM && build_dir == layout.build_dir(&program, n))
        .then_some(n)
}

fn trust_tool_build(layout: &Layout, target: &Path) -> Option<u64> {
    let (program, n) = crate::ops::store_build_of(&layout.prefix, target)?;
    (program == crate::seam::SEAM_PROGRAM
        && target.parent() == Some(layout.build_dir(&program, n).join("bin").as_path()))
    .then_some(n)
}

/// Make the exec root of the trust build at `build_dir` what the build needs, at `depth`.
/// # Errors
/// `build_dir` is not a trust build of this layout, [`needs_root`] could not read it, `compat` or `compat/trust` is not writable (a [`Blocked`] when not a real directory), or the root cannot be removed or laid.
pub fn ensure_root(layout: &Layout, build_dir: &Path, depth: Depth) -> io::Result<Ensured> {
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
        if let Some(blocked) = blocked(layout, n) {
            return Err(io::Error::other(blocked));
        }
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
        let restore_bin = || {
            if carry_bin {
                let _ = std::fs::rename(&tmp_bin, &root_bin);
            }
        };
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

/// Remove a committed root at `root`: a real directory is renamed to `aside` first, so
/// its name vanishes in one step and no shim can route into a tree half-way through
/// deletion; anything else at the name is unlinked.
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

/// Whether `target`, in a build's `bin/` at `bin`, is a stock name
/// ([`crate::seam::STOCK_NAMES`]) that is a separate file from its Trust tool: the bundle's
/// own copy.
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

/// The root file a shim at `shim` forwarding to `target` should run instead, or `None` for the plain shim.
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

/// An entry under `<prefix>/compat/trust` that is not a root any build needs, and that the next [`sweep`] (every `gc`) removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stray {
    /// A numeric name that is not a real directory.
    NotADirectory,
    /// A root whose build is no longer in the store.
    BuildGone(u64),
    /// A root whose build provably needs none ([`needs_root`] read it and answered `Ok(None)`).
    NeedsNone(u64),
}

/// What an entry of `<prefix>/compat/trust` named `name` is: the one classification
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
/// whose build stands, [`needs_root`]'s read of the build).
#[cfg(unix)]
fn classify(layout: &Layout, name: &str, path: &Path) -> Option<RootsEntry> {
    if name.starts_with('.') {
        return Some(RootsEntry::Debris);
    }
    let n = name.parse::<u64>().ok().filter(|n| n.to_string() == name)?;
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

/// `lstat` of `path` as an answer: `Ok(true)` for a real directory, `Ok(false)` for
/// provably none (absent, `ENOTDIR` above it, or a non-directory such as a symlink at the
/// name), and `Err` when the question went unanswered (a component that cannot be
/// searched).
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
/// only shape [`sweep`] and [`strays`] walk.
#[cfg(unix)]
fn walkable_roots_dir(layout: &Layout) -> Option<PathBuf> {
    let dir = roots_dir(layout);
    (crate::seam::is_real_dir(&compat_dir(layout)) && crate::seam::is_real_dir(&dir)).then_some(dir)
}

/// Every entry of `<prefix>/compat/trust` the next [`sweep`] would remove as not a root any build needs, with why, sorted by path.
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

/// Remove, from `<prefix>/compat/trust`, every entry that is not a root a build needs.
#[must_use]
pub fn sweep(layout: &Layout) -> Report {
    #[cfg(unix)]
    {
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

/// Remove the exec root of the build at `build_dir`.
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

/// Remove `<prefix>/compat/<program>` whole.
/// # Errors
/// The directory could not be removed.
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

/// Remove `<prefix>/compat` whole — `uninstall --all`'s half, on both of its branches.
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

/// Bring every trust shim in `bin/` in line with the exec roots its build needs, and write nothing when it already is.
#[must_use]
pub fn reconcile(layout: &Layout, depth: Depth) -> Report {
    if cfg!(not(unix)) {
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
                    Some(blocked) => format!(
                        "trust build {n}: {} — its trust shims render plain and run the store \
                         path; fix: {}",
                        blocked.what(),
                        blocked.fix()
                    ),
                    None => format!(
                        "trust build {n}: exec root {} not put in order ({e}); a shim without \
                         a whole root runs the store path",
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
        match files.iter().try_for_each(crate::lay::write_in_process) {
            Ok(()) => report.routed.extend(files.into_iter().map(|f| f.path)),
            Err(e) => report.errors.push(format!(
                "{} trust shim(s) not re-laid ({e}); they run as they were",
                files.len()
            )),
        }
    }
    report
}

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
    /// `<prefix>/compat` or `<prefix>/compat/trust` is not a real directory.
    Blocked(Blocked),
    /// Nothing at `<prefix>/compat/trust/<n>`.
    Absent,
    /// Something stands there, and this is the first path at which it stops being the build at [`Depth::Deep`].
    Differs(PathBuf),
    /// The root is a clone of the build, every file of it, with its marker.
    Matches,
}

/// What doctor reads about the exec root of one trust build that [`needs_root`]: the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// [`needs_root`]'s count: byte positions at which `bin/rustc` and `bin/trustc` differ.
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
    /// File names of the `bin/` shims whose target is a regular file in this build's `bin/`, sorted, less those forwarding to a stock-name copy.
    pub shims: Vec<String>,
    /// Those of [`Inspection::shims`] whose bytes are not the routed render.
    pub unrouted: Vec<String>,
}

/// Read the exec root of trust build `build` for doctor.
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

    fn exec_path(layout: &Layout, tool: &str) -> Option<PathBuf> {
        crate::ops::managed(layout, tool).map(|copy| copy.target)
    }

    struct Fx {
        root: aterm_tempfile::TempDir,
        layout: Layout,
    }

    /// The shapes a trust build's `bin/` has come in.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Shape {
        /// The affected bundles: `rustc` and `cargo` separate copies that differ.
        Copy,
        /// Trust-names-only: no stock-name copies.
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
            let root = aterm_tempfile::Builder::new()
                .prefix(&format!("atpkg-compat-{label}-"))
                .tempdir()
                .unwrap();
            let prefix = root.path().join("pkg");
            std::fs::create_dir_all(prefix.join("bin")).unwrap();
            Fx {
                root,
                layout: Layout { prefix },
            }
        }

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

        fn plain_shim(&self, name: &str, target: &Path, env: &crate::shim_env::ShimEnv) -> PathBuf {
            let shim = self.shim_path(name);
            std::fs::write(&shim, crate::platform::sh_shim_content_env(target, env)).unwrap();
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
            shim
        }
    }

    fn cloned(store: &Path, at: &Path) -> bool {
        crate::clone::is_clone_of(store, at)
            && matches!(crate::clone::same_bytes(store, at), Ok(true))
    }

    fn ident(path: &Path) -> (u64, u64) {
        let m = std::fs::symlink_metadata(path).unwrap();
        (m.dev(), m.ino())
    }

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

    /// Make `path` unreadable, answering the permissions it had. Restore with
    /// [`restore_mode`], never a literal: `std::fs::write` lays `0o666 & !umask`, and a view
    /// entry is identified by length, mode and mtime ([`crate::clone::is_clone_of`]), so a
    /// literal restore would change the store file and re-lay the view.
    #[must_use]
    fn unreadable(path: &Path) -> std::fs::Permissions {
        let had = std::fs::symlink_metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        had
    }

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

        let elsewhere = fx.root.path().join("elsewhere");
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

    #[test]
    fn a_failed_lay_commits_nothing_and_the_shims_stay_plain() {
        aterm_uds::host::could_not_run_as_root("a mode-0 directory");
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
        let elsewhere = fx.root.path().join("elsewhere");
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
        assert!(
            ensure_root(
                &fx.layout,
                &fx.root.path().join("elsewhere"),
                Depth::Shallow
            )
            .is_err()
        );
        assert!(ensure_root(&fx.layout, &build.join("bin"), Depth::Shallow).is_err());
    }

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

        let ay = fx.layout.build_dir("ay", 18).join("bin");
        std::fs::create_dir_all(&ay).unwrap();
        std::fs::write(ay.join("ay"), b"ay").unwrap();
        let ay_root = compat_dir(&fx.layout).join("ay").join("18").join("bin");
        std::fs::create_dir_all(&ay_root).unwrap();
        std::fs::hard_link(ay.join("ay"), ay_root.join("ay")).unwrap();
        assert_eq!(route_for_shim(&fx.shim_path("ay"), &ay.join("ay")), None);
        let dev = fx
            .root
            .path()
            .join("src")
            .join("store")
            .join("trust")
            .join("8595")
            .join("bin");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("tippy"), b"dev").unwrap();
        assert_eq!(route_for_shim(&shim, &dev.join("tippy")), None);
        let other = fx.root.path().join("pkg2").join("bin").join("tippy");
        assert_eq!(route_for_shim(&other, &target), None);
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

        let rustc = root.join("bin").join("rustc");
        std::fs::remove_file(&rustc).unwrap();
        std::fs::write(&rustc, b"not trustc").unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::remove_file(&rustc).unwrap();
        crate::clone::clone_file(&fx.tool(8595, "trustc"), &rustc).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
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
        let marker = root_marker(&root);
        std::fs::remove_file(&marker).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::os::unix::fs::symlink(&target, &marker).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(&marker, ROOT_MARKER_BODY).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
        let lib_aside = fx.root.path().join("lib-aside");
        std::fs::rename(root.join("lib"), &lib_aside).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
        std::fs::rename(&lib_aside, root.join("lib")).unwrap();
        assert!(route_for_shim(&shim, &target).is_some());
        let moved = fx.root.path().join("moved-root");
        std::fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        assert_eq!(route_for_shim(&shim, &target), None);
    }

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
        assert!(root_dir(&fx.layout, 8595).is_dir());
        let body = crate::platform::shim_executable_to_env(
            &fx.shim_path("tippy"),
            &fx.tool(8595, "tippy"),
            &none,
        )
        .unwrap()
        .body;
        assert!(String::from_utf8(body).unwrap().contains("[ ! -h "));
        std::fs::remove_dir_all(fx.layout.build_dir("trust", 8595)).unwrap();
        let report = reconcile(&fx.layout, Depth::Shallow);
        assert_eq!(report.swept, vec![root_dir(&fx.layout, 8595)]);
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(debris(&fx.layout).is_empty());
    }

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
        fx.plain_shim("tippy-driver", &fx.tool(8595, "tippy-driver-gone"), &none);
        crate::platform::install_tombstone_shim(
            &fx.shim_path("trustdoc"),
            "atpkg: trustdoc was yanked",
        )
        .unwrap();
        crate::stub::write_pending_stub(&fx.layout, &ToolName::new("ay").unwrap()).unwrap();
        let home = fx.root.path().join("home");
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
                "doctor",
                &crate::doctor::Probes::default(),
                crate::doctor::Detail::Everything,
                &mut out,
                &mut err,
            );
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
        for n in names {
            assert_eq!(
                exec_path(&fx.layout, n),
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
        let root_bin = root_dir(&fx.layout, 8595).join("bin");
        for n in names {
            let expected = match crate::ops::which(&fx.layout, n) {
                Some(t) if t.parent() == Some(build.join("bin").as_path()) && t.is_file() => {
                    Some(root_bin.join(t.file_name().unwrap()))
                }
                other => other,
            };
            assert_eq!(exec_path(&fx.layout, n), expected, "{n}");
        }
    }

    #[test]
    fn discard_build_takes_the_root_with_its_build_and_follows_nothing() {
        let fx = Fx::new("discard");
        let build = fx.build(8595, Shape::Copy);
        fx.build(8590, Shape::Copy);
        for n in [8590, 8595] {
            ensure_root(&fx.layout, &fx.layout.build_dir("trust", n), Depth::Shallow).unwrap();
        }
        let root = root_dir(&fx.layout, 8595);

        for not_a_build in [
            fx.layout
                .prefix
                .join("store")
                .join("trust")
                .join("8595.incoming-1"),
            fx.layout.prefix.join("store").join("trust").join("08595"),
            fx.root.path().join("elsewhere").join("trust").join("8595"),
            fx.layout.prefix.join("stash").join("trust").join("8595"),
        ] {
            discard_root_of(&not_a_build);
            assert!(root.is_dir(), "{}", not_a_build.display());
        }

        let recording = aterm_durable::seam::Recording::start();
        crate::store::discard_build(&build);
        assert_eq!(
            recording.take().first(),
            Some(&aterm_durable::Flush::Durable),
            "the marker's removal stays durable ahead of the tree's"
        );
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

        let elsewhere = fx.root.path().join("elsewhere-root");
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

        let lookalike = fx.root.path().join("lookalike");
        std::fs::create_dir_all(lookalike.join("trust").join("8590").join("bin")).unwrap();
        let compat = compat_dir(&fx.layout);
        std::fs::rename(&compat, fx.root.path().join("compat-aside")).unwrap();
        std::os::unix::fs::symlink(&lookalike, &compat).unwrap();
        crate::store::discard_build(&fx.layout.build_dir("trust", 8590));
        assert!(
            lookalike.join("trust").join("8590").join("bin").is_dir(),
            "a symlinked compat/ is not walked"
        );
    }

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

        let elsewhere = fx.root.path().join("elsewhere");
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
        let elsewhere = fx.root.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("bin")).unwrap();
        std::fs::write(elsewhere.join("bin").join("keep"), b"not atpkg's").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("9001")).unwrap();
        std::fs::create_dir_all(dir.join(".8595.tmp-77")).unwrap();

        let report = crate::gc::run(&fx.layout, None);
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
        let again = crate::gc::run(&fx.layout, None);
        assert!(again.swept_exec_roots.is_empty() && again.reclaimed.is_empty());
    }

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

        let elsewhere = fx.root.path().join("elsewhere");
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

        assert_eq!(exec_path(&fx.layout, "tippy"), Some(fx.tool(8595, "tippy")));
        ensure_root(&fx.layout, &build, Depth::Shallow).unwrap();
        let routed = root_dir(&fx.layout, 8595).join("bin").join("tippy");
        assert_eq!(exec_path(&fx.layout, "tippy"), Some(routed.clone()));
        assert_eq!(
            crate::ops::which(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy")),
            "which keeps answering the store"
        );
        assert_eq!(exec_path(&fx.layout, "ny"), Some(ny.join("ny")));
        assert_eq!(exec_path(&fx.layout, "absent"), None);
        assert_eq!(exec_path(&fx.layout, "../tippy"), None);

        let root = root_dir(&fx.layout, 8595);
        std::fs::remove_file(root_marker(&root)).unwrap();
        assert_eq!(
            exec_path(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy")),
            "an unmarked root is never exec'd"
        );
        std::fs::write(root_marker(&root), ROOT_MARKER_BODY).unwrap();
        std::fs::remove_file(&routed).unwrap();
        std::fs::write(&routed, b"not the store's tippy").unwrap();
        assert_eq!(
            exec_path(&fx.layout, "tippy"),
            Some(fx.tool(8595, "tippy")),
            "other bytes in the root are never exec'd"
        );
    }

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

    #[test]
    fn install_over_a_root_that_cannot_be_laid_lays_plain_shims() {
        aterm_uds::host::could_not_run_as_root("a mode-0 directory");
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

    #[test]
    fn a_build_that_cannot_be_read_keeps_its_root_and_its_routes() {
        aterm_uds::host::could_not_run_as_root("a mode-0 file");
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
        let trustc = build.join("bin").join("trustc");
        let trustc_mode = unreadable(&trustc);
        let read = needs_root(&build);
        restore_mode(&trustc, &trustc_mode);
        assert!(read.is_err(), "{read:?}");
        assert_eq!(
            root_first_mismatch(&build, &root_dir(&fx.layout, 8595), Depth::Deep),
            None,
            "the fixture's chmod round-trip, not a reconcile, moved the store"
        );
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

    #[test]
    fn nothing_is_removed_or_trusted_behind_a_linked_compat() {
        let none = crate::shim_env::ShimEnv::NONE;
        for linked in [COMPAT_DIR, "compat/trust"] {
            let fx = Fx::new("linked-plain");
            let build = fx.build(8596, Shape::NamesOnly);
            fx.plain_shim("tippy", &fx.tool(8596, "tippy"), &none);
            let elsewhere = fx.root.path().join("elsewhere");
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
            let aside = fx.root.path().join("aside");
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

        std::fs::write(root.join("bin").join("stray"), b"x").unwrap();
        assert_eq!(
            ensure_root(&fx.layout, &build, Depth::Shallow).unwrap(),
            Ensured::Built
        );
        assert_ne!(ident(&root.join("bin")), root_bin);
        assert!(root_matches(&build, &root, Depth::Deep));
    }
}
