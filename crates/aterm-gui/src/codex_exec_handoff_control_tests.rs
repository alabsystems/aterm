// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use codex_rotation::NoHandoff;

pub(super) fn intent() -> codex_renewal::Intent {
    codex_renewal::Intent {
        purpose: codex_renewal::Purpose::WeeklyQuota,
        thread: "00000000-0000-0000-0000-000000000001".into(),
        wall: "00000000-0000-0000-0000-000000000002".into(),
        identity: codex_renewal::Identity::from_native("source", "user"),
        limit: None,
    }
}

type Read = Result<Option<codex_rotation::Proposal<ExecCommand>>, &'static str>;

/// The progress once a job's first read, come back at `at`, refused the session for `why`.
fn refused(why: &'static str, at: Instant) -> Progress {
    progress_after_observation(&Progress::Reading(0), Err(why), true, at)
}

/// A READ'S REFUSAL THAT STANDS ENDS THE ASKING, UNDER ITS OWN KIND AND TIME: each refusal the
/// pool says stands becomes the answer that no handoff is coming, of the kind the pool names,
/// decided when the read came back and the first such read of its wait. A read the session
/// moved on from, a refusal that does not stand and a read that selected nothing are asked again
/// shortly.
#[test]
fn a_standing_refusal_is_kept_with_its_kind_and_the_time_it_was_read() {
    let now = Instant::now();
    for (why, end) in [
        ("not-walled", NoHandoff::NoWallToLeave),
        ("source-unbound", NoHandoff::NoRowOfThePool),
        ("source-missing", NoHandoff::NoRowOfThePool),
        ("source-unmanaged", NoHandoff::NoRowOfThePool),
        ("source-names-another", NoHandoff::LoginNamesAnother),
    ] {
        let first = NoneComing {
            end,
            decided: now,
            reads: 1,
            asked: now,
        };
        assert!(
            matches!(refused(why, now), Progress::NotNeeded(none) if none == first),
            "{why}"
        );
        assert!(
            matches!(
                progress_after_observation(&Progress::Reading(0), Err(why), false, now),
                Progress::Retry(at) if at > now
            ),
            "stale {why}"
        );
    }
    let again: [Read; 3] = [Err("roster-busy"), Err("probe-panicked"), Ok(None)];
    for result in again {
        assert!(matches!(
            progress_after_observation(&Progress::Reading(0), result, true, now),
            Progress::Retry(at) if at > now
        ));
    }
}

/// NO WALL TO LEAVE IS READ AGAIN ONCE THE WALL IT JUDGED HAS RESET: an exec run is one job at
/// every wall it meets, so the answer that its login stood at no wall a handoff leaves answers
/// its asks only until the nearest reset that answer waited out, and its grace, have passed
/// (`RESET_SOON_S + RESET_GRACE_S`). A moment inside that time the ask is answered `not-needed`
/// and nothing is read; at it, the ask starts a read and is answered `waiting`, as is each ask
/// while that read is out, which starts no second one.
#[test]
fn no_wall_to_leave_stands_until_its_wall_has_reset_and_is_then_read_again() {
    assert_eq!(
        NO_WALL_STANDS,
        Duration::from_secs((accounts::RESET_SOON_S + accounts::RESET_GRACE_S).unsigned_abs())
    );
    let decided = Instant::now();
    let intent = intent();
    let mut progress = refused("not-walled", decided);
    for inside in [Duration::ZERO, NO_WALL_STANDS - Duration::from_millis(1)] {
        assert_eq!(
            answer(&mut progress, &intent, decided + inside),
            (NOT_NEEDED.to_owned(), false),
            "{inside:?}"
        );
        assert!(
            matches!(
                progress,
                Progress::NotNeeded(NoneComing { end: NoHandoff::NoWallToLeave, decided: at, .. })
                    if at == decided
            ),
            "{inside:?}: the answer stands as it was decided"
        );
    }
    let past = decided + NO_WALL_STANDS;
    assert_eq!(
        answer(&mut progress, &intent, past),
        (WAITING.to_owned(), true),
        "the first ask past it reads the login again"
    );
    assert!(matches!(progress, Progress::Reading(_)));
    assert_eq!(
        answer(&mut progress, &intent, past + Duration::from_secs(5)),
        (WAITING.to_owned(), false),
        "one read at a time"
    );
}

/// The seconds from each read's answer to the next read's start, for `reads` reads, of a session
/// that asks every five seconds from `from` on and whose login reads as no wall to leave each
/// time; `progress` is what it asks of.
fn seconds_between_reads(progress: &mut Progress, from: Instant, reads: usize) -> Vec<u64> {
    let intent = intent();
    let mut gaps = Vec::new();
    let (mut now, mut decided) = (from, from);
    while gaps.len() < reads {
        now += Duration::from_secs(5);
        let (reply, starts) = answer(progress, &intent, now);
        if starts {
            assert_eq!(reply, WAITING);
            gaps.push((now - decided).as_secs());
            *progress = progress_after_observation(progress, Err("not-walled"), true, now);
            decided = now;
        } else {
            assert_eq!(reply, NOT_NEEDED, "{:?} in", now - decided);
        }
    }
    gaps
}

/// A WAIT THAT KEEPS READING AS NO WALL IS READ BY RUNGS, NOT AT A FLAT PACE: a session that
/// asks every five seconds, and whose login reads as no wall to leave each time, is read again
/// `RESET_SOON_S + RESET_GRACE_S` after the first read, then by the rungs a wall with no reset
/// stands by (30 minutes, 2 hours, 6 hours, 6 hours), and is answered `not-needed` at every ask
/// between. A flat stand would read its provider about 82 times a day for as long as it asks.
#[test]
fn a_wait_that_keeps_reading_as_no_wall_is_read_by_rungs() {
    let decided = Instant::now();
    let mut progress = refused("not-walled", decided);
    let [first, second, last] = accounts::NO_RESET_WALL_S.map(i64::unsigned_abs);
    assert_eq!(
        seconds_between_reads(&mut progress, decided, 5),
        [NO_WALL_STANDS.as_secs(), first, second, last, last]
    );
}

