// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GUI'S BOOK OF DECLARED WAKES (the occupancy slice-2 plan's 2.5 and 2.6): each session's
//! [`WakeBook`] and its wake K clock behind one process-wide lock ([`WakeHost`]), the `wake`,
//! `waits` and `await wake` verbs, and the one plain thread that reads their time, pid, machine
//! and file levels ([`install`]).
//!
//! Every verb that changes or consumes a book first proves its peer is of the session's own tree
//! ([`declarer::prove`] on the kernel's peer pid, the plan's S1); under the owner's `off` every
//! verb is refused and every book ends ([`note_mode`]), a carried book arrives ended, and the
//! mode is asked again under the books' lock, so nothing books or answers past a switch to
//! `off`. Nothing here types into a session: the
//! `await wake` answer is the agent's own reply on its own connection, told only once it is on
//! the wire ([`hang`], S2), and the K Entry and the `wake #<n> ready` event go up only under the
//! owner's `prompt` ([`crate::occupancy_host::prompt_now`], W4 and W5). A file atom's path is
//! admitted lexically before anything at it is read, and admitted again before every read, which
//! never follows a link ([`files`]); its reads run outside the books' lock.

use std::collections::BTreeMap;
#[cfg(not(test))]
use std::sync::LazyLock;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use aterm_agent::supervise::occupancy::declarer::{self, AgentId, DeclarerRefusal, ProcLink};
use aterm_agent::supervise::occupancy::due::{DueClock, K_LOOKS};
use aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS;
use aterm_agent::supervise::occupancy::rules::RuleId;
use aterm_agent::supervise::occupancy::wake_atoms::AdmittedPath;
use aterm_agent::supervise::occupancy::wakes::{
    Atom, Change, DeclaredWake, Ended, Levels, ParkAnswer, ParkBook, SourceAtom, SysWait, WakeBook,
    WakeReq,
};
use aterm_agent::supervise::occupancy::wakes_seen::{Declarer, ExpireWhy, WakeState};
use aterm_agent::supervise::{Guard, Occupancy};
use aterm_types::MutexExt as _;
use aterm_vitals::model::{SlotsView, Snapshot};
use aterm_vitals::sys::predicate::{Latch, evaluate};

use crate::occupancy_host::{Reserve, fault};

#[path = "occupancy_wake_files.rs"]
mod files;
#[cfg(test)]
pub(crate) use files::privacy;
pub(crate) use files::set_privacy;
use files::{FileLevels, Files, Level, WakeFs, Want};

/// The tick on which pid levels, agents and time atoms are read again.
pub(crate) const TICK: Duration = Duration::from_secs(5);

/// Ticks to a look (the K clock's sample): [`LOOK_EVERY_MS`] apart.
const TICKS_A_LOOK: u32 = (LOOK_EVERY_MS / 5_000) as u32;

/// The least time between two edge passes of the sources thread.
const EDGE_GAP: Duration = Duration::from_millis(50);

/// The longest ancestry a proof walks.
const CHAIN_MAX: usize = 64;

/// Every verb's refusal under the owner's `off`.
pub(crate) const OFF: &str = "ERR denied wake: occupancy=off\n";

/// `wake rm|taken` usage.
const RM_USAGE: &str = "ERR usage: wake rm #<n> | wake taken #<n>\n";

/// `waits` usage.
const WAITS_USAGE: &str = "ERR usage: waits [why [#<n>]]\n";

/// `await wake` usage.
const AWAIT_USAGE: &str = "ERR usage: await wake [timeout=<ms>]\n";

/// The kernel's process records as a proof walks them ([`Kernel`], or a test's arm).
pub(crate) trait ProcTree: Send + Sync {
    /// A live link, a definite exit, or a record that could not be read.
    fn read(&self, pid: u32) -> ProcRead;

    /// The positive identity evidence a proof may walk; unread records prove no link.
    fn link(&self, pid: u32) -> Option<ProcLink> {
        match self.read(pid) {
            ProcRead::Alive(link) => Some(link),
            ProcRead::Exited | ProcRead::Unread => None,
        }
    }
}

/// One process read: unknown evidence is never a positive exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcRead {
    Alive(ProcLink),
    Exited,
    Unread,
}

impl ProcRead {
    fn from_record(read: std::io::Result<Option<aterm_sysprobe::kproc::Record>>) -> Self {
        match read {
            Ok(Some(record)) if !record.zombie => Self::Alive(ProcLink {
                pid: record.pid,
                birth_us: birth_us(record.start_s, record.start_us),
                ppid: record.ppid,
            }),
            Ok(_) => Self::Exited,
            Err(_) => Self::Unread,
        }
    }
}

/// The product's [`ProcTree`]: `kproc::record`.
pub(crate) struct Kernel;

impl ProcTree for Kernel {
    fn read(&self, pid: u32) -> ProcRead {
        ProcRead::from_record(aterm_sysprobe::kproc::record_read(pid))
    }
}

/// A kernel start as unix µs.
pub(crate) fn birth_us(start_s: u64, start_us: u32) -> i64 {
    i64::try_from(start_s)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(start_us))
}

/// `pid`'s ancestry as `tree` reads it, `pid` first, up to and including `stop` (or the root, a
/// missing link, or [`CHAIN_MAX`] links).
pub(crate) fn chain(tree: &dyn ProcTree, pid: u32, stop: u32) -> Vec<ProcLink> {
    let mut links = Vec::new();
    let mut at = pid;
    while links.len() < CHAIN_MAX {
        let Some(link) = tree.link(at) else {
            break;
        };
        links.push(link);
        if link.pid == stop || link.ppid == 0 || link.ppid == link.pid {
            break;
        }
        at = link.ppid;
    }
    links
}

/// THE SESSION A VERB TARGETS, as the host knows it at the verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seat {
    /// Its sid (`s-<hex>`).
    pub(crate) sid: String,
    /// Its local id.
    pub(crate) local: u64,
    /// The tab's PTY child as its session took it, pid and birth: the root of the session's tree;
    /// `None` where it is not known.
    pub(crate) pty_child: Option<AgentId>,
    /// The hosted agent's pid and kernel start (unix seconds), where the harness's snapshot names
    /// one: the proof narrows to it.
    pub(crate) agent: Option<(u32, i64)>,
}

/// `seat`'s root where its pid still names the process its session took: a root gone, or its pid
/// another process's since, is no root.
fn live_root(tree: &dyn ProcTree, seat: &Seat) -> Option<AgentId> {
    seat.pty_child
        .filter(|root| tree.link(root.pid).is_some_and(|live| live.id() == *root))
}

/// THE PROOF that `peer` is of `seat`'s tree: the agent process its wakes are declared for.
/// # Errors
/// [`DeclarerRefusal`]; a tab whose PTY child is not known, gone or reused proves nothing
/// (`NotInTree`).
pub(crate) fn proved(
    tree: &dyn ProcTree,
    seat: &Seat,
    peer: Option<u32>,
) -> Result<AgentId, DeclarerRefusal> {
    let Some(peer) = peer else {
        return Err(DeclarerRefusal::NoPeer);
    };
    let Some(root) = live_root(tree, seat) else {
        return Err(DeclarerRefusal::NotInTree);
    };
    let agent = match seat.agent {
        Some((pid, start_s)) => match tree.read(pid) {
            ProcRead::Unread => return Err(DeclarerRefusal::NotInTree),
            ProcRead::Alive(link) if link.birth_us.div_euclid(1_000_000) == start_s => {
                Some(link.id())
            }
            ProcRead::Alive(_) | ProcRead::Exited => None,
        },
        None => None,
    };
    declarer::prove(&chain(tree, peer, root.pid), root, agent)
}

/// Whether `pid` is of `seat`'s tree (a `pid:<n> exited` atom's own process).
fn of_tree(tree: &dyn ProcTree, seat: &Seat, pid: u32) -> Option<ProcLink> {
    let root = live_root(tree, seat)?;
    let links = chain(tree, pid, root.pid);
    declarer::prove(&links, root, None).ok()?;
    links.first().copied()
}

fn denied(refusal: DeclarerRefusal) -> String {
    format!("ERR denied wake: {}\n", refusal.words())
}

/// A machine snapshot as the book reads it.
#[derive(Debug, Clone)]
pub(crate) struct SysRead {
    /// The snapshot.
    pub(crate) snap: Arc<Snapshot>,
    /// It speaks for now (within its asked freshness).
    pub(crate) fresh: bool,
}

/// A loop's last word for a session: its open hold's start, unix ms, and the guard holding its line.
pub(crate) type Said = (Option<i64>, Option<Guard>);

/// What the book reads of the world ([`Live`], or a test's rig).
pub(crate) trait Env: Send + Sync {
    /// Now, unix ms.
    fn now_ms(&self) -> i64;
    /// The local zone's offset from UTC now, seconds.
    fn local_offset_s(&self) -> i64;
    /// The process records.
    fn tree(&self) -> &dyn ProcTree;
    /// The owner's mode in force.
    fn mode(&self) -> Occupancy;
    /// A machine snapshot: at most `fresh` old (pulled where older), or the one held (`None`).
    fn sys(&self, fresh: Option<Duration>) -> Option<SysRead>;
    /// A build slot reserved for `sid`'s answer ([`crate::occupancy_host::Pace::reserve`]).
    fn reserve(&self, sid: &str, age_ms: i64, usable: u16, now_ms: i64) -> Reserve;
    /// `sid`'s loop's last word: its open hold's start, unix ms, and the guard holding its line;
    /// `None` where no loop told this window a word.
    fn said(&self, sid: &str) -> Option<Said>;
    /// Whether a loop of this build reads `sid`'s book, so that only it tells an answer heard.
    fn hosted(&self, sid: &str) -> bool;
    /// What a file atom's admission and reads touch.
    fn fs(&self) -> &dyn WakeFs;
}

