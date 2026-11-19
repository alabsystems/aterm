// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROSTER ROW'S LOGIN VERDICT IS A FOLD OVER DATED EVIDENCE (plan package UB-04).
//!
//! A row's login on this Mac is judged by what was seen of it, each reading dated by its own
//! evidence: a wall a session met on it ([`LoginLedger::struck`], dated by the wall's first
//! sighting), a turn or a reading it served ([`LoginLedger::served`], dated by the turn's point,
//! its reply or the reading), and a check's word ([`LoginLedger::checked`]: a sign-in's check, a
//! probe, a proof, a displacement by the vendor's own sign-in). The newest of them decides
//! ([`LoginLedger::verdict`], [`aterm_evidence::Ledger::verdict`]); a re-read of an old wall is
//! the same reading and renews nothing. Nothing is written to lift a strike: a turn served after
//! it is newer evidence, and the verdict reads it so.
//!
//! On 2026-11-13 a Cloudflare 403 made Claude Code print `Please run /login` on m16, the row
//! was struck signed out, and its login served all day while the stored word held five idle
//! tabs off a Claude Code upgrade for over eight hours: the word was a source, the served
//! evidence was not, and every lift had to be written by a path whose conditions that day's
//! turns never met. Here the stored word ([`super::Account::auth`], [`super::Account::auth_at`])
//! is only this fold's projection, written so older readers and the survey read the
//! evidence-ordered word.

use aterm_evidence::{CredentialPrint, Ledger, Obs, Source, Subject, WitnessId};

use super::{Auth, Cap, Map, Value, stamp};

/// The namespace of every print a [`LoginLedger`] keys its evidence by: the vendor's stored
/// login, never a token route (a token's reading speaks only for the token).
const STORED_LOGIN: &str = "stored-login";

/// The longest witness id kept: a witness names one reading, never a transcript.
const WITNESS_MAX: usize = 160;

/// What a login's evidence says of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Login {
    /// It served: it works.
    Serves,
    /// It is signed out.
    Struck,
}

impl Login {
    /// The word [`super::Account::auth`] projects it as.
    #[must_use]
    pub const fn auth(self) -> Auth {
        match self {
            Self::Serves => Auth::Ok,
            Self::Struck => Auth::SignedOut,
        }
    }

    /// The login an auth word says, where it says one.
    #[must_use]
    pub const fn of_auth(auth: Auth) -> Option<Self> {
        match auth {
            Auth::Ok => Some(Self::Serves),
            Auth::SignedOut => Some(Self::Struck),
            Auth::Unseen => None,
        }
    }
}

/// Which kind of evidence a reading is, in the order that decides between readings of one
/// second: a check's word loses a tie to what a session saw (a check is this program's belief
/// about the login, a turn served or a wall met is the login's own answer), and a strike decides
/// over a served reading (a wall first seen in the second a turn was served is no older than the
/// turn; the next served second decides again). A sign-in is no check of the login it replaced:
/// it starts the next login's ledger ([`LoginLedger::renew`]), so no strike of the earlier login
/// competes with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Said {
    /// A check's word.
    Checked(Login),
    /// A turn or a reading the login served.
    Served,
    /// A wall a session met on it, or a read that found it ended.
    Struck,
}

impl Said {
    /// What it says of the login.
    #[must_use]
    pub const fn login(self) -> Login {
        match self {
            Self::Served => Login::Serves,
            Self::Struck => Login::Struck,
            Self::Checked(login) => login,
        }
    }

    /// The leg it is kept in, which names its witness apart from the other legs'.
    const fn leg(self) -> &'static str {
        match self {
            Self::Served => "served",
            Self::Struck => "struck",
            Self::Checked(_) => "checked",
        }
    }
}

/// One kept reading: when its evidence is dated, what it was read from, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginObs {
    /// The evidence's own date (Unix seconds): a wall's first sighting, a turn's point or reply,
    /// a reading's time, a check's answer. Never when it was read again or written down.
    pub at: i64,
    /// The reading's witness: the same witness is one reading however often it is read.
    pub witness: String,
    /// Where it was read.
    pub source: Source,
}

impl LoginObs {
    /// A reading dated `at` from `witness` (cut to [`WITNESS_MAX`] bytes on a character
    /// boundary), read at `source`.
    #[must_use]
    pub fn new(at: i64, witness: &str, source: Source) -> Self {
        let mut end = witness.len().min(WITNESS_MAX);
        while !witness.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            at,
            witness: witness[..end].to_owned(),
            source,
        }
    }

    pub(super) fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("at".into(), Value::from(self.at));
        o.insert("witness".into(), Value::from(self.witness.as_str()));
        o.insert("source".into(), Value::from(source_word(self.source)));
        Value::Object(o)
    }

    pub(super) fn from_json(v: &Value, cap: Cap) -> Option<Self> {
        Some(Self::new(
            stamp(v.get("at"), cap)?,
            v.get("witness")?.as_str()?,
            source_of(v.get("source")?.as_str()?)?,
        ))
    }
}

