// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    AdmissionInputs, HandoffLaneFacts, SameImageHandoff, SessionlessFacts, SessionlessLane,
    app_bundle_root, launch_environment_over, out_of_band_lane_refusal, reread_passing,
    sessionless_lane,
};

/// An attempt with nothing wrong with it.
fn eligible() -> HandoffLaneFacts {
    HandoffLaneFacts {
        bundled: true,
        launcher_available: true,
        socket_path_fits: true,
        target_not_older: true,
        sessions: 3,
        grant_chunks: false,
        grant_empty: false,
        grant_desk: false,
        environment_is_a_merge: true,
    }
}

/// One field of [`HandoffLaneFacts`] flipped per row against an eligible attempt, each with whether it must refuse the lane.
#[test]
fn each_fact_alone_decides_the_out_of_band_lane() {
    let limit = crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS;
    let chunked = crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS;
    let rows: [(&str, HandoffLaneFacts, bool); 15] = [
        ("eligible", eligible(), false),
        (
            "no launcher",
            HandoffLaneFacts {
                launcher_available: false,
                ..eligible()
            },
            true,
        ),
        (
            "unbundled",
            HandoffLaneFacts {
                bundled: false,
                ..eligible()
            },
            true,
        ),
        (
            "rendezvous path too long",
            HandoffLaneFacts {
                socket_path_fits: false,
                ..eligible()
            },
            true,
        ),
        (
            "older target",
            HandoffLaneFacts {
                target_not_older: false,
                ..eligible()
            },
            true,
        ),
        (
            "sessions == limit",
            HandoffLaneFacts {
                sessions: limit,
                ..eligible()
            },
            false,
        ),
        (
            "sessions == limit + 1",
            HandoffLaneFacts {
                sessions: limit + 1,
                ..eligible()
            },
            true,
        ),
        (
            "100 sessions, chunked grant declared",
            HandoffLaneFacts {
                sessions: 100,
                grant_chunks: true,
                ..eligible()
            },
            false,
        ),
        (
            "100 sessions, no chunked grant",
            HandoffLaneFacts {
                sessions: 100,
                ..eligible()
            },
            true,
        ),
        (
            "chunked grant at the protocol ceiling",
            HandoffLaneFacts {
                sessions: chunked,
                grant_chunks: true,
                ..eligible()
            },
            false,
        ),
        (
            "chunked grant past the protocol ceiling",
            HandoffLaneFacts {
                sessions: chunked + 1,
                grant_chunks: true,
                ..eligible()
            },
            true,
        ),
        (
            "no sessions, no sessionless grant",
            HandoffLaneFacts {
                sessions: 0,
                ..eligible()
            },
            true,
        ),
        (
            "no sessions, sessionless grant declared",
            HandoffLaneFacts {
                sessions: 0,
                grant_empty: true,
                ..eligible()
            },
            false,
        ),
        (
            "sessionless grant declared, too many sessions",
            HandoffLaneFacts {
                sessions: limit + 1,
                grant_empty: true,
                ..eligible()
            },
            true,
        ),
        (
            "environment needs a removal",
            HandoffLaneFacts {
                environment_is_a_merge: false,
                ..eligible()
            },
            true,
        ),
    ];
    for (label, facts, refused) in rows {
        assert_eq!(
            out_of_band_lane_refusal(facts).is_some(),
            refused,
            "{label}"
        );
    }
}

/// The predicate behind `environment_is_a_merge`, against a real `Command`: removing something the launcher lacks is a no-op (the common case), and removing something it has cannot be expressed.
#[test]
fn only_a_removal_that_would_remove_something_disqualifies_the_launch() {
    let mut command = std::process::Command::new("/bin/echo");
    command.env("ATERM_LANE_TEST_KEY", "value");
    command.env_remove("ATERM_LANE_TEST_ABSENT_KEY");
    let launcher = |key: &std::ffi::OsStr| {
        (key == "ATERM_LANE_TEST_PRESENT_KEY").then(|| std::ffi::OsString::from("set"))
    };
    let carried =
        launch_environment_over(&command, launcher).expect("a no-op removal is expressible");
    assert_eq!(
        carried,
        vec![(
            std::ffi::OsString::from("ATERM_LANE_TEST_KEY"),
            std::ffi::OsString::from("value")
        )],
        "only real assignments travel; a vacuous removal contributes nothing"
    );

    command.env_remove("ATERM_LANE_TEST_PRESENT_KEY");
    assert!(
        launch_environment_over(&command, launcher).is_none(),
        "a merge cannot un-set a variable the launcher's own environment has"
    );
}

/// The successor's control directory is the launcher's by construction.
#[test]
fn a_launched_successor_is_handed_the_launchers_control_directory() {
    let launcher = |key: &std::ffi::OsStr| match key.to_str() {
        Some("XDG_RUNTIME_DIR") => Some(std::ffi::OsString::from("/tmp/launcher-run")),
        Some("HOME") => Some(std::ffi::OsString::from("/tmp/launcher-home")),
        _ => None,
    };
    let pair = |key: &str, value: &str| {
        (
            std::ffi::OsString::from(key),
            std::ffi::OsString::from(value),
        )
    };
    let mut command = std::process::Command::new("/bin/echo");
    command.env("ATERM_LANE_TEST_KEY", "value");
    assert_eq!(
        launch_environment_over(&command, launcher),
        Some(vec![
            pair("ATERM_LANE_TEST_KEY", "value"),
            pair("XDG_RUNTIME_DIR", "/tmp/launcher-run"),
            pair("HOME", "/tmp/launcher-home"),
        ])
    );
    command.env("HOME", "/tmp/explicit-home");
    assert_eq!(
        launch_environment_over(&command, |key| (key == "HOME")
            .then(|| std::ffi::OsString::from("/tmp/launcher-home"))),
        Some(vec![
            pair("ATERM_LANE_TEST_KEY", "value"),
            pair("HOME", "/tmp/explicit-home"),
        ]),
        "the Command's own HOME wins, and an unset XDG_RUNTIME_DIR stays unset"
    );
}

