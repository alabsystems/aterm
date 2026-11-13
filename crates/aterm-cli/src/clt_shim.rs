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
#[path = "clt_shim_tests.rs"]
mod tests;
