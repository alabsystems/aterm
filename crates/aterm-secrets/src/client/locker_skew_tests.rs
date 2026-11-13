// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_release_number_reads_without_its_build_metadata() {
    assert_eq!(version("0.140.0"), Some((0, 140, 0)));
    assert_eq!(version("0.141.0+g8a5d5c1510"), Some((0, 141, 0)));
    assert_eq!(version("1.2.3-rc1"), Some((1, 2, 3)));
    for bad in ["", "0.140", "0.140.0.1", "0.x.0", "v0.140.0"] {
        assert_eq!(version(bad), None, "{bad}");
    }
    let answer = "aterm 0.125.0\nby Andrew Yates · ALab\nbuild: 1\n";
    assert_eq!(version_answer(answer), Some((0, 125, 0)));
    assert_eq!(version_answer("atermx 0.125.0\n"), None);
    assert_eq!(version_answer(""), None);
}

const FROM: &str = "alab-vault-admin/1";
const TO: &str = "alab-vault-admin/3";
/// A format this build would introduce, which no release before it reads.
const NEWEST: &str = "alab-vault-admin/9";
const HERE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const SNAPSHOT: &str = "/Users//a/.local/share/aterm-secrets/aterm";

fn own(release: bool) -> Own {
    Own {
        version: (0, 141, 0),
        release,
        program: Some(PathBuf::from(HERE)),
    }
}

fn running(pid: Option<u32>, program: &str, version: Result<Version, String>) -> Running {
    Running {
        pid,
        program: PathBuf::from(program),
        version,
    }
}

#[test]
fn an_older_approver_refuses_the_rewrite_and_names_the_program_to_use() {
    let old = running(Some(8000), SNAPSHOT, Ok((0, 125, 0)));
    let refused = judge(FROM, TO, &own(true), &old)
        .expect_err("an older approver refuses")
        .to_string();
    for part in [
        "from alab-vault-admin/1 to alab-vault-admin/3",
        "this aterm (0.141.0)",
        "(pid 8000, /Users//a/.local/share/aterm-secrets/aterm, aterm 0.125.0)",
        "lets no machine join",
        "(/Users//a/.local/share/aterm-secrets/aterm secrets …)",
    ] {
        assert!(refused.contains(part), "{part}: {refused}");
    }
    let next = running(None, SNAPSHOT, Ok((0, 125, 0)));
    let refused = judge(FROM, TO, &own(true), &next)
        .expect_err("a job that would start an older program refuses")
        .to_string();
    assert!(
        refused.contains("(the program its job starts next, "),
        "{refused}"
    );
    let unknown = running(Some(8000), SNAPSHOT, Err("timed out".into()));
    let refused = judge(FROM, TO, &own(true), &unknown)
        .expect_err("an approver of unknown release refuses")
        .to_string();
    assert!(
        refused.contains("release is unknown: timed out"),
        "{refused}"
    );
}

#[test]
fn this_program_or_a_release_that_has_this_one_lets_the_rewrite_through() {
    let same = running(Some(1), HERE, Err("never asked".into()));
    assert_eq!(judge(FROM, TO, &own(false), &same), Ok(()));
    for version in [(0, 142, 0), (1, 0, 0)] {
        let newer = running(Some(1), "/opt/aterm", Ok(version));
        assert_eq!(judge(FROM, TO, &own(false), &newer), Ok(()));
    }
    // A format a release before this build introduced passes any release that reads it.
    let reads = running(Some(1), "/opt/aterm", Ok((0, 140, 9)));
    assert_eq!(judge(FROM, TO, &own(false), &reads), Ok(()));
    assert_eq!(
        judge(
            FROM,
            TO,
            &own(false),
            &running(Some(1), "/o", Ok((0, 138, 0)))
        ),
        Ok(())
    );
    let before = running(Some(1), "/opt/aterm", Ok((0, 137, 9)));
    assert!(judge(FROM, TO, &own(true), &before).is_err());
    // A format this build introduces: the same number is as new only when this build is
    // that release, and an older one never is.
    let equal = running(Some(1), "/opt/aterm", Ok((0, 141, 0)));
    assert_eq!(judge(FROM, NEWEST, &own(true), &equal), Ok(()));
    let refused = judge(FROM, NEWEST, &own(false), &equal)
        .expect_err("a development build may write past its own number's release")
        .to_string();
    assert!(
        refused.contains("a release this development build may write past"),
        "{refused}"
    );
    assert!(judge(FROM, NEWEST, &own(true), &reads).is_err());
    let nowhere = Own {
        program: None,
        ..own(true)
    };
    assert!(judge(FROM, NEWEST, &nowhere, &reads).is_err());
}

