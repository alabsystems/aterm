// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::Making;

/// A12: the virtual-machine fact is the probe's answer, read through the injected reading: a
/// probe that answers a guest makes a guest, one that answers bare metal or nothing does not, and
/// a test binary reads no probe.
/// NEGATIVE CONTROL (2026-11-16): `Making::reading` taking `virtual_machine() != Some(false)`
/// (an unanswered probe as a guest): the probe that answers nothing makes a guest (`a probe that
/// answers nothing`).
#[test]
fn the_virtual_machine_fact_is_the_probes_answer() {
    assert!(Making::reading(false, || Some(true), || None).virtual_machine);
    assert!(
        !Making::reading(false, || Some(false), || None).virtual_machine,
        "bare metal"
    );
    assert!(
        !Making::reading(false, || None, || None).virtual_machine,
        "a probe that answers nothing"
    );
    assert!(
        !Making::reading(true, || Some(true), || Some(true)).virtual_machine,
        "a test binary reads no probe"
    );
}

/// A home is foreign where it and the account's home are two folders, and the login's own where
/// they are one, however it is spelled; unresolved, neither.
/// NEGATIVE CONTROL (2026-11-16, Windows): `one_folder` comparing the paths as spelled: `another
/// spelling of the same folder`.
#[test]
fn a_home_is_foreign_only_where_it_is_another_folder() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let (a, b) = (scratch.path().join("a"), scratch.path().join("b"));
    std::fs::create_dir(&a).expect("a");
    std::fs::create_dir(&b).expect("b");
    assert_eq!(Making::foreign_home_of(Some(&a), Some(&b)), Some(true));
    assert_eq!(Making::foreign_home_of(Some(&a), Some(&a)), Some(false));
    let spelled = a.join("..").join("a");
    assert_eq!(
        Making::foreign_home_of(Some(&spelled), Some(&a)),
        Some(false),
        "another spelling of the same folder"
    );
    assert_eq!(Making::foreign_home_of(None, Some(&a)), None);
    assert_eq!(Making::foreign_home_of(Some(&a), None), None);
}

/// Windows: the account's home is the profile folder the shell knows the user by, which no
/// environment variable moves, so a process handed a scratch `USERPROFILE` is a foreign home's
/// and one left its own is not. Each case runs in a child of this test binary, whose
/// environment alone differs.
/// NEGATIVE CONTROL (2026-11-16): `foreign_home_now` reading the account home from
/// `USERPROFILE`, as `home_dir` does: the scratch home reads as the login's own (`a scratch
/// USERPROFILE`).
#[cfg(windows)]
#[test]
fn a_windows_process_handed_a_scratch_profile_is_a_foreign_homes() {
    const CHILD: &str = "ATERM_SECRETS_TEST_FOREIGN_HOME_CHILD";
    if std::env::var_os(CHILD).is_some() {
        println!("foreign home: {:?}", Making::foreign_home_now());
        return;
    }
    let exe = std::env::current_exe().expect("this test binary");
    let scratch = aterm_tempfile::tempdir().expect("scratch home");
    let run = |profile: Option<&std::path::Path>| {
        let mut command = aterm_uds::spawnfd::command(&exe);
        command
            .args([
                "--exact",
                "making::tests::a_windows_process_handed_a_scratch_profile_is_a_foreign_homes",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1");
        if let Some(profile) = profile {
            command.env("USERPROFILE", profile).env("HOME", profile);
        }
        let output =
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                .expect("the child ran");
        assert!(output.status.success(), "{output:?}");
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        out.lines()
            .find_map(|line| {
                line.split_once("foreign home: ")
                    .map(|(_, answer)| answer.to_owned())
            })
            .unwrap_or_else(|| panic!("no answer in {out}"))
    };
    assert_eq!(
        run(Some(scratch.path())),
        "Some(true)",
        "a scratch USERPROFILE"
    );
    assert_eq!(run(None), "Some(false)", "the login's own profile");
}
