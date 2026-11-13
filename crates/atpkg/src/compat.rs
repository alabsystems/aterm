// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-build exec roots for the Trust bundles whose own tippy refuses them.

use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::seam::Depth;
use crate::store::build_stands;

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
    const CHUNK: usize = 64 * 1024;
    let (mut fa, mut fb) = (
        crate::metadata_io::open_regular(a)?,
        crate::metadata_io::open_regular(b)?,
    );
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

/// Hold the existing native build gate across a destructive SDK mutation.
pub(crate) fn sdk_mutation_guard(
    prefix: &Path,
    program: &str,
    build: u64,
) -> io::Result<crate::lease::ReclaimGuard> {
    let subject = crate::lease::Subject::build(program, build)
        .ok_or_else(|| io::Error::other("SDK mutation has no native build subject"))?;
    match crate::lease::reclaim(prefix, &subject) {
        crate::lease::Reclaim::Clear(guard) => Ok(guard),
        crate::lease::Reclaim::Keep(holders) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("SDK {program} build {build} is held or unavailable: {holders:?}"),
        )),
    }
}

/// Exact managed store path only; no holder snapshot supplies mutation authority.
pub(crate) fn sdk_mutation_guard_for_build(
    build_dir: &Path,
) -> io::Result<Option<crate::lease::ReclaimGuard>> {
    let (prefix, program, build) = store_build_parts(build_dir)
        .ok_or_else(|| io::Error::other("SDK mutation path is not a managed build"))?;
    if crate::lease::Subject::build(program, build).is_none() {
        // No native SDK lease can exist for an unrepresentable identifier.
        return Ok(None);
    }
    sdk_mutation_guard(prefix, program, build).map(Some)
}

