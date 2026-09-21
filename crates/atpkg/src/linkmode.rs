// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Registry-free dev loop (§13): symlink curated bins from a sibling checkout into `bin/`
//! under a `0600` per-program marker; `update`/`apply` HARD-SKIP a linked program until
//! `atpkg unlink`.
//!
//! Every linked bin still passes [`crate::store::shim_allowed`] — a dev link can no more
//! shadow `sudo`/`git`/`rustc` than an installed shim can. The link marker is a DISTINCT
//! file from the pin-state file ([`crate::pin`]), so a pin survives a link→unlink cycle
//! untouched (linkmode never reads/writes/clears any pin-state). The marker is a local dev
//! artifact with NO signature — it is not a trust boundary; it can only SUPPRESS registry
//! management of a dev-linked program, never advance a program onto a build, so it cannot
//! bypass a Tombstone or the floor.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// Maximum serialized size of one private dev-link marker.
pub const MAX_LINK_MARKER_BYTES: usize = 128 * 1024;
/// Maximum bin paths retained in one dev-link marker.
pub const MAX_LINK_MARKER_BINS: usize = 1024;
/// Maximum directory entries inspected and program names returned by one
/// linked-program enumeration.
pub const MAX_LINKED_PROGRAMS: usize = 256;

/// What a [`link`]/[`refresh`] did: the bins symlinked, and any refused (sensitive-name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOutcome {
    /// The bin names symlinked into `bin/`.
    pub linked: Vec<String>,
    /// The bin names refused a link (on the [`crate::store::shim_allowed`] deny-list).
    pub refused: Vec<String>,
}

/// Why a link operation failed.
#[derive(Debug)]
pub enum LinkError {
    /// The program name is not a single safe path component.
    BadName(String),
    /// The checkout directory does not exist.
    NoCheckout(PathBuf),
    /// No linkable bin was found (all missing or refused).
    NoBins,
    /// The program is not currently dev-linked.
    NotLinked(String),
    /// An underlying IO failure.
    Io(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::BadName(n) => write!(f, "{n:?} is not a safe program name"),
            LinkError::NoCheckout(p) => write!(f, "checkout {} is not a directory", p.display()),
            LinkError::NoBins => write!(f, "no linkable bin found (all missing or refused)"),
            LinkError::NotLinked(p) => write!(f, "{p} is not dev-linked"),
            LinkError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// The `0600` per-program link marker (toml; NOT a trust boundary).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LinkMarker {
    schema: u32,
    program: String,
    /// Absolute checkout root the linked bins live under.
    checkout: String,
    /// The relative bin paths (as given) linked from the checkout.
    #[serde(default)]
    bins: Vec<String>,
    #[serde(default)]
    linked_at: String,
}

/// Reject a program name that is not a single safe path component (same shape rule
/// [`crate::store::shim_allowed`] / [`crate::ops::uninstall`] use).
fn safe_component(name: &str) -> bool {
    !(name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0'))
}

/// Whether `program` is currently dev-linked (its `0600` link marker exists).
#[must_use]
pub fn is_linked(layout: &Layout, program: &str) -> bool {
    safe_component(program) && read_marker_for_program(layout, program).is_ok()
}

/// Dev-link `program`: symlink each of `bins` (relative paths under `checkout`) into `bin/`,
/// every name gated through [`crate::store::shim_allowed`], and record a `0600` marker so
/// `update`/`apply` HARD-SKIP the program until [`unlink`]. NEVER touches any pin-state.
/// The shim/tool name for a bin path: its file name with the platform executable extension
/// removed (`foo.exe` → `foo` on Windows, case-insensitively; unchanged on Unix, where
/// `EXE_SUFFIX` is empty). Deriving the name this way is what makes a dev-linked tool
/// invokable by its bare name (Windows resolves `foo` → `foo.cmd` via `PATHEXT`, never
/// `foo.exe.cmd`) AND lets the sensitive-name deny-list see the real name (`ssh.exe` is
/// refused as `ssh`). Returns `None` if the path has no usable UTF-8 file name. `link` and
/// `unlink` MUST derive names identically so an unlink removes the exact shim a link created.
fn bin_tool_name(rel: &Path) -> Option<&str> {
    let name = rel.file_name().and_then(|s| s.to_str())?;
    Some(strip_exe_suffix(name, std::env::consts::EXE_SUFFIX))
}

/// `name` with a trailing `ext` removed, compared case-insensitively — the platform
/// executable suffix split out so both halves are exercisable off the platform that
/// has one (`EXE_SUFFIX` is `".exe"` on Windows and `""` everywhere else).
///
/// Compare bytes, never slice the `str`: `len - ext.len()` is a byte offset a non-ASCII
/// name can put inside a multi-byte character, and `&name[cut..]` panics there (`"日本語"`
/// is nine bytes, so `len - 4` is 5). Everything that derives a name — [`link`],
/// [`unlink`], [`refresh`], [`linked_bins`], [`linked_tool_names`] — comes through here.
/// The slice below is safe by construction: the suffix matches only when those trailing
/// bytes are ASCII, which makes `cut` a char boundary.
fn strip_exe_suffix<'a>(name: &'a str, ext: &str) -> &'a str {
    if ext.is_empty() {
        return name;
    }
    match name.len().checked_sub(ext.len()) {
        Some(cut) if cut > 0 && name.as_bytes()[cut..].eq_ignore_ascii_case(ext.as_bytes()) => {
            &name[..cut]
        }
        _ => name,
    }
}

