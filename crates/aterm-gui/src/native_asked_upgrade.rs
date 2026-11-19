// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, data-only succession after a positively settled native forward move.
//! Historical settlement is not current process, account or person authority.
//!
//! A recreated host holds no settlement of the epoch before it, so the move that follows a
//! host recreation is a re-entry ([`AskedIntent::Reentry`]): qualified only by this host's own
//! current-epoch passive registration of the live vendor ([`Registered`]), with no operation of
//! this epoch bound, and never by reviving a predecessor this host did not settle. An operation
//! that ends rollback-safe with no effect returns the table to what it held before it was
//! published: the predecessor it succeeded, or nothing bound. Any other operation that ends
//! owing nothing, with nothing a later asked move succeeds, and a settlement nothing that runs
//! can inherit (its vendor or shell ended or replaced, another conversation current in it, the
//! built-in relaunch continuity it was settled under broken), leave nothing bound, so the epoch
//! is re-entered rather than held. The agent reads which move the table admits, or the stage
//! that holds the epoch ([`Operations::succession`]), before it tells or prepares one.

use super::*;

/// This host's own current-epoch passive registration of exactly the request's vendor under its
/// shell, read under the metadata guard before the operation table
/// ([`crate::native_observe::Store::admits_reentry`]). Only [`Registered::read`] makes one.
#[derive(Debug)]
pub(super) struct Registered(());

impl Registered {
    /// The registration a re-entry `request` names, when this host's store holds it live in the
    /// current epoch for the request's exact vendor and shell under `context`'s guards.
    pub(super) fn read(
        store: &crate::native_observe::Store,
        request: &Prepare,
        context: &crate::native_observe::Context,
        master: i32,
        now_us: u64,
        kernel: &dyn crate::native_observe::Kernel,
    ) -> Option<Self> {
        let (
            Some(AskedIntent::Reentry(intent)),
            NativePlan::Restart {
                process,
                shell,
                observation: None,
                cold: None,
                ..
            },
        ) = (&request.asked, &request.plan)
        else {
            return None;
        };
        store
            .admits_reentry(
                &intent.observation,
                crate::native_observe::Live {
                    vendor: process,
                    shell,
                },
                context,
                master,
                now_us,
                kernel,
            )
            .then_some(Self(()))
    }
}

#[derive(Debug, Default)]
pub(super) enum CurrentBinding {
    #[default]
    Unknown,
    Unsettled {
        binding: Binding,
        prior: Option<Box<SettledBinding>>,
    },
    Settled(Box<SettledBinding>),
}

#[derive(Clone, Debug)]
pub(super) struct SettledBinding {
    binding: Binding,
    host_epoch: String,
    vendor: Process,
    shell: Process,
    conversation: Conversation,
    authority: SupervisorAuthority,
}

/// Only this host derives a qualification, and only for this exact new preparation.
#[derive(Clone, Debug)]
pub(super) struct Qualification {
    request: Prepare,
    pub(super) producer: Option<Arc<relaunch_permit::Permit>>,
    pub(super) signal: Option<Arc<relaunch_permit::Permit>>,
}

impl Qualification {
    pub(super) fn admits(&self, request: &Prepare) -> bool {
        self.matches_request(request)
            && (!request.checked_relaunch()
                || self
                    .producer
                    .as_ref()
                    .is_some_and(|permit| permit.matches(request)))
    }

    pub(super) fn matches_request(&self, request: &Prepare) -> bool {
        self.request == *request && shape(request)
    }

    pub(super) fn consume_first_signal(&mut self) -> bool {
        self.signal
            .take()
            .is_some_and(|permit| permit.consume_signal(&self.request))
    }
}

pub(super) fn shape(request: &Prepare) -> bool {
    request.asked_restart_shape()
}

/// Whether a rollback-safe `op` ended before any accepted effect, as an operation this host
/// qualified where it was asked.
fn effect_free(op: &Operation) -> bool {
    (op.request.asked.is_none()
        || op
            .authority
            .asked
            .as_ref()
            .is_some_and(|qualification| qualification.matches_request(&op.request)))
        && !op.signal_sent
        && !op.resume_sent
        && op.resume_attempt.is_none()
        && op.grant_record == GrantRecord::None
        && op.children.iter().all(Option::is_none)
}