/// Validate an already-held original GC gate without recursively locking it.
pub(crate) fn check_sdk_reclaim(
    build_dir: &Path,
    guard: &crate::lease::ReclaimGuard,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let (prefix, program, build) = store_build_parts(build_dir)
            .ok_or_else(|| io::Error::other("SDK reclaim path is not a managed build"))?;
        let subject = crate::lease::Subject::build(program, build)
            .ok_or_else(|| io::Error::other("SDK reclaim has no subject"))?;
        guard.check_subject(prefix, &subject)?;
    }
    #[cfg(not(unix))]
    let _ = (build_dir, guard);
    Ok(())
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
                if walkable_roots_dir(layout).is_some() && std::fs::symlink_metadata(&root).is_ok()
                {
                    let _sdk = sdk_mutation_guard(&layout.prefix, crate::seam::SEAM_PROGRAM, n)?;
                    retire(&root, &aside)?;
                }
                return Ok(Ensured::Plain);
            }
            Ok(Some(_)) => {}
        }
        if let Some(blocked) = blocked(layout, n) {
            return Err(io::Error::other(blocked));
        }
        if root_matches(build_dir, &root, depth) {
            return Ok(Ensured::Present);
        }
        let _sdk = sdk_mutation_guard(&layout.prefix, crate::seam::SEAM_PROGRAM, n)?;
        layout.ensure_dir(&compat_dir(layout))?;
        layout.ensure_dir(&dir)?;
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
        let marker = root_marker(&tmp);
        let write_marker = || -> io::Result<()> {
            use std::io::Write as _;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            let mut f = opts.open(&marker)?;
            f.write_all(ROOT_MARKER_BODY.as_bytes())
        };
        if let Err(e) = write_marker() {
            restore_bin();
            let _ = remove_entry(&tmp);
            return Err(io::Error::new(
                e.kind(),
                format!("cannot write {} ({e})", marker.display()),
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
    Some(match (crate::store::real_dir(path), build_stands(&build)) {
        (Err(_), _) => RootsEntry::Needed,
        (Ok(false), _) => RootsEntry::Stray(Stray::NotADirectory),
        (Ok(true), Ok(false)) => RootsEntry::Stray(Stray::BuildGone(n)),
        (Ok(true), Ok(true)) if matches!(needs_root(&build), Ok(None)) => {
            RootsEntry::Stray(Stray::NeedsNone(n))
        }
        (Ok(true), _) => RootsEntry::Needed,
    })
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
            if matches!(
                classify(layout, &name, &path),
                None | Some(RootsEntry::Needed)
            ) {
                continue;
            }
            let _sdk = match entry_build(&name)
                .map(|n| sdk_mutation_guard(&layout.prefix, crate::seam::SEAM_PROGRAM, n))
                .transpose()
            {
                Ok(guard) => guard,
                Err(e) => {
                    report
                        .errors
                        .push(format!("{} was not removed ({e})", path.display()));
                    continue;
                }
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

/// Build identity carried by a root, staged tree, aside or sidecar name.
fn entry_build(name: &str) -> Option<u64> {
    let name = name.strip_prefix('.').unwrap_or(name);
    crate::store::parse_build_name(name.split('.').next()?)
}

/// Original native gates held through an enumerated program mutation.
pub(crate) struct SdkRemoval {
    prefix: PathBuf,
    program: String,
    #[cfg(unix)]
    entries: Vec<PathBuf>,
    _guards: Vec<crate::lease::ReclaimGuard>,
}

/// Acquire every native subject BEFORE uninstall changes shims or payloads.
/// The caller keeps its existing store publication/program gate as well.
pub(crate) fn prepare_sdk_removal(
    layout: &Layout,
    program: &str,
    store: bool,
) -> io::Result<SdkRemoval> {
    let compat = compat_dir(layout);
    let dir = compat.join(program);
    let mut entries = Vec::new();
    if crate::seam::is_real_dir(&compat) {
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {
                for entry in std::fs::read_dir(&dir)? {
                    entries.push(entry?.path());
                }
            }
            Ok(_) => entries.push(dir.clone()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let mut subjects = std::collections::BTreeSet::new();
    for path in &entries {
        if let Some(n) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(entry_build)
        {
            subjects.insert(n);
        }
    }
    if store {
        match std::fs::read_dir(layout.prefix.join("store").join(program)) {
            Ok(list) => {
                for entry in list {
                    if let Some(n) = entry?.file_name().to_str().and_then(entry_build) {
                        subjects.insert(n);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let mut guards = Vec::new();
    for n in subjects {
        if crate::lease::Subject::build(program, n).is_some() {
            guards.push(sdk_mutation_guard(&layout.prefix, program, n)?);
        }
    }
    Ok(SdkRemoval {
        prefix: layout.prefix.clone(),
        program: program.into(),
        #[cfg(unix)]
        entries,
        _guards: guards,
    })
}

/// Remove only the enumerated roots under their original native guards.
/// New roots appearing concurrently are never recursively swept by a parent delete.
pub(crate) fn remove_program_guarded(
    layout: &Layout,
    program: &str,
    guard: &SdkRemoval,
) -> io::Result<()> {
    if guard.prefix != layout.prefix || guard.program != program {
        return Err(io::Error::other(
            "SDK removal guard belongs to another program",
        ));
    }
    #[cfg(unix)]
    {
        let compat = compat_dir(layout);
        if !crate::seam::is_real_dir(&compat) {
            return Ok(());
        }
        let dir = compat.join(program);
        for path in &guard.entries {
            remove_entry(path)?;
        }
        match std::fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Remove the program's exec roots only when all native SDK leases are clear.
/// # Errors
/// A root is held, lease state is unknown, or removal fails.
pub fn remove_program(layout: &Layout, program: &str) -> io::Result<()> {
    let guard = prepare_sdk_removal(layout, program, false)?;
    remove_program_guarded(layout, program, &guard)
}

/// Remove compat roots without deleting a leased or newly appearing root.
/// # Errors
/// A root is held, lease state is unknown, or removal fails.
pub fn remove_all(layout: &Layout) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = compat_dir(layout);
        let meta = match std::fs::symlink_metadata(&dir) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if !meta.is_dir() {
            return remove_entry(&dir);
        }
        let mut guards = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("nonUTF8 compat program"))?;
            guards.push(prepare_sdk_removal(layout, name, false)?);
        }
        for guard in &guards {
            remove_program_guarded(layout, &guard.program, guard)?;
        }
        std::fs::remove_dir(dir)
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
        if build_stands(&build).unwrap_or(false) {
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
        for row in shims {
            let want =
                match crate::platform::shim_executable_to_env(&row.shim, &row.target, &row.env) {
                    Ok(want) => want,
                    Err(e) => {
                        report
                            .errors
                            .push(format!("{} not rendered ({e})", row.shim.display()));
                        continue;
                    }
                };
            if !row
                .body
                .as_deref()
                .is_some_and(|have| have.as_bytes() == want.body)
            {
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

struct TrustShim {
    shim: PathBuf,
    target: PathBuf,
    env: crate::shim_env::ShimEnv,
    body: Option<String>,
}

fn trust_shims(layout: &Layout) -> std::collections::BTreeMap<u64, Vec<TrustShim>> {
    let mut groups: std::collections::BTreeMap<u64, Vec<TrustShim>> =
        std::collections::BTreeMap::new();
    if let Ok(listing) = std::fs::read_dir(layout.bin_dir()) {
        for entry in listing.flatten() {
            let shim = entry.path();
            let read = crate::platform::read_shim(&shim);
            let Some(target) = read.target else {
                continue;
            };
            if let Some(n) = trust_tool_build(layout, &target) {
                groups.entry(n).or_default().push(TrustShim {
                    shim,
                    target,
                    env: read.env,
                    body: read.body,
                });
            }
        }
    }
    for shims in groups.values_mut() {
        shims.sort_by(|a, b| (&a.shim, &a.target).cmp(&(&b.shim, &b.target)));
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
        match build_stands(&build_dir) {
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
        for row in trust_shims(layout).remove(&build).unwrap_or_default() {
            // A shim whose target is not a regular file of the build has nothing to route
            // to (no root carries it, and [`route_for_shim`] answers `None`): it is a broken
            // shim, which doctor's own scan names, not an unrouted one.
            if !std::fs::symlink_metadata(&row.target).is_ok_and(|m| m.is_file()) {
                continue;
            }
            let (Some(name), Some(file)) = (row.shim.file_name(), row.target.file_name()) else {
                continue;
            };
            if is_stock_copy(&bin, &row.target) {
                continue;
            }
            let name = name.to_string_lossy().into_owned();
            let want = crate::platform::sh_shim_content_routed(
                &row.target,
                &row.env,
                Some(&root.join("bin").join(file)),
            );
            let routed = row.body.as_deref() == Some(want.as_str());
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
#[path = "compat_tests.rs"]
mod tests;
