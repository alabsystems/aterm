// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **`aterm-link notify`** — the fabric's only push TO a human (§9.3, story A10).
//!
//! ```text
//! aterm-link notify --fleet <F> --broker <ep> --cap-file <p>...
//!                   --on <attention|ask:<p>|halt>,... --exec <cmd>
//!                   [--rate <n>/<window>] [--state DIR] [--since head|start|<off>] [--once]
//!                   [--exec-timeout <ms>] [--tcp [--key-file <p>]]
//! ```
//!
//! Until this verb is green the fabric has no outbound path at all: `ls` and
//! `glance.json` are both PULLS, so an `attention` raised on a headless box at
//! 3 a.m. reaches nobody until somebody looks (§13). This runs a command the
//! operator chose — ntfy, mail, a webhook — once per matching record.
//!
//! ## The failure it chooses, said out loud
//!
//! Exactly-once across a restart is not available: the command is a foreign
//! process, so there is no way to make "the command ran" and "we wrote that down"
//! one atomic act. The journal entry is therefore written **before** the exec and
//! completed **after** it, and an entry left in `firing` by a crash is IN DOUBT.
//!
//! **This rung re-fires an in-doubt entry.** The chosen failure is *a command that
//! runs twice*, never *a command that never runs* — because the one thing A10
//! exists to prevent is an escalation that reaches nobody, and a duplicate push
//! costs a human two seconds while a lost one costs them the night. The re-fire is
//! labelled: the command sees `ATERM_NOTIFY_DUP=1` and the journal keeps the whole
//! history, so a duplicate is legible rather than mysterious. There is no third
//! option, and pretending otherwise would be the "exactly once" claim this
//! codebase keeps refusing to accept.
//!
//! Every other window resolves the same way. A crash between the `firing` write
//! and the spawn re-fires (the command runs, once). A crash between the spawn and
//! the `fired` write re-fires (twice). A command that exits non-zero, or that
//! outlives `--exec-timeout` and is killed, is TERMINAL and is not retried: a
//! notifier that retried a broken command would hammer it forever, and the
//! verdict is on the journal line for a human to read.
//!
//! ## The rate limit never drops anything silently
//!
//! `--rate <n>/<window>` bounds fires, not matches. A record the budget refuses
//! gets its own durable `dropped` journal line, a line on stderr, and a bump of a
//! persisted `suppressed` counter that is handed to the NEXT command that does run
//! as `ATERM_NOTIFY_SUPPRESSED=<n>`. So a human who is being rate-limited learns
//! it from the notification itself rather than from silence. **The counter is
//! cleared only by a command that answered `rc=0`** — a spawn that failed told
//! nobody anything, and a command that exited non-zero, was signalled or was
//! killed on `--exec-timeout` may have died before it pushed. Anything else is
//! the count going to zero while the human is still uninformed, which is the
//! silence again, so the count rides to the next `rc=0`: told twice, never
//! untold.
//!
//! A dropped record is terminal — it is not re-offered when the window rolls,
//! because a queue of deferred 3 a.m. pushes arriving at 6 a.m. is its own kind
//! of noise, and the bus still holds every one of them for `ls` and the digest.
//!
//! ## The body is untrusted data, and it never becomes a command
//!
//! `--exec` is the operator's own shell line and is run as `/bin/sh -c <cmd>`.
//! NOTHING from a record is interpolated into it: every fact reaches the command
//! as an environment variable, and the free text is flattened to printable ASCII
//! and capped at [`TEXT_MAX`] bytes first. A record body cannot become argv, a
//! shell word, or terminal input — the same rule the bridge keeps at the PTY seam,
//! kept here at the exec seam.
//!
//! ## What is remembered, and where
//!
//! Under `<state>/notify/`, beside the bridge's own durable state, each file
//! written the way [`crate::state`] writes its own — temp, `fsync`, `rename`:
//!
//! | file | what it holds |
//! |---|---|
//! | `cur/<key>` | the next offset to scan for one selector; MONOTONE |
//! | `journal` | the newest [`JOURNAL_KEEP`] verdicts, `<off> firing\|fired\|dropped …` |
//! | `rate` | the timestamps of the last fires, for the budget window |
//! | `suppressed` | rate-dropped notifications a human has not been told about yet |
//!
//! Dedup is BY OFFSET, which is §9.3's word and the only identity a record has.
//! A presence row that carries the same `attention=` string at a new offset is a
//! new notification, not a duplicate: collapsing on the value would be a silent
//! drop, which is the one thing this module refuses to do. The budget is what
//! bounds a storm.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::bridge::read_cap_file;
use crate::subject::{is_fleet, is_principal};
use crate::transport::{self, Conn, Transport};

/// How many records one `Fetch` page asks for while catching up.
const PAGE: u32 = 256;

/// How many verdicts the journal keeps. Far above [`PAGE`] on purpose: the
/// cursor never moves backwards, so the only re-scan is of the page a crash
/// interrupted, and the journal has to outlive that page with room to spare.
const JOURNAL_KEEP: usize = 1024;

/// The cap on the free text handed to the command. §3.2's reason, at another
/// seam: a bounded string is one that cannot carry a sentence into somewhere it
/// was not invited.
const TEXT_MAX: usize = 256;

/// How often the exec wait looks at the child. Not synchronisation — the child's
/// exit is the observable, and this only keeps the wait off a core.
const EXEC_POLL: Duration = Duration::from_millis(10);