/// The bundle root of `<bundle>.app/Contents/MacOS/<bin>` is `<bundle>.app`, and the `.app` suffix is checked: a dev binary also sits three levels below something.
#[test]
fn only_a_dot_app_three_levels_up_is_a_bundle_root() {
    let temp = std::env::temp_dir().join(format!("aterm-lane-{}", std::process::id()));
    let bundle = temp.join("aterm.app");
    let macos = bundle.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("fixture bundle");
    assert_eq!(
        app_bundle_root(&macos.join("aterm")).as_deref(),
        Some(bundle.as_path())
    );

    let plain = temp.join("target/debug/deps");
    std::fs::create_dir_all(&plain).expect("fixture dev tree");
    assert_eq!(
        app_bundle_root(&plain.join("aterm-gui")),
        None,
        "a dev build is three levels below a directory too, and is not a bundle"
    );
    assert_eq!(
        app_bundle_root(std::path::Path::new("aterm")),
        None,
        "and a bare name has no three levels at all"
    );
    let _ = std::fs::remove_dir_all(&temp);
}

/// A desk with windows to hand over and a launched lane that carries an attempt with no session.
fn carried_alone() -> SessionlessFacts {
    SessionlessFacts {
        window: true,
        layout: true,
        workers_alone: false,
        lane: HandoffLaneFacts {
            sessions: 0,
            grant_empty: true,
            ..eligible()
        },
        same_image: false,
        verdict: Some(true),
        image_kept: true,
    }
}

