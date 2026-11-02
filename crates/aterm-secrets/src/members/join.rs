// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The joining Mac's flow (`aterm members join`, `docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §3.3.1): ask every member it found, all at once; then run the phrase a person typed with the
//! member that shows it, write `~/.secrets` from that member's bundle (F10's interim handover),
//! ask the vault's server with the `Allowed`, and tell the other members it was let in. How a
//! member is reached is the caller's: the CLI dials the `ask` ALPN, a test a stream pair.

use super::bundle::Handed;
use super::desk::Answer;
use super::joiner::{self, Greeting, Joining};
use crate::client::{self, Paths};
use crate::materialize::Report;
use crate::phrase::Phrase;
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::io::{Read, Write};

pub use crate::alab_join::{JOIN_QUIET, JOINING, LET_IN};

/// A stream to one member's `ask` op.
pub trait Stream: Read + Write + Send {}

impl<T: Read + Write + Send + ?Sized> Stream for T {}

/// How the joining Mac reaches the member at an address: a fresh stream each call, each bound
/// by its own deadline.
pub type Dial<'a> = dyn Fn(&str) -> Result<Box<dyn Stream>> + Sync + 'a;

/// One member that answered: where it is, its greeting, and what it said of this Mac.
#[derive(Clone, Debug)]
pub struct Heard {
    /// The address it was dialed at.
    pub at: String,
    /// What it said first.
    pub greeting: Greeting,
    /// What it said of this Mac's request.
    pub answer: Answer,
    /// Whether its answer proved the greeting's key on that connection ([`joiner::ask_checked`]).
    pub signed: joiner::Signed,
}

/// Ask every member at `ats`, all at once, each within its dial's own deadline: the members
/// that answered, and why each other one did not.
#[must_use]
pub fn ask_all(
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    ats: &[String],
) -> (Vec<Heard>, Vec<(String, String)>) {
    let answers: Vec<(String, Result<Heard>)> = std::thread::scope(|scope| {
        let asking: Vec<_> = ats
            .iter()
            .map(|at| (at, scope.spawn(move || ask_one(dial, joining, at))))
            .collect();
        asking
            .into_iter()
            .map(|(at, asked)| {
                let asked = asked
                    .join()
                    .unwrap_or_else(|_| Err(Error::new("the ask stopped")));
                (at.clone(), asked)
            })
            .collect()
    });
    let mut heard = Vec::new();
    let mut missed = Vec::new();
    for (at, answer) in answers {
        match answer {
            Ok(answer) => heard.push(answer),
            Err(error) => missed.push((at, error.to_string())),
        }
    }
    (heard, missed)
}

fn ask_one(dial: &Dial<'_>, joining: &Joining<'_>, at: &str) -> Result<Heard> {
    let mut stream = dial(at)?;
    let greeting = joiner::greeting(&mut stream)?;
    let (answer, signed) = joiner::ask_checked(&mut stream, &greeting, joining)?;
    Ok(Heard {
        at: at.to_owned(),
        greeting,
        answer,
        signed,
    })
}

/// Whether `member` (`--member`) names `heard`: its machine name, the address it was dialed at
/// (`host:port`, or the host alone), or a prefix of at least 8 hex digits of its key.
fn named(heard: &Heard, member: &str) -> bool {
    let key = aterm_codec::hex::encode_lower(&heard.greeting.key);
    let lower = member.to_ascii_lowercase();
    heard.greeting.name == member
        || heard.at == member
        || heard.at.rsplit_once(':').is_some_and(|(host, _)| {
            host == member
                || host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) == Some(member)
        })
        || (lower.len() >= 8
            && lower.bytes().all(|b| b.is_ascii_hexdigit())
            && key.starts_with(&lower))
}

