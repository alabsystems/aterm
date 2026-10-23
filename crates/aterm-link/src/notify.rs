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
use crate::subject::{is_fleet, is_principal};
use crate::transport::{self, Backoff, Closer, Conn};

/// Records per `Fetch` page while catching up (`POST_PAGE` so `JOURNAL_KEEP` outlives any single
/// page re-scan after a crash).
const PAGE: u32 = crate::transport::POST_PAGE;

/// Verdicts kept in `journal` (must exceed [`PAGE`]).
const JOURNAL_KEEP: usize = 1024;

/// Number of non-root log stores (`logs/<identity>/`) retained by [`Store::prune_logs`].
const LOGS_KEEP: usize = 4;

/// Subjects remembered in `standing`.
const STANDING_KEEP: usize = JOURNAL_KEEP;

/// Cap on free text passed in `$ATERM_NOTIFY_*`.
const TEXT_MAX: usize = 256;

const EXEC_POLL: Duration = Duration::from_millis(10);
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LIMIT: u32 = 10;
const DEFAULT_WINDOW_MS: u64 = 60_000;

/// Maximum bytes of records held while waiting for an ordered broker's durable mark; past this
/// bound, held records are paged immediately with a warning.
const HOLD_MAX: usize = 4 << 20;

#[derive(Debug)]
enum Notice {
    Record(usize, u64, String, Vec<u8>),
    /// Broker scanned through `next` (including unmatched offsets); sent after the page's records.
    Checkpoint(usize, u64),
    /// Reader connected to `ReadLog`; sent before any record of that connection.
    Log(usize, ReadLog),
    /// Broker's durable head (`Event::Mark`), releasing held records below it on an ordered broker.
    Durable(usize, u64),
}

/// Identity, lineage, and served durability (`ordered`) of the log a reader connected to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReadLog {
    id: Option<astream_broker::LogIdentity>,
    lineage: Option<astream_broker::Lineage>,
    ordered: bool,
}

/// A record a follow-mode notifier holds until its broker says it is durable.
#[derive(Debug)]
struct Held {
    off: u64,
    subject: String,
    body: Vec<u8>,
}

/// A zero-capacity handoff from the broker readers to the serial executor: a body can be
/// 16 MiB, and a buffered channel would let a slow command grow memory without limit.
fn notice_handoff() -> (mpsc::SyncSender<Notice>, mpsc::Receiver<Notice>) {
    mpsc::sync_channel(0)
}

/// One `--on` selector: which records are worth waking a human for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    /// A presence row carrying a non-empty `attention=` (§4.1).
    Attention,
    /// An `ask` ADDRESSED TO this principal — a session's `post to=<p> kind=ask`, which its node publishes on `/f/<F>/in/p/<p>/<node>/ask`: how a person is reached (§9.3).
    Ask(String),
    /// A fleet halt coming on (§9.3).
    Halt,
}

impl Sel {
    /// Parse one selector word.
    /// # Errors
    /// A word that is not `attention`, `halt` or `ask:<principal>`.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "attention" => Ok(Sel::Attention),
            "halt" => Ok(Sel::Halt),
            other => match other.strip_prefix("ask:") {
                Some(p) if is_principal(p) => Ok(Sel::Ask(p.to_string())),
                Some(p) => Err(format!("--on ask:{p}: {p} is not a principal")),
                None => Err(format!(
                    "--on {other}: expected attention, halt or ask:<principal>"
                )),
            },
        }
    }

    /// How this selector is written on the command line and in the journal.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Sel::Attention => "attention".to_string(),
            Sel::Ask(p) => format!("ask:{p}"),
            Sel::Halt => "halt".to_string(),
        }
    }

    /// The cursor file name.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Sel::Attention => "attention".to_string(),
            Sel::Ask(p) => format!("ask-to.{p}"),
            Sel::Halt => "halt".to_string(),
        }
    }

    /// The grant that reads [`Sel::filter`], and which cap carries it — what a refused first read names ([`Notifier::preflight`]).
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
        }
    }

    /// The broker filter that carries this selector's records.
    #[must_use]
    pub fn filter(&self, fleet: &str) -> String {
        match self {
            Sel::Attention => format!("/f/{fleet}/pub/*/*/presence"),
            Sel::Ask(p) => format!("/f/{fleet}/in/p/{p}/*/ask"),
            Sel::Halt => format!("/f/{fleet}/fleet/*/halt"),
        }
    }

    /// The reader's own check, behind the grant's filter. It parses by position from the left,
    /// as `subject::parse_in` does: `>` is one or more segments, and a right-anchored reading of
    /// an over-long subject is the forgery a peer would reach for.
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
                })
            }
            Sel::Ask(p) => {
                // `/f/<F>/in/p/<p>/<src>/ask`, by position from the left: the
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
                })
            }
        }
    }
}

