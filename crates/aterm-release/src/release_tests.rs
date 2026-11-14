// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_malformed_tree_admission_stops_that_branch_before_barrier_release() {
    let valid = BranchStop::default();
    apply_tree_admission(true, &valid);
    assert!(!valid.fired());

    let malformed = BranchStop::default();
    apply_tree_admission(false, &malformed);
    assert!(
        malformed.fired(),
        "NEGATIVE CONTROL: an invalid early memory shape cannot later receive reminted tokens"
    );
}

struct AllFreshHandoff {
    expected: std::sync::atomic::AtomicBool,
    admission_waiting: std::sync::atomic::AtomicBool,
    admission_barrier_held: std::sync::atomic::AtomicBool,
    bundle_finished: std::sync::atomic::AtomicBool,
    tree_started: std::sync::atomic::AtomicBool,
    clock: std::sync::atomic::AtomicU64,
}

impl AllFreshHandoff {
    fn new() -> Self {
        Self {
            expected: false.into(),
            admission_waiting: false.into(),
            admission_barrier_held: true.into(),
            bundle_finished: false.into(),
            tree_started: false.into(),
            clock: 1.into(),
        }
    }

    fn await_flag(&self, flag: &std::sync::atomic::AtomicBool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "hung waiting for {what}"
            );
            aterm_uds::deadline::poll_yield();
        }
    }
}

impl Branches for AllFreshHandoff {
    fn say(&self, _line: &str) {}

    fn now(&self) -> u64 {
        self.clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn interrupted(&self) -> bool {
        false
    }

    fn branch_log(&self, branch: Branch) -> PathBuf {
        PathBuf::from(format!("/fixture/{}.log", branch.name()))
    }

    fn expect_tree_index(&self) {
        self.expected
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn build_admitted(&self, ended: &dyn Fn() -> bool) {
        assert!(
            self.expected.load(std::sync::atomic::Ordering::SeqCst),
            "the graph declares the tree index before admitting an all-Fresh build"
        );
        assert!(
            self.admission_barrier_held
                .load(std::sync::atomic::Ordering::SeqCst),
            "NEGATIVE CONTROL: a bundle keeps its initial CPU reservation until build \
             admission can transfer a serial escape to build"
        );
        self.admission_waiting
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !ended()
            && !self
                .bundle_finished
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "hung waiting for build admission"
            );
            aterm_uds::deadline::poll_yield();
        }
    }

    fn release_admission_barrier(&self) {
        self.admission_barrier_held
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn export(
        &self,
        _c: &Candidate,
        _prover: &Prover,
        _target: StageTarget,
        _stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        unreachable!("the fixture has no export branch")
    }

    fn build(
        &self,
        _c: &Candidate,
        _reserved: &Reservation,
        _prover: &Prover,
        work: BuildWork,
        _stop: &BranchStop,
    ) -> Built {
        assert!(work.bundle && work.fixtures);
        self.await_flag(&self.admission_waiting, "the graph's build admission wait");
        // No slice-hold observation occurs: completion itself is the all-Fresh handoff.
        self.bundle_finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Built {
            artifact: None,
            fixtures: None,
        }
    }

    fn verify(
        &self,
        _step: Step,
        _c: &Candidate,
        _prover: &Prover,
        run: &crate::check_record::VerifyRun,
        _artifact: Option<&Artifact>,
        _stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        assert!(run.checks.iter().any(|check| check == "test"));
        assert!(
            self.expected.load(std::sync::atomic::Ordering::SeqCst)
                && self
                    .bundle_finished
                    .load(std::sync::atomic::Ordering::SeqCst),
            "the tree starts only after the all-Fresh bundle releases admission"
        );
        assert!(
            !self
                .admission_barrier_held
                .load(std::sync::atomic::Ordering::SeqCst),
            "a non-lint resume releases the initial CPU reservation after build admission \
             and before tree work"
        );
        self.tree_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Ran {
            code: 0,
            detail: Vec::new(),
        })
    }

    fn side_log(&self, _side: Side) -> PathBuf {
        unreachable!("the fixture has no publish branch")
    }

    fn publish_gate(&self) -> PublishGate {
        unreachable!("the fixture has no publish branch")
    }