/// The sessionless lane agrees with the lane choice row for row, names a cause no desk clears
/// before the desk's and a cause the verdict cannot change before it waits for an unread
/// verdict, and lets a failed verdict through to be reported as the attempt's own failure
/// (never as "cannot hand over") on a kept image only: a replaceable image leaves it to the
/// cold lane.
///
/// REVIEW FINDING (2026-11-03): the desk was named first, so a renewal on a process the lane can
/// never carry (unbundled, a rendezvous path too long) waited uncounted for as long as the desk
/// stood empty, though no desk change could let it run.
#[test]
fn the_sessionless_lane_is_the_launched_lane_with_windows_to_hand_over() {
    use SessionlessLane::{Cannot, Takes, Unread};
    let base = carried_alone();
    let lane = |edit: fn(&mut HandoffLaneFacts)| {
        let mut facts = base;
        edit(&mut facts.lane);
        facts
    };
    let no_tab = "no open tab is one the new build can restore";
    let lists = SessionlessFacts {
        layout: false,
        workers_alone: true,
        ..base
    };
    let declared = |facts: SessionlessFacts| {
        let mut facts = facts;
        facts.lane.grant_desk = true;
        facts
    };
    let rows: [(&str, SessionlessFacts, SessionlessLane); 27] = [
        ("carried", base, Takes),
        (
            "lone lists: only the target's read says whether it declares desk1",
            SessionlessFacts {
                verdict: None,
                ..lists
            },
            Unread,
        ),
        (
            "lone lists to a target that declares desk1",
            declared(lists),
            Takes,
        ),
        (
            "lone lists to a target that does not",
            lists,
            Cannot(no_tab),
        ),
        (
            "tabless, not all lists: no read can change it",
            SessionlessFacts {
                layout: false,
                verdict: None,
                ..base
            },
            Cannot(no_tab),
        ),
        (
            "tabless, not all lists, to a target that declares desk1",
            declared(SessionlessFacts {
                layout: false,
                ..base
            }),
            Cannot(no_tab),
        ),
        (
            "lone lists, a failed verdict on a kept image: the attempt's own failure",
            SessionlessFacts {
                verdict: Some(false),
                ..lists
            },
            Takes,
        ),
        (
            "lone lists, a failed verdict on a replaceable image: the cold lane's",
            SessionlessFacts {
                verdict: Some(false),
                image_kept: false,
                ..lists
            },
            Cannot("the new build failed its pre-park verification"),
        ),
        (
            "lone lists to a target with desk1 and no empty1",
            SessionlessFacts {
                lane: HandoffLaneFacts {
                    grant_empty: false,
                    grant_desk: true,
                    ..lists.lane
                },
                ..lists
            },
            Cannot("the successor does not read a grant with no terminal session"),
        ),
        (
            "lone lists, the same image: it has no verdict to read",
            declared(SessionlessFacts {
                same_image: true,
                verdict: None,
                ..lists
            }),
            Takes,
        ),
        (
            "lone lists, unbundled: a cause no read changes, before any wait",
            SessionlessFacts {
                verdict: None,
                lane: HandoffLaneFacts {
                    bundled: false,
                    ..lists.lane
                },
                ..lists
            },
            Cannot("this process does not run from a .app bundle"),
        ),
        (
            "no window",
            SessionlessFacts {
                window: false,
                ..base
            },
            Cannot("no window is open to hand over"),
        ),
        (
            "no restorable tab",
            SessionlessFacts {
                layout: false,
                ..base
            },
            Cannot("no open tab is one the new build can restore"),
        ),
        (
            "unbundled",
            lane(|lane| lane.bundled = false),
            Cannot("this process does not run from a .app bundle"),
        ),
        (
            "no window, unbundled: the cause no desk clears is named",
            SessionlessFacts {
                window: false,
                lane: HandoffLaneFacts {
                    bundled: false,
                    ..base.lane
                },
                ..base
            },
            Cannot("this process does not run from a .app bundle"),
        ),
        (
            "no restorable tab, rendezvous path too long: the cause no desk clears is named",
            SessionlessFacts {
                layout: false,
                lane: HandoffLaneFacts {
                    socket_path_fits: false,
                    ..base.lane
                },
                ..base
            },
            Cannot("the rendezvous path does not fit sun_path on this machine"),
        ),
        (
            "rendezvous path too long",
            lane(|lane| lane.socket_path_fits = false),
            Cannot("the rendezvous path does not fit sun_path on this machine"),
        ),
        (
            "older target",
            lane(|lane| lane.target_not_older = false),
            Cannot("the authorized target build is older than this build"),
        ),
        (
            "environment needs a removal",
            lane(|lane| lane.environment_is_a_merge = false),
            Cannot("the launch environment needs a removal a merge cannot express"),
        ),
        (
            "unread verdict",
            SessionlessFacts {
                verdict: None,
                ..base
            },
            Unread,
        ),
        (
            "unread verdict, unbundled: the static cause is named, not waited for",
            SessionlessFacts {
                verdict: None,
                lane: HandoffLaneFacts {
                    bundled: false,
                    ..base.lane
                },
                ..base
            },
            Cannot("this process does not run from a .app bundle"),
        ),
        (
            "failed verdict: reported as the attempt's own",
            SessionlessFacts {
                verdict: Some(false),
                lane: HandoffLaneFacts {
                    grant_empty: false,
                    ..base.lane
                },
                ..base
            },
            Takes,
        ),
        (
            "failed verdict, replaceable image: the cold lane's",
            SessionlessFacts {
                verdict: Some(false),
                image_kept: false,
                lane: HandoffLaneFacts {
                    grant_empty: false,
                    ..base.lane
                },
                ..base
            },
            Cannot("the new build failed its pre-park verification"),
        ),
        (
            "passed verdict, no sessionless grant declared",
            lane(|lane| lane.grant_empty = false),
            Cannot("the successor does not read a grant with no terminal session"),
        ),
        (
            "same image, no verdict",
            SessionlessFacts {
                same_image: true,
                verdict: None,
                ..base
            },
            Takes,
        ),
        (
            "same image, unbundled",
            SessionlessFacts {
                same_image: true,
                verdict: None,
                lane: HandoffLaneFacts {
                    bundled: false,
                    ..base.lane
                },
                ..base
            },
            Cannot("this process does not run from a .app bundle"),
        ),
        (
            "no launcher",
            lane(|lane| lane.launcher_available = false),
            Cannot("this platform has no LaunchServices launcher"),
        ),
    ];
    for (label, facts, expected) in rows {
        assert_eq!(sessionless_lane(facts), expected, "{label}");
        let a_desk_to_hand = facts.layout || (facts.workers_alone && facts.lane.grant_desk);
        if facts.window && a_desk_to_hand && facts.verdict == Some(true) {
            assert_eq!(
                expected == Takes,
                out_of_band_lane_refusal(facts.lane).is_none(),
                "{label}: the lane choice agrees"
            );
        }
    }
}

/// A test App that keeps its image with no live terminal session: its one pane exited under
/// `--hold`, and a Settings tab is open.
fn kept_image_without_a_live_terminal() -> crate::App {
    let mut app = crate::App::headless_for_test();
    app.launch_image_kept = true;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
    for id in ids {
        app.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_state(id, crate::session_store::SessionState::Exited);
    }
    assert!(
        app.handoff_live_sessions().is_empty(),
        "PRECONDITION: no live session"
    );
    assert!(
        !app.capture_handoff_layout().is_empty(),
        "PRECONDITION: windows to hand over"
    );
    app
}

fn admit(
    app: &mut crate::App,
    lane: HandoffLaneFacts,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
) -> Result<crate::native_update_admission::ApplyLane, crate::UpdateHandoffStartError> {
    let token = crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0, 0);
    app.admit_update_handoff(
        &[],
        AdmissionInputs {
            build: 7,
            target_build: 7,
            mode: crate::native_updater_service::ApplyMode::Immediate,
            apply_attempt: attempt,
            same_image: attempt.is_none().then_some(SameImageHandoff::DebugSeam),
            safety_token: &token,
            handoff_unavailable: None,
            lane,
        },
    )
}

