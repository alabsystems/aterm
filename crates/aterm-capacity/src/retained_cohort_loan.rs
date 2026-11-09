// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bind a real supplementary GNU byte to the original native subgroup lifecycle.

use super::{
    CancelError, GnuLoan, GnuLoanClosure, GnuLoanReturnError, RetainedCohort, RetiredSubgroup,
    refused,
};
use crate::{
    cohort::launch::{self, CohortChild, LaunchError},
    leadership::Fence,
    retained_gnu::{Epoch, LoanState},
};
use aterm_uds::spawnfd::{Command, NotSpawned, Spawned};
use std::io;

impl RetainedCohort {
    fn loan_epoch(&mut self, fence: &Fence) -> io::Result<&mut Epoch> {
        self.check_bound(fence)?;
        self.gnu
            .as_deref_mut()
            .ok_or_else(|| refused("original cohort has no GNU epoch"))
    }

    /// Remove one actual byte from this bound original paid epoch. `None`
    /// means its nonblocking native read found no byte; no allocation occurs.
    /// Drop or loss of the returned loan permanently keeps its ledger entry.
    /// # Errors
    /// Missing/changed original custody, a one-CPU ceiling, poisoned transport,
    /// or an uncertain native read. Uncertain reads retain original CPU debt.
    pub fn try_loan_gnu(&mut self, fence: &Fence) -> io::Result<Option<GnuLoan>> {
        self.loan_epoch(fence)?.try_loan()
    }

    /// Durably launch and bind a subgroup funded by this exact byte. The native
    /// command is forced to start suspended, with its actual birth recorded in
    /// the original loan before BIND and before any target GO. The Host must
    /// separately enforce and retain its same native no-fork/no-escape witness.
    /// # Errors
    /// Wrong/used loan or leadership, native launch or durable BIND failure.
    /// Every attempted launch keeps the byte unresolved on failure or unwind.
    pub fn launch_loaned_subgroup(
        &mut self,
        fence: &Fence,
        root: &mut CohortChild,
        command: &mut Command,
        loan: &mut GnuLoan,
    ) -> Result<CohortChild, LaunchError> {
        self.launch_loaned_subgroup_via(
            fence,
            root,
            command,
            loan,
            crate::Claim::bind_subgroup_actual,
        )
    }

    // Like the existing native launch module, tests may interrupt the actual
    // private binder after real birth. No callback is exposed as authority.
    fn launch_loaned_subgroup_via(
        &mut self,
        fence: &Fence,
        root: &mut CohortChild,
        command: &mut Command,
        loan: &mut GnuLoan,
        bind: impl FnOnce(&mut crate::Claim, &mut Spawned) -> io::Result<crate::cohort::Cohort>,
    ) -> Result<CohortChild, LaunchError> {
        let epoch = self.loan_epoch(fence).map_err(LaunchError::Refused)?;
        if !matches!(
            epoch.loan_state(loan).map_err(LaunchError::Refused)?,
            LoanState::Unused
        ) {
            return Err(LaunchError::Refused(refused(
                "GNU loan already attempted a subgroup launch",
            )));
        }
        epoch
            .set_loan_state(loan, LoanState::Launching)
            .map_err(LaunchError::Refused)?;
        command.start_suspended(true);
        let epoch = self.gnu.as_deref_mut().expect("checked original epoch");
        let result =
            launch::launch_subgroup_with(&mut self.claim, root, command, |claim, spawned| {
                let birth = spawned.key().clone();
                epoch.set_loan_state(loan, LoanState::Unbound(birth.clone()))?;
                let cohort = bind(claim, spawned)?;
                epoch.set_loan_state(
                    loan,
                    LoanState::Bound {
                        birth,
                        cohort: cohort.clone(),
                    },
                )?;
                Ok(cohort)
            });
        if let Err(LaunchError::NotSpawned(proof)) = &result {
            // A failed identity bookend must not discard the native receipt.
            // Leaving Launching unresolved is restrictive; the actual caller
            // still receives the original no-child owner for supervision.
            let _ = epoch.set_loan_state(loan, LoanState::NoChild(proof.key().clone()));
        }
        result
    }

    /// Retry BIND with the same actual unbound child. A foreign child is
    /// returned unchanged and cannot move this loan's original birth binding.
    /// # Errors
    /// Wrong original epoch/attempt, changed leadership or durable BIND refusal.
    pub fn bind_loaned_subgroup(
        &mut self,
        fence: &Fence,
        spawned: Spawned,
        loan: &mut GnuLoan,
    ) -> Result<CohortChild, LaunchError> {
        let preflight = (|| {
            let epoch = self.loan_epoch(fence)?;
            if !matches!(epoch.loan_state(loan)?, LoanState::Unbound(birth) if *birth == *spawned.key())
            {
                return Err(refused("GNU loan BIND needs the same actual unbound birth"));
            }
            Ok(())
        })();
        if let Err(error) = preflight {
            return Err(LaunchError::Unbound {
                spawned: Box::new(spawned),
                error,
            });
        }
        let birth = spawned.key().clone();
        let child = self.claim.bind_subgroup(spawned)?;
        if let Err(error) = self
            .gnu
            .as_deref_mut()
            .expect("checked epoch")
            .set_loan_state(
                loan,
                LoanState::Bound {
                    birth,
                    cohort: child.cohort().clone(),
                },
            )
        {
            return Err(LaunchError::Unbound {
                spawned: Box::new(child.into_parts().0),
                error,
            });
        }
        Ok(child)
    }

