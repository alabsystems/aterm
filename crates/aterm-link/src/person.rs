// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric mint-for|halt|inbox|post` — the PERSON side of the fabric.
//!
//! ```text
//! aterm fabric mint-for h-<name>|a-<name> [--out <cap>] [--fleet <F>] [--expires <N>d|<N>h]
//! aterm fabric halt  --cap-file <cap> <reason>|off
//! aterm fabric inbox --cap-file <cap> [--peek]
//! aterm fabric post  --cap-file <cap> to=@<sid>@<node> kind=<k> [re=<off>] [--] <text>
//! ```
//!
//! A session speaks through its node's bridge, and its node's cap-forced
//! `<src>` says so: what a person types in an aterm session arrives
//! `trust=agent`, attested by that node. A PERSON speaks for themself, under a
//! cap of their own — §8.2's human ring, [`crate::enable::person_grants`],
//! minted by `aterm fabric mint-for h-<name>` on this machine, whose fabric
//! root holds the mint secret — and the broker binds every publish to the
//! principal the cap names. That is what these four verbs are:
//!
//! * `mint-for` mints that ring under `<root>/mint.secret`
//!   ([`mint_for`]): the cap these verbs take, and the one `aterm link notify
//!   --on ask:<person>` reads its mail with. It refuses anything but an `h-`
//!   or `a-` id, and a secret that is missing, short or readable by another
//!   user (the one reader, [`crate::transport::read_mint_secret`]). It never
//!   prints the secret; `--out` writes the cap 0600, atomically, never over a
//!   DIFFERENT file and never through a symlink, and makes an IDENTICAL file
//!   already there 0600 before it says so; without `--out` the lines go to
//!   stdout (like `aterm link mint`) and the guidance to stderr. `--expires
//!   30d` stamps `exp=` into every grant, and the broker refuses the ring from
//!   that instant on; without it the ring lasts until the secret is rotated.
//!
//! * A HUMAN's `halt` publishes the retained row `/f/<F>/fleet/<person>/halt`
//!   (`v=1 t= state=on reason=<pct>`, or `state=off`). Every bridge applies it to every
//!   session it hosts — `hold=1 origin=fleet`, `ERR halted` to every
//!   PTY-reaching verb — re-reads it at every attach, and answers it on its
//!   node's `ack` face. `off` withdraws THIS person's halt; another person's
//!   still holds. Only an `h-` human halts: a bridge applies no other
//!   principal's row, so an `a-` service's cap carries no halt grant and this
//!   verb refuses one by name. Nothing else lifts a fleet hold: the Owner's
//!   `hold <sid> off` is `ERR denied` for one, and `aterm fabric off` leaves
//!   the row on the bus for the next `on` to re-apply.
//! * `inbox` reads the person's mail lane `/f/<F>/in/p/<person>/>` — every
//!   node's `post to=<person>`, the receipts of what the person sent, and the
//!   bridges' `undeliverable`/`expired` notices — as a durable consumer group,
//!   `/f/<F>/cur/p/<person>/inbox`: from where the last `inbox` left off to the
//!   broker's head as it starts, then the group is COMMITTED past everything
//!   it read, mail or none (never before it is printed; one commit record per
//!   run; `--peek` commits nothing). The cursor lives in the broker, so any
//!   host the person reads from agrees on what is unread.
//! * `post` publishes `/f/<F>/in/<node>/<sid>/<person>/<kind>` — an answer to
//!   an ask (`re=<off>`, which settles the asker's deadline: the reply is from
//!   the principal the ask went to), a note, a report or a task. The address is
//!   always the explicit `@<sid>@<node>` that `inbox` prints: a person has no
//!   bridge, so no roster pins a bare sid.
//!
//! ## The sequence
//!
//! A person has no state dir, so a publish is keyed the way astream's own `asb
//! pub` keys one: the producer sequence is the wall clock in nanoseconds, fresh
//! per invocation and below astream's reserved ack half. One invocation
//! publishes once and never retries, so it never re-uses a sequence; a broker
//! that answers `deduped` (a clock that stepped back onto an earlier run's
//! sequence) appended NOTHING, and that is reported as the failure it is, never
//! as a success. A run whose answer was lost (the broker died after the append)
//! may have landed: `halt` and `halt off` are retained rows, so re-running one
//! is harmless; a re-run `post` is a second message.
//!
//! ## What it does not do
//!
//! It does not count who held a halt: every bridge answers on its node's `ack`
//! face (`re=<off> state=held`), asynchronously, and a count read the instant
//! after the publish would be a race. `aterm fabric` shows each session's
//! `hold=`. And it holds no deadline for an ask a person sends: that sweep is
//! a bridge's, and a person has none.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use astream_broker::Event;

use crate::body::Body;
use crate::bridge::{read_cap_file, Config};
use crate::enable::{is_node_id, write_atomic, Paths};
use crate::fabric::IO_TIMEOUT;
use crate::render::{safe, TEXT_CAP};
use crate::transport::{self, Closer, Conn};

/// The kinds a person may `post`. Not `ack` (a receipt is a recipient bridge's
/// answer to `inbox seen`, carrying a verdict), not `control`, and not
/// `expired`/`undeliverable` (a bridge RECORDS those).
pub const PERSON_KINDS: [&str; 5] = ["ask", "answer", "task", "report", "note"];

/// What a person verb was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// `halt <reason>` (`Some`) or `halt off` (`None`).
    Halt(Option<String>),
    /// `inbox [--peek]`.
    Inbox {
        /// `--peek`: print, commit nothing.
        peek: bool,
    },
    /// `post to=@<sid>@<node> kind=<k> [re=<off>] <text>`.
    Post {
        /// The addressed session.
        sid: String,
        /// The node hosting it.
        node: String,
        /// One of [`PERSON_KINDS`].
        kind: String,
        /// The offset this answers.
        re: Option<u64>,
        /// The message.
        text: String,
    },
}

/// One person verb: what to do, and the cap files that say who is doing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonCmd {
    /// The act.
    pub act: Act,
    /// `--cap-file <path>`, repeatable: the person's ring.
    pub cap_files: Vec<String>,
}

/// Parse `aterm fabric <verb> …` for `verb` one of `halt`, `inbox`, `post`.
/// `Ok(None)` is a request for help.
///
/// # Errors
///
/// An unknown flag, a flag without its value, no `--cap-file`, or the verb's
/// own arguments missing or malformed.
pub fn parse(verb: &str, rest: &[String]) -> Result<Option<PersonCmd>, String> {
    let mut cap_files = Vec::new();
    let mut peek = false;
    let mut words: Vec<String> = Vec::new();
    let mut literal_at = None;
    let mut it = rest.iter();
    while let Some(word) = it.next() {
        match word.as_str() {
            "-h" | "--help" if words.is_empty() => return Ok(None),
            "--cap-file" => cap_files.push(
                it.next()
                    .cloned()
                    .ok_or_else(|| "--cap-file needs a value".to_string())?,
            ),
            "--peek" if verb == "inbox" => peek = true,
            // Everything after `--` is the message, flags and all.
            "--" if verb != "inbox" => {
                literal_at = Some(words.len());
                words.extend(it.by_ref().cloned());
                break;
            }
            flag if flag.starts_with('-') && words.is_empty() => {
                return Err(format!(
                    "unknown flag {} for `{verb}` (--cap-file{})",
                    safe(flag, 64),
                    if verb == "inbox" { ", --peek" } else { "" }
                ));
            }
            other => words.push(other.to_string()),
        }
    }
    if cap_files.is_empty() {
        return Err(format!(
            "`{verb}` needs --cap-file <cap>: a person's cap, from `aterm fabric mint-for \
             h-<name> --out <cap>` on this machine (its fabric root holds the mint secret)"
        ));
    }
    let act = match verb {
        "halt" => match words.as_slice() {
            [] => {
                return Err(
                    "halt needs a reason (`halt --cap-file <cap> main is broken`) or `off`"
                        .to_string(),
                )
            }
            [off] if off == "off" => Act::Halt(None),
            reason => Act::Halt(Some(reason.join(" "))),
        },
        "inbox" => {
            if let Some(w) = words.first() {
                return Err(format!("inbox takes no argument; `{}` is one", safe(w, 64)));
            }
            Act::Inbox { peek }
        }
        _ => parse_post(&words, literal_at)?,
    };
    Ok(Some(PersonCmd { act, cap_files }))
}

