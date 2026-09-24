// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LANDING HAND-OVER, RETIRED TO A PASS-THROUGH (Phase 2 of
//! `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`, 2026-09-22).
//!
//! From 2026-09-16 an update pass wrote `<prefix>/landing/<program>` while it fetched a
//! newer agent build, and the `agents/` twin handed a `claude` typed meanwhile to
//! `atpkg __landing`, which waited up to 45 s and printed progress lines on stderr. Owner,
//! 2026-09-22: updates must be *"non-interrupting (ideally silent)"*. The wait bought
//! nothing the flip does not: activation re-lays `bin/` and the twin atomically, so the
//! next invocation already runs the new build, and one typed during the transfer runs
//! the build the user had.
//!
//! What remains, and why:
//!
//! * **No pass writes a marker**, and every pass SWEEPS `landing/` ([`sweep`], run where
//!   the pass takes the store lock): that pass is the only one, so a marker there is an
//!   older client's leftover, and a twin that still tests for it then runs its own `exec`.
//! * **The hidden verb stays** ([`HIDDEN_VERB`], `cli::cmd_landing`): a twin laid by an
//!   older client still hands over to it while such a marker stands. It `exec`s the
//!   current `bin/<program>` at once with the arguments verbatim ([`HandOver`],
//!   [`shim_command`]) and prints nothing.
//! * **New twins carry no landing prelude** (`crate::platform::twin_prelude`), so the
//!   next pass re-lays an older twin without it; the self-update block stays.

use std::path::PathBuf;

use crate::store::{Layout, ToolName};

/// The hidden verb an older twin's prelude execs: `atpkg __landing <program> -- <args…>`.
/// Unlisted, dispatched before the verb match and before the store lock, like
/// `__pending` and `__reroute`.
pub const HIDDEN_VERB: &str = "__landing";

/// The verb's operands, parsed: `<program> [<prefix>] -- [args…]` — the pure half of
/// `cli::cmd_landing`, so what the verb would run is pinned on every platform.
#[derive(Debug, PartialEq, Eq)]
pub struct HandOver<'a> {
    /// The agent program the twin stands for (`claude`), not yet `ToolName`-vetted.
    pub program: &'a str,
    /// The PREFIX operand the twin's prelude passes (absolute), so the store is found
    /// with no `HOME`; `None` from a twin laid before the operand existed.
    pub prefix: Option<PathBuf>,
    /// The user's arguments, verbatim, with the ONE `--` the twin inserted stripped.
    pub args: &'a [String],
}

impl<'a> HandOver<'a> {
    /// `None` only with no program at all. The prefix is taken when the next operand is
    /// an ABSOLUTE path (a relative one is the user's first argument, as is `--`); the
    /// twin inserts exactly one `--` of its own, so exactly one is stripped and a user's
    /// own leading `--` survives.
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

/// The command the verb runs: the CURRENT `bin/<program>` shim ([`Layout::shim`] —
/// `bin/claude` on Unix, `bin\claude.cmd` on Windows), never the `agents/` twin (its
/// prelude would hand over here again), with `args` verbatim. Built here, apart from the
/// `exec`, so the argument vector is pinned on every platform; `cli::cmd_landing` hands
/// it to [`crate::platform::exec_or_run`].
#[must_use]
pub fn shim_command(layout: &Layout, tool: &ToolName, args: &[String]) -> std::process::Command {
    let mut command = std::process::Command::new(layout.shim(tool));
    command.args(args);
    command
}

/// Remove `<prefix>/landing/` and everything in it — silent, best-effort. Run by every
/// pass once it holds the store lock: no other pass is running, and no pass of this
/// client writes a marker. A link or a file standing at `landing/` is unlinked, never
/// followed (`remove_dir_all` does not follow links either).
pub fn sweep(layout: &Layout) {
    let dir = layout.landing_dir();
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

/// The operands and the command, on every platform.
#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn the_hidden_verb_keeps_its_name() {
        assert_eq!(HIDDEN_VERB, "__landing");
    }

    /// THE VERB'S OPERANDS AND WHAT IT RUNS (pinned on every platform, 2026-09-17): the
    /// program, the absolute prefix operand, exactly one `--` stripped, the user's own
    /// `--` and everything after it verbatim; and the command is the `bin/` shim of the
    /// platform (`.cmd` on Windows) with those arguments — never the `agents/` twin.
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
        // A twin from before the prefix operand: `--` right after the program.
        let old = a(&["codex", "--", "--", "x"]);
        let h = HandOver::parse(&old).unwrap();
        assert_eq!(h.prefix, None);
        assert_eq!(h.args, &a(&["--", "x"])[..]);
        // A relative first argument is the user's, not a prefix; no `--` strips nothing.
        let rel = a(&["claude", "notes.md"]);
        let h = HandOver::parse(&rel).unwrap();
        assert_eq!(h.prefix, None);
        assert_eq!(h.args, &a(&["notes.md"])[..]);
        assert_eq!(HandOver::parse(&[]), None);
        // The command: the bin/ shim of this platform, the arguments verbatim.
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

/// The sweep over a real directory: markers of any shape, and a link where `landing/`
/// should be — unlinked, its target untouched.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn temp_layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-landing-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Layout { prefix: p }
    }

    /// EVERY MARKER GOES, WHOEVER WROTE IT (Phase 2, 2026-09-22): a live pid's marker in
    /// the 2026-09-16 format, garbage, a stray temp — the sweep leaves no `landing/` at
    /// all, and a second sweep over nothing is silent.
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
        sweep(&l);
        assert!(!dir.exists(), "landing/ is gone");
        sweep(&l);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A LINK AT `landing/` IS UNLINKED, NEVER FOLLOWED: the directory it names keeps
    /// every file.
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

/// THE VERB'S HAND-OVER ON WINDOWS — compiled and run ONLY on a Windows host, which no
/// machine of this repo is: the `bin/<program>.cmd` shim runs as a child and its REAL
/// exit code comes back. UNVERIFIED until a Windows box runs it.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn the_verb_runs_the_bin_shim_as_a_child_with_its_real_code() {
        let p = std::env::temp_dir().join(format!("atpkg-landing-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        let layout = Layout { prefix: p };
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
