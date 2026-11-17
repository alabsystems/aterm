// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// This process's groups read on a Mac, so whether its account is an administrator is a
/// yes or a no there.
#[cfg(target_os = "macos")]
#[test]
fn whether_this_account_administers_the_mac_reads() {
    assert!(super::administrator().is_some());
}

#[test]
fn this_account_has_an_absolute_home() {
    let home = super::home().expect("the account database knows this uid");
    assert!(home.is_absolute(), "{}", home.display());
}

#[cfg(unix)]
#[test]
fn a_uid_s_home_is_the_one_the_shell_expands_its_name_to() {
    let out = crate::spawnfd::command("sh")
        .args(["-c", "printf '%s' ~root"])
        .env_remove("HOME")
        .output()
        .expect("run sh");
    assert!(out.status.success(), "sh: {out:?}");
    let root = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        root.starts_with('/'),
        "the shell expanded ~root to {root:?}"
    );
    assert_eq!(super::home_of(0), Some(std::path::PathBuf::from(root)));
    assert_eq!(super::home_of(2_000_000_123), None, "no such uid");
}

#[cfg(unix)]
const LOGIN_SHELL_CHILD_ENV: &str = "ATERM_UDS_TEST_LOGIN_SHELL_CHILD";

#[cfg(unix)]
#[test]
fn a_uid_s_login_shell_comes_from_the_account_record() {
    if std::env::var_os(LOGIN_SHELL_CHILD_ENV).is_some() {
        let shell = super::shell().map(|s| s.display().to_string());
        println!("\nlogin-shell={}", shell.unwrap_or_default());
        return;
    }
    let root = super::shell_of(0).expect("the account database names root's shell");
    assert!(root.is_absolute(), "{}", root.display());
    assert_eq!(super::shell_of(2_000_000_123), None, "no such uid");
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    let record = super::shell_of(uid).map(|s| s.display().to_string());
    let decoy = "/nonexistent/aterm-uds-decoy-shell";
    let out = crate::deadline::ends_within(
        crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "account::tests::a_uid_s_login_shell_comes_from_the_account_record",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(LOGIN_SHELL_CHILD_ENV, "1")
            .env("SHELL", decoy),
        crate::deadline::ECHO_HANG,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    let said = stdout
        .lines()
        .find_map(|line| line.strip_prefix("login-shell="))
        .unwrap_or_else(|| panic!("the child printed no shell: {stdout}"));
    assert_eq!(said, record.unwrap_or_default(), "with $SHELL={decoy}");
}

#[cfg(unix)]
#[test]
fn this_account_has_a_name() {
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    assert!(super::user_name(uid).is_some_and(|n| !n.is_empty()));
    assert!(super::user_name(u32::MAX - 1).is_none_or(|n| !n.is_empty()));
}

#[cfg(windows)]
const PROFILE_CHILD_ENV: &str = "ATERM_UDS_TEST_PROFILE_CHILD";

/// The account home is the user's profile folder, whatever `USERPROFILE` and `HOME` say: the test
/// binary run again with both set to a scratch folder reads the folder this run reads, never the
/// scratch one, so a process handed a scratch home tells it from its login's own.
/// NEGATIVE CONTROL (2026-11-16): `home()` reading `USERPROFILE` (as `aterm_types::dirs::home_dir`
/// does): the child reads the scratch folder (`with USERPROFILE=…`).
#[cfg(windows)]
#[test]
fn the_account_home_is_the_profile_whatever_userprofile_says() {
    if std::env::var_os(PROFILE_CHILD_ENV).is_some() {
        let home = super::home().map(|h| h.display().to_string());
        println!("\naccount-home={}", home.unwrap_or_default());
        return;
    }
    let home = super::home().expect("this user's profile folder");
    assert!(home.is_dir(), "{}", home.display());
    let scratch = aterm_tempfile::tempdir().expect("a scratch home");
    let mut again = crate::spawnfd::command(std::env::current_exe().expect("the test binary"));
    again
        .args([
            "account::tests::the_account_home_is_the_profile_whatever_userprofile_says",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROFILE_CHILD_ENV, "1")
        .env("USERPROFILE", scratch.path())
        .env("HOME", scratch.path());
    let out = crate::deadline::output_within(&mut again, aterm_spec::wait::MUST_HAPPEN)
        .expect("the child answers within the hang detector");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    let said = stdout
        .lines()
        .find_map(|line| line.split_once("account-home=").map(|(_, home)| home))
        .unwrap_or_else(|| panic!("the child printed no home: {stdout}"));
    assert_eq!(
        said,
        home.display().to_string(),
        "with USERPROFILE={}",
        scratch.path().display()
    );
}
