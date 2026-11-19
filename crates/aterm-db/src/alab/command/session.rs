// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who may see plaintext `aterm db` holds: fleet_db's `--show-passwords` and `dump <file>`
//! decide by the session, not the environment (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8). No
//! verb of the registry's tables shows a secret, so none asks.
//!
//! Standard input and output must both be terminals. Then every aterm instance on this Mac is
//! asked `reveal` on its control socket. Each names the session of its own the caller's lineage
//! leads to (by process ancestry or terminal session, never by `ATERM_SESSION_ID`), and refuses
//! one that `ctl spawn` started (on this Mac or through `--host`), one an update carried over,
//! one held for a remote driver or driven, and one that runs an agent in its tree; an instance
//! holding no such session answers `none`. Asking every instance, not the nearest ancestor, finds
//! an instance nested in another's tab, and a process reparented out of its tab
//! ([`combine`]). Only where no aterm instance places the caller in a session does the
//! agent-marker check stand in (`aterm_types::domain::agent_marker`, the rule behind `aterm
//! secrets get`'s display). An instance that does not answer refuses: the question is never
//! skipped because it failed.
//!
//! Where it is allowed decides how plaintext is shown ([`Place`]): in a person's aterm tab, in an
//! Editor tab beside that session (the session the instance named, never `ATERM_SESSION_ID`),
//! since text printed into a tab stays in its history; outside aterm, in that terminal.
//!
//! Whether a process runs under aterm is read from its lineage too: an ancestor running an aterm
//! image (`aterm_uds::left::is_aterm_image`), or `ATERM_SESSION_ID`. Such a process whose
//! instance cannot be found (the instance list unreadable, or a runtime folder pointed
//! elsewhere) refuses, so dropping a variable never turns the question into the marker check.
//! This is a guard against an agent reading a secret into its transcript by accident, not a
//! boundary against one that sets out to: a process of the owner's can read the vault itself.

use crate::{Error, Result};

/// The verdict word the instance answers for a session a person types in.
pub const ALLOWED: &str = "allowed";

/// The words of a `reveal` answer, as the instance spells them, with what each means.
const WORDS: &[(&str, &str)] = &[
    ("remote", "the question came from a remote driver"),
    (
        "indirect",
        "the instance could not tell which process asked",
    ),
    ("none", "no aterm session leads to this process"),
    ("spawned", "`aterm ctl spawn` started this session"),
    (
        "carried",
        "an aterm update carried this session over, with no record of who started or drove it: \
         open a new tab",
    ),
    ("held", "this session is held for a remote driver"),
    ("driven", "this session has been driven by `aterm ctl`"),
    ("agent", "an agent runs in this session"),
];

/// What this Mac's aterm instances answered, combined ([`combine`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// No aterm instance runs here (or none could be listed).
    NoInstance,
    /// The instances' combined verdict word, with the session the answering instance named
    /// (`session=<sid>`; `None` where it named none).
    Answered {
        word: String,
        session: Option<String>,
    },
    /// An instance could not be asked or answered out of grammar: why, never a value.
    Failed(String),
}

/// What decides a reveal: the terminals, the instance's answer, and the environment's markers.
#[derive(Clone, Debug)]
pub struct Facts {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub asked: Asked,
    /// An agent marker this process's environment carries.
    pub marker: Option<&'static str>,
    /// This process runs under aterm: an ancestor runs an aterm image, or `ATERM_SESSION_ID` is
    /// set.
    pub in_aterm: bool,
}

/// Where plaintext may be shown, once [`decide`] allows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A person's own aterm tab, session `sid` (its tab ID, `tqn5r6e0148`, or the `s-` and 20
    /// hex digits of a tab opened before that form): plaintext opens in an Editor tab beside it,
    /// never in the tab, whose text outlives the moment.
    Tab(String),
    /// No aterm session holds this process: the terminal is the person's own, and plaintext is
    /// printed in it.
    Outside,
}

/// Whether `facts` let this process reveal plaintext, and where.
///
/// # Errors
/// The refusal, in words that name the rule and never a value.
pub fn decide(facts: &Facts) -> Result<Place> {
    if !facts.stdin_tty || !facts.stdout_tty {
        return Err(Error::new(
            "plaintext is shown only at a terminal: standard input and output must both be one",
        ));
    }
    match &facts.asked {
        Asked::Answered { word, session } if word == ALLOWED => match session {
            Some(sid) => Ok(Place::Tab(sid.clone())),
            None => Err(Error::new(
                "plaintext is not shown here: the aterm instance allowed it without naming the \
                 session",
            )),
        },
        Asked::Answered { word, .. } if word == "none" && !facts.in_aterm => outside(facts),
        Asked::Answered { word, .. } => {
            let why = WORDS
                .iter()
                .find(|(known, _)| known == word)
                .map_or("the instance refused it", |(_, why)| why);
            Err(Error::new(&format!(
                "plaintext is not shown here: {why} ({word}); an agent edits with `set … --stdin`"
            )))
        }
        Asked::Failed(why) => Err(Error::new(&format!(
            "plaintext is not shown here: the aterm instance this runs under did not answer \
             whether this session may reveal ({why})"
        ))),
        Asked::NoInstance if facts.in_aterm => Err(Error::new(
            "plaintext is not shown here: this process runs under aterm, and no aterm instance \
             among its ancestors could be asked",
        )),
        Asked::NoInstance => outside(facts),
    }
}

