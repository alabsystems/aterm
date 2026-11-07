// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ADMISSION ROW (Continuum TM-0, `aterm_telemetry`): an admission decided in this user's
//! own ledger appends one `kind=admission` row to this Mac's telemetry log, in the claimant's
//! process, the moment it is decided. [`crate::admit_with`] and its kin write theirs. A caller
//! that decides an admission by driving a [`crate::Waiter`] itself owes its row too: it takes an
//! [`Asked`] as it asks and gives it the decision ([`Asked::decided`], [`Asked::decided_in`]),
//! which consumes it, so one row an admission, never one a look. atpkg's host job's one look and
//! a nested build's look in its turn do. A caller whose wait spans calls of its own keeps an
//! [`Owed`] beside its waiter instead, which also writes a row where the wait is given up
//! undecided: the Trust lane's launch gate (`aterm-release`, `trust_lane::Admission`) and the
//! resource host's retained runs (`aterm-resource-host`, its queue's memory admission). A
//! scratch ledger (every test's) writes none, so only this Mac's real claims become rows. This
//! user's ledger is [`Ledger::user`]'s folder: the capacity folder, or the backend of the memory
//! namespace that account's ledger was adopted into. A test reads the rows its admissions owe
//! through [`capture`] (test and debug builds).
//!
//! ONE CLOCK: every wait here is the wall clock's (unix ms). An admitted claim's is its grant's
//! `admitted_ms` less its ask ([`Claim::asked_ms`], its record's `start_ms`), the stamps its
//! build or test row reads from the same record ([`wait`]), so the two rows say one wait
//! (`meter=exact`, the claim's own stamps). A refused or ended one's runs from the ask's stamp to
//! the decision's (`meter=stamped`). No monotonic `Instant` can: a build's row is written in the
//! process that saw it end, often the holder that adopted the claim from its file and reads only
//! its record, and macOS's `Instant` stops while the Mac sleeps, so a claim that waited across a
//! sleep said a shorter wait in its admission row than in its build row. Stamps that run
//! backwards (the clock set back by more than the wait) are `?clock` ([`interval`]), never a
//! known 0; a smaller step back reads as a shorter known wait.
//!
//! The row holds digests of what was asked and where, and its label is the first word of the
//! need's description, which the claimant program wrote (`targo`, `lab`); never the rest of its
//! words. Its CPU and peak memory are `na`: they are the work's, which its command row carries.
//!
//! The write never holds up the claim it reports for long: it goes through the log's one direct
//! writer ([`aterm_telemetry::direct`]), which reads the host's facts once a process and waits at
//! most [`aterm_telemetry::direct::LOCK_WAIT`] for another writer's lock. A row that misses it is
//! lost and counted in a loss record in the log's folder, which the next append of any writer
//! says in a gap line ([`aterm_telemetry::log::mark_lost`]); where no record can be left, the next
//! row this process writes says it. Telemetry never fails an admission.

use std::path::Path;

use aterm_telemetry::{Digest, Fig, Kind, Meter, Outcome, Row, Stop};

use crate::{Claim, Error, Ledger};

/// AN ADMISSION'S ROW, OWED FROM ITS ASK TO ITS DECISION: what it asked, kept before its need
/// moves into the wait, and the wall clock's stamp of the ask. Taken once an admission, before
/// its first entry, and consumed by its decision ([`Self::decided`]), however many looks that
/// decision took.
#[must_use = "an admission owes its row: give it its decision"]
pub struct Asked {
    what: String,
    checkout: Option<String>,
    at_ms: u64,
}

impl Asked {
    /// `need`'s words and checkout, asked now.
    pub fn of(need: &crate::Need) -> Self {
        Self {
            what: need.what.clone(),
            checkout: need.checkout.clone(),
            at_ms: aterm_telemetry::now_ms(),
        }
    }

    /// Write the row of `result`, this admission's decision in `ledger`, where `ledger` is this
    /// user's own ([`Ledger::user`]): to this thread's [`capture`] where a test installed one,
    /// else none in this crate's unit tests.
    pub fn decided(self, ledger: &Ledger, result: &Result<Claim, Error>) {
        decided(ledger, &self, result.as_ref());
    }

