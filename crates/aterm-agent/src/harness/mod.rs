// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness core: the PURE judgments behind the aterm wrapper
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md`), and the read verbs over them.
//! Nothing here types into a session or answers a hook; the one engine that
//! ACTS is [`crate::supervise`], which calls [`rm_policy`] through its
//! approval policy.
//!
//! * [`rm_policy`] — is this Bash command an `rm` the owner's policy approves?
//!   (design §5.1). Built ON [`crate::supervise::classify`]: the shell
//!   segmentation, quote stripping and wrapper see-through there were each paid
//!   for by a misclassification in a real session, and are not re-derived here.
//! * [`source`] — WHERE DID THIS FACT COME FROM, once: the one [`source::Source`]
//!   vocabulary [`usage`] and [`limits`] print.
//! * [`usage`] — the statusLine JSON and the transcript's usage rows, read into
//!   one view; the HUD line and the `harness usage --json` shape (design §5.2).
//! * [`limits`] — the failure classifier and the ordered recovery table
//!   (design §5.8, `FailureRecovery` in §11). Screen evidence comes from
//!   `aterm_phase::phase::limit_notice` and [`crate::supervise::limit`], the
//!   reset clock the supervisor already reads; neither is re-implemented.
//!   `harness limits` runs [`limits::classify`] over one screen read. The
//!   ladder ([`limits::step`]) lost its only production driver with `watch`
//!   on 2026-09-23; it is tested and model-bound, and reached by nothing.
//! * [`disk`] — DISK WATCH AND CLEANUP (design §5.5): the free-space figure
//!   and the stale targets, each row carrying the WITNESS that makes it safe
//!   to remove. Report-only is the shape of the function, not a flag.
//! * [`align`] — the bounded child runner [`disk`] reads `df` through. It
//!   was the packaging contract and the alignment verdict; that part is
//!   deleted (the module doc says why) and the name stays for its caller.
//! * [`cli`] — THE COMMAND: `aterm harness usage|limits|disk|ledger`, the
//!   read views, plus the retired hook-bridge verbs answered as tombstones.
//!
//! # What was deleted, 2026-09-23
//!
//! The SECOND harness stack: `watch` (the loop), `wire` (its control-socket
//! adapter), `mark` (the four-state presence mark), `observe` (the grid
//! spine), `ring` (the JSONL ledger ring), `profile` (the per-program serving
//! table), `accounts` (the roster and rotation), `config` (the watcher's own
//! `config.toml`), most of `align`, and the `status|mark|enable|disable|
//! align|caps|accounts|liveness|recover|nudge|switch|watch|config` verbs. In
//! its whole life none of its acts landed — its own turn lease blocked its
//! `turn`, `appnotice` was denied before the badge went out, its escalation
//! `post` was malformed, it woke on every repaint, and it labelled every
//! finished turn `api-stalled` (measured by the 2026-09-23 harness audit) —
//! and it duplicated the engine in [`crate::supervise`], which is the one
//! supervisor now. Design §0.4 is the record.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested; TWO bounded
//! machines carry a derived model in `aterm-spec` with a Tier-1 bind to the
//! real code — `harness_failure_recovery_model` ([`limits`], in
//! `aterm-agent/tests/conformance_harness.rs`) and
//! `harness_capture_worker_lifecycle_model` ([`align`]'s runner, in its
//! tests). Nothing here has run against a REAL limit or exhausted window: the
//! ladder is exercised against fixtures.

pub mod align;
pub mod cli;
pub mod disk;
pub mod limits;
pub mod rm_policy;
pub mod source;
pub mod upgrade;
pub mod upgrade_drive;
pub mod usage;

/// The longest prefix of `s` that is at most `max` BYTES and ends on a
/// character boundary — never a split character, whatever the input.
///
/// Every reader in this module quotes text it did not write (a command line,
/// a banner, a transcript field) into a bounded ledger row, and each one used
/// to carry its own copy of this walk-back. This is the one copy; a caller
/// that wants an ellipsis appends its own.
pub(crate) fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    // A `str` is UTF-8, so index 0 is always a boundary: this cannot panic.
    s.get(..end).unwrap_or("")
}

/// A caller's text as ONE bounded line: cut to `max` bytes on a character
/// boundary, then folded through the SHIPPED reader
/// ([`crate::supervise::limit::one_line`]) — every line break and control
/// character a space, runs of spaces one, the ends trimmed.
///
/// One copy, because the fold is the part that is easy to get wrong: a
/// hand-rolled `char::is_control()` sweep is Cc-only and lets `U+2028` LINE
/// SEPARATOR through, and this text is quoted from screens, banners and
/// vendor stdout into status fields, ledger rows and lines typed inside
/// someone else's transcript. Three modules here each had their own wrapper
/// around the shipped reader; this is the wrapper.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    crate::supervise::limit::one_line(truncate_bytes(text, max))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{one_line, truncate_bytes};

    #[test]
    fn one_line_folds_what_a_hand_rolled_control_sweep_misses() {
        assert_eq!(one_line("  a \n b  ", 99), "a b");
        assert_eq!(one_line("with\ta tab", 99), "with a tab");
        // NEGATIVE CONTROL for the whole reason this is shared: `U+2028` is
        // Zl, not Cc, so `char::is_control()` is false for it and a local
        // sweep would pass it through into a "one line" payload.
        assert!(!'\u{2028}'.is_control());
        assert_eq!(one_line("a\u{2028}b", 99), "a b");
        assert_eq!(one_line("a\u{2029}b", 99), "a b");
        // The cut is a BYTE cut on a character boundary, taken before the
        // fold, and it never splits a character.
        assert_eq!(one_line("abcdef", 3), "abc");
        assert_eq!(one_line("é😀", 2), "é");
        assert_eq!(one_line("", 0), "");
    }

    #[test]
    fn truncate_bytes_never_splits_a_character_and_never_grows_a_string() {
        assert_eq!(truncate_bytes("abc", 8), "abc");
        assert_eq!(truncate_bytes("abc", 3), "abc");
        assert_eq!(truncate_bytes("abc", 2), "ab");
        assert_eq!(truncate_bytes("abc", 0), "");
        // NEGATIVE CASES: a cut that lands inside a multi-byte character
        // walks back, and a cut inside a four-byte one walks back three.
        assert_eq!(truncate_bytes("é", 1), "", "a two-byte character, cut at 1");
        assert_eq!(truncate_bytes("aé", 2), "a");
        for max in 0..4 {
            assert_eq!(truncate_bytes("😀", max), "", "max {max}");
        }
        assert_eq!(truncate_bytes("😀", 4), "😀");
        // Every prefix of a mixed string is valid UTF-8 and no longer than
        // asked for — the property the three deleted copies each claimed.
        let mixed = "a😀é b\u{2028}ω";
        for max in 0..=mixed.len() + 2 {
            let cut = truncate_bytes(mixed, max);
            assert!(cut.len() <= max.min(mixed.len()), "max {max}");
            assert!(mixed.starts_with(cut), "max {max}");
        }
    }
}
