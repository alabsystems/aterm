// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **`aterm-link notify`** — the fabric's only push TO a human (§9.3, story A10).
//!
//! ```text
//! aterm-link notify --fleet <F> --broker <ep> --cap-file <p>...
//!                   --on <attention|ask:<person>|halt>,... --exec <cmd>
//!                   [--rate <n>/<window>] [--state DIR] [--since head|start|<off>] [--once]
//!                   [--exec-timeout <ms>] [--tcp [--key-file <p>]]
//! ```
//!
//! `ls` and `aterm fabric` are PULLS, so without this an `attention` raised on
//! a headless box reaches nobody until somebody looks (§13). This runs a
//! command the operator chose — ntfy, mail, a webhook — once per matching record.
//!
//! ## The failure it chooses, said out loud
//!
//! Exactly-once across a restart is not available: the command is a foreign
//! process, so there is no way to make "the command ran" and "we wrote that down"
//! one atomic act. The journal entry is therefore written **before** the exec and
//! completed **after** it, and an entry left in `firing` by a crash is IN DOUBT.
//!
//! **An in-doubt entry re-fires.** The chosen failure is *a command that runs
//! twice*, never *a command that never runs*: A10 exists to prevent an
//! escalation that reaches nobody. The re-fire is labelled — the command sees
//! `ATERM_NOTIFY_DUP=1` and the journal keeps the history.
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
//! written temp, `fsync`, `rename` — [`crate::state`]'s durable tier, and for
//! its reason: these dedup a human's notifications by bus offset, and the bus
//! outlives a reboot:
//!
//! | file | what it holds |
//! |---|---|
//! | `cur/<key>` | the next offset to scan for one selector; MONOTONE |
//! | `journal` | the newest [`JOURNAL_KEEP`] verdicts, `<off> firing\|fired\|dropped …` |
//! | `log` | the identity of the bus log the two above are offsets on |
//! | `logs/<identity>/` | `cur/` and `journal` of any other log this notifier read ([`Store::for_log`]) |
//! | `standing` | per presence subject, the `attention=` a human was last told of ([`STANDING_KEEP`]) |
//! | `rate` | the timestamps of the last fires, for the budget window |
//! | `suppressed` | rate-dropped notifications a human has not been told about yet |
//!
//! Dedup is BY OFFSET, which is §9.3's word and the only identity a record has —
//! and, for `attention`, ALSO BY VALUE PER SUBJECT, once per transition. A
//! presence row is a LAST-VALUE face, not an event stream: the bridge writes a
//! session's row again, standing `attention=` and all, on every attach (its
//! first, every reconnect, the successor's after a seamless update), on every
//! role, detail, phase or title change in `meta` mode, and in its `exited` row.
//! Paged once per offset, one open escalation re-paged the human on each of
//! those and spent the budget a genuinely new one then found empty. So a row
//! whose `attention=` is the one the human was last TOLD of on that subject
//! pages nobody and charges nothing (`standing=` in the closing line); a clear
//! (`attention=-`, or none) re-arms the subject, and a raise after it pages
//! again, the same text included. This is not the silent drop the module
//! refuses: the publisher already collapses an unchanged value inside its own
//! window, and every offset of the face is read in order, so a clear that
//! reached the bus is seen before the raise after it — an escalation the bus
//! can tell apart from the last one always pages. "Told" is a command that
//! answered `rc=0`, as for `suppressed`: a rate-dropped escalation, or one whose
//! command failed, leaves nothing standing, so its next republish is offered
//! again — told twice, never untold. The one loss: a retention trim that
//! removed a clear below this notifier's cursor before it read it. The budget is
//! still what bounds a storm of distinct escalations.

use aterm_uds::spawnfd::Stdio;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::bridge::ACK_DEADLINE;
use crate::subject::{is_fleet, is_principal};
use crate::transport::{self, Backoff, Closer, Conn, Transport};

/// How many records one `Fetch` page asks for while catching up.
const PAGE: u32 = 256;

/// How many verdicts the journal keeps. Far above [`PAGE`] on purpose: the
/// cursor never moves backwards, so the only re-scan is of the page a crash
/// interrupted, and the journal has to outlive that page with room to spare.
const JOURNAL_KEEP: usize = 1024;

/// How many subjects `standing` remembers. Past it the oldest is forgotten,
/// which can only cost a re-page of an escalation already told, never a missed
/// one.
const STANDING_KEEP: usize = JOURNAL_KEEP;

/// The cap on the free text handed to the command. §3.2's reason, at another
/// seam: a bounded string is one that cannot carry a sentence into somewhere it
/// was not invited.
const TEXT_MAX: usize = 256;

/// How often the exec wait looks at the child. Not synchronisation — the child's
/// exit is the observable, and this only keeps the wait off a core.
const EXEC_POLL: Duration = Duration::from_millis(10);

