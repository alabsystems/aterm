// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{HandoffLaneFacts, app_bundle_root, launch_environment_over, out_of_band_lane_refusal};

/// An attempt with nothing wrong with it.
fn eligible() -> HandoffLaneFacts {
    HandoffLaneFacts {
        bundled: true,
        launcher_available: true,
        socket_path_fits: true,
        target_not_older: true,
        sessions: 3,
        grant_chunks: false,
        environment_is_a_merge: true,
    }
}

/// One field of [`HandoffLaneFacts`] flipped per row against an eligible attempt,
/// each with whether it must refuse the lane.
#[test]
fn each_fact_alone_decides_the_out_of_band_lane() {
    let limit = crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS;
    let chunked = crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS;
    let rows: [(&str, HandoffLaneFacts, bool); 13] = [
        // An attempt with nothing wrong with it takes the lane.
        ("eligible", eligible(), false),
        // A platform with no LaunchServices launcher has nothing to start.
        (
            "no launcher",
            HandoffLaneFacts {
                launcher_available: false,
                ..eligible()
            },
            true,
        ),
        // A dev build, `targo run` and the test harness reach the seamless lane
        // without a bundle, so LaunchServices has nothing to start.
        (
            "unbundled",
            HandoffLaneFacts {
                bundled: false,
                ..eligible()
            },
            true,
        ),
        // `$HOME` decides whether the rendezvous path fits `sun_path`; when it
        // does not, the fallback must be silent and total, not a bind failing
        // after the park.
        (
            "rendezvous path too long",
            HandoffLaneFacts {
                socket_path_fits: false,
                ..eligible()
            },
            true,
        ),
        // The downgrade guard: an older authorized target must fork, or a
        // successor without rendezvous code is handed descriptors it cannot
        // receive and every session is lost.
        (
            "older target",
            HandoffLaneFacts {
                target_not_older: false,
                ..eligible()
            },
            true,
        ),
        // The transport bounds the pool: one `SCM_RIGHTS` message carries 64
        // descriptors, two of them the pipes. Exactly the limit fits…
        (
            "sessions == limit",
            HandoffLaneFacts {
                sessions: limit,
                ..eligible()
            },
            false,
        ),
        // …one more does not, and must fall back rather than fail at sendmsg…
        (
            "sessions == limit + 1",
            HandoffLaneFacts {
                sessions: limit + 1,
                ..eligible()
            },
            true,
        ),
        // The chunked grant: a candidate whose verified `Info.plist` declares it
        // takes 100 sessions over several messages…
        (
            "100 sessions, chunked grant declared",
            HandoffLaneFacts {
                sessions: 100,
                grant_chunks: true,
                ..eligible()
            },
            false,
        ),
        // …and a candidate that did not declare it still forks them.
        (
            "100 sessions, no chunked grant",
            HandoffLaneFacts {
                sessions: 100,
                ..eligible()
            },
            true,
        ),
        // The protocol's own ceiling still binds a chunked grant.
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
        // …and an overlap with no sessions is not an overlap.
        (
            "no sessions",
            HandoffLaneFacts {
                sessions: 0,
                ..eligible()
            },
            true,
        ),
        // A LaunchServices launch merges its environment and cannot remove a
        // variable; `bind_expected_update_artifact` removes three, so a removal
        // that would remove something disqualifies the lane.
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

/// The predicate behind that fact, against a real `Command`: removing something
/// the launcher lacks is a no-op (the common case), and removing something it
/// has cannot be expressed.
#[test]
fn only_a_removal_that_would_remove_something_disqualifies_the_launch() {
    // std on purpose: never spawned; the shipped `launch_environment` reads a std one.
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

/// the successor's control directory is the launcher's
/// by construction. The variables it is computed from travel explicitly from the
/// launcher's own environment, whatever LaunchServices merges them over; one the
/// `Command` sets wins, and one the launcher lacks is not invented.
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

/// The bundle root is `<bundle>.app/Contents/MacOS/<bin>`, and the `.app` suffix is checked:
/// a dev binary also sits three levels below something.
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
