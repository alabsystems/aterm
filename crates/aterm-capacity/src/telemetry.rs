// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ADMISSION ROW (Continuum TM-0, `aterm_telemetry`): each admission decided in this user's
//! own ledger appends one `kind=admission` row to this Mac's telemetry log, in the claimant's
//! process, the moment it is decided. An admitted claim's wait is its own clock
//! ([`crate::Claim::waited`], `meter=exact`); a refused or ended one's is the call's stopwatch
//! (`meter=stamped`). A scratch ledger (every test's) writes none, so only this Mac's real
//! claims become rows.
//!
//! The row holds digests of what was asked and where, and its label is the first word of the
//! need's description, which the claimant program wrote (`targo`, `lab`); never the rest of its
//! words. Its CPU and peak memory are `na`: they are the work's, which its command row carries.
//!
//! The write never holds up the claim it reports for long: the host's facts are read once a
//! process, and the append waits at most [`LOCK_WAIT`] for another writer's lock. A row that
//! misses it is lost and counted, and the next row this process writes goes after a gap line
//! that says how many, as the window's writer does. Telemetry never fails an admission.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aterm_telemetry::{Digest, Fig, HostFacts, Kind, Meter, Outcome, Row, Stop};

use crate::{Claim, Error, Ledger};

/// How long an admission's row waits for another writer's lock on the log.
pub(crate) const LOCK_WAIT: Duration = Duration::from_millis(50);

/// This process's host facts and precomputed digest, read at its first admission.
static FACTS: OnceLock<(HostFacts, Digest)> = OnceLock::new();

/// The rows this process lost.
static LOST: Lost = Lost::new();

/// Rows a process lost (a missed lock, a failed write) and how many of them a gap line said.
pub(crate) struct Lost {
    lost: AtomicU64,
    told: AtomicU64,
}

impl Lost {
    pub(crate) const fn new() -> Self {
        Self {
            lost: AtomicU64::new(0),
            told: AtomicU64::new(0),
        }
    }
}

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

/// Write `result`'s row where `ledger` is this user's own ledger.
pub(crate) fn decided(ledger: &Ledger, asked: &Asked, result: &Result<Claim, Error>) {
    if cfg!(test) {
        return;
    }
    let Some(user) = crate::ledger::user_dir() else {
        return;
    };
    let Some(dir) = aterm_telemetry::log::default_dir() else {
        return;
    };
    let (facts, digest) = FACTS.get_or_init(|| {
        let facts = HostFacts::read();
        let digest = facts.digest();
        (facts, digest)
    });
    decided_in_digest(ledger, &user, &dir, facts, *digest, asked, result, &LOST);
}

/// [`decided`] with this user's ledger folder `user`, the log's folder `dir`, the host's
/// `facts` and the process's `lost` given: nothing unless `ledger` is `user`'s.
#[cfg(test)]
pub(crate) fn decided_in(
    ledger: &Ledger,
    user: &Path,
    dir: &Path,
    facts: &HostFacts,
    asked: &Asked,
    result: &Result<Claim, Error>,
    lost: &Lost,
) {
    decided_in_digest(
        ledger,
        user,
        dir,
        facts,
        facts.digest(),
        asked,
        result,
        lost,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "shares precomputed HostFacts digest with decided_in test entry"
)]
fn decided_in_digest(
    ledger: &Ledger,
    user: &Path,
    dir: &Path,
    facts: &HostFacts,
    digest: Digest,
    asked: &Asked,
    result: &Result<Claim, Error>,
    lost: &Lost,
) {
    if !is_users(ledger, user) {
        return;
    }
    let decision = match result {
        Ok(claim) => Decision::Admitted(claim.waited()),
        Err(e) => Decision::Not(e, asked.at.elapsed()),
    };
    let row = row(
        asked,
        &decision,
        aterm_telemetry::now_ms(),
        &facts.host,
        digest,
    );
    let _ = write(dir.to_path_buf(), facts, &row, lost);
}

/// Append `row` to the log in `dir` within [`LOCK_WAIT`], after a gap line for the rows `lost`
/// holds that no line has said yet: `false` (and the row counted in `lost`) when it was not
/// written.
pub(crate) fn write(dir: PathBuf, facts: &HostFacts, row: &Row, lost: &Lost) -> bool {
    let log = aterm_telemetry::Log::new(dir, aterm_telemetry::Limits::DEFAULT, facts.clone())
        .waiting(LOCK_WAIT);
    let had = lost.lost.load(Ordering::Acquire);
    let told = lost.told.load(Ordering::Acquire);
    let mut lines = Vec::with_capacity(2);
    if had > told {
        lines.push(aterm_telemetry::row::gap_line(
            aterm_telemetry::now_ms(),
            had - told,
        ));
    }
    lines.push(row.line());
    if log.append(&lines).is_ok() {
        lost.told.fetch_max(had, Ordering::AcqRel);
        true
    } else {
        lost.lost.fetch_add(1, Ordering::AcqRel);
        false
    }
}

fn is_users(ledger: &Ledger, user: &Path) -> bool {
    user == ledger.dir()
        || std::fs::canonicalize(user)
            .ok()
            .zip(std::fs::canonicalize(ledger.dir()).ok())
            .is_some_and(|(a, b)| a == b)
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
                Error::Never(_) | Error::Timing(_) => (Outcome::Refused, Stop::Exit, None),
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

    fn host() -> HostFacts {
        HostFacts {
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
        let lost = Lost::new();
        let at = |ledger: &Path| {
            decided_in(
                &Ledger::at(ledger.to_path_buf()),
                &users,
                &dir,
                &host(),
                &asked(),
                &refused(),
                &lost,
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

    #[test]
    fn a_held_lock_costs_one_row_that_the_next_gap_line_says() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir(&dir).expect("dir");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".lock"))
            .expect("lock file");
        aterm_uds::file_lock::lock_exclusive_until(
            &held,
            Instant::now() + Duration::from_secs(60),
            "the test holds the telemetry lock",
        )
        .expect("hold the lock");
        let lost = Lost::new();
        let one = |at| {
            let r = row(
                &asked(),
                &Decision::Admitted(Duration::from_millis(1)),
                at,
                "m14",
                facts(),
            );
            write(dir.clone(), &host(), &r, &lost)
        };
        assert!(!one(1), "a row written past another writer's lock");
        assert_eq!(lost.lost.load(Ordering::Acquire), 1);
        drop(held);
        assert!(one(2), "the row once the lock is free");
        let read = aterm_telemetry::log::read(&dir, 4, None).expect("read");
        assert_eq!(
            read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(read.dropped, 1, "the lost row is said in a gap line");
        assert!(one(3));
        let again = aterm_telemetry::log::read(&dir, 4, None).expect("read");
        assert_eq!(again.dropped, 1, "a lost row is said once");
    }
}
