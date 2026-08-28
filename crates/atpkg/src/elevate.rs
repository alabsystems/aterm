// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The ELEVATION seam the OS-installer lanes share ([`crate::installer_pkg`],
//! [`crate::softwareupdate`], [`crate::system_pm`]): who may run a subprocess, how it is
//! elevated, and what proves the install afterwards.
//!
//! Three things live here, and nothing else in the crate spells them:
//!
//! * [`Runner`] — the one way a lane runs a tool. The production [`RealRunner`] spawns
//!   the process; a test (or a future GUI lane) installs its own with [`with_runner`], so
//!   every argv a lane builds is asserted EXACTLY and no test ever runs a real installer.
//! * [`Elevation`] — the policy of the CALLING VERB, never of the row: the unattended
//!   pass is always [`Elevation::Deferred`] (it records `needs admin — run: aterm pkg
//!   install <name>` and waits), the explicit door `aterm pkg install <name>` on a
//!   terminal is [`Elevation::Sudo`] (sudo prompts there), and `--elevate=osascript` is
//!   [`Elevation::Osascript`] (the GUI's administrator dialog). The default — with nothing
//!   set — is Deferred, so a library caller, a test and every background pass fail SAFE:
//!   nothing here ever elevates unless a verb said so on this very thread.
//! * [`first_provided`] — the `provides` probe: what proves an OS-installed member is
//!   present, since nothing of it lands in the store.
//!
//! Thread-local, deliberately: the policy is set by the verb on the thread that runs
//! the flow, and parallel tests each own their own.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The `sudo` the terminal door wraps an installer with.
pub const SUDO: &str = "/usr/bin/sudo";
/// The `osascript` the GUI door wraps an installer with.
pub const OSASCRIPT: &str = "/usr/bin/osascript";

/// How a subprocess's standard streams are wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// stdin from `/dev/null`, stdout and stderr CAPTURED — a probe (`pkgutil
    /// --check-signature`, `softwareupdate -l`) whose output the lane parses.
    Capture,
    /// All three INHERITED — an elevated install: `sudo` asks for its password on the
    /// caller's terminal and the installer's own progress reaches the user.
    Inherit,
    /// stdin from `/dev/null`, stdout and stderr INHERITED — an UNATTENDED install
    /// ([`Elevation::Deferred`]: a user-scoped manager run by the six-hourly pass): its
    /// progress still reaches the pass log, but nothing it spawns can wait on a
    /// password or a `[y/N]` — a `brew install` of a cask whose installer asks for
    /// `sudo` fails at once instead of hanging the pass on a prompt nobody can see.
    Unattended,
}

/// What a subprocess did: its exit code (`None` ⇒ killed by a signal) and, for
/// [`Io::Capture`], its output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    /// The exit code, `None` when the process died of a signal.
    pub code: Option<i32>,
    /// Captured stdout (empty under [`Io::Inherit`]).
    pub stdout: String,
    /// Captured stderr (empty under [`Io::Inherit`]).
    pub stderr: String,
}

