// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// Only the app-data item reads `ENOENT` as permission; `EPERM` and `EACCES` fold the same
/// for every item.
#[test]
fn a_missing_path_is_permission_for_the_app_data_item_alone() {
    const EACCES: i32 = 13;
    const ENOENT: i32 = 2;
    assert_eq!(
        fold_read_dir(Folder::AppData, ReadDirOutcome::Failed(ENOENT)),
        WarmupRow::Allowed,
        "being told another app's container is absent means the look was allowed"
    );
    for folder in [Folder::Documents, Folder::Desktop, Folder::Downloads] {
        assert_eq!(
            fold_read_dir(folder, ReadDirOutcome::Failed(ENOENT)),
            WarmupRow::Error,
            "{folder:?}: a missing home folder is not a consent verdict"
        );
    }
    for folder in Folder::ALL {
        assert_eq!(
            fold_read_dir(*folder, ReadDirOutcome::Failed(consent::ERRNO_EPERM)),
            WarmupRow::Denied,
            "{folder:?}"
        );
        assert_eq!(
            fold_read_dir(*folder, ReadDirOutcome::Failed(EACCES)),
            WarmupRow::Error,
            "{folder:?}"
        );
        assert_eq!(
            fold_read_dir(*folder, ReadDirOutcome::Listed),
            WarmupRow::Allowed,
            "{folder:?}"
        );
        assert_eq!(
            fold_read_dir(*folder, ReadDirOutcome::Failed(0)),
            WarmupRow::Error,
            "{folder:?}"
        );
        assert_eq!(
            fold_read_dir(*folder, ReadDirOutcome::Refused),
            WarmupRow::Unknown,
            "{folder:?}: a probe that did not look claims nothing"
        );
    }
}

#[test]
fn every_row_state_has_its_own_word() {
    let words: Vec<&str> = [
        WarmupRow::Unknown,
        WarmupRow::Asking,
        WarmupRow::Allowed,
        WarmupRow::Denied,
        WarmupRow::Error,
    ]
    .into_iter()
    .map(WarmupRow::as_str)
    .collect();
    assert_eq!(
        words,
        vec!["unknown", "asking", "allowed", "denied", "error"]
    );
    assert_eq!(WarmupRow::default(), WarmupRow::Unknown);
}

#[test]
fn a_headless_instance_gets_the_arm_that_cannot_reach_the_filesystem() {
    assert!(!WarmupState::new(true).probe_is_live());
    assert!(WarmupState::new(false).probe_is_live());
    assert!(!WarmupState::default().probe_is_live());
    assert!(!WarmupState::inert().probe_is_live());
    assert_eq!(inert_read_dir(Path::new("/")), ReadDirOutcome::Refused);
}

#[test]
fn the_live_arm_refuses_from_a_binary_outside_a_bundle() {
    // A test binary is not inside a `.app`, so the guard answers before any listing.
    assert_eq!(live_read_dir(Path::new("/")), ReadDirOutcome::Refused);
}

#[test]
fn the_walk_list_drops_repeats_and_keeps_configuration_order() {
    let configured = vec![
        (Folder::Downloads, PathBuf::from("/a")),
        (Folder::Documents, PathBuf::from("/b")),
        (Folder::Downloads, PathBuf::from("/c")),
    ];
    let walk = dedup_folders(&configured);
    assert_eq!(
        walk.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
        vec![Folder::Downloads, Folder::Documents],
        "a folder named twice raises the same dialog twice for no information"
    );
    assert_eq!(walk[0].1, PathBuf::from("/a"));
    assert_eq!(
        folders_to_unknown_rows(&configured),
        vec![
            (Folder::Downloads, WarmupRow::Unknown),
            (Folder::Documents, WarmupRow::Unknown)
        ]
    );
    assert!(2 * Folder::ALL.len() < RESULT_QUEUE_CAP);
}

