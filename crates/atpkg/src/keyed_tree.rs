// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! KEYED TREES (`docs/DESIGN-disk-custody-2026-11-12.md` §5): a build tree another project's
//! build reads whole — Trust's LLVM build tree, its stage-0 seed — named by a key that says
//! what it was built from, never by a build number.
//!
//! A tree is packed whole by `atpkg-publish pack-tree` into a signed manifest whose one row is
//! `kind = "keyed-tree"` with its `key` and the `key_inputs` it is the SHA-256 of inside the
//! signed bytes ([`crate::manifest`] refuses a row whose inputs do not hash to its key). The
//! signed index maps each published key to the build that carries it (`[trees.<program>]`,
//! [`crate::manifest::Index::tree_build`]), and [`fetch`] takes a key's pack from LAN peers and
//! the release host at once, as every program's archive is taken ([`crate::flow::Fetcher`]).
//!
//! [`install`] lays a verified pack at `trees.noindex/<program>/<target>/<key>/` only when the
//! signed key is the key asked for: a pack of any other key is refused, so no Mac is ever handed
//! a tree built from something else. The store's `.noindex` folder keeps every tree, its stage
//! scratch and its records out of Spotlight. A tree is laid once, at a free name
//! ([`crate::install`]'s fresh stage): a key names its bytes, so a tree is never swapped over.
//! Its archive is kept as a LAN unit beside it ([`crate::peers::keep`]), served by
//! `aterm pkg serve-units` as every program's is.
//!
//! A tree leaves the store only as copy-on-write clones ([`hand_out`], over
//! [`crate::clone::cow_clone_file`]): a destination on another volume, or on a volume that
//! cannot clone, is refused and no file is ever byte-copied, so a worktree that takes one holds
//! no private bytes until it writes. A tree shims no tool and no channel pins it; the update
//! pass never installs one ([`crate::dispatch`] has no strategy for the kind). `aterm pkg gc`
//! evicts a tree no hand-out used for [`KEEP`], keeping each program's most recently used one
//! per target, and the scratch an interrupted install left ([`sweep_idle`]).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::manifest::PkgManifest;
use crate::store::Layout;

/// The `[[artifact]] kind` of a keyed tree's row.
pub const KIND: &str = "keyed-tree";

/// The store directory every keyed tree lives under: `<prefix>/trees.noindex`, whose name keeps
/// Spotlight out of every tree, stage and record below it.
pub const TREES_DIR: &str = "trees.noindex";

/// The most bytes a row's signed `key_inputs` (the text its key is the SHA-256 of) may hold.
pub const KEY_INPUTS_MAX: usize = 64 * 1024;

/// The record beside a tree whose mtime is its last use (`<key>.used`).
const USED_SUFFIX: &str = ".used";

/// The lock an install, a hand-out and an eviction of one tree take (`<key>.lock`). It is never
/// removed: a lock file removed while another process waits on it would let two holders in.
const LOCK_SUFFIX: &str = ".lock";

/// How long a tree no hand-out used stays in the store: two weeks, so a worktree made after a
/// pause still clones instead of fetching. Each program's most recently used tree stays anyway.
pub const KEEP: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// How long a hand-out waits for an eviction of the same tree to finish.
const HAND_OUT_WAIT: Duration = Duration::from_secs(60);

/// One program the store holds as keyed trees, and what its key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyedProgram {
    /// The program name (the manifest's `program`).
    pub name: &'static str,
    /// The repository its packs are released on.
    pub repo: &'static str,
    /// What its key names, for the refusal a person reads.
    pub key_names: &'static str,
    /// The completion stamp at the tree's root whose text its signed `key_inputs` must name:
    /// the build that wrote the tree records what it was built from there, so a pack is never
    /// signed for inputs other than the tree's own.
    pub stamp: Option<&'static str>,
}