/// How long a notification command may run before it is killed. A hung `curl`
/// with no timeout would wedge every later notification behind it — which is
/// silence, arriving by a different road.
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// The default budget: ten pushes a minute.
const DEFAULT_LIMIT: u32 = 10;
const DEFAULT_WINDOW_MS: u64 = 60_000;

#[derive(Debug)]
enum Notice {
    Record(usize, u64, String, Vec<u8>),
    /// The broker scanned through `next`, including offsets the selector did
    /// not match. Sent after the page's records so the executor persists it
    /// only after it has handled every matching record in that page.
    Checkpoint(usize, u64),
    /// The reader at this index just connected, to a broker serving the log
    /// with this identity (`None`: one that predates identity). Sent before
    /// any record of that connection, so the executor files each record's
    /// verdict and cursor under the log it was read from ([`Store::for_log`]).
    Log(usize, Option<astream_broker::LogIdentity>),
}

/// A broker reader hands one row directly to the serial notification executor.
/// No second row is buffered while an operator command runs: a broker body can
/// be 16 MiB, and an unbounded channel let a slow command grow without limit.
fn notice_handoff() -> (mpsc::SyncSender<Notice>, mpsc::Receiver<Notice>) {
    mpsc::sync_channel(0)
}

// ---------------------------------------------------------------------------
// the selectors
// ---------------------------------------------------------------------------