/// A SESSION THAT STOPPED ASKING WAITS AT ANOTHER WALL, FROM THE FIRST STAND: the stands one
/// wait climbed to are that wait's. A session whose asks were all within `STILL_ASKING` of the
/// one before is still inside its stand; one that then stops asking for longer than that, and
/// asks again, ran in between: its ask is judged by the first stand
/// (`RESET_SOON_S + RESET_GRACE_S` from the answer), so it is read at once here, and its new
/// wait climbs from the first rung again.
#[test]
fn a_session_that_stopped_asking_is_read_from_the_first_stand_again() {
    let decided = Instant::now();
    let intent = intent();
    let mut progress = refused("not-walled", decided);
    // Three reads in a row said no wall: the answer in hand stands two hours.
    seconds_between_reads(&mut progress, decided, 2);
    let Progress::NotNeeded(NoneComing { decided, asked, .. }) = progress else {
        panic!("{progress:?}");
    };
    assert_eq!(asked, decided);
    let step = STILL_ASKING;
    let mut now = decided;
    while now - decided < NO_WALL_STANDS + step {
        now += step;
        assert_eq!(
            answer(&mut progress, &intent, now),
            (NOT_NEEDED.to_owned(), false),
            "still asking, {:?} into a two-hour stand",
            now - decided
        );
    }
    now += step + Duration::from_millis(1);
    assert_eq!(
        answer(&mut progress, &intent, now),
        (WAITING.to_owned(), true),
        "asked again after stopping, past the first stand"
    );
    progress = progress_after_observation(&progress, Err("not-walled"), true, now);
    assert_eq!(
        seconds_between_reads(&mut progress, now, 2),
        [
            NO_WALL_STANDS.as_secs(),
            accounts::NO_RESET_WALL_S[0].unsigned_abs()
        ],
        "the new wait climbs from the first rung"
    );
}

/// WHAT NO WAIT CHANGES STANDS AT ANY AGE: a login that is no row of the pool, or that names
/// another account than its row's, is answered `not-needed` however long ago it was read, and
/// starts no read.
#[test]
fn a_login_no_wait_changes_is_answered_not_needed_at_any_age() {
    let decided = Instant::now();
    let intent = intent();
    for (why, end) in [
        ("source-unbound", NoHandoff::NoRowOfThePool),
        ("source-names-another", NoHandoff::LoginNamesAnother),
    ] {
        let mut progress = refused(why, decided);
        for age in [
            Duration::ZERO,
            NO_WALL_STANDS,
            NO_WALL_STANDS + Duration::from_secs(1),
            Duration::from_secs(7 * 24 * 3600),
        ] {
            assert_eq!(
                answer(&mut progress, &intent, decided + age),
                (NOT_NEEDED.to_owned(), false),
                "{why} at {age:?}"
            );
            assert!(
                matches!(
                    progress,
                    Progress::NotNeeded(NoneComing { end: kind, decided: at, .. })
                        if kind == end && at == decided
                ),
                "{why} at {age:?}"
            );
        }
    }
}

/// AN ASK THAT STARTS A READ STARTS IT, ONCE AND UNLOCKED: a new job's first ask, and the first
/// ask past a no-wall answer's standing time, each run the read exactly once and are answered
/// `waiting`, with the progress already `Reading` and its lock free for the read to write its
/// result or its failure to start. No other ask runs it: not one while that read is out, and
/// not one an answer still stands for.
#[test]
fn an_ask_that_starts_a_read_runs_it_once_with_the_progress_unlocked() {
    let decided = Instant::now();
    let intent = intent();
    let expired = decided + NO_WALL_STANDS;
    for (first, at) in [
        (Progress::Retry(decided), decided),
        (refused("not-walled", decided), expired),
    ] {
        let progress = Mutex::new(first);
        let reads = std::cell::Cell::new(0_u32);
        let read = || {
            reads.set(reads.get() + 1);
            assert!(
                matches!(progress.try_lock().as_deref(), Ok(Progress::Reading(_))),
                "the read runs with the progress unlocked and already reading"
            );
        };
        assert_eq!(ask(&progress, &intent, at, read), WAITING);
        assert_eq!(reads.get(), 1, "the ask that starts a read runs it");
        assert_eq!(
            ask(&progress, &intent, at + Duration::from_secs(5), read),
            WAITING
        );
        assert_eq!(reads.get(), 1, "one read at a time");
    }
    let standing = Mutex::new(refused("not-walled", decided));
    assert_eq!(
        ask(&standing, &intent, decided + Duration::from_secs(5), || {
            panic!("an answer that stands reads nothing")
        }),
        NOT_NEEDED
    );
}

/// A selection whose read came back at `at`: a proposal around the pool's own plan for a session
/// it plans nothing for, which reads no file, so the test needs no pool on disk.
pub(super) fn selected(at: Instant) -> Progress {
    use aterm_agent::harness::accounts_pool;
    let nowhere = std::path::Path::new("/nonexistent");
    let seen = aterm_agent::harness::observe::Observation::Unread;
    let plan = accounts_pool::native_exec_quota_plan(
        &accounts_pool::Seat {
            config_dir: nowhere,
            state: nowhere,
            home: nowhere,
            now: 0,
        },
        &accounts_pool::Session {
            cli: accounts::Cli::Claude,
            sid: "session",
            supervisor: accounts::Supervisor::Own,
            env: &[],
            seen: &seen,
            sample: None,
            point: accounts::Point::TurnEnd,
            mode: accounts::Mode::Wall,
            relaunch: (),
        },
        std::path::Path::new("/codex"),
        |_| {},
        |_| None,
    );
    let row = |label: &str| accounts::Row {
        label: label.into(),
        ..Default::default()
    };
    Progress::Selected(
        Box::new(codex_rotation::Proposal {
            plan,
            source: row("main"),
            destination: row("work"),
            source_identity: intent().identity,
            destination_identity: target(),
            old_auth: "/main/auth.json".into(),
            new_auth: "/work/auth.json".into(),
        }),
        at,
    )
}

/// The identity [`selected`]'s destination names.
pub(super) fn target() -> codex_renewal::Identity {
    codex_renewal::Identity::from_native("target", "another")
}

/// What `prepare` answers while [`selected`] is offered.
fn offer() -> String {
    let (intent, target) = (intent(), target());
    format!(
        "OK codex-exec-handoff state=selected thread={} turn={} source_account={} source_user={} target_account={} target_user={}\n",
        intent.thread,
        intent.wall,
        intent.identity.account,
        intent.identity.user,
        target.account,
        target.user
    )
}

/// What `prepare` answers while a selection of `proposal` is offered.
fn offer_of(proposal: &codex_rotation::Proposal<ExecCommand>) -> String {
    let (intent, source, target) = (
        intent(),
        &proposal.source_identity,
        &proposal.destination_identity,
    );
    format!(
        "OK codex-exec-handoff state=selected thread={} turn={} source_account={} source_user={} target_account={} target_user={}\n",
        intent.thread, intent.wall, source.account, source.user, target.account, target.user
    )
}

type Written = Result<Option<()>, &'static str>;

const DEADLINE_US: u64 = 987_654_321;

/// What a `reserve` that waits is answered.
fn waiting() -> String {
    format!("OK codex-exec-handoff state=waiting deadline_us={DEADLINE_US}\n")
}

/// A `reserve` whose pool commit was refused, of an operation whose selection's read came back at
/// `read` and that no `reserve`'s writer has held.
fn reserve(read: Instant) -> Step {
    Step::Reserve {
        deadline_us: DEADLINE_US,
        armed: Armed { read, held: false },
    }
}