/// What stood at a `bin/<tool>` name before this [`link`] replaced it — enough to put it
/// back exactly, captured before the overwrite.
///
/// A rollback restores what was there, never what this build would render today: a store
/// shim's body carries its own target, its own exported environment ([`crate::shim_env`])
/// and its own compat route, and a tombstone carries no target at all.
enum PriorShim {
    /// Nothing stood there. The rollback removes the shim this call created, which is the
    /// only arm on which it deletes anything at all.
    Absent,
    /// A regular file — the store's exec stub, a tombstone, or a Windows `.cmd` — kept as
    /// its bytes and its mode.
    File(Vec<u8>, fs::Permissions),
    /// A symlink, the shape a shim laid by an older atpkg still has: its target.
    Symlink(PathBuf),
}

/// Read what stands at `shim` into a [`PriorShim`], or refuse to touch it.
///
/// Only a regular file is read: a FIFO at a shim name would park this process on the open
/// forever, and neither a directory nor a device is a shim this manager laid. Those are an
/// error raised before the overwrite, rather than something replaced with no way back.
fn capture_shim(shim: &Path) -> Result<PriorShim, LinkError> {
    let meta = match fs::symlink_metadata(shim) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PriorShim::Absent),
        Err(e) => return Err(LinkError::Io(format!("{}: {e}", shim.display()))),
    };
    if meta.is_symlink() {
        return fs::read_link(shim)
            .map(PriorShim::Symlink)
            .map_err(|e| LinkError::Io(format!("{}: {e}", shim.display())));
    }
    if !meta.is_file() {
        return Err(LinkError::Io(format!(
            "{} is neither a file nor a symlink; refusing to replace what cannot be put back",
            shim.display()
        )));
    }
    if meta.len() > crate::platform::MAX_SHIM_BYTES as u64 {
        return Err(LinkError::Io(format!(
            "{} is {} bytes; a shim is at most {}",
            shim.display(),
            meta.len(),
            crate::platform::MAX_SHIM_BYTES
        )));
    }
    fs::read(shim)
        .map(|bytes| PriorShim::File(bytes, meta.permissions()))
        .map_err(|e| LinkError::Io(format!("{}: {e}", shim.display())))
}

/// The rollback's temp sibling of `shim`: a dot-name in `bin/`, pid-scoped like every other
/// temp this crate writes.
fn rollback_tmp(shim: &Path) -> std::io::Result<PathBuf> {
    let name = shim.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shim path has no file name",
        )
    })?;
    Ok(shim.with_file_name(format!(".{name}.atpkg-rollback-{}", std::process::id())))
}

/// Put a captured regular-file shim back, through a temp + rename like every other shim
/// writer here, so a name on the user's PATH is never briefly absent or half-written. The
/// temp goes on every error arm.
fn restore_file(shim: &Path, bytes: &[u8], mode: &fs::Permissions) -> std::io::Result<()> {
    let tmp = rollback_tmp(shim)?;
    let _ = fs::remove_file(&tmp);
    let restored = fs::write(&tmp, bytes)
        .and_then(|()| fs::set_permissions(&tmp, mode.clone()))
        .and_then(|()| fs::rename(&tmp, shim));
    if restored.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    restored
}

/// Put a captured symlink shim back, temp + rename like [`restore_file`].
#[cfg(unix)]
fn restore_symlink(shim: &Path, target: &Path) -> std::io::Result<()> {
    let tmp = rollback_tmp(shim)?;
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    let restored = fs::rename(&tmp, shim);
    if restored.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    restored
}

/// Windows shims are `.cmd` files, never links, and an unprivileged process there cannot
/// create a symlink — so nothing this crate lays on Windows is ever captured as one.
#[cfg(not(unix))]
fn restore_symlink(shim: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("cannot restore a symlink shim at {}", shim.display()),
    ))
}

/// Undo the shim mutations of a failed [`link`], best-effort and newest first: a name this
/// call brought into existence is removed, a name it overwrote is put back byte for byte.
///
/// Both arms matter. Removing only the new shims left an already-installed program's shim
/// re-pointed at the dev binary with no marker recording it — the untracked dev shim this
/// rollback exists to prevent, and the one state `unlink` cannot undo, since `unlink` reads
/// that marker. Only paths this call itself replaced are passed in, so the removal arm can
/// never widen into a name that already stood.
fn roll_back(replaced: &[(PathBuf, PriorShim)]) {
    for (shim, prior) in replaced.iter().rev() {
        let _ = match prior {
            PriorShim::Absent => fs::remove_file(shim),
            PriorShim::File(bytes, mode) => restore_file(shim, bytes, mode),
            PriorShim::Symlink(target) => restore_symlink(shim, target),
        };
    }
}