/// `heard` without the answers another answer outranks by what it proved ([`joiner::Signed`]),
/// before any tier is chosen: an answer that proved no key beside one that proved the key its
/// greeting names (a host that copies a member's greeting signs nothing, whatever it answers, so
/// it is no candidate beside that member, even saying "allowed" where the member shows an
/// invite), and a second answer that proved a key one already proved (one member reached at two
/// addresses, its Wi-Fi and its Ethernet, is one candidate). An answer that proved nothing under
/// a key no answer proved stays: a member from before signed greetings is a candidate as before,
/// and beside a member that signs it is a second one, which a person names.
fn outranked(heard: &[Heard]) -> Vec<&Heard> {
    let proved: BTreeSet<[u8; 32]> = heard
        .iter()
        .filter(|heard| heard.signed == joiner::Signed::Verified)
        .map(|heard| heard.greeting.key)
        .collect();
    let mut kept = BTreeSet::new();
    heard
        .iter()
        .filter(|heard| match heard.signed {
            joiner::Signed::Verified => kept.insert(heard.greeting.key),
            joiner::Signed::Unsigned => !proved.contains(&heard.greeting.key),
        })
        .collect()
}

/// The member a typed phrase goes to, among the answers no other outranks ([`outranked`]): the
/// one `member` names ([`named`]); else the one that said a person allowed this Mac; else the one
/// that shows an invite. A phrase sent to another member would burn that member's own phrase, so
/// none or several is an error that says what to do, each candidate by its address, which `--at`
/// takes.
///
/// # Errors
/// No member, or more than one, fits.
pub fn pick<'h>(heard: &'h [Heard], member: Option<&str>) -> Result<&'h Heard> {
    let names = |fits: &[&Heard]| {
        fits.iter()
            .map(|heard| format!("{} at {}", heard.greeting.name, heard.at))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let candidates = outranked(heard);
    let fits: Vec<&Heard> = match member {
        Some(member) => candidates
            .into_iter()
            .filter(|heard| named(heard, member))
            .collect(),
        None => {
            let (allowed, rest): (Vec<&Heard>, Vec<&Heard>) = candidates
                .into_iter()
                .partition(|heard| heard.answer == Answer::Allowed);
            if allowed.is_empty() {
                rest.into_iter()
                    .filter(|heard| heard.greeting.invite)
                    .collect()
            } else {
                allowed
            }
        }
    };
    match fits.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::new(match member {
            Some(member) => format!(
                "no ALab member named {member} answers here; --at <host:port> asks the one at \
                 that address alone"
            ),
            None => "no member shows a phrase for this Mac yet: press Allow on one, then \
                     type the phrase it shows"
                .to_owned(),
        })),
        several => Err(Error::new(format!(
            "{} members show a phrase ({}): name the one whose phrase you type with --member \
             <name>, or ask it alone with --at <host:port>",
            several.len(),
            names(several)
        ))),
    }
}

/// How the vault holds a Mac a run let in.
#[derive(Debug)]
pub enum Admission {
    /// The membership log lets it in by the member's `Admit` (F15): it reads the vault from the
    /// log, with no admin. It still posts its `Allowed`, so an admin's approver names it in
    /// `[machines]` and `aterm-swarm-roster`, which 0.145 Macs read (the fleet admits it by the
    /// log since F17); that changes nothing it reads.
    Logged,
    /// F10's interim, for a vault with no log: the vault server's answer to this Mac's request
    /// with its `Allowed`, its join code or why no server took it. An admin's approver records
    /// the admission once one runs.
    Asked(std::result::Result<String, String>),
}

/// What a run with the phrase did.
#[derive(Debug)]
pub struct Joined {
    /// The member that let this Mac in.
    pub member: String,
    /// The machine name it was allowed under.
    pub name: String,
    /// What the bundle wrote to `~/.secrets`.
    pub report: Report,
    /// How the vault holds this Mac now.
    pub admission: Admission,
}

/// Why [`run_phrase`] let nobody in, and whether the phrase went to the member first.
#[derive(Debug)]
pub enum PhraseFailed {
    /// Before the phrase's run went to a member ([`joiner::run_noting_send`]): no member fits
    /// ([`pick`]: none shows a phrase for this Mac, or several do), the one picked could not be
    /// dialed or greeted, it changed since it was asked, or this Mac could not start the run.
    /// Nothing was asked of the phrase, so the same words go again.
    Unsent(Error),
    /// After the phrase's run went to the member: it refused the phrase or the run broke off,
    /// or the bundle it handed could not be written. A phrase is good for one try.
    Sent(Error),
}

impl PhraseFailed {
    /// Whether the phrase went to a member ([`Self::Sent`]).
    #[must_use]
    pub const fn sent(&self) -> bool {
        matches!(self, Self::Sent(_))
    }
}

