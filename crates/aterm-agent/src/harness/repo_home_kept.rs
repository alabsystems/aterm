// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a home's reads keep between passes ([`Kept`]): git's own answers about a checkout's
//! configuration, its refs and its distance behind its origin's main, each under a digest of the
//! exact bytes of the files git answered from ([`Seen`]). A pass reads those bytes again, and
//! starts a git only where they moved: an unchanged checkout starts none for these answers, and
//! its one `git status` is all that is left, since nothing but a look at the working tree says
//! whether it is dirty.
//!
//! The freshness of a read is what it was: every field it gives is true of bytes the pass itself
//! read from the checkout. An answer is kept only where the bytes read before its git started
//! and after it ended are the same files with the same contents and git answered, it is given
//! again only where the bytes read now are those, and only where its git could have started
//! ([`Reads::start_by`](super::super::Reads::start_by)): a pass held back says nothing.
//!
//! Git reads more than this module names for some repositories, and for those nothing is kept
//! and git is asked at every pass as before ([`config_seen`], [`refs_seen`]): a configuration
//! that includes another file, a `.git` that shares another's (`commondir`) or whose `objects` or
//! `refs` is a link, a ref store other than loose files and `packed-refs`, a symbolic ref to a
//! symbolic ref, any replace ref, objects borrowed from another repository, a file past its bound,
//! or anything that is not a regular file where git reads one. What a kept answer takes on trust
//! is the repository's own object store, as git does: an object a ref names stays in it, and is
//! the object its name says.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use aterm_sysprobe::volumes::Table;

use super::super::{Reads, is_head};

/// The most of a configuration file a reading takes: one past it is not vouched for.
const CONFIG_MAX: u64 = 1024 * 1024;

/// The most of `HEAD` or a loose ref a reading takes.
const REF_FILE_MAX: u64 = 1024;

/// The most of `packed-refs` a reading takes.
const PACKED_REFS_MAX: u64 = 8 * 1024 * 1024;

/// The most of `shallow` or `info/grafts` a reading takes.
const HISTORY_FILE_MAX: u64 = 1024 * 1024;

/// A digest of everything one reading took.
type Digest = [u8; 32];

/// What one reading of a checkout's evidence found.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Seen {
    /// The contents: which files are there, and their exact bytes. An answer is kept under this.
    key: Digest,
    /// Which files those were (device, inode, length, modification and change time): two
    /// readings around a git that agree in this too saw no write between them.
    files: Digest,
}

impl Seen {
    /// This reading with `parts` said as well: the evidence for an answer that turns on them.
    pub(super) fn with(&self, parts: &[&str]) -> Self {
        let mut key = aterm_digest::Sha256::new();
        key.update(self.key);
        for part in parts {
            put(&mut key, part.as_bytes());
        }
        Self {
            key: key.finalize(),
            files: self.files,
        }
    }
}

