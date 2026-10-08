// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{HandoffLaneFacts, app_bundle_root, launch_environment, out_of_band_lane_refusal};

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
/// each with the refusal it must produce.
#[test]
fn each_fact_alone_decides_the_out_of_band_lane() {
    let limit = crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS;
    let chunked = crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS;
    let rows: [(&str, HandoffLaneFacts, Option<&str>); 13] = [
        // An attempt with nothing wrong with it takes the lane.
        ("eligible", eligible(), None),
        // A platform with no LaunchServices launcher has nothing to start.
        (
            "no launcher",
            HandoffLaneFacts {
                launcher_available: false,
                ..eligible()
            },
            Some("this platform has no LaunchServices launcher"),
        ),
        // A dev build, `cargo run` and the test harness reach the seamless lane
        // without a bundle, so LaunchServices has nothing to start.
        (
            "unbundled",
            HandoffLaneFacts {
                bundled: false,
                ..eligible()
            },
            Some("this process does not run from a .app bundle"),
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
            Some("the rendezvous path does not fit sun_path on this machine"),
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
            Some("the authorized target build is older than this build"),
        ),
        // The transport bounds the pool: one `SCM_RIGHTS` message carries 64
        // descriptors, two of them the pipes. Exactly the limit fits…
        (
            "sessions == limit",
            HandoffLaneFacts {
                sessions: limit,
                ..eligible()
            },
            None,
        ),
        // …one more does not, and must fall back rather than fail at sendmsg…
        (
            "sessions == limit + 1",
            HandoffLaneFacts {
                sessions: limit + 1,
                ..eligible()
            },
            Some("the session count does not fit one descriptor message"),
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
            None,
        ),
        // …and a candidate that did not declare it still forks them.
        (
            "100 sessions, no chunked grant",
            HandoffLaneFacts {
                sessions: 100,
                ..eligible()
            },
            Some("the session count does not fit one descriptor message"),
        ),
        // The protocol's own ceiling still binds a chunked grant.
        (
            "chunked grant at the protocol ceiling",
            HandoffLaneFacts {
                sessions: chunked,
                grant_chunks: true,
                ..eligible()
            },
            None,
        ),
        (
            "chunked grant past the protocol ceiling",
            HandoffLaneFacts {
                sessions: chunked + 1,
                grant_chunks: true,
                ..eligible()
            },
            Some("the session count does not fit the successor's chunked descriptor grant"),
        ),
        // …and an overlap with no sessions is not an overlap.
        (
            "no sessions",
            HandoffLaneFacts {
                sessions: 0,
                ..eligible()
            },
            Some("the session count does not fit one descriptor message"),
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
            Some("the launch environment needs a removal a merge cannot express"),
        ),
    ];
    for (label, facts, refusal) in rows {
        assert_eq!(out_of_band_lane_refusal(facts), refusal, "{label}");
    }
}

/// The predicate behind that fact, against a real `Command`: removing something
/// this process lacks is a no-op (the common case), and removing something it
/// has cannot be expressed.
#[test]
fn only_a_removal_that_would_remove_something_disqualifies_the_launch() {
    // std on purpose: never spawned; the shipped `launch_environment` reads a std one.
    let mut command = std::process::Command::new("/bin/echo");
    command.env("ATERM_LANE_TEST_KEY", "value");
    command.env_remove("ATERM_LANE_TEST_ABSENT_KEY_THAT_IS_NOT_SET");
    let carried = launch_environment(&command).expect("a no-op removal is expressible");
    assert_eq!(
        carried,
        vec![(
            std::ffi::OsString::from("ATERM_LANE_TEST_KEY"),
            std::ffi::OsString::from("value")
        )],
        "only real assignments travel; a vacuous removal contributes nothing"
    );

    // The removal now names a variable this process carries, set through the
    // lock-scoped env helper so no concurrent test observes it.
    crate::test_env::scoped("ATERM_LANE_TEST_PRESENT_KEY", "set", || {
        // std on purpose: never spawned; read by the shipped `launch_environment`.
        let mut command = std::process::Command::new("/bin/echo");
        command.env_remove("ATERM_LANE_TEST_PRESENT_KEY");
        assert!(
            launch_environment(&command).is_none(),
            "a merge cannot un-set a variable the launcher's own environment has"
        );
    });
}

/// The bundle root is `<bundle>.app/Contents/MacOS/<bin>` and the `.app`
/// suffix is CHECKED — a dev binary also sits three levels below something.
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
