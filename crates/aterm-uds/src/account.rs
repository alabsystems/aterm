// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account home directory, login name, and login shell from the account database.

use std::path::PathBuf;

/// Returns this account's home directory from `getpwuid_r(getuid())`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn home() -> Option<PathBuf> {
    // SAFETY: `getuid` reads this process's real uid and cannot fail.
    home_of(unsafe { libc::getuid() })
}

/// Returns the home directory the account database names for `uid`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn home_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `pw` and `buf` are stack buffers handed to the reentrant `getpwuid_r`, which writes
    // the entry's strings into `buf` and points `pw`'s fields at them; `pw_dir` is read only when
    // the call succeeded and found an entry, while `buf` is still alive.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(pw.pw_dir).to_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
}

/// Returns the login name for `uid` from the account database.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn user_name(uid: u32) -> Option<String> {
    // SAFETY: same contract as `home`: stack buffers handed to the reentrant call, `pw_name` read
    // only on success while `buf` lives.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_name.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(pw.pw_name).to_str().ok()?;
        (!name.is_empty()).then(|| name.to_string())
    }
}

/// Returns this account's login shell from `getpwuid_r(getuid())`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell() -> Option<PathBuf> {
    // SAFETY: `getuid` reads this process's real uid and cannot fail.
    shell_of(unsafe { libc::getuid() })
}

/// Returns the login shell the account database names for `uid`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: as in `home_of`: stack buffers handed to the reentrant `getpwuid_r`, and `pw_shell`
    // read only when the call succeeded and found an entry, while `buf` lives.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_shell.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(pw.pw_shell).to_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
}

#[cfg(test)]
mod tests {
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
        let out = crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "account::tests::a_uid_s_login_shell_comes_from_the_account_record",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(LOGIN_SHELL_CHILD_ENV, "1")
            .env("SHELL", decoy)
            .output()
            .expect("run this test binary again as the child");
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
}
