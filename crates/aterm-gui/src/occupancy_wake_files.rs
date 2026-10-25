// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BOOK'S FILE ATOMS (the occupancy slice-2 plan's 2.6): `file:PATH`, `gone:PATH` and
//! `file:PATH~/RE/`.
//!
//! A path is admitted lexically before anything at it is read ([`wake_atoms::admit`]: under a
//! session root, outside every guarded root, no dot, credential or link component, on a local
//! volume that no automounter serves and no removable media, in any spelling the volume reads as
//! the same), and admitted again before every read and every watch, so a folder swapped for a
//! link after the declaration is met at the folder and nothing past it is touched. Every read
//! refuses a link anywhere on the path ([`aterm_uds::nofollow`]); none resolves one. The atoms are
//! the user containment's alone (S6), and nothing is admitted before the window has set the
//! owner's `[privacy] protected_roots` ([`set_privacy`]).
//!
//! A watch is an event-only descriptor on the path's folder, and on the file itself for a pattern,
//! within one process-wide budget ([`watch_cap`]). Past it, or when an open fails, the atom is
//! read on the looks alone and `waits why` says `source=looks`; nothing is refused for the budget.
//! A pattern's reader reads what was appended from where it stopped (from 0 for a new file or a
//! shorter one), at most [`PER_EVENT`] per event and [`PER_LOOK`] per look, so a long append lags
//! and is never lost. Nothing of a line is kept but whether one matched.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use aterm_agent::supervise::occupancy::wake_atoms::{
    self, AdmittedPath, ContainmentWord, PathProbe, PathRefusal, Probed, Roots,
};
use aterm_agent::supervise::occupancy::wakes::{AtomPath, SourceAtom};
use aterm_agent::supervise::occupancy::wakes_seen::ExpireWhy;
use aterm_core::terminal::RowMatch;
use aterm_sysprobe::volumes::Table;

/// The session root every session has, under the home.
pub(crate) const SCRATCH: &str = "scratch.noindex";

/// The most a pattern's reader reads of its file at one event, bytes.
pub(crate) const PER_EVENT: u64 = 1 << 20;

/// The most a pattern's reader reads of its file at one look, bytes.
pub(crate) const PER_LOOK: u64 = 8 << 20;

/// The most one pass reads over every atom, bytes: sessions are served round-robin, so a session
/// the cap leaves unread is served first at the next pass.
pub(crate) const PER_PASS: u64 = 64 << 20;

/// One read's size, bytes.
const CHUNK: usize = 64 << 10;

/// The longest line judged whole, bytes; a longer one is judged in pieces of this length, so a
/// reader never holds more of an open line.
pub(crate) const LINE_MAX: usize = 64 << 10;

/// The fewest and the most watches the process holds ([`watch_cap`]).
const WATCH_MIN: usize = 16;
const WATCH_MAX: usize = 256;

/// THE PROCESS'S WATCH BUDGET: a sixteenth of its soft descriptor limit, 16 to 256 (16 where the
/// limit is not read).
pub(crate) fn watch_cap(soft_limit: Option<u64>) -> usize {
    soft_limit.map_or(WATCH_MIN, |soft| {
        usize::try_from(soft / 16)
            .unwrap_or(usize::MAX)
            .clamp(WATCH_MIN, WATCH_MAX)
    })
}

/// The reply refusing a file atom where none is served (off macOS).
pub(crate) const NOT_SERVED: &str = "ERR usage wake: file atoms are served on macOS alone\n";

/// The reply refusing a file atom outside the user containment (S6).
pub(crate) const CONTAINMENT: &str = "ERR denied wake: file atoms need containment=user\n";

/// What one read of a file atom found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// It holds, or it does not.
    Holds(bool),
    /// Its path can no longer be read: its wake ends so.
    Lost(ExpireWhy),
}

/// A shelf's file atoms as one pass read them; an atom absent was not read.
pub(crate) type FileLevels = Vec<(SourceAtom, Level)>;

/// Why a file atom is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// [`wake_atoms::admit`]'s refusal.
    Path(PathRefusal),
    /// It is on a volume that is not local, or that the automounter serves, or the mount table
    /// that would say so is not read whole.
    Remote,
    /// It is on removable media.
    Removable,
}

impl Refused {
    /// The declaration's reply.
    pub(crate) fn reply(self) -> String {
        match self {
            Refused::Path(PathRefusal::Containment) => CONTAINMENT.to_string(),
            Refused::Path(refusal) => format!("ERR denied wake: path {}\n", refusal.word()),
            Refused::Remote => "ERR denied wake: path remote\n".to_string(),
            Refused::Removable => "ERR denied wake: path removable\n".to_string(),
        }
    }

