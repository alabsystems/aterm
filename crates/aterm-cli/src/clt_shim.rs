// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim's dispatch: what the one `aterm` binary does when it is run as
//! `git`, `cc`, `clang`, `make`, `python3` or another of [`aterm_types::clt_shim::SHIM_TOOLS`]
//! through a shim symlink, while Apple's Command Line Tools are being installed (System State
//! design §6.3).
//!
//! The shim directory (the window's symlinks) is on new shells' PATH, ahead of `/usr/bin`, only
//! while a setup pass is installing the tools and they are missing, so the common case is: the
//! tools are still installing, and the shim prints one line and exits
//! [`aterm_types::clt_shim::SHIM_EXIT`] at once — it never runs `/usr/bin/git`, whose
//! `xcode-select` stub would raise macOS's install dialog. A shell opened before the tools
//! arrived keeps the shim on its PATH (bash even hashes its path, which is why the directory
//! stays until the next launch); once the tools are there that shim `exec`s Apple's own
//! `/usr/bin/<tool>`, with the shim directory taken off the PATH it hands on, so a long-lived
//! shell works the moment the install finishes: with the tools in place Apple's front end runs
//! the developer directory's tool with the SDK set (`cc` finds `<stdio.h>`, `ld` finds
//! `libSystem`), raises no dialog, and `xcrun`, which no developer directory carries, runs as it
//! is ([`aterm_types::clt_shim::carried`]). For a name the tools do not carry (`pagestuff`,
//! `llvm-gcc`) the stub would raise Apple's dialog for Xcode, so the shim says so in one line
//! and exits instead. Once nothing installs the tools (a person's Stop, the Developer group
//! turned off, an install that ended unfinished, or an account that may not install them, with
//! the tools still missing: the window's [`aterm_types::clt_shim::wanted_mark`] is gone) the
//! shim hands the call to Apple's own `/usr/bin/<tool>` too: Apple's route, which is the only
//! one left.
//!
//! The shim finds its directory, and so the mark beside it, from the path it was run by (the
//! symlink's own, which macOS keeps as the program's path), never from `$HOME`: a shell that
//! changed `HOME` still reaches a shim that knows the install is under way.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::clt_shim::{SHIM_EXIT, absent_line, arrival_line};