/// `to=@<sid>@<node> kind=<k> [re=<off>] <text…>` — the three keys first, in
/// any order; the first word that is not one of them starts the text, and an
/// explicit `--` makes every word after it literal text.
fn parse_post(words: &[String], literal_at: Option<usize>) -> Result<Act, String> {
    let (mut to, mut kind, mut re) = (None, None, None);
    let mut text_at = literal_at.unwrap_or(words.len());
    for (i, w) in words.iter().enumerate().take(text_at) {
        if let Some(v) = w.strip_prefix("to=").filter(|_| to.is_none()) {
            to = Some(v.to_string());
        } else if let Some(v) = w.strip_prefix("kind=").filter(|_| kind.is_none()) {
            kind = Some(v.to_string());
        } else if let Some(v) = w.strip_prefix("re=").filter(|_| re.is_none()) {
            let n = v.strip_prefix('@').unwrap_or(v);
            re = Some(
                n.parse::<u64>()
                    .map_err(|_| format!("re={} is not a bus offset", safe(v, 64)))?,
            );
        } else {
            text_at = i;
            break;
        }
    }
    let to = to.ok_or_else(|| {
        "post needs to=@<sid>@<node>: the address `aterm fabric inbox` prints".to_string()
    })?;
    let (sid, node) = to
        .strip_prefix('@')
        .and_then(|r| r.split_once('@'))
        .filter(|(sid, node)| {
            sid.starts_with("s-") && crate::subject::is_principal(sid) && is_node_id(node)
        })
        .ok_or_else(|| {
            format!(
                "to={} is not @<sid>@<node> (a person has no bridge to resolve a bare sid, so \
                 the node is always named)",
                safe(&to, 128)
            )
        })?;
    let kind = kind.unwrap_or_else(|| "note".to_string());
    if !PERSON_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "kind={} — a person posts one of {}",
            safe(&kind, 32),
            PERSON_KINDS.join(", ")
        ));
    }
    let text = words[text_at..].join(" ");
    if text.is_empty() {
        return Err("post needs a message after the address".to_string());
    }
    Ok(Act::Post {
        sid: sid.to_string(),
        node: node.to_string(),
        kind,
        re,
        text,
    })
}

/// Run one person verb against the broker `cfg` (the `[fabric] command`'s)
/// names, as the person `cmd`'s cap files are minted for.
#[must_use]
pub fn main(verb: &str, cmd: &PersonCmd, cfg: &Config) -> ExitCode {
    let person = match person_of(&cmd.cap_files, &cfg.fleet) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric {verb}: {e}");
            return ExitCode::from(2);
        }
    };
    // ONLY A HUMAN HALTS THE FLEET: every bridge applies a halt row under an
    // `h-` principal alone (§5.3), so a halt published as an `a-` service
    // would hold nothing while this verb said the fleet was held. Its ring
    // carries no halt grant ([`crate::enable::person_grants`]); this says so
    // by name instead of as the broker's refusal.
    if matches!(cmd.act, Act::Halt(_)) && !person.starts_with("h-") {
        eprintln!(
            "aterm fabric {verb}: {} is not a human: only an `h-<name>` halts the fleet (every \
             bridge applies `/f/<F>/fleet/h-<name>/halt` alone); an agent asks one with `post \
             to=h-<name> kind=ask`",
            safe(&person, 64)
        );
        return ExitCode::from(2);
    }
    let caps = cmd.cap_files.join(" ");
    let done = match &cmd.act {
        Act::Halt(reason) => halt(cfg, cmd, &person, reason.as_deref()),
        Act::Inbox { peek } => inbox(cfg, cmd, &person, *peek, &mut io::stdout(), LIVE),
        Act::Post {
            sid,
            node,
            kind,
            re,
            text,
        } => post(
            cfg,
            cmd,
            &person,
            (sid.as_str(), node.as_str(), kind.as_str(), *re),
            text,
        ),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        // This side's stdout, not the broker: said without the broker's name.
        Err(e) if e.get_ref().is_some_and(|inner| inner.is::<Unprinted>()) => {
            eprintln!("aterm fabric {verb}: {e}");
            ExitCode::FAILURE
        }
        Err(e) => {
            if astream_broker::broker_refusal(&e)
                .is_some_and(astream_broker::BrokerRefusal::is_permanent)
            {
                eprintln!(
                    "aterm fabric {verb}: the broker at {} refused it: {e} — {caps} must be \
                     {person}'s ring, minted under this fleet's secret (`aterm fabric mint-for \
                     {person}` on this machine)",
                    safe(&cfg.broker, 256)
                );
            } else {
                eprintln!(
                    "aterm fabric {verb}: the broker at {}: {e}",
                    safe(&cfg.broker, 256)
                );
            }
            ExitCode::FAILURE
        }
    }
}

/// An inbox whose mail stdout refused (the reader went away, a full disk): nothing is marked
/// read, because the mail it could not print was not read. `peek`: a `--peek`, which marks
/// nothing anyway, so the line does not say so.
#[derive(Debug)]
struct Unprinted {
    error: io::Error,
    peek: bool,
}

impl std::fmt::Display for Unprinted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the inbox could not be printed ({})", self.error)?;
        if self.peek {
            Ok(())
        } else {
            f.write_str(", so nothing was marked read")
        }
    }
}

impl std::error::Error for Unprinted {}

/// The ONE person the cap files bind writes to, on `fleet`: the principal of
/// every read-write grant whose principal is a person ([`is_person_id`]). A
/// node's ring (node.cap passed by mistake) names no person.
///
/// # Errors
///
/// An unreadable cap file, no person's grant, grants for two persons, or a
/// person's grant on another fleet.
pub fn person_of(cap_files: &[String], fleet: &str) -> Result<String, String> {
    use astream_cap::{Grant, Mode};
    let mut people = BTreeSet::new();
    for path in cap_files {
        for cap in read_cap_file(path).map_err(|e| e.to_string())? {
            let Ok(g) = Grant::parse(&cap.grant) else {
                continue;
            };
            let Some(p) = g
                .principal
                .filter(|p| g.mode == Mode::ReadWrite && is_person_id(p))
            else {
                continue;
            };
            let on = g
                .filter
                .strip_prefix("/f/")
                .and_then(|f| f.split('/').next());
            if on != Some(fleet) {
                return Err(format!(
                    "{path} binds {} on fleet {}, and this host's fabric is on fleet {fleet}",
                    safe(&p, 64),
                    safe(on.unwrap_or("-"), 64)
                ));
            }
            people.insert(p);
        }
    }
    let mut it = people.into_iter();
    match (it.next(), it.next()) {
        (Some(p), None) => Ok(p),
        (None, _) => Err(format!(
            "{} holds no person's grant (a node's cap speaks as its node, not as a person): \
             `aterm fabric mint-for h-<name> --out <cap>` on this machine mints one",
            cap_files.join(" ")
        )),
        (Some(a), Some(b)) => Err(format!(
            "{} bind two persons ({a}, {b}, …): pass one person's cap",
            cap_files.join(" ")
        )),
    }
}

/// A broker connection with the person's caps attached, every exchange
/// bounded by [`IO_TIMEOUT`].
fn connect(cfg: &Config, cmd: &PersonCmd) -> io::Result<(Conn, Closer)> {
    connect_within(cfg, cmd, IO_TIMEOUT)
}

/// [`connect`] with every exchange bounded by `request`.
fn connect_within(cfg: &Config, cmd: &PersonCmd, request: Duration) -> io::Result<(Conn, Closer)> {
    let (mut conn, closer) = transport::connect(&cfg.broker, request)?;
    transport::attach_cap_files(&mut conn, &cmd.cap_files)?;
    Ok((conn, closer))
}