/// The reconnect back-off floor and ceiling for a follow-mode reader.
const RECONNECT_MIN: Duration = Duration::from_millis(100);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How long a notification command may run before it is killed. A hung `curl`
/// with no timeout would wedge every later notification behind it — which is
/// silence, arriving by a different road.
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// The default budget: ten pushes a minute.
const DEFAULT_LIMIT: u32 = 10;
const DEFAULT_WINDOW_MS: u64 = 60_000;

// ---------------------------------------------------------------------------
// the selectors
// ---------------------------------------------------------------------------

/// One `--on` selector: which records are worth waking a human for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    /// A presence row carrying a non-empty `attention=` (§4.1).
    Attention,
    /// An `ask` addressed to anyone, from this principal.
    Ask(String),
    /// A fleet halt coming ON (§9.3).
    Halt,
}

impl Sel {
    /// Parse one selector word.
    ///
    /// # Errors
    ///
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

    /// The cursor file this selector's scan position lives in. A principal is
    /// `[a-z0-9-]{1,32}` (§3.2), so this is a filename by construction — there is
    /// no path separator any selector can spell.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Sel::Attention => "attention".to_string(),
            Sel::Ask(p) => format!("ask.{p}"),
            Sel::Halt => "halt".to_string(),
        }
    }

    /// The broker filter that carries this selector's records — POSITIONAL, so
    /// the broker's own subject matcher does the first cut.
    #[must_use]
    pub fn filter(&self, fleet: &str) -> String {
        match self {
            Sel::Attention => format!("/f/{fleet}/pub/*/*/presence"),
            Sel::Ask(p) => format!("/f/{fleet}/in/*/*/{p}/ask"),
            Sel::Halt => format!("/f/{fleet}/fleet/*/halt"),
        }
    }

    /// THE SECOND WALL. The filter is a property of the grant this process
    /// happens to hold; this is a property of the reader, and it parses BY
    /// POSITION FROM THE LEFT for the reason `subject::parse_in` does: `>` is
    /// one-or-more segments, so a right-anchored reading of an over-long subject
    /// is exactly the forgery a peer would reach for.
    ///
    /// **AND EVERY SEGMENT IT KEEPS IS A PRINCIPAL, CHECKED HERE.** The shape
    /// used to be the whole of the wall, and the segments went into [`Hit`]
    /// verbatim — from there into `/bin/sh -c`'s ENVIRONMENT, which is the one
    /// seam in this crate where a stranger's bytes reach a human's phone. They
    /// are not cap-forced: a node's grant is `rw,p=<n>:/f/<F>/pub/<n>/>`, so a
    /// presence row's owner segment is whatever that node types, and an inbox
    /// grant is `rw,p=<p>:/f/<F>/in/*/*/<p>/*`, so BOTH the node and the sid of
    /// an `ask` are the SENDER's. astream refuses only wildcards and bytes below
    /// `0x20`, so a 200 000-byte segment, a bidi override and a homoglyph all
    /// arrive intact — and a big enough environment block makes `Command::spawn`
    /// fail `E2BIG` AFTER the rate budget was charged, which is a push the human
    /// never gets and a slot they cannot get back.
    ///
    /// This crate's other two readers of the same rows already refuse it —
    /// `main.rs`'s `column()` truncates, `glance::Row::parse` REJECTS — so the
    /// wall here is theirs, not a third one: the presence arm calls
    /// [`crate::glance::Row::parse`] outright, and the other two arms call the
    /// same [`crate::subject::is_principal`] that `parse_in` and `Row::parse`
    /// are built out of. A refused subject is not a hit: no journal line, no
    /// budget charged, nothing spawned.
    fn hit(&self, fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Hit> {
        let segs: Vec<&str> = subject.split('/').collect();
        if segs.first() != Some(&"") || segs.get(1) != Some(&"f") || segs.get(2) != Some(&fleet) {
            return None;
        }
        let (parsed, _) = Body::decode(body);
        let field = |k: &str| parsed.unknown.get(k).cloned().unwrap_or_default();
        match self {
            Sel::Attention => {
                // ONE READER OF A PRESENCE ROW, NOT THREE. `glance::Row::parse`
                // is the wall `ls` and `glance.json` already stand behind: seven
                // segments, this fleet, `pub`/`presence`, a node that is a
                // principal and an owner that is `node` or an `s-` principal.
                // A second copy of that rule here is a second chance to disagree
                // with it, and the row that reaches a human's phone is the last
                // place to be the looser reader.
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
                if segs.len() != 8 || segs[3] != "in" || segs[7] != "ask" || segs[6] != p {
                    return None;
                }
                // `parse_in`'s rule, minus the node equality it cannot check
                // here (this reader watches every node's lane, not its own):
                // the node segment is a principal and the sid is an `s-` one.
                // Both are the SENDER's choice — the inbox grant cap-forces
                // only `segs[6]`, which is the operator's own `--on ask:<p>`.
                if !crate::subject::is_principal(segs[4]) {
                    return None;
                }
                if !segs[5].starts_with("s-") || !crate::subject::is_principal(segs[5]) {
                    return None;
                }
                Some(Hit {
                    on: self.label(),
                    offset,
                    subject: subject_env(subject),
                    node: segs[4].to_string(),
                    who: segs[6].to_string(),
                    sid: segs[5].to_string(),
                    text: one_line(&parsed.text),
                })
            }
            Sel::Halt => {
                if segs.len() != 6 || segs[3] != "fleet" || segs[5] != "halt" {
                    return None;
                }
                // The halting principal, same wall. `fleet/<who>/halt` is
                // cap-forced for a human's own grant, but a fleet cap that ends
                // in `>` is not, and this segment becomes `$ATERM_NOTIFY_WHO`.
                if !crate::subject::is_principal(segs[4]) {
                    return None;
                }
                // A halt coming OFF is good news, and good news does not wake
                // anybody at 3 a.m.
                if field("state") != "on" {
                    return None;
                }
                Some(Hit {
                    on: self.label(),
                    offset,
                    subject: subject_env(subject),
                    node: String::new(),
                    who: segs[4].to_string(),
                    sid: String::new(),
                    text: one_line(&crate::pct::decode(&field("reason"))),
                })
            }
        }
    }
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
    /// The node the record is about, when the subject names one.
    pub node: String,
    /// The principal it is about (a presence row's owner, an ask's sender, the
    /// human who halted).
    pub who: String,
    /// The addressed session, for an `ask`.
    pub sid: String,
    /// The free text, flattened and capped. UNTRUSTED, and it never becomes a
    /// shell word.
    pub text: String,
}