fn live_state(generation: u64, hold_cap: Duration) -> WarmupState {
    let mut state = WarmupState::inert();
    state.rows = vec![
        (Folder::Documents, WarmupRow::Unknown),
        (Folder::Desktop, WarmupRow::Unknown),
    ];
    state.generation = generation;
    state.live = Some(LivePass {
        generation,
        started_at: Instant::now(),
        hold_cap,
    });
    state
}

#[test]
fn a_message_from_a_retired_pass_cannot_rewrite_a_row() {
    let mut state = live_state(7, Duration::from_secs(120));
    assert!(
        !state.apply(WarmupProgress::Answered {
            generation: 6,
            folder: Folder::Documents,
            outcome: ReadDirOutcome::Failed(consent::ERRNO_EPERM),
        }),
        "a stale generation is dropped"
    );
    assert_eq!(state.rows()[0].1, WarmupRow::Unknown);
    assert!(state.apply(WarmupProgress::Answered {
        generation: 7,
        folder: Folder::Documents,
        outcome: ReadDirOutcome::Failed(consent::ERRNO_EPERM),
    }));
    assert_eq!(state.rows()[0].1, WarmupRow::Denied);
}

#[test]
fn a_pass_that_ends_without_an_answer_clears_asking_to_unknown() {
    let mut state = live_state(1, Duration::from_secs(120));
    assert!(state.apply(WarmupProgress::Asking {
        generation: 1,
        folder: Folder::Documents,
    }));
    assert_eq!(state.rows()[0].1, WarmupRow::Asking);
    state.apply(WarmupProgress::Answered {
        generation: 1,
        folder: Folder::Desktop,
        outcome: ReadDirOutcome::Listed,
    });
    state.end_pass();
    assert_eq!(
        state.rows()[0].1,
        WarmupRow::Unknown,
        "nobody ever learned the answer, and that is not a denial"
    );
    assert_eq!(state.rows()[1].1, WarmupRow::Allowed);
    assert!(!state.is_live());
}

#[test]
fn the_final_message_repairs_a_dropped_one() {
    let mut state = live_state(3, Duration::from_secs(120));
    state.apply(WarmupProgress::Asking {
        generation: 3,
        folder: Folder::Documents,
    });
    assert!(state.apply(WarmupProgress::Finished {
        generation: 3,
        elapsed_ms: 42,
        answers: vec![
            (Folder::Documents, ReadDirOutcome::Listed),
            (
                Folder::Desktop,
                ReadDirOutcome::Failed(consent::ERRNO_EPERM)
            ),
        ],
    }));
    assert_eq!(state.rows()[0].1, WarmupRow::Allowed);
    assert_eq!(state.rows()[1].1, WarmupRow::Denied);
    assert_eq!(state.last_pass_ms(), Some(42));
    assert!(!state.is_live(), "the pass ended, so the hold ends with it");
}

#[test]
fn the_hold_expires_at_the_cap_and_cannot_pin_a_build() {
    let started = Instant::now();
    let cap = Duration::from_millis(120_000);
    assert!(hold_active(started, cap, started));
    assert!(hold_active(
        started,
        cap,
        started + cap - Duration::from_millis(1)
    ));
    assert!(!hold_active(started, cap, started + cap), "at the cap");
    assert!(
        !hold_active(started, cap, started + cap + Duration::from_secs(3600)),
        "an hour later a build is still not pinned"
    );
    assert!(
        !hold_active(started, Duration::ZERO, started),
        "a zero cap never holds"
    );
    assert!(hold_active(started, cap, started - Duration::from_secs(5)));
}

#[test]
fn a_live_pass_holds_only_the_automatic_apply_and_only_to_the_cap() {
    let cap = Duration::from_millis(120_000);
    let mut state = live_state(1, cap);
    let started = state.live.expect("live").started_at;
    assert!(state.holds_automatic_apply(started));
    assert!(!state.holds_automatic_apply(started + cap));
    assert!(!state.holds_automatic_apply(started + cap + Duration::from_secs(600)));
    state.end_pass();
    assert!(!state.holds_automatic_apply(started));
}

