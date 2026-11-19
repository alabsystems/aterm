// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Heavy runs finish their ordinary tree rows before asking for run resources. This changes
//! neither their source context nor their reporting path: one scheduler executes one plan.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::ladder::Outcome;
use crate::plan::StageId;
use crate::{Check, RunPlan, report};

const TREE: [(StageId, Check); 4] = [
    (StageId::Census, Check::Census),
    (StageId::ReleasedShim, Check::ReleasedShim),
    (StageId::Formatting, Check::Fmt),
    (StageId::ExportScan, Check::ExportScan),
];

type Acquired = (Duration, Result<(), String>);
type Acquire<'a> = dyn Fn(&RunPlan<'_>, Duration) -> Acquired + Sync + 'a;

/// Admit run-wide resources once, before any stage-local claim. The caller retains the actual
/// turn and disk guards through normal cleanup; this boundary retains only their outcome and
/// the turn's actual wait. Concurrent compiler stages share even a refusal, never retry it.
pub(super) struct Admission<'a> {
    acquire: &'a Acquire<'a>,
    acquired: Mutex<Option<Acquired>>,
}

impl<'a> Admission<'a> {
    pub(super) fn new(acquire: &'a Acquire<'a>) -> Self {
        Self {
            acquire,
            acquired: Mutex::new(None),
        }
    }

    /// Tippy normally overlaps formatting. Only a deferred heavy run orders every later stage
    /// behind all four tree rows; it preserves every original lane and prerequisite.
    pub(super) fn prepare(plan: &mut RunPlan<'_>) {
        for stage in &mut plan.stages {
            if !TREE.iter().any(|(id, _)| *id == stage.id) {
                for (id, _) in TREE {
                    if !stage.after_stages.contains(&id) {
                        stage.after_stages.push(id);
                    }
                }
            }
        }
    }

    /// This is called after the scheduler's tree dependencies, before `staged` can take CPU,
    /// memory or quiet-machine resources. Incomplete or unanswered preflight is never admission
    /// authority. Existing named skips remain skips, as in the ordinary verifier.
    pub(super) fn before(
        &self,
        plan: &RunPlan<'_>,
        id: StageId,
        results: &[report::CheckResult],
        elapsed: Duration,
    ) -> Result<(), String> {
        if TREE.iter().any(|(tree, _)| *tree == id) {
            return Ok(());
        }
        if let Some(why) = crate::ended(plan.context()) {
            return Err(why);
        }
        for (_, check) in TREE {
            if !results.iter().any(|result| {
                result.check == check && matches!(result.outcome, Outcome::Ok | Outcome::Skip)
            }) {
                return Err(format!(
                    "the tree preflight did not complete {} successfully; no run resources were requested",
                    check.name()
                ));
            }
        }
        self.acquire(plan, elapsed)
    }

    pub(super) fn acquire(&self, plan: &RunPlan<'_>, elapsed: Duration) -> Result<(), String> {
        let mut acquired = self.acquired.lock().map_err(|_| {
            "run resource admission panicked before recording its outcome; it will not be retried".to_owned()
        })?;
        acquired
            .get_or_insert_with(|| (self.acquire)(plan, elapsed))
            .1
            .clone()
    }

    pub(super) fn waited(&self) -> Duration {
        self.acquired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(Duration::ZERO, |(waited, _)| *waited)
    }
}