/// THE INCIDENT (0.145.0, 2026-11-02): a Dock-launched aterm with no live terminal session
/// refused every update until a tab opened. The admission `start_unix_update_handoff` runs
/// hands it to the overlap handoff, and a desk the launched lane cannot carry is told it
/// installs at the next launch, which the posture then says too.
#[test]
fn a_kept_image_with_no_live_terminal_is_handed_over_not_refused() {
    use crate::native_update_admission::ApplyLane;
    let mut app = kept_image_without_a_live_terminal();
    let carried = HandoffLaneFacts {
        sessions: 0,
        grant_empty: true,
        ..eligible()
    };
    assert_eq!(
        admit(&mut app, carried, None).ok(),
        Some(ApplyLane::Seamless),
        "a same-image handoff, whose target reads a sessionless grant, takes the overlap lane"
    );
    assert_eq!(app.sessionless_handoff_refused, None);
    assert!(!app.kept_image_installs_at_next_launch());

    let unbundled = HandoffLaneFacts {
        bundled: false,
        ..carried
    };
    let refused = admit(&mut app, unbundled, None).expect_err("CONTROL: no bundle, no lane");
    assert!(refused.is_refused());
    assert_eq!(
        refused.into_message(),
        "The update waits for the next launch: this process does not run from a .app bundle"
    );
    assert_eq!(
        app.sessionless_handoff_refused,
        Some("this process does not run from a .app bundle")
    );
    assert!(
        app.kept_image_installs_at_next_launch(),
        "the posture reads what the admission found"
    );
    assert_eq!(
        admit(&mut app, carried, None).ok(),
        Some(ApplyLane::Seamless)
    );
    assert!(
        !app.kept_image_installs_at_next_launch(),
        "an admission that carries it clears the cause"
    );

    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    let publish = |app: &crate::App, grant_empty: bool| {
        super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
            .expect("an attempt names its artifact")
            .publish_pass(
                &aterm_update::HandoffCandidateFacts {
                    policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
                    grant_chunks: false,
                    grant_empty,
                    grant_desk: false,
                },
                crate::running_build_number(),
                "the test candidate",
            );
    };
    publish(&app, false);
    let undeclared = HandoffLaneFacts {
        grant_empty: false,
        ..carried
    };
    assert!(
        admit(&mut app, undeclared, Some(&ticket)).is_err(),
        "a target that does not declare empty1 is never sent an empty grant"
    );
    assert!(app.kept_image_installs_at_next_launch());
    publish(&app, true);
    assert_eq!(
        admit(&mut app, carried, Some(&ticket)).ok(),
        Some(ApplyLane::Seamless),
        "a target that declares empty1 is handed the windows"
    );
    super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
        .expect("an attempt names its artifact")
        .publish_refusal("the install location is not writable");
    assert_eq!(
        admit(&mut app, undeclared, Some(&ticket)).ok(),
        Some(ApplyLane::Seamless),
        "a failed verdict passes the admission, whose verdict branch reports the real reason"
    );
    assert!(!app.kept_image_installs_at_next_launch());

    app.launch_image_kept = false;
    let refused = admit(&mut app, unbundled, None).expect_err("held panes");
    assert!(
        refused.into_message().contains("--hold"),
        "CONTROL: a replaceable image the launched lane cannot carry is the cold lane's, \
         which would close its held panes"
    );
    assert!(!app.kept_image_installs_at_next_launch());
}

/// THE LAST HOLD (2026-11-03): a kept image whose only window held an empty Workers list waited
/// for the person to open a tab or quit. A target that declares `desk1` is handed the list, and
/// one that does not leaves the update to the next launch, naming the desk.
#[test]
fn a_kept_image_holding_only_its_workers_list_is_handed_over() {
    use crate::native_update_admission::ApplyLane;
    let mut app = crate::app_workers_tests::app_with_a_lone_list();
    app.launch_image_kept = true;
    assert!(
        app.handoff_live_sessions().is_empty(),
        "PRECONDITION: no live session"
    );
    assert!(
        app.capture_handoff_layout().is_empty(),
        "PRECONDITION: no tab to hand over"
    );
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    let publish = |app: &crate::App, grant_desk: bool| {
        super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
            .expect("an attempt names its artifact")
            .publish_pass(
                &aterm_update::HandoffCandidateFacts {
                    policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
                    grant_chunks: false,
                    grant_empty: true,
                    grant_desk,
                },
                crate::running_build_number(),
                "the test candidate",
            );
    };
    let carried = HandoffLaneFacts {
        sessions: 0,
        grant_empty: true,
        grant_desk: true,
        ..eligible()
    };
    publish(&app, true);
    assert_eq!(
        admit(&mut app, carried, Some(&ticket)).ok(),
        Some(ApplyLane::Seamless),
        "a target that declares desk1 is handed the list"
    );
    assert_eq!(app.sessionless_handoff_refused, None);
    assert!(!app.kept_image_installs_at_next_launch());

    publish(&app, false);
    let refused = admit(
        &mut app,
        HandoffLaneFacts {
            grant_desk: false,
            ..carried
        },
        Some(&ticket),
    )
    .expect_err("CONTROL: a target that does not declare it never gets a row it cannot place");
    assert_eq!(
        refused.into_message(),
        "The update waits for the next launch: no open tab is one the new build can restore"
    );
    assert!(app.kept_image_installs_at_next_launch());

    assert_eq!(
        admit(&mut app, carried, None).ok(),
        Some(ApplyLane::Seamless),
        "a same-image renewal targets this build, which rebuilds the list"
    );
    assert!(!app.kept_image_installs_at_next_launch());
}