/// A session's wake K Entry: the oldest ready wake past K looks untold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WakeIdle {
    /// The wake.
    pub(crate) ord: u16,
    /// The guard the session's loop last said held its line; `None` where none did.
    pub(crate) guard: Option<Guard>,
    /// A loop told this window a word for the session (else the Entry says `no-loop`).
    pub(crate) looped: bool,
    /// When it was first seen due, unix ms.
    pub(crate) since_ms: i64,
    /// The look that raised it or last changed it, unix ms: a look that only keeps it standing
    /// does not move this.
    pub(crate) at_ms: i64,
}

/// What the book owes the rest of the window (the product's [`deliver`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Out {
    /// Wake `ord` of `sid` came ready: its `wake #<n> ready` event, under `prompt` alone.
    Ready { sid: String, local: u64, ord: u16 },
    /// `sid`'s wake K Entry is raised (`Some`) or ended (`None`), under `prompt` alone.
    Idle { sid: String, idle: Option<WakeIdle> },
}

/// ONE SESSION'S SHELF: its book, its park, what its levels last read and its K clock.
#[derive(Debug, Default)]
struct Shelf {
    local: u64,
    book: WakeBook,
    parks: ParkBook,
    /// The answers owed to parked waits whose last open wake ended, by connection.
    ended: BTreeMap<u64, ParkAnswer>,
    /// Each `pid:` atom's process as admitted: its birth, so a reused pid reads exited.
    pids: BTreeMap<u32, i64>,
    /// Each machine predicate's `for=` latch, by its spelling (`when:` or `need:` and canonical).
    latches: BTreeMap<String, Latch>,
    /// Each predicate's verdict at the last read, by the same key.
    sys_held: BTreeMap<String, bool>,
    /// The wake K clock: ready, need-met, untold wakes by ordinal.
    due: DueClock<u16>,
    /// The K Entry as last owed.
    idle: Option<WakeIdle>,
    /// Each file atom's level as the sources last read it.
    files: FileLevels,
}

impl Shelf {
    fn named_slots(&self) -> bool {
        self.book.wakes.iter().filter(|w| w.state.live()).any(|w| {
            w.need.as_ref().is_some_and(SysWait::names_slots)
                || w.when
                    .iter()
                    .any(|a| matches!(&a.atom, Atom::Sys(sys) if sys.names_slots()))
        })
    }

    fn reads_sys(&self) -> bool {
        self.book
            .wakes
            .iter()
            .filter(|w| w.state.live())
            .any(|w| w.need.is_some() || w.when.iter().any(|a| matches!(a.atom, Atom::Sys(_))))
    }

    /// Step every live predicate's latch on `sys` (`None`: nothing fresh, so none holds).
    fn read_sys(&mut self, sys: Option<&SysRead>) {
        let mut keys: Vec<(String, SysWait)> = Vec::new();
        for wake in self.book.wakes.iter().filter(|w| w.state.live()) {
            for atom in &wake.when {
                if let Atom::Sys(sys) = &atom.atom {
                    keys.push((when_key(sys), sys.clone()));
                }
            }
            if let Some(need) = &wake.need {
                keys.push((need_key(need), need.clone()));
            }
        }
        self.latches
            .retain(|k, _| keys.iter().any(|(named, _)| named == k));
        self.sys_held.clear();
        for (key, wait) in keys {
            let latch = self.latches.entry(key.clone()).or_default();
            let holds = match sys.filter(|s| s.fresh) {
                Some(read) => {
                    let outcome = evaluate(wait.wait(), &read.snap, &[self.local]);
                    let holds = outcome.refused.is_none() && outcome.holds;
                    latch
                        .step(holds, read.snap.at_ns, wait.wait().for_ms)
                        .is_some()
                }
                None => {
                    *latch = Latch::default();
                    false
                }
            };
            self.sys_held.insert(key, holds);
        }
    }

    fn need_met(&self, wake: &DeclaredWake) -> bool {
        wake.need
            .as_ref()
            .is_none_or(|need| self.sys_held.get(&need_key(need)) == Some(&true))
    }

    /// Answer every parked wait that `why` ended for wake `ord`, where no wake is left open.
    fn end_parks(&mut self, ord: u16, why: Ended) {
        for (park, answer) in self.parks.ended(&self.book, ord, why) {
            self.ended.insert(park.conn, answer);
        }
    }

    /// THE FILE ATOMS ITS OPEN WAKES READ: each unheld `when=` atom of a pending wake and each
    /// `unless=` atom of an open one, with the agent it is declared for where that is still the
    /// process it was.
    fn file_wants(&self, tree: &dyn ProcTree) -> Vec<Want> {
        let mut wants: Vec<Want> = Vec::new();
        for wake in &self.book.wakes {
            let pending = match wake.state {
                WakeState::Pending => true,
                WakeState::Ready => false,
                _ => continue,
            };
            let alive = tree
                .link(wake.agent.pid)
                .is_some_and(|link| link.birth_us == wake.agent.birth_us);
            let atoms = wake
                .when
                .iter()
                .filter(|a| pending && !a.held)
                .filter_map(|a| source_of(&a.atom))
                .chain(wake.unless.iter())
                .filter(|a| files::path_of(a).is_some());
            for atom in atoms {
                let at = match wants.iter().position(|w| w.atom == *atom) {
                    Some(at) => at,
                    None => {
                        wants.push(Want {
                            atom: atom.clone(),
                            pids: Vec::new(),
                        });
                        wants.len() - 1
                    }
                };
                if alive && !wants[at].pids.contains(&wake.agent.pid) {
                    wants[at].pids.push(wake.agent.pid);
                }
            }
        }
        wants
    }

    /// The pids no live wake names any more leave the table.
    fn prune_pids(&mut self) {
        let named = |pid: u32| {
            self.book
                .wakes
                .iter()
                .filter(|w| w.state.live())
                .any(|w| names_pid(w, pid))
        };
        let keep: Vec<u32> = self.pids.keys().copied().filter(|p| named(*p)).collect();
        self.pids.retain(|pid, _| keep.contains(pid));
    }

    /// Under the owner's `off`: every live wake ends `Expired why=off`, and each parked wait hears
    /// it.
    fn disarm_off(&mut self) {
        for ord in self.book.disarm(ExpireWhy::Off) {
            self.end_parks(ord, Ended::Expired(ExpireWhy::Off));
        }
        self.prune_pids();
    }
}

fn names_pid(wake: &DeclaredWake, pid: u32) -> bool {
    wake.when
        .iter()
        .any(|a| matches!(a.atom, Atom::Source(SourceAtom::Pid { pid: p }) if p == pid))
        || wake
            .unless
            .iter()
            .any(|a| matches!(a, SourceAtom::Pid { pid: p } if *p == pid))
}

fn when_key(sys: &SysWait) -> String {
    format!("when:{}", sys.canonical())
}

fn need_key(sys: &SysWait) -> String {
    format!("need:{}", sys.canonical())
}

/// THE LEVELS ONE SHELF'S LOOK READS: pids by birth, predicates as their latches last held, agents
/// by birth, file atoms as the sources last read them.
struct ShelfLevels<'a> {
    tree: &'a dyn ProcTree,
    pids: &'a BTreeMap<u32, i64>,
    sys_held: &'a BTreeMap<String, bool>,
    files: &'a FileLevels,
}

impl ShelfLevels<'_> {
    fn file(&self, atom: &SourceAtom) -> Option<Level> {
        self.files
            .iter()
            .find(|(read, _)| read == atom)
            .map(|(_, level)| *level)
    }
}

impl Levels for ShelfLevels<'_> {
    fn source(&self, atom: &SourceAtom) -> Option<bool> {
        match atom {
            SourceAtom::Pid { pid } => {
                let birth = self.pids.get(pid)?;
                match self.tree.read(*pid) {
                    ProcRead::Alive(link) => Some(link.birth_us != *birth),
                    ProcRead::Exited => Some(true),
                    ProcRead::Unread => None,
                }
            }
            SourceAtom::File { .. } | SourceAtom::Gone { .. } => match self.file(atom)? {
                Level::Holds(holds) => Some(holds),
                Level::Lost(_) => None,
            },
        }
    }

    fn lost(&self, atom: &SourceAtom) -> Option<ExpireWhy> {
        match self.file(atom)? {
            Level::Lost(why) => Some(why),
            Level::Holds(_) => None,
        }
    }

    fn sys(&self, wait: &SysWait) -> Option<bool> {
        self.sys_held.get(&when_key(wait)).copied()
    }

    fn agent_alive(&self, agent: AgentId) -> bool {
        match self.tree.read(agent.pid) {
            ProcRead::Alive(link) => link.birth_us == agent.birth_us,
            ProcRead::Exited => false,
            // A failed read does not prove death: retain the declaration for a later look.
            ProcRead::Unread => true,
        }
    }
}

/// Every shelf, and what the book owes the window.
#[derive(Debug, Default)]
struct Books {
    shelves: BTreeMap<String, Shelf>,
    outs: Vec<Out>,
}

