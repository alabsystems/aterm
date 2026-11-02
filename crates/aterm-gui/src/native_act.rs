// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual native mutation custody. Metadata observations and analytics are never grants.
//! Admission uses Meta -> operation table -> input ordering; mutation uses Meta -> operation
//! predicate -> input ordering. Table lookup drops its guard before an operation predicate.
//! Outcome disposal and predecessor waits hold none of those locks. The table, rather than
//! a request/connection stack, owns every started operation, including a lost final ACK.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_agent::harness::contract::Scope;
use aterm_agent::harness::native_act::{
    Binding, CancelPrepare, ClaimOutcome, Constructor, Decision, Image, ImagePhase, NativePlan,
    Prepare, Process, Request, RestartCommand, Retirement,
};

use crate::session_timeline::{
    NativeCustody, NativeDrain, SupervisorAuthority, SupervisorInputCaller,
};

#[cfg(test)]
#[path = "native_act_tests.rs"]
mod tests;

#[path = "native_observe_control.rs"]
mod observe_control;
pub(crate) use observe_control::serve as serve_observation;

const DENIED: &str = "ERR denied native operation authority\n";
const UNKNOWN: &str = "OK native-act state=unknown\n";
#[cfg(test)]
const COMMITTED: &str = "OK native-act state=committed claim=lost revision=-\n";
const ROLLBACK_SAFE: &str = "OK native-act state=rollback-safe\n";
const MAX_COMPLETED: usize = 128;
const MAX_PREPARE_US: u64 = 120_000_000;

fn host_epoch() -> Option<&'static str> {
    static EPOCH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(|| aterm_agent::harness::native_act::operation_id().ok())
        .as_deref()
}

fn own_image_rows(policy: &crate::native_session_policy::SessionPolicy) -> String {
    let rows = policy
        .rows()
        .iter()
        .filter(|row| row.allows.contains(&Scope::Upgrade))
        .map(|row| format!("{}:upgrade", row.id))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        "-".into()
    } else {
        rows.join(",")
    }
}

/// The one transaction clock is shared with the real self-exec constructor across exec.
pub(crate) fn remaining(deadline_us: u64) -> Option<Duration> {
    Clock::System.remaining(deadline_us)
}

#[derive(Clone, Debug)]
enum Clock {
    System,
    #[cfg(test)]
    Facts(Arc<std::sync::atomic::AtomicU64>),
}

impl Clock {
    fn now_us(&self) -> Option<u64> {
        match self {
            Self::System => aterm_agent::harness::native_act::monotonic_us(),
            #[cfg(test)]
            Self::Facts(now) => Some(now.load(std::sync::atomic::Ordering::Acquire)),
        }
    }
    fn remaining(&self, deadline_us: u64) -> Option<Duration> {
        let left = deadline_us.checked_sub(self.now_us()?)?;
        (left != 0).then(|| Duration::from_micros(left))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Operations {
    observations: Arc<Mutex<crate::native_observe::Store>>,
    by_id: BTreeMap<String, Arc<Mutex<Operation>>>,
    completed: std::collections::VecDeque<String>,
    coordinated_connections: std::collections::BTreeSet<u64>,
    coordinated_actors: Vec<(u32, u64)>,
    builtin_coordinated: bool,
    reservations: std::collections::BTreeSet<String>,
    cancelled: BTreeMap<String, CancelledPrepare>,
}

/// A data-only seal for a preparation that had not published an operation. The original
/// absolute deadline also fences late insertion after this bounded record is reclaimed.
#[derive(Debug)]
struct CancelledPrepare {
    request: Prepare,
    principal: crate::control::Principal,
    actor: Process,
}

impl Operations {
    fn slots(&self) -> usize {
        self.by_id
            .len()
            .saturating_add(self.reservations.len())
            .saturating_add(
                self.cancelled
                    .keys()
                    .filter(|id| !self.reservations.contains(*id))
                    .count(),
            )
    }

    fn reclaim(&mut self, now_us: u64) -> Vec<Arc<Mutex<Operation>>> {
        let expired_seals = self
            .cancelled
            .iter()
            .filter(|(id, seal)| {
                seal.request.deadline_us <= now_us && !self.reservations.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired_seals {
            self.cancelled.remove(&id);
        }
        let expired = self
            .completed
            .iter()
            .filter_map(|id| {
                let operation = self.by_id.get(id)?;
                let op = operation.try_lock().ok()?;
                (op.request.deadline_us <= now_us
                    && matches!(op.phase, Phase::Completed(_))
                    && op
                        .completion_drain
                        .as_ref()
                        .is_none_or(NativeDrain::is_drained))
                .then(|| id.clone())
            })
            .collect::<Vec<_>>();
        let mut retired = Vec::new();
        for id in expired {
            self.completed.retain(|old| old != &id);
            if let Some(operation) = self.by_id.remove(&id) {
                retired.push(operation);
            }
        }
        retired
    }
}

/// A bounded data slot, acquired before any app/input custody or watcher resource. Drop
/// cancels an unpublished reservation; it cannot publish a native plan or renew a deadline.
struct PrepareSlot<'a> {
    ctx: &'a crate::SessionCtx,
    id: String,
    published: bool,
}

impl<'a> PrepareSlot<'a> {
    fn reserve(
        ctx: &'a crate::SessionCtx,
        id: &str,
        now_us: u64,
        crash: bool,
    ) -> Result<Self, String> {
        let (retired, result) = {
            let mut table = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let retired = table.reclaim(now_us);
            let unresolved = crash
                && (!table.reservations.is_empty()
                    || table.by_id.values().any(|operation| {
                        operation.try_lock().map_or(true, |op| {
                            !matches!(op.phase, Phase::Completed(_))
                                || op
                                    .completion_drain
                                    .as_ref()
                                    .is_some_and(|drain| !drain.is_drained())
                        })
                    }));
            let result = if unresolved {
                Err("ERR busy unresolved native operation\n".into())
            } else if table.by_id.contains_key(id)
                || table.reservations.contains(id)
                || table.cancelled.contains_key(id)
            {
                Err("ERR busy native operation prepare\n".into())
            } else if table.slots() >= MAX_COMPLETED {
                Err("ERR busy native operation history\n".into())
            } else {
                table.reservations.insert(id.to_string());
                Ok(())
            };
            (retired, result)
        };
        // Completed history has data only; even its final Arc destruction stays outside every
        // metadata, native predicate and table lock, including a retained input observation.
        drop(retired);
        result.map(|()| Self {
            ctx,
            id: id.to_string(),
            published: false,
        })
    }

    fn publish(&mut self, table: &mut Operations, operation: Arc<Mutex<Operation>>) {
        table.reservations.remove(&self.id);
        table.by_id.insert(self.id.clone(), operation);
        self.published = true;
    }
}

impl Drop for PrepareSlot<'_> {
    fn drop(&mut self) {
        if !self.published {
            self.ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .reservations
                .remove(&self.id);
        }
    }
}

#[derive(Clone, Debug)]
struct Authority {
    snapshot: SupervisorAuthority,
    caller: SupervisorInputCaller,
    builtin: bool,
    actor: Option<Process>,
    conversation: Option<Conversation>,
}

#[derive(Clone, Debug)]
enum Conversation {
    Host(String),
    #[cfg(test)]
    Facts(Arc<std::sync::atomic::AtomicBool>),
}

impl Conversation {
    fn current(&self, ctx: &crate::SessionCtx) -> bool {
        match self {
            Self::Host(expected) => current_conversation(ctx).as_ref() == Some(expected),
            #[cfg(test)]
            Self::Facts(valid) => valid.load(std::sync::atomic::Ordering::Acquire),
        }
    }
}

fn snapshot_conversation(snapshot: &aterm_agent::harness::relaunch::Snapshot) -> Option<String> {
    let session = snapshot
        .session
        .as_deref()
        .filter(|session| !session.is_empty())?;
    let root = snapshot.claude_dir.as_ref()?;
    #[cfg(unix)]
    let path = |path: &std::path::Path| {
        use std::os::unix::ffi::OsStrExt;
        aterm_codec::hex::encode_lower(path.as_os_str().as_bytes())
    };
    #[cfg(not(unix))]
    let path = |path: &std::path::Path| path.to_string_lossy().to_string();
    // This is the logical record key. Exact process birth/image and shell facts are bound
    // independently; rotating the vendor must not change the same record's identity.
    let identity = aterm_json::to_string(&vec![path(root), session.to_string()]).ok()?;
    Some(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(identity.as_bytes()),
    ))
}

fn current_conversation(ctx: &crate::SessionCtx) -> Option<String> {
    snapshot_conversation(&crate::harness_host::published_snapshot(
        ctx.self_id.as_str(),
    )?)
}

#[derive(Debug)]
struct Operation {
    request: Prepare,
    binding: Binding,
    authority: Authority,
    custody: Option<Arc<NativeCustody>>,
    phase: Phase,
    signal_sent: bool,
    signal_accepted: Option<bool>,
    resume_sent: bool,
    resume_attempt: Option<Arc<NativeInput>>,
    human_seq: u64,
    channel: Option<u64>,
    channel_closed: bool,
    claim_outcome: ClaimOutcome,
    commit_version: Option<String>,
    completion_drain: Option<NativeDrain>,
    resume_phase: Option<ImagePhase>,
    children: [Option<LaunchChild>; 2],
    effect_started: Arc<std::sync::atomic::AtomicBool>,
    clock: Clock,
    observation: Option<Arc<crate::native_observe::Evidence>>,
    outcome_slot: Option<crate::native_observe::OutcomeSlot>,
    completed_observation: Option<aterm_agent::harness::native_observe::SavedObservation>,
}

#[derive(Debug)]
struct LaunchChild {
    process: Process,
    death: Arc<SharedDeath>,
    connection: u64,
    execute_sent: bool,
    exec_errno: Option<i32>,
}

/// Field order closes the retained descriptor before returning its passive capacity token.
#[derive(Debug)]
struct SharedDeath {
    watch: DeathWatch,
    outcome: std::sync::OnceLock<crate::native_observe::OutcomeHold>,
}

impl SharedDeath {
    fn exited(&self) -> bool {
        self.watch.exited()
    }
}

impl crate::native_observe::DeathWitness for SharedDeath {
    fn exited(&self) -> Option<bool> {
        self.watch.state()
    }
    fn outcome_hold(&self) -> Option<&crate::native_observe::OutcomeHold> {
        self.outcome.get()
    }
}

/// One kernel view drives native predicates and retained-observation predicates alike.
struct ObservationKernel<'a>(&'a dyn Kernel);
impl crate::native_observe::Kernel for ObservationKernel<'_> {
    fn process(&self, pid: u32) -> Option<Process> {
        self.0.process(pid)
    }
    fn foreground(&self, master: i32) -> Option<u32> {
        self.0.foreground(master)
    }
    fn group_gone(&self, group: u32) -> Option<bool> {
        self.0.group_gone(group)
    }
    fn watch(&self, process: &Process) -> Option<Box<dyn crate::native_observe::DeathWitness>> {
        Some(Box::new(SharedDeath {
            watch: self.0.watch(process)?,
            outcome: std::sync::OnceLock::new(),
        }))
    }
}

