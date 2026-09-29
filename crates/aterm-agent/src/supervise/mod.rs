// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor: what a manager agent needs to keep a WORKER (an agent
//! session — Claude Code or Codex — in another aterm tab) moving without
//! rubber-stamping it. Three pure,
//! unit-tested judgments and one loop:
//!
//! * [`classify::classify_command`] — is this shell line read-only? (The
//!   corpus that shaped it is the test.)
//! * [`policy::approval::decide`] — THE approval decider: which option, if
//!   any, to press on an approval box, under which rule and behind which
//!   guard ([`policy`]); a question dialog it hands to
//!   [`policy::question::answer_question`], which answers it with its
//!   recommended option under `[harness] answer_questions`.
//! * [`prompt::parse_prompt`] — the approval box on the screen, parsed.
//! * [`phase::worker_phase`] — busy / prompt / limited / idle / question,
//!   from one read (busy only from the live zone around the composer);
//!   [`phase::survey_open`] — the session survey parked above it;
//!   [`phase::context_left`] — how much context is left before auto-compact.
//!   (Both modules are the `aterm-phase` crate, re-exported. aterm's
//!   server reads the screen with the same crate and publishes `agent=`;
//!   the fabric bridge relays that word and reads no screen of its own.)
//! * [`run::Session`] — `await-turn`, `supervise` and `watch`, over the
//!   control verbs.
//! * [`report`] — `report`: what the worker said since the manager's turn,
//!   the rows a fullscreen app scrolled off (`offscreen`) joined with the
//!   screen's.
//! * [`mail`] — `watch --mail`'s lane on the manager's inbox and `task`: the
//!   worker's report comes by mail, one line per worker turn.
//! * [`limit`] — when a limit notice says it resets, as a Unix time: the
//!   watcher waits for it, stretches its budget past it and continues the
//!   worker a minute past its reset (the turn-end policy's usage resume).
//! * [`codex_usage`] — what Codex's own rollouts say of a session: how much
//!   of its usage window is spent (the rate-limit nudge is answered by it),
//!   and whether its thread fell into a sandbox its launch bypassed.
//!
//! It moved here from a scratchpad script because every rule in it was paid for
//! by a misclassification in a real session; a supervisor that is itself an
//! agent should not have to rediscover them.

pub mod approvals;
pub mod blocks;
pub mod classify;
pub mod codex_usage;
pub mod config;
pub mod ladder;
// THE PHASE READER AND THE PROMPT PARSER LIVE IN `aterm-phase` since round 13
// (the fabric bridge publishes `phase=` from the same reader `aterm drive phase`
// prints), re-exported here under the paths they always had.
pub use aterm_phase::{phase, prompt};
pub mod journal;
pub mod ledger;
pub mod ledger_html;
pub mod limit;
pub mod mail;
pub mod policy;
pub mod report;
pub mod run;
pub mod screen;
pub mod transport;

pub use blocks::View;
pub use classify::{DEFAULT_PYTHON_ALLOW, classify_command_with};
pub use config::SupervisorConfig;
pub use journal::{Journal, read_journal};
pub use ledger::{
    ClockAnchor, Format as LedgerFormat, LedgerHost, LedgerOpts, gather, parse_since,
    render as render_ledger,
};
pub use mail::{DEFAULT_IDLE_GRACE, DEFAULT_REPORT_WINDOW, MailOpts, TaskOpts, task};
pub use phase::{
    Busy, Phase, Zone, busy_signal, context_left, is_placeholder, limit_notice, survey_open,
    transcript_end, worker_phase,
};
pub use report::{DEFAULT_MAX_ROWS, Mark, Marker, Reason, Report, ReportOpts};
pub use run::{
    ATTENTION_OWNER, AnswerOpts, ApprovalEnv, CLAIM_HELD, CLAIM_RENEW, Caps, Ctl, CtlReply,
    EXIT_NO_BOX, EXIT_NOT_SERVED, EXIT_REFUSED, EXIT_TIMEOUT, Fold, Guard, HostStep, IdleHost,
    Interrupter, NoLane, ReportBrief, Session, SuperviseOpts, Turn, UNBOUNDED, WorkerSource,
    event_line, exit_reason, render_phase, render_phase_and_survey, render_result,
    render_result_mail, reported_event_line,
};
pub use transport::{Endpoint, RelayCtl, Transport};

/// A scratch path of ONE call's own, for a test:
/// `<tmp>/aterm-<kind>-<tag>-<pid>-<n>`, `<n>` a per-process call count.
///
/// A test's scratch file is unique by construction, never by its callers
/// agreeing to pass distinct tags: the harness runs tests on parallel threads
/// of one process, and a shared helper hands every caller its own tag. Measured
/// 2026-09-28 (main `37b992a3c`): `run_engine_tests.rs`'s `hosted_with_host`,
/// behind 19 hosted-loop tests, named each one's approval ledger
/// `aterm-ledger-idle-host-<pid>` and cleared it with `remove_dir_all`, so one
/// test deleted another's ledger under its running loop (3 of 8 runs of the
/// engine module: `…/aterm-ledger-idle-host-<pid>/s-1.jsonl: cannot open it`),
/// and under load `create_dir_all` lost the race outright — its `mkdir` saw
/// the other test's directory, a third test removed it before the `is_dir`
/// check, and it returned `AlreadyExists` (`a_turn_the_host_typed_is_no_short_turn_of_the_workers`
/// failed on that `expect`). `harness/upgrade_codex_drive_tests.rs`'s
/// `scratch` paid for the same lesson first. Nothing is created here: the
/// caller makes, and removes, what it needs.
#[cfg(test)]
pub(crate) fn test_scratch_path(kind: &str, tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aterm-{kind}-{tag}-{}-{n}", std::process::id()))
}

#[cfg(test)]
mod scratch_tests {
    /// THE LOSING INTERLEAVING, forced: two tests hand one helper the same tag
    /// (as every hosted-loop test did through `hosted_with_host`). The second
    /// one's set-up (`remove_dir_all` + `create_dir_all`) runs while the first
    /// holds rows in its file, then the first cleans up while the second still
    /// runs. Each keeps what it wrote, and the other's clean-up leaves its
    /// directory standing. NEGATIVE CONTROL: the per-tag name the helpers used
    /// before (`aterm-<kind>-<tag>-<pid>`) loses both.
    #[test]
    fn two_callers_of_one_tag_never_share_a_scratch_path() {
        let set_up = |dir: &std::path::Path| {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).expect("tmp dir");
            dir.join("s-1.jsonl")
        };
        let lost = |first: &std::path::Path, second: &std::path::Path| {
            let a = set_up(first);
            std::fs::write(&a, "{\"decision\":\"approved\"}\n").expect("write");
            let b = set_up(second);
            let a_kept = std::fs::read_to_string(&a).is_ok_and(|t| t.contains("approved"));
            let _ = std::fs::remove_dir_all(first);
            let b_kept = b.parent().is_some_and(std::path::Path::is_dir);
            let _ = std::fs::remove_dir_all(second);
            (!a_kept, !b_kept)
        };
        let first = super::test_scratch_path("scratch-race", "same");
        let second = super::test_scratch_path("scratch-race", "same");
        assert_ne!(first, second, "one tag, two callers, two paths");
        assert_eq!(
            lost(&first, &second),
            (false, false),
            "{first:?} {second:?}"
        );

        let per_tag =
            std::env::temp_dir().join(format!("aterm-scratch-race-per-tag-{}", std::process::id()));
        assert_eq!(
            lost(&per_tag, &per_tag),
            (true, true),
            "the per-tag name is shared, and the race is lost both ways"
        );
    }
}
