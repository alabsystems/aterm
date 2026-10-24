// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor: what a manager agent needs to keep a worker moving without rubber-stamping it.

pub mod approvals;
pub mod background;
pub mod blocks;
pub mod classify;
pub mod config;
pub mod facts;
pub(crate) mod fx;
pub mod ladder;
pub use aterm_phase::{phase, prompt};
pub mod journal;
pub mod ledger;
pub mod ledger_html;
pub mod limit;
pub mod memory;
pub mod mode_words;
pub mod occupancy;
pub mod policy;
pub mod report;
pub mod run;
pub mod screen;
pub mod tap_clock;
pub mod tape;
pub mod transport;

pub use background::{BackgroundCall, BackgroundEvidence};
pub use blocks::View;
pub use classify::{DEFAULT_PYTHON_ALLOW, classify_command_with};
pub use config::{Autopilot, LandAt, Occupancy, SupervisorConfig};
pub use facts::{Fact, FactsReading};
pub use journal::{Journal, read_journal};
pub use ledger::{
    ANCHOR_REMOVED, Format as LedgerFormat, LedgerHost, LedgerOpts, gather, parse_since,
    render as render_ledger,
};
pub use phase::{
    Busy, Phase, Zone, busy_signal, context_left, is_placeholder, limit_notice, survey_open,
    transcript_end, worker_phase,
};
pub use policy::approval::QuestionPriority;
pub use policy::turn_end::WaitKind;
pub use report::{DEFAULT_MAX_ROWS, Mark, Marker, Reason, Report, ReportOpts};
pub use run::{
    ATTENTION_OWNER, ATTENTION_PREFIX, AnswerOpts, ApprovalEnv, CLAIM_HELD, CLAIM_RENEW, Caps, Ctl,
    CtlReply, EXIT_NO_BOX, EXIT_NOT_SERVED, EXIT_REFUSED, EXIT_TIMEOUT, Guard, HELD_HEAD, HostStep,
    IdleHost, IdlePoint, Interrupter, NEEDS_ATTENTION_OWNER, ProbeMiss, ProbeNo,
    RELAUNCH_ATTENTION_OWNER, STARTUP_HEAD, Session, SessionSetup, StallSeen, StuckSurvey,
    SuperviseOpts, Turn, UNBOUNDED, UsageSeen, WaitPlan, WorkerSource, deadline_attention,
    event_line, exit_reason, mark_task_exited, render_phase, render_phase_and_survey,
};
pub use transport::{Endpoint, RelayCtl};

/// A scratch path of one call's own, for a test.
#[cfg(test)]
pub(crate) fn test_scratch_path(kind: &str, tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aterm-{kind}-{tag}-{}-{n}", std::process::id()))
}

/// `path` canonicalized, for a test: every link resolved, as the policy's probes see it.
#[cfg(test)]
pub(crate) fn test_canonical(path: &std::path::Path) -> std::path::PathBuf {
    let real = std::fs::canonicalize(path).expect("canonical");
    let drive = real
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|d| cfg!(windows) && !d.starts_with(r"UNC\"))
        .map(std::path::PathBuf::from);
    drive.unwrap_or(real)
}

/// A scratch directory of one call's own, created, and removed on drop (the guard form of
/// [`test_scratch_path`]), so a failing or early-returning test leaves nothing behind.
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
