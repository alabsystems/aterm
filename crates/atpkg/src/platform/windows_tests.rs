// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// THE ACCOUNT'S PROFILE, MEASURED ON THE MACHINE RUNNING THE TEST: the token's
/// profile directory resolves, is absolute and exists, and is a loaded profile (its
/// `NTUSER.DAT` hive is there: a scratch `USERPROFILE` has none), the same twice.
#[test]
fn the_account_home_resolves_on_the_machine_running_this_test() {
    let account =
        account_home().expect("GetUserProfileDirectoryW must resolve this account's profile");
    assert!(account.is_absolute(), "{}", account.display());
    assert!(
        account.is_dir(),
        "the profile exists: {}",
        account.display()
    );
    assert!(
        std::fs::symlink_metadata(account.join("NTUSER.DAT")).is_ok(),
        "a loaded profile holds its hive: {}",
        account.display()
    );
    assert_eq!(account_home(), Some(account));
}

/// The scratch profile [`probe_the_account_home_under_a_scratch_profile_if_asked`] runs under,
/// set only in the child of this test binary that runs it.
const SCRATCH_PROFILE_ENV: &str = "ATPKG_TEST_SCRATCH_PROFILE";
/// That probe's name, as `--exact` wants it.
const SCRATCH_PROFILE_PROBE: &str =
    "platform::windows::tests::the_account_home_is_not_the_userprofile_a_process_is_given";

/// THE ACCOUNT'S PROFILE IS NOT `%USERPROFILE%`: a process whose `USERPROFILE` and
/// `HOME` name a scratch folder (a lab instance, a worker) still answers the token's
/// own profile, with its hive.
#[test]
fn the_account_home_is_not_the_userprofile_a_process_is_given() {
    if probe_the_account_home_under_a_scratch_profile_if_asked() {
        return;
    }
    let scratch = crate::testdir::dir("atpkg-scratch-profile-");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "--nocapture", SCRATCH_PROFILE_PROBE])
        .env(SCRATCH_PROFILE_ENV, scratch.as_os_str())
        .env("USERPROFILE", scratch.as_os_str())
        .env("HOME", scratch.as_os_str())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("run the probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the account home under a scratch USERPROFILE: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// [`the_account_home_is_not_the_userprofile_a_process_is_given`]'s child: under the
/// scratch profile its parent named, the account's home is another folder, and a
/// loaded profile.
fn probe_the_account_home_under_a_scratch_profile_if_asked() -> bool {
    let Some(scratch) = std::env::var_os(SCRATCH_PROFILE_ENV) else {
        return false;
    };
    let scratch = PathBuf::from(scratch);
    assert_eq!(
        std::env::var_os("USERPROFILE").map(PathBuf::from).as_ref(),
        Some(&scratch),
        "the probe runs under the scratch profile"
    );
    let account = account_home().expect("the token's profile resolves");
    let canon = |p: &Path| fs::canonicalize(p).expect("there");
    assert_ne!(canon(&account), canon(&scratch), "not USERPROFILE");
    assert!(
        fs::symlink_metadata(account.join("NTUSER.DAT")).is_ok(),
        "a loaded profile holds its hive: {}",
        account.display()
    );
    assert!(fs::symlink_metadata(scratch.join("NTUSER.DAT")).is_err());
    true
}
