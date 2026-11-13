// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The census memo: each unit's result (a guard rule's findings, a census's outcome) kept with
//! every question the unit asked of the checkout and the answer it got, and reused only while
//! every one of those answers still holds.
//!
//! A unit reads the checkout only through [`crate::tree`], whose every read, listing and stat
//! answers a recorded question, and through the guards' corpus (`guards::corpus`), whose every
//! accessor records one: what a prefix lists, whether a file is there, whether it mentions a
//! needle set, its content, and the corpus-wide facts (every path, the scripts, the manifests,
//! which files are test code, the engine's baseline). A unit reads no clock, environment,
//! network or process state its result depends on. So when every recorded question gets its
//! recorded answer now, a run of the unit now can take the recorded run's every step, and the
//! recorded result is a result of the unit now. A question about a file whose bytes did not
//! change has the answer it had; only the questions about changed files are asked again.
//!
//! What keys a record: the SHA-256 of the running executable's bytes (the code that judges,
//! whose identity [`pin_code`] pins before anything can rebuild it), [`FORMAT`], the checkout
//! root, and the SHA-256 of each file the record's units read. A record whose seal, format, code
//! or root does not match is no record, and the run recomputes everything. A run stores nothing
//! when its executable changed under it, when its source tree could not be walked, when a
//! question got two answers in one unit, when a unit opened a file whose reads it cannot follow,
//! or when the memo's corpus was read on a thread none of its units runs on ([`Shared::taint`]);
//! and a unit that could not run (a panic) stores no result. Census code starts its threads
//! through [`threads`], which carries a unit's scope and the reader's input capture
//! ([`crate::inputs::capture`]) onto them (the `census-thread` guard holds it to that).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// A SHA-256.
pub(crate) type Digest = [u8; 32];

/// The record's layout. A change to what a record holds or how [`Trace`] questions are answered
/// changes it, so no record of an older layout is read.
pub(crate) const FORMAT: u32 = 1;

const MAGIC: &[u8] = b"aterm-census-memo\n";

/// The SHA-256 of `parts`, each preceded by its length, so no two lists of parts share a digest.
pub(crate) fn digest(parts: &[&[u8]]) -> Digest {
    let mut sha = aterm_digest::Sha256::new();
    for part in parts {
        sha.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        sha.update(part);
    }
    sha.finalize()
}

/// The answer a text read gave ([`crate::tree::read_to_string`]): its bytes or its error.
pub(crate) fn text_answer(read: &std::io::Result<String>) -> Digest {
    match read {
        Ok(text) => digest(&[b"text", text.as_bytes()]),
        Err(e) => error_answer(b"text-error", e),
    }
}

/// The answer a byte read gave ([`crate::tree::read_bytes`]).
pub(crate) fn bytes_answer(read: &std::io::Result<Vec<u8>>) -> Digest {
    match read {
        Ok(bytes) => digest(&[b"bytes", bytes]),
        Err(e) => error_answer(b"bytes-error", e),
    }
}

fn error_answer(tag: &[u8], e: &std::io::Error) -> Digest {
    digest(&[
        tag,
        format!("{:?}", e.kind()).as_bytes(),
        e.to_string().as_bytes(),
    ])
}

/// The answer a listing gave ([`crate::tree::read_dir`], [`crate::tree::read_dir_typed`]): each
/// entry's name (and kind, when typed) or error, in the order the listing gave them.
pub(crate) fn listing_answer<T>(
    listed: &std::io::Result<Vec<std::io::Result<T>>>,
    entry: impl Fn(&T) -> Vec<u8>,
) -> Digest {
    match listed {
        Ok(entries) => {
            let mut parts: Vec<Vec<u8>> = vec![b"listing".to_vec()];
            for item in entries {
                parts.push(match item {
                    Ok(found) => [&[0u8][..], &entry(found)].concat(),
                    Err(e) => [&[1u8][..], &error_answer(b"entry", e)].concat(),
                });
            }
            let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
            digest(&refs)
        }
        Err(e) => error_answer(b"listing-error", e),
    }
}

/// The answer a yes-or-no question gave.
pub(crate) fn bool_answer(yes: bool) -> Digest {
    digest(&[b"bool", &[u8::from(yes)]])
}

/// The checkout question a [`crate::tree`] call asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Op {
    /// [`crate::tree::read_to_string`].
    Text = 1,
    /// [`crate::tree::read_bytes`].
    Bytes = 2,
    /// [`crate::tree::read_dir`].
    List = 3,
    /// [`crate::tree::read_dir_typed`].
    ListTyped = 4,
    /// [`crate::tree::is_dir`].
    IsDir = 5,
    /// [`crate::tree::is_file`].
    IsFile = 6,
    /// [`crate::tree::exists`].
    Exists = 7,
}

impl Op {
    fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Text,
            2 => Self::Bytes,
            3 => Self::List,
            4 => Self::ListTyped,
            5 => Self::IsDir,
            6 => Self::IsFile,
            7 => Self::Exists,
            _ => return None,
        })
    }
}

/// A corpus-wide fact a guard read ([`Trace::globals`]).
pub(crate) mod global {
    /// Every path of the checkout (`Corpus::paths`).
    pub(crate) const PATHS: u8 = 1;
    /// Every script, with its text (`Corpus::scripts`).
    pub(crate) const SCRIPTS: u8 = 2;
    /// Every manifest, with its text (`Corpus::manifests`).
    pub(crate) const MANIFESTS: u8 = 4;
    /// The publication engine's baseline (`Tree::engine`).
    pub(crate) const ENGINE: u8 = 8;
    /// Every fact, in record order.
    pub(crate) const ALL: [u8; 4] = [PATHS, SCRIPTS, MANIFESTS, ENGINE];
}

// ---------------------------------------------------------------------------------------------
// The code that judges.

struct Pinned {
    digest: Digest,
    stamp: Stamp,
    path: PathBuf,
}

/// What a file's metadata says of its identity: a rebuild changes at least one of these.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt as _;
                (meta.dev(), meta.ino())
            },
        }
    }
}

/// The running executable and its [`Stamp`] when [`pin_code`] read them.
static STARTED: OnceLock<Option<(PathBuf, Stamp)>> = OnceLock::new();

static CODE: OnceLock<Option<Pinned>> = OnceLock::new();

/// Pins the code that judges: the running executable's path and identity ([`Stamp`]), read
/// now, before a build can replace the file under a process that is already running other code.
/// A memo hashes the file when it first needs the digest, and only while the file still has
/// the identity pinned here. Call it first in `main` (it reads one stat and starts no thread); a
/// memo pins on first use otherwise. When the executable cannot be read, or was replaced since,
/// no memo is read or written.
pub fn pin_code() {
    let _ = started();
}

