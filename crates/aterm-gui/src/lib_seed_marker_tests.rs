// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Wake, parse_seed_line};

/// The STREAMING contract: `net-starting:` must raise its event from a single
/// line, because that is the whole point — it arrives before the multi-GB
/// download, not bundled with the output after it. A quiet line raises nothing.
#[test]
fn a_streamed_start_marker_raises_the_started_wake() {
    let started = parse_seed_line(
        "atpkg: net-starting: installing 2 program(s) over the network: claude, codex \
         (~410 MB to download)",
    );
    match started {
        Some(Wake::PkgSeedStarted { detail, first_run }) => {
            assert!(detail.starts_with("installing 2 program(s)"), "{detail}");
            assert!(
                !first_run,
                "a line alone is routine: only the lane that spawned the pass tags it"
            );
            assert!(
                !detail.contains("net-starting"),
                "the marker prefix is stripped, not echoed: {detail}"
            );
        }
        other => panic!("expected PkgSeedStarted, got {other:?}"),
    }
    // The completion marker still resolves line-at-a-time…
    match parse_seed_line("atpkg: net-installed: claude, codex") {
        Some(Wake::PkgSeed { installed }) => assert_eq!(installed, ["claude", "codex"]),
        other => panic!("expected PkgSeed, got {other:?}"),
    }
    // …and ordinary chatter raises nothing at all.
    assert!(parse_seed_line("atpkg: the store already holds the ALab toolset").is_none());
    assert!(
        parse_seed_line("atpkg: net-starting: ").is_none(),
        "empty tail"
    );
}

/// The informational markers resolve line by line with or without the `atpkg: ` prefix,
/// and an empty body raises nothing. A coherence group's abort is a terminal Warn row.
#[test]
fn the_abort_marker_parses_and_classifies() {
    let aborted = parse_seed_line(
        "atpkg: group-aborted: rustc update aborted at trust during stage: staging failed",
    )
    .expect("parsed");
    assert!(
        matches!(&aborted, Wake::PkgGroupAborted { detail } if detail.starts_with("rustc update aborted")),
        "{aborted:?}"
    );
    assert!(
        crate::seed_event_is_terminal(&aborted),
        "an abort answers the announcement"
    );
    assert_eq!(atpkg::cli::GROUP_ABORTED_MARKER, "group-aborted: ");
}

#[test]
fn the_r6_markers_raise_their_wakes_with_or_without_the_atpkg_prefix() {
    let managed = "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)";
    match parse_seed_line(&format!("atpkg: managed-current: {managed}")) {
        Some(Wake::PkgManagedCurrent(body)) => assert_eq!(body, managed),
        other => panic!("expected PkgManagedCurrent, got {other:?}"),
    }
    match parse_seed_line(&format!("managed-current: {managed}")) {
        Some(Wake::PkgManagedCurrent(body)) => assert_eq!(body, managed),
        other => panic!("expected PkgManagedCurrent (bare), got {other:?}"),
    }
    let machine = "universal-control disabled";
    match parse_seed_line(&format!("atpkg: machine-settings: {machine}")) {
        Some(Wake::PkgMachineSettings(body)) => assert_eq!(body, machine),
        other => panic!("expected PkgMachineSettings, got {other:?}"),
    }
    // A refusal, or a write that did not land (exit 0, said on stdout), is an answer.
    match parse_seed_line(&format!(
        "{}HOME is /tmp/x but this account's home is /Users//y",
        atpkg::cli::MACHINE_NOT_APPLIED_PREFIX
    )) {
        Some(Wake::PkgMachineRefused(body)) => {
            assert!(body.starts_with("not applied — "), "{body}");
            assert!(body.contains("/tmp/x"), "{body}");
        }
        other => panic!("expected PkgMachineRefused, got {other:?}"),
    }
    match parse_seed_line(&format!(
        "{}Universal Control could not be disabled for this host",
        atpkg::cli::MACHINE_APPLY_FAILED_PREFIX
    )) {
        Some(Wake::PkgMachineRefused(body)) => {
            assert!(body.starts_with("failed — "), "{body}");
            assert!(body.contains("Universal Control"), "{body}");
        }
        other => panic!("expected PkgMachineRefused, got {other:?}"),
    }
    assert!(
        parse_seed_line("atpkg: managed-current: ").is_none(),
        "empty tail"
    );
    assert!(parse_seed_line("atpkg: machine-settings:").is_none());
    assert!(
        parse_seed_line("atpkg: claude: managed 2.1.280 — Anthropic latest").is_none(),
        "a plain state line is not a marker"
    );
    // `machine-state:` is an event (`PkgMachineState`, the card's newest state), never a
    // row, and never mistaken for the change marker sharing its prefix.
    match parse_seed_line(&format!(
        "atpkg: {}universal-control=disabled; policy=off; home=account",
        atpkg::cli::MACHINE_STATE_MARKER
    )) {
        Some(Wake::PkgMachineState(body)) => {
            let state = atpkg::machine::parse_machine_state(&body).expect("the record parses");
            assert_eq!(state.universal_control, atpkg::machine::UcPosture::Disabled);
        }
        other => panic!("expected PkgMachineState, got {other:?}"),
    }
}

