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
         ask:{person} --exec <cmd>    be paged when an agent asks {person}\n\
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
mod tests {
    use super::*;

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

    #[test]
    fn a_person_ring_stamped_with_an_expiry_reads_its_expiry() {
        let exp = 1_800_000_000_000u64;
        let stamped: Vec<String> = crate::enable::person_rules("lab", "h-andrew")
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
        let id = "0123456789abcdef0123456789abcdef";
        for (tail, here) in [("", false), (" --here", true)] {
            let lift = parse("halt", &words(&format!("off {id}{tail} --cap-file c")))
                .unwrap()
                .unwrap();
            let want = Act::Lift {
                id: id.to_string(),
                here,
            };
            assert_eq!(lift.act, want, "off <id>{tail}");
        }
        assert!(
            parse("halt", &words("off 0123 --cap-file c")).is_err(),
            "not a halt id"
        );
        let flag_first = parse("halt", &words(&format!("--here --cap-file c off {id}")));
        assert!(
            matches!(
                flag_first,
                Ok(Some(PersonCmd {
                    act: Act::Lift { here: true, .. },
                    ..
                }))
            ),
            "{flag_first:?}"
        );
        assert!(
            parse("halt", &words(&format!("off {id} extra --cap-file c"))).is_err(),
            "a mistyped halt-off never halts"
        );
        assert!(
            parse("halt", &words("--here --cap-file c main is broken")).is_err(),
            "a halt is never for one Mac"
        );
        assert!(
            parse("halt", &words("--here off --cap-file c")).is_err(),
            "--here names an id"
        );
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
        for re in ["5", "@5"] {
            let post = parse(
                "post",
                &words(&format!("--cap-file c to=@s-a@n-b re={re} hi")),
            )
            .unwrap()
            .unwrap();
            assert!(
                matches!(post.act, Act::Post { re: Some(5), .. }),
                "re={re}: {:?}",
                post.act
            );
        }
        for re in ["+5", "@", "", "@-1"] {
            let refused = parse(
                "post",
                &words(&format!("--cap-file c to=@s-a@n-b re={re} hi")),
            );
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|e| e.contains(&format!("re={re}"))),
                "re={re}: refused, naming the key and the value: {refused:?}"
            );
        }
        for bad in [
            "--cap-file c to=@s-a kind=note hi",
            "--cap-file c to=s-a@n-b kind=note hi",
            "--cap-file c to=@s-a@n-b kind=ack hi",
            "--cap-file c to=@s-a@n-b kind=note",
            "--cap-file c kind=note hi",
            "--cap-file c to=@s-a@n-b re=x note hi",
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
        let mut grants = crate::enable::person_rules("f", person);
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
        crate::bridge::write_owner_only(&cap_path, caps);
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
            mesh: None,
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

    const STAND_IN: Bounds = Bounds {
        request: aterm_spec::wait::MUST_HAPPEN,
        subscription: aterm_spec::wait::MUST_HAPPEN,
    };

    /// A stand-in broker for ONE `inbox` over `conns` connections, at astream's
    /// wire: the head query answers `end`, the group subscription is sent
    /// `tail` (nothing, for an empty one), and every `Commit` is recorded before
    /// its ack, so once `inbox` returns the record is final.
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
                    match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Blocking,
                    ) {
                        Ok((stream, _)) => {
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
                        Some(Request::IdleMarks | Request::GapNotices) => vec![mark(end, end)],
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
    /// then delivers the mail at @7 and marks the end.
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

    #[cfg(unix)]
    #[test]
    fn an_inbox_says_what_retention_gave_up_and_reads_on() {
        use astream_broker::Response;
        let (_dir, cfg, commits) = stand_in(
            10,
            vec![
                Response::Gap(astream_broker::Gap {
                    from: 0,
                    to: 6,
                    records: Some(2),
                }),
                Response::Delivery {
                    offset: 7,
                    subject: "/f/f/in/p/h-x/n-peer/note".into(),
                    body: b"v=1 t=1 text=mail".to_vec(),
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
        inbox(&cfg, &cmd, "h-x", false, &mut out, STAND_IN).expect("the inbox reads past the gap");
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(
            lines[0].starts_with("given up: ") && lines[0].contains("2 record(s) in @0..@6"),
            "{out}"
        );
        assert!(lines[1].starts_with("@7  "), "{out}");
        assert_eq!(
            *commits.lock().unwrap(),
            [("/f/f/cur/p/h-x/inbox".to_string(), 9)]
        );
    }

    /// **THE SUBSCRIPTION IS HELD TO ITS OWN BOUND, NOT A REQUEST'S.** A broker
    /// that takes the subscription and then says nothing: the read ends at the
    /// subscription's bound, in words, with nothing marked read.
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
    /// last message on every run.
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
        let (mut conn, _closer) =
            connect_within(&cfg, &cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
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
        let (mut conn, _closer) =
            connect_within(&cfg, &cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
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

    #[test]
    fn only_a_humans_ring_can_publish_a_halt() {
        let secret = [7u8; 32];
        let can = |person: &str, subject: &str| {
            crate::enable::person_rules("f1", person).iter().any(|g| {
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

    /// **WITH THE MESH ON, A PERSON'S POST TO ANOTHER MAC IS SEALED, OR REFUSED BEFORE ANYTHING
    /// IS WRITTEN** (§11 C3c): to a member the node service's hand-off names, once the node
    /// service vouched for this Mac's person key, the post is written on the person's lane to that
    /// member, sealed so it verifies under this Mac's node key, and to the member a retired node
    /// became; while there is no hand-off, before the person key is vouched for, to a node that is
    /// no member, or while this Mac has no node key, it is refused (exit 2) and nothing is
    /// written. One to this Mac's own session is written unsealed, as before.
    /// NEGATIVE CONTROL: with the mesh off, a post to another Mac is written, unsealed.
    #[cfg(unix)]
    #[test]
    fn with_the_mesh_on_a_persons_post_to_another_mac_is_sealed_or_refused_before_anything_is_written(
    ) {
        use crate::federate::{node_of, Handoff, MeshPaths};
        use crate::person_key::{self, CERT_TOKEN};
        let (dir, _server, mut cfg, mut cmd) = guarded_person("h-andrew", false);
        let keys = dir.path().join("keys");
        aterm_uds::private_file::ensure_private_dir(&keys).unwrap();
        let seed = [5u8; 32];
        let text = format!("{}\n", aterm_codec::hex::encode_lower(&seed));
        aterm_uds::private_file::write_private(
            &keys,
            crate::federate::NODE_KEY_FILE,
            text.as_bytes(),
        )
        .unwrap();
        let node_key = astream_aead::signature::public_key(&seed);
        let here = node_of(&node_key);
        let there = node_of(&astream_aead::signature::public_key(&[6u8; 32]));
        let retired = "n-00000000000000ee";
        let stranger = "n-00000000000000ff";
        // A reader of every lane, minted under the broker's secret: a person's ring reads its own.
        let mut reader = astream_broker::Client::connect(dir.path().join("bus.sock")).unwrap();
        let cap = astream_cap::mint(&[0x51; 32], "ro:/f/f/in/>").unwrap();
        reader.attach(&cap.filter, &cap.tag).unwrap();
        let mut held = || reader.fetch(0, "/f/f/in/>", 64).unwrap().0;
        let to = |node: &str| Act::Post {
            sid: "s-a".to_string(),
            node: node.to_string(),
            kind: "note".to_string(),
            re: None,
            text: "hi".to_string(),
        };
        let mut posted = |cfg: &Config, act: Act| {
            cmd.act = act;
            main("post", &cmd, cfg)
        };
        let mesh = |keys: &std::path::Path| {
            Some(MeshPaths {
                keys: keys.to_path_buf(),
                root: dir.path().join("keys"),
            })
        };
        cfg.mesh = mesh(&keys);
        assert_eq!(posted(&cfg, to(&there)), ExitCode::from(2), "no hand-off");
        Handoff {
            me: here.clone(),
            members: [here.clone(), there.clone()].into(),
            moved: [(retired.to_owned(), there.clone())].into(),
            rank: std::collections::BTreeMap::new(),
            gone: std::collections::BTreeSet::new(),
        }
        .write(&keys)
        .unwrap();
        assert_eq!(
            posted(&cfg, to(&there)),
            ExitCode::from(2),
            "no person key yet"
        );
        assert!(held().is_empty(), "a refused post writes nothing");

        assert!(person_key::vouch(&keys, &seed, crate::now_ms()).is_ok());
        let person = person_key::PersonKey::read(&keys)
            .unwrap()
            .unwrap()
            .public();
        for node in [there.as_str(), retired] {
            assert_eq!(posted(&cfg, to(node)), ExitCode::SUCCESS, "to {node}");
        }
        let rows = held();
        let lane = format!("/f/f/in/{there}/s-a/h-andrew/note");
        assert_eq!(rows.len(), 2);
        for (_, subject, raw) in &rows {
            assert_eq!(subject, &lane, "on the person's lane to the member");
            let (body, _) = Body::decode(raw);
            assert_eq!(body.text, "hi");
            let address = person_key::post_address("f", "s-a", "h-andrew", "note");
            assert_eq!(
                person_key::verify(
                    person_key::Signer::Key(&node_key),
                    &address,
                    "note",
                    &body,
                    crate::now_ms()
                ),
                Ok(person),
                "sealed under this Mac's node key"
            );
        }
        assert_eq!(posted(&cfg, to(stranger)), ExitCode::from(2), "no member");
        assert_eq!(held().len(), 2);
        assert_eq!(posted(&cfg, to(&here)), ExitCode::SUCCESS, "this Mac");
        let rows = held();
        assert_eq!(rows.len(), 3);
        assert!(
            !Body::decode(&rows[2].2).0.unknown.contains_key(CERT_TOKEN),
            "a post to this Mac's own session is not sealed"
        );
        cfg.mesh = mesh(&dir.path().join("no-key"));
        assert_eq!(posted(&cfg, to(&here)), ExitCode::from(2), "no node key");
        assert_eq!(held().len(), 3);
        cfg.mesh = None;
        assert_eq!(posted(&cfg, to(stranger)), ExitCode::SUCCESS, "the control");
        let rows = held();
        assert_eq!(rows.len(), 4, "with the mesh off the post is written");
        assert!(!Body::decode(&rows[3].2).0.unknown.contains_key(CERT_TOKEN));
    }

    /// **A PERSON'S ANSWER TO ANOTHER MAC NAMES THE POST IT ANSWERS AS THAT MAC NUMBERS IT, OR IS
    /// REFUSED** (§11 C3d): `re=` names a post this Mac's bridge handed the person, on the
    /// person's lane of this Mac's node or of a node its seat held before, from the session the
    /// answer goes to (`<sid>@<n>`, `n` that session's member or a node that member's seat held),
    /// brought by that member's producer or one of a node its seat held, and the answer carries
    /// the offset that post has on the asker's Mac, which the bridge stamped on it
    /// ([`WIRE_RE_TOKEN`]). An `re=` that names another session's post, a post of that session
    /// id on another Mac, one with no stamp, a stamp out of range, of another format or under
    /// another Mac's producer, a post on another node's lane, or no post at all, is refused
    /// (exit 2) and nothing is written: an offset on this Mac names nothing on another.
    /// NEGATIVE CONTROLS: to this Mac's own session, named by its node or by a node its seat held
    /// before, and with the mesh off, `re=` is kept as given.
    #[cfg(unix)]
    #[test]
    fn a_persons_answer_to_another_mac_names_the_post_it_answers_as_that_mac_numbers_it_or_is_refused(
    ) {
        use crate::federate::{node_of, Handoff, MeshPaths};
        let (dir, _server, mut cfg, mut cmd) = guarded_person("h-andrew", false);
        let keys = dir.path().join("keys");
        aterm_uds::private_file::ensure_private_dir(&keys).unwrap();
        let seed = [5u8; 32];
        let text = format!("{}\n", aterm_codec::hex::encode_lower(&seed));
        aterm_uds::private_file::write_private(
            &keys,
            crate::federate::NODE_KEY_FILE,
            text.as_bytes(),
        )
        .unwrap();
        let here = node_of(&astream_aead::signature::public_key(&seed));
        let there = node_of(&astream_aead::signature::public_key(&[6u8; 32]));
        let (retired, old_here) = ("n-00000000000000ee", "n-00000000000000dd");
        let stranger = "n-00000000000000ff";
        Handoff {
            me: here.clone(),
            members: [here.clone(), there.clone()].into(),
            moved: [
                (retired.to_owned(), there.clone()),
                (old_here.to_owned(), here.clone()),
            ]
            .into(),
            rank: std::collections::BTreeMap::new(),
            gone: std::collections::BTreeSet::new(),
        }
        .write(&keys)
        .unwrap();
        assert!(crate::person_key::vouch(&keys, &seed, crate::now_ms()).is_ok());
        let socket = dir.path().join("bus.sock");
        // What this Mac's bridge hands the person: a copy on the person's lane of `node`.
        let mut seq = 0;
        let mut handed = |node: &str, from: &str, stamp: Option<String>| {
            let mut writer = astream_broker::Client::connect(&socket).unwrap();
            let grant = format!("rw,p={node}:/f/f/in/p/h-andrew/{node}/ask");
            let cap = astream_cap::mint(&[0x51; 32], &grant).unwrap();
            writer.attach(&cap.filter, &cap.tag).unwrap();
            let mut body = Body::new(crate::now_ms());
            body.from = Some(from.to_owned());
            body.text = "ship it?".to_owned();
            if let Some(stamp) = stamp {
                body.unknown.insert(WIRE_RE_TOKEN.to_owned(), stamp);
            }
            seq += 1;
            let subject = format!("/f/f/in/p/h-andrew/{node}/ask");
            let producer = astream_cap::producer_id_of(node);
            writer
                .publish(producer, seq, &subject, &body.encode())
                .unwrap()
                .0
        };
        let by = |seq: u64, node: &str| Some(wire_stamp(seq, astream_cap::producer_id_of(node)));
        let from_a = format!("s-a@{there}");
        let current = handed(&here, &from_a, by(7, &there));
        let via_retired = handed(&here, &format!("s-a@{retired}"), by(9, retired));
        let pushed_after_rotation = handed(&here, &format!("s-a@{retired}"), by(10, &there));
        let before_rotation = handed(old_here, &from_a, by(11, &there));
        let another_session = handed(&here, &format!("s-other@{there}"), by(13, &there));
        let same_sid_here = handed(&here, &format!("s-a@{here}"), by(17, &there));
        let same_sid_stranger = handed(&here, &format!("s-a@{stranger}"), by(19, &there));
        let same_sid_old_here = handed(&here, &format!("s-a@{old_here}"), by(21, &there));
        let unstamped = handed(&here, &from_a, None);
        let out_of_range = handed(&here, &from_a, by(astream_broker::ACK_SEQ_BASE, &there));
        let bare_seq = handed(&here, &from_a, Some("23".to_owned()));
        let another_producer = handed(&here, &from_a, by(25, stranger));
        let another_lane = handed(stranger, &from_a, by(15, &there));
        // A session of this Mac asked before its key rotated: the bridge wrote it unstamped, from
        // the bare session, on the person's lane of the node it was then.
        let local_before_rotation = handed(old_here, "s-a", None);

        let mut reader = astream_broker::Client::connect(&socket).unwrap();
        let cap = astream_cap::mint(&[0x51; 32], "ro:/f/f/in/>").unwrap();
        reader.attach(&cap.filter, &cap.tag).unwrap();
        let mut answers = |node: &str| -> Vec<Option<u64>> {
            let lane = format!("/f/f/in/{node}/s-a/h-andrew/answer");
            let (rows, _) = reader.fetch(0, &lane, 64).unwrap();
            rows.iter()
                .map(|(_, _, raw)| Body::decode(raw).0.re)
                .collect()
        };
        let answer = |node: &str, re: u64| Act::Post {
            sid: "s-a".to_string(),
            node: node.to_string(),
            kind: "answer".to_string(),
            re: Some(re),
            text: "yes".to_string(),
        };
        let mut posted = |cfg: &Config, act: Act| {
            cmd.act = act;
            main("post", &cmd, cfg)
        };
        cfg.mesh = Some(MeshPaths {
            keys: keys.clone(),
            root: keys.clone(),
        });
        for (node, re) in [
            (there.as_str(), current),
            (there.as_str(), via_retired),
            (retired, via_retired),
            (there.as_str(), pushed_after_rotation),
            (there.as_str(), before_rotation),
        ] {
            assert_eq!(
                posted(&cfg, answer(node, re)),
                ExitCode::SUCCESS,
                "re={re} to s-a@{node}"
            );
        }
        assert_eq!(
            answers(&there),
            [Some(7), Some(9), Some(9), Some(10), Some(11)],
            "each names the post as the asker's Mac numbers it"
        );
        for (why, re) in [
            ("another session's post", another_session),
            ("that session id's post on this Mac", same_sid_here),
            (
                "that session id's post on a node that is no member",
                same_sid_stranger,
            ),
            (
                "that session id's post on a node this Mac held",
                same_sid_old_here,
            ),
            ("an unstamped post", unstamped),
            ("a stamp out of range", out_of_range),
            ("a stamp of another format", bare_seq),
            ("a stamp under another Mac's producer", another_producer),
            ("a post on another node's lane", another_lane),
            ("no post", local_before_rotation + 1000),
        ] {
            assert_eq!(
                posted(&cfg, answer(&there, re)),
                ExitCode::from(2),
                "{why} is refused"
            );
        }
        assert_eq!(answers(&there).len(), 5, "a refused answer writes nothing");
        assert_eq!(
            posted(&cfg, answer(&here, unstamped)),
            ExitCode::SUCCESS,
            "this Mac's own session"
        );
        assert_eq!(
            posted(&cfg, answer(old_here, local_before_rotation)),
            ExitCode::SUCCESS,
            "this Mac's own session, named by a node its seat held before"
        );
        assert_eq!(
            answers(&here),
            [Some(unstamped), Some(local_before_rotation)],
            "keeps re= as given"
        );
        cfg.mesh = None;
        assert_eq!(
            posted(&cfg, answer(&there, another_session)),
            ExitCode::SUCCESS,
            "the mesh off"
        );
        assert_eq!(
            answers(&there).last(),
            Some(&Some(another_session)),
            "with the mesh off, re= is kept as given"
        );
    }

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
            crate::bridge::write_owner_only(&path, lines);
            path.to_string_lossy().into_owned()
        };
        let person = write("h.cap", &crate::enable::person_rules("f1", "h-andrew"));
        let node = write(
            "n.cap",
            &crate::enable::node_rules("f1", "n-00000000000000aa"),
        );
        let other = write("o.cap", &crate::enable::person_rules("f1", "h-other"));
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
