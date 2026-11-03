// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    AdmissionInputs, HandoffLaneFacts, SameImageHandoff, SessionlessFacts, SessionlessLane,
    app_bundle_root, launch_environment_over, out_of_band_lane_refusal, sessionless_lane,
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
        lane: HandoffLaneFacts {
            sessions: 0,
            grant_empty: true,
            ..eligible()
        },
        same_image: false,
        verdict: Some(true),
    }
}

/// The sessionless lane agrees with the lane choice row for row, names a cause the verdict
/// cannot change before it waits for an unread verdict, and lets a failed verdict through to
/// be reported as the attempt's own failure (never as "cannot hand over").
#[test]
fn the_sessionless_lane_is_the_launched_lane_with_windows_to_hand_over() {
    use SessionlessLane::{Cannot, Takes, Unread};
    let base = carried_alone();
    let lane = |edit: fn(&mut HandoffLaneFacts)| {
        let mut facts = base;
        edit(&mut facts.lane);
        facts
    };
    let rows: [(&str, SessionlessFacts, SessionlessLane); 14] = [
        ("carried", base, Takes),
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
        if facts.window && facts.layout && facts.verdict == Some(true) {
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
        "The update installs at the next launch: this process does not run from a .app bundle"
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
