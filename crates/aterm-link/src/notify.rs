// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link notify`: run a command when the fabric needs a human ([`USAGE`] lists the flags).

use aterm_uds::spawnfd::Stdio;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::bridge::ACK_DEADLINE;
use crate::notify_need::{NeedWatch, Step};
use crate::subject::{is_fleet, is_principal};
use crate::transport::{self, Backoff, Closer, Conn};
use aterm_slog::vocab::NEED_WORDS;

const PAGE: u32 = crate::transport::POST_PAGE;

/// Verdicts kept in `journal` (must exceed [`PAGE`]).
const JOURNAL_KEEP: usize = 1024;

/// Number of non-root log stores (`logs/<identity>/`) retained by [`Store::prune_logs`].
const LOGS_KEEP: usize = 4;

const STANDING_KEEP: usize = JOURNAL_KEEP;

const TEXT_MAX: usize = 256;

const EXEC_POLL: Duration = Duration::from_millis(10);
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 10;
const DEFAULT_WINDOW_MS: u64 = 60_000;

const HOLD_MAX: usize = 4 << 20;

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

fn notice_handoff() -> (mpsc::SyncSender<Notice>, mpsc::Receiver<Notice>) {
    mpsc::sync_channel(0)
}

/// One `--on` selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    Attention,
    Ask(String),
    Halt,
    /// `need` (`None`: any word) or `need:<word>`,…: a session's need, as `aterm fabric brief`
    /// folds it from the session log's bus mirror ([`crate::notify_need`]), rising to one of
    /// these words, each one of [`NEED_WORDS`]. Every `need` word of one `--on` list is one
    /// selector ([`Sel::merge`]): one reader folds the faces once.
    Need(Option<Vec<&'static str>>),
}