    /// Why a wake whose atom is refused again ends: a link met is `path`, any other refusal of
    /// the running rules `admit`.
    pub(crate) const fn why(self) -> ExpireWhy {
        match self {
            Refused::Path(PathRefusal::Link) => ExpireWhy::Path,
            Refused::Path(_) | Refused::Remote | Refused::Removable => ExpireWhy::Admit,
        }
    }
}

/// A watch's descriptor: the watch ends when it closes.
pub(crate) struct WatchFd(
    #[cfg(unix)]
    #[expect(dead_code, reason = "held for its close, which ends the watch")]
    pub(crate) std::os::fd::OwnedFd,
);

/// One watch that fired: its id, and whether its vnode left its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fired {
    /// The id it was opened under.
    pub(crate) id: u64,
    /// Its vnode was deleted, renamed or revoked.
    pub(crate) gone: bool,
}

/// What a watch is asked to hear: a folder (its file coming and going) or a regular file (its
/// growth). Anything else at the path (a FIFO, a socket, a device) is refused and never waited
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watched {
    /// A folder.
    Folder,
    /// A regular file.
    File,
}

impl Watched {
    /// The kind [`aterm_uds::nofollow::open_evtonly`] is asked for.
    #[cfg(unix)]
    pub(crate) const fn kind(self) -> aterm_uds::nofollow::Kind {
        match self {
            Watched::Folder => aterm_uds::nofollow::Kind::Dir,
            Watched::File => aterm_uds::nofollow::Kind::File,
        }
    }
}

/// WHAT A FILE ATOM'S ADMISSION AND READS TOUCH ([`Disk`], or a test's arm): every call on a
/// path reads it without following a link anywhere on it.
pub(crate) trait WakeFs: Send + Sync {
    /// Whether file atoms are served here (macOS).
    fn served(&self) -> bool;
    /// The home folder.
    fn home(&self) -> Option<PathBuf>;
    /// Every root admission guards for `home` ([`guarded_roots`]); `None` where the owner's are
    /// not known yet, when nothing is admitted.
    fn protected(&self, home: &Path) -> Option<Vec<PathBuf>>;
    /// The containment the sessions run under.
    fn containment(&self) -> ContainmentWord;
    /// `pid`'s working directory as the kernel has it.
    fn cwd(&self, pid: u32) -> Option<PathBuf>;
    /// The mount table, read without asking a remote volume.
    fn mounts(&self) -> Option<Table>;
    /// What is at `path` (`AT_SYMLINK_NOFOLLOW_ANY`).
    fn kind(&self, path: &Path) -> Probed;
    /// Watch the `what` at `path` under `id` (`O_EVTONLY | O_NONBLOCK | O_NOFOLLOW_ANY`, then
    /// `fstat`), never waiting on the open.
    /// # Errors
    /// The open's or the registration's, or `EFTYPE` for anything at `path` but `what`.
    fn watch(&self, path: &Path, what: Watched, id: u64) -> io::Result<WatchFd>;
    /// The watches that fired within `timeout`.
    fn wait(&self, timeout: Duration) -> Vec<Fired>;
    /// A reader of the regular file at `path` (`O_NONBLOCK | O_NOFOLLOW_ANY`, then `S_ISREG`).
    /// # Errors
    /// The open's, or `EFTYPE` for anything but a regular file.
    fn reader(&self, path: &Path) -> io::Result<std::fs::File>;
    /// The process's watch budget.
    fn watch_cap(&self) -> usize;
}

/// WHAT ONE PASS'S ADMISSIONS READ ONCE: the home, the guarded roots, the containment and the
/// mount table.
pub(crate) struct Ctx {
    home: PathBuf,
    protected: Vec<PathBuf>,
    containment: ContainmentWord,
    mounts: Option<Table>,
}

impl Ctx {
    /// `fs`'s, or `None` where it names no home or does not know the owner's guarded roots yet.
    pub(crate) fn of(fs: &dyn WakeFs) -> Option<Self> {
        let home = fs.home()?;
        Some(Self {
            protected: fs.protected(&home)?,
            containment: fs.containment(),
            mounts: fs.mounts(),
            home,
        })
    }

    /// Why `path` is on no volume admission may ask about ([`off_volume`]), by the table alone.
    fn off_volume(&self, path: &Path) -> Option<Refused> {
        off_volume(self.mounts.as_ref(), path)
    }
}

/// WHY `path` IS ON NO VOLUME ADMISSION MAY ASK ABOUT, by `table` alone: every mount that may hold
/// it ([`may_hold`]: in any spelling its volume reads as the same) must be local, served by no
/// automounter and no removable media. `Remote` where one is not local or is automounted, or the
/// table is unread, incomplete or names no mount that holds it; else `Removable` where one is
/// removable.
pub(crate) fn off_volume(table: Option<&Table>, path: &Path) -> Option<Refused> {
    let Some(table) = table.filter(|table| table.complete) else {
        return Some(Refused::Remote);
    };
    let mut held = false;
    let mut why = None;
    for mount in table.mounts.iter().filter(|m| may_hold(table, &m.on, path)) {
        held = true;
        if !mount.local || mount.automounted {
            return Some(Refused::Remote);
        }
        if mount.removable {
            why = Some(Refused::Removable);
        }
    }
    if held { why } else { Some(Refused::Remote) }
}