fn started() -> Option<&'static (PathBuf, Stamp)> {
    STARTED
        .get_or_init(|| {
            let path = std::env::current_exe().ok()?;
            let stamp = Stamp::of(&std::fs::metadata(&path).ok()?);
            Some((path, stamp))
        })
        .as_ref()
}

fn code() -> Option<&'static Pinned> {
    CODE.get_or_init(|| {
        let (path, stamp) = started()?;
        let file = std::fs::File::open(path).ok()?;
        if Stamp::of(&file.metadata().ok()?) != *stamp {
            return None;
        }
        let digest = file_digest(file)?;
        (Stamp::of(&std::fs::metadata(path).ok()?) == *stamp).then(|| Pinned {
            digest,
            stamp: stamp.clone(),
            path: path.clone(),
        })
    })
    .as_ref()
}

/// Whether the executable [`pin_code`] read is still the file at its path, unchanged.
fn code_unchanged(pinned: &Pinned) -> bool {
    std::fs::metadata(&pinned.path).is_ok_and(|meta| Stamp::of(&meta) == pinned.stamp)
}

/// The SHA-256 over the SHA-256 of each 4 MiB of `file`, within the caller's worker policy.
fn file_digest(mut file: std::fs::File) -> Option<Digest> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    const CHUNK: usize = 4 << 20;
    let chunks: Vec<&[u8]> = bytes.chunks(CHUNK).collect();
    let width = aterm_cores::available_parallelism().map_or(1, std::num::NonZero::get);
    let per = chunks.len().div_ceil(width.max(1)).max(1);
    let digests: Vec<Digest> = if SERIAL.get() {
        chunks.iter().map(|chunk| digest(&[chunk])).collect()
    } else {
        std::thread::scope(|scope| {
            let running: Vec<_> = chunks
                .chunks(per)
                .map(|group| {
                    scope.spawn(move || group.iter().map(|c| digest(&[c])).collect::<Vec<_>>())
                })
                .collect();
            running
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    };
    (digests.len() == chunks.len()).then(|| {
        let refs: Vec<&[u8]> = digests.iter().map(<[u8; 32]>::as_slice).collect();
        digest(&[b"exe", &digest(&refs)])
    })
}

// ---------------------------------------------------------------------------------------------
// The snapshot: every source file read once, in parallel, with its digest.

/// The directories whose `.rs` files make the guards' corpus.
pub(crate) const CORPUS_DIRS: &[&str] = &["crates", "apps"];

/// The directories whose `.rs` files the snapshot reads: the corpus's, and the vendored crates
/// the lock census reads, so a text read of any of them is a question about a snapshot file.
const SOURCE_DIRS: &[&str] = &["crates", "apps", "vendor"];

/// Is root-relative `rel` under [`CORPUS_DIRS`]?
pub(crate) fn in_corpus(rel: &str) -> bool {
    CORPUS_DIRS
        .iter()
        .any(|dir| rel.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
}

/// One `.rs` file of the snapshot.
pub(crate) struct SnapFile {
    /// Root-relative, `/`-separated ([`crate::rel_path`]).
    pub(crate) rel: String,
    /// As the walk spelled it.
    pub(crate) path: PathBuf,
    /// Its text, or why it could not be read (`cannot read …` as the corpus says it).
    pub(crate) text: Result<Arc<str>, String>,
    /// [`text_answer`] of the read.
    pub(crate) digest: Digest,
}

/// Every `.rs` file under [`SOURCE_DIRS`], read once at the start of a run.
pub(crate) struct Snapshot {
    /// By [`SnapFile::rel`].
    pub(crate) files: Vec<SnapFile>,
    /// Indices into `files` in the order of their paths, the order the corpus reads them in.
    pub(crate) path_order: Vec<usize>,
    /// Why each directory of [`SOURCE_DIRS`] that could not be walked could not
    /// (`cannot walk …`); none of its files are in `files`.
    pub(crate) unwalked: Vec<(&'static str, String)>,
}

impl Snapshot {
    /// Reads every `.rs` file under [`SOURCE_DIRS`] at `root`, in parallel, each thread in the
    /// caller's scope (a memo run's loader scope, so the reads answer no unit's question).
    pub(crate) fn read(root: &Path) -> Self {
        let mut paths = Vec::new();
        let mut unwalked = Vec::new();
        for dir in SOURCE_DIRS {
            let mut found = Vec::new();
            match crate::collect_rs_files(&root.join(dir), &mut found) {
                Ok(()) => paths.extend(found),
                Err(e) => unwalked.push((*dir, format!("cannot walk {dir}: {e}"))),
            }
        }
        paths.sort();
        let width = aterm_cores::available_parallelism().map_or(1, std::num::NonZero::get);
        let per = paths.len().div_ceil(width.max(1)).max(1);
        let read: Vec<SnapFile> = threads(|scope| {
            let running: Vec<_> = paths
                .chunks(per)
                .map(|group| {
                    scope.spawn(move || {
                        group
                            .iter()
                            .map(|path| {
                                let read = crate::tree::read_to_string(path);
                                let digest = text_answer(&read);
                                SnapFile {
                                    rel: crate::rel_path(root, path),
                                    path: path.clone(),
                                    text: read.map(Arc::from).map_err(|e| {
                                        format!("cannot read {}: {e}", path.display())
                                    }),
                                    digest,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            running
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        let mut order: Vec<usize> = (0..read.len()).collect();
        order.sort_by(|&a, &b| read[a].rel.cmp(&read[b].rel));
        let mut slots: Vec<Option<SnapFile>> = read.into_iter().map(Some).collect();
        let files: Vec<SnapFile> = order.iter().filter_map(|&i| slots[i].take()).collect();
        // `order[k]` is the path-order position of the k-th file by rel; invert it.
        let mut path_order = vec![0; order.len()];
        for (by_rel, &by_path) in order.iter().enumerate() {
            path_order[by_path] = by_rel;
        }
        Self {
            files,
            path_order,
            unwalked,
        }
    }

    /// A snapshot of nothing.
    pub(crate) fn empty() -> Self {
        Self {
            files: Vec::new(),
            path_order: Vec::new(),
            unwalked: Vec::new(),
        }
    }

    /// The index of the file a text read of `path` under `root` reads, when the snapshot read
    /// that very path.
    pub(crate) fn at(&self, root: &Path, path: &Path) -> Option<usize> {
        let at = self.find(&crate::rel_path(root, path))?;
        (self.files[at].path == path).then_some(at)
    }

    /// The index of the file at root-relative `rel`.
    pub(crate) fn find(&self, rel: &str) -> Option<usize> {
        self.files
            .binary_search_by(|f| f.rel.as_str().cmp(rel))
            .ok()
    }
}

// ---------------------------------------------------------------------------------------------
// Scopes: which unit a thread's reads answer.

/// One memo run, shared by every thread of it.
pub(crate) struct Shared {
    root: PathBuf,
    snapshot: OnceLock<Arc<Snapshot>>,
    tainted: AtomicBool,
    why: Mutex<Option<String>>,
}

impl Shared {
    fn new(root: &Path) -> Arc<Self> {
        Arc::new(Self {
            root: root.to_path_buf(),
            snapshot: OnceLock::new(),
            tainted: AtomicBool::new(false),
            why: Mutex::new(None),
        })
    }

    /// The run stores nothing: a read reached the memo that it cannot attribute to one unit.
    pub(crate) fn taint(&self, why: &str) {
        if !self.tainted.swap(true, Ordering::SeqCst) {
            *self.why.lock().unwrap_or_else(PoisonError::into_inner) = Some(why.to_string());
        }
    }

    fn tainted(&self) -> Option<String> {
        self.tainted.load(Ordering::SeqCst).then(|| {
            self.why
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or_default()
        })
    }
}

#[derive(Clone)]
enum Scope {
    /// No memo run: reads answer nothing.
    None,
    /// A memo run's own reads (its snapshot, the corpus it loads, what it composes around its
    /// units): they answer no unit's question.
    Loader(Arc<Shared>),
    /// A unit's reads, on its own thread or a helper thread it started ([`threads`]): each is
    /// one of its questions.
    Unit(Arc<Shared>, Arc<Mutex<Trace>>),
}

thread_local! {
    static SCOPE: RefCell<Scope> = const { RefCell::new(Scope::None) };
}

/// Puts the previous scope back when the closure it guards ends, a panic included.
struct Restore(Option<Scope>);

impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(scope) = self.0.take() {
            SCOPE.with_borrow_mut(|s| *s = scope);
        }
    }
}

fn swap_scope(scope: Scope) -> Scope {
    SCOPE.with_borrow_mut(|s| std::mem::replace(s, scope))
}

/// Runs `f` with this thread's reads in `shared`'s loader scope.
pub(crate) fn loader<R>(shared: &Arc<Shared>, f: impl FnOnce() -> R) -> R {
    let _restore = Restore(Some(swap_scope(Scope::Loader(Arc::clone(shared)))));
    f()
}

/// Runs `f` as one unit of `shared`'s run and returns what it asked.
pub(crate) fn unit<R>(shared: &Arc<Shared>, f: impl FnOnce() -> R) -> (R, Trace) {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let restore = Restore(Some(swap_scope(Scope::Unit(
        Arc::clone(shared),
        Arc::clone(&trace),
    ))));
    let out = f();
    drop(restore);
    let asked = std::mem::take(&mut *trace.lock().unwrap_or_else(PoisonError::into_inner));
    (out, asked)
}

/// [`std::thread::Scope`] for census code: each thread it starts carries the scope and input
/// capture of the thread that started it, so a unit's helper thread asks that unit's questions
/// and a captured census's helper thread records its inputs. Census code starts its threads only
/// through [`threads`], since a thread started any other way reads in no scope and its checkout
/// reads answer nothing.
#[derive(Clone, Copy)]
pub(crate) struct Threads<'scope, 'env>(&'scope std::thread::Scope<'scope, 'env>);

thread_local! {
    static SERIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run nested census work on the calling thread. Memo scopes and input captures still follow
/// each job. This bounds a caller that already owns one CPU permit without another executor.
/// # Panics
/// The work panics.
pub fn without_parallel_workers<T>(work: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERIAL.set(self.0);
        }
    }
    let _restore = Restore(SERIAL.replace(true));
    work()
}

/// Run census work on one worker with the same 8 MiB stack as its named parallel workers.
/// Nested jobs run inline, carrying their ordinary memo scopes and input captures.
/// # Errors
/// The worker could not be started.
/// # Panics
/// The work panics; its panic is resumed on the caller.
pub fn on_one_worker<T: Send>(work: impl FnOnce() -> T + Send) -> std::io::Result<T> {
    if SERIAL.get() {
        return Ok(work());
    }
    threads(|scope| {
        scope
            .spawn_named("census-serial".into(), 8 << 20, || {
                without_parallel_workers(work)
            })
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    })
}

/// A census job, run on a thread or inline under its caller's single-worker policy.
#[must_use]
pub(crate) enum Joined<'scope, T> {
    Thread(std::thread::ScopedJoinHandle<'scope, T>),
    Inline(Completed<T>),
}

/// An inline job's unjoined panic still ends its scope, as a scoped thread's does.
pub(crate) struct Completed<T>(Option<std::thread::Result<T>>);

impl<T> Completed<T> {
    fn join(mut self) -> std::thread::Result<T> {
        self.0.take().expect("the completed job has one answer")
    }
}

impl<T> Drop for Completed<T> {
    fn drop(&mut self) {
        if let Some(Err(panic)) = self.0.take()
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

impl<T> Joined<'_, T> {
    pub(crate) fn join(self) -> std::thread::Result<T> {
        match self {
            Self::Thread(worker) => worker.join(),
            Self::Inline(answer) => answer.join(),
        }
    }
}

impl<'scope> Threads<'scope, '_> {
    /// Starts `f` on a new thread of this scope, in the starting thread's memo scope and input
    /// capture ([`crate::inputs::capture`]).
    pub(crate) fn spawn<T: Send + 'scope>(
        self,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> Joined<'scope, T> {
        let scope = SCOPE.with_borrow(Scope::clone);
        let capture = crate::inputs::carried();
        let work = move || {
            let _restore = Restore(Some(swap_scope(scope)));
            capture.enter(f)
        };
        if SERIAL.get() {
            Joined::Inline(Completed(Some(std::panic::catch_unwind(
                std::panic::AssertUnwindSafe(work),
            ))))
        } else {
            Joined::Thread(self.0.spawn(work))
        }
    }

    /// [`Self::spawn`] on a thread named `name` with a `stack`-byte stack.
    pub(crate) fn spawn_named<T: Send + 'scope>(
        self,
        name: String,
        stack: usize,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> std::io::Result<Joined<'scope, T>> {
        if SERIAL.get() {
            return Ok(self.spawn(f));
        }
        let scope = SCOPE.with_borrow(Scope::clone);
        let capture = crate::inputs::carried();
        std::thread::Builder::new()
            .name(name)
            .stack_size(stack)
            .spawn_scoped(self.0, move || {
                let _restore = Restore(Some(swap_scope(scope)));
                capture.enter(f)
            })
            .map(Joined::Thread)
    }
}

/// [`std::thread::scope`] whose threads carry their starter's memo scope and input capture
/// ([`Threads`]).
pub(crate) fn threads<'env, T>(f: impl for<'scope> FnOnce(Threads<'scope, 'env>) -> T) -> T {
    std::thread::scope(|scope| f(Threads(scope)))
}

#[cfg(test)]
#[path = "memo_worker_tests.rs"]
mod worker_tests;

/// Runs `f`, inside a unit, with its reads answering none of the unit's questions: for a
/// corpus-wide fact the unit then asks about by its own recorded question (which files are test
/// code), computed once for every unit that asks.
pub(crate) fn unrecorded<R>(f: impl FnOnce() -> R) -> R {
    let shared = SCOPE.with_borrow(|s| match s {
        Scope::Unit(shared, _) | Scope::Loader(shared) => Some(Arc::clone(shared)),
        Scope::None => None,
    });
    match shared {
        Some(shared) => loader(&shared, f),
        None => f(),
    }
}

/// Records a corpus question in the unit this thread runs, when `owner` (the memo run whose
/// corpus was read) has one here; a read of a memo's corpus anywhere else taints that run.
pub(crate) fn note(owner: Option<&Arc<Shared>>, ask: impl FnOnce(&mut Trace)) {
    let Some(owner) = owner else {
        return;
    };
    SCOPE.with_borrow(|scope| match scope {
        Scope::Unit(shared, trace) if Arc::ptr_eq(shared, owner) => {
            ask(&mut trace.lock().unwrap_or_else(PoisonError::into_inner));
        }
        Scope::Loader(shared) if Arc::ptr_eq(shared, owner) => {}
        _ => owner.taint("the memo's corpus was read off its units' threads"),
    });
}

/// Records a checkout question a [`crate::tree`] call asked, in the unit this thread runs.
/// `answer` digests what the call returned, given the run's snapshot and root (a text read
/// reuses the snapshot's digest of the same bytes, [`snapshot_text`]).
pub(crate) fn note_tree(path: &Path, op: Op, answer: impl FnOnce(&Snapshot, &Path) -> Digest) {
    SCOPE.with_borrow(|scope| {
        if let Scope::Unit(shared, trace) = scope {
            let Some(key) = path_key(&shared.root, path) else {
                shared.taint("a unit read a path this platform's record cannot name");
                return;
            };
            let snapshot = shared.snapshot.get();
            let got = match snapshot {
                Some(snapshot) => answer(snapshot, &shared.root),
                None => answer(&Snapshot::empty(), &shared.root),
            };
            let mut trace = trace.lock().unwrap_or_else(PoisonError::into_inner);
            // A text read of a snapshot file that read the snapshot's bytes asks what the
            // file's content question asks: whether those bytes changed.
            let same = snapshot.and_then(|snapshot| {
                let at = snapshot.at(&shared.root, path)?;
                (op == Op::Text && snapshot.files[at].digest == got).then_some(at)
            });
            if let Some(at) = same.and_then(|at| u32::try_from(at).ok()) {
                trace.content(at);
                return;
            }
            if trace
                .tree
                .insert((op, key), got)
                .is_some_and(|had| had != got)
            {
                shared.taint("one checkout question got two answers in one unit");
            }
        }
    });
}

/// Taints the run when a unit on this thread does what the memo cannot follow (`why`).
pub(crate) fn untraceable(why: &str) {
    SCOPE.with_borrow(|scope| {
        if let Scope::Unit(shared, _) = scope {
            shared.taint(why);
        }
    });
}

/// The snapshot's [`text_answer`] for `path`, when the snapshot read that very path: what a text
/// read of it answers as of the run's start, which a recorded question is judged by.
pub(crate) fn snapshot_answer(snapshot: &Snapshot, root: &Path, path: &Path) -> Option<Digest> {
    snapshot.at(root, path).map(|at| snapshot.files[at].digest)
}

/// Does `text` hold any of `needles`? The corpus's mention question, and how a recorded one is
/// asked again of a changed file.
pub(crate) fn mentions_in(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

/// The snapshot's digest of `text`, read from `path` under `root`, when the snapshot read the
/// same bytes there: [`text_answer`] digests the bytes alone, so equal bytes share it.
pub(crate) fn snapshot_text(
    snapshot: &Snapshot,
    root: &Path,
    path: &Path,
    text: &str,
) -> Option<Digest> {
    let file = &snapshot.files[snapshot.find(&crate::rel_path(root, path))?];
    (file.text.as_deref().ok() == Some(text)).then_some(file.digest)
}

/// `path` as a record names it: root-relative when under the root (marked 0), else whole (1);
/// `None` when the platform's path is not bytes this record can hold.
fn path_key(root: &Path, path: &Path) -> Option<Vec<u8>> {
    let (tag, rest) = match path.strip_prefix(root) {
        Ok(rel) => (0u8, rel.as_os_str()),
        Err(_) => (1u8, path.as_os_str()),
    };
    Some([&[tag][..], &os_bytes(rest)?].concat())
}

fn key_path(root: &Path, key: &[u8]) -> Option<PathBuf> {
    let (tag, bytes) = key.split_first()?;
    let text = os_from(bytes)?;
    match tag {
        0 => Some(root.join(text)),
        1 => Some(PathBuf::from(text)),
        _ => None,
    }
}

#[cfg(unix)]
fn os_bytes(text: &std::ffi::OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(text.as_bytes().to_vec())
}

#[cfg(unix)]
fn os_from(bytes: &[u8]) -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(std::ffi::OsStr::from_bytes(bytes).to_os_string())
}

#[cfg(not(unix))]
fn os_bytes(text: &std::ffi::OsStr) -> Option<Vec<u8>> {
    text.to_str().map(|t| t.as_bytes().to_vec())
}

#[cfg(not(unix))]
fn os_from(bytes: &[u8]) -> Option<std::ffi::OsString> {
    std::str::from_utf8(bytes)
        .ok()
        .map(std::ffi::OsString::from)
}

// ---------------------------------------------------------------------------------------------
// What a unit asked.

/// Every question one unit asked, and each answer.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(crate) struct Trace {
    /// Corpus files whose content it read, by corpus index.
    content: BTreeSet<u32>,
    /// Corpus files it asked whether they mention a needle set: (file, set, answer).
    mentions: BTreeSet<(u32, u32, bool)>,
    /// The needle sets, by [`Self::mentions`]'s set index.
    needles: Vec<Vec<String>>,
    /// The prefix sets whose files it listed.
    listings: BTreeSet<Vec<String>>,
    /// Whether a file is in the corpus.
    has: BTreeMap<String, bool>,
    /// Whether a file is test code nothing ships.
    test_code: BTreeMap<String, bool>,
    /// The corpus-wide facts it read ([`global`]).
    globals: u8,
    /// Its [`crate::tree`] questions: (call, path) and the answer's digest.
    tree: BTreeMap<(Op, Vec<u8>), Digest>,
}

impl Trace {
    pub(crate) fn content(&mut self, file: u32) {
        self.content.insert(file);
    }

    pub(crate) fn mentions(&mut self, file: u32, needles: &[&str], answer: bool) {
        let set = match self.needles.iter().position(|known| {
            known.len() == needles.len() && known.iter().zip(needles).all(|(a, b)| a == b)
        }) {
            Some(at) => at,
            None => {
                self.needles
                    .push(needles.iter().map(|n| (*n).to_string()).collect());
                self.needles.len() - 1
            }
        };
        self.mentions
            .insert((file, u32::try_from(set).unwrap_or(u32::MAX), answer));
    }

    pub(crate) fn listing(&mut self, prefixes: &[&str]) {
        if !self.listings.iter().any(|known| {
            known.len() == prefixes.len() && known.iter().zip(prefixes).all(|(a, b)| a == b)
        }) {
            self.listings
                .insert(prefixes.iter().map(|p| (*p).to_string()).collect());
        }
    }

    pub(crate) fn has(&mut self, rel: &str, answer: bool) {
        self.has.insert(rel.to_string(), answer);
    }

    pub(crate) fn test_code(&mut self, rel: &str, answer: bool) {
        self.test_code.insert(rel.to_string(), answer);
    }

    pub(crate) fn global(&mut self, fact: u8) {
        self.globals |= fact;
    }

    /// A mention question about a file whose content the unit read anyway asks nothing more.
    fn settle(&mut self) {
        let content = &self.content;
        self.mentions.retain(|(file, _, _)| !content.contains(file));
    }
}

// ---------------------------------------------------------------------------------------------
// The record.

/// A finding as a record keeps it (its rule is the unit's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) detail: String,
}

/// A census outcome as a record keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeptOutcome {
    pub(crate) ok: bool,
    pub(crate) verdict: String,
    pub(crate) log: String,
    pub(crate) title: String,
    /// `(what, seen, min, max, missed)`.
    pub(crate) floors: Vec<(String, usize, usize, usize, String)>,
}

/// What a unit concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// A guard rule's findings.
    Findings(Vec<Kept>),
    /// A census's outcome.
    Outcome(KeptOutcome),
}

/// One unit's record: what it asked and what it concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnitRecord {
    pub(crate) id: String,
    pub(crate) trace: Trace,
    pub(crate) result: Verdict,
}

/// One run's record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Record {
    code: Digest,
    root: Vec<u8>,
    /// The snapshot's files: (rel, digest), by rel. A trace's file indices index this.
    files: Vec<(String, Digest)>,
    /// The corpus-wide facts' digests, in [`global::ALL`] order.
    globals: [Digest; 4],
    units: Vec<UnitRecord>,
}

struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, v: usize) {
        self.u32(u32::try_from(v).unwrap_or(u32::MAX));
    }
    fn bytes(&mut self, v: &[u8]) {
        self.len(v.len());
        self.0.extend_from_slice(v);
    }
    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }
    fn digest(&mut self, v: &Digest) {
        self.0.extend_from_slice(v);
    }
}

