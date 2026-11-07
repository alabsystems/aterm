// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Failed launch custody retains the actual receipts. No error path upgrades
//! an observed identity or a lost ACK into a no-child or release proof.
use super::*;

pub(super) fn stage_refusal(
    mut events: job::Events,
    reason: &str,
) -> Option<(job::Events, String)> {
    if events.requires_accounting() {
        Some((events, reason.to_owned()))
    } else {
        let _ = events.emit(aterm_types::resource_admission::Event::Refused {
            reason: reason.into(),
        });
        None
    }
}
impl Owner {
    pub(super) fn tick_failed(&mut self) {
        self.failed.retain_mut(|failed| {
            if failed.settled {
                return failed
                    .refusal
                    .as_mut()
                    .is_some_and(|(events, _)| !events.drained());
            }
            if failed
                .checked_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
            {
                return true;
            }
            failed.checked_at = Some(Instant::now());
            if let Some(proof) = failed.no_child.take() {
                match failed.custody.as_mut() {
                    Some(Custody::Fixed(custody)) => {
                        if let Err(error) = custody.cancel(&self.fence, *proof) {
                            failed.no_child = error.into_parts().1.map(Box::new);
                        }
                    }
                    Some(Custody::Cooperative(claim)) => {
                        if self.fence.check_claim(claim).is_err() {
                            failed.no_child = Some(proof);
                        } else {
                            let _ = claim.cancel_cohort(*proof);
                        }
                    }
                    None => {
                        failed.no_child = Some(proof);
                        return true;
                    }
                }
            }
            if let Some(spawned) = failed.spawned.as_mut() {
                match failed.custody.as_mut() {
                    Some(Custody::Fixed(custody)) => {
                        let _ = custody.retry_bind(&self.fence, spawned);
                    }
                    Some(Custody::Cooperative(claim)) => {
                        let _ = claim.bind_cohort(spawned);
                    }
                    None => return true,
                }
                // This child was created stopped at exec and was never resumed.
                // Only its original owned handle is used, never a saved PID.
                let _ = spawned.child_mut().kill();
                let _ = spawned.child_mut().try_wait();
            }
            let finished = match failed.custody.as_mut() {
                Some(Custody::Fixed(custody)) => custody.try_finish(&self.fence),
                Some(Custody::Cooperative(claim)) => claim.finish_cohort(),
                None => Ok(()), // Lexical no-ARM with no admitted custody.
            };
            if finished.is_err()
                || failed
                    ._primary
                    .as_ref()
                    .is_some_and(|primary| primary.settle().is_err())
                || self.journal.settle(&failed.binding).is_err()
            {
                return true;
            }
            failed.settled = true;
            failed.custody.take();
            failed._compiler.take();
            failed._sdk.take();
            failed._primary.take();
            if let Some((events, reason)) = failed.refusal.as_mut() {
                let _ = events.emit(aterm_types::resource_admission::Event::Refused {
                    reason: std::mem::take(reason),
                });
                return !events.drained();
            }
            false
        });
    }
}