/// `arm`, and a `reserve` whose pool commit was refused under a selection read at `read`, each with
/// what it answers a session it neither denies nor lets go on while that selection is young.
fn steps(read: Instant) -> [(Step, String); 2] {
    [(Step::Arm, DENIED.to_owned()), (reserve(read), waiting())]
}

/// Every refusal of the selection's writer that says the selection no longer holds.
const REFUTING: [&str; 11] = [
    "not-ready",
    "rotation-policy",
    "policy-changed",
    "pool-commit-changed",
    "account-lineage",
    "selection-changed",
    "target-unavailable",
    "source-held",
    "source-conflict",
    "same-login",
    "identity-mismatch",
];

/// A WRITER THAT SAYS NOTHING OF THE SELECTION KEEPS IT, AND NOTHING IS READ: the store's writer
/// taken (`Ok(None)`), the sign-in turn, a login file's lock, a file that did not read, a receipt
/// that could not be written and a word nobody listed leave the selection as it was read. An
/// `arm` is denied and a `reserve` waits inside its grant; the session's next `prepare` is
/// offered the same selection and starts no read, and its next `arm` takes it.
#[test]
fn a_writer_that_says_nothing_of_the_selection_keeps_it_for_the_next_arm() {
    let read = Instant::now();
    let intent = intent();
    let target = target();
    let silent: [Written; 13] = [
        Ok(None),
        Err("signin-busy"),
        Err("signin-lock"),
        Err("auth-busy"),
        Err("auth-lock"),
        Err("receipt-lock"),
        Err("roster-unreadable"),
        Err("bindings-unreadable"),
        Err("state-unreadable"),
        Err("source-unreadable"),
        Err("journal-write"),
        Err("home-unavailable"),
        Err("a-word-nobody-listed"),
    ];
    for (step, reply) in steps(read) {
        for outcome in &silent {
            let mut progress = selected(read);
            assert_eq!(
                after_writer(
                    &mut progress,
                    true,
                    step,
                    outcome,
                    read + Duration::from_secs(1)
                ),
                Some(reply.clone()),
                "{step:?} {outcome:?}"
            );
            assert!(
                matches!(&progress, Progress::Selected(_, at) if *at == read),
                "{step:?} {outcome:?}: {progress:?}"
            );
            let later = read + Duration::from_secs(5);
            assert!(
                selection_to_arm(&progress, &target.account, &target.user, later).is_some(),
                "{step:?} {outcome:?}: the next arm takes it"
            );
            assert_eq!(
                ask(&Mutex::new(progress), &intent, later, || {
                    panic!("a kept selection reads nothing")
                }),
                offer(),
                "{step:?} {outcome:?}"
            );
        }
    }
}

/// A WRITER THAT REFUTES THE SELECTION DROPS IT, AND THE NEXT ASK READS: each refusal that says
/// the selection no longer holds denies the step, an `arm` and a `reserve` alike, and retires the
/// selection the writer judged, so the session's next ask starts a read and no `arm` takes what
/// was refuted. A selection that is not the one judged, and a progress that has moved on to a
/// read, are left as they are, and the step is denied all the same.
#[test]
fn a_writer_that_refutes_the_selection_drops_it_and_the_next_ask_reads() {
    let read = Instant::now();
    let now = read + Duration::from_secs(1);
    let intent = intent();
    let target = target();
    for (step, _) in steps(read) {
        for why in REFUTING {
            let outcome: Written = Err(why);
            let mut progress = selected(read);
            assert_eq!(
                after_writer(&mut progress, true, step, &outcome, now),
                Some(DENIED.to_owned()),
                "{step:?} {why}"
            );
            assert!(
                matches!(progress, Progress::Retry(at) if at == now),
                "{step:?} {why}: {progress:?}"
            );
            assert!(
                selection_to_arm(&progress, &target.account, &target.user, now).is_none(),
                "{step:?} {why}: no arm takes a refuted selection"
            );
            assert_eq!(
                answer(&mut progress, &intent, now),
                (WAITING.to_owned(), true),
                "{step:?} {why}: the next ask reads"
            );

            let mut another = selected(read);
            assert_eq!(
                after_writer(&mut another, false, step, &outcome, now),
                Some(DENIED.to_owned()),
                "{step:?} {why}"
            );
            assert!(
                matches!(&another, Progress::Selected(_, at) if *at == read),
                "{step:?} {why}: another selection stands, {another:?}"
            );

            let mut reading = Progress::Reading(2);
            assert_eq!(
                after_writer(&mut reading, true, step, &outcome, now),
                Some(DENIED.to_owned()),
                "{step:?} {why}"
            );
            assert!(
                matches!(reading, Progress::Reading(2)),
                "{step:?} {why}: a read that is out stays the one read, {reading:?}"
            );
        }
    }
}

/// A WRITER THAT HOLDS THE SELECTION LETS AN ARM GO ON AND A RESERVE WAIT: an `arm` whose receipt
/// the writer created is answered by the operation it goes on to make, and a `reserve` whose pool
/// commit was refused under a selection that still holds waits for it (another session may be
/// the destination's canary), at any age: the writer judged the selection anew. Neither changes
/// the selection.
#[test]
fn a_writer_that_holds_the_selection_lets_an_arm_go_on_and_a_reserve_wait() {
    let read = Instant::now();
    let held: Written = Ok(Some(()));
    for (step, reply) in [(Step::Arm, None), (reserve(read), Some(waiting()))] {
        for age in [Duration::from_secs(1), SELECTION_OFFERED] {
            let mut progress = selected(read);
            assert_eq!(
                after_writer(&mut progress, true, step, &held, read + age),
                reply,
                "{step:?} at {age:?}"
            );
            assert!(
                matches!(&progress, Progress::Selected(_, at) if *at == read),
                "{step:?}: {progress:?}"
            );
        }
    }
}