struct Dec<'a>(&'a [u8]);

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)?.try_into().ok().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)?.try_into().ok().map(u64::from_le_bytes)
    }
    fn len(&mut self) -> Option<usize> {
        let n = usize::try_from(self.u32()?).ok()?;
        // No count can exceed the bytes left: a garbled length fails here, not in an allocation.
        (n <= self.0.len()).then_some(n)
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.len()?;
        self.take(n)
    }
    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
    fn digest(&mut self) -> Option<Digest> {
        self.take(32)?.try_into().ok()
    }
}

fn usize_of(v: u64) -> Option<usize> {
    usize::try_from(v).ok()
}

fn put_strs(enc: &mut Enc, list: &[String]) {
    enc.len(list.len());
    for s in list {
        enc.str(s);
    }
}

fn get_strs(dec: &mut Dec<'_>) -> Option<Vec<String>> {
    let n = dec.len()?;
    (0..n).map(|_| dec.str()).collect()
}

impl Trace {
    fn put(&self, enc: &mut Enc) {
        enc.len(self.content.len());
        for &file in &self.content {
            enc.u32(file);
        }
        enc.len(self.needles.len());
        for set in &self.needles {
            put_strs(enc, set);
        }
        enc.len(self.mentions.len());
        for &(file, set, answer) in &self.mentions {
            enc.u32(file);
            enc.u32(set);
            enc.u8(u8::from(answer));
        }
        enc.len(self.listings.len());
        for prefixes in &self.listings {
            put_strs(enc, prefixes);
        }
        for map in [&self.has, &self.test_code] {
            enc.len(map.len());
            for (rel, &answer) in map {
                enc.str(rel);
                enc.u8(u8::from(answer));
            }
        }
        enc.u8(self.globals);
        enc.len(self.tree.len());
        for ((op, path), answer) in &self.tree {
            enc.u8(*op as u8);
            enc.bytes(path);
            enc.digest(answer);
        }
    }

