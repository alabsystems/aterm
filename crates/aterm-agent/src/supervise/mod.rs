// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor: what a manager agent needs to keep a worker (a Claude Code
//! or Codex session in another aterm tab) moving without rubber-stamping it.
//!
//! * [`classify::classify_command`] — is this shell line read-only? (The
//!   corpus that shaped it is the test.)
//! * [`policy::approval::decide`] — THE approval decider: which option, if
//!   any, to press on an approval box, under which rule and behind which
//!   guard ([`policy`]); a question dialog it hands to
//!   [`policy::question::answer_question`], which answers it under
//!   `[harness] answer_questions` — with its recommended option, or, under
//!   `question_priority` `long-term` / `short-term`, by the worker from that
//!   word's policy text ([`QuestionPriority`]).
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
//!   worker 150 s past its reset, and a share of a minute more per session
//!   (the turn-end policy's usage resume).
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
// The phase reader and the prompt parser live in `aterm-phase` (the fabric
// bridge publishes `phase=` from the same reader), re-exported here.
pub use aterm_phase::{phase, prompt};
pub mod journal;
pub mod ledger;
pub mod ledger_html;
pub mod limit;
pub mod mail;
pub mod memory;
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
pub use policy::approval::QuestionPriority;
pub use report::{DEFAULT_MAX_ROWS, Mark, Marker, Reason, Report, ReportOpts};
pub use run::{
    ATTENTION_OWNER, AnswerOpts, ApprovalEnv, CLAIM_HELD, CLAIM_RENEW, Caps, Ctl, CtlReply,
    EXIT_NO_BOX, EXIT_NOT_SERVED, EXIT_REFUSED, EXIT_TIMEOUT, Fold, Guard, HostStep, IdleHost,
    Interrupter, NoLane, ProbeMiss, ReportBrief, Session, StuckSurvey, SuperviseOpts, Turn,
    UNBOUNDED, WaitPlan, WorkerSource, event_line, exit_reason, render_phase,
    render_phase_and_survey, render_result, render_result_mail, reported_event_line,
};
pub use transport::{Endpoint, RelayCtl, Transport};

/// A scratch path of one call's own, for a test:
/// `<tmp>/aterm-<kind>-<tag>-<pid>-<n>`, `<n>` a per-process call count.
///
/// Unique by construction rather than by callers passing distinct tags: tests
/// run on parallel threads of one process, and a shared helper would hand
/// every caller the same tag, letting one test's clean-up delete another's
/// files. Nothing is created here: the caller makes, and removes, what it
/// needs.
#[cfg(test)]
pub(crate) fn test_scratch_path(kind: &str, tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aterm-{kind}-{tag}-{}-{n}", std::process::id()))
}

/// A scratch directory of ONE call's own, CREATED, and removed when this drops:
/// the guard form of [`test_scratch_path`] (`aterm-<kind>-<tag>-…`), for a
/// helper whose callers hold the directory for the length of a test — a test
/// that fails, or returns early, leaves nothing behind. Dereferences to its
/// path.
///
/// Under `/tmp` on unix rather than `$TMPDIR`: these directories hold the
/// stand-in instances' control sockets, and a unix-socket path must fit in 104
/// bytes, which macOS's per-user temp dir (49 of them) plus a unique name and
/// a label can exceed.
#[cfg(test)]
pub(crate) struct ScratchDir(aterm_tempfile::TempDir);

#[cfg(test)]
impl ScratchDir {
    pub(crate) fn new(kind: &str, tag: &str) -> Self {
        #[cfg(unix)]
        let base = std::path::PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        Self(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-{kind}-{tag}-"))
                .tempdir_in(base)
                .expect("scratch dir"),
        )
    }
}

#[cfg(test)]
impl std::ops::Deref for ScratchDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        self.0.path()
    }
}

#[cfg(test)]
impl AsRef<std::path::Path> for ScratchDir {
    fn as_ref(&self) -> &std::path::Path {
        self.0.path()
    }
}

#[cfg(test)]
mod scratch_tests {
    /// Two tests hand one helper the same tag; the second one's set-up
    /// (`remove_dir_all` + `create_dir_all`) runs while the first holds rows
    /// in its file, then the first cleans up while the second still runs.
    /// Each keeps what it wrote. Negative control: a per-tag name
    /// (`aterm-<kind>-<tag>-<pid>`) loses both.
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
