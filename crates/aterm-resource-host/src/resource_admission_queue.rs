// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One unarmed queue head at a time. Distinct roots never borrow memory from
//! the GUI PID merely because the GUI is their common process ancestor.
use super::*;
use aterm_capacity::build_cpu::Owner as CpuOwner;
use aterm_capacity::telemetry::Owed;
use aterm_capacity::{Need, Polled};
use aterm_types::resource_admission::Event;
use std::num::NonZeroU32;

impl Owner {
    pub(super) fn enqueue(
        &mut self,
        submission: AuthorizedSubmission,
        accounted: bool,
    ) -> Result<(), Box<(AuthorizedSubmission, io::Error)>> {
        let queued_at = Instant::now();
        if accounted && !crate::resource_accounting::supported() {
            return Err(Box::new((
                submission,
                refused("owning-wait accounting unavailable"),
            )));
        }
        if bootstrap_graph::purpose(&submission.request)
            && (!accounted || !bootstrap_graph::closed_request(&submission.request))
        {
            return Err(Box::new((
                submission,
                refused("Bootstrap Root requires its closed accounted fixed request"),
            )));
        }
        if workload::purpose(&submission.request)
            && (!accounted
                || !workload::closed_request(&submission.request)
                || submission.request.cpu_units != self.pool.slots().get()
                || self.pool.slots().get() as usize
                    > trust_resource_controller::workload::host::MAX_CPU_SET)
        {
            return Err(Box::new((
                submission,
                refused("workload root requires its closed accounted cooperative request"),
            )));
        }
        if recipe::metadata_purpose(&submission.request)
            && !recipe::metadata_request(&submission.request)
        {
            return Err(Box::new((
                submission,
                refused("metadata service requires the closed fixed-one request"),
            )));
        }
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
        if accounted && let Err(error) = events.account() {
            return Err(Box::new((submission, error)));
        }
        if let Err(error) = self.journal.create(&submission.request.binding) {
            return Err(Box::new((submission, error)));
        }
        let _ = events.emit(Event::Queued);
        self.pending.push_back(Pending {
            queued_at,
            resolver: None,
            bootstrap_graph: None,
            submission,
            events,
            recipe: None,
            sdk: None,
            probe: None,
            primary: None,
            waiter: None,
            claim: None,
            recovery: None,
            admitted: None,
            scope_reference: None,
            cancelled: false,
        });
        Ok(())
    }
    pub(super) fn tick_queue(&mut self) {
        let Some(mut pending) = self.pending.pop_front() else {
            return;
        };
        pending.cancelled |= pending.events.cancelled();
        let expired = pending
            .original_deadline()
            .is_ok_and(|deadline| deadline.is_some_and(|deadline| Instant::now() >= deadline));
        if pending.cancelled || expired {
            // Preparation may still hold and inspect original images. Its
            // paid CPU and memory remain occupied until the worker joins.
            if pending
                .probe
                .as_ref()
                .is_some_and(|probe| !probe.finished())
            {
                self.pending.push_front(pending);
                return;
            }
            if let Some(mut probe) = pending.probe.take() {
                let _ = probe.poll();
            }
            if pending.events.requires_accounting() {
                self.defer_accounted_refusal(
                    pending,
                    if expired {
                        "original workload deadline expired before native launch"
                    } else {
                        "cancelled before native launch"
                    },
                );
                return;
            }
            let retained = self.retain_unlaunched(&mut pending);
            drop(pending.claim.take());
            drop(pending.waiter.take());
            if !retained
                && self
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
                // Publication uncertainty is not an accounted no-resource
                // refusal. Closing this route remains transport-unknown.
                if !pending.events.requires_accounting() {
                    let _ = pending.events.emit(Event::Refused {
                        reason: "resource allocation could not be committed before launch".into(),
                    });
                }
                drop(pending.claim.take());
                self.faulted = true;
            }
            Err(error) => {
                let reason = if workload::purpose(&pending.submission.request)
                    || bootstrap_graph::purpose(&pending.submission.request)
                {
                    let detail: String = error
                        .to_string()
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .take(512)
                        .collect();
                    let kind = if bootstrap_graph::purpose(&pending.submission.request) {
                        "Bootstrap graph"
                    } else {
                        "workload"
                    };
                    format!("native {kind} admission refused before launch: {detail}")
                } else {
                    "resource admission refused before launch".into()
                };
                if pending.events.requires_accounting() {
                    self.defer_accounted_refusal(pending, &reason);
                    return;
                }
                // No CPU grant or ARM occurred on this branch. An exact
                // prepared intent can remain private and unused after refusal.
                let _ = pending.events.emit(Event::Refused { reason });
                drop(pending.claim.take());
                drop(pending.waiter.take());
                let retained = self.retain_unlaunched(&mut pending);
                if !retained
                    && self
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
        self.advance_inspected(pending, Instant::now() + Duration::from_millis(250))
    }
    fn advance_inspected(
        &mut self,
        pending: &mut Pending,
        inspection_deadline: Instant,
    ) -> io::Result<Advance> {
        let metadata = recipe::metadata_request(&pending.submission.request);
        let workload = workload::closed_request(&pending.submission.request);
        let graph = bootstrap_graph::closed_request(&pending.submission.request);
        let root_deadline = pending.original_deadline()?;
        if root_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "original workload admission deadline expired",
            ));
        }
        if metadata || workload || graph {
            if let Some(claim) = &pending.claim {
                pending.scope_reference = Some(claim.cohort_closure_ref()?);
            }
            let inspection = if metadata {
                self.check_fresh_metadata_ancestry_at(pending, Instant::now())
            } else {
                self.check_fresh_metadata_ancestry_until(
                    pending,
                    root_deadline.map_or(inspection_deadline, |deadline| {
                        deadline.min(inspection_deadline)
                    }),
                )
            };
            match inspection {
                Err(error)
                    if (workload || graph)
                        && error.kind() == io::ErrorKind::TimedOut
                        && root_deadline.is_some_and(|deadline| Instant::now() < deadline) =>
                {
                    // This is an incomplete READ-ONLY inspection, never an
                    // execution/delivery outcome. Keep the SAME queued owners;
                    // every later admission still needs a fresh complete look.
                    return Ok(Advance::Wait);
                }
                result => result?,
            }
        }
        if !metadata
            && !workload
            && !graph
            && pending.recipe.is_none()
            && !self.poll_recipe(pending)?
        {
            return Ok(Advance::Wait);
        }
        if pending.admitted.is_some() {
            if !self.poll_recipe(pending)? {
                return Ok(Advance::Wait);
            }
            return Ok(Advance::Launch(Box::new(
                pending
                    .admitted
                    .take()
                    .ok_or_else(|| refused("missing metadata custody"))?,
            )));
        }
        if pending.claim.is_none() {
            self.poll_memory(pending, &Live)?;
            if pending.claim.is_none() {
                return Ok(Advance::Wait);
            }
        }
        if workload {
            if pending.submission.request.cpu_units != self.pool.slots().get()
                || self.pool.slots().get() as usize
                    > trust_resource_controller::workload::host::MAX_CPU_SET
            {
                return Err(refused(
                    "workload requires explicit complete canonical CPU-domain ceiling",
                ));
            }
            if pending.primary.is_none() {
                if pending.recovery.is_none() {
                    let claim = pending
                        .claim
                        .as_ref()
                        .ok_or_else(|| refused("original workload memory unavailable"))?;
                    let reference = claim.cohort_closure_ref()?;
                    let request = &pending.submission.request;
                    let width = self.pool.slots();
                    let context = recovery::context(
                        &request.binding,
                        &reference,
                        request.memory_bytes,
                        width.get(),
                    );
                    let material =
                        self.debt
                            .prepare_recoverable(CpuOwner::current(None), width, context)?;
                    let (parent, leaf) = self.journal.authority(&request.binding)?;
                    pending.recovery = Some(material.persist(&parent, &leaf)?);
                    self.journal.update(&request.binding,"prepared",aterm_json::json!({"cpu_mode":"workload_reserved",
                        "cpu_units":width.get(),"memory_bytes":request.memory_bytes,"memory":reference.to_json()}))?;
                }
                let prepared = pending
                    .recovery
                    .as_mut()
                    .ok_or_else(|| refused("original workload recovery missing"))?;
                let Some(reservation) = self.debt.try_acquire_all_recoverable(prepared)? else {
                    return Ok(Advance::Wait);
                };
                pending.primary = Some(workload::Primary::new(reservation));
            }
            if !self.poll_recipe(pending)? {
                return Ok(Advance::Wait);
            }
        }
        let prepared = self.prepare(pending)?;
        if (metadata || graph)
            && let Advance::Launch(custody) = prepared
        {
            pending.admitted = Some(*custody);
            return self.advance(pending);
        }
        Ok(prepared)
    }
    pub(super) fn check_fresh_metadata_ancestry_at(
        &self,
        pending: &Pending,
        started: Instant,
    ) -> io::Result<()> {
        // Inventory and native rereads establish membership; elapsed wall time
        // is only a hang detector. A loaded host can spend over 250 ms reading
        // an unchanged funded inventory before the final ancestry reread.
        let deadline = started + Duration::from_secs(60);
        self.check_fresh_metadata_ancestry_until(pending, deadline)
    }
    fn check_fresh_metadata_ancestry_until(
        &self,
        pending: &Pending,
        deadline: Instant,
    ) -> io::Result<()> {
        use aterm_capacity::FundedMembership;
        let peer = pending.submission.peer;
        let ancestry = aterm_uds::process::Ancestry::capture(
            peer.pid,
            aterm_uds::process::Birth(peer.birth),
            deadline,
        )?;
        for scope in self.ledger.funded_scopes(deadline)? {
            // This exception is the original Claim held by this exact pending
            // invocation, never a request-provided cover marker or PID label.
            if pending.scope_reference.as_ref() == Some(scope.claim()) {
                continue;
            }
            match scope.membership(&ancestry) {
                FundedMembership::Outside => {}
                FundedMembership::Inside => {
                    return Err(refused(
                        "metadata caller already belongs to a funded original scope; borrow or refuse",
                    ));
                }
                FundedMembership::Unknown => {
                    return Err(refused("funded original scope membership is unresolved"));
                }
            }
        }
        ancestry.check_current(deadline)
    }
    #[cfg(test)]
    pub(super) fn workload_inspection_wait_test(
        &mut self,
        pending: &mut Pending,
        deadline: Instant,
    ) -> io::Result<bool> {
        self.advance_inspected(pending, deadline)
            .map(|state| matches!(state, Advance::Wait))
    }
    fn poll_recipe(&mut self, pending: &mut Pending) -> io::Result<bool> {
        if pending.recipe.is_none() {
            if pending.probe.is_none() {
                #[cfg(test)]
                let layout = self.recipe_layout.clone();
                #[cfg(not(test))]
                let layout = None;
                pending.probe = Some(recipe::Recipe::probe_in(
                    &pending.submission.request,
                    self.pool.slots().get(),
                    layout,
                )?);
            }
            match pending
                .probe
                .as_mut()
                .ok_or_else(|| refused("missing recipe probe"))?
                .poll()?
            {
                Some(result) => {
                    pending.probe = None;
                    let Some(recipe) = result else {
                        return Ok(false);
                    };
                    pending.recipe = Some(recipe);
                }
                None => return Ok(false),
            }
        }
        Ok(true)
    }
    /// One look at `pending`'s memory claim on `machine`, entering its wait at the first. Each
    /// admission writes one `kind=admission` row where it is decided ([`memory_decided`]): at the
    /// look that admits it or ends its wait, or at its refusal as an impossible head, never one
    /// a look; one given up while it waits writes its row as it drops ([`MemoryWait`]).
    pub(super) fn poll_memory(
        &mut self,
        pending: &mut Pending,
        machine: &dyn aterm_capacity::Machine,
    ) -> io::Result<()> {
        if pending.claim.is_some() {
            return Ok(());
        }
        let wait = match pending.waiter.take() {
            Some(wait) => wait,
            None => {
                let need = Need::new(format!(
                    "resource-run: {}",
                    pending.submission.request.label
                ))
                .mem(pending.submission.request.memory_bytes)
                .margin(aterm_capacity::build_cpu::default_memory_reserve());
                // Its row is owed from its ask, its refusal as an impossible head included.
                let owed = Owed::of(&self.ledger, &need);
                // Low-level Waiter polling deliberately does not apply the
                // high-level admission wrapper's permanent-impossibility check.
                // Refuse an impossible head before it can block every later job.
                let entered = match aterm_capacity::never(&need, &machine.read(None)) {
                    Some(reason) => Err(aterm_capacity::Error::Never(reason)),
                    None => Waiter::enter_cohort(&self.ledger, machine, need)
                        .map_err(aterm_capacity::Error::Ledger),
                };
                match entered {
                    Ok(waiter) => MemoryWait { waiter, owed },
                    Err(error) => return memory_decided(owed, Err(error)).map(drop),
                }
            }
        };
        let decided = match wait.waiter.poll(machine) {
            Polled::Waiting(waiter, _) => {
                pending.waiter = Some(MemoryWait {
                    waiter,
                    owed: wait.owed,
                });
                return Ok(());
            }
            Polled::Admitted(claim) => Ok(claim),
            Polled::Gone(error) => Err(aterm_capacity::Error::Ledger(error)),
            Polled::Unqualified(why) => Err(aterm_capacity::Error::Timing(why)),
        };
        pending.claim = Some(memory_decided(wait.owed, decided)?);
        Ok(())
    }
    fn defer_accounted_refusal(&mut self, mut pending: Pending, reason: &str) {
        // A pre-ARM error may still own memory, an admitted CPU bundle and
        // SDK/image preparation. Retain every original owner until cleanup.
        // A memory-only Claim has no CPU admission to reinterpret here.
        let custody = pending
            .admitted
            .take()
            .or_else(|| pending.claim.take().map(Custody::Cooperative));
        self.failed.push(Failed {
            _primary: pending.primary.take(),
            _compiler: pending
                .recipe
                .as_ref()
                .and_then(|recipe| recipe.compiler.clone()),
            binding: pending.submission.request.binding.clone(),
            custody,
            spawned: None,
            no_child: None,
            checked_at: None,
            _sdk: pending
                .recipe
                .as_ref()
                .and_then(|recipe| recipe.metadata_sdk.clone()),
            refusal: Some((pending.events, reason.to_owned())),
            settled: false,
        });
    }
    fn retain_unlaunched(&mut self, pending: &mut Pending) -> bool {
        if let Some(custody) = pending.admitted.take() {
            self.failed.push(Failed {
                _primary: pending.primary.take(),
                _compiler: pending
                    .recipe
                    .as_ref()
                    .and_then(|recipe| recipe.compiler.clone()),
                binding: pending.submission.request.binding.clone(),
                custody: Some(custody),
                refusal: None,
                settled: false,
                spawned: None,
                no_child: None,
                checked_at: None,
                _sdk: pending
                    .recipe
                    .as_ref()
                    .and_then(|recipe| recipe.metadata_sdk.clone()),
            });
            return true;
        }
        false
    }
}
/// The memory admission `owed` came to, `decided`: its row written ([`Owed::decided`]), then its
/// claim, or the error that refuses its request before launch (an impossible head's is
/// `InvalidInput`).
fn memory_decided(owed: Owed, decided: Result<Claim, aterm_capacity::Error>) -> io::Result<Claim> {
    owed.decided(decided.as_ref());
    decided.map_err(|error| match error {
        aterm_capacity::Error::Ledger(error) => error,
        aterm_capacity::Error::Never(reason) => io::Error::new(io::ErrorKind::InvalidInput, reason),
        _ => refused("memory admission unavailable"),
    })
}
pub(super) enum Advance {
    Wait,
    Retain,
    Launch(Box<Custody>),
}

