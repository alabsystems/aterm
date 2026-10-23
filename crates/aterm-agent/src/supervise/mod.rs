// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor: what a manager agent needs to keep a worker (a Claude Code,
//! Codex, agy, Muse Code or Gemini CLI session in another aterm tab) moving
//! without rubber-stamping it. Each is read by its own grammar
//! (`aterm_phase::Program::reader`), Gemini CLI by its screen under `node`,
//! the name its process runs as, and answered by the keys it measured
//! (`aterm_phase::Program::supervisable`): Muse Code's boxes and question by
//! the cursor and Enter, its typed text submitted only once its echo held
//! still. No press or typed line ever answers an agent's sign-in menu, whose
//! options open a browser.
//!
//! * [`classify::classify_command`]: is this shell line read-only?
//! * [`policy::approval::decide`]: which option, if any, to press on an approval
//!   box, under which rule and behind which guard ([`policy`]); a question dialog
//!   goes to [`policy::question::answer_question`], under `[harness]
//!   answer_questions` and `question_priority` ([`QuestionPriority`]).
//! * [`prompt::parse_prompt`]: the approval box on the screen, parsed.
//! * [`phase::worker_phase`]: busy / prompt / limited / idle / question, from one
//!   read; [`phase::survey_open`]: the session survey; [`phase::context_left`]:
//!   context left before auto-compact.
//! * [`run::Session`]: the loop over the control verbs that the window's host
//!   runs on every agent session ([`run::Session::run_hosted`]), and that
//!   `aterm drive phase` and `answer` read and type through.
//! * [`report`]: what the worker said since the manager's turn, with the rows a
//!   fullscreen app scrolled off.
//! * [`limit`]: when a limit notice says it resets, as a Unix time.
//! * [`tap_clock`]: the usage tap's reading of the walled process, which times
//!   the continuation at a usage wall where it names the reset (`[harness]
//!   usage_tap`).
//! * [`occupancy`]: what the harness measures of a session's wait on its own background work
//!   (the hold's clock), under `[harness] occupancy`; [`facts`]: which measured episodes a
//!   session has been told.

pub mod approvals;
pub mod background;
pub mod blocks;
pub mod classify;
pub mod config;
pub mod facts;
pub(crate) mod fx;
pub mod ladder;
// The phase reader and the prompt parser live in `aterm-phase` (the server's
// agent verdict is read by the same reader), re-exported here.
pub use aterm_phase::{phase, prompt};
pub mod journal;
pub mod ledger;
pub mod ledger_html;
pub mod limit;
pub mod memory;
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
pub use journal::{Journal, read_journal};
pub use ledger::{
    ClockAnchor, Format as LedgerFormat, LedgerHost, LedgerOpts, LedgerSource, gather, parse_since,
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

/// A scratch path of one call's own, for a test:
/// `<tmp>/aterm-<kind>-<tag>-<pid>-<n>`, `<n>` a per-process call count.
///
/// Unique by construction: tests run on parallel threads of one process, and a
/// shared tag would let one test's clean-up delete another's files. Nothing is
/// created here.
#[cfg(test)]
pub(crate) fn test_scratch_path(kind: &str, tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aterm-{kind}-{tag}-{}-{n}", std::process::id()))
}

/// `path` canonicalized, for a test: every link resolved, as the policy's probes
/// see it. Windows names a canonical path `\\?\C:\…`, which git takes for no
/// path (a `GIT_CONFIG_GLOBAL`, an `include.path`, a submodule's URL); there it
/// is the drive path it stands for.
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

/// A scratch directory of one call's own, created, and removed on drop (the guard
/// form of [`test_scratch_path`]), so a failing or early-returning test leaves
/// nothing behind. Dereferences to its path.
///
/// Under `/tmp` on unix rather than `$TMPDIR`: it holds stand-in instances'
/// control sockets, and a unix-socket path must fit in 104 bytes, which macOS's
/// 49-byte per-user temp dir plus a unique name can exceed.
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
