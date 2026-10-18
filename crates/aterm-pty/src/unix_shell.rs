// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH PROGRAM a Unix session's shell is: [`select_shell_with_origin`], the one
//! resolution the spawn seam runs and every caller that names or detects the shell
//! asks (aterm-gui's integration and its `[zsh ended at start]` line, `aterm
//! doctor`), so none of them can name a shell the spawn would not run.
//!
//! The order is the override (`--shell` / `shell` in aterm.toml), then a non-empty
//! `$SHELL`, then the account's login shell in the password database, then
//! `/bin/sh` — the order login(1), tmux, xterm, VTE, kitty and foot use. A name with
//! no `/` is looked up on `PATH` as a shell looks up a command, so `shell = "bash"`
//! runs `/usr/bin/bash`.
//!
//! Everything here runs in the PARENT, before the fork: it allocates, stats and
//! reads the environment, none of which the child may do.

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
    /// The account's login shell in the password database: `$SHELL` is unset or
    /// empty.
    Passwd,
    /// `/bin/sh`: `$SHELL` names nothing, and the password database names no
    /// shell this user can run.
    Default,
}

impl ShellOrigin {
    /// Where a shell that cannot start was set, as the end of the sentence that
    /// says so: `` `fish` is not on PATH (from $SHELL)``. Empty for `/bin/sh`,
    /// which nothing set.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Override => " (set by --shell or `shell` in aterm.toml)",
            Self::Env => " (from $SHELL)",
            Self::Passwd => " (your login shell in the password database)",
            Self::Default => "",
        }
    }

    /// The one word a report prints for this arm (`shell_origin: default:<word>`): the
    /// variable or file that chose the shell. `override` for [`Self::Override`], which a
    /// caller names by its own input (`--shell`, `aterm.toml`).
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

/// The program a Unix session's interactive shell runs, and where it came from:
/// `override_shell` when it names one, else a non-empty `$SHELL`, else
/// [`login_shell`].
///
/// A name with no `/` is looked up on `PATH`; one found nowhere is kept as typed,
/// and the spawn then says it `is not on PATH`. A name holding a NUL byte is kept
/// as typed too, for the spawn to refuse ([`crate::program_nul_refusal`]): looking
/// it up would name some other program. An empty `$SHELL` names nothing, as an
/// unset one does — login(1) and tmux read it that way, and `execve("")` can only
/// fail.
#[must_use]
pub fn select_shell_with_origin(override_shell: Option<&OsStr>) -> (OsString, ShellOrigin) {
    select_shell_in(
        override_shell,
        std::env::var_os("SHELL"),
        std::env::var_os("PATH").as_deref(),
        account_shell,
    )
}

/// The shell the account logs in with, for a session whose `$SHELL` names none:
/// the password database's entry when it is a program this user can run (`pw_shell`
/// of `getpwuid_r(getuid())`), else `/bin/sh`. A launcher that scrubs the
/// environment (`env -i`, a container, cron, some window managers) leaves `$SHELL`
/// unset; `/bin/sh` there is dash on Debian and Ubuntu, with no line editing.
#[must_use]
pub fn login_shell() -> (OsString, ShellOrigin) {
    login_shell_from(account_shell)
}

/// The `SHELL` a child is handed when aterm's own environment names none (unset or
/// empty): [`login_shell`], so a program that reads it (`vim`'s `:shell`, `make`,
/// a `ssh` `LocalCommand`) finds the account's shell instead of nothing. `None`
/// when `$SHELL` is set: the child inherits it.
#[must_use]
pub fn shell_env_for_child() -> Option<OsString> {
    names_a_shell(std::env::var_os("SHELL"))
        .is_none()
        .then(|| login_shell().0)
}

/// Why `program` cannot be a session's shell, in the words a failed spawn uses
/// (`` `/usr/bin/zshh` does not exist (from $SHELL)``), or `None` when it is a
/// file this user can run. Checked before the spawn, so a caller can start another
/// shell in its place and say why ([`fallback_shell`]).
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

/// The shell to start in place of `failed`, which cannot start
/// ([`shell_cannot_start`]): the account's login shell, else `/bin/sh` — whichever
/// is first a program this user can run and is not `failed` itself. `None` when
/// neither is.
#[must_use]
pub fn fallback_shell(failed: &OsStr) -> Option<(OsString, ShellOrigin)> {
    fallback_shell_from(failed, account_shell)
}

/// [`select_shell_with_origin`] over its inputs: the environment's `$SHELL` and
/// `PATH`, and the password-database lookup.
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

/// [`login_shell`] over its password-database lookup.
fn login_shell_from(account: impl FnOnce() -> Option<OsString>) -> (OsString, ShellOrigin) {
    match account() {
        Some(shell) => (shell, ShellOrigin::Passwd),
        None => (OsString::from("/bin/sh"), ShellOrigin::Default),
    }
}