/// Outside aterm, the agent-marker check stands in.
fn outside(facts: &Facts) -> Result<Place> {
    match facts.marker {
        Some(marker) => Err(Error::new(&format!(
            "plaintext is not shown here: an agent runs this ({marker} is set)"
        ))),
        None => Ok(Place::Outside),
    }
}

/// This process's facts, read now: its terminals, its environment, and the `reveal` answers of
/// this Mac's aterm instances.
#[must_use]
pub fn facts() -> Facts {
    use std::io::IsTerminal as _;
    let var = |name: &str| std::env::var(name).ok();
    let ancestors = ancestors();
    let image = |pid: u32| match aterm_uds::holders::exe_of(pid) {
        aterm_uds::holders::Exe::At(path) => Some(path),
        aterm_uds::holders::Exe::Unnamed | aterm_uds::holders::Exe::Exited => None,
    };
    Facts {
        stdin_tty: std::io::stdin().is_terminal(),
        stdout_tty: std::io::stdout().is_terminal(),
        asked: ask(),
        marker: aterm_types::domain::agent_marker(&var),
        in_aterm: var("ATERM_SESSION_ID").is_some_and(|sid| !sid.is_empty())
            || aterm_among(&ancestors, &image),
    }
}

/// Whether any of `ancestors` runs an aterm image, as `image` reads each one's executable.
fn aterm_among(ancestors: &[u32], image: &dyn Fn(u32) -> Option<std::path::PathBuf>) -> bool {
    ancestors
        .iter()
        .filter_map(|pid| image(*pid))
        .any(|exe| aterm_uds::left::is_aterm_image(&exe))
}

/// This process's ancestors, nearest first, up to launchd or init.
fn ancestors() -> Vec<u32> {
    let mut out = Vec::new();
    let mut pid = i32::try_from(std::process::id()).unwrap_or(0);
    for _ in 0..64 {
        let Some(lineage) = aterm_uds::process::read_lineage(pid) else {
            break;
        };
        if lineage.parent <= 1 {
            break;
        }
        out.push(lineage.parent);
        pid = i32::try_from(lineage.parent).unwrap_or(0);
    }
    out
}

/// Ask every aterm instance on this Mac whether the session this process belongs to may reveal,
/// and [`combine`] the answers.
fn ask() -> Asked {
    let Ok(instances) = aterm_ctl::local_instances() else {
        return Asked::NoInstance;
    };
    combine(instances.iter().map(|(_, sock)| ask_one(sock)).collect())
}

/// The verdict of every instance's answer. A word that withholds (any but `allowed` and `none`)
/// decides, the first in [`WORDS`]' order: whichever instance holds the session, or the outer one
/// of nested instances, refuses for it. Then an instance that did not answer refuses, since it
/// might hold the session. Then `allowed` (an instance placed the caller in a person's tab), then
/// `none`; no instance at all is [`Asked::NoInstance`].
fn combine(answers: Vec<Asked>) -> Asked {
    let said = |word: &str| {
        answers
            .iter()
            .find(|asked| matches!(asked, Asked::Answered { word: said, .. } if said == word))
    };
    if let Some(withheld) = WORDS
        .iter()
        .filter(|(word, _)| *word != "none")
        .find_map(|(word, _)| said(word))
    {
        return withheld.clone();
    }
    if let Some(failed) = answers
        .iter()
        .find(|asked| matches!(asked, Asked::Failed(_)))
    {
        return failed.clone();
    }
    [ALLOWED, "none"]
        .into_iter()
        .find_map(said)
        .map_or(Asked::NoInstance, Clone::clone)
}

/// Ask the instance listening at `sock` whether the session this process belongs to may reveal.
fn ask_one(sock: &str) -> Asked {
    match reveal(sock) {
        Ok(lines) => parse(&lines),
        Err(why) => Asked::Failed(why),
    }
}

/// The lines of the `reveal` answer of the instance listening at `sock`, or why there are none.
fn reveal(sock: &str) -> std::result::Result<Vec<String>, String> {
    let (stream, token) = aterm_ctl::connect_then_read_token(sock)
        .map_err(|error| format!("connect: {}", error.kind()))?;
    let token = token.map_err(|_| "its token could not be read".to_owned())?;
    aterm_ctl::exchange_lines(
        &stream,
        &token,
        "reveal",
        std::time::Duration::from_secs(10),
    )
    .map_err(|error| format!("reveal: {}", error.kind()))
}