/// Every keyed-tree program this client installs. `atpkg-publish`'s table holds a `Tree` row for
/// each, and only for these.
pub const TREES: &[KeyedProgram] = &[
    KeyedProgram {
        name: "llvm-tree",
        repo: "trust",
        key_names: "the LLVM sources, the `[llvm]` configuration and the host it was built on",
        // Trust's bootstrap: `BuildStamp::new(llvm_root).with_prefix("llvm")`.
        stamp: Some(".llvm-stamp"),
    },
    KeyedProgram {
        name: "trust-stage0",
        repo: "trust",
        key_names: "the channel manifest hash Trust's `src/stage0` pins",
        stamp: None,
    },
];

/// The keyed-tree program named `name`.
#[must_use]
pub fn program(name: &str) -> Option<&'static KeyedProgram> {
    TREES.iter().find(|p| p.name == name)
}

/// A tree's key: 64 lowercase hex, a SHA-256 over what it was built from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeKey(String);

impl TreeKey {
    /// `text` as a key, when it is 64 lowercase hex.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        aterm_codec::hex::is_lower(text, 64).then(|| Self(text.to_string()))
    }

    /// The key of `inputs`: their SHA-256.
    #[must_use]
    pub fn of_inputs(inputs: &[u8]) -> Self {
        Self(crate::tree::sha256_bytes(inputs))
    }

    /// The key's hex.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TreeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a manifest row's `key` and `key_inputs` stand together: a keyed tree's row carries a
/// key and the inputs it is the SHA-256 of (at most [`KEY_INPUTS_MAX`] bytes), and every other
/// row carries neither.
#[must_use]
pub fn row_is_bound(kind: &str, key: &str, key_inputs: &str) -> bool {
    if kind != KIND {
        return key.is_empty() && key_inputs.is_empty();
    }
    !key_inputs.is_empty()
        && key_inputs.len() <= KEY_INPUTS_MAX
        && TreeKey::parse(key).is_some_and(|k| k == TreeKey::of_inputs(key_inputs.as_bytes()))
}

/// Why a keyed tree was not installed or handed out.
#[derive(Debug)]
pub enum TreeError {
    /// The program is not a keyed-tree program of this client ([`TREES`]).
    NotKeyed(String),
    /// The signed manifest is not a keyed tree's for this host.
    Manifest(String),
    /// The signed pack is the tree of another key: never installed under the one asked for.
    OtherKey {
        /// The program.
        program: String,
        /// The key asked for.
        asked: TreeKey,
        /// The key the pack is signed for.
        signed: String,
    },
    /// The signed index names no build of the program for that key.
    NotPublished {
        /// The program.
        program: String,
        /// The key asked for.
        key: TreeKey,
    },
    /// The pack's manifest or archive could not be fetched.
    Fetch(String),
    /// The store holds no complete tree of that key.
    Absent {
        /// The program.
        program: String,
        /// The key asked for.
        key: TreeKey,
    },
    /// The archive's digest, extraction or tree root refused it.
    Stage(crate::install::StageError),
    /// A filesystem step failed (a clone across volumes among them).
    Io(io::Error),
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotKeyed(p) => write!(f, "{p} is not a keyed tree this client installs"),
            Self::Manifest(why) | Self::Fetch(why) => f.write_str(why),
            Self::OtherKey {
                program,
                asked,
                signed,
            } => {
                let names =
                    self::program(program).map_or("what it was built from", |p| p.key_names);
                write!(
                    f,
                    "the {program} pack is signed for key {signed}, not {asked}: a key names \
                     {names}, and a tree of another key is never installed or handed out under it"
                )
            }
            Self::NotPublished { program, key } => write!(
                f,
                "the signed index names no {program} pack of key {key}: no builder has packed \
                 that tree yet"
            ),
            Self::Absent { program, key } => {
                write!(
                    f,
                    "this store holds no complete {program} tree of key {key}"
                )
            }
            Self::Stage(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TreeError {}

impl From<io::Error> for TreeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// `<prefix>/trees.noindex`: every keyed tree the store holds.
#[must_use]
pub fn root(layout: &Layout) -> PathBuf {
    layout.prefix.join(TREES_DIR)
}

/// `<prefix>/trees.noindex/<program>/<target>/<key>`: where the tree of `key` for `target` is
/// installed. The target is part of the name, so a key that does not itself name the host (the
/// stage-0 seed's manifest hash) never answers for another target's tree.
#[must_use]
pub fn dir(layout: &Layout, program: &KeyedProgram, target: &str, key: &TreeKey) -> PathBuf {
    root(layout)
        .join(program.name)
        .join(target)
        .join(key.as_str())
}

fn keyed(name: &str) -> Result<&'static KeyedProgram, TreeError> {
    program(name).ok_or_else(|| TreeError::NotKeyed(name.to_string()))
}

fn target_named(target: &str) -> Result<(), TreeError> {
    if crate::manifest::TARGETS.contains(&target) {
        Ok(())
    } else {
        Err(TreeError::Manifest(format!(
            "{target} is not a target atpkg installs for"
        )))
    }
}

/// The store's complete tree of `key` for `target`, when it holds one: never a tree of another
/// key or target, and never one an interrupted install left unmarked. Crate-private: a build
/// reads a tree only through [`hand_out`]'s clones, which `aterm pkg gc` never takes from under
/// it; [`holds`] answers whether it is there.
pub(crate) fn installed(
    layout: &Layout,
    program: &str,
    target: &str,
    key: &TreeKey,
) -> Option<PathBuf> {
    let at = dir(layout, self::program(program)?, target, key);
    let real_dir = std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_dir());
    (real_dir && crate::store::build_is_complete(&at)).then_some(at)
}

