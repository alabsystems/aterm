// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric halt|inbox|post` — the PERSON side of the fabric.
//!
//! ```text
//! aterm fabric halt  --cap-file <cap> <reason>|off
//! aterm fabric inbox --cap-file <cap> [--peek]
//! aterm fabric post  --cap-file <cap> to=@<sid>@<node> kind=<k> [re=<off>] [--] <text>
//! ```
//!
//! A session speaks through its node's bridge, and its node's cap-forced
//! `<src>` says so: what a person types in an aterm session arrives
//! `trust=agent`, attested by that node. A PERSON speaks for themself, under a
//! cap of their own — §8.2's human ring, [`crate::enable::person_grants`],
//! minted by `aterm fabric mint-for h-<name>` on the host that holds the
//! fleet's mint secret — and the broker binds every publish to the principal
//! the cap names. That is what these three verbs are:
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
use std::process::ExitCode;

use astream_broker::Event;

use crate::body::Body;
use crate::bridge::{read_cap_file, Config};
use crate::fabric::IO_TIMEOUT;
use crate::join::{is_node_id, is_person_id};
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
             h-<name> --out <cap>` on the host that holds the fleet's mint secret"
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
        Act::Inbox { peek } => inbox(cfg, cmd, &person, *peek),
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
        Err(e) => {
            if astream_broker::broker_refusal(&e)
                .is_some_and(astream_broker::BrokerRefusal::is_permanent)
            {
                eprintln!(
                    "aterm fabric {verb}: the broker at {} refused it: {e} — {caps} must be \
                     {person}'s ring, minted under this fleet's secret (`aterm fabric mint-for \
                     {person}` on the host that holds it)",
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
             `aterm fabric mint-for h-<name> --out <cap>` on the host that holds the fleet's \
             mint secret mints one",
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
    let (mut conn, closer) = transport::connect(&cfg.transport, &cfg.broker, IO_TIMEOUT)?;
    transport::attach_cap_files(&mut conn, &cmd.cap_files)?;
    Ok((conn, closer))
}

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
    let subject = format!("/f/{}/fleet/{person}/halt", cfg.fleet);
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
    let subject = format!("/f/{}/in/{node}/{sid}/{person}/{kind}", cfg.fleet);
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
///   a request's [`IO_TIMEOUT`] before its first matching record, so the
///   subscription is held to [`astream_broker::IDLE_MARK_DEADLINE`] instead —
///   a liveness bound, which a broker that marks (checked first) keeps while
///   it scans; past it the link is gone, and nothing is marked read.
/// * WHAT IT MARKS READ: everything below the captured head, not only up to the
///   last message printed. One commit record per run, so the next inbox scans
///   only what arrived since; an inbox that committed only after mail would
///   re-scan the whole bus since the person's last message, every run.
///   `--peek` commits nothing. A new commit connection must identify exactly
///   the log, including its generation, on which the snapshot was read.
fn inbox(cfg: &Config, cmd: &PersonCmd, person: &str, peek: bool) -> io::Result<()> {
    let group = format!("/f/{}/cur/p/{person}/inbox", cfg.fleet);
    let filter = format!("/f/{}/in/p/{person}/>", cfg.fleet);
    let (mut conn, closer) = connect(cfg, cmd)?;
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
    closer.set_read_timeout(Some(astream_broker::IDLE_MARK_DEADLINE))?;
    let cap = cmd.cap_files.join(" --cap-file ");
    let mut count = 0usize;
    loop {
        match sub.recv_event().map_err(went_silent)? {
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
                println!("{line}");
                // The reply, spelled out, for the kinds that wait for one.
                if matches!(t.kind.as_str(), "ask" | "task") && t.from.contains('@') {
                    println!(
                        "    answer: aterm fabric post --cap-file {cap} to=@{} kind=answer \
                         re={off} <text>",
                        t.from
                    );
                }
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
    let (mut conn, _closer) = connect(cfg, cmd)?;
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

/// A read past the inbox's liveness bound, said in words: the broker marks a
/// subscription it is reading for at least every [`astream_broker::IDLE_MARK`],
/// so silence past [`astream_broker::IDLE_MARK_DEADLINE`] is a link that is
/// gone, which a bare errno would not say.
fn went_silent(e: io::Error) -> io::Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => io::Error::new(
            e.kind(),
            format!(
                "went silent: nothing in {} s on a link it marks every {} s while it reads, so \
                 the link is gone; nothing was marked read",
                astream_broker::IDLE_MARK_DEADLINE.as_secs(),
                astream_broker::IDLE_MARK.as_secs()
            ),
        ),
        _ => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            transport: transport::Transport::Unix,
            cap_files: Vec::new(),
            state_dir: dir.join("state").to_string_lossy().into_owned(),
            accept_from: Vec::new(),
            sock: None,
            token: None,
            presence: crate::presence::Mode::default(),
            receipts: true,
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
    }

    #[cfg(unix)]
    struct StandIn {
        _dir: aterm_tempfile::TempDir,
        state: std::sync::Arc<(std::sync::Mutex<StandInState>, std::sync::Condvar)>,
        worker: Option<std::thread::JoinHandle<()>>,
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

    /// A stand-in broker for ONE `inbox` over `conns` connections, at astream's
    /// wire: the head query answers `end`, the group subscription is sent
    /// `tail` after `silence`, and every `Commit` is recorded before its ack,
    /// so once `inbox` returns the record is final. Its returned guard cancels
    /// accept or an active connection and joins, including on assertion unwind.
    #[cfg(unix)]
    fn stand_in(
        end: u64,
        silence: std::time::Duration,
        tail: Vec<astream_broker::Response>,
        conns: usize,
    ) -> (StandIn, Config, Commits) {
        scripted_inbox(
            end,
            silence,
            tail,
            vec![Some(log_identity(1, 0)); conns],
            false,
        )
    }

    #[cfg(unix)]
    fn scripted_inbox(
        end: u64,
        silence: std::time::Duration,
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
                        }],
                        Some(Request::IdleMarks) => vec![mark(end, end)],
                        Some(Request::Fetch { max: 0, .. }) => vec![mark(0, end)],
                        Some(Request::SubscribeGroup { .. }) => {
                            std::thread::sleep(silence);
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
            std::time::Duration::ZERO,
            vec![mail(7), mail(10), mail(11)],
            vec![Some(log_identity(1, 0)); 2],
            true,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        inbox(&cfg, &cmd, "h-x", false)
            .expect("a delivery at the captured head completes the snapshot without another mark");
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_inbox_commits_only_through_its_captured_head() {
        let (_server, cfg, commits) = stand_in(
            10,
            std::time::Duration::ZERO,
            vec![astream_broker::Response::Mark {
                next: 14,
                head: 14,
                resume: String::new(),
            }],
            2,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        inbox(&cfg, &cmd, "h-x", false).unwrap();
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
                std::time::Duration::ZERO,
                vec![astream_broker::Response::Mark {
                    next: 10,
                    head: 10,
                    resume: String::new(),
                }],
                vec![Some(log_identity(1, 0)), Some(replacement)],
                false,
            );
            let cmd = PersonCmd {
                act: Act::Inbox { peek: false },
                cap_files: Vec::new(),
            };
            assert!(
                inbox(&cfg, &cmd, "h-x", false).is_err(),
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
                std::time::Duration::ZERO,
                vec![astream_broker::Response::Mark {
                    next: 10,
                    head: 10,
                    resume: String::new(),
                }],
                logs,
                false,
            );
            let cmd = PersonCmd {
                act: Act::Inbox { peek: false },
                cap_files: Vec::new(),
            };
            assert!(
                inbox(&cfg, &cmd, "h-x", false).is_err(),
                "unknown log identity was treated as equal"
            );
            assert!(commits.lock().unwrap().is_empty());
        }
    }

    /// **A LONG CATCH-UP IS READ TO ITS END, NOT TO A REQUEST'S BOUND OR TO
    /// THE FIRST MARK.** The broker scans for longer than [`IO_TIMEOUT`]
    /// before it sends anything (a long log with no mail near its start),
    /// then marks part-way (`next=4`, below the head of 10 the inbox started
    /// at), then delivers the mail at @7 and marks the end. The inbox prints
    /// the mail and commits through @9 — the end mark's resume point. Under
    /// the request bound the read failed `TimedOut` on every run; read to the
    /// first mark it said "no new mail" with mail at @7.
    #[cfg(unix)]
    #[test]
    fn an_inbox_outlasts_a_long_scan_and_reads_past_its_progress_marks() {
        use astream_broker::Response;
        let mut body = Body::new(1);
        body.text = "is main green?".to_string();
        let (_dir, cfg, commits) = stand_in(
            10,
            IO_TIMEOUT + std::time::Duration::from_secs(1),
            vec![
                Response::Mark {
                    next: 4,
                    head: 10,
                    resume: String::new(),
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
                },
            ],
            2,
        );
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        inbox(&cfg, &cmd, "h-x", false).expect("the inbox reads to its end");
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)],
            "committed through the end mark"
        );
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
            }]
        };
        let cmd = PersonCmd {
            act: Act::Inbox { peek: false },
            cap_files: Vec::new(),
        };
        let (_dir, cfg, commits) = stand_in(10, std::time::Duration::ZERO, end(), 2);
        inbox(&cfg, &cmd, "h-x", false).expect("an inbox with no mail");
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
        );
        let (_dir, cfg, commits) = stand_in(10, std::time::Duration::ZERO, end(), 1);
        inbox(&cfg, &cmd, "h-x", true).expect("a peek with no mail");
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
