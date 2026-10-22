// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This account's own home and login shell, from the account database (Open Directory on
//! macOS) rather than `$HOME` and `$SHELL`: a process given a scratch `$HOME` (a test, a lab
//! instance) still learns where the login it runs in keeps its files, which is where a
//! launchd job of that login looks, and which shell its terminals start.

use std::path::PathBuf;

/// This account's home directory as `getpwuid_r(getuid())` gives it; `None` when the
/// account database has no entry for the uid, or names an empty home.
#[must_use]
// Skip: bottoms out at the `getuid` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn home() -> Option<PathBuf> {
    // SAFETY: `getuid` reads this process's real uid and cannot fail.
    home_of(unsafe { libc::getuid() })
}

/// The home directory the account database names for `uid` (`getpwuid_r(uid)->pw_dir`,
/// the directory `sh` expands `~<name>` to for that account's name); `None` when the
/// database has no entry for the uid, or names an empty home.
#[must_use]
// Skip: bottoms out at the `getpwuid_r` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn home_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `pw` and `buf` are stack buffers handed to the reentrant `getpwuid_r`, which
    // writes the entry's strings into `buf` and points `pw`'s fields at them; `pw_dir` is
    // read only when the call succeeded and found an entry, while `buf` is still alive.
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

/// The login name for `uid` from the account database (`getpwuid_r`);
/// `None` when no entry names it. Display use (an `ls -l`-style owner
/// column), never an identity decision.
#[must_use]
// Skip: bottoms out at the `getpwuid_r` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn user_name(uid: u32) -> Option<String> {
    // SAFETY: same contract as `home`: stack buffers handed to the
    // reentrant call, `pw_name` read only on success while `buf` lives.
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

/// This account's login shell as `getpwuid_r(getuid())` gives it (`pw_shell`, the program
/// `login`, `sshd` and Terminal.app start): `None` when the account database has no entry
/// for the uid, or names no shell. Never `$SHELL`, which a test, a wrapper or a parent
/// process may set to anything.
#[must_use]
// Skip: bottoms out at the `getuid` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell() -> Option<PathBuf> {
    // SAFETY: `getuid` reads this process's real uid and cannot fail.
    shell_of(unsafe { libc::getuid() })
}

/// The login shell the account database names for `uid` (`getpwuid_r(uid)->pw_shell`);
/// `None` when the database has no entry for the uid, or names an empty shell.
#[must_use]
// Skip: bottoms out at the `getpwuid_r` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: as in `home_of`: stack buffers handed to the reentrant `getpwuid_r`, and
    // `pw_shell` read only when the call succeeded and found an entry, while `buf` lives.
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

// No `group_name` beside it: the generated aterm-libc subset declares
// no group-database call (`getgrgid_r`), and widening it is an
// aterm-libc-oracle harvest+regen across all four cells — not worth
// doing for a display column. Callers print the numeric gid.

#[cfg(test)]
mod tests {
    #[test]
    fn this_account_has_an_absolute_home() {
        let home = super::home().expect("the account database knows this uid");
        assert!(home.is_absolute(), "{}", home.display());
    }

    /// [`super::home_of`] reads the uid it is given, not this process's: uid 0's home is the
    /// one `sh` expands `~root` to (no `$HOME` to read), and a uid the database has no entry
    /// for has no home.
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

    /// Set in the child [`a_uid_s_login_shell_comes_from_the_account_record`] runs: it
    /// prints [`super::shell`] and returns.
    const LOGIN_SHELL_CHILD_ENV: &str = "ATERM_UDS_TEST_LOGIN_SHELL_CHILD";

    /// [`super::shell`] reads the account record, never `$SHELL`: this test binary, run
    /// again with `$SHELL` naming a shell no record names, prints this uid's record
    /// ([`super::shell_of`]). uid 0's record names an absolute shell, and a uid the
    /// database has no entry for has none.
    #[test]
    fn a_uid_s_login_shell_comes_from_the_account_record() {
        if std::env::var_os(LOGIN_SHELL_CHILD_ENV).is_some() {
            let shell = super::shell().map(|s| s.display().to_string());
            // On a line of its own: the harness has printed the test's name without one.
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
        // An arbitrary id resolves or not, but never panics and
        // never yields an empty name (u32::MAX-1 is `nobody` here).
        assert!(super::user_name(u32::MAX - 1).is_none_or(|n| !n.is_empty()));
    }
}