impl std::fmt::Display for PhraseFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsent(error) | Self::Sent(error) => error.fmt(formatter),
        }
    }
}

/// Run `phrase` with the member [`pick`] chooses from `heard`, write this Mac's vault
/// configuration and `~/.secrets` from its bundle (from the membership log where the bundle
/// carries it, else from the member's `[everyone]` files, after which this Mac asks the vault's
/// server with its `Allowed`), and tell every other member in `heard` that this Mac was let in.
///
/// A member busy with a vault pass answers before its run touches the phrase, naming how long
/// the phrase has left (`left=`): this Mac says so once (`say`, one line) and runs the same
/// phrase with it again [`BUSY_AGAIN_AFTER`] after each busy answer while the phrase can live
/// (from a member that names no life, the phrase's whole lifetime from now).
///
/// # Errors
/// [`PhraseFailed::Unsent`]: no member fits, the member could not be reached or changed since it
/// was asked, the run could not start, or the member stayed busy until the phrase lapsed.
/// [`PhraseFailed::Sent`]: the run failed (a wrong phrase burns it), or the bundle could not be
/// written.
pub fn run_phrase(
    paths: &Paths,
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    heard: &[Heard],
    phrase: &Phrase,
    member: Option<&str>,
    say: &mut dyn FnMut(&str),
) -> std::result::Result<Joined, PhraseFailed> {
    let chosen = pick(heard, member).map_err(PhraseFailed::Unsent)?;
    // A member busy with a vault pass answers before its run touches the phrase: this Mac asks
    // it again by itself while the phrase can still live, so the person types it once.
    let lives = if chosen.answer == Answer::Allowed {
        crate::pake::ALLOW_LIFETIME
    } else {
        crate::pake::INVITE_LIFETIME
    };
    let mut until = std::time::Instant::now() + lives;
    let mut said = false;
    let (greeting, bundle) = loop {
        let mut left = None;
        match run_once(paths, dial, joining, chosen, phrase, &mut left) {
            Err(PhraseFailed::Unsent(error)) if error.kind() == crate::ErrorKind::MemberBusy => {
                let now = std::time::Instant::now();
                if let Some(left) = left {
                    until = until.min(now + left);
                }
                if !said {
                    said = true;
                    say(&format!(
                        "{} is busy with a vault sync; this Mac asks it again while the phrase lasts",
                        chosen.greeting.name
                    ));
                }
                if now + BUSY_AGAIN_AFTER >= until {
                    return Err(PhraseFailed::Unsent(Error::of_kind(
                        crate::ErrorKind::MemberBusy,
                        format!(
                            "{} stayed busy with a vault sync until the phrase lapsed; press \
                             Allow on it again for a new phrase",
                            chosen.greeting.name
                        ),
                    )));
                }
                std::thread::sleep(BUSY_AGAIN_AFTER);
            }
            ran => break ran?,
        }
    };
    written(paths, dial, joining, heard, chosen, greeting, bundle).map_err(PhraseFailed::Sent)
}

/// How long a joining Mac waits after a busy answer before it asks that member again
/// ([`run_phrase`]); the member itself waits up to `client::HANDOVER_LOCK_WAIT` for its vault
/// pass on each ask, so a round takes up to about two seconds.
pub const BUSY_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// One run of `phrase` with `chosen`, on a fresh connection: its greeting (checked against the
/// one it answered with) and its bundle, acknowledged once this Mac's write would take it
/// (`client::accepts_bundle`). A busy member's answer sets `left` to the phrase life it names.
fn run_once(
    paths: &Paths,
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    chosen: &Heard,
    phrase: &Phrase,
    left: &mut Option<std::time::Duration>,
) -> std::result::Result<(Greeting, super::Bundle), PhraseFailed> {
    let mut stream = dial(&chosen.at).map_err(PhraseFailed::Unsent)?;
    let greeting = joiner::greeting(&mut stream).map_err(PhraseFailed::Unsent)?;
    if greeting.key != chosen.greeting.key {
        return Err(PhraseFailed::Unsent(Error::new(format!(
            "the member at {} is not the one that answered before; ask again",
            chosen.at
        ))));
    }
    let mut sending = joiner::Sending::default();
    let accepts = |bundle: &super::Bundle| client::accepts_bundle(paths, bundle);
    let ran = joiner::run_noting_send(
        &mut stream,
        &greeting,
        joining,
        phrase,
        &mut sending,
        &accepts,
    );
    *left = sending.left;
    let bundle = ran.map_err(|error| {
        if sending.sent {
            PhraseFailed::Sent(error)
        } else {
            PhraseFailed::Unsent(error)
        }
    })?;
    Ok((greeting, bundle))
}