/// The way a real update goes: what the target's read declared, through the lane's own reading of
/// it, to the admission. A same-image handoff has no read, and targets this very build.
#[test]
fn a_targets_read_reaches_the_admission_through_the_lanes_facts() {
    use crate::native_update_admission::ApplyLane;
    let mut app = crate::app_workers_tests::app_with_a_lone_list();
    app.launch_image_kept = true;
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    let read = |app: &crate::App, grant_desk: bool| {
        super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
            .expect("an attempt names its artifact")
            .publish_pass(
                &aterm_update::HandoffCandidateFacts {
                    policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
                    grant_chunks: false,
                    grant_empty: true,
                    grant_desk,
                },
                crate::running_build_number(),
                "the test candidate",
            );
    };
    // Whether this machine's socket path fits is no part of the question.
    let facts = |app: &crate::App, attempt, same_image| HandoffLaneFacts {
        socket_path_fits: true,
        ..app.launched_lane_facts(attempt, same_image, true, true, 0, true)
    };

    read(&app, false);
    let undeclared = facts(&app, Some(&ticket), None);
    assert!(undeclared.grant_empty && !undeclared.grant_desk);
    assert!(
        admit(&mut app, undeclared, Some(&ticket)).is_err(),
        "CONTROL: a read that did not declare it"
    );

    read(&app, true);
    let declared = facts(&app, Some(&ticket), None);
    assert!(declared.grant_desk);
    assert_eq!(
        admit(&mut app, declared, Some(&ticket)).ok(),
        Some(ApplyLane::Seamless),
        "a read that declared it"
    );

    let same_image = facts(&app, None, Some(SameImageHandoff::DebugSeam));
    assert!(same_image.grant_empty && same_image.grant_desk);
    assert_eq!(
        admit(&mut app, same_image, None).ok(),
        Some(ApplyLane::Seamless),
        "this very build"
    );
}

/// The chunk grant is the target's read or a launch-record renewal's: a renewal targets this very
/// build, and any other same-image handoff has only what a read says.
#[test]
fn the_lanes_chunk_grant_is_a_reads_or_a_renewals() {
    let app = crate::App::headless_for_test();
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    let chunks = |attempt, same_image| {
        app.launched_lane_facts(attempt, same_image, true, true, 0, true)
            .grant_chunks
    };
    assert!(!chunks(Some(&ticket), None), "nothing read yet");
    assert!(chunks(None, Some(SameImageHandoff::RenewLaunchRecord)));
    assert!(!chunks(None, Some(SameImageHandoff::DebugSeam)));
    super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
        .expect("an attempt names its artifact")
        .publish_pass(
            &aterm_update::HandoffCandidateFacts {
                policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
                grant_chunks: true,
                grant_empty: false,
                grant_desk: false,
            },
            crate::running_build_number(),
            "the test candidate",
        );
    assert!(chunks(Some(&ticket), None), "a read that declared it");
}

/// The desk grant is learned only from a passed read of the very artifact: a read that ended
/// passing writes nothing, and a refusal or another artifact's pass says nothing about this one.
#[test]
fn only_a_passed_read_of_this_artifact_says_the_target_declares_the_desk() {
    let app = crate::App::headless_for_test();
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    let facts = aterm_update::HandoffCandidateFacts {
        policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
        grant_chunks: false,
        grant_empty: false,
        grant_desk: true,
    };
    let publisher = super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
        .expect("an attempt names its artifact");
    assert!(
        !app.park_policy(Some(&ticket)).grant_desk,
        "nothing read yet"
    );
    publisher.publish_pass(&facts, crate::running_build_number(), "the test candidate");
    assert!(app.park_policy(Some(&ticket)).grant_desk, "a passed read");
    let other =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"cd".repeat(32));
    assert!(
        !app.park_policy(Some(&other)).grant_desk,
        "another artifact's read says nothing about this one"
    );
    publisher.publish_refusal(&format!(
        "lock: could not take the apply lock within 10s ({})",
        aterm_update::PASSING_REFUSAL_KEY
    ));
    assert!(
        app.park_policy(Some(&ticket)).grant_desk,
        "a read that ended passing leaves what was known"
    );
    publisher.publish_refusal("the install location is not writable");
    let refused = app.park_policy(Some(&ticket));
    assert!(
        !refused.grant_desk && !refused.known,
        "a refusal says nothing about what it declares"
    );
}

