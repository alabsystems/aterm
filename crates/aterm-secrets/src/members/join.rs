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
use std::io::{Read, Write};

/// What a Mac that asked does next: the words `aterm members join --no-wait`, `status` and
/// `aterm init` give it (§3.3.3).
pub const LET_IN: &str =
    "press Allow on any Mac in ALab, then run `aterm members join <phrase>` here";

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
    let answer = joiner::ask(&mut stream, &greeting, joining)?;
    Ok(Heard {
        at: at.to_owned(),
        greeting,
        answer,
    })
}

/// Whether `member` (`--member`) names `heard`: its machine name, the address it was dialed at
/// (`host:port`, or the host alone), or a prefix of at least 8 hex digits of its key.
fn named(heard: &Heard, member: &str) -> bool {
    let key = aterm_codec::hex::encode_lower(&heard.greeting.key);
    let lower = member.to_ascii_lowercase();
    heard.greeting.name == member
        || heard.at == member
        || heard
            .at
            .rsplit_once(':')
            .is_some_and(|(host, _)| host == member)
        || (lower.len() >= 8
            && lower.bytes().all(|b| b.is_ascii_hexdigit())
            && key.starts_with(&lower))
}

/// The member a typed phrase goes to: the one `member` names ([`named`]); else the one that said
/// a person allowed this Mac; else the one that shows an invite. A phrase sent to another member
/// would burn that member's own phrase, so none or several is an error that says what to do,
/// each candidate by its address, which `--at` takes.
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
    let fits: Vec<&Heard> = match member {
        Some(member) => heard.iter().filter(|heard| named(heard, member)).collect(),
        None => {
            let allowed: Vec<&Heard> = heard
                .iter()
                .filter(|heard| heard.answer == Answer::Allowed)
                .collect();
            if allowed.is_empty() {
                heard.iter().filter(|heard| heard.greeting.invite).collect()
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

/// Run `phrase` with the member [`pick`] chooses from `heard`, write this Mac's vault
/// configuration and `~/.secrets` from its bundle (from the membership log where the bundle
/// carries it, else from the member's `[everyone]` files, after which this Mac asks the vault's
/// server with its `Allowed`), and tell every other member in `heard` that this Mac was let in.
///
/// # Errors
/// No member fits, the member changed since it was asked, the run failed (a wrong phrase burns
/// it), or the bundle could not be written.
pub fn run_phrase(
    paths: &Paths,
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    heard: &[Heard],
    phrase: &Phrase,
    member: Option<&str>,
) -> Result<Joined> {
    let chosen = pick(heard, member)?;
    let mut stream = dial(&chosen.at)?;
    let greeting = joiner::greeting(&mut stream)?;
    if greeting.key != chosen.greeting.key {
        return Err(Error::new(format!(
            "the member at {} is not the one that answered before; ask again",
            chosen.at
        )));
    }
    let bundle = joiner::run(&mut stream, &greeting, joining, phrase)?;
    drop(stream);
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
