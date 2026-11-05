// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim's dispatch: what the one `aterm` binary does when it is run as
//! `git`, `cc`, `clang`, `make`, `python3` or another of [`aterm_types::clt_shim::SHIM_TOOLS`]
//! through a shim symlink, or as the `xcrun` Apple's stubs reach through `DEVELOPER_DIR`
//! ([`aterm_types::clt_shim::developer_dir`]), on a Mac that has no Command Line Tools yet (System
//! State design §6.3).
//!
//! While the tools are missing the shim prints one line and exits
//! [`aterm_types::clt_shim::SHIM_EXIT`] at once, and it never runs `/usr/bin/git`, whose
//! `xcode-select` stub would raise macOS's install dialog: whether an install is on its way only
//! changes the line. That holds after a person's Stop, an install that ended unfinished, and on an
//! account that may not install them. A shell opened before the tools arrived keeps the shim on
//! its PATH (bash even hashes its path, which is why the directory stays once laid);
//! once the tools are there that shim `exec`s Apple's own `/usr/bin/<tool>`, with the shim
//! directory taken off the PATH it hands on, so a long-lived shell works the moment the install
//! finishes: with the tools in place Apple's front end runs the developer directory's tool with
//! the SDK set (`cc` finds `<stdio.h>`, `ld` finds `libSystem`), raises no dialog, and `xcrun`,
//! which no developer directory carries, runs as it is ([`aterm_types::clt_shim::carried`]). The
//! process start already took an inherited `DEVELOPER_DIR` of the shim's off its environment
//! ([`aterm_types::clt_shim::developer_dir_move`]). For a name the tools do not carry
//! (`pagestuff`, `llvm-gcc`) the stub would raise Apple's dialog for Xcode, so the shim says so in
//! one line and exits instead.
//!
//! The shim finds its directory, and so the mark beside it, from the path it was run by (the
//! symlink's own, which macOS keeps as the program's path), never from `$HOME`: a shell that
//! changed `HOME` still reaches a shim that knows the install is under way.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::clt_shim::{SHIM_EXIT, absent_line, arrival_line, called_tool, missing_line};

/// The seams the dispatch reaches the Mac through, so a test drives it with no real tools and no
/// real `exec`.
trait ShimHost {
    /// Whether Apple's developer tools are installed now ([`aterm_types::clt_shim::developer_git`]).
    fn tools_present(&self) -> bool;
    /// Whether an install of the tools is on its way: the mark beside this shim's directory
    /// ([`aterm_types::clt_shim::installing_mark`]).
    fn installing(&self) -> bool;
    /// Whether this account may install the tools: one of the Mac's administrators
    /// ([`aterm_uds::account::administrator`]), or groups that did not read.
    fn may_install(&self) -> bool;
    /// Whether a developer directory carries `tool` ([`aterm_types::clt_shim::developer_tool`]),
    /// so Apple's `/usr/bin/<tool>` has a tool to run once the tools are present.
    fn carries(&self, tool: &str) -> bool;
    /// Apple's own `/usr/bin/<tool>`, which raises their install dialog while the tools are
    /// missing: run only once they are present, for a tool they carry.
    fn apple_tool(&self, tool: &str) -> PathBuf;
    /// The shim directory, which the PATH handed to Apple's tool leaves out.
    fn shim_dir(&self) -> Option<PathBuf>;
    /// Replace this process with the program at `path`, under `argv[0]` `tool` and `rest`, its
    /// `PATH` set to `path_var` when one is given. Returns only if the `exec` failed (the error).
    fn exec(
        &self,
        path: &Path,
        tool: &str,
        rest: &[OsString],
        path_var: Option<&OsStr>,
    ) -> std::io::Error;
    /// This process's `PATH`.
    fn path_var(&self) -> Option<OsString>;
    /// Print `line` to standard error.
    fn warn(&self, line: &str);
}

/// Serve a shim invoked as `tool` (its `argv[0]`) with `rest` (its `argv[1..]`).
#[must_use = "return the shim's exit status from the front door"]
pub fn run(tool: &str, rest: Vec<OsString>) -> ExitCode {
    ExitCode::from(run_with(&Live::for_tool(tool), tool, &rest))
}