/// The verdict of a [`LoginLedger`]: what its newest reading says, and that reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// What the newest reading says.
    pub login: Login,
    /// Which kind of reading it is.
    pub said: Said,
    /// The reading.
    pub obs: LoginObs,
}

/// ONE ROW'S STORED LOGIN AS THIS MAC HAS SEEN IT: the newest reading of each kind, folded by the
/// date of its own evidence ([`Self::verdict`]). It is the source of the row's login word; the
/// word on record ([`super::Account::auth`]) is its projection where it holds a reading. A login
/// that is replaced (a renewal, a checked sign-in, a changed login context that retires the word
/// on record, a forgotten row) starts an empty ledger under the next print, dated by when it
/// started ([`Self::renew`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginLedger {
    /// Which of this row's logins the readings are of: the ledger's own count, advanced where a
    /// replaced login starts an empty ledger, so no reading of an earlier login is folded with a
    /// later one's ([`Self::print`]).
    pub generation: u64,
    /// When the login this generation describes started (Unix seconds): a reading dated before
    /// it is evidence of a login it replaced, which says nothing of this one, so it is never
    /// kept ([`Self::record`]). `None` for a ledger no replacement started.
    pub since: Option<i64>,
    /// The newest wall a session met on the login, or read that found it ended, by its first
    /// sighting.
    pub struck: Option<LoginObs>,
    /// The newest turn or reading it served.
    pub served: Option<LoginObs>,
    /// The newest check's word and its reading.
    pub checked: Option<(Login, LoginObs)>,
}

impl LoginLedger {
    /// A ledger seeded from a word on record that no ledger kept (a `state.json` an older build
    /// wrote, or a record a reader built by hand): the word, whichever it is, is a check's of no
    /// known cause, dated by the word's time (no time reads as the epoch, which every dated
    /// reading outranks), so a check of the same second replaces it, as a later word did, and a
    /// wall met or a turn served in that second decides over it ([`Said`]'s order).
    #[must_use]
    pub fn of_word(auth: Auth, at: Option<i64>) -> Self {
        let at = at.unwrap_or(0);
        let checked = |login: Login| {
            let word = login.auth().as_str();
            Some((
                login,
                LoginObs::new(at, &format!("record:{word}@{at}"), Source::Record),
            ))
        };
        match Login::of_auth(auth) {
            None => Self::default(),
            Some(login) => Self {
                checked: checked(login),
                ..Self::default()
            },
        }
    }

