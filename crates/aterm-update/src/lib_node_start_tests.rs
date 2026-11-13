// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{newer_build_for_a_start_in, newer_for_a_start, older_than, staged_for_a_start};

/// AN OLDER INSTALLED BUILD IS ONLY ONE BELOW THIS ONE: the build a revert put back, never
/// this build or a newer one, which [`newer_for_a_start`] judges.
#[test]
fn only_a_build_below_this_one_is_an_older_installed_build() {
    assert_eq!(older_than(100, Some(99)), Some(99), "a revert put 99 back");
    assert_eq!(older_than(100, Some(100)), None, "this build stands here");
    assert_eq!(
        older_than(100, Some(101)),
        None,
        "a newer build stands here"
    );
    assert_eq!(
        older_than(100, None),
        None,
        "the installed build cannot be read"
    );
}

/// A NODE START RUNS NO YANKED INSTALLED BUILD: the installed arm counts only once the floor
/// reads that it leaves the build, while a staged build above this one counts as it did. The
/// control is the same installed build with a floor that leaves it.
#[test]
fn a_yanked_or_unjudged_installed_build_is_no_newer_build_for_a_start() {
    let leaves = |_: u64| Ok(false);
    let yanks = |build: u64| Ok(build < 102);
    let unread = |_: u64| Err("the floor is unreadable".to_owned());
    assert_eq!(
        newer_for_a_start(100, None, Some(101), leaves),
        Some(101),
        "CONTROL: a window installed 101 over this image and the floor leaves it"
    );
    assert_eq!(
        newer_for_a_start(100, None, Some(101), yanks),
        None,
        "the floor yanks 101: a start would run a build a window refuses to activate"
    );
    assert_eq!(
        newer_for_a_start(100, None, Some(101), unread),
        None,
        "a floor that cannot be read does not clear the installed build"
    );
    assert_eq!(
        newer_for_a_start(100, Some(102), Some(101), yanks),
        Some(102),
        "a staged build above this one still counts beside a yanked installed one"
    );
    assert_eq!(
        newer_for_a_start(100, Some(100), Some(99), leaves),
        None,
        "nothing above this build"
    );
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Build `build` (digest `ab`×32) staged and published in `s`, as a check leaves it.
fn publish(s: &crate::paths::Staging, build: u64) {
    let commit = COMMIT;
    let ready = crate::manifest::Ready {
        build_number: build,
        version: format!("0.0.{build}"),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&s.ready, ready.to_toml().unwrap()).unwrap();
    let contents = s.staged_app.join("Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
}

/// A START NEVER RUNS HELD BYTES: the staged build a node start would install is none while its
/// exact bytes are on the window-start failure record or that record cannot be read, and is the
/// stage again for other bytes on record and once the record is withdrawn. THE INSTALLED ARM is
/// deliberately not filtered: with the installed receipt naming the very bytes on record, the
/// installed build above this one still answers, so the job restarts for it and its trial gets
/// the headless settle that keeps its window re-trial (an armed trial left unsettled blocks every
/// admission).
/// NEGATIVE CONTROL: a filter that reads `Err` as absent answers `Some(11)` for the unreadable
/// record; one keyed by build alone answers `None` for the other artifact; filtering the
/// installed arm by the record answers `None` for THE INSTALLED ARM.
#[test]
fn a_start_never_runs_held_bytes() {
    let s = crate::paths::Staging::scratch("node-start-held");
    publish(&s, 11);
    let (ab, cd) = ("ab".repeat(32), "cd".repeat(32));
    let fail = |digest: &str| {
        crate::window_start_failure::record(
            &s,
            10,
            crate::FailedStartBytes::Download {
                build: 11,
                dmg_sha256: digest,
            },
            "unexplained/stalled",
            crate::install::unix_now_secs(),
        )
        .unwrap();
    };
    assert_eq!(
        staged_for_a_start(&s),
        Some(11),
        "CONTROL: nothing on record"
    );
    fail(&ab);
    assert_eq!(
        staged_for_a_start(&s),
        None,
        "the stage's own bytes are held"
    );
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        11,
        COMMIT,
        &ab,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
    assert_eq!(
        newer_build_for_a_start_in(Some(&s), 10, || Ok(11)),
        Some(11),
        "THE INSTALLED ARM: installed bytes on record still answer for a start"
    );
    assert_eq!(
        newer_build_for_a_start_in(Some(&s), 10, || Err("no Info.plist".into())),
        None,
        "CONTROL: the held stage is no build a start would run"
    );
    fail(&cd);
    assert_eq!(
        staged_for_a_start(&s),
        Some(11),
        "another artifact of 11 on record"
    );
    fail(&ab);
    assert_eq!(
        crate::window_start_failure::withdraw(
            &s,
            crate::FailedStartBytes::Download {
                build: 11,
                dmg_sha256: &ab,
            }
        ),
        Ok(true)
    );
    assert_eq!(staged_for_a_start(&s), Some(11), "withdrawn");
    std::fs::remove_file(s.record()).unwrap();
    std::os::unix::fs::symlink(s.record(), s.record()).unwrap();
    assert_eq!(staged_for_a_start(&s), None, "a record that cannot be read");
}
