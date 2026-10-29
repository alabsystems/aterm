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
use crate::bridge::ACK_DEADLINE;
use crate::notify_need::{NeedWatch, Pair, Step};
use crate::subject::{is_fleet, is_principal};
use crate::transport::{self, Backoff, Closer, Conn};

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

const EXEC_POLL: Duration = Duration::from_millis(10);
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 10;
const DEFAULT_WINDOW_MS: u64 = 60_000;

const HOLD_MAX: usize = 4 << 20;

/// The development seam that has a following notifier tell the fabric bench the flushes its
/// store asked for ([`FLUSH_TALLY`]).
pub const FLUSH_TALLY_SEAM: &str = "ATERM_LINK_NOTIFY_FLUSH_TALLY";

/// The file under `<state>/notify/` a notifier on [`FLUSH_TALLY_SEAM`] rewrites, unflushed,
/// after each notice that flushed: `notify-flushes v=1 pid=<pid> durable=<n> ordered=<n>`, the
/// file and directory flushes its stores asked for since it started, by kind.
pub const FLUSH_TALLY: &str = "flush-tally";

/// The flushes this process's notify stores asked for ([`FLUSH_TALLY`]): full, then ordered.
static FLUSHES: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

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
/// whole ([`Notifier::save`]), in the order a rewrite writes them: a verdict ahead of the
/// stamps it spends and the count it bumps, those ahead of a standing value.
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
/// face's next record, in this run or after a restart.
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

#[derive(Clone)]
struct Store {
    root: PathBuf,
}

impl Store {
    fn open(state_dir: &str) -> io::Result<Self> {
        let root = Path::new(state_dir).join("notify");
        std::fs::create_dir_all(root.join("cur"))?;
        Ok(Self { root })
    }

    /// Replace `name` with `value` ([`Store::put`]), then flush its directory by `flush`
    /// ([`flush_dir`]). A [`Flush::Ordered`] write costs a repeat if a power loss takes it; a
    /// [`Flush::Durable`] one is the write's one full flush, and it puts on the media every
    /// write ordered before it on the volume.
    fn write(&self, name: &str, value: &str, flush: Flush) -> io::Result<()> {
        let dir = self.put(name, value)?;
        flush_dir(&dir, flush)
    }