#[derive(Debug)]
enum DeathWatch {
    #[cfg(target_vendor = "apple")]
    Darwin(aterm_uds::exitwatch::ExitWatch),
    #[cfg(target_os = "linux")]
    Linux(std::os::fd::OwnedFd),
    #[cfg(test)]
    Facts(Process, Arc<Mutex<BTreeMap<u32, Process>>>),
}

#[cfg(target_vendor = "apple")]
impl aterm_uds::spawn_lock::Descriptors for DeathWatch {
    fn mark(&self) -> std::io::Result<()> {
        let raw = self
            .raw_fd()
            .ok_or_else(|| std::io::Error::other("kernel watch has no descriptor"))?;
        // SAFETY: the borrowed descriptor remains exclusively owned by this watch throughout.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
        aterm_uds::keep_out_of_children(&fd)
    }
}

impl DeathWatch {
    fn watch(process: &Process) -> Option<Self> {
        #[cfg(target_vendor = "apple")]
        let watch = aterm_uds::kept_out(|| {
            aterm_uds::exitwatch::ExitWatch::watch(process.pid)
                .map(Self::Darwin)
                .ok_or_else(|| std::io::Error::other("kernel exit watch unavailable"))
        })
        .ok()?;
        #[cfg(target_os = "linux")]
        let watch = Self::Linux(aterm_uds::exitwatch::exit_fd(process.pid)?);
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        {
            if aterm_uds::process::read_birth(i32::try_from(process.pid).ok()?)?.0 != process.birth
            {
                return None;
            }
            let raw = watch.raw_fd()?;
            // SAFETY: the watch exclusively owns a live descriptor; F_GETFD only reads flags.
            let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
            (flags >= 0 && flags & libc::FD_CLOEXEC != 0).then_some(watch)
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        {
            let _ = process;
            None
        }
    }

    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn raw_fd(&self) -> Option<i32> {
        match self {
            #[cfg(target_vendor = "apple")]
            Self::Darwin(watch) => Some(watch.as_raw_fd()),
            #[cfg(target_os = "linux")]
            Self::Linux(fd) => {
                use std::os::fd::AsRawFd;
                Some(fd.as_raw_fd())
            }
            #[cfg(test)]
            Self::Facts(..) => None,
        }
    }

    /// A retained kernel watch proves this exact child exited even when exec+exit happened
    /// between process snapshots. Linux supplies no wait status; its readable pidfd is death.
    fn exited(&self) -> bool {
        self.state() == Some(true)
    }

    fn state(&self) -> Option<bool> {
        // On the place, not the reference: where no variant is compiled in (Windows), the type
        // has no value, and an empty match is exhaustive only on a place of it.
        match *self {
            #[cfg(target_vendor = "apple")]
            Self::Darwin(ref watch) => {
                if watch.exit_status().is_some() {
                    Some(true)
                } else {
                    watch_readable(watch.as_raw_fd()).and_then(|ready| (!ready).then_some(false))
                }
            }
            #[cfg(target_os = "linux")]
            Self::Linux(ref fd) => {
                use std::os::fd::AsRawFd;
                watch_readable(fd.as_raw_fd())
            }
            #[cfg(test)]
            Self::Facts(ref original, ref processes) => Some(
                processes
                    .lock()
                    .unwrap()
                    .get(&original.pid)
                    .is_none_or(|actual| actual.birth != original.birth),
            ),
            // A platform with no kernel death watch has no value of this type to match; a
            // reference to it counts as inhabited, so the match names that arm.
            #[cfg(not(any(target_vendor = "apple", target_os = "linux", test)))]
            _ => None,
        }
    }
}

#[cfg(unix)]
fn watch_readable(fd: i32) -> Option<bool> {
    let mut item = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized slot naming a still-owned watch descriptor, and zero timeout.
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

const fn phase_index(phase: ImagePhase) -> usize {
    match phase {
        ImagePhase::Forward => 0,
        ImagePhase::Rollback => 1,
    }
}

fn command_for(op: &Operation, phase: ImagePhase) -> Option<&RestartCommand> {
    let NativePlan::Restart {
        forward, rollback, ..
    } = &op.request.plan
    else {
        return None;
    };
    match phase {
        ImagePhase::Forward => Some(forward),
        ImagePhase::Rollback => rollback.as_deref(),
    }
}

/// Sole ownership of terminal completion side effects. Created while the operation's
/// predicate lock is still held; it cannot be cloned or minted for an already terminal op.
struct Completion {
    id: String,
    custody: Option<Arc<NativeCustody>>,
    children: [Option<LaunchChild>; 2],
    observation: Option<Arc<crate::native_observe::Evidence>>,
    outcome_capture: Option<crate::native_observe::OutcomeCapture>,
    outcome_slot: Option<crate::native_observe::OutcomeSlot>,
}

impl Operation {
    fn seal_completion(&mut self, decision: Decision) -> Option<Completion> {
        if matches!(self.phase, Phase::Completed(_)) {
            return None;
        }
        self.phase = Phase::Completed(decision);
        self.completion_drain = self.custody.as_ref().map(|custody| custody.drain());
        Some(Completion {
            id: self.request.id.clone(),
            custody: self.custody.take(),
            children: std::mem::take(&mut self.children),
            observation: self.observation.take(),
            outcome_capture: None,
            outcome_slot: self.outcome_slot.take(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Queued,
    Started,
    Precommitted(ImagePhase),
    Committing(ImagePhase),
    Completed(Decision),
}

/// Process/image facts are injected in conformance tests; the production arm reads the
/// kernel, never a caller's holder, argv text, pathname alone or status-as-permission.
trait Kernel {
    fn constructor_enabled(&self, kind: Constructor) -> bool;
    fn clock(&self) -> Clock;
    fn process(&self, pid: u32) -> Option<Process>;
    fn image(&self, image: &Image) -> bool;
    fn foreground(&self, master: i32) -> Option<u32>;
    fn own_target(&self, holder: &str, target: &Image) -> bool;
    fn own_launcher(&self, image: &Image) -> bool;
    fn birth_alive(&self, process: &Process) -> Option<bool>;
    fn signal(&self, master: i32, process: &Process, ctx: &crate::SessionCtx) -> String;
    fn watch(&self, process: &Process) -> Option<DeathWatch>;
    fn group_gone(&self, group: u32) -> Option<bool>;
    fn conversation(&self, ctx: &crate::SessionCtx, vendor: &Process) -> Option<Conversation>;
    fn record_key(&self, ctx: &crate::SessionCtx) -> Option<String>;
    fn record_guard(&self, ctx: &crate::SessionCtx) -> Option<Conversation> {
        self.record_key(ctx).map(Conversation::Host)
    }
    fn preparing(&self) {}
    fn reserved(&self) {}
    fn completion_sealed(&self) {}
}

struct System<'a>(&'a crate::native_session_policy::SessionPolicy);

impl Kernel for System<'_> {
    fn constructor_enabled(&self, kind: Constructor) -> bool {
        self.0.constructor_enabled(kind)
    }
    fn clock(&self) -> Clock {
        Clock::System
    }
    fn process(&self, pid: u32) -> Option<Process> {
        Process::observe(pid)
    }

    fn image(&self, image: &Image) -> bool {
        image_exists(image)
    }
    fn own_launcher(&self, image: &Image) -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|path| Image::at(&path))
            .as_ref()
            == Some(image)
    }

    fn foreground(&self, master: i32) -> Option<u32> {
        u32::try_from(crate::quit_safety::foreground_pgrp(master))
            .ok()
            .filter(|p| *p > 1)
    }

    fn own_target(&self, holder: &str, target: &Image) -> bool {
        self.0
            .holder(holder)
            .is_some_and(|(row, _)| self.0.target_image(row, target))
    }

    fn birth_alive(&self, process: &Process) -> Option<bool> {
        process_birth_alive(process)
    }
    fn signal(&self, master: i32, process: &Process, _: &crate::SessionCtx) -> String {
        #[cfg(unix)]
        {
            use aterm_uds::process::EndControl;
            let Ok(pid) = i32::try_from(process.pid) else {
                return DENIED.into();
            };
            let Some(identity) = aterm_uds::process::read_identity(pid) else {
                return DENIED.into();
            };
            let mut control = aterm_uds::process::EndControlLive;
            if identity.birth.0 != process.birth
                || self.process(process.pid).as_ref() != Some(process)
                || self.foreground(master) != Some(process.group)
                || control.identity(pid) != Some(identity)
            {
                return DENIED.into();
            }
            // EndControlLive uses a pidfd on Linux, with a last identity read. Darwin's
            // documented read-to-kill race remains; no pathname/status is a process handle.
            if control.signal(&identity, libc::SIGTERM).is_ok() {
                "OK signalled\n".into()
            } else {
                "ERR native signal refused\n".into()
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (master, process);
            DENIED.into()
        }
    }
    fn watch(&self, process: &Process) -> Option<DeathWatch> {
        DeathWatch::watch(process)
    }
    fn group_gone(&self, group: u32) -> Option<bool> {
        process_group_gone(group)
    }
    fn conversation(&self, ctx: &crate::SessionCtx, vendor: &Process) -> Option<Conversation> {
        let snapshot = crate::harness_host::published_snapshot(ctx.self_id.as_str())?;
        (snapshot.pid == vendor.pid
            && snapshot.shell == vendor.parent
            && Image::at(&snapshot.program).as_ref() == Some(&vendor.image))
        .then(|| snapshot_conversation(&snapshot))
        .flatten()
        .map(Conversation::Host)
    }
    fn record_key(&self, ctx: &crate::SessionCtx) -> Option<String> {
        current_conversation(ctx)
    }
}

#[cfg(unix)]
fn image_exists(image: &Image) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&image.path));
    std::fs::metadata(path).is_ok_and(|m| {
        m.is_file() && m.dev() == image.device && m.ino() == image.inode && m.mode() & 0o111 != 0
    })
}