/// Whether the store holds a complete tree of `key` for `target`.
#[must_use]
pub fn holds(layout: &Layout, program: &str, target: &str, key: &TreeKey) -> bool {
    installed(layout, program, target, key).is_some()
}

/// Install `pkg`'s tree for `target` from `archive` under `key`: refused unless `pkg` is a
/// keyed-tree program's manifest (verified by the caller under the roster) whose `target` row is
/// a keyed tree signed for exactly `key`, with signed inputs that hash to it. The archive is held
/// to the signed digest and the extracted tree to the signed root, laid whole at a free
/// `trees.noindex/<program>/<target>/<key>`, marked ready, and kept as a LAN unit beside it. A
/// complete tree of `key` already in the store is kept as it is; one an interrupted install left,
/// and its stage scratch, are removed first. A second install of the same key waits for the
/// first and keeps what it laid. Returns the store's directory, which only [`hand_out`] reads.
///
/// # Errors
/// [`TreeError`]: the manifest's shape, another key, the stage's refusal, or a store write.
pub fn install(
    layout: &Layout,
    pkg: &PkgManifest,
    key: &TreeKey,
    target: &str,
    archive: &Path,
) -> Result<PathBuf, TreeError> {
    let (program, artifact) = admit(pkg, key, target)?;
    let at = dir(layout, program, target, key);
    if installed(layout, program.name, target, key).is_some() {
        return Ok(at);
    }
    let _lock = lock_waiting(&at)?;
    install_held(layout, program, artifact, (key, target), archive)
}

