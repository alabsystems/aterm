// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a tab's CLI really runs on (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1): the
//! login file the process reads, read afresh on every pass, since a `/login` in another tab
//! rewrites it while the process runs. The roster row chosen from the environment is a belief;
//! this is the observation, and [`super::accounts::reconcile`] says where the two disagree.
//!
//! An observer reads identity fields only (the login email and the provider key), never a
//! credential: the profile reader zeroes every buffer it read ([`super::profile::read_with`]).
//! A missing file is [`Observation::Unread`]; a file that names no account is
//! [`Observation::SignedOut`]; neither changes the belief.

use super::accounts::{AuthRoute, Cli, ProcessLogin, Reconciled, Roster, TokenEntry};
use super::journal::{self, LoginState, Note, ObservedLogin, ObservedSrc, SeenWhy, Tab};
use super::profile::{self, AccountKey};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One login a CLI process reads, by its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub cli: Cli,
    /// The login email the file names.
    pub email: String,
    /// The provider's account key, where the file names one.
    pub key: Option<AccountKey>,
    /// Where it was read.
    pub src: ObservedSrc,
    /// The file it was read from: the profile, or for a token route the vault entry's name.
    pub path: PathBuf,
    /// The file's modification time, Unix seconds. Claude Code rewrites its home profile while
    /// it runs (per-project state, counters, caches), so this dates the file, never a `/login`:
    /// a login that changed under a running process is known by its address changing between
    /// two reads ([`Sightings::due`]'s [`SeenWhy::Changed`]).
    pub mtime: Option<i64>,
}

/// What an observer saw of one process's login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A login file that names an account.
    Seen(Observed),
    /// A login file that names none.
    SignedOut { path: PathBuf },
    /// No observation: no file, an environment login, a gateway, a token entry no row names.
    Unread,
}

impl Observation {
    /// The observed login, where one was seen.
    #[must_use]
    pub const fn seen(&self) -> Option<&Observed> {
        match self {
            Self::Seen(seen) => Some(seen),
            Self::SignedOut { .. } | Self::Unread => None,
        }
    }

    /// The observed login email, where one was seen.
    #[must_use]
    pub fn email(&self) -> Option<&str> {
        self.seen().map(|seen| seen.email.as_str())
    }
}

/// An age in seconds, as a sighting's surfaces say it: `12 s`, `4 min`, `3 h`, `2 d`.
#[must_use]
pub fn age_words(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => format!("{secs} s"),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    }
}

fn unix_secs(time: std::time::SystemTime) -> Option<i64> {
    i64::try_from(time.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

/// What Claude Code's process with exec environment `env` runs on, for a user whose home is
/// `user_home`, on `route` (its [`super::accounts::ProcessLogin::route`]). An `env` that carries
/// none of the profile selectors reads the default home's profile.
///
/// - On a token route the profile may be stale (`docs/DESIGN-claude-setup-token-2026-11-05.md`
///   §5.7): the account is the one whose token entry the marker names, the roster row whose
///   entry it is ([`TokenEntry::of_row`]); a marker no row names is [`Observation::Unread`].
/// - On a stored login, the profile [`profile::login_record_of`] selects for `env`: a login
///   directory's own, else the home's.
/// - Any other route (an environment token, an API key, a provider route) is unread.
#[must_use]
pub fn claude(
    env: &[String],
    user_home: &Path,
    route: AuthRoute,
    roster: Option<&Roster>,
) -> Observation {
    match route {
        AuthRoute::Outside => Observation::Unread,
        AuthRoute::VaultToken { entry } => {
            let Some(roster) = roster else {
                return Observation::Unread;
            };
            let mut named = roster
                .rows
                .iter()
                .filter(|row| TokenEntry::of_row(row) == Some(entry));
            let (Some(row), None) = (named.next(), named.next()) else {
                return Observation::Unread;
            };
            let Some(email) = row.expected_identity_email() else {
                return Observation::Unread;
            };
            Observation::Seen(Observed {
                cli: Cli::Claude,
                email: email.to_owned(),
                key: None,
                src: ObservedSrc::VaultToken,
                path: PathBuf::from(entry.name()),
                mtime: None,
            })
        }
        AuthRoute::Stored => {
            if let Some((record, path, mtime)) = profile::login_record_of(env, user_home) {
                return Observation::Seen(Observed {
                    cli: Cli::Claude,
                    email: record.email,
                    key: record.key,
                    src: ObservedSrc::Profile,
                    path,
                    mtime: unix_secs(mtime),
                });
            }
            match profile::login_profile_of(env, user_home) {
                Some(path) if path.is_file() => Observation::SignedOut { path },
                _ => Observation::Unread,
            }
        }
    }
}

/// How often a running tab's sighting of one account is noted again: hourly, so the fleet's
/// view of who runs what is fresh while the journal carries few notes.
pub const SEEN_REFRESH_S: i64 = 3_600;

/// What one tab's host keeps of its sightings across passes: the last account it noted and when,
/// each (observed, believed) disagreement already noted, the session it opened in the journal
/// on an account no row lists (whose end no local row would note), and the last login it handed
/// the registry (whose tab [`Sightings::end`] takes off it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sightings {
    last: Option<(String, i64)>,
    wrong: BTreeSet<(String, String)>,
    open: Option<Note>,
    login: Option<ObservedLogin>,
}

impl Sightings {
    /// Why `email`'s sighting at `now` is noted, or `None` where it is not yet due: the first
    /// sighting, a change of account, then at most once per [`SEEN_REFRESH_S`].
    pub fn due(&mut self, email: &str, now: i64) -> Option<SeenWhy> {
        let email = email.trim().to_ascii_lowercase();
        let why = match &self.last {
            None => SeenWhy::First,
            Some((last, _)) if *last != email => SeenWhy::Changed,
            Some((_, at)) if now.saturating_sub(*at) >= SEEN_REFRESH_S => SeenWhy::Refresh,
            Some(_) => return None,
        };
        self.last = Some((email, now));
        Some(why)
    }

    /// Whether the disagreement of `observed` with the believed row `believed` is new for this
    /// tab, marking it noted.
    pub fn wrong_once(&mut self, observed: &str, believed: &str) -> bool {
        self.wrong
            .insert((observed.trim().to_ascii_lowercase(), believed.to_owned()))
    }

    /// The `session-end` of the session [`sighting`] opened on an account no row lists, at `t`,
    /// closing it.
    fn close(&mut self, t: u64) -> Option<Note> {
        let mut end = self.open.take()?;
        end.kind = "session-end";
        end.t = t;
        Some(end)
    }

    /// The tab's session is over at `now` (its agent left for good): the `session-end` of a
    /// session it opened on an account no row lists, and its last login handed back to the
    /// registry as no longer run by this tab ([`LoginState::Idle`]), so that Mac's count of tabs
    /// on it falls. Everything kept is forgotten.
    pub fn end(&mut self, now: i64) -> Sighted {
        let mut out = Sighted::default();
        let Ok(t) = u64::try_from(now) else {
            *self = Self::default();
            return out;
        };
        out.notes.extend(self.close(t));
        out.login = self.login.take().map(|login| ObservedLogin {
            state: LoginState::Idle,
            at: t,
            ..login
        });
        *self = Self::default();
        out
    }
}

/// What one pass's sighting of a tab gives ([`sighting`]): the journal notes, and the login for
/// the registry's `account-obs` rows where a note was due.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sighted {
    pub notes: Vec<Note>,
    pub login: Option<ObservedLogin>,
}