impl Owner {
    pub(super) fn prepare(&mut self, pending: &mut Pending) -> io::Result<Advance> {
        let metadata = recipe::metadata_request(&pending.submission.request);
        let graph = bootstrap_graph::closed_request(&pending.submission.request);
        let width = if metadata {
            1
        } else if graph {
            pending.submission.request.cpu_units
        } else {
            pending
                .recipe
                .as_ref()
                .ok_or_else(|| refused("missing qualified recipe"))?
                .width
        };
        let claim = pending
            .claim
            .as_mut()
            .ok_or_else(|| refused("missing original memory claim"))?;
        let reference = claim.cohort_closure_ref()?;
        if graph {
            pending.scope_reference = Some(reference.clone());
        }
        let request = &pending.submission.request;
        let cooperative = !metadata
            && pending.recipe.as_ref().is_some_and(|recipe| {
                matches!(recipe.kind, recipe::Kind::Targo | recipe::Kind::Workload)
            });
        if cooperative {
            let mode = if workload::closed_request(request) {
                "workload_reserved"
            } else {
                "cooperative_shared"
            };
            self.journal.update(&request.binding,"prepared",aterm_json::json!({"cpu_mode":mode,
                "cpu_units":width,"memory_bytes":request.memory_bytes,"memory":reference.to_json()}))?;
            let claim = pending
                .claim
                .take()
                .ok_or_else(|| refused("missing original cooperative custody"))?;
            return Ok(Advance::Launch(Box::new(Custody::Cooperative(claim))));
        }
        if pending.recovery.is_none() {
            let width = NonZeroU32::new(width).ok_or_else(|| refused("empty CPU declaration"))?;
            let context = recovery::context(
                &request.binding,
                &reference,
                request.memory_bytes,
                width.get(),
            );
            let material =
                self.debt
                    .prepare_recoverable(CpuOwner::current(None), width, context)?;
            let (parent, leaf) = self.journal.authority(&request.binding)?;
            pending.recovery = Some(material.persist(&parent, &leaf)?);
            // Intent and original private authority are durable before even
            // the first CPU slot can contain this grant's commitment.
            self.journal.update(&request.binding,"prepared",aterm_json::json!({"cpu_mode":"retained_fixed",
                "cpu_units":width.get(),"memory_bytes":request.memory_bytes,"memory":reference.to_json()}))?;
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
