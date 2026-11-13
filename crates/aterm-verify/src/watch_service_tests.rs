// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn persistent_controller_is_independent_of_the_deploying_terminal() {
    let text = manifest(
        Path::new("/state/controllers/123/aterm-verify"),
        Path::new("/state/check & judge"),
        Path::new("/Users//a"),
        "/bin:/custom",
        17,
    );
    // Negative controls: omitting restart or substituting the worker's cwd/executable
    // would lose the lane when that worker exits and removes its worktree.
    assert!(text.contains("<key>KeepAlive</key><true/>"));
    assert!(text.contains("<key>RunAtLoad</key><true/>"));
    assert!(text.contains("<string>/state/controllers/123/aterm-verify</string>"));
    assert!(text.contains("<key>WorkingDirectory</key><string>/state/check &amp; judge</string>"));
    assert!(text.contains("<key>ProcessType</key><string>Interactive</string>"));
    assert!(text.contains("<key>ExitTimeOut</key><integer>60</integer>"));
    assert!(text.contains("<string>17</string>"));
    assert!(text.contains("<key>PATH</key><string>/bin:/custom</string>"));
    assert_eq!(xml("<&\"'>"), "&lt;&amp;&quot;&apos;&gt;");
}

#[test]
fn replacement_waits_for_removal_and_never_bootstraps_over_a_live_job() {
    let mut states = [true, true, false].into_iter();
    let mut waits = 0;
    await_unloaded(
        || Ok(states.next().unwrap()),
        || {
            waits += 1;
            true
        },
    )
    .unwrap();
    assert_eq!(waits, 2);
    assert!(states.next().is_none());
    assert!(await_unloaded(|| Ok(true), || false).is_err());
    assert!(
        await_unloaded(
            || Err("permission refused".into()),
            || panic!("must preserve refusal")
        )
        .is_err()
    );
}