/// The settlement a committed `op` bound by `binding` leaves, where a later asked move succeeds
/// it: a built-in cold restart or asked move that committed its forward image, with the resulting
/// vendor the host observed running exactly the command it resolved.
fn forward_settlement(op: &Operation, binding: &Binding) -> Option<SettledBinding> {
    let NativePlan::Restart { shell, cold, .. } = &op.request.plan else {
        return None;
    };
    let outcome = op.committed_image.as_ref()?;
    let command = command_for(op, ImagePhase::Forward)?;
    if !op.authority.builtin
        || (cold.is_none() && op.request.asked.is_none())
        || op.resume_phase != Some(ImagePhase::Forward)
        || outcome.process.image != *command.settled()
    {
        return None;
    }
    Some(SettledBinding {
        binding: binding.clone(),
        host_epoch: op.request.host_epoch.clone(),
        vendor: outcome.process.clone(),
        shell: shell.clone(),
        conversation: op.authority.conversation.clone()?,
        authority: op.authority.snapshot.clone(),
    })
}

/// Why a settlement is not now what qualification requires of it ([`SettledBinding::live`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unlive {
    /// It can be again, so it holds the epoch: the host reads no current conversation, reads its
    /// vendor or shell as nothing yet cannot say either ended, sees the vendor's group out of the
    /// foreground, or a foreign claim on Relaunch is live now.
    Held(&'static str),
    /// It never can be again, so nothing that runs inherits it ([`Operations::settle_current`]):
    /// the host reads another conversation as current, the built-in harness keeps Relaunch in a
    /// later generation (generations only advance), or its vendor or shell ended or another
    /// identity holds its pid.
    Lost(&'static str),
}

impl Unlive {
    const fn word(self) -> &'static str {
        match self {
            Self::Held(word) | Self::Lost(word) => word,
        }
    }
}

/// Whether `expected` runs now as it was: [`Unlive::Lost`] (`lost`) where it ended or another
/// identity holds its pid, neither of which ever changes back; [`Unlive::Held`] (`unread`) where
/// the kernel reads nothing at its pid yet cannot say it ended.
fn running(
    kernel: &dyn Kernel,
    expected: &Process,
    lost: &'static str,
    unread: &'static str,
) -> Result<(), Unlive> {
    match kernel.process(expected.pid) {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) => Err(Unlive::Lost(lost)),
        None if kernel.birth_alive(expected) == Some(false) => Err(Unlive::Lost(lost)),
        None => Err(Unlive::Held(unread)),
    }
}

impl SettledBinding {
    /// What qualification requires of the settled move now, beyond a request naming it: its
    /// conversation still current, the built-in harness's relaunch continuity unbroken since, its
    /// resulting vendor and shell live as they were, and the vendor's group in the foreground.
    /// Each refusal says whether the settlement can still become so ([`Unlive`]).
    fn live(
        &self,
        ctx: &crate::SessionCtx,
        authority: &SupervisorAuthority,
        master: i32,
        kernel: &dyn Kernel,
    ) -> Result<(), Unlive> {
        match self.conversation.is_current(ctx) {
            Some(true) => {}
            Some(false) => return Err(Unlive::Lost("qualification-conversation")),
            None => return Err(Unlive::Held("qualification-conversation-unread")),
        }
        if !self.authority.same_builtin_relaunch_continuity(authority) {
            // Where the built-in harness keeps Relaunch now in a live generation, it is a later
            // one than the settlement's, which never comes back; a foreign claim on Relaunch live
            // now has yet to end, and the generation it leaves is judged then.
            return Err(if authority.same_builtin_relaunch_continuity(authority) {
                Unlive::Lost("qualification-builtin-continuity")
            } else {
                Unlive::Held("qualification-foreign-relaunch")
            });
        }
        running(
            kernel,
            &self.vendor,
            "qualification-live-vendor",
            "qualification-vendor-unread",
        )?;
        running(
            kernel,
            &self.shell,
            "qualification-live-shell",
            "qualification-shell-unread",
        )?;
        if kernel.foreground(master) != Some(self.vendor.group) {
            return Err(Unlive::Held("qualification-foreground"));
        }
        Ok(())
    }
}

