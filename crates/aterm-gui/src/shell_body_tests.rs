// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const OWN: &str = "0123456789abcdef";
const OLD: &str = "fedcba9876543210";

/// Every arm of the word, from the facts `status` reads.
#[test]
fn the_status_word_says_which_body_the_shell_runs() {
    use ShellIntegrationPosture::{Degraded, Off, On};
    let word = |rev, posture, frozen| status_word_against(Some((rev, posture)), frozen, OWN);
    assert_eq!(word(Some(OWN), On, false), "current");
    assert_eq!(word(Some(OLD), On, false), format!("stale:{OLD}"));
    assert_eq!(word(Some(OWN), On, true), "current");
    assert_eq!(word(None, On, true), "frozen");
    assert_eq!(word(None, Degraded, true), "frozen");
    assert_eq!(word(None, Off, true), "-", "nothing integrated to freeze");
    assert_eq!(
        word(None, On, false),
        "-",
        "a loader shell before its first prompt"
    );
    assert_eq!(
        status_word_against(None, true, OWN),
        "-",
        "the lock was contended"
    );
}

/// A shell already on this build's revision is not pointed anywhere; any other is
/// pointed only once this build's folder is verified (a failing installer or a folder
/// not named by this build's address points nothing). The pointer is the address and a
/// newline, `0600`.
#[test]
fn an_adopted_shell_is_pointed_at_this_builds_body_unless_it_runs_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("body-pointer")
        .tempdir()
        .expect("scratch");
    let ours = scratch.path().join(own_rev());
    let installed = || Ok(ours.clone());
    let pointer = scratch.path().join("s-ab");
    assert!(pending_with(&pointer, 1, Some(own_rev()), installed).is_none());
    for carried in [None, Some(OLD)] {
        let pending = pending_with(&pointer, 1, carried, installed).expect("pointed");
        assert!(pending.deliver());
        assert_eq!(
            std::fs::read_to_string(&pointer).unwrap(),
            format!("{}\n", own_rev())
        );
        let mode = std::fs::metadata(&pointer).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_file(&pointer).unwrap();
    }
    let failed = || Err(std::io::Error::other("disk full"));
    assert!(pending_with(&pointer, 1, Some(OLD), failed).is_none());
    let foreign = || Ok(scratch.path().join(OLD));
    assert!(pending_with(&pointer, 1, Some(OLD), foreign).is_none());
    assert!(!pointer.exists(), "nothing was pointed");
}
