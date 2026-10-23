// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Retired landing hand-over ([`HIDDEN_VERB`], [`HandOver`], [`shim_command`], [`sweep`]).

use std::path::PathBuf;

use crate::store::{Layout, ToolName};

/// The hidden verb an older twin's prelude execs.
pub const HIDDEN_VERB: &str = "__landing";

/// The verb's operands, parsed: `<program> [<prefix>] -- [args…]`.
#[derive(Debug, PartialEq, Eq)]
pub struct HandOver<'a> {
    /// The agent program the twin stands for (`claude`), not yet `ToolName`-vetted.
    pub program: &'a str,
    /// The absolute prefix operand the twin passes, so the store is found with no `HOME`.
    pub prefix: Option<PathBuf>,
    /// The user's arguments, verbatim, with the one `--` the twin inserted stripped.
    pub args: &'a [String],
}

impl<'a> HandOver<'a> {
    /// `None` only with no program.
    #[must_use]
    pub fn parse(rest: &'a [String]) -> Option<Self> {
        let (program, rest) = rest.split_first()?;
        let (prefix, rest) = match rest.split_first() {
            Some((p, tail)) if p != "--" && std::path::Path::new(p).is_absolute() => {
                (Some(PathBuf::from(p)), tail)
            }
            _ => (None, rest),
        };
        let args = if rest.first().is_some_and(|a| a == "--") {
            &rest[1..]
        } else {
            rest
        };
        Some(Self {
            program,
            prefix,
            args,
        })
    }
}

/// The command the verb runs ([`Layout::shim`], [`crate::platform::exec_or_run`]).
#[must_use]
pub fn shim_command(
    layout: &Layout,
    tool: &ToolName,
    args: &[String],
) -> aterm_uds::spawnfd::StdCommand {
    #[expect(
        clippy::disallowed_types,
        reason = "a pass-through by exec: its caller's descriptors are the program's"
    )]
    let mut command = std::process::Command::new(layout.shim(tool));
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(args);
    command
}

/// Remove `<prefix>/landing/` and `<prefix>/optin/` and everything in them.
pub fn sweep(layout: &Layout) {
    for dir in [layout.landing_dir(), layout.prefix.join("optin")] {
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.is_dir() => {
                let _ = std::fs::remove_dir_all(&dir);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&dir);
            }
            Err(_) => {}
        }
    }
}

/// The operands and the command, on every platform.
#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn the_hand_over_parses_its_operands_and_runs_the_bin_shim_verbatim() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let prefix_str = if cfg!(windows) {
            "C:\\Program Files (x86)\\aterm\\pkg"
        } else {
            "/tmp/prefix (x86)"
        };
        let rest = a(&["claude", prefix_str, "--", "-p", "--", "50% done", "a b"]);
        let h = HandOver::parse(&rest).unwrap();
        assert_eq!(h.program, "claude");
        assert_eq!(h.prefix.as_deref(), Some(std::path::Path::new(prefix_str)));
        assert_eq!(h.args, &a(&["-p", "--", "50% done", "a b"])[..]);
        let old = a(&["codex", "--", "--", "x"]);
        let h = HandOver::parse(&old).unwrap();
        assert_eq!(h.prefix, None);
        assert_eq!(h.args, &a(&["--", "x"])[..]);
        let rel = a(&["claude", "notes.md"]);
        let h = HandOver::parse(&rel).unwrap();
        assert_eq!(h.prefix, None);
        assert_eq!(h.args, &a(&["notes.md"])[..]);
        assert_eq!(HandOver::parse(&[]), None);
        let layout = Layout {
            prefix: PathBuf::from(prefix_str),
        };
        let tool = ToolName::new("claude").unwrap();
        let args = a(&["-p", "--", "50% done", "a b"]);
        let command = shim_command(&layout, &tool, &args);
        assert_eq!(
            command.get_program(),
            layout.bin_dir().join(tool.shim_file()).as_os_str()
        );
        assert_eq!(
            command.get_program().to_string_lossy().ends_with(".cmd"),
            cfg!(windows),
            "the .cmd wrapper on Windows, the sh shim elsewhere"
        );
        assert!(!command.get_program().to_string_lossy().contains("agents"));
        let got: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, args);
    }
}

/// The sweep over a real directory.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn temp_layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-landing-{label}-"))
    }

    #[test]
    fn the_sweep_removes_every_marker_and_the_directory() {
        let l = temp_layout("sweep");
        let dir = l.landing_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let live = format!(
            "atpkg-landing-v1 build=2026091702 pid={} from=2026091601 version=2.1.274 \
             from_version=2.1.273\n",
            std::process::id()
        );
        std::fs::write(dir.join("claude"), live).unwrap();
        std::fs::write(dir.join("codex"), b"junk\n").unwrap();
        std::fs::write(dir.join(".claude.tmp-4242"), b"").unwrap();
        let optin = l.prefix.join("optin");
        std::fs::create_dir_all(&optin).unwrap();
        std::fs::write(optin.join("vendorx"), b"# opted in\n").unwrap();
        sweep(&l);
        assert!(!dir.exists(), "landing/ is gone");
        assert!(!optin.exists(), "optin/ is gone");
        sweep(&l);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn the_sweep_never_follows_a_link_at_landing() {
        let l = temp_layout("sweep-link");
        let elsewhere = l.prefix.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("claude"), b"keep me\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, l.landing_dir()).unwrap();
        sweep(&l);
        assert!(
            std::fs::symlink_metadata(l.landing_dir()).is_err(),
            "unlinked"
        );
        assert_eq!(
            std::fs::read(elsewhere.join("claude")).unwrap(),
            b"keep me\n",
            "the link's target is not ours"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}

/// The hand-over on Windows: the `bin/<program>.cmd` shim runs as a child and its real exit
/// code comes back. Runs only on a Windows host.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn the_verb_runs_the_bin_shim_as_a_child_with_its_real_code() {
        let p = crate::testdir::dir("atpkg-landing-win-");
        let layout = Layout {
            prefix: p.to_path_buf(),
        };
        let tool = ToolName::new("claude").unwrap();
        let log = layout.prefix.join("log.txt");
        let target = layout
            .build_dir("claude", 2026091601)
            .join("bin\\claude.cmd");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let mut body = String::from("@echo ran %*>> \"");
        body.push_str(&log.to_string_lossy());
        body.push_str("\"\r\n@exit /b 3\r\n");
        std::fs::write(&target, body).unwrap();
        std::fs::create_dir_all(layout.bin_dir()).unwrap();
        crate::platform::install_shim_to(&layout.shim(&tool), &target).unwrap();
        let args = vec![String::from("--"), String::from("x")];
        let st = shim_command(&layout, &tool, &args).status().unwrap();
        assert_eq!(st.code(), Some(3));
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "ran -- x");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
}