/// The keyed-tree program and `target` row of `pkg`, when it is a tree signed for exactly `key`
/// with signed inputs that hash to it.
fn admit<'p>(
    pkg: &'p PkgManifest,
    key: &TreeKey,
    target: &str,
) -> Result<(&'static KeyedProgram, &'p crate::manifest::Artifact), TreeError> {
    let program = keyed(&pkg.program)?;
    target_named(target)?;
    if !pkg.exposes.is_empty() {
        return Err(TreeError::Manifest(format!(
            "the {} manifest exposes [{}]: a keyed tree shims no tool",
            program.name,
            pkg.exposes.join(", ")
        )));
    }
    let artifact = pkg.artifact_for(target).ok_or_else(|| {
        TreeError::Manifest(format!("the {} pack has no {target} row", program.name))
    })?;
    if artifact.kind != KIND {
        return Err(TreeError::Manifest(format!(
            "the {} pack's {target} row is a {:?}, not a {KIND}",
            program.name, artifact.kind
        )));
    }
    if artifact.key != key.as_str() {
        return Err(TreeError::OtherKey {
            program: program.name.to_string(),
            asked: key.clone(),
            signed: artifact.key.clone(),
        });
    }
    if !row_is_bound(&artifact.kind, &artifact.key, &artifact.key_inputs) {
        return Err(TreeError::Manifest(format!(
            "the {} pack's signed key_inputs do not hash to its key {key}",
            program.name
        )));
    }
    Ok((program, artifact))
}

/// Take the lock of the tree at `at`, waiting for an install of the same key, which may be
/// extracting for minutes, rather than failing.
fn lock_waiting(at: &Path) -> io::Result<aterm_update_core::FileLock> {
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent)?;
    }
    aterm_update_core::FileLock::acquire(&sidecar(at, LOCK_SUFFIX)?)
}

/// [`install`]'s steps under the tree's lock.
fn install_held(
    layout: &Layout,
    program: &KeyedProgram,
    artifact: &crate::manifest::Artifact,
    (key, target): (&TreeKey, &str),
    archive: &Path,
) -> Result<PathBuf, TreeError> {
    let at = dir(layout, program, target, key);
    // Re-read under the lock: another install of this key may have laid it meanwhile.
    if installed(layout, program.name, target, key).is_some() {
        return Ok(at);
    }
    discard(&at)?;
    // The stage scratch only: a fetch's download, which may be `archive` itself, stays.
    remove_scratch(&at, Scratch::Stage);
    crate::install::stage_fresh_tree(artifact, archive, &at).map_err(TreeError::Stage)?;
    crate::store::mark_build_ready(&at)?;
    crate::peers::keep(layout, archive, &artifact.sha256, &at);
    mark_used(&at)?;
    Ok(at)
}

/// The tree of `key` for `target`, installed: the store's own when it holds it, else the pack the
/// signed `index` names for that key ([`crate::manifest::Index::tree_build`]), its manifest
/// verified under the index's roster and its archive taken from LAN peers and the release host
/// at once ([`crate::flow::Fetcher::download_unit`]), then installed. No GitHub credential is
/// needed: every read is anonymous. The download runs under the tree's lock, so a second fetch of
/// the key waits for the first and `aterm pkg gc` leaves it alone.
///
/// # Errors
/// [`TreeError::NotPublished`] where the index names no pack of `key`; a manifest that does not
/// verify, is another build's or another key's; a fetch that failed; [`install`]'s refusals.
pub fn fetch(
    layout: &Layout,
    fetcher: &dyn crate::flow::Fetcher,
    index: &crate::sig::TrustedIndex,
    program: &str,
    target: &str,
    key: &TreeKey,
) -> Result<PathBuf, TreeError> {
    let program = keyed(program)?;
    target_named(target)?;
    if let Some(at) = installed(layout, program.name, target, key) {
        return Ok(at);
    }
    let build = index
        .tree_build(program.name, key.as_str())
        .ok_or_else(|| TreeError::NotPublished {
            program: program.name.to_string(),
            key: key.clone(),
        })?;
    let (raw, sig) = fetcher
        .pkg_manifest(program.repo, program.name, build)
        .map_err(TreeError::Fetch)?;
    let verified = index.verify_pkg(raw, &sig).map_err(|e| {
        TreeError::Manifest(format!(
            "the {} manifest of build {build} does not verify under the index's roster: {e:?}",
            program.name
        ))
    })?;
    let pkg = crate::manifest::parse_pkg(&verified).map_err(|e| {
        TreeError::Manifest(format!(
            "the {} manifest of build {build} does not parse: {e:?}",
            program.name
        ))
    })?;
    if !pkg.is_for(program.name) || pkg.build_number != build {
        return Err(TreeError::Manifest(format!(
            "the signed manifest fetched as {}@{build} is {}@{}",
            program.name, pkg.program, pkg.build_number
        )));
    }
    let (_, artifact) = admit(&pkg, key, target)?;
    let at = dir(layout, program, target, key);
    let _lock = lock_waiting(&at)?;
    if installed(layout, program.name, target, key).is_some() {
        return Ok(at);
    }
    // Under the lock no other fetch of this key is downloading: what is there is left over.
    remove_scratch(&at, Scratch::All);
    let download = sidecar(&at, &format!(".download-{}", std::process::id()))?;
    let got = fetcher
        .download_unit(program.name, program.repo, artifact, &download)
        .map_err(TreeError::Fetch)
        .and_then(|()| install_held(layout, program, artifact, (key, target), &download));
    // The kept unit is a link of its own ([`crate::peers::keep`]): this name goes either way.
    let _ = std::fs::remove_file(&download);
    got
}

