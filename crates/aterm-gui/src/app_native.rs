// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host adapter for first-party native tab applications.
//!
//! Reducers emit typed effects; this module is the only layer allowed to turn
//! them into config persistence, updater work, clipboard access, external opens,
//! tab presentation invalidation, and redraws.

use crate::native_app::{
    AppEffect, AppEvent, ClipboardOutcome, ClipboardRequest, ConfigPatchOutcome, DamageRegion,
    EventResult, ExpectedConfigValue, ExternalOpenOutcome, PackagesOutcome, PackagesRequest,
    TextInputEvent, UpdateOutcome, UpdateRequest, ViewCx,
};
use crate::native_config_service::{
    ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ExpectedValue,
};
use crate::native_updater_service::CheckCompletion;
use crate::native_updater_service::{
    ApplyDecision, ApplyMode, ApplyPreflightStart, CheckBlock, CheckStart, ClosePreflight,
    DurableStageDisposition, DurableUpdateStatus, InstalledUpdate, NativeUpdaterService,
    ReturnedApplyDisposition, ReturnedApplyFacts, UpdaterPhase, UpdaterWorkTicket,
};
use crate::packages_screen::PackagesBusy;
use crate::{App, Wake, WindowId};

#[cfg(test)]
thread_local! {
    static UPDATE_FACT_PROBES_ON_THREAD: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
}

/// Opaque evidence that the main-thread native document/restore preflight was Ready.
/// The constructor is private to this module; process replacement must carry the token
/// rather than recreating the safety boolean at its final admission gate.
pub(crate) struct NativeUpdateSafetyToken {
    _private: (),
}

impl NativeUpdateSafetyToken {
    #[must_use]
    pub(crate) fn is_certified(&self) -> bool {
        true
    }
}

/// Disk facts collected off the event loop after an asynchronous handoff stops.
/// Reduction remains main-thread owned, but neither ledger I/O nor the installed
/// bundle probe belongs on the latency-sensitive completion callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NativeUpdateReconcileTicket {
    request_sequence: u64,
}

impl NativeUpdateReconcileTicket {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn request_sequence(self) -> u64 {
        self.request_sequence
    }
}

/// What the event loop should do only after worker-collected facts have been accepted.
/// Trigger payloads never carry update authority; the effective reducer stage does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeUpdateReconcilePurpose {
    Startup,
    StageAvailable,
    ApplyControl,
}