    fn get(dec: &mut Dec<'_>) -> Option<Self> {
        let mut trace = Self::default();
        for _ in 0..dec.len()? {
            trace.content.insert(dec.u32()?);
        }
        for _ in 0..dec.len()? {
            trace.needles.push(get_strs(dec)?);
        }
        for _ in 0..dec.len()? {
            let file = dec.u32()?;
            let set = dec.u32()?;
            if usize::try_from(set).ok()? >= trace.needles.len() {
                return None;
            }
            trace.mentions.insert((file, set, dec.u8()? != 0));
        }
        for _ in 0..dec.len()? {
            trace.listings.insert(get_strs(dec)?);
        }
        for map in [&mut trace.has, &mut trace.test_code] {
            for _ in 0..dec.len()? {
                let rel = dec.str()?;
                map.insert(rel, dec.u8()? != 0);
            }
        }
        trace.globals = dec.u8()?;
        for _ in 0..dec.len()? {
            let op = Op::from_u8(dec.u8()?)?;
            let path = dec.bytes()?.to_vec();
            trace.tree.insert((op, path), dec.digest()?);
        }
        Some(trace)
    }
}

impl Verdict {
    fn put(&self, enc: &mut Enc) {
        match self {
            Self::Findings(found) => {
                enc.u8(0);
                enc.len(found.len());
                for f in found {
                    enc.str(&f.path);
                    enc.u64(f.line as u64);
                    enc.str(&f.detail);
                }
            }
            Self::Outcome(o) => {
                enc.u8(1);
                enc.u8(u8::from(o.ok));
                enc.str(&o.verdict);
                enc.str(&o.log);
                enc.str(&o.title);
                enc.len(o.floors.len());
                for (what, seen, min, max, missed) in &o.floors {
                    enc.str(what);
                    enc.u64(*seen as u64);
                    enc.u64(*min as u64);
                    enc.u64(*max as u64);
                    enc.str(missed);
                }
            }
        }
    }

