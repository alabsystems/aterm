// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How often one asking process reads a login at a usage limit.
//!
//! A codex-alab at its limit asks for a handoff every few seconds, and an ask the records let
//! through reads its own login and up to three candidates
//! ([`super::codex_rotation::prepare`]), each read able to present that login's single-use
//! refresh token. The records stop most asks, but not a row that reads usable and is never taken:
//! one on the session's own account, one whose relaunch line cannot be built, a selection whose
//! publication keeps failing. A [`ReadPace`] is what one asking process has read, in memory, laid
//! over the loaded records for one ask ([`ReadPace::lay`]). Nothing of it is written to a row:
//! usable but not taken is a fact of the asker, not of the row.
//!
//! A candidate row is read at once, then no sooner than 60 s, 5 min and 30 min after this
//! process's previous read of it ([`PACE_S`]; the last rung repeats): at most 4 reads in an hour
//! of one run. Its run ends after six hours without a read and when its login file holds other
//! credentials. A read of it that answers walled, low or signed out, and that the records then
//! hold, begins a new run at the first rung: the record gates the row from there, and the first
//! ask after the record lets it go and that rung has passed reads it.
//!
//! The session's own login is read once per pass, and a pass is of one of four kinds
//! ([`OwnRead`]).
//! - A news pass finds the own login at a limit and reads at least one row this process has not
//!   read on that login, or last read walled, low or signed out. It is on no ladder: a sweep of
//!   unread rows, a new sign-in and a wall on record that ended are read at the next ask.
//! - A repeat pass finds the own login at a limit and reads no such row: no candidate at all, or
//!   only rows this process last read usable or unanswered. It climbs the own login's ladder, the
//!   same rungs: at most 4 repeat passes in an hour whatever the number of candidates. While that
//!   ladder stands the rows already read usable are not read either, so the forecast names news
//!   past them, and news is still read.
//! - A pass the own login's read ends (no figure, signed out, another identity, a read that
//!   unwinds) climbs the same ladder and holds every ask while it stands, news too: no candidate
//!   changes what that login answers, however many are unread.
//! - A pass whose read shows no limit a handoff leaves (`not-walled`) climbs nothing. It restarts
//!   the own login's ladder at its first rung, which holds every ask for those 60 s: at most one
//!   such pass in 60 s. How much less often is each control's, which decides when it asks again
//!   after one.
//!
//! The own login's ladder lasts one series of asks: it ends once the process has not asked for
//! longer than [`ASKING_S`], or asks at a time before its last ask (a clock set back), so the
//! first ask at a later wall reads. The rows' runs outlast it. Every ask is one of the series, an
//! ask its control answers without a pass too: whoever keeps a process's pace notes each of those
//! ([`ReadPace::asks`]), since a control answers asks from a selection it holds for up to a
//! minute, and a series counted only by its passes would break at nearly every such hold.

use super::accounts::{self, AuthContext, Cli, Row};
use super::accounts_pool::Seat;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// How long one process leaves a login unread after this many of its reads in a row: the first
/// three rungs of the records' own ladder ([`accounts::UNANSWERED_S`]), by index so the two cannot
/// drift apart. Its two-hour rung is left out: while a target is named the session's own login is
/// read at least every 30 minutes, so a limit lifted on it is learned no later.
pub(super) const PACE_S: [i64; 3] = [
    accounts::UNANSWERED_S[0],
    accounts::UNANSWERED_S[1],
    accounts::UNANSWERED_S[2],
];

/// A process that has not asked for longer than this has ended its series of asks (its client asks
/// every few seconds while its wall stands), and the own login's ladder with it. Only asks noted
/// to the pace count ([`ReadPace::asks`]).
pub(super) const ASKING_S: i64 = 60;

/// One process's reads of one login in a row.
#[derive(Debug, Clone, Copy)]
pub(super) struct Run {
    /// When the newest of them was made (Unix seconds).
    pub(super) at: i64,
    /// How many in a row: none for a login that waits a first rung without a read counted.
    pub(super) attempts: u32,
    /// Whether an ask this hold refused was already reported ([`ReadPace::waits`]).
    said: bool,
}