pub fn link(
    layout: &Layout,
    program: &str,
    checkout: &Path,
    bins: &[PathBuf],
) -> Result<LinkOutcome, LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    // Absolutize the checkout so the shim embeds an ABSOLUTE target. A relative target is
    // resolved at INVOCATION time, not link time: a Windows `.cmd` resolves it against the
    // caller's CWD (tool not found from elsewhere, or worse runs a same-named binary in the
    // caller's CWD), and a Unix relative symlink resolves against `bin/`, not the checkout.
    // `path::absolute` is lexical (no filesystem hit, no `\\?\` verbatim prefix), so the
    // embedded path stays clean; the marker then also records an absolute checkout so
    // `refresh` works from any directory.
    let checkout = std::path::absolute(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    if !checkout.is_dir() {
        return Err(LinkError::NoCheckout(checkout.clone()));
    }
    if bins.len() > MAX_LINK_MARKER_BINS {
        return Err(LinkError::Io(format!(
            "link marker has {} bins; limit is {MAX_LINK_MARKER_BINS}",
            bins.len()
        )));
    }
    let marker = LinkMarker {
        schema: 1,
        program: program.to_string(),
        checkout: checkout.display().to_string(),
        bins: bins.iter().map(|b| b.display().to_string()).collect(),
        linked_at: String::new(),
    };
    // Validate the complete marker before mutating any shim, so a request
    // which cannot be recorded never leaves a partially linked dev loop.
    serialize_marker(&marker).map_err(|e| LinkError::Io(e.to_string()))?;
    layout
        .ensure_dir(&layout.bin_dir())
        .map_err(|e| LinkError::Io(e.to_string()))?;
    ensure_private_dir(&layout.links_dir()).map_err(|e| LinkError::Io(e.to_string()))?;

    let mut linked = Vec::new();
    let mut refused = Vec::new();
    // Every shim this call mutates, paired with what stood there first, for the rollback
    // below. A name that already stood is somebody else's (a store shim, a hand-made file),
    // so undoing our own failure restores it and never deletes it.
    let mut replaced: Vec<(PathBuf, PriorShim)> = Vec::new();
    for rel in bins {
        let Some(name) = bin_tool_name(rel) else {
            continue;
        };
        // SECURITY: the sensitive-name deny-list is honored for dev links too — here by the
        // only constructor that can produce a name `Layout::shim` will accept.
        let Some(tool) = crate::store::ToolName::new(name) else {
            refused.push(name.to_string());
            continue;
        };
        let src = checkout.join(rel);
        if !src.is_file() {
            continue; // a not-yet-built bin is simply skipped (refresh picks it up later)
        }
        let shim = layout.shim(&tool);
        // Captured before the overwrite — once the dev shim is laid, the store shim's own
        // bytes are gone and nothing could say what they were.
        let prior = match capture_shim(&shim) {
            Ok(prior) => prior,
            Err(e) => {
                roll_back(&replaced);
                return Err(e);
            }
        };
        if let Err(e) = crate::platform::install_shim_to(&shim, &src) {
            // The shim writer is temp + rename, so a failure here leaves this name as it
            // was; only the ones already replaced need undoing.
            roll_back(&replaced);
            return Err(LinkError::Io(e.to_string()));
        }
        replaced.push((shim, prior));
        linked.push(name.to_string());
    }
    if linked.is_empty() {
        roll_back(&replaced);
        return Err(LinkError::NoBins);
    }

    // The marker is what makes a dev link tracked. Letting a failed marker write propagate
    // left a checkout wired into `bin/` that nothing recorded: `is_linked` said no, so
    // `update`/`apply` would re-point the shims under the developer, and `unlink` — which
    // reads the marker — had nothing to undo. Every shim this call touched is rolled back,
    // so a link either stands recorded or does not stand.
    if let Err(e) = write_marker(&layout.link_marker(program), &marker) {
        roll_back(&replaced);
        return Err(LinkError::Io(e.to_string()));
    }
    // A DEV LINK HAS TO WIN ON PATH. For an agent program (`crate::stub::AGENT_PROGRAMS`)
    // `agents/<tool>` goes FIRST on every PATH and names the STORE build directly, so a
    // link that re-pointed only `bin/<tool>` left the dev checkout unreachable — `claude`
    // kept running the installed copy with nothing to say why. The reconcile lays no twin
    // for a primary that resolves outside the store; its sweep drops the one standing.
    crate::activate::reconcile_agents(layout);
    Ok(LinkOutcome { linked, refused })
}