/// The dispatch over its seams, returning the exit byte so a test can read it (`ExitCode` is
/// opaque). On a real `Live` host the `exec` never returns; the byte is reached only when it
/// fails, when the tools carry no such tool, or while they are missing.
fn run_with(host: &dyn ShimHost, tool: &str, rest: &[OsString]) -> u8 {
    if !host.tools_present() {
        // Never Apple's stub, whatever becomes of the install: it would raise macOS's dialog.
        let called = called_tool(tool, rest);
        host.warn(&if host.installing() {
            arrival_line(called)
        } else {
            missing_line(called, host.may_install())
        });
        return SHIM_EXIT;
    }
    // This shell opened before the tools arrived. Apple's front end runs a tool they carry with
    // the SDK set and raises no dialog now; for one they lack it would raise Apple's dialog for
    // Xcode.
    if !aterm_types::clt_shim::carried(tool, |tool| host.carries(tool)) {
        host.warn(&absent_line(tool));
        return SHIM_EXIT;
    }
    let target = host.apple_tool(tool);
    // Whatever runs gets the PATH without the shim, so nothing it starts comes back here.
    let path_var = host.path_var().map(|path| match host.shim_dir() {
        Some(shim) => aterm_types::clt_shim::without_shim(&path, &shim),
        None => path,
    });
    let error = host.exec(&target, tool, rest, path_var.as_deref());
    host.warn(&format!("{}: {error}", target.display()));
    SHIM_EXIT
}

/// The shim directory a shim run as `tool` belongs to: the parent of `exe`, the path it was run by
/// (macOS keeps a symlink's own path as the program's path, by `PATH` lookup and by a relative
/// path alike), or the directory above that parent's developer directory for the `xcrun` Apple's
/// stubs reach ([`aterm_types::clt_shim::shim_of_program_dir`]), when that path's file name is
/// `tool` and it is absolute; else this account's shim
/// directory ([`aterm_types::clt_shim::shim_dir`], `fallback`). Never `$HOME`'s alone: a shell
/// that changed `HOME` still finds the directory it runs from.
fn own_dir(
    exe: Option<PathBuf>,
    tool: &str,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    exe.filter(|exe| exe.is_absolute() && exe.file_name() == Some(OsStr::new(tool)))
        .and_then(|exe| exe.parent().map(aterm_types::clt_shim::shim_of_program_dir))
        .or_else(fallback)
}

/// This Mac, for a shim run as one tool: the directory it runs from ([`own_dir`]).
struct Live {
    dir: Option<PathBuf>,
}

impl Live {
    fn for_tool(tool: &str) -> Self {
        Self {
            dir: own_dir(
                std::env::current_exe().ok(),
                tool,
                aterm_types::clt_shim::shim_dir,
            ),
        }
    }
}

impl ShimHost for Live {
    fn tools_present(&self) -> bool {
        aterm_types::clt_shim::developer_git()
    }

    fn installing(&self) -> bool {
        self.dir
            .as_deref()
            .is_some_and(|dir| aterm_types::clt_shim::installing_mark(dir).is_file())
    }

    fn may_install(&self) -> bool {
        #[cfg(unix)]
        let admin = aterm_uds::account::administrator();
        // No account groups read off Unix, where no Mac's tools are installed either.
        #[cfg(not(unix))]
        let admin = None;
        admin != Some(false)
    }

    fn carries(&self, tool: &str) -> bool {
        aterm_types::clt_shim::developer_tool(tool).is_some()
    }

    fn apple_tool(&self, tool: &str) -> PathBuf {
        Path::new("/usr/bin").join(tool)
    }

    fn shim_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    #[cfg(unix)]
    fn exec(
        &self,
        path: &Path,
        tool: &str,
        rest: &[OsString],
        path_var: Option<&OsStr>,
    ) -> std::io::Error {
        let mut command = aterm_uds::spawnfd::command(path);
        command.arg0(tool).args(rest);
        if let Some(path_var) = path_var {
            command.env("PATH", path_var);
        }
        command.exec()
    }

