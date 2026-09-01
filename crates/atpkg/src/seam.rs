// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The seam owner (Lockstep, slice S1): atpkg OWNS the rustup toolchain seam.
//!
//! `rust-toolchain.toml` pins a rustup toolchain called `trust`, and rustup finds it
//! at `<rustup_home>/toolchains/trust`. That entry used to be a hand-made `rustup
//! toolchain link` naming wherever the operator last built the compiler, and it
//! dangled whenever the tree under it moved. This module makes the entry a symlink
//! atpkg lays and re-asserts: `<rustup_home>/toolchains/<name>` ->
//! `<prefix>/store/trust/current`. Every rustup-driven `cargo` in a pinned tree then
//! runs whatever build atpkg has live, and the link survives every update because
//! it names `current`, never a numbered build.
//!
//! The rules, all fail-closed:
//!
//! * Names come from a compiled-in allowlist ([`SEAM_NAMES`]); `trust` is the
//!   default. No other name is ever created or removed.
//! * If `<rustup_home>/toolchains` does not exist, rustup is not installed: attach is
//!   a no-op that says so. `~/.rustup` is NEVER created.
//! * An ABSENT entry is created atomically (temp symlink + `rename(2)` — the same
//!   primitive as `store/<p>/current`, [`crate::activate::atomic_symlink`]). An
//!   existing symlink that resolves into `<prefix>/store/trust/` is ADOPTED untouched,
//!   or re-pointed at `current` if it named a numbered build. Anything else — a real
//!   directory, a regular file, a symlink elsewhere — is REFUSED with the one fix,
//!   [`DETACH_FIX`]. Nothing here ever follows an existing link.
//! * A successful attach is RECORDED in `status.toml` as `seams = ["rustup:trust"]`
//!   (load, modify, save through the atomic writer — other fields are never clobbered).
//! * Detach removes the entry only when it is a symlink resolving into the prefix (or
//!   `--force`, which also moves a real directory ASIDE rather than deleting it: a
//!   tree atpkg did not lay is never recursively removed) and drops the record.
//! * The seam is RE-ASSERTED after every successful activation of `trust`, after a
//!   rollback of `trust`, and at the end of the unattended `update` pass, so a deleted
//!   or dangling entry heals on the next pass. Every recorded seam is re-asserted,
//!   plus a FIRST attach of `trust` when rustup is present and the entry is absent
//!   (creating a name nothing else owns is safe by construction).
//! * `uninstall --all` detaches every recorded seam with the toolset it removes.
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

/// `<prefix>/store/trust` — the only tree a seam may resolve into.
#[must_use]
pub fn owned_root(layout: &Layout) -> PathBuf {
    layout.prefix.join("store").join(SEAM_PROGRAM)
}