/// `appnotice` text is recorded: marker-shaped toolchain text as that marker, other
/// text on the named lane; no row, reply `recorded`; a bad lane is refused.
#[test]
fn appnotice_text_is_recorded() {
    let mut app = super::App::headless_for_test();
    let live = |app: &super::App| app.messages.live_rows().count();
    for text in [
        "managed-current: claude 2.1.280 (Anthropic latest)",
        "atpkg: machine-settings: universal-control disabled",
    ] {
        assert_eq!(
            app.take_app_notice("toolchain", text),
            Ok(super::AppNoticeTaken::Recorded),
            "{text}"
        );
        assert_eq!(live(&app), 0, "{text}: no row");
    }
    assert_eq!(
        app.messages
            .log()
            .records()
            .filter(|r| r.tag.as_str() == "toolchain")
            .count(),
        2,
        "both are on record"
    );
    assert_eq!(
        app.take_app_notice("toolchain", "claude installed from a terminal"),
        Ok(super::AppNoticeTaken::Recorded)
    );
    assert_eq!(live(&app), 0, "a plain text is a record, not a row");
    assert_eq!(
        app.take_app_notice("update", "an operator's note"),
        Ok(super::AppNoticeTaken::Recorded)
    );
    assert_eq!(live(&app), 0);
    assert_eq!(app.message_band_rows, 0, "nothing reached the glass");
    let texts: Vec<&str> = app
        .messages
        .log()
        .records()
        .map(|r| r.title.as_str())
        .collect();
    assert!(
        texts.contains(&"claude installed from a terminal")
            && texts.contains(&"an operator's note"),
        "{texts:?}"
    );
    // Harness notes are records, never rows.
    let before = app.messages.log().len();
    assert_eq!(
        app.take_app_notice("harness", "aterm harness acting \u{00b7} approving rm"),
        Ok(super::AppNoticeTaken::Recorded)
    );
    assert_eq!(live(&app), 0, "no row for a harness note");
    let last = app.messages.log().records().last().expect("on record");
    assert_eq!(
        (last.tag.as_str(), last.title.as_str()),
        ("harness", "aterm harness acting \u{00b7} approving rm")
    );
    assert_eq!(app.messages.log().len(), before + 1);
    assert!(app.take_app_notice("elsewhere", "x").is_err());
}