/// Un-link `program`: remove the shims that STILL point into its recorded checkout (never a
/// re-installed store shim), then delete the marker. Leaves any pin-state untouched, so a
/// pin survives the cycle.
pub fn unlink(layout: &Layout, program: &str) -> Result<(), LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let marker_path = layout.link_marker(program);
    let marker = read_marker_for_program(layout, program).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LinkError::NotLinked(program.to_string())
        } else {
            LinkError::Io(format!("could not read link marker for {program}: {error}"))
        }
    })?;
    let checkout = PathBuf::from(&marker.checkout);
    for rel in &marker.bins {
        let Some(name) = bin_tool_name(Path::new(rel)) else {
            continue;
        };
        // A name `link` refused was never given a shim, so there is nothing here to remove —
        // and this is why `link` and `unlink` must derive names identically (see
        // `bin_tool_name`): the admission is now part of that derivation.
        let Some(tool) = crate::store::ToolName::new(name) else {
            continue;
        };
        let link = layout.shim(&tool);
        // Only remove a link STILL pointing into the checkout — never nuke a re-installed
        // store shim that happens to share the name. `resolve_shim` reads the forward
        // target cross-platform (symlink target on Unix, the `.cmd` target on Windows) —
        // a bare `fs::read_link` would Err on a Windows `.cmd` and leak the dev shim.
        if let Some(target) = crate::platform::resolve_shim(&link)
            && target.starts_with(&checkout)
        {
            let _ = fs::remove_file(&link);
        }
    }
    let _ = fs::remove_file(&marker_path);
    // Symmetrically: the shims just removed were the only thing that could vouch for a
    // twin. The cli's unlink restores the installed build's shims after this — and
    // reconciles through `install_tools_env` — but only when there IS an installed build
    // and a name to put back; this is what keeps `agents/` from outliving `bin/` otherwise.
    crate::activate::reconcile_agents(layout);
    Ok(())
}

/// The recorded checkout root of a dev-linked `program` (from its `0600` marker), or
/// `None` when it is not linked / the marker is unreadable. Read-only. The config
/// link reconciliation (`[packages.links]`) uses this to detect a HAND-MADE link
/// pointing at a DIFFERENT checkout — which it must refuse to touch, loudly, rather
/// than silently re-point a developer's live dev loop.
#[must_use]
pub fn linked_checkout(layout: &Layout, program: &str) -> Option<PathBuf> {
    linked_checkout_checked(layout, program).ok().flatten()
}

/// Checked counterpart of [`linked_checkout`]. Missing is `Ok(None)`; an
/// existing malformed, oversized, special, or link-like marker is an error the
/// caller can surface instead of silently treating the program as unmanaged.
pub fn linked_checkout_checked(layout: &Layout, program: &str) -> std::io::Result<Option<PathBuf>> {
    if !safe_component(program) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{program:?} is not a safe program name"),
        ));
    }
    match read_marker_for_program(layout, program) {
        Ok(marker) => Ok(Some(PathBuf::from(marker.checkout))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Re-assert a program's dev links (idempotent): re-run [`link`] from the recorded marker,
/// picking up any newly-built/added bins. No build is invoked — building is producer scope,
/// absent from the consumer (a documented divergence from aterm-pkg's `refresh`).
/// The tool names a dev-linked `program` puts on PATH, from its link marker (the rel
/// bins it was linked with, by file stem) — empty when it is not linked. For `which
/// <program>` to name what the link exposes without re-walking the managed bin/.
#[must_use]
pub fn linked_bins(layout: &Layout, program: &str) -> Vec<String> {
    if !safe_component(program) {
        return Vec::new();
    }
    let Ok(marker) = read_marker_for_program(layout, program) else {
        return Vec::new();
    };
    marker
        .bins
        .iter()
        .filter_map(|rel| bin_tool_name(Path::new(rel)).map(str::to_string))
        .collect()
}

pub fn refresh(layout: &Layout, program: &str) -> Result<LinkOutcome, LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let marker = read_marker_for_program(layout, program).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LinkError::NotLinked(program.to_string())
        } else {
            LinkError::Io(format!("could not read link marker for {program}: {error}"))
        }
    })?;
    let checkout = PathBuf::from(&marker.checkout);
    let bins: Vec<PathBuf> = marker.bins.iter().map(PathBuf::from).collect();
    link(layout, program, &checkout, &bins)
}

/// The names of all dev-linked programs (the safe-component file names under `links/`).
#[must_use]
pub fn linked_programs(layout: &Layout) -> Vec<String> {
    linked_programs_checked(layout).unwrap_or_default()
}

// The directory's OS-level identity — the value that must be IDENTICAL before and
// after an enumeration for that enumeration to describe one directory. `None` means
// the platform could not determine it, which the caller treats as a failure (never as
// "unchanged"), so every arm fails CLOSED.

#[cfg(unix)]
fn directory_identity(_path: &Path, metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((metadata.dev(), metadata.ino()))
}

// Path-based, not metadata-based: `Metadata` carries the same volume-serial/file-index
// pair only behind the unstable `windows_by_handle` feature, so this goes through the
// platform backend's stable `GetFileInformationByHandle` leaf call instead.
#[cfg(windows)]
fn directory_identity(path: &Path, _metadata: &fs::Metadata) -> Option<(u32, u64)> {
    crate::platform::directory_identity(path)
}

#[cfg(not(any(unix, windows)))]
fn directory_identity(
    _path: &Path,
    metadata: &fs::Metadata,
) -> Option<(u64, Option<std::time::SystemTime>)> {
    Some((metadata.len(), metadata.modified().ok()))
}