impl Books {
    /// THE OWNER'S `off` AS THE LOCK SEES IT: where it is in force, every shelf is disarmed
    /// ([`Shelf::disarm_off`]) and the caller books, answers and reads nothing. Asked under the
    /// lock, so a verb that read the mode before a switch to `off` cannot book or answer past the
    /// switch's disarm ([`WakeHost::note_mode`] takes the same lock after the mode is set).
    fn off(&mut self, env: &dyn Env) -> bool {
        if env.mode() != Occupancy::Off {
            return false;
        }
        for shelf in self.shelves.values_mut() {
            shelf.disarm_off();
        }
        true
    }

    fn shelf(&mut self, seat: &Seat) -> &mut Shelf {
        let shelf = self.shelves.entry(seat.sid.clone()).or_default();
        shelf.local = seat.local;
        shelf
    }

    /// The look at `sid`'s shelf at `now_ms`: every level read again; each change answered.
    fn look(&mut self, env: &dyn Env, sid: &str, sys: Option<&SysRead>, now_ms: i64) {
        let prompt = env.mode() == Occupancy::Prompt;
        let Some(shelf) = self.shelves.get_mut(sid) else {
            return;
        };
        if shelf.reads_sys() {
            shelf.read_sys(sys);
        }
        let changes = shelf.book.on_look(
            &ShelfLevels {
                tree: env.tree(),
                pids: &shelf.pids,
                sys_held: &shelf.sys_held,
                files: &shelf.files,
            },
            now_ms,
        );
        for (ord, change) in changes {
            match change {
                Change::Ready if prompt => self.outs.push(Out::Ready {
                    sid: sid.to_string(),
                    local: shelf.local,
                    ord,
                }),
                Change::Ready | Change::Told(_) => {}
                Change::Expired(why) => shelf.end_parks(ord, Ended::Expired(why)),
            }
        }
        shelf.prune_pids();
    }

    /// THE WAKE K at `now_ms`: each ready, need-met, untold wake's clock, sampled at K looks.
    fn sample(&mut self, env: &dyn Env, sid: &str, now_ms: i64) {
        let prompt = env.mode() == Occupancy::Prompt;
        let said = env.said(sid);
        let hosted = env.hosted(sid);
        let Some(shelf) = self.shelves.get_mut(sid) else {
            return;
        };
        let guard = said.and_then(|(_, guard)| guard);
        let due: Vec<(u16, Option<Guard>)> = shelf
            .book
            .wakes
            .iter()
            .filter(|w| {
                matches!(w.state, WakeState::Ready)
                    || (hosted && matches!(w.state, WakeState::Answered { .. }))
            })
            .filter(|w| shelf.need_met(w))
            .map(|w| (w.ord, guard))
            .collect();
        if !fault::armed(fault::Fault::KFromBreakOnly) {
            shelf.due.observe(&due, now_ms);
        }
        let idle = shelf
            .due
            .sample(K_LOOKS, now_ms)
            .into_iter()
            .min_by_key(|raised| (raised.since_ms, raised.key))
            .map(|raised| WakeIdle {
                ord: raised.key,
                guard: raised.guard,
                looped: said.is_some(),
                since_ms: raised.since_ms,
                at_ms: now_ms,
            })
            .filter(|_| prompt);
        let moved = |a: Option<WakeIdle>, b: Option<WakeIdle>| {
            a.map(|i| (i.ord, i.guard, i.looped, i.since_ms))
                != b.map(|i| (i.ord, i.guard, i.looped, i.since_ms))
        };
        if moved(shelf.idle, idle) {
            shelf.idle = idle;
            self.outs.push(Out::Idle {
                sid: sid.to_string(),
                idle,
            });
        }
    }
}

/// How one pass of a parked `await wake` ends ([`WakeHost::pass`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Its reply: an answer to wake `ord` (to be told once written, [`WakeHost::answered`]), or
    /// an end or a refusal (`ord` `None`).
    Reply { reply: String, ord: Option<u16> },
    /// It keeps waiting.
    Wait,
}

/// Whether a read of file atoms, or a close of their watches, waits for the files' lock
/// ([`WakeHost::read_and_look`], [`WakeHost::close_files`]): a verb's connection thread waits;
/// the window's thread never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Take {
    /// Wait for it.
    Wait,
    /// Take it where it is free, else leave the work to the next tick.
    IfFree,
}

/// THE PROCESS'S BOOKS behind their one lock, the bell a parked wait listens on, and the file
/// atoms' watches and readers behind a lock of their own, which a file read holds alone: the two
/// are never held together.
#[derive(Debug, Default)]
pub(crate) struct WakeHost {
    books: Mutex<Books>,
    changed: Condvar,
    files: Mutex<Files>,
}

impl WakeHost {
    fn lock(&self) -> MutexGuard<'_, Books> {
        self.books.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read `sid`'s file atoms for `wants` (each admitted at its declaration where `fresh` holds
    /// it), at most `per_atom` bytes of each pattern's file; nothing under the owner's `off`, asked
    /// again under the files' lock, which closes every watch. Holds the files' lock alone; `None`
    /// where `take` is [`Take::IfFree`] and another read holds it.
    fn read_files(
        &self,
        env: &dyn Env,
        sid: &str,
        wants: &[Want],
        fresh: &[(SourceAtom, AdmittedPath)],
        per_atom: u64,
        take: Take,
    ) -> Option<FileLevels> {
        let fs = env.fs();
        let mut files = if take == Take::Wait {
            self.files.lock_or_recover()
        } else {
            match self.files.try_lock() {
                Ok(files) => files,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return None,
            }
        };
        if env.mode() == Occupancy::Off {
            files.clear();
            return Some(Vec::new());
        }
        let ctx = files::Ctx::of(fs);
        files.sync(fs, ctx.as_ref(), sid, wants, fresh);
        let mut left = files::PER_PASS;
        Some(files.read(fs, ctx.as_ref(), sid, per_atom, &mut left))
    }

    /// `sid`'s look at `now_ms`, its file atoms read first, neither lock held across the other:
    /// `false` where the owner's `off` came into force meanwhile (every book disarmed, every watch
    /// closed). A shelf closed meanwhile lets its watches go here ([`Self::forget`]). With
    /// [`Take::IfFree`], file atoms another read holds the lock over are left to the next tick.
    fn read_and_look(
        &self,
        env: &dyn Env,
        sid: &str,
        sys: Option<&SysRead>,
        now_ms: i64,
        fresh: &[(SourceAtom, AdmittedPath)],
        take: Take,
    ) -> bool {
        let wants = self
            .lock()
            .shelves
            .get(sid)
            .map(|shelf| shelf.file_wants(env.tree()))
            .unwrap_or_default();
        let levels = (!wants.is_empty() || !fresh.is_empty())
            .then(|| self.read_files(env, sid, &wants, fresh, files::PER_EVENT, take))
            .flatten();
        let mut books = self.lock();
        if books.off(env) {
            drop(books);
            self.close_files(take, Files::clear);
            return false;
        }
        let Some(shelf) = books.shelves.get_mut(sid) else {
            drop(books);
            if levels.is_some() {
                self.close_files(take, |files| files.forget(sid));
            }
            return true;
        };
        if let Some(levels) = levels {
            shelf.files = levels;
        }
        books.look(env, sid, sys, now_ms);
        true
    }