    fn get(dec: &mut Dec<'_>) -> Option<Self> {
        Some(match dec.u8()? {
            0 => {
                let n = dec.len()?;
                Self::Findings(
                    (0..n)
                        .map(|_| {
                            Some(Kept {
                                path: dec.str()?,
                                line: usize_of(dec.u64()?)?,
                                detail: dec.str()?,
                            })
                        })
                        .collect::<Option<_>>()?,
                )
            }
            1 => {
                let ok = dec.u8()? != 0;
                let verdict = dec.str()?;
                let log = dec.str()?;
                let title = dec.str()?;
                let n = dec.len()?;
                let floors = (0..n)
                    .map(|_| {
                        Some((
                            dec.str()?,
                            usize_of(dec.u64()?)?,
                            usize_of(dec.u64()?)?,
                            usize_of(dec.u64()?)?,
                            dec.str()?,
                        ))
                    })
                    .collect::<Option<_>>()?;
                Self::Outcome(KeptOutcome {
                    ok,
                    verdict,
                    log,
                    title,
                    floors,
                })
            }
            _ => return None,
        })
    }
}

impl Record {
    /// The record's bytes: [`MAGIC`], [`FORMAT`], the body, and the body's SHA-256 (its seal).
    fn encode(&self) -> Vec<u8> {
        let mut enc = Enc(Vec::with_capacity(1 << 20));
        enc.0.extend_from_slice(MAGIC);
        enc.u32(FORMAT);
        enc.digest(&self.code);
        enc.bytes(&self.root);
        enc.len(self.files.len());
        for (rel, digest) in &self.files {
            enc.str(rel);
            enc.digest(digest);
        }
        for g in &self.globals {
            enc.digest(g);
        }
        enc.len(self.units.len());
        for unit in &self.units {
            enc.str(&unit.id);
            unit.trace.put(&mut enc);
            unit.result.put(&mut enc);
        }
        let seal = digest(&[b"seal", &enc.0]);
        enc.digest(&seal);
        enc.0
    }