/// A RESERVE WHOSE WRITER SAYS NOTHING WAITS ONLY AS LONG AS ITS SELECTION IS OFFERED, UNLESS A
/// WRITER HELD IT: a refusal that keeps standing (a missing home, say) is answered `waiting` until
/// the read behind the operation's selection is 50 seconds old, as an offer of it stands, and is
/// then denied, so the session's retries end there and not at the operation's deadline. Of a
/// selection an earlier `reserve`'s writer held, the same silence is answered `waiting` at any
/// age, and only a refutation denies it. The selection is left for `prepare` to lapse.
#[test]
fn a_reserve_whose_writer_says_nothing_waits_only_while_its_selection_is_offered() {
    let read = Instant::now();
    let held = Step::Reserve {
        deadline_us: DEADLINE_US,
        armed: Armed { read, held: true },
    };
    for outcome in [Ok(None), Err("home-unavailable"), Err("signin-busy")] {
        let outcome: Written = outcome;
        let mut progress = selected(read);
        let inside = read + SELECTION_OFFERED - Duration::from_millis(1);
        assert_eq!(
            after_writer(&mut progress, true, reserve(read), &outcome, inside),
            Some(waiting()),
            "{outcome:?}"
        );
        let lapsed = read + SELECTION_OFFERED;
        assert_eq!(
            after_writer(&mut progress, true, reserve(read), &outcome, lapsed),
            Some(DENIED.to_owned()),
            "{outcome:?}"
        );
        for age in [SELECTION_OFFERED, SELECTION_ARMS * 2] {
            assert_eq!(
                after_writer(&mut progress, true, held, &outcome, read + age),
                Some(waiting()),
                "{outcome:?} of a held selection at {age:?}"
            );
        }
        assert!(
            matches!(&progress, Progress::Selected(_, at) if *at == read),
            "{outcome:?}: {progress:?}"
        );
    }
    let refuted: Written = Err("selection-changed");
    assert_eq!(
        after_writer(&mut selected(read), true, held, &refuted, read),
        Some(DENIED.to_owned()),
        "a refutation denies a held selection"
    );
}

/// A real pool on disk that moves a session at its weekly limit off `main`, the default login,
/// onto `work`, and the proposal the agent crate's own `prepare_exec` makes there from a reader
/// that answers `main` walled and `work` usable: a selection with a successor, as an `arm` takes
/// one. The directory lives as long as the first value.
fn moving() -> (
    aterm_tempfile::TempDir,
    codex_rotation::Proposal<ExecCommand>,
) {
    let (dir, read) = prepared(None);
    let proposal = read.expect("a proposal").expect("a move to work");
    assert!(
        proposal.successor().is_some(),
        "a selection with a successor"
    );
    (dir, proposal)
}

/// When [`prepared`]'s pool is read.
const POOL_NOW: i64 = 1_790_000_000;

/// [`moving`]'s pool on disk, and what the agent crate's own `prepare_exec` answers there where the
/// session's login `main` reads `source` (a `/wham/usage` reply; `None`: its week spent) and `work`
/// reads with room.
fn prepared(source: Option<aterm_json::Value>) -> (aterm_tempfile::TempDir, Read) {
    prepared_asking(source, None, &intent())
}

/// [`prepared`] where `work` reads `target` (a `/wham/usage` reply; `None`: with room) and the
/// session asks as `asking` does.
fn prepared_asking(
    source: Option<aterm_json::Value>,
    target: Option<aterm_json::Value>,
    asking: &codex_renewal::Intent,
) -> (aterm_tempfile::TempDir, Read) {
    use aterm_agent::harness::{accounts_pool, codex_app_login, provider_read};
    const NOW: i64 = POOL_NOW;
    struct Reader(std::collections::BTreeMap<std::path::PathBuf, codex_app_login::IdentifiedUsage>);
    impl codex_rotation::UsageReader for Reader {
        fn read(
            &self,
            home: &std::path::Path,
            _: Option<&str>,
            _: i64,
            _: Duration,
            use_token: provider_read::Use,
        ) -> codex_app_login::IdentifiedRead {
            use_token
                .begin_home(provider_read::Read::CodexEndpoint, home, NOW)
                .ok()
                .and_then(|_| self.0.get(home).cloned())
                .into()
        }
    }
    let usage = |account: &str, full: bool| codex_app_login::IdentifiedUsage {
        identity: codex_renewal::Identity::from_native(account, "user"),
        usage: codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": !full,
                    "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                        "reset_at": NOW + 3600},
                    "secondary_window": {"used_percent": if full { 100 } else { 0 },
                        "limit_window_seconds": 604800, "reset_at": NOW + 86_400}
                }
            }),
            NOW,
        )
        .expect("provider percentages"),
    };
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-exec-handoff-moving-")
        .tempdir()
        .expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
    let codex = accounts::Cli::Codex;
    let main = home.join(codex.accounts().default_home).join("auth.json");
    let work = root.join("logins/codex/work/auth.json");
    for login in [&main, &work] {
        std::fs::create_dir_all(login.parent().expect("login dir")).expect("login dir");
        std::fs::write(login, "{}").expect("stand-in login");
    }
    std::fs::create_dir_all(&cfg).expect("cfg");
    std::fs::create_dir_all(&state).expect("state");
    std::fs::write(
        cfg.join(codex.accounts().roster_file),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"work\"\nclass = \"dedicated\"\ndir = {:?}\n",
            work.parent().expect("work dir")
        ),
    )
    .expect("roster");
    let exe = aterm_agent::harness::codex_alab::lay_package(&root.join("store/keeps-a-row"), true);
    let seat = accounts_pool::Seat {
        config_dir: &cfg,
        state: &state,
        home: &home,
        now: NOW,
    };
    seat.store(codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
            state.active = Some("main".into());
            state.proven = true;
            Ok(Vec::new())
        })
        .expect("seed the store");
    let program = std::env::current_exe().expect("this test's program");
    let image = aterm_agent::harness::native_act::Image::at(&program).expect("its image");
    let seen = aterm_agent::harness::observe::Observation::Unread;
    let source = match source {
        Some(reply) => codex_app_login::IdentifiedUsage {
            identity: codex_renewal::Identity::from_native("source", "user"),
            usage: codex_app_login::usage_from_backend(&reply, NOW).expect("provider reply"),
        },
        None => usage("source", true),
    };
    let target = match target {
        Some(reply) => codex_app_login::IdentifiedUsage {
            identity: codex_renewal::Identity::from_native("target", "user"),
            usage: codex_app_login::usage_from_backend(&reply, NOW).expect("provider reply"),
        },
        None => usage("target", false),
    };
    let reader = Reader(std::collections::BTreeMap::from([
        (main, source),
        (work, target),
    ]));
    let read = codex_rotation::prepare_exec(
        &seat,
        &accounts_pool::Session {
            cli: codex,
            sid: "session",
            supervisor: accounts::Supervisor::Own,
            env: &[],
            seen: &seen,
            sample: None,
            point: codex_rotation::wall_asked(NOW),
            mode: accounts::Mode::Early,
            relaunch: (),
        },
        &exe,
        asking,
        &reader,
        Duration::from_secs(60),
        |row| {
            Some(ExecCommand {
                argv0: "codex".into(),
                image: image.clone(),
                program: program.to_str()?.into(),
                argv: vec!["exec".into()],
                auth_path: row
                    .dir
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| home.join(codex.accounts().default_home))
                    .join("auth.json")
                    .to_str()?
                    .into(),
                cwd: None,
            })
        },
    );
    (dir, read)
}

