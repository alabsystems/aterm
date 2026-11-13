// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::devtools_helper::{Closed, Refusal};

/// A grant verified on this Mac and read off in the process's first seconds is looked at
/// again once settled; one verified on another Mac is handed over as a new Mac's instead.
/// Negative controls: a settled process's off reading, and a handed-over one, look again at
/// nothing.
#[test]
fn only_a_young_processs_off_reading_of_a_grant_verified_here_is_looked_at_again() {
    use crate::consent_card::{LaunchVerdict, NotDue};
    let settling = LaunchVerdict::Quiet(NotDue::Settling);
    let settled = LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified);
    for (verified_here, looks) in [(Some(true), true), (None, true), (Some(false), false)] {
        let hand_over = hands_over(FdaState::Denied, &settling, verified_here);
        assert_eq!(hand_over, !looks, "{verified_here:?}");
        assert_eq!(
            looks_again(&settling, hand_over),
            looks,
            "{verified_here:?}"
        );
        assert!(
            !looks_again(
                &settled,
                hands_over(FdaState::Denied, &settled, verified_here)
            ),
            "the control: settled, no look again ({verified_here:?})"
        );
    }
}

fn facts() -> WarmupFacts {
    WarmupFacts {
        automatic: true,
        fda_launch: FdaLaunch::NotAsked,
        fda: FdaState::Denied,
        probe_pending: false,
        grant_stable: true,
        answered: Some(false),
        walking: false,
    }
}

#[test]
fn the_launch_warm_up_is_a_table_of_named_refusals_then_a_walk() {
    assert_eq!(decide_warmup(&facts()), WarmupStep::Start);
    let ended = |owner_acted| FdaLaunch::Ended { owner_acted };
    let cases = [
        (
            WarmupFacts {
                automatic: false,
                ..facts()
            },
            WarmupStep::Skip(Skip::Off),
        ),
        (
            WarmupFacts {
                fda_launch: FdaLaunch::Undecided,
                ..facts()
            },
            WarmupStep::Wait,
        ),
        (
            WarmupFacts {
                fda_launch: FdaLaunch::Waiting,
                ..facts()
            },
            WarmupStep::Wait,
        ),
        (
            WarmupFacts {
                fda_launch: ended(true),
                ..facts()
            },
            WarmupStep::Skip(Skip::OwnerActed),
        ),
        (
            WarmupFacts {
                fda_launch: ended(false),
                ..facts()
            },
            WarmupStep::Start,
        ),
        (
            WarmupFacts {
                probe_pending: true,
                ..facts()
            },
            WarmupStep::Wait,
        ),
        (
            WarmupFacts {
                fda: FdaState::Granted,
                ..facts()
            },
            WarmupStep::Skip(Skip::NotDenied),
        ),
        (
            WarmupFacts {
                fda: FdaState::Unknown,
                ..facts()
            },
            WarmupStep::Skip(Skip::NotDenied),
        ),
        (
            WarmupFacts {
                grant_stable: false,
                ..facts()
            },
            WarmupStep::Skip(Skip::Unstable),
        ),
        (
            WarmupFacts {
                answered: Some(true),
                ..facts()
            },
            WarmupStep::Skip(Skip::Answered),
        ),
        (
            WarmupFacts {
                walking: true,
                ..facts()
            },
            WarmupStep::Skip(Skip::Walking),
        ),
    ];
    for (facts, step) in cases {
        assert_eq!(decide_warmup(&facts), step, "{facts:?}");
    }
}

#[test]
fn the_probe_free_rules_are_judged_before_a_probe() {
    let unprobed = WarmupFacts {
        fda: FdaState::Unknown,
        probe_pending: true,
        ..facts()
    };
    assert_eq!(decide_warmup(&unprobed), WarmupStep::Wait);
    assert_eq!(decide_warmup(&probe_free(unprobed)), WarmupStep::Start);
    for (facts, why) in [
        (
            WarmupFacts {
                grant_stable: false,
                ..unprobed
            },
            Skip::Unstable,
        ),
        (
            WarmupFacts {
                answered: Some(true),
                ..unprobed
            },
            Skip::Answered,
        ),
        (
            WarmupFacts {
                walking: true,
                ..unprobed
            },
            Skip::Walking,
        ),
    ] {
        assert_eq!(decide_warmup(&facts), WarmupStep::Skip(why));
        assert_eq!(decide_warmup(&probe_free(facts)), WarmupStep::Skip(why));
    }
}

