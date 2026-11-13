// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link notify`: run a command when the fabric needs a human ([`USAGE`] lists the flags).

use aterm_durable::Flush;
use aterm_uds::spawnfd::Stdio;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::bridge::ack_deadline;
use crate::notify_need::{NeedWatch, Pair, Step};
use crate::subject::{is_fleet, is_principal};
use crate::transport::{self, Backoff, Closer, Conn};

#[path = "notify_journal.rs"]
mod journal;

#[path = "notify_directory.rs"]
mod directory;
use aterm_dirfd::OFlags;
use directory::{Directory, TemporaryData};
use std::sync::Arc;

#[cfg(any(test, debug_assertions))]
pub use journal::rewind_cursor;
use journal::StepsFile;
pub use journal::{cursor_of, journal_lines, suppressed_count, FILE as JOURNAL_FILE};

const PAGE: u32 = crate::transport::POST_PAGE;

/// Verdicts kept in `journal` (must exceed [`PAGE`]).
const JOURNAL_KEEP: usize = 1024;

/// Number of non-root log stores (`logs/<identity>/`) retained by [`Store::prune_logs`].
const LOGS_KEEP: usize = 4;

/// The subjects whose standing value the store keeps ([`Notifier::set_standing`]): past it, the
/// oldest written goes, but never a `need` face the watch holds a fold of, whose needs would page
/// again. The watch holds at most [`crate::fabric_brief::SESSIONS_MAX`], so one past the bound
/// is always a subject it does not hold.
const STANDING_KEEP: usize = JOURNAL_KEEP;

const _: () = assert!(crate::fabric_brief::SESSIONS_MAX <= STANDING_KEEP);

/// The entries a `need` selector keeps on one `ev` face ([`Notifier::set_entries`]): twice the
/// needs a fold holds. Past it, the oldest that the face's fold no longer names go first, so the
/// bound never drops a need the fold still names (at most [`aterm_slog::fold::MAX_NEEDS`]); the
/// rest are the newest a partial fold keeps (it drops none: a need it does not name may be one
/// raised before the records it holds, or one cleared there).
const PAIRS_KEEP: usize = 2 * aterm_slog::fold::MAX_NEEDS;

/// Records a following selector judges past its written cursor before it writes it again
/// ([`Notifier::advance`]). Below [`JOURNAL_KEEP`], so a cursor a crash rolls back re-reads only
/// records whose verdicts the journal still holds.
const CURSOR_LAG: u64 = 256;

const _: () = assert!(CURSOR_LAG < JOURNAL_KEEP as u64);

/// How long a following selector's cursor trails the records it has judged before it is
/// written ([`Notifier::persist_due`], [`Notifier::follow`]).
const CURSOR_EVERY: Duration = Duration::from_secs(1);

/// How long a follower waits, after its state dir refused a write, before it writes what it
/// owes again: at the next record or tick past it ([`Notifier::settle`], [`Notifier::drain`]).
const RETRY_EVERY: Duration = Duration::from_secs(1);

const TEXT_MAX: usize = 256;

/// The longest [`Notifier::exec`] sleeps between reaps of a page's command when the kernel
/// gives no word of its exit: no exit descriptor could be made (a `pidfd_open` refused, any
/// other unix, a kqueue Darwin would not make). It is the 10 ms poll the kernel's word
/// replaced, so on such a host a page's reap still comes at most 10 ms after its command's exit.
const EXEC_FALLBACK_TICK: Duration = Duration::from_millis(10);

const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 10;
const DEFAULT_WINDOW_MS: u64 = 60_000;

const HOLD_MAX: usize = 4 << 20;

/// The words a follower's stderr says its durable wait with, once per log, after the selector's
/// label (fix unit E5, E2E-15).
pub const DURABLE_WAIT_WORDS: &str = "waits for each record until the broker says it is durable";

/// The development seam that has a following notifier tell the fabric bench the flushes its
/// store asked for ([`FLUSH_TALLY`]).
pub const FLUSH_TALLY_SEAM: &str = "ATERM_LINK_NOTIFY_FLUSH_TALLY";

/// The file under `<state>/notify/` a notifier on [`FLUSH_TALLY_SEAM`] rewrites, unflushed,
/// after each notice that flushed or wrote: `notify-flushes v=1 pid=<pid> durable=<n>
/// ordered=<n> bytes=<n>`, the file and directory flushes its stores asked for since it
/// started, by kind, and the bytes they wrote ([`BYTES`]).
pub const FLUSH_TALLY: &str = "flush-tally";

/// The development seam that has a following notifier tell the fabric bench, after each fresh
/// page, when its `firing` line was written, its command was reaped and its `fired` line was
/// written ([`EXEC_STAMPS`]).
pub const EXEC_STAMPS_SEAM: &str = "ATERM_LINK_NOTIFY_EXEC_STAMPS";

/// The file under `<state>/notify/` a notifier on [`EXEC_STAMPS_SEAM`] rewrites, unflushed,
/// after each notice that ran a fresh page: `notify-exec v=1 pid=<pid> off=<off> armed=<us>
/// exited=<us> fired=<us>`, each the microseconds from the notifier's start on its monotonic
/// clock to the `firing` line written, the command's wait ended (its exit reaped, or its kill at
/// the timeout) and the `fired` line written. Only their differences mean anything.
pub const EXEC_STAMPS: &str = "exec-stamps";

/// The flushes this process's notify stores asked for ([`FLUSH_TALLY`]): full, then ordered.
/// These count what each store ASKED for (aterm-durable's [`Flush`]), where
/// `astream_sys::flush_tally` counts the CALLS that answered it, every store's in the process:
/// an ordered flush whose barrier the volume refused is one `ordered` here, and one `barrier`
/// plus the one `full` that answered it there.
static FLUSHES: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// The bytes this process's notify stores wrote ([`FLUSH_TALLY`]'s `bytes=`): every replace's
/// ([`Store::put_bytes`]) and every append to a journal ([`journal::StepsFile::write`]).
static BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn note_bytes(n: usize) {
    BYTES.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    BYTES_NOTED.with(|noted| noted.set(noted.get() + n as u64));
}

/// [`BYTES`].
fn written_bytes() -> u64 {
    BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

fn note_flush(flush: Flush) {
    FLUSHES[usize::from(flush.is_ordered())].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    NOTED.with(|noted| noted.borrow_mut().push(flush));
}

/// [`FLUSHES`]: `(durable, ordered)`.
fn flushes() -> (u64, u64) {
    let [durable, ordered] = &FLUSHES;
    (
        durable.load(std::sync::atomic::Ordering::Relaxed),
        ordered.load(std::sync::atomic::Ordering::Relaxed),
    )
}

#[derive(Debug)]
enum Notice {
    Record(usize, u64, String, Vec<u8>),
    /// A record below the reader's cursor that a `need` selector folds before it judges any
    /// ([`crate::notify_need::warm`]).
    Warm(usize, u64, String, Vec<u8>),
    Checkpoint(usize, u64),
    /// A reader's start at the head it found with no cursor: the notifier writes it as the
    /// selector's cursor, ordered, and answers once it is on the media (or why not), before the
    /// reader reads a record ([`connect_for_reader`]).
    Start(usize, u64, mpsc::SyncSender<io::Result<()>>),
    Log(usize, ReadLog),
    Durable(usize, u64),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReadLog {
    id: Option<astream_broker::LogIdentity>,
    lineage: Option<astream_broker::Lineage>,
    ordered: bool,
}

#[derive(Debug)]
struct Held {
    off: u64,
    subject: String,
    body: Vec<u8>,
}

/// A selector's cursor owed and not yet written ([`Notifier::advance`]).
#[derive(Debug, Clone, Copy)]
struct Lag {
    /// Where the cursor goes when it is written.
    want: u64,
    /// The records judged since it was last written.
    records: u64,
    /// When the first of those was judged.
    since: Instant,
    /// Whether it was due and not written (the state dir refused it, or a part a judged record
    /// changed is unwritten): the next settle writes it whatever its age
    /// ([`Notifier::checkpoint`]).
    retry: bool,
}

/// A part of the notifier's state that a judged record changes in memory and the store holds
/// ([`Notifier::save`]), in the order a write puts them on the store: a verdict ahead of the
/// stamps it spends and the count it bumps (one step of the journal, where the log's store is
/// the root's, [`journal`]), those ahead of a standing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Journal,
    Stamps,
    Suppressed,
    Standing,
}

impl Part {
    /// The part as a warning names it ([`Notifier::fail`]).
    const fn what(self) -> &'static str {
        match self {
            Part::Journal => "the journal",
            Part::Stamps => "the rate stamps",
            Part::Suppressed => "the suppressed count",
            Part::Standing => "the standing values",
        }
    }
}

/// The refused write a follower backs off from ([`Notifier::fail`]).
#[derive(Debug, Clone, Copy)]
struct Failing {
    /// When it writes what it owes again.
    next: Instant,
    /// The errno (or the kind, where the error has none) it last warned of.
    errno: Option<i32>,
    kind: io::ErrorKind,
}

/// A page judged and not yet run, because its `firing` line or its stamps could not be put on
/// the media ([`Notifier::arm`]): its command has not run ([`Left::Page`]).
#[derive(Debug)]
struct Page {
    sel: Sel,
    off: u64,
    subject: String,
    /// What it leaves standing once it ran ([`Stand`]).
    raised: Option<Stand>,
    hit: Hit,
    /// The `firing` line and its stamp, for a fresh page; none for a re-run (`dup=1`).
    firing: Option<(String, u64)>,
    /// The reader whose held records it was released from, and the bound they were released to
    /// ([`Notifier::release`]), released on once it ran.
    then: Option<(usize, u64)>,
}

/// What a write the state dir refused left a follower to do, done again in order before it
/// takes another notice ([`Notifier::drain`]).
#[derive(Debug)]
enum Left {
    /// A notice not yet taken: the store of its log could not be taken up
    /// ([`Notifier::scope_to`]), a part an earlier record changed is unwritten
    /// ([`Notifier::owing`]), or another stood ahead of it.
    Notice(Notice),
    /// A page whose command has not run.
    Page(Box<Page>),
    /// A reader's held records, to be judged below the bound ([`Notifier::release`]).
    Release(usize, u64),
}

/// A refused write that stopped the follower where it was: what is left, the error, and what
/// the write held, as a warning names it ([`Notifier::fail`]).
#[derive(Debug)]
struct Stalled {
    left: Left,
    err: io::Error,
    what: &'static str,
}

fn notice_handoff() -> (mpsc::SyncSender<Notice>, mpsc::Receiver<Notice>) {
    mpsc::sync_channel(0)
}

/// One `--on` selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    Attention,
    Ask(String),
    Halt,
    /// `need` (`None`: any word) or `need:<word>`,…: a need a session's fold raises, as
    /// `aterm fabric brief` folds it from the session log's bus mirror
    /// ([`crate::notify_need`]), one of these words ([`need_words`]) or of these families
    /// ([`need_families`]). Every `need` word of one `--on` list is one selector
    /// ([`Sel::merge`]): one reader folds the faces once.
    Need(Option<Vec<&'static str>>),
}

impl Sel {
    /// Parse one selector word.
    /// # Errors
    /// A word that is not `attention`, `halt`, `ask:<principal>`, `need` or `need:<word>` with
    /// `<word>` a need a session's fold raises ([`need_words`]) or a family of them
    /// ([`need_families`]), which the error names.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "attention" => Ok(Sel::Attention),
            "halt" => Ok(Sel::Halt),
            "need" => Ok(Sel::Need(None)),
            other => {
                if let Some(w) = other.strip_prefix("need:") {
                    return need_words()
                        .chain(need_families())
                        .find(|known| *known == w)
                        .map(|w| Sel::Need(Some(vec![w])))
                        .ok_or_else(|| {
                            format!(
                                "--on need:{w}: not a need a session's fold raises ({}; or a \
                                 family: {})",
                                need_words().collect::<Vec<_>>().join(" "),
                                need_families().collect::<Vec<_>>().join(" ")
                            )
                        });
                }
                match other.strip_prefix("ask:") {
                    Some(p) if is_principal(p) => Ok(Sel::Ask(p.to_string())),
                    Some(p) => Err(format!("--on ask:{p}: {p} is not a principal")),
                    None => Err(format!(
                        "--on {other}: expected attention, halt, ask:<principal>, need or \
                         need:<word>"
                    )),
                }
            }
        }
    }

    /// Fold `other` into `self` when both are `need` selectors (a bare `need` takes every word),
    /// and say whether it did.
    fn merge(&mut self, other: &Sel) -> bool {
        let (Sel::Need(have), Sel::Need(more)) = (self, other) else {
            return false;
        };
        match (have.as_mut(), more) {
            (None, _) => {}
            (Some(_), None) => *have = None,
            (Some(words), Some(more)) => {
                for w in more {
                    if !words.contains(w) {
                        words.push(w);
                    }
                }
            }
        }
        true
    }

    /// Whether a `need` selector pages a need that rose to `word`: a word it names, or one of a
    /// family it names (`approval` takes `approval:<kind>`).
    fn selects(&self, word: &str) -> bool {
        match self {
            Sel::Need(None) => true,
            Sel::Need(Some(words)) => words.iter().any(|w| {
                *w == word || word.split_once(':').is_some_and(|(family, _)| family == *w)
            }),
            _ => false,
        }
    }

    /// How this selector is written on the command line and in the journal.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Sel::Attention => "attention".to_string(),
            Sel::Ask(p) => format!("ask:{p}"),
            Sel::Halt => "halt".to_string(),
            Sel::Need(_) => "need".to_string(),
        }
    }

    /// The cursor file name.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Sel::Attention => "attention".to_string(),
            Sel::Ask(p) => format!("ask-to.{p}"),
            Sel::Halt => "halt".to_string(),
            Sel::Need(_) => "need".to_string(),
        }
    }

    /// The grant that reads [`Sel::filter`], and which cap carries it ([`Notifier::preflight`]).
    #[must_use]
    pub fn grant(&self, fleet: &str) -> String {
        match self {
            Sel::Attention => {
                format!("`ro:/f/{fleet}/pub/>`, which node.cap and a person's cap both hold")
            }
            Sel::Ask(p) => format!(
                "`ro:/f/{fleet}/in/p/{p}/>`, which {p}'s own cap holds (`aterm fabric mint-for \
                 {p} --out <cap>` on this machine: its fabric root holds the mint secret)"
            ),
            Sel::Halt => format!("`ro:/f/{fleet}/fleet/>`, which node.cap holds"),
            Sel::Need(_) => format!(
                "`ro:/f/{fleet}/pub/>`, which node.cap and a person's cap both hold (the caps \
                 `aterm fabric brief` reads under)"
            ),
        }
    }

    /// The broker filter that carries this selector's records: a `need` reads every session
    /// and node face, for the `ev` records its fold takes and the presence rows that say a
    /// session or its node is gone.
    #[must_use]
    pub fn filter(&self, fleet: &str) -> String {
        match self {
            Sel::Attention => format!("/f/{fleet}/pub/*/*/presence"),
            Sel::Ask(p) => format!("/f/{fleet}/in/p/{p}/*/ask"),
            Sel::Halt => format!("/f/{fleet}/fleet/*/halt"),
            Sel::Need(_) => format!("/f/{fleet}/pub/*/*/*"),
        }
    }

    fn hit(&self, fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Hit> {
        let segs: Vec<&str> = subject.split('/').collect();
        if segs.first() != Some(&"") || segs.get(1) != Some(&"f") || segs.get(2) != Some(&fleet) {
            return None;
        }
        match self {
            Sel::Attention => {
                let row = crate::glance::Row::parse(fleet, offset, subject, body)?;
                let attention = crate::pct::decode(row.field("attention"));
                if attention.is_empty() || attention == crate::glance::ABSENT {
                    return None;
                }
                Some(Hit {
                    on: self.label(),
                    offset,
                    subject: subject_env(subject),
                    node: row.node,
                    who: row.owner,
                    sid: String::new(),
                    text: one_line(&attention),
                    need: String::new(),
                    detail: String::new(),
                })
            }
            Sel::Ask(p) => {
                if segs.len() != 8
                    || segs[3] != "in"
                    || segs[4] != "p"
                    || segs[5] != p
                    || segs[7] != "ask"
                {
                    return None;
                }
                if !crate::subject::is_principal(segs[6]) {
                    return None;
                }
                let (parsed, _) = Body::decode(body);
                let sid = parsed
                    .from
                    .as_deref()
                    .filter(|s| s.starts_with("s-") && crate::subject::is_principal(s))
                    .unwrap_or_default();
                Some(Hit {
                    on: self.label(),
                    offset,
                    subject: subject_env(subject),
                    node: segs[6].to_string(),
                    who: p.clone(),
                    sid: sid.to_string(),
                    text: one_line(&parsed.text),
                    need: String::new(),
                    detail: String::new(),
                })
            }
            Sel::Halt => {
                let row = crate::subject::parse_halt(fleet, subject, body)?;
                if !crate::subject::is_principal(row.human) {
                    return None;
                }
                if !row.on {
                    return None;
                }
                Some(Hit {
                    on: self.label(),
                    offset,
                    subject: subject_env(subject),
                    node: String::new(),
                    who: row.human.to_string(),
                    sid: String::new(),
                    text: one_line(&crate::pct::decode(
                        row.reason.as_deref().unwrap_or_default(),
                    )),
                    need: String::new(),
                    detail: String::new(),
                })
            }
            // A need is a session's fold, not one record's: [`Notifier::need_hit`].
            Sel::Need(_) => None,
        }
    }
}