/// AN EXEC PREPARE AT A SPEND CONTROL ANSWERS `state=selected`: the session's login reads its
/// workspace's spend control reached, with room in both its windows (codex-alab's
/// `usage-spend-control.json` shape), and `work` reads with room, so the read the ask started
/// comes back with a move off `main`, and the next ask is offered it with no read.
#[test]
fn an_exec_prepare_at_a_spend_control_answers_selected() {
    let spend_control = aterm_json::json!({
        "account_id": "source-account",
        "plan_type": "team",
        "rate_limit": {
            "allowed": false,
            "limit_reached": true,
            "primary_window": {"limit_window_seconds": 18000, "reset_after_seconds": 3600,
                "reset_at": POOL_NOW + 3600, "used_percent": 20},
            "secondary_window": {"limit_window_seconds": 604_800, "reset_after_seconds": 86400,
                "reset_at": POOL_NOW + 86_400, "used_percent": 10}
        },
        "spend_control": {
            "individual_limit": {"limit": "25000", "remaining": "0", "remaining_percent": 0,
                "reset_after_seconds": 1_728_000, "reset_at": POOL_NOW + 1_728_000,
                "source": "workspace_spend_controls", "used": "25000", "used_percent": 100},
            "reached": true
        },
        "user_id": "fixture-user"
    });
    let (_pool, read) = prepared(Some(spend_control));
    let proposal = read
        .expect("a spend control is a wall the handoff leaves")
        .expect("a move to work");
    assert_eq!(
        (
            proposal.source.label.as_str(),
            proposal.destination.label.as_str()
        ),
        ("main", "work")
    );
    let now = Instant::now();
    let progress = Mutex::new(progress_after_observation(
        &Progress::Reading(0),
        Ok(Some(proposal.clone())),
        true,
        now,
    ));
    let reply = ask(&progress, &intent(), now, || {
        panic!("a selection in hand reads nothing")
    });
    assert_eq!(reply, offer_of(&proposal));
    assert!(
        reply.starts_with("OK codex-exec-handoff state=selected "),
        "{reply}"
    );
}

/// A `/wham/usage` reply at [`POOL_NOW`] whose codex bucket has room and which lists the limit
/// `codex_other` at `used` percent of its five-hour window, two hours from its reset (refused
/// where spent), or lists no such limit (`None`).
fn reply_listing_codex_other(used: Option<u32>) -> aterm_json::Value {
    let limits: Vec<aterm_json::Value> = used
        .into_iter()
        .map(|used| {
            aterm_json::json!({"limit_name": "codex_other", "metered_feature": "codex_other",
                "rate_limit": {"allowed": used < 100, "limit_reached": used >= 100,
                    "primary_window": {"limit_window_seconds": 18000,
                        "reset_at": POOL_NOW + 7200, "used_percent": used},
                    "secondary_window": {"limit_window_seconds": 604_800,
                        "reset_at": POOL_NOW + 259_200, "used_percent": 40}}})
        })
        .collect();
    aterm_json::json!({
        "plan_type": "pro",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {"limit_window_seconds": 18000, "reset_at": POOL_NOW + 3600,
                "used_percent": 20},
            "secondary_window": {"limit_window_seconds": 604_800, "reset_at": POOL_NOW + 86_400,
                "used_percent": 10}
        },
        "additional_rate_limits": limits
    })
}

/// AN EXEC PREPARE WITH `limit=` JUDGES THAT LIMIT: the line `prepare … limit=codex_other`
/// parses to an intent naming that limit, which the exec handoff asks with. The session's login
/// reads its codex bucket with room and `codex_other` at its five-hour limit (codex-alab's
/// `usage-model-limit.json` shape), so the read comes back with a move off `main` to `work`,
/// which lists `codex_other` with room, and the next ask is offered it; where `work` lists no
/// such limit, or is spent on it too, nothing is selected and the ask is read again shortly. The
/// same reads asked with no limit find no wall to leave. Where the session's own read lists no
/// `codex_other`, the answer is that no handoff is coming because that limit is absent
/// ([`NoHandoff::LimitAbsent`]), which stands as long as no wall to leave does.
#[test]
fn an_exec_prepare_with_a_limit_judges_that_limit() {
    let base = intent();
    let line = format!(
        "prepare thread={} turn={} source_account={} source_user={} limit=codex_other",
        base.thread, base.wall, base.identity.account, base.identity.user
    );
    let Some(codex_renewal::Request::Prepare(limited)) =
        codex_renewal::Request::parse_rotation(&line)
    else {
        panic!("a prepare naming a limit parses: {line}");
    };
    assert_eq!(limited.limit.as_deref(), Some("codex_other"));
    assert_eq!(
        codex_renewal::Intent {
            limit: None,
            ..limited.clone()
        },
        base
    );
    let walled = Some(reply_listing_codex_other(Some(100)));
    let (_pool, read) = prepared_asking(
        walled.clone(),
        Some(reply_listing_codex_other(Some(30))),
        &limited,
    );
    let proposal = read
        .expect("a model's limit is a wall the handoff leaves")
        .expect("a move to work");
    assert_eq!(
        (
            proposal.source.label.as_str(),
            proposal.destination.label.as_str()
        ),
        ("main", "work")
    );
    let now = Instant::now();
    let progress = Mutex::new(progress_after_observation(
        &Progress::Reading(0),
        Ok(Some(proposal.clone())),
        true,
        now,
    ));
    assert_eq!(
        ask(&progress, &limited, now, || {
            panic!("a selection in hand reads nothing")
        }),
        offer_of(&proposal)
    );
    for target in [None, Some(100)] {
        let (_pool, read) = prepared_asking(
            walled.clone(),
            Some(reply_listing_codex_other(target)),
            &limited,
        );
        assert!(matches!(read, Ok(None)), "work at {target:?}: {read:?}");
    }
    let (_pool, read) = prepared_asking(walled, None, &base);
    assert_eq!(read.err(), Some("not-walled"), "asked with no limit");
    let (_pool, read) = prepared_asking(Some(reply_listing_codex_other(None)), None, &limited);
    assert_eq!(read.err(), Some("limit-absent"));
    let decided = Instant::now();
    let mut progress = refused("limit-absent", decided);
    assert!(matches!(
        progress,
        Progress::NotNeeded(NoneComing {
            end: NoHandoff::LimitAbsent,
            ..
        })
    ));
    assert_eq!(
        answer(&mut progress, &limited, decided),
        (NOT_NEEDED.to_owned(), false)
    );
    assert_eq!(
        answer(&mut progress, &limited, decided + NO_WALL_STANDS),
        (WAITING.to_owned(), true),
        "read again once no wall to leave would be"
    );
}

