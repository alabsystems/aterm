// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One logical recovery outlives individual, immutable native budgets. Quiet
//! time never clears an explicit cancellation or program-reaching replacement.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Debug)]
pub(super) struct IntentFence {
    reached: u64,
    signalled: AtomicBool,
    cancellation: AtomicU8,
    durable: AtomicBool,
}

impl IntentFence {
    pub(super) fn new(ctx: &crate::SessionCtx) -> Self {
        Self {
            reached: ctx.human_input.counts().reached,
            signalled: AtomicBool::new(false),
            cancellation: AtomicU8::new(0),
            durable: AtomicBool::new(false),
        }
    }

    pub(super) fn did_signal(&self) {
        self.signalled.store(true, Ordering::Release);
    }

    pub(super) fn current(&self, ctx: &crate::SessionCtx) -> bool {
        if self.signalled.load(Ordering::Acquire)
            && ctx.human_input.counts().reached != self.reached
        {
            self.cancel(renewal::CancellationKind::NewPersonTask);
        }
        self.reason().is_none()
    }

    pub(super) fn cancel(&self, kind: renewal::CancellationKind) {
        let code = match kind {
            renewal::CancellationKind::OwnerStop => 1,
            renewal::CancellationKind::NewPersonTask => 2,
            renewal::CancellationKind::ChangedRecord => 3,
            renewal::CancellationKind::ChangedSelection => 4,
            renewal::CancellationKind::PolicyRevoked => 5,
        };
        let _first =
            self.cancellation
                .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(super) fn reason(&self) -> Option<renewal::CancellationKind> {
        match self.cancellation.load(Ordering::Acquire) {
            0 => None,
            1 => Some(renewal::CancellationKind::OwnerStop),
            2 => Some(renewal::CancellationKind::NewPersonTask),
            3 => Some(renewal::CancellationKind::ChangedRecord),
            4 => Some(renewal::CancellationKind::ChangedSelection),
            _ => Some(renewal::CancellationKind::PolicyRevoked),
        }
    }

    pub(super) fn cancellation_persisted(&self) {
        self.durable.store(true, Ordering::Release);
    }

    pub(super) fn durably_cancelled(&self) -> bool {
        self.durable.load(Ordering::Acquire) && self.reason().is_some()
    }
}

#[derive(Clone, Debug)]
pub(super) struct RecordWitness {
    pub(super) home: std::path::PathBuf,
    pub(super) pid: u32,
    pub(super) starts: u64,
    pub(super) goal: aterm_json::Value,
}

impl RecordWitness {
    pub(super) fn capture(
        copy: &self_claimed::Copy,
        record: &aterm_json::Value,
        receipt: &Receipt,
    ) -> Option<Self> {
        if copy.pid != receipt.old_pid || !receipt.intent.matches_record(record, Some(&receipt.id))
        {
            return None;
        }
        Some(Self {
            home: copy.home.clone(),
            pid: copy.pid,
            starts: receipt.intent.marker(record)["starts"].as_u64()?,
            goal: receipt.intent.marker(record)["goalId"].clone(),
        })
    }

    /// The live validated path is retained before Signal. A host-initiated
    /// SIGTERM may publish an exit status, but cannot change the wall or goal.
    pub(super) fn current(&self, receipt: &Receipt) -> bool {
        let Some(mut record) = renewal::retained_native_record(&self.home, self.pid) else {
            return false;
        };
        if record["exit"].as_str() == Some("signal:15") {
            let Some(object) = record.as_object_mut() else {
                return false;
            };
            object.insert("status".into(), aterm_json::json!("waiting"));
        }
        receipt.intent.matches_record(&record, Some(&receipt.id))
            && receipt.intent.marker(&record)["goalId"] == self.goal
            && receipt.intent.marker(&record)["starts"].as_u64() == Some(self.starts)
            && renewal::history(&self.home, &receipt.intent).is_some_and(|history| {
                receipt.intent.matches_history(&history)
                    && history.latest_is_wall
                    && history.starts == self.starts
            })
    }
}