    /// Whether it holds no reading.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.struck.is_none() && self.served.is_none() && self.checked.is_none()
    }

    /// Whether no ledger was ever kept for the row: no reading, and no login started one
    /// ([`Self::renew`]). Its word on record, an older build's, is then its only evidence
    /// ([`Self::of_word`]); an empty ledger of a later login has none.
    #[must_use]
    pub const fn kept_nothing(&self) -> bool {
        self.generation == 0 && self.is_empty()
    }

    /// The print every reading of this ledger is of: the stored login, at [`Self::generation`].
    #[must_use]
    pub fn print(&self) -> CredentialPrint {
        CredentialPrint::new(STORED_LOGIN, "", self.generation)
    }

    /// The readings kept, each with its kind.
    fn readings(&self) -> impl Iterator<Item = (Said, &LoginObs)> {
        self.struck
            .iter()
            .map(|obs| (Said::Struck, obs))
            .chain(self.served.iter().map(|obs| (Said::Served, obs)))
            .chain(
                self.checked
                    .iter()
                    .map(|(login, obs)| (Said::Checked(*login), obs)),
            )
    }

    /// THE VERDICT: the newest reading of the current print decides
    /// ([`aterm_evidence::Ledger::verdict`]), by its evidence's date, then its kind ([`Said`]'s
    /// order), then its witness. `None` where nothing was read.
    #[must_use]
    pub fn verdict(&self) -> Option<Verdict> {
        let print = self.print();
        let mut ledger = Ledger::new();
        for (said, obs) in self.readings() {
            ledger.observe(Obs {
                value: said,
                at: u64::try_from(obs.at).unwrap_or(0),
                witness: WitnessId::Reading(format!("{}:{}", said.leg(), obs.witness)),
                source: obs.source,
                subject: Subject::Credential(print),
            });
        }
        let newest = ledger.verdict(&print)?.value;
        let obs = self
            .readings()
            .find(|(said, _)| *said == newest)
            .map(|(_, obs)| obs.clone())?;
        Some(Verdict {
            login: newest.login(),
            said: newest,
            obs,
        })
    }

    /// When the login last served on its evidence: its newest served reading or ok check.
    #[must_use]
    pub fn served_at(&self) -> Option<i64> {
        let checked = self
            .checked
            .as_ref()
            .filter(|(login, _)| *login == Login::Serves)
            .map(|(_, obs)| obs.at);
        self.served.as_ref().map(|obs| obs.at).max(checked)
    }

    /// When its newest reading of any kind is dated.
    #[must_use]
    pub fn newest_at(&self) -> Option<i64> {
        self.readings().map(|(_, obs)| obs.at).max()
    }

    /// Keep `obs` as a reading of kind `said`: a strike or a served reading where it is newer
    /// than the one of its kind kept (by date, then witness), a check where it is dated no
    /// earlier than the check kept (checks are this program's own acts, which its writer's lock
    /// orders). A witness kept already is the same reading and changes nothing, whatever date it
    /// carries now, and a reading dated before the login this generation describes started
    /// ([`Self::since`]) is of a login it replaced and is never kept: an idle tab's old wall,
    /// asked again after the login's context changed, strikes nothing. Whether the ledger
    /// changed.
    pub fn record(&mut self, said: Said, obs: LoginObs) -> bool {
        if self.since.is_some_and(|since| obs.at < since) {
            return false;
        }
        let newer = |held: &Option<LoginObs>| {
            held.as_ref().is_none_or(|held| {
                held.witness != obs.witness && (obs.at, &obs.witness) > (held.at, &held.witness)
            })
        };
        match said {
            Said::Struck if newer(&self.struck) => self.struck = Some(obs),
            Said::Served if newer(&self.served) => self.served = Some(obs),
            Said::Checked(login)
                if self.checked.as_ref().is_none_or(|(held_login, held)| {
                    obs.at >= held.at && (*held_login, held) != (login, &obs)
                }) =>
            {
                self.checked = Some((login, obs));
            }
            Said::Struck | Said::Served | Said::Checked(_) => return false,
        }
        true
    }

    /// Start the next login's empty ledger, that login the row's from `since` (a renewal's or a
    /// sign-in's time, a changed login context's, its first reading's): no reading dated before
    /// it is kept ([`Self::record`]).
    pub fn renew(&mut self, since: i64) {
        *self = Self {
            generation: self.generation.saturating_add(1),
            since: Some(since),
            ..Self::default()
        };
    }

    /// The ledger as `state.json` keeps it.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        if self.generation > 0 {
            o.insert("generation".into(), Value::from(self.generation));
        }
        if let Some(since) = self.since {
            o.insert("since".into(), Value::from(since));
        }
        if let Some(obs) = &self.struck {
            o.insert("struck".into(), obs.to_json());
        }
        if let Some(obs) = &self.served {
            o.insert("served".into(), obs.to_json());
        }
        if let Some((login, obs)) = &self.checked {
            let mut checked = obs.to_json();
            if let Some(object) = checked.as_object_mut() {
                object.insert("word".into(), Value::from(login.auth().as_str()));
            }
            o.insert("checked".into(), checked);
        }
        Value::Object(o)
    }

    /// The ledger `state.json` keeps, each date capped by `cap`; a leg that does not read as one
    /// is no reading.
    pub(super) fn from_json(v: &Value, cap: Cap) -> Self {
        let leg = |name: &str| v.get(name).and_then(|leg| LoginObs::from_json(leg, cap));
        Self {
            generation: v.get("generation").and_then(Value::as_u64).unwrap_or(0),
            since: stamp(v.get("since"), cap),
            struck: leg("struck"),
            served: leg("served"),
            checked: v.get("checked").and_then(|checked| {
                let login = Login::of_auth(Auth::parse(checked.get("word")?.as_str()?))?;
                Some((login, LoginObs::from_json(checked, cap)?))
            }),
        }
    }
}

/// A screen's first sighting of one process's login wall, kept by whoever watches that screen:
/// a wall still drawn at a later look is the same wall, dated by when it was first seen, so a
/// re-read of an old wall on an idle screen renews nothing. The process `P` saw it; another
/// process's wall, or one drawn after that process ran a turn or served ([`Self::cleared`]), is
/// a new sighting: the turn is the wall's epoch, so a wall printed again is another witness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallSighting<P> {
    first: Option<(P, i64)>,
}

impl<P> Default for WallSighting<P> {
    fn default() -> Self {
        Self { first: None }
    }
}

impl<P: Clone> WallSighting<P> {
    /// The first sighting of `process`'s wall, seen again at `now`: the one kept for the process
    /// `same` takes for it, else `now`, kept from here on.
    pub fn sighted(&mut self, process: &P, now: i64, same: impl Fn(&P, &P) -> bool) -> i64 {
        match &self.first {
            Some((held, at)) if same(held, process) => *at,
            _ => {
                self.first = Some((process.clone(), now));
                now
            }
        }
    }

    /// The process ran a turn or served: a wall it shows after this is a new one.
    pub fn cleared(&mut self) {
        self.first = None;
    }
}

fn source_word(source: Source) -> &'static str {
    match source {
        Source::Transcript => "transcript",
        Source::Screen => "screen",
        Source::Reading => "reading",
        Source::Fleet => "fleet",
        Source::Record => "record",
    }
}

fn source_of(word: &str) -> Option<Source> {
    Some(match word {
        "transcript" => Source::Transcript,
        "screen" => Source::Screen,
        "reading" => Source::Reading,
        "fleet" => Source::Fleet,
        "record" => Source::Record,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "accounts_login_ledger_tests.rs"]
mod tests;