/// The receipt an `arm` makes of `proposal` for [`intent`].
pub(super) fn receipt_of(
    proposal: &codex_rotation::Proposal<ExecCommand>,
) -> codex_renewal::Receipt {
    codex_renewal::Receipt {
        destination: Some(proposal.destination_identity.clone()),
        id: "a".repeat(32),
        intent: intent(),
        sid: "session".into(),
        old_pid: 10,
        old_birth: 1,
        claim_revision: 1,
        human_seq: 0,
        old_auth: proposal.old_auth.clone(),
        new_auth: proposal.new_auth.clone(),
        generation: proposal.plan.selected_generation().unwrap_or_default(),
        row_stamp: codex_renewal::row_stamp(&proposal.destination),
        native_prepare: String::new(),
        expires_ms: u64::MAX,
        phase: codex_renewal::Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    }
}

/// `Instant::now()`, `by` ago.
fn ago(by: Duration) -> Instant {
    Instant::now()
        .checked_sub(by)
        .expect("the host has been up longer than a test's offsets")
}

/// AN ARM TAKES ITS SELECTION BY THE CLOCK IT READS ITSELF: `arm`'s own way to its selection
/// (`arm_selection`) takes one read 30 seconds ago, and gives the operation it makes, as the
/// `Kept` it stores, that read's time, held by no `reserve`'s writer yet; it takes none read
/// `SELECTION_ARMS` and 15 seconds ago or naming another destination. The clock is read inside,
/// and no other code can make a `Kept`, so no caller can take a selection by another time or
/// store another read.
#[test]
fn an_arm_takes_its_selection_by_the_clock_it_reads_itself() {
    let target = target();
    let read = ago(Duration::from_secs(30));
    let young = Mutex::new(selected(read));
    assert!(matches!(
        arm_selection(&young, &target.account, &target.user),
        Some((_, kept)) if kept.armed() == Armed { read, held: false }
    ));
    assert!(arm_selection(&young, &target.account, "someone-else").is_none());
    let old = Mutex::new(selected(ago(SELECTION_ARMS + Duration::from_secs(15))));
    assert!(arm_selection(&old, &target.account, &target.user).is_none());
}

/// Four answers of the selection's writer that say nothing of the selection: the store's writer
/// taken, the sign-in turn, a login file's lock and a word nobody listed.
const SILENT: [Written; 4] = [
    Ok(None),
    Err("signin-busy"),
    Err("auth-busy"),
    Err("a-word-nobody-listed"),
];

/// AN ARM OR A RESERVE WHOSE WRITER SAYS NOTHING OF THE SELECTION IT JUDGED KEEPS IT, THROUGH THE
/// WAY BOTH ASK THE WRITER (`writer_answered`): the writer is asked once, of the step's receipt,
/// with the job's progress unlocked. The selection in hand is the pool's own proposal of a move
/// and the receipt is made of it, so it is the one the writer judged. Where the writer answers
/// any of `SILENT`, an `arm` is denied and a `reserve` of a selection read 30 seconds ago waits;
/// either way the next `arm` takes the same selection and the next `prepare` is offered it with
/// no read. A `reserve` of one read `SELECTION_OFFERED` and 15 seconds ago is denied, and keeps
/// it too. CONTROL: a refutation of that receipt drops the same selection.
#[test]
fn a_step_whose_writer_says_nothing_keeps_the_selection() {
    let intent = intent();
    let (_pool, proposal) = moving();
    let target = proposal.destination_identity.clone();
    let receipt = receipt_of(&proposal);
    for outcome in SILENT {
        let read = ago(Duration::from_secs(30));
        let stale = ago(SELECTION_OFFERED + Duration::from_secs(15));
        for (step, reply) in [
            (Step::Arm, DENIED.to_owned()),
            (reserve(read), waiting()),
            (reserve(stale), DENIED.to_owned()),
        ] {
            let progress = Mutex::new(Progress::Selected(Box::new(proposal.clone()), read));
            let asked = std::cell::Cell::new(0_u32);
            let judge = |judged: &codex_renewal::Receipt| {
                asked.set(asked.get() + 1);
                assert!(std::ptr::eq(judged, &receipt), "the step's receipt");
                assert!(progress.try_lock().is_ok(), "the progress is unlocked");
                outcome
            };
            assert_eq!(
                writer_answered(&progress, &intent, step, &receipt, judge),
                Some(reply),
                "{step:?} {outcome:?}"
            );
            assert_eq!(asked.get(), 1, "{step:?} {outcome:?}");
            assert!(
                arm_selection(&progress, &target.account, &target.user).is_some(),
                "{step:?} {outcome:?}: the next arm takes it"
            );
            assert_eq!(
                ask(&progress, &intent, Instant::now(), || {
                    panic!("a kept selection reads nothing")
                }),
                offer_of(&proposal),
                "{step:?} {outcome:?}"
            );
            let refuted = |_: &codex_renewal::Receipt| -> Written { Err("selection-changed") };
            assert_eq!(
                writer_answered(&progress, &intent, step, &receipt, refuted),
                Some(DENIED.to_owned()),
                "{step:?} {outcome:?}"
            );
            assert!(
                arm_selection(&progress, &target.account, &target.user).is_none(),
                "{step:?} {outcome:?}: CONTROL, the selection kept is the one the writer judged"
            );
        }
    }
}

/// A REFUSED RESERVE ANSWERS BY WHAT ITS OPERATION KEPT OF ITS SELECTION: `Kept::reserve_refused`,
/// the way `serve` answers a `reserve` whose pool commit was refused, of the `Kept` that
/// `arm_selection` made for the operation's `arm`, judges the writer's word by the read that `arm`
/// took and by whether a writer of its `reserve`s has held the selection, not by the clock at the
/// `reserve`. Of a selection read 30 seconds ago a writer that says nothing makes the `reserve`
/// wait; of one read 55 seconds ago that no writer held it is denied. A writer that holds that old
/// selection makes the `reserve` wait and is noted, and from then on a writer that says nothing (a
/// lock another writer holds a moment) makes it wait too, at the same age. Through all of it the
/// selection the writer judged is kept, and the next `arm` takes it; a refutation then denies the
/// `reserve` and drops it.
#[test]
fn a_refused_reserve_answers_by_what_its_operation_kept_of_its_selection() {
    let intent = intent();
    let (_pool, proposal) = moving();
    let target = proposal.destination_identity.clone();
    let receipt = receipt_of(&proposal);
    let operation = &receipt;
    let says = move |outcome: Written| {
        move |judged: &codex_renewal::Receipt| {
            assert!(std::ptr::eq(judged, operation), "the operation's receipt");
            outcome
        }
    };
    let arm = |progress: &Mutex<Progress>| {
        arm_selection(progress, &target.account, &target.user)
            .expect("the arm takes the selection")
            .1
    };
    let young = ago(Duration::from_secs(30));
    let progress = Mutex::new(Progress::Selected(Box::new(proposal.clone()), young));
    for outcome in SILENT {
        let mut kept = arm(&progress);
        assert_eq!(
            kept.reserve_refused(&progress, &intent, DEADLINE_US, &receipt, says(outcome)),
            waiting(),
            "{outcome:?} of a young selection"
        );
        assert!(!kept.armed().held, "{outcome:?} holds nothing");
    }

    let old = ago(SELECTION_OFFERED + Duration::from_secs(5));
    let progress = Mutex::new(Progress::Selected(Box::new(proposal.clone()), old));
    let mut kept = arm(&progress);
    let refused = |kept: &mut Kept, outcome: Written| {
        kept.reserve_refused(&progress, &intent, DEADLINE_US, &receipt, says(outcome))
    };
    for outcome in SILENT {
        assert_eq!(
            refused(&mut kept, outcome),
            DENIED,
            "{outcome:?} of an old selection no writer held"
        );
        assert!(!kept.armed().held, "{outcome:?} holds nothing");
    }
    assert_eq!(refused(&mut kept, Ok(Some(()))), waiting(), "held");
    assert_eq!(
        kept.armed(),
        Armed {
            read: old,
            held: true
        },
        "a writer that holds it is noted"
    );
    for outcome in SILENT {
        assert_eq!(
            refused(&mut kept, outcome),
            waiting(),
            "{outcome:?} of an old selection a writer held"
        );
        assert!(kept.armed().held, "{outcome:?} unnotes nothing");
    }
    assert!(
        arm_selection(&progress, &target.account, &target.user).is_some(),
        "the selection the writer judged is kept"
    );
    assert_eq!(refused(&mut kept, Err("selection-changed")), DENIED);
    assert!(
        arm_selection(&progress, &target.account, &target.user).is_none(),
        "a refutation drops it"
    );
}