    fn source(
        &self,
        _prover: &Prover,
        _gate: &PublishGate,
        _stop: &BranchStop,
    ) -> Result<Ran, Refusal> {
        unreachable!("the fixture has no publish branch")
    }

    fn sign(&self, _args: &[String], _prover: &Prover, _stop: &BranchStop) -> Ran {
        unreachable!("the fixture has no publish branch")
    }
}

/// The qualify graph cannot miss a fast all-Fresh bundle between ledger samples: it declares
/// the forthcoming test index, waits for build admission, and starts the tree only after
/// bundle completion. NEGATIVE CONTROL: removing any of those three graph edges panics or
/// reaches the one-minute hang detector instead of starting the tree.
#[test]
fn an_all_fresh_bundle_releases_the_tree_and_preserves_the_fixture_handoff() {
    let branches = AllFreshHandoff::new();
    let candidate = Candidate {
        commit: "11".repeat(20),
        version: "0.0.0".into(),
        why: "fixture".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let prover = Prover {
        bin: PathBuf::from("/fixture/prover/bin"),
        commit: "22".repeat(20),
        source: "fixture".into(),
    };
    let reservation = Reservation {
        build: 1,
        commit: "33".repeat(20),
        line: "1 0.0.0".into(),
    };
    let plan = Plan {
        appkit: None,
        build: Some(BuildWork {
            bundle: true,
            fixtures: true,
        }),
        tree: Some(crate::check_record::VerifyRun {
            half: crate::check_record::Half::Tree,
            checks: vec!["test".into()],
            passed_binaries: Vec::new(),
            passed_lint_packages: Vec::new(),
            passed_appkit_groups: Vec::new(),
        }),
        export: None,
    };
    let graph = run_graph(&branches, &candidate, &prover, Some(&reservation), &plan);
    assert!(
        branches
            .tree_started
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert!(
        graph
            .ended
            .iter()
            .filter(|(_, ended, _)| *ended == Ended::Passed)
            .count()
            == 2,
        "both build and tree pass: {:?}",
        graph.ended
    );
}

/// Step 10 distinguishes an unanswered freshness read from an answered channel verdict.
/// NEGATIVE CONTROLS: a response with no tag and one with the wrong tag remain ordinary
/// refusals, while the expected tag has no refusal.
#[test]
fn latest_transport_is_not_a_channel_refusal() {
    let transport = "curl asset request failed with HTTP 504";
    let unavailable = latest_refusal("0.175.0", Err(transport.into())).unwrap();
    assert!(unavailable.could_not_run);
    assert!(
        unavailable
            .detail
            .iter()
            .any(|line| line.contains(transport)),
        "the transport's own detail is retained"
    );

    for answered in [None, Some("v0.174.0".to_string())] {
        let semantic = latest_refusal("0.175.0", Ok(answered.clone())).unwrap();
        assert!(
            !semantic.could_not_run,
            "an answered channel with {answered:?} made a semantic decision"
        );
    }
    assert!(latest_refusal("0.175.0", Ok(Some("v0.175.0".into()))).is_none());
}

/// An exec'd image continues the live claim of its exact pid and start, atomically replacing
/// its logs and retaining the preceding image's different logs. The continued image's end
/// removes the claim. NEGATIVE CONTROLS: changing either half of the process identity refuses
/// the continuation, and appending the new body instead of replacing it leaves the old logs
/// as the first `logs` field [`read_holder`] reads.
#[test]
fn an_execed_image_continues_only_its_exact_process_claim() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(RUNNING_FILE);
    let me = (4242, "Thu Oct  9 02:00:00 2026");
    let reused_pid = (me.0, "Thu Oct  9 02:01:00 2026");
    let another_pid = (4343, me.1);
    let mut started_of = |pid: u32| {
        [me, another_pid]
            .into_iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, at)| at.to_string())
    };
    let logs = Path::new("/l");
    let claim = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
    assert!(claim.names_this_run());
    drop(claim);
    assert!(!path.exists(), "a claim nobody took is removed");

    let claim = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
    assert!(!claim.continued && claim.previous_logs.is_none());
    for identity in [reused_pid, another_pid] {
        let refused = RunClaim::take(&path, identity, Path::new("/wrong"), &mut started_of);
        assert!(
            matches!(&refused, Err(Ok(Holder { pid, .. })) if *pid == me.0),
            "a different process identity must not continue this claim: {refused:?}"
        );
    }
    let continued_logs = Path::new("/l2");
    let continued = RunClaim::take(&path, me, continued_logs, &mut started_of).unwrap();
    assert!(continued.continued && continued.names_this_run());
    assert_eq!(
        continued.previous_logs.as_deref(),
        Some(logs),
        "the preceding image's logs"
    );
    assert_eq!(
        read_holder(&path).map(|h| h.logs),
        Some(continued_logs.display().to_string()),
        "the atomic replacement leaves one complete current body"
    );
    std::mem::forget(claim); // a successful exec does not run the preceding image's Drop
    drop(continued);
    assert!(!path.exists(), "the continued image's end removes it");
}