/// A target that does not declare `desk1` refuses a layout of lone lists whole and opens a new
/// shell for one beside a tab, so whatever the capture holds, the parent sends it neither.
#[test]
fn a_lone_workers_list_goes_on_the_wire_only_to_a_target_that_declares_it() {
    use crate::restore::{RestoreManifest, WindowLayout};
    let lone = WindowLayout::lone_workers_list(24, 80);
    let alone = RestoreManifest::new(vec![lone.clone()]);
    let undeclared = "the handoff layout carries a Workers list the successor does not declare";
    assert_eq!(
        super::outgoing_layout_wire(&alone, false),
        Err(undeclared),
        "no tab beside it"
    );
    let wire = super::outgoing_layout_wire(&alone, true).expect("a target that declares it");
    assert_eq!(RestoreManifest::from_toml(&wire).as_ref(), Some(&alone));

    let mut tabbed = WindowLayout {
        workers_in_front: false,
        ..lone
    };
    tabbed.restored_tabs = vec![crate::restore::RestoredTab {
        root: crate::restore::RestoredSplitTree::leaf(crate::restore::RestoredView::Native(
            crate::restore::NativeLeafRestore::settings("/".to_string()),
        )),
        focused_path: Vec::new(),
        zoomed: false,
        parked: false,
        ephemeral: false,
    }];
    tabbed.active_item = Some(0);
    let mixed = RestoreManifest::new(vec![tabbed.clone(), alone.windows[0].clone()]);
    assert_eq!(
        super::outgoing_layout_wire(&mixed, false),
        Err(undeclared),
        "beside a tab too: a new shell the person never had"
    );
    assert!(super::outgoing_layout_wire(&mixed, true).is_ok());

    let plain = RestoreManifest::new(vec![tabbed]);
    assert!(
        super::outgoing_layout_wire(&plain, false).is_ok(),
        "CONTROL: a desk with no lone list needs no declaration"
    );
    assert_eq!(
        super::outgoing_layout_wire(&RestoreManifest::new(Vec::new()), true),
        Err("could not persist the bounded handoff layout"),
        "CONTROL: nothing to place is still the producer's fault"
    );
}

/// REVIEW FINDING (2026-11-03): a replaceable image (one a cold relaunch can replace) with no
/// terminal tab and a failed pre-park verdict cached lost its cold lane: the sessionless lane
/// took the attempt and reported the stale verdict, refusing a person's Install for up to ten
/// minutes after its cause was fixed. Before the sessionless lane, the press relaunched and the
/// boot apply verified the stage again; it does so again, and a kept image, which has no cold
/// lane, still reports the verdict as the attempt's own.
#[test]
fn a_failed_verdict_leaves_a_replaceable_image_with_no_terminal_to_the_cold_lane() {
    use crate::native_update_admission::ApplyLane;
    let mut app = crate::App::headless_for_test();
    app.launch_image_kept = false;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(
        app.handoff_exited_session_ids().is_empty(),
        "PRECONDITION: no --hold pane the cold lane would close"
    );
    assert!(
        !app.capture_handoff_layout().is_empty(),
        "PRECONDITION: windows to hand over"
    );
    let ticket =
        crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
    super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
        .expect("an attempt names its artifact")
        .publish_refusal("the install location is not writable");
    let lane = HandoffLaneFacts {
        sessions: 0,
        grant_empty: false,
        ..eligible()
    };
    assert_eq!(
        admit(&mut app, lane, Some(&ticket)).ok(),
        Some(ApplyLane::Cold),
        "the cold lane, whose gate lets a person's press relaunch and verify again"
    );
    assert_eq!(app.sessionless_handoff_refused, None);
    assert!(
        app.cold_lane_verdict_gate(
            crate::native_updater_service::ApplyMode::Immediate,
            Some(&ticket)
        )
        .is_ok(),
        "a person's press goes ahead on the cold lane"
    );
    app.launch_image_kept = true;
    assert_eq!(
        admit(&mut app, lane, Some(&ticket)).ok(),
        Some(ApplyLane::Seamless),
        "CONTROL: a kept image reports the verdict as the attempt's own"
    );
}