/// The subject, as an environment value.
///
/// EVERY SEGMENT IS ALREADY A PRINCIPAL by the time this runs, so the length is
/// bounded by construction — which is exactly the kind of agreement between two
/// places that this project's worst defects are made of. It is bounded HERE too,
/// through the same flattening every other untrusted string in this file goes
/// through, so the bound on what `exec` puts in an environment block is one
/// number in one place rather than a property of a parse three arms away.
fn subject_env(subject: &str) -> String {
    one_line(subject)
}

/// Flatten untrusted text to one line of printable ASCII, capped at
/// [`TEXT_MAX`].
///
/// Not decoration. This string is handed to a command the operator wrote, and
/// that command may well echo it at a terminal: a raw `ESC` or `CR` in an
/// attention string must not become an escape sequence somewhere down the line,
/// and a body is arbitrary bytes from anyone holding a cap.
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

// ---------------------------------------------------------------------------
// the durable side
// ---------------------------------------------------------------------------

/// Where a scan starts when a selector has no cursor yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// The log head: a notifier installed today does not page a human through
    /// every escalation the fleet has ever had. The default.
    Head,
    /// Offset zero — the whole retained log.
    Start,
    /// A named offset.
    At(u64),
}

/// `<state>/notify/` — the four files above, written the way the state dir
/// writes its own.
struct Store {
    root: PathBuf,
}

impl Store {
    fn open(state_dir: &str) -> io::Result<Self> {
        let root = Path::new(state_dir).join("notify");
        std::fs::create_dir_all(root.join("cur"))?;
        Ok(Self { root })
    }

    /// Temp, `fsync`, `rename`. A half-written journal would be worse than none:
    /// a truncated verdict reads as "never fired" and fires again.
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
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &target)
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(name))
            .ok()
            .map(|s| s.trim_end_matches('\n').to_string())
            .filter(|s| !s.is_empty())
    }

    fn cursor(&self, key: &str) -> Option<u64> {
        self.read(&format!("cur/{key}"))
            .and_then(|s| s.trim().parse().ok())
    }

    /// MONOTONE. A cursor that walked backwards would re-offer offsets the
    /// journal has already forgotten, and re-firing a week-old escalation is a
    /// duplicate nobody can explain.
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

    fn suppressed(&self) -> u64 {
        self.read("suppressed")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn set_suppressed(&self, n: u64) -> io::Result<()> {
        self.write("suppressed", &n.to_string())
    }
}

/// TEST-ONLY fault injection, armed by `$ATERM_LINK_NOTIFY_FAULT` — and
/// TEST-ONLY is enforced, not merely documented: [`Fault::from_env`] reads the
/// variable only in a build with `debug_assertions`, so a released `aterm-link
/// notify` honours no fault at all.
///
/// THE GATE IS THE BRIDGE'S, APPLIED TO ITS TWIN. `bridge::Fault` closed exactly
/// this in round 2 and its doc argued the whole case; the argument transfers
/// word for word, because `ATERM_LINK_NOTIFY_FAULT` sits in the same place as
/// `ATERM_LINK_FAULT`: on neither `ENV_DENY_VARS` nor `ENV_DENY_PREFIXES` (see
/// `aterm_types::env_sanitize`), and inherited through
/// `fabric_launch::filter_child_env` ON PURPOSE, which that file states in as
/// many words. So the variable survives the PTY child-shell seam, a login
/// profile, a launchd plist and a nested-aterm hop, and what it buys an attacker
/// here is worse than a doc defect: [`Fault::fire`] calls `abort()` between the
/// operator's escalation command and the journal line that records it, and A10
/// is the fabric's ONLY outbound path — an `attention` raised on a headless box
/// at 3 a.m. reaches nobody until somebody looks. One env var turned the
/// escalation channel off and re-paged the human on restart. A fix that gated
/// one of two identically-placed knobs was half a fix;
/// [`tests::no_fault_knob_this_crate_ships_is_armed_in_a_released_binary`] is
/// what makes the next one impossible to forget.
///
/// The in-doubt window is between the command's exit and the journal's `fired`
/// line, and it is microseconds wide. A test that raced it from outside would be
/// the flake this codebase refuses, so the process crashes ITSELF at exactly that
/// point — A3's idiom, one shot, with the marker in the state dir because the
/// process that would remember is the one that dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// Die after the command has run and before the journal says it did.
    KillAfterExec,
}

impl Fault {
    fn from_env(store: &Store) -> Self {
        // A RELEASED BINARY HONOURS NO FAULT — the same first line, for the same
        // reason, as `bridge::Fault::from_env`. See the type's doc: this knob is
        // stripped by no deny list, and the process it kills is the fleet's only
        // way to wake a human.
        if !cfg!(debug_assertions) {
            return Fault::None;
        }
        if store.root.join("fault-fired").exists() {
            return Fault::None;
        }
        match std::env::var("ATERM_LINK_NOTIFY_FAULT").as_deref() {
            Ok("kill-after-exec") => Fault::KillAfterExec,
            _ => Fault::None,
        }
    }

