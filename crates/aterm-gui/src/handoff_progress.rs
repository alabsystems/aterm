// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, observational successor progress. Nothing here admits a handoff.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Waiting,
    #[cfg(unix)]
    Readers,
    #[cfg(unix)]
    Proof,
    #[cfg(unix)]
    CommitWaiter,
    #[cfg(unix)]
    Write,
    Written,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Gate fields are a snapshot at the start of one readiness visit. Later phase
/// records retain that snapshot; a concurrent control worker may already have advanced.
pub(crate) struct Record {
    pub phase: Phase,
    pub control_pending: bool,
    pub control_failed: bool,
    pub restore_pending: bool,
    pub manifest_windows: usize,
    pub pending_adoptions: usize,
    pub first_present_pending: bool,
    pub unpainted_windows: usize,
}

#[derive(Default)]
pub(crate) struct Progress {
    last: Option<Record>,
    emitted: u8,
    terminal: u8,
}

impl Progress {
    // An oscillating gate cannot fill the journal indefinitely. Terminal records retain
    // their own slots, so exhausting waiting records does not hide the observed outcome.
    const LIMIT: u8 = 64;

    pub fn note(&mut self, record: Record, emit: &mut impl FnMut(Record)) {
        if self.last == Some(record) {
            return;
        }
        self.last = Some(record);
        let terminal = match record.phase {
            Phase::Written => 1,
            Phase::Rejected => 2,
            _ => 0,
        };
        if terminal != 0 {
            if self.terminal & terminal == 0 {
                self.terminal |= terminal;
                emit(record);
            }
        } else if self.emitted < Self::LIMIT {
            self.emitted = self.emitted.saturating_add(1);
            emit(record);
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

fn identity() -> (u32, &'static Option<aterm_uds::process::ProcessStart>) {
    static START: std::sync::OnceLock<Option<aterm_uds::process::ProcessStart>> =
        std::sync::OnceLock::new();
    let pid = std::process::id();
    let start = START.get_or_init(|| {
        i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_process_start)
    });
    (pid, start)
}

pub(crate) fn log(record: Record) {
    let (pid, start) = identity();
    aterm_log::info!(
        "overlap handoff: progress pid={pid} start={start:?} build={} {record:?}",
        crate::build_info::build_number(),
    );
}

pub(crate) fn milestone(stamp: crate::metrics::ClaimStamp) {
    let (pid, start) = identity();
    aterm_log::info!(
        "overlap handoff: milestone pid={pid} start={start:?} build={} stamp={}",
        crate::build_info::build_number(),
        stamp.as_str(),
    );
}

#[cfg(test)]
#[path = "handoff_progress_tests.rs"]
mod tests;
