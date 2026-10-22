// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A REVIEWER INHERITS THE ASKING AGENT'S PERMISSIONS (docs/DESIGN-ai-connections-2026-10-16.md
//! §9.1, "inherit the current parent agent's permissions (which is
//! usually YOLO)").
//!
//! A complete readable process walk identifies a person or the nearest supported AI CLI.
//! Only an explicit unambiguous launch mode proves the CLI's level ([`level_of`]); unknown
//! observations, option grammars and configured defaults refuse inherited permissions.
//! A proven person may do anything: [`Level::Yolo`].
//! `[ai] review_permissions` (or `aterm ai review --permissions`) pins a level instead.

use std::path::Path;

use super::connector::{self, Connector};

/// A permission level every AI CLI can express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// No approvals and no sandbox: Claude Code's `--dangerously-skip-permissions`, Codex's
    /// `--dangerously-bypass-approvals-and-sandbox`, agy's `--dangerously-skip-permissions`,
    /// Muse Code's `--yolo`.
    Yolo,
    /// Edits allowed without asking.
    Edits,
    /// Anything that would ask is refused (a one-shot run has nobody to ask).
    Ask,
    /// Read and plan only.
    Plan,
}

impl Level {
    /// Its word.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Level::Yolo => "yolo",
            Level::Edits => "edits",
            Level::Ask => "ask",
            Level::Plan => "plan",
        }
    }

    /// The level a word names.
    #[inline]
    #[must_use]
    pub fn parse(word: &str) -> Option<Level> {
        [Level::Yolo, Level::Edits, Level::Ask, Level::Plan]
            .into_iter()
            .find(|l| l.word() == word)
    }
}

/// Every word `[ai] review_permissions` takes, `inherit` first (its default).
pub const CHOICE_WORDS: [&str; 5] = ["inherit", "yolo", "edits", "ask", "plan"];

/// `[ai] review_permissions`: inherit the asker's, or a pinned level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The asking agent's own level.
    Inherit,
    /// This level, whoever asks.
    Fixed(Level),
}

impl Choice {
    /// Its word.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Choice::Inherit => "inherit",
            Choice::Fixed(l) => l.word(),
        }
    }

    /// The choice a word names.
    #[inline]
    #[must_use]
    pub fn parse(word: &str) -> Option<Choice> {
        if word == "inherit" {
            Some(Choice::Inherit)
        } else {
            Level::parse(word).map(Choice::Fixed)
        }
    }
}

/// What the process walk proved. A missing observation is not a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asker {
    /// Every observed ancestor was readable, through the process-tree root.
    Person,
    /// The nearest identified CLI and an explicit, unambiguous launch mode.
    Agent(&'static str, Level),
    /// An observation, identity, launch grammar or configured mode was unproven.
    Unknown,
}

impl Asker {
    /// The identified agent, where one was proved.
    #[inline]
    #[must_use]
    pub const fn agent(self) -> Option<(&'static str, Level)> {
        match self {
            Self::Agent(id, level) => Some((id, level)),
            _ => None,
        }
    }
}

/// An explicitly named launch mode, never a vendor/project-config default.
/// Unsupported option arity and conflicting modes are unknown. `--` ends options;
/// a consumed option value is never searched for permission-looking words.
#[must_use]
pub fn level_of(id: &str, argv: &[String], _claude_default: Option<&str>) -> Option<Level> {
    let mut level = None;
    let mut no_write = false;
    let mut no_shell = false;
    let mut i = 0;
    while let Some(arg) = argv.get(i) {
        if arg == "--" {
            break;
        }
        let (flag, attached) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
        let is_value = match id {
            "claude" => flag == "--permission-mode",
            "codex" => matches!(flag, "--sandbox" | "-s"),
            "agy" => flag == "--mode",
            "muse" => flag == "--approval-mode",
            _ => return None,
        };
        let next = if is_value {
            let value = match attached {
                Some(v) => v,
                None => {
                    i += 1;
                    argv.get(i)?.as_str()
                }
            };
            Some(match (id, value) {
                ("claude", "bypassPermissions") => Level::Yolo,
                ("claude", "acceptEdits" | "auto")
                | ("codex", "workspace-write")
                | ("agy", "accept-edits" | "auto")
                | ("muse", "never") => Level::Edits,
                ("claude" | "agy", "plan") | ("codex", "read-only") => Level::Plan,
                _ => return None,
            })
        } else {
            match (id, flag, attached) {
                ("claude" | "agy", "--dangerously-skip-permissions", None)
                | ("codex", "--dangerously-bypass-approvals-and-sandbox" | "--yolo", None)
                | ("muse", "--yolo", None) => Some(Level::Yolo),
                ("muse", "--disable-approval", None) => Some(Level::Edits),
                ("muse", "--disable-write", None) => {
                    no_write = true;
                    None
                }
                ("muse", "--disable-shell", None) => {
                    no_shell = true;
                    None
                }
                // Supported inert switches from the connection's own one-shot grammar.
                ("claude", "-p" | "--print" | "--no-session-persistence", None)
                | ("codex", "--skip-git-repo-check" | "--ephemeral", None) => None,
                // These known value-taking options cannot lend their values authority.
                (
                    "claude",
                    "--append-system-prompt"
                    | "--system-prompt"
                    | "--resume"
                    | "--model"
                    | "--output-format",
                    _,
                )
                | ("codex", "--model" | "-m" | "--color" | "--cd" | "-C", _)
                | ("agy", "--print" | "--model" | "--output-format", _)
                | ("muse", "--prompt-file", _) => {
                    if attached.is_none() {
                        i += 1;
                        argv.get(i)?;
                    }
                    None
                }
                // A positional word may precede later options in a vendor's grammar.
                // Guessing at it, or at an unfamiliar flag's arity, proves no mode.
                _ => return None,
            }
        };
        if let Some(next) = next {
            if level.is_some_and(|old| old != next) {
                return None;
            }
            level = Some(next);
        }
        i += 1;
    }
    if no_write || no_shell {
        if !(no_write && no_shell) || level.is_some_and(|l| l != Level::Plan) {
            return None;
        }
        level = Some(Level::Plan);
    }
    level
}