    /// The record `bytes` hold, or `None` when they are not one whole record of [`FORMAT`].
    fn decode(bytes: &[u8]) -> Option<Self> {
        let (body, seal) = bytes.split_at_checked(bytes.len().checked_sub(32)?)?;
        if digest(&[b"seal", body]) != *seal {
            return None;
        }
        let mut dec = Dec(body);
        if dec.take(MAGIC.len())? != MAGIC || dec.u32()? != FORMAT {
            return None;
        }
        let code = dec.digest()?;
        let root = dec.bytes()?.to_vec();
        let n = dec.len()?;
        let files: Vec<(String, Digest)> = (0..n)
            .map(|_| Some((dec.str()?, dec.digest()?)))
            .collect::<Option<_>>()?;
        if !files.windows(2).all(|w| w[0].0 < w[1].0) {
            return None;
        }
        let mut globals = [[0u8; 32]; 4];
        for g in &mut globals {
            *g = dec.digest()?;
        }
        let n = dec.len()?;
        let units: Vec<UnitRecord> = (0..n)
            .map(|_| {
                let id = dec.str()?;
                let trace = Trace::get(&mut dec)?;
                if trace
                    .content
                    .iter()
                    .chain(trace.mentions.iter().map(|m| &m.0))
                    .any(|&f| usize::try_from(f).map_or(true, |f| f >= files.len()))
                {
                    return None;
                }
                let result = Verdict::get(&mut dec)?;
                Some(UnitRecord { id, trace, result })
            })
            .collect::<Option<_>>()?;
        dec.0.is_empty().then_some(Self {
            code,
            root,
            files,
            globals,
            units,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// A run.

/// Where a census memo lives: a file a run reads its record from and writes its own to.
#[derive(Clone, Debug)]
pub struct Memo {
    path: PathBuf,
    /// The code digest a record must carry; `None` takes the pinned executable's ([`pin_code`]).
    code: Option<Digest>,
}

impl Memo {
    /// The memo kept at `path` (a file; its directory is created on the first write).
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            code: None,
        }
    }

    /// The memo of the checkout at `root`, kept in `dir` under a name of its own, so checkouts
    /// that share a directory keep a record each.
    #[must_use]
    pub fn for_root(dir: &Path, root: &Path) -> Self {
        let named = digest(&[b"root", root.as_os_str().as_encoded_bytes()]);
        let hex: String = named[..8].iter().map(|b| format!("{b:02x}")).collect();
        Self::at(dir.join(format!("census-{hex}.memo")))
    }

    /// The memo at `path` keyed by `code` in place of the executable's digest: a test's census
    /// version.
    #[cfg(test)]
    pub(crate) fn with_code(path: impl Into<PathBuf>, code: Digest) -> Self {
        Self {
            path: path.into(),
            code: Some(code),
        }
    }

    fn code(&self) -> Option<Digest> {
        self.code.or_else(|| code().map(|pinned| pinned.digest))
    }
}

/// Why a unit was judged again rather than reused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejudged {
    /// The record held no result for it.
    Unrecorded,
    /// One of its answers changed: the question, in words.
    Changed(String),
}

/// What a memo run reused and what it judged again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoReport {
    /// Why the run read no record, when it read none.
    pub no_record: Option<String>,
    /// The units whose recorded results were reused.
    pub reused: Vec<String>,
    /// The units judged again, each with why.
    pub rejudged: Vec<(String, Rejudged)>,
    /// Why the run wrote no record, when it wrote none.
    pub not_kept: Option<String>,
}

impl std::fmt::Display for MemoReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total = self.reused.len() + self.rejudged.len();
        write!(
            f,
            "census memo: {} of {total} unit(s) reused",
            self.reused.len()
        )?;
        if let Some(why) = &self.no_record {
            write!(f, " (no record: {why})")?;
        }
        if !self.rejudged.is_empty() {
            let changed: Vec<String> = self
                .rejudged
                .iter()
                .filter_map(|(id, why)| match why {
                    Rejudged::Changed(what) => Some(format!("{id} ({what})")),
                    Rejudged::Unrecorded => None,
                })
                .collect();
            if !changed.is_empty() {
                write!(f, "; judged again: {}", changed.join(", "))?;
            }
        }
        if let Some(why) = &self.not_kept {
            write!(f, "; nothing kept: {why}")?;
        }
        Ok(())
    }
}