#[cfg(not(unix))]
fn image_exists(_: &Image) -> bool {
    false
}

fn capture_native_authority(
    ctx: &crate::SessionCtx,
    meta: &crate::session_timeline::SessionMeta,
    request: &Prepare,
    caller: &SupervisorInputCaller,
    actor: Option<Process>,
    conversation: Option<Conversation>,
    now_us: u64,
) -> Result<Authority, String> {
    let snapshot = meta.supervisor_authority(now_us);
    if !matches!(request.duty, Scope::Upgrade | Scope::Relaunch)
        || request.revision != snapshot.revision()
        || caller.connection.is_none()
    {
        return Err(DENIED.into());
    }
    let builtin = caller.principal == crate::control::Principal::Harness;
    if builtin {
        if request.holder != crate::harness_host::holder()
            || !snapshot.builtin_keeps(&request.holder, request.duty)
            || !matches!(request.plan, NativePlan::Restart { .. })
            || request.expected_human_seq != Some(ctx.human_input.seq())
            || actor
                .as_ref()
                .is_none_or(|actor| caller.peer != Some(actor.pid) || actor.birth == 0)
            || conversation
                .as_ref()
                .is_none_or(|guard| !guard.current(ctx))
        {
            return Err(DENIED.into());
        }
    } else if snapshot.contract() != Some(3)
        || snapshot.holder() != Some(request.holder.as_str())
        || !snapshot.scopes().is_some_and(|s| s.contains(request.duty))
        || snapshot.connection() != caller.connection
        || snapshot.identity() != caller.proof.as_ref()
        || snapshot.identity().map(|p| p.pid) != caller.peer
        || actor.as_ref().map(|p| p.pid) != caller.peer
        || !matches!(request.plan, NativePlan::OwnImage { .. })
    {
        return Err(DENIED.into());
    }
    let _ = ctx;
    Ok(Authority {
        snapshot,
        caller: caller.clone(),
        builtin,
        actor,
        conversation,
    })
}

fn validate_plan(
    request: &Prepare,
    authority: &Authority,
    master: i32,
    kernel: &dyn Kernel,
    crash_proven: bool,
) -> bool {
    match &request.plan {
        NativePlan::OwnImage {
            child,
            target,
            version,
        } => {
            let Some(actor) = authority.actor.as_ref() else {
                return false;
            };
            // The vendor child may own an inner PTY/session. Preserve those exact child
            // facts across exec; requiring the outer supervisor's session would reject it.
            child.parent == actor.pid
                && *target != actor.image
                && kernel.process(actor.pid).is_some_and(|actual| {
                    own_actor_matches(&actual, actor) && actual.image == actor.image
                })
                && kernel.foreground(master) == Some(actor.group)
                && kernel.process(child.pid).as_ref() == Some(child)
                && kernel.image(target)
                && kernel.own_target(&request.holder, target)
                && aterm_agent::harness::contract::ClaimVersion::parse(version).is_some()
        }
        NativePlan::Restart {
            process,
            observation,
            shell,
            launcher,
            socket,
            forward,
            rollback,
        } => {
            authority.builtin
                && process.pid != shell.pid
                && process.group == process.pid
                && process.parent == shell.pid
                && process.session == shell.session
                && if observation.is_some() {
                    crash_proven && kernel.foreground(master) == Some(shell.group)
                } else {
                    kernel.foreground(master) == Some(process.group)
                        && kernel.process(process.pid).as_ref() == Some(process)
                }
                && kernel.process(shell.pid).as_ref() == Some(shell)
                && kernel.image(launcher)
                && kernel.own_launcher(launcher)
                && kernel.image(&forward.image)
                && forward.encode().is_some()
                && std::path::Path::new(socket).is_absolute()
                && rollback.as_ref().is_none_or(|back| {
                    back.image == process.image
                        && kernel.image(&back.image)
                        && back.encode().is_some()
                })
        }
    }
}

fn capability() -> Option<String> {
    let mut bytes = [0; 32];
    aterm_uds::rand::fill(&mut bytes).ok()?;
    Some(aterm_codec::hex::encode_lower(&bytes))
}

fn admitted(binding: &Binding) -> String {
    format!(
        "OK native-act state=admitted id={} cap={}\n",
        binding.id, binding.capability
    )
}

fn committed(claim: ClaimOutcome) -> String {
    let (claim, revision) = match claim {
        ClaimOutcome::Bound(revision) => ("bound", revision.to_string()),
        ClaimOutcome::Lost => ("lost", "-".to_string()),
        ClaimOutcome::Builtin => ("builtin", "-".to_string()),
    };
    format!("OK native-act state=committed claim={claim} revision={revision}\n")
}

fn cancelled(request: &Prepare, actor: &Process) -> String {
    Request::cancel_prepare(request.clone(), actor.clone())
        .encode()
        .map_or_else(
            || DENIED.into(),
            |original| {
                format!(
                    "OK native-act state=cancelled request={}\n",
                    aterm_codec::hex::encode_lower(original.as_bytes())
                )
            },
        )
}

fn cancelled_actor(
    seal: &CancelledPrepare,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> bool {
    caller.principal == seal.principal
        && caller.connection.is_some()
        && caller.peer == Some(seal.actor.pid)
        && kernel.process(seal.actor.pid).is_some_and(|actual| {
            own_actor_matches(&actual, &seal.actor) && actual.image == seal.actor.image
        })
}

/// Cancellation is not admission: an absent original needs no current claim/connection.
/// Its fresh roster-image proof, exact live owned child and kernel actor bind the no-future
/// seal; an existing original additionally binds the actor captured before admission.
fn cancel_prepare(
    ctx: &crate::SessionCtx,
    master: i32,
    cancel: CancelPrepare,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    let CancelPrepare {
        prepare: request,
        actor: original_actor,
    } = cancel;
    if host_epoch() != Some(request.host_epoch.as_str()) {
        return UNKNOWN.into();
    }
    let NativePlan::OwnImage { child, target, .. } = &request.plan else {
        return DENIED.into();
    };
    let Some(now) = kernel.clock().now_us() else {
        return UNKNOWN.into();
    };
    if request.deadline_us.saturating_sub(now) > MAX_PREPARE_US {
        return DENIED.into();
    }
    // Retirement may release a final Arc; keep it outside metadata and operation guards.
    let (previous, retired) = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let previous = table.by_id.get(&request.id).cloned();
        (previous, table.reclaim(now))
    };
    drop(retired);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(operation) = table.by_id.get(&request.id).cloned().or(previous) {
        drop(table);
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op.request != request
            || !same_actor(&op, &caller, kernel)
            || op.authority.actor.as_ref().is_none_or(|actor| {
                !own_actor_matches(&original_actor, actor) || original_actor.image != actor.image
            })
        {
            return DENIED.into();
        }
        if matches!(op.phase, Phase::Completed(_)) {
            drop(op);
            drop(meta);
            let reply = completed_reply(&operation);
            return if reply == "OK native-act state=rollback-safe\n" {
                cancelled(&request, &original_actor)
            } else {
                reply
            };
        }
        let before_mutation = op.authority.actor.as_ref().is_some_and(|original| {
            kernel.process(original.pid).is_some_and(|actual| {
                own_actor_matches(&actual, original) && actual.image == original.image
            }) && kernel.process(child.pid).as_ref() == Some(child)
        });
        if !matches!(op.phase, Phase::Queued | Phase::Started) || !before_mutation {
            return UNKNOWN.into();
        }
        let completion = op.seal_completion(Decision::ProvenNotCommittedRollbackSafe);
        drop(op);
        drop(meta);
        kernel.completion_sealed();
        let reply = finish(ctx, &operation, completion);
        return if reply == "OK native-act state=rollback-safe\n" {
            cancelled(&request, &original_actor)
        } else {
            reply
        };
    }
    if let Some(seal) = table.cancelled.get(&request.id) {
        return if seal.request == request
            && cancelled_actor(seal, &caller, kernel)
            && own_actor_matches(&seal.actor, &original_actor)
            && seal.actor.image == original_actor.image
        {
            cancelled(&request, &original_actor)
        } else {
            DENIED.into()
        };
    }
    if original_actor.image == *target {
        return UNKNOWN.into();
    }
    let actor = caller.peer.and_then(|pid| kernel.process(pid));
    let Some(actor) = actor else {
        return UNKNOWN.into();
    };
    let rostered = ctx
        .native_policy
        .holder(&request.holder)
        .is_some_and(|(_, pid)| pid == actor.pid);
    if caller.principal == crate::control::Principal::Harness
        || caller.connection.is_none()
        || !rostered
        || caller.proof.as_ref().map(|proof| proof.pid) != Some(actor.pid)
        || actor.birth == 0
        || !own_actor_matches(&actor, &original_actor)
        || actor.image != original_actor.image
        || child.parent != actor.pid
        || kernel.process(child.pid).as_ref() != Some(child)
        || kernel.foreground(master) != Some(actor.group)
    {
        return DENIED.into();
    }
    // A pending reservation already owns this id's bounded slot. Publishing the seal
    // leaves that reservation live so neither its late worker nor pressure can erase it.
    if !table.reservations.contains(&request.id) && table.slots() >= MAX_COMPLETED {
        return "ERR busy native operation history\n".into();
    }
    table.cancelled.insert(
        request.id.clone(),
        CancelledPrepare {
            request: request.clone(),
            principal: caller.principal,
            actor: original_actor.clone(),
        },
    );
    cancelled(&request, &original_actor)
}