/// A cutter already running when B1 lands may have written the old `handed` marker before it
/// starts R's newly built cutter as a child. That one live ancestor handoff is adopted and
/// rewritten without the marker; no new code writes it. NEGATIVE CONTROLS: ancestry without
/// the marker, and the marker on a live non-ancestor, are both ordinary competing runs.
#[test]
fn a_pre_b1_handoff_is_adopted_once_only_from_its_live_ancestor() {
    use std::io::Write as _;

    let parent = (4242, "Thu Oct  9 02:00:00 2026");
    let child = (4343, "Thu Oct  9 02:00:01 2026");
    let mut started_of = |pid: u32| {
        [parent, child]
            .into_iter()
            .find(|(found, _)| *found == pid)
            .map(|(_, started)| started.to_string())
    };

    for (name, mark_handed, ancestors, adopted) in [
        ("authentic", true, vec![parent.0], true),
        ("unmarked", false, vec![parent.0], false),
        ("unrelated", true, vec![9999], false),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(RUNNING_FILE);
        let old_logs = Path::new("/old/logs");
        let old =
            RunClaim::take_locking(&path, parent, old_logs, &mut started_of, &[], &claim_lock)
                .unwrap();
        if mark_handed {
            let mut file = crate::durable::open_append(&path, false).unwrap();
            file.write_all(b"handed\n").unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "the legacy-handoff fixture must make the old cutter's marker durable before adoption"
            )]
            file.sync_all().unwrap();
        }

        let answer = RunClaim::take_locking(
            &path,
            child,
            Path::new("/new/logs"),
            &mut started_of,
            &ancestors,
            &claim_lock,
        );
        if adopted {
            let claim = answer.unwrap();
            assert!(claim.continued && claim.legacy_handoff, "{name}");
            assert_eq!(claim.previous_logs.as_deref(), Some(old_logs), "{name}");
            assert!(claim.names_this_run(), "{name}");
            assert!(!read_holder(&path).unwrap().legacy_handed, "{name}");
            drop(claim);
            assert!(!path.exists(), "{name}: the adopted owner removes it");
        } else {
            assert!(
                matches!(answer, Err(Ok(Holder { pid, .. })) if pid == parent.0),
                "{name}: only the authenticated legacy handoff may be adopted"
            );
            assert!(old.names_this_run(), "{name}: the parent retains its claim");
        }
        drop(old);
    }
}