#[test]
fn a_successor_starts_with_no_rows_no_hold_and_no_claim() {
    let successor = WarmupState::default();
    assert!(!successor.is_live());
    assert!(successor.rows().is_empty());
    assert_eq!(successor.last_pass_ms(), None);
    assert!(!successor.holds_automatic_apply(Instant::now()));
    assert_eq!(successor.generation, 0);
}

/// Holds [`parking_read_dir`] parked until the test releases it, as a `tccd` modal would.
static PARK: AtomicBool = AtomicBool::new(false);

fn parking_read_dir(_path: &Path) -> ReadDirOutcome {
    while PARK.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(1));
    }
    ReadDirOutcome::Refused
}

#[test]
fn a_second_gesture_while_a_worker_is_live_is_a_no_op() {
    PARK.store(true, Ordering::Release);
    let mut state = WarmupState::inert();
    state.probe = WarmupProbe {
        read_dir: parking_read_dir,
        live: false,
    };
    let folders = vec![(Folder::Documents, PathBuf::from("/nonexistent-warm-up"))];
    let cap = Duration::from_millis(120_000);
    assert_eq!(
        state.start(&folders, cap, || {}),
        StartOutcome::Started,
        "the gesture starts one worker"
    );
    let generation = state.generation;
    assert_eq!(
        state.start(&folders, cap, || {}),
        StartOutcome::AlreadyLive,
        "a second gesture must not stack a second system modal"
    );
    assert_eq!(
        state.generation, generation,
        "no second pass, so no second worker"
    );
    assert!(state.is_live());
    PARK.store(false, Ordering::Release);
}

#[test]
fn a_pass_with_no_resolvable_folder_starts_nothing() {
    let mut state = WarmupState::inert();
    assert_eq!(
        state.start(&[], Duration::from_millis(120_000), || {}),
        StartOutcome::NoFolders
    );
    assert!(!state.is_live());
    assert!(!state.holds_automatic_apply(Instant::now()));
}

#[test]
fn an_inert_pass_completes_pokes_and_claims_nothing() {
    let mut state = WarmupState::inert();
    let pokes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&pokes);
    let folders = vec![
        (Folder::Documents, PathBuf::from("/nonexistent-warm-up-a")),
        (Folder::Desktop, PathBuf::from("/nonexistent-warm-up-b")),
    ];
    assert_eq!(
        state.start(&folders, Duration::from_millis(120_000), move || {
            counter.fetch_add(1, Ordering::Release);
        }),
        StartOutcome::Started
    );
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while state.is_live() && Instant::now() < deadline {
        state.drain();
        aterm_uds::deadline::poll_yield();
    }
    assert!(!state.is_live(), "the pass ended");
    assert!(state.last_pass_ms().is_some());
    assert_eq!(
        state.rows(),
        &[
            (Folder::Documents, WarmupRow::Unknown),
            (Folder::Desktop, WarmupRow::Unknown)
        ],
        "the inert arm asked nothing, so it claims nothing"
    );
    assert!(
        !state.holds_automatic_apply(Instant::now()),
        "a finished pass holds no apply"
    );
    assert!(
        pokes.load(Ordering::Acquire) >= 1,
        "the worker must poke the event loop"
    );
    assert_eq!(
        state.start(&folders, Duration::from_millis(120_000), || {}),
        StartOutcome::Started
    );
}

