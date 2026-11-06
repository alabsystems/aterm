// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Failed launch custody retains the actual receipts. No error path upgrades
//! an observed identity or a lost ACK into a no-child or release proof.
use super::*;
impl Owner {
    pub(super) fn tick_failed(&mut self) {
        self.failed.retain_mut(|failed| {
            if failed
                .checked_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
            {
                return true;
            }
            failed.checked_at = Some(Instant::now());
            if let Some(proof) = failed.no_child.take() {
                match &mut failed.custody {
                    Custody::Fixed(custody) => {
                        if let Err(error) = custody.cancel(&self.fence, *proof) {
                            failed.no_child = error.into_parts().1.map(Box::new);
                        }
                    }
                    Custody::Cooperative(claim) => {
                        if self.fence.check_claim(claim).is_err() {
                            failed.no_child = Some(proof);
                        } else {
                            let _ = claim.cancel_cohort(*proof);
                        }
                    }
                }
            }
            if let Some(spawned) = failed.spawned.as_mut() {
                match &mut failed.custody {
                    Custody::Fixed(custody) => {
                        let _ = custody.retry_bind(&self.fence, spawned);
                    }
                    Custody::Cooperative(claim) => {
                        let _ = claim.bind_cohort(spawned);
                    }
                }
                // This child was created stopped at exec and was never resumed.
                // Only its original owned handle is used, never a saved PID.
                let _ = spawned.child_mut().kill();
                let _ = spawned.child_mut().try_wait();
            }
            let finished = match &mut failed.custody {
                Custody::Fixed(custody) => custody.try_finish(&self.fence),
                Custody::Cooperative(claim) => claim.finish_cohort(),
            };
            if finished.is_err() {
                return true;
            }
            self.journal.settle(&failed.binding).is_err()
        });
    }
}
