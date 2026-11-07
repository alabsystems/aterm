// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUILD ROW (Continuum TM-1, `aterm_telemetry`): each build or test a claim in this user's
//! own capacity ledger held writes one `kind=build` or `kind=test` row (`test` for the verb
//! `test`, `build` for every other) as its claim ends ([`super::Ended::keep`]), from the process
//! that saw it end: the holder, the supervising front, the host job or the epoch table.
//!
//! - `wall_ms`: from its admission to its end (`now - admitted_ms`), the claim's own clock.
//! - `wait_ms`: its admission wait, `admitted_ms - start_ms` (the claim's record; an adopted
//!   claim's own `waited` is zero). Each subtracts two wall-clock stamps (end, admission,
//!   ask): where they run backwards (the clock set back by more than the interval), it is
//!   `?clock`, never a known 0; a smaller step back reads as a shorter known interval.
//! - `peak_bytes`: the build tree's meter's peak ([`aterm_capacity::tree_meter::TreeMeter`]),
//!   a lower bound read every 500 ms (`meter=sampled`), never below a supervising front's floor
//!   from its reaped children's accounting where that floor rose while the build ran
//!   ([`super::supervise`]); none read is unknown.
//! - `outcome`, `stop`, `code`, `sig`: as the end site knows the end ([`BuildEnd`]).
//! - `cpu_ms=na`: no end site but the host job reads the build's CPU, and that is the work's,
//!   which its command or agent-call row carries; `cache=?targo`: Targo names no cache tier yet.
//!
//! As for an admission row, the row holds digests of what was asked and where (the record's
//! checkout, which its need named), and its label is the first word of the need's description
//! (`targo`); never the rest of its words. Only a claim held in this user's own ledger writes
//! one: its file lies in [`aterm_capacity::Ledger::user`]'s folder, a holder's claim directly
//! and a cohort claim (the host job's, an epoch's) in a subfolder of it. A scratch ledger (every
//! test's) writes none.

use std::path::{Path, PathBuf};

use aterm_capacity::Claim;
use aterm_telemetry::{Digest, Fig, HostFacts, Kind, Meter, Outcome, Row, Stop};

/// How a build ended, as the site that saw it end knows it ([`super::Ended::keep`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildEnd {
    /// The wait status of its root, which ran whole (uncancelled, its cohort settled): an exit
    /// code, or the signal that ended it.
    Status(std::process::ExitStatus),
    /// Whether it succeeded, and no more (an epoch's roots, every one validated).
    Success(bool),
    /// An epoch none of whose validated roots failed, though not every root was validated: how
    /// it ended is unknown, while the peak book keeps it as the success it took it for.
    Unfinished,
    /// Nothing known of how it ended (no status was read, or it succeeded in a run that did not
    /// end whole).
    Unknown,
}

impl BuildEnd {
    /// `status` of a run that ended `whole` (uncancelled, its cohort settled), or `None` where
    /// none was read. A run not whole keeps a failing status, which says how it ended; one that
    /// succeeded there is unknown, its work not known to be done.
    #[must_use]
    pub fn judged(status: Option<std::process::ExitStatus>, whole: bool) -> Self {
        match status {
            Some(status) if whole || !status.success() => Self::Status(status),
            Some(_) | None => Self::Unknown,
        }
    }

    /// An epoch's end by its `roots`: `completed` whose exits were validated, `failed` of them
    /// reporting failure. A failure is a failure; every root validated and none failed a
    /// success; none validated unknown; some, none failed, [`Self::Unfinished`].
    #[must_use]
    pub const fn of_roots(roots: usize, completed: usize, failed: usize) -> Self {
        if failed > 0 {
            Self::Success(false)
        } else if completed == 0 {
            Self::Unknown
        } else if completed >= roots {
            Self::Success(true)
        } else {
            Self::Unfinished
        }
    }

    /// Whether it succeeded, as the peak book judges a run ([`super::Ended::keep`]): a whole run's
    /// zero exit, a success said, or an epoch none of whose validated roots failed (the book's
    /// rule before these rows); never an unknown end.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        match self {
            Self::Status(status) => status.success(),
            Self::Success(ok) => *ok,
            Self::Unfinished => true,
            Self::Unknown => false,
        }
    }

    /// The row's `outcome`, `stop`, `code` and `sig`.
    fn words(&self) -> (Outcome, Stop, Option<i32>, Option<i32>) {
        match self {
            Self::Status(status) => match status.code() {
                Some(0) => (Outcome::Ok, Stop::Exit, Some(0), None),
                Some(code) => (Outcome::Fail, Stop::Exit, Some(code), None),
                None => match signal(status) {
                    Some(sig) => (Outcome::Fail, Stop::Signal, None, Some(sig)),
                    None => (Outcome::Unknown, Stop::Unknown, None, None),
                },
            },
            Self::Success(true) => (Outcome::Ok, Stop::Unknown, None, None),
            Self::Success(false) => (Outcome::Fail, Stop::Unknown, None, None),
            Self::Unfinished | Self::Unknown => (Outcome::Unknown, Stop::Unknown, None, None),
        }
    }
}