/// [`fallback_shell`] over its password-database lookup.
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

/// `value` when it names a shell: set and not empty.
fn names_a_shell(value: Option<OsString>) -> Option<OsString> {
    value.filter(|v| !v.is_empty())
}

/// `name` as the program `execve` is handed: a path (it holds a `/`) as it is, a
/// bare name as the first `PATH` entry holding a program this user can run by that
/// name, and a bare name found nowhere — or holding a NUL — as it is.
fn on_path(name: &OsStr, path_var: Option<&OsStr>) -> OsString {
    if name.as_bytes().contains(&0) {
        return name.to_owned();
    }
    find_on_path(name, path_var).map_or_else(|| name.to_owned(), PathBuf::into_os_string)
}

/// The first `PATH` entry holding a program named `name` this user can run, or
/// `None` (also for a name holding a `/`, which is a path and not looked up, and
/// for an empty name). Only absolute entries are searched: an empty one, `.` or
/// `bin` names a folder relative to aterm's own current folder — not the one the
/// pane starts in — and a terminal's shell or `-e` program must not be taken from
/// whatever folder aterm happened to be started in.
pub(crate) fn find_on_path(name: &OsStr, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return None;
    }
    // A plain loop, no closures: a closure body is an absent callee to the Trust
    // panic-freedom verifier.
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