/// What [`hand_out`] laid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HandOut {
    /// Regular files, each a copy-on-write clone.
    pub files: u64,
    /// Directories made.
    pub dirs: u64,
}

/// Hand the tree of `key` for `target` out at `dest` (which must not exist; its parent must) as
/// copy-on-write clones: every regular file a clone of the store's that the kernel makes
/// ([`crate::clone::cow_clone_file`]), every directory made with the tree's mode. A destination
/// on another volume, or on a volume that cannot clone, is refused, never byte-copied; what was
/// laid before a refusal is removed. The hand-out is the tree's use, which keeps it from
/// `aterm pkg gc` for [`KEEP`].
///
/// # Errors
/// [`TreeError::Absent`] when the store holds no complete tree of `key` (a tree of another key
/// is never handed out), or a filesystem step, a clone the volume cannot make among them.
pub fn hand_out(
    layout: &Layout,
    program: &str,
    target: &str,
    key: &TreeKey,
    dest: &Path,
) -> Result<HandOut, TreeError> {
    let program = keyed(program)?;
    let absent = || TreeError::Absent {
        program: program.name.to_string(),
        key: key.clone(),
    };
    let src = installed(layout, program.name, target, key).ok_or_else(absent)?;
    let _lock = lock(&src, HAND_OUT_WAIT)?;
    // Re-read under the lock: an eviction that held it may have taken the tree.
    installed(layout, program.name, target, key).ok_or_else(absent)?;
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(TreeError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; a tree is handed out only at a free name",
                dest.display()
            ),
        )));
    }
    mark_used(&src)?;
    std::fs::create_dir(dest)?;
    let mut laid = HandOut {
        dirs: 1,
        ..HandOut::default()
    };
    match clone_into(&src, dest, &mut laid) {
        Ok(()) => Ok(laid),
        Err(e) => {
            let _ = remove_handed(dest);
            Err(TreeError::Io(e))
        }
    }
}

/// Clone the contents of `src` into the existing directory `dst`, then give `dst` `src`'s mode.
/// A signed tree holds only files and directories (the pack writer and the extractor admit no
/// link), so anything else in the store's copy is refused.
fn clone_into(src: &Path, dst: &Path, laid: &mut HandOut) -> io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir(&to)?;
            laid.dirs += 1;
            clone_into(&from, &to, laid)?;
        } else if kind.is_file() {
            crate::clone::cow_clone_file(&from, &to)?;
            laid.files += 1;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is neither a file nor a directory; a signed tree holds no other kind",
                    from.display()
                ),
            ));
        }
    }
    std::fs::set_permissions(dst, std::fs::symlink_metadata(src)?.permissions())
}