/// The marker prefixes are atpkg's constants; this exercises the wiring, network lane
/// included.
#[test]
fn every_atpkg_marker_constant_has_a_wake_arm() {
    use super::parse_seed_line;
    let cases: [(&str, &str); 7] = [
        (
            atpkg::cli::LOCK_WAITING_MARKER,
            "another atpkg process holds the store lock at /x/store.lock \u{2014} \
             waiting up to 1800 s for it to finish",
        ),
        (
            atpkg::cli::LOCK_ACQUIRED_MARKER,
            "the other process finished \u{2014} running now",
        ),
        (atpkg::cli::SEED_FAILED_MARKER, "nothing could be installed"),
        (
            atpkg::cli::SEED_UNUSABLE_MARKER,
            "no build for this architecture",
        ),
        (atpkg::cli::NET_INSTALLED_MARKER, "ay, clean"),
        (
            atpkg::cli::NET_STARTING_MARKER,
            "installing 2 program(s) over the network: ay, clean",
        ),
        (
            atpkg::cli::NET_FAILED_MARKER,
            "network provisioning installed nothing",
        ),
    ];
    for (marker, tail) in cases {
        let line = format!("atpkg: {marker}{tail}");
        assert!(
            parse_seed_line(&line).is_some(),
            "marker {marker:?} reaches the user through no Wake — an announcement \
             with no answer leaves its notice on screen forever"
        );
    }
    // The network lane's arrival renders as an install, because to the user that
    // is exactly what it is.
    match parse_seed_line(&format!(
        "atpkg: {}ay, clean",
        atpkg::cli::NET_INSTALLED_MARKER
    )) {
        Some(Wake::PkgSeed { installed, .. }) => assert_eq!(installed, ["ay", "clean"]),
        other => panic!("expected PkgSeed for the network lane, got {other:?}"),
    }
    // An empty tail is not an event.
    assert!(parse_seed_line(&format!("atpkg: {}", atpkg::cli::SEED_FAILED_MARKER)).is_none());
}

/// A `machine-settings:` line followed by its `machine-state:` record posts both, in
/// order; a change with no record by EOF also posts `PkgMachineRecordMissing`; a
/// record alone posts no fallback.
#[test]
fn a_machine_change_is_answered_by_the_records_behind_it_or_by_a_fallback() {
    let change = format!(
        "atpkg: {}universal-control disabled",
        atpkg::cli::MACHINE_SETTINGS_MARKER
    );
    let record = format!(
        "atpkg: {}universal-control=disabled; policy=off; home=account",
        atpkg::cli::MACHINE_STATE_MARKER
    );
    let collect = |stdout: String| {
        let mut events = Vec::new();
        let seen = super::read_seed_markers(stdout.as_bytes(), |e| events.push(e));
        (seen, events)
    };
    let (seen, events) = collect(format!("{change}\n{record}\n"));
    assert!(seen.saw_machine_change && seen.saw_machine_state);
    assert!(
        matches!(events.as_slice(), [Wake::PkgMachineSettings(_), Wake::PkgMachineState(body)]
            if atpkg::machine::parse_machine_state(body.as_str()).is_some()),
        "{events:?}"
    );
    let (seen, events) = collect(format!("{change}\n"));
    assert!(seen.saw_machine_change && !seen.saw_machine_state);
    assert!(
        matches!(
            events.as_slice(),
            [Wake::PkgMachineSettings(_), Wake::PkgMachineRecordMissing]
        ),
        "a change with no record behind it hands the read back: {events:?}"
    );
    let (seen, events) = collect(format!("{record}\n"));
    assert!(!seen.saw_machine_change && seen.saw_machine_state);
    assert!(
        matches!(events.as_slice(), [Wake::PkgMachineState(_)]),
        "{events:?}"
    );
    let (_, events) = collect("atpkg: shadowed: claude\n".to_string());
    assert!(
        events.is_empty(),
        "no change, no record, no fallback: {events:?}"
    );
}