/// A continuation never lands between this image's read of its claim and the removal that
/// read decided: the take, made at that moment without waiting for the claim's lock, finds it
/// held and writes nothing; the removal goes on, and a later take claims anew. The
/// interleaving is the test's: `remove_if_mine` (RunClaim's drop) runs the take between its
/// read and its removal. NEGATIVE CONTROL: without the common lock, the continuation lands
/// and the removal then deletes its rewritten claim.
#[test]
fn a_continuation_never_lands_between_a_claims_read_and_removal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(RUNNING_FILE);
    let me = (4242, "Thu Oct  9 02:00:00 2026");
    let mut started_of = |pid: u32| (pid == me.0).then(|| me.1.to_string());
    let logs = Path::new("/l");
    let at_once = |path: &Path| {
        aterm_update_core::FileLock::acquire_within(
            &path.with_extension("lock"),
            std::time::Duration::ZERO,
        )
    };
    let claim = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
    let mut between = None;
    claim.remove_if_mine(|| {
        between = Some(RunClaim::take_locking(
            &path,
            me,
            Path::new("/l2"),
            &mut started_of,
            &[],
            &at_once,
        ));
    });
    let between = between.expect("the claim named this run when it was read");
    assert!(
        matches!(&between, Err(Err(why)) if why.starts_with("lock ")),
        "the take did not land between the read and the removal: {between:?}"
    );
    assert!(
        !path.exists(),
        "the claim this run read as its own is removed"
    );
    drop(claim);
    let took = RunClaim::take(&path, me, logs, &mut started_of).unwrap();
    assert!(
        !took.continued && took.names_this_run(),
        "the process claims anew after the removal"
    );
}

#[test]
fn estimates_takes_upper_median_of_last_five_and_formats_durations() {
    let tsv = "\
1\t0.1.0\teven\t10\n\
2\t0.1.0\teven\t40\n\
malformed\trow\n\
3\t0.1.0\teven\tnan\n\
3\t0.1.0\teven\t+99\n\
4\t0.1.0\twindow\t1000\n\
5\t0.1.0\twindow\t5\n\
6\t0.1.0\twindow\t30\n\
7\t0.1.0\twindow\t10\n\
8\t0.1.0\twindow\t20\n\
9\t0.1.0\twindow\t40\n";
    let got = estimates(tsv);
    assert_eq!(
        got.get("even"),
        Some(&40),
        "sorted[2 / 2] is the upper median"
    );
    assert_eq!(
        got.get("window"),
        Some(&20),
        "only the last five runs (5, 30, 10, 20, 40) participate"
    );
    assert_eq!(got.len(), 2);

    assert_eq!(fmt_dur(0), "0s");
    assert_eq!(fmt_dur(59), "59s");
    assert_eq!(fmt_dur(60), "1m00s");
    assert_eq!(fmt_dur(185), "3m05s");
    assert_eq!(fmt_dur(3600), "1h00m");
    assert_eq!(fmt_dur(9480), "2h38m");

    assert_eq!(public_version("0.1.0"), Some((0, 1, 0)));
    assert_eq!(public_version("00.1.0"), None);
    assert_eq!(public_version("0.01.0"), None);
    assert_eq!(public_version("1.20261109.2"), Some((1, 20261109, 2)));
    assert!(newer("1.20261109.2", "1.20261109.1"));
    assert!(!newer("1.20261109.1", "1.20261109.2"));
    assert!(newer("1.20261110.1", "1.20261109.999999999"));
    assert_eq!(public_version("1.20261109.0"), None);
    assert_eq!(public_version("1.20260229.1"), None);
    assert!(!newer("+2.0", "1.0"));
    assert!(!newer("1.+2", "1.1"));
}

/// A red's line accounts for every branch: what it stopped, what had ended and how, and a build
/// it leaves running on purpose. NEGATIVE CONTROL: the bare `nothing` of a red that came last
/// said none of it.
#[test]
fn a_red_line_names_what_it_stops_what_had_ended_and_what_goes_on() {
    assert_eq!(red_stops(&["build", "export"], &[], false), "build, export");
    assert_eq!(red_stops(&[], &[], false), "nothing");
    assert_eq!(
        red_stops(
            &[],
            &[
                (Branch::Export, Ended::Passed, 751),
                (Branch::Build, Ended::Passed, 7_500)
            ],
            false
        ),
        format!(
            "nothing (ended before it: export passed after {}, build passed after {})",
            fmt_dur(751),
            fmt_dur(7_500)
        )
    );
    let kept = red_stops(&["export"], &[], true);
    assert!(
        kept.starts_with("export; the build goes on") && kept.contains("the next hop"),
        "{kept}"
    );
    let unsettled = red_stops(&[], &[(Branch::Export, Ended::CouldNotRun, 9)], true);
    assert!(
        unsettled.starts_with("nothing (ended before it: export could not run after ")
            && unsettled.contains("the build goes on"),
        "{unsettled}"
    );
}
