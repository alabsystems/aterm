// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded passive crash observations. No registration owns input custody, a native
//! operation, an app handoff hold, or a mutation deadline. Missing data never proves exit.
//! Reserve under a brief store-only lock, arm outside every metadata/operation/store lock,
//! then publish under freshly captured metadata guards. Returned retired resources are
//! disposed only after those guards end. CrashPrepare must make a NEW atomic admission, reject unresolved old
//! mutators, and reprove retained Evidence after predecessor drain before mutation.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use aterm_agent::harness::native_act::Process;
use aterm_agent::harness::native_observe::{ObservationBinding, Registration, Reply};

use crate::session_timeline::{SupervisorAuthority, SupervisorInputCaller};

const MAX_RECORDS: usize = 2;
const MAX_HISTORY: usize = 128;
const LIFETIME_US: u64 = 30 * 60 * 1_000_000;

/// Host-side observations, never request words accepted as facts. In particular conversation
/// is the host-verified immutable record/conversation key; a caller string alone is not proof.
/// If that guard is unavailable the serving lane must refuse to construct this context.
#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub(crate) session: aterm_session::SessionId,
    pub(crate) nonce: aterm_session::LaunchNonce,
    pub(crate) host_epoch: String,
    pub(crate) caller: SupervisorInputCaller,
    pub(crate) actor: Process,
    pub(crate) authority: SupervisorAuthority,
    pub(crate) human_seq: u64,
    pub(crate) conversation: String,
}

impl Context {
    fn valid(&self) -> bool {
        self.caller.principal == crate::control::Principal::Harness
            && self.caller.connection.is_some()
            && self.caller.peer == Some(self.actor.pid)
            && self.actor.pid > 1
            && self.actor.birth != 0
            && self.actor.image.path.len() <= aterm_agent::harness::native_act::MAX_PATH
            && aterm_codec::hex::is_lower(&self.host_epoch, 32)
            && !self.session.as_str().is_empty()
            && self.session.as_str().len() <= 128
            && self
                .session
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            && !self.conversation.is_empty()
            && self.conversation.len() <= 1024
            && !self.conversation.contains(['\n', '\r', '\0'])
    }

    // Passive capture cannot borrow another supervisor's Relaunch ownership.
    fn passive_valid(&self) -> bool {
        self.valid()
            && self
                .authority
                .same_builtin_relaunch_continuity(&self.authority)
    }

    // A completed native mutation may retain an outcome while its independently admitted
    // sealed service operates under a foreign claim. That evidence keeps the full claim
    // revision; only built-in outcomes use the benign host-claim continuity rule.
    fn outcome_matches(&self, current: &Self) -> bool {
        if self.passive_valid() {
            self.matches(current)
        } else {
            self.stable_matches(current)
                && self.authority == current.authority
                && self.human_seq == current.human_seq
        }
    }

    fn matches(&self, current: &Self) -> bool {
        self.stable_matches(current)
            && self
                .authority
                .same_builtin_relaunch_continuity(&current.authority)
            && self.human_seq == current.human_seq
    }

    fn stable_matches(&self, current: &Self) -> bool {
        current.valid()
            && self.session == current.session
            && self.nonce == current.nonce
            && self.host_epoch == current.host_epoch
            && self.caller.principal == current.caller.principal
            && self.caller.peer == current.caller.peer
            && same_actor(&self.actor, &current.actor)
            && self.conversation == current.conversation
        // The private host opens a fresh authenticated stream each poll. Connection numbers
        // are diagnostic here; only a NEW native admission binds a mutation to its connection.
    }
}

fn same_actor(original: &Process, actual: &Process) -> bool {
    original.pid == actual.pid
        && original.birth == actual.birth
        && original.group == actual.group
        && original.session == actual.session
        && original.image == actual.image
    // Reparenting is diagnostic; it cannot change this kernel actor's identity.
}

pub(crate) trait DeathWitness: std::fmt::Debug + Send + Sync {
    /// Positive death belongs to the retained kernel object, never a later PID lookup.
    fn exited(&self) -> Option<bool>;

    /// A shared native watch pins its passive slot inside the FD-owning object itself.
    /// The FD fields must drop before this hold, including every deferred native owner.
    fn outcome_hold(&self) -> Option<&OutcomeHold> {
        None
    }
}