/// Remove a partial hand-out, making its directories writable first (a tree may hold read-only
/// ones).
fn remove_handed(dest: &Path) -> io::Result<()> {
    make_writable(dest);
    std::fs::remove_dir_all(dest)
}

fn make_writable(dir: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mut perms = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        perms.set_mode(perms.mode() | 0o700);
    }
    #[cfg(not(unix))]
    perms.set_readonly(false);
    let _ = std::fs::set_permissions(dir, perms);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            make_writable(&entry.path());
        }
    }
}

fn sidecar(at: &Path, suffix: &str) -> io::Result<PathBuf> {
    crate::store::sidecar_path(at, suffix).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no name", at.display()),
        )
    })
}

/// Take the tree's lock within `wait`.
fn lock(at: &Path, wait: Duration) -> io::Result<aterm_update_core::FileLock> {
    aterm_update_core::FileLock::acquire_within(&sidecar(at, LOCK_SUFFIX)?, wait)
}

/// Record a use of the tree at `at`: its `.used` record's mtime is now.
fn mark_used(at: &Path) -> io::Result<()> {
    let file = std::fs::File::create(sidecar(at, USED_SUFFIX)?)?;
    file.set_modified(SystemTime::now())
}

/// When the tree at `at` was last used: its `.used` record, else its ready marker.
fn last_used(at: &Path) -> Option<SystemTime> {
    let used = sidecar(at, USED_SUFFIX).ok()?;
    let marker = crate::store::ready_marker_path(at)?;
    [used, marker]
        .iter()
        .find_map(|p| std::fs::symlink_metadata(p).and_then(|m| m.modified()).ok())
}

/// Remove the tree at `at` and its records, its ready marker first so a torn removal reads as
/// not installed. A tree that is not there is no error. The lock file stays ([`LOCK_SUFFIX`]).
fn discard(at: &Path) -> io::Result<()> {
    crate::store::clear_build_ready(at)?;
    if std::fs::symlink_metadata(at).is_ok() {
        make_writable(at);
        crate::store::remove_tree(at)?;
    }
    for suffix in [crate::peers::UNIT_SUFFIX, USED_SUFFIX] {
        match std::fs::remove_file(sidecar(at, suffix)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Which leftovers [`remove_scratch`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scratch {
    /// An interrupted install's stage scratch (`<key>.incoming-…`).
    Stage,
    /// That and an interrupted fetch's downloads (`<key>.download-…`).
    All,
}

/// Remove what an interrupted install or fetch of the tree at `at` left ([`Scratch`]). The
/// caller holds the tree's lock, so no install of that key is staging into one and no fetch of it
/// is downloading. Returns how many it removed.
fn remove_scratch(at: &Path, which: Scratch) -> usize {
    let (Some(parent), Some(name)) = (at.parent(), at.file_name().and_then(|n| n.to_str())) else {
        return 0;
    };
    let incoming = format!("{name}.incoming-");
    let download = format!("{name}.download-");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Some(n) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let gone = if n.starts_with(&incoming) && kind.is_dir() {
            remove_handed(&entry.path()).is_ok()
        } else if which == Scratch::All && n.starts_with(&download) && kind.is_file() {
            std::fs::remove_file(entry.path()).is_ok()
        } else {
            false
        };
        removed += usize::from(gone);
    }
    removed
}

/// One keyed tree directory the store holds, complete or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The program.
    pub program: &'static KeyedProgram,
    /// The target it was installed for.
    pub target: String,
    /// Its key.
    pub key: TreeKey,
    /// Its directory.
    pub at: PathBuf,
}

/// Every key the store holds anything of, a tree or an interrupted install's scratch, for each
/// program and target, in key order.
fn keys_present(layout: &Layout) -> Vec<Tree> {
    let mut out = Vec::new();
    for program in TREES {
        let program_root = root(layout).join(program.name);
        let Ok(targets) = std::fs::read_dir(&program_root) else {
            continue;
        };
        let mut targets: Vec<String> = targets
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|t| crate::manifest::TARGETS.contains(&t.as_str()))
            .collect();
        targets.sort();
        for target in targets {
            let Ok(entries) = std::fs::read_dir(program_root.join(&target)) else {
                continue;
            };
            let mut keys: Vec<TreeKey> = entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    let key = name.split_once('.').map_or(name.as_str(), |(k, _)| k);
                    TreeKey::parse(key)
                })
                .collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let at = dir(layout, program, &target, &key);
                out.push(Tree {
                    program,
                    target: target.clone(),
                    key,
                    at,
                });
            }
        }
    }
    out
}