/// One length-framed field of a digest.
fn put(digest: &mut aterm_digest::Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

/// Which file `meta` is and when it last changed, `None` where this platform does not say (a
/// reading there vouches for nothing, and nothing is kept).
#[cfg(unix)]
fn file_stamp(meta: &std::fs::Metadata) -> Option<([u64; 3], [i64; 4])> {
    use std::os::unix::fs::MetadataExt as _;
    Some((
        [meta.dev(), meta.ino(), meta.len()],
        [
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        ],
    ))
}

#[cfg(not(unix))]
fn file_stamp(_: &std::fs::Metadata) -> Option<([u64; 3], [i64; 4])> {
    None
}

/// The owner and mode of the folder `meta` describes, which git's ownership and access checks
/// read, `None` where this platform does not say.
#[cfg(unix)]
fn folder_stamp(meta: &std::fs::Metadata) -> Option<[u32; 2]> {
    use std::os::unix::fs::MetadataExt as _;
    Some([meta.uid(), meta.mode()])
}

#[cfg(not(unix))]
fn folder_stamp(_: &std::fs::Metadata) -> Option<[u32; 2]> {
    None
}

/// A reading in progress. Each look returns `None` where it cannot vouch for what git would
/// read there, which ends the reading: nothing is kept under it.
struct Reading {
    key: aterm_digest::Sha256,
    files: aterm_digest::Sha256,
}

impl Reading {
    /// A reading for the answer `what` of the git that says `version`, in the checkout `root`.
    fn new(what: &str, version: &str, root: &Path) -> Self {
        let mut key = aterm_digest::Sha256::new();
        put(&mut key, b"aterm home answer v1");
        put(&mut key, what.as_bytes());
        put(&mut key, version.as_bytes());
        put(&mut key, root.as_os_str().as_encoded_bytes());
        Self {
            key,
            files: aterm_digest::Sha256::new(),
        }
    }

    /// The regular file at `path`, at most `max` bytes of it: `Some(None)` where nothing is
    /// there, and `None` where it is a link or no regular file, is longer, could not be read, or
    /// changed under the read.
    fn file(&mut self, label: &str, path: &Path, max: u64) -> Option<Option<Vec<u8>>> {
        use std::io::Read as _;
        put(&mut self.key, label.as_bytes());
        let (file, meta) = match aterm_uds::regular_file::open(path) {
            Ok(opened) => opened,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                put(&mut self.key, b"absent");
                return Some(None);
            }
            Err(_) => return None,
        };
        let stamp = file_stamp(&meta)?;
        if meta.len() > max {
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).ok()?.saturating_add(1));
        (&file)
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        if u64::try_from(bytes.len()).ok()? != meta.len()
            || file.metadata().ok().and_then(|now| file_stamp(&now)) != Some(stamp)
        {
            return None;
        }
        put(&mut self.key, b"file");
        put(&mut self.key, &bytes);
        for part in stamp.0 {
            self.files.update(part.to_be_bytes());
        }
        for part in stamp.1 {
            self.files.update(part.to_be_bytes());
        }
        Some(Some(bytes))
    }

    /// The folder at `path` itself (no link to one), with the owner and mode git's ownership and
    /// access checks read.
    fn folder(&mut self, label: &str, path: &Path) -> Option<()> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.is_dir() {
            return None;
        }
        put(&mut self.key, label.as_bytes());
        for part in folder_stamp(&meta)? {
            self.key.update(part.to_be_bytes());
        }
        Some(())
    }

    /// Nothing at `path`.
    fn nothing(&mut self, label: &str, path: &Path) -> Option<()> {
        std::fs::symlink_metadata(path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            .then(|| put(&mut self.key, label.as_bytes()))
    }

    /// A field that is no file: a name the answer was asked for.
    fn said(&mut self, text: &str) {
        put(&mut self.key, text.as_bytes());
    }

    fn done(self) -> Seen {
        Seen {
            key: self.key.finalize(),
            files: self.files.finalize(),
        }
    }
}

/// Whether a configuration file's `bytes` may name another file to read (`[include]`,
/// `[includeIf …]`): any spelling of the word, so a value that merely holds it counts too.
fn includes(bytes: &[u8]) -> bool {
    bytes
        .windows(b"include".len())
        .any(|window| window.eq_ignore_ascii_case(b"include"))
}