    /// `close` on the files, at once where `take` waits for their lock or it is free; else the
    /// next tick's pass does it (it lets go every shelf no longer booked, and every watch under
    /// `off`).
    fn close_files(&self, take: Take, close: impl FnOnce(&mut Files)) {
        if take == Take::Wait {
            close(&mut self.files.lock_or_recover());
            return;
        }
        match self.files.try_lock() {
            Ok(mut files) => close(&mut files),
            Err(TryLockError::Poisoned(poisoned)) => close(&mut poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {}
        }
    }

    /// What the book owes the window since the last take.
    pub(crate) fn take_outs(&self) -> Vec<Out> {
        std::mem::take(&mut self.lock().outs)
    }

    /// `wake <rest>` from `peer` on `seat`.
    pub(crate) fn wake(&self, env: &dyn Env, seat: &Seat, peer: Option<u32>, rest: &str) -> String {
        if env.mode() == Occupancy::Off {
            return OFF.to_string();
        }
        let agent = match proved(env.tree(), seat, peer) {
            Ok(agent) => agent,
            Err(refusal) => return denied(refusal),
        };
        let words: Vec<&str> = rest.split_whitespace().collect();
        match words.as_slice() {
            ["rm", ord] => self.rm_or_take(env, seat, ord, false),
            ["taken", ord] => self.rm_or_take(env, seat, ord, true),
            ["rm" | "taken", ..] => RM_USAGE.to_string(),
            _ => self.declare(env, seat, agent, rest),
        }
    }

    fn declare(&self, env: &dyn Env, seat: &Seat, agent: AgentId, rest: &str) -> String {
        let req = match WakeReq::parse(rest) {
            Ok(req) => req,
            Err(usage) => return usage.reply(),
        };
        let sources = req
            .when
            .iter()
            .filter_map(source_of)
            .chain(req.unless.iter());
        let mut pids: Vec<ProcLink> = Vec::new();
        let mut file_atoms: Vec<SourceAtom> = Vec::new();
        for source in sources {
            match source {
                SourceAtom::File { .. } | SourceAtom::Gone { .. } => {
                    if !env.fs().served() {
                        return files::NOT_SERVED.to_string();
                    }
                    file_atoms.push(source.clone());
                }
                SourceAtom::Pid { pid } => match of_tree(env.tree(), seat, *pid) {
                    Some(link) => pids.push(link),
                    None => return denied(DeclarerRefusal::NotInTree),
                },
            }
        }
        // Each path admitted before anything at it is read, outside the books' lock; its roots
        // are the kernel's working directories of the tab's PTY child (where its pid is still
        // the process the session took) and the proved agent.
        let mut fresh: Vec<(SourceAtom, AdmittedPath)> = Vec::new();
        if !file_atoms.is_empty() {
            let fs = env.fs();
            let ctx = files::Ctx::of(fs);
            let rooted: Vec<u32> = live_root(env.tree(), seat)
                .map(|root| root.pid)
                .into_iter()
                .chain([agent.pid])
                .collect();
            for atom in file_atoms {
                let Some(path) = files::path_of(&atom) else {
                    continue;
                };
                match files::admit(fs, ctx.as_ref(), &rooted, path) {
                    Ok(admitted) => fresh.push((atom, admitted)),
                    Err(refused) => return refused.reply(),
                }
            }
        }
        let await_only = req.await_only();
        let now_ms = env.now_ms();
        let sys = req_reads_sys(&req).then(|| env.sys(None)).flatten();
        let mut books = self.lock();
        if books.off(env) {
            return OFF.to_string();
        }
        let shelf = books.shelf(seat);
        if pids.iter().any(|link| {
            shelf
                .pids
                .get(&link.pid)
                .is_some_and(|b| *b != link.birth_us)
        }) {
            // A pid reused since an older wake named it: that wake reads its exit first.
            books.look(env, &seat.sid, sys.as_ref(), now_ms);
        }
        let shelf = books.shelf(seat);
        let declared =
            shelf
                .book
                .declare(req, Declarer::Agent, agent, now_ms, env.local_offset_s());
        let ord = match declared {
            Ok(ord) => ord,
            Err(refusal) => return format!("ERR denied wake: {}\n", refusal.words()),
        };
        for link in pids {
            shelf.pids.insert(link.pid, link.birth_us);
        }
        if fresh.is_empty() {
            books.look(env, &seat.sid, sys.as_ref(), now_ms);
            drop(books);
        } else {
            drop(books);
            if !self.read_and_look(env, &seat.sid, sys.as_ref(), now_ms, &fresh, Take::Wait) {
                self.changed.notify_all();
                return OFF.to_string();
            }
        }
        self.changed.notify_all();
        let only = if await_only { " await_only=1" } else { "" };
        format!("OK wake #{ord}{only}\n")
    }

    fn rm_or_take(&self, env: &dyn Env, seat: &Seat, word: &str, take: bool) -> String {
        let Some(ord) = ordinal(word) else {
            return RM_USAGE.to_string();
        };
        let mut books = self.lock();
        if books.off(env) {
            return OFF.to_string();
        }
        let shelf = books.shelf(seat);
        let done = if take {
            shelf.book.taken(ord)
        } else {
            shelf.book.rm(ord)
        };
        if !done {
            return format!("ERR no such wake #{ord}\n");
        }
        shelf.end_parks(ord, if take { Ended::Taken } else { Ended::Removed });
        shelf.prune_pids();
        let wants = shelf.file_wants(env.tree());
        drop(books);
        self.files.lock_or_recover().keep(&seat.sid, &wants);
        self.changed.notify_all();
        format!("OK wake #{ord} {}\n", if take { "taken" } else { "rm" })
    }

    /// `waits <rest>` from `peer` on `seat`: the book, redacted for a peer outside its tree (S12).
    pub(crate) fn waits(
        &self,
        env: &dyn Env,
        seat: &Seat,
        peer: Option<u32>,
        rest: &str,
    ) -> String {
        if env.mode() == Occupancy::Off {
            return OFF.to_string();
        }
        let words: Vec<&str> = rest.split_whitespace().collect();
        let (why, only) = match words.as_slice() {
            [] => (false, None),
            ["why"] => (true, None),
            ["why", ord] => match ordinal(ord) {
                Some(ord) => (true, Some(ord)),
                None => return WAITS_USAGE.to_string(),
            },
            _ => return WAITS_USAGE.to_string(),
        };
        let inside = proved(env.tree(), seat, peer).is_ok();
        // How a wake is read tells of the process's watch budget: the session's own tree alone.
        let sources = if why && inside {
            self.files.lock_or_recover().sources(&seat.sid)
        } else {
            Vec::new()
        };
        let mut books = self.lock();
        if books.off(env) {
            return OFF.to_string();
        }
        let wakes: Vec<&DeclaredWake> = books
            .shelves
            .get(&seat.sid)
            .map(|shelf| {
                shelf
                    .book
                    .wakes
                    .iter()
                    .filter(|w| only.is_none_or(|ord| w.ord == ord))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(ord) = only
            && wakes.is_empty()
        {
            return format!("ERR no such wake #{ord}\n");
        }
        let mut out = format!("OK {}\n", wakes.len());
        for wake in wakes {
            out.push_str(&if why {
                why_line(wake, inside, source_word(wake, &sources))
            } else {
                wake_line(wake, inside)
            });
            out.push('\n');
        }
        out
    }

    /// `await wake` parks `conn` from `peer` on `seat`.
    /// # Errors
    /// The reply refusing it: `off`, a peer outside the tree, a park standing, nothing open.
    pub(crate) fn park(
        &self,
        env: &dyn Env,
        seat: &Seat,
        peer: Option<u32>,
        conn: u64,
    ) -> Result<(), String> {
        if env.mode() == Occupancy::Off {
            return Err(OFF.to_string());
        }
        proved(env.tree(), seat, peer).map_err(denied)?;
        let now_ms = env.now_ms();
        let mut books = self.lock();
        if books.off(env) {
            return Err(OFF.to_string());
        }
        let shelf = books.shelf(seat);
        shelf
            .parks
            .park(conn, &shelf.book, now_ms)
            .map_err(|refusal| refusal.reply().to_string())
    }

    /// Whether `sid`'s live wakes name build slots (a parked wait arms their sampling).
    pub(crate) fn names_slots(&self, sid: &str) -> bool {
        self.lock().shelves.get(sid).is_some_and(Shelf::named_slots)
    }

    /// ONE PASS of `conn`'s parked `await wake` on `sid`: an end owed to it, else the levels read
    /// again and the first ready wake whose need holds (a slot reserved where it names slots)
    /// begun as its answer.
    pub(crate) fn pass(&self, env: &dyn Env, sid: &str, conn: u64) -> Pass {
        let reads = self.lock().shelves.get(sid).map(Shelf::reads_sys);
        let Some(reads) = reads else {
            return Pass::Reply {
                reply: "ERR exited\n".to_string(),
                ord: None,
            };
        };
        let sys = if reads && env.mode() != Occupancy::Off {
            env.sys(Some(crate::sys_host::AWAIT_PERIOD))
        } else {
            None
        };
        let now_ms = env.now_ms();
        let mut books = self.lock();
        // Under `off` the shelf is disarmed here, so the wait hears its end below.
        books.off(env);
        let Some(shelf) = books.shelves.get_mut(sid) else {
            return Pass::Reply {
                reply: "ERR exited\n".to_string(),
                ord: None,
            };
        };
        if let Some(answer) = shelf.ended.remove(&conn) {
            shelf.parks.unpark(conn);
            return Pass::Reply {
                reply: answer.reply(),
                ord: None,
            };
        }
        if !shelf.parks.parked().iter().any(|p| p.conn == conn) {
            return Pass::Reply {
                reply: "ERR usage await wake: no wake pending\n".to_string(),
                ord: None,
            };
        }
        books.look(env, sid, sys.as_ref(), now_ms);
        let hold_since = env.said(sid).and_then(|(since, _)| since);
        let mode = env.mode();
        let Some(shelf) = books.shelves.get_mut(sid) else {
            return Pass::Wait;
        };
        if let Some(answer) = shelf.ended.remove(&conn) {
            shelf.parks.unpark(conn);
            return Pass::Reply {
                reply: answer.reply(),
                ord: None,
            };
        }
        for ord in shelf.book.for_await() {
            let Some(wake) = shelf.book.get(ord) else {
                continue;
            };
            let since_ms = wake.ready_since_ms.unwrap_or(now_ms);
            let age_ms = hold_since.unwrap_or(since_ms);
            let need_slots = wake.need.as_ref().is_some_and(SysWait::names_slots);
            let seen = sys.as_ref().and_then(|s| slots_seen(&s.snap));
            if !shelf.need_met(wake) {
                if need_slots && seen.is_some_and(|(usable, _)| usable == 0) {
                    // No slot is usable: it waits in the slots' line by its age, so the first
                    // slot to free is the oldest wait's, whichever pass asks first.
                    let _ = slot_granted(env, mode, sid, age_ms, 0, now_ms);
                }
                continue;
            }
            let slots = if need_slots {
                let Some(seen) = seen else {
                    continue;
                };
                if !slot_granted(env, mode, sid, age_ms, seen.0, now_ms) {
                    continue;
                }
                Some(seen)
            } else {
                None
            };
            if shelf.book.answer_begin(ord, now_ms) {
                shelf.parks.unpark(conn);
                return Pass::Reply {
                    reply: ParkAnswer::Ready {
                        ord,
                        since_ms,
                        slots,
                    }
                    .reply(),
                    ord: Some(ord),
                };
            }
        }
        Pass::Wait
    }

    /// `conn`'s park on `sid` ends unanswered (its peer hung up, its time ran out).
    pub(crate) fn unpark(&self, sid: &str, conn: u64) {
        if let Some(shelf) = self.lock().shelves.get_mut(sid) {
            shelf.parks.unpark(conn);
            shelf.ended.remove(&conn);
        }
    }

    /// THE ANSWER TO `ord` OF `sid` WAS WRITTEN (`ok`) or not: told (answered, in a hosted
    /// session), or ready again for the next wait.
    pub(crate) fn answered(&self, env: &dyn Env, sid: &str, ord: u16, ok: bool) {
        let hosted = env.hosted(sid);
        let now_ms = env.now_ms();
        if let Some(shelf) = self.lock().shelves.get_mut(sid) {
            shelf.book.answer_done(ord, ok, hosted, now_ms);
        }
        self.changed.notify_all();
    }

    /// Wait for a change of any book, until `until` at most.
    pub(crate) fn wait_change(&self, until: Instant) {
        let books = self.lock();
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        drop(
            self.changed
                .wait_timeout(books, remaining)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// THE TICK at `now_ms`: every shelf with a wake open has its levels read again (its file
    /// atoms first, with the books' lock let go, at most [`files::PER_LOOK`] of each pattern's
    /// file); on a look (`sample`), its wake K clock is sampled too.
    pub(crate) fn tick(&self, env: &dyn Env, sample: bool) {
        let now_ms = env.now_ms();
        let (reads, wants) = if env.mode() == Occupancy::Off {
            (false, BTreeMap::new())
        } else {
            let books = self.lock();
            let wants: BTreeMap<String, Vec<Want>> = books
                .shelves
                .iter()
                .map(|(sid, shelf)| (sid.clone(), shelf.file_wants(env.tree())))
                .collect();
            (books.shelves.values().any(Shelf::reads_sys), wants)
        };
        let sys = if reads { env.sys(None) } else { None };
        let mut levels = self.read_every_file(env, &wants);
        let mut books = self.lock();
        if books.off(env) {
            drop(books);
            self.files.lock_or_recover().clear();
            self.changed.notify_all();
            return;
        }
        let sids: Vec<String> = books.shelves.keys().cloned().collect();
        for sid in &sids {
            if let Some(shelf) = books.shelves.get_mut(sid) {
                shelf.files = levels.remove(sid).unwrap_or_default();
            }
            books.look(env, sid, sys.as_ref(), now_ms);
            if sample {
                books.sample(env, sid, now_ms);
            }
        }
        drop(books);
        // A shelf read and closed meanwhile lets its watches go ([`Self::forget`]).
        if !levels.is_empty() {
            let mut files = self.files.lock_or_recover();
            for sid in levels.keys() {
                files.forget(sid);
            }
        }
        self.changed.notify_all();
    }

    /// EVERY SHELF'S FILE ATOMS READ for the look, round-robin from the shelf the last pass
    /// reached, within [`files::PER_PASS`]; a shelf no longer booked lets its watches go. Under
    /// the owner's `off`, asked again under the files' lock before each shelf, nothing is read
    /// (the tick then closes every watch).
    fn read_every_file(
        &self,
        env: &dyn Env,
        wants: &BTreeMap<String, Vec<Want>>,
    ) -> BTreeMap<String, FileLevels> {
        let fs = env.fs();
        let reading = || env.mode() != Occupancy::Off;
        let mut files = self.files.lock_or_recover();
        for sid in files.sids() {
            if !wants.contains_key(&sid) {
                files.forget(&sid);
            }
        }
        if wants.values().all(Vec::is_empty) {
            for sid in wants.keys() {
                files.keep(sid, &[]);
            }
            return BTreeMap::new();
        }
        let ctx = files::Ctx::of(fs);
        files.pass(
            fs,
            ctx.as_ref(),
            wants,
            files::PER_LOOK,
            files::PER_PASS,
            &reading,
        )
    }

    /// THE EDGE PASS on `fired` watches: their shelves' file atoms read again at once (at most
    /// [`files::PER_EVENT`] of each pattern's file), and each `when=` file atom of a pending wake
    /// found holding latched ([`WakeBook::on_source`]), so a level that holds between two looks
    /// is not missed. A wake whose `unless=` file atom the same read finds holding is latched
    /// nothing, and owes no `ready` (the next look ends it); lost paths are judged at the next
    /// look. Under the owner's `off`, asked again under the files' lock before each shelf, nothing
    /// is read (and every watch closes).
    pub(crate) fn on_files(&self, env: &dyn Env, fired: &[files::Fired]) {
        if fired.is_empty() || env.mode() == Occupancy::Off {
            return;
        }
        let fs = env.fs();
        let read: Vec<(String, FileLevels)> = {
            let mut files = self.files.lock_or_recover();
            let sids = files.fired(fired);
            if sids.is_empty() {
                return;
            }
            let ctx = files::Ctx::of(fs);
            let mut left = files::PER_PASS;
            let mut read = Vec::with_capacity(sids.len());
            for sid in sids {
                if env.mode() == Occupancy::Off {
                    break;
                }
                let levels = files.read(fs, ctx.as_ref(), &sid, files::PER_EVENT, &mut left);
                read.push((sid, levels));
            }
            read
        };
        let now_ms = env.now_ms();
        let prompt = env.mode() == Occupancy::Prompt;
        let mut books = self.lock();
        if books.off(env) {
            drop(books);
            self.files.lock_or_recover().clear();
            self.changed.notify_all();
            return;
        }
        let Books { shelves, outs } = &mut *books;
        let mut gone: Vec<String> = Vec::new();
        for (sid, levels) in read {
            let Some(shelf) = shelves.get_mut(&sid) else {
                gone.push(sid);
                continue;
            };
            let holds = |atom: &SourceAtom| {
                levels
                    .iter()
                    .any(|(read, level)| read == atom && *level == Level::Holds(true))
            };
            let edges: Vec<(u16, usize)> = shelf
                .book
                .wakes
                .iter()
                .filter(|w| w.state == WakeState::Pending && !w.unless.iter().any(holds))
                .flat_map(|w| {
                    w.when
                        .iter()
                        .enumerate()
                        .filter(|(_, a)| !a.held && source_of(&a.atom).is_some_and(holds))
                        .map(move |(at, _)| (w.ord, at))
                })
                .collect();
            for (ord, at) in edges {
                if shelf.book.on_source(ord, at, now_ms) == Some(Change::Ready) && prompt {
                    outs.push(Out::Ready {
                        sid: sid.clone(),
                        local: shelf.local,
                        ord,
                    });
                }
            }
            shelf.files = levels;
        }
        drop(books);
        // A shelf read and closed meanwhile lets its watches go ([`Self::forget`]).
        if !gone.is_empty() {
            let mut files = self.files.lock_or_recover();
            for sid in &gone {
                files.forget(sid);
            }
        }
        self.changed.notify_all();
    }

    /// The file watches that fired within `timeout` (the sources thread's wait between ticks).
    pub(crate) fn wait_files(&self, env: &dyn Env, timeout: Duration) -> Vec<files::Fired> {
        env.fs().wait(timeout)
    }

    /// The owner's mode moved to `mode`: under `off` every book ends (`Expired why=off`), and
    /// out of `prompt` every K Entry is owed its end.
    pub(crate) fn note_mode(&self, mode: Occupancy) {
        let mut books = self.lock();
        let Books { shelves, outs } = &mut *books;
        for (sid, shelf) in shelves.iter_mut() {
            if mode == Occupancy::Off {
                shelf.disarm_off();
            }
            if mode != Occupancy::Prompt && shelf.idle.take().is_some() {
                outs.push(Out::Idle {
                    sid: sid.clone(),
                    idle: None,
                });
            }
        }
        drop(books);
        if mode == Occupancy::Off {
            // As in [`Self::forget`]: a read under way closes them once it reads `off`.
            self.close_files(Take::IfFree, Files::clear);
        }
        self.changed.notify_all();
    }

    /// Session `sid` closed: its shelf goes, its parks with it, and its file watches. The window's
    /// thread calls this (and [`Self::note_mode`]), so it never waits behind a file read: where
    /// one holds the files' lock, that read lets the watches go once it finds the shelf gone, and
    /// the next tick's pass at the latest.
    pub(crate) fn forget(&self, sid: &str) {
        self.lock().shelves.remove(sid);
        self.close_files(Take::IfFree, |files| files.forget(sid));
        self.changed.notify_all();
    }

    /// `waits= waits_ready=` of `sid`.
    pub(crate) fn counts(&self, sid: &str) -> (usize, usize) {
        self.lock().shelves.get(sid).map_or((0, 0), |shelf| {
            let seen = shelf.book.seen();
            (seen.pending(), seen.ready())
        })
    }

    /// `sid`'s K Entry as last owed.
    pub(crate) fn idle_of(&self, sid: &str) -> Option<WakeIdle> {
        self.lock().shelves.get(sid).and_then(|shelf| shelf.idle)
    }

    /// `sid`'s book as a session record carries it ([`Carry`]); `None` for an empty one.
    /// Built where `SessionHandoff::from_store` is (Unix, and tests).
    #[cfg(any(unix, test))]
    pub(crate) fn carry_row(&self, sid: &str) -> Option<String> {
        let books = self.lock();
        let shelf = books.shelves.get(sid)?;
        if shelf.book.wakes.is_empty() {
            return None;
        }
        let carry = Carry {
            next_ord: shelf.book.next_ord,
            wakes: shelf
                .book
                .wakes
                .iter()
                .filter_map(|w| aterm_json::to_value(w).ok())
                .collect(),
            pids: shelf
                .pids
                .iter()
                .map(|(pid, birth)| (*pid, *birth))
                .collect(),
            due: shelf
                .book
                .wakes
                .iter()
                .filter_map(|w| shelf.due.get(&w.ord).map(|d| (w.ord, d.since_ms)))
                .collect(),
        };
        aterm_json::to_string(&carry).ok()
    }

    /// THE RESTORE of a carried `row` onto `seat` ([`WakeBook::restored`] first): every wake the
    /// current rules refuse ends `Expired why=admit` (a file atom's path met at a link, `why=path`),
    /// and the levels are read at once (the file atoms where no other read holds the files' lock,
    /// else at the next tick); under the owner's `off` the book arrives disarmed (`Expired
    /// why=off`) and nothing is read, no path admitted.
    pub(crate) fn restore(&self, env: &dyn Env, seat: &Seat, row: &str) -> Restored {
        let Some(carry) = Carry::lenient(row) else {
            return Restored::Unread;
        };
        let mut book = WakeBook {
            wakes: carry.wakes,
            next_ord: carry.next_ord.max(1),
        }
        .restored();
        let mut refused = 0;
        let live_cap = aterm_agent::supervise::occupancy::wakes::MAX_WAKES;
        let mut counted = 0;
        let reading = env.mode() != Occupancy::Off;
        let ctx = reading.then(|| files::Ctx::of(env.fs())).flatten();
        let mut fresh: Vec<(SourceAtom, AdmittedPath)> = Vec::new();
        for wake in &mut book.wakes {
            if !wake.state.live() {
                continue;
            }
            counted += 1;
            let why = if !admitted(wake) || counted > live_cap {
                Some(ExpireWhy::Admit)
            } else if reading {
                admit_files(env, ctx.as_ref(), wake, &mut fresh)
            } else {
                None
            };
            if let Some(why) = why {
                wake.state = WakeState::Expired { why };
                refused += 1;
            }
        }
        let now_ms = env.now_ms();
        let sys = (env.mode() != Occupancy::Off)
            .then(|| env.sys(None))
            .flatten();
        let mut books = self.lock();
        let shelf = books.shelf(seat);
        shelf.book = book;
        if !fault::armed(fault::Fault::NoRearm) {
            shelf.pids = carry.pids.into_iter().collect();
        }
        if !books.off(env) {
            let shelf = books.shelf(seat);
            for (ord, since_ms) in carry.due {
                shelf.due.seed(ord, since_ms);
            }
            drop(books);
            // The window's thread restores an adopted tab's book: it never waits on a file read.
            self.read_and_look(env, &seat.sid, sys.as_ref(), now_ms, &fresh, Take::IfFree);
            books = self.lock();
        }
        let live = books
            .shelf(seat)
            .book
            .wakes
            .iter()
            .filter(|w| w.state.live())
            .count();
        drop(books);
        self.changed.notify_all();
        Restored::Read { refused, live }
    }
}

/// What a restore read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Restored {
    /// The row is no book this build reads: nothing restored.
    Unread,
    /// Restored, `refused` wakes ended `Expired why=admit` and `live` left open to an answer.
    Read { refused: usize, live: usize },
}

/// Whether `wake`'s `by=` is one the current rules admit: within
/// [`aterm_agent::supervise::occupancy::wakes::BY_MAX_MS`].
fn admitted(wake: &DeclaredWake) -> bool {
    use aterm_agent::supervise::occupancy::wakes::BY_MAX_MS;
    wake.due_by_ms.saturating_sub(wake.declared_ms) <= BY_MAX_MS
}

/// A RESTORED WAKE'S FILE ATOMS ADMITTED under the current rules, each kept in `fresh`; rooted
/// at its agent's working directory where the agent is still the process it was. `Some` is why
/// it ends: a path met at a link, any other refusal, or file atoms not served here.
fn admit_files(
    env: &dyn Env,
    ctx: Option<&files::Ctx>,
    wake: &DeclaredWake,
    fresh: &mut Vec<(SourceAtom, AdmittedPath)>,
) -> Option<ExpireWhy> {
    let atoms: Vec<&SourceAtom> = wake
        .when
        .iter()
        .filter_map(|a| source_of(&a.atom))
        .chain(wake.unless.iter())
        .filter(|a| files::path_of(a).is_some())
        .collect();
    if atoms.is_empty() {
        return None;
    }
    let fs = env.fs();
    if !fs.served() {
        return Some(ExpireWhy::Admit);
    }
    let alive = env
        .tree()
        .link(wake.agent.pid)
        .is_some_and(|link| link.birth_us == wake.agent.birth_us);
    let rooted: Vec<u32> = if alive {
        vec![wake.agent.pid]
    } else {
        Vec::new()
    };
    for atom in atoms {
        let path = files::path_of(atom)?;
        match files::admit(fs, ctx, &rooted, path) {
            Ok(admitted) => {
                if !fresh.iter().any(|(held, _)| held == atom) {
                    fresh.push((atom.clone(), admitted));
                }
            }
            Err(refused) => return Some(refused.why()),
        }
    }
    None
}

/// A BOOK AS A SESSION RECORD CARRIES IT (`SessionRecord::wakes`, one JSON row): its wakes each
/// read alone, so one this build cannot read is dropped and the rest stand.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Carry {
    next_ord: u16,
    wakes: Vec<aterm_json::Value>,
    #[serde(default)]
    pids: Vec<(u32, i64)>,
    #[serde(default)]
    due: Vec<(u16, i64)>,
}

/// [`Carry`] read back, its wakes parsed.
struct CarryRead {
    next_ord: u16,
    wakes: Vec<DeclaredWake>,
    pids: Vec<(u32, i64)>,
    due: Vec<(u16, i64)>,
}

impl Carry {
    fn lenient(row: &str) -> Option<CarryRead> {
        let carry: Carry = aterm_json::from_str(row).ok()?;
        let mut wakes: Vec<DeclaredWake> = carry
            .wakes
            .into_iter()
            .filter_map(|w| aterm_json::from_value(w).ok())
            .collect();
        wakes.sort_by_key(|w| w.ord);
        wakes.dedup_by_key(|w| w.ord);
        let next_ord = wakes
            .iter()
            .map(|w| w.ord.saturating_add(1))
            .max()
            .unwrap_or(1)
            .max(carry.next_ord);
        Some(CarryRead {
            next_ord,
            wakes,
            pids: carry.pids,
            due: carry.due,
        })
    }
}

fn req_reads_sys(req: &WakeReq) -> bool {
    req.need.is_some() || req.when.iter().any(|a| matches!(a, Atom::Sys(_)))
}

const fn source_of(atom: &Atom) -> Option<&SourceAtom> {
    match atom {
        Atom::Source(source) => Some(source),
        Atom::Time(_) | Atom::Sys(_) => None,
    }
}

/// The build slots `snap` proves usable, and how many there are.
fn slots_seen(snap: &Snapshot) -> Option<(u16, u16)> {
    match snap.slots {
        Some(SlotsView::Read {
            n,
            free,
            discounted,
            ..
        }) => Some((free.saturating_sub(discounted), n)),
        _ => None,
    }
}

/// Whether `sid`'s answer has its build slot: reserved in every mode but off (S9), never through
/// the typed lines' pace ([`crate::occupancy_host::pace_on`]).
fn slot_granted(
    env: &dyn Env,
    mode: Occupancy,
    sid: &str,
    age_ms: i64,
    usable: u16,
    now_ms: i64,
) -> bool {
    if fault::armed(fault::Fault::PaceOnAwait) {
        let kept = Mutex::new(None);
        return crate::occupancy_host::pace_on(
            &kept,
            mode,
            (sid, RuleId::HoldRoom, age_ms),
            &mut || None,
            &mut || Some(now_ms),
        )
        .lets();
    }
    if fault::armed(fault::Fault::AwaitSkipsReserve) {
        return true;
    }
    env.reserve(sid, age_ms, usable, now_ms) == Reserve::Granted
}

/// `#<n>` as an ordinal.
fn ordinal(word: &str) -> Option<u16> {
    let digits = word.strip_prefix('#')?;
    let canonical = (1..=5).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0');
    digits.parse::<u16>().ok().filter(|_| canonical)
}

fn kinds(wake: &DeclaredWake) -> String {
    wake.when
        .iter()
        .map(|a| a.atom.kind().word())
        .collect::<Vec<_>>()
        .join(",")
}

/// `waits`' line for `wake`: its label only `inside` the session's tree.
fn wake_line(wake: &DeclaredWake, inside: bool) -> String {
    let since = wake
        .ready_since_ms
        .map_or_else(|| "-".to_string(), |ms| ms.to_string());
    let mut line = format!(
        "#{} {} prio={} kinds={} declared={} by={} since={since}",
        wake.ord,
        wake.state.word(),
        wake.prio.word(),
        kinds(wake),
        wake.declared_ms,
        wake.due_by_ms,
    );
    if inside {
        line.push_str(&format!(" label={}", wake.label.as_str()));
    }
    line
}

/// How `wake`'s file atoms are read: `looks` where one is read on the looks alone (the budget
/// spent, an open failed), else `watch`; `None` where it reads none now.
fn source_word(wake: &DeclaredWake, sources: &[(SourceAtom, bool)]) -> Option<&'static str> {
    let mut words = wake
        .when
        .iter()
        .filter(|a| !a.held)
        .filter_map(|a| source_of(&a.atom))
        .chain(wake.unless.iter())
        .filter_map(|atom| sources.iter().find(|(read, _)| read == atom))
        .map(|(_, looks)| *looks)
        .peekable();
    words.peek()?;
    Some(if words.any(|looks| looks) {
        "looks"
    } else {
        "watch"
    })
}

/// `waits why`'s line for `wake`: its need only `inside` the session's tree, and how its file
/// atoms are read (`source=`, a fixed word, given `inside` alone).
fn why_line(wake: &DeclaredWake, inside: bool, source: Option<&str>) -> String {
    let mut line = format!("#{} {}", wake.ord, wake.state.word());
    if let WakeState::Expired { why } = wake.state {
        line.push_str(&format!(" why={}", why.word()));
    }
    let atoms = wake
        .when
        .iter()
        .map(|a| format!("{}:{}", a.atom.kind().word(), u8::from(a.held)))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str(&format!(" atoms={atoms}"));
    if inside && let Some(need) = &wake.need {
        line.push_str(&format!(" need={}", need.canonical()));
    }
    if wake.await_only {
        line.push_str(" await_only=1");
    }
    if wake.state.live()
        && let Some(source) = source
    {
        line.push_str(&format!(" source={source}"));
    }
    line
}

// ---------------------------------------------------------------------------------------------
// The product: one host, its sources thread, the verbs as the control server serves them.
// ---------------------------------------------------------------------------------------------

/// THE PROCESS'S HOST (named apart: the lazy-init census merges cells by name).
#[cfg(not(test))]
static WAKE_HOST: LazyLock<Arc<WakeHost>> = LazyLock::new(|| Arc::new(WakeHost::default()));

/// The process's host ([`WAKE_HOST`]); a test thread's own in tests, so tests run in parallel share
/// no book (as `occupancy_host`'s mode is a test thread's own).
fn host() -> Arc<WakeHost> {
    #[cfg(test)]
    {
        thread_local! {
            static WAKE_HOST: Arc<WakeHost> = Arc::new(WakeHost::default());
        }
        WAKE_HOST.with(Arc::clone)
    }
    #[cfg(not(test))]
    {
        Arc::clone(&WAKE_HOST)
    }
}

/// The product's [`Env`].
pub(crate) struct Live;

impl Env for Live {
    fn now_ms(&self) -> i64 {
        aterm_time::unix_millis()
            .and_then(|ms| i64::try_from(ms).ok())
            .unwrap_or(0)
    }

    fn local_offset_s(&self) -> i64 {
        crate::presence::local_offset_s().unwrap_or(0)
    }

    fn tree(&self) -> &dyn ProcTree {
        &Kernel
    }

    fn mode(&self) -> Occupancy {
        crate::occupancy_host::mode()
    }

    fn sys(&self, fresh: Option<Duration>) -> Option<SysRead> {
        let shared = &crate::sys_host::SYS;
        match fresh {
            Some(fresh) => {
                let (held, freshness) = shared.fresh(fresh, false);
                held.map(|h| SysRead {
                    snap: h.snap,
                    fresh: freshness == aterm_vitals::sys::Freshness::Fresh,
                })
            }
            None => shared.held_now().map(|h| SysRead {
                fresh: h.at.elapsed() <= crate::occupancy_host::STALE_AFTER,
                snap: h.snap,
            }),
        }
    }

    fn reserve(&self, sid: &str, age_ms: i64, usable: u16, now_ms: i64) -> Reserve {
        crate::occupancy_host::reserve_for_await(sid, age_ms, usable, now_ms)
    }

    fn said(&self, sid: &str) -> Option<Said> {
        crate::occupancy_host::said_of(sid)
            .map(|said| (said.hold.map(|h| h.since_ms), said.guard()))
    }

    fn hosted(&self, _sid: &str) -> bool {
        // No loop of this build reads the book (the plan's 2.7 does): an answer written is told.
        false
    }

    fn fs(&self) -> &dyn WakeFs {
        &files::Disk
    }
}

/// What the sources thread posts and reads ([`install`]).
struct Wiring {
    store: crate::session_store::Store,
    note: Arc<dyn Fn(crate::harness_host::HostNote) + Send + Sync>,
}

static WIRING: OnceLock<Wiring> = OnceLock::new();

/// Wire the book to the window: its session store and the host's note sink (`lib.rs`, beside
/// `program_check_watch::start`). The sources thread starts at the first declaration.
pub(crate) fn install(
    store: crate::session_store::Store,
    note: Arc<dyn Fn(crate::harness_host::HostNote) + Send + Sync>,
) {
    let _ = WIRING.set(Wiring { store, note });
}

/// Start the sources thread once: a plain thread, no child process.
fn ensure_running() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if WIRING.get().is_none() {
        return;
    }
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name(format!(
                "{}occupancy-wakes",
                crate::harness_host::THREAD_PREFIX
            ))
            .spawn(|| {
                let mut ticks: u32 = 0;
                let mut next = Instant::now() + TICK;
                loop {
                    let left = next.saturating_duration_since(Instant::now());
                    if !left.is_zero() {
                        // Between ticks the file watches are heard; their events coalesce for
                        // an edge gap, so a file written without pause costs a pass a gap.
                        let fired = host().wait_files(&Live, left);
                        if !fired.is_empty() {
                            host().on_files(&Live, &fired);
                            deliver(host().take_outs());
                            std::thread::sleep(EDGE_GAP.min(left));
                        }
                        continue;
                    }
                    next = Instant::now() + TICK;
                    ticks = ticks.wrapping_add(1);
                    forget_closed();
                    host().tick(&Live, ticks.is_multiple_of(TICKS_A_LOOK));
                    deliver(host().take_outs());
                }
            });
        if let Err(e) = spawned {
            aterm_log::warn!("occupancy wakes: the sources thread did not start: {e}");
        }
    });
}