/// The store-lock wait line raises its own wake (prefix stripped, empty tail nothing)
/// and is neither a start nor an answer.
#[test]
fn a_lock_wait_marker_is_neither_a_start_nor_an_answer() {
    use super::{PassVerdict, classify_pass_exit};
    let line = format!(
        "atpkg: {}another atpkg process holds the store lock at /x/store.lock \u{2014} \
         waiting up to 1800 s for it to finish",
        atpkg::cli::LOCK_WAITING_MARKER
    );
    match parse_seed_line(&line) {
        Some(Wake::PkgLockWaiting { detail }) => {
            assert!(
                detail.starts_with("another atpkg process holds"),
                "{detail}"
            );
            assert!(
                !detail.contains("lock-waiting"),
                "prefix stripped: {detail}"
            );
        }
        other => panic!("expected PkgLockWaiting, got {other:?}"),
    }
    assert!(parse_seed_line(&format!("atpkg: {}", atpkg::cli::LOCK_WAITING_MARKER)).is_none());
    assert_eq!(atpkg::cli::LOCK_WAITING_MARKER, "lock-waiting: ");
    // Through `read_seed_markers`: the wait sets neither `saw_start` nor `saw_terminal`,
    // so "waited, then ran quietly" is the healthy verdict and "waited, timed out" is
    // deferred.
    use super::read_seed_markers;
    let read = |stdout: &str| read_seed_markers(stdout.as_bytes(), |_| {});
    let seen = read(&format!("{line}\n"));
    assert!(seen.saw_lock_wait, "the wait opens the lane's row");
    assert!(
        !seen.saw_start && !seen.saw_terminal,
        "…and is neither a start nor an answer"
    );
    assert_eq!(
        classify_pass_exit(Some(0), seen.saw_start, seen.saw_terminal),
        PassVerdict::Quiet,
        "waited, then ran quietly: nothing raised"
    );
    assert_eq!(
        classify_pass_exit(Some(75), seen.saw_start, seen.saw_terminal),
        PassVerdict::Busy,
        "waited, then timed out: deferred, never failed"
    );
    // …and a timed-out wait that says so (0.82.0's `seed-busy:` terminal rides
    // the same exit 75) is still deferred, never a pass that ran.
    let busy = read(&format!(
        "{line}\natpkg: {}another atpkg process holds the store lock\n",
        atpkg::cli::SEED_BUSY_MARKER
    ));
    assert!(busy.saw_lock_wait && busy.saw_terminal && busy.saw_busy && !busy.saw_start);
    assert_eq!(
        classify_pass_exit(Some(75), busy.saw_start, busy.saw_terminal),
        PassVerdict::Busy,
        "waited, timed out and said so: deferred"
    );
    let started = read(&format!(
        "{line}\natpkg: {}installing 8 program(s)\n",
        atpkg::cli::NET_STARTING_MARKER
    ));
    assert!(
        started.saw_start && !started.saw_terminal,
        "a start opens the card, answers nothing"
    );
    assert_eq!(
        classify_pass_exit(Some(1), started.saw_start, started.saw_terminal),
        PassVerdict::AnnouncedThenDied
    );
    let answered = read(&format!(
        "{line}\natpkg: {}installing 8 program(s)\natpkg: {}ay, trust\n",
        atpkg::cli::NET_STARTING_MARKER,
        atpkg::cli::NET_INSTALLED_MARKER
    ));
    assert!(answered.saw_terminal, "a terminal marker is the answer");
    assert_eq!(
        classify_pass_exit(Some(1), answered.saw_start, answered.saw_terminal),
        PassVerdict::Answered
    );
    // `lock-acquired:` likewise: its own wake, neither start nor answer, and the wait
    // stays seen for the lane's exit bookkeeping.
    let acquired_line = format!(
        "atpkg: {}the other process finished \u{2014} running now",
        atpkg::cli::LOCK_ACQUIRED_MARKER
    );
    match parse_seed_line(&acquired_line) {
        Some(Wake::PkgLockAcquired { detail }) => {
            assert!(detail.starts_with("the other process finished"), "{detail}");
            assert!(
                !detail.contains("lock-acquired"),
                "prefix stripped: {detail}"
            );
        }
        other => panic!("expected PkgLockAcquired, got {other:?}"),
    }
    assert!(parse_seed_line(&format!("atpkg: {}", atpkg::cli::LOCK_ACQUIRED_MARKER)).is_none());
    assert_eq!(atpkg::cli::LOCK_ACQUIRED_MARKER, "lock-acquired: ");
    let mut posted = Vec::new();
    let acquired = read_seed_markers(format!("{line}\n{acquired_line}\n").as_bytes(), |event| {
        posted.push(event)
    });
    assert!(acquired.saw_lock_wait, "the wait stays seen");
    assert!(
        !acquired.saw_start && !acquired.saw_terminal,
        "the acquisition is neither a start nor an answer"
    );
    assert_eq!(
        classify_pass_exit(Some(0), acquired.saw_start, acquired.saw_terminal),
        PassVerdict::Quiet,
        "waited, acquired, then ran quietly: nothing raised"
    );
    assert!(
        matches!(
            posted.as_slice(),
            [Wake::PkgLockWaiting { .. }, Wake::PkgLockAcquired { .. }]
        ),
        "both lines are posted, in order: {posted:?}"
    );
}