#[test]
fn a_subscriber_about_to_restart_into_a_newer_program_says_to_publish_again() {
    let old = running(
        Some(8000),
        "/Applications/aterm.app.old/Contents/MacOS/aterm",
        Ok((0, 137, 0)),
    );
    let next = running(None, HERE, Ok((0, 141, 0)));
    let refused = verdict(FROM, TO, &own(true), &[old.clone(), next])
        .expect_err("the running program is still older")
        .to_string();
    assert!(
        refused.starts_with("refusing to publish for now:"),
        "{refused}"
    );
    assert!(
        refused.contains("(pid 8000) still runs /Applications/aterm.app.old"),
        "{refused}"
    );
    assert!(
        refused.contains("publish again once it has restarted"),
        "{refused}"
    );
    // CONTROL: a job that starts the old program again gets the plain refusal.
    let stale = running(None, SNAPSHOT, Ok((0, 125, 0)));
    let refused = verdict(FROM, TO, &own(true), &[old, stale])
        .expect_err("both are older")
        .to_string();
    assert!(refused.starts_with("refusing to publish: "), "{refused}");
    assert!(
        refused.contains("publish with that program instead"),
        "{refused}"
    );
    let fine = running(Some(1), HERE, Ok((0, 141, 0)));
    assert_eq!(verdict(FROM, TO, &own(true), &[fine]), Ok(()));
    assert_eq!(verdict(FROM, TO, &own(true), &[]), Ok(()));
}

#[test]
fn every_build_reads_version_one_and_a_format_this_build_does_not_know_needs_it() {
    assert_eq!(first_reader("alab-vault-admin/1", (0, 141, 0)), None);
    assert_eq!(
        first_reader("alab-vault-admin/2", (0, 141, 0)),
        Some((0, 138, 0))
    );
    assert_eq!(
        first_reader("alab-vault-admin/3", (0, 141, 0)),
        Some((0, 138, 0))
    );
    assert_eq!(
        first_reader("alab-vault-admin/9", (0, 141, 0)),
        Some((0, 141, 0))
    );
}

#[test]
fn the_programs_checked_are_the_running_one_and_the_one_its_job_starts_next() {
    let unit = PathBuf::from("/u/systems.alab.aterm-secrets.plist");
    let installed = |pid| {
        Ok(service::State::Installed {
            pid,
            unit: unit.clone(),
        })
    };
    let at = |path: &str| Some(PathBuf::from(path));
    let snapshot = |_: &Path| at(SNAPSHOT);
    let nothing = |_: &Path| None;
    assert_eq!(
        programs(Ok(service::State::Absent), &|_| at(HERE), &snapshot),
        Ok(Vec::new())
    );
    assert_eq!(
        programs(installed(Some(8000)), &|_| at(SNAPSHOT), &snapshot),
        Ok(vec![(Some(8000), PathBuf::from(SNAPSHOT))])
    );
    assert_eq!(
        programs(installed(Some(8000)), &|_| at(HERE), &snapshot),
        Ok(vec![
            (Some(8000), PathBuf::from(HERE)),
            (None, PathBuf::from(SNAPSHOT))
        ])
    );
    // Between two runs the unit names what launchd starts next.
    assert_eq!(
        programs(installed(None), &|_| at(HERE), &snapshot),
        Ok(vec![(None, PathBuf::from(SNAPSHOT))])
    );
    assert_eq!(
        programs(installed(Some(8000)), &|_| None, &snapshot),
        Ok(vec![(None, PathBuf::from(SNAPSHOT))])
    );
    assert_eq!(
        programs(installed(Some(8000)), &|_| at(SNAPSHOT), &nothing),
        Ok(vec![(Some(8000), PathBuf::from(SNAPSHOT))])
    );
    // Nothing nameable, or an unreadable state, refuses rather than passes.
    let refused = programs(installed(None), &|_| None, &nothing)
        .expect_err("an installed job of no nameable program refuses");
    assert!(
        refused.contains("systems.alab.aterm-secrets.plist starts cannot be named"),
        "{refused}"
    );
    let refused = programs(Err(Error::new("launchctl hung")), &|_| at(HERE), &snapshot)
        .expect_err("an unknown state refuses");
    assert!(
        refused.contains("runs is unknown (launchctl hung)"),
        "{refused}"
    );
}