/// Shelves of sessions the store no longer holds alive go.
fn forget_closed() {
    let Some(wiring) = WIRING.get() else {
        return;
    };
    let sids: Vec<String> = host().lock().shelves.keys().cloned().collect();
    let store = wiring.store.read().unwrap_or_else(PoisonError::into_inner);
    let gone: Vec<String> = sids
        .into_iter()
        .filter(|sid| {
            store
                .by_sid(&aterm_session::SessionId::new(sid.clone()))
                .is_none_or(|h| h.state == crate::session_store::SessionState::Exited)
        })
        .collect();
    drop(store);
    for sid in gone {
        host().forget(&sid);
    }
}

/// Deliver what the book owes through the window's wiring ([`deliver_to`]).
fn deliver(outs: Vec<Out>) {
    if outs.is_empty() {
        return;
    }
    if let Some(wiring) = WIRING.get() {
        deliver_to(wiring, outs);
    }
}

/// Deliver `outs` through `wiring`: each re-checked against the owner's mode in force (W4, W5).
fn deliver_to(wiring: &Wiring, outs: Vec<Out>) {
    for out in outs {
        match out {
            Out::Ready { local, ord, .. } => {
                if !crate::occupancy_host::prompt_now() {
                    continue;
                }
                let timeline = wiring
                    .store
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .by_local(local)
                    .map(|h| Arc::clone(&h.ctx.timeline));
                if let Some(timeline) = timeline {
                    timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .record_row(WAKE_READY, format!("#{ord} ready"));
                }
            }
            Out::Idle { sid, .. } => {
                (wiring.note)(crate::harness_host::HostNote::WakeIdle { sid });
            }
        }
    }
}

