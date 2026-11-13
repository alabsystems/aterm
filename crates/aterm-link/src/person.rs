// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric mint-for|halt|inbox|post` — the person side of the fabric.

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

/// The kinds a person may `post`.
pub const PERSON_KINDS: [&str; 5] = ["ask", "answer", "task", "report", "note"];

/// The body token on a post a bridge hands to a person from a session on another Mac (§11 C3d,
/// the bridge's `hand_to_person`): `<seq>:<producer>` ([`wire_stamp`]), the offset the post has
/// on that Mac's bus and the producer whose push brought it, so a person's answer to it carries
/// the offset as its `re=` there where the producer is that Mac's ([`wire_re`]).
pub const WIRE_RE_TOKEN: &str = "wre";

/// The value of [`WIRE_RE_TOKEN`]: `seq`, then `producer` in 16 hex digits.
#[must_use]
pub(crate) fn wire_stamp(seq: u64, producer: u64) -> String {
    format!("{seq}:{producer:016x}")
}

/// The offset and producer a [`wire_stamp`] names, `None` for any other value.
fn stamped(value: &str) -> Option<(u64, u64)> {
    let (seq, producer) = value.split_once(':')?;
    let producer = (producer.len() == 16)
        .then(|| u64::from_str_radix(producer, 16).ok())
        .flatten()?;
    Some((crate::parse_uint::<u64>(seq)?, producer))
}