/// Prove this Mac to the members at `ats` with nobody's Allow (`desk::serve_with`'s `prove`):
/// its request's writer key and its fleet node key (`sign_node`) sign each member's fresh
/// challenge, and it answers the possession challenge. The first member that lets it in hands the
/// log bundle (`client::admit_by_proof`), which this Mac writes and then follows. Members are
/// tried one after another in address order, never at once, and none after one that may have
/// signed (its possession answer went): two members removing one old seat at one cutoff would
/// make a quorum `Remove`, which no Allow undoes, and the next try reaches the same member first,
/// which then signs nothing new. Where none let it in, why each refused, by address: this Mac then
/// asks a person as before.
///
/// # Errors
/// No member let it in ([`Missed`]).
pub fn prove_first(
    paths: &Paths,
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    sign_node: joiner::SignNode<'_>,
    ats: &[String],
) -> std::result::Result<Joined, Missed> {
    let mut ats = ats.to_vec();
    ats.sort();
    ats.dedup();
    let mut missed = Vec::new();
    for at in &ats {
        let mut sent = false;
        let tried = (|| {
            let mut stream = dial(at)?;
            let greeting = joiner::greeting(&mut stream)?;
            let bundle = joiner::prove(&mut stream, &greeting, joining, sign_node, &mut sent)?;
            drop(stream);
            if !matches!(bundle.handed, Handed::Log { .. }) {
                return Err(Error::new(
                    "a member handed files, not the log, for a proof",
                ));
            }
            let report = client::write_from_log_bundle(paths, &bundle)?;
            // Best effort, as after a phrase: the servers and this Mac's subscriber follow on.
            drop(client::sync(paths, None));
            drop(client::ask_allowed(paths));
            Ok(Joined {
                member: greeting.name,
                name: bundle.allowed.name().to_owned(),
                report,
                admission: Admission::Logged,
            })
        })();
        match tried {
            Ok(joined) => return Ok(joined),
            Err(error) => {
                missed.push((at.clone(), error.to_string()));
                if sent {
                    return Err(Missed {
                        refused: missed,
                        signed_maybe: true,
                    });
                }
            }
        }
    }
    Err(Missed {
        refused: missed,
        signed_maybe: false,
    })
}

/// Why [`prove_first`] let this Mac in nowhere.
#[derive(Debug)]
pub struct Missed {
    /// Why each member it tried refused or failed, by address.
    pub refused: Vec<(String, String)>,
    /// The last one tried may have signed this Mac's records (its possession answer went): try no
    /// other member, on any path, until that one's records had time to reach the others.
    pub signed_maybe: bool,
}

/// [`run_phrase`] once `chosen` handed its `bundle`: write it, and tell the other members.
fn written(
    paths: &Paths,
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    heard: &[Heard],
    chosen: &Heard,
    greeting: Greeting,
    bundle: super::Bundle,
) -> Result<Joined> {
    let (report, admission) = match bundle.handed {
        Handed::Log { .. } => {
            let report = client::write_from_log_bundle(paths, &bundle)?;
            // Best effort: the vault's servers follow from here, and an admin's approver names
            // this Mac in `[machines]` from its `Allowed`; this Mac's subscriber keeps trying.
            drop(client::sync(paths, None));
            drop(client::ask_allowed(paths));
            (report, Admission::Logged)
        }
        Handed::Files(_) => {
            let report = client::write_from_bundle(paths, &bundle)?;
            let asked = client::ask_allowed(paths).map_err(|error| error.to_string());
            (report, Admission::Asked(asked))
        }
    };
    for other in heard.iter().filter(|other| other.at != chosen.at) {
        let told = dial(&other.at).and_then(|mut stream| {
            let greeting = joiner::greeting(&mut stream)?;
            joiner::done(&mut stream, &greeting, joining)
        });
        drop(told);
    }
    Ok(Joined {
        member: greeting.name,
        name: bundle.allowed.name().to_owned(),
        report,
        admission,
    })
}