/// Folders the [`parking_two`] arm is looking at now.
static LOOKING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Parks each look until two are under way at once, as two `tccd` questions queued back to
/// back are; a pass that looks one folder after the other never gets there (the hang
/// detector ends it, refused).
fn parking_two(_path: &Path) -> ReadDirOutcome {
    LOOKING.fetch_add(1, Ordering::AcqRel);
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while LOOKING.load(Ordering::Acquire) < 2 {
        if Instant::now() >= give_up {
            return ReadDirOutcome::Refused;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    ReadDirOutcome::Listed
}

/// The launch pass, after a Full Disk Access page answered without a grant, asks every
/// folder at once: both looks are under way together and both are answered, in the walk's
/// order.
#[test]
fn the_launch_pass_asks_its_folders_together() {
    let mut state = WarmupState::inert();
    state.probe = WarmupProbe {
        read_dir: parking_two,
        live: false,
    };
    let folders = vec![
        (Folder::Documents, PathBuf::from("/nonexistent-warm-up-a")),
        (Folder::Desktop, PathBuf::from("/nonexistent-warm-up-b")),
    ];
    assert_eq!(
        state.start_gated(
            &folders,
            Duration::from_millis(120_000),
            Some(WarmupGate::open()),
            || {}
        ),
        StartOutcome::Started
    );
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN + Duration::from_secs(5);
    while state.is_live() && Instant::now() < deadline {
        state.drain();
        aterm_uds::deadline::poll_yield();
    }
    assert!(!state.is_live(), "the pass ended");
    assert_eq!(
        state.rows(),
        &[
            (Folder::Documents, WarmupRow::Allowed),
            (Folder::Desktop, WarmupRow::Allowed)
        ],
        "both looks were under way at once"
    );
}

/// Folders the [`counting_read_dir`] arm was asked for.
static LOOKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn counting_read_dir(_path: &Path) -> ReadDirOutcome {
    LOOKED.fetch_add(1, Ordering::AcqRel);
    ReadDirOutcome::Listed
}

/// Looks waiting at a shut gate (the person away from System State) never ask once the
/// pass is retired (Stop, a revoked approval): the pass ends with every row unknown.
#[test]
fn a_retired_gate_asks_nothing_of_the_looks_waiting_at_it() {
    let mut state = WarmupState::inert();
    state.probe = WarmupProbe {
        read_dir: counting_read_dir,
        live: false,
    };
    let gate = WarmupGate::default();
    let folders = vec![
        (Folder::Documents, PathBuf::from("/nonexistent-warm-up-a")),
        (Folder::Desktop, PathBuf::from("/nonexistent-warm-up-b")),
    ];
    assert_eq!(
        state.start_gated(
            &folders,
            Duration::from_millis(120_000),
            Some(gate.clone()),
            || {}
        ),
        StartOutcome::Started
    );
    gate.retire();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while state.is_live() && Instant::now() < deadline {
        state.drain();
        aterm_uds::deadline::poll_yield();
    }
    assert!(!state.is_live(), "the retired pass ended");
    assert_eq!(LOOKED.load(Ordering::Acquire), 0, "no look asked");
    assert_eq!(
        state.rows(),
        &[
            (Folder::Documents, WarmupRow::Unknown),
            (Folder::Desktop, WarmupRow::Unknown)
        ]
    );
}

#[test]
fn a_gated_pass_asks_nothing_until_its_gate_opens() {
    let mut state = WarmupState::inert();
    let gate = WarmupGate::default();
    assert!(!gate.is_open());
    let folders = vec![
        (Folder::Documents, PathBuf::from("/nonexistent-warm-up-a")),
        (Folder::Desktop, PathBuf::from("/nonexistent-warm-up-b")),
    ];
    assert_eq!(
        state.start_gated(
            &folders,
            Duration::from_millis(120_000),
            Some(gate.clone()),
            || {}
        ),
        StartOutcome::Started
    );
    assert_eq!(state.drain(), 0, "nothing asked behind a shut gate");
    assert!(state.is_live(), "parked, not ended");
    gate.set(true);
    assert!(gate.is_open());
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while state.is_live() && Instant::now() < deadline {
        state.drain();
        aterm_uds::deadline::poll_yield();
    }
    assert!(!state.is_live(), "the pass ended once the gate opened");
    assert!(state.last_pass_ms().is_some());
}