impl Run {
    /// Until when the login is left unread ([`PACE_S`]).
    pub(super) fn stands_until(self) -> i64 {
        self.at
            .saturating_add(accounts::rung(&PACE_S, self.attempts))
    }

    /// The run after one more read at `now`: `earlier`'s next rung, or the first of a new run
    /// where there is none or its newest is [`accounts::UNANSWERED_RUN_S`] old. A clock set back
    /// never moves a run's newest back.
    pub(super) fn climbed(earlier: Option<Run>, now: i64) -> Run {
        let attempts = earlier
            .filter(|run| now.saturating_sub(run.at) < accounts::UNANSWERED_RUN_S)
            .map_or(0, |run| run.attempts)
            .saturating_add(1);
        Run {
            at: earlier.map_or(now, |run| run.at.max(now)),
            attempts,
            said: false,
        }
    }

    /// A new run whose first read was made at `now`, whatever `earlier` had climbed to.
    fn first(earlier: Option<Run>, now: i64) -> Run {
        Run {
            at: earlier.map_or(now, |run| run.at.max(now)),
            attempts: 1,
            said: false,
        }
    }
}

/// What one process holds of a candidate row.
pub(super) struct RowPace {
    /// Its reads of the row.
    pub(super) run: Run,
    /// The row's login as it stood after the newest read, or when its forecast named the row
    /// unread; `None` where its file gave none.
    context: Option<AuthContext>,
    /// Whether a later read of the row would be a repeat: its newest read answered usable or no
    /// figure, or a pass named it and did not read it. Not after one that answered walled, low or
    /// signed out.
    pub(super) repeat: bool,
}

/// The ladder of the session's own login.
pub(super) struct OwnPace {
    /// The login file its passes read: a ladder is one file's, and holds no ask of another.
    file: PathBuf,
    /// The passes that climbed it.
    pub(super) run: Run,
    /// Whether an ask whose forecast names news is let past it: only where the pass that last
    /// moved it found the login at a limit a handoff leaves ([`OwnRead::Walled`]).
    admits_news: bool,
}

/// What a pass's read of the session's own login came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnRead {
    /// It ended the pass before any candidate: no figure, signed out, another identity, or a
    /// read that never came back. What a pass is until it says otherwise.
    Ended,
    /// It shows no limit a handoff leaves.
    NoWall,
    /// It shows a limit a handoff leaves, so the pass goes on to its candidates.
    Walled,
}

/// A read a pass made, as it settles ([`ReadPace::settle`]): a hold this process now keeps. Its
/// text is the fact a log line of the asking session says.
#[derive(Debug)]
pub(super) struct Climb {
    /// The candidate row, or `None` for the session's own login.
    pub(super) row: Option<String>,
    /// How many reads in a row that makes.
    pub(super) attempts: u32,
    /// Until when the login is left unread (Unix seconds).
    pub(super) until: i64,
}

impl fmt::Display for Climb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            row,
            attempts,
            until,
        } = self;
        match row {
            Some(row) => write!(
                f,
                "read {row} ({attempts} in a row by this process); it is left unread until {until}"
            ),
            None => write!(
                f,
                "read its own login ({attempts} passes in a row by this process); it is left \
                 unread until {until}"
            ),
        }
    }
}

/// A hold that refused an ask, reported once per rung ([`ReadPace::waits`]). Its text is the fact
/// a log line of the asking session says.
#[derive(Debug)]
pub(super) struct Wait {
    /// The candidate row, or `None` for the session's own login.
    pub(super) row: Option<String>,
    /// How many reads in a row the hold stands on.
    pub(super) attempts: u32,
    /// Until when the login is left unread (Unix seconds).
    pub(super) until: i64,
}