/// A REFUSAL THAT REFUTES THE SELECTION DROPS ONLY THE ONE THE WRITER JUDGED: through the way
/// `arm` and `reserve` ask the writer, a refutation of the receipt made of the selection in hand
/// (the pool's own proposal of a move) denies the step and drops that selection, so no `arm`
/// takes it and the next ask reads. The same refutation of a receipt for another wall, or of a
/// selection the receipt was not made of, denies the step and leaves the selection in hand.
#[test]
fn a_refutation_drops_only_the_selection_it_judged() {
    let intent = intent();
    let (_pool, proposal) = moving();
    let destination = proposal.destination_identity.clone();
    let receipt = receipt_of(&proposal);
    let mut elsewhere = receipt.clone();
    elsewhere.intent.wall = "00000000-0000-0000-0000-000000000003".into();
    let refuted = |_: &codex_renewal::Receipt| -> Written { Err("selection-changed") };
    let read = ago(Duration::from_secs(1));
    for step in [Step::Arm, reserve(read)] {
        let progress = Mutex::new(Progress::Selected(Box::new(proposal.clone()), read));
        assert_eq!(
            writer_answered(&progress, &intent, step, &elsewhere, refuted),
            Some(DENIED.to_owned()),
            "{step:?}"
        );
        assert!(
            arm_selection(&progress, &destination.account, &destination.user).is_some(),
            "{step:?}: a refutation of another wall's receipt drops nothing"
        );
        assert_eq!(
            writer_answered(&progress, &intent, step, &receipt, refuted),
            Some(DENIED.to_owned()),
            "{step:?}"
        );
        assert!(
            arm_selection(&progress, &destination.account, &destination.user).is_none(),
            "{step:?}: no arm takes a refuted selection"
        );
        assert_eq!(
            ask(&progress, &intent, Instant::now(), || {}),
            WAITING,
            "{step:?}"
        );
        assert!(
            matches!(*progress.lock().unwrap(), Progress::Reading(_)),
            "{step:?}: the next ask reads"
        );

        let another = Mutex::new(selected(read));
        assert_eq!(
            writer_answered(&another, &intent, step, &receipt, refuted),
            Some(DENIED.to_owned()),
            "{step:?}"
        );
        assert!(
            matches!(&*another.lock().unwrap(), Progress::Selected(_, at) if *at == read),
            "{step:?}: a selection the receipt was not made of stands"
        );
    }
}

/// A SELECTION IS OFFERED FOR FIFTY SECONDS AND ARMED FOR SIXTY: `prepare` offers a selection
/// until its read is 50 seconds old, then lets it lapse, and the ask after that reads the logins
/// again; `arm` takes one until it is 60 seconds old, so a session offered one has ten seconds
/// to arm with it. An `arm` that names another destination than the selection's takes nothing.
#[test]
fn a_selection_is_offered_for_fifty_seconds_and_armed_for_sixty() {
    assert_eq!(SELECTION_OFFERED, Duration::from_secs(50));
    assert_eq!(SELECTION_ARMS, Duration::from_secs(60));
    let read = Instant::now();
    let intent = intent();
    let target = target();
    let mut progress = selected(read);
    let inside = read + SELECTION_OFFERED - Duration::from_millis(1);
    assert_eq!(answer(&mut progress, &intent, inside), (offer(), false));
    let lapsed = read + SELECTION_OFFERED;
    assert_eq!(
        answer(&mut progress, &intent, lapsed),
        (WAITING.to_owned(), false)
    );
    assert!(
        matches!(progress, Progress::Retry(at) if at == lapsed),
        "{progress:?}"
    );
    assert_eq!(
        answer(&mut progress, &intent, lapsed),
        (WAITING.to_owned(), true),
        "the ask after the lapse reads"
    );

    let progress = selected(read);
    let arms = |account: &str, user: &str, age: Duration| {
        selection_to_arm(&progress, account, user, read + age).is_some()
    };
    assert!(arms(
        &target.account,
        &target.user,
        SELECTION_ARMS - Duration::from_millis(1)
    ));
    assert!(!arms(&target.account, &target.user, SELECTION_ARMS));
    let source = intent.identity;
    assert!(!arms(&source.account, &target.user, Duration::ZERO));
    assert!(!arms(&target.account, &source.user, Duration::ZERO));
}

/// The kernel's birth of the live process `pid`, which its tab keys it by.
pub(super) fn born(pid: u32) -> u64 {
    aterm_uds::process::read_birth(i32::try_from(pid).expect("a pid"))
        .expect("a live process's birth")
        .0
}

/// A process with the pid `pid`, born at `birth`, running this test's program.
pub(super) fn process(pid: u32, birth: u64) -> Process {
    Process {
        pid,
        birth,
        parent: 1,
        group: pid,
        session: pid,
        image: Image::at(&std::env::current_exe().expect("this test's program"))
            .expect("its image"),
    }
}

/// The exec source a capture of `process` as a `codex exec` run gives, with `home` as its
/// `HOME` and working directory.
pub(super) fn source(process: Process, home: &std::path::Path) -> ExecSource {
    let exec_path = String::from_utf8(process.image.path.clone()).expect("a UTF-8 program");
    ExecSource::observed(
        process,
        atpkg::caller_shell::ProcArgs {
            exec_path,
            argv: vec!["codex".into(), "exec".into(), "-".into()],
            env: vec![format!("HOME={}", home.display())],
        },
        home.into(),
    )
    .expect("an exec source")
}

