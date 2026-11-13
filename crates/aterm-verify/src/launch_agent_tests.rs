// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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

#[test]
fn the_agents_list_is_the_users_own() {
    assert_eq!(
        plist(Path::new("/Users//a"), "systems.alab.x"),
        Path::new("/Users//a/Library/LaunchAgents/systems.alab.x.plist")
    );
}

/// AN AGENT ASKED TO GO DOES NOT COME BACK AT THE NEXT LOGIN. A job that is still ending when
/// the wait gives up (a pump waiting for its release to reach a boundary) leaves no property
/// list behind, and the caller is told the job is still exiting.
/// NEGATIVE CONTROL: unloading first and returning on its error keeps the list.
#[test]
fn removal_takes_the_list_even_when_the_job_outlives_the_wait() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let list = scratch.path().join("systems.alab.x.plist");
    fs::write(&list, "<plist/>").unwrap();
    let still_loaded = || await_unloaded(|| Ok(true), || false);
    assert!(matches!(
        remove_list(&list, still_loaded),
        Ok(Removed::Exiting(why)) if why.contains("did not unload")
    ));
    assert!(!list.exists(), "the list is gone although the job is not");

    // A job that unloads in time, with and without a list.
    fs::write(&list, "<plist/>").unwrap();
    assert_eq!(remove_list(&list, || Ok(())), Ok(Removed::Unloaded));
    assert!(!list.exists());
    assert_eq!(remove_list(&list, || Ok(())), Ok(Removed::Absent));

    // A list that cannot be removed is the error, and the job is left alone.
    let not_a_file = scratch.path().join("dir.plist");
    fs::create_dir(&not_a_file).unwrap();
    assert!(remove_list(&not_a_file, || panic!("the job is not asked to unload")).is_err());
}