impl fmt::Display for Wait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            row,
            attempts,
            until,
        } = self;
        let login = row.as_deref().unwrap_or("its own login");
        write!(f, "waits: {login} is left unread until {until}")?;
        // A ladder a reading of no wall restarted stands on no pass counted.
        if *attempts > 0 {
            write!(f, " ({attempts} in a row by this process)")?;
        }
        Ok(())
    }
}

/// What one asking process has read at its usage limit, and so what it leaves unread for now (the
/// module's text has the rules). In memory only, and one process's: another process, and this
/// one after aterm restarts, starts with none and reads at its first ask. Whoever asks keeps it
/// from one ask to the next; [`super::codex_rotation::prepare`] keeps none, so each of its calls
/// starts from none.
pub struct ReadPace {
    /// The candidate rows it read, or was named and did not read, by label.
    pub(super) rows: BTreeMap<String, RowPace>,
    /// The ladder of its own login.
    pub(super) own: Option<OwnPace>,
    /// When it last asked.
    asked_at: Option<i64>,
}

impl fmt::Debug for ReadPace {
    /// Labels, counts and ends: never a login's context.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|(label, held)| {
                (
                    label,
                    held.run.attempts,
                    held.run.stands_until(),
                    held.repeat,
                )
            })
            .collect();
        let own = self
            .own
            .as_ref()
            .map(|own| (own.run.attempts, own.run.stands_until(), own.admits_news));
        f.debug_struct("ReadPace")
            .field("rows", &rows)
            .field("own", &own)
            .field("asked_at", &self.asked_at)
            .finish()
    }
}

/// A candidate read of one pass.
pub(super) struct Minted {
    /// The row.
    pub(super) label: String,
    /// The row's login: as captured before the read, then as it stood after it.
    pub(super) context: Option<AuthContext>,
    /// Whether the row was news to this process when the read was minted.
    pub(super) news: bool,
    /// What [`RowPace::repeat`] becomes. A read that never comes back counts as a repeat, so the
    /// own login's ladder bounds it.
    pub(super) repeat: bool,
    /// Whether the read answered walled, low or signed out and the records then held the row.
    pub(super) on_record: bool,
}