impl Ran {
    /// Exit status zero.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// The one way a lane runs a tool. `argv[0]` is the program (an absolute path — the
/// lanes never search `PATH` for an installer), `argv[1..]` its arguments, each ONE
/// argument, never a shell word.
pub trait Runner {
    /// Run `argv` to completion.
    ///
    /// # Errors
    /// The process could not be spawned (the message names the program).
    fn run(&self, argv: &[String], io: Io) -> Result<Ran, String>;
}

/// The production runner: `std::process::Command`, exactly as the argv says.
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, argv: &[String], io: Io) -> Result<Ran, String> {
        let Some((exe, args)) = argv.split_first() else {
            return Err(String::from("empty argv"));
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args);
        match io {
            Io::Capture => {
                cmd.stdin(std::process::Stdio::null());
                let out = cmd.output().map_err(|e| spawn_failed(exe, &e))?;
                Ok(Ran {
                    code: out.status.code(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                })
            }
            Io::Inherit | Io::Unattended => {
                if io == Io::Unattended {
                    cmd.stdin(std::process::Stdio::null());
                }
                let status = cmd.status().map_err(|e| spawn_failed(exe, &e))?;
                Ok(Ran {
                    code: status.code(),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
    }
}

/// `cannot run <exe>: <error>` (manual concat — see `lib.rs` on `format!`).
fn spawn_failed(exe: &str, e: &std::io::Error) -> String {
    let mut m = String::from("cannot run ");
    m.push_str(exe);
    m.push_str(": ");
    m.push_str(&e.to_string());
    m
}

/// The calling verb's elevation policy — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// Never elevate. A member that needs admin records the canonical `needs admin —
    /// run: aterm pkg install <name>` state and waits for the explicit door. The default.
    Deferred,
    /// `sudo <installer …>` on the caller's terminal — the explicit door with a TTY.
    Sudo,
    /// `osascript -e 'do shell script "<installer …>" with administrator privileges'` —
    /// the GUI's administrator dialog (`--elevate=osascript`).
    Osascript,
}

thread_local! {
    static ELEVATION: Cell<Elevation> = const { Cell::new(Elevation::Deferred) };
    static RUNNER: RefCell<Option<Rc<dyn Runner>>> = const { RefCell::new(None) };
    #[cfg(test)]
    static PATH_VAR: RefCell<Option<Option<std::ffi::OsString>>> = const { RefCell::new(None) };
}

/// This process's `PATH`, as the lanes read it for their `provides` probes and the
/// `system-pm` manager lookup — or, inside a test, the value [`with_path_var`] injected
/// for its scope, so a flow test that walks `PATH` never meets the developer's own
/// `brew` and never runs it.
#[must_use]
pub fn path_var() -> Option<std::ffi::OsString> {
    #[cfg(test)]
    {
        if let Some(injected) = PATH_VAR.with(|p| p.borrow().clone()) {
            return injected;
        }
    }
    std::env::var_os("PATH")
}

/// Run `f` with `path` as what [`path_var`] answers on this thread for its duration —
/// `None` is an unset `PATH`. Nested scopes restore the outer one. Test-only.
#[cfg(test)]
pub(crate) fn with_path_var<R>(path: Option<&OsStr>, f: impl FnOnce() -> R) -> R {
    let prior = PATH_VAR.with(|p| p.replace(Some(path.map(OsStr::to_os_string))));
    let out = f();
    PATH_VAR.with(|p| {
        *p.borrow_mut() = prior;
    });
    out
}

/// Set this thread's elevation policy. Called by the explicit door only.
pub fn set_elevation(e: Elevation) {
    ELEVATION.with(|c| c.set(e));
}

/// This thread's elevation policy — [`Elevation::Deferred`] unless a verb set otherwise.
#[must_use]
pub fn elevation() -> Elevation {
    ELEVATION.with(Cell::get)
}

/// The door's policy: an explicit `--elevate=…` wins; otherwise a terminal on stdin means
/// `sudo` can prompt there, and no terminal means the pass must defer. Pure, so the rule
/// is pinned by a test rather than by prose.
#[must_use]
pub fn door_elevation(explicit: Option<Elevation>, stdin_tty: bool) -> Elevation {
    explicit.unwrap_or(if stdin_tty {
        Elevation::Sudo
    } else {
        Elevation::Deferred
    })
}

/// Whether this process's stdin is a terminal — the door's TTY test.
#[must_use]
pub fn stdin_is_tty() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

/// Run `f` with `runner` installed as this thread's runner for its duration — the seam
/// tests (and a future GUI lane) inject through. Nested scopes restore the outer one.
pub fn with_runner<R>(runner: Rc<dyn Runner>, f: impl FnOnce() -> R) -> R {
    let prior = RUNNER.with(|r| r.replace(Some(runner)));
    let out = f();
    RUNNER.with(|r| {
        *r.borrow_mut() = prior;
    });
    out
}

/// Hand this thread's runner — the injected one, else [`RealRunner`] — to `f`.
pub fn with_current_runner<R>(f: impl FnOnce(&dyn Runner) -> R) -> R {
    let injected = RUNNER.with(|r| r.borrow().clone());
    match injected {
        Some(r) => f(&*r),
        None => f(&RealRunner),
    }
}

/// `inner` wrapped for `elevation`: `sudo …`, the `osascript` administrator form, or
/// `None` when the policy is [`Elevation::Deferred`] (nothing may run).
#[must_use]
pub fn elevated_argv(elevation: Elevation, inner: &[String]) -> Option<Vec<String>> {
    match elevation {
        Elevation::Deferred => None,
        Elevation::Sudo => Some(sudo_argv(inner)),
        Elevation::Osascript => Some(osascript_argv(inner)),
    }
}

/// `/usr/bin/sudo <inner…>` — the terminal door's spelling.
#[must_use]
pub fn sudo_argv(inner: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(inner.len() + 1);
    argv.push(String::from(SUDO));
    argv.extend(inner.iter().cloned());
    argv
}

/// `/usr/bin/osascript -e 'do shell script "<inner, sh-quoted>" with administrator
/// privileges'` — the GUI door's spelling. Every element of `inner` is single-quoted for
/// `sh` (the script `do shell script` hands to `/bin/sh`), then the whole string is
/// escaped as an AppleScript string literal, so no argument can break out of either.
#[must_use]
pub fn osascript_argv(inner: &[String]) -> Vec<String> {
    let mut script = String::from("do shell script \"");
    script.push_str(&applescript_escape(&sh_join(inner)));
    script.push_str("\" with administrator privileges");
    vec![String::from(OSASCRIPT), String::from("-e"), script]
}

/// `inner` as ONE `sh` command line: each element single-quoted, space-separated.
fn sh_join(inner: &[String]) -> String {
    let mut out = String::new();
    for (i, a) in inner.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&sh_single_quote(a));
    }
    out
}

/// Single-quote `s` for `sh` (the POSIX `'\''` escape).
fn sh_single_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Escape `s` for the inside of an AppleScript `"…"` literal: backslash and double quote.
fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// The `provides` probe: the first entry that proves the install. An ABSOLUTE entry
/// proves it by existing as a regular file (following symlinks — `/opt/homebrew/bin/brew`
/// is one); a BARE tool name proves it by resolving on `path_var` OUTSIDE the managed
/// `prefix` ([`crate::vendor::system_binary_on_path`] — the store never proves a system
/// install). `None` ⇒ not installed.
#[must_use]
pub fn first_provided(
    prefix: &Path,
    provides: &[String],
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    for p in provides {
        if p.starts_with('/') {
            let path = Path::new(p);
            // Nothing under atpkg's own prefix — a shim, a store copy, a stub — proves
            // an OS install, whatever the row says.
            if crate::vendor::under_managed_prefix(prefix, path) {
                continue;
            }
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Some(PathBuf::from(p));
            }
        } else if let Some(found) = crate::vendor::system_binary_on_path(prefix, p, path_var) {
            return Some(found);
        }
    }
    None
}