/// The timeline row a wake's coming ready is recorded as, under `prompt` alone; `subscribe …
/// events` pushes it as `EVENT <sid> wake #<n> ready` (`subscribe::timeline_wire_kind`).
pub(crate) const WAKE_READY: &str = "wake-ready";

/// The root a session's shell identity pins: its pid and the birth the session read as it took
/// the shell (unix µs on macOS, the kernel's `p_start` a [`ProcLink`] reads); `None` with no shell
/// or no birth read, so a bare pid roots no tree.
fn root_of(shell: aterm_pty::ShellIdentity) -> Option<AgentId> {
    Some(AgentId {
        pid: u32::try_from(shell.pid).ok().filter(|&pid| pid != 0)?,
        birth_us: i64::try_from(shell.birth?).ok()?,
    })
}

/// The product's [`Seat`] of local session `session`: its PTY child as its handle was registered
/// at spawn, pinned by its birth (a machine snapshot names a new tab only at its next pass, and
/// the pid may be another process's by the verb), its hosted agent from the harness's published
/// snapshot.
fn seat_of(store: &crate::session_store::Store, session: u64) -> Option<Seat> {
    let (sid, shell) = {
        let store = store.read().unwrap_or_else(PoisonError::into_inner);
        let handle = store.by_local(session)?;
        (handle.sid.as_str().to_string(), handle.shell_identity)
    };
    let pty_child = root_of(shell);
    let agent = crate::harness_host::published_snapshot(&sid).and_then(|snap| {
        let start = aterm_agent::harness::upgrade_models::parse_lstart(&snap.start)?;
        Some((snap.pid, i64::try_from(start).ok()?))
    });
    Some(Seat {
        sid,
        local: session,
        pty_child,
        agent,
    })
}