/// What a `prepare` of `source`'s process at `wall` asks while that process holds its tab's
/// claim, as `serve` makes it.
pub(super) fn ask_of(source: ExecSource, wall: &str) -> Preparation {
    let mut intent = intent();
    intent.wall = wall.into();
    Preparation {
        proof: crate::self_claim::Proven {
            pid: source.process.pid,
            start: "fixture".into(),
        },
        source,
        intent,
        revision: 1,
        human_seq: 0,
        mover: "session".into(),
        progress: Mutex::new(Progress::Retry(Instant::now())),
    }
}

/// A child of this test that sleeps until the value drops, when it is killed and reaped.
pub(super) struct Sleeper(aterm_uds::spawnfd::Child);

impl Sleeper {
    pub(super) fn start() -> Self {
        Self(
            aterm_uds::spawnfd::command("/bin/sleep")
                .arg("600")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("a sleeping child"),
        )
    }

    /// The child as its tab keys it, alive.
    pub(super) fn process(&self) -> Process {
        process(self.0.id(), born(self.0.id()))
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A wall other than [`intent`]'s.
const LATER_WALL: &str = "00000000-0000-0000-0000-000000000003";

/// TWO PROCESSES' PREPARATIONS IN ONE TAB DO NOT EVICT EACH OTHER: a `prepare` of a second
/// process keeps its job beside the first's; each job is the one its own process's read still
/// finds its process's (`holds`), so neither read is thrown away, the one its process's `arm`
/// takes its selection from (`job`, by the arming process's key), and the one a later ask of the
/// same process answers from. A later process at one of their pids (another birth) finds none.
#[test]
fn two_processes_preparations_in_one_tab_do_not_evict_each_other() {
    let home = aterm_tempfile::tempdir().expect("a home");
    let other = Sleeper::start();
    let me = std::process::id();
    let wall = intent().wall;
    let mut jobs = Table::default();
    let first = jobs.job_for(ask_of(source(process(me, born(me)), home.path()), &wall));
    let second = jobs.job_for(ask_of(source(other.process(), home.path()), &wall));
    assert!(
        !Arc::ptr_eq(&first, &second),
        "each process has its own job"
    );
    for job in [&first, &second] {
        let key = ProcessKey::of(&job.source.process);
        assert!(
            jobs.holds(job),
            "{:?}: its read is still its job's",
            job.source
        );
        assert!(
            jobs.job(key).is_some_and(|armed| Arc::ptr_eq(armed, job)),
            "{:?}: its arm takes its own job",
            job.source
        );
        let again = jobs.job_for(ask_of(job.source.clone(), &wall));
        assert!(
            Arc::ptr_eq(&again, job),
            "{:?}: a later ask of its process answers from it",
            job.source
        );
    }
    assert!(
        jobs.job(ProcessKey::of(&process(me, born(me) + 1)))
            .is_none(),
        "a later process at this pid finds no job"
    );
    assert_eq!(jobs.processes().len(), 2);
}

/// A NEW INTENT OF ONE PROCESS REPLACES ONLY ITS OWN JOB: a `prepare` of a process at another
/// wall makes that process a new job, in its own place, so the read its earlier job started no
/// longer finds itself its process's; the other process's job, and its read, stand as they were.
#[test]
fn a_new_intent_of_one_process_replaces_only_its_own_job() {
    let home = aterm_tempfile::tempdir().expect("a home");
    let other = Sleeper::start();
    let me = std::process::id();
    let wall = intent().wall;
    let mut jobs = Table::default();
    let mine = source(process(me, born(me)), home.path());
    let earlier = jobs.job_for(ask_of(mine.clone(), &wall));
    let theirs = jobs.job_for(ask_of(source(other.process(), home.path()), &wall));
    let later = jobs.job_for(ask_of(mine, LATER_WALL));
    assert!(!Arc::ptr_eq(&later, &earlier), "a new intent is a new job");
    assert_eq!(later.intent.wall, LATER_WALL);
    assert!(jobs.holds(&later));
    assert!(
        !jobs.holds(&earlier),
        "the replaced job's read selects nothing"
    );
    assert!(jobs.holds(&theirs), "the other process's job stands");
    assert_eq!(jobs.processes().len(), 2);
}

/// A GONE PROCESS'S PREPARATION IS FORGOTTEN: of a tab's preparations, `forget_gone` drops the
/// job of a process that has exited and been reaped, and the job keyed by this test's pid at a
/// birth that is not its own (that pid's next process), and keeps the job of this live process.
#[test]
fn a_gone_process_preparation_is_forgotten() {
    let home = aterm_tempfile::tempdir().expect("a home");
    let ended = Sleeper::start().process();
    let me = std::process::id();
    let wall = intent().wall;
    let native = Mutex::new(Operations::default());
    let live = {
        let jobs = &mut native.lock().unwrap().exec;
        let live = jobs.job_for(ask_of(source(process(me, born(me)), home.path()), &wall));
        jobs.job_for(ask_of(source(ended, home.path()), &wall));
        jobs.job_for(ask_of(
            source(process(me, born(me) + 1), home.path()),
            &wall,
        ));
        assert_eq!(jobs.processes().len(), 3);
        live
    };
    forget_gone(&native);
    let table = native.lock().unwrap();
    assert_eq!(
        table.exec.processes(),
        BTreeSet::from([ProcessKey::of(&live.source.process)]),
        "only the live process's job is kept"
    );
    assert!(table.exec.holds(&live));
}

/// A COMPLETION A LOCK HELD UP WAITS, ONE THE RECORDS REFUSE IS DENIED: what `codex-exec-handoff
/// complete` answers once its gates passed (`operation::completion_reply`), for each answer of
/// the completion's writer. A recorded proof is `state=completed`. A lock another writer held
/// (`Unrecorded::Busy`: its `accounts.lock`, the sign-in turn, or a login's auth lock a token
/// refresh holds a moment) is `state=waiting`, which the exec successor asks again, never
/// `denied`, which would end its report and leave the walled sessions behind its canary to the
/// hold's lapse. Each refusal of the records is `denied`.
#[test]
fn a_completion_a_lock_held_up_waits_and_one_the_records_refuse_is_denied() {
    use codex_renewal::Unrecorded;
    assert_eq!(
        super::operation::completion_reply(Ok(())),
        "OK codex-exec-handoff state=completed\n"
    );
    assert_eq!(
        super::operation::completion_reply(Err(Unrecorded::Busy)),
        WAITING
    );
    for why in [
        "not-consumed",
        "selection-changed",
        "target-unavailable",
        "source-held",
        "identity-mismatch",
        "policy-changed",
    ] {
        assert_eq!(
            super::operation::completion_reply(Err(Unrecorded::Refused(why))),
            DENIED,
            "{why}"
        );
    }
}