/// A regular file this user may execute (`access(X_OK)` and a regular file:
/// something `execve` can run).
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
// Skip: one libc call through raw out-pointers; every failure is `None`, and the
// caller then runs `/bin/sh`.
#[cfg_attr(trust_verify, trust::skip)]
fn account_shell() -> Option<OsString> {
    // SAFETY: `passwd` is plain integers and pointers, for which all-zero is a
    // valid value; it is only an out-param here.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // An entry that does not fit (a large LDAP or SSSD record) is ERANGE: the
    // buffer doubles, to 1 MiB, rather than the account reading as having no
    // shell.
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
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A scratch folder holding `bin/fish` (a program) and `bin/notes` (a file no
    /// one may run), and a `PATH` that names an empty entry, a missing folder and
    /// then `bin`.
    struct Bed {
        _root: aterm_tempfile::TempDir,
        bin: PathBuf,
        path: OsString,
    }

    impl Bed {
        fn new(tag: &str) -> Self {
            let root = aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-pty-shell-{tag}-"))
                .tempdir()
                .expect("an exclusively owned scratch root");
            let bin = root.path().join("bin");
            std::fs::create_dir_all(&bin).expect("a scratch bin");
            for (name, mode) in [("fish", 0o755), ("notes", 0o644)] {
                let file = bin.join(name);
                std::fs::write(&file, b"#!/bin/sh\n").expect("a scratch file");
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))
                    .expect("its mode");
            }
            let mut path = OsString::from(":");
            path.push(root.path().join("missing"));
            path.push(":");
            path.push(&bin);
            Self {
                _root: root,
                bin,
                path,
            }
        }
    }

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    /// The override wins, then a non-empty `$SHELL`, then the password database,
    /// then `/bin/sh`; a bare name is looked up on `PATH` wherever it came from.
    #[test]
    fn the_shell_is_the_override_then_shell_then_the_account_then_sh() {
        let bed = Bed::new("order");
        let path = Some(bed.path.as_os_str());
        let fish = bed.bin.join("fish").into_os_string();
        let account = || Some(os("/bin/bash"));
        assert_eq!(
            select_shell_in(
                Some(OsStr::new("fish")),
                Some(os("/bin/zsh")),
                path,
                account
            ),
            (fish.clone(), ShellOrigin::Override),
            "a bare override is looked up on PATH"
        );
        assert_eq!(
            select_shell_in(Some(OsStr::new("")), Some(os("fish")), path, account),
            (fish, ShellOrigin::Env),
            "an empty override is none, and a bare $SHELL is looked up too"
        );
        assert_eq!(
            select_shell_in(None, Some(os("/bin/zsh")), path, account),
            (os("/bin/zsh"), ShellOrigin::Env),
            "a path is taken as it is"
        );
        for env in [None, Some(os(""))] {
            assert_eq!(
                select_shell_in(None, env.clone(), path, account),
                (os("/bin/bash"), ShellOrigin::Passwd),
                "$SHELL {env:?} names nothing: the account's login shell"
            );
            assert_eq!(
                select_shell_in(None, env, path, || None),
                (os("/bin/sh"), ShellOrigin::Default)
            );
        }
    }

    /// A bare name found nowhere, or one only a file nobody may run carries, stays
    /// as typed, for the spawn to say so; a NUL is never looked up.
    #[test]
    fn a_name_path_cannot_resolve_stays_as_typed() {
        let bed = Bed::new("typed");
        let path = Some(bed.path.as_os_str());
        for name in ["zsh-nowhere", "notes"] {
            assert_eq!(
                select_shell_in(Some(OsStr::new(name)), None, path, || None).0,
                os(name)
            );
        }
        assert_eq!(on_path(OsStr::new("fi\0sh"), path), os("fi\0sh"));
        assert_eq!(on_path(OsStr::new("fish"), None), os("fish"), "no PATH");
        assert_eq!(find_on_path(OsStr::new(""), path), None);
        assert_eq!(find_on_path(OsStr::new("./fish"), path), None);
    }

    /// A relative `PATH` entry is never searched: it names a folder under aterm's
    /// own current folder, not the pane's. The same folder, spelled absolute, is.
    #[test]
    fn a_relative_path_entry_is_not_searched() {
        let bed = Bed::new("relative");
        let cwd = std::env::current_dir().expect("the test's folder");
        let mut relative = PathBuf::new();
        for _ in cwd.components().skip(1) {
            relative.push("..");
        }
        relative.push(bed.bin.strip_prefix("/").expect("an absolute bin"));
        assert!(
            runnable(&relative.join("fish")),
            "the relative spelling names the program"
        );
        assert_eq!(
            find_on_path(OsStr::new("fish"), Some(relative.as_os_str())),
            None
        );
        assert_eq!(
            find_on_path(OsStr::new("fish"), Some(bed.bin.as_os_str())),
            Some(bed.bin.join("fish"))
        );
    }

    /// Each reason a shell cannot start, in the spawn's own words; a program this
    /// user can run is none.
    #[test]
    fn a_shell_that_cannot_start_is_named_with_where_it_was_set() {
        let bed = Bed::new("cannot");
        let fish = bed.bin.join("fish");
        assert_eq!(
            shell_cannot_start(fish.as_os_str(), ShellOrigin::Override),
            None
        );
        assert_eq!(
            shell_cannot_start(OsStr::new("bash-nowhere"), ShellOrigin::Override).as_deref(),
            Some("`bash-nowhere` is not on PATH (set by --shell or `shell` in aterm.toml)")
        );
        let gone = bed.bin.join("zhs");
        assert_eq!(
            shell_cannot_start(gone.as_os_str(), ShellOrigin::Env),
            Some(format!("`{}` does not exist (from $SHELL)", gone.display()))
        );
        let notes = bed.bin.join("notes");
        assert_eq!(
            shell_cannot_start(notes.as_os_str(), ShellOrigin::Passwd),
            Some(format!(
                "`{}` is not executable (your login shell in the password database)",
                notes.display()
            ))
        );
        assert!(
            shell_cannot_start(OsStr::new("/bin/fi\0sh"), ShellOrigin::Override)
                .is_some_and(|why| why.contains("NUL")),
        );
    }

    /// The fallback is the account's shell, else `/bin/sh`, never the shell that
    /// failed and never one nobody may run.
    #[test]
    fn the_fallback_is_the_account_shell_then_sh_never_the_failed_one() {
        let bed = Bed::new("fallback");
        let fish = bed.bin.join("fish").into_os_string();
        let notes = bed.bin.join("notes").into_os_string();
        let failed = OsStr::new("/nonexistent/zsh");
        assert_eq!(
            fallback_shell_from(failed, || Some(fish.clone())),
            Some((fish.clone(), ShellOrigin::Passwd))
        );
        assert_eq!(
            fallback_shell_from(&fish, || Some(fish.clone())),
            Some((os("/bin/sh"), ShellOrigin::Default)),
            "the account's shell is the one that failed"
        );
        assert_eq!(
            fallback_shell_from(failed, || Some(notes.clone())),
            Some((os("/bin/sh"), ShellOrigin::Default)),
            "nobody may run the account's shell"
        );
        assert_eq!(
            fallback_shell_from(failed, || None),
            Some((os("/bin/sh"), ShellOrigin::Default))
        );
        assert_eq!(fallback_shell_from(OsStr::new("/bin/sh"), || None), None);
    }

    /// The password database names this account's shell as an absolute program
    /// this user can run, or nothing — never a bare or relative name.
    #[test]
    fn the_account_shell_is_an_absolute_program_or_none() {
        if let Some(shell) = account_shell() {
            let path = Path::new(&shell);
            assert!(path.is_absolute() && runnable(path), "{shell:?}");
        }
        let (shell, origin) = login_shell();
        assert!(matches!(origin, ShellOrigin::Passwd | ShellOrigin::Default));
        assert!(Path::new(&shell).is_absolute(), "{shell:?}");
    }
}