    /// Write `value` beside `name`, order its bytes (`aterm_durable::ordered_or_fsync`) and
    /// rename it over `name`, and return the directory whose entry that moved, unflushed: the
    /// caller flushes it ([`Store::write`]).
    fn put(&self, name: &str, value: &str) -> io::Result<PathBuf> {
        #[cfg(test)]
        if PUT_FAULT.with(|left| {
            let dead = left.get() == Some(0);
            left.set(left.get().map(|n| n.saturating_sub(1)));
            dead
        }) {
            return Err(io::Error::other("PUT_FAULT: killed before this write"));
        }
        #[cfg(test)]
        if let Some(errno) = refused(name) {
            return Err(io::Error::from_raw_os_error(errno));
        }
        #[cfg(test)]
        PUT_NAMES.with(|names| {
            if let Some(names) = names.borrow_mut().as_mut() {
                names.push(name.to_string());
            }
        });
        let target = self.root.join(name);
        let tmp = target.with_extension("tmp");
        let dir = target.parent().unwrap_or(&self.root).to_path_buf();
        std::fs::create_dir_all(&dir)?;
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(value.as_bytes())?;
            f.write_all(b"\n")?;
            aterm_durable::ordered_or_fsync(&f)?;
            note_flush(Flush::Ordered);
        }
        std::fs::rename(&tmp, &target)?;
        Ok(dir)
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(name))
            .ok()
            .map(|s| s.trim_end_matches('\n').to_string())
            .filter(|s| !s.is_empty())
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
        let path = self.root.join("log");
        let owner = match std::fs::read_to_string(&path) {
            Ok(saved) => Some(
                astream_broker::LogIdentity::parse(saved.trim_end_matches('\n')).ok_or_else(
                    || {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("{}: invalid notifier log identity", path.display()),
                        )
                    },
                )?,
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(&path) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => None,
                    Err(other) => return Err(other),
                    Ok(_) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        match owner {
            None => {
                self.write("log", &name, Flush::Ordered)?;
                Ok(self.clone())
            }
            Some(owner) if owner == log => Ok(self.clone()),
            Some(owner) => {
                let root = self.root.join("logs").join(&name);
                if !root.exists() {
                    if let Some((from, fork)) = self.predecessor(owner, log, lineage)? {
                        self.carry(&from, &root, fork)?;
                    }
                }
                std::fs::create_dir_all(root.join("cur"))?;
                if let Err(e) = self.prune_logs(&name) {
                    eprintln!(
                        "aterm-link notify: could not prune the stores of old logs under {}: {e}",
                        self.root.join("logs").display()
                    );
                }
                Ok(Store { root })
            }
        }
    }

    fn predecessor(
        &self,
        owner: astream_broker::LogIdentity,
        log: astream_broker::LogIdentity,
        lineage: Option<&astream_broker::Lineage>,
    ) -> io::Result<Option<(Store, u64)>> {
        let Some(lineage) = lineage else {
            return Ok(None);
        };
        let mut held = vec![(owner, self.root.clone())];
        match std::fs::read_dir(self.root.join("logs")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    let name = entry.file_name();
                    if let Some(id) = name.to_str().and_then(astream_broker::LogIdentity::parse) {
                        held.push((id, entry.path()));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(held
            .into_iter()
            .filter(|(earlier, _)| *earlier != log)
            .filter_map(|(earlier, root)| {
                let fork = log.continues(earlier, lineage)?;
                Some((earlier.generation, Store { root }, fork))
            })
            .max_by_key(|(generation, _, _)| *generation)
            .map(|(_, store, fork)| (store, fork)))
    }

    fn carry_text(&self, name: &str) -> io::Result<Option<String>> {
        let path = self.root.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(&path) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    fn carry(&self, from: &Store, to: &Path, fork: u64) -> io::Result<()> {
        let logs = to.parent().unwrap_or(&self.root).to_path_buf();
        std::fs::create_dir_all(&logs)?;
        let scratch = aterm_tempfile::Builder::new()
            .prefix(".carry-")
            .tempdir_in(&logs)?;
        let built = Store {
            root: scratch.path().to_path_buf(),
        };
        std::fs::create_dir(built.root.join("cur"))?;
        let carried = (|| -> io::Result<()> {
            match std::fs::read_dir(from.root.join("cur")) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry?;
                        let name = entry.file_name();
                        let Some(key) = name.to_str().filter(|k| !k.contains('.')) else {
                            continue;
                        };
                        let text = std::fs::read_to_string(entry.path())?;
                        let off = text.trim().parse::<u64>().map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid carried notifier cursor",
                            )
                        })?;
                        built.write(
                            &format!("cur/{key}"),
                            &off.min(fork).to_string(),
                            Flush::Ordered,
                        )?;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(from.root.join("cur")) {
                        Err(missing) if missing.kind() == io::ErrorKind::NotFound => {}
                        Err(other) => return Err(other),
                        Ok(_) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
            let journal = from.carry_text("journal")?.unwrap_or_default();
            let below: Vec<String> = journal
                .lines()
                .filter(|line| {
                    line.split_whitespace()
                        .next()
                        .and_then(|t| t.parse::<u64>().ok())
                        .is_some_and(|off| off < fork)
                })
                .map(str::to_string)
                .collect();
            if !below.is_empty() {
                built.set_journal(&below, Flush::Ordered)?;
            }
            if let Some(standing) = from.carry_text("standing")? {
                if !standing.is_empty() {
                    built.write("standing", &standing, Flush::Ordered)?;
                }
            }
            flush_dir(&built.root.join("cur"), Flush::Durable)?;
            flush_dir(&built.root, Flush::Durable)
        })();
        carried?;
        match std::fs::rename(scratch.path(), to) {
            Ok(()) => {
                let _ = scratch.keep();
                flush_dir(&logs, Flush::Durable)
            }
            Err(_) if to.join("cur").is_dir() => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Keep only the newest [`LOGS_KEEP`] stores under `logs/<identity>/`.
    fn prune_logs(&self, name: &str) -> io::Result<()> {
        let mut used: Vec<String> = self
            .read("logs/used")
            .map(|text| {
                text.lines()
                    .filter(|l| *l != name && astream_broker::LogIdentity::parse(l).is_some())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        used.push(name.to_string());
        let keep = used.split_off(used.len().saturating_sub(LOGS_KEEP));
        self.write("logs/used", &keep.join("\n"), Flush::Ordered)?;
        for entry in std::fs::read_dir(self.root.join("logs"))? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(id) = file_name
                .to_str()
                .filter(|n| astream_broker::LogIdentity::parse(n).is_some())
            else {
                continue;
            };
            if !keep.iter().any(|k| k == id) {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    fn cursor(&self, key: &str) -> Option<u64> {
        self.read(&format!("cur/{key}"))
            .and_then(|s| s.trim().parse().ok())
    }

    /// Move `key`'s cursor forward to `off` (never back), ordered: a cursor a power loss takes
    /// re-reads records the journal and the standing values dedup.
    fn set_cursor(&self, key: &str, off: u64) -> io::Result<()> {
        if self.cursor(key).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write(&format!("cur/{key}"), &off.to_string(), Flush::Ordered)
    }

    fn journal(&self) -> Vec<String> {
        self.read("journal")
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn set_journal(&self, lines: &[String], flush: Flush) -> io::Result<()> {
        self.write("journal", &lines.join("\n"), flush)
    }

    fn stamps(&self) -> Vec<u64> {
        self.read("rate")
            .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
            .unwrap_or_default()
    }

    /// Put the rate stamps in place, ordered, and return the directory whose entry moved,
    /// unflushed ([`Notifier::charge`] flushes it with the page's `firing` line).
    fn set_stamps(&self, stamps: &[u64]) -> io::Result<PathBuf> {
        let joined: Vec<String> = stamps.iter().map(u64::to_string).collect();
        self.put("rate", &joined.join(","))
    }

    /// `<subject> <attention token>` per line, the oldest first ([`escalation`]).
    fn standing(&self) -> Vec<(String, String)> {
        self.read("standing")
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let (subject, token) = l.split_once(' ')?;
                        (!subject.is_empty() && !token.is_empty() && !token.contains(' '))
                            .then(|| (subject.to_string(), token.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_standing(&self, rows: &[(String, String)]) -> io::Result<()> {
        let lines: Vec<String> = rows.iter().map(|(s, t)| format!("{s} {t}")).collect();
        self.write("standing", &lines.join("\n"), Flush::Ordered)
    }

    /// The suppressed count, and the `t=` of the newest `dropped` line it counts (0 for none):
    /// `<n> t=<mark>` ([`Notifier::restore`]).
    fn suppressed(&self) -> (u64, u64) {
        let text = self.read("suppressed").unwrap_or_default();
        let mut words = text.split_whitespace();
        let n = words.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let mark = words
            .find_map(|w| w.strip_prefix("t="))
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        (n, mark)
    }

    fn set_suppressed(&self, n: u64, mark: u64) -> io::Result<()> {
        self.write("suppressed", &format!("{n} t={mark}"), Flush::Ordered)
    }
}

/// Flush `dir`'s entries by `flush`, going on where they cannot be flushed here by astream's
/// one rule (`astream_sys::dir_flush_unavailable`, which `crate::state::sync_dir_as` keeps too).
/// It takes the flush through `aterm_durable` in a test build as well, so a test's recording
/// sees it.
fn flush_dir(dir: &Path, flush: Flush) -> io::Result<()> {
    note_flush(flush);
    flush.dir_best_effort(dir).map(drop)
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
        if store.root.join("fault-fired").exists() {
            return Fault::None;
        }
        match fault.to_str() {
            Some("kill-after-exec") => Fault::KillAfterExec,
            _ => Fault::None,
        }
    }

    fn fire(self, store: &Store) -> ! {
        let _ = std::fs::write(store.root.join("fault-fired"), b"1\n");
        eprintln!("aterm-link notify: ATERM_LINK_NOTIFY_FAULT={self:?}: dying");
        aterm_uds::process::kill_self_now()
    }
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
    store: Store,
    /// The store of the log this notifier is reading NOW ([`Store::for_log`]).
    log_store: Store,
    log_now: Option<astream_broker::LogIdentity>,
    /// The log each follow-mode reader's connection last reached ([`Notice::Log`]).
    reader_log: Vec<ReadLog>,
    journal: Vec<String>,
    /// The log in hand's `standing` ([`Store::standing`]).
    standing: Vec<(String, String)>,
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
    /// key's cursor is written at it before the values are ([`Notifier::put_standing`]).
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
    /// Where [`FLUSH_TALLY_SEAM`] has the flushes told, and the counts last told there.
    flush_tally: Option<(PathBuf, (u64, u64))>,
}

impl Notifier {
    fn new(cfg: Config) -> io::Result<Self> {
        let store = Store::open(&cfg.state_dir)?;
        let fault = Fault::from_env(&store);
        let flush_tally = aterm_types::dev_seam!(FLUSH_TALLY_SEAM)
            .map(|_| (store.root.join(FLUSH_TALLY), (u64::MAX, u64::MAX)));
        let n = cfg.on.len();
        let need = NeedWatch::new(&cfg.fleet);
        let (suppressed, suppressed_mark) = store.suppressed();
        Ok(Self {
            journal: store.journal(),
            standing: store.standing(),
            stamps: store.stamps(),
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
            flush_tally,
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

    /// Rewrite the [`FLUSH_TALLY`] where its seam asked for it and the counts moved.
    fn tell_flushes(&mut self) {
        let Some((path, told)) = self.flush_tally.as_mut() else {
            return;
        };
        let now = flushes();
        if *told == now {
            return;
        }
        *told = now;
        let tally = format!(
            "notify-flushes v=1 pid={} durable={} ordered={}\n",
            std::process::id(),
            now.0,
            now.1
        );
        if let Err(e) = std::fs::write(&*path, tally) {
            warn(&format!("{FLUSH_TALLY_SEAM}: {e}"));
        }
    }

    /// One broker connection with every capability attached ([`ACK_DEADLINE`]).
    fn connect(&self) -> io::Result<Conn> {
        let (mut conn, _closer) = transport::connect(&self.cfg.broker, ACK_DEADLINE)?;
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
            self.journal = scoped.journal();
            self.standing = scoped.standing();
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
        let Ok((mut conn, _closer)) = transport::connect(&self.cfg.broker, ACK_DEADLINE) else {
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
            let at: u64 = it.next()?.parse().ok()?;
            (at == off).then(|| it.next()).flatten()
        })
    }

    fn told(&self, off: u64) -> bool {
        self.journal.iter().any(|line| {
            let mut it = line.split_whitespace();
            it.next().and_then(|t| t.parse::<u64>().ok()) == Some(off)
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
    /// `off` that selector `key` is judging says. A change is written at once, ordered, after
    /// `key`'s cursor is written AT `off` ([`Notifier::put_standing`]): whatever a power loss
    /// keeps of the two, the selector re-reads no record but `off` under a standing value that a
    /// later record set, so a value a record left standing never pages it again. Until the
    /// change is written (the state dir refused it, [`Notifier::save`]), no cursor is written
    /// ([`Notifier::checkpoint`]) and no later record is judged ([`Notifier::owing`]), so a
    /// second change never waits behind it: one cursor write could not keep the law for both.
    fn set_standing(&mut self, key: &str, off: u64, subject: &str, token: Option<&str>) {
        if self.standing_of(subject) == token {
            return;
        }
        self.standing.retain(|(s, _)| s != subject);
        if let Some(token) = token {
            self.standing.push((subject.to_string(), token.to_string()));
        }
        while self.standing.len() > STANDING_KEEP {
            let at = self
                .standing
                .iter()
                .position(|(s, _)| !self.need.holds(s))
                .unwrap_or(0);
            self.standing.remove(at);
        }
        self.standing_at.entry(key.to_string()).or_insert(off);
        self.save(Part::Standing);
    }

    /// Note one verdict in the journal, replacing the offset's old one, keeping the newest
    /// [`JOURNAL_KEEP`]: the verdict is the journal's newest line ([`Notifier::restore`]).
    fn note_verdict(&mut self, off: u64, line: String) {
        self.journal.retain(|l| {
            l.split_whitespace()
                .next()
                .and_then(|t| t.parse::<u64>().ok())
                != Some(off)
        });
        self.journal.push(line);
        while self.journal.len() > JOURNAL_KEEP {
            self.journal.remove(0);
        }
    }

    /// Write one verdict ([`Notifier::note_verdict`]): the journal, its directory flushed
    /// ordered ([`Notifier::save`]). A page's `firing` line goes through [`Notifier::arm`].
    fn record(&mut self, off: u64, line: String) {
        self.note_verdict(off, line);
        self.save(Part::Journal);
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
    /// the newest `limit` stamps, and write them ([`Notifier::put_stamps`]).
    fn charge(&mut self, t: u64) -> io::Result<()> {
        if !self.stamps.contains(&t) {
            self.stamps.push(t);
            self.trim_stamps();
        }
        self.put_stamps()
    }

    /// Keep the newest `limit` stamps.
    fn trim_stamps(&mut self) {
        let limit = self.cfg.limit as usize;
        while self.stamps.len() > limit {
            self.stamps.remove(0);
        }
    }

    /// Write the stamps, ordered after the page's `firing` line and ahead of the full flush of
    /// the journal's directory ([`Notifier::arm`]): that flush covers their rename where the
    /// two share a directory, and their own directory is flushed otherwise, in full only where
    /// it is on another device. A kill between the line and the stamps leaves the line pending
    /// with its stamp unspent, and [`Notifier::restore`] spends it.
    fn put_stamps(&self) -> io::Result<()> {
        let dir = self.store.set_stamps(&self.stamps)?;
        if dir != self.log_store.root {
            flush_dir(
                &dir,
                Flush::ordered_if_same_device(&dir, &self.log_store.root),
            )?;
        }
        Ok(())
    }

    /// Count one drop whose `dropped` line, `t=mark`, is noted: the count is written after its
    /// line ([`Part`]), so a kill between the two leaves the line uncounted, and
    /// [`Notifier::restore`] counts it.
    fn count_drop(&mut self, mark: u64) {
        self.suppressed += 1;
        self.suppressed_mark = mark;
        self.save(Part::Suppressed);
    }

    /// Finish what a kill cut short after the newest verdict of the log in hand, before its
    /// first record is judged: a verdict is written ahead of what it spends or counts, so only
    /// that one can be ahead of it. A `firing` line whose `t=` the stamps do not hold spends
    /// it ([`Notifier::charge`]); a `dropped` line whose `t=` is past the newest drop the
    /// suppressed count holds is counted ([`Notifier::count_drop`]). Otherwise it writes
    /// nothing.
    fn restore(&mut self) {
        let Some(newest) = self.journal.last() else {
            return;
        };
        let mut words = newest.split_whitespace();
        let verdict = words.nth(1);
        let Some(t) = words
            .find_map(|w| w.strip_prefix("t="))
            .and_then(|t| t.parse::<u64>().ok())
        else {
            return;
        };
        match verdict {
            Some("firing") if !self.stamps.contains(&t) => {
                let at = self.stamps.partition_point(|&s| s < t);
                self.stamps.insert(at, t);
                self.trim_stamps();
                self.save(Part::Stamps);
            }
            Some("dropped") if t > self.suppressed_mark => self.count_drop(t),
            _ => {}
        }
    }

    /// Mark `part` changed in memory, and write what is owed at once ([`Notifier::rewrite`]),
    /// unless the state dir refused a write and its retry is not due: then the next settle
    /// past it writes it ([`Notifier::settle`]).
    fn save(&mut self, part: Part) {
        self.dirty.insert(part);
        let now = self.now();
        if self.writable(now) {
            let _ = self.rewrite(now);
        }
    }

    /// Write each changed part whole, in [`Part`]'s order (a standing value through
    /// [`Notifier::put_standing`]), stopping at the first the state dir refuses
    /// ([`Notifier::fail`]), so no part reaches the store ahead of one before it.
    fn rewrite(&mut self, now: Instant) -> io::Result<()> {
        while let Some(&part) = self.dirty.first() {
            let written = match part {
                Part::Journal => self
                    .log_store
                    .put("journal", &self.journal.join("\n"))
                    .and_then(|dir| flush_dir(&dir, Flush::Ordered)),
                Part::Stamps => self.put_stamps(),
                Part::Suppressed => self
                    .store
                    .set_suppressed(self.suppressed, self.suppressed_mark),
                Part::Standing => self.put_standing(),
            };
            if let Err(e) = written {
                self.fail(&e, now, part.what());
                return Err(e);
            }
            self.dirty.remove(&part);
        }
        Ok(())
    }

    /// Write the standing values, each selector's cursor first at the record that changed one
    /// ([`Notifier::set_standing`]), forgetting the cursor it owed up to there.
    fn put_standing(&mut self) -> io::Result<()> {
        let at: Vec<(String, u64)> = self
            .standing_at
            .iter()
            .map(|(key, &at)| (key.clone(), at))
            .collect();
        for (key, at) in at {
            self.log_store.set_cursor(&key, at)?;
            if self.lag.get(&key).is_some_and(|lag| lag.want <= at) {
                self.lag.remove(&key);
            }
        }
        self.log_store.set_standing(&self.standing)?;
        self.standing_at.clear();
        Ok(())
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
    /// record: a standing value written after its selector's cursor at its record
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
            match self.log_store.set_cursor(key, off) {
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
            if let Err(e) = self.log_store.set_cursor(&key, want) {
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
    /// taking the fold's pair it matches; a session that exited without its close keeps none; a
    /// node's will keeps them, so a reattach that restores the same needs pages nothing. The
    /// hit, and the pair it leaves standing once its command ran (or owed when it did not), is
    /// the most urgent need `sel` selects that rose where the watch saw it and is no pair
    /// standing: one a whole fold names, or one whose raise a partial fold holds with nothing
    /// lost after it.
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
        let selected = |n: &&crate::notify_need::Named| sel.selects(n.pair.word);
        let Some(rose) = needs.iter().filter(selected).find(|n| n.rose && !stands(n)) else {
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
                    self.record(
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
        let carried = self.suppressed;
        let verdict = self.exec(&hit, dup, carried);
        if self.fault == Fault::KillAfterExec {
            self.fault.fire(&self.store);
        }
        self.record(
            off,
            format!("{off} fired on={} {verdict} dup={}", hit.on, u8::from(dup)),
        );
        if carried > 0 && verdict == "rc=0" {
            self.suppressed -= carried;
            self.save(Part::Suppressed);
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
    /// its command runs: the line and then the stamps written ordered ([`Notifier::charge`]),
    /// then the journal's directory flushed in full ([`Self::firing`]), the page's one full
    /// flush, so a power loss mid-command runs it again with `ATERM_NOTIFY_DUP=1`. Refused, it
    /// leaves the journal and the stamps in memory as they were, so the page, done again, writes
    /// the same line and spends its stamp once.
    fn arm(&mut self, off: u64, line: String, t: u64) -> io::Result<()> {
        let (journal, stamps) = (self.journal.clone(), self.stamps.clone());
        self.note_verdict(off, line);
        let armed = self.put_firing(t);
        if armed.is_err() {
            self.journal = journal;
            self.stamps = stamps;
        } else {
            self.dirty.remove(&Part::Journal);
            self.dirty.remove(&Part::Stamps);
        }
        armed
    }

    /// [`Notifier::arm`]'s writes: the journal, the stamps with `t` spent, the full flush.
    fn put_firing(&mut self, t: u64) -> io::Result<()> {
        let dir = self.log_store.put("journal", &self.journal.join("\n"))?;
        self.charge(t)?;
        flush_dir(&dir, self.firing)
    }

    fn exec(&self, hit: &Hit, dup: bool, suppressed: u64) -> String {
        #[cfg(test)]
        AT_EXEC.with(|seam| {
            if let Some(seen) = seam.borrow_mut().as_mut() {
                seen(hit);
            }
        });
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.arg("-c")
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
        let deadline = Instant::now() + self.cfg.exec_timeout;
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
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!(
                    "aterm-link notify: --exec timed out for off={}; killed",
                    hit.offset
                );
                return "rc=timeout".to_string();
            }
            std::thread::sleep(EXEC_POLL);
        }
    }

    fn start_offset(&self, conn: &mut Conn, sel: &Sel) -> io::Result<u64> {
        if let Some(off) = self.log_store.cursor(&sel.key()) {
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
    /// part the state dir refused is not judged ([`Notifier::owing`]).
    fn catch_up(&mut self) -> io::Result<()> {
        let caught = self.catch_up_to_head();
        let persisted = self.persist();
        caught.and(persisted)
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
            let root = self.store.root.clone();
            std::thread::Builder::new()
                .name(format!("notify-{}", sel.key()))
                .spawn(move || reader(idx, &sel, &cfg, &root, &tx))?;
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
    fn serve(&mut self, rx: &mpsc::Receiver<Notice>) -> io::Result<()> {
        loop {
            if self.stalled {
                let wait = self.failing.map_or(Duration::ZERO, |f| {
                    f.next.saturating_duration_since(self.now())
                });
                std::thread::sleep(wait);
                let _ = self.tick(self.now());
                self.tell_flushes();
                continue;
            }
            let notice = match self.next_due() {
                None => rx.recv().ok(),
                Some(due) => match rx.recv_timeout(due.saturating_duration_since(self.now())) {
                    Ok(notice) => Some(notice),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let _ = self.tick(self.now());
                        self.tell_flushes();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                },
            };
            let Some(notice) = notice else {
                break;
            };
            let _ = self.take(notice);
            self.tell_flushes();
        }
        let _ = self.persist();
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "every subscription ended",
        ))
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
                        eprintln!(
                            "aterm-link notify: {} holds {} bytes the broker has not said are \
                             durable — past {HOLD_MAX}, so they are paged now, before it does",
                            sel.label(),
                            self.held_bytes + size
                        );
                    } else if self.held[idx].is_empty() {
                        eprintln!(
                            "aterm-link notify: {} off={off} waits until the broker says it \
                             is durable (it serves ordered: its horizon plus one batch \
                             and one full flush)",
                            sel.label()
                        );
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
        if self.verdicts() == verdicts {
            self.advance(&sel.key(), off + 1);
        } else {
            self.checkpoint(&sel.key(), off + 1);
        }
        Ok(())
    }
}

fn reader(idx: usize, sel: &Sel, cfg: &Config, root: &Path, tx: &mpsc::SyncSender<Notice>) {
    let filter = sel.filter(&cfg.fleet);
    let key = sel.key();
    let mut seen: Option<u64> = None;
    let mut seen_on: Option<PathBuf> = None;
    let mut backoff = Backoff::default();
    loop {
        let wait = match connect_for_reader(cfg, root, &key, &filter, &mut seen, &mut seen_on) {
            Ok((mut conn, closer, mut from, log)) => {
                if tx.send(Notice::Log(idx, log)).is_err() {
                    return;
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
                        seen = Some(seen.map_or(off + 1, |s| s.max(off + 1)));
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
                                        seen = Some(seen.map_or(off + 1, |s| s.max(off + 1)));
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
                        Err(e) => eprintln!(
                            "aterm-link notify: {} could not subscribe: {e}",
                            sel.label()
                        ),
                    }
                }
                backoff.lost(Instant::now())
            }
            Err(e) => {
                eprintln!(
                    "aterm-link notify: {} could not subscribe: {e}",
                    sel.label()
                );
                Some(backoff.failed())
            }
        };
        if let Some(wait) = wait {
            std::thread::sleep(wait);
        }
    }
}

fn connect_for_reader(
    cfg: &Config,
    root: &Path,
    key: &str,
    filter: &str,
    seen: &mut Option<u64>,
    seen_on: &mut Option<PathBuf>,
) -> io::Result<(Conn, Closer, u64, ReadLog)> {
    let (mut conn, closer) = transport::connect(&cfg.broker, ACK_DEADLINE)?;
    transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
    let log = log_of(&mut conn, cfg)?;
    let store = Store {
        root: root.to_path_buf(),
    }
    .for_log(log.id, log.lineage.as_ref())?;
    if seen_on.as_ref().is_some_and(|on| *on != store.root) {
        *seen = None;
    }
    *seen_on = Some(store.root.clone());
    let seen = *seen;
    let from = match (store.cursor(key), seen) {
        (Some(c), Some(s)) => c.max(s),
        (Some(c), None) => c,
        (None, Some(s)) => s,
        (None, None) => match cfg.since {
            Since::Start => 0,
            Since::At(n) => n,
            Since::Head => {
                let head = conn.fetch(0, filter, 0)?.1 .1;
                store.set_cursor(key, head)?;
                head
            }
        },
    };
    Ok((conn, closer, from, log))
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
    /// The [`Store::put`]s left before a test's kill: at `Some(0)` every put fails before it
    /// writes, as if the process had died there.
    static PUT_FAULT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
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
    /// What the backoff warned of on this thread ([`Notifier::fail`], [`Notifier::settle`]).
    static WARNED: std::cell::RefCell<Vec<Warned>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A warning of the backoff, as [`WARNED`] keeps it: a refusal and its errno, or its end.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Warned {
    Refused(Option<i32>),
    Cleared,
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
  --state <dir>          state dir (`<dir>/notify/` holds cursors and the journal)

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
session's next record, in the same run or the next (`--once` too). A session whose own row
says it exited before its close reached the bus has no need; a session's close raises
`exited`, which pages once, whether the close or the exited row reached the bus first. A
node's will (its row's `state=away`, or the legacy `gone`) pages and clears nothing, so a
reattach that restores the same needs pages nothing. A session whose fold is partial (the
brief's `partial`: no start, lost records, or a snapshot short of its needs) clears nothing,
and pages a need only when it holds the record that raised it and lost nothing after it; a
need only a snapshot names never pages. A need a consumer claims
(`aterm ctl @<sid> mark - need word=<review|decision|help>`) never pages:
`aterm ctl @<sid> await log need` waits on it in the session's own log. On an `ordered`
broker a record waits until it is durable; `--once` leaves one not yet durable for the next
run. Prints `notify: fired= dup= skipped= dropped= suppressed= standing=` on exit. Exits 2
on a usage error, an unreadable --cap-file, or a broker that refuses the cap files or a
selector's read (no --cap-file grants it); 1 when the state dir cannot open or the run
fails.
";

/// `aterm link notify --help`: [`USAGE`] with its cursor lag filled in ([`CURSOR_LAG`]), then
/// the words and families `need:<word>` takes ([`need_words`], [`need_families`]).
#[must_use]
pub fn usage() -> String {
    format!(
        "{}\nNeed words (`need:<word>`, the needs a session's fold raises):\n{}\n\
         Need families (`need:<family>`, each `<family>:<kind>` word above):\n{}\n",
        USAGE.replace("{CURSOR_LAG}", &CURSOR_LAG.to_string()),
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
#[must_use]
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
                        n.parse()
                            .map_err(|_| format!("--since {n}: head, start or an offset"))?,
                    ),
                };
            }
            "--exec-timeout" => {
                let ms: u64 = value()?
                    .parse()
                    .map_err(|_| "--exec-timeout takes milliseconds".to_string())?;
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
    let count: u64 = digits.parse().map_err(|_| bad())?;
    let ms = count.checked_mul(scale).ok_or_else(bad)?;
    if ms == 0 {
        return Err(format!("--rate {s}: the window must be non-zero"));
    }
    Ok((limit, ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_handoff_buffers_no_row_for_a_stalled_executor() {
        let (tx, _rx) = notice_handoff();
        let row = Notice::Record(0, 41, String::from("first"), vec![1; 1024]);
        assert!(
            matches!(tx.try_send(row), Err(mpsc::TrySendError::Full(_))),
            "a stalled executor must leave no queued row"
        );
    }

    #[test]
    fn a_subject_segment_that_is_not_a_principal_never_becomes_an_environment_value() {
        let huge = "x".repeat(200_000);
        let bidi = "s-\u{202e}gnitset";

        let ask = Sel::Ask("h-andrew".into());
        assert!(ask
            .hit(
                "f1",
                7,
                "/f/f1/in/p/h-andrew/n-a/ask",
                b"v=1 t=1 from=s-b text=hi"
            )
            .is_some());
        for bad in [
            format!("/f/f1/in/p/h-andrew/{huge}/ask"),
            format!("/f/f1/in/p/h-andrew/{bidi}/ask"),
            "/f/f1/in/p/h-andrew/NODE/ask".to_string(),
        ] {
            assert_eq!(ask.hit("f1", 7, &bad, b"v=1 t=1"), None, "{}", &bad[..40]);
        }
        for from in [huge.as_str(), bidi, "h-b"] {
            let body = format!("v=1 t=1 from={from} text=hi");
            let hit = ask
                .hit("f1", 7, "/f/f1/in/p/h-andrew/n-a/ask", body.as_bytes())
                .expect("the subject is honest");
            assert_eq!(hit.sid, "", "{}", &from[..from.len().min(40)]);
        }

        let att = Sel::Attention;
        let body = b"v=1 t=1 state=live attention=needs%20a%20key";
        assert!(att
            .hit("f1", 3, "/f/f1/pub/n-a/s-b/presence", body)
            .is_some());
        for bad in [
            format!("/f/f1/pub/n-a/{huge}/presence"),
            format!("/f/f1/pub/{huge}/s-b/presence"),
            format!("/f/f1/pub/n-a/{bidi}/presence"),
        ] {
            assert_eq!(att.hit("f1", 3, &bad, body), None, "{}", &bad[..40]);
        }

        let halt = Sel::Halt;
        let on = b"v=1 t=1 state=on reason=main%20broken";
        assert!(halt.hit("f1", 4, "/f/f1/fleet/h-andrew/halt", on).is_some());
        assert_eq!(
            halt.hit("f1", 4, &format!("/f/f1/fleet/{huge}/halt"), on),
            None
        );

        let hit = ask
            .hit(
                "f1",
                7,
                "/f/f1/in/p/h-andrew/n-a/ask",
                b"v=1 t=1 from=s-b text=hi",
            )
            .expect("the honest ask");
        for v in [&hit.subject, &hit.node, &hit.who, &hit.sid, &hit.text] {
            assert!(v.len() <= TEXT_MAX, "{v} is {} bytes", v.len());
            assert!(
                v.chars().all(|c| c.is_ascii_graphic() || c == ' '),
                "{v} is not printable ASCII"
            );
        }
    }

    fn scratch(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("atlink-notify-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p.to_string_lossy().into_owned()
    }

    fn cfg(dir: &str) -> Config {
        Config {
            fleet: "f1".into(),
            broker: String::new(),
            cap_files: Vec::new(),
            state_dir: dir.to_string(),
            on: vec![Sel::Attention],
            exec: "true".into(),
            limit: 2,
            window_ms: 60_000,
            since: Since::Head,
            once: true,
            exec_timeout: DEFAULT_EXEC_TIMEOUT,
        }
    }

    #[test]
    fn a_selector_matches_by_position_and_an_overlong_subject_never_matches() {
        let ask = Sel::Ask("h-andrew".into());
        let honest = "/f/f1/in/p/h-andrew/n-a/ask";
        let hit = ask
            .hit("f1", 7, honest, b"v=1 t=1 from=s-b text=is%20this%20ok%3F")
            .expect("the honest ask to h-andrew matches");
        assert_eq!(
            (hit.who.as_str(), hit.node.as_str(), hit.sid.as_str()),
            ("h-andrew", "n-a", "s-b")
        );
        assert_eq!(hit.text, "is this ok?");
        for bad in [
            "/f/f1/in/n-a/s-b/h-andrew/ask",
            "/f/f1/in/p/h-andrew/n-a/s-1/ask",
            "/f/f1/in/p/h-other/n-a/ask",
            "/f/f1/in/p/h-andrew/n-a/note",
            "/f/f2/in/p/h-andrew/n-a/ask",
            "/f/f1/pub/p/h-andrew/n-a/ask",
            "/f/f1/in/n-a/h-andrew/n-b/ask",
        ] {
            assert_eq!(ask.hit("f1", 7, bad, b"v=1 t=1"), None, "{bad}");
        }
    }

    #[test]
    fn attention_fires_only_on_a_presence_row_that_carries_one() {
        let sel = Sel::Attention;
        let subject = "/f/f1/pub/n-a/s-b/presence";
        let hit = sel
            .hit(
                "f1",
                3,
                subject,
                b"v=1 t=1 state=live attention=needs%20a%20key",
            )
            .expect("a presence row with an attention matches");
        assert_eq!(hit.text, "needs a key");
        assert_eq!((hit.node.as_str(), hit.who.as_str()), ("n-a", "s-b"));
        for body in [
            &b"v=1 t=1 state=live"[..],
            b"v=1 t=1 attention=",
            b"v=1 t=1 attention=-",
        ] {
            assert_eq!(sel.hit("f1", 3, subject, body), None);
        }
        let halt = Sel::Halt;
        let lane = "/f/f1/fleet/h-andrew/halt";
        assert!(halt
            .hit("f1", 4, lane, b"v=1 t=1 state=on reason=main%20broken")
            .is_some());
        assert_eq!(halt.hit("f1", 4, lane, b"v=1 t=1 state=off"), None);
        for lane in ["/f/f1/fleet/a-svc/halt", "/f/f1/fleet/n-node/halt"] {
            assert_eq!(
                halt.hit("f1", 4, lane, b"v=1 t=1 state=on reason=stop"),
                None,
                "{lane}"
            );
        }
    }

    #[test]
    fn untrusted_text_is_flattened_and_capped() {
        let text = one_line("hi\x1b[2Jthere\nand\ta tab");
        assert_eq!(text, "hi.[2Jthere.and.a tab");
        assert_eq!(one_line(&"x".repeat(400)).len(), TEXT_MAX);
    }

    #[test]
    fn the_budget_is_a_sliding_window_over_the_stamps_it_persisted() {
        let dir = scratch("budget");
        let mut n = Notifier::new(cfg(&dir)).expect("open");
        assert!(n.has_room(1_000));
        n.charge(1_000).expect("charge");
        assert!(n.has_room(1_100));
        n.charge(1_100).expect("charge");
        assert!(!n.has_room(1_200), "two in the window is the limit of 2");
        assert!(n.has_room(61_050), "the 1_000 stamp has left the window");
        let mut back = Notifier::new(cfg(&dir)).expect("reopen");
        assert!(
            !back.has_room(1_200),
            "a restart does not hand the budget back"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn appending(dir: &str, limit: u32, exec: &str) -> (Notifier, PathBuf) {
        std::fs::create_dir_all(dir).expect("scratch");
        let fired = Path::new(dir).join("fired");
        let mut c = cfg(dir);
        c.limit = limit;
        c.exec = format!(
            "{exec}printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        (Notifier::new(c).expect("open"), fired)
    }

    fn fired_offsets(fired: &Path) -> Vec<u64> {
        std::fs::read_to_string(fired)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    fn feed(n: &mut Notifier, off: u64, sid: &str, body: &str) {
        let subject = format!("/f/f1/pub/n-a/{sid}/presence");
        n.on_record(&Sel::Attention, off, &subject, body.as_bytes())
            .expect("on_record");
    }

    #[test]
    fn a_standing_attention_pages_once_per_transition_not_once_per_offset() {
        let dir = scratch("standing");
        let (mut n, fired) = appending(&dir, 100, "");
        let key = "attention=needs%20a%20key";
        feed(
            &mut n,
            1,
            "s-one",
            &format!("v=1 t=1 state=live inc=1 {key}"),
        );
        feed(
            &mut n,
            2,
            "s-one",
            &format!("v=1 t=2 state=live inc=2 {key}"),
        );
        feed(
            &mut n,
            3,
            "s-one",
            &format!("v=1 t=3 inc=2 phase=busy {key}"),
        );
        feed(
            &mut n,
            4,
            "s-one",
            &format!("v=1 t=4 state=exited inc=2 {key}"),
        );
        feed(
            &mut n,
            5,
            "s-two",
            &format!("v=1 t=5 state=live inc=1 {key}"),
        );
        assert_eq!(fired_offsets(&fired), [1, 5]);
        assert_eq!((n.counts.fired, n.counts.standing), (2, 3));
        for off in [2, 3, 4] {
            assert_eq!(n.verdict(off), None, "a republish is not a verdict");
        }
        assert_eq!(n.stamps.len(), 2, "and charges no budget");

        feed(&mut n, 6, "s-one", "v=1 t=6 state=live inc=3 attention=-");
        feed(
            &mut n,
            7,
            "s-one",
            &format!("v=1 t=7 state=live inc=3 {key}"),
        );
        assert_eq!(fired_offsets(&fired), [1, 5, 7], "a clear re-arms it");

        let (mut back, _) = appending(&dir, 100, "");
        feed(
            &mut back,
            8,
            "s-one",
            &format!("v=1 t=8 state=live inc=4 {key}"),
        );
        assert_eq!(fired_offsets(&fired), [1, 5, 7], "a restart remembers");
        feed(
            &mut back,
            9,
            "s-one",
            "v=1 t=9 attention=a%20new%20question",
        );
        feed(&mut back, 10, "s-one", &format!("v=1 t=10 {key}"));
        assert_eq!(
            fired_offsets(&fired),
            [1, 5, 7, 9, 10],
            "a value that moved is a new escalation, and so is moving back"
        );

        feed(&mut back, 11, "s-two", "v=1 t=11 attention=-");
        feed(&mut back, 12, "s-two", &format!("v=1 t=12 {key}"));
        feed(&mut back, 11, "s-two", "v=1 t=11 attention=-");
        feed(&mut back, 12, "s-two", &format!("v=1 t=12 {key}"));
        feed(&mut back, 13, "s-two", &format!("v=1 t=13 inc=2 {key}"));
        assert_eq!(fired_offsets(&fired), [1, 5, 7, 9, 10, 12]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_escalation_nobody_was_told_of_is_offered_again_on_its_republish() {
        let dir = scratch("untold");
        let (mut n, fired) = appending(&dir, 1, "");
        feed(&mut n, 1, "s-one", "v=1 t=1 attention=first");
        feed(&mut n, 2, "s-two", "v=1 t=2 attention=second");
        assert_eq!(n.verdict(2), Some("dropped"));
        let (mut room, _) = appending(&dir, 10, "");
        feed(&mut room, 3, "s-two", "v=1 t=3 inc=2 attention=second");
        assert_eq!(
            fired_offsets(&fired),
            [1, 3],
            "the dropped escalation pages"
        );
        feed(&mut room, 4, "s-two", "v=1 t=4 inc=3 attention=second");
        assert_eq!(fired_offsets(&fired), [1, 3], "and, told, stands");

        let (mut broken, _) = appending(
            &dir,
            10,
            &format!("if [ ! -e {dir}/fixed ]; then exit 3; fi; "),
        );
        feed(&mut broken, 5, "s-three", "v=1 t=5 attention=third");
        feed(&mut broken, 6, "s-three", "v=1 t=6 inc=2 attention=third");
        assert!(
            broken
                .journal
                .iter()
                .any(|l| l.starts_with("6 fired on=attention rc=3")),
            "an untold escalation runs the command on its republish: {:?}",
            broken.journal
        );
        std::fs::write(Path::new(&dir).join("fixed"), b"").expect("fix the command");
        feed(&mut broken, 7, "s-three", "v=1 t=7 inc=3 attention=third");
        feed(&mut broken, 8, "s-three", "v=1 t=8 inc=4 attention=third");
        assert_eq!(fired_offsets(&fired), [1, 3, 7]);
        assert_eq!(broken.counts.standing, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_journal_keeps_one_verdict_per_offset_and_is_bounded() {
        let dir = scratch("journal");
        let mut n = Notifier::new(cfg(&dir)).expect("open");
        n.record(5, "5 firing on=attention".into());
        assert_eq!(n.verdict(5), Some("firing"));
        n.record(5, "5 fired on=attention rc=0 dup=0".into());
        assert_eq!(n.verdict(5), Some("fired"), "the firing line is replaced");
        assert_eq!(n.journal.len(), 1);
        assert_eq!(
            Notifier::new(cfg(&dir)).expect("reopen").verdict(5),
            Some("fired")
        );
        n.journal.extend(
            (100..99 + JOURNAL_KEEP as u64)
                .map(|off| format!("{off} fired on=attention rc=0 dup=0")),
        );
        for off in 5_000..5_008 {
            n.record(off, format!("{off} fired on=attention rc=0 dup=0"));
        }
        assert_eq!(n.journal.len(), JOURNAL_KEEP);
        assert_eq!(n.verdict(5), None, "the oldest verdicts are forgotten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_uncertain_notify_log_identity_never_claims_another_logs_cursors() {
        let a = astream_broker::LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let b = astream_broker::LogIdentity {
            id: [0xbb; 16],
            generation: 0,
        };
        for bad in ["invalid-utf8", "malformed", "dangling"] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("notify-owner")
                .tempdir()
                .unwrap();
            let store = Store::open(dir.path().to_str().unwrap()).unwrap();
            store.set_cursor("attention", 900).unwrap();
            store
                .set_journal(&["7 fired".to_string()], Flush::Ordered)
                .unwrap();
            let path = store.root.join("log");
            match bad {
                "invalid-utf8" => std::fs::write(&path, [0xff]).unwrap(),
                "malformed" => std::fs::write(&path, "not-a-log\n").unwrap(),
                "dangling" => std::os::unix::fs::symlink("missing-owner", &path).unwrap(),
                _ => unreachable!(),
            }
            assert!(
                store.for_log(Some(b), None).is_err(),
                "{bad}: uncertainty is not permission to adopt A's offsets as B's"
            );
            assert_eq!(store.cursor("attention"), Some(900));
            assert_eq!(store.journal(), ["7 fired"]);
            match bad {
                "invalid-utf8" => assert_eq!(std::fs::read(&path).unwrap(), [0xff]),
                "malformed" => assert_eq!(std::fs::read_to_string(&path).unwrap(), "not-a-log\n"),
                "dangling" => assert_eq!(
                    std::fs::read_link(&path).unwrap(),
                    PathBuf::from("missing-owner")
                ),
                _ => unreachable!(),
            }
            std::fs::remove_file(&path).unwrap();
            store.write("log", &a.to_string(), Flush::Ordered).unwrap();
            let resumed = store.for_log(Some(a), None).unwrap();
            assert_eq!(resumed.cursor("attention"), Some(900));
            assert_eq!(resumed.journal(), ["7 fired"]);
            let fresh = store.for_log(Some(b), None).unwrap();
            assert_eq!(fresh.cursor("attention"), None);
            assert!(fresh.journal().is_empty());
        }
    }

    #[test]
    fn only_the_newest_other_logs_stores_are_kept() {
        let id = |n: u8| astream_broker::LogIdentity {
            id: [n; 16],
            generation: 0,
        };
        let dir = aterm_tempfile::Builder::new()
            .prefix("notify-prune")
            .tempdir()
            .unwrap();
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        store
            .for_log(Some(id(0)), None)
            .unwrap()
            .set_cursor("attention", 900)
            .unwrap();
        let logs = store.root.join("logs");
        std::fs::create_dir_all(logs.join("not-an-identity")).unwrap();
        std::fs::create_dir_all(logs.join(id(99).to_string()).join("cur")).unwrap();
        let kept = || {
            let mut names: Vec<String> = std::fs::read_dir(&logs)
                .unwrap()
                .filter_map(|e| e.unwrap().file_name().into_string().ok())
                .filter(|n| astream_broker::LogIdentity::parse(n).is_some())
                .collect();
            names.sort();
            names
        };
        let named = |ns: &[u8]| ns.iter().map(|n| id(*n).to_string()).collect::<Vec<_>>();
        for n in 1..=6 {
            store
                .for_log(Some(id(n)), None)
                .unwrap()
                .set_cursor("attention", u64::from(n))
                .unwrap();
        }
        assert_eq!(kept(), named(&[3, 4, 5, 6]), "the newest {LOGS_KEEP}");
        assert_eq!(
            store
                .for_log(Some(id(3)), None)
                .unwrap()
                .cursor("attention"),
            Some(3),
            "a kept log resumes"
        );
        store.for_log(Some(id(7)), None).unwrap();
        assert_eq!(kept(), named(&[3, 5, 6, 7]), "3 was read again, 4 was not");
        assert_eq!(
            store
                .for_log(Some(id(1)), None)
                .unwrap()
                .cursor("attention"),
            None,
            "a pruned log comes back as a first run"
        );
        assert_eq!(
            store
                .for_log(Some(id(0)), None)
                .unwrap()
                .cursor("attention"),
            Some(900),
            "the root's store stands"
        );
        assert!(logs.join("not-an-identity").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_notifier_files_its_cursor_and_journal_under_the_log_it_read() {
        let dir = scratch("logs");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let serve = |name: &str| {
            let broker = crate::test_broker(base.join(format!("{name}.log"))).unwrap();
            let sock = base.join(format!("{name}.sock"));
            let handle = broker.serve(&sock).unwrap();
            let (mut c, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
            let (off, _) = c
                .publish(
                    7,
                    1,
                    "/f/f1/pub/n-a/s-x/presence",
                    b"v=1 t=1 attention=help",
                )
                .unwrap();
            assert_eq!(off, 0);
            (sock.to_string_lossy().into_owned(), broker, handle)
        };
        let (a, broker_a, mut server_a) = serve("a");
        let (b, broker_b, mut server_b) = serve("b");
        let fired = base.join("fired");
        let mut c = cfg(&dir);
        c.since = Since::Start;
        c.limit = 10;
        c.exec = format!(
            "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        let run = |broker: &str| {
            let mut c = c.clone();
            c.broker = broker.to_string();
            Notifier::new(c)
                .expect("open")
                .catch_up()
                .expect("catch up");
            std::fs::read_to_string(&fired)
                .unwrap_or_default()
                .lines()
                .count()
        };
        assert_eq!(run(&a), 1, "A's escalation fires");
        assert_eq!(
            run(&b),
            2,
            "B's escalation is B's, whatever A's cursor says"
        );
        assert_eq!(run(&a), 2, "back on A, nothing fires twice");
        assert_eq!(run(&b), 2, "nor on B");
        let root = base.join("notify");
        assert_eq!(
            std::fs::read_to_string(root.join("log")).unwrap().trim(),
            broker_a.log_identity().to_string(),
            "the root is the first log's"
        );
        assert!(root
            .join("logs")
            .join(broker_b.log_identity().to_string())
            .join("cur/attention")
            .exists());
        server_a.shutdown();
        server_b.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_catch_up_below_the_retained_floor_reads_on_from_it() {
        let dir = scratch("kept");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let sock = base.join("b.sock");
        let mut server = broker.serve(&sock).unwrap();
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        for (seq, sid) in [(1, "s-a"), (2, "s-b"), (3, "s-c")] {
            let subject = format!("/f/f1/pub/n-a/{sid}/presence");
            let (off, _) = conn
                .publish(7, seq, &subject, b"v=1 t=1 attention=help")
                .unwrap();
            assert_eq!(off, seq - 1);
        }
        broker.retain_before_carrying_state(1).expect("the cut");
        let fired = base.join("fired");
        let mut c = cfg(&dir);
        c.broker = sock.to_string_lossy().into_owned();
        c.since = Since::Start;
        c.limit = 10;
        c.exec = format!(
            "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("the catch-up reads on from the floor");
        let offsets = std::fs::read_to_string(&fired).unwrap_or_default();
        assert_eq!(
            offsets.lines().collect::<Vec<_>>(),
            ["1", "2"],
            "the escalations the broker kept fire"
        );
        server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum Ended {
        Clean,
        Killed,
        PowerLoss,
    }

    #[cfg(unix)]
    fn after_a_relaxed_stop(
        tag: &str,
        since: Since,
        ended: Ended,
        boot: u8,
        forget: bool,
    ) -> (u64, Vec<String>) {
        use crate::durability::{self, Durability};
        let dir = scratch(tag);
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let log = base.join("r.log");
        let sock = base.join("r.sock");
        let fired = base.join("fired");
        let mut c = cfg(&dir);
        c.broker = sock.to_string_lossy().into_owned();
        c.since = since;
        c.limit = 10;
        c.exec = format!(
            "printf '%s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_TEXT\" >> {}",
            fired.display()
        );
        let escalate = |text: &str| {
            let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
            let seq = u64::from(text.as_bytes()[text.len() - 1] - b'0') + 1;
            let body = format!("v=1 t=1 attention={text}");
            conn.publish(7, seq, "/f/f1/pub/n-a/s-x/presence", body.as_bytes())
                .unwrap()
                .0
        };
        let run = || {
            Notifier::new(c.clone())
                .expect("open")
                .catch_up()
                .expect("catch up");
            std::fs::read_to_string(&fired).unwrap_or_default()
        };
        let (before, cut) = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            if !matches!(ended, Ended::Clean) {
                opened.broker.arm_unclean_stop();
            }
            let mut server = opened.broker.serve(&sock).unwrap();
            assert_eq!(run(), "", "an empty log: the cursor is claimed at 0");
            assert_eq!(escalate("help-0"), 0);
            let cut = std::fs::metadata(&log).unwrap().len();
            for text in ["help-1", "help-2", "help-3"] {
                escalate(text);
            }
            assert_eq!(run().lines().count(), 4, "all four fire in boot 1");
            server.shutdown();
            (opened.broker.log_identity(), cut)
        };
        if matches!(ended, Ended::PowerLoss) {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(cut)
                .unwrap();
        }
        let reopened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(boot)),
        )
        .unwrap();
        let now = reopened.broker.log_identity();
        let moved = now != before;
        assert_eq!(
            now.id, before.id,
            "{tag}: the same log, whatever its generation"
        );
        let mut server = reopened.broker.serve(&sock).unwrap();
        let off = escalate("help-9");
        FORGET_LINEAGE.with(|f| f.set(forget));
        let after = run().lines().skip(4).map(str::to_string).collect();
        FORGET_LINEAGE.with(|f| f.set(false));
        server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            moved,
            boot != 1 && !matches!(ended, Ended::Clean),
            "{tag}: only an unclean stop and a reboot move the generation"
        );
        (off, after)
    }

    #[cfg(unix)]
    #[test]
    fn a_power_loss_under_a_relaxed_broker_resumes_the_notifier_where_the_log_continues() {
        for since in [Since::Head, Since::Start] {
            let tag = format!("pl-{since:?}");
            let (off, fired) = after_a_relaxed_stop(&tag, since, Ended::PowerLoss, 2, false);
            assert_eq!(off, 1, "{tag}: the new escalation reuses offset 1");
            assert_eq!(
                fired,
                ["1 help-9"],
                "{tag}: carried over, clamped at the fork"
            );
        }

        let (off, fired) =
            after_a_relaxed_stop("pl-forget", Since::Head, Ended::PowerLoss, 2, true);
        assert_eq!(off, 1);
        assert!(
            fired.is_empty(),
            "CONTROL: a first run starts past it: {fired:?}"
        );
        let (_, fired) =
            after_a_relaxed_stop("pl-forget-0", Since::Start, Ended::PowerLoss, 2, true);
        assert_eq!(
            fired,
            ["0 help-0", "1 help-9"],
            "CONTROL: re-read from the start"
        );

        let (off, fired) = after_a_relaxed_stop("pl-ctl", Since::Start, Ended::PowerLoss, 1, false);
        assert_eq!(off, 1);
        assert!(
            fired.is_empty(),
            "CONTROL: the kept identity swallows it: {fired:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_clean_stop_or_a_kill_keeps_a_default_notifiers_place_across_a_reboot() {
        let (off, fired) = after_a_relaxed_stop("clean", Since::Head, Ended::Clean, 2, false);
        assert_eq!(off, 4, "nothing was lost");
        assert_eq!(fired, ["4 help-9"]);

        let (off, fired) = after_a_relaxed_stop("killed", Since::Head, Ended::Killed, 2, false);
        assert_eq!(off, 4, "nothing was lost");
        assert_eq!(fired, ["4 help-9"], "the moved generation continues at 4");

        let (off, fired) = after_a_relaxed_stop("killed-ctl", Since::Head, Ended::Killed, 2, true);
        assert_eq!(off, 4);
        assert!(fired.is_empty(), "CONTROL: read from its head: {fired:?}");
    }

    #[test]
    fn a_moved_generation_carries_the_newest_store_it_continues() {
        let dir = scratch("carry");
        let store = Store::open(&dir).expect("open");
        let g = |generation| astream_broker::LogIdentity {
            id: [7; 16],
            generation,
        };
        let root = store.for_log(Some(g(0)), None).unwrap();
        root.set_cursor("attention", 4).unwrap();
        root.set_journal(&["1 fired rc=0".to_string()], Flush::Ordered)
            .unwrap();
        let one = store.for_log(Some(g(1)), None).unwrap();
        assert_ne!(one.root, root.root);
        assert_eq!(one.cursor("attention"), None, "no lineage: a first run");
        one.set_cursor("attention", 9).unwrap();
        one.set_journal(
            &[
                "2 fired rc=0".to_string(),
                "6 fired rc=0".to_string(),
                "junk".into(),
            ],
            Flush::Ordered,
        )
        .unwrap();
        one.set_standing(&[("/f/f1/pub/n-a/s-x/presence".into(), "help".into())])
            .unwrap();

        let lineage = lineage_of(&[(1, 3), (2, 5)]);
        let two = store.for_log(Some(g(2)), Some(&lineage)).unwrap();
        assert_eq!(
            two.cursor("attention"),
            Some(5),
            "from (id, 1), at its fork"
        );
        assert_eq!(
            two.journal(),
            ["2 fired rc=0"],
            "the verdicts below the fork"
        );
        assert_eq!(two.standing(), one.standing());
        two.set_cursor("attention", 7).unwrap();
        let again = store.for_log(Some(g(2)), Some(&lineage)).unwrap();
        assert_eq!(
            again.cursor("attention"),
            Some(7),
            "carried once, then its own"
        );
        let leftovers: Vec<_> = std::fs::read_dir(PathBuf::from(&dir).join("notify/logs"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".carry-"))
            .collect();
        assert!(leftovers.is_empty(), "no scratch left behind");

        let gap = lineage_of(&[(3, 4)]);
        let three = store.for_log(Some(g(4)), Some(&gap)).unwrap();
        assert_eq!(
            three.cursor("attention"),
            None,
            "a step missing: a first run"
        );
        let other = astream_broker::LogIdentity {
            id: [8; 16],
            generation: 1,
        };
        let foreign = store
            .for_log(Some(other), Some(&lineage_of(&[(1, 9)])))
            .unwrap();
        assert_eq!(foreign.cursor("attention"), None, "another id: a first run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_executor_holds_a_record_until_a_durable_mark_covers_it() {
        let presence = |sid: &str| format!("/f/f1/pub/n-a/{sid}/presence");
        let raise = |text: &str| format!("v=1 t=1 attention={text}").into_bytes();
        let log = |ordered: bool, generation: u64| ReadLog {
            id: Some(astream_broker::LogIdentity {
                id: [7; 16],
                generation,
            }),
            lineage: None,
            ordered,
        };
        let feed = |n: &mut Notifier, notices: Vec<Notice>| {
            for notice in notices {
                n.take(notice).expect("take");
            }
        };
        let cursor = |n: &Notifier| n.log_store.cursor(&Sel::Attention.key());

        let dir = scratch("held");
        let (mut n, fired) = appending(&dir, 100, "");
        feed(
            &mut n,
            vec![
                Notice::Log(0, log(true, 0)),
                Notice::Record(0, 5, presence("s-a"), raise("key")),
                Notice::Record(0, 7, presence("s-b"), raise("disk")),
                Notice::Checkpoint(0, 9),
            ],
        );
        assert!(fired_offsets(&fired).is_empty(), "held, not paged");
        assert_eq!(cursor(&n), None, "and nothing moves past them");
        n.take(Notice::Durable(0, 5)).unwrap();
        assert!(
            fired_offsets(&fired).is_empty(),
            "5 is not below a durable 5"
        );
        n.take(Notice::Durable(0, 6)).unwrap();
        assert_eq!(fired_offsets(&fired), [5]);
        assert_eq!(cursor(&n), Some(6), "past the paged one, at the held one");
        n.take(Notice::Durable(0, 8)).unwrap();
        assert_eq!(fired_offsets(&fired), [5, 7]);
        assert_eq!(cursor(&n), Some(9), "the held-back checkpoint");
        assert_eq!(n.held_bytes, 0);

        feed(
            &mut n,
            vec![
                Notice::Record(0, 10, presence("s-c"), raise("lost")),
                Notice::Log(0, log(true, 1)),
                Notice::Durable(0, 99),
            ],
        );
        assert_eq!(
            fired_offsets(&fired),
            [5, 7],
            "another log's offset pages nothing"
        );
        assert_eq!(n.held_bytes, 0);

        feed(
            &mut n,
            vec![
                Notice::Log(0, log(true, 1)),
                Notice::Record(0, 20, presence("s-f"), raise("held")),
                Notice::Log(0, log(false, 1)),
                Notice::Record(0, 21, presence("s-g"), raise("strict")),
            ],
        );
        assert_eq!(fired_offsets(&fired), [5, 7, 20, 21], "in read order");
        assert_eq!(n.held_bytes, 0);
        n.take(Notice::Log(0, log(true, 1))).unwrap();

        feed(
            &mut n,
            vec![
                Notice::Record(0, 30, presence("s-d"), raise("first")),
                Notice::Record(0, 31, presence("s-e"), {
                    let mut body = raise("big");
                    body.resize(HOLD_MAX, b' ');
                    body
                }),
            ],
        );
        assert_eq!(fired_offsets(&fired), [5, 7, 20, 21, 30, 31]);
        assert_eq!(n.held_bytes, 0);
        let _ = std::fs::remove_dir_all(&dir);

        for (tag, ordered, wait) in [("strict", false, true), ("early", true, false)] {
            let dir = scratch(tag);
            let (mut n, fired) = appending(&dir, 100, "");
            n.wait_durable = wait;
            feed(
                &mut n,
                vec![
                    Notice::Log(0, log(ordered, 0)),
                    Notice::Record(0, 5, presence("s-a"), raise("key")),
                    Notice::Checkpoint(0, 9),
                ],
            );
            assert_eq!(fired_offsets(&fired), [5], "{tag}: paged as read");
            assert_eq!(cursor(&n), Some(9), "{tag}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A log a follow-mode reader reached that serves strict: a record is judged as it is read.
    fn strict_log() -> ReadLog {
        ReadLog {
            id: Some(astream_broker::LogIdentity {
                id: [7; 16],
                generation: 0,
            }),
            lineage: None,
            ordered: false,
        }
    }

    fn presence_of(sid: &str) -> String {
        format!("/f/f1/pub/n-a/{sid}/presence")
    }

    /// By command, the flushes asked for since the one before ([`flushes_by_command`]).
    type ByCommand = std::rc::Rc<std::cell::RefCell<Vec<Vec<Flush>>>>;

    /// The flushes this thread asked for before [`Notifier::exec`] started each command
    /// ([`AT_EXEC`]), and the recording that goes on after.
    fn flushes_by_command() -> (ByCommand, std::rc::Rc<aterm_durable::seam::Recording>) {
        let recording = std::rc::Rc::new(aterm_durable::seam::Recording::start());
        let before = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = (std::rc::Rc::clone(&recording), std::rc::Rc::clone(&before));
        AT_EXEC.with(|seam| {
            *seam.borrow_mut() = Some(Box::new(move |_: &Hit| {
                seen.1.borrow_mut().push(seen.0.take());
            }));
        });
        (before, recording)
    }

    #[test]
    fn a_page_flushes_in_full_once_and_before_its_command_runs() {
        use Flush::{Durable as D, Ordered as O};
        let durable = |flushes: &[Flush]| flushes.iter().filter(|f| f.is_durable()).count();
        let row = |off: u64, sid: &str, attention: &str| {
            Notice::Record(
                0,
                off,
                presence_of(sid),
                format!("v=1 t=1 state=live attention={attention}").into_bytes(),
            )
        };
        /// A case's name, its `firing` flush, what it judges before the recording starts, the
        /// pages it records, and what each page asks for before its command: the general law
        /// is one full flush, last.
        type Case = (
            &'static str,
            Flush,
            Vec<Notice>,
            Vec<Notice>,
            &'static [Flush],
        );
        let cases: [Case; 4] = [
            (
                "a first raise",
                D,
                Vec::new(),
                vec![row(1, "s-a", "help-a"), row(2, "s-b", "help-b")],
                &[O, O, D],
            ),
            (
                "NEGATIVE CONTROL: an ordered `firing` line",
                O,
                Vec::new(),
                vec![row(1, "s-a", "help-a"), row(2, "s-b", "help-b")],
                &[O, O, O],
            ),
            (
                "a raise that replaces another token, its cursor owed",
                D,
                vec![row(1, "s-a", "help-1"), row(2, "s-q", "-")],
                vec![row(3, "s-a", "help-2")],
                &[O, O, O, O, O, O, D],
            ),
            (
                "a log's own store, under logs/",
                D,
                Vec::new(),
                vec![row(1, "s-a", "help-a")],
                &[O, O, O, D],
            ),
        ];
        for (at, (case, firing, setup, pages, asks)) in cases.into_iter().enumerate() {
            let dir = scratch(&format!("page-flush-{at}"));
            if case.contains("logs/") {
                Store::open(&dir)
                    .expect("open")
                    .for_log(
                        Some(astream_broker::LogIdentity {
                            id: [8; 16],
                            generation: 0,
                        }),
                        None,
                    )
                    .expect("the root's store claimed by another log");
            }
            let (mut n, fired) = appending(&dir, 100, "");
            n.firing = firing;
            n.cursor_every = Duration::MAX;
            n.take(Notice::Log(0, strict_log())).expect("log");
            n.take(Notice::Checkpoint(0, 1))
                .expect("the log's store claimed, its cursor written, before the recording");
            for notice in setup {
                n.take(notice).expect("judged before the recording");
            }
            let paged = fired_offsets(&fired).len();
            let (before, recording) = flushes_by_command();
            let mut after = Vec::new();
            let count = pages.len();
            for notice in pages {
                n.take(notice).expect("a page");
                after.push(recording.take());
            }
            AT_EXEC.with(|seam| seam.borrow_mut().take());
            let before = before.take();
            assert_eq!(
                fired_offsets(&fired).len() - paged,
                count,
                "{case}: each page ran its command"
            );
            assert_eq!(before.len(), count, "{case}: once");
            for (page, (before, after)) in before.iter().zip(&after).enumerate() {
                assert_eq!(
                    before.as_slice(),
                    asks,
                    "{case}, page {page}: what it asks for before its command"
                );
                if firing == O {
                    assert_eq!(
                        durable(before),
                        0,
                        "{case}: nothing is on the media before the command runs"
                    );
                    continue;
                }
                assert_eq!(
                    (durable(before), before.last()),
                    (1, Some(&D)),
                    "{case}, page {page}: one full flush before its command, the last"
                );
                assert!(
                    !after.is_empty() && durable(after) == 0,
                    "{case}, page {page}: its `fired` line, standing value and cursor ordered \
                     after its command, none in full: {after:?}"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_record_that_pages_nothing_flushes_nothing_and_the_cursor_trails_it_by_at_most_its_lag() {
        let mut s = Needs::open();
        let start = s.bodies();
        for _ in 0..2 * CURSOR_LAG + 3 {
            s.quiet();
        }
        let mut records: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, body) in s.bodies().into_iter().enumerate() {
            records.push((NEED_FACE.to_string(), body.into_bytes()));
            if i % 3 == 2 {
                records.push((presence_of("s-x"), b"v=1 t=1 inc=1 state=live".to_vec()));
            }
        }
        let need = Sel::Need(None);
        let key = need.key();
        for (lag, every) in [
            (CURSOR_LAG, Duration::MAX),
            (1, Duration::MAX),
            (CURSOR_LAG, Duration::ZERO),
        ] {
            let tag = format!("lag {lag}, every {every:?}");
            let dir = scratch(&format!("lag-{lag}-{}", every.is_zero()));
            let (mut n, fired) = appending(&dir, 100, "");
            n.cfg.on = vec![need.clone()];
            n.cursor_lag = lag;
            n.cursor_every = every;
            n.take(Notice::Log(0, strict_log())).expect("log");
            let mut off = 0;
            for body in &start {
                off += 1;
                n.take(Notice::Record(
                    0,
                    off,
                    NEED_FACE.into(),
                    body.clone().into_bytes(),
                ))
                .expect("the session's start");
            }
            n.persist().expect("written before the recording");
            let recording = aterm_durable::seam::Recording::start();
            let mut flushed = Vec::new();
            for (judged, (subject, body)) in (1..).zip(&records) {
                off += 1;
                n.take(Notice::Record(0, off, subject.clone(), body.clone()))
                    .expect("a record");
                if !recording.take().is_empty() {
                    flushed.push(judged);
                }
                let at = n.log_store.cursor(&key).expect("a cursor");
                assert!(
                    off + 1 - at <= lag,
                    "{tag}: the cursor at {at} trails @{off} by more than {lag}"
                );
            }
            assert!(fired_offsets(&fired).is_empty(), "{tag}: nothing rose");
            let all: Vec<u64> = (1..=records.len() as u64).collect();
            if lag == 1 {
                assert_eq!(
                    flushed, all,
                    "NEGATIVE CONTROL: a cursor written per record flushes per record"
                );
            } else if every.is_zero() {
                assert_eq!(
                    flushed, all,
                    "a cursor owed for its whole interval is written by the next record"
                );
            } else {
                let each_lag: Vec<u64> = all
                    .iter()
                    .copied()
                    .filter(|j| j % CURSOR_LAG == 0)
                    .collect();
                assert!(each_lag.len() >= 2, "PRECONDITION: two lags of records");
                assert_eq!(
                    flushed, each_lag,
                    "a record that pages nothing flushes nothing; its cursor is written once \
                     every {CURSOR_LAG}"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_standing_value_never_reaches_the_media_ahead_of_its_selectors_cursor() {
        let key = Sel::Attention.key();
        let raise = b"v=1 t=1 attention=help".to_vec();
        let dir = scratch("standing-cursor");
        let (mut n, fired) = appending(&dir, 100, "");
        n.cursor_every = Duration::MAX;
        n.take(Notice::Log(0, strict_log())).expect("log");
        let records = [
            (1, "s-a", raise.clone()),
            (2, "s-a", raise.clone()),
            (3, "s-a", b"v=1 t=1 attention=-".to_vec()),
            (4, "s-b", b"v=1 t=1 state=live".to_vec()),
        ];
        for (off, sid, body) in &records {
            n.take(Notice::Record(0, *off, presence_of(sid), body.clone()))
                .expect("a record");
        }
        assert_eq!(fired_offsets(&fired), [1], "the republish at 2 stood");
        assert_eq!(
            n.log_store.cursor(&key),
            Some(3),
            "the clear at 3 wrote the cursor AT 3 before it wrote the standing value"
        );
        drop(n);

        let replay = |cursor: u64| {
            let (mut back, _) = appending(&dir, 100, "");
            back.take(Notice::Log(0, strict_log())).expect("log");
            for (off, sid, body) in records.iter().filter(|(off, ..)| *off >= cursor) {
                back.take(Notice::Record(0, *off, presence_of(sid), body.clone()))
                    .expect("a record re-read");
            }
            fired_offsets(&fired)
        };
        assert_eq!(
            replay(3),
            [1],
            "a crash that loses the owed cursor re-reads from 3, and pages nothing again"
        );
        std::fs::write(Path::new(&dir).join("notify/cur").join(&key), "2\n")
            .expect("CONTROL: the cursor as it stood before the clear");
        assert_eq!(
            replay(2),
            [1, 2],
            "NEGATIVE CONTROL: the clear on the media ahead of the cursor pages the republish \
             it judged under the standing value"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cursor writes ([`PUT_NAMES`]) a `need` follower makes over a fleet's busy faces: one
    /// at each page and each need it clears, one a second by its clock, and one per
    /// [`CURSOR_LAG`] records at most, never one a record.
    #[test]
    fn a_followers_cursor_is_written_per_page_and_per_second_never_per_record() {
        let need = Sel::Need(None);
        let mut s = Needs::open();
        let mut bodies = s.bodies();
        for _ in 0..3 {
            for _ in 0..4 {
                for _ in 0..50 {
                    s.quiet();
                }
                bodies.extend(s.bodies());
            }
            let asked = s.question();
            bodies.extend(s.bodies());
            s.answer(asked);
            bodies.extend(s.bodies());
        }
        let mut records: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, body) in bodies.into_iter().enumerate() {
            records.push((NEED_FACE.to_string(), body.into_bytes()));
            if i % 2 == 1 {
                records.push((presence_of("s-y"), b"v=1 t=1 inc=1 state=live".to_vec()));
            }
        }
        let n_records = u32::try_from(records.len()).expect("a small run");
        let step = Duration::from_millis(50);
        let dir = scratch("cursor-writes");
        let (mut n, fired) = appending(&dir, 100, "");
        n.cfg.on = vec![need.clone()];
        let base = Instant::now();
        n.clock = Some(base);
        n.take(Notice::Log(0, strict_log())).expect("log");
        PUT_NAMES.with(|names| *names.borrow_mut() = Some(Vec::new()));
        for ((at, off), (subject, body)) in (0..).zip(1..).zip(&records) {
            n.clock = Some(base + step * at);
            n.take(Notice::Record(0, off, subject.clone(), body.clone()))
                .expect("a record");
        }
        let names = PUT_NAMES
            .with(|names| names.borrow_mut().take())
            .unwrap_or_default();
        let cursors = names.iter().filter(|name| name.starts_with("cur/")).count();
        let pages = fired_offsets(&fired).len();
        assert_eq!(pages, 3, "PRECONDITION: each question paged once");
        let seconds = usize::try_from((step * n_records).as_secs()).expect("seconds");
        let lags = records.len() / usize::try_from(CURSOR_LAG).expect("lag");
        let bound = 3 * pages + seconds + lags + 2;
        assert!(
            records.len() > 4 * bound,
            "PRECONDITION: a bound well short of a write per record"
        );
        assert!(
            cursors <= bound,
            "{} records, {pages} pages over {seconds} s: {cursors} cursor writes, past \
             {bound} (two a page and one a clear, one a second, one a lag)",
            records.len()
        );
        n.clock = Some(base + step * n_records + CURSOR_EVERY);
        n.tick(n.now()).expect("a tick");
        assert_eq!(
            n.log_store.cursor(&need.key()),
            Some(u64::from(n_records) + 1),
            "a second after the last record, its cursor is written"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Records as a test feeds them: offset, subject, body.
    type Records = Vec<(u64, String, Vec<u8>)>;

    /// A session's start, then three failed runs, each lifted by the good run after it ([`Needs`]),
    /// at offsets from 1; and the offsets of the failed runs' closes, each a rise to `failure`.
    fn three_failures() -> (Records, Vec<u64>) {
        let mut s = Needs::open();
        let mut bodies = s.bodies();
        for _ in 0..3 {
            let bad = s.cmd_open();
            s.cmd_close(bad, aterm_slog::RunOut::Fail);
            bodies.extend(s.bodies());
            let good = s.cmd_open();
            s.cmd_close(good, aterm_slog::RunOut::Ok);
            bodies.extend(s.bodies());
        }
        let rises = (1..)
            .zip(&bodies)
            .filter(|(_, b)| b.contains(" op=close ") && b.contains("out=fail"))
            .map(|(off, _)| off)
            .collect();
        let records = (1..)
            .zip(bodies)
            .map(|(off, b)| (off, NEED_FACE.to_string(), b.into_bytes()))
            .collect();
        (records, rises)
    }

    #[test]
    fn a_refused_write_never_ends_the_follower_and_each_rise_pages_once() {
        let (records, rises) = three_failures();
        assert_eq!(rises.len(), 3, "PRECONDITION: three rises");
        let need = Sel::Need(None);
        let dir = scratch("refused");
        let (mut n, fired) = appending(&dir, 100, "");
        n.cfg.on = vec![need.clone()];
        n.cursor_every = Duration::MAX;
        n.retry_every = Duration::ZERO;
        WARNED.with(|warned| warned.borrow_mut().clear());
        let enospc = libc::ENOSPC;
        REFUSE.with(|refuse| {
            *refuse.borrow_mut() = vec![
                ("cur/", 1, enospc),
                ("standing", 0, enospc),
                ("journal", 2, enospc),
                ("journal", 1, enospc),
            ];
        });
        let mut notices = vec![Notice::Log(0, strict_log())];
        notices.extend(
            records
                .iter()
                .map(|(off, subject, body)| Notice::Record(0, *off, subject.clone(), body.clone())),
        );
        let (tx, rx) = mpsc::sync_channel(notices.len());
        for notice in notices {
            tx.send(notice).expect("queued");
        }
        drop(tx);
        let ended = n.serve(&rx);
        assert_eq!(
            ended.map_err(|e| e.kind()),
            Err(io::ErrorKind::ConnectionAborted),
            "the follower took every notice and ended only with its readers"
        );
        assert_eq!(
            fired_offsets(&fired),
            rises,
            "each rise paged once: the one whose `firing` line was refused once it was written"
        );
        let unspent = REFUSE.with(std::cell::RefCell::take);
        assert!(
            unspent.is_empty(),
            "PRECONDITION: the first page's cursor and standing value, the second's `firing` \
             and `fired` lines were refused: {unspent:?} left"
        );
        let warned = WARNED.with(std::cell::RefCell::take);
        assert!(
            warned.contains(&Warned::Refused(Some(enospc))),
            "a refusal is warned of with its errno: {warned:?}"
        );
        assert_eq!(
            (warned.last(), n.failing.is_none()),
            (Some(&Warned::Cleared), true),
            "and so is the end of it: {warned:?}"
        );
        assert_eq!(
            n.log_store.cursor(&need.key()),
            Some(records.len() as u64 + 1),
            "every cursor owed is written"
        );
        let journal = n.log_store.journal();
        assert!(
            rises.iter().all(|off| journal
                .iter()
                .any(|l| l.starts_with(&format!("{off} fired ")) && l.contains(" rc=0 "))),
            "each page's `fired` line is on the store: {journal:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_page_whose_firing_line_is_refused_runs_its_command_only_once_it_is_written() {
        let (records, rises) = three_failures();
        let need = Sel::Need(None);
        let open = |dir: &str| {
            let (mut n, fired) = appending(dir, 100, "");
            n.cfg.on = vec![need.clone()];
            n.cursor_every = Duration::MAX;
            n.retry_every = Duration::from_secs(60);
            n.take(Notice::Log(0, strict_log())).expect("log");
            (n, fired)
        };
        let record = |off: u64| {
            let (off, subject, body) = &records[usize::try_from(off - 1).expect("an index")];
            Notice::Record(0, *off, subject.clone(), body.clone())
        };
        for crash in [false, true] {
            let dir = scratch(&format!("firing-refused-{crash}"));
            let (mut n, fired) = open(&dir);
            let base = Instant::now();
            n.clock = Some(base);
            REFUSE.with(|refuse| *refuse.borrow_mut() = vec![("journal", 0, libc::ENOSPC)]);
            for off in 1..rises[0] {
                n.take(record(off)).expect("judged");
            }
            let stalled = n.take(record(rises[0]));
            assert!(
                fired_offsets(&fired).is_empty(),
                "crash={crash}: its command did not run without its `firing` line"
            );
            assert!(
                REFUSE.with(std::cell::RefCell::take).is_empty(),
                "PRECONDITION: the page's `firing` line was refused"
            );
            assert_eq!(
                stalled.map_err(|e| e.raw_os_error()),
                Err(Some(libc::ENOSPC)),
                "the page stalls on the refusal"
            );
            let waits = n.take(record(rises[0] + 1));
            assert_eq!(
                (waits.map_err(|e| e.kind()), fired_offsets(&fired).len()),
                (Err(io::ErrorKind::WouldBlock), 0),
                "crash={crash}: a notice taken before the retry is due waits behind the page"
            );
            let (mut n, fired) = if crash {
                drop(n);
                let (mut back, fired) = open(&dir);
                let from = back.log_store.cursor(&need.key()).unwrap_or(1);
                assert!(from <= rises[0], "PRECONDITION: the cursor is not past it");
                for off in from..=rises[0] + 1 {
                    back.take(record(off)).expect("re-read");
                }
                (back, fired)
            } else {
                n.clock = Some(base + Duration::from_secs(60));
                n.tick(n.now()).expect("the retry runs it");
                (n, fired)
            };
            assert_eq!(
                fired_offsets(&fired),
                [rises[0]],
                "crash={crash}: the page ran once, once its `firing` line was written"
            );
            for off in rises[0] + 2..=records.len() as u64 {
                n.take(record(off)).expect("judged");
            }
            assert_eq!(
                fired_offsets(&fired),
                rises,
                "crash={crash}: each rise once"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_crash_after_a_page_whose_cursor_was_refused_runs_its_command_once() {
        let (records, rises) = three_failures();
        let need = Sel::Need(None);
        let dir = scratch("crash-cursor");
        let open = |dir: &str| {
            let (mut n, fired) = appending(dir, 100, "");
            n.cfg.on = vec![need.clone()];
            n.cursor_every = Duration::MAX;
            n.retry_every = Duration::from_secs(3_600);
            n.take(Notice::Log(0, strict_log())).expect("log");
            (n, fired)
        };
        let (mut n, fired) = open(&dir);
        REFUSE.with(|refuse| *refuse.borrow_mut() = vec![("cur/", 0, libc::ENOSPC)]);
        for (off, subject, body) in records.iter().filter(|(off, ..)| *off <= rises[0]) {
            n.take(Notice::Record(0, *off, subject.clone(), body.clone()))
                .expect("judged");
        }
        assert!(
            REFUSE.with(std::cell::RefCell::take).is_empty(),
            "PRECONDITION: the page's cursor was refused"
        );
        assert_eq!(fired_offsets(&fired), [rises[0]], "PRECONDITION: it paged");
        let cursor = n.log_store.cursor(&need.key()).unwrap_or(0);
        assert!(
            cursor <= rises[0],
            "PRECONDITION: the cursor on the store, {cursor}, is not past the page"
        );
        drop(n);
        let (mut back, _) = open(&dir);
        for (off, subject, body) in records.iter().filter(|(off, ..)| *off >= cursor) {
            back.take(Notice::Record(0, *off, subject.clone(), body.clone()))
                .expect("re-read");
        }
        assert_eq!(
            fired_offsets(&fired),
            rises,
            "the restart re-read the page and ran nothing again; each later rise paged once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A presence row: offset, session, the `attention=` it raises (`-` clears it).
    type Row = (u64, &'static str, &'static str);

    fn raised(&(off, sid, att): &Row) -> Notice {
        Notice::Record(
            0,
            off,
            presence_of(sid),
            format!("v=1 t=1 state=live attention={att}").into_bytes(),
        )
    }

    /// An `attention` follower that writes an owed cursor only at a verdict or a standing
    /// change, retrying a refused write `retry` later.
    fn attending(dir: &str, limit: u32, retry: Duration) -> (Notifier, PathBuf) {
        let (mut n, fired) = appending(dir, limit, "");
        n.cfg.window_ms = 3_600_000;
        n.cursor_every = Duration::MAX;
        n.retry_every = retry;
        n.take(Notice::Log(0, strict_log())).expect("log");
        (n, fired)
    }

    /// Re-read `rows` from the cursor on `dir`'s store, as a restart after a crash does: the
    /// follower, and every offset whose command ran in either run.
    fn restart(dir: &str, rows: &[Row], limit: u32) -> (Notifier, Vec<u64>) {
        let (mut back, fired) = attending(dir, limit, Duration::from_secs(60));
        let from = back.log_store.cursor(&Sel::Attention.key()).unwrap_or(1);
        for row in rows.iter().filter(|(off, ..)| *off >= from) {
            back.take(raised(row)).expect("re-read");
        }
        (back, fired_offsets(&fired))
    }

    fn refuse(name: &'static str, times: usize) {
        REFUSE.with(|r| *r.borrow_mut() = vec![(name, 0, libc::ENOSPC); times]);
    }

    /// While a standing value a record changed is unwritten, the follower judges no later
    /// record, so no second change waits behind it: no one cursor write keeps the store's law
    /// for two. With the cursor written at the newest of two, a crash loses the older change
    /// and misses the rise after it (`miss`); at the oldest, the store holds the newer values
    /// with its cursor before a record judged under the older ones, and a crash pages it again
    /// (`repeat`). The retry that writes the change judges the record waiting behind it.
    #[test]
    fn a_record_waits_behind_an_unwritten_standing_change_so_a_crash_neither_misses_nor_repeats() {
        let retry = Duration::from_secs(60);
        let miss: [Row; 4] = [
            (1, "s-a", "help"),
            (2, "s-a", "-"),
            (3, "s-b", "other"),
            (4, "s-a", "help"),
        ];
        for crash in [false, true] {
            let dir = scratch(&format!("standing-miss-{crash}"));
            let (mut n, fired) = attending(&dir, 100, retry);
            let base = Instant::now();
            n.clock = Some(base);
            n.take(raised(&miss[0])).expect("1 pages");
            refuse("standing", 2);
            n.take(raised(&miss[1]))
                .expect("2 clears s-a, its standing value refused");
            let waits = n.take(raised(&miss[2]));
            assert_eq!(
                (waits.map_err(|e| e.raw_os_error()), fired_offsets(&fired)),
                (Err(Some(libc::ENOSPC)), vec![1]),
                "crash={crash}: s-b's raise at 3 waits behind the clear at 2"
            );
            n.clock = Some(base + retry);
            let _ = n.tick(n.now());
            assert!(
                REFUSE.with(std::cell::RefCell::take).is_empty(),
                "PRECONDITION: the clear was refused at 2 and again at its retry"
            );
            let paged = if crash {
                drop(n);
                restart(&dir, &miss, 100).1
            } else {
                n.clock = Some(base + retry * 2);
                n.tick(n.now())
                    .expect("the retry writes the clear and judges 3");
                assert_eq!(
                    fired_offsets(&fired),
                    [1, 3],
                    "the record waiting behind the clear is judged at the retry that wrote it"
                );
                n.take(raised(&miss[3])).expect("4 pages");
                fired_offsets(&fired)
            };
            assert_eq!(
                paged,
                [1, 3, 4],
                "crash={crash}: s-b's raise at 3 pages once, and s-a's help, cleared at 2, \
                 pages again at 4"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        let repeat: [Row; 4] = [
            (1, "s-a", "help"),
            (2, "s-b", "help"),
            (3, "s-a", "help"),
            (4, "s-a", "-"),
        ];
        let dir = scratch("standing-repeat");
        let (mut n, fired) = attending(&dir, 100, retry);
        let base = Instant::now();
        n.clock = Some(base);
        n.take(raised(&repeat[0])).expect("1 pages");
        REFUSE.with(|r| {
            *r.borrow_mut() = vec![("standing", 0, libc::ENOSPC), ("cur/", 0, libc::ENOSPC)];
        });
        n.take(raised(&repeat[1]))
            .expect("2 pages s-b, its standing value refused");
        for row in &repeat[2..] {
            let _ = n.take(raised(row));
        }
        n.clock = Some(base + retry);
        let _ = n.tick(n.now());
        assert_eq!(
            (
                fired_offsets(&fired),
                REFUSE.with(std::cell::RefCell::take).len()
            ),
            (vec![1, 2], 0),
            "PRECONDITION: 1 and 2 paged, 3 stood, s-b's standing value was refused, and at \
             its retry a cursor"
        );
        drop(n);
        assert_eq!(
            restart(&dir, &repeat, 100).1,
            [1, 2],
            "a crash re-reads s-a's republish at 3 under the value it stood under, and pages \
             nothing again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No cursor reaches the store while a part a record changed is unwritten, even where the
    /// retry is due at once: a clear refused, a crash, and the restart re-reads the clear, so
    /// the re-raise after it pages.
    #[test]
    fn no_cursor_passes_a_record_whose_standing_change_is_unwritten() {
        let rows: [Row; 4] = [
            (1, "s-a", "help"),
            (2, "s-a", "-"),
            (3, "s-c", "-"),
            (4, "s-a", "help"),
        ];
        let dir = scratch("standing-hold");
        let (mut n, _) = attending(&dir, 100, Duration::ZERO);
        n.cursor_lag = 1;
        n.clock = Some(Instant::now());
        n.take(raised(&rows[0])).expect("1 pages");
        refuse("standing", 2);
        n.take(raised(&rows[1]))
            .expect("2 clears s-a, refused, and its retry refused");
        assert!(
            REFUSE.with(std::cell::RefCell::take).is_empty(),
            "PRECONDITION: the clear and its retry, due at once, were refused"
        );
        drop(n);
        assert_eq!(
            restart(&dir, &rows, 100).1,
            [1, 4],
            "the restart re-read the clear at 2, so s-a's help at 4 paged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A drop whose `dropped` line is refused: its count stays off the store until the line is
    /// on it (each part in its order, [`Part`]), no cursor passes it, and a crash before either
    /// is written has the restart judge it again and count it once.
    #[test]
    fn a_drops_count_never_reaches_the_store_ahead_of_its_line_and_no_cursor_passes_it() {
        let key = Sel::Attention.key();
        let retry = Duration::from_secs(60);
        let rows: [Row; 2] = [(1, "s-a", "help"), (2, "s-b", "other")];
        let on_store = |n: &Notifier| {
            (
                n.log_store
                    .journal()
                    .iter()
                    .any(|l| l.starts_with("2 dropped ")),
                n.store.suppressed().0,
                n.log_store.cursor(&key),
            )
        };
        for crash in [false, true] {
            let dir = scratch(&format!("drop-order-{crash}"));
            let (mut n, fired) = attending(&dir, 1, retry);
            let base = Instant::now();
            n.clock = Some(base);
            n.take(raised(&rows[0])).expect("1 pages");
            refuse("journal", 2);
            n.take(raised(&rows[1])).expect("2 is judged");
            assert_eq!(
                (fired_offsets(&fired), n.counts.dropped),
                (vec![1], 1),
                "PRECONDITION: 1 paged and 2 was over the budget"
            );
            n.clock = Some(base + retry);
            let _ = n.take(Notice::Checkpoint(0, 3));
            assert!(
                REFUSE.with(std::cell::RefCell::take).is_empty(),
                "PRECONDITION: the `dropped` line was refused, and again at its retry"
            );
            assert_eq!(
                on_store(&n),
                (false, 0, Some(2)),
                "crash={crash}: the line refused, neither its count nor a cursor past it is on \
                 the store"
            );
            if crash {
                drop(n);
                let (back, paged) = restart(&dir, &rows, 1);
                assert_eq!(
                    (paged, on_store(&back)),
                    (vec![1], (true, 1, Some(3))),
                    "the restart judged 2 again: dropped and counted once"
                );
            } else {
                n.clock = Some(base + retry * 2);
                n.tick(n.now()).expect("the retry");
                assert_eq!(
                    on_store(&n),
                    (true, 1, Some(3)),
                    "the retry wrote the line, its count, then the cursor past it"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// What a run left: the offsets whose command ran (each once, however often), the
    /// journal's verdicts, the suppressed count and the stamps spent, as the store holds them.
    type Left = (Vec<u64>, Vec<(u64, String)>, u64, usize);

    #[test]
    fn a_kill_between_a_verdict_and_its_count_neither_charges_nor_counts_a_record_twice() {
        let key = Sel::Attention.key();
        let raise = |sid: &str| {
            (
                presence_of(sid),
                format!("v=1 t=1 attention=help-{sid}").into_bytes(),
            )
        };
        let quiet = || (presence_of("s-q"), b"v=1 t=1 state=live".to_vec());
        let records: Vec<(u64, String, Vec<u8>)> = [
            quiet(),
            raise("s-a"),
            quiet(),
            raise("s-b"),
            raise("s-c"),
            raise("s-d"),
            raise("s-e"),
            quiet(),
        ]
        .into_iter()
        .zip(1..)
        .map(|((subject, body), off)| (off, subject, body))
        .collect();
        let open = |dir: &str| {
            let (mut n, fired) = appending(dir, 3, "");
            n.cfg.window_ms = 3_600_000;
            n.cursor_every = Duration::MAX;
            n.take(Notice::Log(0, strict_log())).expect("log");
            (n, fired)
        };
        let feed = |n: &mut Notifier| -> io::Result<()> {
            let from = n.log_store.cursor(&key).unwrap_or(0);
            for (off, subject, body) in records.iter().filter(|(off, ..)| *off >= from) {
                n.take(Notice::Record(0, *off, subject.clone(), body.clone()))?;
            }
            Ok(())
        };
        let left = |n: &Notifier, fired: &Path| -> Left {
            let mut paged = fired_offsets(fired);
            paged.sort_unstable();
            paged.dedup();
            let verdicts = records
                .iter()
                .filter_map(|(off, ..)| n.verdict(*off).map(|v| (*off, v.to_string())))
                .collect();
            (
                paged,
                verdicts,
                n.store.suppressed().0,
                n.store.stamps().len(),
            )
        };

        let dir = scratch("kill-sweep");
        let (mut n, fired) = open(&dir);
        PUT_FAULT.with(|left| left.set(Some(u32::MAX)));
        feed(&mut n).expect("the run no kill stops");
        let writes = u32::MAX - PUT_FAULT.with(std::cell::Cell::take).expect("counted");
        let whole = left(&n, &fired);
        assert_eq!(
            whole,
            (
                vec![2, 4, 5],
                vec![
                    (2, "fired".into()),
                    (4, "fired".into()),
                    (5, "fired".into()),
                    (6, "dropped".into()),
                    (7, "dropped".into()),
                ],
                2,
                3
            ),
            "PRECONDITION: a budget of 3 pages 2, 4 and 5 and drops 6 and 7"
        );
        drop(n);
        let _ = std::fs::remove_dir_all(&dir);

        let mut unrestored = Vec::new();
        for kill in 0..writes {
            for restore in [true, false] {
                let dir = scratch(&format!("kill-sweep-{kill}-{restore}"));
                let (mut n, _) = open(&dir);
                PUT_FAULT.with(|left| left.set(Some(kill)));
                let _ = feed(&mut n);
                assert_eq!(
                    PUT_FAULT.with(std::cell::Cell::take),
                    Some(0),
                    "PRECONDITION: the run reached write {kill} of {writes}, and wrote nothing \
                     past it, as if it died there"
                );
                drop(n);
                let (mut back, fired) = open(&dir);
                back.restore_due = restore;
                feed(&mut back).expect("the restart reads on from its cursor");
                let after = left(&back, &fired);
                if restore {
                    assert_eq!(
                        after, whole,
                        "killed before write {kill} of {writes}: the restart pages and drops \
                         what the whole run did, spends its budget once a page and counts each \
                         drop once"
                    );
                } else if after != whole {
                    unrestored.push(kill);
                }
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        assert_eq!(
            unrestored.len(),
            5,
            "NEGATIVE CONTROL: with no restore, a kill between each page's `firing` line and its \
             stamps (3) or each drop's line and its count (2) under-spends or under-counts: \
             {unrestored:?}"
        );
    }

    #[test]
    fn the_flush_tally_tells_every_flush_a_carried_generation_asks_for() {
        let dir = scratch("tally-carry");
        let store = Store::open(&dir).expect("open");
        let g = |generation| astream_broker::LogIdentity {
            id: [7; 16],
            generation,
        };
        store.for_log(Some(g(0)), None).unwrap();
        let one = store.for_log(Some(g(1)), None).unwrap();
        one.set_cursor("attention", 9).unwrap();
        one.set_journal(&["2 fired rc=0".to_string()], Flush::Ordered)
            .unwrap();
        one.set_standing(&[("/f/f1/pub/n-a/s-x/presence".into(), "help".into())])
            .unwrap();

        let recording = aterm_durable::seam::Recording::start();
        NOTED.with(|noted| noted.borrow_mut().clear());
        let two = store
            .for_log(Some(g(2)), Some(&lineage_of(&[(1, 3), (2, 5)])))
            .unwrap();
        let noted = NOTED.with(|noted| noted.take());
        assert_eq!(two.cursor("attention"), Some(5), "PRECONDITION: carried");
        assert_eq!(
            noted.iter().filter(|f| f.is_durable()).count(),
            3,
            "the carried store's `cur/` and its root, then `logs/` once it is renamed in, each \
             flushed in full: {noted:?}"
        );
        assert_eq!(
            noted,
            recording.take(),
            "the tally tells each flush the carry asked of aterm_durable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ordered_broker(log: &Path) -> Option<astream_broker::Broker> {
        let mut opts = crate::durability::Durability::Ordered.options();
        opts.durability = astream_broker::Durability::Ordered { horizon: None };
        let broker =
            astream_broker::Broker::open_opts(log, opts, || Ok(astream_broker::Prepared::Nothing))
                .expect("open");
        (broker.served_durability().served == astream_broker::Tier::Ordered).then_some(broker)
    }

    #[cfg(unix)]
    #[test]
    fn an_ordered_broker_pages_a_record_only_once_it_is_durable() {
        for wait in [true, false] {
            let dir = scratch(&format!("ord-{wait}"));
            let base = PathBuf::from(&dir);
            std::fs::create_dir_all(&base).expect("scratch");
            let log = base.join("o.log");
            let sock = base.join("o.sock");
            let Some(broker) = ordered_broker(&log) else {
                eprintln!("ordered is served strict here: nothing waits, nothing to prove");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            };
            let _server = broker.serve(&sock).unwrap();
            let fired = base.join("fired");
            let mut c = cfg(&dir);
            c.broker = sock.to_string_lossy().into_owned();
            c.since = Since::Start;
            c.limit = 10;
            c.on = vec![Sel::Attention, Sel::Halt];
            c.exec = format!(
                "printf '%s %s\\n' \"$ATERM_NOTIFY_ON\" \"$ATERM_NOTIFY_OFFSET\" >> {}",
                fired.display()
            );
            let run = || {
                let mut n = Notifier::new(c.clone()).expect("open");
                n.wait_durable = wait;
                n.catch_up().expect("catch up");
                (
                    std::fs::read_to_string(&fired).unwrap_or_default(),
                    n.log_store.cursor(&Sel::Attention.key()),
                )
            };
            let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
            let raised = conn
                .publish(
                    7,
                    1,
                    "/f/f1/pub/n-a/s-x/presence",
                    b"v=1 t=1 attention=needs%20a%20key",
                )
                .unwrap()
                .0;
            let halt = conn
                .publish(
                    8,
                    1,
                    "/f/f1/fleet/h-x/halt",
                    b"v=1 t=1 state=on reason=stop",
                )
                .unwrap()
                .0;
            assert!(broker.durable_head() > halt, "the halt is escalated");
            let waits = conn
                .publish(
                    7,
                    2,
                    "/f/f1/pub/n-b/s-y/presence",
                    b"v=1 t=1 attention=disk",
                )
                .unwrap()
                .0;
            assert!(raised < halt && broker.durable_head() <= waits);
            let (first, at) = run();
            if wait {
                assert_eq!(
                    first,
                    format!("attention {raised}\nhalt {halt}\n"),
                    "only what is durable pages"
                );
                assert_eq!(at, Some(waits), "the cursor stays AT the row that waits");
                broker.flush_durable().expect("a full flush");
                assert!(broker.durable_head() > waits);
                let (second, at) = run();
                assert_eq!(
                    second,
                    format!("attention {raised}\nhalt {halt}\nattention {waits}\n"),
                    "durable now: paged, once"
                );
                assert_eq!(at, Some(waits + 1));
                assert_eq!(run().0, second, "and never again");
            } else {
                assert_eq!(
                    first,
                    format!("attention {raised}\nattention {waits}\nhalt {halt}\n"),
                    "NEGATIVE CONTROL: paged before any full flush covered it"
                );
            }
            drop(conn);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    fn pump(
        n: &mut Notifier,
        rx: &mpsc::Receiver<Notice>,
        what: &str,
        done: impl Fn(&Notifier) -> bool,
    ) -> &'static str {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let mut last = "none";
        while !done(n) {
            assert!(Instant::now() < deadline, "{what}: never happened");
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(notice) => {
                    last = match notice {
                        Notice::Record(..) => "record",
                        Notice::Warm(..) => "warm",
                        Notice::Checkpoint(..) => "checkpoint",
                        Notice::Log(..) => "log",
                        Notice::Durable(..) => "durable",
                    };
                    n.take(notice).expect("take");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: the reader ended"),
            }
        }
        last
    }

    #[cfg(unix)]
    #[test]
    fn in_follow_mode_the_horizon_alone_pages_an_attention_row() {
        for wait in [true, false] {
            let dir = scratch(&format!("horizon-{wait}"));
            let base = PathBuf::from(&dir);
            std::fs::create_dir_all(&base).expect("scratch");
            let log = base.join("h.log");
            let sock = base.join("h.sock");
            let broker = astream_broker::Broker::open_opts(
                &log,
                crate::durability::Durability::Ordered.options(),
                || Ok(astream_broker::Prepared::Nothing),
            )
            .expect("open");
            let served = broker.served_durability();
            if served.served != astream_broker::Tier::Ordered {
                eprintln!("ordered is served strict here: nothing waits, nothing to prove");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            assert_eq!(served.horizon_ms, Some(100), "aterm's own horizon");
            let mut server = broker.serve(&sock).unwrap();
            let fired = base.join("fired");
            let mut c = cfg(&dir);
            c.broker = sock.to_string_lossy().into_owned();
            c.since = Since::Start;
            c.once = false;
            c.limit = 10;
            c.exec = format!(
                "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
                fired.display()
            );
            let mut n = Notifier::new(c.clone()).expect("open");
            n.wait_durable = wait;
            let (tx, rx) = notice_handoff();
            let follower = {
                let root = n.store.root.clone();
                std::thread::spawn(move || reader(0, &Sel::Attention, &c, &root, &tx))
            };
            pump(&mut n, &rx, "the reader's subscription", |_| {
                broker.stats().parked_tails == 1
            });
            let start = broker.flush_counts().full;
            let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
            let raised = conn
                .publish(
                    7,
                    1,
                    "/f/f1/pub/n-a/s-x/presence",
                    b"v=1 t=1 attention=needs%20a%20key",
                )
                .unwrap()
                .0;
            let by = pump(&mut n, &rx, "the page", |_| {
                !fired_offsets(&fired).is_empty()
            });
            assert_eq!(fired_offsets(&fired), [raised], "{wait}: paged once");
            if wait {
                assert_eq!(by, "durable", "the durable mark is what paged it");
                assert!(broker.durable_head() > raised);
                assert_eq!(
                    broker.flush_counts().full,
                    start + 1,
                    "one full flush, the horizon's"
                );
                assert_eq!(n.held_bytes, 0);
                assert_eq!(n.log_store.cursor(&Sel::Attention.key()), Some(raised + 1));
            } else {
                assert_eq!(by, "record", "NEGATIVE CONTROL: paged as it was read");
            }
            drop(rx);
            conn.publish(
                7,
                2,
                "/f/f1/pub/n-a/s-y/presence",
                b"v=1 t=1 attention=disk",
            )
            .unwrap();
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the reader ends",
                || follower.is_finished().then_some(()),
            );
            follower.join().expect("the reader");
            drop(conn);
            server.shutdown();
            drop(broker);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    fn lineage_of(steps: &[(u64, u64)]) -> astream_broker::Lineage {
        astream_broker::Lineage::from_steps(steps).expect("rising generations, at most MAX")
    }

    #[test]
    fn the_cursor_never_goes_backwards() {
        let dir = scratch("cursor");
        let store = Store::open(&dir).expect("open");
        store.set_cursor("attention", 40).expect("write");
        store.set_cursor("attention", 9).expect("write");
        assert_eq!(store.cursor("attention"), Some(40));
        store.set_cursor("attention", 41).expect("write");
        assert_eq!(store.cursor("attention"), Some(41));
        assert_eq!(store.cursor("halt"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selectors_parse_and_spell_a_filename() {
        assert_eq!(Sel::parse("attention"), Ok(Sel::Attention));
        assert_eq!(Sel::parse("halt"), Ok(Sel::Halt));
        assert_eq!(Sel::parse("ask:h-andrew"), Ok(Sel::Ask("h-andrew".into())));
        assert_eq!(Sel::Ask("h-andrew".into()).key(), "ask-to.h-andrew");
        assert_eq!(
            Sel::Ask("h-andrew".into()).filter("f1"),
            "/f/f1/in/p/h-andrew/*/ask"
        );
        for bad in ["ask:../../etc", "ask:", "ask:H-Andrew", "wake", "ask"] {
            assert!(Sel::parse(bad).is_err(), "{bad}");
        }
    }

    /// The bodies a session log's mirror lands on the bus, take by take, as the bridge takes
    /// them ([`aterm_slog::Log::bus_copy`]).
    fn mirrored(log: &mut aterm_slog::Log) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let take = log
                .bus_copy(64, aterm_slog::Stamp::new(9, 9))
                .encode(aterm_slog::Tier::Wire, &|_| None);
            if take.slots.is_empty() {
                return out;
            }
            out.extend(take.slots.iter().map(|s| s.body.clone()));
            log.land(take.slots.last().expect("a slot").at)
                .expect("lands");
            if !take.more {
                return out;
            }
        }
    }

    /// A session log that a test drives through needs: `failure` from a shell command's
    /// failed run, lifted by the next one of its kind, and `question` from a step.
    struct Needs {
        log: aterm_slog::Log,
        ms: u64,
    }

    impl Needs {
        fn open() -> Self {
            let mut log = aterm_slog::Log::new(1);
            log.open_session(
                aterm_slog::Kind::Spawn,
                aterm_slog::Src::Aterm,
                aterm_slog::Conf::Exact,
                aterm_slog::Actor::Aterm,
                aterm_slog::Fields::new(),
                aterm_slog::Stamp::new(1_760_000_000_000, 0),
            )
            .expect("opens");
            Self { log, ms: 0 }
        }

        fn at(&mut self) -> aterm_slog::Stamp {
            self.ms += 1;
            aterm_slog::Stamp::new(1_760_000_000_000 + self.ms, self.ms)
        }

        fn cmd_open(&mut self) -> aterm_slog::Node {
            let at = self.at();
            self.log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Cmd,
                        aterm_slog::Src::Shell,
                        aterm_slog::Conf::Strong,
                    ),
                    at,
                )
                .expect("a command")
        }

        fn cmd_close(&mut self, cmd: aterm_slog::Node, out: aterm_slog::RunOut) {
            let at = self.at();
            self.log
                .close(
                    cmd,
                    aterm_slog::Close::new(
                        aterm_slog::Out::Run(out),
                        aterm_slog::Src::Shell,
                        aterm_slog::Conf::Exact,
                    ),
                    at,
                )
                .expect("closes");
        }

        fn question(&mut self) -> aterm_slog::Node {
            let turn = self.turn();
            self.ask(turn)
        }

        /// An agent run and a turn under it: the turn.
        fn turn(&mut self) -> aterm_slog::Node {
            let at = self.at();
            let run = self
                .log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Agent,
                        aterm_slog::Src::Status,
                        aterm_slog::Conf::Strong,
                    ),
                    at,
                )
                .expect("an agent run");
            let at = self.at();
            self.log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Turn,
                        aterm_slog::Src::Screen,
                        aterm_slog::Conf::Strong,
                    )
                    .under(run),
                    at,
                )
                .expect("a turn")
        }

        /// A question asked under `turn`.
        fn ask(&mut self, turn: aterm_slog::Node) -> aterm_slog::Node {
            let at = self.at();
            self.log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Question,
                        aterm_slog::Src::Screen,
                        aterm_slog::Conf::Strong,
                    )
                    .under(turn),
                    at,
                )
                .expect("a question")
        }

        /// The question `step` answered, which lifts its need.
        fn answer(&mut self, step: aterm_slog::Node) {
            let at = self.at();
            self.log
                .close(
                    step,
                    aterm_slog::Close::new(
                        aterm_slog::Out::Step(aterm_slog::StepOut::Answered),
                        aterm_slog::Src::Screen,
                        aterm_slog::Conf::Strong,
                    ),
                    at,
                )
                .expect("answered");
        }

        /// Mail delivered to the session, which moves its counts: the mirror owes a snapshot.
        fn mail(&mut self) {
            let at = self.at();
            self.log
                .mark(
                    aterm_slog::Mark::new(
                        aterm_slog::Kind::Mail,
                        aterm_slog::Src::Fabric,
                        aterm_slog::Conf::Exact,
                    )
                    .fields(
                        aterm_slog::Fields::new()
                            .word(aterm_slog::Key::Dir, "in")
                            .expect("dir"),
                    ),
                    at,
                )
                .expect("mail");
        }

        /// A mark that moves no need (a handoff naming no epoch).
        fn quiet(&mut self) {
            let at = self.at();
            self.log
                .mark(
                    aterm_slog::Mark::new(
                        aterm_slog::Kind::Handoff,
                        aterm_slog::Src::Aterm,
                        aterm_slog::Conf::Exact,
                    )
                    .at(aterm_slog::Node::SESSION)
                    .by(aterm_slog::Actor::Aterm),
                    at,
                )
                .expect("a handoff mark");
        }

        /// The session's close (its shell exited), which raises `exited` and drops every other
        /// need.
        fn exit(&mut self) {
            let at = self.at();
            self.log
                .close_session(
                    aterm_slog::Close::new(
                        aterm_slog::Out::Session(aterm_slog::SessionOut::ShellExit),
                        aterm_slog::Src::Shell,
                        aterm_slog::Conf::Exact,
                    ),
                    at,
                )
                .expect("closes");
        }

        fn bodies(&mut self) -> Vec<String> {
            mirrored(&mut self.log)
        }
    }

    const NEED_FACE: &str = "/f/f1/pub/n-a/s-x/ev";

    #[cfg(unix)]
    #[test]
    fn a_need_pages_once_per_rise_and_a_restart_pages_nothing_again() {
        let dir = scratch("need");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("n.log")).unwrap();
        let sock = base.join("n.sock");
        let mut server = broker.serve(&sock).unwrap();
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let mut seq = 0;
        let mut publish = |bodies: Vec<String>| -> Vec<(u64, String)> {
            bodies
                .into_iter()
                .map(|b| {
                    seq += 1;
                    let off = conn.publish(7, seq, NEED_FACE, b.as_bytes()).unwrap().0;
                    (off, b)
                })
                .collect()
        };
        let mut s = Needs::open();
        let first = s.cmd_open();
        s.cmd_close(first, aterm_slog::RunOut::Fail);
        let rise = publish(s.bodies());
        let second = s.cmd_open();
        let standing = publish(s.bodies());
        s.cmd_close(second, aterm_slog::RunOut::Ok);
        let clear = publish(s.bodies());
        let third = s.cmd_open();
        s.cmd_close(third, aterm_slog::RunOut::Fail);
        let again = publish(s.bodies());
        s.question();
        let moved = publish(s.bodies());
        let fail_close = |page: &[(u64, String)]| {
            page.iter()
                .find(|(_, b)| b.contains(" op=close ") && b.contains("out=fail"))
                .map(|(off, _)| *off)
                .expect("the failed run's close")
        };
        let (rose, rose_again) = (fail_close(&rise), fail_close(&again));
        let asked = moved
            .iter()
            .find(|(_, b)| b.contains(" op=open ") && b.contains("k=question"))
            .map(|(off, _)| *off)
            .expect("the question's open");
        assert!(
            !standing.is_empty() && !clear.is_empty(),
            "a repeat and a clear on the bus"
        );

        let run = |tag: &str, on: Sel| -> Vec<String> {
            let state = base.join(tag);
            let fired = base.join(format!("{tag}.fired"));
            let mut c = cfg(&state.to_string_lossy());
            c.broker = sock.to_string_lossy().into_owned();
            c.since = Since::Start;
            c.limit = 100;
            c.on = vec![on];
            c.exec = format!(
                "printf '%s %s %s %s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_ON\" \
                 \"$ATERM_NOTIFY_NEED\" \"$ATERM_NOTIFY_SID\" \"$ATERM_NOTIFY_NODE\" >> {}",
                fired.display()
            );
            Notifier::new(c)
                .expect("open")
                .catch_up()
                .expect("catch up");
            std::fs::read_to_string(&fired)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        };
        let line = |off: u64, word: &str| format!("{off} need {word} s-x n-a");
        let failure = Sel::Need(Some(vec!["failure"]));
        assert_eq!(
            run("failure", failure.clone()),
            [line(rose, "failure"), line(rose_again, "failure")],
            "need:failure pages each rise to failure once"
        );
        assert_eq!(
            run("any", Sel::Need(None)),
            [
                line(rose, "failure"),
                line(rose_again, "failure"),
                line(asked, "question")
            ],
            "need pages each rise, to any word"
        );

        for _ in 0..=aterm_slog::SNAP_EVERY {
            s.quiet();
        }
        let after = publish(s.bodies());
        assert!(
            after.iter().any(|(_, b)| b.contains(" op=snap ")),
            "a snapshot of the standing needs reaches the bus after the restart"
        );
        assert_eq!(
            run("failure", failure).len(),
            2,
            "a restart pages nothing again"
        );
        assert_eq!(
            run("any", Sel::Need(None)).len(),
            3,
            "nor for a need standing through the restart and a snapshot"
        );
        server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_following_need_reader_folds_below_its_cursor_before_it_judges() {
        let dir = scratch("need-follow");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("f.log")).unwrap();
        let sock = base.join("f.sock");
        let mut server = broker.serve(&sock).unwrap();
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let mut seq = 0;
        let mut publish = |conn: &mut Conn, face: &str, bodies: Vec<String>| -> Option<u64> {
            let mut rose = None;
            for b in bodies {
                seq += 1;
                let off = conn.publish(7, seq, face, b.as_bytes()).unwrap().0;
                if b.contains(" op=close ") && b.contains("out=fail") {
                    rose = Some(off);
                }
            }
            rose
        };
        let failing = |s: &mut Needs| {
            let cmd = s.cmd_open();
            s.cmd_close(cmd, aterm_slog::RunOut::Fail);
            s.bodies()
        };
        let mut x = Needs::open();
        let rose = publish(&mut conn, NEED_FACE, failing(&mut x)).expect("a rise");
        let fired = base.join("fired");
        let mut c = cfg(&dir);
        c.broker = sock.to_string_lossy().into_owned();
        c.since = Since::Start;
        c.limit = 10;
        c.on = vec![Sel::Need(None)];
        c.exec = format!(
            "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        Notifier::new(c.clone())
            .expect("open")
            .catch_up()
            .expect("catch up");
        assert_eq!(fired_offsets(&fired), [rose]);

        c.once = false;
        let mut n = Notifier::new(c.clone()).expect("reopen");
        let (tx, rx) = notice_handoff();
        let follower = {
            let root = n.store.root.clone();
            std::thread::spawn(move || reader(0, &Sel::Need(None), &c, &root, &tx))
        };
        pump(&mut n, &rx, "the reader's subscription", |_| {
            broker.stats().parked_tails == 1
        });
        for _ in 0..=aterm_slog::SNAP_EVERY {
            x.quiet();
        }
        let quiet = x.bodies();
        assert!(quiet.iter().any(|b| b.contains(" op=snap ")), "a snapshot");
        publish(&mut conn, NEED_FACE, quiet);
        let mut y = Needs::open();
        let other = publish(&mut conn, "/f/f1/pub/n-a/s-y/ev", failing(&mut y)).expect("a rise");
        pump(&mut n, &rx, "the other session's page", |_| {
            fired_offsets(&fired).contains(&other)
        });
        assert_eq!(
            fired_offsets(&fired),
            [rose, other],
            "the standing failure, reseeded by a snapshot past the cursor, pages nothing again"
        );
        drop(rx);
        publish(&mut conn, NEED_FACE, {
            x.quiet();
            x.bodies()
        });
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the reader ends", || {
            follower.is_finished().then_some(())
        });
        follower.join().expect("the reader");
        drop(conn);
        server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The words of the pairs standing on `face` ([`Notifier::pairs_of`]).
    fn standing_words(n: &Notifier, face: &str) -> Vec<&'static str> {
        n.pairs_of(face).into_iter().map(|p| p.word).collect()
    }

    #[test]
    fn an_exited_session_clears_its_standing_needs_and_a_will_or_a_relaunch_does_not() {
        for will in ["away", "gone"] {
            let dir = scratch(&format!("need-gone-{will}"));
            let (mut n, fired) = appending(&dir, 100, "");
            n.cfg.on = vec![Sel::Need(None)];
            let sel = Sel::Need(None);
            let mut off = 0;
            let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
                off += 1;
                n.on_record(&sel, off, subject, body).expect("on_record");
                off
            };
            let faces = ["s-x", "s-y"].map(|sid| format!("/f/f1/pub/n-a/{sid}/ev"));
            let mut rises = Vec::new();
            for face in &faces {
                let mut s = Needs::open();
                let cmd = s.cmd_open();
                s.cmd_close(cmd, aterm_slog::RunOut::Fail);
                for b in s.bodies() {
                    let at = feed(&mut n, face, b.as_bytes());
                    if b.contains("out=fail") {
                        rises.push(at);
                    }
                }
            }
            assert_eq!(fired_offsets(&fired), rises, "each session's failure pages");
            let standing = |n: &Notifier| faces.clone().map(|f| standing_words(n, &f));
            let both = [vec!["failure"], vec!["failure"]];
            assert_eq!(standing(&n), both);

            feed(
                &mut n,
                "/f/f1/pub/n-a/node/presence",
                b"v=1 t=1 state=live inc=2",
            );
            feed(
                &mut n,
                "/f/f1/pub/n-a/s-x/presence",
                b"v=1 t=1 inc=1 state=live",
            );
            assert_eq!(
                standing(&n),
                both,
                "{will}: a relaunch whose bridge has not republished the session is not its end"
            );
            feed(
                &mut n,
                "/f/f1/pub/n-a/s-x/presence",
                b"v=1 t=2 inc=2 state=exited",
            );
            assert_eq!(
                standing(&n),
                [vec![], vec!["failure"]],
                "{will}: an exited session's needs clear"
            );
            feed(
                &mut n,
                "/f/f1/pub/n-a/node/presence",
                format!("v=1 t=3 state={will} inc=2 fabric=disconnected").as_bytes(),
            );
            assert_eq!(
                standing(&n),
                [vec![], vec!["failure"]],
                "{will}: a node's will keeps its sessions' needs standing"
            );
            assert_eq!(
                fired_offsets(&fired),
                rises,
                "{will}: and nothing pages for either"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_need_standing_since_before_the_warm_window_pages_nothing_again_after_a_restart() {
        let dir = scratch("need-window");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("w.log")).unwrap();
        let sock = base.join("w.sock");
        let mut server = broker.serve(&sock).unwrap();
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let mut seq = 0;
        let mut publish = |conn: &mut Conn, subject: &str, body: &[u8]| -> u64 {
            seq += 1;
            conn.publish(7, seq, subject, body).unwrap().0
        };
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let mut rose = None;
        for b in s.bodies() {
            let off = publish(&mut conn, NEED_FACE, b.as_bytes());
            if b.contains("out=fail") {
                rose = Some(off);
            }
        }
        let rose = rose.expect("the failed run's close");
        for _ in 0..crate::notify_need::WARM_WINDOW + 10 {
            publish(&mut conn, "/f/f1/pub/n-a/s-z/filler", b"v=1 t=1");
        }
        let state = base.join("state");
        let fired = base.join("fired");
        let run = || {
            let mut c = cfg(&state.to_string_lossy());
            c.broker = sock.to_string_lossy().into_owned();
            c.since = Since::Start;
            c.limit = 100;
            c.on = vec![Sel::Need(None)];
            c.exec = format!(
                "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
                fired.display()
            );
            let mut n = Notifier::new(c).expect("open");
            n.catch_up().expect("catch up");
            fired_offsets(&fired)
        };
        assert_eq!(run(), [rose], "the rise pages once");

        s.quiet();
        let partial = s.bodies();
        assert!(
            partial.iter().all(|b| !b.contains(" op=snap ")),
            "PRECONDITION: a record the restarted watch folds from no start"
        );
        for b in partial {
            publish(&mut conn, NEED_FACE, b.as_bytes());
        }
        assert_eq!(
            run(),
            [rose],
            "a partial fold past the warm window clears nothing"
        );
        for _ in 0..=aterm_slog::SNAP_EVERY {
            s.quiet();
        }
        let reseeded = s.bodies();
        assert!(
            reseeded.iter().any(|b| b.contains(" op=snap ")),
            "PRECONDITION: a snapshot reseeds the standing failure"
        );
        for b in reseeded {
            publish(&mut conn, NEED_FACE, b.as_bytes());
        }
        assert_eq!(
            run(),
            [rose],
            "the snapshot's need was standing: nothing pages again"
        );
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Ok);
        s.question();
        let mut asked = None;
        for b in s.bodies() {
            let off = publish(&mut conn, NEED_FACE, b.as_bytes());
            if b.contains("k=question") {
                asked = Some(off);
            }
        }
        assert_eq!(
            run(),
            [rose, asked.expect("the question's open")],
            "the seeded fold judges again: the next rise pages"
        );
        drop(conn);
        server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sessions_exit_pages_once_whichever_of_its_close_and_its_row_comes_first() {
        let presence = "/f/f1/pub/n-a/s-x/presence";
        let exited: &[u8] = b"v=1 t=2 inc=1 state=exited";
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let failing = s.bodies();
        s.exit();
        let closing = s.bodies();
        assert!(
            closing.iter().any(|b| b.contains(" op=close node=- ")),
            "PRECONDITION: the session's close: {closing:?}"
        );
        for (tag, sel) in [
            ("exited", Sel::Need(Some(vec!["exited"]))),
            ("any", Sel::Need(None)),
        ] {
            for close_first in [true, false] {
                let dir = scratch(&format!("need-exit-{tag}-{close_first}"));
                let (mut n, fired) = appending(&dir, 100, "");
                n.cfg.on = vec![sel.clone()];
                let mut off = 0;
                let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
                    off += 1;
                    n.on_record(&sel, off, subject, body).expect("on_record");
                    off
                };
                let mut want = Vec::new();
                for b in &failing {
                    let at = feed(&mut n, NEED_FACE, b.as_bytes());
                    if b.contains("out=fail") && tag == "any" {
                        want.push(at);
                    }
                }
                if !close_first {
                    feed(&mut n, presence, exited);
                }
                for b in &closing {
                    let at = feed(&mut n, NEED_FACE, b.as_bytes());
                    if b.contains(" op=close node=- ") {
                        want.push(at);
                    }
                }
                if close_first {
                    feed(&mut n, presence, exited);
                }
                feed(
                    &mut n,
                    "/f/f1/pub/n-a/node/presence",
                    b"v=1 t=3 state=gone inc=1",
                );
                assert_eq!(
                    fired_offsets(&fired),
                    want,
                    "{tag}, close first {close_first}: the exit pages once, at the close"
                );
                assert_eq!(
                    standing_words(&n, NEED_FACE),
                    ["exited"],
                    "{tag}, close first {close_first}: and stands"
                );
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    #[test]
    fn a_record_held_for_durability_is_judged_before_a_reconnects_warm_up_folds_it() {
        let log = ReadLog {
            id: Some(astream_broker::LogIdentity {
                id: [7; 16],
                generation: 0,
            }),
            lineage: None,
            ordered: true,
        };
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Ok);
        let records: Vec<(u64, String)> = (1..).zip(s.bodies()).collect();
        let rose = records
            .iter()
            .find(|(_, b)| b.contains("out=fail"))
            .map(|(off, _)| *off)
            .expect("the failed run's close");
        assert!(
            records
                .iter()
                .any(|(off, b)| *off > rose && b.contains("out=ok")),
            "PRECONDITION: the run that lifts it comes after"
        );
        let dir = scratch("need-held");
        let (mut n, fired) = appending(&dir, 100, "");
        n.cfg.on = vec![Sel::Need(None)];
        let mut notices = vec![Notice::Log(0, log.clone())];
        for (off, b) in &records {
            notices.push(Notice::Record(
                0,
                *off,
                NEED_FACE.into(),
                b.clone().into_bytes(),
            ));
        }
        notices.push(Notice::Log(0, log));
        for (off, b) in &records {
            notices.push(Notice::Warm(
                0,
                *off,
                NEED_FACE.into(),
                b.clone().into_bytes(),
            ));
        }
        for notice in notices {
            n.take(notice).expect("take");
        }
        assert!(fired_offsets(&fired).is_empty(), "held, not paged");
        n.take(Notice::Durable(0, u64::MAX)).expect("durable");
        assert_eq!(
            fired_offsets(&fired),
            [rose],
            "the held failure is judged by the fold as it stood at its own record"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each line a test's command wrote to `fired`.
    fn fired_lines(fired: &Path) -> Vec<String> {
        std::fs::read_to_string(fired)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A need test's config over the broker at `sock`, its state `state`: `on` from `since`, its
    /// command writing `<offset> <need>` to `<state>.fired`.
    fn need_cfg(sock: &Path, state: &Path, on: Sel, since: Since) -> (Config, PathBuf) {
        let fired = state.with_extension("fired");
        let mut c = cfg(&state.to_string_lossy());
        c.broker = sock.to_string_lossy().into_owned();
        c.since = since;
        c.limit = 100;
        c.on = vec![on];
        c.exec = format!(
            "printf '%s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_NEED\" >> {}",
            fired.display()
        );
        (c, fired)
    }

    /// One `--once` run of [`need_cfg`]: every line its command has written on that state.
    fn need_once(sock: &Path, state: &Path, on: Sel, since: Since) -> Vec<String> {
        let (c, fired) = need_cfg(sock, state, on, since);
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        fired_lines(&fired)
    }

    /// A broker in `dir` for a need test, and a publisher on it: each body to `subject`, its
    /// offset with it.
    struct NeedBus {
        server: astream_broker::BrokerHandle,
        conn: Conn,
        seq: u64,
        sock: PathBuf,
    }

    impl NeedBus {
        fn open(base: &Path, broker: &astream_broker::Broker) -> Self {
            let sock = base.join("b.sock");
            let server = broker.serve(&sock).unwrap();
            let (conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
            Self {
                server,
                conn,
                seq: 0,
                sock,
            }
        }

        fn publish(&mut self, subject: &str, bodies: Vec<String>) -> Vec<(u64, String)> {
            bodies
                .into_iter()
                .map(|b| {
                    self.seq += 1;
                    let off = self
                        .conn
                        .publish(7, self.seq, subject, b.as_bytes())
                        .unwrap()
                        .0;
                    (off, b)
                })
                .collect()
        }

        fn row(&mut self, subject: &str, body: &str) -> u64 {
            self.publish(subject, vec![body.to_string()])[0].0
        }
    }

    /// The offset of the first of `page`'s bodies that holds every one of `words`.
    fn offset_of(page: &[(u64, String)], words: &[&str]) -> u64 {
        page.iter()
            .find(|(_, b)| words.iter().all(|w| b.contains(w)))
            .map(|(off, _)| *off)
            .unwrap_or_else(|| panic!("no body holds {words:?}: {page:?}"))
    }

    #[cfg(unix)]
    #[test]
    fn a_notifier_started_over_a_standing_need_pages_only_what_rises_past_its_cursor() {
        let dir = scratch("need-head");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        bus.publish(NEED_FACE, s.bodies());
        let once = base.join("once");
        let none: Vec<String> = Vec::new();
        assert_eq!(
            need_once(&sock, &once, Sel::Need(None), Since::Head),
            none,
            "a fresh notifier at the head pages nothing at its start"
        );

        let (mut c, fired) = need_cfg(&sock, &base.join("follow"), Sel::Need(None), Since::Head);
        c.once = false;
        let mut n = Notifier::new(c.clone()).expect("open");
        let (tx, rx) = notice_handoff();
        let follower = {
            let root = n.store.root.clone();
            std::thread::spawn(move || reader(0, &Sel::Need(None), &c, &root, &tx))
        };
        pump(&mut n, &rx, "the reader's subscription", |_| {
            broker.stats().parked_tails == 1
        });

        s.mail();
        let snapped = bus.publish(NEED_FACE, s.bodies());
        assert!(
            snapped.iter().any(|(_, b)| b.contains(" op=snap ")),
            "PRECONDITION: the mail count's snapshot reaches the bus: {snapped:?}"
        );
        s.cmd_open();
        let opened = bus.publish(NEED_FACE, s.bodies());
        assert!(
            !opened.is_empty(),
            "PRECONDITION: a run opens past the cursor"
        );
        s.question();
        let asked = offset_of(&bus.publish(NEED_FACE, s.bodies()), &["k=question"]);
        pump(&mut n, &rx, "a page", |_| !fired_lines(&fired).is_empty());
        let question = [format!("{asked} question")];
        assert_eq!(
            fired_lines(&fired),
            question,
            "the follower pages the question alone: the failure stood before it started"
        );
        assert_eq!(
            need_once(&sock, &once, Sel::Need(None), Since::Head),
            question,
            "so does the --once run over the snapshot, the open and the question"
        );
        assert_eq!(
            need_once(&sock, &once, Sel::Need(None), Since::Head),
            question,
            "and a run again pages nothing more"
        );
        assert_eq!(
            need_once(&sock, &base.join("late"), Sel::Need(None), Since::Head),
            none,
            "a notifier started now pages neither need: both stood before it"
        );
        drop(rx);
        s.quiet();
        bus.publish(NEED_FACE, s.bodies());
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the reader ends", || {
            follower.is_finished().then_some(())
        });
        follower.join().expect("the reader");
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_rise_in_a_session_older_than_the_warm_window_pages_once() {
        let dir = scratch("need-old");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let mut s = Needs::open();
        let start = bus.publish(NEED_FACE, s.bodies());
        assert!(
            start.iter().any(|(_, b)| b.contains(" op=snap ")),
            "PRECONDITION: the session's start and its first snapshot"
        );
        for _ in 0..crate::notify_need::WARM_WINDOW + 10 {
            bus.row("/f/f1/pub/n-a/s-z/filler", "v=1 t=1");
        }
        let state = base.join("state");
        let run = || need_once(&sock, &state, Sel::Need(None), Since::Start);
        let none: Vec<String> = Vec::new();
        assert_eq!(run(), none, "PRECONDITION: nothing has risen");
        s.question();
        let page = bus.publish(NEED_FACE, s.bodies());
        assert!(
            page.iter().all(|(_, b)| !b.contains(" op=snap ")),
            "PRECONDITION: no snapshot seeds the session past the window"
        );
        let asked = [format!("{} question", offset_of(&page, &["k=question"]))];
        assert_eq!(
            run(),
            asked,
            "the question rises where the partial fold holds its record: it pages once"
        );
        s.quiet();
        bus.publish(NEED_FACE, s.bodies());
        assert_eq!(run(), asked, "a second run pages nothing more");
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_standing_failure_pages_once_around_a_question_that_outranks_it() {
        let dir = scratch("need-around");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let failure = || Sel::Need(Some(vec!["failure"]));
        let stepwise = base.join("stepwise");
        let step = || need_once(&sock, &stepwise, failure(), Since::Start);
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let failed = offset_of(
            &bus.publish(NEED_FACE, s.bodies()),
            &[" op=close ", "out=fail"],
        );
        let once = [format!("{failed} failure")];
        assert_eq!(step(), once, "the failure rises");
        let q = s.question();
        let asked = offset_of(&bus.publish(NEED_FACE, s.bodies()), &["k=question"]);
        assert_eq!(step(), once, "a question outranks it, unselected");
        s.answer(q);
        let answered = bus.publish(NEED_FACE, s.bodies());
        assert!(
            answered.iter().any(|(_, b)| b.contains("out=answered")),
            "PRECONDITION: the answer reaches the bus: {answered:?}"
        );
        assert_eq!(
            step(),
            once,
            "the same failure stands again on top, and pages nothing more"
        );
        let whole = |tag: &str, on: Sel| need_once(&sock, &base.join(tag), on, Since::Start);
        assert_eq!(whole("failure", failure()), once, "read whole, once");
        let both = [format!("{failed} failure"), format!("{asked} question")];
        assert_eq!(
            whole("any", Sel::Need(None)),
            both,
            "a bare need pages each need the session raised, once"
        );
        assert_eq!(
            whole("two", Sel::Need(Some(vec!["failure", "question"]))),
            both
        );
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A NODE'S WILL KEEPS ITS SESSIONS' NEEDS (session log Phase 6 fix unit G2): a failure
    /// stands, the node's will (`away` or the legacy `gone`) comes, the node and the session
    /// reattach, and a handoff mark folds the same failure. Read in one run with the will, a will
    /// that dropped the failure's pair would page it again; read apart, the will pages nothing,
    /// and the next run's warm-up finds the failure standing.
    #[cfg(unix)]
    #[test]
    fn a_nodes_will_keeps_its_sessions_needs_and_a_reattach_pages_nothing_again() {
        for (will, apart) in [
            ("away", false),
            ("gone", false),
            ("away", true),
            ("gone", true),
        ] {
            let dir = scratch(&format!("need-will-{will}-{apart}"));
            let base = PathBuf::from(&dir);
            std::fs::create_dir_all(&base).expect("scratch");
            let broker = crate::test_broker(base.join("b.log")).unwrap();
            let mut bus = NeedBus::open(&base, &broker);
            let sock = bus.sock.clone();
            let state = base.join("state");
            let run = || need_once(&sock, &state, Sel::Need(None), Since::Start);
            let (node, session) = ("/f/f1/pub/n-a/node/presence", "/f/f1/pub/n-a/s-x/presence");
            bus.row(node, "v=1 t=1 state=live inc=1");
            bus.row(session, "v=1 t=1 inc=1 state=live");
            let mut s = Needs::open();
            let cmd = s.cmd_open();
            s.cmd_close(cmd, aterm_slog::RunOut::Fail);
            let failed = offset_of(
                &bus.publish(NEED_FACE, s.bodies()),
                &[" op=close ", "out=fail"],
            );
            let once = [format!("{failed} failure")];
            assert_eq!(run(), once, "{will}: the failure rises");
            bus.row(
                node,
                &format!("v=1 t=2 state={will} inc=1 fabric=disconnected"),
            );
            if apart {
                assert_eq!(run(), once, "{will}: the will pages nothing");
            }
            bus.row(node, "v=1 t=3 state=live inc=2");
            bus.row(session, "v=1 t=3 inc=2 state=live");
            s.quiet();
            bus.publish(NEED_FACE, s.bodies());
            assert_eq!(
                run(),
                once,
                "{will}, read {}: a reattach that restores the same failure pages it not again",
                if apart {
                    "apart"
                } else {
                    "in one run with the will"
                }
            );
            drop(bus.conn);
            bus.server.shutdown();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_old_stores_word_stands_as_the_folds_pair_and_a_new_rise_still_pages() {
        let dir = scratch("need-old-store");
        let (mut n, fired) = appending(&dir, 100, "");
        let sel = Sel::Need(None);
        n.cfg.on = vec![sel.clone()];
        let key = sel.key();
        n.standing = vec![(NEED_FACE.to_string(), "failure".to_string())];
        n.log_store
            .set_standing(&n.standing)
            .expect("an old store's one word");
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let mut off = 0;
        for b in s.bodies() {
            off += 1;
            n.need.warm(off, NEED_FACE, b.as_bytes());
        }
        n.seed_standing(&key, off + 1);
        let mut feed = |n: &mut Notifier, bodies: Vec<String>| -> Vec<(u64, String)> {
            bodies
                .into_iter()
                .map(|b| {
                    off += 1;
                    n.on_record(&sel, off, NEED_FACE, b.as_bytes())
                        .expect("on_record");
                    (off, b)
                })
                .collect()
        };
        s.quiet();
        feed(&mut n, s.bodies());
        assert!(fired_offsets(&fired).is_empty(), "the old word stood");
        let pairs = n.pairs_of(NEED_FACE);
        assert!(
            matches!(
                pairs.as_slice(),
                [Pair {
                    word: "failure",
                    raised: crate::notify_need::Raised::At { .. }
                }]
            ),
            "the fold's pair took its place: {pairs:?}"
        );
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Ok);
        feed(&mut n, s.bodies());
        assert_eq!(
            standing_words(&n, NEED_FACE),
            Vec::<&str>::new(),
            "it cleared"
        );
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let again = offset_of(&feed(&mut n, s.bodies()), &[" op=close ", "out=fail"]);
        assert_eq!(fired_offsets(&fired), [again], "a new rise pages");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Feed `bodies` to `n` as `face`'s records under `sel`, from offset `*off + 1` on: each
    /// body with its offset.
    fn feed_need(
        n: &mut Notifier,
        sel: &Sel,
        off: &mut u64,
        face: &str,
        bodies: Vec<String>,
    ) -> Vec<(u64, String)> {
        bodies
            .into_iter()
            .map(|b| {
                *off += 1;
                n.on_record(sel, *off, face, b.as_bytes())
                    .expect("on_record");
                (*off, b)
            })
            .collect()
    }

    /// A PARTIAL FOLD'S BOUND KEEPS WHAT THE FOLD NAMES (session log Phase 6 fix unit G2,
    /// review): the watch joins late (the session's first take, its start and snapshot, never
    /// reaches it), then folds a failure and twenty questions asked and answered under one
    /// turn, with no snapshot between. A partial fold drops no pair, so the answered questions'
    /// pairs pile up past [`PAIRS_KEEP`]: the bound drops those, never the failure the fold
    /// still names, which pages once.
    #[test]
    fn a_partial_folds_bound_keeps_the_needs_it_names_so_a_standing_failure_pages_once() {
        let dir = scratch("need-bound");
        let (mut n, fired) = appending(&dir, 100, "");
        let sel = Sel::Need(None);
        n.cfg.on = vec![sel.clone()];
        let mut off = 0;
        let mut s = Needs::open();
        let withheld = s.bodies();
        assert!(
            withheld.iter().any(|b| b.contains(" op=snap ")),
            "PRECONDITION: the withheld first take holds the start and its snapshot"
        );
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let mut fed = feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
        let mut paged = vec![offset_of(&fed, &[" op=close ", "out=fail"])];
        let turn = s.turn();
        fed.extend(feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies()));
        for _ in 0..20 {
            let q = s.ask(turn);
            let asked = feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
            paged.push(offset_of(&asked, &["k=question"]));
            s.answer(q);
            fed.extend(asked);
            fed.extend(feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies()));
        }
        assert!(
            fed.iter().all(|(_, b)| !b.contains(" op=snap ")),
            "PRECONDITION: no snapshot seeds the fold"
        );
        assert!(
            paged.len() > PAIRS_KEEP + 1,
            "PRECONDITION: more questions page than the bound keeps"
        );
        assert_eq!(
            fired_offsets(&fired),
            paged,
            "the failure pages once, and each question once"
        );
        assert!(
            standing_words(&n, NEED_FACE).contains(&"failure"),
            "the failure still stands"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE STORE'S BOUND KEEPS A FACE THE WATCH HOLDS (G2 review, the same class): a failure
    /// stands on a face the watch folds, the oldest subject written, when the store takes one
    /// subject past [`STANDING_KEEP`]. The bound drops the oldest subject the watch holds no fold
    /// of, so the face's next record pages nothing again.
    #[test]
    fn the_standing_stores_bound_drops_no_face_the_watch_holds() {
        let dir = scratch("need-keep");
        let (mut n, fired) = appending(&dir, 100, "");
        let sel = Sel::Need(None);
        n.cfg.on = vec![sel.clone()];
        let mut off = 0;
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let page = feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
        let rose = offset_of(&page, &[" op=close ", "out=fail"]);
        assert_eq!(
            n.standing
                .iter()
                .map(|(s, _)| s.as_str())
                .collect::<Vec<_>>(),
            [NEED_FACE],
            "PRECONDITION: the face is the oldest subject written"
        );
        let other = |i: usize| format!("/f/f1/pub/n-b/s-{i}/presence");
        n.standing
            .extend((1..STANDING_KEEP).map(|i| (other(i), "x".to_string())));
        n.set_standing(&sel.key(), off, &other(STANDING_KEEP), Some("x"));
        s.quiet();
        feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
        assert_eq!(
            fired_offsets(&fired),
            [rose],
            "the failure the face's fold still names pages not again"
        );
        assert_eq!(n.standing.len(), STANDING_KEEP);
        assert!(
            !n.standing.iter().any(|(s, _)| *s == other(1)),
            "the oldest subject the watch holds no fold of went"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AN OWED PAGE PAGES AGAIN AFTER A RESTART (G2 review): a need whose command failed, or
    /// that `--rate` dropped, is owed, so the next `--once` run's warm-up does not stand it and
    /// the session's next record pages it, once, as it would in one run.
    #[cfg(unix)]
    #[test]
    fn a_need_whose_page_failed_or_was_dropped_pages_at_its_next_record_after_a_restart() {
        let dir = scratch("need-owed");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let mut x = Needs::open();
        let cmd = x.cmd_open();
        x.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let failed = offset_of(
            &bus.publish(NEED_FACE, x.bodies()),
            &[" op=close ", "out=fail"],
        );

        let failing = base.join("failing");
        let (mut c, fired) = need_cfg(&sock, &failing, Sel::Need(None), Since::Start);
        c.exec = format!("{}; exit 1", c.exec);
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        let tried = fired_lines(&fired);
        assert!(
            tried.first() == Some(&format!("{failed} failure"))
                && tried.iter().all(|l| l.ends_with(" failure")),
            "PRECONDITION: the failure's command ran and failed: {tried:?}"
        );
        x.quiet();
        let quiet = bus.publish(NEED_FACE, x.bodies());
        let next = quiet.last().expect("the quiet mark").0;
        let mut once = tried.clone();
        once.push(format!("{next} failure"));
        let run = |state: &Path| need_once(&sock, state, Sel::Need(None), Since::Start);
        assert_eq!(
            run(&failing),
            once,
            "a restart pages the failed need at its session's next record"
        );
        assert_eq!(run(&failing), once, "once: it stands now");

        let y_face = "/f/f1/pub/n-a/s-y/ev";
        let mut y = Needs::open();
        let cmd = y.cmd_open();
        y.cmd_close(cmd, aterm_slog::RunOut::Fail);
        bus.publish(y_face, y.bodies());
        let dropping = base.join("dropping");
        let (mut c, fired) = need_cfg(&sock, &dropping, Sel::Need(None), Since::Start);
        c.limit = 1;
        c.window_ms = 3_600_000;
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        let budget = [format!("{failed} failure")];
        assert_eq!(
            fired_lines(&fired),
            budget,
            "PRECONDITION: s-x's failure spends the budget and --rate drops s-y's"
        );
        y.quiet();
        let quiet = bus.publish(y_face, y.bodies());
        let next = quiet.last().expect("the quiet mark").0;
        let both = [format!("{failed} failure"), format!("{next} failure")];
        assert_eq!(
            run(&dropping),
            both,
            "a restart with room pages the dropped need at its session's next record"
        );
        assert_eq!(run(&dropping), both, "once");
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn need_selectors_take_the_needs_a_fold_raises_and_refuse_any_other() {
        assert_eq!(Sel::parse("need"), Ok(Sel::Need(None)));
        let words: Vec<&str> = aterm_slog::observed_need_words().collect();
        let families: Vec<&str> = need_families().collect();
        for w in &words {
            assert_eq!(
                Sel::parse(&format!("need:{w}")),
                Ok(Sel::Need(Some(vec![*w]))),
                "{w}"
            );
        }
        for f in &families {
            let sel = Sel::parse(&format!("need:{f}")).expect("a family");
            assert!(
                words.iter().any(|w| sel.selects(w)),
                "{f}: a family names some word"
            );
            for w in &words {
                assert_eq!(
                    sel.selects(w),
                    w.split_once(':').is_some_and(|(family, _)| family == *f),
                    "need:{f} selects {w} exactly when it is of the family"
                );
            }
        }
        assert!(
            Sel::parse("need:approval")
                .expect("approval is a family")
                .selects("approval:bash"),
            "need:approval takes an approval:bash rise"
        );
        let never: Vec<&str> = aterm_slog::vocab::NEED_WORDS
            .iter()
            .copied()
            .filter(|w| !words.contains(w) && !families.contains(w))
            .collect();
        for named in ["ask", "review", "decision", "help", "memory", "stalled"] {
            assert!(
                never.contains(&named),
                "PRECONDITION: the log spells {named}, which no fold raises as observed"
            );
        }
        for bad in never.iter().map(|w| format!("need:{w}")).chain(
            [
                "need:nope",
                "need:",
                "need:FAILURE",
                "need: failure",
                "need:approval:",
                "needs",
            ]
            .map(str::to_string),
        ) {
            let e = Sel::parse(&bad).expect_err(&bad);
            if let Some(word) = bad.strip_prefix("need:") {
                assert!(e.starts_with(&format!("--on need:{word}: ")), "{e}");
                for w in words.iter().chain(&families) {
                    assert!(
                        e.split(['(', ' ', ')', ';']).any(|t| t == *w),
                        "{bad}: the error names {w}: {e}"
                    );
                }
            }
        }
        let args = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
        let base = "--fleet f1 --broker /tmp/b.sock --state /tmp/x --exec true";
        let on = |rest: &str| parse(&args(&format!("{base} {rest}"))).expect("parse").on;
        assert_eq!(
            on("--on need:failure,attention,need:question,need:failure"),
            [Sel::Need(Some(vec!["failure", "question"])), Sel::Attention],
            "one need selector, its words in first-seen order"
        );
        assert_eq!(
            on("--on need:failure --on need"),
            [Sel::Need(None)],
            "a bare need takes every word"
        );
        assert_eq!(on("--on need,need:failure"), [Sel::Need(None)]);
        assert_eq!(Sel::Need(None).key(), "need");
        assert_eq!(Sel::Need(None).filter("f1"), "/f/f1/pub/*/*/*");
    }

    #[test]
    fn the_help_names_exactly_the_needs_a_fold_raises_and_their_families() {
        let help = usage();
        let (words, families) = (fill(need_words()), fill(need_families()));
        assert_eq!(
            words.split_whitespace().collect::<Vec<_>>(),
            aterm_slog::observed_need_words().collect::<Vec<_>>(),
            "the list is the needs a fold raises, in the log's order"
        );
        let spelled: Vec<&str> = aterm_slog::observed_need_words()
            .filter_map(|w| w.split_once(':').map(|(family, _)| family))
            .collect();
        let listed: Vec<&str> = families.split_whitespace().collect();
        assert!(
            !listed.is_empty() && listed.iter().all(|f| spelled.contains(f)),
            "each family is one the words spell: {listed:?}"
        );
        assert!(
            spelled.iter().all(|f| listed.contains(f)),
            "and every family they spell is listed: {listed:?}"
        );
        for list in [&words, &families] {
            assert!(
                help.contains(&format!("\n{list}\n")),
                "the help prints that list whole: {help}"
            );
        }
        assert!(help.contains("need[:<word>]"), "{help}");
    }

    #[test]
    fn the_rate_flag_refuses_a_budget_that_would_drop_everything() {
        assert_eq!(parse_rate("10/1m"), Ok((10, 60_000)));
        assert_eq!(parse_rate("1/30s"), Ok((1, 30_000)));
        assert_eq!(parse_rate("40/2h"), Ok((40, 7_200_000)));
        assert!(
            parse_rate("0/1m").is_err(),
            "a zero limit is refused by name"
        );
        assert!(parse_rate("10/0m").is_err(), "a zero window is refused");
        for bad in ["10", "10/1d", "x/1m", "10/m", ""] {
            assert!(parse_rate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rate_windows_refuse_overflow_and_non_ascii_units() {
        for (unit, scale) in [('s', 1_000), ('m', 60_000), ('h', 3_600_000)] {
            let largest = u64::MAX / scale;
            assert_eq!(
                parse_rate(&format!("1/{largest}{unit}")),
                Ok((1, largest * scale))
            );
            assert!(parse_rate(&format!("1/{}{unit}", largest + 1)).is_err());
        }
        for bad in ["1/1é", "1/日", "1/1\u{1f600}", "1/"] {
            assert!(parse_rate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn argv_requires_a_watch_and_a_command() {
        let args = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
        let base = "--fleet f1 --broker /tmp/b.sock --state /tmp/x --exec true";
        let cfg = parse(&args(&format!("{base} --on attention,halt,attention"))).expect("parse");
        assert_eq!(
            cfg.on,
            vec![Sel::Attention, Sel::Halt],
            "a repeat is folded"
        );
        assert_eq!(
            (cfg.limit, cfg.window_ms),
            (DEFAULT_LIMIT, DEFAULT_WINDOW_MS)
        );
        assert_eq!(
            cfg.since,
            Since::Head,
            "a fresh install does not page history"
        );
        assert!(!cfg.once);
        assert!(
            parse(&args("--fleet f1 --broker b --exec true")).is_err(),
            "no --on"
        );
        assert!(
            parse(&args("--fleet f1 --broker b --on halt")).is_err(),
            "no --exec"
        );
        assert!(
            parse(&args(&format!("{base} --on halt --rate"))).is_err(),
            "a flag needs a value"
        );
    }

    #[test]
    fn a_carry_refuses_incomplete_cursor_and_verdict_reads() {
        let dir = aterm_tempfile::TempDir::new().expect("owned notifier");
        let from = Store {
            root: dir.path().join("from"),
        };
        std::fs::create_dir_all(from.root.join("cur")).unwrap();
        from.set_cursor("attention", 2).unwrap();
        from.set_journal(
            &["1 fired rc=0".into(), "7 fired rc=0".into()],
            Flush::Ordered,
        )
        .unwrap();
        from.set_standing(&[("/f/f/pub/n-a/s-a/presence".into(), "help".into())])
            .unwrap();
        let logs = dir.path().join("logs");
        let ordinary = logs.join("ordinary");
        from.carry(&from, &ordinary, 6).unwrap();
        let carried = Store { root: ordinary };
        assert_eq!(carried.cursor("attention"), Some(2));
        assert_eq!(carried.journal(), ["1 fired rc=0"]);
        assert_eq!(carried.standing(), from.standing());
        let empty = Store {
            root: dir.path().join("empty"),
        };
        std::fs::create_dir_all(&empty.root).unwrap();
        empty.carry(&empty, &logs.join("empty"), 6).unwrap();

        let mut refused = Vec::new();
        for (n, bytes) in [b"not an offset".as_slice(), b"", b"\xff"]
            .into_iter()
            .enumerate()
        {
            std::fs::write(from.root.join("cur/attention"), bytes).unwrap();
            let to = logs.join(format!("bad-cursor-{n}"));
            let result = from.carry(&from, &to, 6);
            refused.push(
                result.is_err()
                    && !to.exists()
                    && std::fs::read(from.root.join("cur/attention")).is_ok_and(|got| got == bytes),
            );
        }
        from.set_cursor("attention", 2).unwrap();
        for name in ["journal", "standing"] {
            let path = from.root.join(name);
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            let to = logs.join(format!("unreadable-{name}"));
            let result = from.carry(&from, &to, 6);
            refused.push(result.is_err() && !to.exists() && path.is_dir());
            std::fs::remove_dir(&path).unwrap();
            from.write(name, "", Flush::Ordered).unwrap();
        }
        assert!(
            refused.into_iter().all(|held| held),
            "CARRY_PUBLISHED_INCOMPLETE_EVIDENCE"
        );
    }

    #[test]
    fn a_carry_retires_only_its_exclusively_owned_stage() {
        let dir = aterm_tempfile::TempDir::new().expect("owned carry parent");
        let from = Store {
            root: dir.path().join("from"),
        };
        std::fs::create_dir_all(from.root.join("cur")).unwrap();
        from.set_cursor("attention", 2).unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir(&logs).unwrap();
        let foreign = logs.join(format!(".carry-{}-0", std::process::id()));
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("owner"), b"standing owner").unwrap();
        let target = logs.join("landed");
        from.carry(&from, &target, 6).unwrap();
        assert_eq!(Store { root: target }.cursor("attention"), Some(2));
        let winner = logs.join("winner");
        std::fs::create_dir_all(winner.join("cur")).unwrap();
        std::fs::write(winner.join("cur/attention"), b"4").unwrap();
        from.carry(&from, &winner, 6).unwrap();
        assert_eq!(std::fs::read(winner.join("cur/attention")).unwrap(), b"4");
        let blocked = logs.join("blocked");
        std::fs::write(&blocked, b"do not replace").unwrap();
        assert!(from.carry(&from, &blocked, 6).is_err());
        assert_eq!(std::fs::read(&blocked).unwrap(), b"do not replace");
        assert_eq!(
            std::fs::read(foreign.join("owner")).unwrap(),
            b"standing owner"
        );
        let stages: Vec<_> = std::fs::read_dir(&logs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".carry-")
            })
            .collect();
        assert_eq!(stages, [foreign], "only the pre-existing owner remains");
    }
}
