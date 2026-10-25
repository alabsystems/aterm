// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rewind fold thread: it lives while a view is open, folds one replay at a time, and arms no timer.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use aterm_core::config::TerminalConfig;
use aterm_core::terminal::Terminal;
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::temporal::{Fold, Judged, ReplayPlan};

/// One replay asked of the thread.
struct Job {
    session: u64,
    serial: u64,
    plan: ReplayPlan,
    judge: Option<ReplayPlan>,
    colors: Option<Arc<TerminalConfig>>,
}

/// A fold the thread finished, for the main thread to show ([`crate::App::rewind_folded`]).
pub(crate) struct Folded {
    pub(crate) session: u64,
    pub(crate) serial: u64,
    /// The replayed engine at the asked instant.
    pub(crate) term: Terminal,
    /// The verdicts of the live witnesses the folds passed, for [`crate::temporal::TemporalRecorder::judge`].
    pub(crate) judged: Vec<Judged>,
}

impl std::fmt::Debug for Folded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Folded")
            .field("session", &self.session)
            .field("serial", &self.serial)
            .field("judged_folds", &self.judged.len())
            .finish_non_exhaustive()
    }
}

/// A running fold thread; dropping it ends the thread once its fold in hand is done.
pub(crate) struct RewindWorker {
    tx: SyncSender<Job>,
}

impl RewindWorker {
    /// Start the thread, which posts each fold to the main thread through `proxy`.
    pub(crate) fn start(proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("aterm-rewind".to_string())
            .spawn(move || run(&rx, &proxy))
            .map_err(|e| format!("cannot start the Rewind thread: {e}"))?;
        Ok(Self { tx })
    }

    /// Ask the thread to fold `plan`, one of `session`'s replays, as fold `serial` drawn in `colors`, and `judge` for its witnesses alone.
    pub(crate) fn fold(
        &self,
        session: u64,
        serial: u64,
        (plan, judge): (ReplayPlan, Option<ReplayPlan>),
        colors: Option<Arc<TerminalConfig>>,
    ) -> Result<(), String> {
        let job = Job {
            session,
            serial,
            plan,
            judge,
            colors,
        };
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("a Rewind fold is already running".to_string()),
            Err(TrySendError::Disconnected(_)) => Err("the Rewind thread stopped".to_string()),
        }
    }
}

fn run(rx: &Receiver<Job>, proxy: &EventLoopProxy<Wake>) {
    while let Ok(job) = rx.recv() {
        let folded = fold(
            job.session,
            job.serial,
            (job.plan, job.judge),
            job.colors.as_deref(),
        );
        if proxy
            .send_event(Wake::RewindFolded(Box::new(folded)))
            .is_err()
        {
            return;
        }
    }
}

/// Fold `plan` to its instant in the session's `colors`, an open mode-2026 frame ended, and `judge` (a fold to the witness that covers the instant) for its verdicts alone.
pub(crate) fn fold(
    session: u64,
    serial: u64,
    (plan, judge): (ReplayPlan, Option<ReplayPlan>),
    colors: Option<&TerminalConfig>,
) -> Folded {
    let Fold { mut term, judged } = plan.fold();
    let mut verdicts = vec![judged];
    verdicts.extend(judge.map(|plan| plan.fold().judged));
    if let Some(colors) = colors {
        let _ = term.apply_config(colors);
    }
    if term.modes().synchronized_output() {
        term.process(b"\x1b[?2026l");
    }
    Folded {
        session,
        serial,
        term,
        judged: verdicts,
    }
}