#[test]
fn only_a_panel_left_alone_is_followed_by_the_folders() {
    let acted = |why, dropped| {
        FdaLaunch::ended(FdaEnded { why, dropped }) == FdaLaunch::Ended { owner_acted: true }
    };
    assert!(!acted(Closed::NotNow, false));
    assert!(!acted(Closed::Lapsed, false));
    assert!(
        !acted(Closed::GaveWay, false),
        "a press elsewhere is no act on the page"
    );
    assert!(acted(Closed::NotNow, true));
    assert!(acted(Closed::Lapsed, true));
    assert!(acted(Closed::GaveWay, true));
    for why in [
        Closed::SwitchedOn,
        Closed::Done,
        Closed::SettingsGone,
        Closed::Replaced,
    ] {
        assert!(acted(why, false), "{why:?}");
    }
}

#[test]
fn each_hand_over_answer_moves_the_launch_state() {
    assert_eq!(
        FdaLaunch::after(HandOver::Queued, false),
        FdaLaunch::Waiting
    );
    assert_eq!(FdaLaunch::after(HandOver::Opened, true), FdaLaunch::Waiting);
    assert_eq!(
        FdaLaunch::after(HandOver::Opened, false),
        FdaLaunch::Ended { owner_acted: true }
    );
    for answer in [
        HandOver::Headless,
        HandOver::OpenFailed,
        HandOver::Refused(Refusal::Declined),
        HandOver::Refused(Refusal::Shown),
        HandOver::Refused(Refusal::Granted),
    ] {
        assert_eq!(
            FdaLaunch::after(answer, false),
            FdaLaunch::NotAsked,
            "{answer:?}"
        );
    }
}

#[test]
fn the_launch_pass_never_walks_app_data() {
    let folders = vec![
        (Folder::Documents, 1),
        (Folder::AppData, 2),
        (Folder::Downloads, 3),
    ];
    assert_eq!(
        launch_folders(folders),
        vec![(Folder::Documents, 1), (Folder::Downloads, 3)]
    );
}

#[test]
fn a_pass_is_answered_only_when_every_folder_is() {
    use WarmupRow::{Allowed, Asking, Denied, Error, Unknown};
    assert!(!every_folder_answered(&[]));
    assert!(every_folder_answered(&[
        (Folder::Documents, Allowed),
        (Folder::Desktop, Denied),
        (Folder::Downloads, Error),
    ]));
    for open in [Unknown, Asking] {
        assert!(!every_folder_answered(&[
            (Folder::Documents, Allowed),
            (Folder::Desktop, open),
        ]));
    }
}

#[test]
fn the_marker_names_whole_bundle_ids() {
    let text = "com.aterm.aterm.dev\n";
    assert_eq!(names(text, "com.aterm.aterm.dev"), Some(true));
    assert_eq!(names(text, "com.aterm.aterm"), Some(false));
    assert_eq!(names("", "com.aterm.aterm"), None);
}