    /// [`Self::decided`] with this user's ledger folder `user` (its [`Ledger::dir`]) and the
    /// host's name and facts digest, the row handed to `write`: nothing unless `ledger` is
    /// `user`'s. The seam through which a caller's test reads the rows its admissions owe.
    pub fn decided_in(
        self,
        ledger: &Ledger,
        user: &Path,
        host: &str,
        digest: Digest,
        result: &Result<Claim, Error>,
        write: impl FnOnce(&Row) -> bool,
    ) {
        decided_in(ledger, user, host, digest, &self, result.as_ref(), write);
    }
}

/// AN ADMISSION'S ROW, OWED ACROSS A WAIT ITS CALLER DRIVES: an [`Asked`] kept with the ledger it
/// was asked in, for a caller that keeps a [`crate::Waiter`] across calls of its own and decides
/// its admission at whichever of them admits it or ends its wait (the Trust lane's launch gate,
/// the resource host's queue). Taken before the first entry; its decision consumes it
/// ([`Self::decided`]), however many looks that took. One given up undecided writes the row of a
/// wait stopped, `outcome=fail` ([`Error::Stopped`]): by a signal where its caller knows one
/// ([`Self::stopped`]), else as it drops (the run that waited ended, the request cancelled or
/// refused for another reason, a panic unwinding). So an admission asked writes exactly one row,
/// however its wait ends while its process lives. A process that exits or is killed with the wait
/// still open runs no drop and writes none, as no claimant's open wait does.
#[must_use = "an admission owes its row: give it its decision"]
pub struct Owed {
    ledger: Ledger,
    /// The ask, until its decision takes it.
    asked: Option<Asked>,
}

impl Owed {
    /// `need`'s admission in `ledger`, asked now.
    pub fn of(ledger: &Ledger, need: &crate::Need) -> Self {
        Self {
            ledger: ledger.clone(),
            asked: Some(Asked::of(need)),
        }
    }

    /// Write the row of `result`, this admission's decision, where its ledger is this user's own
    /// ([`Asked::decided`]).
    pub fn decided(mut self, result: Result<&Claim, &Error>) {
        if let Some(asked) = self.asked.take() {
            decided(&self.ledger, &asked, result);
        }
    }

    /// Write the row of its wait, given up undecided: `signal` stopped it, or (`None`) its caller
    /// did.
    pub fn stopped(self, signal: Option<i32>) {
        self.decided(Err(&Error::Stopped {
            signal,
            short: None,
        }));
    }
}

impl Drop for Owed {
    fn drop(&mut self) {
        if let Some(asked) = self.asked.take() {
            decided(
                &self.ledger,
                &asked,
                Err(&Error::Stopped {
                    signal: None,
                    short: None,
                }),
            );
        }
    }
}

/// AN ADMITTED CLAIM'S WAIT, as its admission row and its build or test row both say it
/// (`wait_ms`): its grant's `admitted_ms` less its ask's `asked_ms` ([`Claim::asked_ms`]), by
/// the wall clock ([`interval`]). `admitted_ms` is `None` where its record holds no grant, or a
/// clock read before 1970 left it 0: `?aterm-capacity`.
#[must_use]
pub fn wait(asked_ms: u64, admitted_ms: Option<u64>) -> Fig<u64> {
    admitted_ms.filter(|&ms| ms != 0).map_or_else(
        || Fig::Unknown("aterm-capacity".to_owned()),
        |admitted| interval(admitted, asked_ms),
    )
}

/// The interval from `earlier` to `later`, two wall-clock stamps (unix ms): `?clock` where they
/// run backwards (the clock set back between them by more than the interval), never a known 0.
#[must_use]
pub fn interval(later: u64, earlier: u64) -> Fig<u64> {
    later
        .checked_sub(earlier)
        .map_or_else(|| Fig::Unknown("clock".to_owned()), Fig::Known)
}

/// Write `result`'s row where `ledger` is this user's own ledger ([`Ledger::user`]): to this
/// thread's [`capture`] where a test installed one.
pub(crate) fn decided(ledger: &Ledger, asked: &Asked, result: Result<&Claim, &Error>) {
    #[cfg(any(test, debug_assertions))]
    if seam::took(ledger, asked, result) {
        return;
    }
    if cfg!(test) {
        return;
    }
    let Some(source) = crate::ledger::user_dir() else {
        return;
    };
    let Some(dir) = aterm_telemetry::log::default_dir() else {
        return;
    };
    let (facts, digest) = aterm_telemetry::direct::facts();
    decided_from(ledger, source, &facts.host, *digest, asked, result, |row| {
        aterm_telemetry::direct::write_now(dir, row)
    });
}

