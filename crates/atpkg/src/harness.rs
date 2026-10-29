// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! ALAB HARNESS PROGRAMS: `<agent>-alab`, one program per vendor agent, each in its own
//! repo with its own version, running the vendor's unmodified CLI as its child
//! (`docs/DESIGN-harness-repos-2026-10-24.md`).
//!
//! The one-word rule (that page's §1) makes a harness's repo, program and holder id one
//! word ending in [`SUFFIX`], so the name alone says what a program is and no list has to
//! be kept. codex-alab is not one of them here: its program is `codex`, which ALab builds
//! from source (repo `codex-alab`, [`crate::alab::ALAB_AGENTS`]).
//!
//! A harness is in the default set like every program the index lists
//! ([`crate::manifest::Index::installable`]): the passes install it unasked and keep it current,
//! and only `[packages].exclude` or a person's uninstall keeps it out (owner ruling 2026-10-27,
//! "everything must be ON BY DEFAULT in atpkg!!!", that page's §11 D45). The name still decides
//! two things: no `alab-<tool>` alias is laid for a harness (D13), and only the harness
//! [`twin_harness`] names runs a vendor's sessions.

/// The suffix the one-word rule gives every harness program.
pub const SUFFIX: &str = "-alab";

/// Whether `program` names an ALab harness program: a non-empty word, then [`SUFFIX`].
#[must_use]
pub const fn is_harness(program: &str) -> bool {
    let name = program.as_bytes();
    let tail = SUFFIX.as_bytes();
    if name.len() <= tail.len() {
        return false;
    }
    let at = name.len() - tail.len();
    let mut i = 0;
    while i < tail.len() {
        if name[at + i] != tail[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The harness a vendor program's `agents/` twin hands its session to once that harness is
/// installed, as `(program, tool)`: the harness that serves aterm's harness contract 3 for that
/// agent (`crates/aterm-agent/src/harness/contract.rs`, `ROSTER`;
/// `docs/DESIGN-harness-repos-2026-10-24.md` §9 step 6). Only grok-alab today: a harness joins
/// here in the aterm change that rosters it, never before (its claim would be refused and it
/// would only run the vendor in its place).
#[must_use]
pub const fn twin_harness(vendor: &str) -> Option<(&'static str, &'static str)> {
    match vendor.as_bytes() {
        b"grok" => Some(("grok-alab", "grok-alab")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE FIVE HARNESSES ARE HARNESSES AND NOTHING ELSE IS: the vendor programs they wrap,
    /// codex (built from source, not a harness here), the toolchain programs and a bare
    /// suffix are not.
    #[test]
    fn a_harness_is_a_word_then_the_suffix() {
        for h in [
            "claude-code-alab",
            "muse-alab",
            "agy-alab",
            "grok-alab",
            "kimi-alab",
        ] {
            assert!(is_harness(h), "{h}");
        }
        for p in [
            "claude",
            "muse",
            "agy",
            "grok",
            "kimi",
            "codex",
            "codex-old",
            "ay",
            "trust",
            "trust-vc",
            "alab",
            "-alab",
            "alab-ay",
            "muse-alab2",
            "",
        ] {
            assert!(!is_harness(p), "{p:?}");
        }
        const _: () = assert!(is_harness("x-alab") && !is_harness("-alab"));
    }

    /// Only grok's twin hands its session to a harness, grok-alab, which is a harness program;
    /// no other vendor's does yet, and no harness's own name does.
    #[test]
    fn only_grok_hands_its_session_to_grok_alab() {
        assert_eq!(twin_harness("grok"), Some(("grok-alab", "grok-alab")));
        assert!(is_harness("grok-alab"));
        for other in [
            "claude",
            "codex",
            "codex-old",
            "agy",
            "muse",
            "kimi",
            "grok-alab",
            "Grok",
            "",
        ] {
            assert_eq!(twin_harness(other), None, "{other:?}");
        }
    }
}
