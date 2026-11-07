// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ADMISSION ROW (Continuum TM-0, `aterm_telemetry`): each admission decided in this user's
//! own ledger appends one `kind=admission` row to this Mac's telemetry log, in the claimant's
//! process, the moment it is decided. An admitted claim's wait is its own clock
//! ([`crate::Claim::waited`], `meter=exact`); a refused or ended one's is the call's stopwatch
//! (`meter=stamped`). A scratch ledger (every test's) writes none, so only this Mac's real
//! claims become rows. This user's ledger is [`Ledger::user`]'s folder: the capacity folder, or
//! the backend of the memory namespace that account's ledger was adopted into.
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
use std::time::{Duration, Instant};

use aterm_telemetry::{Digest, Fig, Kind, Meter, Outcome, Row, Stop};

use crate::{Claim, Error, Ledger};

/// What an admission asked, kept before its need moves into the wait.
pub(crate) struct Asked {
    what: String,
    checkout: Option<String>,
    at: Instant,
}

impl Asked {
    /// `need`'s words and checkout, and the stopwatch started now.
    pub(crate) fn of(need: &crate::Need) -> Self {
        Self {
            what: need.what.clone(),
            checkout: need.checkout.clone(),
            at: Instant::now(),
        }
    }
}

/// Write `result`'s row where `ledger` is this user's own ledger ([`Ledger::user`]).
pub(crate) fn decided(ledger: &Ledger, asked: &Asked, result: &Result<Claim, Error>) {
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
    result: &Result<Claim, Error>,
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
    result: &Result<Claim, Error>,
    write: impl FnOnce(&Row) -> bool,
) {
    if !aterm_telemetry::direct::same_folder(ledger.dir(), user) {
        return;
    }
    let decision = match result {
        Ok(claim) => Decision::Admitted(claim.waited()),
        Err(e) => Decision::Not(e, asked.at.elapsed()),
    };
    let row = row(asked, &decision, aterm_telemetry::now_ms(), host, digest);
    let _ = write(&row);
}

/// How an admission ended.
pub(crate) enum Decision<'a> {
    /// Admitted after the claim's own wait.
    Admitted(Duration),
    /// Not admitted, after the call's stopwatch.
    Not(&'a Error, Duration),
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
        Decision::Admitted(w) => (Outcome::Ok, Stop::Exit, None, *w, Meter::Exact),
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
            (outcome, stop, sig, *w, Meter::Stamped)
        }
    };
    let wait = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
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
        wall_ms: Fig::Known(wait),
        cpu_ms: Fig::Na,
        peak_bytes: Fig::Na,
        wait_ms: Fig::Known(wait),
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
            at: Instant::now(),
        }
    }

    fn facts() -> Digest {
        Digest::of("test", &[b"facts"])
    }

    #[test]
    fn an_admitted_claim_writes_its_own_wait_and_no_words() {
        let row = row(
            &asked(),
            &Decision::Admitted(Duration::from_millis(4_250)),
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
                &Decision::Not(e, Duration::from_millis(9)),
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
                &refused(),
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
