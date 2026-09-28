// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness core: the PURE judgments behind the aterm wrapper
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md`), and the read verbs over them.
//! Nothing here types into a session or answers a hook; the one engine that
//! ACTS is [`crate::supervise`]. (The hook-era `rm_policy` went 2026-09-25:
//! the supervisor's rm breaker, `supervise::policy::rm_breaker`, keeps the
//! two tables it reused.)
//!
//! * [`source`] — WHERE DID THIS FACT COME FROM, once: the one [`source::Source`]
//!   vocabulary [`usage`] and `harness limits` print.
//! * [`usage`] — the transcript's usage rows, read into one view; the HUD
//!   line and the `harness usage --json` shape (design §5.2). Its statusLine
//!   reader has had NO live producer since the `harness statusline` bridge
//!   retired (decision B): only the vendor-corpus canary and the
//!   `harness_try` example feed it, so the view's account windows are empty
//!   in every live `harness usage`; deleting it reshapes that verb's JSON
//!   (`windows`), which is the owner's call;
//!   and the `/usage` panel's painted windows `harness limits` prints beside
//!   the wall the engine's own reader names (`aterm_phase::wall`, the one
//!   wall classifier). The second classifier that used to live here
//!   (`limits`: hook values, a pair rule, a banner table that disagreed with
//!   aterm-phase's) was deleted 2026-09-25; its recovery ladder had gone
//!   with its only driver, `harness watch`.
//! * [`disk`] — DISK WATCH AND CLEANUP (design §5.5): the free-space figure
//!   and the stale targets, each row carrying the WITNESS that makes it safe
//!   to remove. Report-only is the shape of the function, not a flag.
//! * [`align`] — the bounded child runner [`disk`] reads `df` through. It
//!   was the packaging contract and the alignment verdict; that part is
//!   deleted (the module doc says why) and the name stays for its caller.
//! * [`footer`] — the Claude Code footer aterm paints in place of the vendor's
//!   permission-mode row: model + effort, working directory, branch, read
//!   from the files Claude Code already keeps (owner direction, 2026-09-24).
//! * [`lights`] — the row of lights at the footer's end (auto-approve, auto
//!   mode, fast mode): read from what Claude Code draws,
//!   toggled through its own inputs, read back after every toggle.
//! * [`cli`] — THE COMMAND: `aterm harness usage|limits|disk|ledger`, the
//!   read views, `upgrade` (one hand-run pass of the live upgrade) and
//!   `upgrade models` (its model priority list), plus the retired hook-bridge
//!   verbs answered as tombstones.
//! * [`upgrade`], [`upgrade_drive`], [`upgrade_wake`] — THE LIVE UPGRADE of a
//!   running Claude Code or Codex onto a newer build: the pure plan, its steps
//!   over one tab or every session, and atpkg's activation notice as a push.
//!   The window's supervisor host takes the steps at each session's idle
//!   points.
//! * [`upgrade_codex`] — the same step's CODEX branch, its pure half (the
//!   daemon rule, the flag table, the exit hint, the rollout and composer
//!   readers); its I/O half is `upgrade_drive`'s `codex` module. Codex's
//!   shared daemon is moved first by the vendor's own verb, then each TUI by
//!   a typed `/exit` and `codex resume` through [`relaunch`]'s line — never a
//!   signal.
//! * [`upgrade_models`], [`upgrade_catalog`] — THE MODEL HALF of the live
//!   upgrade: the priority list (grown from Claude Code's own
//!   recommendations), the managed build's baked catalog, and THE MODEL
//!   LADDER that moves a session onto the newest model of ITS OWN family the
//!   build offers — and only with none newer, up the list for a model nobody
//!   chose; never down — on its relaunch line (`--model`, never `/model`).
//! * [`relaunch`] — THE RELAUNCH PRIMITIVE both the upgrade and relaunch on
//!   exit use: an agent that no longer runs started again in its own tab, on
//!   its own conversation (Claude Code's `--resume`, Codex's `resume`), and
//!   the host's per-session back-off.
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
//! STATUS (docs/README.md honesty ratchet): unit-tested; TEN bounded
//! machines carry a derived model in `aterm-spec` with a Tier-1 bind to the
//! real code — `harness_capture_worker_lifecycle_model` ([`align`]'s runner,
//! in its tests), `harness_upgrade_notice_owner_model` ([`upgrade_drive`]'s
//! tests), `harness_upgrade_drain_bound_model` ([`upgrade::next_step`], in
//! [`upgrade`]'s tests), `harness_upgrade_never_strands_model` (no notice into
//! a session at its usage limit, a late READY honoured, every agent asked
//! restarted or released — a release dropped only once the agent took up
//! direction given after its last READY, never a restart over direction given
//! after it, and no stopped round a permanent wait — each re-armed once it
//! has rested `upgrade::RETRY_S`; one stated exception, an agent no job of a job-control shell,
//! refused and owed no line: [`upgrade_drive`]'s tests, over the real reducer,
//! gates, record transitions, READY, direction and release rules and the
//! window's reading of each step's word), `harness_worker_lifecycle_model` and
//! `harness_relaunch_on_exit_model` ([`relaunch`]'s and the window host's
//! tests), `harness_upgrade_look_model` (the window host's looks at a
//! session over [`upgrade_drive::due`] — never let go for what a look could
//! not read — and its note behind over [`upgrade_drive::note_behind`], the
//! state behind from the worker's attach: the host's reaction in the window
//! host's tests, the classifier's reads in [`upgrade_drive`]'s),
//! `harness_exit_record_model` (what an exit left of Claude's own
//! record, read as it is seen: [`upgrade_drive`]'s tests, over the real
//! [`relaunch::exit_record`] and [`relaunch::after_exit`]),
//! `harness_model_priority_model` ([`upgrade_models`], in
//! `aterm-agent/tests/conformance_upgrade_models/priority.rs`),
//! `harness_model_ladder_model` (WHEN a due model move is taken —
//! [`upgrade_models::model_moves_now`] over every reachable state, in
//! `aterm-agent/tests/conformance_upgrade_models/ladder.rs`),
//! `harness_login_wall_model` (nothing of the upgrade's typed at the login
//! wall, no give-up spent on a notice it answered, no continuation into a
//! login the supervisor saw gone, the owner told first: in
//! `aterm-agent/tests/conformance_login_wall.rs`, over the real readers,
//! reducer and turn-end decider on every reachable state), and
//! `harness_codex_daemon_update_model` ([`upgrade_codex::daemon_step`] over
//! every reachable state, in [`upgrade_codex`]'s tests). The Codex branch
//! has run end to end against a REAL Codex (0.157.0 → 0.157.1, a private
//! headless aterm, network denied) through the window's OWN HOST, no sweep
//! (`tools/test-codex-live-upgrade.sh`, 2026-09-26): a background terminal
//! held a daemon-mode tab and its daemon until it was stopped; an embedded
//! session got its notice at that break and no `/exit` until the terminal
//! was stopped; then the daemon moved, each TUI was exited, relaunched,
//! adopted and carried on, and so was an inline TUI at a two-row prompt. One
//! of three full runs had the embedded session's carry-on still owed ten
//! minutes after its adoption (the cause was not captured; the script now
//! dumps the host's log and journals on a failure). Nothing here has run
//! against a REAL exhausted
//! window: the `/usage` panel reader is exercised against captured and
//! hand-built fixtures.

/// The harness's mark as a literal ([`upgrade::HARNESS_MARK`] is this), for
/// the texts `concat!` builds on it: every turn the harness types into an
/// agent's conversation begins with it.
macro_rules! harness_mark {
    () => {
        "[aterm harness]"
    };
}

pub mod align;
pub mod cli;
pub mod disk;
pub mod footer;
pub mod lights;
pub mod relaunch;
pub mod source;
pub mod upgrade;
pub mod upgrade_catalog;
pub mod upgrade_codex;
pub mod upgrade_drive;
pub mod upgrade_models;
pub mod upgrade_wake;
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