impl Operations {
    /// Called only at accepted publication, after actual authority/custody checks.
    pub(super) fn admitted_binding(&mut self, op: &Operation) {
        relaunch_permit::superseded(self, Some(&op.request));
        let request = &op.request;
        let previous = std::mem::take(&mut self.asked_binding);
        let prior = match previous {
            CurrentBinding::Settled(settled)
                if request.asked.as_ref().is_some_and(|asked| {
                    shape(request)
                        && op.authority.asked_matches(request)
                        && asked.predecessor() == Some(settled.binding.id.as_str())
                }) =>
            {
                Some(settled)
            }
            _ => None,
        };
        self.asked_binding = CurrentBinding::Unsettled {
            binding: op.binding.clone(),
            prior,
        };
    }

    /// Preserve positive settlement before ordinary bounded history reclamation, and resolve a
    /// binding that owes nothing more: a committed forward move a later asked move succeeds
    /// settles; an effect-free rollback-safe abort returns the table to what it held before the
    /// operation was published; any other resolved end (an abort after an effect, a retirement, a
    /// close, a commit no asked move succeeds) leaves nothing bound, which only a re-entry
    /// qualified afresh from this host's own registration of the live vendor may follow. A binding
    /// that still owes something (a receipt flight, an account lineage, its terminal append, a
    /// continuation, custody or a completion drain), or whose outcome is unresolved, stays as it
    /// is. No kernel query, I/O, resource owner or mutation qualification is created here.
    pub(super) fn settle_asked_binding(&mut self) -> Result<(), &'static str> {
        let binding = match &self.asked_binding {
            CurrentBinding::Unsettled { binding, .. } => binding,
            CurrentBinding::Settled(_) => return Ok(()),
            CurrentBinding::Unknown => return Err("settlement-unknown"),
        };
        if self.receipt_flights.contains_key(&binding.id) || self.account_lineages.pins(&binding.id)
        {
            return Err("settlement-receipt-or-account-debt");
        }
        let Some(operation) = self.by_id.get(&binding.id) else {
            return Err("settlement-operation-absent");
        };
        let Ok(op) = operation.try_lock() else {
            return Err("settlement-operation-busy");
        };
        if op.binding != *binding {
            return Err("settlement-binding");
        }
        let Phase::Completed(decision) = op.phase else {
            return Err("settlement-not-completed");
        };
        if op.continuation_pending() {
            return Err("settlement-continuation");
        }
        if op.cold_resolving {
            return Err("settlement-cold-resolving");
        }
        if op.custody.is_some() {
            return Err("settlement-custody");
        }
        if op
            .completion_drain
            .as_ref()
            .is_some_and(|drain| !drain.is_drained())
        {
            return Err("settlement-drain");
        }
        let settled = match decision {
            // An unresolved outcome retains every descriptor and begins no new move.
            Decision::Unknown => return Err("settlement-unresolved"),
            Decision::Committed | Decision::ProvenNotCommittedRollbackSafe
                if op.terminal_acknowledged != Some(decision) =>
            {
                return Err("settlement-terminal-ack");
            }
            // An effect-free abort returns the table to exactly what it held before the operation
            // was published: the predecessor an asked move succeeded, settled again, or, where
            // nothing of this epoch was settled before it (a cold or private restart, a re-entry),
            // nothing bound. A rollback-safe result after any accepted effect is not a retry
            // grant, nor is an asked operation's that this host never qualified: it restores no
            // predecessor and leaves nothing bound.
            Decision::ProvenNotCommittedRollbackSafe if effect_free(&op) => None,
            Decision::Committed => Some(forward_settlement(&op, binding)),
            Decision::ProvenNotCommittedRollbackSafe | Decision::Retired(_) | Decision::Closed => {
                Some(None)
            }
        };
        drop(op);
        let previous = std::mem::take(&mut self.asked_binding);
        let CurrentBinding::Unsettled { prior, .. } = previous else {
            unreachable!("the table exclusively owns the current binding");
        };
        // `None` restores what preceded the operation; otherwise its own settlement, if any.
        self.asked_binding = settled
            .map_or(prior, |settled| settled.map(Box::new))
            .map_or(CurrentBinding::Unknown, CurrentBinding::Settled);
        if matches!(self.asked_binding, CurrentBinding::Settled(_)) {
            Ok(())
        } else {
            Err("settlement-released")
        }
    }

    /// Settle the current binding ([`Self::settle_asked_binding`]), then release a settlement
    /// nothing can succeed, one that can never again be what qualification requires
    /// ([`Unlive::Lost`]): the vendor its move placed, or its shell, ended or is another identity
    /// now (a person quit it and ran another by hand, or it crashed), another conversation is
    /// current in it (a `/clear`, or a `/resume` of another), or the built-in relaunch
    /// continuity it was settled under broke (a foreign claim on Relaunch came and went). No
    /// later read revives any of these, so nothing that runs inherits it; a re-entry is qualified
    /// only from this host's own current-epoch registration of whatever vendor runs now, under
    /// the current continuity. A settlement still pinned by a receipt flight or an account
    /// lineage, or one that can still become live ([`Unlive::Held`]), is kept.
    fn settle_current(
        &mut self,
        ctx: &crate::SessionCtx,
        authority: &SupervisorAuthority,
        master: i32,
        kernel: &dyn Kernel,
    ) -> Result<(), &'static str> {
        self.settle_asked_binding()?;
        let CurrentBinding::Settled(settled) = &self.asked_binding else {
            unreachable!("a settlement that succeeded is held");
        };
        if self.receipt_flights.contains_key(&settled.binding.id)
            || self.account_lineages.pins(&settled.binding.id)
        {
            return Ok(());
        }
        let Err(Unlive::Lost(lost)) = settled.live(ctx, authority, master, kernel) else {
            return Ok(());
        };
        self.asked_binding = CurrentBinding::Unknown;
        Err(lost)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUnadmittedPrepare",
            action = "QualifyUpgrade",
            project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
        )
    )]
    pub(super) fn qualify_asked(
        &mut self,
        ctx: &crate::SessionCtx,
        request: &Prepare,
        authority: &Authority,
        master: i32,
        kernel: &dyn Kernel,
        registered: Option<Registered>,
    ) -> Result<Qualification, &'static str> {
        if !shape(request)
            || !authority.builtin
            || authority.renewal.is_some()
            || authority.account.is_some()
        {
            return Err("qualification-caller-or-shape");
        }
        if matches!(request.asked, Some(AskedIntent::Reentry(_))) {
            return self.qualify_reentry(ctx, request, authority, master, kernel, registered);
        }
        self.settle_current(ctx, &authority.snapshot, master, kernel)?;
        let CurrentBinding::Settled(settled) = &self.asked_binding else {
            return Err("qualification-no-settled-binding");
        };
        let asked = request.asked.as_ref().ok_or("qualification-no-intent")?;
        let NativePlan::Restart { process, shell, .. } = &request.plan else {
            return Err("qualification-plan");
        };
        if asked.predecessor() != Some(settled.binding.id.as_str()) {
            return Err("qualification-predecessor");
        }
        if request.host_epoch != settled.host_epoch {
            return Err("qualification-request-epoch");
        }
        if host_epoch() != Some(settled.host_epoch.as_str()) {
            return Err("qualification-current-epoch");
        }
        if process != &settled.vendor {
            return Err("qualification-resulting-vendor");
        }
        if shell != &settled.shell {
            return Err("qualification-resulting-shell");
        }
        settled
            .live(ctx, &authority.snapshot, master, kernel)
            .map_err(Unlive::word)?;
        if request.expected_human_seq != Some(ctx.human_input.seq()) {
            return Err("qualification-human-sequence");
        }
        if self.receipt_flights.contains_key(&settled.binding.id) {
            return Err("qualification-receipt-flight");
        }
        if self.account_lineages.pins(&settled.binding.id) {
            return Err("qualification-account-lineage");
        }
        let producer = if request.checked_relaunch() {
            Some(relaunch_permit::pending(self, request).ok_or("qualification-checked-producer")?)
        } else {
            None
        };
        Ok(Qualification {
            request: request.clone(),
            producer,
            signal: None,
        })
    }

    /// Which asked move this table admits now after `predecessor`, or after none for a record that
    /// completed no native operation ([`Succession`]), the agent's read before it tells or prepares
    /// one, judged as qualification judges it: an upgrade or Memory restart naming `predecessor`
    /// where this epoch settled exactly it, its resulting vendor still live in the foreground under
    /// its shell, its conversation current and the built-in relaunch continuity unbroken
    /// ([`SettledBinding::live`]), with no receipt flight or account lineage held for it; a
    /// re-entry where nothing of this epoch is bound (a settlement nothing that runs can inherit
    /// binds nothing, [`Self::settle_current`]) and no receipt flight or account lineage is held,
    /// whatever predecessor was named, if any; otherwise neither, naming the stage that holds the
    /// epoch, for the agent's wait log (a settlement where none was named is another
    /// predecessor's). The caller reclaims the table first, as a prepare does, so the read sees
    /// what a prepare's qualification would. It qualifies nothing: each prepare is still judged
    /// afresh by [`Self::qualify_asked`].
    pub(super) fn succession(
        &mut self,
        ctx: &crate::SessionCtx,
        predecessor: Option<&str>,
        authority: &SupervisorAuthority,
        master: i32,
        kernel: &dyn Kernel,
    ) -> Succession {
        let settlement = self.settle_current(ctx, authority, master, kernel);
        let stage = match &self.asked_binding {
            CurrentBinding::Unknown if !self.receipt_flights.is_empty() => "reentry-receipt-flight",
            CurrentBinding::Unknown if self.account_lineages.held() => "reentry-account-lineage",
            CurrentBinding::Unknown => return Succession::Reentry,
            // A binding left unsettled is one its settlement refused, which names why.
            CurrentBinding::Unsettled { .. } => settlement.err().unwrap_or("settlement-unsettled"),
            CurrentBinding::Settled(settled)
                if predecessor != Some(settled.binding.id.as_str()) =>
            {
                "succession-another-predecessor"
            }
            CurrentBinding::Settled(settled)
                if self.receipt_flights.contains_key(&settled.binding.id) =>
            {
                "qualification-receipt-flight"
            }
            CurrentBinding::Settled(settled) if self.account_lineages.pins(&settled.binding.id) => {
                "qualification-account-lineage"
            }
            CurrentBinding::Settled(settled) => {
                match settled.live(ctx, authority, master, kernel) {
                    Ok(()) => return Succession::Predecessor,
                    Err(unlive) => unlive.word(),
                }
            }
        };
        Succession::Bound(stage.to_owned())
    }

    /// A re-entry's qualification ([`AskedIntent::Reentry`]): no operation of this host's epoch
    /// is bound (an operation that ended owing nothing with nothing to settle forward, or a
    /// settlement nothing that runs can inherit, binds nothing: [`Self::settle_current`]), and
    /// this host's own current-epoch registration names the request's exact live vendor and
    /// shell (`registered`), with the vendor's group in the foreground, the conversation current,
    /// the person's sequence unmoved, and no receipt flight or account lineage held but the
    /// request's own (its receipt flight, and the root slot a stored login reserves before
    /// qualification). A Memory restart's re-entry further requires its scoped checked-producer
    /// authorization, as any Memory restart does.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUnadmittedPrepare",
            action = "Reenter",
            project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeUnadmittedPrepare",
            action = "Settle",
            reason = "An earlier operation of this epoch commits forward and settles at its next settlement: a reclamation, a succession read or a qualification (settle_asked_binding)"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeUnadmittedPrepare",
            action = "Bind",
            reason = "Another operation of this epoch is published, which binds the table (admitted_binding)"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeUnadmittedPrepare",
            action = "Recreate",
            reason = "A new host process starts with a fresh operation table and observation store under a new epoch"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeUnadmittedPrepare",
            action = "Unbind",
            reason = "An operation of this epoch (another, or the one modelled once completed) ends owing nothing with nothing a later asked move succeeds (an effect-free abort with nothing settled before it, an abort after an effect, a retirement, a close), or a settlement nothing that runs can inherit (its vendor or shell ended or replaced, another conversation current, its built-in relaunch continuity broken), which leaves the table unbound at its next settlement (settle_asked_binding, settle_current)"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeUnadmittedPrepare",
            action = "Register",
            reason = "The passive observation lane registers the live vendor in this epoch, granting nothing"
        )
    )]
    fn qualify_reentry(
        &mut self,
        ctx: &crate::SessionCtx,
        request: &Prepare,
        authority: &Authority,
        master: i32,
        kernel: &dyn Kernel,
        registered: Option<Registered>,
    ) -> Result<Qualification, &'static str> {
        let Some(AskedIntent::Reentry(intent)) = &request.asked else {
            return Err("qualification-no-intent");
        };
        // A settlement a live vendor can inherit is succeeded by an upgrade naming it, never
        // re-entered; an operation still owing anything, or unresolved, keeps the epoch bound.
        let _settlement = self.settle_current(ctx, &authority.snapshot, master, kernel);
        if !matches!(self.asked_binding, CurrentBinding::Unknown) {
            return Err("qualification-reentry-bound");
        }
        let NativePlan::Restart { process, shell, .. } = &request.plan else {
            return Err("qualification-plan");
        };
        let Some(epoch) = host_epoch() else {
            return Err("qualification-current-epoch");
        };
        if request.host_epoch != epoch || intent.observation.host_epoch != epoch {
            return Err("qualification-request-epoch");
        }
        if registered.is_none() {
            return Err("qualification-reentry-registration");
        }
        if !authority
            .conversation
            .as_ref()
            .is_some_and(|conversation| conversation.current(ctx))
        {
            return Err("qualification-conversation");
        }
        if kernel.process(process.pid).as_ref() != Some(process) {
            return Err("qualification-live-vendor");
        }
        if kernel.process(shell.pid).as_ref() != Some(shell) {
            return Err("qualification-live-shell");
        }
        if kernel.foreground(master) != Some(process.group) {
            return Err("qualification-foreground");
        }
        if request.expected_human_seq != Some(ctx.human_input.seq()) {
            return Err("qualification-human-sequence");
        }
        // The request's own receipt flight is the one this serving holds.
        if self.receipt_flights.keys().any(|id| *id != request.id) {
            return Err("qualification-receipt-flight");
        }
        // The request's own root slot, reserved before qualification, is no lineage it inherits.
        if self.account_lineages.held_except(&request.id) {
            return Err("qualification-account-lineage");
        }
        let producer = if request.checked_relaunch() {
            Some(relaunch_permit::pending(self, request).ok_or("qualification-checked-producer")?)
        } else {
            None
        };
        Ok(Qualification {
            request: request.clone(),
            producer,
            signal: None,
        })
    }
}