fn prepare(
    ctx: &crate::SessionCtx,
    master: i32,
    request: Prepare,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    if host_epoch() != Some(request.host_epoch.as_str()) {
        return "ERR native host epoch changed\n".into();
    }
    {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(seal) = table.cancelled.get(&request.id) {
            return if seal.request == request && cancelled_actor(seal, &caller, kernel) {
                cancelled(&request, &seal.actor)
            } else {
                DENIED.into()
            };
        }
    }
    let previous = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&request.id)
        .cloned();
    if let Some(previous) = previous {
        let op = previous.lock().unwrap_or_else(|p| p.into_inner());
        return if op.request == request && same_actor(&op, &caller, kernel) {
            match op.phase {
                Phase::Started | Phase::Precommitted(_) => admitted(&op.binding),
                Phase::Committing(_) => UNKNOWN.into(),
                Phase::Completed(_) => {
                    drop(op);
                    completed_reply(&previous)
                }
                Phase::Queued => format!(
                    "OK native-act state=pending id={} cap={}\n",
                    op.binding.id, op.binding.capability
                ),
            }
        } else {
            DENIED.into()
        };
    }
    if !kernel.constructor_enabled(request.plan.constructor()) {
        return "ERR native constructor disabled\n".into();
    }
    let clock = kernel.clock();
    let Some(now) = clock.now_us() else {
        return DENIED.into();
    };
    if request
        .deadline_us
        .checked_sub(now)
        .is_none_or(|left| left == 0 || left > MAX_PREPARE_US)
    {
        return "ERR native operation deadline\n".into();
    }
    let crash = matches!(
        &request.plan,
        NativePlan::Restart {
            observation: Some(_),
            ..
        }
    );
    let mut slot = match PrepareSlot::reserve(ctx, &request.id, now, crash) {
        Ok(slot) => slot,
        Err(refusal) => return refusal,
    };
    let Some(capability) = capability() else {
        return "ERR native entropy unavailable\n".into();
    };
    kernel.preparing();
    let observations = observe_control::store(ctx);
    // Every watch/slot carrier precedes metadata guards, so refusal disposal stays outside.
    let mut observation = None;
    let outcome_slot = if matches!(request.plan, NativePlan::Restart { .. }) {
        let initial = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let Some(context) = observe_control::context(ctx, &meta, &caller, kernel) else {
                return DENIED.into();
            };
            if let NativePlan::Restart {
                observation: Some(binding),
                ..
            } = &request.plan
            {
                observation = observations
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .prove_with(binding, &context, master, now, &ObservationKernel(kernel));
                if observation.is_none() {
                    return UNKNOWN.into();
                }
            }
            context
        };
        let (mut reserved, retired) = observations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .reserve_outcome(&initial, now)
            .into_parts();
        let reclaimed = !retired.is_empty();
        drop(retired);
        if reserved.is_none() && reclaimed {
            let (next, retired) = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .reserve_outcome(&initial, now)
                .into_parts();
            drop(retired);
            reserved = next;
        }
        let Some(reserved) = reserved else {
            return "ERR busy native passive outcome capacity\n".into();
        };
        Some(reserved)
    } else {
        None
    };
    let actor = caller.peer.and_then(|pid| kernel.process(pid));
    let conversation = match &request.plan {
        NativePlan::Restart {
            observation: Some(_),
            ..
        } => kernel.record_guard(ctx),
        NativePlan::Restart { process, .. } => kernel.conversation(ctx, process),
        NativePlan::OwnImage { .. } => None,
    };
    // Declared before Meta so refused passive proof cannot close its watch under a guard.
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = match capture_native_authority(
        ctx,
        &meta,
        &request,
        &caller,
        actor,
        conversation,
        crate::metrics::now_us(),
    ) {
        Ok(authority) => authority,
        Err(refusal) => return refusal,
    };
    if let NativePlan::Restart {
        process,
        shell,
        observation: Some(binding),
        ..
    } = &request.plan
    {
        let Some(context) = observe_control::context(ctx, &meta, &caller, kernel) else {
            return DENIED.into();
        };
        if observation.as_ref().is_none_or(|proof| {
            proof.vendor() != process
                || proof.shell() != shell
                || proof.binding() != binding
                || !proof.reprove_with(&context, master, now, &ObservationKernel(kernel))
        }) {
            return UNKNOWN.into();
        }
    }
    if !validate_plan(&request, &authority, master, kernel, observation.is_some()) {
        return DENIED.into();
    }
    // Repeated prepare never renews a deadline or allocates another operation. A request
    // differing in any authority/plan field cannot reuse the original idempotency key.
    // Final cancellation/deadline checks, custody reservation and insertion share this
    // Meta -> native-table boundary. A worker delayed before it cannot publish past a seal.
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(seal) = table.cancelled.get(&request.id) {
        return if seal.request == request && cancelled_actor(seal, &caller, kernel) {
            cancelled(&request, &seal.actor)
        } else {
            DENIED.into()
        };
    }
    if clock.remaining(request.deadline_us).is_none() {
        return "ERR native operation deadline\n".into();
    }
    if !kernel.constructor_enabled(request.plan.constructor()) {
        return "ERR native constructor disabled\n".into();
    }
    if table.by_id.contains_key(&request.id) {
        return "ERR busy native operation prepare\n".into();
    }
    let Some(boundary) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return "ERR busy app handoff\n".into();
    };
    let custody = match ctx
        .fenced_writes
        .reserve_native(request.revision, &request.id, boundary)
    {
        Ok(custody) => custody,
        Err(refusal) => return refusal,
    };
    let binding = Binding {
        id: request.id.clone(),
        capability,
    };
    let custody = Arc::new(custody);
    let deadline_us = request.deadline_us;
    let actor_history = authority
        .actor
        .as_ref()
        .map(|actor| (actor.pid, actor.birth));
    let claim_outcome = if authority.builtin {
        ClaimOutcome::Builtin
    } else {
        ClaimOutcome::Lost
    };
    let human_seq = request
        .expected_human_seq
        .unwrap_or_else(|| ctx.human_input.seq());
    let operation = Arc::new(Mutex::new(Operation {
        request,
        binding,
        authority,
        custody: Some(Arc::clone(&custody)),
        phase: Phase::Queued,
        signal_sent: false,
        signal_accepted: None,
        resume_sent: false,
        resume_attempt: None,
        human_seq,
        channel: caller.connection,
        channel_closed: false,
        claim_outcome,
        commit_version: None,
        completion_drain: None,
        resume_phase: None,
        children: [None, None],
        effect_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        clock,
        observation,
        outcome_slot,
        completed_observation: None,
    }));
    slot.publish(&mut table, Arc::clone(&operation));
    {
        if let Some(connection) = caller.connection {
            table.coordinated_connections.insert(connection);
        }
        if caller.principal == crate::control::Principal::Harness {
            table.builtin_coordinated = true;
        } else if let Some(identity) = actor_history
            && !table.coordinated_actors.contains(&identity)
        {
            table.coordinated_actors.push(identity);
        }
    }
    drop(table);
    drop(meta);
    kernel.reserved();
    // This is a control worker, never the GUI. The marker prevents new revision work from
    // starving this exact predecessor drain while already-started suffixes finish normally.
    let ready = custody.await_predecessors(deadline_us);
    drop(custody);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if matches!(op.phase, Phase::Completed(_)) {
        drop(op);
        drop(meta);
        return completed_reply(&operation);
    }
    let unchanged = meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot;
    let crash_proven = op.observation.as_ref().is_some_and(|proof| {
        observe_control::context(ctx, &meta, &caller, kernel).is_some_and(|context| {
            proof.reprove_with(
                &context,
                master,
                op.clock.now_us().unwrap_or(u64::MAX),
                &ObservationKernel(kernel),
            )
        })
    });
    let kernel_unchanged = validate_plan(&op.request, &op.authority, master, kernel, crash_proven);
    let conversation_current = op
        .authority
        .conversation
        .as_ref()
        .is_none_or(|guard| guard.current(ctx));
    if !ready
        || op.channel_closed
        || !kernel.constructor_enabled(op.request.plan.constructor())
        || !unchanged
        || !kernel_unchanged
        || !conversation_current
        || op.clock.remaining(op.request.deadline_us).is_none()
    {
        let completion = op.seal_completion(Decision::ProvenNotCommittedRollbackSafe);
        drop(meta);
        drop(op);
        kernel.completion_sealed();
        return finish(ctx, &operation, completion);
    }
    op.phase = Phase::Started;
    if matches!(op.request.plan, NativePlan::OwnImage { .. }) {
        op.effect_started
            .store(true, std::sync::atomic::Ordering::Release);
    }
    admitted(&op.binding)
}

fn lookup(ctx: &crate::SessionCtx, binding: &Binding) -> Option<Arc<Mutex<Operation>>> {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&binding.id)?
        .clone();
    let matches = op.lock().unwrap_or_else(|p| p.into_inner()).binding == *binding;
    matches.then_some(op)
}