#[derive(Debug)]
struct KernelDeath {
    #[cfg(target_vendor = "apple")]
    darwin: aterm_uds::exitwatch::ExitWatch,
    #[cfg(target_os = "linux")]
    linux: std::os::fd::OwnedFd,
}

#[cfg(target_vendor = "apple")]
impl aterm_uds::spawn_lock::Descriptors for KernelDeath {
    fn mark(&self) -> std::io::Result<()> {
        // SAFETY: this borrow ends before self, which exclusively owns the watch descriptor.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.raw_fd()) };
        aterm_uds::keep_out_of_children(&fd)
    }
}

impl KernelDeath {
    fn watch(process: &Process) -> Option<Self> {
        #[cfg(target_vendor = "apple")]
        let watch = aterm_uds::kept_out(|| {
            aterm_uds::exitwatch::ExitWatch::watch(process.pid)
                .map(|darwin| Self { darwin })
                .ok_or_else(|| std::io::Error::other("kernel exit watch unavailable"))
        })
        .ok()?;
        #[cfg(target_os = "linux")]
        let watch = Self {
            linux: aterm_uds::exitwatch::exit_fd(process.pid)?,
        };
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        {
            let pid = i32::try_from(process.pid).ok().filter(|pid| *pid > 1)?;
            // A reused PID cannot substitute a watch for the captured birth. Registration
            // additionally brackets the complete mapped Process around this acquisition.
            if aterm_uds::process::read_birth(pid)?.0 != process.birth {
                return None;
            }
            let fd = watch.raw_fd();
            // SAFETY: this watch exclusively owns a valid descriptor; F_GETFD only reads flags.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            (flags >= 0 && flags & libc::FD_CLOEXEC != 0).then_some(watch)
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        {
            let _ = process;
            None
        }
    }

    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn raw_fd(&self) -> i32 {
        #[cfg(target_vendor = "apple")]
        {
            self.darwin.as_raw_fd()
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            self.linux.as_raw_fd()
        }
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn readable(fd: i32) -> Option<bool> {
    let mut item = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized live poll slot, no borrowed process state, and zero timeout.
    match unsafe { libc::poll(&mut item, 1, 0) } {
        0 => Some(false),
        1 if item.revents & (libc::POLLERR | libc::POLLNVAL) == 0
            && item.revents & libc::POLLIN != 0 =>
        {
            Some(true)
        }
        _ => None,
    }
}

impl DeathWitness for KernelDeath {
    fn exited(&self) -> Option<bool> {
        #[cfg(target_vendor = "apple")]
        {
            if self.darwin.exit_status().is_some() {
                Some(true)
            } else {
                // A readable queue without a validated NOTE_EXIT status is uncertainty,
                // not a fabricated death or a successful live refresh.
                (!readable(self.raw_fd())?).then_some(false)
            }
        }
        #[cfg(target_os = "linux")]
        {
            readable(self.raw_fd())
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        {
            None
        }
    }
}

pub(crate) trait Kernel {
    fn process(&self, pid: u32) -> Option<Process>;
    fn foreground(&self, master: i32) -> Option<u32>;
    fn watch(&self, process: &Process) -> Option<Box<dyn DeathWitness>>;
    fn group_gone(&self, group: u32) -> Option<bool>;
}

struct System;

impl Kernel for System {
    fn process(&self, pid: u32) -> Option<Process> {
        Process::observe(pid)
    }

    fn foreground(&self, master: i32) -> Option<u32> {
        u32::try_from(crate::quit_safety::foreground_pgrp(master))
            .ok()
            .filter(|group| *group > 1)
    }

    fn watch(&self, process: &Process) -> Option<Box<dyn DeathWitness>> {
        Some(Box::new(KernelDeath::watch(process)?))
    }

    fn group_gone(&self, group: u32) -> Option<bool> {
        #[cfg(unix)]
        {
            let group = i32::try_from(group).ok().filter(|group| *group > 1)?;
            // SAFETY: positive representable group, negated to address only that cohort;
            // signal zero observes existence without delivering a signal to any process.
            if unsafe { libc::kill(-group, 0) } == 0 {
                Some(false)
            } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                Some(true)
            } else {
                None
            }
        }
        #[cfg(not(unix))]
        {
            let _ = group;
            None
        }
    }
}

/// Capacity belongs to the owned witness, even if an external Arc/Weak race outlives its
/// index entry. Reserving a slot precedes opening any kernel descriptor.
#[derive(Debug)]
struct Slot {
    count: Arc<AtomicUsize>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Shared ownership preserves the original kernel watch through a later prepare/drain.
/// This is evidence only: callers must reprove it and make normal atomic native admission.
#[derive(Debug)]
pub(crate) struct Evidence {
    registration: Registration,
    binding: ObservationBinding,
    context: Context,
    expires_us: AtomicU64,
    invalidated: AtomicBool,
    refresh_closed: AtomicBool,
    death: Arc<dyn DeathWitness>,
    // Field order releases capacity only AFTER the owned descriptor has closed.
    reservation: ReservationOwner,
}

impl Evidence {
    pub(crate) fn vendor(&self) -> &Process {
        &self.registration.vendor
    }

    pub(crate) fn shell(&self) -> &Process {
        &self.registration.shell
    }

    pub(crate) fn binding(&self) -> &ObservationBinding {
        &self.binding
    }

    fn unexpired(&self, now_us: u64) -> bool {
        !self.invalidated.load(Ordering::Acquire)
            && self
                .expires_us
                .load(Ordering::Acquire)
                .checked_sub(now_us)
                .is_some_and(|left| left != 0 && left <= LIFETIME_US)
    }

    fn guards(&self, context: &Context, master: i32, kernel: &dyn Kernel) -> bool {
        (match &self.reservation {
            ReservationOwner::Live { .. } => self.context.matches(context),
            ReservationOwner::Outcome { .. } => self.context.outcome_matches(context),
        }) && kernel
            .process(context.actor.pid)
            .is_some_and(|actual| same_actor(&context.actor, &actual))
            && kernel.process(self.shell().pid).as_ref() == Some(self.shell())
            && self.vendor().parent == self.shell().pid
            && self.vendor().session == self.shell().session
            && kernel.foreground(master).is_some()
    }

    fn current(&self, context: &Context, master: i32, now_us: u64, kernel: &dyn Kernel) -> bool {
        if !self.unexpired(now_us) || !self.guards(context, master, kernel) {
            self.invalidated.store(true, Ordering::Release);
            return false;
        }
        true
    }

    fn refresh(&self, context: &Context, master: i32, now_us: u64, kernel: &dyn Kernel) -> Reply {
        if !self.current(context, master, now_us, kernel)
            || self.refresh_closed.load(Ordering::Acquire)
        {
            return Reply::Unknown;
        }
        if self.death.exited() != Some(false)
            || kernel.process(self.vendor().pid).as_ref() != Some(self.vendor())
            || kernel.foreground(master) != Some(self.vendor().group)
        {
            // A late live hook must not destroy an already retained crash witness. It only
            // freezes refresh: later proof still needs positive kernel death and all guards.
            self.refresh_closed.store(true, Ordering::Release);
            return Reply::Unknown;
        }
        let Some(expires) = now_us.checked_add(LIFETIME_US) else {
            return Reply::Unknown;
        };
        self.expires_us.store(expires, Ordering::Release);
        Reply::Registered(self.binding.clone())
    }

    /// Freshly prove the same retained death and exact named group quiescence after any
    /// predecessor drain. Detached descendants outside this cohort are not universally proved.
    pub(crate) fn reprove_with(
        &self,
        context: &Context,
        master: i32,
        now_us: u64,
        kernel: &dyn Kernel,
    ) -> bool {
        if !self.current(context, master, now_us, kernel) {
            return false;
        }
        if self.death.exited() != Some(true)
            || kernel.group_gone(self.vendor().group) != Some(true)
            || kernel.foreground(master) != Some(self.shell().group)
        {
            return false;
        }
        self.refresh_closed.store(true, Ordering::Release);
        self.current(context, master, now_us, kernel)
            && kernel.foreground(master) == Some(self.shell().group)
    }
}

#[derive(Debug)]
struct Reserved {
    registration: Registration,
    context: Context,
    expires_us: u64,
    _slot: Slot,
}

#[derive(Debug)]
enum ReservationOwner {
    Live { _reserved: Arc<Reserved> },
    Outcome { _reserved: Arc<OutcomeReserved> },
}

#[derive(Debug)]
struct OutcomeReserved {
    id: String,
    capability: String,
    context: Context,
    expires_us: u64,
    _slot: Slot,
}

/// Capacity for one future passive outcome, reserved before native admission/effects.
/// No watch, native operation, custody or mutation budget is created by this value.
/// Stable session/actor/conversation facts and expiry are retained; fresh outcome capture,
/// not this reservation, fixes the later observation's person sequence and authority revision.
#[must_use]
#[derive(Debug)]
pub(crate) struct OutcomeSlot {
    reserved: Arc<OutcomeReserved>,
}

/// Capacity only, never permission. Store this inside the shared native watch after its FD
/// fields, so every outliving Arc keeps the slot until the actual descriptor has closed.
#[must_use]
#[derive(Debug)]
pub(crate) struct OutcomeHold {
    reserved: Arc<OutcomeReserved>,
}

/// An exact mapped outcome was positively observed while this same watch was live.
/// Later death before publication does not undo that observation or open another watch.
#[must_use]
#[derive(Debug)]
pub(crate) struct OutcomeCapture {
    death: Arc<dyn DeathWitness>,
    reserved: Arc<OutcomeReserved>,
    registration: Registration,
    context: Context,
}

/// Capture refusal retains both owners for disposal only AFTER the native predicates end.
#[must_use]
#[derive(Debug)]
pub(crate) struct OutcomeCaptureRefusal {
    pub(crate) death: Arc<dyn DeathWitness>,
    pub(crate) slot: OutcomeSlot,
}

impl OutcomeSlot {
    /// Expiry or changed stable facts can refuse passive handoff without preventing
    /// completion of an already positively observed mutation. This never renews retention.
    pub(crate) fn is_current(&self, context: &Context, now_us: u64) -> bool {
        self.reserved.context.stable_matches(context)
            && self
                .reserved
                .expires_us
                .checked_sub(now_us)
                .is_some_and(|left| left != 0 && left <= LIFETIME_US)
    }
    /// Pin capacity in the existing shared watch before moving this slot into capture.
    /// This copies no descriptor and conveys no right to capture or publish an outcome.
    pub(crate) fn hold(&self) -> OutcomeHold {
        OutcomeHold {
            reserved: Arc::clone(&self.reserved),
        }
    }

    /// Capture under the trusted native outcome predicate. This opens no descriptor and
    /// supplies no permission: the caller already owns the positively mapped child/watch.
    /// The native operation independently admits effects and person changes; this binds its
    /// fresh host-verified outcome context while preserving the reservation's stable facts.
    /// Keep either result in a carrier declared before predicate guards; dispose it outside.
    #[expect(
        clippy::too_many_arguments,
        reason = "the pure kernel seam preserves every native capture fact explicitly"
    )]
    pub(crate) fn capture_with(
        self,
        vendor: &Process,
        shell: &Process,
        context: &Context,
        master: i32,
        now_us: u64,
        death: Arc<dyn DeathWitness>,
        kernel: &dyn Kernel,
    ) -> Result<OutcomeCapture, OutcomeCaptureRefusal> {
        let registration = Registration {
            id: self.reserved.id.clone(),
            host_epoch: self.reserved.context.host_epoch.clone(),
            vendor: vendor.clone(),
            shell: shell.clone(),
        };
        let fresh = self.reserved.context.stable_matches(context)
            && death
                .outcome_hold()
                .is_some_and(|hold| Arc::ptr_eq(&hold.reserved, &self.reserved))
            && self
                .reserved
                .expires_us
                .checked_sub(now_us)
                .is_some_and(|left| left != 0 && left <= LIFETIME_US)
            && registration.is_valid()
            && death.exited() == Some(false)
            && live(&registration, context, master, kernel)
            && kernel.process(vendor.pid).as_ref() == Some(vendor)
            && death.exited() == Some(false);
        if !fresh {
            return Err(OutcomeCaptureRefusal { slot: self, death });
        }
        Ok(OutcomeCapture {
            death,
            reserved: self.reserved,
            registration,
            context: context.clone(),
        })
    }
}

/// Data-only receipt. No FD or watch enters a completed native operation's history.
#[derive(Clone, Debug)]
pub(crate) struct PublishedOutcome {
    pub(crate) registration: Registration,
    pub(crate) binding: ObservationBinding,
}

#[must_use]
pub(crate) struct OutcomeReservationResult {
    reservation: Option<OutcomeSlot>,
    retired: Retired,
}

impl OutcomeReservationResult {
    pub(crate) fn into_parts(self) -> (Option<OutcomeSlot>, Retired) {
        (self.reservation, self.retired)
    }
}

#[must_use]
pub(crate) struct OutcomePublication {
    outcome: Option<PublishedOutcome>,
    retired: Retired,
}

impl OutcomePublication {
    pub(crate) fn into_parts(self) -> (Option<PublishedOutcome>, Retired) {
        (self.outcome, self.retired)
    }
}

enum Candidate {
    Existing(Arc<Evidence>),
    New(Arc<Reserved>),
}

/// An unarmed reservation. It is not clonable: only its consuming arm can open a watch.
/// Drop it outside metadata/operation locks, as with an armed preparation.
#[must_use]
pub(crate) struct Reservation {
    candidate: Candidate,
}

struct Armed {
    // Keep the death witness BEFORE the reservation: capacity returns after its FD closes.
    death: Arc<dyn DeathWitness>,
    reserved: Arc<Reserved>,
    capability: String,
}

enum PreparedKind {
    Existing(Arc<Evidence>),
    New(Armed),
}

/// Watch construction has finished. Publication or refusal carries its ownership explicitly.
#[must_use]
pub(crate) struct Prepared {
    kind: PreparedKind,
}

/// The serving lane must dispose this value only AFTER all metadata, operation and store
/// guards end. It owns every watch removed from storage or refused at final publication.
#[must_use]
#[derive(Default)]
pub(crate) struct Retired {
    evidence: Vec<Arc<Evidence>>,
    prepared: Vec<Prepared>,
    outcomes: Vec<OutcomeCapture>,
}

impl Retired {
    pub(crate) fn is_empty(&self) -> bool {
        self.evidence.is_empty() && self.prepared.is_empty() && self.outcomes.is_empty()
    }
}

/// Reservation and deferred disposal, separated so no watch closes beneath a store lock.
#[must_use]
pub(crate) struct ReservationResult {
    reservation: Option<Reservation>,
    retired: Retired,
}

impl ReservationResult {
    pub(crate) fn into_parts(self) -> (Option<Reservation>, Retired) {
        (self.reservation, self.retired)
    }
}

/// Registration acknowledgement plus watch resources to dispose after all guards end.
#[must_use]
pub(crate) struct Publication {
    reply: Reply,
    retired: Retired,
}

impl Publication {
    pub(crate) fn into_parts(self) -> (Reply, Retired) {
        (self.reply, self.retired)
    }
}

fn live(registration: &Registration, context: &Context, master: i32, kernel: &dyn Kernel) -> bool {
    registration.vendor.pid != registration.shell.pid
        && registration.vendor.parent == registration.shell.pid
        && registration.vendor.session == registration.shell.session
        && kernel.process(registration.vendor.pid).as_ref() == Some(&registration.vendor)
        && kernel.process(registration.shell.pid).as_ref() == Some(&registration.shell)
        && kernel
            .process(context.actor.pid)
            .is_some_and(|actual| same_actor(&context.actor, &actual))
        && kernel.foreground(master) == Some(registration.vendor.group)
}

impl Reservation {
    /// Call with NO metadata, operation, or store lock held. The watch constructor may take
    /// the process-wide spawn/CLOEXEC lock. Failure drops only this proposed reservation.
    pub(crate) fn arm(self, master: i32) -> Option<Prepared> {
        self.arm_with(master, &System, || {
            let mut bytes = [0; 32];
            aterm_uds::rand::fill(&mut bytes).ok()?;
            Some(aterm_codec::hex::encode_lower(&bytes))
        })
    }

    fn arm_with(
        self,
        master: i32,
        kernel: &dyn Kernel,
        capability: impl FnOnce() -> Option<String>,
    ) -> Option<Prepared> {
        let reserved = match self.candidate {
            Candidate::Existing(record) => {
                return Some(Prepared {
                    kind: PreparedKind::Existing(record),
                });
            }
            Candidate::New(reserved) => reserved,
        };
        if !live(&reserved.registration, &reserved.context, master, kernel) {
            return None;
        }
        let death: Arc<dyn DeathWitness> = Arc::from(kernel.watch(&reserved.registration.vendor)?);
        if death.exited() != Some(false)
            || !live(&reserved.registration, &reserved.context, master, kernel)
        {
            return None;
        }
        let capability = capability().filter(|cap| aterm_codec::hex::is_lower(cap, 64))?;
        Some(Prepared {
            kind: PreparedKind::New(Armed {
                death,
                reserved,
                capability,
            }),
        })
    }
}

/// At most two proposed or owned kernel witnesses per session, including evidence handed to pending
/// native work. Referenced records cannot be evicted to allocate additional descriptors.
#[derive(Debug, Default)]
pub(crate) struct Store {
    entries: VecDeque<Arc<Evidence>>,
    retired: VecDeque<String>,
    slots: Arc<AtomicUsize>,
    pending: BTreeMap<String, Weak<Reserved>>,
    outcomes: BTreeMap<String, Weak<OutcomeReserved>>,
}

impl Store {
    fn allocate_slot(&mut self, retired: &mut Retired) -> Option<Slot> {
        if self.entries.len() == MAX_RECORDS {
            let index = self
                .entries
                .iter()
                .position(|record| Arc::strong_count(record) == 1)?;
            self.retire(index, retired);
        }
        self.slots
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_RECORDS).then_some(count + 1)
            })
            .ok()?;
        Some(Slot {
            count: Arc::clone(&self.slots),
        })
    }

    /// Reserve one future outcome under STORE ONLY, before native admission/effects. This
    /// creates no watch. Release the lock before disposing Retired; if that disposal returned
    /// capacity, one new reservation attempt may then use it.
    pub(crate) fn reserve_outcome(
        &mut self,
        context: &Context,
        now_us: u64,
    ) -> OutcomeReservationResult {
        self.reserve_outcome_with(context, now_us, || {
            let mut bytes = [0; 48];
            aterm_uds::rand::fill(&mut bytes).ok()?;
            Some((
                aterm_codec::hex::encode_lower(&bytes[..16]),
                aterm_codec::hex::encode_lower(&bytes[16..]),
            ))
        })
    }

    fn reserve_outcome_with(
        &mut self,
        context: &Context,
        now_us: u64,
        keys: impl FnOnce() -> Option<(String, String)>,
    ) -> OutcomeReservationResult {
        let mut retired = Retired::default();
        self.prune(now_us, &mut retired);
        self.pending
            .retain(|_, pending| pending.strong_count() != 0);
        self.outcomes
            .retain(|_, pending| pending.strong_count() != 0);
        let unknown = |retired| OutcomeReservationResult {
            reservation: None,
            retired,
        };
        if !context.valid() {
            return unknown(retired);
        }
        let Some(expires_us) = now_us.checked_add(LIFETIME_US) else {
            return unknown(retired);
        };
        let Some((id, capability)) = keys() else {
            return unknown(retired);
        };
        if !aterm_codec::hex::is_lower(&id, 32)
            || !aterm_codec::hex::is_lower(&capability, 64)
            || self
                .entries
                .iter()
                .any(|record| record.registration.id == id)
            || self.pending.contains_key(&id)
            || self.outcomes.contains_key(&id)
            || self.retired.contains(&id)
        {
            return unknown(retired);
        }
        let Some(slot) = self.allocate_slot(&mut retired) else {
            return unknown(retired);
        };
        let reserved = Arc::new(OutcomeReserved {
            id: id.clone(),
            capability,
            context: context.clone(),
            expires_us,
            _slot: slot,
        });
        self.outcomes.insert(id, Arc::downgrade(&reserved));
        OutcomeReservationResult {
            reservation: Some(OutcomeSlot { reserved }),
            retired,
        }
    }

    /// Publish an already watched, positively observed outcome under fresh metadata then
    /// store guards. A death after capture is retained, not mistaken for failed registration.
    /// No new descriptor opens and no watch closes here. Dispose Retired after ALL guards.
    pub(crate) fn publish_outcome_with(
        &mut self,
        capture: OutcomeCapture,
        context: &Context,
        master: i32,
        now_us: u64,
        kernel: &dyn Kernel,
    ) -> OutcomePublication {
        let mut retired = Retired::default();
        let reserved = &capture.reserved;
        let owned = self
            .outcomes
            .get(&reserved.id)
            .and_then(Weak::upgrade)
            .is_some_and(|pending| Arc::ptr_eq(&pending, reserved));
        let fresh = owned
            && !self.retired.contains(&reserved.id)
            && self.entries.len() < MAX_RECORDS
            && reserved
                .expires_us
                .checked_sub(now_us)
                .is_some_and(|left| left != 0 && left <= LIFETIME_US)
            && capture.context.outcome_matches(context)
            && kernel
                .process(context.actor.pid)
                .is_some_and(|actual| same_actor(&context.actor, &actual))
            && kernel.process(capture.registration.shell.pid).as_ref()
                == Some(&capture.registration.shell)
            && kernel.foreground(master).is_some_and(|group| {
                group == capture.registration.vendor.group
                    || group == capture.registration.shell.group
            });
        if owned {
            self.outcomes.remove(&reserved.id);
        }
        if !fresh {
            self.retire_id(reserved.id.clone());
            retired.outcomes.push(capture);
            return OutcomePublication {
                outcome: None,
                retired,
            };
        }
        let OutcomeCapture {
            death,
            reserved,
            registration,
            context: captured_context,
        } = capture;
        let binding = ObservationBinding {
            id: reserved.id.clone(),
            host_epoch: context.host_epoch.clone(),
            capability: reserved.capability.clone(),
        };
        self.entries.push_back(Arc::new(Evidence {
            registration: registration.clone(),
            binding: binding.clone(),
            context: captured_context,
            expires_us: AtomicU64::new(reserved.expires_us),
            invalidated: AtomicBool::new(false),
            refresh_closed: AtomicBool::new(false),
            death,
            reservation: ReservationOwner::Outcome {
                _reserved: reserved,
            },
        }));
        OutcomePublication {
            outcome: Some(PublishedOutcome {
                registration,
                binding,
            }),
            retired,
        }
    }

    fn retire(&mut self, index: usize, retired: &mut Retired) {
        let record = self.entries.remove(index).expect("existing observation");
        self.retire_id(record.registration.id.clone());
        retired.evidence.push(record);
    }

    fn retire_id(&mut self, id: String) {
        self.retired.push_back(id);
        while self.retired.len() > MAX_HISTORY {
            self.retired.pop_front();
        }
    }

    fn prune(&mut self, now_us: u64, retired: &mut Retired) {
        let mut index = 0;
        while index < self.entries.len() {
            let record = &self.entries[index];
            if Arc::strong_count(record) == 1 && !record.unexpired(now_us) {
                self.retire(index, retired);
            } else {
                index += 1;
            }
        }
    }

    /// Reserve capacity under a brief STORE-ONLY lock, with no metadata/operation lock.
    /// Unlock before disposing retired resources or calling Reservation::arm. If retirement
    /// returned capacity but no reservation, callers may reserve again after that disposal.
    pub(crate) fn reserve(
        &mut self,
        registration: &Registration,
        context: &Context,
        now_us: u64,
    ) -> ReservationResult {
        let mut retired = Retired::default();
        self.prune(now_us, &mut retired);
        self.pending
            .retain(|_, pending| pending.strong_count() != 0);
        self.outcomes
            .retain(|_, pending| pending.strong_count() != 0);
        let unknown = |retired| ReservationResult {
            reservation: None,
            retired,
        };
        if !registration.is_valid()
            || !context.passive_valid()
            || registration.host_epoch != context.host_epoch
        {
            return unknown(retired);
        }
        if let Some(existing) = self
            .entries
            .iter()
            .find(|record| record.registration.id == registration.id)
        {
            return if existing.registration == *registration {
                ReservationResult {
                    reservation: Some(Reservation {
                        candidate: Candidate::Existing(Arc::clone(existing)),
                    }),
                    retired,
                }
            } else {
                unknown(retired)
            };
        }
        if self.retired.contains(&registration.id)
            || self.pending.contains_key(&registration.id)
            || self.outcomes.contains_key(&registration.id)
        {
            return unknown(retired);
        }
        let Some(expires_us) = now_us.checked_add(LIFETIME_US) else {
            return unknown(retired);
        };
        let Some(slot) = self.allocate_slot(&mut retired) else {
            return unknown(retired);
        };
        let reserved = Arc::new(Reserved {
            registration: registration.clone(),
            context: context.clone(),
            expires_us,
            _slot: slot,
        });
        self.pending
            .insert(registration.id.clone(), Arc::downgrade(&reserved));
        ReservationResult {
            reservation: Some(Reservation {
                candidate: Candidate::New(reserved),
            }),
            retired,
        }
    }

    /// Publish under freshly recaptured metadata guards, then the store lock. This opens no
    /// watch and releases none: dispose Publication's Retired only AFTER those guards end.
    pub(crate) fn publish(
        &mut self,
        prepared: Prepared,
        context: &Context,
        master: i32,
        now_us: u64,
    ) -> Publication {
        self.publish_with(prepared, context, master, now_us, &System)
    }

    fn publish_with(
        &mut self,
        prepared: Prepared,
        context: &Context,
        master: i32,
        now_us: u64,
        kernel: &dyn Kernel,
    ) -> Publication {
        let mut retired = Retired::default();
        if let PreparedKind::Existing(record) = &prepared.kind {
            let reply = if self
                .entries
                .iter()
                .any(|current| Arc::ptr_eq(current, record))
            {
                record.refresh(context, master, now_us, kernel)
            } else {
                Reply::Unknown
            };
            retired.prepared.push(prepared);
            return Publication { reply, retired };
        }
        let PreparedKind::New(armed) = &prepared.kind else {
            unreachable!()
        };
        let reserved = &armed.reserved;
        let owned = self
            .pending
            .get(&reserved.registration.id)
            .and_then(Weak::upgrade)
            .is_some_and(|pending| Arc::ptr_eq(&pending, reserved));
        let fresh = owned
            && !self.retired.contains(&reserved.registration.id)
            && self.entries.len() < MAX_RECORDS
            && reserved
                .expires_us
                .checked_sub(now_us)
                .is_some_and(|left| left != 0 && left <= LIFETIME_US)
            && reserved.context.matches(context)
            && armed.death.exited() == Some(false)
            && live(&reserved.registration, context, master, kernel);
        if owned {
            self.pending.remove(&reserved.registration.id);
        }
        if !fresh {
            self.retire_id(reserved.registration.id.clone());
            retired.prepared.push(prepared);
            return Publication {
                reply: Reply::Unknown,
                retired,
            };
        }
        let PreparedKind::New(Armed {
            death,
            reserved,
            capability,
        }) = prepared.kind
        else {
            unreachable!()
        };
        let binding = ObservationBinding {
            id: reserved.registration.id.clone(),
            host_epoch: context.host_epoch.clone(),
            capability,
        };
        self.entries.push_back(Arc::new(Evidence {
            registration: reserved.registration.clone(),
            binding: binding.clone(),
            context: context.clone(),
            expires_us: AtomicU64::new(reserved.expires_us),
            invalidated: AtomicBool::new(false),
            refresh_closed: AtomicBool::new(false),
            death,
            reservation: ReservationOwner::Live {
                _reserved: reserved,
            },
        }));
        Publication {
            reply: Reply::Registered(binding),
            retired,
        }
    }

    #[cfg(test)]
    fn register_with(
        &mut self,
        registration: &Registration,
        context: &Context,
        master: i32,
        now_us: u64,
        kernel: &dyn Kernel,
        capability: impl FnOnce() -> Option<String>,
    ) -> Reply {
        // The pure driver owns no predicate/store mutex: dispose every returned resource
        // before arming or returning. Production performs the same steps across lock scopes.
        let (mut reservation, retired) = self.reserve(registration, context, now_us).into_parts();
        let reclaimed = !retired.is_empty();
        drop(retired);
        if reservation.is_none() && reclaimed {
            let (next, retired) = self.reserve(registration, context, now_us).into_parts();
            drop(retired);
            reservation = next;
        }
        let Some(prepared) =
            reservation.and_then(|reservation| reservation.arm_with(master, kernel, capability))
        else {
            return Reply::Unknown;
        };
        let (reply, retired) = self
            .publish_with(prepared, context, master, now_us, kernel)
            .into_parts();
        drop(retired);
        reply
    }

    /// Refresh only an existing exact binding, without reacquiring or replacing its watch.
    pub(crate) fn refresh(
        &mut self,
        binding: &ObservationBinding,
        context: &Context,
        master: i32,
        now_us: u64,
    ) -> Reply {
        self.refresh_with(binding, context, master, now_us, &System)
    }

    fn refresh_with(
        &mut self,
        binding: &ObservationBinding,
        context: &Context,
        master: i32,
        now_us: u64,
        kernel: &dyn Kernel,
    ) -> Reply {
        self.entries
            .iter()
            .find(|record| record.binding == *binding)
            .map_or(Reply::Unknown, |record| {
                record.refresh(context, master, now_us, kernel)
            })
    }

    /// A positive retained-death witness plus current guards, never a native mutation grant.
    pub(crate) fn prove_with(
        &mut self,
        binding: &ObservationBinding,
        context: &Context,
        master: i32,
        now_us: u64,
        kernel: &dyn Kernel,
    ) -> Option<Arc<Evidence>> {
        let record = self
            .entries
            .iter()
            .find(|record| record.binding == *binding)?;
        record
            .reprove_with(context, master, now_us, kernel)
            .then(|| Arc::clone(record))
    }
}

#[cfg(test)]
#[path = "native_observe_tests.rs"]
mod tests;