impl ReadPace {
    /// A process that has read nothing.
    pub(crate) const fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
            own: None,
            asked_at: None,
        }
    }

    /// The process asks at `now` (Unix seconds, the clock [`super::codex_rotation::prepare`]'s seat
    /// reads): where it had not for longer than a minute (`ASKING_S`), or had last asked later
    /// than `now` (the clock was set back, and how long it did not ask is unknown), its earlier
    /// series of asks is over and the own login's ladder with it. The rows' runs stay.
    ///
    /// Each pass notes its own ask. Whoever keeps a process's pace notes every other ask of it,
    /// each one its control answers without a pass: a selection it offers or publishes again, a
    /// read that is out, an answer that stands. Unnoted, a control's hold of a selection (up to a
    /// minute and the ask after it) leaves more than a minute between two passes of one series,
    /// the ladder ends at that pass, and the session's own login is read at each of them.
    pub fn asks(&mut self, now: i64) {
        if self
            .asked_at
            .is_some_and(|at| now < at || now.saturating_sub(at) > ASKING_S)
        {
            self.own = None;
        }
        self.asked_at = Some(now);
    }

    /// Lays what this process leaves unread over `state`, the records one ask loaded, at
    /// `seat.now`, for a session on `source` whose login file is `own`. First it forgets what no
    /// longer binds it: a row off `roster`, a run [`accounts::UNANSWERED_RUN_S`] quiet, a row
    /// whose login file now holds other credentials than it read (a file that gives none is no
    /// change), and the own login's ladder where it is another file's. Then every row but
    /// `source` is left unread to its own rung's end, and one whose next read would be a repeat
    /// also to the end of the own login's ladder.
    pub(super) fn lay(
        &mut self,
        seat: &Seat<'_>,
        roster: &accounts::Roster,
        source: &Row,
        own: &Path,
        state: &mut accounts::State,
    ) {
        let now = seat.now;
        let quiet = |run: &Run| now.saturating_sub(run.at) >= accounts::UNANSWERED_RUN_S;
        let store = seat.store(Cli::Codex);
        self.rows.retain(|label, held| {
            let Some(row) = roster.row(label) else {
                return false;
            };
            if quiet(&held.run) {
                return false;
            }
            match (&held.context, store.capture_auth_context(row)) {
                (Some(read), Some(current)) => *read == current,
                (None, Some(_)) => false,
                (_, None) => true,
            }
        });
        if self
            .own
            .as_ref()
            .is_some_and(|ladder| ladder.file != own || quiet(&ladder.run))
        {
            self.own = None;
        }
        let ladder = self
            .own
            .as_ref()
            .map(|ladder| ladder.run.stands_until())
            .filter(|until| *until > now);
        for (label, held) in &self.rows {
            if *label == source.label {
                continue;
            }
            let rung = (held.run.attempts >= 1).then(|| held.run.stands_until());
            let hidden = if held.repeat { ladder } else { None };
            if let Some(until) = rung.max(hidden).filter(|until| *until > now) {
                accounts::lay_unread_until(state, label, until);
            }
        }
    }

    /// Whether a read of `label` would be a repeat ([`RowPace::repeat`]); a row this process
    /// holds nothing of is news.
    pub(super) fn asked(&self, label: &str) -> bool {
        self.rows.get(label).is_some_and(|held| held.repeat)
    }

    /// Whether the ladder of the own login in `own` refuses an ask at `now`: it stands, and the
    /// ask's forecast names a `repeat` (or no row) or the ladder lets no news past it.
    pub(super) fn holds(&self, own: &Path, repeat: bool, now: i64) -> bool {
        self.own.as_ref().is_some_and(|ladder| {
            ladder.file == own && ladder.run.stands_until() > now && (repeat || !ladder.admits_news)
        })
    }

    /// The holds standing at `now` that have refused no ask yet, each reported this once: the own
    /// login's ladder, and each row on a rung of its own.
    pub(super) fn waits(&mut self, now: i64) -> Vec<Wait> {
        let mut waits = Vec::new();
        let mut report = |row: Option<&String>, run: &mut Run| {
            if run.stands_until() > now && !run.said {
                run.said = true;
                waits.push(Wait {
                    row: row.cloned(),
                    attempts: run.attempts,
                    until: run.stands_until(),
                });
            }
        };
        if let Some(ladder) = &mut self.own {
            report(None, &mut ladder.run);
        }
        for (label, held) in &mut self.rows {
            if held.run.attempts >= 1 {
                report(Some(label), &mut held.run);
            }
        }
        waits
    }

    /// Begins a pass at `now` that reads the login in `own`, where the forecast `named` a row
    /// (with its login's context) or none. The pass settles when it is dropped, however it ends.
    pub(super) fn begin(
        &mut self,
        own: &Path,
        named: Option<(String, Option<AuthContext>)>,
        now: i64,
    ) -> Pass<'_> {
        Pass {
            pace: self,
            own: own.to_owned(),
            named,
            read: Vec::new(),
            own_read: OwnRead::Ended,
            now,
        }
    }

    /// Settles a pass at `now` that read the login in `own`, which came to `own_read`, and the
    /// candidates in `read`; returns the holds it made. A read of the own login that showed no
    /// limit to leave restarts its ladder at the first rung and climbs nothing; one that ended
    /// the pass climbs it; one that showed a limit climbs it where none of the candidate reads
    /// was news. Only that last ladder lets news past it. Each candidate read climbs its row
    /// whatever it answered, but one the records now hold begins a new run. A row the forecast
    /// `named` that a pass at a limit did not read counts as asked, or an ask that never reaches
    /// its row (no line can be built to it, the records were busy) would be let through as news
    /// every time.
    pub(super) fn settle(
        &mut self,
        own: &Path,
        own_read: OwnRead,
        named: Option<(String, Option<AuthContext>)>,
        read: Vec<Minted>,
        now: i64,
    ) -> Vec<Climb> {
        let mut climbs = Vec::new();
        let walled = own_read == OwnRead::Walled;
        if own_read == OwnRead::NoWall {
            self.own = Some(OwnPace {
                file: own.to_owned(),
                run: Run {
                    at: now,
                    attempts: 0,
                    said: false,
                },
                admits_news: false,
            });
        } else if read.iter().all(|minted| !minted.news) {
            let earlier = self
                .own
                .take()
                .filter(|ladder| ladder.file == own)
                .map(|ladder| ladder.run);
            let run = Run::climbed(earlier, now);
            climbs.push(Climb {
                row: None,
                attempts: run.attempts,
                until: run.stands_until(),
            });
            self.own = Some(OwnPace {
                file: own.to_owned(),
                run,
                admits_news: walled,
            });
        }
        let named = named
            .filter(|_| walled)
            .filter(|(label, _)| read.iter().all(|minted| minted.label != *label));
        for minted in read {
            let earlier = self.rows.get(&minted.label).map(|held| held.run);
            let run = if !minted.repeat && minted.on_record {
                Run::first(earlier, now)
            } else {
                Run::climbed(earlier, now)
            };
            climbs.push(Climb {
                row: Some(minted.label.clone()),
                attempts: run.attempts,
                until: run.stands_until(),
            });
            self.rows.insert(
                minted.label,
                RowPace {
                    run,
                    context: minted.context,
                    repeat: minted.repeat,
                },
            );
        }
        if let Some((label, context)) = named {
            self.rows
                .entry(label)
                .or_insert(RowPace {
                    run: Run {
                        at: now,
                        attempts: 0,
                        said: true,
                    },
                    context,
                    repeat: true,
                })
                .repeat = true;
        }
        climbs
    }
}