/// Every complete keyed tree the store holds, each program's per target in key order.
#[must_use]
pub fn list(layout: &Layout) -> Vec<Tree> {
    keys_present(layout)
        .into_iter()
        .filter(|t| installed(layout, t.program.name, &t.target, &t.key).is_some())
        .collect()
}

/// What `aterm pkg gc`'s pass over the keyed trees removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sweep {
    /// `(program, keys)` of trees evicted for idleness.
    pub evicted: Vec<(String, Vec<String>)>,
    /// `(program, keys)` whose interrupted install or fetch left a tree with no ready marker,
    /// stage scratch or a download, now removed.
    pub interrupted: Vec<(String, Vec<String>)>,
}

fn push_key(into: &mut Vec<(String, Vec<String>)>, program: &str, key: &TreeKey) {
    match into.iter_mut().find(|(p, _)| p == program) {
        Some((_, keys)) => keys.push(key.to_string()),
        None => into.push((program.to_string(), vec![key.to_string()])),
    }
}

/// Whether a tree last used at `used` is idle at `now`: used more than `keep` before it. A tree
/// with no readable use is not (no verdict on it).
fn idle(used: Option<SystemTime>, now: SystemTime, keep: Duration) -> bool {
    used.is_some_and(|used| now.duration_since(used).unwrap_or_default() > keep)
}

/// The complete trees `aterm pkg gc` would evict at `now` ([`sweep_idle`]'s rule, read without
/// a lock), each with the most recently used tree of its program and target, which stays.
fn idle_trees(
    layout: &Layout,
    now: SystemTime,
    keep: Duration,
    in_scope: &dyn Fn(&str) -> bool,
) -> Vec<(Tree, TreeKey)> {
    type Group = ((&'static str, String), Vec<(Option<SystemTime>, Tree)>);
    let mut groups: Vec<Group> = Vec::new();
    for tree in list(layout) {
        if !in_scope(tree.program.name) {
            continue;
        }
        let used = last_used(&tree.at);
        let group = (tree.program.name, tree.target.clone());
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, trees)) => trees.push((used, tree)),
            None => groups.push((group, vec![(used, tree)])),
        }
    }
    let mut out = Vec::new();
    for (_, mut trees) in groups {
        // The most recently used first; it stays however old.
        trees.sort_by_key(|tree| std::cmp::Reverse(tree.0));
        let Some(newest) = trees.first().map(|(_, t)| t.key.clone()) else {
            continue;
        };
        for (used, tree) in trees.into_iter().skip(1) {
            if idle(used, now, keep) {
                out.push((tree, newest.clone()));
            }
        }
    }
    out
}

/// One tree `aterm pkg gc` would evict now, for the disk report's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evictable {
    /// The tree.
    pub tree: Tree,
    /// The key of its program's most recently used tree on its target, which stays.
    pub newest: TreeKey,
}

/// Every tree `aterm pkg gc` would evict at `now`: not its program's most recently used on its
/// target, and unused for [`KEEP`]. Read only; [`sweep_idle`] decides again under each lock.
#[must_use]
pub fn evictable(layout: &Layout, now: SystemTime) -> Vec<Evictable> {
    idle_trees(layout, now, KEEP, &|_| true)
        .into_iter()
        .map(|(tree, newest)| Evictable { tree, newest })
        .collect()
}