/// Whether `HEAD`'s `bytes` are a `HEAD` git takes this `.git` for a repository by (a symbolic
/// ref into `refs/`, or a commit's name): with any other, git looks for a repository in the
/// folders above, whose files no reading here takes.
fn head_names_a_repository(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let line = text.trim_end_matches(['\n', '\r']);
    line.strip_prefix("ref: refs/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains(char::is_whitespace))
        || is_head(line)
}

/// What says which repository `root` is, to every git a read starts in it: the checkout's folder
/// and its `.git`, `objects` and `refs` as folders of their own with their owners and modes, no
/// `commondir`, a `HEAD` that names a repository, and its own configuration with no include.
/// `HEAD`'s bytes are part of the reading only `with_head`: what git says of the configuration
/// turns on whether `HEAD` names a repository, not on which branch it names (no include, so no
/// `includeIf "onbranch:…"`), and a checkout that changes branch is not asked for it again.
fn repository(reading: &mut Reading, root: &Path, git_dir: &Path, with_head: bool) -> Option<()> {
    reading.folder("root", root)?;
    reading.folder("git", git_dir)?;
    reading.folder("objects", &git_dir.join("objects"))?;
    reading.folder("refs", &git_dir.join("refs"))?;
    reading.nothing("commondir", &git_dir.join("commondir"))?;
    let head = if with_head {
        reading.file("HEAD", &git_dir.join("HEAD"), REF_FILE_MAX)??
    } else {
        Reading::new("HEAD", "", root).file("HEAD", &git_dir.join("HEAD"), REF_FILE_MAX)??
    };
    if !head_names_a_repository(&head) {
        return None;
    }
    for name in ["config", "config.worktree"] {
        if reading
            .file(name, &git_dir.join(name), CONFIG_MAX)?
            .is_some_and(|bytes| includes(&bytes))
        {
            return None;
        }
    }
    Some(())
}

/// The person's own configuration under `home` (`~/.gitconfig` and `~/.config/git/config`, each
/// as its links resolve on local volumes and outside every folder macOS guards), with no include.
fn person(reading: &mut Reading, home: &Path, table: &Table) -> Option<()> {
    use crate::harness::disk::idle::{resolve_dangling, resolve_local};
    let real_home = resolve_local(home, table)?;
    for (label, path) in [
        ("gitconfig", home.join(".gitconfig")),
        ("xdg", home.join(".config").join("git").join("config")),
    ] {
        if atpkg::protected::under_protected_root(home, &path) {
            return None;
        }
        let real = resolve_dangling(&path, table)?;
        if atpkg::protected::under_protected_root(home, &real)
            || atpkg::protected::under_protected_root(&real_home, &real)
        {
            return None;
        }
        reading.said(&real.to_string_lossy());
        if reading
            .file(label, &real, CONFIG_MAX)?
            .is_some_and(|bytes| includes(&bytes))
        {
            return None;
        }
    }
    Some(())
}

/// The evidence for what `git config` says of the checkout `root` to the git that says
/// `version`: which repository it is ([`repository`]) and the person's own configuration
/// ([`person`]), which holds every `safe.directory` too.
pub(super) fn config_seen(
    version: &str,
    home: &Path,
    root: &Path,
    git_dir: &Path,
    table: &Table,
) -> Option<Seen> {
    let mut reading = Reading::new("config", version, root);
    repository(&mut reading, root, git_dir, false)?;
    person(&mut reading, home, table)?;
    Some(reading.done())
}

/// The evidence for what `git config --global --path` makes of the person's `core.excludesFile`
/// under `home`: their own configuration ([`person`]).
pub(super) fn person_seen(version: &str, home: &Path, table: &Table) -> Option<Seen> {
    let mut reading = Reading::new("global path", version, home);
    person(&mut reading, home, table)?;
    Some(reading.done())
}

/// The loose file of the ref `name` under `git_dir`, read into `reading`: `Some(Some(target))`
/// where it is a symbolic ref to `target`, `Some(None)` where it holds a commit's name or is
/// not there, and `None` where `name` is no plain path under `refs/` or the file cannot be
/// vouched for.
fn loose_ref(reading: &mut Reading, git_dir: &Path, name: &str) -> Option<Option<String>> {
    if !name.starts_with("refs/")
        || name.contains('\\')
        || !Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    reading.said(name);
    let Some(bytes) = reading.file("loose", &git_dir.join(name), REF_FILE_MAX)? else {
        return Some(None);
    };
    let Some(target) = bytes.strip_prefix(b"ref:") else {
        return Some(None);
    };
    Some(Some(std::str::from_utf8(target).ok()?.trim().to_owned()))
}

/// The evidence for what `git for-each-ref` says of the refs `asked`, what `HEAD` resolves to and
/// how far one commit is behind another, in the checkout `root`, to the git that says `version`:
/// which repository it is ([`repository`], which holds `HEAD`), each asked ref's loose file and
/// that of the ref it names where it is symbolic, the whole of `packed-refs`, and the two files
/// that change which commits a history holds (`shallow`, `info/grafts`). `None` where a ref asked
/// for names a symbolic ref to a symbolic ref, the repository holds any replace ref, or it
/// borrows another's objects (`objects/info/alternates`: that store lets go of objects by its own
/// refs, not this one's).
pub(super) fn refs_seen(
    version: &str,
    root: &Path,
    git_dir: &Path,
    asked: &[&str],
) -> Option<Seen> {
    let mut reading = Reading::new("refs", version, root);
    repository(&mut reading, root, git_dir, true)?;
    for name in asked {
        if let Some(target) = loose_ref(&mut reading, git_dir, name)?
            && loose_ref(&mut reading, git_dir, &target)?.is_some()
        {
            return None;
        }
    }
    if reading
        .file("packed-refs", &git_dir.join("packed-refs"), PACKED_REFS_MAX)?
        .is_some_and(|packed| {
            packed
                .windows(b" refs/replace/".len())
                .any(|window| window == b" refs/replace/")
        })
    {
        return None;
    }
    match std::fs::read_dir(git_dir.join("refs").join("replace")) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return None;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    reading.nothing(
        "alternates",
        &git_dir.join("objects").join("info").join("alternates"),
    )?;
    reading.file("shallow", &git_dir.join("shallow"), HISTORY_FILE_MAX)?;
    reading.file(
        "grafts",
        &git_dir.join("info").join("grafts"),
        HISTORY_FILE_MAX,
    )?;
    Some(reading.done())
}

/// One answer, and the contents it was given under.
type Slot<T> = Option<(Digest, T)>;

/// The answer kept in `slot` where `before`, read now, is the evidence it was given under and
/// its git could start now (`None`, with [`Reads::cut`](super::super::Reads::cut) set, where it
/// could not: an answer is never given by a read that is held back); else `ask`'s, which says
/// whether git gave it, kept where it did and `after`, read once `ask` returned, is `before`.
pub(super) fn kept_or<T: Clone>(
    slot: &mut Slot<T>,
    reads: &Reads<'_>,
    before: Option<Seen>,
    after: impl FnOnce() -> Option<Seen>,
    ask: impl FnOnce() -> (T, bool),
) -> Option<T> {
    if let (Some(seen), Some((key, answer))) = (&before, slot.as_ref())
        && seen.key == *key
    {
        return reads.start_by().map(|_| answer.clone());
    }
    *slot = None;
    let (answer, said) = ask();
    if said
        && let Some(before) = before
        && after().as_ref() == Some(&before)
    {
        *slot = Some((before.key, answer.clone()));
    }
    Some(answer)
}

/// Where the person's configuration sets `core.excludesFile` for a checkout that sets none.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Excludes {
    /// The checkout's own configuration sets it.
    Own,
    /// Neither sets it: Git's default file.
    Default,
    /// The person's configuration sets it, to `raw` as written.
    Person { raw: String },
}