#[cfg(test)]
thread_local! {
    /// NEGATIVE CONTROL: [`wire_re`] takes a read refused below the lanes' oldest for no post,
    /// as it would without the told read.
    pub(crate) static UNTOLD_WIRE_RE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What a person verb was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// `halt <reason>` (`Some`) or `halt off` (`None`): with the mesh on, `off` lifts every halt
    /// that stands on this Mac, each by its id, on every Mac.
    Halt(Option<String>),
    /// `halt off <id> [--here]`, with the mesh on: lift the halt that id names, on every Mac,
    /// whichever member's person set it (§11 C4), or with `--here` on this Mac alone.
    Lift {
        /// The halt id.
        id: String,
        /// `--here`: this Mac alone.
        here: bool,
    },
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
/// # Errors
/// An unknown flag, a flag without its value, no `--cap-file`, or the verb's own arguments missing
/// or malformed.
pub fn parse(verb: &str, rest: &[String]) -> Result<Option<PersonCmd>, String> {
    let mut cap_files = Vec::new();
    let mut peek = false;
    let mut here = false;
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
            "--here" if verb == "halt" => here = true,
            "--" if verb != "inbox" => {
                literal_at = Some(words.len());
                words.extend(it.by_ref().cloned());
                break;
            }
            flag if flag.starts_with('-') && words.is_empty() => {
                return Err(format!(
                    "unknown flag {} for `{verb}` (--cap-file{})",
                    safe(flag, 64),
                    match verb {
                        "inbox" => ", --peek",
                        "halt" => ", --here",
                        _ => "",
                    }
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
                );
            }
            _ if here && !matches!(words.as_slice(), [off, _] if off == "off") => {
                return Err(
                    "--here lifts one halt by its id on this Mac alone: `halt off <id> --here`"
                        .to_string(),
                );
            }
            [off] if off == "off" => Act::Halt(None),
            [off, id] if off == "off" => {
                if !crate::halt::is_id(id) {
                    return Err(format!(
                        "halt off {}: a halt id is {} lowercase hex digits (`aterm fabric status` \
                         names each halt that stands, with its id)",
                        safe(id, 64),
                        crate::halt::ID_HEX
                    ));
                }
                Act::Lift {
                    id: id.clone(),
                    here,
                }
            }
            [off, ..] if off == "off" => {
                return Err(
                    "halt off takes at most a halt id (`halt off <id> [--here]`); a reason that \
                     starts with `off` reads as a halt-off"
                        .to_string(),
                );
            }
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

fn parse_post(words: &[String], literal_at: Option<usize>) -> Result<Act, String> {
    let (mut to, mut kind, mut re) = (None, None, None);
    let mut text_at = literal_at.unwrap_or(words.len());
    for (i, w) in words.iter().enumerate().take(text_at) {
        if let Some(v) = w.strip_prefix("to=").filter(|_| to.is_none()) {
            to = Some(v.to_string());
        } else if let Some(v) = w.strip_prefix("kind=").filter(|_| kind.is_none()) {
            kind = Some(v.to_string());
        } else if let Some(v) = w.strip_prefix("re=").filter(|_| re.is_none()) {
            re = Some(
                aterm_types::fabric_grammar::parse_bus_offset(v)
                    .ok_or_else(|| format!("re={} is not a bus offset", safe(v, 64)))?,
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

/// Run one person verb against the broker `cfg` names using the person's cap files.
#[must_use = "handle or propagate the command's exit status"]
pub fn main(verb: &str, cmd: &PersonCmd, cfg: &Config) -> ExitCode {
    let person = match person_of(&cmd.cap_files, &cfg.fleet) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric {verb}: {e}");
            return ExitCode::from(2);
        }
    };
    if matches!(cmd.act, Act::Halt(_) | Act::Lift { .. }) && !person.starts_with("h-") {
        eprintln!(
            "aterm fabric {verb}: {} is not a human: only an `h-<name>` halts the fleet (every \
             bridge applies `/f/<F>/fleet/h-<name>/halt` alone); an agent's session asks one \
             with `aterm ctl @<sid> post to=h-<name> kind=ask <text>`",
            safe(&person, 64)
        );
        return ExitCode::from(2);
    }
    let sealing = match &cmd.act {
        Act::Post { sid, node, .. } => match sealing_for(cfg, sid, node, crate::now_ms()) {
            Ok(sealing) => sealing,
            Err(why) => {
                eprintln!("aterm fabric {verb}: {why}");
                return ExitCode::from(2);
            }
        },
        _ => None,
    };
    let caps = cmd.cap_files.join(" ");
    let done = match &cmd.act {
        Act::Halt(reason) if cfg.mesh.is_some() => mesh_halt(cfg, cmd, &person, reason.as_deref()),
        Act::Halt(reason) => halt(cfg, cmd, &person, reason.as_deref()),
        Act::Lift { id, here } => lift(cfg, cmd, &person, id, *here),
        Act::Inbox { peek } => inbox(cfg, cmd, &person, *peek, &mut io::stdout(), live()),
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
            sealing.as_ref(),
        ),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.get_ref().is_some_and(|inner| inner.is::<Unprinted>()) => {
            eprintln!("aterm fabric {verb}: {e}");
            ExitCode::FAILURE
        }
        Err(e) if e.get_ref().is_some_and(|inner| inner.is::<Unmapped>()) => {
            eprintln!("aterm fabric {verb}: {e}");
            ExitCode::from(2)
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

/// How a person's post to another Mac is sealed (§11 C3c): this Mac's person key, the node key's
/// certificate of it, and the member the post goes to.
struct Sealing {
    key: crate::person_key::PersonKey,
    cert: crate::person_key::Cert,
    member: String,
    /// The node service's hand-off it was routed by: this Mac's node, the nodes its seat held
    /// before, and where each rotated node's seat is now.
    handoff: crate::federate::Handoff,
}

/// With the mesh on (§11 C3c), how a person's post to `@<sid>@<node>` at `now_ms` is written:
/// `None` for one to this Mac's own session (and for every post with the mesh off), which is
/// written as it always was, and a [`Sealing`] for one to another Mac's. A post to another Mac
/// goes on the person's own lane, sealed by this Mac's person key under its node key's
/// certificate, and the node service carries it, once the seal verifies, onto this Mac's own
/// lane to that Mac ([`crate::federate::Routes`]), whose bridge verifies it again.
///
/// # Errors
/// What would leave the post uncarried, so it is refused before anything is written: this Mac's
/// node key cannot be read (its node is not known); the node service's hand-off is missing,
/// unreadable or stale, or names no current member that `node` is; or the person key or a
/// certificate of it in force under this Mac's node key is not there yet, which the node
/// service makes as it runs.
fn sealing_for(
    cfg: &Config,
    sid: &str,
    node: &str,
    now_ms: u64,
) -> Result<Option<Sealing>, String> {
    let Some(mesh) = &cfg.mesh else {
        return Ok(None);
    };
    let to = format!("@{}@{}", safe(sid, 64), safe(node, 64));
    let status = "(`aterm node status` says whether the node service runs)";
    let node_key = match crate::federate::node_key(&mesh.keys) {
        Ok(Some(key)) => key,
        Ok(None) => {
            return Err(format!(
                "{} holds no node key yet, so this Mac cannot tell whether {to} is one of its own \
                 sessions; nothing was posted {status}",
                mesh.keys.display()
            ))
        }
        Err(e) => {
            return Err(format!(
                "this Mac's node key in {} cannot be read ({e}), so it cannot tell whether {to} \
                 is one of its own sessions; nothing was posted",
                mesh.keys.display()
            ))
        }
    };
    let here = crate::federate::node_of(&node_key);
    if node == here {
        return Ok(None);
    }
    let handoff = match crate::federate::Handoff::read(&mesh.root) {
        Ok(Some(handoff)) if handoff.me == here => handoff,
        Ok(Some(_)) => {
            return Err(format!(
                "the node service's hand-off in {} names another node as this Mac ({here}), so \
                 {to} is not routed; nothing was posted {status}",
                mesh.root.display()
            ))
        }
        Ok(None) => {
            return Err(format!(
                "there is no hand-off from the node service in {}, so {to} is not routed; \
                 nothing was posted {status}",
                mesh.root.display()
            ))
        }
        Err(e) => {
            return Err(format!(
                "the node service's hand-off cannot be read ({e}), so {to} is not routed; \
                 nothing was posted {status}"
            ))
        }
    };
    let Some(member) = handoff.current(node).map(str::to_owned) else {
        return Err(format!(
            "{to} names no current member of the fleet; nothing was posted"
        ));
    };
    let unvouched = |why: String| {
        format!(
            "this Mac's person key {why}, so a post to another Mac cannot be sealed; the node \
             service makes the key and signs its certificate as it runs; nothing was posted \
             {status}"
        )
    };
    let key = match crate::person_key::PersonKey::read(&mesh.keys) {
        Ok(Some(key)) => key,
        Ok(None) => return Err(unvouched("is not there yet".to_owned())),
        Err(e) => return Err(unvouched(format!("cannot be read ({e})"))),
    };
    let cert = match crate::person_key::read_cert(&mesh.keys) {
        Ok(Some(cert)) => cert,
        Ok(None) => return Err(unvouched("has no certificate yet".to_owned())),
        Err(e) => {
            return Err(unvouched(format!(
                "has a certificate that cannot be read ({e})"
            )))
        }
    };
    if cert.person != key.public() {
        return Err(unvouched("is newer than its certificate".to_owned()));
    }
    if let Err(refusal) = cert.check(crate::person_key::Signer::Key(&node_key), now_ms) {
        return Err(unvouched(format!(
            "has a certificate not in force under this Mac's node key ({})",
            refusal.describe()
        )));
    }
    Ok(Some(Sealing {
        key,
        cert,
        member,
        handoff,
    }))
}

/// An inbox whose mail stdout refused (the reader went away, a full disk): nothing is marked
/// read, because the mail it could not print was not read.
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

/// The ONE person the cap files bind writes to, on `fleet`.
/// # Errors
/// An unreadable cap file, no person's grant, grants for two persons, or a person's grant on
/// another fleet.
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

fn connect(cfg: &Config, cmd: &PersonCmd) -> io::Result<(Conn, Closer)> {
    connect_within(cfg, cmd, crate::fabric::io_timeout())
}

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

/// The bounds a person's inbox runs under: each request [`crate::fabric::io_timeout`], and its
/// subscription astream's [`astream_broker::IDLE_MARK_DEADLINE`] or one idle mark past the
/// request's bound, whichever is later, so the subscription always outlasts a request.
fn live() -> Bounds {
    bounds(crate::fabric::io_timeout())
}

const fn bounds(request: Duration) -> Bounds {
    let after_request = request.saturating_add(astream_broker::IDLE_MARK);
    Bounds {
        request,
        subscription: if after_request.as_nanos() > astream_broker::IDLE_MARK_DEADLINE.as_nanos() {
            after_request
        } else {
            astream_broker::IDLE_MARK_DEADLINE
        },
    }
}

// The subscription keeps the protocol's minimum and outlasts a request by an idle mark.
const _: () = assert!(
    bounds(IO_TIMEOUT).subscription.as_nanos() >= astream_broker::IDLE_MARK_DEADLINE.as_nanos()
        && bounds(IO_TIMEOUT).subscription.as_nanos()
            >= IO_TIMEOUT
                .saturating_add(astream_broker::IDLE_MARK)
                .as_nanos()
);

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
    // The person's own producer, whose bucket the broker's rate limit keeps: a refusal is waited
    // out and sent again at the same sequence ([`crate::rate_hold::waiting_out`]).
    let producer = astream_cap::producer_id_of(person);
    let (off, deduped) = crate::rate_hold::waiting_out(
        || conn.publish(producer, seq, subject, body),
        Duration::from_secs(3),
        std::thread::sleep,
    )?;
    if deduped {
        return Err(io::Error::other(format!(
            "the broker deduped it at @{off} and appended nothing: sequence {seq} was already \
             used under {person} (a clock that stepped back) — run it again"
        )));
    }
    Ok(off)
}

/// Ask this Mac's bridge for a halt (`reason`) or, with none, to lift every halt that stands
/// here (§11 C4): the request goes on `/f/alab/fleet/<person>/halts` under the person's ring,
/// and the bridge says it on this Mac's halt subject, which the mesh relays to every Mac.
fn mesh_halt(cfg: &Config, cmd: &PersonCmd, person: &str, reason: Option<&str>) -> io::Result<()> {
    let subject = crate::subject::halt_ask(crate::federate::FLEET, person);
    let cap = cmd.cap_files.join(" --cap-file ");
    let Some(reason) = reason else {
        let standing = standing_here(cfg);
        if standing.is_empty() {
            println!(
                "no halt stands on this Mac (its bridge's state names none); `aterm fabric \
                 status` names any halt another Mac set that has not reached it yet"
            );
            return Ok(());
        }
        for id in &standing {
            ask(
                cfg,
                cmd,
                person,
                &subject,
                id,
                crate::halt::Word::Off,
                false,
            )?;
        }
        println!(
            "{person} lifts {} halt(s), on every Mac: {}",
            standing.len(),
            standing.join(", ")
        );
        return Ok(());
    };
    let id = crate::halt::fresh_id()?;
    let token = crate::halt::reason_token(Some(&crate::pct::encode(reason)));
    let off = ask(
        cfg,
        cmd,
        person,
        &subject,
        &id,
        crate::halt::Word::On(token),
        false,
    )?;
    println!(
        "{person}'s halt {id} is ON at @{off} (reason={}): this Mac's bridge says it to every Mac, \
         and each holds every session (`hold=1 origin=fleet`) as it reads it. `aterm fabric halt \
         --cap-file {cap} off {id}` lifts it, from any member's person",
        safe(reason, 128)
    );
    Ok(())
}

/// `halt off <id> [--here]` (§11 C4): ask this Mac's bridge to lift that halt on every Mac, or
/// with `here` on this Mac alone.
fn lift(cfg: &Config, cmd: &PersonCmd, person: &str, id: &str, here: bool) -> io::Result<()> {
    if cfg.mesh.is_none() {
        return Err(io::Error::other(
            "a halt id names a halt only with the mesh on; `halt off` withdraws this person's \
             own halt row",
        ));
    }
    let subject = crate::subject::halt_ask(crate::federate::FLEET, person);
    let off = ask(cfg, cmd, person, &subject, id, crate::halt::Word::Off, here)?;
    if here {
        println!(
            "{person} lifts halt {id} on this Mac alone at @{off}: its bridge releases the \
             sessions it held for it (another halt still standing holds them); every other Mac \
             keeps it"
        );
    } else {
        println!(
            "{person} lifts halt {id} at @{off}: this Mac's bridge says it to every Mac, and each \
             releases the sessions it held for it (another halt still standing holds them)"
        );
    }
    Ok(())
}

/// Write one halt request (`word` for `id`, for this Mac alone when `here`) on `subject`;
/// answers its offset.
fn ask(
    cfg: &Config,
    cmd: &PersonCmd,
    person: &str,
    subject: &str,
    id: &str,
    word: crate::halt::Word,
    here: bool,
) -> io::Result<u64> {
    let body = crate::halt::Ask {
        id: id.to_owned(),
        word,
        here,
    }
    .render(crate::now_ms());
    publish(cfg, cmd, person, subject, body.as_bytes())
}

/// The ids of the halts this Mac's bridge holds its sessions under, from its state dir.
fn standing_here(cfg: &Config) -> Vec<String> {
    if cfg.state_dir.is_empty() {
        return Vec::new();
    }
    crate::state::StateDir::peek(&cfg.state_dir)
        .mesh_halts()
        .and_then(|text| crate::halt::Halts::parse(&text).ok())
        .map(|(halts, _)| halts.standing().keys().cloned().collect())
        .unwrap_or_default()
}

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

fn post(
    cfg: &Config,
    cmd: &PersonCmd,
    person: &str,
    (sid, node, kind, re): (&str, &str, &str, Option<u64>),
    text: &str,
    sealing: Option<&Sealing>,
) -> io::Result<()> {
    let wire = match (sealing, re) {
        // A node this Mac's seat held before names this Mac's own session, where an offset here
        // names the post as its session does.
        (Some(sealing), Some(re)) if sealing.member != sealing.handoff.me => {
            let (mut conn, _closer) = connect(cfg, cmd)?;
            let to = (sid, sealing.member.as_str());
            let wire = wire_re(&mut conn, (&cfg.fleet, person), &sealing.handoff, to, re)?;
            let Some(wire) = wire else {
                return Err(io::Error::other(Unmapped {
                    re,
                    sid: sid.to_owned(),
                    node: node.to_owned(),
                }));
            };
            Some(wire)
        }
        _ => re,
    };
    let node = sealing.map_or(node, |sealing| sealing.member.as_str());
    let subject = crate::subject::post_lane(&cfg.fleet, node, sid, person, kind);
    let mut body = Body::new(crate::now_ms());
    body.re = wire;
    body.text = text.to_string();
    if let Some(sealing) = sealing {
        let address = crate::person_key::post_address(&cfg.fleet, sid, person, kind);
        sealing.key.seal(&sealing.cert, &address, &mut body);
    }
    let off = publish(cfg, cmd, person, &subject, &body.encode())?;
    let named = match (re, wire) {
        (Some(re), Some(wire)) if wire != re => format!(" re={re} (@{wire} on {node})"),
        (Some(re), _) => format!(" re={re}"),
        (None, _) => String::new(),
    };
    println!("OK off={off} to=@{sid}@{node} kind={kind}{named} from={person}");
    Ok(())
}

/// A person's post to another Mac (a member other than this Mac's node) whose `re=` names
/// nothing there ([`wire_re`]): nothing is posted, and the person is told why (exit 2).
#[derive(Debug)]
struct Unmapped {
    re: u64,
    sid: String,
    node: String,
}

impl std::fmt::Display for Unmapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (re, sid, node) = (self.re, safe(&self.sid, 64), safe(&self.node, 64));
        write!(
            f,
            "re={re} names no post of {sid}'s on {node} whose offset there this Mac knows (an \
             offset on this Mac names nothing on {node}): nothing was posted (`aterm fabric \
             inbox` shows the re= that names each post; without re= the post names none)"
        )
    }
}

impl std::error::Error for Unmapped {}

/// The `re=` a person's post to `@<sid>@<node>` on another Mac carries (§11 C3d), `member`
/// being the member that post goes to (`handoff`'s current of `node`, another than this Mac's):
/// an offset on this Mac's bus means nothing on another Mac's, so `re` must be a post this Mac's
/// bridge handed the person from that session, and the post carries the offset that post has
/// there, which the bridge stamped on it ([`WIRE_RE_TOKEN`]), as a session's answer to it names
/// it. The asking session's wait is on that offset. The post at `re` is read on the person's
/// lanes here, those of this Mac's node and of the nodes its seat held before (`handoff`), which
/// the person's ring reads and only this Mac's bridge and node service write; it is from that
/// session when its `from=` is `<sid>@<n>` with `handoff`'s current of `n` the member, and its
/// stamp's producer is that member's or a node's whose seat the member holds now (the push of a
/// Mac that rotated its key brings what it wrote before under its new node), judged here by the
/// hand-off the post is routed by: the bridge reads none to stamp a copy no move put on its
/// lane. A read refused below the lanes' oldest is read again told, as the bridge
/// reads one ([`crate::bridge::told_read_at`]). `None` where `re` is no such post, it carries no
/// offset there, or retention gave it up.
///
/// # Errors
/// The broker cannot be read.
pub(crate) fn wire_re(
    conn: &mut Conn,
    (fleet, person): (&str, &str),
    handoff: &crate::federate::Handoff,
    (sid, member): (&str, &str),
    re: u64,
) -> io::Result<Option<u64>> {
    let filter = crate::subject::person_mail_filter(fleet, person);
    let rows = match conn.fetch(re, &filter, 1) {
        Ok((rows, _)) => rows,
        Err(e) if transport::retained_past(&e, re).is_some() => {
            #[cfg(test)]
            let told = !UNTOLD_WIRE_RE.with(std::cell::Cell::get);
            #[cfg(not(test))]
            let told = true;
            if told {
                crate::bridge::told_read_at(conn, re, &filter)?.unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        Err(e) => return Err(e),
    };
    let Some((_, subject, raw)) = rows.into_iter().find(|(off, _, _)| *off == re) else {
        return Ok(None);
    };
    let lane = format!("/f/{fleet}/in/p/{person}/");
    let written_here = subject
        .strip_prefix(&lane)
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(node, _)| node == handoff.me || handoff.retired(&handoff.me).contains(node));
    let (body, _) = Body::decode(&raw);
    let from_there = body
        .from
        .as_deref()
        .and_then(|from| from.split_once('@'))
        .is_some_and(|(s, n)| s == sid && handoff.current(n) == Some(member));
    let seats = handoff.retired(member);
    let brought_by_there = |producer: u64| {
        std::iter::once(member)
            .chain(seats.iter().map(String::as_str))
            .any(|node| astream_cap::producer_id_of(node) == producer)
    };
    let seq = body
        .unknown
        .get(WIRE_RE_TOKEN)
        .and_then(|value| stamped(value))
        .filter(|(seq, producer)| {
            *seq < astream_broker::ACK_SEQ_BASE && brought_by_there(*producer)
        })
        .map(|(seq, _)| seq);
    Ok(seq.filter(|_| written_here && from_there))
}

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
    let (_, (_, end)) = conn.fetch(0, &filter, 0)?;
    let offset = crate::fabric::local_offset_at_ms(crate::now_ms());
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
                ));
            }
            Some(Event::Mark { next, .. }) if next >= end => break,
            Some(Event::Mark { .. }) => {}
            Some(Event::Gap(gap)) if gap.from >= end => break,
            Some(Event::Gap(gap)) => {
                let line = format!(
                    "given up: the broker's retention gave up {} before this inbox read them\n",
                    crate::transport::gap_lost(&gap)
                );
                out.write_all(line.as_bytes())
                    .and_then(|()| out.flush())
                    .map_err(|error| io::Error::new(error.kind(), Unprinted { error, peek }))?;
            }
            Some(Event::Delivery {
                offset: off,
                subject,
                body,
            }) => {
                if off >= end {
                    break;
                }
                let line = inbox_delivery_line(
                    (&cfg.fleet, &cap),
                    off,
                    &subject,
                    &body,
                    crate::now_ms(),
                    offset,
                    &aterm_uds::zone::local_offset_at,
                );
                out.write_all(line.as_bytes())
                    .and_then(|()| out.flush())
                    .map_err(|error| io::Error::new(error.kind(), Unprinted { error, peek }))?;
                count += 1;
            }
        }
    }
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

