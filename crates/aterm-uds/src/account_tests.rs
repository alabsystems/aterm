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

const LOGIN_SHELL_CHILD_ENV: &str = "ATERM_UDS_TEST_LOGIN_SHELL_CHILD";

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

#[test]
fn this_account_has_a_name() {
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    assert!(super::user_name(uid).is_some_and(|n| !n.is_empty()));
    assert!(super::user_name(u32::MAX - 1).is_none_or(|n| !n.is_empty()));
}
