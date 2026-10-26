// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-agent-names**: the one agent name table. aterm-phase's `program_of` reads a
//! program's name through it, and a crate that may not link the screen reader (deny.toml's
//! `aterm-phase` ban) names an agent by it.

/// A coding agent the table names. aterm-phase's `Program` is these and the generic program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Agent {
    Claude,
    Codex,
    /// Google's Antigravity CLI.
    Agy,
    /// Meta's Muse Code.
    Muse,
    /// xAI's Grok Build.
    Grok,
    /// Moonshot AI's Kimi Code.
    Kimi,
}

impl Agent {
    /// Display name used on harness surfaces.
    #[inline]
    #[must_use]
    pub const fn product(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Agy => "Antigravity CLI",
            Agent::Muse => "Muse Code",
            Agent::Grok => "Grok Build",
            Agent::Kimi => "Kimi Code",
        }
    }
}

/// Map a program command string or shell-wrapped command to the [`Agent`] it names, if any.
#[must_use]
pub fn agent_of(program: &str) -> Option<Agent> {
    let word = program_word(program);
    let is = |name: &str| word.eq_ignore_ascii_case(name);
    if is("claude") || is("claude-code") {
        Some(Agent::Claude)
    } else if is("codex") || is("codex-old") {
        Some(Agent::Codex)
    } else if is("agy") {
        Some(Agent::Agy)
    } else if is("muse") || is_muse_build(word) {
        Some(Agent::Muse)
    } else if is("grok") || is("grok-alab") {
        // grok-alab, the Grok Build harness, leads the session's foreground group and runs the
        // vendor's grok as its child on the same terminal: the screen is Grok's
        // (`docs/DESIGN-harness-repos-2026-10-24.md` §8.1 item 5).
        Some(Agent::Grok)
    } else if is("kimi") || is("kimi-code") {
        Some(Agent::Kimi)
    } else {
        None
    }
}

/// `muse-bin-1.4.10-R6427.1`: the build Meta's installer's launcher execs
/// (`muse-bin-` and a version that opens on a digit), case aside.
fn is_muse_build(word: &str) -> bool {
    word.get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("muse-bin-"))
        && word[9..].starts_with(|c: char| c.is_ascii_digit())
}

/// The script runtimes an agent runs under without renaming itself (an npm or bun install of
/// Claude Code starts as `node` or `bun`): a program so named may be an agent, which
/// aterm-phase's `identify` tells by its screen.
pub const AGENT_RUNTIMES: &[&str] = &["node", "bun", "deno"];

/// Whether `program` is one of the [`AGENT_RUNTIMES`], which may host an agent.
#[must_use]
pub fn may_host_agent(program: &str) -> bool {
    let word = program_word(program);
    AGENT_RUNTIMES
        .iter()
        .any(|runtime| word.eq_ignore_ascii_case(runtime))
}

/// Borrowed basename of `program`'s first word with any leading login-shell `-` stripped.
#[must_use]
pub fn program_word(program: &str) -> &str {
    let head = program.split_whitespace().next().unwrap_or(program);
    let base = head.rsplit('/').next().unwrap_or(head);
    base.trim_start_matches('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program word matches without case, from a path and past a login shell's `-`, and is
    /// borrowed, never rebuilt: the GUI asks it of every pane on every frame.
    #[test]
    fn program_words_match_without_case_from_a_path_and_a_login_dash() {
        for (program, want) in [
            ("codex", Some(Agent::Codex)),
            ("CODEX", Some(Agent::Codex)),
            ("/usr/local/bin/Claude --resume", Some(Agent::Claude)),
            ("-Claude-Code", Some(Agent::Claude)),
            ("Agy", Some(Agent::Agy)),
            ("MUSE x", Some(Agent::Muse)),
            (
                "/Users//x/.local/bin/muse-bin-1.4.10-R6427.1",
                Some(Agent::Muse),
            ),
            ("muse-bin-", None),
            ("muse-bin-latest", None),
            ("muse-binary", None),
            ("codexx", None),
            ("codex-old", Some(Agent::Codex)),
            (
                "/opt/pkg/store/codex-old/1000000000163000000/bin/codex-old resume",
                Some(Agent::Codex),
            ),
            ("CODEX-OLD", Some(Agent::Codex)),
            ("codex-older", None),
            ("xcodex-old", None),
            ("codex-", None),
            ("grok", Some(Agent::Grok)),
            ("grok-alab", Some(Agent::Grok)),
            (
                "/opt/pkg/bin/grok-alab run -- --resume x",
                Some(Agent::Grok),
            ),
            ("Grok-Alab", Some(Agent::Grok)),
            ("grok-alabs", None),
            ("grok-", None),
            ("kimi-alab", None),
            ("claude-codes", None),
            ("node", None),
            ("", None),
        ] {
            assert_eq!(agent_of(program), want, "{program:?}");
        }
        assert!(may_host_agent("Node"));
        assert!(may_host_agent("/opt/homebrew/bin/BUN run x"));
        assert!(!may_host_agent("nodes"));
        assert!(!may_host_agent("codex"));
    }
}