/// The [`Request::Succession`] reply for this session's host: its current epoch and what its
/// operation table admits after `predecessor`, or none ([`Operations::succession`]), read under the
/// same reclamation a prepare runs first ([`Operations::reclaim`]), so a committed move's account
/// lineage compacts and its settlement is judged as the next prepare's qualification would judge
/// it, never held for a prepare the answer itself withholds.
pub(super) fn succession_reply(
    ctx: &crate::SessionCtx,
    master: i32,
    kernel: &dyn Kernel,
    predecessor: Option<&str>,
) -> String {
    let Some(epoch) = host_epoch() else {
        return "ERR native entropy unavailable\n".into();
    };
    let Some(now) = kernel.clock().now_us() else {
        return UNKNOWN.into();
    };
    let authority = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    let (admits, retired) = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let retired = table.reclaim(now);
        (
            table.succession(ctx, predecessor, &authority, master, kernel),
            retired,
        )
    };
    // Retired history may release a final Arc; keep it outside the table lock.
    drop(retired);
    aterm_agent::harness::native_act::succession_reply(epoch, &admits)
        .unwrap_or_else(|| UNKNOWN.into())
}

/// A positive terminal append belongs to the exact captured operation, never a reused id.
pub(super) fn acknowledge_terminal(
    ctx: &crate::SessionCtx,
    operation: &Arc<Mutex<Operation>>,
    request: &Prepare,
    binding: &Binding,
    decision: Decision,
) {
    let table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if !table
        .by_id
        .get(&binding.id)
        .is_some_and(|current| Arc::ptr_eq(current, operation))
    {
        return;
    }
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if op.request == *request
        && op.binding == *binding
        && op.phase == Phase::Completed(decision)
        && matches!(
            decision,
            Decision::Committed | Decision::ProvenNotCommittedRollbackSafe
        )
    {
        op.terminal_acknowledged = Some(decision);
    }
}