/// What an inbox waits on: each `request` (the dial, every request and reply)
/// and its `subscription`'s silence (the catch-up's liveness bound).
#[derive(Clone, Copy, Debug)]
struct Bounds {
    request: Duration,
    subscription: Duration,
}

/// The operator's bounds: a request answers within [`IO_TIMEOUT`], and a
/// catch-up scan, which a broker marks while it reads, may stay silent up to
/// [`astream_broker::IDLE_MARK_DEADLINE`] — longer than a request may take, so a
/// long scan is read to its end.
const LIVE: Bounds = Bounds {
    request: IO_TIMEOUT,
    subscription: astream_broker::IDLE_MARK_DEADLINE,
};
const _: () = assert!(LIVE.subscription.as_millis() > LIVE.request.as_millis());

/// Publish once, as `person`, under a fresh sequence (the module doc's
/// rule), and answer the record's offset.
fn publish(
    cfg: &Config,
    cmd: &PersonCmd,
    person: &str,
    subject: &str,
    body: &[u8],
) -> io::Result<u64> {
    let seq = aterm_time::since_epoch()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .filter(|&ns| ns > 0 && ns < astream_broker::ACK_SEQ_BASE)
        .ok_or_else(|| io::Error::other("the wall clock reads before 1970: no fresh sequence"))?;
    let (mut conn, _closer) = connect(cfg, cmd)?;
    let (off, deduped) = conn.publish(astream_cap::producer_id_of(person), seq, subject, body)?;
    if deduped {
        return Err(io::Error::other(format!(
            "the broker deduped it at @{off} and appended nothing: sequence {seq} was already \
             used under {person} (a clock that stepped back) — run it again"
        )));
    }
    Ok(off)
}

/// `halt <reason>` / `halt off`.
fn halt(cfg: &Config, cmd: &PersonCmd, person: &str, reason: Option<&str>) -> io::Result<()> {
    let subject = crate::subject::person_halt(&cfg.fleet, person);
    let mut body = format!(
        "v=1 t={} state={}",
        crate::now_ms(),
        if reason.is_some() { "on" } else { "off" }
    );
    if let Some(r) = reason {
        body.push_str(&format!(" reason={}", crate::pct::encode(r)));
    }
    let off = publish(cfg, cmd, person, &subject, body.as_bytes())?;
    let cap = cmd.cap_files.join(" --cap-file ");
    match reason {
        Some(r) => println!(
            "{person}'s halt is ON at @{off} (reason={}): every session on fleet {} is held \
             (`hold=1 origin=fleet`) as each node reads it. `aterm fabric halt --cap-file {cap} \
             off` withdraws it",
            safe(r, 128),
            cfg.fleet
        ),
        None => println!(
            "{person}'s halt is OFF at @{off}: each node releases the sessions it held for \
             {person} (a halt another person set still holds them; `aterm fabric` names it)"
        ),
    }
    Ok(())
}

/// `post to=@<sid>@<node> kind=<k> [re=<off>] <text>`.
fn post(
    cfg: &Config,
    cmd: &PersonCmd,
    person: &str,
    (sid, node, kind, re): (&str, &str, &str, Option<u64>),
    text: &str,
) -> io::Result<()> {
    let subject = crate::subject::post_lane(&cfg.fleet, node, sid, person, kind);
    let mut body = Body::new(crate::now_ms());
    body.re = re;
    body.text = text.to_string();
    let off = publish(cfg, cmd, person, &subject, &body.encode())?;
    println!(
        "OK off={off} to=@{sid}@{node} kind={kind}{} from={person}",
        re.map_or_else(String::new, |r| format!(" re={r}"))
    );
    Ok(())
}

/// `inbox [--peek]`: the person's unread mail, then the group committed past
/// everything it read — mail or none. Printed as it arrives, so a deep backlog
/// costs one record of memory.
///
/// * WHERE IT ENDS: the broker's head as the inbox starts. A mark or delivery
///   at or past that head ends the read; that delivery belongs to the next
///   inbox and is not printed. A progress mark below the captured head is
///   never taken for "that was all", and a busy matching tail needs no mark.
/// * HOW LONG IT MAY WAIT: a catch-up over a long log can scan for longer than
///   a request's bound before its first matching record, so the subscription
///   is held to its own (`bounds`; [`LIVE`] for the operator) — a liveness
///   bound, which a broker that marks (checked first) keeps while it scans;
///   past it the link is gone, and nothing is marked read.
/// * WHAT IT MARKS READ: everything below the captured head, not only up to the
///   last message printed. One commit record per run, so the next inbox scans
///   only what arrived since; an inbox that committed only after mail would
///   re-scan the whole bus since the person's last message, every run.
///   `--peek` commits nothing. A new commit connection must identify exactly
///   the log, including its generation, on which the snapshot was read.
/// * WHEN IT MARKS NOTHING: mail `out` refused ([`Unprinted`]) was not read,
///   whether its reader went away (`| head -1`) or the disk filled.
fn inbox(
    cfg: &Config,
    cmd: &PersonCmd,
    person: &str,
    peek: bool,
    out: &mut dyn io::Write,
    bounds: Bounds,
) -> io::Result<()> {
    let group = crate::subject::person_inbox_group(&cfg.fleet, person);
    let filter = crate::subject::person_mail_filter(&cfg.fleet, person);
    let (mut conn, closer) = connect_within(cfg, cmd, bounds.request)?;
    // THE END OF THE MAIL IS THE BROKER'S MARK. A broker that sends none
    // cannot say "that was all", and an inbox that waited for silence instead
    // would be a timing guess.
    if !conn.idle_marks()? {
        return Err(io::Error::other(
            "this broker sends no catch-up mark, so it cannot say where the mail ends: it is an \
             older astream (`aterm fabric on` restarts a broker its update replaced)",
        ));
    }
    let read_log = if peek {
        None
    } else {
        inbox_log_identity(&mut conn)?
    };
    // The head query (`max = 0`): the snapshot excludes records at this head,
    // even if later appends keep the subscription continuously busy.
    let (_, (_, end)) = conn.fetch(0, &filter, 0)?;
    let offset = crate::fabric::local_offset();
    eprintln!(
        "aterm fabric inbox — {person}'s mail on fleet {} ({}){}",
        cfg.fleet,
        crate::fabric::zone_label(offset),
        if peek {
            "; --peek: nothing is marked read"
        } else {
            ""
        }
    );
    let mut sub = conn.subscribe_group(&group, &filter)?;
    // A broker that already answered and hung up leaves nothing to bound; its mail
    // is still read below, and its close ends the inbox in words.
    closer.set_read_timeout(Some(bounds.subscription))?;
    let cap = cmd.cap_files.join(" --cap-file ");
    let mut count = 0usize;
    loop {
        match sub
            .recv_event()
            .map_err(|e| went_silent(e, bounds.subscription))?
        {
            None => {
                return Err(io::Error::other(
                    "the broker closed the inbox before it caught up; nothing was marked read",
                ))
            }
            Some(Event::Mark { next, .. }) if next >= end => break,
            // A mark part-way through the scan: the broker is still reading.
            Some(Event::Mark { .. }) => {}
            Some(Event::Delivery {
                offset: off,
                subject,
                body,
            }) => {
                if off >= end {
                    break;
                }
                let (b, _) = Body::decode(&body);
                let (re, verdict, dl) = (b.re, b.verdict.clone(), b.dl);
                let t = crate::fabric::traffic_of_body(&cfg.fleet, off, &subject, b);
                let mut line = format!(
                    "@{off}  {}  {}  from={} trust={}",
                    crate::fabric::clock(t.t_ms, crate::now_ms(), offset.unwrap_or(0)),
                    t.kind,
                    t.from,
                    t.trust
                );
                if let Some(re) = re {
                    line.push_str(&format!(" re={re}"));
                }
                if let Some(v) = verdict {
                    line.push_str(&format!(" verdict={}", safe(&v, 16)));
                }
                if let Some(dl) = dl {
                    line.push_str(&format!(" dl={dl}"));
                }
                line.push_str("  text=");
                line.push_str(&safe(&t.text, TEXT_CAP));
                line.push('\n');
                // The reply, spelled out, for the kinds that wait for one.
                if matches!(t.kind.as_str(), "ask" | "task") && t.from.contains('@') {
                    line.push_str(&format!(
                        "    answer: aterm fabric post --cap-file {cap} to=@{} kind=answer \
                         re={off} <text>\n",
                        t.from
                    ));
                }
                // Read before it is marked read: a reader that went away (`| head -1`) has
                // not read the rest, so this ends the inbox with nothing committed.
                out.write_all(line.as_bytes())
                    .and_then(|()| out.flush())
                    .map_err(|error| io::Error::new(error.kind(), Unprinted { error, peek }))?;
                count += 1;
            }
        }
    }
    // The subscription is done: end it (the closer's duplicate descriptor
    // would otherwise hold it open), so the broker's tail is released before
    // the commit, not when this process exits.
    drop(sub);
    closer.close();
    let read = if count == 0 {
        "no new mail".to_string()
    } else {
        format!("{count} message(s)")
    };
    if peek {
        eprintln!("{read}, left unread (--peek)");
        return Ok(());
    }
    // COMMITTED AFTER IT WAS PRINTED, only through the captured head. A newer
    // mark cannot include mail that arrived after this snapshot began.
    let Some(upto) = end.checked_sub(1) else {
        eprintln!("{read}");
        return Ok(());
    };
    let Some(read_log) = read_log else {
        return Err(io::Error::other(
            "the broker did not identify the inbox log; nothing was marked read \
             (use --peek to read without committing)",
        ));
    };
    let (mut conn, _closer) = connect_within(cfg, cmd, bounds.request)?;
    if inbox_log_identity(&mut conn)? != Some(read_log) {
        return Err(io::Error::other(
            "the broker log changed or could not be identified after reading the inbox; \
             nothing was marked read",
        ));
    }
    conn.commit(&group, upto)?;
    eprintln!("{read}, marked read through @{upto}");
    Ok(())
}