/// One memo run: the previous record, this run's answers, and the records its units make.
pub(crate) struct Run {
    pub(crate) shared: Arc<Shared>,
    memo: Option<(Memo, Digest)>,
    previous: Option<Record>,
    engine: Digest,
    /// Whether each previous file's bytes changed or it is gone, by previous index.
    changed: Vec<bool>,
    /// Whether the previous run listed exactly the files this one lists.
    same_files: bool,
    /// This run's answers to [`crate::tree`] questions, asked once each.
    answers: Mutex<BTreeMap<(Op, Vec<u8>), Digest>>,
    kept: Mutex<Vec<UnitRecord>>,
    report: Mutex<MemoReport>,
    /// The corpus-wide facts' digests and test-code answers, once the guards' corpus is read.
    corpus: OnceLock<CorpusNow>,
}

/// The corpus-wide facts as this run's corpus has them.
pub(crate) struct CorpusNow {
    /// In [`global::ALL`] order.
    pub(crate) globals: [Digest; 4],
    /// Whether a file is test code nothing ships, by root-relative path.
    pub(crate) test_code: Box<dyn Fn(&str) -> bool + Send + Sync>,
}

/// The digest of the engine baseline a run is handed.
pub(crate) fn engine_digest(engine: &crate::EngineBaseline) -> Digest {
    match engine {
        Ok((at, text)) => digest(&[b"engine", at.as_bytes(), text.as_bytes()]),
        Err(why) => digest(&[b"engine-none", why.as_bytes()]),
    }
}