fn same_actor(op: &Operation, caller: &SupervisorInputCaller, kernel: &dyn Kernel) -> bool {
    if op.authority.builtin {
        caller.principal == crate::control::Principal::Harness
            && caller.peer == op.authority.caller.peer
            && op.authority.actor.as_ref().is_some_and(|old| {
                kernel
                    .process(old.pid)
                    .is_some_and(|now| own_actor_matches(&now, old) && now.image == old.image)
            })
    } else {
        caller.principal == op.authority.caller.principal
            && caller.peer == op.authority.caller.peer
            && op.authority.actor.as_ref().is_some_and(|old| {
                kernel
                    .process(old.pid)
                    .is_some_and(|now| now.pid == old.pid && now.birth == old.birth)
            })
    }
}

fn image_outcome(op: &Operation, master: i32, phase: ImagePhase, kernel: &dyn Kernel) -> bool {
    if let NativePlan::Restart { process, shell, .. } = &op.request.plan {
        if (!op.signal_sent && op.observation.is_none())
            || !op.resume_sent
            || op.resume_phase != Some(phase)
            || kernel.birth_alive(process) != Some(false)
        {
            return false;
        }
        let Some(child) = op.children[phase_index(phase)].as_ref() else {
            return false;
        };
        let Some(command) = command_for(op, phase) else {
            return false;
        };
        let Some(actual) = kernel.process(child.process.pid) else {
            return false;
        };
        return child.execute_sent
            && actual.pid == child.process.pid
            && actual.birth == child.process.birth
            && actual.parent == shell.pid
            && actual.group == child.process.group
            && actual.session == child.process.session
            && actual.image == command.image
            && kernel.process(shell.pid).as_ref() == Some(shell)
            && kernel.foreground(master) == Some(actual.group);
    }
    let NativePlan::OwnImage { child, target, .. } = &op.request.plan else {
        return false;
    };
    let Some(original) = op.authority.actor.as_ref() else {
        return false;
    };
    let Some(actual) = kernel.process(original.pid) else {
        return false;
    };
    let image = match phase {
        ImagePhase::Forward => target,
        ImagePhase::Rollback => &original.image,
    };
    own_actor_matches(&actual, original)
        && actual.image == *image
        && kernel.foreground(master) == Some(original.group)
        && kernel.process(child.pid).as_ref() == Some(child)
}

fn own_actor_matches(actual: &Process, original: &Process) -> bool {
    // The outer parent is diagnostic and may change after a queued HUP/reparent. The owned
    // vendor child still binds its parent to this same supervisor PID independently.
    actual.pid == original.pid
        && actual.birth == original.birth
        && actual.group == original.group
        && actual.session == original.session
}

fn finish(
    ctx: &crate::SessionCtx,
    operation: &Arc<Mutex<Operation>>,
    completion: Option<Completion>,
) -> String {
    let Some(Completion {
        id,
        custody,
        children,
        observation,
        outcome_capture,
        outcome_slot,
    }) = completion
    else {
        return completed_reply(operation);
    };
    // Releasing ordering custody happens after kernel outcome, not reply delivery or EOF.
    drop(children);
    drop(observation);
    drop(outcome_capture);
    drop(outcome_slot);
    drop(custody);
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    table.completed.push_back(id);
    drop(table);
    completed_reply(operation)
}

fn completed_reply(operation: &Arc<Mutex<Operation>>) -> String {
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    let phase = op.phase;
    let claim = op.claim_outcome;
    let drain = op.completion_drain.clone();
    let observation = op.completed_observation.clone();
    drop(op);
    if drain.is_some_and(|drain| !drain.is_drained()) {
        return UNKNOWN.into();
    }
    match phase {
        Phase::Completed(Decision::Committed) => {
            if claim == ClaimOutcome::Builtin
                && let Some(encoded) = observation.and_then(|observation| observation.encode())
            {
                format!(
                    "OK native-act state=committed claim=builtin revision=- observation={}\n",
                    aterm_codec::hex::encode_lower(encoded.as_bytes())
                )
            } else {
                committed(claim)
            }
        }
        Phase::Completed(Decision::ProvenNotCommittedRollbackSafe) => ROLLBACK_SAFE.into(),
        Phase::Completed(Decision::Retired(reason)) => {
            format!("OK native-act state=retired reason={}\n", reason.word())
        }
        _ => UNKNOWN.into(),
    }
}

/// Publish passive provenance before releasing the native marker. Readers therefore cannot
/// observe a terminal receipt before its data-only handoff is installed. No GUI lane calls
/// this function: only the private control worker's positive kernel completion can own it.
fn finish_restart(
    ctx: &crate::SessionCtx,
    master: i32,
    operation: &Arc<Mutex<Operation>>,
    mut completion: Option<Completion>,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    let mut capture = completion
        .as_mut()
        .and_then(|completion| completion.outcome_capture.take());
    if capture.is_some() {
        let observations = observe_control::store(ctx);
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let published = if let Some(context) = observe_control::context(ctx, &meta, caller, kernel)
        {
            let (published, retired) = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .publish_outcome_with(
                    capture.take().expect("captured outcome"),
                    &context,
                    master,
                    kernel.clock().now_us().unwrap_or(u64::MAX),
                    &ObservationKernel(kernel),
                )
                .into_parts();
            drop(meta);
            drop(retired);
            published
        } else {
            drop(meta);
            None
        };
        if let Some(published) = published {
            operation
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .completed_observation =
                Some(aterm_agent::harness::native_observe::SavedObservation {
                    registration: published.registration,
                    binding: published.binding,
                });
        }
    }
    drop(capture);
    finish(ctx, operation, completion)
}

#[cfg_attr(
    not(any(target_vendor = "apple", target_os = "linux", test)),
    expect(
        unreachable_code,
        reason = "no DeathWatch can exist here, so an attach past the armed watch never runs"
    )
)]
fn serve_launcher(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: &Request,
    caller: &SupervisorInputCaller,
    operation: &Arc<Mutex<Operation>>,
    kernel: &dyn Kernel,
) -> String {
    let phase = match request {
        Request::Attach(_, phase)
        | Request::Execute(_, phase)
        | Request::ExecFailed(_, phase, _) => *phase,
        _ => return DENIED.into(),
    };
    // Watch creation may acquire spawn/CLOEXEC synchronization. Arm it before either
    // predicate; after locking, the exact complete process identity is read again.
    let mut armed = if matches!(request, Request::Attach(..)) {
        caller
            .peer
            .and_then(|pid| kernel.process(pid))
            .and_then(|process| kernel.watch(&process).map(|death| (process, death)))
    } else {
        None
    };
    // Only Execute grants the first exec effect. Attach is observation: even after a guard
    // lapses, recording the already launched child's birth/witness is needed to retire it.
    let meta = matches!(request, Request::Execute(..))
        .then(|| ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    let NativePlan::Restart {
        shell, launcher, ..
    } = &op.request.plan
    else {
        return DENIED.into();
    };
    if caller.principal != crate::control::Principal::Instance
        || !op.authority.builtin
        || op.phase != Phase::Started
        || op.resume_phase != Some(phase)
        || !op.resume_sent
    {
        return DENIED.into();
    }
    let Some(connection) = caller.connection else {
        return DENIED.into();
    };
    let Some(actual) = caller.peer.and_then(|pid| kernel.process(pid)) else {
        return DENIED.into();
    };
    if actual.parent != shell.pid
        || actual.group != actual.pid
        || actual.session != shell.session
        || actual.image != *launcher
        || kernel.process(shell.pid).as_ref() != Some(shell)
        || kernel.foreground(master) != Some(actual.group)
    {
        return DENIED.into();
    }
    let index = phase_index(phase);
    if let Some(child) = &op.children[index] {
        if child.process != actual || child.connection != connection {
            return DENIED.into();
        }
    } else if matches!(request, Request::Attach(..)) {
        if armed.as_ref().is_none_or(|(watched, _)| watched != &actual) {
            return UNKNOWN.into();
        }
        let (_, death) = armed.take().expect("exact prearmed process");
        op.children[index] = Some(LaunchChild {
            process: actual,
            death: Arc::new(SharedDeath {
                watch: death,
                outcome: std::sync::OnceLock::new(),
            }),
            connection,
            execute_sent: false,
            exec_errno: None,
        });
    } else {
        return DENIED.into();
    }
    match request {
        Request::Attach(..) => {
            let Some(command) = command_for(&op, phase).and_then(RestartCommand::encode) else {
                return DENIED.into();
            };
            format!(
                "OK native-act state=launch deadline-us={} command={}\n",
                op.request.deadline_us,
                aterm_codec::hex::encode_lower(command.as_bytes())
            )
        }
        Request::Execute(..) => {
            let Some(meta) = meta else {
                return DENIED.into();
            };
            if op.clock.remaining(op.request.deadline_us).is_none()
                || ctx.human_input.seq() != op.human_seq
                || op
                    .authority
                    .conversation
                    .as_ref()
                    .is_none_or(|guard| !guard.current(ctx))
                || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                    && meta.supervisor_authority(crate::metrics::now_us()) != op.authority.snapshot)
                || command_for(&op, phase).is_none_or(|command| !kernel.image(&command.image))
                || op.children[index]
                    .as_ref()
                    .is_some_and(|child| child.exec_errno.is_some())
            {
                return DENIED.into();
            }
            op.children[index].as_mut().unwrap().execute_sent = true;
            op.effect_started
                .store(true, std::sync::atomic::Ordering::Release);
            format!(
                "OK native-act state=execute deadline-us={}\n",
                op.request.deadline_us
            )
        }
        Request::ExecFailed(_, _, errno) => {
            let child = op.children[index].as_mut().unwrap();
            if !child.execute_sent || child.exec_errno.is_some_and(|old| old != *errno) {
                return DENIED.into();
            }
            child.exec_errno = Some(*errno);
            "OK native-act state=exec-failed\n".into()
        }
        _ => unreachable!(),
    }
}