/// The identity answered on this connection's attach, or a fresh Hello for an
/// unguarded connection. A retained witness from another connection proves none.
fn inbox_log_identity(conn: &mut Conn) -> io::Result<Option<astream_broker::LogIdentity>> {
    match conn.seen_log_identity() {
        Some(log) => Ok(Some(log)),
        None => conn.log_identity(),
    }
}

/// A read past the inbox's liveness bound (`bound`), said in words: the broker
/// marks a subscription it is reading for at least every
/// [`astream_broker::IDLE_MARK`], so silence past the bound is a link that is
/// gone, which a bare errno would not say.
fn went_silent(e: io::Error, bound: Duration) -> io::Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => io::Error::new(
            e.kind(),
            format!(
                "went silent: nothing in {} s on a link it marks every {} s while it reads, so \
                 the link is gone; nothing was marked read",
                bound.as_secs(),
                astream_broker::IDLE_MARK.as_secs()
            ),
        ),
        _ => e,
    }
}

// mint-for

/// `aterm fabric mint-for`'s flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MintForOpts {
    /// The person to mint for: `h-<name>` or `a-<name>`.
    pub person: String,
    /// `--out <cap>`: write the cap there (0600) instead of stdout.
    pub out: Option<String>,
    /// `--fleet <F>`: default this host's configured fleet.
    pub fleet: Option<String>,
    /// `--expires <N>d|<N>h` ([`parse_lifetime`]), as milliseconds: every
    /// grant is stamped `exp=<now + this>`, and the broker refuses the ring
    /// from that instant on. `None` mints a ring that never expires.
    pub expires: Option<u64>,
}

/// Whether `id` is a PERSON: an `h-` human or an `a-` agent holding its own cap
/// (§3.2), the principals `mint-for` mints [`crate::enable::person_grants`] for.
#[must_use]
pub fn is_person_id(id: &str) -> bool {
    (id.starts_with("h-") || id.starts_with("a-")) && crate::subject::is_principal(id)
}

/// `--expires <N>d|<N>h`: a capability lifetime, as milliseconds.
///
/// # Errors
///
/// Anything but a positive whole number of days or hours, at most ten years.
pub fn parse_lifetime(word: &str) -> Result<u64, String> {
    const HOUR_MS: u64 = 3_600_000;
    const MAX_HOURS: u64 = 10 * 366 * 24;
    let refuse = || {
        format!(
            "--expires {}: a whole number of days or hours, e.g. 30d or 12h",
            safe(word, 32)
        )
    };
    let (n, unit_hours) = if let Some(d) = word.strip_suffix('d') {
        (d, 24)
    } else if let Some(h) = word.strip_suffix('h') {
        (h, 1)
    } else {
        return Err(refuse());
    };
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refuse());
    }
    let hours = n
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit_hours))
        .filter(|&h| (1..=MAX_HOURS).contains(&h))
        .ok_or_else(refuse)?;
    Ok(hours * HOUR_MS)
}

/// `grant` with `exp=<exp_ms>` added to its prefix: `rw,p=<P>:` becomes
/// `rw,p=<P>,exp=<ms>:` and `ro:` becomes `ro,exp=<ms>:` — the grammar
/// astream-cap reads (`field := "p=" … | "exp=" …`). Every person grant has a
/// prefix, and the mint re-parses, so a stamp that went wrong fails closed.
fn with_expiry(grant: &str, exp_ms: u64) -> String {
    grant.replacen(':', &format!(",exp={exp_ms}:"), 1)
}

/// The fleet and the broker of THIS host's fabric, when it has one: the
/// config's `[fabric] command`, else the rendezvous file's.
fn this_fabric() -> Option<(String, String)> {
    crate::fabric::resolve_command(None, crate::fabric::config_path().as_deref())
        .ok()
        .flatten()
        .and_then(|(_, cmd)| crate::fabric::bridge_config(&cmd).ok())
        .map(|cfg| (cfg.fleet, cfg.broker))
        .or_else(|| {
            crate::enable::Rendezvous::read()
                .ok()
                .flatten()
                .map(|r| (r.fleet, r.broker))
        })
}

/// `path` as an absolute path: itself, or joined to the working directory, so
/// the line that names the cap names the file a later command reads.
fn absolute(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map_or(p.clone(), |cwd| cwd.join(&p))
    }
}