/// One pass over the session's own login and its candidates, from just before the own login's
/// read is authorized. Dropping it settles the pace ([`ReadPace::settle`]), so a pass that
/// returns early, and one whose read unwinds, is counted like any other.
pub(super) struct Pass<'a> {
    pace: &'a mut ReadPace,
    own: PathBuf,
    named: Option<(String, Option<AuthContext>)>,
    read: Vec<Minted>,
    own_read: OwnRead,
    now: i64,
}

impl Pass<'_> {
    /// A read of the candidate `label` is authorized, its login standing at `before`.
    pub(super) fn minted(&mut self, label: &str, before: Option<AuthContext>) {
        self.read.push(Minted {
            label: label.to_owned(),
            context: before,
            news: !self.pace.asked(label),
            repeat: true,
            on_record: false,
        });
    }

    /// The read of `label` came back with its login standing at `after`: `repeat` where it
    /// answered usable or no figure, not where it answered walled, low or signed out. The login
    /// kept is the one the read left, so a read that renewed its token does not end its own hold.
    pub(super) fn answered(&mut self, label: &str, after: Option<AuthContext>, repeat: bool) {
        if let Some(minted) = self
            .read
            .iter_mut()
            .rev()
            .find(|minted| minted.label == label)
        {
            minted.context = after;
            minted.repeat = repeat;
        }
    }

    /// Asks `holds` of each row this pass read walled, low or signed out whether the records now
    /// hold it: such a row's run begins anew, since its record gates it from here.
    pub(super) fn held_on_record(&mut self, mut holds: impl FnMut(&str) -> bool) {
        for minted in &mut self.read {
            if !minted.repeat {
                minted.on_record = holds(&minted.label);
            }
        }
    }

    /// The own login's read came to `read`; until this is said the pass is one that read ended.
    pub(super) const fn own_answered(&mut self, read: OwnRead) {
        self.own_read = read;
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        // What the pass climbed is said nowhere yet: a line would name holds that the pace of a
        // single ask ([`super::codex_rotation::prepare`]) never keeps.
        let _ = self.pace.settle(
            &self.own,
            self.own_read,
            self.named.take(),
            std::mem::take(&mut self.read),
            self.now,
        );
    }
}