fn inbox_log_identity(conn: &mut Conn) -> io::Result<Option<astream_broker::LogIdentity>> {
    match conn.seen_log_identity() {
        Some(log) => Ok(Some(log)),
        None => conn.log_identity(),
    }
}

pub(crate) fn inbox_delivery_line(
    (fleet, cap): (&str, &str),
    off: u64,
    subject: &str,
    raw: &[u8],
    now_ms: u64,
    header_offset: Option<i64>,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> String {
    let (b, _) = Body::decode(raw);
    let (re, verdict, dl) = (b.re, b.verdict.clone(), b.dl);
    let t = crate::fabric::traffic_of_body(fleet, off, subject, b, now_ms);
    let mut line = format!(
        "@{off}  {}  {}  from={} trust={}",
        crate::fabric::clock_at(t.t_ms, now_ms, header_offset, local_at),
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
    if matches!(t.kind.as_str(), "ask" | "task") && t.from.contains('@') {
        line.push_str(&format!(
            "    answer: aterm fabric post --cap-file {cap} to=@{} kind=answer re={off} <text>\n",
            t.from
        ));
    }
    line
}

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

/// `aterm fabric mint-for`'s flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MintForOpts {
    /// The person to mint for: `h-<name>` or `a-<name>`.
    pub person: String,
    /// `--out <cap>`: write the cap there (0600) instead of stdout.
    pub out: Option<String>,
    /// `--fleet <F>`: default this host's configured fleet.
    pub fleet: Option<String>,
    /// `--expires <N>d|<N>h` ([`parse_lifetime`]), as milliseconds.
    pub expires: Option<u64>,
}

/// Whether `id` is a PERSON.
#[must_use]
pub fn is_person_id(id: &str) -> bool {
    (id.starts_with("h-") || id.starts_with("a-")) && crate::subject::is_principal(id)
}

/// `--expires <N>d|<N>h`: a capability lifetime, as milliseconds.
/// # Errors
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

fn with_expiry(grant: &str, exp_ms: u64) -> String {
    grant.replacen(':', &format!(",exp={exp_ms}:"), 1)
}

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

fn absolute(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map_or(p.clone(), |cwd| cwd.join(&p))
    }
}