/// One pass's sighting of the tab `tab` whose process `who` runs and whose login file says
/// `seen`, run by `harness`, at `now`: a `seen` note when one is due ([`Sightings::due`]), a
/// `wrong-account` once per disagreement with the believed row, and for an account no row lists
/// (whose session no local row records) a `session-start` keyed by the observed account, closed
/// by a `session-end` when the tab moves to another account or ends ([`Sightings::end`]). Keys
/// come from the installed journal sink; with none, no note is made.
#[must_use]
pub fn sighting(
    roster: &Roster,
    who: &ProcessLogin,
    seen: &Observation,
    sightings: &mut Sightings,
    tab: Tab,
    harness: &'static str,
    now: i64,
) -> Sighted {
    let mut out = Sighted::default();
    let Some(observed) = seen.seen() else {
        return out;
    };
    let Ok(t) = u64::try_from(now) else {
        return out;
    };
    let Some((realm, email)) = journal::claims_of(observed.cli, &observed.email) else {
        return out;
    };
    let key = journal::realm_key(&realm, &email);
    let why = sightings.due(&observed.email, now);
    if let (Some(why), Some(key)) = (why, key) {
        out.notes.push(journal::seen_note(
            observed.cli,
            key,
            tab,
            harness,
            observed.src,
            why,
            t,
        ));
    }
    let reconciled = super::accounts::reconcile(roster, who, seen);
    if reconciled.disagrees()
        && let Some(believed) =
            super::accounts::judged_label(roster, who).and_then(|label| roster.row(label))
        && let Some(key) = key
        && sightings.wrong_once(&observed.email, &believed.label)
    {
        let to = journal::account_key(observed.cli, believed);
        out.notes.push(journal::wrong_account_note(
            observed.cli,
            key,
            to,
            tab,
            observed.src,
            t,
        ));
    }
    let unlisted = matches!(reconciled, Reconciled::Unlisted(_));
    if matches!(why, Some(SeenWhy::First | SeenWhy::Changed))
        && sightings
            .open
            .is_some_and(|open| !unlisted || open.account != key)
    {
        out.notes.extend(sightings.close(t));
    }
    if unlisted
        && sightings.open.is_none()
        && let Some(key) = key
    {
        let mut start = journal::seen_note(
            observed.cli,
            key,
            tab,
            harness,
            observed.src,
            SeenWhy::First,
            t,
        );
        start.kind = "session-start";
        start.why = None;
        start.src = "cli-output";
        sightings.open = Some(start);
        out.notes.push(start);
    }
    if why.is_some() {
        let login = ObservedLogin {
            cli: observed.cli,
            realm,
            email,
            state: LoginState::Running,
            tab: Some(tab),
            at: t,
            src: observed.src,
        };
        sightings.login = Some(login.clone());
        out.login = Some(login);
    }
    out
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