impl Run {
    /// A run over `root`: with `memo`, its record read and this run's snapshot taken.
    pub(crate) fn start(
        root: &Path,
        memo: Option<&Memo>,
        engine: &crate::EngineBaseline,
    ) -> (Self, Arc<Snapshot>) {
        let shared = Shared::new(root);
        let snapshot = Arc::new(loader(&shared, || Snapshot::read(root)));
        let _ = shared.snapshot.set(Arc::clone(&snapshot));
        let mut report = MemoReport::default();
        let keyed = memo.and_then(|m| match m.code() {
            Some(code) => Some((m.clone(), code)),
            None => {
                report.no_record = Some(
                    "the running executable cannot be read, or changed since it started".into(),
                );
                report.not_kept = report.no_record.clone();
                None
            }
        });
        let root_bytes = root.as_os_str().as_encoded_bytes().to_vec();
        let previous = keyed.as_ref().and_then(|(memo, code)| {
            let read = match std::fs::read(&memo.path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    report.no_record = Some(if e.kind() == std::io::ErrorKind::NotFound {
                        "none kept yet".into()
                    } else {
                        format!("cannot read {}: {e}", memo.path.display())
                    });
                    return None;
                }
            };
            let Some(record) = Record::decode(&read) else {
                report.no_record =
                    Some("the kept record is not a whole record of this format".into());
                return None;
            };
            if record.code != *code {
                report.no_record = Some("it was made by other census code".into());
                return None;
            }
            if record.root != root_bytes {
                report.no_record = Some("it judged another checkout".into());
                return None;
            }
            Some(record)
        });
        let (changed, same_files) = previous.as_ref().map_or((Vec::new(), false), |prev| {
            let changed = prev
                .files
                .iter()
                .map(|(rel, was)| {
                    snapshot
                        .find(rel)
                        .is_none_or(|i| snapshot.files[i].digest != *was)
                })
                .collect();
            let same = prev
                .files
                .iter()
                .map(|(rel, _)| rel.as_str())
                .eq(snapshot.files.iter().map(|f| f.rel.as_str()));
            (changed, same)
        });
        (
            Self {
                shared,
                memo: keyed,
                previous,
                engine: engine_digest(engine),
                changed,
                same_files,
                answers: Mutex::new(BTreeMap::new()),
                kept: Mutex::new(Vec::new()),
                report: Mutex::new(report),
                corpus: OnceLock::new(),
            },
            snapshot,
        )
    }

    /// Hands the run the guards' corpus-wide facts, which the guard units' questions are judged
    /// by.
    pub(crate) fn set_corpus(&self, now: CorpusNow) {
        let _ = self.corpus.set(now);
    }

    /// The previous record's result for unit `id`, when every answer it recorded still holds;
    /// else why not.
    pub(crate) fn reusable(&self, id: &str, snapshot: &Snapshot) -> Result<UnitRecord, Rejudged> {
        let prev = self.previous.as_ref().ok_or(Rejudged::Unrecorded)?;
        let unit = prev
            .units
            .iter()
            .find(|u| u.id == id)
            .ok_or(Rejudged::Unrecorded)?;
        self.holds(prev, &unit.trace, snapshot)
            .map_err(Rejudged::Changed)?;
        Ok(UnitRecord {
            id: unit.id.clone(),
            trace: remap(&unit.trace, &prev.files, snapshot)
                .ok_or_else(|| Rejudged::Changed("a file it read is gone".to_string()))?,
            result: unit.result.clone(),
        })
    }

    /// Whether every answer `trace` recorded against `prev` still holds.
    fn holds(&self, prev: &Record, trace: &Trace, snapshot: &Snapshot) -> Result<(), String> {
        let rel = |i: u32| prev.files[i as usize].0.as_str();
        if let Some(&file) = trace.content.iter().find(|&&f| self.changed[f as usize]) {
            return Err(format!("{} changed", rel(file)));
        }
        for &(file, set, answer) in &trace.mentions {
            if !self.changed[file as usize] {
                continue;
            }
            let at = rel(file);
            let text = snapshot
                .find(at)
                .and_then(|i| snapshot.files[i].text.as_deref().ok())
                .ok_or_else(|| format!("{at} is gone"))?;
            let needles: Vec<&str> = trace.needles[set as usize]
                .iter()
                .map(String::as_str)
                .collect();
            if mentions_in(text, &needles) != answer {
                return Err(format!("{at} changed what it mentions"));
            }
        }
        if !self.same_files {
            for prefixes in &trace.listings {
                let under =
                    |r: &str| in_corpus(r) && prefixes.iter().any(|p| r.starts_with(p.as_str()));
                let was = prev
                    .files
                    .iter()
                    .map(|(r, _)| r.as_str())
                    .filter(|r| under(r));
                let now = snapshot
                    .files
                    .iter()
                    .map(|f| f.rel.as_str())
                    .filter(|r| under(r));
                if !was.eq(now) {
                    return Err(format!("the files under {} changed", prefixes.join(", ")));
                }
            }
        }
        for (at, &answer) in &trace.has {
            let held = in_corpus(at)
                && snapshot
                    .find(at)
                    .is_some_and(|i| snapshot.files[i].text.is_ok());
            if held != answer {
                return Err(format!("{at} came or went"));
            }
        }
        if trace.globals != 0 || !trace.test_code.is_empty() {
            let corpus = self
                .corpus
                .get()
                .ok_or_else(|| "the corpus was not read".to_string())?;
            for (k, fact) in global::ALL.iter().enumerate() {
                if trace.globals & fact == 0 {
                    continue;
                }
                let now = if *fact == global::ENGINE {
                    self.engine
                } else {
                    corpus.globals[k]
                };
                if now != prev.globals[k] {
                    return Err(global_name(*fact).to_string());
                }
            }
            if !(self.same_files && !self.changed.iter().any(|&c| c)) {
                for (at, &answer) in &trace.test_code {
                    if (corpus.test_code)(at) != answer {
                        return Err(format!("whether {at} is test code changed"));
                    }
                }
            }
        }
        // A unit can ask of every file of the checkout (`conflict-marker` reads each): its
        // questions are asked again on every core.
        let asked: Vec<(&(Op, Vec<u8>), &Digest)> = trace.tree.iter().collect();
        let width = aterm_cores::available_parallelism().map_or(1, std::num::NonZero::get);
        let per = asked.len().div_ceil(width).max(256);
        let wrong = threads(|scope| {
            let running: Vec<_> = asked
                .chunks(per)
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .find(|&&((op, key), &answer)| {
                                self.answer(*op, key, snapshot) != Some(answer)
                            })
                            .map(|&((op, key), _)| (*op, key.clone()))
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Some((Op::Exists, Vec::new()))))
                .find_map(|wrong| wrong)
        });
        if let Some((op, key)) = wrong {
            let shown = key_path(&self.shared.root, &key)
                .map_or_else(|| "a path".to_string(), |p| p.display().to_string());
            return Err(format!("{shown} answers {op:?} otherwise"));
        }
        Ok(())
    }

    /// This run's answer to a [`crate::tree`] question, asked once.
    fn answer(&self, op: Op, key: &[u8], snapshot: &Snapshot) -> Option<Digest> {
        if let Some(&known) = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(op, key.to_vec()))
        {
            return Some(known);
        }
        let path = key_path(&self.shared.root, key)?;
        let got = crate::tree::answer_now(op, &path, snapshot, &self.shared.root);
        self.answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((op, key.to_vec()), got);
        Some(got)
    }

    /// Keeps a unit's record whose answers all still hold ([`Self::reusable`]), and reports it
    /// reused.
    pub(crate) fn keep_reused(&self, record: UnitRecord) {
        self.report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reused
            .push(record.id.clone());
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record);
    }

    /// Keeps unit `id`, judged again for `why`: what it asked and concluded, or nothing when it
    /// could not run.
    pub(crate) fn keep_judged(&self, id: &str, why: Rejudged, judged: Option<(Trace, Verdict)>) {
        self.report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rejudged
            .push((id.to_string(), why));
        if let Some((mut trace, result)) = judged {
            trace.settle();
            self.kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(UnitRecord {
                    id: id.to_string(),
                    trace,
                    result,
                });
        }
    }

    /// Whether this run keeps a record at all (so its units should record what they ask).
    pub(crate) fn keeps(&self) -> bool {
        self.memo.is_some()
    }

    /// Writes this run's record (when it may keep one) and reports what it reused.
    pub(crate) fn finish(self, snapshot: &Snapshot) -> MemoReport {
        let mut report = self
            .report
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        report.reused.sort();
        report.rejudged.sort_by(|a, b| a.0.cmp(&b.0));
        let Some((memo, code)) = self.memo else {
            return report;
        };
        let not_kept = if let Some(why) = self.shared.tainted() {
            Some(why)
        } else if memo.code.is_none() && !self::code().is_some_and(code_unchanged) {
            Some("the running executable changed under the run".into())
        } else if let Some((_, why)) = snapshot.unwalked.first() {
            Some(format!("the source tree could not be walked ({why})"))
        } else {
            None
        };
        if let Some(why) = not_kept {
            report.not_kept = Some(why);
            return report;
        }
        let mut units = self
            .kept
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        units.sort_by(|a, b| a.id.cmp(&b.id));
        let globals = self.corpus.get().map_or([[0u8; 32]; 4], |c| {
            let mut g = c.globals;
            g[3] = self.engine;
            g
        });
        let record = Record {
            code,
            root: self.shared.root.as_os_str().as_encoded_bytes().to_vec(),
            files: snapshot
                .files
                .iter()
                .map(|f| (f.rel.clone(), f.digest))
                .collect(),
            globals,
            units,
        };
        if let Err(why) = write_atomically(&memo.path, &record.encode()) {
            report.not_kept = Some(format!("cannot write {}: {why}", memo.path.display()));
        }
        report
    }
}

fn global_name(fact: u8) -> &'static str {
    match fact {
        global::PATHS => "the checkout's paths changed",
        global::SCRIPTS => "a script changed",
        global::MANIFESTS => "a manifest changed",
        _ => "the engine's baseline changed",
    }
}

/// `trace`'s file indices into `was` re-pointed at the same files in `now`; `None` when one is
/// not in `now`.
fn remap(trace: &Trace, was: &[(String, Digest)], now: &Snapshot) -> Option<Trace> {
    let at = |i: u32| -> Option<u32> {
        let rel = &was.get(i as usize)?.0;
        u32::try_from(now.find(rel)?).ok()
    };
    let mut out = trace.clone();
    out.content = trace
        .content
        .iter()
        .map(|&f| at(f))
        .collect::<Option<_>>()?;
    out.mentions = trace
        .mentions
        .iter()
        .map(|&(f, s, a)| Some((at(f)?, s, a)))
        .collect::<Option<_>>()?;
    Some(out)
}

/// Writes `bytes` to `path` by a sibling temporary file renamed over it, so a reader sees the
/// old record or the new one, never part of one. Nothing is flushed: the record is a cache its
/// seal checks, so what a power loss leaves of it reads as no record ([`Record::decode`]).
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map_or_else(
        || "census.memo".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

impl MemoReport {
    /// The line a census transcript prints for it.
    #[must_use]
    pub fn line(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{self}");
        out
    }
}

#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;