/// `wake <rest>` on local session `session`, for the connection this thread serves.
pub(crate) fn cmd_wake(store: &crate::session_store::Store, session: u64, rest: &str) -> String {
    let Some(seat) = seat_of(store, session) else {
        return "ERR exited\n".to_string();
    };
    let reply = host().wake(&Live, &seat, crate::control::serving_peer_pid(), rest);
    if reply.starts_with("OK wake #") {
        ensure_running();
    }
    deliver(host().take_outs());
    reply
}

/// `waits <rest>` on local session `session`.
pub(crate) fn cmd_waits(store: &crate::session_store::Store, session: u64, rest: &str) -> String {
    let Some(seat) = seat_of(store, session) else {
        return "ERR exited\n".to_string();
    };
    host().waits(&Live, &seat, crate::control::serving_peer_pid(), rest)
}

/// `await wake [timeout=<ms>]` on local session `session`: parked until its answer, its end, its
/// peer's hang-up, the session's exit or its time.
pub(crate) fn cmd_await_wake(
    store: &crate::session_store::Store,
    session: u64,
    rest: &str,
) -> crate::control::ControlReply {
    let mut timeout_ms = aterm_control::wire::WAIT_DEFAULT_MS;
    for tok in rest.split_whitespace().skip(1) {
        let Some(ms) = tok
            .strip_prefix("timeout=")
            .and_then(aterm_control::wire::wait_ms)
        else {
            return AWAIT_USAGE.to_string().into();
        };
        timeout_ms = ms;
    }
    let Some(seat) = seat_of(store, session) else {
        return "ERR exited\n".to_string().into();
    };
    let Some(conn) = crate::control::serving_connection() else {
        return denied(DeclarerRefusal::NoPeer).into();
    };
    let exited = || {
        store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_local(session)
            .is_none_or(|h| h.state == crate::session_store::SessionState::Exited)
    };
    let host = host();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let reply = await_wake(
        &host,
        &Live,
        &seat,
        crate::control::serving_peer_pid(),
        conn,
        &Waits {
            deadline,
            hung_up: &crate::control::caller_hung_up,
            exited: &exited,
            arm_slots: &|| crate::occupancy_host::SLOTS_ARMED.arm(),
        },
    );
    deliver(host.take_outs());
    match reply {
        Pass::Reply {
            reply,
            ord: Some(ord),
        } => {
            let (wire, written) = crate::control::ReplyWire::new();
            hang(host, Arc::new(Live), &wire, seat.sid, ord);
            crate::control::ControlReply::from(reply).releasing_on_write(written)
        }
        Pass::Reply { reply, ord: None } => reply.into(),
        Pass::Wait => "OK timeout\n".to_string().into(),
    }
}