/// `<prefix>/store/trust/current` — what every seam targets.
#[must_use]
pub fn seam_target(layout: &Layout) -> PathBuf {
    layout.program_current(SEAM_PROGRAM)
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
    fn describe(&self, layout: &Layout) -> String {
        match self {
            Entry::Absent => "absent".to_string(),
            Entry::Link(raw) => format!(
                "a symlink to {} outside {}",
                raw.display(),
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

/// Whether an absolute target lies inside `<prefix>/store/trust/`: lexically first,
/// then by canonical path when both sides resolve (so a prefix reached through a
/// symlinked ancestor still counts). A dangling target decides lexically alone.
fn resolves_into_prefix(layout: &Layout, abs: &Path) -> bool {
    let root = normalize(&owned_root(layout));
    if abs.starts_with(&root) {
        return true;
    }
    match (std::fs::canonicalize(abs), std::fs::canonicalize(&root)) {
        (Ok(a), Ok(r)) => a.starts_with(&r),
        _ => false,
    }
}

/// Whether an absolute target IS `<prefix>/store/trust/current` — lexically only, on
/// purpose: canonicalizing `current` yields the numbered build it names, which would
/// make a link to that build look like a link to `current`.
fn targets_current(layout: &Layout, abs: &Path) -> bool {
    abs == normalize(&seam_target(layout))
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
    /// For a link: whether the target lies inside `<prefix>/store/trust/`.
    pub in_prefix: bool,
    /// For a link: whether the target is exactly `<prefix>/store/trust/current`.
    pub targets_current: bool,
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
    let targets_current = target
        .as_deref()
        .is_some_and(|t| targets_current(layout, t));
    Ok(Probe {
        path,
        entry,
        target,
        in_prefix,
        targets_current,
    })
}

/// What an [`attach`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attached {
    /// `<rustup_home>/toolchains` is absent: rustup is not installed, nothing done.
    NoRustup { key: String, toolchains: PathBuf },
    /// The entry was absent and is now a link to `current`.
    Created {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// The entry already linked to `current`; left byte-for-byte alone.
    Adopted {
        key: String,
        path: PathBuf,
        target: PathBuf,
    },
    /// The entry linked into the prefix but at a numbered build; now `current`.
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
        matches!(self, Attached::Created { .. } | Attached::Repointed { .. })
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
            Attached::Adopted { key, path, target } => write!(
                f,
                "{key}: adopted {} (already -> {})",
                path.display(),
                target.display()
            ),
            Attached::Repointed {
                key,
                path,
                from,
                to,
            } => write!(
                f,
                "{key}: re-pointed {} -> {} (was {}, a numbered build)",
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
    /// `<prefix>/store/trust/current` does not exist: there is nothing to point at.
    NotInstalled { target: PathBuf },
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

/// Attach the seam `name`: lay `<rustup_home>/toolchains/<name>` ->
/// `<prefix>/store/trust/current` and record it. See the module doc for the rules.
///
/// # Errors
/// [`Refusal`] — the name is not allowed, trust is not installed, the entry is not
/// atpkg's, or the filesystem failed. Nothing changed on any `Err`.
pub fn attach(layout: &Layout, rustup_home: &Path, name: &str) -> Result<Attached, Refusal> {
    if !name_allowed(name) {
        return Err(Refusal::BadName(name.to_string()));
    }
    let key = record_key(name);
    let toolchains = toolchains_dir(rustup_home);
    if !toolchains.is_dir() {
        return Ok(Attached::NoRustup { key, toolchains });
    }
    let target = seam_target(layout);
    let p = probe(layout, rustup_home, name)?;
    match p.entry {
        Entry::Absent => {
            if std::fs::symlink_metadata(&target).is_err() {
                return Err(Refusal::NotInstalled { target });
            }
            crate::activate::atomic_symlink(&target, &p.path)?;
            record(layout, name)?;
            Ok(Attached::Created {
                key,
                path: p.path,
                target,
            })
        }
        Entry::Link(raw) => {
            if !p.in_prefix {
                return Err(Refusal::Foreign {
                    what: Entry::Link(raw).describe(layout),
                    path: p.path,
                });
            }
            if p.targets_current {
                record(layout, name)?;
                return Ok(Attached::Adopted {
                    key,
                    path: p.path,
                    target,
                });
            }
            // A link into the prefix at a numbered build: `rename(2)` a fresh link over
            // it — the existing link is replaced, never followed.
            crate::activate::atomic_symlink(&target, &p.path)?;
            record(layout, name)?;
            Ok(Attached::Repointed {
                key,
                path: p.path,
                from: raw,
                to: target,
            })
        }
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
    /// For a link: inside `<prefix>/store/trust/`.
    pub in_prefix: bool,
    /// For a link: exactly `<prefix>/store/trust/current`.
    pub targets_current: bool,
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
            "{}: recorded={} path={} target={target} in-prefix={} targets-current={}",
            self.key,
            yn(self.recorded),
            self.path.display(),
            yn(self.in_prefix),
            yn(self.targets_current)
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
            targets_current: p.targets_current,
        },
        Err(e) => SeamStatus {
            key,
            recorded,
            path: seam_path(rustup_home, name),
            rustup_present,
            entry: Err(e.to_string()),
            target: None,
            in_prefix: false,
            targets_current: false,
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

fn fresh_status() -> crate::Status {
    crate::Status {
        schema: 1,
        ..Default::default()
    }
}

/// Add `rustup:<name>` to `status.toml`'s `seams` (load, modify, save). Idempotent.
fn record(layout: &Layout, name: &str) -> io::Result<()> {
    let key = record_key(name);
    let mut s = crate::status::read(layout).unwrap_or_else(fresh_status);
    if s.seams.contains(&key) {
        return Ok(());
    }
    s.seams.push(key);
    s.seams.sort();
    s.seams.dedup();
    crate::status::write(layout, &s)
}

/// Drop `rustup:<name>` from `status.toml`'s `seams`. No record ⇒ nothing to do.
fn unrecord(layout: &Layout, name: &str) -> io::Result<()> {
    let key = record_key(name);
    let Some(mut s) = crate::status::read(layout) else {
        return Ok(());
    };
    let before = s.seams.len();
    s.seams.retain(|k| *k != key);
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
    if !names.contains(DEFAULT_SEAM)
        && matches!(
            inspect(&seam_path(rustup_home, DEFAULT_SEAM)),
            Ok(Entry::Absent)
        )
        && std::fs::symlink_metadata(seam_target(layout)).is_ok()
    {
        names.insert(DEFAULT_SEAM.to_string());
    }
    for name in names {
        match attach(layout, rustup_home, &name) {
            Ok(a) if a.changed() => lines.push(a.to_string()),
            Ok(_) => {}
            Err(e) => lines.push(e.to_string()),
        }
    }
    lines
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

        /// Lay `store/trust/<build>/` and `store/trust/current -> <build>`.
        fn install_trust(&self, build: u64) -> PathBuf {
            let dir = self.layout.build_dir("trust", build);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            crate::activate::atomic_symlink(&dir, &seam_target(&self.layout)).unwrap();
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
            seam_target(&fx.layout),
            "the link names `current`, never the numbered build"
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
        assert!(after.recorded && after.in_prefix && after.targets_current);
        let line = after.to_string();
        assert!(
            line.starts_with("rustup:trust: recorded=yes path="),
            "{line}"
        );
        assert!(line.contains("in-prefix=yes targets-current=yes"), "{line}");
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
        link(&seam_target(&fx.layout), &fx.seam("trust"));
        let ino_before = std::fs::symlink_metadata(fx.seam("trust")).unwrap();
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::Adopted { .. }), "{out}");
        assert!(out.to_string().contains("adopted"));
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout)
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
        // `toolchains/trust -> ../../prefix/store/trust/current` — relative to the link.
        link(
            Path::new("../../prefix/store/trust/current"),
            &fx.seam("trust"),
        );
        let p = probe(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(p.in_prefix && p.targets_current, "{p:?}");
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        assert!(matches!(out, Attached::Adopted { .. }), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn numbered_build_link_is_repointed_to_current() {
        let fx = Fixture::new("repoint");
        let build = fx.install_trust(6808);
        link(&build, &fx.seam("trust"));
        let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
        match &out {
            Attached::Repointed { from, to, .. } => {
                assert_eq!(from, &build);
                assert_eq!(to, &seam_target(&fx.layout));
            }
            other => panic!("expected Repointed, got {other}"),
        }
        assert!(out.changed());
        assert!(out.to_string().contains("re-pointed"), "{out}");
        assert!(out.to_string().contains("numbered build"), "{out}");
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout)
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
            s.to_string().contains("in-prefix=no targets-current=no"),
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
        // The store's own `current` is untouched — only the rustup entry went.
        assert!(std::fs::symlink_metadata(seam_target(&fx.layout)).is_ok());

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
            seam_target(&fx.layout)
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
}