/// One `--on` selector: which records are worth waking a human for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    /// A presence row carrying a non-empty `attention=` (§4.1).
    Attention,
    /// An `ask` ADDRESSED TO this principal — a session's `post to=<p>
    /// kind=ask`, which its node publishes on `/f/<F>/in/p/<p>/<node>/ask`: how
    /// a person is reached (§9.3). The person's own cap
    /// ([`crate::enable::person_grants`]'s `ro:/f/<F>/in/p/<p>/>`) reads it.
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
            // `ask-to.`, not the `ask.` an older build kept for its asks FROM
            // `<p>`: that cursor scanned another filter, and resuming this one
            // from it would skip every ask TO `<p>` below it, unseen.
            Sel::Ask(p) => format!("ask-to.{p}"),
            Sel::Halt => "halt".to_string(),
        }
    }

    /// The grant that reads [`Sel::filter`], and which cap carries it — what a
    /// refused first read names ([`Notifier::preflight`]).
    #[must_use]
    pub fn grant(&self, fleet: &str) -> String {
        match self {
            Sel::Attention => {
                format!("`ro:/f/{fleet}/pub/>`, which node.cap and a person's cap both hold")
            }
            Sel::Ask(p) => format!(
                "`ro:/f/{fleet}/in/p/{p}/>`, which {p}'s own cap holds (`aterm fabric mint-for \
                 {p} --out <cap>` on the host that holds the fleet's mint secret)"
            ),
            Sel::Halt => format!("`ro:/f/{fleet}/fleet/>`, which node.cap holds"),
        }
    }

    /// The broker filter that carries this selector's records — POSITIONAL, so
    /// the broker's own subject matcher does the first cut.
    #[must_use]
    pub fn filter(&self, fleet: &str) -> String {
        match self {
            Sel::Attention => format!("/f/{fleet}/pub/*/*/presence"),
            Sel::Ask(p) => format!("/f/{fleet}/in/p/{p}/*/ask"),
            Sel::Halt => format!("/f/{fleet}/fleet/*/halt"),
        }
    }

    /// THE SECOND WALL. The filter is a property of the grant this process
    /// happens to hold; this is a property of the reader, and it parses BY
    /// POSITION FROM THE LEFT for the reason `subject::parse_in` does: `>` is
    /// one-or-more segments, so a right-anchored reading of an over-long subject
    /// is exactly the forgery a peer would reach for.
    ///
    /// **Every segment it keeps is a principal, checked here**, because each one
    /// becomes an environment value of `/bin/sh -c`. They are not cap-forced: a
    /// presence row's owner, and an `ask`'s node and sid, are the SENDER's
    /// choice, and astream's subject grammar checks structure, not shape: it
    /// refuses wildcards, empty segments and control bytes, but no character
    /// set, so a bidi override or a shell metacharacter arrives intact. Its
    /// 4 KiB bound on a published subject is the broker's, and `--broker` may
    /// name one that predates it; an oversized segment would then make `spawn`
    /// fail `E2BIG` after the rate budget was charged. The presence arm calls
    /// [`crate::glance::Row::parse`]; the others call
    /// [`crate::subject::is_principal`]. A refused subject is not a hit: no
    /// journal line, no budget charged, nothing spawned.
    fn hit(&self, fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Hit> {
        let segs: Vec<&str> = subject.split('/').collect();
        if segs.first() != Some(&"") || segs.get(1) != Some(&"f") || segs.get(2) != Some(&fleet) {
            return None;
        }
        match self {
            Sel::Attention => {
                // One reader of a presence row: the same wall `ls` and
                // `aterm fabric` stand behind, so the three cannot disagree.
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
                // literal owner `p`, the watched principal, and the sender.
                if segs.len() != 8
                    || segs[3] != "in"
                    || segs[4] != "p"
                    || segs[5] != p
                    || segs[7] != "ask"
                {
                    return None;
                }
                // The sender is cap-forced (a node, or a person under a ring of
                // its own) but still checked: it becomes `$ATERM_NOTIFY_NODE`.
                if !crate::subject::is_principal(segs[6]) {
                    return None;
                }
                let (parsed, _) = Body::decode(body);
                // The asking session, as its node attests it in the body; any
                // other shape is the empty string, never the raw token.
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
                // A halt the bridges apply, by their own rule
                // ([`crate::subject::parse_halt`]): a human's row. An `a-`
                // service's or a node's `state=on` holds no session anywhere,
                // and waking someone for it reports a stop that never happened.
                let row = crate::subject::parse_halt(fleet, subject, body)?;
                // The halting principal, same wall. `fleet/<who>/halt` is
                // cap-forced for a human's own grant, but a fleet cap that ends
                // in `>` is not, and this segment becomes `$ATERM_NOTIFY_WHO`.
                if !crate::subject::is_principal(row.human) {
                    return None;
                }
                // A halt coming OFF is good news, and good news does not wake
                // anybody at 3 a.m.
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
/// Raw, not decoded: [`crate::glance::Row::parse`] splits a body on
/// whitespace, so a token is one word, and `standing` keeps it on a line.
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
    /// The node the record is about, when the subject names one (for an `ask`,
    /// the asking node).
    pub node: String,
    /// The principal it is about (a presence row's owner, the person an ask is
    /// addressed to, the human who halted).
    pub who: String,
    /// The asking session, for an `ask`: with [`Hit::node`], the
    /// `to=@<sid>@<node>` an answer goes to.
    pub sid: String,
    /// The free text, flattened and capped. UNTRUSTED, and it never becomes a
    /// shell word.
    pub text: String,
}

/// The subject, as an environment value. Every segment is already a
/// principal, but it is flattened and capped here too, so the bound on what
/// `exec` puts in an environment block is [`TEXT_MAX`], not a property of the
/// parse.
fn subject_env(subject: &str) -> String {
    one_line(subject)
}

/// Flatten untrusted text to one line of printable ASCII, capped at
/// [`TEXT_MAX`]. The command may echo it at a terminal, so a raw `ESC` or `CR`
/// from a cap holder must not survive.
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
///
/// THE CURSORS AND THE JOURNAL ARE ONE LOG'S. Both are bus offsets, and an
/// offset names a record in one log only: pointed at a log that was deleted
/// and recreated, repaired, restored, or another host's, a `cur/` past its
/// head pages to an empty "head" and subscribes beyond every escalation below
/// it, and a journal verdict keyed by a bare offset skips a new record that
/// reuses it. So they are filed by the log's identity ([`Store::for_log`]):
/// the root's are the log `notify/log` names (the first one this notifier
/// read under an identity — the files from before identity were that log's),
/// and any other log's live in `notify/logs/<identity>/`, where a log this
/// notifier has never read starts the way a first run does, from `--since`.
/// The rate budget and the `suppressed` count are the human's, not a log's,
/// and stay at the root.
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

    /// The store whose `cur/` and `journal` are the log `log`'s
    /// ([`Store`]'s doc): this root for a broker that predates identity, for
    /// the log the root's `log` file names, and — recorded there — for the
    /// first identity read; `logs/<identity>/` for any other.
    fn for_log(&self, log: Option<astream_broker::LogIdentity>) -> io::Result<Store> {
        let Some(log) = log else {
            return Ok(self.clone());
        };
        let name = log.to_string();
        let path = self.root.join("log");
        // Only an absent identity file permits the first claim. An unreadable
        // or damaged witness cannot turn the existing cursors and journal into
        // another log's state. A dangling symlink is an existing, unreadable
        // witness too, not an absent file to replace.
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
            Some(_) => {
                let root = self.root.join("logs").join(&name);
                std::fs::create_dir_all(root.join("cur"))?;
                Ok(Store { root })
            }
        }
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

/// TEST-ONLY fault injection, armed by `$ATERM_LINK_NOTIFY_FAULT`.
/// [`Fault::from_env`] reads it through `aterm_types::dev_seam!`, which a
/// release build compiles to `None`. The gate is required, as for
/// `bridge::Fault`: the variable is on no env deny list (it is inherited
/// through `fabric_launch::filter_child_env` on purpose), and [`Fault::fire`]
/// kills the notifier — the fabric's only outbound path — between a command
/// and its journal line.
///
/// The in-doubt window (command exit to the `fired` line) is microseconds
/// wide, so instead of racing it from outside the process kills ITSELF there —
/// one shot, with the marker in the state dir because the process that would
/// remember is the one that dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// Die after the command has run and before the journal says it did.
    KillAfterExec,
}

impl Fault {
    fn from_env(store: &Store) -> Self {
        // A released binary honours no fault (see the type's doc).
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

    /// Die HERE, running no destructors — by `SIGKILL` through the `aterm-uds`
    /// cordon, for the reasons the bridge's twin gives: not `exit` (it would run
    /// destructors), not a raw `kill(2)` (it would put `unsafe` in this module),
    /// and not `abort` (its `SIGABRT` made macOS write a crash report per fault).
    fn fire(self, store: &Store) -> ! {
        let _ = std::fs::write(store.root.join("fault-fired"), b"1\n");
        eprintln!("aterm-link notify: ATERM_LINK_NOTIFY_FAULT={self:?}: dying");
        aterm_uds::process::kill_self_now()
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
    /// `attention` rows that republished the escalation a human was already
    /// told of on that subject: no command, no journal line, no budget.
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
    reader_log: Vec<Option<astream_broker::LogIdentity>>,
    journal: Vec<String>,
    /// The log in hand's `standing` ([`Store::standing`]): per presence
    /// subject, the `attention=` token a human was last told of.
    standing: Vec<(String, String)>,
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
            standing: store.standing(),
            stamps: store.stamps(),
            suppressed: store.suppressed(),
            log_store: store.clone(),
            log_now: None,
            reader_log: vec![None; cfg.on.len()],
            store,
            cfg,
            fault,
            counts: Counts::default(),
        })
    }

    /// One broker connection with every capability attached, every exchange on
    /// it bounded by [`ACK_DEADLINE`]: a wedged broker ends `--once` with an
    /// error, not a cron run that never exits.
    fn connect(&self) -> io::Result<Conn> {
        let (mut conn, _closer) =
            transport::connect(&self.cfg.transport, &self.cfg.broker, ACK_DEADLINE)?;
        transport::attach_cap_files(&mut conn, &self.cfg.cap_files)?;
        Ok(conn)
    }

    /// Read and write cursors and verdicts as the log `log`'s from here on
    /// ([`Store::for_log`]), loading its journal when that is another log
    /// than the one in hand. Said once on stderr: the human reading it learns
    /// why escalations already journalled are not skipped on this log.
    fn scope_to(&mut self, log: Option<astream_broker::LogIdentity>) -> io::Result<()> {
        if log == self.log_now {
            return Ok(());
        }
        let scoped = self.store.for_log(log)?;
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

    /// THE GRANTS, CHECKED ONCE, BEFORE ANYTHING RUNS. A cap file that cannot
    /// be read, a ring the broker refuses, or a selector whose filter no
    /// attached grant covers is a configuration that can never succeed — a
    /// follow-mode reader would retry it every few seconds for ever and a
    /// `--once` would stop at it before the selectors after it ran — so it is
    /// a refusal naming what is missing. Only a PERMANENT answer
    /// ([`astream_broker::BrokerRefusal::is_permanent`]) refuses: a broker that
    /// cannot be reached, or one that refuses for now, is left to the readers'
    /// own back-off, which is how this notifier outlives a broker restart.
    ///
    /// # Errors
    ///
    /// What is missing, in words.
    fn preflight(&self) -> Result<(), String> {
        for path in &self.cfg.cap_files {
            crate::bridge::read_cap_file(path).map_err(|e| format!("--cap-file {path}: {e}"))?;
        }
        let permanent = |e: &io::Error| {
            astream_broker::broker_refusal(e)
                .is_some_and(astream_broker::BrokerRefusal::is_permanent)
        };
        let Ok((mut conn, _closer)) =
            transport::connect(&self.cfg.transport, &self.cfg.broker, ACK_DEADLINE)
        else {
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
    /// Written only when it changes, so a republished row writes nothing.
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
        self.log_store.set_journal(&self.journal)
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
        // `attention` IS A LAST-VALUE FACE (the module doc): what the row says
        // of its subject's escalation decides before anything else whether it
        // is news. A clear re-arms the subject; a raise of ANOTHER value means
        // the one standing there is not standing any more, told or not.
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
            // Terminal: this offset has had its answer. Never fired twice, and
            // never re-offered after the window rolls. A replay of a TOLD
            // escalation puts it back as standing — the re-scan of a page a
            // crash interrupted can deliver its subject's earlier clear first.
            Some("fired" | "dropped") => {
                if let Some(token) = raised.as_deref().filter(|_| self.told(off)) {
                    self.set_standing(subject, Some(token))?;
                }
                self.counts.skipped += 1;
                return Ok(());
            }
            // IN DOUBT — a crash between the spawn and the completion. Fire
            // again and say so; see the module doc for why this direction.
            Some(_) => true,
            // THE ESCALATION THE HUMAN WAS ALREADY TOLD OF, republished — an
            // attach, a reconnect, an update's successor, a phase change, the
            // `exited` row. Not news: no command, no journal line, no budget.
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
        // CLEARED ONLY WHEN THE COMMAND WAS TOLD, and `rc=0` is the only verdict
        // that says so: `spawn-failed` delivered nothing, and `rc=<n>`,
        // `signalled`, `timeout` and `wait-failed` may not have pushed. The
        // count rides to the next `rc=0` — told twice, never untold.
        if carried > 0 && verdict == "rc=0" {
            self.suppressed -= carried;
            self.store.set_suppressed(self.suppressed)?;
        }
        // AND ONLY A TOLD ESCALATION STANDS, after its `fired` line: a crash
        // between the two is the replay above, which reads the line back.
        if let Some(token) = raised.as_deref().filter(|_| verdict == "rc=0") {
            self.set_standing(subject, Some(token))?;
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
            // `max = 0` is the broker's head query (R2).
            Since::Head => conn.fetch(0, &sel.filter(&self.cfg.fleet), 0)?.1 .1,
        })
    }

    /// Catch up to the head, once, and stop. This is the whole of `--once`, and
    /// it is also what a follow-mode reader's first subscription replays.
    fn catch_up(&mut self) -> io::Result<()> {
        let mut conn = self.connect()?;
        let log = log_of(&mut conn, &self.cfg)?;
        self.scope_to(log)?;
        for sel in self.cfg.on.clone() {
            let filter = sel.filter(&self.cfg.fleet);
            let key = sel.key();
            let mut from = self.start_offset(&mut conn, &sel)?;
            loop {
                let (rows, (next, head)) = conn.fetch(from, &filter, PAGE)?;
                for (off, subject, body) in rows {
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

    /// Follow the bus until something kills us. One subscription per selector,
    /// each on its own connection and its own thread, all feeding one executor —
    /// because the command must run one at a time and the journal has one writer.
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
            match notice {
                Notice::Log(idx, log) => {
                    if let Some(slot) = self.reader_log.get_mut(idx) {
                        *slot = log;
                    }
                }
                Notice::Record(idx, off, subject, body) => {
                    let Some(sel) = self.cfg.on.get(idx).cloned() else {
                        continue;
                    };
                    self.scope_to(self.reader_log[idx])?;
                    self.on_record(&sel, off, &subject, &body)?;
                    self.log_store.set_cursor(&sel.key(), off + 1)?;
                }
                Notice::Checkpoint(idx, next) => {
                    if let Some(key) = self.cfg.on.get(idx).map(Sel::key) {
                        self.scope_to(self.reader_log[idx])?;
                        self.log_store.set_cursor(&key, next)?;
                    }
                }
            }
        }
        // Every reader gave up, which only happens when they all failed to
        // reconnect for good. Say so rather than exiting 0.
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "every subscription ended",
        ))
    }
}

/// One selector's reader thread: bounded catch-up, then subscribe from the
/// page's resume cursor, and on any failure back off and do it again. Every
/// selector has its own reader, so one deep backlog or slow notification does
/// not delay the other selectors' subscriptions.
///
/// It re-reads the cursor from DISK on every reconnect and never resumes below
/// what it has already forwarded, so a reconnect replays at most the records the
/// executor had not finished with — which the journal then dedups. The paged
/// scan is what moves the durable cursor across records this selector does NOT
/// match: a `Fetch` page's `next` is the offset after the last record the
/// broker scanned, and the per-page `Checkpoint` persists it. A subscription
/// reports only matches, so a sparse selector (`halt`, `ask:<p>`) subscribed
/// straight from a stale cursor would restart from its last match after every
/// restart. (Broker memory is not the reason: astream's catch-up copies the log
/// a bounded `TAIL_CHUNK` at a time.)
fn reader(idx: usize, sel: &Sel, cfg: &Config, root: &Path, tx: &mpsc::SyncSender<Notice>) {
    let filter = sel.filter(&cfg.fleet);
    let key = sel.key();
    let mut seen: Option<u64> = None;
    // The log the offsets in `seen` were read on: a reconnect to ANOTHER log
    // never resumes from them ([`Store::for_log`]).
    let mut seen_on: Option<PathBuf> = None;
    // Reset only by a connection that stayed up, so a broker that accepts and
    // then refuses or drops is redialled under the back-off, not every 100 ms.
    let mut backoff = Backoff::default();
    loop {
        let wait = match connect_for_reader(cfg, root, &key, &filter, &mut seen, &mut seen_on) {
            Ok((mut conn, closer, mut from, log)) => {
                if tx.send(Notice::Log(idx, log)).is_err() {
                    return; // the executor is gone
                }
                backoff.attached(Instant::now());
                let mut closed = false;
                loop {
                    let (rows, (next, head)) = match conn.fetch(from, &filter, PAGE) {
                        Ok(page) => page,
                        Err(e) => {
                            eprintln!("aterm-link notify: {} catch-up ended: {e}", sel.label());
                            closed = true;
                            break;
                        }
                    };
                    for (off, subject, body) in rows {
                        if tx.send(Notice::Record(idx, off, subject, body)).is_err() {
                            return; // the executor is gone
                        }
                        seen = Some(seen.map_or(off + 1, |s| s.max(off + 1)));
                    }
                    if tx.send(Notice::Checkpoint(idx, next)).is_err() {
                        return;
                    }
                    // The checkpoint follows all records from this page in the
                    // zero-buffer handoff. Once accepted, those rows have all
                    // been processed, even if the checkpoint's fsync is pending.
                    seen = Some(seen.map_or(next, |s| s.max(next)));
                    let made_progress = next > from;
                    from = next;
                    if next >= head || !made_progress {
                        break;
                    }
                }
                if !closed {
                    match conn.subscribe(from, &filter) {
                        Ok(mut sub) => {
                            // Following is parking in `recv`: the read bound
                            // the catch-up ran under comes off here, and comes
                            // back as a LIVENESS bound on the broker's first
                            // idle mark — so a link that went silent without a
                            // FIN ends the subscription and is redialled,
                            // instead of parking this reader for good.
                            let _ = closer.set_read_timeout(None);
                            loop {
                                match sub.recv_live(&closer, astream_broker::IDLE_MARK_DEADLINE) {
                                    Ok(Some((off, subject, body))) => {
                                        if tx.send(Notice::Record(idx, off, subject, body)).is_err()
                                        {
                                            return; // the executor is gone
                                        }
                                        seen = Some(seen.map_or(off + 1, |s| s.max(off + 1)));
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

/// Open one reader's connection from the highest of the durable cursor, what
/// this thread has already forwarded, and — only when neither exists —
/// `--since`. A first `--since=head` is persisted before subscribing, so a
/// restart while idle still replays records published after this first start.
fn connect_for_reader(
    cfg: &Config,
    root: &Path,
    key: &str,
    filter: &str,
    seen: &mut Option<u64>,
    seen_on: &mut Option<PathBuf>,
) -> io::Result<(Conn, Closer, u64, Option<astream_broker::LogIdentity>)> {
    let (mut conn, closer) = transport::connect(&cfg.transport, &cfg.broker, ACK_DEADLINE)?;
    transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
    // THE CURSOR IS THIS LOG'S ([`Store::for_log`]), and so is what this
    // thread forwarded: offsets read on another log name nothing here.
    let log = log_of(&mut conn, cfg)?;
    let store = Store {
        root: root.to_path_buf(),
    }
    .for_log(log)?;
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

/// The identity of the log `conn` reaches: what its attach's `Hello` answered,
/// or — with no cap file, so no `Hello` went — one asked for now. `None` is a
/// broker that predates identity.
fn log_of(conn: &mut Conn, cfg: &Config) -> io::Result<Option<astream_broker::LogIdentity>> {
    match conn.seen_log_identity() {
        Some(log) => Ok(Some(log)),
        None if cfg.cap_files.is_empty() => conn.log_identity(),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// the command line
// ---------------------------------------------------------------------------

/// Usage, printed to stderr on a usage error (exit 2, aterm's convention).
const USAGE: &str = "\
aterm-link notify — run a command when the fabric needs a human

  aterm-link notify --fleet <F> --broker <ep> --cap-file <path>...
                    --on <attention|ask:<person>|halt>,... --exec <cmd> [options]

  --on <sel>,...         what is worth waking someone for; repeatable.
                         attention: a presence row raising one (node.cap reads it);
                         ask:<person>: an ask a session posted to=<person>, read
                         with that person's cap (`aterm fabric mint-for <person>`);
                         halt: a fleet halt coming on (node.cap reads it). A
                         selector no --cap-file grants is refused, exit 2
  --exec <cmd>           run as `/bin/sh -c <cmd>`; record facts arrive only in
                         $ATERM_NOTIFY_*, never in <cmd>
  --rate <n>/<window>    the budget, e.g. 10/1m, 1/30s, 40/1h (default 10/1m)
  --since head|start|<n> where a selector with no cursor starts (default head)
  --once                 catch up to the head and exit, rather than following
  --exec-timeout <ms>    how long the command may run before it is killed (30000)
  --state <dir>          the state dir; the journal lives in <dir>/notify/
  --tcp                  reach the broker over TCP — PLAINTEXT unless --key-file
  --key-file <path>      the sealed wire's key (needs --tcp and a `sealed` build)

The command runs once per matching offset, deduped across restarts, and for
attention once per escalation: a session's row republished with the attention
it was told of pages nobody; a clear and a raise pages again. A crash during
the exec re-fires it with ATERM_NOTIFY_DUP=1: twice, never zero times.
";

/// `aterm-link notify`.
///
/// # Panics
///
/// Never: every failure is an exit code and a line on stderr.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    // A build without the sealed wire refuses it by name, before the key file
    // is read, rather than retrying a subscribe that can never succeed.
    if let Some(refusal) = transport::sealed_flags_refusal(args) {
        eprintln!("aterm-link notify: --key-file: {refusal}");
        return ExitCode::from(2);
    }
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
        return Err("--on is required".to_string());
    }
    if cfg.exec.is_empty() {
        return Err("--exec is required".to_string());
    }
    cfg.transport = crate::cli::transport_from_flags(
        tcp,
        key_file,
        "aterm-link notify: --tcp without --key-file is PLAINTEXT. Trusted networks only.",
    )?;
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

    /// A BUILD WITHOUT THE SEALED WIRE refuses notify's `--tcp --key-file` by
    /// name in `main`, before `parse` reads the key file (which does not exist
    /// here, so the refusal is the only answer that can name the feature).
    #[cfg(not(feature = "sealed"))]
    #[test]
    fn a_default_build_refuses_the_sealed_wire_by_name() {
        let args: Vec<String> =
            "--fleet f1 --broker 127.0.0.1:7000 --on halt --exec true --tcp --key-file /none/k"
                .split(' ')
                .map(str::to_string)
                .collect();
        let why = transport::sealed_flags_refusal(&args).expect("a default build refuses");
        assert!(why.contains("`sealed` cargo feature"), "{why}");
        let Err(e) = parse(&args) else {
            panic!("a key file that does not exist parsed");
        };
        assert!(e.starts_with("--key-file /none/k: "), "{e}");
    }

    /// A slow `--exec` leaves the consumer unavailable. Readers must stop at
    /// their current row rather than queue broker bodies while it runs. Once
    /// the executor accepts rows, a reader still delivers them in order.
    #[test]
    fn follow_handoff_backpressures_a_stalled_executor_without_losing_order() {
        let (tx, rx) = notice_handoff();
        let first = Notice::Record(0, 41, String::from("first"), vec![1; 1024]);
        let first = match tx.try_send(first) {
            Err(mpsc::TrySendError::Full(row)) => row,
            other => panic!("a stalled executor must leave no queued row: {other:?}"),
        };
        let second = Notice::Record(0, 42, String::from("second"), vec![2; 1024]);
        let producer = std::thread::spawn(move || {
            tx.send(first).expect("executor accepts first row");
            tx.send(second).expect("executor accepts second row");
        });

        // Hang detectors for a producer thread's two sends, each of which
        // lands the moment that thread is scheduled.
        let first = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(matches!(first, Notice::Record(0, 41, subject, _) if subject == "first"));
        let second = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(matches!(second, Notice::Record(0, 42, subject, _) if subject == "second"));
        producer.join().unwrap();
    }

    /// Every subject segment that becomes an environment value is a principal:
    /// a bidi override passes astream's subject grammar, and a sender-chosen
    /// 200 000-byte segment, which a broker without the 4 KiB name bound
    /// passes, would fail `spawn` with `E2BIG` after the budget slot was
    /// charged.
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
            format!("/f/f1/in/p/h-andrew/{huge}/ask"), // the SENDER's node
            format!("/f/f1/in/p/h-andrew/{bidi}/ask"), // a bidi override
            "/f/f1/in/p/h-andrew/NODE/ask".to_string(), // not lowercase
        ] {
            assert_eq!(ask.hit("f1", 7, &bad, b"v=1 t=1"), None, "{}", &bad[..40]);
        }
        // The asking SESSION rides in the body, the node's word: a `from=` that
        // is not an `s-` principal is no sid at all, never the raw token.
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

        // And what does pass is bounded by `TEXT_MAX`.
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
    /// however its tail reads. `ask:<p>` is an ask ADDRESSED TO `<p>` — the
    /// `/f/<F>/in/p/<p>/<node>/ask` a session's `post to=<p> kind=ask` lands on
    /// — and never one `<p>` sent, which reads `…/in/<node>/<sid>/<p>/ask`.
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
            "/f/f1/in/n-a/s-b/h-andrew/ask",   // an ask FROM h-andrew
            "/f/f1/in/p/h-andrew/n-a/s-1/ask", // nine segments, forged tail
            "/f/f1/in/p/h-other/n-a/ask",      // to someone else
            "/f/f1/in/p/h-andrew/n-a/note",    // a different kind
            "/f/f2/in/p/h-andrew/n-a/ask",     // another fleet
            "/f/f1/pub/p/h-andrew/n-a/ask",    // another face
            "/f/f1/in/n-a/h-andrew/n-b/ask",   // no literal `p` owner
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
        // And a halt no bridge applies wakes nobody either: a bridge holds a
        // session for a HUMAN's row alone, so an `a-` service's or a node's
        // `state=on` is a stop that never happened.
        for lane in ["/f/f1/fleet/a-svc/halt", "/f/f1/fleet/n-node/halt"] {
            assert_eq!(
                halt.hit("f1", 4, lane, b"v=1 t=1 state=on reason=stop"),
                None,
                "{lane}"
            );
        }
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
    /// (r3-22). Presence is a last-value face the bridge rewrites on every
    /// attach, reconnect, successor after an update, meta change and exit,
    /// standing `attention=` and all. s-one raises "needs a key": it pages.
    /// Its row republished by an attach (`inc=2`), a phase change and its
    /// `exited` row pages nobody and charges nothing — the negative control:
    /// offset dedup alone ran the command for all four. s-two's identical text
    /// is s-two's own escalation and pages. A clear and the same text again
    /// pages; so does another value, and the first value after it. A restart
    /// remembers what stands. And the re-scan of a page a crash interrupted —
    /// the clear below an escalation already told, then that escalation —
    /// leaves it standing, so its next republish pages nobody.
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

        // THE RE-SCAN of offsets 6 and 7, after a crash before their cursor.
        feed(&mut back, 11, "s-two", "v=1 t=11 attention=-");
        feed(&mut back, 12, "s-two", &format!("v=1 t=12 {key}"));
        feed(&mut back, 11, "s-two", "v=1 t=11 attention=-");
        feed(&mut back, 12, "s-two", &format!("v=1 t=12 {key}"));
        feed(&mut back, 13, "s-two", &format!("v=1 t=13 inc=2 {key}"));
        assert_eq!(fired_offsets(&fired), [1, 5, 7, 9, 10, 12]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **AN ESCALATION NOBODY WAS TOLD OF DOES NOT STAND** (r3-22). Value
    /// dedup must not become the silent drop the module refuses. With a budget
    /// of one, s-one's raise pages and s-two's is rate-dropped: s-two's
    /// republish is offered again, and with room it pages. A command that
    /// answered `rc=3` told nobody, so its escalation's republish runs the
    /// command again; once it answers `rc=0`, the next republish pages nobody.
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
                store.for_log(Some(b)).is_err(),
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
            // Repair the same state in place, then prove A resumes and B starts
            // with its own empty namespace, rather than A's cursor/journal.
            std::fs::remove_file(&path).unwrap();
            store.write("log", &a.to_string()).unwrap();
            let resumed = store.for_log(Some(a)).unwrap();
            assert_eq!(resumed.cursor("attention"), Some(900));
            assert_eq!(resumed.journal(), ["7 fired"]);
            let fresh = store.for_log(Some(b)).unwrap();
            assert_eq!(fresh.cursor("attention"), None);
            assert!(fresh.journal().is_empty());
        }
    }

    /// The cursor is MONOTONE. A cursor that walked backwards would re-offer
    /// offsets the bounded journal has already forgotten.
    /// **ANOTHER LOG IS READ AS A FIRST RUN, AND THE OLD ONE RESUMES** (r2-01).
    /// Each log carries an escalation at offset 0. Run against log A, it fires
    /// and A's cursor and journal say so. Pointed at log B — a recreated
    /// bus.log, or another host's broker — that cursor and that journal name
    /// A's records: B's escalation at the same offset fires all the same,
    /// filed under B. Back on A, A's cursor and journal stand and nothing
    /// fires twice. Negative control: one cursor and one journal for every
    /// log, as before, and B's escalation is skipped as already fired.
    #[cfg(unix)]
    #[test]
    fn a_notifier_files_its_cursor_and_journal_under_the_log_it_read() {
        let dir = scratch("logs");
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let serve = |name: &str| {
            let broker = astream_broker::Broker::open(base.join(format!("{name}.log"))).unwrap();
            let sock = base.join(format!("{name}.sock"));
            let handle = broker.serve(&sock).unwrap();
            let (mut c, _) =
                transport::connect(&Transport::Unix, &sock.to_string_lossy(), ACK_DEADLINE)
                    .unwrap();
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
        let (a, broker_a, _a) = serve("a");
        let (b, broker_b, _b) = serve("b");
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
        let _ = std::fs::remove_dir_all(&dir);
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

    /// Every selector spells a filename and a filter, and a bad one is refused
    /// by name. `--on ask:../../etc` must never become a path.
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