thread_local! {
    static NOTED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn noting(bundle_id: &str) {
    NOTED.with(|noted| noted.borrow_mut().push(bundle_id.to_owned()));
}

#[test]
fn a_whole_pass_notes_its_bundle_id_and_a_partial_one_does_not() {
    let reach = LaunchReach {
        note_answered: noting,
        live: true,
        ..LaunchReach::inert()
    };
    let mut state = LaunchConsent::with_reach(reach);
    state.started("com.aterm.aterm", 7);
    assert!(state.walking());
    state.pass_ended(&[(Folder::Documents, WarmupRow::Denied)], false);
    assert!(state.settled());
    assert!(state.owns_rows(7), "its rows stay its own after it ended");
    assert!(!state.owns_rows(8), "an owner's later pass is not");
    assert_eq!(state.answered("com.aterm.aterm"), Some(true));
    NOTED.with(|noted| assert_eq!(*noted.borrow(), ["com.aterm.aterm"]));

    let mut state = LaunchConsent::with_reach(reach);
    state.started("com.aterm.aterm", 1);
    state.pass_ended(&[(Folder::Documents, WarmupRow::Asking)], false);
    assert!(state.settled());
    assert_eq!(state.answered("com.aterm.aterm"), Some(false));
    NOTED.with(|noted| assert_eq!(noted.borrow().len(), 1));
}

#[test]
#[cfg(unix)]
fn the_launch_marker_is_bounded_and_never_follows_a_link() {
    const RELEASE: &str = "com.aterm.aterm";
    const DEV: &str = "com.aterm.aterm.dev";
    let root = aterm_tempfile::TempDir::new().expect("a private launch marker");
    let ordinary = root.path().join("ordinary");
    assert_eq!(
        answered_at(&ordinary, RELEASE),
        Some(false),
        "a genuine absence"
    );
    let recording = aterm_durable::seam::Recording::start();
    note_answered_at(&ordinary, RELEASE);
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "an answered pass is only ordered: an absent marker asks it again"
    );
    drop(recording);
    assert_eq!(
        answered_at(&ordinary, RELEASE),
        Some(true),
        "ordinary write/read"
    );
    assert_eq!(answered_at(&ordinary, DEV), Some(false), "another bundle");
    note_answered_at(&ordinary, DEV);
    let whole = std::fs::read(&ordinary).expect("both bundles");
    assert_eq!(
        (answered_at(&ordinary, RELEASE), answered_at(&ordinary, DEV)),
        (Some(true), Some(true))
    );
    note_answered_at(&ordinary, RELEASE);
    assert_eq!(std::fs::read(&ordinary).unwrap(), whole, "no duplicate");

    let target = root.path().join("target");
    let standing = format!("{DEV}\n");
    std::fs::write(&target, &standing).expect("private target");
    let link = root.path().join("linked");
    std::os::unix::fs::symlink(&target, &link).expect("private link");
    let link_admitted = answered_at(&link, DEV);
    note_answered_at(&link, RELEASE);
    let target_after = std::fs::read(&target).expect("target after writer");

    let oversized = root.path().join("oversized");
    let large = format!("{RELEASE}\n{}\n", "x".repeat(1024 * 1024));
    std::fs::write(&oversized, &large).expect("oversized private record");
    let large_admitted = answered_at(&oversized, RELEASE);
    note_answered_at(&oversized, DEV);
    let large_preserved = std::fs::read(&oversized).unwrap() == large.as_bytes();

    let malformed = root.path().join("malformed");
    let invalid = format!("{RELEASE}\nnot a bundle id\n");
    std::fs::write(&malformed, &invalid).expect("malformed private record");
    let malformed_admitted = answered_at(&malformed, RELEASE);
    note_answered_at(&malformed, DEV);
    let malformed_preserved = std::fs::read(&malformed).unwrap() == invalid.as_bytes();

    assert_eq!(
        (
            link_admitted,
            target_after,
            large_admitted,
            large_preserved,
            malformed_admitted,
            malformed_preserved
        ),
        (None, standing.into_bytes(), None, true, None, true),
        "all standing occupants are observed before the aggregate refusal"
    );
}

#[test]
fn an_unknown_launch_marker_never_admits_an_automatic_pass() {
    const ID: &str = "com.aterm.aterm";
    let root = aterm_tempfile::TempDir::new().expect("private marker state");
    let path = root.path().join("answered");
    let judge = || WarmupFacts {
        answered: answered_at(&path, ID),
        ..facts()
    };
    assert_eq!(judge().answered, Some(false), "known absence");
    assert_eq!(decide_warmup(&judge()), WarmupStep::Start);
    for bytes in [
        &b""[..],
        &b"com.aterm.aterm"[..],
        &b"bad record\n"[..],
        &b"\xff\n"[..],
    ] {
        std::fs::write(&path, bytes).expect("unknown private record");
        assert_eq!(judge().answered, None);
        assert_eq!(
            decide_warmup(&judge()),
            WarmupStep::Skip(Skip::MarkerUnknown)
        );
        note_answered_at(&path, ID);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "unknown is never overwritten"
        );
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(judge().answered, None, "a nonregular occupant");
    assert_eq!(
        decide_warmup(&judge()),
        WarmupStep::Skip(Skip::MarkerUnknown)
    );
    note_answered_at(&path, ID);
    assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
}