/// The seams the dispatch reaches the Mac through, so a test drives it with no real tools and no
/// real `exec`.
trait ShimHost {
    /// Whether Apple's developer tools are installed now ([`aterm_types::clt_shim::developer_git`]).
    fn tools_present(&self) -> bool;
    /// Whether a setup pass is installing the tools now: the mark beside this shim's directory
    /// ([`aterm_types::clt_shim::wanted_mark`]).
    fn installing(&self) -> bool;
    /// Whether a developer directory carries `tool` ([`aterm_types::clt_shim::developer_tool`]),
    /// so Apple's `/usr/bin/<tool>` has a tool to run once the tools are present.
    fn carries(&self, tool: &str) -> bool;
    /// Apple's own `/usr/bin/<tool>`, which raises their install dialog while the tools are
    /// missing: run only once they are present (for a tool they carry) or nothing installs
    /// them.
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
/// fails, when the tools carry no such tool, or while they install.
fn run_with(host: &dyn ShimHost, tool: &str, rest: &[OsString]) -> u8 {
    if host.tools_present() {
        // This shell opened before the tools arrived. Apple's front end runs a tool they carry
        // with the SDK set and raises no dialog now; for one they lack it would raise Apple's
        // dialog for Xcode.
        if !aterm_types::clt_shim::carried(tool, |tool| host.carries(tool)) {
            host.warn(&absent_line(tool));
            return SHIM_EXIT;
        }
    } else if host.installing() {
        // The tools are still installing: say so and exit, never raising macOS's dialog.
        host.warn(&arrival_line(tool));
        return SHIM_EXIT;
    }
    // The tools are here, or nothing installs them (a Stop, the Developer group turned off, an
    // install that ended unfinished): Apple's own tool.
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

/// The directory a shim run as `tool` lives in: the parent of `exe`, the path it was run by
/// (macOS keeps a symlink's own path as the program's path, by `PATH` lookup and by a relative
/// path alike), when that path's file name is `tool` and it is absolute; else this account's shim
/// directory ([`aterm_types::clt_shim::shim_dir`], `fallback`). Never `$HOME`'s alone: a shell
/// that changed `HOME` still finds the directory it runs from.
fn own_dir(
    exe: Option<PathBuf>,
    tool: &str,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    exe.filter(|exe| exe.is_absolute() && exe.file_name() == Some(OsStr::new(tool)))
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
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
            .is_some_and(|dir| aterm_types::clt_shim::wanted_mark(dir).is_file())
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
        /// Whether a setup pass is installing the tools (the window's mark).
        installing: bool,
        /// The tools the developer directory carries, by name.
        carries: Vec<&'static str>,
        path: Option<OsString>,
        warnings: RefCell<Vec<String>>,
        execs: RefCell<Vec<Exec>>,
    }

    const SHIM: &str = "/Users//alice/.local/state/aterm/clt-shim";

    /// Where the fake keeps Apple's own tools: never run, only named.
    const STUBS: &str = "/stand-in/usr/bin";

    impl ShimHost for Fake {
        fn tools_present(&self) -> bool {
            self.present
        }

        fn installing(&self) -> bool {
            self.installing
        }

        fn carries(&self, tool: &str) -> bool {
            self.carries.contains(&tool)
        }

        fn apple_tool(&self, tool: &str) -> PathBuf {
            Path::new(STUBS).join(tool)
        }

        fn shim_dir(&self) -> Option<PathBuf> {
            Some(PathBuf::from(SHIM))
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
            path: Some(OsString::from(format!(
                "/opt/homebrew/bin:{SHIM}:/usr/bin:/bin"
            ))),
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
            execs[0].3.as_deref(),
            Some(OsStr::new("/opt/homebrew/bin:/usr/bin:/bin")),
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
            path: Some(OsString::from(format!("{SHIM}:/usr/bin:/bin"))),
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
        assert_eq!(execs[0].3.as_deref(), Some(OsStr::new("/usr/bin:/bin")));
    }

    /// A person's Stop (or the Developer group turned off) with the tools still missing leaves
    /// nothing installing them: a shell that still has the shim on its PATH gets Apple's own
    /// `/usr/bin/<tool>`, the route the person chose, with the shim off the PATH it hands on so
    /// nothing comes back here, and no line of the shim's own.
    #[test]
    fn with_nothing_installing_the_tools_it_hands_the_call_to_apples_own_tool() {
        for tool in ["git", "cc", "python3", "pagestuff"] {
            let fake = Fake {
                path: Some(OsString::from(format!("{SHIM}:/usr/bin:/bin"))),
                ..Fake::default()
            };
            assert_eq!(
                run_with(&fake, tool, &[OsString::from("--version")]),
                69,
                "{tool}: the exec fake fails, so the byte is reached"
            );
            let execs = fake.execs.borrow();
            assert_eq!(execs.len(), 1, "{tool}");
            assert_eq!(execs[0].0, Path::new(STUBS).join(tool), "{tool}");
            assert_eq!(execs[0].1, *tool, "{tool}: argv0 is preserved");
            assert_eq!(
                execs[0].3.as_deref(),
                Some(OsStr::new("/usr/bin:/bin")),
                "{tool}: the shim is off the PATH Apple's tool gets"
            );
            let warnings = fake.warnings.borrow();
            assert!(
                warnings.iter().all(|line| !line.contains("arrives with")),
                "{tool}: no word of an install nothing runs: {warnings:?}"
            );
        }
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
        let home = || {
            Some(PathBuf::from(
                "/Users//elsewhere/.local/state/aterm/clt-shim",
            ))
        };
        assert_eq!(
            own_dir(Some(PathBuf::from(format!("{SHIM}/git"))), "git", home),
            Some(PathBuf::from(SHIM))
        );
        for exe in [
            Some(PathBuf::from(
                "/Applications/aterm.app/Contents/MacOS/aterm",
            )),
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
        std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
        assert!(live.installing(), "the mark beside its own directory");
        assert_eq!(live.shim_dir(), Some(shim));
    }

    /// A stand-in Mac on disk: a developer directory carrying `git` alone, a `/usr/bin` of
    /// stand-ins for Apple's tools that note each run, and the shim directory first on PATH. The
    /// real resolver ([`aterm_types::clt_shim::developer_tool_in`]) says what the tools carry;
    /// the stand-in's exec starts the call's program as a child. With the tools present, `git`
    /// runs Apple's own (the stand-in), with the shim off its PATH; `pagestuff`, which the tools
    /// lack, runs nothing. While the tools install nothing runs at all. Once nothing installs
    /// them (a Stop), Apple's own runs again, with the shim off its PATH: nothing can lead back
    /// to the shim.
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

        // A person's Stop with the tools still missing: Apple's own git runs, once, with the
        // shim off its PATH.
        let stopped = StandIn {
            installing: false,
            ..installing
        };
        assert_eq!(run_with(&stopped, "git", &[]), 69);
        assert_eq!(*stopped.ran.borrow(), [stubs.join("git")]);
        assert_eq!(
            read(),
            format!("apple git {0}\napple git {0}\n", stubs.display()),
            "after a Stop the call goes to Apple's own git, never back to the shim"
        );
    }

    /// With the tools installed, a shell opened during the install (the shim stays on its PATH
    /// until the next launch) builds a C program as one without the shim does: the shim's `cc`,
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
