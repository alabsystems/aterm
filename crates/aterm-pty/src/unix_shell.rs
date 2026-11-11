// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH PROGRAM a Unix session's shell is.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

/// Where the shell [`select_shell_with_origin`] chose came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOrigin {
    /// `--shell`, or `shell` in aterm.toml.
    Override,
    /// `$SHELL`.
    Env,
    /// The account's login shell in the password database: `$SHELL` is unset or empty.
    Passwd,
    /// `/bin/sh`: `$SHELL` names nothing.
    Default,
}

impl ShellOrigin {
    /// Where a shell that cannot start was set, as the end of the sentence that says so.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Override => " (set by --shell or `shell` in aterm.toml)",
            Self::Env => " (from $SHELL)",
            Self::Passwd => " (your login shell in the password database)",
            Self::Default => "",
        }
    }

    /// The one word a report prints for this arm: the variable or file that chose the shell.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Env => "SHELL",
            Self::Passwd => "passwd",
            Self::Default => "/bin/sh",
        }
    }
}

/// The program a Unix session's interactive shell runs, and where it came from.
#[must_use]
pub fn select_shell_with_origin(override_shell: Option<&OsStr>) -> (OsString, ShellOrigin) {
    select_shell_in(
        override_shell,
        std::env::var_os("SHELL"),
        std::env::var_os("PATH").as_deref(),
        account_shell,
    )
}

/// The shell the account logs in with, for a session whose `$SHELL` names none.
#[must_use]
pub fn login_shell() -> (OsString, ShellOrigin) {
    login_shell_from(account_shell)
}

/// The `SHELL` a child is handed when aterm's own environment names none (unset or empty).
#[must_use]
pub fn shell_env_for_child() -> Option<OsString> {
    names_a_shell(std::env::var_os("SHELL"))
        .is_none()
        .then(|| login_shell().0)
}

/// Why `program` cannot be a session's shell.
#[must_use]
pub fn shell_cannot_start(program: &OsStr, origin: ShellOrigin) -> Option<String> {
    let Ok(target) = CString::new(program.as_bytes()) else {
        return Some(format!(
            "the shell{} holds a NUL byte, which no program name can",
            origin.suffix()
        ));
    };
    if program.as_bytes().contains(&b'/') && runnable(Path::new(program)) {
        return None;
    }
    Some(crate::unix::exec_failure(
        &target,
        origin.suffix(),
        false,
        None,
    ))
}

/// The shell to start in place of `failed`, which cannot start ([`shell_cannot_start`]).
#[must_use]
pub fn fallback_shell(failed: &OsStr) -> Option<(OsString, ShellOrigin)> {
    fallback_shell_from(failed, account_shell)
}

fn select_shell_in(
    override_shell: Option<&OsStr>,
    env_shell: Option<OsString>,
    path_var: Option<&OsStr>,
    account: impl FnOnce() -> Option<OsString>,
) -> (OsString, ShellOrigin) {
    if let Some(name) = override_shell.filter(|s| !s.is_empty()) {
        return (on_path(name, path_var), ShellOrigin::Override);
    }
    if let Some(name) = names_a_shell(env_shell) {
        return (on_path(&name, path_var), ShellOrigin::Env);
    }
    login_shell_from(account)
}

fn login_shell_from(account: impl FnOnce() -> Option<OsString>) -> (OsString, ShellOrigin) {
    match account() {
        Some(shell) => (shell, ShellOrigin::Passwd),
        None => (OsString::from("/bin/sh"), ShellOrigin::Default),
    }
}

fn fallback_shell_from(
    failed: &OsStr,
    account: impl FnOnce() -> Option<OsString>,
) -> Option<(OsString, ShellOrigin)> {
    let account = account().map(|shell| (shell, ShellOrigin::Passwd));
    let sh = Some((OsString::from("/bin/sh"), ShellOrigin::Default));
    [account, sh]
        .into_iter()
        .flatten()
        .find(|(shell, _)| shell != failed && runnable(Path::new(shell)))
}

fn names_a_shell(value: Option<OsString>) -> Option<OsString> {
    value.filter(|v| !v.is_empty())
}

fn on_path(name: &OsStr, path_var: Option<&OsStr>) -> OsString {
    if name.as_bytes().contains(&0) {
        return name.to_owned();
    }
    find_on_path(name, path_var).map_or_else(|| name.to_owned(), PathBuf::into_os_string)
}

/// The first `PATH` entry holding a program named `name` this user can run, or `None`.
pub(crate) fn find_on_path(name: &OsStr, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return None;
    }
    for dir in std::env::split_paths(path_var?) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if runnable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// A regular file this user may execute (`access` and a regular file: something `execve` can run).
pub(crate) fn runnable(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path string that outlives the call;
    // `access` only reads it.
    let executable = unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0;
    executable && path.is_file()
}

/// The account's login shell from the password database (`pw_shell` of
/// `getpwuid_r(getuid())`) when it is an absolute path to a program this user can
/// run; `None` when the lookup fails, the entry is empty (which passwd(5) reads as
/// `/bin/sh`, the caller's own fallback) or names nothing runnable (`nologin` is
/// runnable and is returned: an account that may not log in gets what it asked for).
#[cfg_attr(trust_verify, trust::skip)]
fn account_shell() -> Option<OsString> {
    // SAFETY: `passwd` is plain integers and pointers, for which all-zero is a
    // valid value; it is only an out-param here.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    let mut len = 4096;
    let shell = loop {
        let mut buf = vec![0u8; len];
        // SAFETY: `pw` is a valid, writable passwd out-param; `buf` is a valid
        // writable buffer of the stated length that outlives every read of `pw`'s
        // string fields, which point INTO it on success; `out` is a valid
        // out-pointer. getpwuid_r writes nothing past buflen. `pw_shell` is read
        // only when `out` is non-null (success), and is then a NUL-terminated C
        // string inside `buf`, copied out before `buf` goes out of scope.
        unsafe {
            let rc = libc::getpwuid_r(
                libc::getuid(),
                &raw mut pw,
                buf.as_mut_ptr().cast::<libc::c_char>(),
                buf.len(),
                &raw mut out,
            );
            if rc == libc::ERANGE && len < 1 << 20 {
                len *= 2;
                continue;
            }
            if rc != 0 || out.is_null() || pw.pw_shell.is_null() {
                return None;
            }
            break OsStr::from_bytes(CStr::from_ptr(pw.pw_shell).to_bytes()).to_owned();
        }
    };
    let path = Path::new(&shell);
    (path.is_absolute() && runnable(path)).then_some(shell)
}

#[cfg(test)]
#[path = "unix_shell_tests.rs"]
mod tests;