/// `aterm fabric mint-for h-<name>|a-<name> [--out <cap>] [--fleet <F>] [--expires <N>d|<N>h]`.
#[must_use]
pub fn mint_for(opts: &MintForOpts) -> ExitCode {
    let person = &opts.person;
    if !is_person_id(person) {
        eprintln!(
            "aterm fabric mint-for: `{}` is not a person: `h-<name>` for a human, `a-<name>` \
             for an agent with its own cap ([a-z0-9-])",
            safe(person, 64)
        );
        return ExitCode::from(2);
    }
    let (p, root_from) = match Paths::resolve_for_off() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric mint-for: {e}");
            return ExitCode::from(2);
        }
    };
    let here = this_fabric();
    let fleet = opts
        .fleet
        .clone()
        .or_else(|| here.as_ref().map(|(f, _)| f.clone()))
        .unwrap_or_else(|| p.fleet.clone());
    if !crate::subject::is_fleet(&fleet) {
        eprintln!(
            "aterm fabric mint-for: fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            safe(&fleet, 64)
        );
        return ExitCode::from(2);
    }
    // The secret, through the one reader (0600, 32+ bytes, its exact bytes).
    let secret_path = p.secret();
    let shown = secret_path.to_string_lossy().into_owned();
    let secret = match crate::transport::read_mint_secret(&shown) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!(
                "aterm fabric mint-for: no mint secret at {shown}: `aterm fabric on` mints it"
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!(
                "aterm fabric mint-for: {}",
                crate::transport::describe(&shown, &e)
            );
            return ExitCode::FAILURE;
        }
    };
    let exp = opts
        .expires
        .map(|lifetime| crate::now_ms().saturating_add(lifetime));
    let ring = crate::enable::person_grants(&fleet, person);
    let mut lines = String::new();
    for grant in &ring {
        let grant = exp.map_or_else(|| grant.clone(), |exp| with_expiry(grant, exp));
        match astream_cap::mint(&secret, &grant) {
            Ok(c) => {
                lines.push_str(&astream_cap::capfile::format_line(&c.filter, &c.tag));
                lines.push('\n');
            }
            Err(e) => {
                eprintln!("aterm fabric mint-for: the mint refused `{grant}`: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let written = match &opts.out {
        None => {
            // THE CAP IS THE ANSWER: one that did not reach stdout whole is no mint the
            // caller holds, so it is refused before the guidance below says "to stdout".
            if let Err(line) = cap_on_stdout(write_capability_to(&mut io::stdout(), &lines)) {
                eprintln!("{line}");
                return ExitCode::FAILURE;
            }
            None
        }
        Some(out) => match write_cap(&absolute(out), &lines, &secret_path) {
            Ok(path) => Some(path),
            Err(e) => {
                eprintln!("aterm fabric mint-for: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    // THE GUIDANCE, on stderr when the cap itself went to stdout, so a
    // redirect captures exactly the cap's lines.
    let say = |line: String| {
        if written.is_some() {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    };
    say(format!(
        "minted {person}'s {} grants on fleet {fleet} under {} (root from {root_from}){}",
        ring.len(),
        secret_path.display(),
        written.as_ref().map_or_else(
            || " — to stdout".to_string(),
            |w| format!(" into {} (0600)", w.display())
        )
    ));
    if let Some(exp) = exp {
        say(format!(
            "they expire at {} (exp={exp}): the broker refuses them from then on; mint again \
             here before then",
            aterm_types::rfc3339::format_rfc3339(exp / 1000)
        ));
    }
    let cap_shown = written
        .as_ref()
        .map_or_else(|| "<cap>".to_string(), |w| w.display().to_string());
    let broker = here.as_ref().map_or("<broker>", |(_, b)| b.as_str());
    say(person_guidance(person, &fleet, broker, &cap_shown));
    ExitCode::SUCCESS
}

/// Write a capability result completely and flush it. Unlike incidental diagnostic
/// printing, every error (including a departed reader) prevents claiming delivery.
///
/// # Errors
/// Any write or flush failure; no partial result is certified as delivered.
pub(crate) fn write_capability_to(out: &mut dyn io::Write, text: &str) -> io::Result<()> {
    out.write_all(text.as_bytes()).and_then(|()| out.flush())
}

/// Whether the cap `mint-for` wrote to stdout reached its reader whole, as `written` says: any
/// write error refuses, a reader that went away included. A verb's answer into `| head -1` is no
/// failure ([`aterm_uds::stdout::answer_status`]), but a cap cut short (`| ssh host 'cat > cap'`
/// whose ssh died, `> cap` on a full disk) is no cap a person can present, and "minted … to
/// stdout" would say it was.
fn cap_on_stdout(written: io::Result<()>) -> Result<(), String> {
    written.map_err(|e| {
        format!(
            "aterm fabric mint-for: the cap was not written whole to stdout ({e}), so none was \
             handed over; mint again"
        )
    })
}

/// Write the cap `lines` to `path`, 0600 — never through a symlink, never over
/// a DIFFERENT file; an IDENTICAL file already there is made 0600, and a wider
/// mode it had is reported with what voids a copy (`secret`, the mint secret
/// the cap was minted under). The path written.
fn write_cap(path: &Path, lines: &str, secret: &Path) -> Result<PathBuf, String> {
    // NEVER THROUGH A SYMLINK: `read` and the write below would both follow
    // one at `--out`, into a file somebody else chose.
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(format!(
            "{} exists and is not a regular file; remove it or choose another --out",
            path.display()
        ));
    }
    match std::fs::read(path) {
        // THE SAME CAP IS ALREADY THERE: made 0600 first, since the line
        // after says so, and a wider mode is reported.
        Ok(old) if old == lines.as_bytes() => match private_mode(path) {
            Ok(None) => {}
            Ok(Some(was)) => eprintln!(
                "aterm fabric mint-for: {} held this cap at mode {was:03o}, readable by other \
                 users; now 0600. A copy of it works until its grants expire (a cap minted \
                 with --expires lapses on its own) or the mint secret is replaced: move {} \
                 aside and run `aterm fabric on` again, which re-mints the node's cap; every \
                 person's cap minted under the old secret stops verifying, so mint each again",
                path.display(),
                secret.display()
            ),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        },
        Ok(_) => {
            return Err(format!(
                "{} exists and holds a different cap; remove it or choose another --out",
                path.display()
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            write_atomic(path, lines.as_bytes(), 0o600)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    Ok(path.to_path_buf())
}

/// What a person does with the cap `mint-for <person>` just made: the
/// commands, spelled with that cap. Only a human (`h-`) halts, and every node
/// takes a human's task as a task; an `a-` service's ring has no halt grant,
/// and its task is one only where `--accept-from` lists it (the bridge's
/// `accepted`).
fn person_guidance(person: &str, fleet: &str, broker: &str, cap: &str) -> String {
    let human = person.starts_with("h-");
    let halt = if human {
        format!(
            "  aterm fabric halt --cap-file {cap} <reason>    hold every session on the fleet\n  \
             aterm fabric halt --cap-file {cap} off         withdraw that halt\n"
        )
    } else {
        String::new()
    };
    let task = if human {
        format!("a `task` from {person} arrives as a task on every node")
    } else {
        format!(
            "{person} is not a human: it halts nothing, and its `task` arrives as a task only on \
             a node whose --accept-from lists {person}"
        )
    };
    format!(
        "with it, {person} uses the fleet (keep the cap 0600; it IS {person}):\n\
         {halt}  \
         aterm fabric inbox --cap-file {cap}            read what agents posted to={person}\n  \
         aterm fabric post --cap-file {cap} to=@<sid>@<node> kind=answer re=<off> <text>\n  \
         aterm link notify --fleet {fleet} --broker {broker} --cap-file {cap} --on \
         ask:{person} --exec <cmd>    be paged when an agent asks {person}\n\
         {task}"
    )
}

/// Make `path` 0600 if it is not: `Ok(None)` when it already was, `Ok(Some(
/// <old mode>))` when it was changed.
///
/// # Errors
///
/// The stat or the chmod.
fn private_mode(path: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    let was = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if was == 0o600 {
        return Ok(None);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(was))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CAP STDOUT DID NOT TAKE WHOLE IS REFUSED, a reader that went away included: the cap
    /// is the whole answer, and a person cannot present part of one.
    #[test]
    fn a_cap_stdout_did_not_take_whole_is_refused() {
        assert_eq!(cap_on_stdout(Ok(())), Ok(()));
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::StorageFull] {
            let refused = cap_on_stdout(Err(kind.into())).expect_err("no cap was handed over");
            assert!(
                refused.starts_with("aterm fabric mint-for: the cap was not written whole"),
                "{kind:?}: {refused}"
            );
        }
    }

    /// `--expires` is a whole number of days or hours, never zero, never
    /// past ten years.
    #[test]
    fn a_lifetime_is_days_or_hours() {
        assert_eq!(parse_lifetime("30d"), Ok(30 * 24 * 3_600_000));
        assert_eq!(parse_lifetime("12h"), Ok(12 * 3_600_000));
        for bad in [
            "0d", "0h", "d", "30", "30m", "-1d", "+3h", "1.5d", "99999d", "",
        ] {
            assert!(parse_lifetime(bad).is_err(), "{bad}");
        }
    }

    /// A PERSON'S RING STAMPED WITH AN EXPIRY keeps its authority: every grant
    /// carries `exp=` in astream's grammar, a principal grant beside its `p=`.
    #[test]
    fn a_person_ring_stamped_with_an_expiry_reads_its_expiry() {
        let exp = 1_800_000_000_000u64;
        let stamped: Vec<String> = crate::enable::person_grants("lab", "h-andrew")
            .iter()
            .map(|g| with_expiry(g, exp))
            .collect();
        assert!(
            stamped
                .iter()
                .any(|g| g.starts_with(&format!("rw,p=h-andrew,exp={exp}:"))),
            "{stamped:?}"
        );
        for g in &stamped {
            assert_eq!(astream_cap::expires_at(g), Some(exp), "{g}");
        }
    }

    /// A PERSON is an `h-` or `a-` principal; a node, a session and a
    /// malformed id are not.
    #[test]
    fn a_person_is_h_or_a_dash_and_a_principal() {
        assert!(is_person_id("h-andrew"));
        assert!(is_person_id("a-review"));
        for bad in ["n-0123", "s-0123", "h-", "h-UPPER", "h-a b", "andrew", ""] {
            assert!(!is_person_id(bad), "{bad}");
        }
    }

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// The three verbs' grammar: what each takes, and that a cap is required.
    #[test]
    fn each_verb_parses_its_words_and_refuses_the_rest() {
        let halt = parse("halt", &words("--cap-file c main is broken")).unwrap();
        assert_eq!(
            halt,
            Some(PersonCmd {
                act: Act::Halt(Some("main is broken".to_string())),
                cap_files: vec!["c".to_string()],
            })
        );
        let off = parse("halt", &words("off --cap-file c")).unwrap().unwrap();
        assert_eq!(off.act, Act::Halt(None));
        assert!(parse("halt", &words("--cap-file c")).is_err());
        assert!(parse("halt", &words("main is broken")).is_err(), "no cap");

        let post = parse(
            "post",
            &words("--cap-file c to=@s-a@n-b kind=answer re=@12 -- yes, ship it"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            post.act,
            Act::Post {
                sid: "s-a".to_string(),
                node: "n-b".to_string(),
                kind: "answer".to_string(),
                re: Some(12),
                text: "yes, ship it".to_string(),
            }
        );
        for bad in [
            "--cap-file c to=@s-a kind=note hi",     // no node
            "--cap-file c to=s-a@n-b kind=note hi",  // no @
            "--cap-file c to=@s-a@n-b kind=ack hi",  // a bridge's kind
            "--cap-file c to=@s-a@n-b kind=note",    // no text
            "--cap-file c kind=note hi",             // no address
            "--cap-file c to=@s-a@n-b re=x note hi", // not an offset
        ] {
            assert!(parse("post", &words(bad)).is_err(), "{bad}");
        }

        let inbox = parse("inbox", &words("--peek --cap-file c"))
            .unwrap()
            .unwrap();
        assert_eq!(inbox.act, Act::Inbox { peek: true });
        assert!(parse("inbox", &words("--cap-file c extra")).is_err());
        assert_eq!(parse("inbox", &words("--help")).unwrap(), None);
    }

    #[test]
    fn a_post_delimiter_makes_key_shaped_words_literal_message_text() {
        for text in [
            "kind=task",
            "re=17 --cap-file literal.cap",
            "--help kind=answer",
        ] {
            let parsed = parse(
                "post",
                &words(&format!("--cap-file c to=@s-a@n-b -- {text}")),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                parsed.act,
                Act::Post {
                    sid: "s-a".to_string(),
                    node: "n-b".to_string(),
                    kind: "note".to_string(),
                    re: None,
                    text: text.to_string(),
                }
            );
            assert_eq!(parsed.cap_files, ["c"]);
        }
    }

    #[cfg(unix)]
    fn guarded_person(
        person: &str,
        legacy_halt: bool,
    ) -> (
        aterm_tempfile::TempDir,
        astream_broker::BrokerHandle,
        Config,
        PersonCmd,
    ) {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-p")
            .tempdir()
            .unwrap();
        let secret = vec![0x51; 32];
        let broker =
            astream_broker::Broker::open_guarded(dir.path().join("bus.log"), secret.clone())
                .unwrap();
        let socket = dir.path().join("bus.sock");
        let server = broker.serve(&socket).unwrap();
        let cap_path = dir.path().join("person.cap");
        let mut grants = crate::enable::person_grants("f", person);
        if legacy_halt {
            grants.push(format!("rw,p={person}:/f/f/fleet/{person}/>"));
            grants.sort();
            grants.dedup();
        }
        let caps: String = grants
            .iter()
            .map(|grant| {
                let cap = astream_cap::mint(&secret, grant).unwrap();
                format!(
                    "{}\n",
                    astream_cap::capfile::format_line(&cap.filter, &cap.tag)
                )
            })
            .collect();
        std::fs::write(&cap_path, caps).unwrap();
        let cfg = unix_cfg(dir.path(), &socket);
        let cmd = PersonCmd {
            act: Act::Halt(Some("stop".to_string())),
            cap_files: vec![cap_path.to_string_lossy().into_owned()],
        };
        (dir, server, cfg, cmd)
    }

    /// Fleet `f` on the broker socket `socket`.
    #[cfg(unix)]
    fn unix_cfg(dir: &std::path::Path, socket: &std::path::Path) -> Config {
        Config {
            fleet: "f".to_string(),
            broker: socket.to_string_lossy().into_owned(),
            cap_files: Vec::new(),
            state_dir: dir.join("state").to_string_lossy().into_owned(),
            accept_from: Vec::new(),
            sock: None,
            token: None,
            presence: crate::presence::Mode::default(),
            receipts: true,
            log: crate::mirror::LogMode::Off,
        }
    }

    /// The commits a stand-in broker acked, `(group, upto)`.
    #[cfg(unix)]
    type Commits = std::sync::Arc<std::sync::Mutex<Vec<(String, u64)>>>;

    #[cfg(unix)]
    #[derive(Default)]
    struct StandInState {
        stopped: bool,
        active: Option<std::sync::Weak<std::os::unix::net::UnixStream>>,
        /// When the group subscription arrived.
        subscribed: Option<std::time::Instant>,
    }

    #[cfg(unix)]
    struct StandIn {
        _dir: aterm_tempfile::TempDir,
        state: std::sync::Arc<(std::sync::Mutex<StandInState>, std::sync::Condvar)>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl StandIn {
        /// When the inbox's group subscription reached the stand-in, if it did.
        fn subscribed(&self) -> Option<std::time::Instant> {
            let (state, _) = &*self.state;
            state.lock().unwrap_or_else(|e| e.into_inner()).subscribed
        }
    }

    #[cfg(unix)]
    impl Drop for StandIn {
        fn drop(&mut self) {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
            if let Some(stream) = state.active.take().and_then(|s| s.upgrade()) {
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let _ = stream.shutdown(std::net::Shutdown::Read);
            }
            changed.notify_one();
            drop(state);
            if let Some(worker) = self.worker.take() {
                if let Err(panic) = worker.join() {
                    if !std::thread::panicking() {
                        std::panic::resume_unwind(panic);
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    fn log_identity(id: u8, generation: u64) -> astream_broker::LogIdentity {
        astream_broker::LogIdentity {
            id: [id; 16],
            generation,
        }
    }

    /// What an inbox waits on against a stand-in broker: every wait a hang
    /// detector. The stand-in is a thread of this test binary, which a loaded
    /// machine can starve for longer than an operator's bounds ([`LIVE`]), so
    /// no wait on it is held to them.
    const STAND_IN: Bounds = Bounds {
        request: aterm_spec::wait::MUST_HAPPEN,
        subscription: aterm_spec::wait::MUST_HAPPEN,
    };

    /// A stand-in broker for ONE `inbox` over `conns` connections, at astream's
    /// wire: the head query answers `end`, the group subscription is sent
    /// `tail` (nothing, for an empty one), and every `Commit` is recorded before
    /// its ack, so once `inbox` returns the record is final. Its returned guard
    /// cancels accept or an active connection and joins, including on assertion
    /// unwind.
    #[cfg(unix)]
    fn stand_in(
        end: u64,
        tail: Vec<astream_broker::Response>,
        conns: usize,
    ) -> (StandIn, Config, Commits) {
        scripted_inbox(end, tail, vec![Some(log_identity(1, 0)); conns], false)
    }

    #[cfg(unix)]
    fn scripted_inbox(
        end: u64,
        tail: Vec<astream_broker::Response>,
        logs: Vec<Option<astream_broker::LogIdentity>>,
        close_after_tail: bool,
    ) -> (StandIn, Config, Commits) {
        use astream_broker::proto::{
            decode_request, encode_response, read_frame, write_frame, Request, Response,
        };
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-inbox")
            .tempdir_in("/tmp")
            .expect("the fixture dir");
        let socket = dir.path().join("b.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).unwrap();
        let commits = Commits::default();
        let seen = std::sync::Arc::clone(&commits);
        let state = std::sync::Arc::new((
            std::sync::Mutex::new(StandInState::default()),
            std::sync::Condvar::new(),
        ));
        let accepting = state.clone();
        let worker = std::thread::spawn(move || {
            let mark = |next, head| Response::Mark {
                next,
                head,
                resume: String::new(),
                durable: None,
                oldest: None,
            };
            for log in logs {
                let (state, changed) = &*accepting;
                let stream = loop {
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.stopped {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            let stream = std::sync::Arc::new(stream);
                            state.active = Some(std::sync::Arc::downgrade(&stream));
                            break stream;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => drop(
                            changed
                                .wait_timeout(state, std::time::Duration::from_millis(20))
                                .unwrap_or_else(|e| e.into_inner()),
                        ),
                        Err(e) => panic!("accept the inbox fixture: {e}"),
                    }
                };
                let mut c = stream.as_ref();
                while let Ok(Some(frame)) = read_frame(&mut c) {
                    let mut end_stream = false;
                    let replies = match decode_request(&frame) {
                        Some(Request::Hello) => vec![Response::Nonce {
                            nonce: vec![0; 32],
                            log,
                            ext: astream_broker::NonceExt::default(),
                        }],
                        Some(Request::IdleMarks) => vec![mark(end, end)],
                        Some(Request::Fetch { max: 0, .. }) => vec![mark(0, end)],
                        Some(Request::SubscribeGroup { .. }) => {
                            accepting
                                .0
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .subscribed = Some(std::time::Instant::now());
                            end_stream = close_after_tail;
                            tail.clone()
                        }
                        Some(Request::Commit { group, upto }) => {
                            seen.lock().unwrap().push((group, upto));
                            vec![Response::PublishAck {
                                offset: end,
                                deduped: false,
                            }]
                        }
                        other => panic!("an inbox sends no {other:?}"),
                    };
                    for r in &replies {
                        if write_frame(&mut c, &encode_response(r)).is_err() {
                            break;
                        }
                    }
                    if end_stream {
                        break;
                    }
                }
            }
        });
        let cfg = unix_cfg(dir.path(), &socket);
        (
            StandIn {
                _dir: dir,
                state,
                worker: Some(worker),
            },
            cfg,
            commits,
        )
    }

    #[cfg(unix)]
    #[test]
    fn an_inbox_stops_at_its_captured_head_without_a_tail_mark() {
        use astream_broker::Response;
        let mail = |offset| Response::Delivery {
            offset,
            subject: "/f/f/in/p/h-x/n-peer/note".into(),
            body: b"v=1 t=1 text=mail".to_vec(),
        };
        let (_server, cfg, commits) = scripted_inbox(
            10,
            vec![mail(7), mail(10), mail(11)],
            vec![Some(log_identity(1, 0)); 2],
            true,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        let mut out = Vec::new();
        inbox(&cfg, &cmd, "h-x", false, &mut out, STAND_IN)
            .expect("a delivery at the captured head completes the snapshot without another mark");
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
        );
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.starts_with("@7  ") && out.ends_with("  text=mail\n"),
            "{out}"
        );
        assert_eq!(
            out.lines().count(),
            1,
            "only the mail below the head: {out}"
        );
    }

    /// Mail stdout refused was not read: a reader that went away (`aterm fabric inbox |
    /// head -1`) ends the inbox with [`Unprinted`] and nothing committed. std's `println!`
    /// panicked there; a printer that dropped the line would have marked it read.
    #[cfg(unix)]
    #[test]
    fn an_inbox_whose_reader_went_away_marks_nothing_read() {
        struct Gone;
        impl io::Write for Gone {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (_server, cfg, commits) = scripted_inbox(
            10,
            vec![astream_broker::Response::Delivery {
                offset: 7,
                subject: "/f/f/in/p/h-x/n-peer/note".into(),
                body: b"v=1 t=1 text=mail".to_vec(),
            }],
            vec![Some(log_identity(1, 0)); 2],
            true,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        let e = inbox(&cfg, &cmd, "h-x", false, &mut Gone, STAND_IN)
            .expect_err("mail nobody read is not an inbox read");
        assert!(
            e.get_ref().is_some_and(|inner| inner.is::<Unprinted>()),
            "{e}"
        );
        assert!(e.to_string().contains("nothing was marked read"), "{e}");
        assert!(
            commits.lock().unwrap().is_empty(),
            "mail the reader never had was marked read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_inbox_commits_only_through_its_captured_head() {
        let (_server, cfg, commits) = stand_in(
            10,
            vec![astream_broker::Response::Mark {
                next: 14,
                head: 14,
                resume: String::new(),
                durable: None,
                oldest: None,
            }],
            2,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).unwrap();
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)],
            "later mail was never part of this snapshot"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_inbox_never_commits_its_old_offsets_on_a_replacement_log() {
        for replacement in [log_identity(2, 0), log_identity(1, 1)] {
            let (_server, cfg, commits) = scripted_inbox(
                10,
                vec![astream_broker::Response::Mark {
                    next: 10,
                    head: 10,
                    resume: String::new(),
                    durable: None,
                    oldest: None,
                }],
                vec![Some(log_identity(1, 0)), Some(replacement)],
                false,
            );
            let cmd = PersonCmd {
                act: Act::Inbox { peek: false },
                cap_files: Vec::new(),
            };
            assert!(
                inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).is_err(),
                "a replacement log accepted the old snapshot cursor"
            );
            assert!(
                commits.lock().unwrap().is_empty(),
                "unread replacement mail was marked read"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_inbox_with_an_unidentified_connection_never_marks_mail_read() {
        for logs in [
            vec![None, Some(log_identity(1, 0))],
            vec![Some(log_identity(1, 0)), None],
        ] {
            let (_server, cfg, commits) = scripted_inbox(
                10,
                vec![astream_broker::Response::Mark {
                    next: 10,
                    head: 10,
                    resume: String::new(),
                    durable: None,
                    oldest: None,
                }],
                logs,
                false,
            );
            let cmd = PersonCmd {
                act: Act::Inbox { peek: false },
                cap_files: Vec::new(),
            };
            assert!(
                inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).is_err(),
                "unknown log identity was treated as equal"
            );
            assert!(commits.lock().unwrap().is_empty());
        }
    }

    /// **A CATCH-UP IS READ TO ITS END, NOT TO THE FIRST MARK.** The broker
    /// marks part-way (`next=4`, below the head of 10 the inbox started at),
    /// then delivers the mail at @7 and marks the end. The inbox prints the mail
    /// and commits through @9 — the end mark's resume point. Read to the first
    /// mark it said "no new mail" with mail at @7.
    #[cfg(unix)]
    #[test]
    fn an_inbox_reads_past_its_progress_marks() {
        use astream_broker::Response;
        let mut body = Body::new(1);
        body.text = "is main green?".to_string();
        let (_dir, cfg, commits) = stand_in(
            10,
            vec![
                Response::Mark {
                    next: 4,
                    head: 10,
                    resume: String::new(),
                    durable: None,
                    oldest: None,
                },
                Response::Delivery {
                    offset: 7,
                    subject: "/f/f/in/p/h-x/n-00000000000000aa/ask".to_string(),
                    body: body.encode(),
                },
                Response::Mark {
                    next: 10,
                    head: 10,
                    resume: String::new(),
                    durable: None,
                    oldest: None,
                },
            ],
            2,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        let mut out = Vec::new();
        inbox(&cfg, &cmd, "h-x", false, &mut out, STAND_IN).expect("the inbox reads to its end");
        assert!(
            String::from_utf8(out).unwrap().starts_with("@7  "),
            "the mail past the progress mark is printed"
        );
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)],
            "committed through the end mark"
        );
    }

    /// **THE SUBSCRIPTION IS HELD TO ITS OWN BOUND, NOT A REQUEST'S.** A broker
    /// that takes the subscription and then says nothing: the read ends at the
    /// subscription's bound, in words, with nothing marked read. Held to the
    /// request's bound instead (two hang detectors here), the subscription
    /// outlasts the hang detector, which is timed from the subscription's arrival
    /// alone. That the operator's subscription bound outlasts a request's, so a
    /// scan longer than a request is read to its end, is [`LIVE`]'s own assertion.
    /// NOT COVERED: a subscription bound LONGER than the request's, held to the
    /// shorter of the two (`min` at the `set_read_timeout`) — the live inbox's
    /// failure. Run, it would wait out a request's bound, which against a stand-in
    /// must be a hang detector ([`STAND_IN`]); `inbox`'s one `set_read_timeout`
    /// line, which sets `bounds.subscription` alone, is what holds it.
    #[cfg(unix)]
    #[test]
    fn an_inbox_subscription_is_held_to_its_own_bound() {
        let (server, cfg, commits) = stand_in(10, Vec::new(), 1);
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        let bounds = Bounds {
            request: aterm_spec::wait::MUST_HAPPEN * 2,
            subscription: Duration::from_millis(50),
        };
        let e = inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), bounds)
            .expect_err("a broker that never marks ends no inbox well");
        let held = server.subscribed().expect("the inbox subscribed").elapsed();
        assert!(
            held < aterm_spec::wait::MUST_HAPPEN,
            "the subscription waited out the request's bound ({held:?}): {e}"
        );
        assert!(e.to_string().starts_with("went silent"), "{e}");
        assert!(commits.lock().unwrap().is_empty(), "nothing marked read");
    }

    /// **AN INBOX WITH NO NEW MAIL MARKS READ WHAT IT SCANNED; `--peek` MARKS
    /// NOTHING.** Ten records, none the person's: the cursor moves past them
    /// (through @9), so the next inbox does not scan them again — committed
    /// only after mail, an inbox re-scanned the whole bus since the person's
    /// last message on every run. `--peek` opens no second connection.
    #[cfg(unix)]
    #[test]
    fn an_inbox_with_no_new_mail_marks_what_it_scanned_and_peek_marks_nothing() {
        let end = || {
            vec![astream_broker::Response::Mark {
                next: 10,
                head: 10,
                resume: String::new(),
                durable: None,
                oldest: None,
            }]
        };
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        let (_dir, cfg, commits) = stand_in(10, end(), 2);
        inbox(&cfg, &cmd, "h-x", false, &mut Vec::new(), STAND_IN).expect("an inbox with no mail");
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
        );
        let (_dir, cfg, commits) = stand_in(10, end(), 1);
        inbox(&cfg, &cmd, "h-x", true, &mut Vec::new(), STAND_IN).expect("a peek with no mail");
        assert!(commits.lock().unwrap().is_empty(), "--peek committed");
    }

    /// The CLI must reject an agent halt before publishing an ignored fleet
    /// row, even if an older minted ring still grants that write.
    #[cfg(unix)]
    #[test]
    fn an_agent_halt_is_refused_before_it_can_append_a_fleet_row() {
        let (_dir, _server, cfg, cmd) = guarded_person("a-review", true);
        let (mut conn, _closer) = connect(&cfg, &cmd).unwrap();
        let (_, (_, before)) = conn.fetch(0, "/f/f/pub/>", 0).unwrap();
        assert_eq!(main("halt", &cmd, &cfg), ExitCode::from(2));
        let (_, (_, after)) = conn.fetch(0, "/f/f/pub/>", 0).unwrap();
        assert_eq!(after, before, "a refused agent halt appended a bus record");
    }

    /// The minted agent ring keeps its mail and cursor authority, while the
    /// guarded broker refuses a direct attempt to bypass the CLI's halt gate.
    #[cfg(unix)]
    #[test]
    fn an_agents_ring_reads_and_sends_mail_but_cannot_write_a_halt() {
        let (_dir, _server, cfg, cmd) = guarded_person("a-review", false);
        let (mut conn, _closer) = connect(&cfg, &cmd).unwrap();
        let producer = astream_cap::producer_id_of("a-review");
        let (off, _) = conn
            .publish(
                producer,
                1,
                "/f/f/in/p/a-review/a-review/note",
                b"v=1 text=mail",
            )
            .unwrap();
        let (rows, _) = conn.fetch(0, "/f/f/in/p/a-review/>", 8).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, off);
        conn.commit("/f/f/cur/p/a-review/inbox", off).unwrap();
        conn.publish(
            producer,
            2,
            "/f/f/in/n-target/s-target/a-review/note",
            b"v=1 text=reply",
        )
        .unwrap();
        let denied = conn
            .publish(producer, 3, "/f/f/fleet/a-review/halt", b"v=1 state=on")
            .unwrap_err();
        assert!(astream_broker::broker_refusal(&denied)
            .is_some_and(astream_broker::BrokerRefusal::is_permanent));
    }

    /// ONLY A HUMAN'S RING CAN WRITE A HALT ROW, by astream's own publish rule:
    /// a bridge applies a halt under an `h-` principal alone, so an `a-`
    /// service's ring reaches its sessions and its mail, never the fleet face.
    #[test]
    fn only_a_humans_ring_can_publish_a_halt() {
        let secret = [7u8; 32];
        let can = |person: &str, subject: &str| {
            crate::enable::person_grants("f1", person).iter().any(|g| {
                let cap = astream_cap::mint(&secret, g).expect("mint");
                astream_cap::grants_publish(
                    &secret,
                    &cap,
                    subject,
                    astream_cap::producer_id_of(person),
                    0,
                )
            })
        };
        assert!(can("h-andrew", "/f/f1/fleet/h-andrew/halt"));
        assert!(!can("a-svc", "/f/f1/fleet/a-svc/halt"));
        for p in ["h-andrew", "a-svc"] {
            assert!(can(p, &format!("/f/f1/in/n-a/s-b/{p}/answer")), "{p} posts");
        }
    }

    /// The person is the one person-bound principal in the cap files, on this
    /// fleet; a node's ring alone names none.
    #[test]
    fn the_person_is_read_off_the_cap_files() {
        let dir = std::env::temp_dir().join(format!("atl-person-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, grants: &[String]| {
            let path = dir.join(name);
            let lines: String = grants
                .iter()
                .map(|g| format!("{g} {}\n", "ab".repeat(32)))
                .collect();
            std::fs::write(&path, lines).unwrap();
            path.to_string_lossy().into_owned()
        };
        let person = write("h.cap", &crate::enable::person_grants("f1", "h-andrew"));
        let node = write(
            "n.cap",
            &crate::enable::node_grants("f1", "n-00000000000000aa"),
        );
        let other = write("o.cap", &crate::enable::person_grants("f1", "h-other"));
        assert_eq!(
            person_of(&[node.clone(), person.clone()], "f1"),
            Ok("h-andrew".to_string())
        );
        assert!(
            person_of(&[node], "f1").is_err(),
            "a node's ring is no person"
        );
        assert!(person_of(&[person.clone(), other], "f1").is_err());
        assert!(person_of(&[person], "f2").is_err(), "another fleet");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
