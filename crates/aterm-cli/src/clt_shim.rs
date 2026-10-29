// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim's dispatch: what the one `aterm` binary does when it is run as
//! `git`, `cc`, `clang`, `make`, `python3` or another of [`aterm_types::clt_shim::SHIM_TOOLS`]
//! through a shim symlink, while Apple's Command Line Tools are being installed (System State
//! design §6.3).
//!
//! The shim directory ([`crate::clt_shim`]'s symlinks) sits ahead of `/usr/bin` only while the
//! tools are missing, so the common case is: the tools are still installing, and the shim prints
//! one line and exits [`aterm_types::clt_shim::SHIM_EXIT`] at once — it never runs `/usr/bin/git`,
//! whose `xcode-select` stub would raise macOS's install dialog. A tab opened before the tools
//! arrived keeps the shim on its PATH; that shim `exec`s the real tool instead, so a long-lived
//! shell works the moment the install finishes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::clt_shim::{SHIM_EXIT, arrival_line};

/// Where `base.clt` installs the real tools: the presence of this directory's `git` means the
/// Command Line Tools arrived. A fast stat, never a subprocess that could raise the dialog the
/// shim exists to avoid.
const CLT_BIN: &str = "/Library/Developer/CommandLineTools/usr/bin";

/// The seams the dispatch reaches the Mac through, so a test drives it with no real tools and no
/// real `exec`.
trait ShimHost {
    /// Whether Apple's Command Line Tools are installed now.
    fn tools_present(&self) -> bool;
    /// Replace this process with the real tool at `path`, under `argv[0]` `tool` and `rest`.
    /// Returns only if the `exec` failed (the error).
    fn exec(&self, path: &Path, tool: &str, rest: &[OsString]) -> std::io::Error;
    /// Print `line` to standard error.
    fn warn(&self, line: &str);
}

/// Serve a shim invoked as `tool` (its `argv[0]`) with `rest` (its `argv[1..]`).
#[must_use = "return the shim's exit status from the front door"]
pub fn run(tool: &str, rest: Vec<OsString>) -> ExitCode {
    ExitCode::from(run_with(&Live, tool, &rest))
}

/// The dispatch over its seams, returning the exit byte so a test can read it (`ExitCode` is
/// opaque). On a real `Live` host with the tools present the `exec` never returns; the byte is
/// reached only when it fails, or when the tools are missing.
fn run_with(host: &dyn ShimHost, tool: &str, rest: &[OsString]) -> u8 {
    if host.tools_present() {
        // This tab opened before the tools arrived; hand the call to the real tool, now at
        // `/usr/bin/<tool>` (its `xcode-select` stub dispatches to it). Exec by absolute path,
        // so the shim directory on PATH is never a loop.
        let real = PathBuf::from("/usr/bin").join(tool);
        let error = host.exec(&real, tool, rest);
        host.warn(&format!("{}: {error}", real.display()));
        return SHIM_EXIT;
    }
    // The tools are still installing: say so and exit, never raising macOS's dialog.
    host.warn(&arrival_line(tool));
    SHIM_EXIT
}

struct Live;

impl ShimHost for Live {
    fn tools_present(&self) -> bool {
        Path::new(CLT_BIN).join("git").exists()
    }

    #[cfg(unix)]
    fn exec(&self, path: &Path, tool: &str, rest: &[OsString]) -> std::io::Error {
        aterm_uds::spawnfd::command(path)
            .arg0(tool)
            .args(rest)
            .exec()
    }

    /// Off Unix there are no `xcode-select` stubs to stand in for and the GUI never lays the
    /// shim ([`crate::clt_shim`]'s `refresh` is a no-op off Unix), so a shim symlink is never on
    /// any PATH and this is never reached. The portable `Command` carries no `exec`, so there is
    /// nothing to replace this process with; report it unsupported (the caller then prints the
    /// arrival line and exits), keeping the dispatch compiling on every target.
    #[cfg(not(unix))]
    fn exec(&self, _path: &Path, _tool: &str, _rest: &[OsString]) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the Command Line Tools shim is macOS-only",
        )
    }

    fn warn(&self, line: &str) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        present: bool,
        warnings: RefCell<Vec<String>>,
        execs: RefCell<Vec<(PathBuf, String, Vec<OsString>)>>,
    }

    impl ShimHost for Fake {
        fn tools_present(&self) -> bool {
            self.present
        }

        fn exec(&self, path: &Path, tool: &str, rest: &[OsString]) -> std::io::Error {
            self.execs
                .borrow_mut()
                .push((path.to_owned(), tool.to_owned(), rest.to_vec()));
            std::io::Error::other("test: no real exec")
        }

        fn warn(&self, line: &str) {
            self.warnings.borrow_mut().push(line.to_owned());
        }
    }

    #[test]
    fn with_the_tools_missing_it_prints_once_exits_69_and_spawns_nothing() {
        for tool in aterm_types::clt_shim::SHIM_TOOLS {
            let fake = Fake::default();
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

    /// The negative control: with the tools present, the same call `exec`s the real tool instead
    /// of printing. (On the old world there was no dispatch at all; this asserts the branch.)
    #[test]
    fn with_the_tools_present_it_execs_the_real_tool_by_absolute_path() {
        let fake = Fake {
            present: true,
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
        assert_eq!(execs[0].0, PathBuf::from("/usr/bin/git"));
        assert_eq!(execs[0].1, "git", "argv0 is preserved");
        assert_eq!(
            execs[0].2,
            vec![OsString::from("status"), OsString::from("-s")]
        );
    }
}