/// What a presence row says of its subject's escalation ([`Sel::hit`]).
fn escalation(fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Option<String>> {
    let row = crate::glance::Row::parse(fleet, offset, subject, body)?;
    let raw = row.field("attention");
    let text = crate::pct::decode(raw);
    Some((!text.is_empty() && text != crate::glance::ABSENT).then(|| raw.to_string()))
}

/// What a page leaves standing once its command ran ([`Notifier::stand`]), or owed when it did
/// not ([`Notifier::owe`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stand {
    /// `attention`'s token on its presence subject: the row's raw `attention=` value.
    Token(String),
    /// A `need`'s pair on the session's `ev` face, with the pairs the face's fold names as it
    /// rose (which the face's bound keeps, [`PAIRS_KEEP`]).
    Need {
        face: String,
        pair: Pair,
        names: Vec<Pair>,
    },
}

/// One need on a `need` face's standing value ([`Notifier::entries_of`]): its pair, and whether
/// its page is owed: it rose and its command ended other than `rc=0`, or `--rate` dropped it.
/// An owed need stands for no warm-up ([`Notifier::seed_standing`]), so it pages again at its
/// face's next record whose fold still sees it ([`crate::notify_need::Named`]), in this run or
/// after a restart, however that fold knows its raise.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    pair: Pair,
    owed: bool,
}

impl Entry {
    /// The entry as the store spells it: its [`Pair::token`], `!` before it when owed.
    fn token(&self) -> String {
        if self.owed {
            format!("!{}", self.pair.token())
        } else {
            self.pair.token()
        }
    }

    /// An [`Entry::token`] read back.
    fn parse(token: &str) -> Option<Entry> {
        let (owed, pair) = match token.strip_prefix('!') {
            Some(pair) => (true, pair),
            None => (false, token),
        };
        Pair::parse(pair).map(|pair| Entry { pair, owed })
    }
}

/// `entries` within [`PAIRS_KEEP`]: past it, the oldest that none of `names` (the pairs the
/// face's fold names) is the same need as goes first, then the oldest.
fn bounded(mut entries: Vec<Entry>, names: &[Pair]) -> Vec<Entry> {
    while entries.len() > PAIRS_KEEP {
        let at = entries
            .iter()
            .position(|e| !names.iter().any(|n| n.same(&e.pair)))
            .unwrap_or(0);
        entries.remove(at);
    }
    entries
}

/// One record worth waking a human for, reduced to the facts the command gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub on: String,
    pub offset: u64,
    pub subject: String,
    pub node: String,
    pub who: String,
    /// The asking session, for an `ask` ([`Hit::node`]); the session whose need rose, for a
    /// `need`.
    pub sid: String,
    pub text: String,
    /// The need word a `need` rose to (empty for the other selectors).
    pub need: String,
    /// The detail `aterm fabric brief` shows beside that need (a wall's reset, a failure's exit
    /// code), or empty.
    pub detail: String,
}

/// The subject as an environment value ([`TEXT_MAX`]).
fn subject_env(subject: &str) -> String {
    one_line(subject)
}

/// Flatten untrusted text to one line of printable ASCII, capped at [`TEXT_MAX`].
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(TEXT_MAX));
    for ch in s.chars() {
        if out.len() >= TEXT_MAX {
            break;
        }
        out.push(if ch.is_ascii_graphic() || ch == ' ' {
            ch
        } else {
            '.'
        });
    }
    out
}

/// Where a scan starts when a selector has no cursor yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    Head,
    Start,
    At(u64),
}

#[derive(Clone, Debug)]
struct Store<Access = Owned> {
    root: PathBuf,
    directory: Arc<Directory>,
    access: Access,
}

#[derive(Clone, Copy, Debug)]
struct ReadOnly;

#[derive(Clone, Debug)]
struct Owned(Arc<Hold>);

/// PID diagnostic entry. Ownership rests on the directory descriptor, not this name.
const LOCK: &str = "lock";

/// One independent directory description with an exclusive flock. Only its final owner
/// releases LOCK_UN; store and journal clones share that owner through Arc<Hold>.
#[derive(Debug)]
struct StoreLock {
    file: std::fs::File,
    path: PathBuf,
    id: (u64, u64),
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Complete directory capture attempts before refusing a root that keeps changing.
const HOLD_TRIES: usize = 3;

/// A file's `(dev, ino)`, what [`StoreLock`] holds it by.
fn file_id(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    (meta.dev(), meta.ino())
}

impl StoreLock {
    /// The directory name's current identity, without following a final symlink.
    /// Missing or non-directory ancestors answer None; unreadable metadata is Unknown.
    ///
    /// # Errors
    /// Metadata could not be read for a reason other than absence.
    fn named(&self) -> io::Result<Option<(u64, u64)>> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => Ok(Some(file_id(&meta))),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the root pathname still names the directory this independent description
    /// locks. Replacing the PID file cannot affect this directory's identity or lease.
    ///
    /// # Errors
    /// The pathname's metadata is unavailable; no ownership loss is inferred from that.
    fn moved(&self) -> io::Result<Option<Moved>> {
        Ok(match self.named()? {
            Some(id) if id == self.id => None,
            Some(other) => Some(Moved::Replaced(other)),
            None => Some(Moved::Removed),
        })
    }

    /// The refusal a running notifier gives once its store `moved` ([`Hold::check`]): kind
    /// [`io::ErrorKind::NotFound`] where the path names nothing, [`io::ErrorKind::Other`] where
    /// it names another file, the words naming the store either way.
    fn lost(&self, moved: Moved) -> io::Error {
        let store = self.path.display();
        let (kind, now) = match moved {
            Moved::Removed => (io::ErrorKind::NotFound, "names nothing".to_string()),
            Moved::Replaced((dev, ino)) => (io::ErrorKind::Other, format!("names {dev}:{ino}")),
        };
        io::Error::new(
            kind,
            format!(
                "the notify store {store} was removed or replaced under this notifier: {} {now}, \
                 not the directory it holds ({}:{}); it stops rather than write a store another \
                 notifier may now hold",
                self.path.display(),
                self.id.0,
                self.id.1
            ),
        )
    }
}

/// What replaced the held directory at its original pathname ([`StoreLock::moved`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moved {
    /// The root directory is no longer named here.
    Removed,
    /// Another file, by its `(dev, ino)`.
    Replaced((u64, u64)),
}

/// The root directory's actual lease, shared by writers, journals and readers that can
/// take up a writable log store. A changed name stops the notifier at its next check;
/// operations already in progress remain rooted in their original open directories.
#[derive(Debug)]
struct Hold {
    /// The root store's directory, `notify/`.
    root: PathBuf,
    directory: Arc<Directory>,
    lock: StoreLock,
    /// The refusal's kind and words, set by the first check that found the path moved.
    lost: std::sync::OnceLock<(io::ErrorKind, String)>,
}

impl Hold {
    fn new(root: PathBuf, directory: Arc<Directory>, lock: StoreLock) -> Self {
        Self {
            root,
            directory,
            lock,
            lost: std::sync::OnceLock::new(),
        }
    }

    /// Whether the store's path still names the directory held ([`StoreLock::moved`], one `lstat`).
    /// Once it does not, the store was removed or replaced and is a new store a second
    /// notifier can take: the first check to find it warns once and records the refusal, and
    /// it and every check after answer that refusal, which ends the run.
    ///
    /// # Errors
    /// The refusal ([`StoreLock::lost`]), recorded; or an `lstat` that did not answer, its errno
    /// as it is and not recorded: a write the state dir refuses, retried like any other
    /// (`Notifier::fail`).
    fn check(&self) -> io::Result<()> {
        if let Some(e) = self.lost() {
            return Err(e);
        }
        let Some(moved) = self.lock.moved()? else {
            return Ok(());
        };
        let e = self.lock.lost(moved);
        if self.lost.set((e.kind(), e.to_string())).is_ok() {
            warn(&e.to_string());
        }
        Err(self.lost().unwrap_or(e))
    }

    /// The refusal [`Hold::check`] recorded, once the store's path no longer named its directory.
    fn lost(&self) -> Option<io::Error> {
        self.lost
            .get()
            .map(|(kind, said)| io::Error::new(*kind, said.clone()))
    }
}

impl<Access> Store<Access> {
    fn read_only(&self) -> Store<ReadOnly> {
        Store {
            root: self.root.clone(),
            directory: Arc::clone(&self.directory),
            access: ReadOnly,
        }
    }

    fn read_bytes(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        let (parent, leaf) = self.directory.parent(name, false)?;
        parent.read(&leaf, max as u64)
    }

    fn read_text(&self, name: &str, max: usize) -> io::Result<String> {
        String::from_utf8(self.read_bytes(name, max)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn metadata(&self, name: &str) -> io::Result<aterm_dirfd::Stat> {
        let (parent, leaf) = self.directory.parent(name, false)?;
        parent.stat(&leaf)
    }

    fn read(&self, name: &str) -> Option<String> {
        self.read_text(name, HOLD_MAX)
            .ok()
            .map(|text| text.trim_end_matches('\n').to_owned())
            .filter(|text| !text.is_empty())
    }

    fn carry_text(&self, name: &str) -> io::Result<Option<String>> {
        match self.read_text(name, HOLD_MAX) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => match self.metadata(name) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(other) => Err(other),
                Ok(_) => Err(error),
            },
            Err(error) => Err(error),
        }
    }
}

impl Store<ReadOnly> {
    fn existing(root: PathBuf) -> io::Result<Self> {
        let directory = Directory::open(&root)?;
        Ok(Self {
            root,
            directory,
            access: ReadOnly,
        })
    }
}

impl Store {
    fn open(state_dir: &str) -> io::Result<Self> {
        Self::at(Path::new(state_dir).join("notify"))
    }