    /// Cancel only this loan's original actual no-child receipt. Successful
    /// persistence supplies a one-use NoLaunch bookend; it does not refund yet.
    /// # Errors
    /// Wrong receipt is returned. Once accepted, failed cancellation persistence
    /// stays in the original claim and keeps the loan unresolved indefinitely.
    pub fn cancel_loaned_subgroup(
        &mut self,
        fence: &Fence,
        proof: NotSpawned,
        loan: &mut GnuLoan,
    ) -> Result<GnuLoanClosure, CancelError> {
        let preflight = (|| {
            let epoch = self.loan_epoch(fence)?;
            if !matches!(epoch.loan_state(loan)?, LoanState::NoChild(birth) if *birth == *proof.key())
            {
                return Err(refused(
                    "GNU loan cancellation needs its same no-child receipt",
                ));
            }
            self.claim.check_subgroup_cancel(&proof)
        })();
        if let Err(error) = preflight {
            return Err(CancelError::Refused {
                error,
                proof: Box::new(proof),
            });
        }
        if let Err(error) = self
            .gnu
            .as_deref_mut()
            .expect("checked epoch")
            .set_loan_state(loan, LoanState::Unknown)
        {
            return Err(CancelError::Refused {
                error,
                proof: Box::new(proof),
            });
        }
        self.claim
            .cancel_subgroup(proof)
            .map_err(CancelError::Retained)?;
        self.gnu
            .as_deref_mut()
            .expect("checked epoch")
            .close_no_launch(loan)
            .map_err(CancelError::Retained)
    }

    /// Seal actual lexical non-use before any attempt. No public status or
    /// no-child Boolean is accepted, and a used loan can never enter this path.
    /// # Errors
    /// Missing/wrong original custody or any previous launch attempt.
    pub fn cancel_unused_gnu_loan(
        &mut self,
        fence: &Fence,
        loan: &mut GnuLoan,
    ) -> io::Result<GnuLoanClosure> {
        self.loan_epoch(fence)?.close_unused(loan)
    }

    /// Mint the private one-use bookend only after the same original child was
    /// actually reaped and its exact registered subgroup durably retired. This
    /// covers that subgroup only; escaped descendants remain outside its scope.
    /// # Errors
    /// Missing/wrong original loan/child, no actual owning reap, or native group
    /// drain/persistence refusal. The caller retains the unresolved loan.
    pub fn retire_loaned_subgroup(
        &mut self,
        fence: &Fence,
        subgroup: &mut CohortChild,
        loan: &mut GnuLoan,
    ) -> io::Result<RetiredSubgroup> {
        let epoch = self.loan_epoch(fence)?;
        if !matches!(epoch.loan_state(loan)?, LoanState::Bound { birth, cohort }
            if *birth == *subgroup.birth_key() && *cohort == *subgroup.cohort())
        {
            return Err(refused(
                "GNU loan retirement needs the same actual bound subgroup",
            ));
        }
        if !subgroup.child_mut().is_reaped() {
            return Err(refused(
                "GNU loan retirement requires the same child's actual owning reap",
            ));
        }
        self.claim.retire_subgroup(subgroup)?;
        self.gnu
            .as_deref_mut()
            .expect("checked epoch")
            .close_retired(loan)
    }

    /// Return exactly one original byte using its same opaque native bookend.
    /// No Drop or ambiguous native write refunds/replays a byte. The independent
    /// unresolved entry blocks epoch retirement even with forged replacement data.
    /// # Errors
    /// Wrong/used bookend or native return failure. Preflight errors retain the
    /// supplied bookend; attempted writes keep debt and consume that bookend.
    pub fn return_gnu_loan(
        &mut self,
        fence: &Fence,
        loan: &mut GnuLoan,
        proof: GnuLoanClosure,
    ) -> Result<(), GnuLoanReturnError> {
        match self.loan_epoch(fence) {
            Ok(epoch) => epoch.return_loan(loan, proof),
            Err(error) => Err(GnuLoanReturnError::refused(error, proof)),
        }
    }

    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) fn launch_loaned_subgroup_with(
        &mut self,
        fence: &Fence,
        root: &mut CohortChild,
        command: &mut Command,
        loan: &mut GnuLoan,
        bind: impl FnOnce(&mut crate::Claim, &mut Spawned) -> io::Result<crate::cohort::Cohort>,
    ) -> Result<CohortChild, LaunchError> {
        self.launch_loaned_subgroup_via(fence, root, command, loan, bind)
    }
}