/// `aterm fabric mint-for h-<name>|a-<name> [--out <cap>] [--fleet <F>] [--expires <N>d|<N>h]`.
#[must_use = "handle or propagate the command's exit status"]
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
    let ring = match astream_cap::GrantCeiling::aterm_person_ring(&fleet, person) {
        Ok(ceiling) => ceiling.rules(),
        Err(e) => {
            eprintln!("aterm fabric mint-for: {e}");
            return ExitCode::from(2);
        }
    };
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

/// Write a capability result completely and flush it.
/// # Errors
/// Any write or flush failure; no partial result is certified as delivered.
pub(crate) fn write_capability_to(out: &mut dyn io::Write, text: &str) -> io::Result<()> {
    out.write_all(text.as_bytes()).and_then(|()| out.flush())
}

fn cap_on_stdout(written: io::Result<()>) -> Result<(), String> {
    written.map_err(|e| {
        format!(
            "aterm fabric mint-for: the cap was not written whole to stdout ({e}), so none was \
             handed over; mint again"
        )
    })
}

fn write_cap(path: &Path, lines: &str, secret: &Path) -> Result<PathBuf, String> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(format!(
            "{} exists and is not a regular file; remove it or choose another --out",
            path.display()
        ));
    }
    let wrote = match read_existing_cap(path, lines.len()) {
        Ok(Some((file, old))) if old == lines.as_bytes() => {
            match private_mode(&file) {
                Ok(None) => {}
                Ok(Some(was)) if (was & 0o077) == 0 => eprintln!(
                    "aterm fabric mint-for: {} held this cap at mode {was:03o}; now 0600",
                    path.display()
                ),
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
            }
            false
        }
        Ok(Some(_)) => {
            return Err(format!(
                "{} exists and holds a different cap; remove it or choose another --out",
                path.display()
            ));
        }
        Ok(None) => {
            write_atomic(path, lines.as_bytes(), 0o600)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            true
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    // WHAT THE CALLER IS ABOUT TO SAY OF THE FILE IS WHAT THE READERS JUDGE: its mode can be 0600
    // and an access list still let others in, one a folder's inheriting entry gave it.
    if let Err(e) = crate::bridge::read_cap_file(&path.to_string_lossy()) {
        let carries_an_entry = matches!(
            astream_cap::capfile::refusal_of(&e),
            Some(astream_cap::capfile::Refusal::File(
                astream_sys::NotPrivate::Acl
            ))
        );
        if !wrote || !carries_an_entry {
            return Err(e.to_string());
        }
        // What this run wrote is gone again, so nobody else can read it from here on. The entry is
        // the folder's, a choice of its owner's, so aterm does not clear it from the file it gave
        // nor from the folder: `--out` names the place, and a person can name another.
        let gone = std::fs::remove_file(path)
            .err()
            .map_or_else(String::new, |rm| {
                format!(" ({} could not be removed: {rm})", path.display())
            });
        return Err(format!(
            "the access list of {}'s folder lets other users into every new file there, so no \
             cap is kept there{gone}: choose another --out, or clear the folder's with `chmod -N`, \
             and mint again ({e})",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

fn person_guidance(person: &str, fleet: &str, broker: &str, cap: &str) -> String {
    let human = person.starts_with("h-");
    let halt = if human {
        format!(
            "  aterm fabric halt --cap-file {cap} <reason>    hold every session on the fleet\n  \
             aterm fabric halt --cap-file {cap} off         withdraw that halt (with the mesh on: \
             every halt that stands here)\n  \
             aterm fabric halt --cap-file {cap} off <id>    with the mesh on: lift that halt on \
             every Mac (`--here`: on this Mac alone)\n"
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
        "with it, {person} uses the fleet (it IS {person}: keep it 0600, and aterm refuses one \
         others can read):\n\
         {halt}  \
         aterm fabric inbox --cap-file {cap}            read what agents posted to={person}\n  \
         aterm fabric post --cap-file {cap} to=@<sid>@<node> kind=answer re=<off> <text>\n  \
         aterm link notify --fleet {fleet} --broker {broker} --cap-file {cap} --on \
         ask:{person} --exec <cmd>    be paged when an agent asks {person} (one notifier per \
         state dir: add selectors to its --on, or give another its own --state)\n\
         {task}"
    )
}

fn read_existing_cap(
    path: &Path,
    expected_len: usize,
) -> io::Result<Option<(std::fs::File, Vec<u8>)>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let cap = u64::try_from(expected_len.saturating_add(1)).unwrap_or(u64::MAX);
    let mut bytes = Vec::with_capacity(
        usize::try_from(meta.len().min(cap).saturating_add(1))
            .unwrap_or(0)
            .min(4097),
    );
    (&file).take(cap).read_to_end(&mut bytes)?;
    Ok(Some((file, bytes)))
}

/// Make `file` 0600 if it is not.
/// # Errors
/// The stat or the chmod.
fn private_mode(file: &std::fs::File) -> io::Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    let was = file.metadata()?.permissions().mode() & 0o777;
    if was == 0o600 {
        return Ok(None);
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(was))
}

#[cfg(test)]
#[path = "person_tests.rs"]
mod tests;