/// What a presence row says of its subject's escalation, for `attention`'s
/// standing set: `None` for a row this reader does not accept (the wall
/// [`Sel::hit`] stands behind), `Some(None)` for a row that raises none — a
/// clear — and `Some(Some(token))` for the raw `attention=` token it raises.
fn escalation(fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Option<String>> {
    let row = crate::glance::Row::parse(fleet, offset, subject, body)?;
    let raw = row.field("attention");
    let text = crate::pct::decode(raw);
    Some((!text.is_empty() && text != crate::glance::ABSENT).then(|| raw.to_string()))
}

/// One record worth waking a human for, reduced to the facts the command gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The selector that matched, as written on the command line.
    pub on: String,
    /// The record's offset — the identity everything here dedups on.
    pub offset: u64,
    /// The full subject, verbatim.
    pub subject: String,
    /// The node the record is about, when the subject names one (for an `ask`, the asking node).
    pub node: String,
    /// The principal it is about (a presence row's owner, the person an ask is addressed to, the human who halted).
    pub who: String,
    /// The asking session, for an `ask`: with [`Hit::node`], the `to=@<sid>@<node>` an answer goes to.
    pub sid: String,
    /// The free text, flattened and capped.
    pub text: String,
}

/// The subject as an environment value, flattened and capped like the text so [`TEXT_MAX`]
/// bounds it whatever the parse admitted.
fn subject_env(subject: &str) -> String {
    one_line(subject)
}

/// Flatten untrusted text to one line of printable ASCII, capped at [`TEXT_MAX`]: the command
/// may echo it at a terminal.
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
    /// The log head, so a new notifier does not page through the fleet's history.
    Head,
    /// Offset zero — the whole retained log.
    Start,
    /// A named offset.
    At(u64),
}

/// `<state>/notify/`: durable cursors, journal, standing tokens, and rate-limit state.
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

    /// Temp, `fsync`, `rename`: a truncated verdict would read as "never fired" and fire again.
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

    /// Return the [`Store`] for `log` (`self` for the root log or a pre-identity broker;
    /// `logs/<identity>/` for any other, carrying over from a predecessor generation when
    /// `lineage` links them).
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

    /// Find the newest stored generation of the same log that `log` continues under `lineage`,
    /// together with the fork offset where they agree.
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

    /// Carry admits absence only when the name is absent. A read failure or
    /// damaged cursor must not publish a successor with missing state.
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

    /// Carry `from`'s store into `to` for a generation that agrees below `fork`: clamp each
    /// `cur/<key>` to `min(cur, fork)`, keep only `journal` verdicts below `fork`, and copy
    /// `standing` intact.
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

    /// Keep only the newest [`LOGS_KEEP`] stores under `logs/<identity>/` (tracked in `logs/used`).
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
                // Not followed if it is a link: the link itself goes.
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    fn cursor(&self, key: &str) -> Option<u64> {
        self.read(&format!("cur/{key}"))
            .and_then(|s| s.trim().parse().ok())
    }

    /// Monotone: a cursor that walked backwards would re-offer offsets the journal has
    /// already forgotten.
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

    /// `<subject> <attention token>` per line, the oldest first. Both are one
    /// word by construction ([`escalation`]); a line that is not two words is
    /// skipped, which can only cost a re-page.
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

