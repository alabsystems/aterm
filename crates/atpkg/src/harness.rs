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

/// Whether the default set holds index program `program`: every program but a harness.
///
/// A harness installs only when a person names it (`aterm pkg install <program>`) and is
/// kept current once installed, as a vendor row outside the default set is
/// ([`crate::vendor_direct::VendorSpec::default_set`]). Nothing runs one until aterm serves
/// harness contract v3, so a harness joins the default set in that aterm change, once it
/// has a signed release (`docs/DESIGN-harness-repos-2026-10-24.md` §11 D17).
#[must_use]
pub const fn in_default_set(program: &str) -> bool {
    !is_harness(program)
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
            assert!(!in_default_set(h), "{h} installs only when named");
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
            assert!(in_default_set(p), "{p:?} stays in the default set");
        }
        const _: () = assert!(is_harness("x-alab") && !is_harness("-alab"));
    }
}