/// Enumerate linked program names with a strict entry/result cap and explicit
/// diagnostics for a link-like, replaced, or non-directory `links/` path.
pub fn linked_programs_checked(layout: &Layout) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let directory = layout.links_dir();
    let before = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error),
    };
    if !before.file_type().is_dir() || crate::platform::is_reparse(&before) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "package links path is not a real directory",
        ));
    }
    // Fail CLOSED on an unknowable identity: without it the post-enumeration compare
    // below would be `None != None` — false — and would wave a swapped directory through.
    let Some(identity) = directory_identity(&directory, &before) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "could not determine the identity of the package links directory",
        ));
    };
    let entries = fs::read_dir(&directory)?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_LINKED_PROGRAMS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("package links directory exceeds the {MAX_LINKED_PROGRAMS}-entry limit"),
            ));
        }
        let e = entry?;
        if let Some(n) = e.file_name().to_str()
            && safe_component(n)
        {
            out.push(n.to_string());
        }
    }
    let after = fs::symlink_metadata(&directory)?;
    if !after.file_type().is_dir()
        || crate::platform::is_reparse(&after)
        || directory_identity(&directory, &after) != Some(identity)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "package links directory changed while it was enumerated",
        ));
    }
    out.sort();
    Ok(out)
}

/// The tool NAMES a program's dev link currently owns, or `None` when it is not linked.
///
/// Exposed so `unlink` can restore exactly what the link took over — no more, and no fewer.
/// Deriving that set any other way is unsafe: the installed build's `bin/` is NOT the answer
/// for a sysroot bundle, because a toolchain ships backends belonging to OTHER atpkg
/// programs (`trust`'s bin/ carries `ay`, `clean`, `ty`), and restoring from the directory
/// listing repoints those programs' shims at the wrong build. Measured: doing exactly that
/// clobbered `ay`, `clean` and `ty` onto `store/trust/6459`.
///
/// The marker is the only record that is both LOCAL and SCOPED to this program's link, which
/// makes unlink's restoration symmetric with link's takeover by construction.
pub fn linked_tool_names(layout: &Layout, program: &str) -> Option<Vec<String>> {
    let marker = read_marker_for_program(layout, program).ok()?;
    let names: Vec<String> = marker
        .bins
        .iter()
        .filter_map(|rel| bin_tool_name(Path::new(rel)).map(str::to_string))
        .collect();
    if names.is_empty() { None } else { Some(names) }
}

fn read_marker_for_program(layout: &Layout, program: &str) -> std::io::Result<LinkMarker> {
    let marker = read_marker(&layout.link_marker(program))?;
    if marker.program != program {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "link marker program {:?} does not match filename {program:?}",
                marker.program
            ),
        ));
    }
    Ok(marker)
}

fn read_marker(path: &Path) -> std::io::Result<LinkMarker> {
    let text = crate::metadata_io::read_bounded_regular_utf8(path, MAX_LINK_MARKER_BYTES)?;
    let marker: LinkMarker = aterm_toml::from_str(&text).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("link marker is invalid: {error}"),
        )
    })?;
    if !safe_component(&marker.program) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "link marker contains an unsafe program name",
        ));
    }
    if marker.bins.len() > MAX_LINK_MARKER_BINS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "link marker has {} bins; limit is {MAX_LINK_MARKER_BINS}",
                marker.bins.len()
            ),
        ));
    }
    Ok(marker)
}

fn serialize_marker(marker: &LinkMarker) -> std::io::Result<String> {
    if marker.bins.len() > MAX_LINK_MARKER_BINS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "link marker has {} bins; limit is {MAX_LINK_MARKER_BINS}",
                marker.bins.len()
            ),
        ));
    }
    let text = aterm_toml::to_string(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if text.len() > MAX_LINK_MARKER_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("link marker exceeds the {MAX_LINK_MARKER_BYTES}-byte limit"),
        ));
    }
    Ok(text)
}