/// WHETHER THE MOUNT AT `on` MAY HOLD `path`: `on` is a prefix of it, component by component, in
/// a spelling a case-insensitive volume reads as the same ([`same_folded`]), each side spelled as
/// given and through the table's firmlinks (`/Users` on the data volume).
fn may_hold(table: &Table, on: &Path, path: &Path) -> bool {
    let paths = spellings(table, path);
    spellings(table, on)
        .iter()
        .any(|on| paths.iter().any(|path| prefix_folded(on, path)))
}

/// `path` as given, and through each firmlink whose root is a prefix of it ([`prefix_folded`]).
fn spellings(table: &Table, path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    for (root, data) in &table.firmlinks {
        if prefix_folded(root, path) {
            let rest: PathBuf = path.components().skip(root.components().count()).collect();
            out.push(data.join(rest));
        }
    }
    out
}

/// Whether `prefix` is a prefix of `path`, component by component, each pair [`same_folded`].
fn prefix_folded(prefix: &Path, path: &Path) -> bool {
    let mut path = path.components();
    prefix.components().all(|part| {
        path.next()
            .is_some_and(|at| same_folded(part.as_os_str(), at.as_os_str()))
    })
}

/// Whether two components may name one entry on a case-insensitive volume: equal ignoring ASCII
/// case where both are ASCII; where either is not, they may (a volume folds case and normalizes
/// past ASCII, `K` onto `k`), so they are taken to.
fn same_folded(a: &OsStr, b: &OsStr) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) if a.is_ascii() && b.is_ascii() => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

/// The probe admission asks: a path on no volume it may ask about is answered without a read,
/// and why remembered.
struct Probe<'a> {
    fs: &'a dyn WakeFs,
    ctx: &'a Ctx,
    refused: std::cell::Cell<Option<Refused>>,
}

impl<'a> Probe<'a> {
    fn new(fs: &'a dyn WakeFs, ctx: &'a Ctx) -> Self {
        Self {
            fs,
            ctx,
            refused: std::cell::Cell::new(None),
        }
    }
}

impl PathProbe for Probe<'_> {
    fn probe(&self, path: &Path) -> Probed {
        if let Some(refused) = self.ctx.off_volume(path) {
            self.refused.set(Some(refused));
            return Probed::Other;
        }
        self.fs.kind(path)
    }
}

/// Whether `dir` lies below `home` (never `home` itself).
fn below_home(dir: &Path, home: &Path) -> bool {
    dir != home && dir.starts_with(home)
}

/// ADMIT `path`, a wake's, for the processes `pids`: its roots are `~/scratch.noindex` and the
/// kernel's working directories of `pids` below the home (never a tab's reported folder, which a
/// program sets). Nothing at the path is read before the lexical checks pass.
/// # Errors
/// [`Refused`]: the first reason.
pub(crate) fn admit(
    fs: &dyn WakeFs,
    ctx: Option<&Ctx>,
    pids: &[u32],
    path: &AtomPath,
) -> Result<AdmittedPath, Refused> {
    let ctx = ctx.ok_or(Refused::Path(PathRefusal::Relative))?;
    let cwds: Vec<PathBuf> = pids
        .iter()
        .filter_map(|pid| fs.cwd(*pid))
        .filter(|cwd| below_home(cwd, &ctx.home))
        .collect();
    let roots = wake_atoms::roots(&ctx.home, &cwds, &ctx.home.join(SCRATCH), &ctx.protected);
    admitted_under(fs, ctx, &roots, path)
}

fn admitted_under(
    fs: &dyn WakeFs,
    ctx: &Ctx,
    roots: &Roots,
    path: &AtomPath,
) -> Result<AdmittedPath, Refused> {
    let probe = Probe::new(fs, ctx);
    let admitted = wake_atoms::admit(
        path.as_str(),
        path.home_relative(),
        roots,
        &ctx.protected,
        ctx.containment,
        &probe,
    )
    .map_err(Refused::Path)?;
    if let Some(refused) = probe
        .refused
        .get()
        .or_else(|| ctx.off_volume(&admitted.path))
    {
        return Err(refused);
    }
    Ok(admitted)
}

/// The path of a file atom.
pub(crate) const fn path_of(atom: &SourceAtom) -> Option<&AtomPath> {
    match atom {
        SourceAtom::File { path, .. } | SourceAtom::Gone { path } => Some(path),
        SourceAtom::Pid { .. } => None,
    }
}

/// A file atom a shelf wants read: the processes whose working directories root it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Want {
    /// The atom.
    pub(crate) atom: SourceAtom,
    /// The live processes it is declared for.
    pub(crate) pids: Vec<u32>,
}