/// A checkout's own `remote.origin.url`, and where its excludes file is set.
pub(super) type ConfigAnswer = (Option<String>, Excludes);

/// What `HEAD` names, origin's main ref in full, and that ref's commit.
pub(super) type RefsAnswer = (Option<String>, Option<String>, Option<String>);

/// What the reads of one checkout keep.
#[derive(Default)]
pub(super) struct KeptRepo {
    /// `None` inside where the read was cut: never kept, since no git gave it.
    pub(super) config: Slot<Option<ConfigAnswer>>,
    pub(super) refs: Slot<RefsAnswer>,
    pub(super) behind: Slot<Option<u32>>,
}

/// What a home's reads keep between passes ([the module](self)).
#[derive(Default)]
pub(super) struct Kept {
    /// The home these answers are of, and what its git said of itself.
    of: Option<(PathBuf, String)>,
    /// By the checkout's name in the home.
    repos: HashMap<String, KeptRepo>,
    /// What `--path` made of the person's `core.excludesFile`.
    pub(super) person_excludes: Slot<Option<PathBuf>>,
}

impl Kept {
    /// A pass over `home` begins, with a git that says `version`: answers of another home or
    /// another git are let go.
    pub(super) fn begin(&mut self, home: &Path, version: &str) {
        if self
            .of
            .as_ref()
            .is_none_or(|(of, said)| of != home || said != version)
        {
            *self = Self {
                of: Some((home.to_path_buf(), version.to_owned())),
                ..Self::default()
            };
        }
    }

    /// What the git of this pass said of itself.
    pub(super) fn version(&self) -> &str {
        self.of.as_ref().map_or("", |(_, version)| version)
    }

    /// The checkout `name`'s kept answers, and the home's.
    pub(super) fn repo(&mut self, name: &str) -> (&mut KeptRepo, &mut Slot<Option<PathBuf>>) {
        (
            self.repos.entry(name.to_owned()).or_default(),
            &mut self.person_excludes,
        )
    }

    /// A pass that listed `names` ended: the answers of every other checkout are let go.
    pub(super) fn end(&mut self, names: &[String]) {
        self.repos.retain(|name, _| names.contains(name));
    }
}
