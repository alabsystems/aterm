// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One unarmed queue head at a time. Distinct roots never borrow memory from
//! the GUI PID merely because the GUI is their common process ancestor.
use super::*;
use aterm_capacity::build_cpu::Owner as CpuOwner;
use aterm_capacity::{Need, Polled};
use aterm_types::resource_admission::Event;
use std::num::NonZeroU32;

impl Owner {
    pub(super) fn enqueue(
        &mut self,
        submission: AuthorizedSubmission,
    ) -> Result<(), Box<(AuthorizedSubmission, io::Error)>> {
        if !self.accepts(&submission.request.binding) {
            return Err(Box::new((submission, refused("host queue unavailable"))));
        }
        let events = submission
            .fds
            .events
            .try_clone()
            .and_then(|fd| job::Events::new(fd, submission.request.binding.clone()));
        let mut events = match events {
            Ok(events) => events,
            Err(error) => return Err(Box::new((submission, error))),
        };
        if let Err(error) = self.journal.create(&submission.request.binding) {
            return Err(Box::new((submission, error)));
        }
        let _ = events.emit(Event::Queued);
        self.pending.push_back(Pending {
            submission,
            events,
            recipe: None,
            probe: None,
            waiter: None,
            claim: None,
            recovery: None,
        });
        Ok(())
    }
    pub(super) fn tick_queue(&mut self) {
        let Some(mut pending) = self.pending.pop_front() else {
            return;
        };
        if pending.events.cancelled() {
            drop(pending.claim.take());
            drop(pending.waiter.take());
            if self
                .journal
                .settle(&pending.submission.request.binding)
                .is_err()
            {
                self.faulted = true;
            }
            return;
        }
        match self.advance(&mut pending) {
            Ok(Advance::Wait) => self.pending.push_front(pending),
            Ok(Advance::Launch(custody)) => self.launch(pending, *custody),
            Ok(Advance::Retain) => {
                // No ARM has occurred, but CPU publication may be uncertain.
                // Preserve the prepared recovery record instead of certifying
                // settlement from a failed write or dropped capability.
                let _ = pending.events.emit(Event::Refused {
                    reason: "resource allocation could not be committed before launch".into(),
                });
                drop(pending.claim.take());
                self.faulted = true;
            }
            Err(_) => {
                // No CPU grant or ARM occurred on this branch. An exact
                // prepared intent can remain private and unused after refusal.
                let _ = pending.events.emit(Event::Refused {
                    reason: "resource admission refused before launch".into(),
                });
                drop(pending.claim.take());
                drop(pending.waiter.take());
                if self
                    .journal
                    .settle(&pending.submission.request.binding)
                    .is_err()
                {
                    self.faulted = true;
                }
            }
        }
    }
    fn advance(&mut self, pending: &mut Pending) -> io::Result<Advance> {
        if pending.recipe.is_none() {
            if pending.probe.is_none() {
                pending.probe = Some(recipe::Recipe::probe(
                    &pending.submission.request,
                    self.pool.slots().get(),
                )?);
            }
            match pending
                .probe
                .as_ref()
                .ok_or_else(|| refused("missing recipe probe"))?
                .try_recv()
            {
                Ok(result) => {
                    pending.recipe = Some(result?);
                    pending.probe = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(Advance::Wait),
                Err(_) => return Err(refused("recipe probe unavailable")),
            }
        }
        if pending.claim.is_none() {
            if pending.waiter.is_none() {
                let need = Need::new(format!(
                    "resource-run: {}",
                    pending.submission.request.label
                ))
                .mem(pending.submission.request.memory_bytes)
                .margin(aterm_capacity::build_cpu::default_memory_reserve());
                // Low-level Waiter polling deliberately does not apply the
                // high-level admission wrapper's permanent-impossibility check.
                // Refuse an impossible head before it can block every later job.
                if let Some(reason) =
                    aterm_capacity::never(&need, &aterm_capacity::Machine::read(&Live, None))
                {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
                }
                pending.waiter = Some(Waiter::enter_cohort(&self.ledger, &Live, need)?);
            }
            match pending
                .waiter
                .take()
                .ok_or_else(|| refused("missing memory waiter"))?
                .poll(&Live)
            {
                Polled::Admitted(claim) => pending.claim = Some(claim),
                Polled::Waiting(waiter, _) => {
                    pending.waiter = Some(waiter);
                    return Ok(Advance::Wait);
                }
                Polled::Gone(error) => return Err(error),
                Polled::Unqualified(_) => return Err(refused("memory admission unavailable")),
            }
        }
        self.prepare(pending)
    }
}
enum Advance {
    Wait,
    Retain,
    Launch(Box<Custody>),
}

impl Owner {
    fn prepare(&mut self, pending: &mut Pending) -> io::Result<Advance> {
        let recipe = pending
            .recipe
            .as_ref()
            .ok_or_else(|| refused("missing qualified recipe"))?;
        let claim = pending
            .claim
            .as_mut()
            .ok_or_else(|| refused("missing original memory claim"))?;
        let reference = claim.cohort_closure_ref()?;
        let request = &pending.submission.request;
        let cooperative = matches!(recipe.kind, recipe::Kind::Targo);
        if cooperative {
            self.journal.update(&request.binding,"prepared",aterm_json::json!({"cpu_mode":"cooperative_shared",
                "cpu_units":recipe.width,"memory_bytes":request.memory_bytes,"memory":reference.to_json()}))?;
            let claim = pending
                .claim
                .take()
                .ok_or_else(|| refused("missing original cooperative custody"))?;
            return Ok(Advance::Launch(Box::new(Custody::Cooperative(claim))));
        }
        if pending.recovery.is_none() {
            let width =
                NonZeroU32::new(recipe.width).ok_or_else(|| refused("empty CPU declaration"))?;
            let context = recovery::context(
                &request.binding,
                &reference,
                request.memory_bytes,
                recipe.width,
            );
            let material =
                self.debt
                    .prepare_recoverable(CpuOwner::current(None), width, context)?;
            let (parent, leaf) = self.journal.authority(&request.binding)?;
            pending.recovery = Some(material.persist(&parent, &leaf)?);
            // Intent and original private authority are durable before even
            // the first CPU slot can contain this grant's commitment.
            self.journal.update(&request.binding,"prepared",aterm_json::json!({"cpu_mode":"retained_fixed",
                "cpu_units":recipe.width,"memory_bytes":request.memory_bytes,"memory":reference.to_json()}))?;
        }
        let prepared = pending
            .recovery
            .as_mut()
            .ok_or_else(|| refused("missing original prepared CPU authority"))?;
        match self
            .debt
            .try_reserve_recoverable(prepared, self.pool.slots().get())
        {
            Ok(None) => Ok(Advance::Wait),
            Err(_) => Ok(Advance::Retain),
            Ok(Some(cpu)) => {
                let claim = pending
                    .claim
                    .take()
                    .ok_or_else(|| refused("missing original fixed custody"))?;
                match RetainedCohort::new_bundle(&self.fence, claim, cpu) {
                    Ok(custody) => Ok(Advance::Launch(Box::new(Custody::Fixed(custody)))),
                    Err(error) => {
                        let (_, claim, cpu) = error.into_parts();
                        pending.claim = Some(claim);
                        drop(cpu); // Durable debt, not capacity, survives dropped caps.
                        Ok(Advance::Retain)
                    }
                }
            }
        }
    }
}