/// One watch: its id, its descriptor, and whether its vnode left its name.
struct Watch {
    id: u64,
    _fd: WatchFd,
    gone: bool,
}

/// A PATTERN'S READER: the file it reads (device and inode), where it stopped, the line it
/// stopped inside, and whether a line matched. Its `Debug` holds numbers alone.
#[derive(Default)]
struct Reader {
    file: Option<(u64, u64)>,
    offset: u64,
    tail: Vec<u8>,
    matched: bool,
}

impl fmt::Debug for Reader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Reader(offset={}, tail={} bytes, matched={})",
            self.offset,
            self.tail.len(),
            self.matched
        )
    }
}

fn judge(matcher: &dyn RowMatch, line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    matcher.matches(&String::from_utf8_lossy(line))
}

impl Reader {
    /// READ `file` (`id`, `size` bytes now) from where the last read stopped, at most `budget`
    /// bytes: whether a line of it matches.
    fn read(
        &mut self,
        file: &mut std::fs::File,
        id: (u64, u64),
        size: u64,
        matcher: &dyn RowMatch,
        budget: u64,
    ) -> io::Result<(bool, u64)> {
        if self.file != Some(id) || size < self.offset {
            *self = Self {
                file: Some(id),
                ..Self::default()
            };
        }
        if self.matched {
            return Ok((true, 0));
        }
        let start = self.offset;
        let end = size.min(self.offset.saturating_add(budget));
        file.seek(io::SeekFrom::Start(self.offset))?;
        let mut buf = vec![0u8; CHUNK];
        while self.offset < end {
            let want = usize::try_from(end - self.offset)
                .unwrap_or(CHUNK)
                .min(CHUNK);
            let got = file.read(&mut buf[..want])?;
            if got == 0 {
                break;
            }
            self.offset = self.offset.saturating_add(got as u64);
            if self.scan(&buf[..got], matcher) {
                self.matched = true;
                break;
            }
        }
        if !self.matched && self.offset >= size && !self.tail.is_empty() {
            // The last line, with no newline yet: judged as it stands, kept for what follows.
            self.matched = judge(matcher, &self.tail);
        }
        if self.matched {
            self.tail = Vec::new();
        }
        Ok((self.matched, self.offset.saturating_sub(start)))
    }

    /// Judge each line `bytes` ends; keep the one it leaves open.
    fn scan(&mut self, bytes: &[u8], matcher: &dyn RowMatch) -> bool {
        let mut rest = bytes;
        while let Some(at) = rest.iter().position(|b| *b == b'\n') {
            let hit = if self.tail.is_empty() {
                judge(matcher, &rest[..at])
            } else {
                self.tail.extend_from_slice(&rest[..at]);
                let hit = judge(matcher, &self.tail);
                self.tail.clear();
                hit
            };
            rest = &rest[at + 1..];
            if hit {
                return true;
            }
        }
        self.tail.extend_from_slice(rest);
        if self.tail.len() > LINE_MAX {
            let hit = judge(matcher, &self.tail);
            self.tail.clear();
            return hit;
        }
        false
    }
}

/// ONE FILE ATOM OF A SHELF as the sources read it.
struct Entry {
    atom: SourceAtom,
    admitted: AdmittedPath,
    matcher: Option<Arc<dyn RowMatch>>,
    /// The watch on the path's folder, which hears the file come and go.
    dir: Option<Watch>,
    /// A pattern's watch on the file, which hears it grow.
    file: Option<Watch>,
    /// It is read on the looks alone (the budget spent, or an open failed).
    looks: bool,
    reader: Reader,
    /// Its path can no longer be read.
    lost: Option<ExpireWhy>,
}

impl Entry {
    fn new(atom: SourceAtom, admitted: AdmittedPath) -> Self {
        let matcher = match &atom {
            SourceAtom::File {
                pattern: Some(pattern),
                ..
            } => aterm_observe::row_matcher(pattern.as_str()).ok(),
            SourceAtom::File { pattern: None, .. }
            | SourceAtom::Gone { .. }
            | SourceAtom::Pid { .. } => None,
        };
        Self {
            atom,
            admitted,
            matcher,
            dir: None,
            file: None,
            looks: false,
            reader: Reader::default(),
            lost: None,
        }
    }

    fn pattern(&self) -> bool {
        matches!(
            self.atom,
            SourceAtom::File {
                pattern: Some(_),
                ..
            }
        )
    }

    /// The folder its watch hears, where it lies within the atom's root.
    fn folder(&self) -> Option<&Path> {
        self.admitted
            .path
            .parent()
            .filter(|dir| dir.starts_with(&self.admitted.root))
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Entry({:?}, dir={}, file={}, looks={}, {:?}, lost={:?})",
            self.atom.kind(),
            self.dir.is_some(),
            self.file.is_some(),
            self.looks,
            self.reader,
            self.lost
        )
    }
}