/// The processes above this one. Injected: tests read no real process.
pub trait Procs {
    /// This process's parent.
    fn first(&self) -> Option<u32>;
    /// `pid`'s parent.
    fn parent(&self, pid: u32) -> Option<u32>;
    /// `pid`'s executable path and argv.
    fn args(&self, pid: u32) -> Option<(String, Vec<String>)>;
}

/// The shipped [`Procs`]: the kernel's own records.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveProcs;

impl Procs for LiveProcs {
    fn first(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            Some(std::os::unix::process::parent_id())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let after = stat.rsplit_once(')')?.1;
            after.split_whitespace().nth(1)?.parse().ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            aterm_uds::process::read_process_parent(i32::try_from(pid).ok()?)
        }
    }

    fn args(&self, pid: u32) -> Option<(String, Vec<String>)> {
        let a = atpkg::caller_shell::process_args(pid)?;
        Some((a.exec_path, a.argv))
    }
}

/// How far up the walk goes.
pub const MAX_HOPS: usize = 24;

/// The AI CLI a path runs, by its file name (a managed copy lives at
/// `…/store/<id>/<build>/bin/<id>`, a vendor's own at `…/<id>` or `…/<id>/versions/<v>`).
#[must_use]
pub fn cli_of_path(path: &str) -> Option<&'static Connector> {
    let p = Path::new(path);
    let name = p.file_name()?.to_str()?;
    // `program_of` reads program/status words; this input is one kernel path component.
    if name.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(program) = aterm_phase::reader::program_of(name) {
        return connector::connector(program.name());
    }
    // Claude's own install may name the executable only by its version.
    let parent = p.parent()?;
    if parent.file_name().and_then(|n| n.to_str()) == Some("versions") {
        return cli_of_path(parent.parent()?.file_name()?.to_str()?);
    }
    None
}

/// The nearest supported AI CLI with proven explicit permissions, or a complete
/// readable walk to the root. Runtime/script ambiguity and unreadable/truncated walks
/// are Unknown. This is a launch observation, not a live interactive-mode proof.
#[must_use]
pub fn asker(procs: &dyn Procs, _claude_default: &dyn Fn() -> Option<String>) -> Asker {
    let Some(mut pid) = procs.first() else {
        return Asker::Unknown;
    };
    for _ in 0..MAX_HOPS {
        if pid == 1 {
            return Asker::Person;
        }
        if pid == 0 {
            return Asker::Unknown;
        }
        let Some((exec, argv)) = procs.args(pid) else {
            return Asker::Unknown;
        };
        let Some(exec_name) = Path::new(&exec).file_name().and_then(|n| n.to_str()) else {
            return Asker::Unknown;
        };
        let direct = cli_of_path(&exec).map(|c| (c, 1));
        let runtime = aterm_phase::reader::may_host_agent(exec_name);
        let identified = if runtime {
            // A renamed runtime may put its CLI in argv[0]; conventional node/bun
            // argv keeps the script at argv[1]. No runtime flags are guessed past.
            argv.first()
                .and_then(|a| cli_of_path(a))
                .map(|c| (c, 1))
                .or_else(|| argv.get(1).and_then(|a| cli_of_path(a)).map(|c| (c, 2)))
        } else {
            direct
        };
        if let Some((c, start)) = identified {
            return match level_of(c.id, argv.get(start..).unwrap_or_default(), None) {
                Some(level) => Asker::Agent(c.id, level),
                None => Asker::Unknown,
            };
        }
        // An unresolved script runtime or a known agent without a connection row
        // is not evidence of a human caller (Gemini, for example).
        if runtime || aterm_phase::reader::program_of(exec_name).is_some() {
            return Asker::Unknown;
        }
        let Some(parent) = procs.parent(pid) else {
            return Asker::Unknown;
        };
        pid = parent;
    }
    Asker::Unknown
}

/// Claude Code's `permissions.defaultMode` in the user settings of its home.
#[must_use]
pub fn claude_default_mode(home: &Path) -> Option<String> {
    let text = crate::harness::accounts::read_bounded(&home.join("settings.json"), 1024 * 1024)
        .ok()
        .flatten()?;
    let v: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    v.get("permissions")?
        .get("defaultMode")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
#[path = "permission_tests.rs"]
mod tests;