    fn at(root: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        for _ in 0..HOLD_TRIES {
            let directory = Directory::open(&root)?;
            // A fresh description is essential: flock on a duplicated I/O description
            // could create two guards whose first LOCK_UN unlocks the other.
            let file = directory.independent()?;
            let id = file_id(&file.metadata()?);
            #[expect(
                clippy::disallowed_methods,
                reason = "StoreLock owns this independent descriptor and releases LOCK_UN in Drop"
            )]
            let taken = file.try_lock();
            Self::judge_take_at(&root, &directory, taken)?;
            let lock = StoreLock {
                file,
                path: root.clone(),
                id,
            };
            if lock.moved()?.is_some() {
                continue;
            }
            let hold = Arc::new(Hold::new(root.clone(), Arc::clone(&directory), lock));
            let store = Self {
                root: root.clone(),
                directory,
                access: Owned(hold),
            };
            let mut pid = store.directory.open_file(
                std::ffi::OsStr::new(LOCK),
                OFlags::RDWR | OFlags::CREATE,
                0o600,
            )?;
            use std::os::unix::fs::MetadataExt as _;
            let metadata = pid.metadata()?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.uid() != aterm_uds::peer::our_uid()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "notify PID entry is not a regular file",
                ));
            }
            pid.set_len(0)?;
            pid.write_all(format!("{}\n", std::process::id()).as_bytes())?;
            return Ok(store);
        }
        Err(io::Error::other(format!(
            "the notify directory {} changed during every ownership attempt",
            root.display()
        )))
    }

    fn child(&self, name: &str, create: bool) -> io::Result<Self> {
        let (parent, leaf) = self.directory.parent(name, create)?;
        let directory = parent.child(&leaf, create)?;
        Ok(Self {
            root: self.root.join(name),
            directory,
            access: self.access.clone(),
        })
    }

    /// Judge the directory flock, refusing contention and unsupported locking. The PID
    /// entry can add diagnostic context to contention; it never grants ownership.
    ///
    /// # Errors
    /// Every answer but a take.
    fn judge_take_at(
        root: &Path,
        directory: &Directory,
        taken: Result<(), std::fs::TryLockError>,
    ) -> io::Result<()> {
        match taken {
            Ok(()) => Ok(()),
            Err(std::fs::TryLockError::Error(error)) if error.kind() == io::ErrorKind::Unsupported => {
                Err(io::Error::new(io::ErrorKind::Unsupported, format!("the file system of notify store {} takes no directory lock ({error}); use --state on a volume that locks", root.display())))
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error),
            Err(std::fs::TryLockError::WouldBlock) => {
                let holder = directory.read(std::ffi::OsStr::new(LOCK), 64).ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .and_then(|text| crate::parse_uint::<u32>(text.trim()))
                    .map_or_else(String::new, |pid| format!(" (pid {pid})"));
                Err(io::Error::new(io::ErrorKind::WouldBlock, format!("the notify store {} is held by another `aterm link notify`{holder}: one notifier runs per state dir", root.display())))
            }
        }
    }

    /// Replace `name` with `value` ([`Store::put`]), then flush its directory by `flush`
    /// ([`flush_dir`]). A [`Flush::Ordered`] write costs a repeat if a power loss takes it; a
    /// [`Flush::Durable`] one is the write's one full flush, and it puts on the media every
    /// write ordered before it on the volume.
    fn write(&self, name: &str, value: &str, flush: Flush) -> io::Result<()> {
        let dir = self.put(name, value)?;
        flush_dir(&dir, flush)
    }

    /// [`Store::put_bytes`] of `value` and a newline: a file of lines.
    fn put(&self, name: &str, value: &str) -> io::Result<Arc<Directory>> {
        self.put_bytes(name, format!("{value}\n").as_bytes())
    }

    /// Write `bytes` beside `name`, order them (`aterm_durable::ordered_or_fsync`) and rename
    /// them over `name`, and return the directory whose entry that moved, unflushed: the caller
    /// flushes it ([`Store::write`]).
    fn put_bytes(&self, name: &str, bytes: &[u8]) -> io::Result<Arc<Directory>> {
        test_fault(name)?;
        let (parent, leaf) = self.directory.parent(name, true)?;
        let prefix = leaf.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 notify target")
        })?;
        let mut temporary = parent.temporary(prefix, false)?;
        let TemporaryData::File(file) = &mut temporary.data else {
            return Err(io::Error::other("notify file temporary has the wrong kind"));
        };
        file.write_all(bytes)?;
        note_bytes(bytes.len());
        aterm_durable::ordered_or_fsync(file)?;
        note_flush(Flush::Ordered);
        temporary.publish(&parent, &leaf)?;
        Ok(parent)
    }

    /// Return the [`Store`] for `log`.
    fn for_log(
        &self,
        log: Option<astream_broker::LogIdentity>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> io::Result<Store> {
        let Some(log) = log else {
            return Ok(self.clone());
        };
        let name = log.to_string();
        let owner = match self.read_text("log", 4096) {
            Ok(saved) => Some(
                astream_broker::LogIdentity::parse(saved.trim_end_matches('\n')).ok_or_else(
                    || io::Error::new(io::ErrorKind::InvalidData, "invalid notifier log identity"),
                )?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => match self.metadata("log") {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => None,
                Err(other) => return Err(other),
                Ok(_) => return Err(error),
            },
            Err(error) => return Err(error),
        };
        match owner {
            None => {
                self.write("log", &name, Flush::Ordered)?;
                Ok(self.clone())
            }
            Some(owner) if owner == log => Ok(self.clone()),
            Some(owner) => {
                let logs = self.child("logs", true)?;
                let absent = match logs.directory.stat(std::ffi::OsStr::new(&name)) {
                    Ok(_) => false,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                    Err(error) => return Err(error),
                };
                if absent {
                    if let Some((from, fork)) = self.predecessor(owner, log, lineage)? {
                        logs.carry(&from, &name, fork)?;
                    }
                }
                let directory = logs.directory.child(std::ffi::OsStr::new(&name), true)?;
                if let Err(error) = self.prune_logs(&name) {
                    eprintln!(
                        "aterm-link notify: could not prune stores under {}: {error}",
                        logs.root.display()
                    );
                }
                Ok(Store {
                    root: self.root.join("logs").join(name),
                    directory,
                    access: self.access.clone(),
                })
            }
        }
    }

    fn predecessor(
        &self,
        owner: astream_broker::LogIdentity,
        log: astream_broker::LogIdentity,
        lineage: Option<&astream_broker::Lineage>,
    ) -> io::Result<Option<(Store<ReadOnly>, u64)>> {
        let Some(lineage) = lineage else {
            return Ok(None);
        };
        let mut held = vec![(owner, self.read_only())];
        match self.directory.child(std::ffi::OsStr::new("logs"), false) {
            Ok(logs) => {
                for name in logs.names()? {
                    if let Some(id) = name.to_str().and_then(astream_broker::LogIdentity::parse) {
                        let directory = logs.child(&name, false)?;
                        held.push((
                            id,
                            Store {
                                root: logs.path.join(name),
                                directory,
                                access: ReadOnly,
                            },
                        ));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(held
            .into_iter()
            .filter(|(earlier, _)| *earlier != log)
            .filter_map(|(earlier, store)| {
                Some((earlier.generation, store, log.continues(earlier, lineage)?))
            })
            .max_by_key(|(generation, _, _)| *generation)
            .map(|(_, store, fork)| (store, fork)))
    }

    fn carry(&self, from: &Store<ReadOnly>, name: &str, fork: u64) -> io::Result<()> {
        let temporary = self.directory.temporary("carry", true)?;
        let TemporaryData::Directory(directory) = &temporary.data else {
            return Err(io::Error::other(
                "notify directory temporary has the wrong kind",
            ));
        };
        let built = Store {
            root: directory.path.clone(),
            directory: Arc::clone(directory),
            access: self.access.clone(),
        };
        let held = from.read_verdicts()?;
        let below = journal::Verdicts {
            lines: held
                .lines
                .into_iter()
                .filter(|line| journal::offset_of(line).is_some_and(|off| off < fork))
                .collect(),
            standing: held.standing,
            cursors: held
                .cursors
                .into_iter()
                .map(|(key, off)| (key, off.min(fork)))
                .collect(),
            ..journal::Verdicts::default()
        };
        built.set_verdicts(&below, Flush::Ordered)?;
        flush_dir(&built.directory, Flush::Durable)?;
        match temporary.publish(&self.directory, std::ffi::OsStr::new(name)) {
            Ok(()) => flush_dir(&self.directory, Flush::Durable),
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && self
                        .directory
                        .child(std::ffi::OsStr::new(name), false)
                        .is_ok() =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn prune_logs(&self, name: &str) -> io::Result<()> {
        let mut used: Vec<String> = self
            .read("logs/used")
            .map(|text| {
                text.lines()
                    .filter(|line| {
                        *line != name && astream_broker::LogIdentity::parse(line).is_some()
                    })
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        used.push(name.to_owned());
        let keep = used.split_off(used.len().saturating_sub(LOGS_KEEP));
        self.write("logs/used", &keep.join("\n"), Flush::Ordered)?;
        let logs = self.directory.child(std::ffi::OsStr::new("logs"), false)?;
        for entry in logs.names()? {
            let Some(id) = entry
                .to_str()
                .filter(|name| astream_broker::LogIdentity::parse(name).is_some())
            else {
                continue;
            };
            if !keep.iter().any(|kept| kept == id) {
                let metadata = logs.stat(&entry)?;
                if !aterm_dirfd::FileType::from_raw_mode(metadata.st_mode).is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "old notify log store is not a directory",
                    ));
                }
                logs.remove_tree(&entry)?;
            }
        }
        Ok(())
    }

    /// `key`'s cursor as the store holds it, read and never written ([`Store::read_cursor`]:
    /// an old store's `cur/<key>` file read as its migration would): none where it holds none
    /// or does not read. A reader's start, before the notifier that
    /// writes the store has its own copy ([`Notifier::cursor`]).
    fn cursor(&self, key: &str) -> Option<u64> {
        self.read_cursor(key)
    }

    /// A test's write of `key`'s cursor to this store outside a notifier: forward to `off`
    /// (never back), one step of its journal ([`Store::load`]), ordered, as
    /// [`Notifier::put_cursor`] writes it.
    #[cfg(test)]
    fn set_cursor(&self, key: &str, off: u64) -> io::Result<()> {
        if self
            .load()?
            .held
            .cursors
            .get(key)
            .is_some_and(|&have| have >= off)
        {
            return Ok(());
        }
        self.put_cursor_at(key, off)
    }

    /// A test's write of `key`'s cursor at `off`, back or forward: one step of the store's
    /// journal, ordered, as the store would hold it had its cursor never passed `off`.
    #[cfg(test)]
    fn put_cursor_at(&self, key: &str, off: u64) -> io::Result<()> {
        self.load()?.write(
            &[journal::Step {
                cursor: vec![(key.to_string(), off)],
                ..journal::Step::default()
            }],
            Flush::Ordered,
            None,
        )
    }
}

/// What a test has a store's write meet before it writes ([`PUT_FAULT`], [`REFUSE`],
/// [`PUT_NAMES`]): a replace's and a journal append's alike. Nothing outside a test.
#[allow(clippy::unnecessary_wraps)]
fn test_fault(name: &str) -> io::Result<()> {
    #[cfg(test)]
    {
        if PUT_FAULT.with(|left| {
            let dead = left.get() == Some(0);
            left.set(left.get().map(|n| n.saturating_sub(1)));
            dead
        }) {
            return Err(io::Error::other("PUT_FAULT: killed before this write"));
        }
        if let Some(errno) = refused(name) {
            return Err(io::Error::from_raw_os_error(errno));
        }
        PUT_NAMES.with(|names| {
            if let Some(names) = names.borrow_mut().as_mut() {
                names.push(name.to_string());
            }
        });
    }
    #[cfg(not(test))]
    let _ = name;
    Ok(())
}

/// Flush `dir`'s entries by `flush`, going on where they cannot be flushed here by astream's
/// one rule (`astream_sys::dir_flush_unavailable`, which `crate::state::sync_dir_as` keeps too).
/// It takes the flush through `aterm_durable` in a test build as well, so a test's recording
/// sees it.
fn flush_dir(dir: &Directory, flush: Flush) -> io::Result<()> {
    note_flush(flush);
    #[cfg(test)]
    DIRS_FLUSHED.with(|dirs| {
        if let Some(dirs) = dirs.borrow_mut().as_mut() {
            dirs.push(dir.path.clone());
        }
    });
    aterm_durable::DirFlushed::classify(flush.file(&dir.file)).map(drop)
}

/// One `aterm-link notify:` line on stderr.
fn warn(line: &str) {
    eprintln!("aterm-link notify: {line}");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    KillAfterExec,
}

impl Fault {
    fn from_env(store: &Store) -> Self {
        let Some(fault) = aterm_types::dev_seam!("ATERM_LINK_NOTIFY_FAULT") else {
            return Fault::None;
        };
        if store.metadata("fault-fired").is_ok() {
            return Fault::None;
        }
        match fault.to_str() {
            Some("kill-after-exec") => Fault::KillAfterExec,
            _ => Fault::None,
        }
    }

    fn fire(self, store: &Store) -> ! {
        let _ = write_unflushed_private(store, "fault-fired", b"1\n");
        eprintln!("aterm-link notify: ATERM_LINK_NOTIFY_FAULT={self:?}: dying");
        aterm_uds::process::kill_self_now()
    }
}

/// Replace a named store entry with owner-only bytes, without a flush: the dev
/// seams' files, among which no power loss falls. The bench reads the exec stamps and the flush
/// tally (which a flush would add to) while this notifier runs; `fault-fired` is read after
/// [`Fault::fire`] has killed it, by a later notifier's [`Fault::from_env`] and by the test.
fn write_unflushed_private(store: &Store, name: &str, bytes: &[u8]) -> io::Result<()> {
    let (parent, leaf) = store.directory.parent(name, true)?;
    let mut temporary = parent.temporary(
        leaf.to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 notify seam"))?,
        false,
    )?;
    let TemporaryData::File(file) = &mut temporary.data else {
        return Err(io::Error::other("notify seam temporary has wrong kind"));
    };
    file.write_all(bytes)?;
    temporary.publish(&parent, &leaf)
}

/// Everything the verb was told.
#[derive(Debug, Clone)]
pub struct Config {
    pub fleet: String,
    pub broker: String,
    pub cap_files: Vec<String>,
    pub state_dir: String,
    pub on: Vec<Sel>,
    pub exec: String,
    pub limit: u32,
    pub window_ms: u64,
    pub since: Since,
    pub once: bool,
    pub exec_timeout: Duration,
}

/// What one run did, for the closing line and for the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub fired: u64,
    pub dup: u64,
    pub skipped: u64,
    pub dropped: u64,
    pub standing: u64,
}

struct Notifier {
    cfg: Config,
    /// The root store's lock, held for the notifier's life ([`Store::open`]) and shared with its
    /// readers: it is the one writer of every store under it while the store's path names it
    /// ([`Hold::check`]).
    hold: std::sync::Arc<Hold>,
    store: Store,
    /// The store of the log this notifier is reading NOW ([`Store::for_log`]).
    log_store: Store,
    log_now: Option<astream_broker::LogIdentity>,
    /// The log each follow-mode reader's connection last reached ([`Notice::Log`]).
    reader_log: Vec<ReadLog>,
    /// The verdicts of the log in hand, oldest first ([`journal::note`]).
    journal: std::collections::VecDeque<String>,
    /// The root store's journal: the stamps and the suppressed count, and the root log's
    /// verdicts ([`journal`]).
    root_file: StepsFile,
    /// The journal of the log in hand's own store, where that is not the root's.
    log_file: Option<StepsFile>,
    /// The verdicts noted and not yet written, oldest first ([`Part::Journal`]).
    owed_verdicts: Vec<String>,
    /// The stamps spent and not yet written ([`Part::Stamps`]).
    owed_spent: Vec<u64>,
    /// The log in hand's standing values, the oldest set first ([`journal::Verdicts::standing`]).
    standing: Vec<(String, String)>,
    /// The changes of [`Self::standing`] not yet written, in order ([`Part::Standing`]).
    owed_stand: Vec<journal::Stand>,
    stamps: Vec<u64>,
    suppressed: u64,
    /// The `t=` of the newest `dropped` line [`Self::suppressed`] counts ([`Store::suppressed`]).
    suppressed_mark: u64,
    /// Whether [`Notifier::restore`] is still to run on [`Self::log_store`]: the log in hand
    /// was taken up (opened, or scoped to another store) and no record of it judged yet.
    restore_due: bool,
    fault: Fault,
    counts: Counts,
    wait_durable: bool,
    /// By selector index, the records held for durability, in read order ([`Notifier::take`]).
    held: Vec<std::collections::VecDeque<Held>>,
    /// The bytes [`Self::held`] holds, against [`HOLD_MAX`].
    held_bytes: usize,
    /// By selector index, whether the durable wait was said for the log its reader reads now:
    /// once per selector per log (fix unit E5, E2E-15), again only for another log or another
    /// `ordered` ([`Notice::Log`]).
    durable_said: Vec<bool>,
    /// By selector index, while a streak of holds past [`HOLD_MAX`] lasts, the newest record
    /// it paged before the broker said it durable. The hold is said once per streak, which
    /// ends once the broker's durable head passes that record, or with another log or another
    /// `ordered` ([`Notice::Log`]), however the held queue emptied (fix unit E5, E2E-15).
    over_paged: Vec<Option<u64>>,
    scanned: Vec<Option<u64>>,
    /// By selector index, the durable head the reader's current connection last told
    /// ([`Notice::Durable`]).
    durable: Vec<u64>,
    /// The sessions a `need` selector follows.
    need: NeedWatch,
    /// Whether [`Self::need`] takes a warm-up's records: it is fresh (just made, or made again
    /// for another log) and its reader has handed it no record past the cursor yet, judged or
    /// held for durability ([`Notifier::take`]): a reconnect's warm-up would fold a held record
    /// before it is judged.
    need_fresh: bool,
    /// The flush that puts a page's `firing` line on the media before its command runs:
    /// [`Flush::Durable`] (a test's negative control weakens it).
    firing: Flush,
    /// The records a following selector's cursor may trail by ([`CURSOR_LAG`]).
    cursor_lag: u64,
    /// How long a following selector's cursor may trail ([`CURSOR_EVERY`]).
    cursor_every: Duration,
    /// By selector key, the cursor of [`Self::log_store`] owed and not yet written.
    lag: BTreeMap<String, Lag>,
    /// The parts changed in memory and not yet written ([`Notifier::save`]).
    dirty: std::collections::BTreeSet<Part>,
    /// By selector key, the record whose change of a standing value is not yet written: one at
    /// most, since no record is judged while a part is unwritten ([`Notifier::owing`]); the
    /// key's cursor is written at it in the values' step ([`Notifier::put_standing`]).
    standing_at: BTreeMap<String, u64>,
    /// What a refused write left to do, in order ([`Notifier::drain`]).
    left: std::collections::VecDeque<Left>,
    /// Whether [`Self::left`]'s first item stalled: it is done again only once the retry falls
    /// due, and a follower takes no notice before it is done.
    stalled: bool,
    /// While a write the state dir refused is owed ([`Notifier::fail`]).
    failing: Option<Failing>,
    /// How long it waits after a refused write ([`RETRY_EVERY`]).
    retry_every: Duration,
    /// A test's clock ([`Notifier::now`]).
    #[cfg(test)]
    clock: Option<Instant>,
    /// A test's mutant: each change of the standing values replaces the line-format `standing`
    /// file whole, the write a build before the steps made ([`Notifier::put_standing`]).
    #[cfg(test)]
    standing_whole: bool,
    /// A test's mutant: a change of the standing values and the cursor it orders are written as
    /// two steps, the change's first, in one append ([`Notifier::put_standing`]).
    #[cfg(test)]
    standing_split: bool,
    /// A test's mutant: each cursor is written as a file of its own, `cur/<key>` replaced and
    /// its directory ordered, ahead of the standing change it orders, as a build before the
    /// cursor steps wrote it ([`Notifier::put_cursor`], [`Notifier::put_standing`]).
    #[cfg(test)]
    cursor_file: bool,
    /// Where [`FLUSH_TALLY_SEAM`] has the flushes told, and the counts last told there: the
    /// flushes by kind, then the bytes.
    flush_tally: Option<(u64, u64, u64)>,
    /// Where [`EXEC_STAMPS_SEAM`] has each fresh page's stamps told.
    exec_stamps: Option<ExecStamps>,
}

/// [`EXEC_STAMPS_SEAM`]'s file, the instant its stamps count from, and the last fresh page's
/// offset and stamps (`firing` written, the command's wait ended, `fired` written) not yet told
/// there.
struct ExecStamps {
    origin: Instant,
    untold: Option<(u64, [Instant; 3])>,
}

impl Notifier {
    fn new(cfg: Config) -> io::Result<Self> {
        let store = Store::open(&cfg.state_dir)?;
        let hold = Arc::clone(&store.access.0);
        let fault = Fault::from_env(&store);
        let flush_tally =
            aterm_types::dev_seam!(FLUSH_TALLY_SEAM).map(|_| (u64::MAX, u64::MAX, u64::MAX));
        let exec_stamps = aterm_types::dev_seam!(EXEC_STAMPS_SEAM).map(|_| ExecStamps {
            origin: Instant::now(),
            untold: None,
        });
        let n = cfg.on.len();
        let need = NeedWatch::new(&cfg.fleet);
        let root_file = store.load()?;
        let mut held = root_file.held.clone();
        held.keep_stamps(cfg.limit as usize);
        let (suppressed, suppressed_mark) = held.suppressed;
        Ok(Self {
            hold,
            journal: held.lines,
            root_file,
            log_file: None,
            owed_verdicts: Vec::new(),
            owed_spent: Vec::new(),
            standing: held.standing,
            owed_stand: Vec::new(),
            stamps: held.stamps,
            suppressed,
            suppressed_mark,
            restore_due: true,
            log_store: store.clone(),
            log_now: None,
            reader_log: vec![ReadLog::default(); n],
            store,
            cfg,
            fault,
            counts: Counts::default(),
            wait_durable: aterm_types::dev_seam!("ATERM_LINK_NOTIFY_PAGE_EARLY").is_none(),
            held: (0..n).map(|_| std::collections::VecDeque::new()).collect(),
            held_bytes: 0,
            durable_said: vec![false; n],
            over_paged: vec![None; n],
            scanned: vec![None; n],
            durable: vec![0; n],
            need,
            need_fresh: true,
            firing: Flush::Durable,
            cursor_lag: CURSOR_LAG,
            cursor_every: CURSOR_EVERY,
            lag: BTreeMap::new(),
            dirty: std::collections::BTreeSet::new(),
            standing_at: BTreeMap::new(),
            left: std::collections::VecDeque::new(),
            stalled: false,
            failing: None,
            retry_every: RETRY_EVERY,
            #[cfg(test)]
            clock: None,
            #[cfg(test)]
            standing_whole: false,
            #[cfg(test)]
            standing_split: false,
            #[cfg(test)]
            cursor_file: false,
            flush_tally,
            exec_stamps,
        })
    }

    /// Now, by [`Self::clock`] where a test set one.
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.clock {
            return now;
        }
        Instant::now()
    }

    /// Tell the development seams on what the last notice or tick did: the flushes
    /// ([`Self::tell_flushes`]) and the last fresh page's stamps ([`Self::tell_exec`]). Off the
    /// page's path: its `fired` line and its cursor are written by then.
    fn tell(&mut self) {
        self.tell_flushes();
        self.tell_exec();
    }

    /// Rewrite the [`EXEC_STAMPS`] where its seam asked for it and a fresh page ran since.
    fn tell_exec(&mut self) {
        let Some(stamps) = self.exec_stamps.as_mut() else {
            return;
        };
        let Some((off, at)) = stamps.untold.take() else {
            return;
        };
        let us = |t: Instant| {
            u64::try_from(t.saturating_duration_since(stamps.origin).as_micros())
                .unwrap_or(u64::MAX)
        };
        let line = format!(
            "notify-exec v=1 pid={} off={off} armed={} exited={} fired={}\n",
            std::process::id(),
            us(at[0]),
            us(at[1]),
            us(at[2])
        );
        if let Err(e) = write_unflushed_private(&self.store, EXEC_STAMPS, line.as_bytes()) {
            warn(&format!("{EXEC_STAMPS_SEAM}: {e}"));
        }
    }

    /// Rewrite the [`FLUSH_TALLY`] where its seam asked for it and the counts moved.
    fn tell_flushes(&mut self) {
        let Some(told) = self.flush_tally.as_mut() else {
            return;
        };
        let (durable, ordered) = flushes();
        let now = (durable, ordered, written_bytes());
        if *told == now {
            return;
        }
        *told = now;
        let tally = format!(
            "notify-flushes v=1 pid={} durable={durable} ordered={ordered} bytes={}\n",
            std::process::id(),
            now.2
        );
        if let Err(e) = write_unflushed_private(&self.store, FLUSH_TALLY, tally.as_bytes()) {
            warn(&format!("{FLUSH_TALLY_SEAM}: {e}"));
        }
    }

    /// One broker connection with every capability attached ([`ack_deadline`]).
    fn connect(&self) -> io::Result<Conn> {
        let (mut conn, _closer) = transport::connect(&self.cfg.broker, ack_deadline())?;
        transport::attach_cap_files(&mut conn, &self.cfg.cap_files)?;
        Ok(conn)
    }

    /// Read and write cursors and verdicts as the log `log`'s from here on ([`Store::for_log`]),
    /// the cursors owed the log in hand written to its store first ([`Notifier::persist`]).
    fn scope_to(&mut self, read: &ReadLog) -> io::Result<()> {
        let log = read.id;
        if log == self.log_now {
            return Ok(());
        }
        self.held()?;
        self.persist()?;
        let scoped = self.store.for_log(log, read.lineage.as_ref())?;
        if scoped.root != self.log_store.root {
            eprintln!(
                "aterm-link notify: the broker serves {} — its cursors and journal are {}",
                log.map_or_else(
                    || "a log with no identity".to_string(),
                    |l| format!("log {l}")
                ),
                scoped.root.display()
            );
            self.log_file = if scoped.root == self.store.root {
                None
            } else {
                Some(scoped.load()?)
            };
            let held = &self.log_file.as_ref().unwrap_or(&self.root_file).held;
            self.journal = held.lines.clone();
            self.standing = held.standing.clone();
            self.restore_due = true;
        }
        self.log_store = scoped;
        self.log_now = log;
        Ok(())
    }

    fn preflight(&self) -> Result<(), String> {
        for path in &self.cfg.cap_files {
            crate::bridge::read_cap_file(path).map_err(|e| format!("--cap-file {path}: {e}"))?;
        }
        let permanent = |e: &io::Error| {
            astream_broker::broker_refusal(e)
                .is_some_and(astream_broker::BrokerRefusal::is_permanent)
        };
        let Ok((mut conn, _closer)) = transport::connect(&self.cfg.broker, ack_deadline()) else {
            return Ok(());
        };
        match transport::attach_cap_files(&mut conn, &self.cfg.cap_files) {
            Ok(()) => {}
            Err(e) if permanent(&e) => {
                return Err(format!(
                    "the broker refused the cap files ({}): {e}",
                    self.cfg.cap_files.join(" ")
                ));
            }
            Err(_) => return Ok(()),
        }
        for sel in &self.cfg.on {
            let filter = sel.filter(&self.cfg.fleet);
            match conn.fetch(0, &filter, 0) {
                Ok(_) => {}
                Err(e) if permanent(&e) => {
                    return Err(format!(
                        "--on {} reads {filter}, and the broker refused it ({e}): no --cap-file \
                         grants it. It needs {}",
                        sel.label(),
                        sel.grant(&self.cfg.fleet)
                    ));
                }
                Err(_) => return Ok(()),
            }
        }
        Ok(())
    }

    fn verdict(&self, off: u64) -> Option<&str> {
        self.journal.iter().find_map(|line| {
            let mut it = line.split_whitespace();
            let at: u64 = crate::parse_uint(it.next()?)?;
            (at == off).then(|| it.next()).flatten()
        })
    }

    fn told(&self, off: u64) -> bool {
        self.journal.iter().any(|line| {
            let mut it = line.split_whitespace();
            it.next().and_then(crate::parse_uint::<u64>) == Some(off)
                && it.next() == Some("fired")
                && it.any(|t| t == "rc=0")
        })
    }

    fn standing_of(&self, subject: &str) -> Option<&str> {
        self.standing
            .iter()
            .find(|(s, _)| s == subject)
            .map(|(_, t)| t.as_str())
    }

    /// The entries on the `ev` face `face` ([`Entry::token`]s, comma-joined, in its standing
    /// value): each need a `need` selector paged there, or found standing when its warm-up
    /// ended, that the face's fold still holds, and each whose page is owed. A store written
    /// before the pairs holds one word, read as a pair whose raise is unknown ([`Pair::same`]).
    fn entries_of(&self, face: &str) -> Vec<Entry> {
        self.standing_of(face)
            .map(|t| t.split(',').filter_map(Entry::parse).collect())
            .unwrap_or_default()
    }

    /// The pairs standing on the `ev` face `face`: its entries whose page is not owed.
    #[cfg(test)]
    fn pairs_of(&self, face: &str) -> Vec<Pair> {
        self.entries_of(face)
            .into_iter()
            .filter(|e| !e.owed)
            .map(|e| e.pair)
            .collect()
    }

    /// Make `entries` the ones on `face`, within [`PAIRS_KEEP`] ([`bounded`], keeping every one
    /// the same need as one of `names`, the pairs the face's fold names), through
    /// [`Self::set_standing`] (none: the face's value cleared).
    fn set_entries(
        &mut self,
        key: &str,
        off: u64,
        face: &str,
        entries: Vec<Entry>,
        names: &[Pair],
    ) {
        let token: Vec<String> = bounded(entries, names).iter().map(Entry::token).collect();
        let token = token.join(",");
        self.set_standing(
            key,
            off,
            face,
            (!token.is_empty()).then_some(token.as_str()),
        );
    }

    /// Leave what a page raised standing: `attention`'s token, or a `need`'s pair beside the
    /// others on its face, in place of the same need owed.
    fn stand(&mut self, key: &str, off: u64, subject: &str, stand: &Stand) {
        match stand {
            Stand::Token(token) => self.set_standing(key, off, subject, Some(token)),
            Stand::Need { face, pair, names } => {
                let mut entries = self.entries_of(face);
                let before = entries.clone();
                entries.retain(|e| !(e.owed && e.pair.same(pair)));
                if !entries.iter().any(|e| e.pair.same(pair)) {
                    entries.push(Entry {
                        pair: pair.clone(),
                        owed: false,
                    });
                }
                if entries != before {
                    self.set_entries(key, off, face, entries, names);
                }
            }
        }
    }

    /// Leave a `need` page that did not end `rc=0` owed ([`Entry`]): no warm-up stands it, so
    /// the face's next record pages it again, in this run or after a restart, as `attention`
    /// pages a row again whose command failed. `attention`'s token needs nothing: only a page
    /// that ended `rc=0` sets it.
    fn owe(&mut self, key: &str, off: u64, stand: &Stand) {
        let Stand::Need { face, pair, names } = stand else {
            return;
        };
        let mut entries = self.entries_of(face);
        if entries.iter().any(|e| e.pair.same(pair)) {
            return;
        }
        entries.push(Entry {
            pair: pair.clone(),
            owed: true,
        });
        self.set_entries(key, off, face, entries, names);
    }

    /// The warm-up's end (`at` the selector's cursor, which the warm-up read below): every pair
    /// the watch's folds name stands where no entry is the same need, so a need raised below
    /// the cursor never pages, one raised at or after it is a rise, and one whose page is owed
    /// stays owed.
    fn seed_standing(&mut self, key: &str, at: u64) {
        for (face, pairs) in self.need.standing() {
            let mut have = self.entries_of(&face);
            let before = have.len();
            for pair in &pairs {
                if !have.iter().any(|e| e.pair.same(pair)) {
                    have.push(Entry {
                        pair: pair.clone(),
                        owed: false,
                    });
                }
            }
            if have.len() != before {
                self.set_entries(key, at, &face, have, &pairs);
            }
        }
    }

    /// Make `token` the escalation standing on `subject` ([`STANDING_KEEP`]), as the record at
    /// `off` that selector `key` is judging says. A change is written at once, ordered, in one
    /// step with `key`'s cursor AT `off` ([`Notifier::put_standing`]): a power loss keeps both
    /// or neither, so the selector re-reads no record but `off` under a standing value that a
    /// later record set, and a value a record left standing never pages it again. Until the
    /// change is written (the state dir refused it, [`Notifier::save`]), no cursor is written
    /// ([`Notifier::checkpoint`]) and no later record is judged ([`Notifier::owing`]), so a
    /// second change never waits behind it: one cursor write could not keep the law for both.
    fn set_standing(&mut self, key: &str, off: u64, subject: &str, token: Option<&str>) {
        if self.standing_of(subject) == token {
            return;
        }
        let token = token.map(str::to_string);
        journal::note_stand(&mut self.standing, subject.to_string(), token.clone());
        self.owe_stand(subject.to_string(), token);
        while self.standing.len() > STANDING_KEEP {
            let at = self
                .standing
                .iter()
                .position(|(s, _)| !self.need.holds(s))
                .unwrap_or(0);
            let (gone, _) = self.standing.remove(at);
            self.owe_stand(gone, None);
        }
        self.standing_at.entry(key.to_string()).or_insert(off);
        self.save(&[Part::Standing]);
    }

    /// Owe the journal one change of the standing values ([`Notifier::put_standing`]): a step
    /// carries any subject and value but an empty one ([`journal::Step`]), so the journal never
    /// holds an older value than the notifier. An empty value is owed as a clear, and an empty
    /// subject (no bus subject is one) owes nothing, since no step ever set one.
    fn owe_stand(&mut self, subject: String, token: Option<String>) {
        if !subject.is_empty() {
            self.owed_stand
                .push((subject, token.filter(|t| !t.is_empty())));
        }
    }

    /// Note one verdict in the journal, replacing the offset's old one, keeping the newest
    /// [`JOURNAL_KEEP`]: the verdict is the journal's newest line ([`journal::note`],
    /// [`Notifier::restore`]).
    fn note_verdict(&mut self, off: u64, line: String) {
        debug_assert_eq!(journal::offset_of(&line), Some(off));
        journal::note(&mut self.journal, line);
    }

    /// Note one verdict ([`Notifier::note_verdict`]) as owed to the journal, unwritten.
    fn owe_verdict(&mut self, off: u64, line: String) {
        self.note_verdict(off, line.clone());
        self.owed_verdicts.push(line);
    }

    /// Write one verdict ([`Notifier::note_verdict`]): one step appended to the journal, ordered
    /// ([`Notifier::save`]), as a page's `fired` line is ([`Notifier::fire`]); a `firing` line
    /// goes through [`Notifier::arm`], and a `dropped` line with its count.
    #[cfg(test)]
    fn record(&mut self, off: u64, line: String) {
        self.owe_verdict(off, line);
        self.save(&[Part::Journal]);
    }

    fn has_room(&mut self, now: u64) -> bool {
        let window = self.cfg.window_ms;
        self.stamps.retain(|t| now.saturating_sub(*t) < window);
        u64::from(self.cfg.limit) > self.stamps.len() as u64
    }

    /// The stamp a page spends at `now`: `now`, or one past the newest stamp held where that
    /// is later, so no two stamps are equal and a `firing` line's `t=` names its own
    /// ([`Notifier::restore`]).
    fn stamp(&self, now: u64) -> u64 {
        self.stamps
            .iter()
            .max()
            .map_or(now, |&newest| now.max(newest.saturating_add(1)))
    }

    /// Spend `t`, a page's stamp ([`Notifier::stamp`]), once however often it is asked, keeping
    /// the newest `limit` stamps, owed to the journal ([`Part::Stamps`]).
    fn spend(&mut self, t: u64) {
        if !self.stamps.contains(&t) {
            self.stamps.push(t);
            self.trim_stamps();
        }
        self.owed_spent.push(t);
    }

    /// [`Notifier::spend`] `t`, and write it at once, ordered.
    #[cfg(test)]
    fn charge(&mut self, t: u64) -> io::Result<()> {
        self.spend(t);
        self.dirty.insert(Part::Stamps);
        self.rewrite(self.now())
    }

    /// Keep the newest `limit` stamps.
    fn trim_stamps(&mut self) {
        let over = self.stamps.len().saturating_sub(self.cfg.limit as usize);
        self.stamps.drain(..over);
    }

    /// Count one drop whose `dropped` line, `t=mark`, is noted: the count goes in the line's
    /// step, or, in a log's own store, is written after the line ([`Part`]), so a kill between
    /// the two leaves the line uncounted, and [`Notifier::restore`] counts it.
    fn count_drop(&mut self, mark: u64) {
        self.suppressed += 1;
        self.suppressed_mark = mark;
    }

    /// Finish what a kill cut short after the newest verdict of the log in hand, before its
    /// first record is judged: a verdict is written with what it spends or counts, in one step
    /// where the log's store is the root's, and ahead of it in a log's own store, so only that
    /// one can be ahead of it. A `firing` line whose `t=` the stamps do not hold spends it; a
    /// `dropped` line whose `t=` is past the newest drop the suppressed count holds is counted
    /// ([`Notifier::count_drop`]). Otherwise it writes nothing.
    fn restore(&mut self) {
        let Some(newest) = self.journal.back() else {
            return;
        };
        let mut words = newest.split_whitespace();
        let verdict = words.nth(1);
        let Some(t) = words
            .find_map(|w| w.strip_prefix("t="))
            .and_then(crate::parse_uint::<u64>)
        else {
            return;
        };
        match verdict {
            Some("firing") if !self.stamps.contains(&t) => {
                let at = self.stamps.partition_point(|&s| s < t);
                self.stamps.insert(at, t);
                self.trim_stamps();
                self.owed_spent.push(t);
                self.save(&[Part::Stamps]);
            }
            Some("dropped") if t > self.suppressed_mark => {
                self.count_drop(t);
                self.save(&[Part::Suppressed]);
            }
            _ => {}
        }
    }

    /// Mark `parts` changed in memory, and write what is owed at once ([`Notifier::rewrite`]),
    /// unless the state dir refused a write and its retry is not due: then the next settle
    /// past it writes it ([`Notifier::settle`]).
    fn save(&mut self, parts: &[Part]) {
        self.dirty.extend(parts.iter().copied());
        let now = self.now();
        if self.writable(now) {
            let _ = self.rewrite(now);
        }
    }

    /// Write what is owed in [`Part`]'s order ([`Notifier::put_steps`], then a standing value
    /// through [`Notifier::put_standing`]), stopping at the first the state dir refuses
    /// ([`Notifier::fail`]), so no part reaches the store ahead of one before it.
    fn rewrite(&mut self, now: Instant) -> io::Result<()> {
        if let Err((part, e)) = self.put_steps(Flush::Ordered) {
            self.fail(&e, now, part.what());
            return Err(e);
        }
        if self.dirty.contains(&Part::Standing) {
            if let Err(e) = self.put_standing() {
                self.fail(&e, now, Part::Standing.what());
                return Err(e);
            }
            self.dirty.remove(&Part::Standing);
        }
        Ok(())
    }

    /// Write the verdicts, stamps and suppressed count owed, the last write flushed by `last`.
    /// Where the log in hand's store is the root's, they are one journal: each verdict a step,
    /// what is spent and counted in the last (a step of its own with no verdict), one append.
    /// Otherwise the verdicts go to the log's own journal first, ordered ahead of the root's
    /// (in full where that is on another device), and what is spent and counted to the root's
    /// after them. Err names the first part not written; what is written is no longer owed.
    fn put_steps(&mut self, last: Flush) -> Result<(), (Part, io::Error)> {
        let parts = [Part::Journal, Part::Stamps, Part::Suppressed];
        if !parts.iter().any(|part| self.dirty.contains(part)) {
            return Ok(());
        }
        let mut verdicts: Vec<journal::Step> = self
            .owed_verdicts
            .iter()
            .map(|line| journal::Step {
                verdict: Some(line.clone()),
                ..journal::Step::default()
            })
            .collect();
        let state = journal::Step {
            verdict: None,
            spent: self.owed_spent.clone(),
            suppressed: self
                .dirty
                .contains(&Part::Suppressed)
                .then_some((self.suppressed, self.suppressed_mark)),
            ..journal::Step::default()
        };
        let failed = |dirty: &std::collections::BTreeSet<Part>, among: &[Part]| {
            among
                .iter()
                .copied()
                .find(|part| dirty.contains(part))
                .unwrap_or(Part::Journal)
        };
        if let Err(e) = self.held() {
            return Err((failed(&self.dirty, &parts), e));
        }
        match self.log_file.as_mut() {
            None => {
                if !state.is_empty() {
                    match verdicts.last_mut() {
                        Some(step) => {
                            step.spent = state.spent;
                            step.suppressed = state.suppressed;
                        }
                        None => verdicts.push(state),
                    }
                }
                if let Err(e) = self.root_file.write(&verdicts, last, Some(&self.stamps)) {
                    return Err((failed(&self.dirty, &parts), e));
                }
            }
            Some(log) => {
                let first = if log.same_device(&self.root_file) {
                    Flush::Ordered
                } else {
                    Flush::Durable
                };
                if let Err(e) = log.write(&verdicts, first, None) {
                    return Err((Part::Journal, e));
                }
                self.owed_verdicts.clear();
                self.dirty.remove(&Part::Journal);
                if !state.is_empty() {
                    if let Err(e) = self.root_file.write(&[state], last, Some(&self.stamps)) {
                        return Err((failed(&self.dirty, &parts), e));
                    }
                }
            }
        }
        self.owed_verdicts.clear();
        self.owed_spent.clear();
        for part in parts {
            self.dirty.remove(&part);
        }
        Ok(())
    }

    /// Write the changes of the standing values owed, with each selector's cursor at the record
    /// that changed one ([`Notifier::set_standing`]), in one step appended to the log in hand's
    /// journal, ordered, forgetting the cursor each owed up to there: a step reads whole or not
    /// at all, so no standing value reaches the media ahead of the cursor it orders, and a
    /// flood of changes writes each change's own step, never every value standing. A cursor
    /// the journal already holds at or past its record is not written again. A rewrite of the
    /// root's journal carries the stamps the notifier holds.
    fn put_standing(&mut self) -> io::Result<()> {
        self.held()?;
        let at: Vec<(String, u64)> = self
            .standing_at
            .iter()
            .map(|(key, &at)| (key.clone(), at))
            .collect();
        let cursor: Vec<(String, u64)> = at
            .iter()
            .filter(|(key, at)| self.cursor(key).is_none_or(|have| have < *at))
            .cloned()
            .collect();
        #[cfg(test)]
        let cursor = if self.cursor_file {
            for (key, at) in cursor {
                self.log_store.write(
                    &format!("{}/{key}", journal::CURSORS),
                    &at.to_string(),
                    Flush::Ordered,
                )?;
            }
            Vec::new()
        } else {
            cursor
        };
        for (key, _) in &cursor {
            test_fault(&format!("{}/{key}", journal::CURSORS))?;
        }
        #[cfg(test)]
        if self.standing_whole {
            let lines: Vec<String> = self
                .standing
                .iter()
                .map(|(s, t)| format!("{s} {t}"))
                .collect();
            self.log_store
                .write(journal::STANDING, &lines.join("\n"), Flush::Ordered)?;
            self.owed_stand.clear();
        }
        if !self.owed_stand.is_empty() {
            test_fault(journal::STANDING)?;
        }
        let step = journal::Step {
            stand: self.owed_stand.clone(),
            cursor,
            ..journal::Step::default()
        };
        #[cfg(test)]
        let step = if self.standing_split && !step.stand.is_empty() && !step.cursor.is_empty() {
            let cursor = journal::Step {
                cursor: step.cursor.clone(),
                ..journal::Step::default()
            };
            self.write_log(
                &[
                    journal::Step {
                        stand: step.stand,
                        ..journal::Step::default()
                    },
                    cursor,
                ],
                Flush::Ordered,
            )?;
            journal::Step::default()
        } else {
            step
        };
        if !step.is_empty() {
            self.write_log(&[step], Flush::Ordered)?;
        }
        self.owed_stand.clear();
        for (key, at) in at {
            if self.lag.get(&key).is_some_and(|lag| lag.want <= at) {
                self.lag.remove(&key);
            }
        }
        self.standing_at.clear();
        Ok(())
    }

    /// THE STORE BY ITS PATH: before each write to a store (a part owed, a standing change, a
    /// cursor, another log's store taken up), the root store's path (`notify/`) still names the
    /// directory whose lock this notifier holds ([`Hold::check`]; `notify/lock` only names its
    /// pid). The write itself goes through that directory's held descriptor, so one that falls
    /// between a check and the store's removal reaches only the removed directory. Once the path
    /// names another directory or nothing, this one writes nothing more, to that store or to the
    /// descriptors of the one removed, and every write after answers the same refusal
    /// ([`Notifier::lost`]), which ends its run.
    ///
    /// # Errors
    /// As [`Hold::check`]'s.
    fn held(&self) -> io::Result<()> {
        self.hold.check()
    }

    /// The refusal [`Hold::check`] recorded, once the store's path no longer named its directory.
    fn lost(&self) -> Option<io::Error> {
        self.hold.lost()
    }

    /// The journal of the log in hand ([`Self::log_file`], or the root's where it reads the
    /// root's log), as this notifier holds it.
    fn log_journal(&self) -> &journal::StepsFile {
        self.log_file.as_ref().unwrap_or(&self.root_file)
    }

    /// Append `steps` to the log in hand's journal, flushed by `flush`; a rewrite of the root's
    /// carries the stamps the notifier holds ([`journal::StepsFile::write`]).
    fn write_log(&mut self, steps: &[journal::Step], flush: Flush) -> io::Result<()> {
        match self.log_file.as_mut() {
            Some(log) => log.write(steps, flush, None),
            None => self.root_file.write(steps, flush, Some(&self.stamps)),
        }
    }

    /// `key`'s cursor as the log in hand's journal holds it, the newest written: none before
    /// the first.
    fn cursor(&self, key: &str) -> Option<u64> {
        #[cfg(test)]
        if self.cursor_file {
            return self
                .log_store
                .read(&format!("{}/{key}", journal::CURSORS))
                .and_then(|s| crate::parse_uint(s.trim()));
        }
        self.log_journal().held.cursors.get(key).copied()
    }

    /// Move `key`'s cursor forward to `off` (never back): one step of the log in hand's journal,
    /// `cursor <key> <off>`, ordered, where it holds the key's cursor below `off` or none. A
    /// cursor a power loss takes re-reads records the journal and the standing values dedup.
    fn put_cursor(&mut self, key: &str, off: u64) -> io::Result<()> {
        if self.cursor(key).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.held()?;
        #[cfg(test)]
        if self.cursor_file {
            return self.log_store.write(
                &format!("{}/{key}", journal::CURSORS),
                &off.to_string(),
                Flush::Ordered,
            );
        }
        test_fault(&format!("{}/{key}", journal::CURSORS))?;
        self.write_log(
            &[journal::Step {
                cursor: vec![(key.to_string(), off)],
                ..journal::Step::default()
            }],
            Flush::Ordered,
        )
    }

    /// Whether to write what is owed now: no write was refused, or its retry is due.
    fn writable(&self, now: Instant) -> bool {
        self.failing.is_none_or(|f| now >= f.next)
    }

    /// Back off from a write the state dir refused (`what`, `e`): what is owed is written again
    /// [`Self::retry_every`] from `now` on, at the next record or tick past it, or at the retry
    /// already set where that is still to come (a refusal before it moves it no later). A
    /// warning names the error and its errno the first time and whenever it changes.
    fn fail(&mut self, e: &io::Error, now: Instant, what: &str) {
        if self.hold.lost.get().is_some() {
            return;
        }
        let (errno, kind) = (e.raw_os_error(), e.kind());
        let told = self
            .failing
            .is_some_and(|f| f.errno == errno && f.kind == kind);
        let next = match self.failing {
            Some(f) if f.next > now => f.next,
            _ => now.checked_add(self.retry_every).unwrap_or(now),
        };
        self.failing = Some(Failing { next, errno, kind });
        if !told {
            warn(&format!(
                "the state dir refused {what}: {e}; it goes on, holds what it owes, and writes it \
                 again from {} ms on (a page waits for its `firing` line)",
                self.retry_every.as_millis()
            ));
            #[cfg(test)]
            WARNED.with(|warned| warned.borrow_mut().push(Warned::Refused(errno)));
        }
    }

    /// After each notice and at each tick: once a refused write's retry is due, write the parts
    /// owed ([`Notifier::rewrite`]); then each cursor due ([`Notifier::persist_due`]); and say
    /// so once nothing a refused write left is owed any more.
    fn settle(&mut self, now: Instant) {
        if !self.dirty.is_empty() && self.writable(now) {
            let _ = self.rewrite(now);
        }
        self.persist_due(now);
        let owed = !self.dirty.is_empty()
            || !self.left.is_empty()
            || self.lag.values().any(|lag| lag.retry);
        if self.failing.is_some() && !owed {
            self.failing = None;
            warn("the state dir takes writes again: what it owed is written");
            #[cfg(test)]
            WARNED.with(|warned| warned.borrow_mut().push(Warned::Cleared));
        }
    }

    /// Whether a part a judged record changed is still unwritten ([`Self::dirty`]): then no
    /// further record is judged (it is left, [`Left`]) until the part is written, so a record's
    /// changes never wait behind another's. That keeps every law the store holds for one
    /// record: a standing value written in one step with its selector's cursor at its record
    /// ([`Notifier::put_standing`]), a drop's count trailing its `dropped` line by that line
    /// at most ([`Notifier::restore`]). Some, it is the refusal the record waits behind
    /// ([`Self::failing`]) and what that write held.
    fn owing(&self) -> Option<(io::Error, &'static str)> {
        let part = *self.dirty.first()?;
        let err = self.failing.map_or_else(
            || io::Error::from(io::ErrorKind::WouldBlock),
            |f| {
                f.errno
                    .map_or_else(|| io::Error::from(f.kind), io::Error::from_raw_os_error)
            },
        );
        Some((err, part.what()))
    }

    /// Write `key`'s cursor at `off`, and forget the cursor it owed up to there
    /// ([`Notifier::advance`]). While a part a judged record changed is unwritten
    /// ([`Self::dirty`]: a verdict, a stamp, the suppressed count or a standing value), or a
    /// refused write's retry is not due, or where the state dir refuses it, the cursor stays
    /// owed, and the next settle writes it ([`Lag::retry`]): no cursor reaches the store ahead
    /// of what the records it passes changed, so a restart re-reads each record whose change
    /// the store lacks.
    fn checkpoint(&mut self, key: &str, off: u64) {
        let now = self.now();
        let held = !self.dirty.is_empty();
        if !held && self.writable(now) {
            match self.put_cursor(key, off) {
                Ok(()) => {
                    if self.lag.get(key).is_some_and(|lag| lag.want <= off) {
                        self.lag.remove(key);
                    }
                    return;
                }
                Err(e) => self.fail(&e, now, "a cursor"),
            }
        }
        let lag = self.lag.entry(key.to_string()).or_insert(Lag {
            want: off,
            records: 0,
            since: now,
            retry: true,
        });
        lag.want = lag.want.max(off);
        lag.retry = true;
    }

    /// Owe `key`'s cursor `next`, one record judged, and write it once [`Self::cursor_lag`]
    /// records have passed since the first it owes ([`Notifier::persist_due`] writes it once
    /// [`Self::cursor_every`] has): a record that writes no verdict (it pages nothing and is not
    /// dropped) and moves no standing value writes nothing.
    fn advance(&mut self, key: &str, next: u64) {
        let now = self.now();
        let lag = self.lag.entry(key.to_string()).or_insert(Lag {
            want: next,
            records: 0,
            since: now,
            retry: false,
        });
        lag.want = lag.want.max(next);
        lag.records += 1;
        if lag.records >= self.cursor_lag && !lag.retry {
            let want = lag.want;
            self.checkpoint(key, want);
        }
    }

    /// Write each cursor owed since [`Self::cursor_every`] before `now` or longer, and each one
    /// left owed when due ([`Lag::retry`]), once a refused write's retry is due.
    fn persist_due(&mut self, now: Instant) {
        if !self.writable(now) {
            return;
        }
        let every = self.cursor_every;
        let due: Vec<(String, u64)> = self
            .lag
            .iter()
            .filter(|(_, lag)| lag.retry || now.saturating_duration_since(lag.since) >= every)
            .map(|(key, lag)| (key.clone(), lag.want))
            .collect();
        for (key, want) in due {
            self.checkpoint(&key, want);
        }
    }

    /// When the follower next has something to write by the clock: the oldest cursor owed
    /// falling due ([`Notifier::persist_due`]), or a refused write's retry
    /// ([`Notifier::settle`], [`Notifier::drain`]).
    fn next_due(&self) -> Option<Instant> {
        self.lag
            .values()
            .filter(|lag| !lag.retry)
            .filter_map(|lag| lag.since.checked_add(self.cursor_every))
            .chain(self.failing.map(|f| f.next))
            .min()
    }

    /// Write every part and cursor owed ([`Notifier::save`], [`Notifier::advance`]), whether
    /// or not a refused write's retry is due: Err at the first the state dir refuses, what is
    /// not written still owed.
    fn persist(&mut self) -> io::Result<()> {
        let now = self.now();
        self.rewrite(now)?;
        let owed: Vec<(String, u64)> = self
            .lag
            .iter()
            .map(|(key, lag)| (key.clone(), lag.want))
            .collect();
        for (key, want) in owed {
            if let Err(e) = self.put_cursor(&key, want) {
                self.fail(&e, now, "a cursor");
                if let Some(lag) = self.lag.get_mut(&key) {
                    lag.retry = true;
                }
                return Err(e);
            }
            self.lag.remove(&key);
        }
        Ok(())
    }

    /// The verdicts this run has written ([`Counts::fired`], [`Counts::dropped`]).
    const fn verdicts(&self) -> u64 {
        self.counts.fired + self.counts.dropped
    }

    /// What a `need` selector makes of one record. A whole fold keeps on the face the entries
    /// it still holds (a partial one keeps them all), an unknown raise read from an old store
    /// taking the fold's pair it matches; a session that exited without its close keeps none,
    /// unless a successor's restore says it died (its incarnation ended without withdrawing it,
    /// [`crate::fabric_brief::Died::Restored`]): the watch derives that close and judges the
    /// restore as the dead session's record, so its `exited` (detail `gone`) pages once for its
    /// space (fix unit E9); a node's will keeps them, so a reattach that restores the same needs
    /// pages nothing. The hit, and the pair it leaves standing once its command ran (or owed
    /// when it did not), is the most urgent need `sel` selects that rose where the watch saw it
    /// and is no pair standing (one a whole fold names, or one whose raise a partial fold holds
    /// with nothing lost after it), or that is the same need as an entry owed on the face and
    /// that the fold still sees ([`crate::notify_need::Named`]): an owed page runs again however
    /// the fold now knows the need's raise (a snapshot past it after a restart or a refold), and
    /// whatever older need of its word stands on the face (fix unit J0-3).
    fn need_hit(
        &mut self,
        sel: &Sel,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> Option<(Stand, Hit)> {
        self.need_fresh = false;
        let key = sel.key();
        let (face, node, sid, needs, whole) = match self.need.step(off, subject, body) {
            Step::Nothing => return None,
            Step::Gone(face) => {
                self.set_entries(&key, off, &face, Vec::new(), &[]);
                return None;
            }
            Step::Folded {
                face,
                node,
                sid,
                needs,
                whole,
            } => (face, node, sid, needs, whole),
        };
        let names: Vec<Pair> = needs.iter().map(|n| n.pair.clone()).collect();
        let have = self.entries_of(&face);
        let mut kept: Vec<Entry> = Vec::with_capacity(have.len());
        for e in &have {
            let now = needs.iter().find(|n| n.pair.same(&e.pair)).map(|n| &n.pair);
            let pair = match (now, e.pair.raised) {
                (Some(now), crate::notify_need::Raised::Unknown) => now.clone(),
                (None, _) if whole => continue,
                _ => e.pair.clone(),
            };
            match kept.iter_mut().find(|k| k.pair == pair) {
                Some(k) => k.owed &= e.owed,
                None => kept.push(Entry { pair, owed: e.owed }),
            }
        }
        if kept != have {
            self.set_entries(&key, off, &face, kept.clone(), &names);
        }
        let stands =
            |n: &crate::notify_need::Named| kept.iter().any(|e| !e.owed && n.pair.same(&e.pair));
        let owed =
            |n: &crate::notify_need::Named| kept.iter().any(|e| e.owed && n.pair.same(&e.pair));
        let due = |n: &&crate::notify_need::Named| (n.rose && !stands(n)) || (n.seen && owed(n));
        let selected = |n: &&crate::notify_need::Named| sel.selects(n.pair.word);
        let Some(rose) = needs.iter().filter(selected).find(due) else {
            if needs.iter().filter(selected).any(stands) {
                self.counts.standing += 1;
            }
            return None;
        };
        let word = rose.pair.word;
        let detail = rose.detail.as_deref().map(one_line).unwrap_or_default();
        let text = if detail.is_empty() {
            word.to_string()
        } else {
            one_line(&format!("{word} {detail}"))
        };
        let hit = Hit {
            on: sel.label(),
            offset: off,
            subject: subject_env(&face),
            node: one_line(&node),
            who: String::new(),
            sid: one_line(&sid),
            text,
            need: word.to_string(),
            detail,
        };
        let pair = rose.pair.clone();
        Some((Stand::Need { face, pair, names }, hit))
    }

    /// Judge one record, and page it where it hits ([`Notifier::fire`]). A hit over the budget
    /// writes its `dropped` line (its `t=`) and then the suppressed count, ordered. A record
    /// that neither pages, nor is dropped, nor moves a standing value writes nothing. The first
    /// record judged in a log taken up finishes what a kill left of its newest verdict
    /// ([`Notifier::restore`]). A write the state dir refuses leaves what it held owed
    /// ([`Notifier::save`]), except a page's `firing` line: Err, the page left to run later
    /// ([`Left::Page`]), its command not run.
    fn on_record(
        &mut self,
        sel: &Sel,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> Result<(), Stalled> {
        if std::mem::take(&mut self.restore_due) {
            self.restore();
        }
        let key = sel.key();
        let (raised, hit) = match sel {
            Sel::Attention => {
                let raised = match escalation(&self.cfg.fleet, off, subject, body) {
                    None => return Ok(()),
                    Some(None) => {
                        self.set_standing(&key, off, subject, None);
                        return Ok(());
                    }
                    Some(Some(token)) => {
                        if self.standing_of(subject).is_some_and(|t| t != token) {
                            self.set_standing(&key, off, subject, None);
                        }
                        Some(Stand::Token(token))
                    }
                };
                (raised, sel.hit(&self.cfg.fleet, off, subject, body))
            }
            Sel::Need(_) => match self.need_hit(sel, off, subject, body) {
                Some((stand, hit)) => (Some(stand), Some(hit)),
                None => return Ok(()),
            },
            Sel::Ask(_) | Sel::Halt => (None, sel.hit(&self.cfg.fleet, off, subject, body)),
        };
        let Some(hit) = hit else {
            return Ok(());
        };
        let firing = match self.verdict(off) {
            Some("fired" | "dropped") => {
                match raised.as_ref() {
                    Some(stand) if self.told(off) => self.stand(&key, off, subject, stand),
                    Some(stand) => self.owe(&key, off, stand),
                    None => {}
                }
                self.counts.skipped += 1;
                return Ok(());
            }
            Some(_) => None,
            None if matches!(&raised, Some(Stand::Token(t)) if self.standing_of(subject) == Some(t)) =>
            {
                self.counts.standing += 1;
                return Ok(());
            }
            None => {
                let now = crate::now_ms();
                if !self.has_room(now) {
                    let mark = now.max(self.suppressed_mark.saturating_add(1));
                    self.owe_verdict(
                        off,
                        format!(
                            "{off} dropped on={} reason=rate limit={}/{}ms suppressed={} t={mark}",
                            hit.on,
                            self.cfg.limit,
                            self.cfg.window_ms,
                            self.suppressed + 1
                        ),
                    );
                    self.count_drop(mark);
                    self.save(&[Part::Journal, Part::Suppressed]);
                    if let Some(stand) = &raised {
                        self.owe(&key, off, stand);
                    }
                    eprintln!(
                        "aterm-link notify: DROPPED off={off} on={} reason=rate limit={}/{}ms \
                         suppressed={}; the record stays on the bus",
                        hit.on, self.cfg.limit, self.cfg.window_ms, self.suppressed
                    );
                    self.counts.dropped += 1;
                    return Ok(());
                }
                let t = self.stamp(now);
                Some((format!("{off} firing on={} t={t}", hit.on), t))
            }
        };
        self.fire(Page {
            sel: sel.clone(),
            off,
            subject: subject.to_string(),
            raised,
            hit,
            firing,
            then: None,
        })
    }

    /// Run a judged page: a fresh one's `firing` line and stamp on the media first
    /// ([`Notifier::arm`]), which, refused, leaves the page to run later, its command not run
    /// (Err, [`Left::Page`]); then its command, and after it the `fired` line, the suppressed
    /// count it told and the value it leaves standing, ordered ([`Notifier::save`]): losing one
    /// costs a repeat.
    fn fire(&mut self, page: Page) -> Result<(), Stalled> {
        if let Some((line, t)) = &page.firing {
            if let Err(err) = self.arm(page.off, line.clone(), *t) {
                return Err(Stalled {
                    left: Left::Page(Box::new(page)),
                    err,
                    what: "a page's `firing` line",
                });
            }
        }
        let Page {
            sel,
            off,
            subject,
            raised,
            hit,
            firing,
            ..
        } = page;
        let (key, dup) = (sel.key(), firing.is_none());
        let armed = Instant::now();
        let carried = self.suppressed;
        let verdict = self.exec(&hit, dup, carried);
        let exited = Instant::now();
        if self.fault == Fault::KillAfterExec {
            self.fault.fire(&self.store);
        }
        self.owe_verdict(
            off,
            format!("{off} fired on={} {verdict} dup={}", hit.on, u8::from(dup)),
        );
        if carried > 0 && verdict == "rc=0" {
            self.suppressed -= carried;
            self.save(&[Part::Journal, Part::Suppressed]);
        } else {
            self.save(&[Part::Journal]);
        }
        if let Some(stamps) = self.exec_stamps.as_mut().filter(|_| !dup) {
            stamps.untold = Some((off, [armed, exited, Instant::now()]));
        }
        match raised.as_ref() {
            Some(stand) if verdict == "rc=0" => self.stand(&key, off, &subject, stand),
            Some(stand) => self.owe(&key, off, stand),
            None => {}
        }
        if dup {
            self.counts.dup += 1;
        }
        self.counts.fired += 1;
        Ok(())
    }

    /// Put a fresh page's `firing` line (`t=` its stamp) and the stamp spent on the media before
    /// its command runs ([`Notifier::put_steps`]): one step, the line and the stamp, appended to
    /// the journal and flushed in full ([`Self::firing`]), the page's one full flush, so a power
    /// loss mid-command runs it again with `ATERM_NOTIFY_DUP=1`; in a log's own store the line's
    /// step ordered ahead of the stamp's, whose flush is that one. Refused, it leaves the
    /// journal, the stamps and what is owed in memory as they were, so the page, done again,
    /// writes the same line and spends its stamp once.
    fn arm(&mut self, off: u64, line: String, t: u64) -> io::Result<()> {
        let held = (
            self.journal.clone(),
            self.stamps.clone(),
            self.owed_verdicts.clone(),
            self.owed_spent.clone(),
            self.dirty.clone(),
        );
        self.owe_verdict(off, line);
        self.spend(t);
        self.dirty.extend([Part::Journal, Part::Stamps]);
        let armed = self.put_steps(self.firing).map_err(|(_, e)| e);
        if armed.is_err() {
            (
                self.journal,
                self.stamps,
                self.owed_verdicts,
                self.owed_spent,
                self.dirty,
            ) = held;
        }
        armed
    }

    fn exec(&self, hit: &Hit, dup: bool, suppressed: u64) -> String {
        #[cfg(test)]
        AT_EXEC.with(|seam| {
            if let Some(seen) = seam.borrow_mut().as_mut() {
                seen(hit);
            }
        });
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.process_group(0)
            .arg("-c")
            .arg(&self.cfg.exec)
            .stdin(Stdio::null())
            .env("ATERM_NOTIFY_FLEET", &self.cfg.fleet)
            .env("ATERM_NOTIFY_ON", &hit.on)
            .env("ATERM_NOTIFY_OFFSET", hit.offset.to_string())
            .env("ATERM_NOTIFY_SUBJECT", &hit.subject)
            .env("ATERM_NOTIFY_NODE", &hit.node)
            .env("ATERM_NOTIFY_WHO", &hit.who)
            .env("ATERM_NOTIFY_SID", &hit.sid)
            .env("ATERM_NOTIFY_TEXT", &hit.text)
            .env("ATERM_NOTIFY_NEED", &hit.need)
            .env("ATERM_NOTIFY_DETAIL", &hit.detail)
            .env("ATERM_NOTIFY_DUP", if dup { "1" } else { "0" })
            .env("ATERM_NOTIFY_SUPPRESSED", suppressed.to_string());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("aterm-link notify: could not run --exec: {e}");
                return "rc=spawn-failed".to_string();
            }
        };
        #[cfg(test)]
        BEFORE_EXIT_WAIT.with(|seam| {
            if let Some(before) = seam.borrow_mut().as_mut() {
                before(&mut child);
            }
        });
        // The kernel's word of the exit (a kqueue on Darwin, a pidfd on Linux), registered while
        // the child is unreaped, and each turn reaps first. Darwin's kqueue will not watch a
        // zombie, so a command that exited before the registration leaves the wait nothing to
        // report: the leading reap ends it at once rather than after a fallback tick. A Linux
        // pidfd of a zombie reads ready at once, and there the leading reap saves one poll.
        let mut exit =
            aterm_uds::deadline::ExitWait::new(&child).ticking_at_most(EXEC_FALLBACK_TICK);
        let deadline = Instant::now().checked_add(self.cfg.exec_timeout);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return match status.code() {
                        Some(0) => "rc=0".to_string(),
                        Some(n) => {
                            eprintln!(
                                "aterm-link notify: --exec exited {n} for off={}",
                                hit.offset
                            );
                            format!("rc={n}")
                        }
                        None => {
                            eprintln!(
                                "aterm-link notify: --exec was signalled for off={}",
                                hit.offset
                            );
                            "rc=signalled".to_string()
                        }
                    };
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("aterm-link notify: waiting on --exec: {e}");
                    return "rc=wait-failed".to_string();
                }
            }
            let left = deadline.map_or(Duration::MAX, |d| {
                d.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                if let Ok(pgid) = i32::try_from(child.id()) {
                    let _ = aterm_uds::process::kill_group_until_empty(pgid);
                }
                aterm_uds::deadline::kill_within(&mut child, Duration::from_secs(5));
                eprintln!(
                    "aterm-link notify: --exec timed out for off={}; killed",
                    hit.offset
                );
                return "rc=timeout".to_string();
            }
            #[cfg(test)]
            EXIT_WAITS.with(|turns| turns.set(turns.get() + 1));
            exit.wait(left);
        }
    }

    fn start_offset(&self, conn: &mut Conn, sel: &Sel) -> io::Result<u64> {
        if let Some(off) = self.cursor(&sel.key()) {
            return Ok(off);
        }
        Ok(match self.cfg.since {
            Since::Start => 0,
            Since::At(n) => n,
            Since::Head => conn.fetch(0, &sel.filter(&self.cfg.fleet), 0)?.1 .1,
        })
    }

    /// Read every selector up to the broker's head (`--once`), then write what is owed
    /// ([`Notifier::persist`]): Err when a page's `firing` line or anything owed could not be
    /// written, its cursor left before what is not done, for the next run; a record behind a
    /// part the state dir refused is not judged ([`Notifier::owing`]); Err with the refusal
    /// where the store was removed or replaced under it ([`Notifier::held`]).
    fn catch_up(&mut self) -> io::Result<()> {
        let caught = self.catch_up_to_head();
        let persisted = self.persist();
        self.lost().map_or(caught.and(persisted), Err)
    }

    fn catch_up_to_head(&mut self) -> io::Result<()> {
        let mut conn = self.connect()?;
        let log = log_of(&mut conn, &self.cfg)?;
        self.scope_to(&log)?;
        'selectors: for sel in self.cfg.on.clone() {
            if let Some((err, _)) = self.owing() {
                return Err(err);
            }
            let filter = sel.filter(&self.cfg.fleet);
            let key = sel.key();
            let mut from = self.start_offset(&mut conn, &sel)?;
            if matches!(sel, Sel::Need(_)) {
                let need = &mut self.need;
                crate::notify_need::warm(
                    &mut conn,
                    &self.cfg.fleet,
                    from,
                    |off, subject, body| {
                        need.warm(off, &subject, &body);
                        true
                    },
                )?;
                self.seed_standing(&key, from);
            }
            loop {
                let ((rows, (next, head)), floor) =
                    transport::fetch_kept(&mut conn, from, &filter, PAGE)?;
                if let Some(floor) = floor {
                    eprintln!(
                        "aterm-link notify: {}: the broker's retention gave up the records \
                         below @{floor} (from @{from}); reading on from there",
                        sel.label()
                    );
                }
                let durable = conn.seen_durable();
                for (off, subject, body) in rows {
                    if self.wait_durable && log.ordered && durable.is_some_and(|d| off >= d) {
                        self.checkpoint(&key, off);
                        eprintln!(
                            "aterm-link notify: {} off={off} is not durable yet (the broker \
                             serves ordered, and flushes it in full within its horizon plus a \
                             batch and a full flush): it and what follows it wait for the next \
                             run",
                            sel.label()
                        );
                        continue 'selectors;
                    }
                    if let Some((err, _)) = self.owing() {
                        self.checkpoint(&key, off);
                        return Err(err);
                    }
                    self.on_record(&sel, off, &subject, &body)
                        .map_err(|stalled| stalled.err)?;
                }
                self.checkpoint(&key, next);
                if next >= head || next <= from {
                    break;
                }
                from = next;
            }
        }
        Ok(())
    }

    /// Follow every selector until each reader ends ([`Notifier::serve`]).
    fn follow(&mut self) -> io::Result<()> {
        let (tx, rx) = notice_handoff();
        for (idx, sel) in self.cfg.on.iter().enumerate() {
            let cfg = self.cfg.clone();
            let sel = sel.clone();
            let tx = tx.clone();
            let hold = std::sync::Arc::clone(&self.hold);
            std::thread::Builder::new()
                .name(format!("notify-{}", sel.key()))
                .spawn(move || reader(idx, &sel, &cfg, &hold, &tx))?;
        }
        drop(tx);
        self.serve(&rx)
    }

    /// Take the readers' notices in turn until each reader ends, writing an owed cursor when it
    /// falls due though nothing arrives ([`Notifier::persist_due`]), and everything owed should
    /// every reader end. A reader reconnects for as long as the process runs, so in practice the
    /// loop ends with the process, a stop or a kill losing the cursor owed: [`CURSOR_LAG`]
    /// records or [`CURSOR_EVERY`] at most, which a restart re-reads and the dedup absorbs. A
    /// write the state dir refuses never ends it: what the write held stays owed and is written
    /// again [`RETRY_EVERY`] later, at the next notice or tick ([`Notifier::tick`]); a page
    /// whose `firing` line it refuses, or a record behind a verdict, count or standing value it
    /// refuses ([`Notifier::owing`]), stops the loop, which takes no notice (its readers wait in
    /// the rendezvous) until that page has run or that part is written ([`Notifier::drain`]).
    /// A store removed or replaced under it does end it, Err with that refusal
    /// ([`Notifier::held`]): it writes nothing more.
    fn serve(&mut self, rx: &mpsc::Receiver<Notice>) -> io::Result<()> {
        loop {
            if let Some(e) = self.lost() {
                return Err(e);
            }
            if self.stalled {
                let wait = self.failing.map_or(Duration::ZERO, |f| {
                    f.next.saturating_duration_since(self.now())
                });
                std::thread::sleep(wait);
                let _ = self.tick(self.now());
                self.tell();
                continue;
            }
            let notice = match self.next_due() {
                None => rx.recv().ok(),
                Some(due) => match rx.recv_timeout(due.saturating_duration_since(self.now())) {
                    Ok(notice) => Some(notice),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let _ = self.tick(self.now());
                        self.tell();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                },
            };
            let Some(notice) = notice else {
                break;
            };
            let _ = self.take(notice);
            self.tell();
        }
        let _ = self.persist();
        Err(self.lost().unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::ConnectionAborted, "every subscription ended")
        }))
    }

    /// Take one [`Notice`] from a follow-mode reader in read order ([`Notice::Durable`]), after
    /// whatever a refused write left ([`Notifier::tick`]): Err when a refused write stalled it,
    /// the notice left to be taken once that is done.
    fn take(&mut self, notice: Notice) -> io::Result<()> {
        self.left.push_back(Left::Notice(notice));
        self.tick(self.now())
    }

    /// Once a refused write's retry is due, write the parts owed first ([`Notifier::rewrite`]),
    /// so what waits behind them is done at that retry; then do what a refused write left
    /// ([`Notifier::drain`]), and write what is owed and due ([`Notifier::settle`]).
    fn tick(&mut self, now: Instant) -> io::Result<()> {
        if !self.dirty.is_empty() && self.writable(now) {
            let _ = self.rewrite(now);
        }
        let drained = self.drain(now);
        self.settle(self.now());
        drained
    }

    /// Do [`Self::left`] in order, a stalled first item only once its retry is due: Err at the
    /// first a refused write stalls, it and what follows it left.
    fn drain(&mut self, now: Instant) -> io::Result<()> {
        if self.stalled && !self.writable(now) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "a write the state dir refused is not due for its retry",
            ));
        }
        while let Some(left) = self.left.pop_front() {
            self.stalled = false;
            if let Err(Stalled { left, err, what }) = self.run(left) {
                self.left.push_front(left);
                self.stalled = true;
                self.fail(&err, self.now(), what);
                return Err(err);
            }
        }
        Ok(())
    }

    /// One item of [`Self::left`]: a page, once it ran, moves its selector's cursor past it, as a
    /// verdict does ([`Notifier::judge`]), and releases on what it was released with.
    fn run(&mut self, left: Left) -> Result<(), Stalled> {
        match left {
            Left::Notice(notice) => self.take_one(notice),
            Left::Page(page) => {
                let (key, off, then) = (page.sel.key(), page.off, page.then);
                self.fire(*page)?;
                self.checkpoint(&key, off + 1);
                match then {
                    Some((idx, bound)) => self.release(idx, bound),
                    None => Ok(()),
                }
            }
            Left::Release(idx, bound) => self.release(idx, bound),
        }
    }

    fn take_one(&mut self, notice: Notice) -> Result<(), Stalled> {
        match notice {
            Notice::Log(idx, log) => {
                let Some(slot) = self.reader_log.get(idx) else {
                    return Ok(());
                };
                if slot.id != log.id {
                    let dropped: usize = self.held[idx]
                        .drain(..)
                        .map(|h| h.subject.len() + h.body.len())
                        .sum();
                    self.held_bytes -= dropped;
                    self.scanned[idx] = None;
                    if matches!(self.cfg.on.get(idx), Some(Sel::Need(_))) {
                        self.need = NeedWatch::new(&self.cfg.fleet);
                        self.need_fresh = true;
                    }
                }
                self.durable[idx] = 0;
                let ordered = log.ordered;
                if (slot.id, slot.ordered) != (log.id, ordered) {
                    self.durable_said[idx] = false;
                    self.over_paged[idx] = None;
                }
                self.reader_log[idx] = log;
                if !ordered && !self.held[idx].is_empty() {
                    self.release(idx, u64::MAX)?;
                }
            }
            Notice::Warm(idx, off, subject, body) => {
                if self.need_fresh && matches!(self.cfg.on.get(idx), Some(Sel::Need(_))) {
                    self.need.warm(off, &subject, &body);
                }
            }
            Notice::Record(idx, off, subject, body) => {
                let Some(sel) = self.cfg.on.get(idx).cloned() else {
                    return Ok(());
                };
                let log = self.reader_log[idx].clone();
                if let Err(err) = self.scope_to(&log) {
                    return Err(Stalled {
                        left: Left::Notice(Notice::Record(idx, off, subject, body)),
                        err,
                        what: "the store of the log in hand",
                    });
                }
                if let Some((err, what)) = self.owing() {
                    return Err(Stalled {
                        left: Left::Notice(Notice::Record(idx, off, subject, body)),
                        err,
                        what,
                    });
                }
                let warmed = matches!(sel, Sel::Need(_)) && std::mem::take(&mut self.need_fresh);
                if warmed {
                    self.seed_standing(&sel.key(), off);
                }
                let waits = self.wait_durable
                    && log.ordered
                    && (!self.held[idx].is_empty() || off >= self.durable[idx]);
                if waits {
                    let size = subject.len() + body.len();
                    let over = self.held_bytes + size > HOLD_MAX;
                    if over {
                        let streak = self.over_paged[idx].is_some_and(|o| self.durable[idx] <= o);
                        self.over_paged[idx] = Some(off);
                        if !streak {
                            warn(&format!(
                                "{} holds {} bytes the broker has not said are durable — past \
                                 {HOLD_MAX}, so they are paged now, before it does (said once \
                                 until it says they are)",
                                sel.label(),
                                self.held_bytes + size
                            ));
                            #[cfg(test)]
                            WARNED.with(|warned| warned.borrow_mut().push(Warned::HoldOver(idx)));
                        }
                    } else if !std::mem::replace(&mut self.durable_said[idx], true) {
                        warn(&format!(
                            "{} {DURABLE_WAIT_WORDS} (it serves ordered: its horizon plus one \
                             batch and one full flush); said once for this log",
                            sel.label()
                        ));
                        #[cfg(test)]
                        WARNED.with(|warned| warned.borrow_mut().push(Warned::DurableWait(idx)));
                    }
                    self.held_bytes += size;
                    self.held[idx].push_back(Held { off, subject, body });
                    if over {
                        self.release(idx, u64::MAX)?;
                    }
                    return Ok(());
                }
                self.judge(&sel, off, &subject, &body)?;
            }
            Notice::Checkpoint(idx, next) => {
                let Some(key) = self.cfg.on.get(idx).map(Sel::key) else {
                    return Ok(());
                };
                if self.held[idx].is_empty() {
                    let log = self.reader_log[idx].clone();
                    if let Err(err) = self.scope_to(&log) {
                        return Err(Stalled {
                            left: Left::Notice(Notice::Checkpoint(idx, next)),
                            err,
                            what: "the store of the log in hand",
                        });
                    }
                    self.checkpoint(&key, next);
                } else {
                    self.scanned[idx] = Some(self.scanned[idx].map_or(next, |s| s.max(next)));
                }
            }
            Notice::Start(idx, head, done) => {
                let Some(key) = self.cfg.on.get(idx).map(Sel::key) else {
                    return Ok(());
                };
                let log = self.reader_log[idx].clone();
                let wrote = self
                    .scope_to(&log)
                    .and_then(|()| self.put_cursor(&key, head));
                if wrote.is_ok() && self.lag.get(&key).is_some_and(|lag| lag.want <= head) {
                    self.lag.remove(&key);
                }
                let _ = done.send(wrote);
            }
            Notice::Durable(idx, durable) => {
                let Some(slot) = self.durable.get_mut(idx) else {
                    return Ok(());
                };
                *slot = (*slot).max(durable);
                let durable = *slot;
                self.release(idx, durable)?;
            }
        }
        Ok(())
    }

    /// Judge reader `idx`'s held records below `bound` in order, then write the checkpoint they
    /// held back. A page among them that a refused write stalls leaves the rest held, to be
    /// released on once it ran ([`Page::then`]), as does a part one of them changed and the
    /// state dir refused ([`Notifier::owing`]).
    fn release(&mut self, idx: usize, bound: u64) -> Result<(), Stalled> {
        let Some(sel) = self.cfg.on.get(idx).cloned() else {
            return Ok(());
        };
        let log = self.reader_log[idx].clone();
        if let Err(err) = self.scope_to(&log) {
            return Err(Stalled {
                left: Left::Release(idx, bound),
                err,
                what: "the store of the log in hand",
            });
        }
        while self.held[idx].front().is_some_and(|h| h.off < bound) {
            if let Some((err, what)) = self.owing() {
                return Err(Stalled {
                    left: Left::Release(idx, bound),
                    err,
                    what,
                });
            }
            let Some(h) = self.held[idx].pop_front() else {
                break;
            };
            self.held_bytes -= h.subject.len() + h.body.len();
            if let Err(mut stalled) = self.judge(&sel, h.off, &h.subject, &h.body) {
                if let Left::Page(page) = &mut stalled.left {
                    page.then = Some((idx, bound));
                }
                return Err(stalled);
            }
        }
        if self.held[idx].is_empty() {
            if let Some(next) = self.scanned[idx].take() {
                self.checkpoint(&sel.key(), next);
            }
        }
        Ok(())
    }

    /// Judge a following selector's record ([`Notifier::on_record`]) and move its cursor past
    /// it: written at once after a verdict (a page, or a drop), owed otherwise
    /// ([`Notifier::advance`]).
    fn judge(&mut self, sel: &Sel, off: u64, subject: &str, body: &[u8]) -> Result<(), Stalled> {
        let verdicts = self.verdicts();
        self.on_record(sel, off, subject, body)?;
        let next = off.saturating_add(1);
        if self.verdicts() == verdicts {
            self.advance(&sel.key(), next);
        } else {
            self.checkpoint(&sel.key(), next);
        }
        Ok(())
    }
}