/// THE PROCESS'S FILE ATOMS: each shelf's, the watches they hold and the budget they draw on.
#[derive(Debug, Default)]
pub(crate) struct Files {
    shelves: BTreeMap<String, Vec<Entry>>,
    /// Each watch's shelf, by id.
    ids: BTreeMap<u64, String>,
    next_id: u64,
    /// The watches held.
    open: usize,
    /// The shelf a pass serves first.
    turn: usize,
}

/// The watches an entry holds back to the budget.
fn release(open: &mut usize, ids: &mut BTreeMap<u64, String>, watch: &mut Option<Watch>) {
    if let Some(watch) = watch.take() {
        *open = open.saturating_sub(1);
        ids.remove(&watch.id);
    }
}

impl Files {
    /// The watches held.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) const fn open(&self) -> usize {
        self.open
    }

    /// The ids of the watches held.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn watch_ids(&self) -> Vec<u64> {
        self.ids.keys().copied().collect()
    }

    /// `sid`'s entries no want names any more go, and their watches.
    pub(crate) fn keep(&mut self, sid: &str, wants: &[Want]) {
        let Self {
            shelves, ids, open, ..
        } = self;
        let Some(entries) = shelves.get_mut(sid) else {
            return;
        };
        entries.retain_mut(|entry| {
            let keep = wants.iter().any(|w| w.atom == entry.atom);
            if !keep {
                release(open, ids, &mut entry.dir);
                release(open, ids, &mut entry.file);
            }
            keep
        });
        if entries.is_empty() {
            shelves.remove(sid);
        }
    }

    /// KEEP `sid`'s ENTRIES FOR `wants` ALONE: each new one admitted (from `fresh`, admitted at
    /// its declaration, else now), each no longer wanted released.
    pub(crate) fn sync(
        &mut self,
        fs: &dyn WakeFs,
        ctx: Option<&Ctx>,
        sid: &str,
        wants: &[Want],
        fresh: &[(SourceAtom, AdmittedPath)],
    ) {
        self.keep(sid, wants);
        let entries = self.shelves.entry(sid.to_string()).or_default();
        for want in wants {
            if entries.iter().any(|e| e.atom == want.atom) {
                continue;
            }
            let Some(path) = path_of(&want.atom) else {
                continue;
            };
            let admitted = fresh
                .iter()
                .find(|(atom, _)| *atom == want.atom)
                .map(|(_, admitted)| Ok(admitted.clone()))
                .unwrap_or_else(|| admit(fs, ctx, &want.pids, path));
            let entry = match admitted {
                Ok(admitted) => Entry::new(want.atom.clone(), admitted),
                Err(refused) => {
                    let mut entry = Entry::new(
                        want.atom.clone(),
                        AdmittedPath {
                            path: PathBuf::new(),
                            root: PathBuf::new(),
                        },
                    );
                    entry.lost = Some(refused.why());
                    entry
                }
            };
            entries.push(entry);
        }
        if entries.is_empty() {
            self.shelves.remove(sid);
        }
    }

    /// The shelves holding entries.
    pub(crate) fn sids(&self) -> Vec<String> {
        self.shelves.keys().cloned().collect()
    }

    /// The shelves in the order a pass serves them: from the turn on, the turn moving one on.
    pub(crate) fn order(&mut self, mut sids: Vec<String>) -> Vec<String> {
        if !sids.is_empty() {
            let at = self.turn % sids.len();
            sids.rotate_left(at);
            self.turn = self.turn.wrapping_add(1);
        }
        sids
    }

    /// ONE PASS OVER `wants`' SHELVES in [`Self::order`]: each kept for its wants alone and read
    /// ([`Self::read`]), at most `per_atom` bytes of each pattern's file and `budget` over the
    /// pass, so a shelf the budget leaves unread is served first at the next pass; it stops before
    /// the first shelf `reading` refuses (the owner's `off` came into force meanwhile).
    pub(crate) fn pass(
        &mut self,
        fs: &dyn WakeFs,
        ctx: Option<&Ctx>,
        wants: &BTreeMap<String, Vec<Want>>,
        per_atom: u64,
        budget: u64,
        reading: &dyn Fn() -> bool,
    ) -> BTreeMap<String, FileLevels> {
        let mut out = BTreeMap::new();
        let mut left = budget;
        for sid in self.order(wants.keys().cloned().collect()) {
            if !reading() {
                break;
            }
            let want = wants.get(&sid).map_or(&[][..], Vec::as_slice);
            self.sync(fs, ctx, &sid, want, &[]);
            let read = self.read(fs, ctx, &sid, per_atom, &mut left);
            out.insert(sid, read);
        }
        out
    }

    /// READ `sid`'s ENTRIES: each admitted again, its watches armed, its level read, at most
    /// `per_atom` bytes of a pattern's file and `left` over the pass.
    pub(crate) fn read(
        &mut self,
        fs: &dyn WakeFs,
        ctx: Option<&Ctx>,
        sid: &str,
        per_atom: u64,
        left: &mut u64,
    ) -> FileLevels {
        let cap = fs.watch_cap();
        let Self {
            shelves,
            ids,
            next_id,
            open,
            ..
        } = self;
        let Some(entries) = shelves.get_mut(sid) else {
            return Vec::new();
        };
        let mut levels = Vec::with_capacity(entries.len());
        for entry in entries.iter_mut() {
            let mut arms = Arms {
                fs,
                sid,
                cap,
                ids,
                next_id,
                open,
            };
            let level = arms.read(ctx, entry, per_atom, left);
            if let Some(level) = level {
                levels.push((entry.atom.clone(), level));
            }
        }
        levels
    }

    /// The shelves whose watches `fired` names, each fired watch that went marked to be armed
    /// again.
    pub(crate) fn fired(&mut self, fired: &[Fired]) -> Vec<String> {
        let mut sids: Vec<String> = Vec::new();
        for fire in fired {
            let Some(sid) = self.ids.get(&fire.id).cloned() else {
                continue;
            };
            if fire.gone
                && let Some(entries) = self.shelves.get_mut(&sid)
            {
                for watch in entries
                    .iter_mut()
                    .flat_map(|e| [e.dir.as_mut(), e.file.as_mut()])
                    .flatten()
                    .filter(|w| w.id == fire.id)
                {
                    watch.gone = true;
                }
            }
            if !sids.contains(&sid) {
                sids.push(sid);
            }
        }
        sids
    }

    /// `sid`'s entries go, and their watches.
    pub(crate) fn forget(&mut self, sid: &str) {
        let Some(mut entries) = self.shelves.remove(sid) else {
            return;
        };
        for entry in &mut entries {
            release(&mut self.open, &mut self.ids, &mut entry.dir);
            release(&mut self.open, &mut self.ids, &mut entry.file);
        }
    }

    /// Every entry goes, and every watch (the owner's `off`).
    pub(crate) fn clear(&mut self) {
        let sids: Vec<String> = self.shelves.keys().cloned().collect();
        for sid in sids {
            self.forget(&sid);
        }
    }

    /// `sid`'s atoms, each with whether it is read on the looks alone.
    pub(crate) fn sources(&self, sid: &str) -> Vec<(SourceAtom, bool)> {
        self.shelves.get(sid).map_or_else(Vec::new, |entries| {
            entries
                .iter()
                .filter(|e| e.lost.is_none())
                .map(|e| (e.atom.clone(), e.looks))
                .collect()
        })
    }
}