fn retirement(op: &Operation, kernel: &dyn Kernel) -> Option<Retirement> {
    match &op.request.plan {
        NativePlan::OwnImage { child, .. } => op
            .authority
            .actor
            .as_ref()
            .is_some_and(|actor| {
                [actor, child].iter().all(|process| {
                    kernel.birth_alive(process) == Some(false)
                        && kernel.group_gone(process.group) == Some(true)
                })
            })
            .then_some(Retirement::Exited),
        NativePlan::Restart { process, .. } => {
            if kernel.birth_alive(process) != Some(false)
                || kernel.group_gone(process.group) != Some(true)
                || op
                    .resume_attempt
                    .as_ref()
                    .is_some_and(|attempt| !attempt.settled())
                || op
                    .children
                    .iter()
                    .flatten()
                    .filter(|child| child.execute_sent)
                    .any(|child| {
                        !child.death.exited()
                            || kernel.group_gone(child.process.group) != Some(true)
                    })
            {
                return None;
            }
            Some(
                if op.children.iter().flatten().any(|child| child.execute_sent) {
                    Retirement::Exited
                } else {
                    Retirement::NotGranted
                },
            )
        }
    }
}

/// Whether `ctx` may hold an operation not yet completed: a table this could not read counts, so
/// the sweep looks again.
pub(crate) fn maybe_unsettled(ctx: &crate::SessionCtx) -> bool {
    match ctx.fenced_writes.native.try_lock() {
        Ok(table) => table.by_id.len() != table.completed.len(),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            let table = p.into_inner();
            table.by_id.len() != table.completed.len()
        }
        Err(std::sync::TryLockError::WouldBlock) => true,
    }
}

/// Close a failed plan without ever waiting for input or GUI-needed work. Every Go-granted
/// child needs retained death/group proof; a terminal no-Go seal rejects delayed helpers,
/// without claiming an unseen/ungranted helper is dead. Pending actual input stays Unknown.
pub(crate) fn reconcile_extinct(ctx: &crate::SessionCtx) {
    reconcile_extinct_by(ctx, &System(&ctx.native_policy));
}

fn reconcile_extinct_by(ctx: &crate::SessionCtx, kernel: &dyn Kernel) {
    let operations = {
        let table = match ctx.fenced_writes.native.try_lock() {
            Ok(table) => table,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        // Ordinary sessions and receipt-only history need no metadata clone or process probe.
        // A terminal prefix still owns its separate input lease; its writer rings that drain.
        if table.by_id.len() == table.completed.len() {
            return;
        }
        table.by_id.values().cloned().collect::<Vec<_>>()
    };
    let snapshot = match ctx.meta.try_lock() {
        Ok(meta) => meta.supervisor_authority(crate::metrics::now_us()),
        Err(std::sync::TryLockError::Poisoned(p)) => p
            .into_inner()
            .supervisor_authority(crate::metrics::now_us()),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    for operation in operations {
        let mut op = match operation.try_lock() {
            Ok(op) => op,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => continue,
        };
        if matches!(op.phase, Phase::Queued | Phase::Completed(_)) {
            continue;
        }
        let abandoned = match &op.request.plan {
            NativePlan::OwnImage { .. } => true,
            NativePlan::Restart { shell, .. } => {
                op.clock.remaining(op.request.deadline_us).is_none()
                    || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                        && snapshot != op.authority.snapshot)
                    || kernel.birth_alive(shell) == Some(false)
            }
        };
        if !abandoned {
            continue;
        }
        if op.authority.builtin
            && (!op.signal_sent || op.signal_accepted == Some(false))
            && !op.resume_sent
            && op.resume_attempt.is_none()
        {
            let completion = op.seal_completion(Decision::ProvenNotCommittedRollbackSafe);
            drop(op);
            let _receipt = finish(ctx, &operation, completion);
            ctx.watchers.ring_watchers();
            continue;
        }
        let Some(reason) = retirement(&op, kernel) else {
            continue;
        };
        let completion = op.seal_completion(Decision::Retired(reason));
        drop(op);
        let _receipt = finish(ctx, &operation, completion);
        ctx.watchers.ring_watchers();
    }
}

pub(crate) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    tail: &str,
    resume: &mut dyn FnMut(&str) -> String,
) -> String {
    let Some(request) = Request::parse(tail) else {
        return "ERR usage: native-act <prepare|precommit|commit|query|signal|resume> <bounded fields>\n".into();
    };
    let principal = crate::control::serving_principal();
    let holder = match &request {
        Request::Prepare(request) => request.holder.clone(),
        Request::CancelPrepare(cancel) => cancel.prepare.holder.clone(),
        _ => String::new(),
    };
    let caller = SupervisorInputCaller {
        principal,
        connection: crate::control::serving_connection(),
        peer: crate::control::serving_peer_pid(),
        proof: (principal != crate::control::Principal::Harness)
            .then(|| crate::control::prove_self_claim_for(ctx, &holder, master))
            .flatten(),
    };
    serve_by(
        ctx,
        master,
        request,
        caller,
        &System(&ctx.native_policy),
        resume,
    )
}