/// Test-only fault injection, armed by `$ATERM_LINK_NOTIFY_FAULT` through
/// `aterm_types::dev_seam!`, so a release build compiles it to `None`; the gate matters because
/// aterm's `fabric_launch::filter_child_env` passes the variable on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// Die after the command has run and before the journal says it did.
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

    /// Die by `SIGKILL`, running no destructors.
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
    /// `attention` rows that republished the escalation a human was already told of on that subject: no command, no journal line, no budget.
    pub standing: u64,
}

struct Notifier {
    cfg: Config,
    store: Store,
    /// The store of the log this notifier is reading NOW ([`Store::for_log`]):
    /// where its cursors and `journal` are read and written.
    log_store: Store,
    /// That log's identity, `None` for a broker that predates one.
    log_now: Option<astream_broker::LogIdentity>,
    /// The log each follow-mode reader's connection last reached
    /// ([`Notice::Log`]), by selector index.
    reader_log: Vec<ReadLog>,
    journal: Vec<String>,
    /// The log in hand's `standing` ([`Store::standing`]): per presence
    /// subject, the `attention=` token a human was last told of.
    standing: Vec<(String, String)>,
    stamps: Vec<u64>,
    suppressed: u64,
    fault: Fault,
    counts: Counts,
    /// Whether a page waits for its record to be durable on a broker that
    /// serves ordered (the module doc).
    wait_durable: bool,
    /// By selector index, the records held for durability, in read order
    /// ([`Notifier::take`]).
    held: Vec<std::collections::VecDeque<Held>>,
    /// The bytes [`Self::held`] holds, against [`HOLD_MAX`].
    held_bytes: usize,
    /// By selector index, the highest page checkpoint not yet written because
    /// a held record lies below it: the cursor it moves to once the last held
    /// record is paged.
    scanned: Vec<Option<u64>>,
    /// By selector index, the durable head the reader's current connection
    /// last told ([`Notice::Durable`]).
    durable: Vec<u64>,
}