/// `none of the provides paths exists after the install: <a>, <b>` — the message every
/// lane returns when the installer reported success and nothing proves it. Names the
/// paths so a mis-authored `provides` fails legibly on the authoring machine.
#[must_use]
pub fn nothing_provided(protocol: &str, provides: &[String]) -> String {
    let mut m = String::from(protocol);
    m.push_str(" reported success, but none of the provides paths exists: ");
    m.push_str(&provides.join(", "));
    m
}

#[cfg(test)]
pub(crate) mod testkit {
    //! A recording runner for the lanes' tests: scripted answers, exact argv capture.

    use super::{Io, Ran, Runner};
    use std::cell::RefCell;

    /// A hook run with each argv before the scripted answer is handed back.
    pub type RunHook = Box<dyn Fn(&[String])>;

    /// Records every `(argv, io)` it is asked to run and answers from a queue of scripted
    /// [`Ran`]s (the last one repeats). `on_run` is a hook a test uses to make an "install"
    /// leave its `provides` path behind, or to observe the placeholder mid-call.
    pub struct Recorder {
        pub calls: RefCell<Vec<(Vec<String>, Io)>>,
        pub answers: RefCell<Vec<Ran>>,
        pub on_run: Option<RunHook>,
    }

    impl Recorder {
        pub fn new(answers: Vec<Ran>) -> Self {
            Recorder {
                calls: RefCell::new(Vec::new()),
                answers: RefCell::new(answers),
                on_run: None,
            }
        }
        pub fn argvs(&self) -> Vec<Vec<String>> {
            self.calls.borrow().iter().map(|(a, _)| a.clone()).collect()
        }
    }