fn serve_by(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: Request,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
    resume: &mut dyn FnMut(&str) -> String,
) -> String {
    let request = match request {
        Request::Constructors => {
            let enabled = Constructor::ALL.into_iter().filter(|kind| kernel.constructor_enabled(*kind)).map(Constructor::word).collect::<Vec<_>>();
            return format!("OK native-act state=constructors plans={}\n", if enabled.is_empty() { "-".into() } else { enabled.join(",") });
        }
        Request::Info => return host_epoch().map_or_else(|| "ERR native entropy unavailable\n".into(), |epoch|
            format!("OK native-act state=info epoch={epoch} native-act=1 handoff=blocked clock=monotonic-us own-image={}\n", own_image_rows(&ctx.native_policy))),
        Request::Prepare(request) => return prepare(ctx, master, *request, caller, kernel),
        Request::CancelSupport => return "OK native-act state=cancel-support cancel-prepare=1\n".into(),
        Request::CancelPrepare(request) => return cancel_prepare(ctx, master, *request, caller, kernel),
        request => request,
    };
    let binding = match &request {
        Request::Info
        | Request::Constructors
        | Request::Prepare(_)
        | Request::CancelSupport
        | Request::CancelPrepare(_) => unreachable!(),
        Request::Precommit(b, _)
        | Request::Commit(b, _, _)
        | Request::Query(b)
        | Request::Abort(b)
        | Request::Recover(b)
        | Request::Signal(b)
        | Request::Resume(b, _, _, _)
        | Request::Attach(b, _)
        | Request::Execute(b, _)
        | Request::ExecFailed(b, _, _)
        | Request::Reconcile(b) => b,
    };
    let Some(operation) = lookup(ctx, binding) else {
        return DENIED.into();
    };
    if matches!(
        request,
        Request::Attach(..) | Request::Execute(..) | Request::ExecFailed(..)
    ) {
        return serve_launcher(ctx, master, &request, &caller, &operation, kernel);
    }
    // Retry disposal is deferred beyond both predicates, including early refusal paths.
    let mut retired_children = Vec::new();
    let mut retired_witnesses = Vec::new();
    let mutation_meta = matches!(
        request,
        Request::Signal(_) | Request::Resume(..) | Request::Commit(..)
    )
    .then(|| ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if !same_actor(&op, &caller, kernel) {
        return DENIED.into();
    }
    match request {
        Request::Recover(_) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            let Some(connection) = caller.connection else {
                return DENIED.into();
            };
            if !op.channel_closed || matches!(op.phase, Phase::Queued | Phase::Completed(_)) {
                return DENIED.into();
            }
            op.channel = Some(connection);
            op.channel_closed = false;
            admitted(&op.binding)
        }
        Request::Abort(_) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            let before_mutation = match &op.request.plan {
                NativePlan::Restart { .. } => {
                    (!op.signal_sent || op.signal_accepted == Some(false)) && !op.resume_sent
                }
                NativePlan::OwnImage { child, .. } => {
                    op.authority.actor.as_ref().is_some_and(|actor| {
                        kernel.process(actor.pid).is_some_and(|actual| {
                            own_actor_matches(&actual, actor) && actual.image == actor.image
                        }) && kernel.process(child.pid).as_ref() == Some(child)
                    })
                }
            };
            if op.phase != Phase::Started || !before_mutation {
                return UNKNOWN.into();
            }
            let completion = op.seal_completion(Decision::ProvenNotCommittedRollbackSafe);
            drop(op);
            kernel.completion_sealed();
            finish(ctx, &operation, completion)
        }
        Request::Reconcile(_) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            if op.phase == Phase::Queued {
                return UNKNOWN.into();
            }
            let Some(reason) = retirement(&op, kernel) else {
                return UNKNOWN.into();
            };
            let completion = op.seal_completion(Decision::Retired(reason));
            drop(op);
            finish(ctx, &operation, completion)
        }
        Request::Query(_) => match op.phase {
            Phase::Completed(_) => {
                drop(op);
                completed_reply(&operation)
            }
            Phase::Started
                if op.authority.builtin
                    && op
                        .resume_phase
                        .is_some_and(|phase| image_outcome(&op, master, phase, kernel)) =>
            {
                format!(
                    "OK native-act state=resumed phase={}\n",
                    op.resume_phase.unwrap().word()
                )
            }
            Phase::Queued | Phase::Started | Phase::Precommitted(_) | Phase::Committing(_) => {
                UNKNOWN.into()
            }
        },
        Request::Precommit(_, phase) => {
            if matches!(op.phase, Phase::Started | Phase::Precommitted(_))
                && image_outcome(&op, master, phase, kernel)
            {
                op.phase = Phase::Precommitted(phase);
                "OK native-act state=precommitted\n".into()
            } else {
                UNKNOWN.into()
            }
        }
        Request::Commit(_, phase, version) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            if op.authority.builtin {
                if !matches!(op.phase, Phase::Precommitted(p) if p == phase)
                    || !image_outcome(&op, master, phase, kernel)
                {
                    return UNKNOWN.into();
                }
                let Some(meta) = mutation_meta.as_ref() else {
                    return UNKNOWN.into();
                };
                let mut capture = None;
                if let Some(context) = observe_control::context(ctx, meta, &caller, kernel)
                    && op
                        .authority
                        .conversation
                        .as_ref()
                        .is_some_and(|guard| guard.current(ctx))
                    && op.outcome_slot.as_ref().is_some_and(|slot| {
                        slot.is_current(&context, op.clock.now_us().unwrap_or(u64::MAX))
                    })
                {
                    let Some(slot) = op.outcome_slot.take() else {
                        return UNKNOWN.into();
                    };
                    let Some(child) = op.children[phase_index(phase)].as_ref() else {
                        op.outcome_slot = Some(slot);
                        return UNKNOWN.into();
                    };
                    let Some(vendor) = kernel.process(child.process.pid) else {
                        op.outcome_slot = Some(slot);
                        return UNKNOWN.into();
                    };
                    let NativePlan::Restart { shell, .. } = &op.request.plan else {
                        unreachable!();
                    };
                    if child.death.outcome.get().is_none() {
                        let _installed = child.death.outcome.set(slot.hold());
                    }
                    let death: Arc<dyn crate::native_observe::DeathWitness> = child.death.clone();
                    match slot.capture_with(
                        &vendor,
                        shell,
                        &context,
                        master,
                        op.clock.now_us().unwrap_or(u64::MAX),
                        death,
                        &ObservationKernel(kernel),
                    ) {
                        Ok(captured) => capture = Some(captured),
                        Err(refused) => {
                            op.outcome_slot = Some(refused.slot);
                            retired_witnesses.push(refused.death);
                            return UNKNOWN.into();
                        }
                    }
                }
                op.commit_version = Some(version);
                let mut completion = op.seal_completion(Decision::Committed);
                if let Some(completion) = &mut completion {
                    completion.outcome_capture = capture;
                }
                drop(op);
                drop(mutation_meta);
                kernel.completion_sealed();
                return finish_restart(ctx, master, &operation, completion, &caller, kernel);
            }
            // Final commit is sent on the original authoritative stream. A fresh proof stream
            // may precommit/query the SAME operation, never substitute for that stream here.
            if (!op.authority.builtin && (caller.connection != op.channel || op.channel_closed))
                || !matches!(op.phase, Phase::Precommitted(p) | Phase::Committing(p) if p == phase)
                || op
                    .commit_version
                    .as_ref()
                    .is_some_and(|expected| expected != &version)
                || !image_outcome(&op, master, phase, kernel)
            {
                return UNKNOWN.into();
            }
            if let NativePlan::OwnImage {
                version: expected, ..
            } = &op.request.plan
                && phase == ImagePhase::Forward
                && &version != expected
            {
                return UNKNOWN.into();
            }
            // Rebind only if no transition replaced the captured authority. Invalidation is
            // never undone by an old operation's successful continuation.
            // Seal the commit choice BEFORE releasing this lock to acquire MetaLock. A
            // concurrent proof/query can never promise rollback during an in-flight commit.
            op.phase = Phase::Committing(phase);
            op.commit_version = Some(version.clone());
            drop(op);
            drop(mutation_meta);
            let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
            if op.phase != Phase::Committing(phase)
                || (!op.authority.builtin && (caller.connection != op.channel || op.channel_closed))
                || !image_outcome(&op, master, phase, kernel)
            {
                return UNKNOWN.into();
            }
            if !op.authority.builtin
                && meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot
                && let Some(scoped) = meta
                    .supervisor
                    .as_mut()
                    .and_then(|claim| claim.scoped.as_mut())
            {
                let Some(version) = aterm_agent::harness::contract::ClaimVersion::parse(&version)
                else {
                    return UNKNOWN.into();
                };
                scoped.claim.version = Some(version);
                meta.supervisor_revision = meta.supervisor_revision.wrapping_add(1);
                op.claim_outcome = ClaimOutcome::Bound(meta.supervisor_revision);
            }
            let completion = op.seal_completion(Decision::Committed);
            drop(meta);
            drop(op);
            kernel.completion_sealed();
            let reply = finish(ctx, &operation, completion);
            ctx.watchers.ring_watchers();
            reply
        }
        Request::Signal(_) => {
            if !op.authority.builtin || op.phase != Phase::Started {
                return DENIED.into();
            }
            if op.signal_sent {
                return if op.signal_accepted == Some(true) {
                    "OK native-act state=signalled\n"
                } else {
                    UNKNOWN
                }
                .into();
            }
            let NativePlan::Restart {
                process,
                shell,
                observation: None,
                ..
            } = &op.request.plan
            else {
                return DENIED.into();
            };
            if ctx.human_input.seq() != op.human_seq
                || op
                    .authority
                    .conversation
                    .as_ref()
                    .is_none_or(|guard| !guard.current(ctx))
                || op.clock.remaining(op.request.deadline_us).is_none()
                || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                    && mutation_meta.as_ref().is_none_or(|meta| {
                        meta.supervisor_authority(crate::metrics::now_us()) != op.authority.snapshot
                    }))
                || kernel.process(process.pid).as_ref() != Some(process)
                || kernel.process(shell.pid).as_ref() != Some(shell)
                || kernel.foreground(master) != Some(process.group)
            {
                return DENIED.into();
            }
            // Mark dispatch before the syscall: retrying an ambiguous ACK never signals a
            // newly reused pid or performs a second mutation. Only this retained plan is used.
            let process = process.clone();
            op.signal_sent = true;
            op.effect_started
                .store(true, std::sync::atomic::Ordering::Release);
            drop(mutation_meta);
            let reply = kernel.signal(master, &process, ctx);
            op.signal_accepted = Some(reply.starts_with("OK "));
            if reply.starts_with("OK ") {
                "OK native-act state=signalled\n".into()
            } else {
                UNKNOWN.into()
            }
        }
        Request::Resume(_, phase, generation, expected_human_seq) => {
            if !op.authority.builtin
                || !matches!(op.phase, Phase::Started | Phase::Precommitted(_))
                || (!op.signal_sent && op.observation.is_none())
            {
                return DENIED.into();
            }
            if op.resume_phase == Some(ImagePhase::Rollback) && phase == ImagePhase::Forward {
                return DENIED.into();
            }
            if phase == ImagePhase::Rollback && op.resume_phase != Some(ImagePhase::Rollback) {
                if op.children[0].as_ref().is_some_and(|child| {
                    child.execute_sent
                        && (!child.death.exited()
                            || kernel.group_gone(child.process.group) != Some(true))
                }) || op
                    .resume_attempt
                    .as_ref()
                    .is_none_or(|attempt| !attempt.settled())
                    || command_for(&op, phase).is_none()
                {
                    return UNKNOWN.into();
                }
                // Closing this phase rejects every delayed forward Attach/Execute. When no
                // Go was granted it does not assert an unregistered helper or cohort died.
                op.resume_phase = Some(phase);
                op.phase = Phase::Started;
                op.resume_sent = false;
                op.resume_attempt = None;
            }
            if op.resume_sent {
                if op
                    .resume_attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.unwritten())
                {
                    op.resume_sent = false;
                    op.resume_attempt = None;
                } else if op.children[phase_index(phase)]
                    .as_ref()
                    .is_some_and(|child| {
                        !child.execute_sent
                            && child.death.exited()
                            && kernel.group_gone(child.process.group) == Some(true)
                    })
                    && op
                        .resume_attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.settled())
                {
                    retired_children.extend(op.children[phase_index(phase)].take());
                    op.resume_sent = false;
                    op.resume_attempt = None;
                } else {
                    return UNKNOWN.into();
                }
            }
            let NativePlan::Restart {
                process,
                shell,
                launcher,
                socket,
                ..
            } = &op.request.plan
            else {
                return DENIED.into();
            };
            // Missing/unread facts are not proof of exit. The exact old birth must be gone,
            // while the original shell and its foreground still agree before any bytes start.
            if kernel.birth_alive(process) != Some(false)
                || kernel.group_gone(process.group) != Some(true)
                || kernel.process(shell.pid).as_ref() != Some(shell)
                || kernel.foreground(master) != Some(shell.group)
                || ctx.human_input.seq() != expected_human_seq
                || op
                    .authority
                    .conversation
                    .as_ref()
                    .is_none_or(|guard| !guard.current(ctx))
                || op.clock.remaining(op.request.deadline_us).is_none()
                || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                    && mutation_meta.as_ref().is_none_or(|meta| {
                        meta.supervisor_authority(crate::metrics::now_us()) != op.authority.snapshot
                    }))
            {
                return DENIED.into();
            }
            let Some(line) = command_for(&op, phase).and_then(|command| {
                command.launcher_line(launcher, socket, ctx.self_id.as_str(), &op.binding, phase)
            }) else {
                return DENIED.into();
            };
            let turn = format!("if-gen={generation} idle=1 timeout=1000 {line}");
            let Some(custody) = op.custody.as_ref().cloned() else {
                return DENIED.into();
            };
            op.resume_sent = true;
            op.resume_phase = Some(phase);
            op.human_seq = expected_human_seq;
            let human_seq = expected_human_seq;
            let attempt = Arc::new(NativeInput::new(
                ctx,
                custody,
                human_seq,
                op.authority.snapshot.clone(),
                op.request.deadline_us,
                Arc::clone(&op.effect_started),
                op.authority.conversation.clone(),
                op.clock.clone(),
            ));
            op.resume_attempt = Some(Arc::clone(&attempt));
            drop(op);
            drop(mutation_meta);
            let carried = Carried::enter(ctx, Arc::clone(&attempt));
            let reply = resume(&turn);
            drop(carried);
            if attempt.unwritten() {
                return "OK native-act state=unwritten\n".into();
            }
            if reply.starts_with("OK ") && !reply.split_ascii_whitespace().any(|w| w == "skipped") {
                format!("OK native-act state=resumed phase={}\n", phase.word())
            } else {
                UNKNOWN.into()
            }
        }
        Request::Info
        | Request::Constructors
        | Request::Prepare(_)
        | Request::CancelSupport
        | Request::CancelPrepare(_)
        | Request::Attach(..)
        | Request::Execute(..)
        | Request::ExecFailed(..) => unreachable!(),
    }
}