/// [`decided`] with this user's capacity folder `source` (`ledger::user_dir`): held to the folder
/// of the ledger it opens ([`Ledger::user`]), the folder itself or the backend of the memory
/// namespace its ledger was adopted into.
pub(crate) fn decided_from(
    ledger: &Ledger,
    source: std::path::PathBuf,
    host: &str,
    digest: Digest,
    asked: &Asked,
    result: Result<&Claim, &Error>,
    write: impl FnOnce(&Row) -> bool,
) {
    let user = Ledger::account_at(source);
    decided_in(ledger, user.dir(), host, digest, asked, result, write);
}

/// [`decided`] with this user's ledger folder `user` (its [`Ledger::dir`]) and the host's name and
/// facts digest, the row handed to `write`: nothing unless `ledger` is `user`'s.
pub(crate) fn decided_in(
    ledger: &Ledger,
    user: &Path,
    host: &str,
    digest: Digest,
    asked: &Asked,
    result: Result<&Claim, &Error>,
    write: impl FnOnce(&Row) -> bool,
) {
    if !aterm_telemetry::direct::same_folder(ledger.dir(), user) {
        return;
    }
    let now = aterm_telemetry::now_ms();
    let decision = match result {
        Ok(claim) => Decision::Admitted(wait(
            claim.asked_ms(),
            claim.record().held.map(|h| h.admitted_ms),
        )),
        Err(e) => Decision::Not(e, interval(now, asked.at_ms)),
    };
    let row = row(asked, &decision, now, host, digest);
    let _ = write(&row);
}

/// TEST SEAM (test and debug builds), past the unit-test build's silence and into a dependent's
/// tests: run `f` on this thread with `user` standing for this user's ledger
/// ([`Ledger::user`]), and return what it returned with the `kind=admission` row each admission
/// decided meanwhile on this thread wrote, in order ([`Asked::decided`], [`Owed`],
/// [`crate::admit_with`] and its kin), none of them to this Mac's log. An admission in another
/// ledger writes none, as in a production build. A capture inside another takes the rows written
/// while it runs; the outer one takes the rest.
#[cfg(any(test, debug_assertions))]
pub fn capture<T>(user: Ledger, f: impl FnOnce() -> T) -> (T, Vec<Row>) {
    seam::capture(user, f)
}

/// [`capture`]'s per-thread stand-ins.
#[cfg(any(test, debug_assertions))]
mod seam {
    use std::cell::RefCell;

    use aterm_telemetry::Row;

    use super::Asked;
    use crate::{Claim, Error, Ledger};

    /// A capture's stand-in for this user's ledger, and the rows it took.
    struct Seam {
        user: Ledger,
        rows: Vec<Row>,
    }

    thread_local! {
        static SEAM: RefCell<Option<Seam>> = const { RefCell::new(None) };
    }

    pub(super) fn capture<T>(user: Ledger, f: impl FnOnce() -> T) -> (T, Vec<Row>) {
        /// This capture, installed over the one it replaced (none, or an outer capture), which
        /// it puts back however `f` ends.
        struct Installed(Option<Option<Seam>>);
        impl Installed {
            /// Put back the one it replaced, and take this one's stand-ins.
            fn restore(&mut self) -> Option<Seam> {
                let outer = self.0.take()?;
                SEAM.with(|seam| std::mem::replace(&mut *seam.borrow_mut(), outer))
            }
        }
        impl Drop for Installed {
            fn drop(&mut self) {
                let _ = self.restore();
            }
        }
        let outer = SEAM.with(|seam| {
            seam.borrow_mut().replace(Seam {
                user,
                rows: Vec::new(),
            })
        });
        let mut installed = Installed(Some(outer));
        let out = f();
        let rows = installed.restore().map(|s| s.rows).unwrap_or_default();
        (out, rows)
    }

    /// Whether a capture on this thread took `result`'s row (written where `ledger` is its
    /// stand-in for this user's ledger), so none goes to this Mac's log.
    pub(super) fn took(ledger: &Ledger, asked: &Asked, result: Result<&Claim, &Error>) -> bool {
        SEAM.with(|seam| {
            // Only a capture borrows it: one is installed, and its row goes nowhere else.
            let Ok(mut seam) = seam.try_borrow_mut() else {
                return true;
            };
            let Some(seam) = seam.as_mut() else {
                return false;
            };
            let (facts, digest) = aterm_telemetry::direct::facts();
            let user = seam.user.dir().to_path_buf();
            super::decided_in(ledger, &user, &facts.host, *digest, asked, result, |row| {
                seam.rows.push(row.clone());
                true
            });
            true
        })
    }
}