    /// Die HERE, running no destructors — `abort` rather than `exit` for the
    /// reason the bridge gives, and rather than a raw `kill(2)` because it keeps
    /// this module free of `unsafe`.
    fn fire(self, store: &Store) -> ! {
        let _ = std::fs::write(store.root.join("fault-fired"), b"1\n");
        eprintln!("aterm-link notify: ATERM_LINK_NOTIFY_FAULT={self:?} — dying here");
        std::process::abort();
    }
}

// ---------------------------------------------------------------------------
// the notifier
// ---------------------------------------------------------------------------

/// Everything the verb was told.
#[derive(Debug, Clone)]
pub struct Config {
    pub fleet: String,
    pub broker: String,
    pub transport: Transport,
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
}

struct Notifier {
    cfg: Config,
    store: Store,
    journal: Vec<String>,
    stamps: Vec<u64>,
    suppressed: u64,
    fault: Fault,
    counts: Counts,
}

impl Notifier {
    fn new(cfg: Config) -> io::Result<Self> {
        let store = Store::open(&cfg.state_dir)?;
        let fault = Fault::from_env(&store);
        Ok(Self {
            journal: store.journal(),
            stamps: store.stamps(),
            suppressed: store.suppressed(),
            store,
            cfg,
            fault,
            counts: Counts::default(),
        })
    }

    /// One broker connection with every capability attached.
    fn connect(&self) -> io::Result<Conn> {
        let (mut conn, _closer) = transport::connect(&self.cfg.transport, &self.cfg.broker)?;
        for path in &self.cfg.cap_files {
            for cap in read_cap_file(path)? {
                conn.attach(&cap.grant, &cap.tag)?;
            }
        }
        Ok(conn)
    }

    /// The verdict the journal holds for an offset, if any.
    ///
    /// Keyed by OFFSET alone, not by (offset, selector): a record is one record,
    /// and it should wake a human once however many selectors name it. The three
    /// selector filters are disjoint at the subject's third segment anyway
    /// (`pub` / `in` / `fleet`), so today no offset can match two.
    fn verdict(&self, off: u64) -> Option<&str> {
        self.journal.iter().find_map(|line| {
            let mut it = line.split_whitespace();
            let at: u64 = it.next()?.parse().ok()?;
            (at == off).then(|| it.next()).flatten()
        })
    }

    /// Write one verdict, replacing any the offset already had, and keep the
    /// newest [`JOURNAL_KEEP`].
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
        self.store.set_journal(&self.journal)
    }

    /// Whether the budget has room, at `now`. Stamps outside the window are
    /// forgotten here rather than on a timer.
    ///
    /// A clock that jumped BACKWARDS leaves stamps in the future; those count as
    /// inside the window, so the budget errs towards suppressing (and saying so)
    /// rather than towards a burst nobody asked for.
    fn has_room(&mut self, now: u64) -> bool {
        let window = self.cfg.window_ms;
        self.stamps.retain(|t| now.saturating_sub(*t) < window);
        u64::from(self.cfg.limit) > self.stamps.len() as u64
    }

    /// Charge one fire against the budget, durably — BEFORE the command runs, so
    /// a crash cannot hand the budget back and let a restart burst.
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
        let Some(hit) = sel.hit(&self.cfg.fleet, off, subject, body) else {
            return Ok(());
        };
        let dup = match self.verdict(off) {
            // Terminal: this offset has had its answer. Never fired twice, and
            // never re-offered after the window rolls.
            Some("fired" | "dropped") => {
                self.counts.skipped += 1;
                return Ok(());
            }
            // IN DOUBT — a crash between the spawn and the completion. Fire
            // again and say so; see the module doc for why this direction.
            Some(_) => true,
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
                     suppressed={} — the record is still on the bus",
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
        // CLEARED ONLY WHEN THE COMMAND WAS TOLD, and `rc=0` is the only verdict
        // that says so.
        //
        // This used to clear unconditionally, `verdict` unread — including
        // `rc=spawn-failed`, which is the case where `Command::spawn` itself
        // failed and NO process ever received `ATERM_NOTIFY_SUPPRESSED`. Twenty
        // rate-dropped escalations then became zero on disk and the human was
        // never told any of them existed, which is precisely the silence the
        // rate limiter's whole design exists to avoid (see the header).
        //
        // The other verdicts are judged the same way and for the same reason.
        // `rc=<n>` and `rc=signalled` mean the command ran and did not say it
        // succeeded; `rc=timeout` means it was killed part-way; `rc=wait-failed`
        // means this process lost track of it. In every one of those the env was
        // delivered but the PUSH may not have been, and the module's chosen
        // direction is stated at the top of this file: told twice, never untold.
        // So the count is KEPT and rides on the next command that answers `rc=0`.
        // It is a counter, not a queue — keeping it costs one number.
        if carried > 0 && verdict == "rc=0" {
            self.suppressed -= carried;
            self.store.set_suppressed(self.suppressed)?;
        }
        if dup {
            self.counts.dup += 1;
        }
        self.counts.fired += 1;
        Ok(())
    }

    /// Run the operator's command. Never fails the run: a notifier that exited
    /// because `curl` was missing would be a notifier that stopped notifying.
    fn exec(&self, hit: &Hit, dup: bool, suppressed: u64) -> String {
        let mut cmd = Command::new("/bin/sh");
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
                    }
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
                    "aterm-link notify: --exec outlived --exec-timeout for off={}; killed",
                    hit.offset
                );
                return "rc=timeout".to_string();
            }
            std::thread::sleep(EXEC_POLL);
        }
    }

    /// Where a selector's scan starts: its cursor, or `--since`.
    fn start_offset(&self, conn: &mut Conn, sel: &Sel) -> io::Result<u64> {
        if let Some(off) = self.store.cursor(&sel.key()) {
            return Ok(off);
        }
        Ok(match self.cfg.since {
            Since::Start => 0,
            Since::At(n) => n,
            // `max = 0` is the broker's head query (R2).
            Since::Head => conn.fetch(0, &sel.filter(&self.cfg.fleet), 0)?.1 .1,
        })
    }

    /// Catch up to the head, once, and stop. This is the whole of `--once`, and
    /// it is also what a follow-mode reader's first subscription replays.
    fn catch_up(&mut self) -> io::Result<()> {
        let mut conn = self.connect()?;
        for sel in self.cfg.on.clone() {
            let filter = sel.filter(&self.cfg.fleet);
            let key = sel.key();
            let mut from = self.start_offset(&mut conn, &sel)?;
            loop {
                let (rows, (next, head)) = conn.fetch(from, &filter, PAGE)?;
                for (off, subject, body) in rows {
                    self.on_record(&sel, off, &subject, &body)?;
                }
                self.store.set_cursor(&key, next)?;
                if next >= head || next <= from {
                    break;
                }
                from = next;
            }
        }
        Ok(())
    }

    /// Follow the bus until something kills us. One subscription per selector,
    /// each on its own connection and its own thread, all feeding one executor —
    /// because the command must run one at a time and the journal has one writer.
    fn follow(&mut self) -> io::Result<()> {
        let (tx, rx) = mpsc::channel::<(usize, u64, String, Vec<u8>)>();
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
        while let Ok((idx, off, subject, body)) = rx.recv() {
            let Some(sel) = self.cfg.on.get(idx).cloned() else {
                continue;
            };
            self.on_record(&sel, off, &subject, &body)?;
            self.store.set_cursor(&sel.key(), off + 1)?;
        }
        // Every reader gave up, which only happens when they all failed to
        // reconnect for good. Say so rather than exiting 0.
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "every subscription ended",
        ))
    }
}