/// EOF publishes native channel loss without retiring any actual custody. Only an explicit
/// same-operation recovery with its opaque capability and fresh kernel actor proof can rebind.
pub(crate) fn connection_closed(ctx: &crate::SessionCtx, connection: u64) {
    connection_closed_by(ctx, connection, &System(&ctx.native_policy));
}

fn connection_closed_by(ctx: &crate::SessionCtx, connection: u64, kernel: &dyn Kernel) {
    let operations: Vec<_> = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        table.coordinated_connections.remove(&connection);
        table.by_id.values().cloned().collect()
    };
    for operation in operations {
        let mut operation = operation.lock().unwrap_or_else(|p| p.into_inner());
        if operation.channel == Some(connection) {
            operation.channel_closed = true;
        }
    }
    reconcile_extinct_by(ctx, kernel);
}

#[cfg(unix)]
fn process_group_gone(group: u32) -> Option<bool> {
    let group = i32::try_from(group).ok().filter(|group| *group > 1)?;
    // SAFETY: signal zero only observes the positive group identity captured from the kernel.
    let observed = unsafe { libc::kill(-group, 0) };
    if observed == 0 {
        return Some(false);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(true),
        Some(libc::EPERM) => Some(false),
        _ => None,
    }
}

#[cfg(not(unix))]
fn process_group_gone(_: u32) -> Option<bool> {
    None
}

/// Native coordination is explicitly selected by actual prepare, rather than retroactively
/// rejecting existing v3 clients. Once selected, omitting native words cannot regain general
/// signal/restart authority on this connection or the same kernel actor after release/expiry.
pub(crate) fn untyped_refusal(ctx: &crate::SessionCtx) -> Option<String> {
    let peer = crate::control::serving_peer_pid();
    let birth = peer
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(aterm_uds::process::read_birth)
        .map(|birth| birth.0);
    untyped_required(
        ctx,
        crate::control::serving_principal(),
        crate::control::serving_connection(),
        peer,
        birth,
    )
    .then(|| DENIED.into())
}

fn untyped_required(
    ctx: &crate::SessionCtx,
    principal: crate::control::Principal,
    connection: Option<u64>,
    peer: Option<u32>,
    birth: Option<u64>,
) -> bool {
    let table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    (principal == crate::control::Principal::Harness && table.builtin_coordinated)
        || connection.is_some_and(|c| table.coordinated_connections.contains(&c))
        || peer.is_some_and(|pid| {
            table.coordinated_actors.iter().any(|(known, old_birth)| {
                *known == pid && birth.is_none_or(|birth| birth == *old_birth)
            })
        })
}

#[cfg(unix)]
fn process_birth_alive(process: &Process) -> Option<bool> {
    let pid = i32::try_from(process.pid).ok()?;
    if let Some(actual) = aterm_uds::process::read_birth(pid) {
        return Some(actual.0 == process.birth);
    }
    // SAFETY: signal zero observes existence without mutating the positive original pid.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Some(false)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn process_birth_alive(_: &Process) -> Option<bool> {
    None
}

thread_local! {
    static CONTINUATION: std::cell::RefCell<Option<(aterm_session::SessionId, aterm_session::LaunchNonce,
        std::sync::Weak<NativeInput>)>> = const { std::cell::RefCell::new(None) };
}

/// One exact constructor-owned input batch. The original gesture fence is checked at its
/// first actual write; admitted suffixes then finish under the same native custody.
#[derive(Debug)]
pub(crate) struct NativeInput {
    human: Arc<crate::human_input::HumanInputStamp>,
    expected: u64,
    ctx: std::sync::Weak<crate::SessionCtx>,
    authority: SupervisorAuthority,
    deadline_us: u64,
    effect_started: Arc<std::sync::atomic::AtomicBool>,
    conversation: Option<Conversation>,
    clock: Clock,
    state: Mutex<NativeInputState>,
}

#[derive(Debug)]
struct NativeInputState {
    custody: Option<Arc<NativeCustody>>,
    started: Option<bool>,
    sealed: bool,
    pending: usize,
    accepted: bool,
    uncertain: bool,
}

impl NativeInput {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ctx: &Arc<crate::SessionCtx>,
        custody: Arc<NativeCustody>,
        expected: u64,
        authority: SupervisorAuthority,
        deadline_us: u64,
        effect_started: Arc<std::sync::atomic::AtomicBool>,
        conversation: Option<Conversation>,
        clock: Clock,
    ) -> Self {
        Self {
            human: Arc::clone(&ctx.human_input),
            expected,
            ctx: Arc::downgrade(ctx),
            authority,
            deadline_us,
            effect_started,
            conversation,
            clock,
            state: Mutex::new(NativeInputState {
                custody: Some(custody),
                started: None,
                sealed: false,
                pending: 0,
                accepted: false,
                uncertain: false,
            }),
        }
    }

    pub(crate) fn reserve(self: &Arc<Self>) -> Option<Arc<NativeInputWork>> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.sealed {
            return None;
        }
        state.pending = state.pending.checked_add(1)?;
        Some(Arc::new(NativeInputWork {
            input: Arc::clone(self),
            attempted: std::sync::atomic::AtomicBool::new(false),
            finished: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub(crate) fn producer(self: &Arc<Self>) -> NativeInputProducer {
        NativeInputProducer(Arc::clone(self))
    }

    pub(crate) fn seal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed = true;
        let custody = (state.pending == 0).then(|| state.custody.take()).flatten();
        drop(state);
        drop(custody);
    }

    fn unwritten(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed && state.pending == 0 && !state.accepted && !state.uncertain
    }

    fn settled(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed && state.pending == 0 && !state.uncertain
    }

    pub(crate) fn start(&self, nonparking: bool) -> bool {
        {
            let state = match self.state.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if nonparking => return false,
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.state.lock().unwrap_or_else(|p| p.into_inner())
                }
            };
            if let Some(started) = state.started {
                return started;
            }
        }
        let Some(ctx) = self.ctx.upgrade() else {
            return false;
        };
        let meta = if nonparking {
            match ctx.meta.try_lock() {
                Ok(meta) => meta,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            }
        } else {
            ctx.meta.lock().unwrap_or_else(|p| p.into_inner())
        };
        let mut state = if nonparking {
            match self.state.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            }
        } else {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        };
        if state.custody.is_none() || state.pending == 0 {
            return false;
        }
        let started = *state.started.get_or_insert_with(|| {
            self.human.seq() == self.expected
                && self
                    .conversation
                    .as_ref()
                    .is_some_and(|guard| guard.current(&ctx))
                && self.clock.remaining(self.deadline_us).is_some()
                && (self
                    .effect_started
                    .load(std::sync::atomic::Ordering::Acquire)
                    || meta.supervisor_authority(crate::metrics::now_us()) == self.authority)
        });
        if started {
            self.effect_started
                .store(true, std::sync::atomic::Ordering::Release);
        }
        started
    }
}

pub(crate) struct NativeInputProducer(Arc<NativeInput>);

impl Drop for NativeInputProducer {
    fn drop(&mut self) {
        self.0.seal();
    }
}

pub(crate) struct NativeInputWork {
    input: Arc<NativeInput>,
    attempted: std::sync::atomic::AtomicBool,
    finished: std::sync::atomic::AtomicBool,
}

impl NativeInputWork {
    pub(crate) fn attempted(&self) {
        self.attempted
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn finish(&self, accepted: bool) {
        let mut state = self.input.state.lock().unwrap_or_else(|p| p.into_inner());
        state.accepted |= accepted;
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for NativeInputWork {
    fn drop(&mut self) {
        let mut state = self.input.state.lock().unwrap_or_else(|p| p.into_inner());
        state.uncertain |= self.attempted.load(std::sync::atomic::Ordering::Acquire)
            && !self.finished.load(std::sync::atomic::Ordering::Acquire);
        state.pending = state.pending.saturating_sub(1);
        let custody = (state.sealed && state.pending == 0)
            .then(|| state.custody.take())
            .flatten();
        drop(state);
        drop(custody);
    }
}

struct Carried {
    previous: Option<(
        aterm_session::SessionId,
        aterm_session::LaunchNonce,
        std::sync::Weak<NativeInput>,
    )>,
    _custody: Arc<NativeInput>,
}

impl Carried {
    fn enter(ctx: &crate::SessionCtx, custody: Arc<NativeInput>) -> Self {
        let previous = CONTINUATION.with(|slot| {
            slot.replace(Some((
                ctx.self_id.clone(),
                ctx.nonce,
                Arc::downgrade(&custody),
            )))
        });
        Self {
            previous,
            _custody: custody,
        }
    }
}

impl Drop for Carried {
    fn drop(&mut self) {
        self._custody.seal();
        CONTINUATION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

pub(crate) fn carried_custody(ctx: &crate::SessionCtx) -> Option<Arc<NativeInput>> {
    CONTINUATION.with(|slot| {
        let binding = slot.borrow();
        let (sid, nonce, custody) = binding.as_ref()?;
        (sid == &ctx.self_id && nonce == &ctx.nonce)
            .then(|| custody.upgrade())
            .flatten()
    })
}