    impl Runner for Recorder {
        fn run(&self, argv: &[String], io: Io) -> Result<Ran, String> {
            self.calls.borrow_mut().push((argv.to_vec(), io));
            if let Some(hook) = &self.on_run {
                hook(argv);
            }
            let mut answers = self.answers.borrow_mut();
            if answers.len() > 1 {
                Ok(answers.remove(0))
            } else {
                answers
                    .first()
                    .cloned()
                    .ok_or_else(|| String::from("no scripted answer"))
            }
        }
    }

    /// A clean exit with `stdout`.
    pub fn ok(stdout: &str) -> Ran {
        Ran {
            code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    /// A failed exit.
    pub fn failed(code: i32, stderr: &str) -> Ran {
        Ran {
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    /// The default is DEFERRED, on every thread, until a verb says otherwise on that
    /// thread — a background pass can never elevate by accident.
    #[test]
    fn the_default_policy_is_deferred_and_thread_local() {
        assert_eq!(elevation(), Elevation::Deferred);
        set_elevation(Elevation::Sudo);
        assert_eq!(elevation(), Elevation::Sudo);
        let other = std::thread::spawn(elevation).join().unwrap();
        assert_eq!(other, Elevation::Deferred, "another thread is untouched");
        set_elevation(Elevation::Deferred);
        assert_eq!(elevated_argv(Elevation::Deferred, &s(&["/x"])), None);
    }

    /// The door's rule: explicit wins; a terminal means sudo; no terminal means defer.
    #[test]
    fn the_door_elevates_only_with_a_terminal_or_an_explicit_ask() {
        assert_eq!(door_elevation(None, true), Elevation::Sudo);
        assert_eq!(door_elevation(None, false), Elevation::Deferred);
        assert_eq!(
            door_elevation(Some(Elevation::Osascript), false),
            Elevation::Osascript
        );
        assert_eq!(
            door_elevation(Some(Elevation::Deferred), true),
            Elevation::Deferred
        );
    }

    /// The two wrappers, EXACTLY: sudo prefixes; osascript builds the administrator
    /// `do shell script` with every argument sh-quoted and the literal escaped.
    #[test]
    fn the_sudo_and_osascript_wrappers_are_exact() {
        let inner = s(&[
            "/usr/sbin/installer",
            "-pkg",
            "/tmp/Homebrew.pkg",
            "-target",
            "/",
        ]);
        assert_eq!(
            sudo_argv(&inner),
            s(&[
                "/usr/bin/sudo",
                "/usr/sbin/installer",
                "-pkg",
                "/tmp/Homebrew.pkg",
                "-target",
                "/"
            ])
        );
        assert_eq!(
            elevated_argv(Elevation::Sudo, &inner),
            Some(sudo_argv(&inner))
        );
        assert_eq!(
            osascript_argv(&inner),
            s(&[
                "/usr/bin/osascript",
                "-e",
                "do shell script \"'/usr/sbin/installer' '-pkg' '/tmp/Homebrew.pkg' '-target' \
                 '/'\" with administrator privileges"
            ])
        );
        assert_eq!(
            elevated_argv(Elevation::Osascript, &inner),
            Some(osascript_argv(&inner))
        );
        // A hostile file name cannot escape either quoting layer.
        let nasty = s(&[
            "/usr/sbin/installer",
            "-pkg",
            "/tmp/a'b\"c\\d.pkg",
            "-target",
            "/",
        ]);
        let script = &osascript_argv(&nasty)[2];
        assert_eq!(
            script,
            "do shell script \"'/usr/sbin/installer' '-pkg' '/tmp/a'\\\\''b\\\"c\\\\d.pkg' \
             '-target' '/'\" with administrator privileges"
        );
    }

    /// An injected `PATH` is what the lanes read, for the scope of the injection only;
    /// outside it the process's own `PATH` answers.
    #[test]
    fn an_injected_path_is_scoped_to_the_call() {
        let real = std::env::var_os("PATH");
        assert_eq!(path_var(), real);
        let fake = std::ffi::OsString::from("/atpkg/injected/bin");
        let seen = with_path_var(Some(&fake), || {
            let inner = path_var();
            let nested = with_path_var(None, path_var);
            assert_eq!(nested, None, "None injects an UNSET PATH");
            assert_eq!(
                path_var(),
                Some(fake.clone()),
                "the outer scope is restored"
            );
            inner
        });
        assert_eq!(seen, Some(fake));
        assert_eq!(path_var(), real);
    }

    /// An injected runner is what the lanes see, for the scope of the injection only.
    #[test]
    fn an_injected_runner_is_scoped_to_the_call() {
        let rec = Rc::new(testkit::Recorder::new(vec![testkit::ok("hello")]));
        let got = with_runner(rec.clone(), || {
            with_current_runner(|r| r.run(&s(&["/bin/echo", "x"]), Io::Capture).unwrap())
        });
        assert_eq!(got.stdout, "hello", "the recorder answered, not /bin/echo");
        assert_eq!(rec.argvs(), vec![s(&["/bin/echo", "x"])]);
        // Outside the scope the real runner is back: a real, harmless process.
        let real = with_current_runner(|r| r.run(&s(&["/bin/echo", "real"]), Io::Capture));
        assert!(real.is_ok_and(|r| r.success() && r.stdout.trim() == "real"));
        let missing = RealRunner.run(&s(&["/nonexistent/atpkg-no-such-tool"]), Io::Capture);
        assert!(missing.unwrap_err().starts_with("cannot run /nonexistent/"));
        assert!(RealRunner.run(&[], Io::Capture).is_err());
        // UNATTENDED wires stdin to /dev/null: a tool that would wait on input (`cat`)
        // sees EOF at once and exits clean — nothing can hang the pass on a prompt.
        #[cfg(unix)]
        {
            let unattended = RealRunner.run(&s(&["/bin/cat"]), Io::Unattended);
            assert!(
                unattended.is_ok_and(|r| r.success()),
                "cat must see EOF, not wait"
            );
        }
    }

    /// The provides probe: an absolute path proves by existing as a file, a bare name by
    /// resolving on PATH outside the prefix; a directory, a missing path, nothing.
    #[cfg(unix)]
    #[test]
    fn the_provides_probe_reads_absolute_paths_and_bare_names() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("atpkg-elevate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prefix = root.join("prefix");
        let sys = root.join("sys-bin");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let brew = sys.join("brew");
        std::fs::write(&brew, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).unwrap();
        let abs = brew.to_string_lossy().into_owned();
        let path = std::env::join_paths([sys.clone()]).unwrap();
        assert_eq!(
            first_provided(
                &prefix,
                &[String::from("/nope/x"), abs.clone()],
                Some(&path)
            ),
            Some(brew.clone()),
            "the first that EXISTS wins, in row order"
        );
        assert_eq!(
            first_provided(&prefix, &[String::from("brew")], Some(&path)),
            Some(brew.clone())
        );
        assert_eq!(
            first_provided(&prefix, &[sys.to_string_lossy().into_owned()], Some(&path)),
            None,
            "a directory is not a proof"
        );
        assert_eq!(first_provided(&prefix, &[String::from("brew")], None), None);
        assert_eq!(first_provided(&prefix, &[], Some(&path)), None);
        // A file under the managed prefix (a shim, a store copy) never proves an OS
        // install, however the row spells it — even through a symlink into the prefix.
        let managed = prefix.join("bin/brew");
        std::fs::write(&managed, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            first_provided(
                &prefix,
                &[managed.to_string_lossy().into_owned()],
                Some(&path)
            ),
            None
        );
        let alias = root.join("alias-brew");
        std::os::unix::fs::symlink(&managed, &alias).unwrap();
        assert_eq!(
            first_provided(
                &prefix,
                &[alias.to_string_lossy().into_owned()],
                Some(&path)
            ),
            None
        );
        assert_eq!(
            nothing_provided("pkg", &[String::from("/a"), String::from("/b")]),
            "pkg reported success, but none of the provides paths exists: /a, /b"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