/// One entry's read, with what arming its watches draws on.
struct Arms<'a> {
    fs: &'a dyn WakeFs,
    sid: &'a str,
    cap: usize,
    ids: &'a mut BTreeMap<u64, String>,
    next_id: &'a mut u64,
    open: &'a mut usize,
}

impl Arms<'_> {
    fn lose(&mut self, entry: &mut Entry, why: ExpireWhy) -> Option<Level> {
        self.release(entry);
        entry.lost = Some(why);
        Some(Level::Lost(why))
    }

    fn release(&mut self, entry: &mut Entry) {
        release(self.open, self.ids, &mut entry.dir);
        release(self.open, self.ids, &mut entry.file);
    }

    fn read(
        &mut self,
        ctx: Option<&Ctx>,
        entry: &mut Entry,
        per_atom: u64,
        left: &mut u64,
    ) -> Option<Level> {
        if let Some(why) = entry.lost {
            return Some(Level::Lost(why));
        }
        let Some(ctx) = ctx else {
            return self.lose(entry, ExpireWhy::Admit);
        };
        let path = path_of(&entry.atom)?.clone();
        // The root admitted at the declaration, judged again with the path under the rules now.
        let roots = wake_atoms::roots(
            &ctx.home,
            std::slice::from_ref(&entry.admitted.root),
            &ctx.home.join(SCRATCH),
            &ctx.protected,
        );
        if let Err(refused) = admitted_under(self.fs, ctx, &roots, &path) {
            return self.lose(entry, refused.why());
        }
        if let Err(why) = self.arm_folder(entry) {
            return self.lose(entry, why);
        }
        let at = entry.admitted.path.clone();
        let level = match (&entry.atom, entry.matcher.clone()) {
            (SourceAtom::File { pattern: None, .. }, _) => match self.fs.kind(&at) {
                Probed::Link => return self.lose(entry, ExpireWhy::Path),
                kind => Level::Holds(kind == Probed::File),
            },
            (SourceAtom::Gone { .. }, _) => match self.fs.kind(&at) {
                Probed::Link => return self.lose(entry, ExpireWhy::Path),
                kind => Level::Holds(kind == Probed::Missing),
            },
            (
                SourceAtom::File {
                    pattern: Some(_), ..
                },
                Some(matcher),
            ) => match self.read_pattern(entry, &at, &*matcher, per_atom, left) {
                Ok(held) => Level::Holds(held),
                Err(e) if met_link(&e) => return self.lose(entry, ExpireWhy::Path),
                Err(e) if nothing_regular(&e) => {
                    entry.reader = Reader::default();
                    release(self.open, self.ids, &mut entry.file);
                    Level::Holds(false)
                }
                Err(_) => return None,
            },
            (
                SourceAtom::File {
                    pattern: Some(_), ..
                },
                None,
            )
            | (SourceAtom::Pid { .. }, _) => {
                return None;
            }
        };
        Some(level)
    }

    /// A pattern's read: the file opened again, its new bytes read, its own watch armed.
    fn read_pattern(
        &mut self,
        entry: &mut Entry,
        at: &Path,
        matcher: &dyn RowMatch,
        per_atom: u64,
        left: &mut u64,
    ) -> io::Result<bool> {
        let mut file = self.fs.reader(at)?;
        let (dev, ino, size) = identity_of(&file)?;
        let (held, read) =
            entry
                .reader
                .read(&mut file, (dev, ino), size, matcher, per_atom.min(*left))?;
        *left = left.saturating_sub(read);
        drop(file);
        if entry.file.as_ref().is_some_and(|w| w.gone) {
            release(self.open, self.ids, &mut entry.file);
        }
        if !held && entry.file.is_none() && !entry.looks {
            match self.open_watch(at, Watched::File) {
                Ok(watch) => entry.file = Some(watch),
                Err(e) if met_link(&e) => return Err(e),
                Err(_) => {
                    entry.looks = true;
                    self.release(entry);
                }
            }
        }
        Ok(held)
    }

    /// The folder's watch, armed where it went or was never held (and the budget allows).
    fn arm_folder(&mut self, entry: &mut Entry) -> Result<(), ExpireWhy> {
        if entry.dir.as_ref().is_some_and(|w| w.gone) {
            release(self.open, self.ids, &mut entry.dir);
        }
        if entry.file.as_ref().is_some_and(|w| w.gone) {
            release(self.open, self.ids, &mut entry.file);
        }
        if entry.dir.is_some() {
            return Ok(());
        }
        let Some(folder) = entry.folder().map(Path::to_path_buf) else {
            entry.looks = true;
            return Ok(());
        };
        let needs = 1 + usize::from(entry.pattern());
        if *self.open + needs > self.cap {
            entry.looks = true;
            self.release(entry);
            return Ok(());
        }
        match self.open_watch(&folder, Watched::Folder) {
            Ok(watch) => {
                entry.dir = Some(watch);
                entry.looks = false;
                Ok(())
            }
            Err(e) if met_link(&e) => Err(ExpireWhy::Path),
            Err(_) => {
                entry.looks = true;
                self.release(entry);
                Ok(())
            }
        }
    }

    fn open_watch(&mut self, path: &Path, what: Watched) -> io::Result<Watch> {
        if *self.open >= self.cap {
            return Err(io::Error::from(io::ErrorKind::QuotaExceeded));
        }
        *self.next_id = self.next_id.wrapping_add(1);
        let id = *self.next_id;
        let fd = self.fs.watch(path, what, id)?;
        *self.open += 1;
        self.ids.insert(id, self.sid.to_string());
        Ok(Watch {
            id,
            _fd: fd,
            gone: false,
        })
    }
}