/// What a parked wait stops for.
pub(crate) struct Waits<'a, A> {
    /// Its time.
    pub(crate) deadline: Instant,
    /// Its peer hung up.
    pub(crate) hung_up: &'a dyn Fn() -> bool,
    /// Its session exited.
    pub(crate) exited: &'a dyn Fn() -> bool,
    /// Arm the build slots' sampling while it waits on a wake naming them.
    pub(crate) arm_slots: &'a dyn Fn() -> A,
}

/// THE PARKED WAIT: parked, then a pass at each change or [`crate::sys_host::AWAIT_PERIOD`],
/// until its reply ([`Pass::Reply`]) or its time ([`Pass::Wait`]).
pub(crate) fn await_wake<A>(
    host: &WakeHost,
    env: &dyn Env,
    seat: &Seat,
    peer: Option<u32>,
    conn: u64,
    waits: &Waits<'_, A>,
) -> Pass {
    if let Err(reply) = host.park(env, seat, peer, conn) {
        return Pass::Reply { reply, ord: None };
    }
    let _armed = host.names_slots(&seat.sid).then(|| (waits.arm_slots)());
    loop {
        if let reply @ Pass::Reply { .. } = host.pass(env, &seat.sid, conn) {
            return reply;
        }
        let stop = if (waits.hung_up)() {
            Some(crate::control::HUNG_UP_REPLY.to_string())
        } else if (waits.exited)() {
            Some("ERR exited\n".to_string())
        } else if Instant::now() >= waits.deadline {
            Some("OK timeout\n".to_string())
        } else {
            None
        };
        if let Some(reply) = stop {
            host.unpark(&seat.sid, conn);
            return Pass::Reply { reply, ord: None };
        }
        host.wait_change(
            waits.deadline.min(
                Instant::now() + crate::control::HANGUP_POLL.min(crate::sys_host::AWAIT_PERIOD),
            ),
        );
    }
}

/// Hang the answer to `ord` of `sid` on its reply's latch: told once it is delivered, ready
/// again when it is not ([`WakeHost::answered`], S2).
pub(crate) fn hang(
    host: Arc<WakeHost>,
    env: Arc<dyn Env>,
    wire: &crate::control::ReplyWire,
    sid: String,
    ord: u16,
) {
    wire.on_release(move |delivered| {
        host.answered(&*env, &sid, ord, delivered);
        if !delivered {
            aterm_log::info!("occupancy wakes @{sid}: #{ord}'s answer was not delivered; ready");
        }
    });
}

/// `status`' `waits=<pending> waits_ready=<ready>` of `sid`.
pub(crate) fn status_fields(sid: Option<&str>) -> String {
    let (pending, ready) = sid.map_or((0, 0), |sid| host().counts(sid));
    format!("waits={pending} waits_ready={ready}")
}

/// The owner's mode moved ([`crate::exceptions_host`]'s `note_occupancy_mode`): under `off` every
/// book ends; the sids whose K Entry ends.
pub(crate) fn note_mode(mode: Occupancy) -> Vec<String> {
    host().note_mode(mode);
    host()
        .take_outs()
        .into_iter()
        .filter_map(|out| match out {
            Out::Idle { sid, idle: None } => Some(sid),
            Out::Idle { .. } | Out::Ready { .. } => None,
        })
        .collect()
}

/// `sid`'s wake K Entry as last owed, under the owner's `prompt` alone.
pub(crate) fn idle_of(sid: &str) -> Option<WakeIdle> {
    host()
        .idle_of(sid)
        .filter(|_| crate::occupancy_host::prompt_now())
}

/// TESTS ONLY: stand `idle` as `sid`'s K Entry on this thread's host.
#[cfg(test)]
pub(crate) fn set_idle_for_test(sid: &str, idle: Option<WakeIdle>) {
    host()
        .lock()
        .shelves
        .entry(sid.to_string())
        .or_default()
        .idle = idle;
}

/// TESTS ONLY: book `decl` for `agent` as session `sid`'s (local `local`) on this thread's host at
/// `now_ms`, no proof asked (the window's wiring tests' own book); its ordinal.
#[cfg(test)]
pub(crate) fn book_for_test(sid: &str, local: u64, agent: AgentId, decl: &str, now_ms: i64) -> u16 {
    let host = host();
    let mut books = host.lock();
    let shelf = books.shelf(&Seat {
        sid: sid.to_string(),
        local,
        pty_child: None,
        agent: None,
    });
    shelf
        .book
        .declare(
            WakeReq::parse(decl).expect("the grammar reads it"),
            Declarer::Agent,
            agent,
            now_ms,
            0,
        )
        .expect("booked")
}

/// TESTS ONLY: the states of `sid`'s book on this thread's host, by ordinal.
#[cfg(test)]
pub(crate) fn states_for_test(sid: &str) -> Vec<WakeState> {
    host()
        .lock()
        .shelves
        .get(sid)
        .map_or_else(Vec::new, |shelf| {
            shelf.book.wakes.iter().map(|w| w.state).collect()
        })
}

/// Tab `sid` closed: its book goes.
pub(crate) fn forget(sid: &str) {
    host().forget(sid);
}

/// `sid`'s book as its session record carries it.
#[cfg(any(unix, test))]
pub(crate) fn carry_row(sid: &str) -> Option<String> {
    host().carry_row(sid)
}

/// A book declared at `now_ms` as a working agent's tab carries it (a timed wake and one that
/// needs a slot), for the handoff fixture desks; and how many wakes a carried row reads back.
#[cfg(all(test, unix))]
pub(crate) fn fixture_row(now_ms: i64) -> String {
    let agent = AgentId {
        pid: 48_200,
        birth_us: now_ms.saturating_sub(600_000).saturating_mul(1_000),
    };
    let mut book = WakeBook::default();
    for decl in [
        "label=lunch when=in:30m",
        "label=build when=in:1h need=slots>=1",
    ] {
        book.declare(
            WakeReq::parse(decl).expect("the grammar reads it"),
            Declarer::Agent,
            agent,
            now_ms,
            0,
        )
        .expect("booked");
    }
    aterm_json::to_string(&Carry {
        next_ord: book.next_ord,
        wakes: book
            .wakes
            .iter()
            .map(|w| aterm_json::to_value(w).expect("a value"))
            .collect(),
        pids: Vec::new(),
        due: Vec::new(),
    })
    .expect("a carry row")
}

#[cfg(test)]
pub(crate) fn carried_wakes(row: &str) -> Option<usize> {
    Carry::lenient(row).map(|read| read.wakes.len())
}

/// A seamless successor's adopted session `sid` (local `local`) takes the book its record
/// carried: restored, re-admitted and level-checked at once ([`WakeHost::restore`]).
pub(crate) fn seed_carried(local: u64, sid: &str, row: Option<&str>) {
    let Some(row) = row else {
        return;
    };
    let seat = Seat {
        sid: sid.to_string(),
        local,
        pty_child: None,
        agent: None,
    };
    match host().restore(&Live, &seat, row) {
        Restored::Unread => {
            aterm_log::warn!(
                "occupancy wakes @{sid}: the carried book is not one this build reads"
            );
        }
        Restored::Read { refused, live } => {
            if refused > 0 {
                aterm_log::info!("occupancy wakes @{sid}: {refused} carried wake(s) not admitted");
            }
            if live > 0 {
                ensure_running();
            }
        }
    }
    deliver(host().take_outs());
}

#[cfg(test)]
#[path = "occupancy_wakes_tests.rs"]
mod tests;