/// The aterm session this process's lineage leads to and the agent nearest it in that lineage,
/// as an aterm instance on this Mac answers `reveal` (by the kernel's peer process and its
/// ancestry, never by `ATERM_SESSION_ID`): what a record states as its principal's session and
/// program (`crate::said`). The instance nearest this process in its own ancestry answers first
/// ([`nearest_first`]): of nested instances, the inner one holds the session this process runs
/// in, while the outer one names the session the inner instance runs in. `None` where no
/// instance names a session: none runs here, none answered, or this process runs in none of
/// their sessions.
#[must_use]
pub fn lineage() -> Option<Lineage> {
    let instances = aterm_ctl::local_instances().ok()?;
    let mut ancestors = Vec::new();
    let mut at = aterm_uds::sudo::parent_pid();
    while at > 1 && ancestors.len() < 64 {
        ancestors.push(at);
        let Some(parent) = aterm_uds::sudo::parent_of(at) else {
            break;
        };
        at = parent;
    }
    nearest_first(&instances, &ancestors)
        .into_iter()
        .find_map(|sock| reveal(sock).ok().and_then(|lines| named(&lines)))
}

/// The sockets of `instances` (pid, socket), those whose pid is in `ancestors` (this process's,
/// nearest first) first and nearest first, then the rest in their order.
fn nearest_first<'a>(instances: &'a [(u32, String)], ancestors: &[i32]) -> Vec<&'a String> {
    let place = |pid: u32| {
        ancestors
            .iter()
            .position(|at| i32::try_from(pid).is_ok_and(|pid| pid == *at))
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<_> = instances.iter().collect();
    ordered.sort_by_key(|(pid, _)| place(*pid));
    ordered.into_iter().map(|(_, sock)| sock).collect()
}

/// What an instance's `reveal` answer names of the asking process ([`lineage`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lineage {
    /// The session its lineage leads to: an ID aterm minted.
    pub session: String,
    /// The agent nearest it in that lineage (`claude`, `codex`, …), where one runs there.
    pub program: Option<String>,
    /// A person typed it: the instance found no agent in its lineage (`program=-`) and nothing
    /// else acts in that session (`verdict=allowed`: not spawned, carried, held or driven by
    /// `aterm ctl`, and no agent in the session's tree). False where the answer leaves it unknown,
    /// such as one from an instance before `program=`.
    pub person: bool,
}

/// The session and agent a `reveal` answer's one line names (`verdict=<word> session=<sid> …
/// program=<agent|->`): `None` where it names no session, or names one or an agent out of
/// grammar. An instance before `program=` names no agent and no person ([`Lineage::person`]).
fn named(lines: &[String]) -> Option<Lineage> {
    let [line] = lines else {
        return None;
    };
    let field = |key: &str| {
        line.split_ascii_whitespace()
            .find_map(|field| field.strip_prefix(key))
    };
    let session = field("session=")
        .filter(|sid| aterm_types::control_request::Sid::parse(sid).is_some())?
        .to_owned();
    let mut person = false;
    let program = match field("program=") {
        None => None,
        Some("-") => {
            person = field("verdict=") == Some(ALLOWED);
            None
        }
        Some(word)
            if (1..=32).contains(&word.len())
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') =>
        {
            Some(word.to_owned())
        }
        Some(_) => return None,
    };
    Some(Lineage {
        session,
        program,
        person,
    })
}

/// The verdict of a `reveal` answer's lines (`verdict=<word> session=<sid|->`), with the session
/// it names: an exact local session ID ([`aterm_types::control_request::Sid`]): a tab ID, or the
/// `s-` and 20 lower-case hex digits a tab opened before the `t` form keeps until it closes), or
/// none for `-`. The instance names its own session, on this Mac, so the legacy form is read
/// here as every reader of this Mac's own shells reads it; any other word is out of grammar.
fn parse(lines: &[String]) -> Asked {
    let [line] = lines else {
        return Asked::Failed("an answer of more or fewer than one line".to_owned());
    };
    let field = |key: &str| {
        line.split_ascii_whitespace()
            .find_map(|field| field.strip_prefix(key))
    };
    let Some(word) = field("verdict=")
        .filter(|word| *word == ALLOWED || WORDS.iter().any(|(known, _)| known == word))
    else {
        return Asked::Failed("an answer without a known verdict".to_owned());
    };
    let session = match field("session=") {
        None | Some("-") => None,
        Some(sid) if aterm_types::control_request::Sid::parse(sid).is_some() => {
            Some(sid.to_owned())
        }
        Some(_) => return Asked::Failed("an answer whose session is no session id".to_owned()),
    };
    Asked::Answered {
        word: word.to_owned(),
        session,
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