/// One selector's reader thread: connect, subscribe from the durable cursor,
/// pump, and on any failure back off and do it again.
///
/// It re-reads the cursor from DISK on every reconnect and never resumes below
/// what it has already forwarded, so a reconnect replays at most the records the
/// executor had not finished with — which the journal then dedups.
fn reader(
    idx: usize,
    sel: &Sel,
    cfg: &Config,
    root: &Path,
    tx: &mpsc::Sender<(usize, u64, String, Vec<u8>)>,
) {
    let filter = sel.filter(&cfg.fleet);
    let key = sel.key();
    let mut seen: Option<u64> = None;
    let mut backoff = RECONNECT_MIN;
    loop {
        match subscribe(cfg, root, &key, &filter, seen) {
            Ok(mut sub) => {
                backoff = RECONNECT_MIN;
                loop {
                    match sub.recv() {
                        Ok(Some((off, subject, body))) => {
                            seen = Some(seen.map_or(off + 1, |s| s.max(off + 1)));
                            if tx.send((idx, off, subject, body)).is_err() {
                                return; // the executor is gone
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            eprintln!("aterm-link notify: {} subscription ended: {e}", sel.label());
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
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// Open one subscription for a reader thread, from the highest of the durable
/// cursor, what the thread has already forwarded, and — only when neither
/// exists — `--since`.
fn subscribe(
    cfg: &Config,
    root: &Path,
    key: &str,
    filter: &str,
    seen: Option<u64>,
) -> io::Result<astream_broker::Subscription<Box<dyn transport::Stream>>> {
    let store = Store {
        root: root.to_path_buf(),
    };
    let (mut conn, _closer) = transport::connect(&cfg.transport, &cfg.broker)?;
    for path in &cfg.cap_files {
        for cap in read_cap_file(path)? {
            conn.attach(&cap.grant, &cap.tag)?;
        }
    }
    let from = match (store.cursor(key), seen) {
        (Some(c), Some(s)) => c.max(s),
        (Some(c), None) => c,
        (None, Some(s)) => s,
        (None, None) => match cfg.since {
            Since::Start => 0,
            Since::At(n) => n,
            Since::Head => conn.fetch(0, filter, 0)?.1 .1,
        },
    };
    conn.subscribe(from, filter)
}

// ---------------------------------------------------------------------------
// the command line
// ---------------------------------------------------------------------------

/// Usage, printed to stderr on a usage error (exit 2, aterm's convention).
const USAGE: &str = "\
aterm-link notify — run a command when the fabric needs a human (§9.3)

  aterm-link notify --fleet <F> --broker <ep> --cap-file <path>...
                    --on <attention|ask:<p>|halt>,... --exec <cmd> [options]

  --on <sel>,...         what is worth waking someone for; repeatable
  --exec <cmd>           the command, run as `/bin/sh -c <cmd>`. Every fact about
                         the record arrives in $ATERM_NOTIFY_* — nothing from a
                         record is ever interpolated into this string
  --rate <n>/<window>    the budget, e.g. 10/1m, 1/30s, 40/1h (default 10/1m)
  --since head|start|<n> where a selector with no cursor starts (default head)
  --once                 catch up to the head and exit, rather than following
  --exec-timeout <ms>    how long the command may run before it is killed (30000)
  --state <dir>          the state dir; the journal lives in <dir>/notify/
  --tcp                  reach the broker over TCP — PLAINTEXT unless --key-file
  --key-file <path>      astream's sealed wire (needs --tcp; see `ls` for the build note)

The command runs once per matching OFFSET, deduped durably across a restart. The
one window that cannot be closed is the exec itself: a crash between the spawn
and the journal's completion RE-FIRES, with ATERM_NOTIFY_DUP=1. A push that
arrives twice is the failure this verb chooses; one that never arrives is not.
";

/// `aterm-link notify`.
///
/// # Panics
///
/// Never: every failure is an exit code and a line on stderr.
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
    let outcome = if once {
        notifier.catch_up()
    } else {
        notifier.catch_up().and_then(|()| notifier.follow())
    };
    let c = notifier.counts;
    println!(
        "notify: fired={} dup={} skipped={} dropped={} suppressed={}",
        c.fired, c.dup, c.skipped, c.dropped, notifier.suppressed
    );
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm-link notify: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Parse argv. Every flag takes a value except `--tcp` and `--once`; a flag
/// without one is a usage error rather than a silently defaulted setting.
fn parse(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config {
        fleet: String::new(),
        broker: String::new(),
        transport: Transport::Unix,
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
    let mut tcp = false;
    let mut key_file: Option<String> = None;
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
            "--tcp" => tcp = true,
            "--key-file" => key_file = Some(value()?),
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
        return Err("--on is required: there is nothing to watch for".to_string());
    }
    if cfg.exec.is_empty() {
        return Err("--exec is required: there is nothing to run".to_string());
    }
    // A KEY WITHOUT `--tcp` IS A MISTAKE, NOT A DEFAULT — `serve`'s rule, for
    // `serve`'s reason, and the two must not disagree about it.
    cfg.transport = match (tcp, key_file) {
        (false, None) => Transport::Unix,
        (true, None) => {
            eprintln!(
                "aterm-link notify: --tcp without --key-file is PLAINTEXT. Trusted networks only."
            );
            Transport::Tcp
        }
        (true, Some(path)) => Transport::Sealed(Box::new(
            transport::read_key_file(&path).map_err(|e| format!("--key-file {path}: {e}"))?,
        )),
        (false, Some(_)) => {
            return Err("--key-file needs --tcp (the sealed wire is a TCP transport)".to_string())
        }
    };
    if cfg.state_dir.is_empty() {
        cfg.state_dir = default_state_dir();
    }
    Ok(cfg)
}

/// `<n>/<window>`, where the window is `<n>s`, `<n>m` or `<n>h`.
fn parse_rate(s: &str) -> Result<(u32, u64), String> {
    let bad = || format!("--rate {s}: expected <n>/<window>, e.g. 10/1m, 1/30s, 40/1h");
    let (n, window) = s.split_once('/').ok_or_else(bad)?;
    let limit: u32 = n.parse().map_err(|_| bad())?;
    if limit == 0 {
        return Err(format!(
            "--rate {s}: a limit of 0 would drop every notification; \
             stop the notifier instead of blinding it"
        ));
    }
    let (digits, unit) = window.split_at(window.len().saturating_sub(1));
    let count: u64 = digits.parse().map_err(|_| bad())?;
    let ms = match unit {
        "s" => count * 1_000,
        "m" => count * 60_000,
        "h" => count * 3_600_000,
        _ => return Err(bad()),
    };
    if ms == 0 {
        return Err(format!("--rate {s}: a zero window is not a window"));
    }
    Ok((limit, ms))
}

/// `$XDG_STATE_HOME/aterm-link`, else `$HOME/.local/state/aterm-link`, else the
/// working directory.
///
/// The same rule `serve` uses, and deliberately the same directory: §9.3 puts the
/// journal "beside the bridge's other durable state". It is duplicated rather
/// than shared because this wave's ownership split gave `main.rs` to another
/// rung; one of the two copies should go when a later rung next touches it.
fn default_state_dir() -> String {
    if let Ok(x) = std::env::var("XDG_STATE_HOME") {
        if !x.is_empty() {
            return format!("{x}/aterm-link");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return format!("{home}/.local/state/aterm-link");
        }
    }
    "./aterm-link-state".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **EVERY SUBJECT SEGMENT THAT BECOMES AN ENVIRONMENT VALUE IS A
    /// PRINCIPAL.** The shape check was the whole wall, and the segments went
    /// into `Hit` verbatim — from there into `/bin/sh -c`'s environment.
    ///
    /// They are not cap-forced. A presence row's OWNER segment is whatever the
    /// publishing node types; an `ask`'s node AND sid are whatever the SENDER
    /// types (its grant cap-forces only the addressee, which is the operator's
    /// own `--on ask:<p>`). astream refuses wildcards and bytes under `0x20` and
    /// nothing else, so 200 000 bytes, a bidi override and a homoglyph all
    /// arrive intact. The 200 000-byte case is the sharp one: the budget slot is
    /// charged BEFORE `exec`, and an environment block over `ARG_MAX` fails
    /// `spawn` with `E2BIG` — a terminal `fired rc=spawn-failed` for a push that
    /// never happened, ten times a minute, while the operator's real escalations
    /// answer `dropped reason=rate`.
    ///
    /// `main.rs`'s `column()` truncates these segments and `glance::Row::parse`
    /// rejects them; this is the reader whose output reaches a human's phone.
    #[test]
    fn a_subject_segment_that_is_not_a_principal_never_becomes_an_environment_value() {
        let huge = "x".repeat(200_000);
        let bidi = "s-\u{202e}gnitset";

        let ask = Sel::Ask("h-andrew".into());
        assert!(ask
            .hit("f1", 7, "/f/f1/in/n-a/s-b/h-andrew/ask", b"v=1 t=1 text=hi")
            .is_some());
        for bad in [
            format!("/f/f1/in/n-a/{huge}/h-andrew/ask"), // the SENDER's sid
            format!("/f/f1/in/{huge}/s-b/h-andrew/ask"), // the SENDER's node
            format!("/f/f1/in/n-a/{bidi}/h-andrew/ask"), // a bidi override
            "/f/f1/in/n-a/h-b/h-andrew/ask".to_string(), // a sid that is not s-
            "/f/f1/in/NODE/s-b/h-andrew/ask".to_string(), // not lowercase
        ] {
            assert_eq!(ask.hit("f1", 7, &bad, b"v=1 t=1"), None, "{}", &bad[..40]);
        }

        let att = Sel::Attention;
        let body = b"v=1 t=1 state=live attention=needs%20a%20key";
        assert!(att
            .hit("f1", 3, "/f/f1/pub/n-a/s-b/presence", body)
            .is_some());
        for bad in [
            format!("/f/f1/pub/n-a/{huge}/presence"), // the publisher's own face
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

        // AND WHAT DOES PASS IS BOUNDED, at one number in one place rather than
        // as a consequence of a parse three arms away.
        let hit = ask
            .hit("f1", 7, "/f/f1/in/n-a/s-b/h-andrew/ask", b"v=1 t=1 text=hi")
            .expect("the honest ask");
        for v in [&hit.subject, &hit.node, &hit.who, &hit.sid, &hit.text] {
            assert!(v.len() <= TEXT_MAX, "{v} is {} bytes", v.len());
            assert!(
                v.chars().all(|c| c.is_ascii_graphic() || c == ' '),
                "{v} is not printable ASCII"
            );
        }
    }

    /// **NO FAULT KNOB THIS CRATE SHIPS IS ARMED IN A RELEASED BINARY**, and
    /// the scan is the whole `src/` directory rather than the two files that
    /// have one today.
    ///
    /// The defect this replaces was a fix that closed ONE of two identical
    /// doors: `bridge.rs` gated `$ATERM_LINK_FAULT` on `debug_assertions` and
    /// wrote a long doc about why, and `notify.rs` — same idiom, same env
    /// position, same `abort()` — was left armed in every build. Four
    /// independent lenses reported the survivor. A list of files to check would
    /// have the same shape of gap as that fix did, so this reads the directory:
    /// any file that reads a `*_FAULT` variable must carry the release gate, and
    /// the next knob added anywhere in the crate fails here until it does.
    #[test]
    fn no_fault_knob_this_crate_ships_is_armed_in_a_released_binary() {
        // Assembled, so this test's own source is not the thing it finds.
        let reads_a_fault = concat!("_FAULT", "\").as_deref()");
        let gate = concat!("!cfg!(debug_", "assertions)");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut knobs: Vec<String> = Vec::new();
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).expect("the crate's own src/") {
            let path = entry.expect("a dir entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            files += 1;
            let body = std::fs::read_to_string(&path).expect("read a source file");
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !body.contains(reads_a_fault) {
                continue;
            }
            knobs.push(name.clone());
            assert!(
                body.contains(gate),
                "{name} reads a fault knob out of the environment with no \
                 release gate: a shipped binary honours it"
            );
        }
        assert!(
            files > 1,
            "the scan must read the whole crate, not one file"
        );
        knobs.sort();
        // NOT VACUOUS. Both known knobs must still be FOUND by the scan — a
        // rename that hides one from this search is the same hole again.
        assert_eq!(
            knobs,
            vec!["bridge.rs".to_string(), "notify.rs".to_string()],
            "the two fault knobs this crate ships"
        );
    }

    /// The text of `fn default_state_dir` in `src`, from its signature to the
    /// line that closes it. Whitespace and all: the point of the pin is that the
    /// copies are IDENTICAL, so a comparison that normalised anything would let
    /// the copies drift in exactly the way that made this worth pinning.
    fn state_dir_rule(src: &str) -> &str {
        let at = src
            .find("fn default_state_dir() -> String {")
            .expect("the state-dir rule");
        let rest = &src[at..];
        let end = rest.find("\n}\n").expect("its closing brace") + 3;
        &rest[..end]
    }

    /// **THE STATE-DIR RULE IS DUPLICATED FROM `cli.rs`, SO IT IS PINNED TO
    /// IT.** `cli.rs` carried this rule as a binary main until 2026-09-10, so it
    /// exists four times — there, in `hook.rs`, in `notify.rs` and in `tui.rs` —
    /// and it belongs in `state.rs` where all four could call one copy. Until it
    /// moves, every copy is pinned to the original: change `main.rs`'s rule and
    /// this fails, rather than leaving `notify` reading its state under a
    /// directory `serve` no longer writes.
    #[test]
    fn the_state_dir_rule_matches_the_serve_binarys() {
        assert_eq!(
            state_dir_rule(include_str!("notify.rs")),
            state_dir_rule(include_str!("cli.rs")),
            "notify.rs's copy of the state-dir rule has drifted from main.rs's; \
             `aterm-link notify` would read its state under a directory \
             `aterm-link serve` no longer uses"
        );
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
            transport: Transport::Unix,
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
    /// however its tail reads. `…/in/n-a/s-b/s-1/h-andrew/ask` has a
    /// right-anchored reading in which `h-andrew` asked; parsing from the left
    /// makes it a nine-segment subject and nothing else.
    #[test]
    fn a_selector_matches_by_position_and_an_overlong_subject_never_matches() {
        let ask = Sel::Ask("h-andrew".into());
        let honest = "/f/f1/in/n-a/s-b/h-andrew/ask";
        let hit = ask
            .hit("f1", 7, honest, b"v=1 t=1 text=is%20this%20ok%3F")
            .expect("the honest seven-segment ask matches");
        assert_eq!((hit.who.as_str(), hit.sid.as_str()), ("h-andrew", "s-b"));
        assert_eq!(hit.text, "is this ok?");
        for bad in [
            "/f/f1/in/n-a/s-b/s-1/h-andrew/ask", // eight segments, forged tail
            "/f/f1/in/n-a/s-b/s-1/ask",          // a different sender
            "/f/f1/in/n-a/s-b/h-andrew/note",    // a different kind
            "/f/f2/in/n-a/s-b/h-andrew/ask",     // another fleet
            "/f/f1/pub/n-a/s-b/h-andrew/ask",    // another face
        ] {
            assert_eq!(ask.hit("f1", 7, bad, b"v=1 t=1"), None, "{bad}");
        }
    }

    /// An `attention` selector fires on a presence row that HAS one, and on
    /// nothing else — a row with no `attention=`, an empty one, or the `-` the
    /// roster prints for "none" are all silence.
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
        // A halt coming OFF is good news, and good news wakes nobody.
        let halt = Sel::Halt;
        let lane = "/f/f1/fleet/h-andrew/halt";
        assert!(halt
            .hit("f1", 4, lane, b"v=1 t=1 state=on reason=main%20broken")
            .is_some());
        assert_eq!(halt.hit("f1", 4, lane, b"v=1 t=1 state=off"), None);
    }

    /// UNTRUSTED TEXT IS FLATTENED. An attention string can carry any byte a cap
    /// holder likes, including an escape sequence, and the command that receives
    /// it may well echo it at a terminal.
    #[test]
    fn untrusted_text_is_flattened_and_capped() {
        let text = one_line("hi\x1b[2Jthere\nand\ta tab");
        assert_eq!(text, "hi.[2Jthere.and.a tab");
        assert_eq!(one_line(&"x".repeat(400)).len(), TEXT_MAX);
    }

    /// The budget counts fires inside the window and forgets older ones. The
    /// clock is INJECTED, so this is arithmetic rather than a race.
    #[test]
    fn the_budget_is_a_sliding_window_over_the_stamps_it_persisted() {
        let dir = scratch("budget");
        let mut n = Notifier::new(cfg(&dir)).expect("open");
        assert!(n.has_room(1_000));
        n.charge(1_000).expect("charge");
        assert!(n.has_room(1_100));
        n.charge(1_100).expect("charge");
        assert!(!n.has_room(1_200), "two in the window is the limit of 2");
        // The window rolls: the first stamp is 60s old at 61_000.
        assert!(n.has_room(61_050), "the 1_000 stamp has left the window");
        // And the stamps are DURABLE: a restart inside the window still counts.
        let mut back = Notifier::new(cfg(&dir)).expect("reopen");
        assert!(
            !back.has_room(1_200),
            "a restart does not hand the budget back"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The journal is keyed by offset, keeps one verdict per offset, is bounded,
    /// and survives a reopen — which is the whole of "deduped by offset across a
    /// restart".
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
        for off in 100..(100 + JOURNAL_KEEP as u64 + 8) {
            n.record(off, format!("{off} fired on=attention rc=0 dup=0"))
                .expect("write");
        }
        assert_eq!(n.journal.len(), JOURNAL_KEEP);
        assert_eq!(n.verdict(5), None, "the oldest verdicts are forgotten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cursor is MONOTONE. A cursor that walked backwards would re-offer
    /// offsets the bounded journal has already forgotten.
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

    /// Every selector spells a filename and a filter, and a bad one is refused
    /// by name. `--on ask:../../etc` must never become a path.
    #[test]
    fn selectors_parse_and_spell_a_filename() {
        assert_eq!(Sel::parse("attention"), Ok(Sel::Attention));
        assert_eq!(Sel::parse("halt"), Ok(Sel::Halt));
        assert_eq!(Sel::parse("ask:h-andrew"), Ok(Sel::Ask("h-andrew".into())));
        assert_eq!(Sel::Ask("h-andrew".into()).key(), "ask.h-andrew");
        assert_eq!(
            Sel::Ask("h-andrew".into()).filter("f1"),
            "/f/f1/in/*/*/h-andrew/ask"
        );
        for bad in ["ask:../../etc", "ask:", "ask:H-Andrew", "wake", "ask"] {
            assert!(Sel::parse(bad).is_err(), "{bad}");
        }
    }

    /// The rate flag's grammar, including the two limits that would blind the
    /// notifier without saying so.
    #[test]
    fn the_rate_flag_refuses_a_budget_that_would_drop_everything() {
        assert_eq!(parse_rate("10/1m"), Ok((10, 60_000)));
        assert_eq!(parse_rate("1/30s"), Ok((1, 30_000)));
        assert_eq!(parse_rate("40/2h"), Ok((40, 7_200_000)));
        assert!(
            parse_rate("0/1m").is_err(),
            "a zero limit is refused by name"
        );
        assert!(
            parse_rate("10/0m").is_err(),
            "a zero window is not a window"
        );
        for bad in ["10", "10/1d", "x/1m", "10/m", ""] {
            assert!(parse_rate(bad).is_err(), "{bad}");
        }
    }

    /// Argv: what is required, what defaults, and the key-without-`--tcp`
    /// mistake `serve` also refuses.
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
        assert!(parse(&args(&format!("{base} --on halt --key-file /dev/null"))).is_err());
        assert!(
            parse(&args(&format!("{base} --on halt --rate"))).is_err(),
            "a flag needs a value"
        );
    }
}