/// Write the marker `0600` via temp + rename.
fn write_marker(dest: &Path, marker: &LinkMarker) -> std::io::Result<()> {
    let text = serialize_marker(marker)?;
    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "link marker has no parent",
        )
    })?;
    let tmp = parent.join(format!(".link.tmp-{}", std::process::id()));
    // No temp may survive a failure: `links_dir` is enumerated by name, and
    // `linked_programs` admits every entry that is a safe path component, so a leftover
    // `.link.tmp-<pid>` was reported as a dev-linked program by `atpkg list`, by `which` and
    // by the Packages screen — one nobody linked and no `unlink` could remove. Born `0600`
    // through `create_new` because `fs::write` creates at the umask default.
    let _ = fs::remove_file(&tmp);
    let staged = create_marker_temp(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
        .and_then(|()| crate::platform::harden_file(&tmp));
    if let Err(e) = staged {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Create the link marker's temp exclusively and, on Unix, born `0600`.
#[cfg(unix)]
fn create_marker_temp(tmp: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

#[cfg(not(unix))]
fn create_marker_temp(tmp: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs::Permissions;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-link-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&p, Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// `bin/<name>` for a name the test knows is admissible.
    fn shim_of(layout: &Layout, name: &str) -> PathBuf {
        layout.shim(&crate::store::ToolName::new(name).unwrap())
    }

    /// A fake checkout with `target/release/<bins>`.
    fn checkout(label: &str, bins: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-checkout-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("target/release")).unwrap();
        for b in bins {
            fs::write(d.join("target/release").join(b), b"#!/bin/true\n").unwrap();
        }
        d
    }

    /// The exe suffix is stripped by bytes, so a name can never be cut mid-character.
    /// Driven with an explicit `".exe"` rather than `EXE_SUFFIX`, so the Windows behaviour
    /// is exercised on every host: only the byte offset matters, and `&name[cut..]` panicked
    /// on `"日本語"` (nine bytes, so `len - 4` is 5, inside its third character).
    #[test]
    fn the_exe_suffix_is_stripped_without_ever_slicing_mid_character() {
        // A non-ASCII name whose length puts the cut inside a character.
        assert_eq!(strip_exe_suffix("日本語", ".exe"), "日本語");
        assert_eq!(strip_exe_suffix("日本語.exe", ".exe"), "日本語");
        assert_eq!(strip_exe_suffix("日本語.EXE", ".exe"), "日本語");
        assert_eq!(strip_exe_suffix("é", ".exe"), "é");
        assert_eq!(strip_exe_suffix("éxe", ".exe"), "éxe");
        assert_eq!(strip_exe_suffix("é.exe", ".exe"), "é");
        // Plain ASCII, unchanged in both directions.
        assert_eq!(strip_exe_suffix("ay.exe", ".exe"), "ay");
        assert_eq!(strip_exe_suffix("ay.ExE", ".exe"), "ay");
        assert_eq!(strip_exe_suffix("ay", ".exe"), "ay");
        // The Unix suffix is empty: nothing is ever stripped.
        assert_eq!(strip_exe_suffix("ay", ""), "ay");
        assert_eq!(strip_exe_suffix("ay.exe", ""), "ay.exe");
        // A name that is only the suffix keeps it — `cut > 0` — since ".exe" names no tool.
        assert_eq!(strip_exe_suffix(".exe", ".exe"), ".exe");
        // And the derivation `link`/`unlink` share still agrees on a real bin path.
        assert_eq!(bin_tool_name(Path::new("target/release/ay")), Some("ay"));
    }
    /// A marker that cannot be written leaves no dev shim and no temp.
    ///
    /// The marker is the only thing that records a dev link, so a failed marker write that
    /// propagated left a checkout wired into `bin/` that nothing tracked: `is_linked`
    /// answered no and `unlink` had nothing to undo. A leaked `.link.tmp-<pid>` was worse —
    /// `linked_programs` admits every safe-named entry of the links dir, so the litter was
    /// reported as a dev-linked program that no `unlink` could remove.
    #[cfg(unix)]
    #[test]
    fn a_link_whose_marker_cannot_be_written_rolls_back_and_leaves_no_temp() {
        let l = layout("markerfail");
        let co = checkout("markerfail", &["ay"]);
        let bins = [PathBuf::from("target/release/ay")];
        // The marker's destination is a non-empty directory, so the rename cannot replace
        // it — the arm that used to leak the temp with the shim already laid.
        fs::create_dir_all(l.links_dir().join("ay").join("occupied")).unwrap();

        let out = link(&l, "ay", &co, &bins);
        assert!(
            out.is_err(),
            "a marker that cannot be written must fail the link"
        );
        assert!(
            !shim_of(&l, "ay").exists(),
            "the shim this call created must be rolled back — an UNTRACKED dev shim is \
             worse than no link at all"
        );
        let litter: Vec<String> = fs::read_dir(l.links_dir())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| n.starts_with(".link.tmp-"))
            .collect();
        assert!(
            litter.is_empty(),
            "no marker temp may survive a failed write, found {litter:?}"
        );
        assert!(
            !linked_programs(&l)
                .iter()
                .any(|p| p.starts_with(".link.tmp-")),
            "a leaked temp must never be reported as a dev-linked program"
        );

        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    /// And it puts back the shim it overwrote, not only the ones it created.
    ///
    /// Recording a shim only when the name was new undid nothing for an already-installed
    /// program — the ordinary case for a dev link, and the only one in which a shim is
    /// overwritten at all. A failed marker write then left `bin/ay` re-pointed at the dev
    /// checkout with nothing recording it, which `unlink` cannot put back either. The
    /// store's shim must come out of a failed link byte for byte.
    #[cfg(unix)]
    #[test]
    fn a_failed_link_restores_the_shim_it_overwrote() {
        let l = layout("overwrite");
        let co = checkout("overwrite", &["ay"]);
        let bins = [PathBuf::from("target/release/ay")];
        // The program is already installed: the store's own shim stands at `bin/ay`.
        fs::create_dir_all(l.bin_dir()).unwrap();
        let store_bin = l.prefix.join("store-build").join("ay");
        fs::create_dir_all(store_bin.parent().unwrap()).unwrap();
        fs::write(&store_bin, b"#!/bin/true\n").unwrap();
        let shim = shim_of(&l, "ay");
        crate::platform::install_shim_to(&shim, &store_bin).unwrap();
        let before = fs::read(&shim).unwrap();
        let mode_before = fs::metadata(&shim).unwrap().permissions().mode() & 0o777;

        // The marker's destination is a non-empty directory, so the marker write fails with
        // the shim already re-pointed at the checkout.
        fs::create_dir_all(l.links_dir().join("ay").join("occupied")).unwrap();

        let out = link(&l, "ay", &co, &bins);
        assert!(
            out.is_err(),
            "a marker that cannot be written must fail the link"
        );
        assert_eq!(
            fs::read(&shim).unwrap(),
            before,
            "the OVERWRITTEN store shim must be restored byte for byte, never left \
             pointing into the dev checkout"
        );
        assert_eq!(
            fs::metadata(&shim).unwrap().permissions().mode() & 0o777,
            mode_before,
            "the restored shim keeps its mode"
        );
        assert_eq!(
            crate::platform::resolve_shim(&shim).as_deref(),
            Some(store_bin.as_path()),
            "the restored shim forwards to the STORE build, never to the checkout"
        );
        assert!(
            !is_linked(&l, "ay"),
            "no marker, so no dev link is recorded"
        );
        // And the restore leaves no temp of its own beside the shim.
        let litter: Vec<String> = fs::read_dir(l.bin_dir())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| n.contains(".atpkg-rollback-"))
            .collect();
        assert!(
            litter.is_empty(),
            "the rollback must leave no temp behind, found {litter:?}"
        );

        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_and_unlink_round_trip() {
        let l = layout("rt");
        let co = checkout("rt", &["ay", "ny"]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/ny"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string(), "ny".to_string()]);
        // Shims point INTO the checkout; marker exists 0600; is_linked true. resolve_shim
        // reads the forward target cross-platform (a `.cmd` is not read_link-able).
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "ay")).unwrap(),
            co.join("target/release/ay")
        );
        assert!(is_linked(&l, "ay"));
        // marker is 0600 — Unix-only mode check.
        #[cfg(unix)]
        {
            let mode = fs::metadata(l.link_marker("ay"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        unlink(&l, "ay").unwrap();
        assert!(!is_linked(&l, "ay"));
        assert!(
            fs::symlink_metadata(shim_of(&l, "ay")).is_err(),
            "shim removed"
        );
        assert!(
            fs::symlink_metadata(shim_of(&l, "ny")).is_err(),
            "shim removed"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    /// A DEV LINK MUST WIN AT THE FRONT OF PATH. For an agent program, `agents/<tool>` is
    /// laid beside `bin/<tool>` and goes FIRST on every PATH — naming the store build
    /// directly — so a `link` that re-pointed only `bin/claude` left the dev checkout
    /// unreachable: typing `claude` still ran the installed copy. `unlink` is the mirror:
    /// the shims it removes were the only thing vouching for a twin.
    #[test]
    fn link_and_unlink_sweep_the_front_of_path_agents_twin() {
        let l = layout("agents-twin");
        let co = checkout("agents-twin", &["claude"]);
        let claude = crate::store::ToolName::new("claude").unwrap();
        // The installed shape: `bin/claude` and its `agents/` twin on the same store build.
        let store_bin = l.build_dir("claude", 2_026_091_001).join("bin");
        fs::create_dir_all(&store_bin).unwrap();
        let target = store_bin.join(claude.exe_file());
        fs::write(&target, b"#!/bin/true\n").unwrap();
        let lay_twin = || {
            l.ensure_dir(&l.agents_dir()).unwrap();
            crate::platform::install_shim_to(&l.agent_shim(&claude), &target).unwrap();
        };
        l.ensure_dir(&l.bin_dir()).unwrap();
        crate::platform::install_shim_to(&shim_of(&l, "claude"), &target).unwrap();
        lay_twin();

        let bins = [PathBuf::from("target/release/claude")];
        link(&l, "claude", &co, &bins).unwrap();
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "claude")).unwrap(),
            co.join("target/release/claude"),
            "fixture: bin/claude is the dev link"
        );
        assert!(
            fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
            "no twin shadows the dev link at the front of PATH"
        );

        // A twin an older client laid before the link goes on unlink too — the cli's
        // restore reconciles only when there is an installed build and an owned name to
        // put back.
        lay_twin();
        unlink(&l, "claude").unwrap();
        assert!(
            fs::symlink_metadata(shim_of(&l, "claude")).is_err(),
            "fixture: the dev shim is gone"
        );
        assert!(
            fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
            "agents/ does not outlive the bin/ shim that vouched for it"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn sensitive_bin_name_is_refused_a_link() {
        let l = layout("sensitive");
        let co = checkout("sensitive", &["ay", "git"]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/git"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string()]);
        assert_eq!(out.refused, vec!["git".to_string()]);
        // `l.shim("git")` does not exist to be written: `ToolName::new("git")` is `None`, so
        // the path is spelled by hand here to assert nothing landed at it.
        assert!(crate::store::ToolName::new("git").is_none());
        assert!(
            fs::symlink_metadata(
                l.bin_dir()
                    .join(format!("git{}", crate::platform::SHIM_SUFFIX))
            )
            .is_err(),
            "sensitive name never shimmed"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn unlink_only_removes_links_into_its_checkout() {
        let l = layout("scoped");
        let co = checkout("scoped", &["ay"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        // Overwrite the ay shim with a store-style shim (a re-install) OUTSIDE the checkout,
        // via the same primitive a real install uses (a symlink on Unix, a `.cmd` on Windows
        // — atomic_symlink is the DIRECTORY-junction primitive there, wrong for a bin shim).
        let store_target = l.build_dir("ay", 18).join("bin/ay");
        fs::create_dir_all(store_target.parent().unwrap()).unwrap();
        fs::write(&store_target, b"#!/bin/true\n").unwrap();
        crate::platform::install_shim_to(&shim_of(&l, "ay"), &store_target).unwrap();
        unlink(&l, "ay").unwrap();
        // The store shim survived — unlink only removes links into the checkout.
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "ay")).unwrap(),
            store_target
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_preserves_a_pre_existing_pin() {
        let l = layout("pin");
        crate::pin::set_pinned(&l, "ay", true).unwrap();
        let before = fs::read(l.prefix.join("pins")).unwrap();
        let co = checkout("pin", &["ay"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        unlink(&l, "ay").unwrap();
        let after = fs::read(l.prefix.join("pins")).unwrap();
        assert_eq!(
            before, after,
            "pin file is byte-identical across a link→unlink cycle"
        );
        assert!(crate::pin::is_pinned(&l, "ay"));
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn refresh_relinks_newly_built_bins() {
        let l = layout("refresh");
        let co = checkout("refresh", &["ay"]);
        // Link with both bins recorded, but ny not yet built.
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/ny"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string()], "ny not built yet");
        // Build ny, then refresh.
        fs::write(co.join("target/release/ny"), b"#!/bin/true\n").unwrap();
        let out2 = refresh(&l, "ay").unwrap();
        assert!(
            out2.linked.contains(&"ny".to_string()),
            "refresh picks up the new bin"
        );
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "ny")).unwrap(),
            co.join("target/release/ny")
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn linked_programs_lists_markers() {
        let l = layout("list");
        let co = checkout("list", &["ay", "ny"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        link(&l, "ny", &co, &[PathBuf::from("target/release/ny")]).unwrap();
        assert_eq!(
            linked_programs(&l),
            vec!["ay".to_string(), "ny".to_string()]
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_marker_bin_count_is_rejected_before_shim_mutation() {
        let l = layout("bin-cap");
        let co = checkout("bin-cap", &["ay"]);
        let bins = vec![PathBuf::from("target/release/ay"); MAX_LINK_MARKER_BINS + 1];
        let error = link(&l, "ay", &co, &bins).unwrap_err();
        assert!(error.to_string().contains("limit is"), "{error}");
        assert!(
            crate::platform::resolve_shim(&shim_of(&l, "ay")).is_none(),
            "an unrecordable request must not mutate shims"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn linked_program_enumeration_accepts_exact_cap_and_rejects_one_more() {
        let l = layout("entry-cap");
        fs::create_dir_all(l.links_dir()).unwrap();
        for index in 0..MAX_LINKED_PROGRAMS {
            fs::write(l.links_dir().join(format!("p{index:03}")), b"x").unwrap();
        }
        assert_eq!(
            linked_programs_checked(&l).unwrap().len(),
            MAX_LINKED_PROGRAMS
        );
        fs::write(l.links_dir().join("overflow"), b"x").unwrap();
        let error = linked_programs_checked(&l).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("entry limit"), "{error}");
        assert!(
            linked_programs(&l).is_empty(),
            "compatibility wrapper fails closed instead of returning a partial set"
        );
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_retargetable_link_markers_are_explicit_errors() {
        use std::os::unix::ffi::OsStrExt as _;

        let l = layout("marker-special");
        fs::create_dir_all(l.links_dir()).unwrap();
        let marker = l.link_marker("ay");
        let marker_c = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
        // SAFETY: `marker_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(marker_c.as_ptr(), 0o600) }, 0);
        let fifo_error = linked_checkout_checked(&l, "ay").unwrap_err();
        assert_eq!(fifo_error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!is_linked(&l, "ay"));
        fs::remove_file(&marker).unwrap();

        let first = l.prefix.join("first.toml");
        let second = l.prefix.join("second.toml");
        let marker_text = |checkout: &str| {
            aterm_toml::to_string(&LinkMarker {
                schema: 1,
                program: "ay".to_string(),
                checkout: checkout.to_string(),
                bins: Vec::new(),
                linked_at: String::new(),
            })
            .unwrap()
        };
        fs::write(&first, marker_text("/first")).unwrap();
        fs::write(&second, marker_text("/second")).unwrap();
        std::os::unix::fs::symlink(&first, &marker).unwrap();
        assert!(linked_checkout_checked(&l, "ay").is_err());
        fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(&second, &marker).unwrap();
        assert!(
            linked_checkout_checked(&l, "ay").is_err(),
            "retargeting the link never makes a link-like marker admissible"
        );
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn link_directory_symlink_is_rejected_instead_of_followed() {
        let l = layout("directory-link");
        let foreign = layout("directory-link-foreign");
        fs::create_dir_all(foreign.links_dir()).unwrap();
        fs::write(foreign.links_dir().join("ay"), b"foreign").unwrap();
        std::os::unix::fs::symlink(foreign.links_dir(), l.links_dir()).unwrap();
        let error = linked_programs_checked(&l).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("real directory"));
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&foreign.prefix);
    }
}