#[cfg(unix)]
fn signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
const fn signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// What a claim's record says of its build, kept as the claim goes ([`super::Held::let_go`],
/// `into_epoch_parts`): when it was asked and admitted, what it asked for and where, and the
/// ledger it was held in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Run {
    /// When the claim was made, unix ms.
    start_ms: u64,
    /// When it was admitted, unix ms (`None` where its record holds no grant, or a clock read
    /// before 1970 left it 0).
    admitted_ms: Option<u64>,
    /// The need's words (`targo test -p a`): digested, its first word the label.
    what: String,
    /// The checkout it ran for: its record's (the need's, which the record takes), digested.
    checkout: Option<String>,
    /// The folder its claim's file lay in, resolved while the file was there.
    ledger: Option<PathBuf>,
}

impl Run {
    /// `claim`'s.
    pub(crate) fn of(claim: &Claim) -> Self {
        let record = claim.record();
        let ledger = claim
            .path()
            .parent()
            .map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
        Self {
            start_ms: record.start_ms,
            admitted_ms: record
                .held
                .as_ref()
                .map(|h| h.admitted_ms)
                .filter(|&ms| ms != 0),
            what: record.need.what.clone(),
            // A laid record takes its need's checkout (`Need::checkout` is `None` on it after).
            checkout: Some(record.checkout.clone()).filter(|c| !c.is_empty()),
            ledger,
        }
    }

    /// Whether its claim was held in the ledger whose folder is `user`: its file lay there (a
    /// holder's claim) or below it (a cohort claim's subfolder).
    pub(crate) fn held_in(&self, user: &Path) -> bool {
        let Some(ledger) = self.ledger.as_deref() else {
            return false;
        };
        let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        resolved(ledger).starts_with(resolved(user))
    }

    /// Its checkout.
    #[cfg(test)]
    pub(crate) fn checkout(&self) -> Option<&str> {
        self.checkout.as_deref()
    }
}

/// Write the row of `run`'s build, of stage `stage` (`targo test`), that ended `end` with the
/// meter's `peak`, where its ledger is this user's own.
pub(crate) fn ended(run: &Run, stage: &str, end: &BuildEnd, peak: u64) {
    if cfg!(test) {
        return;
    }
    let Some(user) = aterm_capacity::Ledger::user() else {
        return;
    };
    let Some(dir) = aterm_telemetry::log::default_dir() else {
        return;
    };
    let _ = ended_in(run, stage, end, peak, user.dir(), |row| {
        aterm_telemetry::direct::write_now(dir, row)
    });
}

/// [`ended`] with this user's ledger folder `user`, the row handed to `write`: whether a row was
/// written. None unless `run`'s claim was held in `user` ([`Run::held_in`]).
pub(crate) fn ended_in(
    run: &Run,
    stage: &str,
    end: &BuildEnd,
    peak: u64,
    user: &Path,
    write: impl FnOnce(&Row) -> bool,
) -> bool {
    if !run.held_in(user) {
        return false;
    }
    let (facts, digest) = aterm_telemetry::direct::facts();
    let row = row(
        run,
        stage,
        end,
        peak,
        aterm_telemetry::now_ms(),
        facts,
        *digest,
    );
    write(&row)
}