impl Sel {
    /// Parse one selector word.
    /// # Errors
    /// A word that is not `attention`, `halt`, `ask:<principal>`, `need` or `need:<word>` with
    /// `<word>` one of the session log's need words ([`NEED_WORDS`], which the error names).
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "attention" => Ok(Sel::Attention),
            "halt" => Ok(Sel::Halt),
            "need" => Ok(Sel::Need(None)),
            other => {
                if let Some(w) = other.strip_prefix("need:") {
                    return aterm_slog::vocab::lookup(NEED_WORDS, w)
                        .map(|w| Sel::Need(Some(vec![w])))
                        .ok_or_else(|| {
                            format!(
                                "--on need:{w}: not one of the session log's need words ({})",
                                NEED_WORDS.join(" ")
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

    /// Whether a `need` selector pages a need that rose to `word`.
    fn selects(&self, word: &str) -> bool {
        match self {
            Sel::Need(None) => true,
            Sel::Need(Some(words)) => words.contains(&word),
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

    fn write(&self, name: &str, value: &str) -> io::Result<()> {
        let target = self.root.join(name);
        let tmp = target.with_extension("tmp");
        if let Some(parent) = tmp.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(value.as_bytes())?;
            f.write_all(b"\n")?;
            if crate::state::flushes_media() {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "aterm-link's flushes are the Ordered-tier work's to classify; the full flush is kept until then"
                )]
                f.sync_all()?;
            }
        }
        std::fs::rename(&tmp, &target)
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
                self.write("log", &name)?;
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
                        built.write(&format!("cur/{key}"), &off.min(fork).to_string())?;
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
                built.set_journal(&below)?;
            }
            if let Some(standing) = from.carry_text("standing")? {
                if !standing.is_empty() {
                    built.write("standing", &standing)?;
                }
            }
            crate::state::sync_dir(&built.root.join("cur"))?;
            crate::state::sync_dir(&built.root)
        })();
        carried?;
        match std::fs::rename(scratch.path(), to) {
            Ok(()) => {
                let _ = scratch.keep();
                crate::state::sync_dir(&logs)
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
        self.write("logs/used", &keep.join("\n"))?;
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

    fn set_cursor(&self, key: &str, off: u64) -> io::Result<()> {
        if self.cursor(key).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write(&format!("cur/{key}"), &off.to_string())
    }

    fn journal(&self) -> Vec<String> {
        self.read("journal")
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn set_journal(&self, lines: &[String]) -> io::Result<()> {
        self.write("journal", &lines.join("\n"))
    }

    fn stamps(&self) -> Vec<u64> {
        self.read("rate")
            .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
            .unwrap_or_default()
    }

    fn set_stamps(&self, stamps: &[u64]) -> io::Result<()> {
        let joined: Vec<String> = stamps.iter().map(u64::to_string).collect();
        self.write("rate", &joined.join(","))
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
        self.write("standing", &lines.join("\n"))
    }

    fn suppressed(&self) -> u64 {
        self.read("suppressed")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn set_suppressed(&self, n: u64) -> io::Result<()> {
        self.write("suppressed", &n.to_string())
    }
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
}

impl Notifier {
    fn new(cfg: Config) -> io::Result<Self> {
        let store = Store::open(&cfg.state_dir)?;
        let fault = Fault::from_env(&store);
        let n = cfg.on.len();
        let need = NeedWatch::new(&cfg.fleet);
        Ok(Self {
            journal: store.journal(),
            standing: store.standing(),
            stamps: store.stamps(),
            suppressed: store.suppressed(),
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
        })
    }

    /// One broker connection with every capability attached ([`ACK_DEADLINE`]).
    fn connect(&self) -> io::Result<Conn> {
        let (mut conn, _closer) = transport::connect(&self.cfg.broker, ACK_DEADLINE)?;
        transport::attach_cap_files(&mut conn, &self.cfg.cap_files)?;
        Ok(conn)
    }

    /// Read and write cursors and verdicts as the log `log`'s from here on ([`Store::for_log`]).
    fn scope_to(&mut self, read: &ReadLog) -> io::Result<()> {
        let log = read.id;
        if log == self.log_now {
            return Ok(());
        }
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

    /// Make `token` the escalation standing on `subject` ([`STANDING_KEEP`]).
    fn set_standing(&mut self, subject: &str, token: Option<&str>) -> io::Result<()> {
        if self.standing_of(subject) == token {
            return Ok(());
        }
        self.standing.retain(|(s, _)| s != subject);
        if let Some(token) = token {
            self.standing.push((subject.to_string(), token.to_string()));
        }
        while self.standing.len() > STANDING_KEEP {
            self.standing.remove(0);
        }
        self.log_store.set_standing(&self.standing)
    }

    /// Write one verdict, replacing the offset's old one, keeping the newest [`JOURNAL_KEEP`].
    fn record(&mut self, off: u64, line: String) -> io::Result<()> {
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
        self.log_store.set_journal(&self.journal)
    }

    fn has_room(&mut self, now: u64) -> bool {
        let window = self.cfg.window_ms;
        self.stamps.retain(|t| now.saturating_sub(*t) < window);
        u64::from(self.cfg.limit) > self.stamps.len() as u64
    }

    fn charge(&mut self, now: u64) -> io::Result<()> {
        self.stamps.push(now);
        let limit = self.cfg.limit as usize;
        while self.stamps.len() > limit {
            self.stamps.remove(0);
        }
        self.store.set_stamps(&self.stamps)
    }

    /// What a `need` selector makes of one record: the standing need cleared where the fold, or
    /// a presence row saying the session is gone without its close, names none or another word,
    /// and the need word (the standing token) and the hit where the fold names a word `sel`
    /// selects. A partial fold ([`Step::Partial`]) names nothing and clears nothing.
    fn need_hit(
        &mut self,
        sel: &Sel,
        off: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<Option<(String, Hit)>> {
        self.need_fresh = false;
        match self.need.step(off, subject, body) {
            Step::Nothing | Step::Partial(_) => Ok(None),
            Step::Gone(faces) => {
                for face in faces {
                    self.set_standing(&face, None)?;
                }
                Ok(None)
            }
            Step::Folded {
                face, need: None, ..
            } => {
                self.set_standing(&face, None)?;
                Ok(None)
            }
            Step::Folded {
                face,
                node,
                sid,
                need: Some((word, detail)),
            } => {
                if self.standing_of(&face).is_some_and(|t| t != word) {
                    self.set_standing(&face, None)?;
                }
                if !sel.selects(word) {
                    return Ok(None);
                }
                let detail = detail.as_deref().map(one_line).unwrap_or_default();
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
                Ok(Some((word.to_string(), hit)))
            }
        }
    }

    fn on_record(&mut self, sel: &Sel, off: u64, subject: &str, body: &[u8]) -> io::Result<()> {
        let (raised, hit) = match sel {
            Sel::Attention => {
                let raised = match escalation(&self.cfg.fleet, off, subject, body) {
                    None => return Ok(()),
                    Some(None) => return self.set_standing(subject, None),
                    Some(Some(token)) => {
                        if self.standing_of(subject).is_some_and(|t| t != token) {
                            self.set_standing(subject, None)?;
                        }
                        Some(token)
                    }
                };
                (raised, sel.hit(&self.cfg.fleet, off, subject, body))
            }
            Sel::Need(_) => match self.need_hit(sel, off, subject, body)? {
                Some((token, hit)) => (Some(token), Some(hit)),
                None => return Ok(()),
            },
            Sel::Ask(_) | Sel::Halt => (None, sel.hit(&self.cfg.fleet, off, subject, body)),
        };
        let Some(hit) = hit else {
            return Ok(());
        };
        let dup = match self.verdict(off) {
            Some("fired" | "dropped") => {
                if let Some(token) = raised.as_deref().filter(|_| self.told(off)) {
                    self.set_standing(subject, Some(token))?;
                }
                self.counts.skipped += 1;
                return Ok(());
            }
            Some(_) => true,
            None if raised.is_some() && self.standing_of(subject) == raised.as_deref() => {
                self.counts.standing += 1;
                return Ok(());
            }
            None => false,
        };
        let now = crate::now_ms();
        if !dup {
            if !self.has_room(now) {
                self.suppressed += 1;
                self.store.set_suppressed(self.suppressed)?;
                self.record(
                    off,
                    format!(
                        "{off} dropped on={} reason=rate limit={}/{}ms suppressed={}",
                        hit.on, self.cfg.limit, self.cfg.window_ms, self.suppressed
                    ),
                )?;
                eprintln!(
                    "aterm-link notify: DROPPED off={off} on={} reason=rate limit={}/{}ms \
                     suppressed={}; the record stays on the bus",
                    hit.on, self.cfg.limit, self.cfg.window_ms, self.suppressed
                );
                self.counts.dropped += 1;
                return Ok(());
            }
            self.charge(now)?;
            self.record(off, format!("{off} firing on={}", hit.on))?;
        }
        let carried = self.suppressed;
        let verdict = self.exec(&hit, dup, carried);
        if self.fault == Fault::KillAfterExec {
            self.fault.fire(&self.store);
        }
        self.record(
            off,
            format!("{off} fired on={} {verdict} dup={}", hit.on, u8::from(dup)),
        )?;
        if carried > 0 && verdict == "rc=0" {
            self.suppressed -= carried;
            self.store.set_suppressed(self.suppressed)?;
        }
        if let Some(token) = raised.as_deref().filter(|_| verdict == "rc=0") {
            self.set_standing(subject, Some(token))?;
        }
        if dup {
            self.counts.dup += 1;
        }
        self.counts.fired += 1;
        Ok(())
    }

    fn exec(&self, hit: &Hit, dup: bool, suppressed: u64) -> String {
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

    fn catch_up(&mut self) -> io::Result<()> {
        let mut conn = self.connect()?;
        let log = log_of(&mut conn, &self.cfg)?;
        self.scope_to(&log)?;
        'selectors: for sel in self.cfg.on.clone() {
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
                        self.log_store.set_cursor(&key, off)?;
                        eprintln!(
                            "aterm-link notify: {} off={off} is not durable yet (the broker \
                             serves ordered, and flushes it in full within its horizon plus a \
                             batch and a full flush): it and what follows it wait for the next \
                             run",
                            sel.label()
                        );
                        continue 'selectors;
                    }
                    self.on_record(&sel, off, &subject, &body)?;
                }
                self.log_store.set_cursor(&key, next)?;
                if next >= head || next <= from {
                    break;
                }
                from = next;
            }
        }
        Ok(())
    }

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
        while let Ok(notice) = rx.recv() {
            self.take(notice)?;
        }
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "every subscription ended",
        ))
    }

    /// Process one [`Notice`] from a follow-mode reader in read order ([`Notice::Durable`]).
    fn take(&mut self, notice: Notice) -> io::Result<()> {
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
                if matches!(sel, Sel::Need(_)) {
                    self.need_fresh = false;
                }
                let log = self.reader_log[idx].clone();
                self.scope_to(&log)?;
                let waits = self.wait_durable
                    && log.ordered
                    && (!self.held[idx].is_empty() || off >= self.durable[idx]);
                if waits {
                    let size = subject.len() + body.len();
                    if self.held_bytes + size <= HOLD_MAX {
                        if self.held[idx].is_empty() {
                            eprintln!(
                                "aterm-link notify: {} off={off} waits until the broker says it \
                                 is durable (it serves ordered: its horizon plus one batch \
                                 and one full flush)",
                                sel.label()
                            );
                        }
                        self.held_bytes += size;
                        self.held[idx].push_back(Held { off, subject, body });
                        return Ok(());
                    }
                    eprintln!(
                        "aterm-link notify: {} holds {} bytes the broker has not said are \
                         durable — past {HOLD_MAX}, so they are paged now, before it does",
                        sel.label(),
                        self.held_bytes + size
                    );
                    self.release(idx, u64::MAX)?;
                }
                self.on_record(&sel, off, &subject, &body)?;
                self.log_store.set_cursor(&sel.key(), off + 1)?;
            }
            Notice::Checkpoint(idx, next) => {
                let Some(key) = self.cfg.on.get(idx).map(Sel::key) else {
                    return Ok(());
                };
                if self.held[idx].is_empty() {
                    let log = self.reader_log[idx].clone();
                    self.scope_to(&log)?;
                    self.log_store.set_cursor(&key, next)?;
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

    fn release(&mut self, idx: usize, durable: u64) -> io::Result<()> {
        let Some(sel) = self.cfg.on.get(idx).cloned() else {
            return Ok(());
        };
        let log = self.reader_log[idx].clone();
        self.scope_to(&log)?;
        while self.held[idx].front().is_some_and(|h| h.off < durable) {
            let Some(h) = self.held[idx].pop_front() else {
                break;
            };
            self.held_bytes -= h.subject.len() + h.body.len();
            self.on_record(&sel, h.off, &h.subject, &h.body)?;
            self.log_store.set_cursor(&sel.key(), h.off + 1)?;
        }
        if self.held[idx].is_empty() {
            if let Some(next) = self.scanned[idx].take() {
                self.log_store.set_cursor(&sel.key(), next)?;
            }
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
}

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
                         - `need`, `need:<word>`: a session's need, as `aterm fabric brief`
                           folds it from the session log on the bus, rising to any word or
                           to <word> (the words are below; it reads `ro:/f/<F>/pub/>`, which
                           node.cap and a person's cap hold, the caps `fabric brief` reads
                           under)
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

Dedups across restarts by offset, and `attention` and `need` by each subject's standing
value (clearing it and raising it again pages again; a need that moves to another word
rose); a crash mid-`--exec` runs it again with `ATERM_NOTIFY_DUP=1`. A `need` folds the
records below its cursor, up to `fabric brief`'s window, before it pages. A session gone
before its close reached the bus (its own row says it exited, or its node's says gone)
has no need; a session's close raises `exited`, which pages once, whether the close or the
exited row reached the bus first. A session whose fold is partial (the brief's `partial`:
no start, lost records, or a snapshot short of its needs) pages nothing and clears nothing
until a snapshot seeds it. On an `ordered` broker a record waits until it is durable;
`--once` leaves one not yet durable for the next run. Prints
`notify: fired= dup= skipped= dropped= suppressed= standing=` on exit. Exits 2 on a usage
error, an unreadable --cap-file, or a broker that refuses the cap files or a selector's
read (no --cap-file grants it); 1 when the state dir cannot open or the run fails.
";

/// `aterm link notify --help`: [`USAGE`], then the need words `need:<word>` takes, read from
/// the session log's own list ([`NEED_WORDS`]).
#[must_use]
pub fn usage() -> String {
    format!(
        "{USAGE}\nNeed words (`need:<word>`, the session log's own list):\n{}\n",
        need_words()
    )
}

/// The need words as [`usage`] lists them: [`NEED_WORDS`], in its order, filled to the help's
/// width.
fn need_words() -> String {
    let words: Vec<String> = NEED_WORDS.iter().map(|w| (*w).to_string()).collect();
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
        n.record(5, "5 firing on=attention".into()).expect("write");
        assert_eq!(n.verdict(5), Some("firing"));
        n.record(5, "5 fired on=attention rc=0 dup=0".into())
            .expect("write");
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
            n.record(off, format!("{off} fired on=attention rc=0 dup=0"))
                .expect("write");
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
            store.set_journal(&["7 fired".to_string()]).unwrap();
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
            store.write("log", &a.to_string()).unwrap();
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
        root.set_journal(&["1 fired rc=0".to_string()]).unwrap();
        let one = store.for_log(Some(g(1)), None).unwrap();
        assert_ne!(one.root, root.root);
        assert_eq!(one.cursor("attention"), None, "no lineage: a first run");
        one.set_cursor("attention", 9).unwrap();
        one.set_journal(&[
            "2 fired rc=0".to_string(),
            "6 fired rc=0".to_string(),
            "junk".into(),
        ])
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

        fn question(&mut self) {
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
            let turn = self
                .log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Turn,
                        aterm_slog::Src::Screen,
                        aterm_slog::Conf::Strong,
                    )
                    .under(run),
                    at,
                )
                .expect("a turn");
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
                .expect("a question");
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

    #[test]
    fn a_gone_session_clears_its_standing_need_and_a_relaunch_does_not() {
        let dir = scratch("need-gone");
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
        let standing = |n: &Notifier| faces.clone().map(|f| n.standing_of(&f).map(str::to_string));
        let both = [Some("failure".to_string()), Some("failure".to_string())];
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
            "a relaunch whose bridge has not republished the session is not its end"
        );
        feed(
            &mut n,
            "/f/f1/pub/n-a/s-x/presence",
            b"v=1 t=2 inc=2 state=exited",
        );
        assert_eq!(
            standing(&n),
            [None, Some("failure".to_string())],
            "an exited session's need clears"
        );
        feed(
            &mut n,
            "/f/f1/pub/n-a/node/presence",
            b"v=1 t=3 state=gone inc=2",
        );
        assert_eq!(
            standing(&n),
            [None, None],
            "so does a gone node's sessions'"
        );
        assert_eq!(fired_offsets(&fired), rises, "and nothing pages for it");
        let _ = std::fs::remove_dir_all(&dir);
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
                    n.standing_of(NEED_FACE),
                    Some("exited"),
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

    #[test]
    fn need_selectors_take_the_logs_own_words_and_refuse_any_other() {
        assert_eq!(Sel::parse("need"), Ok(Sel::Need(None)));
        for w in NEED_WORDS {
            assert_eq!(
                Sel::parse(&format!("need:{w}")),
                Ok(Sel::Need(Some(vec![*w]))),
                "{w}"
            );
        }
        for bad in [
            "need:nope",
            "need:",
            "need:FAILURE",
            "need: failure",
            "need:approval:",
            "needs",
        ] {
            let e = Sel::parse(bad).expect_err(bad);
            if let Some(word) = bad.strip_prefix("need:") {
                assert!(e.starts_with(&format!("--on need:{word}: ")), "{e}");
                for w in NEED_WORDS {
                    assert!(
                        e.split(['(', ' ', ')']).any(|t| t == *w),
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
    fn the_help_names_exactly_the_need_words_the_log_folds() {
        let help = usage();
        let list = need_words();
        assert_eq!(
            list.split_whitespace().collect::<Vec<_>>(),
            NEED_WORDS,
            "the list is the log's need words, in its order"
        );
        assert!(
            help.contains(&format!("\n{list}\n")),
            "the help prints that list whole: {help}"
        );
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
        from.set_journal(&["1 fired rc=0".into(), "7 fired rc=0".into()])
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
            from.write(name, "").unwrap();
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