#[must_use]
fn merge_reconcile_purpose(
    left: NativeUpdateReconcilePurpose,
    right: NativeUpdateReconcilePurpose,
) -> NativeUpdateReconcilePurpose {
    use NativeUpdateReconcilePurpose::{ApplyControl, StageAvailable, Startup};
    match (left, right) {
        (ApplyControl, _) | (_, ApplyControl) => ApplyControl,
        (StageAvailable, _) | (_, StageAvailable) => StageAvailable,
        (Startup, Startup) => Startup,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NativeUpdateReconcileFacts {
    pub(crate) _ticket: NativeUpdateReconcileTicket,
    /// Assigned by the sole facts worker immediately before the read. This, not
    /// request dispatch order, is the freshness authority.
    pub(crate) observation_sequence: u64,
    pub(crate) durable: Option<DurableUpdateStatus>,
    pub(crate) installed: Option<InstalledUpdate>,
}

enum NativeUpdateFactDestination {
    Wake {
        purpose: NativeUpdateReconcilePurpose,
        proxy: winit::event_loop::EventLoopProxy<Wake>,
    },
    Reply(std::sync::mpsc::SyncSender<NativeUpdateReconcileFacts>),
}

pub(crate) struct NativeUpdateReconcileRequest {
    ticket: NativeUpdateReconcileTicket,
    current_build: u64,
    destination: NativeUpdateFactDestination,
}

pub(crate) enum NativeUpdateWorkerRequest {
    Reconcile(NativeUpdateReconcileRequest),
    ConfirmBootHealth { current_build: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeUpdateDispatch {
    Queued,
    Saturated,
    Unavailable,
}

pub(crate) type NativeUpdateReconcileSender =
    std::sync::mpsc::SyncSender<NativeUpdateWorkerRequest>;

/// One buffered item plus the worker's active item is sufficient; further UI
/// intents are retained by the purpose coalescer. Keeping this capacity named
/// binds the shipping constructor to the Tier-1 Full/drain/retry boundary and
/// prevents stale disk work from accumulating ahead of user-visible updates.
pub(crate) const NATIVE_UPDATE_WORKER_CAPACITY: usize = 1;

/// Result of reducing worker-collected disk facts on the event loop. Active updater
/// work defers the owned facts without rereading disk; otherwise presentation must use
/// only the returned effective reducer stage, never the stale trigger payload.
pub(crate) enum NativeUpdateFactsResult {
    IgnoredStale,
    Deferred(NativeUpdateReconcileFacts),
    Reduced {
        effective_stage: Option<crate::native_updater_service::StagedUpdate>,
    },
}

#[must_use]
fn read_native_update_reconcile_facts(
    ticket: NativeUpdateReconcileTicket,
    observation_sequence: u64,
    current_build: u64,
) -> NativeUpdateReconcileFacts {
    #[cfg(test)]
    UPDATE_FACT_PROBES_ON_THREAD.with(|count| count.set(count.get().saturating_add(1)));
    read_native_update_reconcile_facts_with(
        ticket,
        observation_sequence,
        || aterm_update::status(current_build).map(durable_update_status),
        || {
            aterm_update::installed_update_facts().map(|installed| InstalledUpdate {
                build: installed.build_number,
                commit: installed.git_commit,
                receipt_build: installed.receipt_build_number,
                receipt_dmg_sha256: installed.receipt_dmg_sha256,
            })
        },
    )
}

fn read_native_update_reconcile_facts_with(
    ticket: NativeUpdateReconcileTicket,
    observation_sequence: u64,
    read_durable: impl FnOnce() -> Option<DurableUpdateStatus>,
    read_installed: impl FnOnce() -> Option<InstalledUpdate>,
) -> NativeUpdateReconcileFacts {
    // Load the ready marker before canonical bundle identity. Shipping order swaps
    // the bundle first and removes ready afterward, so this observes ready+old,
    // ready+new, or missing+new; exact installed proof safely dominates either
    // surviving-marker case.
    let durable = read_durable();
    let installed = read_installed();
    NativeUpdateReconcileFacts {
        _ticket: ticket,
        observation_sequence,
        durable,
        installed,
    }
}

/// Start the one process-wide, bounded FIFO facts worker. Every ledger/provenance
/// observation is serialized here, and freshness is stamped at read time.
pub(crate) fn spawn_native_update_reconcile_worker(
    drain_proxy: winit::event_loop::EventLoopProxy<Wake>,
) -> Result<NativeUpdateReconcileSender, String> {
    let (sender, receiver) =
        std::sync::mpsc::sync_channel::<NativeUpdateWorkerRequest>(NATIVE_UPDATE_WORKER_CAPACITY);
    std::thread::Builder::new()
        .name("aterm-update-facts".to_string())
        .spawn(move || {
            let health_proxy = drain_proxy.clone();
            run_native_update_worker(
                receiver,
                read_native_update_reconcile_facts,
                move |confirmed| {
                    let _ = health_proxy
                        .send_event(Wake::NativeBootHealthConfirmationFinished { confirmed });
                },
                || {
                    let _ = drain_proxy.send_event(Wake::NativeUpdateWorkerDrained);
                },
            );
        })
        .map_err(|error| format!("could not start updater facts worker: {error}"))?;
    Ok(sender)
}

fn run_native_update_worker(
    receiver: std::sync::mpsc::Receiver<NativeUpdateWorkerRequest>,
    mut read: impl FnMut(NativeUpdateReconcileTicket, u64, u64) -> NativeUpdateReconcileFacts,
    mut health_finished: impl FnMut(bool),
    mut drained: impl FnMut(),
) {
    let mut next_observation_sequence = 1_u64;
    while let Ok(work) = receiver.recv() {
        let request = match work {
            NativeUpdateWorkerRequest::Reconcile(request) => request,
            NativeUpdateWorkerRequest::ConfirmBootHealth { current_build } => {
                let confirmed = aterm_update::confirm_boot_health_exact(
                    current_build,
                    crate::build_info::GIT_COMMIT,
                );
                health_finished(confirmed);
                drained();
                continue;
            }
        };
        let observation_sequence = next_observation_sequence;
        let Some(next) = observation_sequence.checked_add(1) else {
            break;
        };
        next_observation_sequence = next;
        let facts = read(request.ticket, observation_sequence, request.current_build);
        match request.destination {
            NativeUpdateFactDestination::Wake { purpose, proxy } => {
                let _ = proxy.send_event(Wake::NativeUpdateReconcileFinished { purpose, facts });
            }
            NativeUpdateFactDestination::Reply(reply) => {
                let _ = reply.send(facts);
            }
        }
        // A Reply or boot-health item need not emit any other UI event. This edge
        // guarantees a Full-queue pending latch gets another nonblocking chance.
        drained();
    }
}

/// Queue one handoff-worker observation on the same FIFO and wait off the event
/// loop for its ordered result.
pub(crate) fn collect_native_update_reconcile_facts(
    worker: &NativeUpdateReconcileSender,
    ticket: NativeUpdateReconcileTicket,
    current_build: u64,
) -> Option<NativeUpdateReconcileFacts> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    worker
        .send(NativeUpdateWorkerRequest::Reconcile(
            NativeUpdateReconcileRequest {
                ticket,
                current_build,
                destination: NativeUpdateFactDestination::Reply(reply),
            },
        ))
        .ok()?;
    result.recv().ok()
}

const MAX_AUTOMATIC_UPDATE_CYCLES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutomaticRetryKind {
    PreflightBlocked,
    /// The overlap attempt parked, spawned, and was then revoked by USER OR
    /// TERMINAL ACTIVITY before Commit. The rollback is proven lossless (kill,
    /// reap, resume readers; zero bytes consumed by the parent post-park), so
    /// — unlike `PhysicalFailure` — repeating at a later quiet window is as
    /// safe as the first attempt. Budgeted and exponentially spaced so a busy
    /// terminal converges to manual-only instead of parking forever.
    ActivityRevoked,
    PhysicalFailure,
}

/// A bounded retry plan for cheap ordering/preflight races and for lossless
/// activity-revoked overlap returns. GENUINE physical handoff failures (child
/// died, proof mismatch, safety loss) never call this helper with a retry-able
/// kind: they become manual-only immediately.
#[must_use]
fn automatic_retry_delay(cycles: u8, kind: AutomaticRetryKind) -> Option<std::time::Duration> {
    if kind == AutomaticRetryKind::PhysicalFailure {
        return None;
    }
    if cycles >= MAX_AUTOMATIC_UPDATE_CYCLES {
        return None;
    }
    let seconds = match (kind, cycles) {
        (AutomaticRetryKind::PreflightBlocked, 0 | 1) => 5,
        (AutomaticRetryKind::PreflightBlocked, _) => 15,
        // Exponential spacing: each revoked attempt costs a park/spawn/paint
        // round trip, so back off hard while the terminal stays busy. The
        // ≥500 ms quiet-epoch admission still gates every re-attempt.
        (AutomaticRetryKind::ActivityRevoked, 0) => 2,
        (AutomaticRetryKind::ActivityRevoked, 1) => 8,
        (AutomaticRetryKind::ActivityRevoked, _) => 30,
        // Kept explicit so this helper stays fail-closed if the early return above is
        // ever refactored. A physical handoff failure must never mint a timer retry.
        (AutomaticRetryKind::PhysicalFailure, _) => return None,
    };
    Some(std::time::Duration::from_secs(seconds))
}

/// Parse the durable artifact identity without allocating. Reducer-imported stages
/// are already canonicalized, but automatic application fails closed if a future
/// producer ever hands this layer malformed identity bytes.
#[must_use]
fn decode_dmg_sha256(digest: &str) -> Option<[u8; 32]> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = digest.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        decoded[index] = nibble(pair[0])?.checked_shl(4)? | nibble(pair[1])?;
    }
    Some(decoded)
}

/// Seed the process service from the same durable status marker every existing
/// update surface reads. A missing marker still produces an honest enabled/idle
/// (or disabled) service; no network work occurs here.
pub(crate) fn load_native_updater_service() -> NativeUpdaterService {
    let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
    let version = crate::build_info::version_display();
    // Startup ledger reconciliation is posted by an off-loop worker. Construction
    // stays filesystem/process-spawn free so first input cannot queue behind TOML or
    // PlistBuddy work on the event loop.
    NativeUpdaterService::new(build, version, aterm_update::enabled())
}

/// Bounded, owned result suitable for a typed event-loop wake.
pub(crate) fn durable_update_status(status: aterm_update::UpdateStatus) -> DurableUpdateStatus {
    DurableUpdateStatus {
        enabled: status.enabled,
        current_build: status.current_build,
        staged_build: status.staged_build,
        staged_version: status.staged_version,
        staged_commit: status.staged_commit,
        staged_dmg_sha256: status.staged_dmg_sha256,
        changelog: status.changelog,
        outcome: status.outcome,
        failing_checks: status.failing_checks,
    }
}

fn failed_update_status(build: u64, message: String) -> DurableUpdateStatus {
    DurableUpdateStatus {
        enabled: true,
        current_build: build,
        staged_build: None,
        staged_version: None,
        staged_commit: None,
        staged_dmg_sha256: None,
        changelog: None,
        outcome: message,
        failing_checks: 1,
    }
}

fn union_native_damage(first: DamageRegion, second: DamageRegion) -> DamageRegion {
    let (
        DamageRegion::Rect {
            x: first_x,
            y: first_y,
            width: first_width,
            height: first_height,
        },
        DamageRegion::Rect {
            x: second_x,
            y: second_y,
            width: second_width,
            height: second_height,
        },
    ) = (first, second)
    else {
        return DamageRegion::All;
    };
    let x = first_x.min(second_x);
    let y = first_y.min(second_y);
    let right = first_x
        .saturating_add(first_width)
        .max(second_x.saturating_add(second_width));
    let bottom = first_y
        .saturating_add(first_height)
        .max(second_y.saturating_add(second_height));
    DamageRegion::Rect {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

/// Identity of every input that can change the active native app's compiled UI.
/// A cached frame is observable only while this stamp still matches the live
/// controller, service, document, and window geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NativeUiCompileStamp {
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) geometry: u64,
    pub(crate) config_revision: u64,
    pub(crate) update_revision: u64,
    pub(crate) document_seq: Option<u64>,
    pub(crate) presentation_revision: u64,
    /// Theme and chrome-font identity consumed only by raster lowering. A
    /// paint-input change must never reuse a reducer-local rectangle.
    pub(crate) paint_revision: u64,
}

fn native_appearance_revision(preferences: crate::native_appearance::AppearancePreferences) -> u64 {
    use std::hash::{Hash, Hasher};

    let preferences = preferences.normalized();
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    preferences.high_contrast.hash(&mut revision);
    preferences.reduced_transparency.hash(&mut revision);
    preferences.text_scale.to_bits().hash(&mut revision);
    revision.finish()
}

/// Paint identity for the host-owned motion facts supplied to every native app.
/// These facts are intentionally not reducer state: focus, OS Reduce Motion,
/// recording-watch focus, load shedding, and the process-wide serious override can
/// all change while the native view generation remains stable. They must nevertheless
/// invalidate retained pixels because Settings previews resolve animation, static
/// effect output, and their static/live badge from this exact context.
fn native_motion_revision(motion: crate::native_app::ViewMotionCx) -> u8 {
    (motion.system_reduced as u8)
        | ((motion.focused as u8) << 1)
        | ((motion.performance_reduced as u8) << 2)
        | ((motion.serious as u8) << 3)
}

impl NativeUiCompileStamp {
    /// Regional reducer damage remains sound only when the view-local reducer
    /// generation is the sole stamp input that changed. Geometry, services,
    /// document content, or presentation changes conservatively widen to All.
    pub(crate) fn accepts_regional_damage_from(self, previous: Self) -> bool {
        self.instance == previous.instance
            && self.view == previous.view
            && self.geometry == previous.geometry
            && self.config_revision == previous.config_revision
            && self.update_revision == previous.update_revision
            && self.document_seq == previous.document_seq
            && self.presentation_revision == previous.presentation_revision
            && self.paint_revision == previous.paint_revision
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeCompiledPhase {
    Staged,
    Presented,
}

/// The exact semantic/layout product lowered into the native tray for a frame.
/// Keeping this beside the raster makes control inspection an observer of the
/// same artifact as pixels, hit testing, and AccessKit rather than a second
/// speculative compiler invocation.
#[derive(Clone, Debug)]
pub(crate) struct NativeCompiledFrame {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) phase: NativeCompiledPhase,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Retained semantic scene for exactly one native split leaf. This is the
/// backend-neutral handoff: raster today consumes `compiled.tray`, while a direct
/// GPU/CPU `UiScene` backend can consume the same view/stamp/viewport without
/// changing tab geometry, cache identity, hit testing or accessibility.
#[derive(Clone, Debug)]
pub(crate) struct NativeLeafScene {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) viewport: crate::native_ui::LogicalRect,
    /// View-local logical-pixel damage declared by the reducer. The retained
    /// tray adapter patches the outward-rounded device tile when every other
    /// compile-stamp input is stable, without widening to the leaf or window.
    pub(crate) damage: DamageRegion,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Immutable view of one compositor-retained native leaf. The full live compile
/// stamp, lifecycle generation, semantic tree, and exact raw-window destination
/// have already been cross-checked by the resolver that returns it.
pub(crate) struct RetainedNativeLeafArtifact<'a> {
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) compiled: &'a crate::native_ui::CompiledUi,
    /// Signed window-space destination. A centred transient surface crop can
    /// place the retained leaf partly above/left of the physical client area.
    pub(crate) device_x: i64,
    pub(crate) device_y: i64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scale: f64,
}

struct NativeConfigJob {
    snapshot: crate::native_config_service::ConfigSnapshot,
    undo: Option<u64>,
    instance: crate::tab_model::AppInstanceId,
    view: crate::tab_model::ViewId,
    reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
}

pub(crate) enum NativeConfigWork {
    Patch(crate::native_app::ConfigPatch),
    Undo(u64),
}

pub(crate) struct NativeConfigRequest {
    instance: crate::tab_model::AppInstanceId,
    view: crate::tab_model::ViewId,
    reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
    work: NativeConfigWork,
}

fn native_config_queue() -> Result<&'static std::sync::mpsc::Sender<NativeConfigJob>, String> {
    static QUEUE: std::sync::OnceLock<Result<std::sync::mpsc::Sender<NativeConfigJob>, String>> =
        std::sync::OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::channel::<NativeConfigJob>();
            std::thread::Builder::new()
                .name("aterm-native-config".to_string())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        let outcome =
                            match crate::prefs::save_prefs_snapshot(job.snapshot.text.as_ref()) {
                                crate::prefs::SaveOutcome::Saved
                                | crate::prefs::SaveOutcome::Unchanged => {
                                    ConfigPatchOutcome::Applied {
                                        revision: job.snapshot.revision,
                                        undo: job.undo,
                                    }
                                }
                                crate::prefs::SaveOutcome::Error(message) => {
                                    ConfigPatchOutcome::Rejected { message }
                                }
                            };
                        let _ = job.proxy.send_event(Wake::NativeConfigFinished {
                            instance: job.instance,
                            view: job.view,
                            reply: job.reply,
                            outcome,
                        });
                    }
                })
                .map_err(|error| format!("could not start config worker: {error}"))?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl App {
    fn ensure_native_update_reconcile_worker(&mut self) -> Option<NativeUpdateReconcileSender> {
        if let Some(worker) = self.native_update_reconcile_worker.clone() {
            return Some(worker);
        }
        let proxy = self.proxy.clone()?;
        match spawn_native_update_reconcile_worker(proxy) {
            Ok(worker) => {
                self.native_update_reconcile_worker = Some(worker.clone());
                Some(worker)
            }
            Err(error) => {
                aterm_log::warn!("native updater worker restart failed: {error}");
                None
            }
        }
    }

    /// Mint one monotonic identity before dispatching any durable updater read.
    /// Exhaustion fails closed: sequence reuse could let an old completion outrank
    /// a newer artifact observation.
    pub(crate) fn mint_native_update_reconcile_ticket(
        &mut self,
    ) -> Option<NativeUpdateReconcileTicket> {
        let request_sequence = self.next_native_update_reconcile_sequence;
        self.next_native_update_reconcile_sequence = request_sequence.checked_add(1)?;
        Some(NativeUpdateReconcileTicket { request_sequence })
    }

    #[must_use]
    pub(crate) fn native_update_reconcile_worker(&self) -> Option<NativeUpdateReconcileSender> {
        self.native_update_reconcile_worker.clone()
    }

    fn try_dispatch_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        destination: impl FnOnce(NativeUpdateReconcilePurpose) -> NativeUpdateFactDestination,
    ) -> NativeUpdateDispatch {
        let Some(worker) = self.ensure_native_update_reconcile_worker() else {
            return NativeUpdateDispatch::Unavailable;
        };
        let Some(ticket) = self.mint_native_update_reconcile_ticket() else {
            aterm_log::warn!("native updater reconciliation identity space exhausted");
            return NativeUpdateDispatch::Unavailable;
        };
        let current_build = self.native_updater_service.snapshot().current_build;
        let work = NativeUpdateWorkerRequest::Reconcile(NativeUpdateReconcileRequest {
            ticket,
            current_build,
            destination: destination(purpose),
        });
        match worker.try_send(work) {
            Ok(()) => NativeUpdateDispatch::Queued,
            Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
            Err(std::sync::mpsc::TrySendError::Disconnected(work)) => {
                self.native_update_reconcile_worker = None;
                let Some(restarted) = self.ensure_native_update_reconcile_worker() else {
                    return NativeUpdateDispatch::Unavailable;
                };
                match restarted.try_send(work) {
                    Ok(()) => NativeUpdateDispatch::Queued,
                    Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        self.native_update_reconcile_worker = None;
                        NativeUpdateDispatch::Unavailable
                    }
                }
            }
        }
    }

    fn request_native_update_reconcile_with(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        destination: impl FnOnce(NativeUpdateReconcilePurpose) -> NativeUpdateFactDestination,
    ) -> bool {
        if !self.native_updater_service.snapshot().enabled {
            self.pending_native_update_reconcile_purpose = None;
            return false;
        }
        let effective = self
            .pending_native_update_reconcile_purpose
            .take()
            .map_or(purpose, |pending| merge_reconcile_purpose(pending, purpose));
        match self.try_dispatch_native_update_reconcile(effective, destination) {
            NativeUpdateDispatch::Queued => true,
            NativeUpdateDispatch::Saturated => {
                self.pending_native_update_reconcile_purpose = Some(effective);
                true
            }
            NativeUpdateDispatch::Unavailable => {
                self.pending_native_update_reconcile_purpose = None;
                false
            }
        }
    }

    /// Queue status + installed-bundle probes on the sole process worker. `try_send`
    /// is nonblocking, so a saturated worker can never stall input. Saturation is an
    /// accepted, coalesced request: it remains pending until a later event-loop turn
    /// enters it into the FIFO.
    pub(crate) fn request_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
    ) -> bool {
        if !self.native_updater_service.snapshot().enabled {
            self.pending_native_update_reconcile_purpose = None;
            return false;
        }
        let Some(proxy) = self.proxy.clone() else {
            self.pending_native_update_reconcile_purpose = None;
            return false;
        };
        self.request_native_update_reconcile_with(purpose, move |effective| {
            NativeUpdateFactDestination::Wake {
                purpose: effective,
                proxy,
            }
        })
    }

    fn retry_pending_native_update_reconcile_with(
        &mut self,
        destination: impl FnOnce(NativeUpdateReconcilePurpose) -> NativeUpdateFactDestination,
    ) -> NativeUpdateDispatch {
        // The event loop calls this at every park point. Establish that there is
        // real work BEFORE consulting service state or constructing a destination:
        // on macOS, cloning the destination EventLoopProxy installs a CFRunLoop
        // source. Doing that while idle turns a no-op park into a self-waking hot
        // loop (and, when updates are disabled, one warning per iteration).
        let Some(purpose) = self.pending_native_update_reconcile_purpose.take() else {
            return NativeUpdateDispatch::Queued;
        };
        if !self.native_updater_service.snapshot().enabled {
            return NativeUpdateDispatch::Unavailable;
        }
        let outcome = self.try_dispatch_native_update_reconcile(purpose, destination);
        if outcome == NativeUpdateDispatch::Saturated {
            self.pending_native_update_reconcile_purpose = Some(purpose);
        } else if outcome == NativeUpdateDispatch::Unavailable {
            self.pending_native_update_reconcile_purpose = None;
        }
        outcome
    }

    /// Pure guard for the event-loop retry wrapper. It is intentionally kept
    /// separate from proxy materialization so Tier-1 can prove that an idle park
    /// cannot clone/wake an `EventLoopProxy` or enter the unavailable/log path.
    #[must_use]
    fn has_pending_native_update_reconcile(&self) -> bool {
        self.pending_native_update_reconcile_purpose.is_some()
    }

    /// Injectable wrapper around proxy materialization. Keeping the factory
    /// behind the pending-work guard lets Tier-1 count the exact side effect the
    /// historical macOS hot loop performed, rather than testing only the worker
    /// dispatch below it.
    fn retry_pending_native_update_reconcile_via(
        &mut self,
        materialize_proxy: impl FnOnce(&Self) -> Option<winit::event_loop::EventLoopProxy<crate::Wake>>,
    ) {
        if !self.has_pending_native_update_reconcile() {
            return;
        }
        let Some(proxy) = materialize_proxy(self) else {
            return;
        };
        let outcome = self.retry_pending_native_update_reconcile_with(move |purpose| {
            NativeUpdateFactDestination::Wake { purpose, proxy }
        });
        if outcome == NativeUpdateDispatch::Unavailable {
            aterm_log::warn!("native updater reconcile worker is unavailable; retry stopped");
        }
    }

    /// Retry one coalesced request after the worker has had an opportunity to drain.
    /// No filesystem work and no blocking operation occurs on the event loop.
    pub(crate) fn retry_pending_native_update_reconcile(&mut self) {
        self.retry_pending_native_update_reconcile_via(|app| app.proxy.clone());
    }

    /// Queue boot-sentinel confirmation/GC behind already-requested fact reads.
    /// This is nonblocking and performs no filesystem work on the event loop.
    pub(crate) fn request_native_boot_health_confirmation(&mut self) -> NativeUpdateDispatch {
        let Some(worker) = self.ensure_native_update_reconcile_worker() else {
            return NativeUpdateDispatch::Unavailable;
        };
        let work = NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: self.native_updater_service.snapshot().current_build,
        };
        match worker.try_send(work) {
            Ok(()) => NativeUpdateDispatch::Queued,
            Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
            Err(std::sync::mpsc::TrySendError::Disconnected(work)) => {
                self.native_update_reconcile_worker = None;
                let Some(restarted) = self.ensure_native_update_reconcile_worker() else {
                    return NativeUpdateDispatch::Unavailable;
                };
                match restarted.try_send(work) {
                    Ok(()) => NativeUpdateDispatch::Queued,
                    Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        self.native_update_reconcile_worker = None;
                        NativeUpdateDispatch::Unavailable
                    }
                }
            }
        }
    }

    pub(crate) fn finish_native_boot_health_confirmation(
        &mut self,
        confirmed: bool,
        now: std::time::Instant,
    ) {
        if confirmed {
            self.boot_health_confirmation_dispatched = true;
            self.boot_health_confirmation_retry_at = None;
        } else {
            self.boot_health_confirmation_dispatched = false;
            self.boot_health_confirmation_retry_at = Some(
                now.checked_add(std::time::Duration::from_secs(1))
                    .unwrap_or(now),
            );
        }
    }

    /// Accept one ordered fact completion, then derive every presentation/action from
    /// the effective reducer stage. The original stage wake's build/version are never
    /// trusted as authority.
    pub(crate) fn finish_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        facts: NativeUpdateReconcileFacts,
    ) {
        match self.reconcile_native_update_facts(facts) {
            NativeUpdateFactsResult::IgnoredStale => {}
            NativeUpdateFactsResult::Deferred(facts) => {
                self.deferred_native_update_reconcile =
                    Some(match self.deferred_native_update_reconcile.take() {
                        None => (purpose, facts),
                        Some((old_purpose, old_facts)) => {
                            let merged = merge_reconcile_purpose(old_purpose, purpose);
                            let newest =
                                if facts.observation_sequence > old_facts.observation_sequence {
                                    facts
                                } else {
                                    old_facts
                                };
                            (merged, newest)
                        }
                    });
            }
            NativeUpdateFactsResult::Reduced { effective_stage } => {
                let newly_announced = effective_stage.as_ref().is_some_and(|stage| {
                    self.relaunch.as_ref().map(|notice| notice.build) != Some(stage.build)
                });
                self.publish_native_update_state();

                if let Some(stage) = effective_stage {
                    if newly_announced
                        && matches!(
                            purpose,
                            NativeUpdateReconcilePurpose::Startup
                                | NativeUpdateReconcilePurpose::StageAvailable
                        )
                    {
                        self.notice = Some(crate::notice::TransientNotice::update_ready(
                            stage.version.clone(),
                            stage.build,
                            std::time::Instant::now(),
                        ));
                        self.level_up = Some(crate::level_up::LevelUp::new(
                            stage.build,
                            std::time::Instant::now(),
                        ));
                        self.request_redraw_all_windows();
                    }

                    if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                        let outcome = self.apply_native_update(ApplyMode::Immediate);
                        self.surface_update_apply_outcome("control request", outcome, false);
                    } else {
                        self.arm_native_auto_apply(stage.build, &stage.dmg_sha256);
                        self.try_pending_native_auto_apply(newly_announced);
                    }
                } else if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                    self.surface_update_apply_outcome(
                        "control request",
                        UpdateOutcome::Blocked {
                            reasons: vec!["No newer verified update is staged".to_string()],
                        },
                        false,
                    );
                }
            }
        }
    }

    /// Retry already-collected facts after service-owned check/apply work releases the
    /// reducer. No disk is reread and only the newest deferred sequence survives.
    pub(crate) fn finish_deferred_native_update_reconcile(&mut self) {
        if self.native_updater_service.snapshot().active.is_none()
            && self.native_updater_service.snapshot().phase != UpdaterPhase::Applying
            && let Some((purpose, facts)) = self.deferred_native_update_reconcile.take()
        {
            self.finish_native_update_reconcile(purpose, facts);
        }
    }

    /// Return the active Settings view only while its compact/desktop semantic
    /// route is Diagnostics. The event-loop cadence uses this exact route fact;
    /// hidden tabs and every other native app remain timer-free.
    pub(crate) fn active_native_diagnostics_view(
        &self,
        wid: WindowId,
    ) -> Option<crate::tab_model::ViewId> {
        let (_, view) = self.windows.get(&wid)?.front_content?.native()?;
        match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Diagnostics =>
            {
                Some(view)
            }
            _ => None,
        }
    }

    pub(crate) fn native_ui_viewport(
        &self,
        wid: WindowId,
    ) -> Result<crate::native_ui::LogicalRect, String> {
        let Some(ws) = self.windows.get(&wid) else {
            return Err("unknown window".to_string());
        };
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let trailing_pad = if self.hud_rows.min(ws.hud_cap) == 0 {
            pad
        } else {
            0
        };
        let scale = ws.scale.max(f64::EPSILON) as f32;
        if let Some((_, view)) = self.active_native_view(wid)
            && let Some(plan) = self.active_visible_leaf_plan(wid)
            && plan.leaves.len() > 1
            && let Some(leaf) = plan.leaf(view)
        {
            return Ok(crate::native_ui::LogicalRect::new(
                0.0,
                0.0,
                leaf.rect.size.width * cw as f32 / scale,
                leaf.rect.size.height * ch as f32 / scale,
            ));
        }
        Ok(crate::native_ui::LogicalRect::new(
            0.0,
            0.0,
            usize::from(ws.cols)
                .saturating_mul(cw)
                .saturating_add(pad.saturating_mul(2)) as f32
                / scale,
            usize::from(ws.rows)
                .saturating_mul(ch)
                // With no HUD, native content extends through the outer base-bottom
                // pad. With a HUD below it, that pad belongs after the HUD instead,
                // so the card stops at the native row boundary.
                .saturating_add(trailing_pad) as f32
                / scale,
        ))
    }

    /// Device-pixel Y where native content begins: effective top pad + OS head band +
    /// in-frame tab-strip rows. The painter and pointer projection share this one
    /// origin so a native card can neither cover the strip nor hit-test one pad off.
    pub(crate) fn native_content_origin_y(&self, wid: WindowId) -> usize {
        let (_, ch) = self.win_cell_size(wid);
        self.win_pad_top(wid)
            .saturating_add(self.win_head(wid))
            .saturating_add(usize::from(self.tab_strip_rows).saturating_mul(ch))
    }

    pub(crate) fn native_ui_compile_stamp(
        &self,
        wid: WindowId,
    ) -> Result<NativeUiCompileStamp, String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        self.native_ui_compile_stamp_for(wid, instance, view, self.native_ui_viewport(wid)?)
    }

    pub(crate) fn native_ui_compile_stamp_for(
        &self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        viewport: crate::native_ui::LogicalRect,
    ) -> Result<NativeUiCompileStamp, String> {
        use std::hash::{Hash, Hasher};

        let Some(ws) = self.windows.get(&wid) else {
            return Err("unknown window".to_string());
        };
        let generation = self
            .native_runtime
            .view_generation(view)
            .ok_or_else(|| "native view is no longer live".to_string())?;
        let presentation_revision = self
            .native_runtime
            .view_state(view)
            .map_or(0, |state| state.common().presentation_revision);
        let document_seq = self
            .native_runtime
            .document_id(instance)
            .and_then(|document| self.document_store.snapshot(document))
            .map(|snapshot| snapshot.seq.0);
        let (cw, ch) = self.win_cell_size(wid);
        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        ws.cols.hash(&mut geometry);
        ws.rows.hash(&mut geometry);
        cw.hash(&mut geometry);
        ch.hash(&mut geometry);
        self.win_pad(wid).hash(&mut geometry);
        self.win_pad_top(wid).hash(&mut geometry);
        self.win_head(wid).hash(&mut geometry);
        ws.scale.to_bits().hash(&mut geometry);
        viewport.x.to_bits().hash(&mut geometry);
        viewport.y.to_bits().hash(&mut geometry);
        viewport.width.to_bits().hash(&mut geometry);
        viewport.height.to_bits().hash(&mut geometry);
        let mut paint = std::collections::hash_map::DefaultHasher::new();
        self.theme.fg.hash(&mut paint);
        self.theme.bg.hash(&mut paint);
        self.theme.cursor.hash(&mut paint);
        self.theme.selection.hash(&mut paint);
        self.win_font_px(wid).to_bits().hash(&mut paint);
        self.font_family.hash(&mut paint);
        self.font_config.styled_paths.hash(&mut paint);
        self.font_config.synthetic_style.hash(&mut paint);
        self.font_config.fallback_fonts.hash(&mut paint);
        self.font_config.symbol_font.hash(&mut paint);
        self.font_config.emoji_font.hash(&mut paint);
        for (tag, value) in &self.font_variations {
            tag.hash(&mut paint);
            value.to_bits().hash(&mut paint);
        }
        self.font_weight_dark_nudge.to_bits().hash(&mut paint);
        native_appearance_revision(crate::native_appearance::current_preferences())
            .hash(&mut paint);
        native_motion_revision(self.native_view_motion_cx(wid, view)).hash(&mut paint);
        Ok(NativeUiCompileStamp {
            instance,
            view,
            generation,
            geometry: geometry.finish(),
            config_revision: self.native_config_service.snapshot().revision,
            update_revision: self.native_updater_service.snapshot().revision,
            document_seq,
            presentation_revision,
            paint_revision: paint.finish(),
        })
    }

    /// Return the compiled artifact retained for glass only while every live
    /// input still matches the stamp captured with it.
    pub(crate) fn cached_native_ui(&self, wid: WindowId) -> Option<&NativeCompiledFrame> {
        let stamp = self.native_ui_compile_stamp(wid).ok()?;
        self.windows
            .get(&wid)?
            .native_ui_compiled
            .as_ref()
            .filter(|frame| frame.stamp == stamp)
    }

    pub(crate) fn invalidate_native_ui_cache(&mut self, wid: WindowId) {
        if let Some(window) = self.windows.get_mut(&wid) {
            window.native_ui_compiled = None;
            for cache in window.leaf_render_cache.values_mut() {
                cache.native = None;
                cache.native_damage = Some(DamageRegion::All);
            }
        }
    }

    /// Mark exactly one retained native leaf dirty. Damage is leaf-local and
    /// never invalidates a sibling's scene; repeated regions are conservatively
    /// unioned until the renderer consumes them.
    pub(crate) fn invalidate_native_view_cache(
        &mut self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        damage: DamageRegion,
    ) {
        let Some(window) = self.windows.get_mut(&wid) else {
            return;
        };
        if window
            .native_ui_compiled
            .as_ref()
            .is_some_and(|frame| frame.stamp.view == view)
        {
            window.native_ui_compiled = None;
        }
        let cache = window.leaf_render_cache.entry(view).or_default();
        cache.native_damage = Some(match cache.native_damage {
            Some(existing) => union_native_damage(existing, damage),
            None => damage,
        });
        window.last_present = None;
    }

    /// Compile the active native app's one semantic tree for inspection or hit
    /// testing. Paint uses this exact compiler in `redraw_native_window`.
    pub(crate) fn compiled_native_ui(
        &self,
        wid: WindowId,
    ) -> Result<crate::native_ui::CompiledUi, String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let viewport = self.native_ui_viewport(wid)?;
        self.compiled_native_ui_for(wid, instance, view, viewport)
    }

    pub(crate) fn compiled_native_ui_for(
        &self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        viewport: crate::native_ui::LogicalRect,
    ) -> Result<crate::native_ui::CompiledUi, String> {
        let document = self
            .native_runtime
            .document_id(instance)
            .and_then(|document| self.document_store.snapshot(document));
        let animation_phase_ms =
            u64::try_from(self.lat_epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        let semantic_font = self.prepare_native_semantic_font(wid, view, animation_phase_ms);
        let tree = self
            .native_runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: self.native_config_service.snapshot().revision,
                    update_revision: self.native_updater_service.snapshot().revision,
                    animation_phase_ms,
                    motion: self.native_view_motion_cx(wid, view),
                    terminal_font_px: self.win_font_px(wid),
                    terminal_theme: self.theme,
                    semantic_font,
                    document: document.as_ref(),
                },
            )
            .map_err(|error| format!("native render failed: {error:?}"))?;
        let compiled = tree
            .compile(viewport)
            .map_err(|error| format!("native compile failed: {error:?}"))?;
        compiled
            .validate_parity()
            .map_err(|error| format!("native observer parity failed: {error:?}"))?;
        Ok(compiled)
    }

    /// Build one independently cacheable native leaf scene. No active/focused
    /// window assumption enters this seam.
    pub(crate) fn build_native_leaf_scene(
        &self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        viewport: crate::native_ui::LogicalRect,
        damage: DamageRegion,
    ) -> Result<NativeLeafScene, String> {
        Ok(NativeLeafScene {
            stamp: self.native_ui_compile_stamp_for(wid, instance, view, viewport)?,
            instance,
            view,
            viewport,
            damage,
            compiled: self.compiled_native_ui_for(wid, instance, view, viewport)?,
        })
    }

    pub(crate) fn native_view_motion_cx(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
    ) -> crate::native_app::ViewMotionCx {
        let focused_leaf = self
            .active_native_view(wid)
            .is_some_and(|(_, active)| active == view);
        let focused_window = self.windows.get(&wid).is_some_and(|window| window.focused);
        crate::native_app::ViewMotionCx {
            system_reduced: self.system_reduce_motion,
            focused: self.motion_focus(wid, focused_leaf && focused_window),
            performance_reduced: self.perf_reduced,
            serious: self.serious_mode_enabled(),
        }
    }

    /// Resolve one exact retained native artifact. Paint, pointer routing,
    /// accessibility and inspection consume this same semantic tree and device
    /// destination. Any lifecycle, geometry, service, theme, or document drift
    /// changes the compile stamp and therefore fails closed.
    pub(crate) fn retained_native_leaf_artifact(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        require_presented: bool,
    ) -> Option<RetainedNativeLeafArtifact<'_>> {
        let window = self.windows.get(&wid)?;
        if window.overlay.is_some() {
            return None;
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        let leaf = plan.leaf(view)?;
        let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        let generation = self.native_runtime.view_generation(view)?;
        let (cw, ch) = self.win_cell_size(wid);
        let scale = window.scale.max(f64::EPSILON);
        let viewport = if plan.leaves.len() == 1 {
            self.native_ui_viewport(wid).ok()?
        } else {
            crate::native_ui::LogicalRect::new(
                0.0,
                0.0,
                leaf.rect.size.width * cw as f32 / scale as f32,
                leaf.rect.size.height * ch as f32 / scale as f32,
            )
        };
        let expected_stamp = self
            .native_ui_compile_stamp_for(wid, native.instance, view, viewport)
            .ok()?;
        let raster = window.leaf_render_cache.get(&view)?.native.as_ref()?;
        if raster.stamp != expected_stamp
            || raster.stamp.generation != generation
            || require_presented && !raster.presented
        {
            return None;
        }
        let card = window.settings_card.as_ref()?;
        if raster.presented_x.checked_add(raster.width)? > card.pw
            || raster.presented_y.checked_add(raster.height)? > card.ph
            || usize::try_from(card.pw)
                .ok()?
                .checked_mul(usize::try_from(card.ph).ok()?)?
                .checked_mul(4)?
                != card.rgba.len()
        {
            return None;
        }
        let (frame_x, frame_y) = self.frame_origin(wid);
        let device_x = frame_x
            .checked_add(i64::from(card.dx))?
            .checked_add(i64::from(raster.presented_x))?;
        let device_y = frame_y
            .checked_add(i64::from(card.dy))?
            .checked_add(i64::from(raster.presented_y))?;
        if let Some(size) = window.win_px {
            let right = device_x.checked_add(i64::from(raster.width))?;
            let bottom = device_y.checked_add(i64::from(raster.height))?;
            // Partial intersection is the normal centred-crop case. Reject only
            // a truly offscreen retained destination (or a torn empty extent).
            if raster.width == 0
                || raster.height == 0
                || right <= 0
                || bottom <= 0
                || device_x >= i64::from(size.width)
                || device_y >= i64::from(size.height)
            {
                return None;
            }
        }
        Some(RetainedNativeLeafArtifact {
            instance: native.instance,
            view,
            generation,
            compiled: &raster.compiled,
            device_x,
            device_y,
            width: raster.width,
            height: raster.height,
            scale,
        })
    }

    /// Resolve the retained leaf containing a raw window-space pointer and map
    /// it into that leaf's canonical logical coordinates.
    pub(crate) fn retained_native_leaf_at_pointer(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(RetainedNativeLeafArtifact<'_>, f32, f32)> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        for leaf in &plan.leaves {
            let Some(artifact) = self.retained_native_leaf_artifact(wid, leaf.view, true) else {
                continue;
            };
            let local_x = x - artifact.device_x as f64;
            let local_y = y - artifact.device_y as f64;
            if local_x >= 0.0
                && local_y >= 0.0
                && local_x < f64::from(artifact.width)
                && local_y < f64::from(artifact.height)
            {
                let logical_x = (local_x / artifact.scale) as f32;
                let logical_y = (local_y / artifact.scale) as f32;
                return Some((artifact, logical_x, logical_y));
            }
        }
        None
    }

    /// Convert a window-space pointer into the focused native app's retained
    /// content coordinates.
    #[cfg(test)]
    pub(crate) fn native_content_point(&self, wid: WindowId, x: f64, y: f64) -> Option<(f32, f32)> {
        let (_, focused) = self.active_native_view(wid)?;
        let (artifact, x, y) = self.retained_native_leaf_at_pointer(wid, x, y)?;
        (artifact.view == focused).then_some((x, y))
    }

    /// Dispatch through the active app reducer, execute every typed effect, then
    /// refresh canonical tab chrome. This is shared by human pointer/keyboard
    /// input and the control `act` path.
    pub(crate) fn reconcile_active_editor_viewport(
        &mut self,
        wid: WindowId,
    ) -> Result<bool, String> {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return Ok(false);
        };
        if !self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Editor)
        {
            return Ok(false);
        }
        let palette_candidates = self
            .native_runtime
            .view_state(view)
            .and_then(|state| match state {
                crate::native_app::AppViewState::Editor(state) => state.buffer.as_ref(),
                _ => None,
            })
            .and_then(|buffer| match &buffer.minibuffer {
                crate::native_editor::Minibuffer::Command { query, .. } => {
                    Some(crate::native_editor::command_completions(query).len())
                }
                _ => None,
            })
            .unwrap_or(0);
        let visible_lines = crate::native_ui::editor_visible_line_capacity_with_palette(
            self.native_ui_viewport(wid)?,
            palette_candidates,
        );
        if matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Editor(state))
                if state.buffer.as_ref().is_some_and(|buffer| {
                    buffer.viewport_lines() == visible_lines
                })
        ) {
            return Ok(false);
        }
        let Some(_) = self.dispatch_editor_event(
            instance,
            view,
            &AppEvent::EditorViewportChanged { visible_lines },
        )?
        else {
            return Ok(false);
        };
        self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        Ok(true)
    }

    pub(crate) fn dispatch_native_event(
        &mut self,
        wid: WindowId,
        event: AppEvent,
    ) -> Result<EventResult, String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let kind = self
            .native_runtime
            .app(instance)
            .map(crate::native_app::NativeApp::kind)
            .ok_or_else(|| "active native app disappeared".to_string())?;
        if kind == crate::native_app::AppKind::Editor
            && !matches!(event, AppEvent::EditorViewportChanged { .. })
        {
            let _ = self.reconcile_active_editor_viewport(wid)?;
        }
        if matches!(
            kind,
            crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor
        ) && let AppEvent::Action(invocation) = &event
            && crate::command_registry::native_document_action(invocation.id.as_str()).is_none()
        {
            return Err(format!(
                "unregistered native document command: {}",
                invocation.id.as_str()
            ));
        }
        if let AppEvent::EditorCommand(command) = &event {
            let _ = crate::command_registry::editor_command(command);
        }
        // Palette/buttons and the Emacs keymap lower into the same typed editor
        // reducer. The command registry above classifies authority before this
        // adapter is allowed to interpret an ActionId.
        let event = if kind == crate::native_app::AppKind::Markdown {
            match event {
                AppEvent::ScrollLines(lines) => {
                    let viewport = self.native_ui_viewport(wid)?;
                    AppEvent::MarkdownScroll {
                        lines,
                        viewport_width: viewport.width,
                        viewport_height: viewport.height,
                    }
                }
                AppEvent::MarkdownPage {
                    direction,
                    viewport_width: _,
                    viewport_height: _,
                } => {
                    let viewport = self.native_ui_viewport(wid)?;
                    AppEvent::MarkdownPage {
                        direction,
                        viewport_width: viewport.width,
                        viewport_height: viewport.height,
                    }
                }
                event => event,
            }
        } else if kind == crate::native_app::AppKind::Editor {
            match &event {
                AppEvent::Action(invocation) => match invocation.id.as_str() {
                    "editor/save" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Save)
                    }
                    "editor/undo" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Undo)
                    }
                    "editor/redo" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Redo)
                    }
                    "editor/find" => AppEvent::EditorCommand(
                        crate::native_editor::EditorCommand::IncrementalSearch,
                    ),
                    "editor/goto-line" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::GotoLine)
                    }
                    "editor/commands" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::ExecuteCommand)
                    }
                    "editor/revert" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::RevertBuffer)
                    }
                    action if action.starts_with("editor/completion/") => invocation
                        .id
                        .as_str()
                        .strip_prefix("editor/completion/")
                        .and_then(|index| index.parse::<usize>().ok())
                        .map(|index| {
                            AppEvent::EditorCompletion(
                                crate::native_editor::EditorCompletionAction::Choose(index),
                            )
                        })
                        .unwrap_or_else(|| event.clone()),
                    _ => event,
                },
                _ => event,
            }
        } else {
            event
        };
        if let Some(result) = self.dispatch_editor_event(instance, view, &event)? {
            // The editor workspace is an independent reducer and currently has
            // no regional-damage effect lane. Invalidate its exact leaf only.
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            self.refresh_native_presentation(wid, instance, view);
            return Ok(result);
        }
        let outcome = self
            .native_runtime
            .dispatch(instance, view, event)
            .map_err(|error| format!("native dispatch failed: {error:?}"))?;
        // A reducer that mutates without asking for repaint still fails safe,
        // but only its own leaf is widened to full damage.
        if !outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::RepaintSelf(_)))
        {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        }
        for effect in outcome.effects {
            self.execute_native_effect(wid, instance, view, effect)?;
        }
        self.refresh_native_presentation(wid, instance, view);
        Ok(outcome.result)
    }

    /// Dispatch to one exact native view, refusing a stale control target or a
    /// view that is no longer active in the requested window. Human input uses
    /// [`Self::dispatch_native_event`]; semantic control actions use this form so
    /// an intervening tab switch can never redirect an action to the wrong app.
    pub(crate) fn dispatch_native_view_event(
        &mut self,
        wid: WindowId,
        expected_view: crate::tab_model::ViewId,
        event: AppEvent,
    ) -> Result<EventResult, String> {
        let (_, active_view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        if active_view != expected_view {
            return Err("native app target is stale or no longer active".to_string());
        }
        self.dispatch_native_event(wid, event)
    }

    /// Native tabs are an input boundary: keyboard/text events are reduced by
    /// the app and never fall through to a hidden PTY. Geometry and focus are
    /// deliberately returned to the host seam because they remain window
    /// properties even while a native tab is active.
    pub(crate) fn native_input_event(
        &mut self,
        wid: WindowId,
        event: &crate::input::InputEvent,
    ) -> bool {
        use crate::input::{InputEvent, ScrollIntent};
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        let Some((instance, active_view)) = self.active_native_view(wid) else {
            return false;
        };
        let editor_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Editor);
        let editor_chord_pending = editor_active
            && matches!(
                self.native_runtime.view_state(active_view),
                Some(crate::native_app::AppViewState::Editor(state))
                    if state
                        .buffer
                        .as_ref()
                        .is_some_and(crate::native_editor::EditorBufferView::chord_pending)
            );
        let editor_command_palette_active = editor_active
            && matches!(
                self.native_runtime.view_state(active_view),
                Some(crate::native_app::AppViewState::Editor(state))
                    if state.buffer.as_ref().is_some_and(|buffer| {
                        matches!(
                            buffer.minibuffer,
                            crate::native_editor::Minibuffer::Command { .. }
                        )
                    })
            );
        let settings_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings);
        let markdown_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Markdown);

        if let InputEvent::Key {
            key: Key::Named(NamedKey::Tab),
            mods,
            event_type: KeyEventType::Press,
            ..
        } = event
        {
            if editor_command_palette_active {
                let _ = self.dispatch_native_event(
                    wid,
                    AppEvent::EditorCompletion(
                        crate::native_editor::EditorCompletionAction::Complete,
                    ),
                );
                return true;
            }
            let _ = self.move_native_focus(wid, mods.contains(Modifiers::SHIFT));
            return true;
        }
        if !editor_active
            && !self.native_text_field_has_focus(wid)
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Enter | NamedKey::Space),
                event_type: KeyEventType::Press,
                ..
            } = event
            && self.activate_native_focus(wid).unwrap_or(false)
        {
            return true;
        }

        let app_event = match event {
            InputEvent::Resize { .. } | InputEvent::Focus(_) => return false,
            InputEvent::Text(text) | InputEvent::Paste(text) => {
                Some(AppEvent::TextInput(TextInputEvent::Commit(text.clone())))
            }
            InputEvent::Key {
                event_type: KeyEventType::Release,
                ..
            } => None,
            InputEvent::Key {
                key,
                mods,
                event_type: KeyEventType::Press | KeyEventType::Repeat,
                ..
            } => {
                let command = mods.intersects(Modifiers::SUPER | Modifiers::CTRL);
                let extend = mods.contains(Modifiers::SHIFT);
                if settings_active
                    && mods.contains(Modifiers::SUPER)
                    && matches!(key, Key::Character('f' | 'F'))
                {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("settings/search"),
                        value: None,
                    }))
                } else if editor_active
                    && mods.contains(Modifiers::SUPER)
                    && matches!(key, Key::Character('s' | 'S'))
                {
                    Some(AppEvent::EditorCommand(
                        crate::native_editor::EditorCommand::Save,
                    ))
                } else if markdown_active && command && matches!(key, Key::Character('[')) {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("markdown/back"),
                        value: None,
                    }))
                } else if markdown_active && command && matches!(key, Key::Character(']')) {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("markdown/forward"),
                        value: None,
                    }))
                } else if markdown_active && command && matches!(key, Key::Character('e' | 'E')) {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("markdown/edit"),
                        value: None,
                    }))
                } else if editor_active
                    && (editor_chord_pending
                        || mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::META))
                {
                    let key = match key {
                        Key::Character(character) => {
                            Some(character.to_ascii_lowercase().to_string())
                        }
                        Key::Named(NamedKey::Space) => Some("space".to_string()),
                        Key::Named(NamedKey::Backspace) => Some("backspace".to_string()),
                        Key::Named(NamedKey::Delete) => Some("delete".to_string()),
                        Key::Named(NamedKey::Enter) => Some("enter".to_string()),
                        Key::Named(NamedKey::Escape) => Some("escape".to_string()),
                        _ => None,
                    };
                    key.map(|key| {
                        AppEvent::EditorChord(crate::native_editor::KeyChord {
                            control: mods.contains(Modifiers::CTRL),
                            meta: mods.intersects(Modifiers::ALT | Modifiers::META),
                            shift: mods.contains(Modifiers::SHIFT),
                            key,
                        })
                    })
                } else {
                    // READLINE caret bindings for the Settings text fields (the
                    // search + free-form value fields): Ctrl-A/E home/end, Ctrl-B/F
                    // left/right, Ctrl-D delete forward, Ctrl-K/U kill to end/start,
                    // Ctrl-W word back — the macOS system Emacs set. CTRL-only
                    // (never ⌘/⌥), so ⌘A Select-All and the chords above are
                    // untouched, and matched BEFORE the `command` arms because
                    // `command` deliberately folds CTRL in. Scoped to Settings so
                    // other native apps keep their existing Ctrl shortcuts.
                    let readline = settings_active
                        && mods.contains(Modifiers::CTRL)
                        && !mods.intersects(
                            Modifiers::SUPER | Modifiers::ALT | Modifiers::HYPER | Modifiers::META,
                        );
                    match key {
                        Key::Character('a' | 'A') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Home { extend }))
                        }
                        Key::Character('e' | 'E') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::End { extend }))
                        }
                        Key::Character('b' | 'B') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Left { extend }))
                        }
                        Key::Character('f' | 'F') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Right { extend }))
                        }
                        Key::Character('d' | 'D') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Delete))
                        }
                        Key::Character('k' | 'K') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::KillToEnd))
                        }
                        Key::Character('u' | 'U') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::KillToStart))
                        }
                        Key::Character('w' | 'W') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::DeleteWordBackward))
                        }
                        Key::Character('a' | 'A') if command => {
                            Some(AppEvent::TextInput(TextInputEvent::SelectAll))
                        }
                        Key::Character('z' | 'Z') if command && extend => {
                            Some(AppEvent::TextInput(TextInputEvent::Redo))
                        }
                        Key::Character('z' | 'Z') if command => {
                            Some(AppEvent::TextInput(TextInputEvent::Undo))
                        }
                        Key::Character('y' | 'Y') if command => {
                            Some(AppEvent::TextInput(TextInputEvent::Redo))
                        }
                        Key::Character(character)
                            if !mods.intersects(
                                Modifiers::CTRL
                                    | Modifiers::ALT
                                    | Modifiers::SUPER
                                    | Modifiers::HYPER
                                    | Modifiers::META,
                            ) =>
                        {
                            Some(AppEvent::TextInput(TextInputEvent::Commit(
                                character.to_string(),
                            )))
                        }
                        Key::Named(NamedKey::Space)
                            if !mods.intersects(
                                Modifiers::CTRL
                                    | Modifiers::ALT
                                    | Modifiers::SUPER
                                    | Modifiers::HYPER
                                    | Modifiers::META,
                            ) =>
                        {
                            Some(AppEvent::TextInput(TextInputEvent::Commit(" ".to_string())))
                        }
                        Key::Named(NamedKey::Backspace) => {
                            Some(AppEvent::TextInput(TextInputEvent::Backspace))
                        }
                        Key::Named(NamedKey::Delete) => {
                            Some(AppEvent::TextInput(TextInputEvent::Delete))
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            Some(AppEvent::TextInput(TextInputEvent::Left { extend }))
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            Some(AppEvent::TextInput(TextInputEvent::Right { extend }))
                        }
                        Key::Named(NamedKey::ArrowUp) if editor_active => {
                            if editor_command_palette_active {
                                Some(AppEvent::EditorCompletion(
                                    crate::native_editor::EditorCompletionAction::Previous,
                                ))
                            } else {
                                Some(AppEvent::EditorCommand(
                                    crate::native_editor::EditorCommand::MoveLineUp,
                                ))
                            }
                        }
                        Key::Named(NamedKey::ArrowDown) if editor_active => {
                            if editor_command_palette_active {
                                Some(AppEvent::EditorCompletion(
                                    crate::native_editor::EditorCompletionAction::Next,
                                ))
                            } else {
                                Some(AppEvent::EditorCommand(
                                    crate::native_editor::EditorCommand::MoveLineDown,
                                ))
                            }
                        }
                        Key::Named(NamedKey::Home) if editor_active => {
                            Some(AppEvent::EditorCommand(
                                crate::native_editor::EditorCommand::MoveLineStart,
                            ))
                        }
                        Key::Named(NamedKey::End) if editor_active => {
                            Some(AppEvent::EditorCommand(
                                crate::native_editor::EditorCommand::MoveLineEnd,
                            ))
                        }
                        Key::Named(NamedKey::Enter) => {
                            Some(AppEvent::TextInput(TextInputEvent::Submit))
                        }
                        Key::Named(NamedKey::Escape) => {
                            Some(AppEvent::TextInput(TextInputEvent::Cancel))
                        }
                        Key::Named(NamedKey::ArrowUp) => Some(AppEvent::ScrollLines(-1)),
                        Key::Named(NamedKey::ArrowDown) => Some(AppEvent::ScrollLines(1)),
                        Key::Named(NamedKey::PageUp) if markdown_active => {
                            Some(AppEvent::MarkdownPage {
                                direction: -1,
                                viewport_width: 0.0,
                                viewport_height: 0.0,
                            })
                        }
                        Key::Named(NamedKey::PageDown) if markdown_active => {
                            Some(AppEvent::MarkdownPage {
                                direction: 1,
                                viewport_width: 0.0,
                                viewport_height: 0.0,
                            })
                        }
                        Key::Named(NamedKey::PageUp) => Some(AppEvent::ScrollLines(-8)),
                        Key::Named(NamedKey::PageDown) => Some(AppEvent::ScrollLines(8)),
                        Key::Named(NamedKey::Home) if markdown_active || command => {
                            Some(AppEvent::ScrollLines(-10_000))
                        }
                        Key::Named(NamedKey::End) if markdown_active || command => {
                            Some(AppEvent::ScrollLines(10_000))
                        }
                        _ => None,
                    }
                }
            }
            InputEvent::Key { .. } => None,
            InputEvent::Wheel { dir_up, lines, .. } => Some(AppEvent::ScrollLines(if *dir_up {
                -(*lines).max(1)
            } else {
                (*lines).max(1)
            })),
            InputEvent::ScrollView(ScrollIntent::Up | ScrollIntent::PrevPrompt)
                if markdown_active =>
            {
                Some(AppEvent::MarkdownPage {
                    direction: -1,
                    viewport_width: 0.0,
                    viewport_height: 0.0,
                })
            }
            InputEvent::ScrollView(ScrollIntent::Down | ScrollIntent::NextPrompt)
                if markdown_active =>
            {
                Some(AppEvent::MarkdownPage {
                    direction: 1,
                    viewport_width: 0.0,
                    viewport_height: 0.0,
                })
            }
            InputEvent::ScrollView(intent) => Some(AppEvent::ScrollLines(match intent {
                ScrollIntent::By(lines) => -*lines,
                ScrollIntent::Up | ScrollIntent::PrevPrompt => -8,
                ScrollIntent::Down | ScrollIntent::NextPrompt => 8,
                ScrollIntent::Top => -10_000,
                ScrollIntent::Bottom => 10_000,
            })),
            InputEvent::KeySequence(_)
            | InputEvent::MouseButton { .. }
            | InputEvent::MouseMove { .. } => None,
        };

        if let Some(event) = app_event {
            let _ = self.dispatch_native_event(wid, event);
        }
        true
    }

    fn move_native_focus(&mut self, wid: WindowId, backwards: bool) -> Result<(), String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let compiled = self.compiled_native_ui(wid)?;
        if compiled.focus_order.is_empty() {
            return Ok(());
        }
        let current = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.as_ref());
        let next = current
            .and_then(|key| {
                compiled
                    .focus_order
                    .iter()
                    .position(|candidate| candidate == key)
            })
            .map_or_else(
                || {
                    if backwards {
                        compiled.focus_order.len() - 1
                    } else {
                        0
                    }
                },
                |index| {
                    if backwards {
                        index
                            .checked_sub(1)
                            .unwrap_or(compiled.focus_order.len() - 1)
                    } else {
                        (index + 1) % compiled.focus_order.len()
                    }
                },
            );
        let key = compiled.focus_order[next].clone();
        self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        let outcome = self
            .native_runtime
            .dispatch(instance, view, AppEvent::FocusChanged(Some(key)))
            .map_err(|error| format!("native focus failed: {error:?}"))?;
        for effect in outcome.effects {
            self.execute_native_effect(wid, instance, view, effect)?;
        }
        if let Some(state) = self.native_runtime.view_state_mut(view) {
            state.common_mut().focus_visible = true;
        }
        self.request_redraw_all_windows();
        Ok(())
    }

    fn native_text_field_has_focus(&self, wid: WindowId) -> bool {
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        let Some(key) = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.as_ref())
        else {
            return false;
        };
        self.compiled_native_ui(wid).ok().and_then(|compiled| {
            compiled.semantic(key).map(|semantic| {
                semantic.role == crate::native_ui::SemanticRole::TextField
                    && semantic.state.is_none_or(|state| state.enabled)
            })
        }) == Some(true)
    }

    fn activate_native_focus(&mut self, wid: WindowId) -> Result<bool, String> {
        let (_, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let Some(key) = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
        else {
            return Ok(false);
        };
        let compiled = self.compiled_native_ui(wid)?;
        let Some(semantic) = compiled.semantic(&key) else {
            return Ok(false);
        };
        if semantic.state.is_some_and(|state| !state.enabled) {
            return Ok(true);
        }
        let Some(action) = semantic.action.clone() else {
            return Ok(false);
        };
        self.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: action,
                value: None,
            }),
        )?;
        Ok(true)
    }

    fn execute_native_effect(
        &mut self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        effect: AppEffect,
    ) -> Result<(), String> {
        match effect {
            AppEffect::ConfigPatch { patch, reply } => {
                self.native_config_pending.push_back(NativeConfigRequest {
                    instance,
                    view,
                    reply,
                    work: NativeConfigWork::Patch(patch),
                });
                self.pump_native_config()?;
            }
            AppEffect::ConfigUndo { token, reply } => {
                self.native_config_pending.push_back(NativeConfigRequest {
                    instance,
                    view,
                    reply,
                    work: NativeConfigWork::Undo(token),
                });
                self.pump_native_config()?;
            }
            AppEffect::OpenExternal { request, reply } => {
                let safe = crate::is_safe_url(&request.uri);
                let outcome = if request.user_initiated && safe {
                    crate::app_mouse::open_url_external(&request.uri);
                    ExternalOpenOutcome::Opened
                } else if !safe {
                    ExternalOpenOutcome::Denied {
                        message: "unsupported or unsafe URL scheme".to_string(),
                    }
                } else {
                    ExternalOpenOutcome::Denied {
                        message: "external opens require a user gesture".to_string(),
                    }
                };
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::ExternalOpenFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Clipboard { request, reply } => {
                let copied = match request {
                    ClipboardRequest::CopyText { text, .. } => crate::control::pbcopy(&text),
                    ClipboardRequest::CopyDocumentRange {
                        document, range, ..
                    } => self
                        .document_store
                        .snapshot(document)
                        .is_some_and(|snapshot| {
                            range.start < range.end
                                && range.end <= snapshot.text.len()
                                && snapshot.text.is_char_boundary(range.start)
                                && snapshot.text.is_char_boundary(range.end)
                                && crate::control::pbcopy(&snapshot.text[range])
                        }),
                };
                let outcome = if copied {
                    ClipboardOutcome::Copied
                } else {
                    ClipboardOutcome::Failed {
                        message: "clipboard unavailable".to_string(),
                    }
                };
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::ClipboardFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Recovery { request, reply } => {
                let outcome = self.execute_recovery_request(wid, request);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::RecoveryFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Update { request, reply } => {
                let outcome = self.execute_native_update(request);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::UpdateFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Packages { request, reply } => {
                let outcome = self.execute_native_packages(request);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::PackagesFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::OpenDocumentEditor { document } => {
                let uri = self
                    .document_store
                    .canonical_uri(document)
                    .ok_or_else(|| "Markdown document disappeared before edit".to_string())?
                    .to_string();
                self.open_document_tab_in_window(wid, crate::native_app::AppKind::Editor, &uri)?;
                self.dispatch_native_completion(
                    wid,
                    instance,
                    view,
                    AppEvent::DocumentEditorOpened { document },
                )?;
            }
            AppEffect::RequestCloseSelf => {
                self.close_active_native_tab(wid)?;
            }
            AppEffect::InvalidateOwnPresentation => {
                self.refresh_native_presentation(wid, instance, view);
            }
            AppEffect::RepaintSelf(damage) => {
                self.invalidate_native_view_cache(wid, view, damage);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.last_present = None;
                    // The native tray is a retained full-page raster. Dropping it
                    // here makes a repaint effect fail closed: neither a control
                    // capture nor the next glass present can reuse pixels from the
                    // previous route while the new semantic tree is already live.
                    ws.settings_card = None;
                }
                if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    window.request_redraw();
                }
            }
        }
        Ok(())
    }

    fn pump_native_config(&mut self) -> Result<(), String> {
        while !self.native_config_inflight {
            if self.native_config_pending.is_empty() {
                return Ok(());
            }
            // Acquire every fallible host capability before reducing the queued
            // request. Otherwise an unavailable event-loop proxy/worker could
            // advance the in-memory revision without ever making it durable.
            let proxy = self
                .proxy
                .clone()
                .ok_or_else(|| "native config persistence needs an event-loop proxy".to_string())?;
            let queue = native_config_queue()?;
            let request = self
                .native_config_pending
                .pop_front()
                .expect("queue was checked before capability acquisition");
            let result = match request.work {
                NativeConfigWork::Patch(patch) => {
                    self.native_config_service.patch(ConfigPatchRequest {
                        base_revision: patch.base_revision,
                        edits: patch
                            .edits
                            .into_iter()
                            .map(|edit| ConfigKeyEdit {
                                key: edit.key,
                                expected: match edit.expected {
                                    ExpectedConfigValue::Any => ExpectedValue::Any,
                                    ExpectedConfigValue::Exact(value) => {
                                        ExpectedValue::Exact(value)
                                    }
                                },
                                value: edit.value,
                            })
                            .collect(),
                    })
                }
                NativeConfigWork::Undo(token) => self
                    .native_config_service
                    .undo(crate::native_config_service::UndoToken::from_stored(token)),
            };
            match result {
                ConfigPatchResult::Applied { snapshot, undo } => {
                    queue
                        .send(NativeConfigJob {
                            snapshot,
                            undo: Some(undo.get()),
                            instance: request.instance,
                            view: request.view,
                            reply: request.reply,
                            proxy,
                        })
                        .map_err(|_| "native config worker stopped".to_string())?;
                    self.native_config_inflight = true;
                    return Ok(());
                }
                ConfigPatchResult::Unchanged { snapshot } => {
                    self.publish_native_config_completion(
                        request.instance,
                        request.view,
                        request.reply,
                        ConfigPatchOutcome::Applied {
                            revision: snapshot.revision,
                            undo: None,
                        },
                    );
                }
                ConfigPatchResult::Conflict { snapshot, .. } => {
                    self.publish_native_config_completion(
                        request.instance,
                        request.view,
                        request.reply,
                        ConfigPatchOutcome::Conflict {
                            revision: snapshot.revision,
                        },
                    );
                }
                ConfigPatchResult::Rejected { message, .. } => {
                    self.publish_native_config_completion(
                        request.instance,
                        request.view,
                        request.reply,
                        ConfigPatchOutcome::Rejected { message },
                    );
                }
            }
        }
        Ok(())
    }

    /// Main-thread half of the config worker protocol. The durable transaction
    /// already completed even if its initiating view/instance closed; revision
    /// publication fans out to every currently live Settings view.
    pub(crate) fn finish_native_config_write(
        &mut self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
        outcome: ConfigPatchOutcome,
    ) {
        self.native_config_inflight = false;
        if matches!(outcome, ConfigPatchOutcome::Rejected { .. })
            && let Ok(current) =
                crate::native_config_service::VersionedConfigService::load_current()
        {
            self.native_config_service = current;
        }
        self.publish_native_config_completion(instance, view, reply, outcome);
        if let Some(text) = self.native_config_external_pending.take()
            && self
                .sync_native_config_external(text)
                .ok()
                .flatten()
                .is_some()
            && let Some(proxy) = self.proxy.as_ref()
        {
            let _ = proxy.send_event(Wake::ConfigReload);
        }
        let _ = self.pump_native_config();
    }

    fn publish_native_config_completion(
        &mut self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
        outcome: ConfigPatchOutcome,
    ) {
        let revision = match &outcome {
            ConfigPatchOutcome::Applied { revision, .. }
            | ConfigPatchOutcome::Conflict { revision } => Some(*revision),
            ConfigPatchOutcome::Rejected { .. } => None,
        };
        if self.native_runtime.completion_is_current(&reply)
            && self.native_runtime.view_state(view).is_some()
            && self.native_runtime.app(instance).is_some()
            && let Some(wid) = self.windows.iter().find_map(|(wid, ws)| {
                ws.tab_set
                    .tabs()
                    .iter()
                    .any(|tab| tab.root.contains(view))
                    .then_some(*wid)
            })
        {
            let _ = self.dispatch_native_completion(
                wid,
                instance,
                view,
                AppEvent::ConfigPatchFinished {
                    operation: reply.operation,
                    outcome,
                },
            );
        }

        if revision.is_some() {
            // The durable snapshot is fanned out by `reload_config` only after
            // the live App has installed the same parsed Config + catalog Arc.
            if let Some(proxy) = self.proxy.as_ref() {
                let _ = proxy.send_event(Wake::ConfigReload);
            }
            self.request_redraw_all_windows();
        }
    }

    /// Feed watcher snapshots into the same serialized config lane. An external
    /// edit observed while a durable write is in flight waits behind that write;
    /// queued UI requests are reduced only after its changed-key stamps land.
    pub(crate) fn sync_native_config_external(
        &mut self,
        text: String,
    ) -> Result<Option<crate::native_config_service::ConfigSnapshot>, String> {
        if self.native_config_inflight {
            self.native_config_external_pending = Some(text);
            return Ok(None);
        }
        let snapshot = self.native_config_service.replace_external(text)?;
        Ok(Some(snapshot))
    }

    /// Publish one already-installed App config generation to every live
    /// Settings presentation. Call only after `App.config` and
    /// `App.config_assets` have been replaced with this snapshot's data.
    pub(crate) fn publish_native_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) {
        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in views {
            let _ = self.native_runtime.dispatch(
                instance,
                view,
                AppEvent::ConfigChanged(snapshot.clone()),
            );
        }
    }

    fn dispatch_native_completion(
        &mut self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        event: AppEvent,
    ) -> Result<(), String> {
        let outcome = self
            .native_runtime
            .dispatch(instance, view, event)
            .map_err(|error| format!("native completion failed: {error:?}"))?;
        for effect in outcome.effects {
            match effect {
                AppEffect::RepaintSelf(damage) => {
                    self.invalidate_native_view_cache(wid, view, damage);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.last_present = None;
                    }
                }
                AppEffect::InvalidateOwnPresentation => {
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.last_present = None;
                    }
                }
                other => self.execute_native_effect(wid, instance, view, other)?,
            }
        }
        // Completion reducers remove pending work even when the host rejected
        // or blocked it. Refresh from the reducer's final presentation on every
        // accepted completion so the tab's busy/attention state cannot remain
        // one request behind its Settings view.
        self.refresh_native_presentation(wid, instance, view);
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
        Ok(())
    }

    fn execute_native_update(&mut self, request: UpdateRequest) -> UpdateOutcome {
        match request {
            UpdateRequest::Check | UpdateRequest::Retry => self.start_native_update_check(),
            UpdateRequest::InstallAndRelaunch => self.apply_native_update(ApplyMode::Immediate),
            UpdateRequest::InstallWhenSafe => {
                if self.native_updater_service.install_when_safe()
                    || self.native_updater_service.snapshot().install_on_clean_quit
                {
                    self.publish_native_update_state();
                    UpdateOutcome::Accepted
                } else {
                    UpdateOutcome::Blocked {
                        reasons: vec!["No newer verified update is staged".to_string()],
                    }
                }
            }
        }
    }

    /// Admit one Packages verb. Physical work (spawning the CO-LOCATED `atpkg`
    /// binary, reading its `status.toml`) always happens on a worker thread —
    /// this admission is memory-only plus one `current_exe`-sibling stat.
    fn execute_native_packages(&mut self, request: PackagesRequest) -> PackagesOutcome {
        let atpkg = crate::co_located_atpkg();
        let verb: &[&str] = match request {
            PackagesRequest::CheckUpdate => &["update"],
            PackagesRequest::InstallDefaultSet => &["install", "--default-set"],
        };
        let Some(atpkg) = atpkg else {
            return PackagesOutcome::Failed {
                message: "no co-located atpkg binary beside this executable".to_string(),
            };
        };
        if !atpkg::enabled() {
            // Same trust posture the binary itself enforces; refusing here is
            // honesty, not authority — atpkg would refuse loudly anyway.
            return PackagesOutcome::Blocked {
                message: "the package manager is inert (no root key pinned)".to_string(),
            };
        }
        if self.native_packages_service.busy().is_some() {
            return PackagesOutcome::Blocked {
                message: "a packages operation is already running".to_string(),
            };
        }
        let busy = match request {
            PackagesRequest::CheckUpdate => PackagesBusy::Check,
            PackagesRequest::InstallDefaultSet => PackagesBusy::Install,
        };
        let Some(proxy) = self.proxy.clone() else {
            return PackagesOutcome::Failed {
                message: "packages verbs require the event-loop service".to_string(),
            };
        };
        let Some(sequence) = self.native_packages_service.begin(Some(busy)) else {
            // Backstop only: the page disables its buttons whenever ANY worker
            // (including the silent status refresh) is inflight, so reaching
            // this means a click raced the disable — say so in user voice.
            return PackagesOutcome::Blocked {
                message: "Still collecting package status — try again in a moment.".to_string(),
            };
        };
        // The busy flip is observable: publish before the worker starts.
        self.publish_native_packages_state();
        let verb: Vec<String> = verb.iter().map(ToString::to_string).collect();
        let spawn = std::thread::Builder::new()
            .name("aterm-packages-verb".into())
            .spawn(move || {
                // Output is discarded on purpose: atpkg records its own
                // status.toml, which the collection below reads back.
                let _ = std::process::Command::new(&atpkg)
                    .args(&verb)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                let report = crate::packages_screen::collect_packages_status(true);
                let _ = proxy.send_event(Wake::NativePackagesFinished { sequence, report });
            });
        match spawn {
            Ok(_) => PackagesOutcome::Accepted,
            Err(error) => {
                // Roll the reservation back so the surface does not stay busy
                // forever; the synchronous failure is the user feedback. abort
                // (not finish) keeps any previously-observed report's real
                // facts — no worker ran, so there is nothing new to store.
                let _ = self.native_packages_service.abort(sequence);
                self.publish_native_packages_state();
                PackagesOutcome::Failed {
                    message: format!("could not start the packages worker: {error}"),
                }
            }
        }
    }

    /// Start one status-collection worker (no verb). A running worker makes
    /// this a no-op join; headless hosts (no proxy) skip silently.
    pub(crate) fn start_native_packages_refresh(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let Some(sequence) = self.native_packages_service.begin(None) else {
            return;
        };
        // The stat is `current_exe`-sibling metadata (cheap); the status.toml
        // parse stays on the worker.
        let available = crate::co_located_atpkg().is_some();
        let spawn = std::thread::Builder::new()
            .name("aterm-packages-status".into())
            .spawn(move || {
                let report = crate::packages_screen::collect_packages_status(available);
                let _ = proxy.send_event(Wake::NativePackagesFinished { sequence, report });
            });
        if spawn.is_err() {
            // Release the reservation via abort (never a fabricated report):
            // a never-observed surface honestly stays on "Reading package
            // status…", and a previously-observed one keeps its real facts.
            let _ = self.native_packages_service.abort(sequence);
        }
        self.publish_native_packages_state();
    }

    /// Main-thread half of the packages worker protocol (the packages analogue
    /// of [`Self::finish_native_update_check`]): stale sequences are inert.
    pub(crate) fn finish_native_packages(
        &mut self,
        sequence: u64,
        report: crate::packages_screen::PackagesStatusReport,
    ) {
        if !self.native_packages_service.finish(sequence, report) {
            return;
        }
        self.publish_native_packages_state();
    }

    /// Publish the shared packages projection to the Settings controller and
    /// fan the revision out to every Settings view (the packages analogue of
    /// [`Self::publish_native_update_state`]).
    pub(crate) fn publish_native_packages_state(&mut self) {
        let revision = self.native_packages_service.revision();
        let state = self.native_packages_service.state(
            self.config.packages_update_loop_enabled(),
            self.config.packages_auto_update(),
            self.config.packages_auto_install(),
        );
        if !self
            .native_runtime
            .replace_settings_packages(state, revision)
        {
            return;
        }
        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in &views {
            let _ = self.native_runtime.dispatch(
                *instance,
                *view,
                AppEvent::PackagesChanged { revision },
            );
        }
        let setting_views = views
            .iter()
            .map(|(_, view)| *view)
            .collect::<std::collections::BTreeSet<_>>();
        for ws in self.windows.values_mut() {
            let count = ws.tab_set.len();
            let mut shows_settings = false;
            for index in 0..count {
                let Some(tab) = ws.tab_set.tab_at_mut(index) else {
                    continue;
                };
                if tab
                    .root
                    .leaves()
                    .iter()
                    .any(|view| setting_views.contains(view))
                {
                    shows_settings = true;
                }
            }
            if shows_settings {
                ws.last_present = None;
                if let Some(window) = ws.os_window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }

    fn execute_recovery_request(
        &mut self,
        wid: WindowId,
        request: crate::native_app::RecoveryRequest,
    ) -> crate::native_app::RecoveryOutcome {
        use crate::native_app::{RecoveryCapability, RecoveryOutcome, RecoveryRequest};

        let open_document =
            |app: &mut Self, kind: crate::native_app::AppKind, uri: String| -> RecoveryOutcome {
                let safe = uri.len() <= 4_096
                    && uri.starts_with("file:///")
                    && !uri
                        .chars()
                        .any(|character| matches!(character, '\0' | '\r' | '\n'))
                    && matches!(
                        kind,
                        crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor
                    );
                if !safe {
                    return RecoveryOutcome::Denied {
                        message: "The retained document capability is invalid or unsafe"
                            .to_string(),
                    };
                }
                match app.open_document_tab_in_window(wid, kind, &uri) {
                    Ok(_) => RecoveryOutcome::Opened {
                        message: format!("Opened original in {}", kind.as_str()),
                    },
                    Err(message) => RecoveryOutcome::Failed { message },
                }
            };

        match request {
            RecoveryRequest::Retry(RecoveryCapability::Settings { route }) => {
                let Some(route) = crate::native_settings::SettingsRoute::from_path(&route) else {
                    return RecoveryOutcome::Denied {
                        message: "The retained Settings route is unavailable".to_string(),
                    };
                };
                if self.open_settings_tab(route) {
                    RecoveryOutcome::Opened {
                        message: format!("Reopened Settings · {}", route.label()),
                    }
                } else {
                    RecoveryOutcome::Failed {
                        message: "Settings could not be reopened".to_string(),
                    }
                }
            }
            RecoveryRequest::Retry(RecoveryCapability::Document { kind, uri }) => {
                open_document(self, kind, uri)
            }
            RecoveryRequest::OpenOriginal { uri } => {
                open_document(self, crate::native_app::AppKind::Editor, uri)
            }
        }
    }

    pub(crate) fn start_native_update_check(&mut self) -> UpdateOutcome {
        // Joining is pure with respect to physical work and does not require minting a
        // new event-loop capability. This fast path also makes the no-duplicate promise
        // directly testable in a headless host.
        if self.native_updater_service.snapshot().active.is_some() {
            return match self.native_updater_service.request_check() {
                CheckStart::Joined(_) => UpdateOutcome::Accepted,
                _ => UpdateOutcome::Failed {
                    message: "the active updater ticket could not be joined".to_string(),
                },
            };
        }
        let Some(proxy) = self.proxy.clone() else {
            return UpdateOutcome::Failed {
                message: "update checks require the event-loop service".to_string(),
            };
        };
        match self.native_updater_service.request_check() {
            CheckStart::Joined(_) => UpdateOutcome::Accepted,
            CheckStart::Rejected(block) => match block {
                CheckBlock::Disabled => UpdateOutcome::Failed {
                    message: "automatic updates are disabled on this build".to_string(),
                },
                CheckBlock::UpdateAlreadyStaged => UpdateOutcome::Blocked {
                    reasons: vec!["A verified update is already ready to install".to_string()],
                },
                CheckBlock::Applying => UpdateOutcome::Blocked {
                    reasons: vec!["An update is already being applied".to_string()],
                },
                CheckBlock::GenerationExhausted => UpdateOutcome::Failed {
                    message: "updater identity space is exhausted".to_string(),
                },
            },
            CheckStart::Start(ticket) => {
                self.publish_native_update_state();
                let (owner, repo) = self
                    .config
                    .update
                    .as_ref()
                    .map(|update| (update.owner.clone(), update.repo.clone()))
                    .unwrap_or((None, None));
                let snapshot = self.native_updater_service.snapshot();
                let build = snapshot.current_build;
                let version = snapshot.current_version.clone();
                let spawn = std::thread::Builder::new()
                    .name("aterm-native-update-check".into())
                    .spawn(move || {
                        let source =
                            aterm_update::Source::resolve(owner.as_deref(), repo.as_deref());
                        let status = aterm_update::check_now(build, &version, &source);
                        let _ = proxy.send_event(Wake::NativeUpdateFinished {
                            ticket,
                            status: durable_update_status(status),
                        });
                    });
                match spawn {
                    Ok(_) => UpdateOutcome::Accepted,
                    Err(error) => {
                        let message = format!("could not start updater worker: {error}");
                        self.finish_native_update_check(
                            ticket,
                            failed_update_status(build, message.clone()),
                        );
                        UpdateOutcome::Failed { message }
                    }
                }
            }
        }
    }

    /// Main-thread half of the updater worker protocol. Service-owned completion
    /// remains valid after every Settings view closes and publishes one revision
    /// to all Settings subscribers.
    pub(crate) fn finish_native_update_check(
        &mut self,
        ticket: UpdaterWorkTicket,
        status: DurableUpdateStatus,
    ) {
        let completion = self.native_updater_service.finish_check(ticket, status);
        if completion != CheckCompletion::Reduced {
            // An old worker cannot redraw, notify, arm intent, or drain a newer
            // deferred observation. Reducer-inert means presentation-inert too.
            return;
        }
        self.publish_native_update_state();
        #[cfg(test)]
        self.update_screen_refresh();

        // A later durable observation outranks the just-finished check. Reduce it
        // before arming/applying anything derived from this completion.
        self.finish_deferred_native_update_reconcile();
        if self.native_updater_service.snapshot().phase == UpdaterPhase::Applying {
            return;
        }

        if let Some((build, digest)) = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.dmg_sha256.clone()))
        {
            self.arm_native_auto_apply(build, &digest);
        }
        self.try_pending_native_auto_apply(true);
    }

    /// Retain automatic apply intent for one exact (or superseding) staged build.
    /// Returns true only when this call armed a new intent.
    pub(crate) fn arm_native_auto_apply(&mut self, build: u64, digest: &str) -> bool {
        use crate::native_update_auto_intent::{ArmDecision, ArmFacts};

        let enabled = crate::app_config::update_auto_apply(&self.config)
            && std::env::var_os("ATERM_DEBUG_RELAUNCH_NUDGE").is_none();
        let Some(dmg_sha256) = decode_dmg_sha256(digest) else {
            aterm_log::warn!(
                "refusing to arm automatic update build {build}: malformed DMG identity"
            );
            return false;
        };
        let armed = self.auto_apply_intent;
        let manual_only = self.auto_apply_manual_only;
        match crate::native_update_auto_intent::arm(ArmFacts {
            enabled,
            current_build: self.native_updater_service.snapshot().current_build,
            armed_build: armed.map(|intent| intent.build),
            armed_exact: armed
                .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256),
            manual_only_exact: manual_only
                .is_some_and(|manual| manual.build == build && manual.dmg_sha256 == dmg_sha256),
            manual_only_build: manual_only.map(|manual| manual.build),
            incoming_build: build,
        }) {
            ArmDecision::Clear => {
                self.auto_apply_intent = None;
                false
            }
            ArmDecision::SuppressManualOnly => {
                self.auto_apply_intent = None;
                false
            }
            ArmDecision::Keep => false,
            ArmDecision::Set(build) => {
                // A different artifact at the same build, or a newer build, owns a
                // distinct budget. Older wakes were suppressed by `arm` above.
                self.auto_apply_manual_only = None;
                self.auto_apply_intent = Some(crate::AutoApplyIntent {
                    build,
                    dmg_sha256,
                    retry_at: std::time::Instant::now() + crate::AUTOMATIC_UPDATE_QUIET_EPOCH,
                    attempts: 0,
                });
                true
            }
        }
    }

    /// Attempt a retained automatic apply from an exact event or timer wake.
    /// Every caller honors the same retained deadline and activity gate, so a
    /// duplicate stage wake cannot bypass backoff or spend another attempt.
    pub(crate) fn try_pending_native_auto_apply(&mut self, announce: bool) {
        use crate::native_update_auto_intent::{
            AttemptDisposition, AttemptResult, PollDecision, PollFacts, WaitReason,
        };

        let Some(mut intent) = self.auto_apply_intent else {
            return;
        };
        let now = std::time::Instant::now();
        let deadline_ready = now >= intent.retry_at;
        let work_active = self.native_updater_service.snapshot().active.is_some();
        let applying = self.native_updater_service.snapshot().phase == UpdaterPhase::Applying;
        // Durable facts are collected by workers and reduced by their exact wakes. A
        // timer callback is intentionally memory-only: it may wait for publication,
        // but it never reads/parses ledger files or launches PlistBuddy on the UI loop.
        if deadline_ready && !work_active && !applying {
            let Some(authoritative_intent) = self.auto_apply_intent else {
                // Asynchronous reconciliation retired or consumed the stage. Never
                // resurrect the copied intent into another wake loop.
                return;
            };
            intent = authoritative_intent;
        }
        // Durable reconciliation above is the one filesystem observation for this
        // retry wake. From here on, reduce the process-owned snapshot only; calling
        // `staged_update_ready()` would re-read the ledger on the latency-sensitive
        // event loop before immediately consulting the same reducer state.
        let updater = self.native_updater_service.snapshot();
        let current_build = updater.current_build;
        let staged_build = updater.staged.as_ref().map(|staged| staged.build);
        let staged_exact_target = updater.staged.as_ref().is_some_and(|staged| {
            staged.build == intent.build
                && decode_dmg_sha256(&staged.dmg_sha256) == Some(intent.dmg_sha256)
        });
        let staged_ready = updater.phase == UpdaterPhase::Staged
            && staged_build.is_some_and(|build| build > current_build);
        let decision = crate::native_update_auto_intent::poll(PollFacts {
            enabled: crate::app_config::update_auto_apply(&self.config)
                && std::env::var_os("ATERM_DEBUG_RELAUNCH_NUDGE").is_none(),
            deadline_ready,
            current_build,
            target_build: intent.build,
            work_active,
            applying,
            activity_quiet: self.automatic_update_activity_quiet(now),
            staged_ready,
            staged_build,
            staged_exact_target,
        });
        let attempt_build = match decision {
            PollDecision::Clear => {
                self.auto_apply_intent = None;
                return;
            }
            PollDecision::Wait(WaitReason::Deadline) => return,
            PollDecision::Wait(
                reason @ (WaitReason::WorkActive | WaitReason::Activity | WaitReason::StagePending),
            ) => {
                // Work/publication/activity are ordering facts, not physical
                // attempts. Retain exact intent through arbitrarily many bounded
                // active/drain transitions and consume zero retry budget.
                intent.retry_at = match reason {
                    WaitReason::Activity => {
                        crate::automatic_update_activity_retry_at(std::time::Instant::now())
                    }
                    WaitReason::WorkActive | WaitReason::StagePending => {
                        now + std::time::Duration::from_secs(2)
                    }
                    WaitReason::Deadline => unreachable!("matched above"),
                };
                self.auto_apply_intent = Some(intent);
                return;
            }
            PollDecision::Attempt { build } => build,
        };
        intent.build = attempt_build;
        self.auto_apply_intent = None;
        let outcome = self.apply_native_update(ApplyMode::Automatic);
        if let UpdateOutcome::Deferred { reason } = outcome {
            intent.retry_at = std::time::Instant::now() + crate::AUTOMATIC_UPDATE_QUIET_EPOCH;
            self.auto_apply_intent = Some(intent);
            aterm_log::debug!(
                "automatic update retained exact intent after activity deferral: {reason}"
            );
            return;
        }
        intent.attempts = intent.attempts.saturating_add(1);
        let disposition = crate::native_update_auto_intent::finish(match &outcome {
            UpdateOutcome::Accepted => AttemptResult::Accepted,
            UpdateOutcome::InstalledNeedsRelaunch { .. } => AttemptResult::InstalledNeedsRelaunch,
            UpdateOutcome::Blocked { .. } => AttemptResult::Blocked,
            UpdateOutcome::Failed { .. } => AttemptResult::Failed,
            UpdateOutcome::Deferred { .. } => unreachable!("returned above"),
        });
        match (disposition, outcome) {
            (AttemptDisposition::Complete, UpdateOutcome::Accepted) => {
                // A successful replacement never returns. Accepted here can only be a
                // joined in-flight request, whose owner now owns completion/recovery.
                aterm_log::info!(
                    "update auto-apply accepted for build {} after {} attempt(s)",
                    intent.build,
                    intent.attempts
                );
            }
            (
                AttemptDisposition::Complete,
                UpdateOutcome::InstalledNeedsRelaunch { build, message },
            ) => {
                self.auto_apply_intent = None;
                self.surface_update_apply_outcome(
                    "automatic",
                    UpdateOutcome::InstalledNeedsRelaunch { build, message },
                    false,
                );
            }
            (AttemptDisposition::Retry, UpdateOutcome::Blocked { reasons }) => {
                if let Some(delay) =
                    automatic_retry_delay(intent.attempts, AutomaticRetryKind::PreflightBlocked)
                {
                    intent.retry_at = std::time::Instant::now() + delay;
                    self.auto_apply_intent = Some(intent);
                } else {
                    self.auto_apply_intent = None;
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        build: intent.build,
                        dmg_sha256: intent.dmg_sha256,
                    });
                }
                if announce && intent.attempts == 1 {
                    self.surface_update_apply_outcome(
                        "automatic",
                        UpdateOutcome::Blocked {
                            reasons: reasons.clone(),
                        },
                        false,
                    );
                }
                if self.auto_apply_intent.is_some() {
                    aterm_log::info!(
                        "update auto-apply remains pending for build {}; bounded retry armed",
                        intent.build
                    );
                } else {
                    let message = format!(
                        "{} · automatic retries reached their safe cap; use the Update menu",
                        reasons.join(" · ")
                    );
                    aterm_log::warn!("{message}");
                    self.surface_nonmodal_update_status("↑ Update paused — manual retry");
                }
            }
            (AttemptDisposition::ManualOnly, UpdateOutcome::Failed { message }) => {
                // A returned physical handoff can park/read/checkpoint sessions. Never
                // repeat it from a timer: retain the manual menu affordance with zero
                // further wake cost.
                self.auto_apply_intent = None;
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: intent.build,
                    dmg_sha256: intent.dmg_sha256,
                });
                debug_assert_eq!(
                    automatic_retry_delay(intent.attempts, AutomaticRetryKind::PhysicalFailure),
                    None
                );
                self.surface_update_apply_outcome(
                    "automatic · manual retry required",
                    UpdateOutcome::Failed { message },
                    false,
                );
            }
            (_, outcome) => {
                // A future policy/outcome mismatch must fail safe, never panic in the
                // input loop or accidentally arm an unbounded physical retry.
                self.auto_apply_intent = None;
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: intent.build,
                    dmg_sha256: intent.dmg_sha256,
                });
                aterm_log::warn!(
                    "automatic update policy mismatch; clearing timer and requiring manual retry"
                );
                self.surface_update_apply_outcome("automatic policy fallback", outcome, false);
            }
        }
    }

    fn publish_native_update_state(&mut self) {
        let snapshot = self.native_updater_service.snapshot();
        let revision = snapshot.revision;
        let checking = matches!(
            snapshot.phase,
            UpdaterPhase::Checking | UpdaterPhase::Available | UpdaterPhase::Downloading
        );
        let attention = snapshot.attention_pending();
        debug_assert!(
            !snapshot.has_determinate_progress(),
            "the current updater API supplies no progress denominator"
        );
        let staged = snapshot
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.version.clone()));
        let update = self.update_snapshot(checking);
        self.native_runtime
            .replace_settings_update(update, revision);

        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in &views {
            let _ = self.native_runtime.dispatch(
                *instance,
                *view,
                AppEvent::UpdateChanged { revision },
            );
        }

        let setting_views = views
            .iter()
            .map(|(_, view)| *view)
            .collect::<std::collections::BTreeSet<_>>();
        for ws in self.windows.values_mut() {
            let count = ws.tab_set.len();
            for index in 0..count {
                let Some(tab) = ws.tab_set.tab_at_mut(index) else {
                    continue;
                };
                if tab
                    .root
                    .leaves()
                    .iter()
                    .any(|view| setting_views.contains(view))
                {
                    tab.presentation.indicators.busy = checking;
                    tab.presentation.indicators.attention = attention;
                }
            }
            ws.last_present = None;
        }

        self.relaunch = staged
            .map(|(build, version)| crate::relaunch_notice::RelaunchNotice { build, version });
        self.refresh_version_menu();
        self.palette_refresh_live();
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            self.refresh_window_tabs(wid);
        }
        self.request_redraw_all_windows();
    }

    pub(crate) fn reconcile_native_update_facts(
        &mut self,
        facts: NativeUpdateReconcileFacts,
    ) -> NativeUpdateFactsResult {
        if facts.observation_sequence <= self.last_native_update_reconcile_sequence {
            return NativeUpdateFactsResult::IgnoredStale;
        }
        if self.native_updater_service.snapshot().active.is_some()
            || self.native_updater_service.snapshot().phase == UpdaterPhase::Applying
        {
            return NativeUpdateFactsResult::Deferred(facts);
        }
        self.last_native_update_reconcile_sequence = facts.observation_sequence;
        let NativeUpdateReconcileFacts {
            _ticket: _,
            observation_sequence: _,
            durable,
            installed,
        } = facts;
        let build = self.native_updater_service.snapshot().current_build;
        let current = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .cloned();
        let observed_stage_floor = current.as_ref().map_or(0, |stage| stage.build);
        let durable_enabled = durable.as_ref().is_some_and(|status| status.enabled);
        let durable_build = durable.as_ref().and_then(|status| status.staged_build);
        let durable_commit = durable
            .as_ref()
            .and_then(|status| status.staged_commit.as_deref());
        let durable_digest = durable
            .as_ref()
            .and_then(|status| status.staged_dmg_sha256.as_deref());
        if current.is_some() {
            let disposition = self.native_updater_service.reconcile_durable_stage(
                durable_enabled,
                durable_build,
                durable_commit,
                durable_digest,
                installed.as_ref(),
            );
            if disposition != DurableStageDisposition::Unchanged {
                self.auto_apply_intent = None;
                self.publish_native_update_state();
                match disposition {
                    DurableStageDisposition::InstalledNeedsRelaunch { build } => {
                        let message = format!(
                            "Build {build} was installed by another aterm process; relaunch once to activate it"
                        );
                        aterm_log::warn!("update sync: {message}");
                    }
                    DurableStageDisposition::Retired => {
                        aterm_log::warn!(
                            "update sync: stale in-memory stage retired after durable marker changed"
                        );
                    }
                    DurableStageDisposition::Unchanged => {}
                }
            }
        }

        let effective = self.native_updater_service.snapshot().staged.as_ref();
        let exact_effective = effective.is_some_and(|staged| {
            crate::native_updater_service::durable_artifact_identity_matches(
                durable_build,
                durable_commit,
                durable_digest,
                staged.build,
                staged.commit.as_deref(),
                &staged.dmg_sha256,
            )
        });
        if exact_effective {
            return NativeUpdateFactsResult::Reduced {
                effective_stage: effective.cloned(),
            };
        }

        let installed_floor = installed
            .as_ref()
            .map_or(0, |installed| installed.build)
            .max(build)
            .max(observed_stage_floor);
        if let Some(mut durable) = durable {
            let eligible = durable.enabled
                && durable
                    .staged_build
                    .is_some_and(|staged| staged > installed_floor);
            if !eligible {
                durable.staged_build = None;
                durable.staged_version = None;
                durable.staged_commit = None;
                durable.staged_dmg_sha256 = None;
                durable.changelog = None;
            }
            if let CheckStart::Start(ticket) = self.native_updater_service.request_check() {
                let _ = self.native_updater_service.finish_check(ticket, durable);
                self.publish_native_update_state();
            }
        }
        NativeUpdateFactsResult::Reduced {
            effective_stage: self.native_updater_service.snapshot().staged.clone(),
        }
    }

    pub(crate) fn apply_native_update(&mut self, mode: ApplyMode) -> UpdateOutcome {
        if mode == ApplyMode::Automatic
            && !self.automatic_update_activity_quiet(std::time::Instant::now())
        {
            return UpdateOutcome::Deferred {
                reason: "terminal input/output is still inside the quiet epoch".to_string(),
            };
        }
        let start = self.native_updater_service.begin_apply_preflight(mode);
        let ticket = match start {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            ApplyPreflightStart::Joined(_) => return UpdateOutcome::Accepted,
            ApplyPreflightStart::Disabled => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["Automatic updates are disabled on this build".to_string()],
                };
            }
            ApplyPreflightStart::NotStaged => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["No newer verified update is staged".to_string()],
                };
            }
            ApplyPreflightStart::NotDeferred => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["Install when safe has not been requested".to_string()],
                };
            }
            ApplyPreflightStart::Applying => return UpdateOutcome::Accepted,
            ApplyPreflightStart::GenerationExhausted => {
                return UpdateOutcome::Failed {
                    message: "updater preflight identity space is exhausted".to_string(),
                };
            }
        };
        let (readiness, safety_token) = self.native_update_close_preflight();
        match self
            .native_updater_service
            .finish_apply_preflight(ticket, readiness)
        {
            ApplyDecision::Execute(command) => {
                let attempt = command.attempt();
                let Some(safety_token) = safety_token else {
                    return self.abort_unstarted_native_apply(
                        &attempt,
                        "native update preflight returned Ready without safety evidence"
                            .to_string(),
                    );
                };
                self.publish_native_update_state();
                let worker_attempt = attempt.clone();
                match command.execute(|| {
                    self.apply_staged_update_now(safety_token, mode, Some(worker_attempt))
                }) {
                    Ok(()) => UpdateOutcome::Accepted,
                    Err(error) if error.is_activity_deferred() => {
                        let message = error.into_message();
                        if self
                            .native_updater_service
                            .abort_apply(&attempt, message.clone())
                        {
                            self.publish_native_update_state();
                        }
                        UpdateOutcome::Deferred { reason: message }
                    }
                    // Submission failed before the worker could touch disk. Re-arm the
                    // exact authority directly; physical failures always return through
                    // `finish_async_native_update_handoff` with worker-collected facts.
                    Err(error) => self.abort_unstarted_native_apply(&attempt, error.into_message()),
                }
            }
            ApplyDecision::Blocked(reasons) => {
                self.publish_native_update_state();
                UpdateOutcome::Blocked { reasons }
            }
            ApplyDecision::Ignored => UpdateOutcome::Failed {
                message: "updater apply preflight became stale".to_string(),
            },
        }
    }

    /// Main-thread completion for any apply attempt that left this process alive,
    /// including the asynchronous overlap waiter. Disk is authoritative: the exact
    /// stage may be re-armed, a consumed/swapped stage becomes InstalledNeedsRelaunch,
    /// and a changed stage retires the old generation before importing the new one.
    ///
    /// `activity_revoked` is the completion path's TYPED classification (never a
    /// string match): true only when an AUTOMATIC attempt was revoked by user or
    /// terminal activity and rolled back losslessly. A re-armed stage then spends
    /// bounded [`AutomaticRetryKind::ActivityRevoked`] budget instead of latching
    /// manual-only.
    pub(crate) fn finish_async_native_update_handoff(
        &mut self,
        attempt: crate::native_updater_service::ApplyAttemptTicket,
        facts: NativeUpdateReconcileFacts,
        message: String,
        activity_revoked: bool,
    ) -> Option<UpdateOutcome> {
        self.reconcile_returned_native_apply_with_facts(attempt, facts, message, activity_revoked)
    }

    /// Consume one activity-revoked overlap retry cycle for this exact artifact
    /// and re-arm the automatic intent at its exponentially spaced deadline.
    /// `None` = the budget is exhausted (or the artifact identity is malformed);
    /// the caller then falls back to the manual-only latch. The cycle counter
    /// lives on `auto_overlap_retry`, keyed by (build, dmg) — duplicate or
    /// reordered completions for the same artifact can never mint fresh budget,
    /// and a different artifact starts a fresh budget by construction.
    fn arm_activity_revoked_overlap_retry(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<std::time::Duration> {
        let dmg_sha256 = decode_dmg_sha256(attempt.target_dmg_sha256())?;
        let build = attempt.target_build();
        let cycles = self
            .auto_overlap_retry
            .filter(|retry| retry.build == build && retry.dmg_sha256 == dmg_sha256)
            .map_or(0, |retry| retry.cycles);
        let delay = automatic_retry_delay(cycles, AutomaticRetryKind::ActivityRevoked)?;
        self.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256,
            cycles: cycles.saturating_add(1),
        });
        self.auto_apply_manual_only = None;
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at: std::time::Instant::now() + delay,
            attempts: cycles,
        });
        Some(delay)
    }

    fn abort_unstarted_native_apply(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        message: String,
    ) -> UpdateOutcome {
        if self
            .native_updater_service
            .abort_apply(attempt, message.clone())
        {
            self.publish_native_update_state();
        }
        UpdateOutcome::Failed { message }
    }

    /// A handoff worker has already killed/reaped its child, but ordered disk
    /// reconciliation may still be queued. Re-arm the exact in-memory authority
    /// immediately so the UI never remains Applying while readers resume; the
    /// later generic reconcile wake retires or imports disk authority.
    pub(crate) fn abort_reaped_native_apply_before_reconcile(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        message: String,
    ) -> UpdateOutcome {
        if self
            .native_updater_service
            .abort_apply(attempt, message.clone())
        {
            if let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256()) {
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: attempt.target_build(),
                    dmg_sha256,
                });
            }
            self.auto_apply_intent = None;
            self.publish_native_update_state();
        }
        UpdateOutcome::Failed { message }
    }

    fn reconcile_returned_native_apply_with_facts(
        &mut self,
        attempt: crate::native_updater_service::ApplyAttemptTicket,
        facts: NativeUpdateReconcileFacts,
        message: String,
        activity_revoked: bool,
    ) -> Option<UpdateOutcome> {
        let durable_enabled = facts
            .durable
            .as_ref()
            .is_some_and(|durable| durable.enabled);
        let durable_staged_build = facts
            .durable
            .as_ref()
            .and_then(|durable| durable.staged_build);
        let durable_staged_commit = facts
            .durable
            .as_ref()
            .and_then(|durable| durable.staged_commit.as_deref());
        let durable_staged_digest = facts
            .durable
            .as_ref()
            .and_then(|durable| durable.staged_dmg_sha256.as_deref());
        let disposition = self.native_updater_service.finish_returned_apply(
            &attempt,
            ReturnedApplyFacts::new(
                durable_enabled,
                durable_staged_build,
                durable_staged_commit,
                durable_staged_digest,
                facts.installed.as_ref(),
            ),
            message.clone(),
        );
        match disposition {
            ReturnedApplyDisposition::Rearmed => {
                // ACTIVITY-REVOKED + budget remaining: the exact stage was
                // re-armed on disk and the rollback was lossless, so schedule
                // one bounded quiet-window re-attempt instead of latching
                // manual-only. Exhausted budget (or a genuine failure, where
                // `activity_revoked` is false) takes the sticky manual latch.
                if activity_revoked
                    && let Some(delay) = self.arm_activity_revoked_overlap_retry(&attempt)
                {
                    self.publish_native_update_state();
                    let _ = self.reconcile_native_update_facts(facts);
                    self.finish_deferred_native_update_reconcile();
                    aterm_log::info!(
                        "update apply: activity revoked the overlap; automatic retry in {:?}",
                        delay
                    );
                    return Some(UpdateOutcome::Deferred { reason: message });
                }
                if let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256()) {
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        build: attempt.target_build(),
                        dmg_sha256,
                    });
                }
                self.auto_apply_intent = None;
                self.publish_native_update_state();
                let _ = self.reconcile_native_update_facts(facts);
                self.finish_deferred_native_update_reconcile();
                Some(UpdateOutcome::Failed { message })
            }
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build } => {
                self.auto_apply_intent = None;
                self.auto_apply_manual_only = None;
                self.publish_native_update_state();
                // A newer artifact is imported only when it exceeds the canonical
                // installed build; the fact reducer enforces that floor.
                let _ = self.reconcile_native_update_facts(facts);
                self.finish_deferred_native_update_reconcile();
                Some(UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: "The update is already on disk; relaunch aterm once to activate it"
                        .to_string(),
                })
            }
            ReturnedApplyDisposition::Retired => {
                self.auto_apply_intent = None;
                // Retiring old authority must not clear a sticky latch for a newer
                // artifact that may already have completed out of order.
                if self.auto_apply_manual_only.is_some_and(|manual| {
                    manual.build == attempt.target_build()
                        && decode_dmg_sha256(attempt.target_dmg_sha256()) == Some(manual.dmg_sha256)
                }) {
                    self.auto_apply_manual_only = None;
                }
                self.publish_native_update_state();
                let _ = self.reconcile_native_update_facts(facts);
                self.finish_deferred_native_update_reconcile();
                Some(UpdateOutcome::Failed {
                    message: format!(
                        "{message}; the durable stage changed and the old apply intent was retired"
                    ),
                })
            }
            // A delayed callback for attempt A must not perturb attempt B's UI. The
            // service is reducer-inert and this `None` forbids logs, alerts, redraws,
            // or misleading "remains ready" text at the caller.
            ReturnedApplyDisposition::Ignored => None,
        }
    }

    fn native_update_close_preflight(&self) -> (ClosePreflight, Option<NativeUpdateSafetyToken>) {
        let documents = self
            .view_store
            .iter()
            .filter_map(|(_, link)| match link {
                crate::tab_model::View::Native(native) => {
                    self.native_runtime.document_id(native.instance)
                }
                crate::tab_model::View::Terminal(_) => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let dirty_documents = documents
            .iter()
            .filter(|document| self.document_store.dirty(**document) == Some(true))
            .count();
        let failed_checkpoints = documents
            .iter()
            .filter(|document| {
                matches!(
                    self.document_store.phase(**document),
                    Some(crate::document_store::DocumentPhase::Blocked { .. })
                )
            })
            .count();
        let pending_checkpoints = documents
            .iter()
            .filter(|document| {
                matches!(
                    self.document_store.phase(**document),
                    Some(crate::document_store::DocumentPhase::Closing { .. })
                )
            })
            .count();

        let mut blockers = Vec::new();
        if dirty_documents > 0 {
            blockers.push(format!(
                "Checkpoint Drafts: {dirty_documents} document(s) have uncheckpointed edits"
            ));
        }
        if failed_checkpoints > 0 {
            blockers.push(format!(
                "Retry: {failed_checkpoints} document checkpoint(s) previously failed"
            ));
        }
        if pending_checkpoints > 0 {
            blockers.push(format!(
                "Wait: {pending_checkpoints} document checkpoint(s) are still running"
            ));
        }
        if self.pending_restore.is_some() || !self.seamless_adopt.is_empty() {
            blockers.push("Wait for session restore to finish before relaunching".to_string());
        }
        if blockers.is_empty() {
            (
                ClosePreflight::Ready,
                Some(NativeUpdateSafetyToken { _private: () }),
            )
        } else {
            (ClosePreflight::Blocked(blockers), None)
        }
    }

    /// Re-run native document/restore safety at asynchronous handoff completion.
    /// The original token authorizes preparation only; user edits can occur while the
    /// child boots, so the outgoing process must obtain fresh evidence before exit.
    pub(crate) fn revalidate_native_update_safety(
        &self,
    ) -> Result<NativeUpdateSafetyToken, Vec<String>> {
        match self.native_update_close_preflight() {
            (ClosePreflight::Ready, Some(token)) => Ok(token),
            (ClosePreflight::Blocked(reasons), None) => Err(reasons),
            _ => Err(vec![
                "Native update safety preflight returned inconsistent evidence".to_string(),
            ]),
        }
    }

    /// QA same-binary reexec through the exact production native-state preflight.
    /// This keeps `ATERM_DEBUG_SEAMLESS_REEXEC` useful without granting it a bypass
    /// around dirty documents, checkpoint work, or restore/adoption state.
    pub(crate) fn apply_debug_seamless_update(&mut self) -> UpdateOutcome {
        let (readiness, safety_token) = self.native_update_close_preflight();
        match readiness {
            ClosePreflight::Blocked(reasons) => UpdateOutcome::Blocked { reasons },
            ClosePreflight::Ready => {
                let Some(token) = safety_token else {
                    return UpdateOutcome::Failed {
                        message: "debug update preflight returned Ready without safety evidence"
                            .to_string(),
                    };
                };
                match self.apply_staged_update_now(token, ApplyMode::Immediate, None) {
                    Ok(()) => UpdateOutcome::Accepted,
                    Err(error) => UpdateOutcome::Failed {
                        message: error.into_message(),
                    },
                }
            }
        }
    }

    /// Clean-quit hook for an update previously deferred with Install When Safe.
    /// Returns true only when an asynchronous overlap child now owns the quit:
    /// callers must defer structural teardown/`el.exit()` until its completion
    /// wake succeeds or rolls back. A synchronous exec does not return on success;
    /// a returned failure leaves no pending handoff and lets quitting continue.
    pub(crate) fn apply_deferred_native_update_on_clean_quit(&mut self) -> bool {
        if !self.native_updater_service.snapshot().install_on_clean_quit {
            return false;
        }
        let accepted = matches!(
            self.apply_native_update(ApplyMode::CleanQuit),
            UpdateOutcome::Accepted
        );
        accepted && self.pending_update_handoff.is_some()
    }

    /// Viewing the exact published update revision quiets its one announcement
    /// without hiding the staged artifact or inventing a second notification state.
    pub(crate) fn acknowledge_native_update_attention(&mut self) {
        let Some(revision) = self.native_updater_service.snapshot().attention_revision else {
            return;
        };
        if self.native_updater_service.acknowledge_attention(revision) {
            self.publish_native_update_state();
        }
    }

    pub(crate) fn refresh_native_presentation(
        &mut self,
        wid: WindowId,
        _instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
    ) {
        let tab = self.windows.get(&wid).and_then(|window| {
            window
                .tab_set
                .tabs()
                .iter()
                .find(|tab| tab.root.leaves().contains(&view))
                .map(|tab| tab.id)
        });
        if let Some(tab) = tab {
            self.refresh_aggregate_tab_presentation(wid, tab);
        } else {
            self.refresh_active_split_presentation(wid);
        }
        self.refresh_window_tabs(wid);
    }

    /// Fold presentation across every leaf of the active split. Focus supplies
    /// title/icon while dirty, busy and attention remain independently visible
    /// if any sibling owns them.
    pub(crate) fn refresh_active_split_presentation(&mut self, wid: WindowId) {
        let Some((tab_id, focused, leaves)) = self.windows.get(&wid).and_then(|window| {
            let tab = window.tab_set.active()?;
            Some((tab.id, tab.focus, tab.root.leaves()))
        }) else {
            return;
        };
        let mut presentations = Vec::with_capacity(leaves.len());
        for view in leaves {
            let Some(linked) = self.view_store.get(view).copied() else {
                continue;
            };
            let presentation = match linked {
                crate::tab_model::View::Terminal(terminal) => {
                    let title = self
                        .pool
                        .get(terminal.session)
                        .map(|session| crate::term_lock(&session.term).title().to_string())
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "aterm".to_string());
                    crate::tab_model::TabPresentation::terminal(title)
                }
                crate::tab_model::View::Native(native) => {
                    let Ok(presentation) = self.native_runtime.presentation(native.instance, view)
                    else {
                        continue;
                    };
                    crate::tab_model::TabPresentation {
                        title: presentation.title,
                        icon: Some(match presentation.icon {
                            crate::native_app::AppIcon::Settings => {
                                crate::tab_model::TabIconKind::Settings
                            }
                            crate::native_app::AppIcon::Markdown => {
                                crate::tab_model::TabIconKind::Markdown
                            }
                            crate::native_app::AppIcon::Editor => {
                                crate::tab_model::TabIconKind::Editor
                            }
                            crate::native_app::AppIcon::Recovery => {
                                crate::tab_model::TabIconKind::Recovery
                            }
                        }),
                        indicators: crate::tab_model::TabIndicators {
                            dirty: presentation.indicators.dirty,
                            busy: presentation.indicators.busy,
                            attention: presentation.indicators.attention,
                        },
                        closable: presentation.closable,
                        tooltip: presentation.tooltip,
                    }
                }
            };
            presentations.push((view, presentation));
        }
        let Some(presentation) = crate::tab_model::aggregate_presentations(focused, presentations)
        else {
            return;
        };
        if let Some(window) = self.windows.get_mut(&wid)
            && let Some(index) = window
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)
            && let Some(tab) = window.tab_set.tab_at_mut(index)
        {
            tab.presentation = presentation;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sole_worker_stamps_fifo_read_order_not_request_order() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(4);
        let worker = std::thread::spawn(move || {
            run_native_update_worker(
                receiver,
                |ticket, observation_sequence, _| NativeUpdateReconcileFacts {
                    _ticket: ticket,
                    observation_sequence,
                    durable: None,
                    installed: None,
                },
                |_| {},
                || drained_tx.send(()).unwrap(),
            );
        });
        let enqueue = |request_sequence| {
            let (reply, result) = std::sync::mpsc::sync_channel(1);
            sender
                .send(NativeUpdateWorkerRequest::Reconcile(
                    NativeUpdateReconcileRequest {
                        ticket: NativeUpdateReconcileTicket { request_sequence },
                        current_build: 10,
                        destination: NativeUpdateFactDestination::Reply(reply),
                    },
                ))
                .unwrap();
            result
        };

        // A later-minted request can reach the shared FIFO first (handoff and UI
        // producers race). Observation identity follows the actual read order.
        let later_request = enqueue(2);
        let earlier_request = enqueue(1);
        let later_facts = later_request.recv().unwrap();
        let earlier_facts = earlier_request.recv().unwrap();
        assert_eq!(later_facts._ticket.request_sequence(), 2);
        assert_eq!(later_facts.observation_sequence, 1);
        assert_eq!(earlier_facts._ticket.request_sequence(), 1);
        assert_eq!(earlier_facts.observation_sequence, 2);
        drained_rx.recv().unwrap();
        drained_rx.recv().unwrap();
        drop(sender);
        worker.join().unwrap();
    }

    #[test]
    fn durable_marker_is_observed_before_installed_bundle_identity() {
        let step = std::cell::Cell::new(0_u8);
        let facts = read_native_update_reconcile_facts_with(
            NativeUpdateReconcileTicket {
                request_sequence: 1,
            },
            1,
            || {
                assert_eq!(step.replace(1), 0);
                None
            },
            || {
                assert_eq!(step.replace(2), 1, "installed probe ran before ready read");
                None
            },
        );
        assert_eq!(step.get(), 2);
        assert!(facts.durable.is_none() && facts.installed.is_none());

        // Negative control: the seam assertion is not vacuous and catches reversal.
        let reversed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let step = std::cell::Cell::new(0_u8);
            let installed_first = || assert_eq!(step.replace(2), 1);
            installed_first();
        }));
        assert!(reversed.is_err());
    }

    #[test]
    fn updater_ui_snapshots_do_not_probe_ledgers_or_installed_bundle() {
        let app = App::headless_for_test();
        let before = UPDATE_FACT_PROBES_ON_THREAD.with(std::cell::Cell::get);
        let _ = app.update_snapshot(false).projection();
        let after = UPDATE_FACT_PROBES_ON_THREAD.with(std::cell::Cell::get);
        assert_eq!(before, after, "UI projection invoked an updater disk probe");
    }

    #[test]
    fn saturated_boot_health_queue_retains_nonblocking_retry() {
        let mut app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        app.native_update_reconcile_worker = Some(sender);

        assert_eq!(
            app.request_native_boot_health_confirmation(),
            NativeUpdateDispatch::Saturated,
            "a full queue must retain the pending latch"
        );
        let _ = receiver.recv().unwrap();
        assert_eq!(
            app.request_native_boot_health_confirmation(),
            NativeUpdateDispatch::Queued
        );
        assert!(matches!(
            receiver.recv().unwrap(),
            NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 10 }
        ));
    }

    #[test]
    fn recovery_host_revalidates_capabilities_and_never_interprets_diagnostics() {
        let mut app = App::headless_for_test();
        let denied = app.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::OpenOriginal {
                uri: "file:///tmp/safe.md\nhttps://attacker.example".to_string(),
            },
        );
        assert!(matches!(
            denied,
            crate::native_app::RecoveryOutcome::Denied { .. }
        ));

        let invalid_route = app.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::Retry(
                crate::native_app::RecoveryCapability::Settings {
                    route: "../../diagnostics-from-metadata".to_string(),
                },
            ),
        );
        assert!(matches!(
            invalid_route,
            crate::native_app::RecoveryOutcome::Denied { .. }
        ));

        let settings = app.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::Retry(
                crate::native_app::RecoveryCapability::Settings {
                    route: "/about".to_string(),
                },
            ),
        );
        assert!(matches!(
            settings,
            crate::native_app::RecoveryOutcome::Opened { .. }
        ));
    }

    #[test]
    fn disconnected_reconcile_worker_drops_retry_latch_without_hot_loop() {
        let mut app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        drop(receiver);
        app.native_update_reconcile_worker = Some(sender);
        app.pending_native_update_reconcile_purpose =
            Some(NativeUpdateReconcilePurpose::StageAvailable);
        let destinations = std::cell::Cell::new(0_u32);
        for _ in 0..1_000 {
            let (reply, _result) = std::sync::mpsc::sync_channel(1);
            let _ = app.retry_pending_native_update_reconcile_with(|_| {
                destinations.set(destinations.get() + 1);
                NativeUpdateFactDestination::Reply(reply)
            });
        }
        assert_eq!(
            destinations.get(),
            1,
            "a disconnected worker gets one restart/failure attempt, not one poll per event turn"
        );
        assert!(app.pending_native_update_reconcile_purpose.is_none());
    }

    #[test]
    fn idle_reconcile_park_never_materializes_a_destination_or_warning_path() {
        let mut app = App::headless_for_test();
        assert!(!app.has_pending_native_update_reconcile());

        // This is the shipping wrapper with only proxy materialization injected:
        // a future move of the clone above the guard increments the counter and
        // fails the test (the previous `_with`-only test could not see that bug).
        let materializations = std::cell::Cell::new(0_u32);
        for _ in 0..10_000 {
            app.retry_pending_native_update_reconcile_via(|_| {
                materializations.set(materializations.get() + 1);
                None
            });
        }
        assert_eq!(
            materializations.get(),
            0,
            "idle park materialized/woke an EventLoopProxy"
        );

        // Tier-1 projection: the genuine idle guard and retry seam realize the
        // model's ParkIdle transition, whose observable proxy-wake and warning
        // counters remain exactly zero. The mutant makes both one, proving this
        // assertion catches the historical unconditional-clone regression.
        let model = aterm_spec::derive::native_update_worker_queue_model();
        let mut idle = model.init_state();
        assert!(model.fire("ParkIdle", &mut idle));
        assert_eq!(idle.get("idle_proxy_wakes"), Some(&0));
        assert_eq!(idle.get("idle_warnings"), Some(&0));
        assert!(model.check_invariant("IdleParkHasNoProxyWake", &idle));
        assert!(model.check_invariant("IdleParkHasNoWarning", &idle));

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let polluted = buggy.successors("ParkIdle", &buggy.init_state())[0].clone();
        assert!(!buggy.check_invariant("IdleParkHasNoProxyWake", &polluted));
        assert!(!buggy.check_invariant("IdleParkHasNoWarning", &polluted));
    }

    /// Tier-1 binding for `NativeUpdateWorkerQueue`: consume the actual shipping
    /// capacity constant while driving the genuine `try_send` Full/drain/retry
    /// decisions, purpose coalescer, and disconnected restart path. The model
    /// specifies that externally-visible boundary, not worker scheduling timing.
    #[test]
    fn native_update_worker_queue_conforms_to_saturation_coalescing_and_disconnect() {
        let model = aterm_spec::derive::native_update_worker_queue_model();
        let mut state = model.init_state();

        let mut app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(NATIVE_UPDATE_WORKER_CAPACITY);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        app.native_update_reconcile_worker = Some(sender);
        assert!(model.fire("OccupyWorker", &mut state));

        let (stage_reply, _stage_result) = std::sync::mpsc::sync_channel(1);
        assert!(app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::StageAvailable,
            |_| NativeUpdateFactDestination::Reply(stage_reply),
        ));
        assert!(model.fire("RequestStageFull", &mut state));
        assert_eq!(
            app.pending_native_update_reconcile_purpose,
            Some(NativeUpdateReconcilePurpose::StageAvailable)
        );
        assert_eq!(state.get("pending"), Some(&1));

        // A stronger intent arriving during the same saturation episode must
        // replace only the purpose, never allocate or lose a second request.
        let (apply_reply, _apply_result) = std::sync::mpsc::sync_channel(1);
        assert!(app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::ApplyControl,
            |_| NativeUpdateFactDestination::Reply(apply_reply),
        ));
        assert!(model.fire("UpgradePendingToApply", &mut state));
        assert_eq!(
            app.pending_native_update_reconcile_purpose,
            Some(NativeUpdateReconcilePurpose::ApplyControl)
        );
        assert_eq!(state.get("purpose"), Some(&2));

        let _ = receiver.recv().unwrap();
        assert!(model.fire("WorkerDrainsFiller", &mut state));
        let observed = std::cell::Cell::new(None);
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            app.retry_pending_native_update_reconcile_with(|purpose| {
                observed.set(Some(purpose));
                NativeUpdateFactDestination::Reply(reply)
            }),
            NativeUpdateDispatch::Queued
        );
        assert!(model.fire("RetryPendingOnDrain", &mut state));
        assert!(app.pending_native_update_reconcile_purpose.is_none());
        assert_eq!(
            observed.get(),
            Some(NativeUpdateReconcilePurpose::ApplyControl),
            "the real coalescer must preserve the strongest accepted purpose"
        );

        let NativeUpdateWorkerRequest::Reconcile(request) = receiver.recv().unwrap() else {
            panic!("the retained intent must become one real worker request");
        };
        assert!(model.fire("WorkerCompletesIntent", &mut state));
        let NativeUpdateFactDestination::Reply(reply) = request.destination else {
            panic!("the conformance request uses a reply destination");
        };
        reply
            .send(reconcile_facts(request.ticket.request_sequence(), 1, None))
            .unwrap();
        app.finish_native_update_reconcile(observed.get().unwrap(), result.recv().unwrap());
        assert!(model.fire("ReduceCompletion", &mut state));
        assert_eq!(state.get("delivered"), Some(&1));
        assert!(model.check_invariant("NoSilentlyLostAcceptedIntent", &state));

        // Dispatch-time disconnection gets exactly one restart attempt. A
        // headless App has no event-loop proxy with which to spawn that worker,
        // so the real path deterministically reports Unavailable and clears the
        // latch; repeated event turns cannot become a retry hot loop.
        let mut disconnected_app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        disconnected_app.native_update_reconcile_worker = Some(sender);
        let mut disconnected_state = model.init_state();
        assert!(model.fire("OccupyWorker", &mut disconnected_state));
        let (discard_reply, _discard_result) = std::sync::mpsc::sync_channel(1);
        assert!(disconnected_app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::StageAvailable,
            |_| NativeUpdateFactDestination::Reply(discard_reply),
        ));
        assert!(model.fire("RequestStageFull", &mut disconnected_state));
        drop(receiver);
        assert!(model.fire("DisconnectWithPending", &mut disconnected_state));

        let destinations = std::cell::Cell::new(0_u32);
        let (unavailable_reply, _unavailable_result) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            disconnected_app.retry_pending_native_update_reconcile_with(|_| {
                destinations.set(destinations.get() + 1);
                NativeUpdateFactDestination::Reply(unavailable_reply)
            }),
            NativeUpdateDispatch::Unavailable
        );
        assert!(model.fire("RestartPendingUnavailable", &mut disconnected_state));
        for _ in 0..32 {
            let (reply, _result) = std::sync::mpsc::sync_channel(1);
            assert_eq!(
                disconnected_app.retry_pending_native_update_reconcile_with(|_| {
                    destinations.set(destinations.get() + 1);
                    NativeUpdateFactDestination::Reply(reply)
                }),
                NativeUpdateDispatch::Queued
            );
        }
        assert_eq!(destinations.get(), 1);
        assert!(
            disconnected_app
                .pending_native_update_reconcile_purpose
                .is_none()
        );
        assert_eq!(disconnected_state.get("failed_explicitly"), Some(&1));
        assert_eq!(disconnected_state.get("restarts"), Some(&1));

        // Negative control: the historical "Full means success but retain
        // nothing" projection is rejected by the same invariant.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let occupied = buggy.successors("OccupyWorker", &buggy.init_state())[0].clone();
        let lost = buggy.successors("RequestApplyFull", &occupied)[0].clone();
        assert!(!buggy.check_invariant("NoSilentlyLostAcceptedIntent", &lost));
    }

    #[test]
    fn boot_health_failure_rearms_bounded_retry_then_success_closes_latch() {
        let mut app = App::headless_for_test();
        let now = std::time::Instant::now();
        app.boot_health_confirmation_dispatched = true;
        app.finish_native_boot_health_confirmation(false, now);
        assert!(!app.boot_health_confirmation_dispatched);
        assert_eq!(
            app.boot_health_confirmation_retry_at,
            Some(now + std::time::Duration::from_secs(1))
        );

        app.finish_native_boot_health_confirmation(true, now);
        assert!(app.boot_health_confirmation_dispatched);
        assert!(app.boot_health_confirmation_retry_at.is_none());
    }

    #[test]
    fn saturated_stage_wake_drains_and_schedules_automatic_apply() {
        let mut app = App::headless_for_test();
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(true),
            ..crate::app_config::UpdateConfig::default()
        });
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        app.native_update_reconcile_worker = Some(sender);

        let (discard_reply, _discard_result) = std::sync::mpsc::sync_channel(1);
        assert!(app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::StageAvailable,
            |_| NativeUpdateFactDestination::Reply(discard_reply),
        ));
        assert_eq!(
            app.pending_native_update_reconcile_purpose,
            Some(NativeUpdateReconcilePurpose::StageAvailable),
            "a Full FIFO accepts the wake into the coalesced latch"
        );

        let _ = receiver.recv().unwrap();
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed_for_destination = std::sync::Arc::clone(&observed);
        assert_eq!(
            app.retry_pending_native_update_reconcile_with(move |purpose| {
                *observed_for_destination.lock().unwrap() = Some(purpose);
                NativeUpdateFactDestination::Reply(reply)
            }),
            NativeUpdateDispatch::Queued
        );
        assert!(app.pending_native_update_reconcile_purpose.is_none());

        let NativeUpdateWorkerRequest::Reconcile(request) = receiver.recv().unwrap() else {
            panic!("pending stage retry must enqueue a facts read");
        };
        let facts = reconcile_facts(
            request.ticket.request_sequence(),
            1,
            Some(status(Some(11), 0)),
        );
        let NativeUpdateFactDestination::Reply(reply) = request.destination else {
            panic!("test retry uses reply destination");
        };
        reply.send(facts).unwrap();
        let purpose = observed.lock().unwrap().expect("effective purpose");
        app.finish_native_update_reconcile(purpose, result.recv().unwrap());

        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build),
            Some(11)
        );
        let intent = app
            .auto_apply_intent
            .expect("drained StageAvailable wake retains exact automatic intent");
        assert_eq!(intent.build, 11);
        assert_eq!(intent.attempts, 0);
        assert_eq!(
            crate::fold_auto_apply_deadline(Some(intent), None),
            Some(intent.retry_at),
            "the retained intent must arm the event-loop wake without spending budget early"
        );
    }

    #[test]
    fn regional_damage_rejects_theme_or_font_paint_revision_changes() {
        let previous = NativeUiCompileStamp {
            instance: crate::tab_model::AppInstanceId::from_stored(1),
            view: crate::tab_model::ViewId::from_stored(2),
            generation: 7,
            geometry: 11,
            config_revision: 13,
            update_revision: 17,
            document_seq: None,
            presentation_revision: 19,
            paint_revision: 23,
        };
        let mut current = previous;
        current.generation += 1;
        assert!(current.accepts_regional_damage_from(previous));
        current.paint_revision += 1;
        assert!(
            !current.accepts_regional_damage_from(previous),
            "paint input changes promote reducer-local damage to a full raster"
        );
    }

    #[test]
    fn native_appearance_inputs_have_distinct_cache_revisions() {
        let base = crate::native_appearance::AppearancePreferences::default();
        let revision = native_appearance_revision(base);
        for preferences in [
            crate::native_appearance::AppearancePreferences {
                high_contrast: true,
                ..base
            },
            crate::native_appearance::AppearancePreferences {
                reduced_transparency: true,
                ..base
            },
            crate::native_appearance::AppearancePreferences {
                text_scale: 1.25,
                ..base
            },
        ] {
            assert_ne!(native_appearance_revision(preferences), revision);
        }
    }

    #[test]
    fn native_motion_inputs_have_distinct_cache_revisions() {
        let base = crate::native_app::ViewMotionCx::default();
        let revision = native_motion_revision(base);
        for motion in [
            crate::native_app::ViewMotionCx {
                system_reduced: true,
                ..base
            },
            crate::native_app::ViewMotionCx {
                focused: false,
                ..base
            },
            crate::native_app::ViewMotionCx {
                performance_reduced: true,
                ..base
            },
            crate::native_app::ViewMotionCx {
                serious: true,
                ..base
            },
        ] {
            assert_ne!(native_motion_revision(motion), revision);
        }
    }

    #[test]
    fn live_motion_context_changes_the_native_compile_stamp() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        app.windows.get_mut(&wid).unwrap().focused = true;

        let base = app.native_ui_compile_stamp(wid).unwrap();
        app.system_reduce_motion = true;
        let system_reduced = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(system_reduced.paint_revision, base.paint_revision);

        app.system_reduce_motion = false;
        app.windows.get_mut(&wid).unwrap().focused = false;
        let unfocused = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(unfocused.paint_revision, base.paint_revision);

        app.windows.get_mut(&wid).unwrap().focused = true;
        app.perf_reduced = true;
        let performance_reduced = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(performance_reduced.paint_revision, base.paint_revision);

        app.perf_reduced = false;
        app.set_serious_mode(true);
        let serious = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(serious.paint_revision, base.paint_revision);
    }

    #[test]
    fn native_compile_uses_each_window_font_for_stamp_and_preview() {
        let mut app = App::headless_for_test();
        let first = WindowId(0);
        app.windows.get_mut(&first).unwrap().metrics.font_px = 12.0;
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let (first_instance, first_view) = app.active_native_view(first).unwrap();
        let first_viewport = app.native_ui_viewport(first).unwrap();

        let next_session = app.next_session_id;
        let (rows, cols) = (app.windows[&first].rows, app.windows[&first].cols);
        let second = app.insert_logical_window(crate::stub_session(next_session), rows, cols);
        {
            let window = app.windows.get_mut(&second).unwrap();
            window.metrics.font_px = 24.0;
            window.scale = 2.0;
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let (second_instance, second_view) = app.active_native_view(second).unwrap();
        let second_viewport = app.native_ui_viewport(second).unwrap();
        assert_eq!(
            second_instance, first_instance,
            "the Settings controller remains process-singleton"
        );

        let first_stamp = app
            .native_ui_compile_stamp_for(first, first_instance, first_view, first_viewport)
            .unwrap();
        let second_stamp = app
            .native_ui_compile_stamp_for(second, second_instance, second_view, second_viewport)
            .unwrap();
        assert_ne!(
            first_stamp.paint_revision, second_stamp.paint_revision,
            "window-local font size is a native paint input"
        );

        let preview_value = |compiled: &crate::native_ui::CompiledUi| {
            compiled
                .semantics
                .iter()
                .find(|node| node.label == "Typography preview")
                .and_then(|node| match &node.value {
                    crate::native_ui::SemanticValue::Text(value) => Some(value.clone()),
                    _ => None,
                })
                .expect("Typography renderer preview semantics")
        };
        let first_compiled = app
            .compiled_native_ui_for(first, first_instance, first_view, first_viewport)
            .unwrap();
        let second_compiled = app
            .compiled_native_ui_for(second, second_instance, second_view, second_viewport)
            .unwrap();
        assert!(preview_value(&first_compiled).contains("12 px monospace"));
        assert!(preview_value(&second_compiled).contains("24 px monospace"));

        // Moving the process-global renderer activation must not perturb either
        // window's native stamp; their MetricsView records remain authoritative.
        app.font_px = 31.0;
        assert_eq!(
            app.native_ui_compile_stamp_for(first, first_instance, first_view, first_viewport)
                .unwrap()
                .paint_revision,
            first_stamp.paint_revision
        );
        assert_eq!(
            app.native_ui_compile_stamp_for(second, second_instance, second_view, second_viewport)
                .unwrap()
                .paint_revision,
            second_stamp.paint_revision
        );
    }

    #[test]
    fn native_damage_union_is_bounded_and_all_dominates() {
        assert_eq!(
            union_native_damage(
                DamageRegion::Rect {
                    x: 10,
                    y: 20,
                    width: 8,
                    height: 9,
                },
                DamageRegion::Rect {
                    x: 4,
                    y: 25,
                    width: 20,
                    height: 10,
                },
            ),
            DamageRegion::Rect {
                x: 4,
                y: 20,
                width: 20,
                height: 15,
            }
        );
        assert_eq!(
            union_native_damage(
                DamageRegion::Rect {
                    x: u32::MAX - 2,
                    y: u32::MAX - 1,
                    width: 20,
                    height: 20,
                },
                DamageRegion::Rect {
                    x: u32::MAX - 4,
                    y: u32::MAX - 3,
                    width: 1,
                    height: 1,
                },
            ),
            DamageRegion::Rect {
                x: u32::MAX - 4,
                y: u32::MAX - 3,
                width: 4,
                height: 3,
            }
        );
        assert_eq!(
            union_native_damage(
                DamageRegion::Rect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                },
                DamageRegion::All,
            ),
            DamageRegion::All
        );
    }

    fn status(staged_build: Option<u64>, failing_checks: u32) -> DurableUpdateStatus {
        DurableUpdateStatus {
            enabled: true,
            current_build: 10,
            staged_build,
            staged_version: staged_build.map(|build| format!("1.0.{build}")),
            staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            staged_dmg_sha256: staged_build.map(|_| "ab".repeat(32)),
            changelog: Some("# Release notes".to_string()),
            outcome: if failing_checks == 0 {
                "up to date".to_string()
            } else {
                "network failed".to_string()
            },
            failing_checks,
        }
    }

    fn reconcile_facts(
        request_sequence: u64,
        observation_sequence: u64,
        durable: Option<DurableUpdateStatus>,
    ) -> NativeUpdateReconcileFacts {
        NativeUpdateReconcileFacts {
            _ticket: NativeUpdateReconcileTicket { request_sequence },
            observation_sequence,
            durable,
            installed: None,
        }
    }

    #[test]
    fn observation_order_prevents_old_or_missing_read_from_downgrading_stage() {
        let mut missing_then_new = App::headless_for_test();
        let _ = missing_then_new.reconcile_native_update_facts(reconcile_facts(2, 1, None));
        let _ = missing_then_new.reconcile_native_update_facts(reconcile_facts(
            1,
            2,
            Some(status(Some(12), 0)),
        ));
        assert_eq!(
            missing_then_new
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build),
            Some(12)
        );

        let mut completion_reordered = App::headless_for_test();
        let _ = completion_reordered.reconcile_native_update_facts(reconcile_facts(
            1,
            2,
            Some(status(Some(12), 0)),
        ));
        assert!(matches!(
            completion_reordered.reconcile_native_update_facts(reconcile_facts(2, 1, None)),
            NativeUpdateFactsResult::IgnoredStale
        ));
        assert_eq!(
            completion_reordered
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build),
            Some(12),
            "older observation completion cannot retire the newer imported stage"
        );
    }

    #[test]
    fn same_build_different_digest_uses_latest_observation_only() {
        let mut app = App::headless_for_test();
        let mut newest = status(Some(11), 0);
        newest.staged_dmg_sha256 = Some("cd".repeat(32));
        let _ = app.reconcile_native_update_facts(reconcile_facts(1, 4, Some(newest)));
        assert!(matches!(
            app.reconcile_native_update_facts(reconcile_facts(2, 3, Some(status(Some(11), 0)),)),
            NativeUpdateFactsResult::IgnoredStale
        ));
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.dmg_sha256.as_str()),
            Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")
        );
    }

    #[test]
    fn deferred_reconcile_merges_strongest_purpose_independently_of_fact_order() {
        for (first_purpose, first_observation, second_purpose, second_observation) in [
            (
                NativeUpdateReconcilePurpose::StageAvailable,
                9,
                NativeUpdateReconcilePurpose::ApplyControl,
                8,
            ),
            (
                NativeUpdateReconcilePurpose::ApplyControl,
                8,
                NativeUpdateReconcilePurpose::Startup,
                9,
            ),
            (
                NativeUpdateReconcilePurpose::Startup,
                7,
                NativeUpdateReconcilePurpose::ApplyControl,
                10,
            ),
        ] {
            let mut app = App::headless_for_test();
            let _active = start(&mut app.native_updater_service);
            app.finish_native_update_reconcile(
                first_purpose,
                reconcile_facts(1, first_observation, Some(status(Some(11), 0))),
            );
            app.finish_native_update_reconcile(
                second_purpose,
                reconcile_facts(2, second_observation, Some(status(Some(12), 0))),
            );
            let (purpose, facts) = app
                .deferred_native_update_reconcile
                .as_ref()
                .expect("active service defers facts");
            assert_eq!(*purpose, NativeUpdateReconcilePurpose::ApplyControl);
            assert_eq!(
                facts.observation_sequence,
                first_observation.max(second_observation),
                "purpose merge and newest-facts selection are independent"
            );
        }
    }

    fn start(service: &mut NativeUpdaterService) -> UpdaterWorkTicket {
        match service.request_check() {
            CheckStart::Start(ticket) => ticket,
            other => panic!("expected updater work, got {other:?}"),
        }
    }

    #[test]
    fn same_auto_apply_build_keeps_existing_backoff_and_attempt_count() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let retry_at = std::time::Instant::now() + std::time::Duration::from_secs(23);
        let retained = crate::AutoApplyIntent {
            build,
            dmg_sha256: [0xab; 32],
            retry_at,
            attempts: 3,
        };
        app.auto_apply_intent = Some(retained);

        assert!(!app.arm_native_auto_apply(build, &"ab".repeat(32)));
        assert_eq!(app.auto_apply_intent, Some(retained));
    }

    #[test]
    fn automatic_retry_budget_is_bounded_and_physical_failures_have_no_timer_plan() {
        assert_eq!(
            automatic_retry_delay(2, AutomaticRetryKind::PreflightBlocked),
            Some(std::time::Duration::from_secs(15))
        );
        assert_eq!(
            automatic_retry_delay(u8::MAX, AutomaticRetryKind::PreflightBlocked),
            None
        );
        assert_eq!(
            automatic_retry_delay(1, AutomaticRetryKind::PhysicalFailure),
            None
        );
    }

    #[test]
    fn superseding_auto_apply_build_replaces_intent_without_ui_churn() {
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: current + 1,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(30),
            attempts: 4,
        });

        let before = std::time::Instant::now();
        assert!(app.arm_native_auto_apply(current + 2, &"cd".repeat(32)));
        let after = std::time::Instant::now();
        let intent = app.auto_apply_intent.expect("newer build stays armed");
        assert_eq!(intent.build, current + 2);
        assert_eq!(intent.dmg_sha256, [0xcd; 32]);
        assert_eq!(intent.attempts, 0);
        assert!(intent.retry_at >= before + crate::AUTOMATIC_UPDATE_QUIET_EPOCH);
        assert!(intent.retry_at <= after + crate::AUTOMATIC_UPDATE_QUIET_EPOCH);
    }

    /// Regression trace for the presentation-ack liveness bug:
    /// HiddenOutput -> WakeHandledNoPresent -> quiet epoch -> Attempt.
    ///
    /// The hidden session deliberately retains its first-edge presentation stamp
    /// forever. With cursor/blink/effects disabled there is no incidental redraw to
    /// clear it, yet latest actual output ages to quiet and the compiled auto-intent
    /// reducer admits the exact staged build.
    #[test]
    fn old_hidden_output_without_present_ages_to_automatic_attempt() {
        let mut app = App::headless_for_test();
        app.config.cursor_blink = Some(false);
        app.sparkle_force_off = true;
        // Matrix rain needs no kill here: the config default is OFF and no
        // session override exists, so no engine (hence no effect redraw) can
        // arise (the old app-global `rain_force_off` latch is retired).
        if let Some(window) = app.windows.get_mut(&crate::WindowId(0)) {
            window.focused = false;
            window.next_blink = None;
        }

        // Appending session 1 makes session 0 a genuine hidden background tab.
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
        assert!(!app.is_visible_session(0));
        assert!(app.headless);

        let output_at = std::time::Instant::now();
        let output_ns = u64::try_from(
            output_at
                .saturating_duration_since(app.lat_epoch)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        let (hidden_present_stamp, hidden_activity_stamp) = {
            let hidden = app.pool.get(0).expect("hidden session remains pooled");
            (
                hidden.last_output_ns.clone(),
                hidden.latest_output_activity_ns.clone(),
            )
        };
        hidden_present_stamp.store(output_ns, std::sync::atomic::Ordering::Relaxed);
        hidden_activity_stamp.store(output_ns, std::sync::atomic::Ordering::Release);

        // Before the output wake is handled, the latest-output clock alone closes
        // the race even though the preceding handled activity is already quiet.
        app.last_update_activity_at = output_at
            .checked_sub(crate::AUTOMATIC_UPDATE_QUIET_EPOCH + std::time::Duration::from_nanos(1))
            .expect("monotonic clock has at least one quiet epoch of history");
        assert!(!app.automatic_update_activity_quiet_with_pending_input(output_at, false));

        // Handle the wake without presenting the hidden tab. Both recent-output
        // clocks reject immediately after the wake.
        app.note_update_handoff_activity();
        let wake_handled_at = app.last_update_activity_at;
        assert!(!app.automatic_update_activity_quiet_with_pending_input(wake_handled_at, false));

        let quiet_at = wake_handled_at
            + crate::AUTOMATIC_UPDATE_QUIET_EPOCH
            + std::time::Duration::from_nanos(1);
        assert!(app.automatic_update_activity_quiet_with_pending_input(quiet_at, false));
        assert_ne!(
            hidden_present_stamp.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no present acknowledged the hidden latency sample"
        );

        let target_build = app.native_updater_service.snapshot().current_build + 1;
        assert_eq!(
            crate::native_update_auto_intent::poll(crate::native_update_auto_intent::PollFacts {
                enabled: true,
                deadline_ready: true,
                current_build: target_build - 1,
                target_build,
                work_active: false,
                applying: false,
                activity_quiet: true,
                staged_ready: true,
                staged_build: Some(target_build),
                staged_exact_target: true,
            }),
            crate::native_update_auto_intent::PollDecision::Attempt {
                build: target_build
            }
        );

        let retry_now = std::time::Instant::now();
        assert!(crate::automatic_update_activity_retry_at(retry_now) > retry_now);
    }

    /// Seamless seam 4 (retry budget): an activity-revoked overlap schedules
    /// bounded, exponentially spaced automatic re-attempts — 2 s, 8 s, 30 s —
    /// then exhausts to `None` so the caller latches manual-only. Genuine
    /// physical failures never mint a timer retry at any cycle count.
    #[test]
    fn activity_revoked_retry_budget_spaces_exponentially_then_exhausts() {
        use std::time::Duration;
        assert_eq!(
            automatic_retry_delay(0, AutomaticRetryKind::ActivityRevoked),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            automatic_retry_delay(1, AutomaticRetryKind::ActivityRevoked),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            automatic_retry_delay(2, AutomaticRetryKind::ActivityRevoked),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            automatic_retry_delay(
                MAX_AUTOMATIC_UPDATE_CYCLES,
                AutomaticRetryKind::ActivityRevoked
            ),
            None
        );
        for cycles in 0..=MAX_AUTOMATIC_UPDATE_CYCLES {
            assert_eq!(
                automatic_retry_delay(cycles, AutomaticRetryKind::PhysicalFailure),
                None,
                "a genuine physical failure must never mint a timer retry"
            );
        }
    }

    /// The App-side budget consumer: each activity-revoked completion for one
    /// exact artifact re-arms `auto_apply_intent` (clearing any manual-only
    /// latch) and consumes exactly one cycle; the fourth revocation returns
    /// `None` so the completion path falls back to the sticky manual latch.
    /// A different artifact owns a fresh budget by construction.
    #[test]
    fn overlap_retry_budget_rearms_intent_per_artifact_and_exhausts_to_manual_only() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );

        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build: 77,
            dmg_sha256: [0xab; 32],
        });
        let expected = [
            Some(std::time::Duration::from_secs(2)),
            Some(std::time::Duration::from_secs(8)),
            Some(std::time::Duration::from_secs(30)),
            None,
        ];
        for (cycle, want) in expected.into_iter().enumerate() {
            let got = app.arm_activity_revoked_overlap_retry(&ticket);
            assert_eq!(got, want, "cycle {cycle}");
            if want.is_some() {
                let intent = app.auto_apply_intent.expect("intent re-armed");
                assert_eq!(intent.build, 77);
                assert_eq!(intent.dmg_sha256, [0xab; 32]);
                assert!(
                    app.auto_apply_manual_only.is_none(),
                    "a live retry budget clears the manual-only latch"
                );
            }
        }
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(MAX_AUTOMATIC_UPDATE_CYCLES),
            "duplicate completions cannot mint fresh budget"
        );

        // A different artifact (same build, new bytes) starts a fresh budget.
        let other = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"cd".repeat(32),
        );
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&other),
            Some(std::time::Duration::from_secs(2))
        );
    }

    #[test]
    fn manual_only_latch_survives_duplicate_wakes_and_exactly_new_bytes_rearm() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
        });

        for _ in 0..3 {
            assert!(!app.arm_native_auto_apply(build, &"ab".repeat(32)));
            assert!(app.auto_apply_intent.is_none());
            assert_eq!(
                app.auto_apply_manual_only,
                Some(crate::AutoApplyManualOnly {
                    build,
                    dmg_sha256: [0xab; 32],
                })
            );
        }

        assert!(app.arm_native_auto_apply(build, &"cd".repeat(32)));
        assert_eq!(
            app.auto_apply_intent.map(|intent| intent.dmg_sha256),
            Some([0xcd; 32])
        );
        assert!(app.auto_apply_manual_only.is_none());
    }

    #[test]
    fn disabling_auto_apply_clears_retained_intent() {
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: current + 1,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now(),
            attempts: 1,
        });
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(false),
            ..crate::app_config::UpdateConfig::default()
        });

        assert!(!app.arm_native_auto_apply(current + 2, &"cd".repeat(32)));
        assert!(app.auto_apply_intent.is_none());
    }

    #[test]
    fn absent_auto_apply_intent_contributes_no_event_loop_deadline() {
        let other = std::time::Instant::now() + std::time::Duration::from_secs(7);
        assert_eq!(crate::fold_auto_apply_deadline(None, None), None);
        assert_eq!(
            crate::fold_auto_apply_deadline(None, Some(other)),
            Some(other)
        );

        let retry = other + std::time::Duration::from_secs(5);
        let intent = crate::AutoApplyIntent {
            build: 11,
            dmg_sha256: [0xab; 32],
            retry_at: retry,
            attempts: 1,
        };
        assert_eq!(
            crate::fold_auto_apply_deadline(Some(intent), None),
            Some(retry)
        );
        assert_eq!(
            crate::fold_auto_apply_deadline(Some(intent), Some(other)),
            Some(other),
            "the retry joins the existing minimum-deadline fold"
        );
    }

    #[test]
    fn debug_reexec_and_menu_feedback_cannot_bypass_dirty_native_state() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-update-dirty-preflight-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("draft.md");
        std::fs::write(&path, "draft\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));

        let mut app = App::headless_for_test();
        app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        let wid = WindowId(0);
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "unsaved ".to_string(),
            )),
        )
        .unwrap();

        let outcome = app.apply_debug_seamless_update();
        assert!(matches!(
            &outcome,
            UpdateOutcome::Blocked { reasons }
                if reasons.iter().any(|reason| reason.contains("Checkpoint Drafts"))
        ));
        // This is the exact outcome sink used by the enabled ApplyUpdate menu row:
        // the click becomes visible Software Update details instead of a dead item.
        app.surface_update_apply_outcome("menu test", outcome, true);
        let (_, view) = app.active_native_view(wid).expect("Software Update tab");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn app_reduces_up_to_date_completion_and_broadcasts_revision() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate));
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("active Settings view");
        let before_presentation = match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                state.common.presentation_revision
            }
            _ => panic!("Settings view state"),
        };

        let ticket = start(&mut app.native_updater_service);
        app.finish_native_update_check(ticket, status(None, 0));

        assert_eq!(
            app.native_updater_service.snapshot().phase,
            UpdaterPhase::Idle
        );
        assert_eq!(
            app.native_updater_service.last_transitions()[0]
                .action
                .model_action(),
            Some("CheckUpToDate")
        );
        let after_presentation = match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                state.common.presentation_revision
            }
            _ => panic!("Settings view state after completion"),
        };
        assert!(after_presentation > before_presentation);
    }

    #[test]
    fn rejected_native_completion_clears_the_settings_tab_busy_indicator() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).expect("active Settings view");
        let pending = app
            .native_runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new(format!(
                        "settings/set/{}",
                        crate::prefs::EDIT_CURSOR_BLINK
                    )),
                    value: Some(crate::native_app::SemanticInput::Bool(false)),
                }),
            )
            .unwrap();
        let operation = pending
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { reply, .. } => Some(reply.operation),
                _ => None,
            })
            .expect("Settings reducer owns pending config work");
        // Drive the same presentation refresh the normal initiating dispatch
        // performs, while deliberately retaining the effect for this test.
        app.refresh_native_presentation(wid, instance, view);
        assert!(
            app.windows
                .get(&wid)
                .and_then(|window| window.tab_set.active())
                .unwrap()
                .presentation
                .indicators
                .busy
        );

        app.dispatch_native_completion(
            wid,
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: crate::native_app::ConfigPatchOutcome::Rejected {
                    message: "read-only test config".to_string(),
                },
            },
        )
        .unwrap();
        assert!(
            !app.windows
                .get(&wid)
                .and_then(|window| window.tab_set.active())
                .unwrap()
                .presentation
                .indicators
                .busy,
            "completion-time presentation refresh removes stale busy state"
        );
    }

    #[test]
    fn app_drops_stale_worker_completion_while_new_generation_runs() {
        let mut app = App::headless_for_test();
        let first = start(&mut app.native_updater_service);
        app.finish_native_update_check(first, status(None, 1));
        let second = start(&mut app.native_updater_service);
        let before_revision = app.native_updater_service.snapshot().revision;

        app.finish_native_update_check(first, status(Some(99), 0));

        let snapshot = app.native_updater_service.snapshot();
        assert_eq!(snapshot.phase, UpdaterPhase::Checking);
        assert_eq!(snapshot.active, Some(second));
        assert!(snapshot.staged.is_none());
        assert_eq!(snapshot.ignored_completions, 1);
        assert!(snapshot.revision > before_revision);
        assert!(app.native_updater_service.last_transitions().is_empty());
    }

    #[test]
    fn tab_focus_walks_the_compiled_semantic_order() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let wid = WindowId(0);
        let (_, view) = app.active_native_view(wid).expect("active Settings view");

        app.move_native_focus(wid, false).unwrap();
        let first = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
            .expect("first focusable semantic node");
        app.move_native_focus(wid, false).unwrap();
        let second = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
            .expect("second focusable semantic node");
        assert_ne!(first, second);

        app.move_native_focus(wid, true).unwrap();
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().last_focus.as_ref()),
            Some(&first)
        );
    }

    #[test]
    fn focused_native_text_field_receives_space_and_submit_before_activation() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();

        let press = |key| crate::input::InputEvent::Key {
            key: Key::Named(key),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        assert!(app.native_input_event(wid, &press(NamedKey::Space)));
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), " ");

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Preedit("候補".to_string())),
        )
        .unwrap();
        assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), " ");
        assert!(
            state.search_input.preedit().is_none(),
            "Return is Submit for a focused text field, not generic activation"
        );
    }

    #[test]
    fn settings_search_field_honors_readline_control_keys() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("cursor trail".to_string())),
        )
        .unwrap();

        let ctrl = |c| crate::input::InputEvent::Key {
            key: Key::Character(c),
            mods: Modifiers::CTRL,
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        let (_, view) = app.active_native_view(wid).unwrap();
        let search = |app: &App| match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => (
                state.search_input.value().to_string(),
                state.search_input.selection().range(),
            ),
            _ => panic!("Settings view"),
        };

        // Ctrl-A: caret to the start (NOT select-all, NOT swallowed).
        assert!(app.native_input_event(wid, &ctrl('a')));
        assert_eq!(search(&app), ("cursor trail".to_string(), 0..0));
        // Ctrl-F / Ctrl-B: caret right then left.
        assert!(app.native_input_event(wid, &ctrl('f')));
        assert_eq!(search(&app).1, 1..1);
        assert!(app.native_input_event(wid, &ctrl('b')));
        assert_eq!(search(&app).1, 0..0);
        // Ctrl-D: forward-delete at the caret.
        assert!(app.native_input_event(wid, &ctrl('d')));
        assert_eq!(search(&app), ("ursor trail".to_string(), 0..0));
        // Ctrl-E: caret to the end; Ctrl-W: previous word dies.
        assert!(app.native_input_event(wid, &ctrl('e')));
        assert_eq!(search(&app).1, 11..11);
        assert!(app.native_input_event(wid, &ctrl('w')));
        assert_eq!(search(&app).0, "ursor ");
        // Ctrl-K after Ctrl-A kills the whole line; Ctrl-U from the end does too.
        assert!(app.native_input_event(wid, &ctrl('a')));
        assert!(app.native_input_event(wid, &ctrl('k')));
        assert_eq!(search(&app).0, "");
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("beam".to_string())),
        )
        .unwrap();
        assert!(app.native_input_event(wid, &ctrl('u')));
        assert_eq!(search(&app).0, "");
    }

    #[test]
    fn explicit_font_headless_native_card_and_hud_cover_the_exact_composed_frame() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        // Reproduce the failing renderer configuration: `$ATERM_FONT_PX=16` builds
        // a 16 px backend. Production startup passes these exact values into the
        // WindowState constructor; no constructor-owned automatic seed exists.
        app.backend.activate_px(16.0);
        app.font_px = 16.0;
        app.backend.set_pad(crate::pad_for_scale(1.0));
        app.backend.set_pad_top(crate::pad_top_for_scale(1.0));
        app.backend.set_head(0);
        let metrics = app.unattached_window_metrics();
        app.windows.get_mut(&wid).unwrap().metrics = metrics;
        assert_eq!(app.win_cell_size(wid), app.cell_size());
        assert_eq!(app.win_pad(wid), app.backend.pad());
        assert_eq!(app.win_pad_top(wid), app.backend.pad_top());
        assert_eq!(app.win_head(wid), app.backend.head());

        app.tab_strip_rows = 1;
        app.set_panel(crate::hud_bar::PanelId::Resources, true);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        assert!(app.prepare_native_input_scratch(wid));

        let ws = &app.windows[&wid];
        let card = ws.settings_card.as_ref().expect("native card raster");
        let composed_rows = u16::try_from(ws.input_scratch.cells.len()).unwrap();
        let frame = app.frame_px(composed_rows, ws.cols);
        assert_eq!(card.dx, 0);
        assert_eq!(card.dx + card.pw, frame.width, "no uncovered right band");
        let hud_rows = usize::from(app.hud_rows.min(ws.hud_cap));
        assert!(
            hud_rows > 0,
            "fixture must exercise native + HUD composition"
        );
        assert_eq!(
            card.dy as usize
                + card.ph as usize
                + hud_rows * app.win_cell_size(wid).1
                + app.win_pad(wid),
            frame.height as usize,
            "native card and HUD exactly cover the visible frame through its base bottom pad",
        );
        assert_eq!(
            card.dy as usize,
            app.win_pad_top(wid) + app.win_head(wid) + app.win_cell_size(wid).1,
            "native content begins below the padded one-row tab strip"
        );
        assert_eq!(
            card.ph as usize,
            usize::from(ws.rows) * app.win_cell_size(wid).1,
            "HUD owns the rows below native content; the base bottom pad follows the HUD",
        );
        assert_eq!(
            frame.height as usize
                - usize::from(composed_rows) * app.win_cell_size(wid).1
                - app.win_pad_top(wid),
            app.win_pad(wid),
            "visible native frame has explicit configured top and base bottom",
        );
        assert_ne!(
            card.ph as usize,
            usize::from(ws.rows) * app.win_cell_size(wid).1 + 2 * app.win_pad(wid)
                - app.win_pad_top(wid),
            "negative control: native viewport must not conserve 2*pad",
        );
        assert_eq!(
            card.rgba.len(),
            usize::try_from(card.pw).unwrap() * usize::try_from(card.ph).unwrap() * 4,
            "the retained card owns every pixel in its declared extent"
        );
        assert_ne!(
            card.rgba.last().copied(),
            Some(0),
            "the native root paints the bottom-right content pixel"
        );
    }

    #[test]
    fn explicit_font_metrics_seed_initial_and_additional_headless_windows() {
        let mut app = App::headless_for_test();
        app.backend.activate_px(16.0);
        app.font_px = 16.0;
        app.backend.set_pad(crate::pad_for_scale(1.0));
        app.backend.set_head(0);
        let expected = app.unattached_window_metrics();
        // The initial production call has to provide MetricsView explicitly. Verify
        // the constructor stores the supplied renderer truth without deriving a
        // separate automatic-font value of its own.
        let initial = crate::WindowState::new_native(
            None,
            24,
            80,
            expected,
            crate::tab_model::TabSet::default(),
        );
        assert_eq!(initial.metrics, expected);

        // Every post-startup creation seam uses the same renderer authority. Drive
        // the real testable logical-window installation path to guard that wiring.
        let sid = app.next_session_id;
        let additional = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        assert_eq!(app.windows[&additional].metrics, expected);
        assert_eq!(app.win_cell_size(additional), app.cell_size());
        assert_eq!(app.win_pad(additional), app.backend.pad());
    }
}