/// How an admission ended, and its wait by the wall clock.
pub(crate) enum Decision<'a> {
    /// Admitted, after the wait its record's stamps say ([`wait`]).
    Admitted(Fig<u64>),
    /// Not admitted, after the wait from its ask to the decision ([`interval`]).
    Not(&'a Error, Fig<u64>),
}

/// The `kind=admission` row for `decision`, decided at `at_ms` on `host_name` with facts `facts`.
pub(crate) fn row(
    asked: &Asked,
    decision: &Decision<'_>,
    at_ms: u64,
    host_name: &str,
    facts: Digest,
) -> Row {
    let (outcome, stop, sig, waited, meter) = match decision {
        Decision::Admitted(w) => (Outcome::Ok, Stop::Exit, None, w.clone(), Meter::Exact),
        Decision::Not(e, w) => {
            let (outcome, stop, sig) = match e {
                Error::Never(_) | Error::Unmet { .. } | Error::Timing(_) => {
                    (Outcome::Refused, Stop::Exit, None)
                }
                Error::Stopped {
                    signal: Some(s), ..
                } => (Outcome::Fail, Stop::Signal, Some(*s)),
                Error::Stopped { signal: None, .. } => (Outcome::Fail, Stop::Exit, None),
                Error::Stuck { .. } => (Outcome::Fail, Stop::Stalled, None),
                Error::Ledger(_) => (Outcome::Fail, Stop::Unknown, None),
            };
            (outcome, stop, sig, w.clone(), Meter::Stamped)
        }
    };
    let program = asked.what.split_whitespace().next().unwrap_or("");
    Row {
        at_ms,
        kind: Kind::Admission,
        key: Some(Digest::key(
            &Kind::Admission,
            &asked.what,
            asked.checkout.as_deref(),
        )),
        dir: asked.checkout.as_deref().map(Digest::dir),
        label: aterm_telemetry::row::label(program),
        sid: None,
        on: None,
        host: aterm_telemetry::row::host_word(host_name),
        facts,
        wall_ms: waited.clone(),
        cpu_ms: Fig::Na,
        peak_bytes: Fig::Na,
        wait_ms: waited,
        cache: Fig::Na,
        outcome,
        stop,
        code: None,
        sig,
        meter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked() -> Asked {
        Asked {
            what: "targo test aterm-gui".to_owned(),
            checkout: Some("/Users//me/secret-project".to_owned()),
            at_ms: 1_000,
        }
    }

    fn facts() -> Digest {
        Digest::of("test", &[b"facts"])
    }

    #[test]
    fn an_admitted_claim_writes_its_own_wait_and_no_words() {
        let row = row(
            &asked(),
            &Decision::Admitted(Fig::Known(4_250)),
            1_000,
            "m14",
            facts(),
        );
        assert_eq!(row.kind, Kind::Admission);
        assert_eq!(
            (row.wait_ms.clone(), row.wall_ms.clone()),
            (Fig::Known(4_250), Fig::Known(4_250))
        );
        assert_eq!(
            (row.cpu_ms.clone(), row.peak_bytes.clone()),
            (Fig::Na, Fig::Na)
        );
        assert_eq!(
            (row.outcome, row.stop, row.meter),
            (Outcome::Ok, Stop::Exit, Meter::Exact)
        );
        assert_eq!(row.label, "targo");
        let line = row.line();
        assert!(
            !line.contains("aterm-gui") && !line.contains("secret-project"),
            "{line}"
        );
        assert_eq!(
            row.key,
            Some(Digest::key(
                &Kind::Admission,
                "targo test aterm-gui",
                Some("/Users//me/secret-project")
            ))
        );
    }

    #[test]
    fn a_claim_not_admitted_says_why_by_its_outcome_and_stop() {
        let at = |e: &Error| {
            let r = row(
                &asked(),
                &Decision::Not(e, Fig::Known(9)),
                1,
                "m14",
                facts(),
            );
            (r.outcome, r.stop, r.sig, r.meter)
        };
        assert_eq!(
            at(&Error::Never("too big".to_owned())),
            (Outcome::Refused, Stop::Exit, None, Meter::Stamped)
        );
        assert_eq!(
            at(&Error::Unmet {
                what: "verify".to_owned(),
                short: crate::Shortfall::default(),
                holders: None,
                relieved: true,
            }),
            (Outcome::Refused, Stop::Exit, None, Meter::Stamped)
        );
        assert_eq!(
            at(&Error::Stopped {
                signal: Some(2),
                short: None
            }),
            (Outcome::Fail, Stop::Signal, Some(2), Meter::Stamped)
        );
        assert_eq!(
            at(&Error::Ledger(std::io::Error::other("gone"))),
            (Outcome::Fail, Stop::Unknown, None, Meter::Stamped)
        );
    }

    fn host() -> aterm_telemetry::HostFacts {
        aterm_telemetry::HostFacts {
            host: "m14".to_owned(),
            model: "Mac15,14".to_owned(),
            cpu: "Apple M3 Ultra".to_owned(),
            cores: 32,
            mem_mib: 98_304,
            os: "macOS 27.0.1".to_owned(),
            aterm: "0.155.0+gtest".to_owned(),
        }
    }

    fn refused() -> Result<Claim, Error> {
        Err(Error::Never("too big".to_owned()))
    }

    /// A CLAIM THAT WAITED ACROSS A SLEEP (TM-1 review): macOS's monotonic clock stops while the
    /// Mac sleeps and the wall clock does not, so the claim's own [`Claim::waited`] says an hour
    /// less than its record's stamps. Its admission row says the record's wait, by the wall
    /// clock, which is what its build or test row reads from the same record ([`wait`]): one
    /// wait from one clock. NEGATIVE CONTROL (by hand: the admitted row's wait taken from
    /// `waited`, as before): the row says seconds where the build row says an hour.
    #[test]
    fn a_claim_that_waited_across_a_sleep_says_one_wait_in_both_rows() {
        const HOUR: u64 = 3_600_000;
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let ledger = Ledger::at(tmp.path().join("ledger"));
        let m = crate::Fixed::new(crate::Reading::of_gib(48, 40, 14));
        let need = || crate::Need::new("targo test -p a").mem(crate::floor::GIB);
        let asked = Asked::of(&need());
        let mut claim = crate::admit_with(
            &ledger,
            &m,
            need(),
            crate::Admit::Wait,
            crate::Pace::default(),
            &mut |_| {},
        )
        .expect("held");
        claim.slept_while_waiting(HOUR);
        assert!(
            claim.waited() < std::time::Duration::from_secs(60),
            "this process's clock saw no sleep"
        );
        let record = claim.record().clone();
        let build_rows = wait(record.start_ms, record.held.map(|h| h.admitted_ms));
        assert!(
            build_rows.known().is_some_and(|w| w >= HOUR),
            "{build_rows:?}"
        );
        let mut rows = Vec::new();
        let facts = host();
        decided_in(
            &ledger,
            ledger.dir(),
            &facts.host,
            facts.digest(),
            &asked,
            Ok(&claim),
            |row| {
                rows.push(row.clone());
                true
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].wait_ms, build_rows, "one wait in both rows");
        assert_eq!(rows[0].wall_ms, rows[0].wait_ms);
        assert_eq!(rows[0].meter, Meter::Exact);
    }

    /// A refused admission's wait runs from its ask's stamp to its decision's, by the same wall
    /// clock: a clock set back between them is `?clock`, never a known 0, and so is an admitted
    /// claim's whose grant reads before its ask. NEGATIVE CONTROL: stamps in order give theirs.
    #[test]
    fn a_wait_whose_stamps_run_backwards_is_no_known_wait() {
        assert_eq!(interval(5, 9), Fig::Unknown("clock".to_owned()));
        assert_eq!(interval(9, 5), Fig::Known(4));
        assert_eq!(wait(9, Some(5)), Fig::Unknown("clock".to_owned()));
        assert_eq!(wait(5, Some(9)), Fig::Known(4));
        assert_eq!(wait(5, Some(0)), Fig::Unknown("aterm-capacity".to_owned()));
        assert_eq!(wait(5, None), Fig::Unknown("aterm-capacity".to_owned()));
        let later = Asked {
            at_ms: u64::MAX,
            ..asked()
        };
        let mut rows = Vec::new();
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let ledger = Ledger::at(tmp.path().to_path_buf());
        let facts = host();
        decided_in(
            &ledger,
            ledger.dir(),
            &facts.host,
            facts.digest(),
            &later,
            refused().as_ref(),
            |row| {
                rows.push(row.clone());
                true
            },
        );
        assert_eq!(
            (rows[0].wait_ms.clone(), rows[0].meter),
            (Fig::Unknown("clock".to_owned()), Meter::Stamped)
        );
    }

    /// AN OWED ROW IS WRITTEN ONCE, AT ITS DECISION OR AS ITS WAIT IS GIVEN UP (TM-1 review): an
    /// admission whose caller decides it across calls of its own ([`Owed`]) writes one row at its
    /// decision, with the wait its record's stamps say, and none as it drops after; one given up
    /// undecided writes one, `fail`, as it drops, or with the signal its caller names. One in
    /// another ledger writes none. The capture takes `admit_with`'s row too, and one inside
    /// another takes only its own. NEGATIVE CONTROLS (by hand): with `decided` writing nothing,
    /// the first case has no row; with `Drop` writing whatever was decided, it has two.
    #[test]
    fn an_owed_row_is_written_once_at_its_decision_or_as_its_wait_is_given_up() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let ledger = Ledger::at(tmp.path().join("ledger"));
        let m = crate::Fixed::new(crate::Reading::of_gib(48, 40, 14));
        let need = || crate::Need::new("trust check a").mem(crate::floor::GIB);
        let words = |r: &Row| (r.kind.clone(), r.outcome, r.stop, r.sig, r.meter);
        let (claim, rows) = capture(ledger.clone(), || {
            let owed = Owed::of(&ledger, &need());
            let claim = match crate::Waiter::enter(&ledger, &m, need())
                .expect("entered")
                .poll(&m)
            {
                crate::Polled::Admitted(claim) => claim,
                other => panic!("not admitted: {other:?}"),
            };
            owed.decided(Ok(&claim));
            claim
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            words(&rows[0]),
            (Kind::Admission, Outcome::Ok, Stop::Exit, None, Meter::Exact)
        );
        assert_eq!(
            rows[0].wait_ms,
            wait(claim.asked_ms(), claim.record().held.map(|h| h.admitted_ms))
        );
        let ((), rows) = capture(ledger.clone(), || drop(Owed::of(&ledger, &need())));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Exit,
                None,
                Meter::Stamped
            )
        );
        let ((), rows) = capture(ledger.clone(), || {
            Owed::of(&ledger, &need()).stopped(Some(15));
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            words(&rows[0]),
            (
                Kind::Admission,
                Outcome::Fail,
                Stop::Signal,
                Some(15),
                Meter::Stamped
            )
        );
        let other = Ledger::at(tmp.path().join("other"));
        let ((), rows) = capture(ledger.clone(), || drop(Owed::of(&other, &need())));
        assert!(rows.is_empty(), "another ledger: {rows:?}");
        let ((held, inner), outer) = capture(ledger.clone(), || {
            let inner = capture(ledger.clone(), || {
                crate::admit_with(
                    &ledger,
                    &m,
                    need(),
                    crate::Admit::Wait,
                    crate::Pace::default(),
                    &mut |_| {},
                )
                .expect("held")
            });
            drop(Owed::of(&ledger, &need()));
            inner
        });
        assert_eq!(
            (inner.len(), inner[0].outcome),
            (1, Outcome::Ok),
            "{inner:?}"
        );
        assert_eq!(
            (outer.len(), outer[0].outcome),
            (1, Outcome::Fail),
            "{outer:?}"
        );
        drop((claim, held));
    }

    #[test]
    fn only_this_users_ledger_writes_a_row() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let users = tmp.path().join("ledger");
        std::fs::create_dir(&users).expect("ledger");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir(&scratch).expect("scratch");
        let dir = tmp.path().join("telemetry");
        let lost = aterm_telemetry::direct::Lost::new();
        let at = |ledger: &Path| {
            let facts = host();
            decided_in(
                &Ledger::at(ledger.to_path_buf()),
                &users,
                &facts.host,
                facts.digest(),
                &asked(),
                refused().as_ref(),
                |row| aterm_telemetry::direct::write(dir.clone(), &facts, row, &lost),
            );
        };
        at(&scratch);
        assert!(!dir.exists(), "a scratch ledger wrote a row");
        at(&users);
        let read = aterm_telemetry::log::read(&dir, 4, None).expect("read");
        assert_eq!(read.rows.len(), 1);
        let r = &read.rows[0];
        assert_eq!(
            (&r.kind, r.outcome, r.label.as_str()),
            (&Kind::Admission, Outcome::Refused, "targo")
        );
    }
}