/// The row of `run`'s build of stage `stage`, ended `end` at `at_ms` with the meter's `peak`, on
/// the host `facts` (digest `digest`) describe.
fn row(
    run: &Run,
    stage: &str,
    end: &BuildEnd,
    peak: u64,
    at_ms: u64,
    facts: &HostFacts,
    digest: Digest,
) -> Row {
    let kind = if stage.split_whitespace().last() == Some("test") {
        Kind::Test
    } else {
        Kind::Build
    };
    let unknown = || Fig::Unknown("aterm-capacity".to_owned());
    // The three stamps are wall-clock times: one that runs backwards (the clock was set back
    // between them) is no interval, never a known 0.
    let since = |later: u64, earlier: u64| {
        later
            .checked_sub(earlier)
            .map_or_else(|| Fig::Unknown("clock".to_owned()), Fig::Known)
    };
    let (outcome, stop, code, sig) = end.words();
    let program = run.what.split_whitespace().next().unwrap_or("");
    Row {
        at_ms,
        key: Some(Digest::key(&kind, &run.what, run.checkout.as_deref())),
        kind,
        dir: run.checkout.as_deref().map(Digest::dir),
        label: aterm_telemetry::row::label(program),
        sid: None,
        on: None,
        host: aterm_telemetry::row::host_word(&facts.host),
        facts: digest,
        wall_ms: run
            .admitted_ms
            .map_or_else(unknown, |admitted| since(at_ms, admitted)),
        cpu_ms: Fig::Na,
        peak_bytes: if peak > 0 {
            Fig::Known(peak)
        } else {
            unknown()
        },
        wait_ms: run
            .admitted_ms
            .map_or_else(unknown, |admitted| since(admitted, run.start_ms)),
        cache: Fig::Unknown("targo".to_owned()),
        outcome,
        stop,
        code,
        sig,
        meter: Meter::Sampled,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;

    fn run(ledger: &Path) -> Run {
        Run {
            start_ms: 1_000,
            admitted_ms: Some(4_000),
            what: "targo test -p secret-crate".to_owned(),
            checkout: Some("/Users//me/secret-project".to_owned()),
            ledger: Some(ledger.to_path_buf()),
        }
    }

    fn facts() -> HostFacts {
        HostFacts {
            host: "m101.local".to_owned(),
            model: "Mac16,7".to_owned(),
            cpu: "Apple M4 Pro".to_owned(),
            cores: 14,
            mem_mib: 24_576,
            os: "macOS 27.0.1".to_owned(),
            aterm: "0.160.0+gtest".to_owned(),
        }
    }

    fn at(end: BuildEnd) -> Row {
        let f = facts();
        row(
            &run(Path::new("/l")),
            "targo test",
            &end,
            3 << 30,
            10_000,
            &f,
            f.digest(),
        )
    }

    /// `exit(code)`'s wait status.
    fn exited(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn a_build_row_says_its_wall_wait_peak_and_no_words() {
        let r = at(BuildEnd::Status(exited(0)));
        assert_eq!(r.kind, Kind::Test, "the verb test");
        assert_eq!(r.wall_ms, Fig::Known(6_000), "admission to end");
        assert_eq!(r.wait_ms, Fig::Known(3_000), "admitted less asked");
        assert_eq!(r.peak_bytes, Fig::Known(3 << 30));
        assert_eq!(
            (r.cpu_ms.clone(), r.cache.clone()),
            (Fig::Na, Fig::Unknown("targo".to_owned()))
        );
        assert_eq!((r.label.as_str(), r.host.as_str()), ("targo", "m101"));
        assert_eq!(r.meter, Meter::Sampled);
        let line = r.line();
        assert!(
            !line.contains("secret-crate") && !line.contains("secret-project"),
            "{line}"
        );
        let f = facts();
        let check = row(
            &run(Path::new("/l")),
            "targo trust check",
            &BuildEnd::Unknown,
            0,
            10_000,
            &f,
            f.digest(),
        );
        assert_eq!(check.kind, Kind::Build, "every verb but test");
        assert_eq!(
            check.peak_bytes,
            Fig::Unknown("aterm-capacity".to_owned()),
            "no peak read"
        );
        let unadmitted = Run {
            admitted_ms: None,
            ..run(Path::new("/l"))
        };
        let r = row(
            &unadmitted,
            "targo build",
            &BuildEnd::Unknown,
            1,
            10_000,
            &f,
            f.digest(),
        );
        assert_eq!(
            (r.wall_ms, r.wait_ms),
            (
                Fig::Unknown("aterm-capacity".to_owned()),
                Fig::Unknown("aterm-capacity".to_owned())
            ),
            "no grant read, no clock"
        );
    }

    /// The stamps are wall-clock times: a clock set back between them gives no interval, never a
    /// known 0. NEGATIVE CONTROL: stamps in order give theirs.
    #[test]
    fn a_clock_set_back_is_no_known_interval() {
        let f = facts();
        let stepped = Run {
            start_ms: 5_000,
            admitted_ms: Some(4_000),
            ..run(Path::new("/l"))
        };
        let r = row(
            &stepped,
            "targo build",
            &BuildEnd::Unknown,
            1,
            3_000,
            &f,
            f.digest(),
        );
        assert_eq!(
            (r.wall_ms, r.wait_ms),
            (
                Fig::Unknown("clock".to_owned()),
                Fig::Unknown("clock".to_owned())
            )
        );
        let r = row(
            &run(Path::new("/l")),
            "targo build",
            &BuildEnd::Unknown,
            1,
            4_000,
            &f,
            f.digest(),
        );
        assert_eq!((r.wall_ms, r.wait_ms), (Fig::Known(0), Fig::Known(3_000)));
    }

    #[test]
    fn each_end_maps_to_its_outcome_and_stop() {
        let words = |end| {
            let r = at(end);
            (r.outcome, r.stop, r.code, r.sig)
        };
        assert_eq!(
            words(BuildEnd::Status(exited(0))),
            (Outcome::Ok, Stop::Exit, Some(0), None)
        );
        assert_eq!(
            words(BuildEnd::Status(exited(101))),
            (Outcome::Fail, Stop::Exit, Some(101), None)
        );
        assert_eq!(
            words(BuildEnd::Status(ExitStatus::from_raw(9))),
            (Outcome::Fail, Stop::Signal, None, Some(9))
        );
        assert_eq!(
            words(BuildEnd::Success(true)),
            (Outcome::Ok, Stop::Unknown, None, None)
        );
        assert_eq!(
            words(BuildEnd::Success(false)),
            (Outcome::Fail, Stop::Unknown, None, None)
        );
        assert_eq!(
            words(BuildEnd::Unknown),
            (Outcome::Unknown, Stop::Unknown, None, None)
        );
        assert_eq!(
            words(BuildEnd::Unfinished),
            (Outcome::Unknown, Stop::Unknown, None, None)
        );
    }

    #[test]
    fn a_run_not_whole_is_known_only_by_a_failing_status() {
        assert_eq!(
            BuildEnd::judged(Some(exited(0)), true),
            BuildEnd::Status(exited(0))
        );
        assert_eq!(
            BuildEnd::judged(Some(exited(0)), false),
            BuildEnd::Unknown,
            "a cancelled run's zero exit is no finished work"
        );
        assert_eq!(
            BuildEnd::judged(Some(ExitStatus::from_raw(2)), false),
            BuildEnd::Status(ExitStatus::from_raw(2)),
            "the signal that cancelled it"
        );
        assert_eq!(BuildEnd::judged(None, true), BuildEnd::Unknown);
        assert_eq!(BuildEnd::of_roots(3, 0, 0), BuildEnd::Unknown);
        assert_eq!(BuildEnd::of_roots(3, 3, 0), BuildEnd::Success(true));
        assert_eq!(BuildEnd::of_roots(3, 3, 1), BuildEnd::Success(false));
        assert_eq!(
            BuildEnd::of_roots(3, 1, 1),
            BuildEnd::Success(false),
            "a failure is known though roots are missing"
        );
        assert_eq!(
            BuildEnd::of_roots(3, 2, 0),
            BuildEnd::Unfinished,
            "a root never validated: not known to have succeeded"
        );
        // The book's success is the old one at every site.
        assert!(BuildEnd::judged(Some(exited(0)), true).succeeded());
        assert!(!BuildEnd::judged(Some(exited(0)), false).succeeded());
        assert!(!BuildEnd::judged(Some(exited(1)), true).succeeded());
        assert!(!BuildEnd::Unknown.succeeded());
        // The epoch table's old rule, `completed != 0 && failed == 0`, at each count.
        for roots in 0..4 {
            for completed in 0..=roots {
                for failed in 0..=completed {
                    assert_eq!(
                        BuildEnd::of_roots(roots, completed, failed).succeeded(),
                        completed != 0 && failed == 0,
                        "{roots} {completed} {failed}"
                    );
                }
            }
        }
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
        let end = BuildEnd::Status(exited(3));
        let write = |row: &Row| aterm_telemetry::direct::write(dir.clone(), &facts(), row, &lost);
        assert!(
            !ended_in(&run(&scratch), "targo test", &end, 1, &users, write),
            "a scratch ledger writes no row"
        );
        assert!(!dir.exists(), "nothing was written");
        let none = Run {
            ledger: None,
            ..run(&users)
        };
        assert!(!ended_in(&none, "targo test", &end, 1, &users, write));
        let beside = tmp.path().join("ledger-2");
        std::fs::create_dir(&beside).expect("beside");
        assert!(
            !ended_in(&run(&beside), "targo test", &end, 1, &users, write),
            "a folder whose name the user's begins is no folder of it"
        );
        assert!(!dir.exists(), "nothing was written");
        // A cohort claim's file lies in a subfolder of the ledger's.
        let cohort = users.join("active.noindex").join("key");
        std::fs::create_dir_all(&cohort).expect("cohort");
        assert!(ended_in(
            &run(&cohort),
            "targo build",
            &end,
            1,
            &users,
            write
        ));
        assert!(ended_in(&run(&users), "targo test", &end, 1, &users, write));
        let read = aterm_telemetry::log::read(&dir, 4, None).expect("read");
        assert_eq!(read.rows.len(), 2);
        assert_eq!(read.rows[0].kind, Kind::Build);
        let r = &read.rows[1];
        assert_eq!(
            (&r.kind, r.outcome, r.code, r.label.as_str()),
            (&Kind::Test, Outcome::Fail, Some(3), "targo")
        );
    }
}