/// This account's own home as the account database has it, whatever `$HOME` says.
fn account_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        aterm_uds::account::home()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// A link met on the way (`ELOOP`).
fn met_link(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        aterm_uds::nofollow::is_link(error)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Nothing there, or not a regular file.
fn nothing_regular(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        aterm_uds::nofollow::is_missing(error) || aterm_uds::nofollow::not_regular(error)
    }
    #[cfg(not(unix))]
    {
        error.kind() == io::ErrorKind::NotFound
    }
}

/// A reader's file: its device, inode and size.
fn identity_of(file: &std::fs::File) -> io::Result<(u64, u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = file.metadata()?;
        Ok((meta.dev(), meta.ino(), meta.len()))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

// ---------------------------------------------------------------------------------------------
// The product's arm.
// ---------------------------------------------------------------------------------------------

/// `[privacy] protected_roots` as the owner's config last resolved them ([`set_privacy`]); `None`
/// until the window first sets them.
#[cfg(not(test))]
static WAKE_PRIVACY: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// The roots this test thread set ([`set_privacy`]): parallel tests never read another's.
    static PRIVACY_FOR_TEST: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// The owner's `[privacy] protected_roots`, resolved, which every admission guards beside the
/// fixed set (the window's install and each config reload).
pub(crate) fn set_privacy(roots: Vec<PathBuf>) {
    #[cfg(test)]
    PRIVACY_FOR_TEST.with(|privacy| *privacy.borrow_mut() = Some(roots));
    #[cfg(not(test))]
    {
        *WAKE_PRIVACY.lock().unwrap_or_else(PoisonError::into_inner) = Some(roots);
    }
}

/// The owner's resolved `[privacy] protected_roots` ([`set_privacy`]); `None` before the window
/// sets them.
pub(crate) fn privacy() -> Option<Vec<PathBuf>> {
    #[cfg(test)]
    {
        PRIVACY_FOR_TEST.with(|privacy| privacy.borrow().clone())
    }
    #[cfg(not(test))]
    {
        WAKE_PRIVACY
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// EVERY ROOT ADMISSION GUARDS for `homes` (the `$HOME`, and the account's own where it differs)
/// and `owner`, the owner's resolved `[privacy] protected_roots`, which can only add: each home's
/// roots whose `stat` can raise a privacy dialog, and the folder holding every other home.
pub(crate) fn guarded_roots(homes: &[PathBuf], owner: &[PathBuf]) -> Vec<PathBuf> {
    use aterm_containment::consent::{other_homes_root, tcc_prompting_roots};
    let mut roots = owner.to_vec();
    for home in homes {
        roots.extend(tcc_prompting_roots(home, &[]));
        roots.extend(other_homes_root(home));
    }
    roots
}

/// The word `mode` reads as for admission; a mode this build does not name reads as the
/// narrowest, which admits no file atom.
pub(crate) const fn containment_word(mode: aterm_containment::ContainmentMode) -> ContainmentWord {
    use aterm_containment::ContainmentMode as Cm;
    match mode {
        Cm::User => ContainmentWord::User,
        Cm::Safety => ContainmentWord::Safety,
        _ => ContainmentWord::Containment,
    }
}

/// The kqueue every product watch registers on (named apart: the lazy-init census merges cells by
/// name).
#[cfg(unix)]
static WAKE_WATCHER: std::sync::LazyLock<Option<aterm_uds::nofollow::Watcher>> =
    std::sync::LazyLock::new(|| aterm_uds::nofollow::Watcher::new().ok());

/// The product's [`WakeFs`].
pub(crate) struct Disk;

impl WakeFs for Disk {
    fn served(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
    }

    fn protected(&self, home: &Path) -> Option<Vec<PathBuf>> {
        let owner = privacy()?;
        let homes: Vec<PathBuf> = std::iter::once(home.to_path_buf())
            .chain(account_home().filter(|account| account != home))
            .collect();
        Some(guarded_roots(&homes, &owner))
    }

    fn containment(&self) -> ContainmentWord {
        containment_word(aterm_containment::mode_or_containment())
    }

    fn cwd(&self, pid: u32) -> Option<PathBuf> {
        aterm_sysprobe::process_cwd(pid)
    }

    fn mounts(&self) -> Option<Table> {
        Some(aterm_sysprobe::volumes::table())
    }

    fn kind(&self, path: &Path) -> Probed {
        #[cfg(unix)]
        {
            use aterm_uds::nofollow::{Kind, kind_of};
            match kind_of(path) {
                Ok(Kind::Missing) => Probed::Missing,
                Ok(Kind::Dir) => Probed::Dir,
                Ok(Kind::File) => Probed::File,
                Ok(Kind::Link) => Probed::Link,
                Ok(Kind::Other) | Err(_) => Probed::Other,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Probed::Other
        }
    }

    fn watch(&self, path: &Path, what: Watched, id: u64) -> io::Result<WatchFd> {
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let watcher = WAKE_WATCHER
                .as_ref()
                .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
            let fd = aterm_uds::nofollow::open_evtonly(path, what.kind())?;
            watcher.add(fd.as_fd(), id)?;
            Ok(WatchFd(fd))
        }
        #[cfg(not(unix))]
        {
            let _ = (path, what, id);
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    fn wait(&self, timeout: Duration) -> Vec<Fired> {
        #[cfg(unix)]
        if let Some(watcher) = WAKE_WATCHER.as_ref() {
            return watcher.wait(timeout).map_or_else(
                |_| {
                    std::thread::sleep(timeout);
                    Vec::new()
                },
                |fired| {
                    fired
                        .into_iter()
                        .map(|f| Fired {
                            id: f.id,
                            gone: f.gone,
                        })
                        .collect()
                },
            );
        }
        std::thread::sleep(timeout);
        Vec::new()
    }

    fn reader(&self, path: &Path) -> io::Result<std::fs::File> {
        #[cfg(unix)]
        {
            aterm_uds::nofollow::open_reader(path).map(std::fs::File::from)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    fn watch_cap(&self) -> usize {
        watch_cap(crate::control::soft_descriptor_limit())
    }
}