fn reader(idx: usize, sel: &Sel, cfg: &Config, hold: &Arc<Hold>, tx: &mpsc::SyncSender<Notice>) {
    let filter = sel.filter(&cfg.fleet);
    let key = sel.key();
    let mut seen: Option<u64> = None;
    let mut seen_on: Option<PathBuf> = None;
    let mut backoff = Backoff::default();
    let mut outage = transport::Outage::default();
    let say = |line: Option<String>| {
        if let Some(line) = line {
            eprintln!("aterm-link notify: {} {line}", sel.label());
        }
    };
    loop {
        let wait = match connect_for_reader(cfg, hold, &key, &filter, &mut seen, &mut seen_on) {
            Ok((mut conn, closer, mut from, log, start)) => 'attached: {
                if tx.send(Notice::Log(idx, log)).is_err() {
                    return;
                }
                if let Some(head) = start {
                    let (done, wrote) = mpsc::sync_channel(1);
                    if tx.send(Notice::Start(idx, head, done)).is_err() {
                        return;
                    }
                    match wrote.recv() {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            say(outage.failed(Instant::now(), &e));
                            break 'attached Some(backoff.failed());
                        }
                        Err(_) => return,
                    }
                }
                backoff.attached(Instant::now());
                let mut closed = false;
                if matches!(sel, Sel::Need(_)) {
                    let mut gone = false;
                    let warmed =
                        crate::notify_need::warm(&mut conn, &cfg.fleet, from, |off, s, b| {
                            gone = tx.send(Notice::Warm(idx, off, s, b)).is_err();
                            !gone
                        });
                    if gone {
                        return;
                    }
                    if let Err(e) = warmed {
                        eprintln!("aterm-link notify: {} warm-up ended: {e}", sel.label());
                        closed = true;
                    }
                }
                while !closed {
                    let (rows, (next, head)) =
                        match transport::fetch_kept(&mut conn, from, &filter, PAGE) {
                            Ok((page, floor)) => {
                                if let Some(floor) = floor {
                                    eprintln!(
                                        "aterm-link notify: {}: the broker's retention gave up \
                                         the records below @{floor} (from @{from}); reading on \
                                         from there",
                                        sel.label()
                                    );
                                }
                                page
                            }
                            Err(e) => {
                                eprintln!("aterm-link notify: {} catch-up ended: {e}", sel.label());
                                closed = true;
                                break;
                            }
                        };
                    for (off, subject, body) in rows {
                        if tx.send(Notice::Record(idx, off, subject, body)).is_err() {
                            return;
                        }
                        let after = off.saturating_add(1);
                        seen = Some(seen.map_or(after, |s| s.max(after)));
                    }
                    if tx.send(Notice::Checkpoint(idx, next)).is_err() {
                        return;
                    }
                    seen = Some(seen.map_or(next, |s| s.max(next)));
                    if let Some(durable) = conn.seen_durable() {
                        if tx.send(Notice::Durable(idx, durable)).is_err() {
                            return;
                        }
                    }
                    let made_progress = next > from;
                    from = next;
                    if next >= head || !made_progress {
                        break;
                    }
                }
                if !closed {
                    match conn.subscribe(from, &filter) {
                        Ok(mut sub) => {
                            let _ = closer.set_read_timeout(None);
                            say(outage.attached(Instant::now()));
                            loop {
                                match sub
                                    .recv_live_event(&closer, astream_broker::IDLE_MARK_DEADLINE)
                                {
                                    Ok(Some(astream_broker::Event::Delivery {
                                        offset: off,
                                        subject,
                                        body,
                                    })) => {
                                        if tx.send(Notice::Record(idx, off, subject, body)).is_err()
                                        {
                                            return;
                                        }
                                        let after = off.saturating_add(1);
                                        seen = Some(seen.map_or(after, |s| s.max(after)));
                                    }
                                    Ok(Some(astream_broker::Event::Mark { durable, .. })) => {
                                        if let Some(durable) = durable {
                                            if tx.send(Notice::Durable(idx, durable)).is_err() {
                                                return;
                                            }
                                        }
                                    }
                                    Ok(Some(astream_broker::Event::Gap(gap))) => eprintln!(
                                        "aterm-link notify: {}: the broker's retention gave up \
                                         {} below this tail; reading on from there",
                                        sel.label(),
                                        transport::gap_lost(&gap)
                                    ),
                                    Ok(None) => break,
                                    Err(e) => {
                                        eprintln!(
                                            "aterm-link notify: {} subscription ended: {e}",
                                            sel.label()
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => say(outage.failed(Instant::now(), &e)),
                    }
                }
                backoff.lost(Instant::now())
            }
            Err(_) if hold.lost.get().is_some() => return,
            Err(e) => {
                say(outage.failed(Instant::now(), &e));
                Some(backoff.failed())
            }
        };
        if let Some(wait) = wait {
            std::thread::sleep(wait);
        }
    }
}

/// Connect reader `key` to the broker and find where it reads from: its cursor in the store of
/// the log it reaches ([`Store::cursor`]), or past what this reader already handed on
/// (`seen`), or `--since`. A reader writes nothing to the store's journal, whose one writer is
/// the notifier ([`Store::open`]): a start at the head it found with no cursor is handed back
/// (the last item) for the notifier to write ([`Notice::Start`]), and the reader reads no
/// record until the notifier answers that it is on the media, so a restart after any record
/// was read re-reads from that head, never from a later one; where the write is refused the
/// reader connects again after its backoff, as a refused connect does.
fn connect_for_reader(
    cfg: &Config,
    hold: &Arc<Hold>,
    key: &str,
    filter: &str,
    seen: &mut Option<u64>,
    seen_on: &mut Option<PathBuf>,
) -> io::Result<(Conn, Closer, u64, ReadLog, Option<u64>)> {
    let (mut conn, closer) = transport::connect(&cfg.broker, ack_deadline())?;
    transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
    let log = log_of(&mut conn, cfg)?;
    let store = reader_store(hold, &log)?;
    if seen_on.as_ref().is_some_and(|on| *on != store.root) {
        *seen = None;
    }
    *seen_on = Some(store.root.clone());
    let seen = *seen;
    let mut start = None;
    let from = match (store.cursor(key), seen) {
        (Some(c), Some(s)) => c.max(s),
        (Some(c), None) => c,
        (None, Some(s)) => s,
        (None, None) => match cfg.since {
            Since::Start => 0,
            Since::At(n) => n,
            Since::Head => {
                let head = conn.fetch(0, filter, 0)?.1 .1;
                start = Some(head);
                head
            }
        },
    };
    Ok((conn, closer, from, log, start))
}

/// The store of the log a reader reached ([`Store::for_log`] of the root store), which can write
/// the root's `log` file, carry a moved generation's store and prune old ones: so it runs
/// [`Hold::check`] first, as every write of the notifier's does, and a reader whose store was
/// removed or replaced under the notifier writes nothing and ends ([`reader`]).
///
/// # Errors
/// [`Hold::check`]'s refusal, or [`Store::for_log`]'s.
fn reader_store(hold: &Arc<Hold>, log: &ReadLog) -> io::Result<Store> {
    hold.check()?;
    Store {
        root: hold.root.clone(),
        directory: Arc::clone(&hold.directory),
        access: Owned(Arc::clone(hold)),
    }
    .for_log(log.id, log.lineage.as_ref())
}

/// The log `conn` reaches ([`astream_broker::Client::log_lineage`]).
fn log_of(conn: &mut Conn, cfg: &Config) -> io::Result<ReadLog> {
    let id = match conn.seen_log_identity() {
        Some(log) => Some(log),
        None if cfg.cap_files.is_empty() => conn.log_identity()?,
        None => None,
    };
    let lineage = conn.log_lineage().cloned();
    #[cfg(test)]
    let lineage = lineage.filter(|_| !FORGET_LINEAGE.with(std::cell::Cell::get));
    let ordered = conn
        .served_durability()
        .is_some_and(|s| s.served == astream_broker::Tier::Ordered);
    Ok(ReadLog {
        id,
        lineage,
        ordered,
    })
}

#[cfg(test)]
thread_local! {
    static FORGET_LINEAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Called as [`Notifier::exec`] starts a command, so a test can ask what was flushed
    /// before it.
    static AT_EXEC: std::cell::RefCell<Option<ExecSeam>> = const { std::cell::RefCell::new(None) };
    /// Called with the command [`Notifier::exec`] spawned, before it registers the command's
    /// exit wait, so a test can have the command exit first.
    static BEFORE_EXIT_WAIT: std::cell::RefCell<Option<ChildSeam>> =
        const { std::cell::RefCell::new(None) };
    /// The turns [`Notifier::exec`] waited on its commands' exits on this thread.
    static EXIT_WAITS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// The [`Store::put`]s left before a test's kill: at `Some(0)` every put fails before it
    /// writes, as if the process had died there.
    static PUT_FAULT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    /// The bytes [`note_bytes`] tallied on this thread.
    static BYTES_NOTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The flushes [`note_flush`] tallied on this thread, so a test can compare them with what
    /// it asked of `aterm_durable`.
    static NOTED: std::cell::RefCell<Vec<Flush>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The writes a test has the store refuse: by name prefix, the puts to let through first and
    /// the errno the next one fails with, once ([`refused`]).
    static REFUSE: std::cell::RefCell<Vec<(&'static str, u32, i32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// While `Some`, the name of each [`Store::put`] this thread made, in order.
    static PUT_NAMES: std::cell::RefCell<Option<Vec<String>>> =
        const { std::cell::RefCell::new(None) };
    /// While `Some`, each directory [`flush_dir`] flushed on this thread, in order.
    static DIRS_FLUSHED: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
    /// What the notifier warned of on this thread ([`Notifier::fail`], [`Notifier::settle`],
    /// [`Notifier::take_one`]).
    static WARNED: std::cell::RefCell<Vec<Warned>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A warning, as [`WARNED`] keeps it: the backoff's refusal and its errno, or its end; a
/// selector's durable wait, or its hold past [`HOLD_MAX`] (fix unit E5).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Warned {
    Refused(Option<i32>),
    Cleared,
    DurableWait(usize),
    HoldOver(usize),
}

/// The errno a test has the store refuse the put of `name` with ([`REFUSE`]): the first entry
/// whose prefix `name` starts with lets its count through, then fails one and is spent.
#[cfg(test)]
fn refused(name: &str) -> Option<i32> {
    REFUSE.with(|refuse| {
        let mut refuse = refuse.borrow_mut();
        let at = refuse.iter().position(|(p, ..)| name.starts_with(p))?;
        let (_, pass, errno) = &mut refuse[at];
        if *pass > 0 {
            *pass -= 1;
            return None;
        }
        let errno = *errno;
        refuse.remove(at);
        Some(errno)
    })
}

/// What [`AT_EXEC`] calls with the hit whose command starts.
#[cfg(test)]
type ExecSeam = Box<dyn FnMut(&Hit)>;

/// What [`BEFORE_EXIT_WAIT`] calls with the command just spawned.
#[cfg(test)]
type ChildSeam = Box<dyn FnMut(&mut aterm_uds::spawnfd::Child)>;

const USAGE: &str = "\
aterm link notify — run a command when the fabric needs a human

  aterm link notify --fleet <F> --broker <socket> [--cap-file <path>...]
                    --on <attention|ask:<person>|halt|need[:<word>]>,... --exec <cmd>
                    [options]

  --on <sel>,...         what to watch (repeatable):
                         - `attention`: a presence row raising `attention=` (node.cap,
                           or a person's cap)
                         - `ask:<person>`: an ask addressed to <person> (that person's own
                           cap, from `aterm fabric mint-for <person>`)
                         - `halt`: a fleet halt coming on (node.cap)
                         - `need`, `need:<word>`: a need a session raises, as `aterm fabric
                           brief` folds it from the session log on the bus: any, or <word>,
                           or a family's (`need:approval` takes every `approval:<kind>`; the
                           words and families are below). It reads `ro:/f/<F>/pub/>`, which
                           node.cap and a person's cap hold, the caps `fabric brief` reads
                           under
  --exec <cmd>           run `/bin/sh -c <cmd>` per record, its facts in `$ATERM_NOTIFY_*`
                         (FLEET ON OFFSET SUBJECT NODE WHO SID TEXT NEED DETAIL DUP
                         SUPPRESSED; a need's NEED is its word and DETAIL what the brief
                         shows beside it, a wall's reset or a failure's exit code)
  --rate <n>/<window>    at most <n> runs per window, e.g. `10/1m`, `1/30s`, `40/1h`
                         (default `10/1m`); a record over it is dropped and counted in the
                         next command's `$ATERM_NOTIFY_SUPPRESSED`
  --since head|start|<n> where a selector with no cursor starts (default `head`)
  --once                 catch up to the head, then exit
  --exec-timeout <ms>    SIGKILL a command still running after this (default `30000`)
  --state <dir>          state dir (`<dir>/notify/` holds cursors and the journal); one
                         notifier runs per state dir, and a second is refused at its start:
                         give one notifier every selector (`--on ask:<person>,need`), or
                         each its own --state

Dedups across restarts by offset, `attention` by each subject's standing value (clearing it
and raising it again pages again), and `need` by each session's standing needs: a need is
its word and the record that raised it, so each need a session raises pages once (a new
failure, or a question asked while a failure stands) and one that stays standing never
pages again, across records and restarts. A crash mid-`--exec`, a power loss too, runs it
again with `ATERM_NOTIFY_DUP=1`. A record that runs no command, is not dropped by `--rate`
and moves no standing value writes nothing: the cursor follows within {CURSOR_LAG} records or a
second, and a restart after a stop or a kill re-reads at most those, which the dedup
absorbs. A state dir that refuses a write (a full disk) never stops a follower: it warns with
the error, holds what it owes, and writes it again a second later; a page runs only once its
`firing` line is written, and the follower reads nothing past it, or past a record whose
verdict, drop count or standing value is unwritten, meanwhile (`--once` exits 1 and leaves it
to the next run), so a crash then re-reads that record.
A `need` folds the records below its cursor, up to `fabric brief`'s window, before
it judges one: every need that warm-up finds stands, so a notifier pages only a need raised
at or past its cursor (a fresh one's is the head). A need whose command did not end
`rc=0`, or that `--rate` dropped, is owed: no warm-up stands it, and it pages again at its
session's next record that still shows it (the record that raised it, or a whole snapshot
naming it, with nothing lost after), in the same run or the next (`--once` too), whatever
older need of its word stands. A session whose own row
says it exited before its close reached the bus has no need; a session's close raises
`exited`, which pages once, whether the close or the exited row reached the bus first. A
session whose close never reached the bus pages `exited` with detail `gone` once, at a
successor's restore that names it (`restore prev=<sid>`), when its aterm died: the
incarnation that hosted it ended and never said it exited, as `fabric brief` derives its
close. A clean quit closes every session it carried on the bus (`out=app-quit`) before
its bridge withdraws them, and that close pages nothing; nor do the restores of the relaunch
after it, nor a reopened tab's. A
node's will (its row's `state=away`, or the legacy `gone`) pages and clears nothing, so a
reattach that restores the same needs pages nothing. A session whose fold is partial (the
brief's `partial`: no start, lost records, or a snapshot short of its needs) clears nothing,
and pages a need only when it holds the record that raised it and lost nothing after it. A
need only a snapshot names pages once, at the first whole snapshot (one that lists every need
its session held) naming it with nothing lost past it, only when the bus lost its raising
record while the follower read the session whole: the follower judged a record of the session
whose fold was whole with nothing lost and did not name the need, so it pages though an older
need of its word stands; one that stood before then never pages. A need a consumer claims
(`aterm ctl @<sid> mark - need word=<review|decision|help>`) never pages:
`aterm ctl @<sid> await log need` waits on it in the session's own log. On an `ordered`
broker a record waits until it is durable, which stderr says once per log; `--once` leaves
one not yet durable for the next run. A follower that cannot reach the broker retries with
back-off up to {RECONNECT_MAX} s, and says so once per outage (once more for each other
error it meets) and once when it subscribes again.
Prints `notify: fired= dup= skipped= dropped= suppressed= standing=` on exit, `standing=`
counting this run's records whose need or `attention` value already stood, so ran nothing
(not the values standing now: a `--once` run that reads no new record says `standing=0`). Exits 2
on a usage error, an unreadable --cap-file, or a broker that refuses the cap files or a
selector's read (no --cap-file grants it); 1 when the state dir cannot open or the run
fails.
";

/// `aterm link notify --help`: [`USAGE`] with its cursor lag and retry ceiling filled in
/// ([`CURSOR_LAG`], `transport::RECONNECT_MAX`), then the words and families `need:<word>`
/// takes ([`need_words`], [`need_families`]).
#[must_use]
pub fn usage() -> String {
    format!(
        "{}\nNeed words (`need:<word>`, the needs a session's fold raises):\n{}\n\
         Need families (`need:<family>`, each `<family>:<kind>` word above):\n{}\n",
        USAGE
            .replace("{CURSOR_LAG}", &CURSOR_LAG.to_string())
            .replace(
                "{RECONNECT_MAX}",
                &transport::RECONNECT_MAX.as_secs().to_string()
            ),
        fill(need_words()),
        fill(need_families())
    )
}

/// The words `need:<word>` takes: the needs a session's fold raises as observed, in the log's
/// order ([`aterm_slog::observed_need_words`]). The log's other need words
/// ([`aterm_slog::vocab::NEED_WORDS`]) never page: a consumer's claim, or a word no fold raises.
fn need_words() -> impl Iterator<Item = &'static str> {
    aterm_slog::observed_need_words()
}

/// The families `need:<family>` takes: each prefix before `:` of a [`need_words`] word
/// (`approval`, `wall`), first seen first.
fn need_families() -> impl Iterator<Item = &'static str> {
    let mut families: Vec<&'static str> = Vec::new();
    for (family, _) in need_words().filter_map(|w| w.split_once(':')) {
        if !families.contains(&family) {
            families.push(family);
        }
    }
    families.into_iter()
}

/// `words` as [`usage`] lists them, filled to the help's width.
fn fill(words: impl Iterator<Item = &'static str>) -> String {
    let words: Vec<String> = words.map(str::to_string).collect();
    crate::render::fill(&words, 2, 90)
}

/// `aterm-link notify`.
#[must_use = "handle or propagate the command's exit status"]
pub fn main(args: &[String]) -> ExitCode {
    if crate::cli::asks_help(args) {
        return aterm_uds::stdout::answer_exit("aterm link notify", &usage(), 0);
    }
    let cfg = match parse(args) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("aterm link notify: {e}");
            eprint!("{}", usage());
            return ExitCode::from(2);
        }
    };
    let once = cfg.once;
    let mut notifier = match Notifier::new(cfg) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("aterm-link notify: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = notifier.preflight() {
        eprintln!("aterm-link notify: {e}");
        return ExitCode::from(2);
    }
    let outcome = if once {
        notifier.catch_up()
    } else {
        notifier.follow()
    };
    let c = notifier.counts;
    println!(
        "notify: fired={} dup={} skipped={} dropped={} suppressed={} standing={}",
        c.fired, c.dup, c.skipped, c.dropped, notifier.suppressed, c.standing
    );
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm-link notify: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config {
        fleet: String::new(),
        broker: String::new(),
        cap_files: Vec::new(),
        state_dir: String::new(),
        on: Vec::new(),
        exec: String::new(),
        limit: DEFAULT_LIMIT,
        window_ms: DEFAULT_WINDOW_MS,
        since: Since::Head,
        once: false,
        exec_timeout: DEFAULT_EXEC_TIMEOUT,
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--fleet" => cfg.fleet = value()?,
            "--broker" => cfg.broker = value()?,
            "--cap-file" => cfg.cap_files.push(value()?),
            "--state" => cfg.state_dir = value()?,
            "--exec" => cfg.exec = value()?,
            "--once" => cfg.once = true,
            "--on" => {
                for word in value()?.split(',').filter(|w| !w.is_empty()) {
                    let sel = Sel::parse(word)?;
                    if !cfg.on.contains(&sel) && !cfg.on.iter_mut().any(|on| on.merge(&sel)) {
                        cfg.on.push(sel);
                    }
                }
            }
            "--rate" => {
                let (limit, window) = parse_rate(&value()?)?;
                cfg.limit = limit;
                cfg.window_ms = window;
            }
            "--since" => {
                cfg.since = match value()?.as_str() {
                    "head" => Since::Head,
                    "start" => Since::Start,
                    n => Since::At(
                        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                            .then(|| n.parse().ok())
                            .flatten()
                            .ok_or_else(|| format!("--since {n}: head, start or an offset"))?,
                    ),
                };
            }
            "--exec-timeout" => {
                let v = value()?;
                let ms: u64 = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| v.parse().ok())
                    .flatten()
                    .ok_or_else(|| "--exec-timeout takes milliseconds".to_string())?;
                cfg.exec_timeout = Duration::from_millis(ms);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if cfg.fleet.is_empty() || cfg.broker.is_empty() {
        return Err("--fleet and --broker are required".to_string());
    }
    if !is_fleet(&cfg.fleet) {
        return Err(format!(
            "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            cfg.fleet
        ));
    }
    if cfg.on.is_empty() {
        return Err("--on is required".to_string());
    }
    if cfg.exec.is_empty() {
        return Err("--exec is required".to_string());
    }
    if cfg.state_dir.is_empty() {
        cfg.state_dir = crate::cli::default_state_dir();
    }
    Ok(cfg)
}

fn parse_rate(s: &str) -> Result<(u32, u64), String> {
    let bad = || format!("--rate {s}: expected <n>/<window>, e.g. 10/1m, 1/30s, 40/1h");
    let (n, window) = s.split_once('/').ok_or_else(bad)?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let limit: u32 = n.parse().map_err(|_| bad())?;
    if limit == 0 {
        return Err(format!("--rate {s}: a limit of 0 drops every notification"));
    }
    let (digits, scale) = if let Some(digits) = window.strip_suffix('s') {
        (digits, 1_000)
    } else if let Some(digits) = window.strip_suffix('m') {
        (digits, 60_000)
    } else if let Some(digits) = window.strip_suffix('h') {
        (digits, 3_600_000)
    } else {
        return Err(bad());
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let count: u64 = digits.parse().map_err(|_| bad())?;
    let ms = count.checked_mul(scale).ok_or_else(bad)?;
    if ms == 0 {
        return Err(format!("--rate {s}: the window must be non-zero"));
    }
    Ok((limit, ms))
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "notify_store_custody_tests.rs"]
mod store_custody_tests;