/// REVIEW FINDING (2026-11-03): an attempt with no terminal session read its targeted stage
/// again whenever its last read ended without a verdict, so a stage waiting for an update check
/// to confirm its signer spun a person's held Install through back-to-back reads (every 20 ms)
/// and the automatic lane through one every 500 ms. A signer wait is now held for that check,
/// as the cold lane holds it, and any other passing cause is read again unless the wait started
/// the read.
///
/// REVIEW FINDING (2026-11-03, the fix's own review): the wait told its own read by start time
/// against a wait that never ended, so a person's every press for ten minutes met the stale read
/// (no read, no cold lane) and a read the roster lane started hours later counted as the wait's.
/// The wait now knows its own read by deadline, its failure ends the wait, and a person's press on
/// a replaceable image goes on to the cold lane.
///
/// REVIEW FINDING (2026-11-03, the second fix's review): one record served both kinds of attempt
/// and outlived the press that set it, so a press could be answered by a read only the automatic
/// lane waited for, or by its own earlier press's read after that press ended another way; and a
/// press over `--hold` panes was sent on to a lane choice that refuses it before the cold lane.
/// Each kind now owns the read it waited for, a press's entry ends with the press, and the press
/// goes on only where the cold lane takes it.
///
/// REVIEW FINDING (2026-11-03, the third fix's review): a window an earlier read spent answered a
/// read the arm started since as "not read", so a press during it was refused while its verdict
/// was seconds away. The attempt now gets a window of its own.
#[test]
fn a_sessionless_read_that_ends_without_a_verdict_is_never_read_again_in_a_loop() {
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let artifact = "ab".repeat(32);
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, commit, &artifact);
    let roster = format!(
        "staged update failed pre-park verification: staged build {target} has not been {} on \
         this Mac, so a revocation of its signer could have been missed; it installs once the \
         next update check confirms it ({})",
        aterm_update::ROSTER_UNCONFIRMED_KEY,
        aterm_update::PASSING_REFUSAL_KEY
    );
    let lock = format!(
        "staged update failed pre-park verification: lock: could not take the apply lock \
         within 10s (another aterm holds it) ({})",
        aterm_update::PASSING_REFUSAL_KEY
    );
    assert!(aterm_update::is_roster_confirmation_wait(&roster));
    assert!(
        !aterm_update::is_roster_confirmation_wait(&lock)
            && aterm_update::is_passing_refusal(&lock)
    );
    let ended_passing = |reason: &str| {
        let passing = Arc::new(std::sync::OnceLock::new());
        passing
            .set(crate::PassingPreverification {
                reason: reason.to_string(),
                ended: Instant::now(),
            })
            .unwrap();
        crate::PreverifyInFlight {
            build: target,
            commit: commit.to_string(),
            artifact: artifact.clone(),
            deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
            done: Arc::new(AtomicBool::new(true)),
            passing,
        }
    };
    let deadline = |app: &crate::App| {
        app.handoff_preverify_in_flight
            .as_ref()
            .map(|read| read.deadline)
    };

    // The re-read rule: a read is known by its deadline.
    let now = Instant::now();
    let other = now + std::time::Duration::from_secs(1);
    assert!(
        reread_passing(&lock, now, None),
        "no read this kind of attempt waited for"
    );
    assert!(
        reread_passing(&lock, now, Some(other)),
        "a read it did not wait for (the arm-time read, the roster lane's)"
    );
    assert!(
        !reread_passing(&lock, now, Some(now)),
        "the read it waited for"
    );
    assert!(
        reread_passing(&roster, now, Some(now)),
        "a signer wait the roster gate let through"
    );

    // A signer wait holds a person's press for the check, starting no read and arming nothing.
    app.handoff_preverify_in_flight = Some(ended_passing(&roster));
    let before = deadline(&app);
    let held = app
        .sessionless_preverify_wait(ApplyMode::Immediate, &ticket)
        .expect("the attempt waits for the check");
    assert!(held.is_activity_deferred(), "charged nothing: {held:?}");
    assert!(
        aterm_update::is_roster_confirmation_wait(&held.to_string()),
        "and names the wait: {held}"
    );
    assert_eq!(deadline(&app), before, "no read started");
    assert_eq!(app.auto_apply_intent, None, "a person's press arms nothing");
    // The automatic lane holds for it too, due when the hold asks again.
    let held = app
        .sessionless_preverify_wait(ApplyMode::Automatic, &ticket)
        .expect("the attempt waits for the check");
    assert!(held.is_activity_deferred(), "{held:?}");
    let wait = app.auto_apply_roster_wait.clone().expect("the lane holds");
    assert_eq!(wait.build, target);
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == target && intent.retry_at == wait.reread_at)
    );
    assert_eq!(deadline(&app), before, "no read started");

    // Any other passing cause is read again unless this kind of attempt waited for the read.
    // The stage is seeded, so a read can start (a stageless App starts none, and every half
    // below would hold on code that read in a loop or never read again).
    app.auto_apply_roster_wait = None;
    app.auto_apply_intent = None;
    app.launch_image_kept = true;
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let waited = |app: &mut crate::App, mode: ApplyMode| *app.sessionless_waited.of(mode);
    // The read `mode` waited for ends passing.
    let waited_read_ended = |app: &mut crate::App, mode: ApplyMode| {
        let own = waited(app, mode).expect("the read the attempt waited for");
        app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
            deadline: own,
            ..ended_passing(&lock)
        });
        own
    };
    // A read this test starts runs for real (on the scratch update root) and writes its verdict
    // when it ends; each such read gets a slot of its own, so no verdict reaches a later step.
    let read_alone = |app: &mut crate::App| {
        app.handoff_preverified = Arc::new(std::sync::Mutex::new(None));
    };
    let reads_again = |app: &mut crate::App, mode: ApplyMode| {
        let before = deadline(app);
        let wait = app.sessionless_preverify_wait(mode, &ticket);
        read_alone(app);
        let wait = wait.expect("the attempt waits for the read");
        assert_ne!(deadline(app), before, "a read started");
        assert_eq!(
            waited(app, mode),
            deadline(app),
            "the attempt waits for the read it started"
        );
        wait
    };
    // With no read at all, the wait starts one (whether it still runs when the wait looks is
    // the read thread's pace, so the ownership is set by hand below).
    app.sessionless_waited = crate::app_update_handoff::SessionlessWaited::default();
    app.handoff_preverify_in_flight = None;
    app.preverify_read_waited = None;
    read_alone(&mut app);
    let _ = app.sessionless_preverify_wait(ApplyMode::Immediate, &ticket);
    read_alone(&mut app);
    assert!(deadline(&app).is_some(), "a read started");
    // A press held for a read it did not start (the arm-time read, still running) waits for it,
    // and so owns its end.
    app.sessionless_waited = crate::app_update_handoff::SessionlessWaited::default();
    let running = crate::PreverifyInFlight {
        done: Arc::new(AtomicBool::new(false)),
        passing: Arc::new(std::sync::OnceLock::new()),
        ..ended_passing(&lock)
    };
    let running_deadline = running.deadline;
    app.handoff_preverify_in_flight = Some(running);
    app.preverify_read_waited = None;
    let held = app
        .sessionless_preverify_wait(ApplyMode::Immediate, &ticket)
        .expect("the press waits for the running read");
    read_alone(&mut app);
    assert!(held.held_for_read_of().is_some(), "{held:?}");
    assert_eq!(deadline(&app), Some(running_deadline), "no second read");
    assert_eq!(
        waited(&mut app, ApplyMode::Immediate),
        Some(running_deadline)
    );
    assert_eq!(
        waited(&mut app, ApplyMode::Automatic),
        None,
        "the automatic lane waited for nothing"
    );
    // Its passing end is the press's failure; the wait asks for no second read.
    let own = waited_read_ended(&mut app, ApplyMode::Immediate);
    let failed = app
        .sessionless_preverify_wait(ApplyMode::Immediate, &ticket)
        .expect("the read answered the attempt");
    assert!(
        !failed.is_activity_deferred() && !failed.is_refused(),
        "a Failed: {failed:?}"
    );
    assert!(
        failed.to_string().contains("press again to check again"),
        "nothing tries again for a press: {failed}"
    );
    assert_eq!(deadline(&app), Some(own), "no read started");
    assert_eq!(
        waited(&mut app, ApplyMode::Immediate),
        None,
        "the wait ended"
    );
    // So the person's next press reads afresh instead of meeting the stale read for ten
    // minutes.
    let held = reads_again(&mut app, ApplyMode::Immediate);
    assert!(held.held_for_read_of().is_some(), "{held:?}");
    // A window an earlier read spent does not answer a read started since (the arm's): the press
    // is held for that read, in a window of its own.
    let running = crate::PreverifyInFlight {
        done: Arc::new(AtomicBool::new(false)),
        passing: Arc::new(std::sync::OnceLock::new()),
        ..ended_passing(&lock)
    };
    let running_deadline = running.deadline;
    app.handoff_preverify_in_flight = Some(running);
    app.preverify_read_waited = Some((target, Instant::now()));
    let held = app
        .sessionless_preverify_wait(ApplyMode::Immediate, &ticket)
        .expect("the press waits for the later read");
    read_alone(&mut app);
    assert!(held.held_for_read_of().is_some(), "{held:?}");
    assert_eq!(deadline(&app), Some(running_deadline), "no second read");
    // With none running, a spent window starts a read rather than answering "not read".
    app.handoff_preverify_in_flight = None;
    app.preverify_read_waited = Some((target, Instant::now()));
    read_alone(&mut app);
    let _ = app.sessionless_preverify_wait(ApplyMode::Immediate, &ticket);
    read_alone(&mut app);
    assert!(deadline(&app).is_some(), "a read started");
    // A press is never answered by a read only the automatic lane waited for: it reads again.
    app.sessionless_waited = crate::app_update_handoff::SessionlessWaited::default();
    let lane_read = deadline(&app);
    app.sessionless_waited.automatic = lane_read;
    waited_read_ended(&mut app, ApplyMode::Automatic);
    let held = reads_again(&mut app, ApplyMode::Immediate);
    assert!(held.held_for_read_of().is_some(), "{held:?}");
    assert_eq!(
        app.sessionless_waited.automatic, lane_read,
        "the lane's entry is its own"
    );
    // The automatic lane: one read, then its failure, which it does try again; then it reads
    // afresh.
    app.sessionless_waited.automatic = deadline(&app);
    let own = waited_read_ended(&mut app, ApplyMode::Automatic);
    let failed = app
        .sessionless_preverify_wait(ApplyMode::Automatic, &ticket)
        .expect("the read answered the attempt");
    assert!(
        !failed.is_activity_deferred() && !failed.is_refused(),
        "{failed:?}"
    );
    assert!(failed.to_string().contains("tried again later"), "{failed}");
    assert_eq!(deadline(&app), Some(own), "no read started");
    assert_eq!(
        waited(&mut app, ApplyMode::Automatic),
        None,
        "the wait ended"
    );
    let deferred = reads_again(&mut app, ApplyMode::Automatic);
    assert!(deferred.is_activity_deferred(), "{deferred:?}");
    // A person's press on an image a relaunch replaces goes on to the cold lane instead, which
    // relaunches and verifies again; the automatic lane fails there too.
    app.launch_image_kept = false;
    app.sessionless_waited.person = deadline(&app);
    waited_read_ended(&mut app, ApplyMode::Immediate);
    assert!(
        app.sessionless_preverify_wait(ApplyMode::Immediate, &ticket)
            .is_none(),
        "a person's press reaches the lane choice"
    );
    assert_eq!(
        waited(&mut app, ApplyMode::Immediate),
        None,
        "and the wait ended"
    );
    assert!(
        app.sessionless_preverify_wait(ApplyMode::Automatic, &ticket)
            .is_some_and(|failed| !failed.is_activity_deferred() && !failed.is_refused()),
        "CONTROL: the automatic lane's failure stands"
    );
    // A `--hold` pane, which the admission refuses to relaunch over, keeps the press's failure.
    let ids: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
    for id in ids {
        app.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_state(id, crate::session_store::SessionState::Exited);
    }
    assert!(
        !app.handoff_exited_session_ids().is_empty(),
        "PRECONDITION: a pane kept open by --hold"
    );
    app.sessionless_waited.person = deadline(&app);
    waited_read_ended(&mut app, ApplyMode::Immediate);
    assert!(
        app.sessionless_preverify_wait(ApplyMode::Immediate, &ticket)
            .is_some_and(|failed| !failed.is_activity_deferred() && !failed.is_refused()),
        "a press over --hold panes is answered, and the next press checks again"
    );
}