impl Notifier {
    fn new(cfg: Config) -> io::Result<Self> {
        let store = Store::open(&cfg.state_dir)?;
        let fault = Fault::from_env(&store);
        let n = cfg.on.len();
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
        })
    }

    /// One broker connection with every capability attached, every exchange bounded by
    /// [`ACK_DEADLINE`] so a wedged broker ends `--once` with an error.
    fn connect(&self) -> io::Result<Conn> {
        let (mut conn, _closer) = transport::connect(&self.cfg.broker, ACK_DEADLINE)?;
        transport::attach_cap_files(&mut conn, &self.cfg.cap_files)?;
        Ok(conn)
    }

    /// Read and write cursors and verdicts as the log `log`'s from here on
    /// ([`Store::for_log`]), loading its journal when that is another log
    /// than the one in hand.
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

    /// Check once before starting that `--cap-file` paths read cleanly and no permanent broker
    /// refusal rejects the attached capabilities or selector filters.
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

    /// The verdict the journal holds for an offset. Keyed by offset alone: a record wakes a human
    /// once however many selectors name it.
    fn verdict(&self, off: u64) -> Option<&str> {
        self.journal.iter().find_map(|line| {
            let mut it = line.split_whitespace();
            let at: u64 = it.next()?.parse().ok()?;
            (at == off).then(|| it.next()).flatten()
        })
    }

    /// Whether the journal's verdict for `off` says a human was TOLD: a
    /// `fired` line whose command answered `rc=0`, the module doc's rule.
    fn told(&self, off: u64) -> bool {
        self.journal.iter().any(|line| {
            let mut it = line.split_whitespace();
            it.next().and_then(|t| t.parse::<u64>().ok()) == Some(off)
                && it.next() == Some("fired")
                && it.any(|t| t == "rc=0")
        })
    }

    /// The `attention=` token a human was last told of on `subject`.
    fn standing_of(&self, subject: &str) -> Option<&str> {
        self.standing
            .iter()
            .find(|(s, _)| s == subject)
            .map(|(_, t)| t.as_str())
    }

    /// Make `token` the escalation standing on `subject` (`None`: nothing
    /// stands there), durably, keeping the newest [`STANDING_KEEP`] subjects.
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

    /// Whether the budget has room at `now`, forgetting stamps outside the window. Stamps a
    /// backwards clock left in the future count as inside, erring towards suppressing.
    fn has_room(&mut self, now: u64) -> bool {
        let window = self.cfg.window_ms;
        self.stamps.retain(|t| now.saturating_sub(*t) < window);
        u64::from(self.cfg.limit) > self.stamps.len() as u64
    }

    /// Charge one fire against the budget, durably, before the command runs, so a restart cannot
    /// burst.
    fn charge(&mut self, now: u64) -> io::Result<()> {
        self.stamps.push(now);
        let limit = self.cfg.limit as usize;
        while self.stamps.len() > limit {
            self.stamps.remove(0);
        }
        self.store.set_stamps(&self.stamps)
    }

    /// One record off the bus, already matched against `sel`.
    fn on_record(&mut self, sel: &Sel, off: u64, subject: &str, body: &[u8]) -> io::Result<()> {
        let raised = match sel {
            Sel::Attention => match escalation(&self.cfg.fleet, off, subject, body) {
                None => return Ok(()),
                Some(None) => return self.set_standing(subject, None),
                Some(Some(token)) => {
                    if self.standing_of(subject).is_some_and(|t| t != token) {
                        self.set_standing(subject, None)?;
                    }
                    Some(token)
                }
            },
            Sel::Ask(_) | Sel::Halt => None,
        };
        let Some(hit) = sel.hit(&self.cfg.fleet, off, subject, body) else {
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

    /// Run the operator's command and return its verdict. Never fails the run: a missing `curl`
    /// must not stop the notifier.
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

    /// Where a selector's scan starts: its cursor, or `--since`.
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

    /// Catch up to the head once and stop: the whole of `--once`.
    fn catch_up(&mut self) -> io::Result<()> {
        let mut conn = self.connect()?;
        let log = log_of(&mut conn, &self.cfg)?;
        self.scope_to(&log)?;
        'selectors: for sel in self.cfg.on.clone() {
            let filter = sel.filter(&self.cfg.fleet);
            let key = sel.key();
            let mut from = self.start_offset(&mut conn, &sel)?;
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

    /// Follow the bus. One subscription per selector, each on its own connection and thread, all
    /// feeding one executor: the command runs one at a time and the journal has one writer.
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

    /// Process one [`Notice`] from a follow-mode reader in read order, holding records from an
    /// `ordered` broker until a [`Notice::Durable`] mark covers them.
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
                }
                self.durable[idx] = 0;
                let ordered = log.ordered;
                self.reader_log[idx] = log;
                if !ordered && !self.held[idx].is_empty() {
                    self.release(idx, u64::MAX)?;
                }
            }
            Notice::Record(idx, off, subject, body) => {
                let Some(sel) = self.cfg.on.get(idx).cloned() else {
                    return Ok(());
                };
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

    /// Page selector `idx`'s held records below `durable`, oldest first, each
    /// cursor moved past it as it goes; once none is held, write the page
    /// checkpoint they held back.
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

/// One selector's reader thread: bounded catch-up, then subscribe from the page's resume
/// cursor, and on any failure back off and do it again.
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
                    // the executor is gone
                    return;
                }
                backoff.attached(Instant::now());
                let mut closed = false;
                loop {
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
                            // the executor is gone
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
                                            // the executor is gone
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

/// Open one reader's connection from the higher of the durable cursor and what this thread
/// has forwarded, else `--since`.
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

/// The log `conn` reaches: the identity its attach's `Hello` answered, or —
/// with no cap file, so no `Hello` went — one asked for now (`None`: a broker
/// that predates identity), and the lineage that SAME answer told beside it
/// ([`astream_broker::Client::log_lineage`]), so the two always describe one
/// log.
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
    /// THE NEGATIVE CONTROL of the carry-over tests: a notifier on this thread
    /// reads no lineage, and so meets a moved generation as another log, from
    /// `--since` — the notifier before lineage.
    static FORGET_LINEAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Usage, printed to stderr on a usage error (exit 2).
const USAGE: &str = "\
aterm-link notify — run a command when the fabric needs a human

  aterm-link notify --fleet <F> --broker <socket> --cap-file <path>...
                    --on <attention|ask:<person>|halt>,... --exec <cmd> [options]

  --on <sel>,...         selectors to watch (repeatable):
                         - `attention`: presence row raising `attention=` (`node.cap`)
                         - `ask:<person>`: ask addressed to `<person>` (`aterm fabric mint-for <person>`)
                         - `halt`: fleet halt coming on (`node.cap`)
                         Refuses (exit 2) if no `--cap-file` grants a selector.
  --exec <cmd>           run `/bin/sh -c <cmd>`; record facts arrive in `$ATERM_NOTIFY_*`
  --rate <n>/<window>    rate budget, e.g. `10/1m`, `1/30s`, `40/1h` (default `10/1m`)
  --since head|start|<n> initial scan offset when a selector has no cursor (default `head`)
  --once                 catch up to the head and exit
  --exec-timeout <ms>    command timeout before SIGKILL (default `30000`)
  --state <dir>          state dir (`<dir>/notify/` holds cursors and journal)

Deduplicated across restarts by offset, and for `attention` by standing value per
subject (clearing and re-raising pages again; crashes during `--exec` re-fire with
`ATERM_NOTIFY_DUP=1`). On an `ordered` broker, records wait until durable (`--once`
leaves non-durable records for the next run).
";

/// `aterm-link notify`.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    let cfg = match parse(args) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("aterm-link notify: {e}");
            eprint!("{USAGE}");
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

/// Parse argv. Every flag takes a value except `--once`; a flag
/// without one is a usage error rather than a silently defaulted setting.
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
                    if !cfg.on.contains(&sel) {
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

/// `<n>/<window>`, where the window is `<n>s`, `<n>m` or `<n>h`.
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

    /// A slow `--exec` leaves the consumer unavailable, and a reader must stop
    /// at its current row rather than queue broker bodies while it runs: an
    /// unbounded channel let a slow command grow without limit.
    #[test]
    fn follow_handoff_buffers_no_row_for_a_stalled_executor() {
        let (tx, _rx) = notice_handoff();
        let row = Notice::Record(0, 41, String::from("first"), vec![1; 1024]);
        assert!(
            matches!(tx.try_send(row), Err(mpsc::TrySendError::Full(_))),
            "a stalled executor must leave no queued row"
        );
    }

    /// Every subject segment that becomes an environment value is a principal: no bidi override,
    /// no oversized segment.
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

    /// THE SELECTORS MATCH BY POSITION, and an over-long subject is not a hit
    /// however its tail reads.
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

    /// `attention` fires only on a row that carries one; a halt only when it comes on.
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

    /// Untrusted text is flattened to printable ASCII and capped.
    #[test]
    fn untrusted_text_is_flattened_and_capped() {
        let text = one_line("hi\x1b[2Jthere\nand\ta tab");
        assert_eq!(text, "hi.[2Jthere.and.a tab");
        assert_eq!(one_line(&"x".repeat(400)).len(), TEXT_MAX);
    }

    /// The budget is a sliding window over durable stamps (the clock is injected).
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

    /// A notifier over `dir` whose command appends each offset it is run for to
    /// `<dir>/fired`, under a budget of `limit` (and `exec` in front of it).
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

    /// **A STANDING ATTENTION PAGES ONCE PER TRANSITION, NOT ONCE PER OFFSET**
    /// (r3-22).
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

    /// **AN ESCALATION NOBODY WAS TOLD OF DOES NOT STAND** (r3-22). Value
    /// dedup must not become the silent drop the module refuses.
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

    /// The journal keeps one verdict per offset, is bounded, and survives a reopen.
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

    /// **A LOG CHANGE DOES NOT LEAVE A STORE BEHIND FOR EVER.** The root
    /// claims log 0; six log changes follow (each a relaxed broker's unclean
    /// reboot, say), each filing a cursor under its own `logs/<identity>/`.
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

    /// The cursor never moves backwards.
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

    /// **A CURSOR BELOW A RETAINING BROKER'S FLOOR READS ON FROM IT**.
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
        broker.retain_before(1).expect("the cut");
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

    /// How the relaxed broker of [`after_a_relaxed_stop`] ended in boot 1.
    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum Ended {
        /// Its clean stop: the listener dropped, and astream flushed the log
        /// in full and recorded it `Closed`.
        Clean,
        /// Killed, nothing lost: the page cache kept every record, and the
        /// run stays `Open` in the log's sidecar.
        Killed,
        /// A power loss: every ack after the first was page cache, so the log
        /// comes back cut to one record, the run still `Open`.
        PowerLoss,
    }

    /// A relaxed broker in boot 1 carries four escalations (`help-0`..`3`,
    /// offsets 0..3), each fired by a notifier on `since` that read the empty
    /// log once before them — its cursor and journal filed under that log's
    /// identity.
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
                // What a stop that never reached the clean close leaves
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

    /// **A RELAXED BROKER'S POWER LOSS COSTS THE NOTIFIER NOTHING BUT WHAT IT
    /// TOOK** ([`crate::durability`]).
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

    /// **NEITHER A CLEAN STOP NOR A KILL COSTS A DEFAULT NOTIFIER ANYTHING
    /// ACROSS A REBOOT.** Stopped cleanly (astream's close: a full flush, then
    /// `Closed`), the log keeps its identity in boot 2, and the new escalation
    /// at offset 4 fires on the default `--since head`, as under a strict
    /// broker.
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

    /// THE CARRY-OVER IS THE STORE OF THE NEWEST GENERATION IT CONTINUES,
    /// whole or not at all.
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

    /// THE EXECUTOR HOLDS A RECORD UNTIL A DURABLE MARK COVERS IT, on a
    /// broker that serves ordered — the follow-mode notices, fed in the
    /// order a reader sends them.
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

    /// A broker opened as aterm opens an ordered one, but with NO HORIZON, so
    /// its durable head moves only for an escalated record or a
    /// `flush_durable` — what the `--once` test below depends on, and nothing
    /// else (the follow-mode one keeps aterm's horizon, which is what it
    /// proves).
    fn ordered_broker(log: &Path) -> Option<astream_broker::Broker> {
        let mut opts = crate::durability::Durability::Ordered.options();
        opts.durability = astream_broker::Durability::Ordered { horizon: None };
        let broker =
            astream_broker::Broker::open_opts(log, opts, || Ok(astream_broker::Prepared::Nothing))
                .expect("open");
        (broker.served_durability().served == astream_broker::Tier::Ordered).then_some(broker)
    }

    /// **ON A BROKER THAT SERVES ORDERED, `--once` PAGES A RECORD ONLY ONCE IT
    /// IS DURABLE** (the module doc).
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

    /// Take a follow-mode reader's notices into `n` until `done` holds, under
    /// a one-minute hang detector (`what` names what never happened).
    fn pump(
        n: &mut Notifier,
        rx: &mpsc::Receiver<Notice>,
        what: &str,
        done: impl Fn(&Notifier) -> bool,
    ) -> &'static str {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut last = "none";
        while !done(n) {
            assert!(Instant::now() < deadline, "{what}: never happened");
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(notice) => {
                    last = match notice {
                        Notice::Record(..) => "record",
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

    /// **IN FOLLOW MODE, ON A BROKER OPENED AS ATERM OPENS AN ORDERED ONE, THE
    /// HORIZON ALONE PAGES AN ATTENTION ROW.** The broker takes
    /// `Durability::Ordered.options()` unchanged — aterm's 100 ms horizon and
    /// escalation table, not [`ordered_broker`]'s no horizon — and a
    /// follow-mode reader subscribes.
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
            let deadline = Instant::now() + Duration::from_secs(60);
            while !follower.is_finished() {
                assert!(Instant::now() < deadline, "the reader never ended");
                std::thread::yield_now();
            }
            follower.join().expect("the reader");
            drop(conn);
            server.shutdown();
            drop(broker);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A lineage of these steps (astream's [`astream_broker::Lineage::from_steps`]).
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

    /// Every selector spells a filename and a filter; `--on ask:../../etc` is refused.
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

    /// The rate flag's grammar, including a zero limit and a zero window.
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

    /// Argv: what is required, what defaults, and a flag with no value.
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

    /// Publication and returned-error cleanup may retire only the stage this
    /// invocation acquired; pre-existing sibling and target bytes stay whole.
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