    /// Off Unix there are no `xcode-select` stubs to stand in for and the GUI never lays the
    /// shim (its `refresh` is a no-op off Unix), so a shim symlink is never on any PATH and this
    /// is never reached. The portable `Command` carries no `exec`, so there is nothing to replace
    /// this process with; report it unsupported (the caller then prints a line and exits),
    /// keeping the dispatch compiling on every target.
    #[cfg(not(unix))]
    fn exec(
        &self,
        _path: &Path,
        _tool: &str,
        _rest: &[OsString],
        _path_var: Option<&OsStr>,
    ) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the Command Line Tools shim is macOS-only",
        )
    }

    fn path_var(&self) -> Option<OsString> {
        std::env::var_os("PATH")
    }

    fn warn(&self, line: &str) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One `exec` the dispatch asked for: the program, its `argv[0]`, the rest and its `PATH`.
    type Exec = (PathBuf, String, Vec<OsString>, Option<OsString>);

    #[derive(Default)]
    struct Fake {
        present: bool,
        /// Whether an install of the tools is on its way (the window's mark).
        installing: bool,
        /// An account that is none of the Mac's administrators.
        not_admin: bool,
        /// The tools the developer directory carries, by name.
        carries: Vec<&'static str>,
        path: Option<OsString>,
        warnings: RefCell<Vec<String>>,
        execs: RefCell<Vec<Exec>>,
    }

    /// The root of an absolute path on the host the test runs on: a drive's on Windows, where
    /// a `/`-rooted path is relative and so never a shim's own ([`own_dir`]).
    #[cfg(not(windows))]
    const ROOT: &str = "/";
    #[cfg(windows)]
    const ROOT: &str = r"C:\";

    /// `rest`, `/`-separated, under [`ROOT`].
    fn at(rest: &str) -> PathBuf {
        rest.split('/')
            .fold(PathBuf::from(ROOT), |path, part| path.join(part))
    }

    /// This account's shim directory.
    fn shim() -> PathBuf {
        at("Users/alice/.local/state/aterm/clt-shim")
    }

    /// `entries` as one `PATH`, joined by this host's separator (`;` on Windows) as the
    /// dispatch splits it.
    fn path_of(entries: &[PathBuf]) -> OsString {
        std::env::join_paths(entries).expect("PATH entries")
    }

    /// Where the fake keeps Apple's own tools: never run, only named.
    const STUBS: &str = "/stand-in/usr/bin";

    impl ShimHost for Fake {
        fn tools_present(&self) -> bool {
            self.present
        }

        fn installing(&self) -> bool {
            self.installing
        }

        fn may_install(&self) -> bool {
            !self.not_admin
        }

        fn carries(&self, tool: &str) -> bool {
            self.carries.contains(&tool)
        }

        fn apple_tool(&self, tool: &str) -> PathBuf {
            Path::new(STUBS).join(tool)
        }

        fn shim_dir(&self) -> Option<PathBuf> {
            Some(shim())
        }

        fn exec(
            &self,
            path: &Path,
            tool: &str,
            rest: &[OsString],
            path_var: Option<&OsStr>,
        ) -> std::io::Error {
            self.execs.borrow_mut().push((
                path.to_owned(),
                tool.to_owned(),
                rest.to_vec(),
                path_var.map(OsStr::to_owned),
            ));
            std::io::Error::other("test: no real exec")
        }

        fn path_var(&self) -> Option<OsString> {
            self.path.clone()
        }

        fn warn(&self, line: &str) {
            self.warnings.borrow_mut().push(line.to_owned());
        }
    }

    #[test]
    fn with_the_tools_installing_it_prints_once_exits_69_and_spawns_nothing() {
        for tool in aterm_types::clt_shim::SHIM_TOOLS {
            let fake = Fake {
                installing: true,
                ..Fake::default()
            };
            let code = run_with(&fake, tool, &[OsString::from("--version")]);
            assert_eq!(code, 69, "{tool}: exits EX_UNAVAILABLE");
            assert_eq!(
                fake.execs.borrow().len(),
                0,
                "{tool}: raised no /usr/bin spawn"
            );
            let warnings = fake.warnings.borrow();
            assert_eq!(warnings.len(), 1, "{tool}: one line");
            assert!(
                warnings[0].starts_with(&format!("{tool} arrives with Apple's Command Line Tools")),
                "{tool}: {}",
                warnings[0]
            );
        }
    }

    /// With the tools present, the same call `exec`s Apple's own `/usr/bin/<tool>` (which runs
    /// the developer directory's tool with the SDK set, and raises no dialog with the tools in
    /// place), with the shim directory off the PATH it hands on and `argv` as given.
    #[test]
    fn with_the_tools_present_it_execs_apples_own_tool_off_the_shim() {
        let fake = Fake {
            present: true,
            carries: vec!["git"],
            path: Some(path_of(&[
                at("opt/homebrew/bin"),
                shim(),
                at("usr/bin"),
                at("bin"),
            ])),
            ..Fake::default()
        };
        let code = run_with(
            &fake,
            "git",
            &[OsString::from("status"), OsString::from("-s")],
        );
        assert_eq!(code, 69, "the exec fake fails, so the byte is reached");
        let execs = fake.execs.borrow();
        assert_eq!(execs.len(), 1, "it execs exactly once");
        assert_eq!(execs[0].0, Path::new(STUBS).join("git"));
        assert_eq!(execs[0].1, "git", "argv0 is preserved");
        assert_eq!(
            execs[0].2,
            vec![OsString::from("status"), OsString::from("-s")]
        );
        assert_eq!(
            execs[0].3,
            Some(path_of(&[at("opt/homebrew/bin"), at("usr/bin"), at("bin")])),
            "the shim directory is off the PATH Apple's tool gets"
        );
    }

    /// `/usr/bin/xcrun` is macOS's own `xcrun`, not one of the install stubs, and no developer
    /// directory carries one (measured: `/Library/Developer/CommandLineTools/usr/bin` has no
    /// `xcrun`). So in a shell opened during the install, which keeps the shim until the next
    /// launch, `xcrun --show-sdk-path` (what the `cc` crate, CMake and Homebrew run) reaches
    /// `/usr/bin/xcrun` once the tools are installed, never refused as a tool they lack.
    #[test]
    fn with_the_tools_present_xcrun_is_handed_on_not_refused() {
        let fake = Fake {
            present: true,
            carries: vec!["git", "cc", "clang", "ld"],
            path: Some(path_of(&[shim(), at("usr/bin"), at("bin")])),
            ..Fake::default()
        };
        let _ = run_with(&fake, "xcrun", &[OsString::from("--show-sdk-path")]);
        let execs = fake.execs.borrow();
        assert_eq!(
            execs.iter().map(|exec| exec.0.clone()).collect::<Vec<_>>(),
            [Path::new(STUBS).join("xcrun")],
            "xcrun ran nothing and said: {:?}",
            fake.warnings.borrow()
        );
        assert_eq!(execs[0].3, Some(path_of(&[at("usr/bin"), at("bin")])));
    }

    /// A person's Stop, an install that ended unfinished, or an account that may not install them
    /// leaves nothing installing the tools: the call still never reaches Apple's stub, and fails
    /// plainly, with no word of an install nothing runs.
    /// NEGATIVE CONTROL: the same call with the tools present does `exec` Apple's own tool
    /// (`with_the_tools_present_it_execs_apples_own_tool_off_the_shim`).
    #[test]
    fn with_nothing_installing_the_tools_it_fails_plainly_and_never_runs_a_stub() {
        for tool in ["git", "cc", "python3", "pagestuff", "xcrun"] {
            let fake = Fake {
                path: Some(path_of(&[shim(), at("usr/bin"), at("bin")])),
                ..Fake::default()
            };
            assert_eq!(run_with(&fake, tool, &[]), 69, "{tool}");
            assert!(fake.execs.borrow().is_empty(), "{tool}: ran a stub");
            assert_eq!(
                *fake.warnings.borrow(),
                [aterm_types::clt_shim::missing_line(tool, true)],
                "{tool}"
            );
        }
        let standard = Fake {
            not_admin: true,
            ..Fake::default()
        };
        assert_eq!(run_with(&standard, "git", &[]), 69);
        assert!(standard.execs.borrow().is_empty());
        assert_eq!(
            *standard.warnings.borrow(),
            [aterm_types::clt_shim::missing_line("git", false)],
            "an account that may not install them is not sent to an Install… it lacks"
        );
    }

    /// Apple's stubs reach this shim as `$DEVELOPER_DIR/usr/bin/xcrun git --version`: with the
    /// tools missing its line names `git`, and nothing runs. With them present the same call goes
    /// to Apple's own `xcrun`, words as given: that is how a process that kept the variable from
    /// before the tools arrived keeps working.
    #[test]
    fn a_stubs_call_through_the_developer_dir_names_its_tool_and_runs_nothing_while_missing() {
        let rest = [OsString::from("git"), OsString::from("--version")];
        let missing = Fake::default();
        assert_eq!(run_with(&missing, "xcrun", &rest), 69);
        assert!(missing.execs.borrow().is_empty());
        assert_eq!(
            *missing.warnings.borrow(),
            [aterm_types::clt_shim::missing_line("git", true)]
        );
        let installing = Fake {
            installing: true,
            ..Fake::default()
        };
        assert_eq!(run_with(&installing, "xcrun", &rest), 69);
        assert_eq!(
            *installing.warnings.borrow(),
            [aterm_types::clt_shim::arrival_line("git")]
        );
        let present = Fake {
            present: true,
            path: Some(path_of(&[at("usr/bin"), at("bin")])),
            ..Fake::default()
        };
        let _ = run_with(&present, "xcrun", &rest);
        let execs = present.execs.borrow();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].0, Path::new(STUBS).join("xcrun"));
        assert_eq!(execs[0].2, rest);
    }

    /// With the tools present, a name they do not carry (Apple's stub for it would raise their
    /// install dialog for Xcode) prints one line and exits 69, running nothing.
    #[test]
    fn a_name_the_tools_lack_prints_once_and_runs_nothing() {
        for tool in [
            "pagestuff",
            "cmpdylib",
            "llvm-gcc",
            "llvm-g++",
            "xcodebuild",
        ] {
            let fake = Fake {
                present: true,
                carries: vec!["git", "cc"],
                ..Fake::default()
            };
            assert_eq!(run_with(&fake, tool, &[]), 69, "{tool}");
            assert!(fake.execs.borrow().is_empty(), "{tool}");
            assert_eq!(
                *fake.warnings.borrow(),
                [aterm_types::clt_shim::absent_line(tool)],
                "{tool}"
            );
        }
    }

    /// A shim finds its directory, and so the mark that says the tools are installing, from the
    /// path it was run by (a symlink in the shim directory), never from `$HOME`: a shell whose
    /// `HOME` changed still hears that the tools are on their way rather than reach Apple's stub.
    /// A path that is not the tool's own (the front door run by name) or not absolute falls back
    /// to this account's directory.
    #[test]
    fn the_shim_finds_its_directory_from_the_path_it_ran_by_not_home() {
        let home = || Some(at("Users/elsewhere/.local/state/aterm/clt-shim"));
        assert_eq!(own_dir(Some(shim().join("git")), "git", home), Some(shim()));
        for exe in [
            Some(at("Applications/aterm.app/Contents/MacOS/aterm")),
            Some(PathBuf::from("clt-shim/git")),
            None,
        ] {
            assert_eq!(own_dir(exe.clone(), "git", home), home(), "{exe:?}");
        }

        // On disk: the mark beside the directory the shim ran from says the install is under
        // way, whatever this account's directory holds.
        let tmp = aterm_tempfile::tempdir().unwrap();
        let shim = tmp.path().join("clt-shim");
        std::fs::create_dir_all(&shim).unwrap();
        let live = Live {
            dir: own_dir(Some(shim.join("git")), "git", home),
        };
        assert!(!live.installing(), "no mark yet");
        std::fs::write(aterm_types::clt_shim::installing_mark(&shim), b"").unwrap();
        assert!(live.installing(), "the mark beside its own directory");
        assert_eq!(live.shim_dir(), Some(shim.clone()));
        assert!(!{
            std::fs::remove_file(aterm_types::clt_shim::installing_mark(&shim)).unwrap();
            live.installing()
        });
        // Apple's stub ran the developer directory's `xcrun`: the same directory's mark.
        std::fs::write(aterm_types::clt_shim::installing_mark(&shim), b"").unwrap();
        let through_stub = Live {
            dir: own_dir(Some(shim.join("developer/usr/bin/xcrun")), "xcrun", home),
        };
        assert!(through_stub.installing());
    }

    /// A stand-in Mac on disk: a developer directory carrying `git` alone, a `/usr/bin` of
    /// stand-ins for Apple's tools that note each run, and the shim directory first on PATH. The
    /// real resolver ([`aterm_types::clt_shim::developer_tool_in`]) says what the tools carry;
    /// the stand-in's exec starts the call's program as a child. With the tools present, `git`
    /// runs Apple's own (the stand-in), with the shim off its PATH; `pagestuff`, which the tools
    /// lack, runs nothing. While the tools are missing nothing runs at all, whether or not
    /// anything installs them.
    #[cfg(unix)]
    #[test]
    fn on_a_stand_in_mac_the_shim_runs_only_apples_own_tool_and_never_reenters() {
        struct StandIn {
            present: bool,
            installing: bool,
            developer: PathBuf,
            stubs: PathBuf,
            shim: PathBuf,
            path: OsString,
            ran: RefCell<Vec<PathBuf>>,
        }
        impl ShimHost for StandIn {
            fn tools_present(&self) -> bool {
                self.present
            }
            fn installing(&self) -> bool {
                self.installing
            }
            fn may_install(&self) -> bool {
                true
            }
            fn carries(&self, tool: &str) -> bool {
                aterm_types::clt_shim::developer_tool_in(
                    tool,
                    std::slice::from_ref(&self.developer),
                    Path::is_file,
                )
                .is_some()
            }
            fn apple_tool(&self, tool: &str) -> PathBuf {
                self.stubs.join(tool)
            }
            fn shim_dir(&self) -> Option<PathBuf> {
                Some(self.shim.clone())
            }
            fn exec(
                &self,
                path: &Path,
                tool: &str,
                rest: &[OsString],
                path_var: Option<&OsStr>,
            ) -> std::io::Error {
                self.ran.borrow_mut().push(path.to_owned());
                // A script just written, which a sibling test thread's fork can hold open
                // for a moment (Linux's ETXTBSY).
                let status = aterm_spec::wait::past_text_busy(|| {
                    let mut child = aterm_uds::spawnfd::command(path);
                    child.arg0(tool).args(rest);
                    if let Some(path_var) = path_var {
                        child.env("PATH", path_var);
                    }
                    child.status()
                });
                match status {
                    Ok(status) if status.success() => std::io::Error::other("stand-in: ran"),
                    Ok(status) => std::io::Error::other(format!("stand-in: {status}")),
                    Err(e) => e,
                }
            }
            fn path_var(&self) -> Option<OsString> {
                Some(self.path.clone())
            }
            fn warn(&self, _line: &str) {}
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let developer = root.join("Developer");
        let stubs = root.join("usr-bin");
        let shim = root.join("clt-shim");
        for dir in [developer.join("usr/bin"), stubs.clone(), shim.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let script = |path: &Path, body: &str| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let noted = root.join("noted");
        script(
            &developer.join("usr/bin/git"),
            &format!(
                "printf 'developer git %s\\n' \"$PATH\" >> '{}'",
                noted.display()
            ),
        );
        for stub in ["git", "pagestuff"] {
            script(
                &stubs.join(stub),
                &format!(
                    "printf 'apple {stub} %s\\n' \"$PATH\" >> '{}'",
                    noted.display()
                ),
            );
        }
        let host = StandIn {
            present: true,
            installing: true,
            developer: developer.clone(),
            stubs: stubs.clone(),
            shim: shim.clone(),
            path: OsString::from(format!("{}:{}", shim.display(), stubs.display())),
            ran: RefCell::default(),
        };
        assert_eq!(run_with(&host, "git", &[OsString::from("status")]), 69);
        assert_eq!(run_with(&host, "pagestuff", &[]), 69);
        assert_eq!(*host.ran.borrow(), [stubs.join("git")]);
        let read = || std::fs::read_to_string(&noted).unwrap_or_default();
        assert_eq!(
            read(),
            format!("apple git {}\n", stubs.display()),
            "Apple's own git ran once, the shim off its PATH; pagestuff ran nothing"
        );

        // The tools still installing: nothing runs at all.
        let installing = StandIn {
            present: false,
            ran: RefCell::default(),
            ..host
        };
        assert_eq!(run_with(&installing, "git", &[]), 69);
        assert!(installing.ran.borrow().is_empty());

        // A person's Stop with the tools still missing: still nothing.
        let stopped = StandIn {
            installing: false,
            ..installing
        };
        assert_eq!(run_with(&stopped, "git", &[]), 69);
        assert!(stopped.ran.borrow().is_empty());
        assert_eq!(
            read(),
            format!("apple git {}\n", stubs.display()),
            "after a Stop Apple's stub is still never run"
        );
    }

    /// With the tools installed, a shell opened during the install (the shim stays on its PATH
    /// for as long as that shell lives) builds a C program as one without the shim does: the shim's `cc`,
    /// as the real dispatch resolves it on this Mac, compiles and links `#include <stdio.h>`.
    /// Measured on this Mac: the Command Line Tools' own `usr/bin/cc` run by absolute path with no
    /// `SDKROOT` fails `'stdio.h' file not found` (and a bare link fails `ld: library 'System' not
    /// found`), while `/usr/bin/cc` (which sets the SDK) builds it. Runs only where the Command
    /// Line Tools' own `usr/bin` carries `cc`, so no install stub of a tool they lack can run.
    #[cfg(target_os = "macos")]
    #[test]
    fn with_the_tools_present_the_shims_cc_builds_as_usr_bin_cc_does() {
        struct Mac {
            ran: RefCell<Vec<(PathBuf, Option<i32>, String)>>,
        }
        impl ShimHost for Mac {
            fn tools_present(&self) -> bool {
                aterm_types::clt_shim::developer_git()
            }
            fn installing(&self) -> bool {
                false
            }
            fn may_install(&self) -> bool {
                true
            }
            fn carries(&self, tool: &str) -> bool {
                aterm_types::clt_shim::developer_tool(tool).is_some()
            }
            fn apple_tool(&self, tool: &str) -> PathBuf {
                Path::new("/usr/bin").join(tool)
            }
            fn shim_dir(&self) -> Option<PathBuf> {
                Some(PathBuf::from("/nonexistent/clt-shim"))
            }
            fn exec(
                &self,
                path: &Path,
                tool: &str,
                rest: &[OsString],
                path_var: Option<&OsStr>,
            ) -> std::io::Error {
                // Never a stub for a tool the Command Line Tools lack.
                assert!(
                    Path::new(aterm_types::clt_shim::CLT_DIR)
                        .join("usr/bin")
                        .join(tool)
                        .is_file(),
                    "{tool}: would run a stub of a tool the Command Line Tools lack"
                );
                let mut child = aterm_uds::spawnfd::command(path);
                child.arg0(tool).args(rest).env_remove("SDKROOT");
                if let Some(path_var) = path_var {
                    child.env("PATH", path_var);
                }
                let out = child.output().expect("the tool starts");
                self.ran.borrow_mut().push((
                    path.to_owned(),
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                ));
                std::io::Error::other("stand-in: ran")
            }
            fn path_var(&self) -> Option<OsString> {
                Some(OsString::from("/nonexistent/clt-shim:/usr/bin:/bin"))
            }
            fn warn(&self, _line: &str) {}
        }
        let clt_cc = Path::new(aterm_types::clt_shim::CLT_DIR).join("usr/bin/cc");
        if !clt_cc.is_file() || aterm_types::clt_shim::developer_tool("cc").is_none() {
            eprintln!("no Command Line Tools cc on this Mac: nothing to measure");
            return;
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let source = dir.path().join("t.c");
        let program = dir.path().join("t");
        std::fs::write(
            &source,
            "#include <stdio.h>\nint main(void) { return 0; }\n",
        )
        .unwrap();
        let host = Mac {
            ran: RefCell::default(),
        };
        let _ = run_with(
            &host,
            "cc",
            &[
                OsString::from("-o"),
                program.clone().into_os_string(),
                source.into_os_string(),
            ],
        );
        assert!(
            program.is_file(),
            "the shim's cc built nothing: {:?}",
            host.ran.borrow()
        );
    }
}
