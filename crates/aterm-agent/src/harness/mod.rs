// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness core: the PURE judgments behind the aterm wrapper
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md`), kept free of every seam that
//! needs an owner ruling (the design's D1, D1' and D6): nothing here talks to a
//! control socket, answers a hook, types into a session or paints a status bar.
//! A host inside `aterm-gui` (TARGET, design §4.1) will call these; until then
//! they are libraries with tests.
//!
//! * [`accounts`] — THE ACCOUNT ROSTER (design §5.6): `accounts.toml`, the
//!   two non-secret facts each config directory's own `.claude.json` already
//!   carries, and §5.6's selection rule — lowest seven-day utilisation among
//!   accounts whose five-hour window is not exhausted, ties by priority, and
//!   never an account whose last observed state is `login-expired` (§5.8.10).
//!   NO credential is read, stored or forwarded anywhere in it. It is what
//!   fills [`watch::WatchConfig`]'s `accounts_enabled` and `account_dir`;
//!   before it existed nothing did, so a rotation was refused
//!   `refused:unresolved` while its table row read as live. The ROTATION
//!   ITSELF is refused `refused:unsupported` as of 2026-09-22: no control
//!   verb carries an environment into a new session, so §5.6's mechanism (a
//!   `spawn` of the twin whose PRELUDE selects `CLAUDE_CONFIG_DIR`) waits on
//!   the §4.1 host. The selection is real and printed; the act is not
//!   available, and the ledger now says which.
//! * [`align`] — THE PACKAGING CONTRACT AND THE ALIGNMENT VERDICT (design
//!   §1.2, §3.1-§3.8): `harness.toml` admitted fail-closed and whole-file, the
//!   probes of §3.2 run against the installed program, and the per-capability
//!   verdict that comes out — degradation rather than all-or-nothing, and the
//!   signed × local intersection that can only ever narrow. The row it keys is
//!   spelled in `atpkg::harness`; this module owns the WIRE word.
//! * [`observe`] — THE GRID SPINE: harness events read from aterm's own view
//!   of a session (`status`, the parsed grid, `offscreen`, `search`) with NO
//!   vendor hook in the path (design §0.2, §4.2, §5.8.1). It is the stage
//!   every other capability stands on, and the answer to the owner's central
//!   objection: `--bare` removes hooks, plugins and the statusLine in one
//!   flag, and an ADOPTED session owns no shell-integration block, so a
//!   capability that only works when a hook fires is the wrong shape.
//! * [`ring`] — the bounded, append-only JSONL ledger every capability writes
//!   (design §4.4, `LedgerRing` in §11).
//! * [`rm_policy`] — is this Bash command an `rm` the owner's policy approves?
//!   (design §5.1). Built ON [`crate::supervise::classify`]: the shell
//!   segmentation, quote stripping and wrapper see-through there were each paid
//!   for by a misclassification in a real session, and are not re-derived here.
//! * [`source`] — WHERE DID THIS FACT COME FROM, once: the one [`source::Source`]
//!   vocabulary every surface prints, and the [`source::SourceSet`] an event
//!   carries when more than one read produced it. It replaced four separate
//!   spellings of the same question (a window's authority, a read verb's
//!   answer, an event's provenance, an evidence channel) in which the word
//!   `grid` was four unrelated types and one surface printed `spine` where
//!   another printed `grid` for the identical fact.
//! * [`usage`] — the statusLine JSON and the transcript's usage rows, read into
//!   one view; the HUD line and the `harness usage --json` shape (design §5.2).
//! * [`limits`] — the failure classifier and the ordered recovery table
//!   (design §5.8, `FailureRecovery` in §11). Screen evidence comes from
//!   `aterm_phase::phase::limit_notice` and [`crate::supervise::limit`], the
//!   reset clock the supervisor already reads; neither is re-implemented.
//!
//! * [`mark`] — PRESENCE AND CONTROL (design §4.6): the four-state mark
//!   (`armed`/`acting`/`degraded`/`bypassed`) that every surface renders the
//!   same, the in-band attribution helper every actuation uses so a line
//!   inside the vendor's transcript names the harness and its rule id, and
//!   the per-capability off switches. The mark reports the HOST; the hook
//!   channel is reported BESIDE it, never folded into it, because a harness
//!   with zero hooks is alive.
//! * [`watch`] — THE WATCH LOOP (design §5.3, §5.4, §5.6, §5.8, §5.8.10): the
//!   actuator and the timing over that spine. Ingress is a PARKED subscribe
//!   stream plus the `await` family for deadlines — there is no sleep and no
//!   cadence in it. That sentence was true of the CODE and false of the
//!   BEHAVIOUR until 2026-09-22, and both halves are worth keeping in mind:
//!   nothing parked, because every arm's condition was `await seq 0` (the
//!   stream is `events`, which carries no `DELTA`, so the one writer of
//!   `last_seq` never fired) and because a deadline already in the past was
//!   clamped to `timeout=1` rather than dropped. A loop with no sleep in it
//!   can still be a poll; what stops it is a predicate that is not already
//!   true and a floor that is not one millisecond.
//!   Every act obeys §4.3's order: journal a row before,
//!   take the lease, respect `hold`, the inject floor and the generation,
//!   write the verdict after. It never types `y` at an approval and never
//!   presses Enter bare.
//! * [`disk`] — DISK WATCH AND CLEANUP (design §5.5): the free-space figure
//!   and the stale targets, each row carrying the WITNESS that makes it safe
//!   to remove. Report-only is the shape of the function, not a flag: removal
//!   needs a named safelist class, a second fence checks every path that
//!   reaches the filesystem, a refusal is journalled as a denial row, and
//!   nothing under the transcripts root is removable under any flag.
//! * [`profile`] — PER-PROGRAM PROFILES (design §6.1-§6.3): the unit of
//!   adaptation, and the proof that none of the above is a Claude Code
//!   script. Three profiles ship — claude, codex and emacs — and they differ
//!   as far as three programs can: a native hook channel, a trust-gated one,
//!   and none at all. The serving table — per capability, whether a program
//!   can serve it and WHY NOT — is AUTHORED AND REACHED ONLY BY
//!   [`profile::identify`], which is the frame test that names the program on
//!   the grid; `harness caps` renders from the alignment report and the
//!   contract's own rows and consults no profile, so no per-program "why
//!   not" is printed anywhere today. Corrected 2026-09-22: this paragraph
//!   claimed the printing. The spine above IS unchanged by any of it, which
//!   is the claim the tests discharge from real codex and emacs sessions.
//! * [`config`] — THE HARNESS'S OWN `config.toml` (design §3.7, §5.8.7): a
//!   CLOSED registry of keys, so an unknown one is refused by name rather
//!   than ignored, and every write is validated against §5.8.7's per-class
//!   allowed sets before it is committed. That is what makes §11's table
//!   invariants hold by construction rather than by default.
//! * [`wire`] — THE ADAPTER: [`observe::Introspect`] and [`watch::Wire`]
//!   against a real aterm, over two control connections — one parked on
//!   `subscribe … events`, one carrying requests — with every deadline armed
//!   as an `await` on a third, cancellable connection. No sleep, no cadence.
//! * [`cli`] — THE COMMAND: `aterm harness hook|statusline|install|uninstall|
//!   status|mark|enable|disable|align|caps|usage|accounts|limits|liveness|
//!   recover|nudge|switch|watch|config|disk|ledger` (design §4.5, §5.7),
//!   which is `cli`'s own `USAGE` and is read from it rather than recalled —
//!   this list said eight when the parser accepted seventeen. It is the hook
//!   bridge, the read verbs and the two verbs that ACT, and it is where the
//!   central law becomes checkable from the outside: every read verb answers
//!   with ZERO hooks installed and carries a `source` field saying so.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested; THREE bounded
//! machines carry a derived model in `aterm-spec` and a Tier-1 bind in
//! `aterm-agent/tests/conformance_harness.rs` that drives the real code —
//! `harness_ledger_ring_model` ([`ring`]), `harness_failure_recovery_model`
//! ([`limits`]) and `harness_turn_observation_model` ([`observe`]), each with
//! a negative control. [`cli`] is routed at the front door (`aterm harness`);
//! the control-socket verbs of design §5.7 (`aterm ctl … harness …`) do not
//! exist yet. Nothing here has run against a REAL limit, exhausted window or
//! expired login: the ladder is exercised against fixtures and a fake wire.
//!
//! TWO seams are still AUTHORED AND UNREACHED, and saying so is the honest
//! reading of design §4.1's TARGET host rather than a scheduling detail.
//!
//! The FIRST is [`profile`]'s serving table above — `cap_rows`,
//! `contract_toml`, `render_tree_text`, `serves` and `runnable` have no
//! caller outside that module and its tests. The SECOND is the
//! `atpkg` harness family — the §3.8 row words and the §1.3 paths — is written
//! and rendered nowhere. It has ONE home as of 2026-09-22, `atpkg::harness`,
//! and that module's own doc is where the decision is stated: which pass would
//! write those rows, which ceremony would write those files, and the one
//! function that IS reached (`clear_sidecar`, from `store::discard_build`).
//! Nothing here restates it, so the two cannot drift.
//!
//! The other two are reached as of 2026-09-22. [`observe::Introspect`] and
//! [`watch::Wire`] have a production implementor ([`wire::CtlWire`]), and
//! [`watch::Watcher::pump`] has a verb (`aterm harness watch`); one hand-asked
//! act runs through the same actuator by way of `aterm harness switch`, which
//! calls [`watch::Watcher::execute`] — the second half of `pump` itself, split
//! out rather than copied, so a manual act can reach nothing the loop could
//! not. What remains TARGET on that path is the host inside `aterm-gui`: these
//! verbs are a process an operator or an agent starts, not a supervisor the
//! window runs on its own.

pub mod accounts;
pub mod align;
pub mod cli;
pub mod config;
pub mod disk;
pub mod limits;
pub mod mark;
pub mod observe;
pub mod profile;
pub mod ring;
pub mod rm_policy;
pub mod source;
pub mod usage;
pub mod watch;
pub mod wire;

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