/// The keyed-tree stores of the account at `home` (`<prefix>/trees.noindex`), for the disk
/// budget's sizing: refetchable bytes aterm's `aterm pkg gc` evicts.
#[must_use]
pub fn roots(home: &Path) -> Vec<PathBuf> {
    vec![root(&Layout {
        prefix: crate::store::default_prefix(home),
    })]
}

/// `aterm pkg gc`'s pass over the keyed trees: evict every complete tree whose last use is more
/// than `keep` before `now`, except each program's most recently used one per target, and remove
/// what an interrupted install or fetch left of any key. A tree an install, a hand-out or another
/// eviction holds the lock of at that moment is left, and a tree's last use is read again under
/// its lock, so a hand-out that used it meanwhile keeps it.
#[must_use]
pub fn sweep_idle(
    layout: &Layout,
    now: SystemTime,
    keep: Duration,
    in_scope: &dyn Fn(&str) -> bool,
) -> Sweep {
    let mut sweep = Sweep::default();
    for tree in keys_present(layout) {
        if !in_scope(tree.program.name)
            || installed(layout, tree.program.name, &tree.target, &tree.key).is_some()
        {
            continue;
        }
        let Ok(_lock) = lock(&tree.at, Duration::ZERO) else {
            continue;
        };
        if installed(layout, tree.program.name, &tree.target, &tree.key).is_some() {
            continue;
        }
        let had_dir = std::fs::symlink_metadata(&tree.at).is_ok();
        let scratch = remove_scratch(&tree.at, Scratch::All);
        if discard(&tree.at).is_ok() && (had_dir || scratch > 0) {
            push_key(&mut sweep.interrupted, tree.program.name, &tree.key);
        }
    }
    for (tree, _) in idle_trees(layout, now, keep, in_scope) {
        let Ok(_lock) = lock(&tree.at, Duration::ZERO) else {
            continue;
        };
        // Read again under the lock: a hand-out that held it may have used the tree.
        if !idle(last_used(&tree.at), now, keep) {
            continue;
        }
        if discard(&tree.at).is_ok() {
            remove_scratch(&tree.at, Scratch::All);
            push_key(&mut sweep.evicted, tree.program.name, &tree.key);
        }
    }
    for list in [&mut sweep.evicted, &mut sweep.interrupted] {
        list.sort();
        for (_, keys) in list.iter_mut() {
            keys.sort();
        }
    }
    sweep
}

/// Every LAN unit kept beside a tree (`<key>.unit`, [`crate::peers::keep`]), for
/// `serve-units --off`, which keeps none.
pub(crate) fn kept_units(layout: &Layout) -> Vec<PathBuf> {
    keys_present(layout)
        .into_iter()
        .filter_map(|t| crate::store::sidecar_path(&t.at, crate::peers::UNIT_SUFFIX))
        .filter(|unit| std::fs::symlink_metadata(unit).is_ok_and(|m| m.is_file()))
        .collect()
}

/// `aterm pkg uninstall --all`'s removal of every keyed tree: each under its lock, then the
/// whole `trees.noindex` when no hand-out held one. Returns the keys a hand-out held, which stay.
///
/// # Errors
/// A tree or the store directory could not be removed.
pub fn remove_all(layout: &Layout) -> io::Result<Vec<String>> {
    let mut held = Vec::new();
    for tree in keys_present(layout) {
        let Ok(_lock) = lock(&tree.at, Duration::ZERO) else {
            held.push(format!("{} {}", tree.program.name, tree.key));
            continue;
        };
        discard(&tree.at)?;
        remove_scratch(&tree.at, Scratch::All);
    }
    let all = root(layout);
    if held.is_empty() && std::fs::symlink_metadata(&all).is_ok() {
        make_writable(&all);
        crate::store::remove_tree(&all)?;
    }
    Ok(held)
}

#[cfg(all(test, unix))]
#[path = "keyed_tree_tests.rs"]
mod tests;
