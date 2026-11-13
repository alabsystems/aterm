// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only Allocation application. Inputs are independent immutable vitals
//! and delegated native-session observations; rendering performs no probes or I/O.

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, Command, CommonViewState, DamageRegion, EventResult, NativeAppModel, UpdateCx,
    ViewCx,
};
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    StyleRef, UiContent, UiNode, UiTree,
};
use aterm_capacity::CpuObservation;

/// CPU leases have their own observation time, independent of kernel samples.
#[derive(Clone, Debug)]
pub(crate) struct ObservedCpu {
    pub(crate) at: Instant,
    pub(crate) value: CpuObservation,
}

#[derive(Default)]
pub(crate) struct AllocationApp {
    #[cfg(target_os = "macos")]
    pub(crate) monitor: Option<Arc<crate::allocation_monitor_host::Presentation>>,
    pub(crate) leases: Option<Arc<ObservedCpu>>,
    pub(crate) machine: Option<Arc<aterm_vitals::Snapshot>>,
    pub(crate) machine_at: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct AllocationViewState {
    pub(crate) common: CommonViewState,
    scroll: f32,
    limit: Cell<f32>,
    room: Cell<f32>,
}

const STALE: Duration = Duration::from_secs(15);

#[cfg(target_os = "macos")]
fn whole_age_seconds(lower: Duration, upper: Duration) -> Option<(u64, u64)> {
    if lower > upper {
        return None;
    }
    Some((
        lower.as_secs(),
        upper
            .as_secs()
            .checked_add(u64::from(upper.subsec_nanos() != 0))?,
    ))
}

#[cfg(target_os = "macos")]
fn completion_row(
    record: &atpkg::build_admit::service::monitor::RecordSummary,
) -> Option<(String, String)> {
    use atpkg::build_admit::service::monitor::EventKind;

    let (outcome, revision) = match record.kind {
        EventKind::Finished {
            succeeded,
            cancelled,
        } => (
            format!(
                "{} · Finished · {}{}",
                record.attempt.map_or_else(
                    || "Session outcome".into(),
                    |attempt| format!("Session attempt {attempt}")
                ),
                if succeeded {
                    "success established"
                } else {
                    "success not established"
                },
                if cancelled {
                    " · cancellation requested"
                } else {
                    ""
                }
            ),
            if record.attempt.is_some() {
                "completion revision"
            } else {
                "history revision"
            },
        ),
        EventKind::Failed => match record.attempt {
            Some(attempt) => (
                format!("Session attempt {attempt} · Owner-reported failure"),
                "completion revision",
            ),
            None => ("Session service failure".into(), "history revision"),
        },
        _ => return None,
    };
    Some((
        format!("monitor/completion/{}", record.revision),
        format!("{outcome} · {revision} {}", record.revision),
    ))
}

#[cfg(target_os = "macos")]
fn epoch_rows(epoch: &aterm_capacity::host_protocol::Epoch) -> [(String, String); 4] {
    let key = |part| format!("monitor/epoch/{}/{part}", epoch.sequence);
    let yes = |value| if value { "yes" } else { "no" };
    let launch = if epoch.cancellation_started {
        " · launch cancellation started"
    } else if epoch.not_spawned {
        " · no child spawned; cancellation pending"
    } else if epoch.unbound {
        " · child held; durable binding pending"
    } else {
        ""
    };
    let fault = if epoch.faulted {
        " · protocol fault"
    } else {
        ""
    };
    let failure = epoch.failure.map_or_else(String::new, |failure| {
        format!(" · owner error {:?}", failure.kind)
    });
    // Endpoint cleanup follows the original claim's group-drain check. The
    // snapshot exposes their combined gate, not an independent group probe.
    let cleanup = match (epoch.cancellation_started, epoch.endpoint_cleaned) {
        (true, true) => "unlaunched cleanup confirmed",
        (true, false) => "unlaunched cleanup pending",
        (false, true) => "group drain + endpoint cleanup confirmed",
        (false, false) => "group drain + endpoint cleanup pending",
    };
    [
        (
            key("budget"),
            format!(
                "Epoch {} · CPU backing N={} · root slots R={} · original admitted memory peak {} bytes ({:.1} GiB)",
                epoch.sequence,
                epoch.backing,
                epoch.roots,
                epoch.reserved_bytes,
                epoch.reserved_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
            ),
        ),
        (
            key("roots"),
            format!(
                "Roots: {} completed of {} slots · {} failed{launch}{fault}{failure}",
                epoch.completed_roots, epoch.roots, epoch.failed_roots
            ),
        ),
        (
            key("transport"),
            format!(
                "Transport: opened {} · Retired {} · helper exited {} · output ended {}",
                yes(epoch.opened),
                yes(epoch.retired),
                yes(epoch.helper_exited),
                yes(epoch.control_eof)
            ),
        ),
        (
            key("backing"),
            format!(
                "CPU backing: {} returned · {} remaining · {} uncertain · {cleanup}",
                epoch.settled_units, epoch.remaining_units, epoch.uncertain_units
            ),
        ),
    ]
}

#[cfg(target_os = "macos")]
fn cache_work_rows(epoch: &aterm_capacity::host_protocol::Epoch) -> Vec<(String, String)> {
    use aterm_capacity::cache_work::{Availability, UnavailableReason};
    let mut rows = Vec::new();
    let mut records: Vec<_> = epoch.cache_work.iter().collect();
    records.sort_unstable_by_key(|record| record.root);
    for record in records {
        let key = |part| {
            format!(
                "monitor/epoch/{}/root/{}/cache/{part}",
                epoch.sequence, record.root
            )
        };
        let prefix = format!("Root {} · Historical cache validation", record.root);
        if let Some(reason) = record.unavailable {
            let reason = match reason {
                UnavailableReason::NotRecorded => "not recorded",
                UnavailableReason::UnsupportedDriver => "unsupported driver",
                UnavailableReason::MissingSession => "session marker missing",
                UnavailableReason::AmbiguousSession => "session marker ambiguous",
                UnavailableReason::QueryFailed => "report query failed",
                UnavailableReason::InvalidReport => "invalid report",
                UnavailableReason::PersistenceFailed => "report persistence failed",
            };
            rows.push((
                key("availability"),
                format!("{prefix}: unavailable · {reason}"),
            ));
            continue;
        }
        let Some(summary) = record
            .summary
            .as_ref()
            .filter(|summary| summary.validate().is_ok())
        else {
            rows.push((
                key("availability"),
                format!("{prefix}: unavailable · invalid summary"),
            ));
            continue;
        };
        let availability = match summary.availability {
            Availability::Complete => "complete",
            Availability::UnavailableLegacyLog => "legacy log unavailable",
        };
        rows.push((
            key("availability"),
            format!(
                "{prefix} · run {} · scope {} · {availability}",
                summary.run_id,
                summary.scope()
            ),
        ));
        if let Some(work) = summary.totals {
            let d = summary.denominators;
            rows.push((key("coverage"), format!("Build-script input work: {} observed of {} script units · {} registered units · {} capture attempts · {} uncacheable", d.observed_units, d.run_script_units, d.registered_units, d.attempts_finished, d.uncacheable_attempts)));
            rows.push((
                key("work"),
                format!(
                    "Inputs: {} bytes hashed · {} metadata reads · {} revalidations",
                    work.bytes_hashed, work.metadata_reads, work.revalidations
                ),
            ));
        }
    }
    let not_recorded = epoch
        .completed_roots
        .saturating_sub(epoch.cache_work.len() as u32);
    if not_recorded != 0 || epoch.cache_work.is_empty() {
        rows.push((
            format!("monitor/epoch/{}/cache/not-recorded", epoch.sequence),
            if not_recorded == 0 {
                "Cache validation: no completed-root report recorded".into()
            } else {
                format!("Cache validation: not recorded for {not_recorded} completed roots")
            },
        ));
    }
    rows
}

fn age(at: Option<Instant>, now: Instant) -> String {
    match at {
        None => "Awaiting observation".into(),
        Some(at) => {
            let elapsed = now.saturating_duration_since(at);
            format!(
                "{} · {}s ago",
                if elapsed > STALE { "Stale" } else { "Observed" },
                elapsed.as_secs()
            )
        }
    }
}

impl AllocationApp {
    fn rows(&self, now: Instant) -> Vec<(String, String)> {
        let mut rows = vec![
            (
                "service".into(),
                "Host service: not connected. Observing this account's CPU leases.".into(),
            ),
            (
                "lease-age".into(),
                format!(
                    "CPU lease observation: {}",
                    age(self.leases.as_ref().map(|s| s.at), now)
                ),
            ),
        ];
        #[cfg(target_os = "macos")]
        self.monitor_rows(&mut rows);
        match self.leases.as_ref().map(|s| &s.value) {
            None => rows.push((
                "lease-state".into(),
                "CPU leases: awaiting observation".into(),
            )),
            Some(CpuObservation::Unsupported) => rows.push((
                "lease-state".into(),
                "CPU leases: unsupported on this platform".into(),
            )),
            Some(CpuObservation::Absent) => rows.push((
                "lease-state".into(),
                "CPU leases: pool absent; no allocation totals available".into(),
            )),
            Some(CpuObservation::Initializing) => rows.push((
                "lease-state".into(),
                "CPU leases: initializing; occupancy unavailable".into(),
            )),
            Some(CpuObservation::Busy) => rows.push((
                "lease-state".into(),
                "CPU leases: admission snapshot busy; occupancy unavailable".into(),
            )),
            Some(CpuObservation::Unreadable(error)) => rows.push((
                "lease-state".into(),
                format!("CPU leases: unreadable ({error})"),
            )),
            Some(
                value @ CpuObservation::Ready {
                    total,
                    holders,
                    external,
                },
            ) => {
                rows.push((
                    "lease-state".into(),
                    format!(
                        "CPU leases: {} held · {} available · {total} total",
                        value.occupied().unwrap_or(0),
                        value.available().unwrap_or(0)
                    ),
                ));
                rows.push((
                    "lease-basis".into(),
                    "A held lease is not measured CPU use or proof of process identity.".into(),
                ));
                if let Some(external) = external {
                    rows.push((
                        "lease-external".into(),
                        format!(
                            "Shared backing: {} native · {} external held · {} crash-retained",
                            holders.len(),
                            external.holders.len(),
                            external.retained.len()
                        ),
                    ));
                }
                for holder in holders {
                    rows.push((
                        format!("slot/{}", holder.slot),
                        format!(
                            "Slot {} · reported PID {} · reported birth {}",
                            holder.slot,
                            holder
                                .reported_pid
                                .map_or_else(|| "unknown".into(), |pid| pid.to_string()),
                            holder
                                .reported_birth
                                .map_or_else(|| "unknown".into(), |birth| birth.to_string())
                        ),
                    ));
                }
            }
        }
        rows.push((
            "machine-age".into(),
            format!("System observation: {}", age(self.machine_at, now)),
        ));
        let cpu = self.machine.as_ref().and_then(|s| s.cpu.as_ref());
        rows.push((
            "cpu".into(),
            cpu.map_or_else(
                || "CPU use: unavailable".into(),
                |cpu| {
                    format!(
                        "CPU: {} logical cores · {} busy",
                        cpu.total(),
                        cpu.current_busy().map_or_else(
                            || "unavailable".into(),
                            |busy| format!("{busy:.1} cores")
                        )
                    )
                },
            ),
        ));
        let memory = self.machine.as_ref().and_then(|s| s.mem.as_ref());
        rows.push((
            "memory".into(),
            memory.map_or_else(
                || "Physical memory: unavailable".into(),
                |memory| {
                    let gib = 1024.0 * 1024.0 * 1024.0;
                    format!(
                        "Physical memory: {:.1} GiB · free {:.1} GiB · swap used {:.1} GiB",
                        memory.sample.total as f64 / gib,
                        memory.sample.free as f64 / gib,
                        memory.sample.swap_used as f64 / gib
                    )
                },
            ),
        ));
        rows.push(("limits".into(), "Computer-wide demand, wait reasons and arena accounting are not yet provided by the host service.".into()));
        rows
    }

    #[cfg(target_os = "macos")]
    fn monitor_rows(&self, rows: &mut Vec<(String, String)>) {
        use crate::allocation_monitor_host::Phase;
        use aterm_capacity::host_protocol::Phase as JobPhase;
        let Some(monitor) = self.monitor.as_ref().filter(|m| m.requested != 0) else {
            return;
        };
        let mut native = vec![(
            "monitor/scope".into(),
            "Delegated native session · read-only observation".into(),
        )];
        let phase = match monitor.phase {
            Phase::Unattached => "Unattached".into(),
            Phase::Queued => "Queued".into(),
            Phase::Reading => "Reading".into(),
            Phase::Observed => "Observed".into(),
            Phase::Failed(failure) => format!(
                "Failed ({:?}); previous observations remain historical",
                failure.kind
            ),
        };
        native.push((
            "monitor/attachment".into(),
            format!("Monitor attachment {}: {phase}", monitor.requested),
        ));
        if let Some(generation) = monitor.desired_generation
            && monitor
                .last
                .as_ref()
                .is_none_or(|last| last.observation.snapshot.generation != generation)
        {
            native.push((
                "monitor/requested-source".into(),
                format!(
                    "Requested source generation: {}",
                    aterm_uds::rand::hex_encode(&generation.0)
                ),
            ));
        }
        let Some(last) = &monitor.last else {
            native.push((
                "monitor/empty".into(),
                "No native-session observation received yet".into(),
            ));
            rows.splice(1..1, native);
            return;
        };
        if last.attachment != monitor.requested {
            native.push((
                "monitor/retained".into(),
                format!(
                    "Retained observation from attachment {}; attachment {} has no observation",
                    last.attachment, monitor.requested
                ),
            ));
        }
        let snapshot = &last.observation.snapshot;
        native.push((
            "monitor/source".into(),
            format!(
                "Source generation: {} · snapshot revision {} · attachment {}",
                aterm_uds::rand::hex_encode(&snapshot.generation.0),
                snapshot.revision,
                last.attachment
            ),
        ));
        native.push((
            "monitor/history".into(),
            format!(
                "Delivered history through revision {} · last published frame {}",
                last.observation.next.revision, last.observation.request_sequence
            ),
        ));
        if let Some(gap) = last.last_gap {
            native.push((
                "monitor/history-gap".into(),
                format!(
                    "Latest reported history gap: revisions {}–{} unavailable",
                    gap.first, gap.last
                ),
            ));
        }
        native.push((
            "monitor/age".into(),
            last.observation
                .age_bounds()
                .and_then(|age| {
                    whole_age_seconds(age.lower, age.upper).map(|seconds| (age, seconds))
                })
                .map_or_else(
                    || "Native observation age: unavailable".into(),
                    |(age, (lower, upper))| {
                        format!(
                            "Native observation age: {}–{}s{}",
                            lower,
                            upper,
                            if age.upper > STALE { " · Stale" } else { "" }
                        )
                    },
                ),
        ));
        if snapshot.closed {
            native.push((
                "monitor/closed".into(),
                "Source owner closed; last observation retained".into(),
            ));
        }
        for job in &snapshot.jobs {
            let phase = match job.phase {
                JobPhase::Starting => "Starting",
                JobPhase::Waiting => "Waiting for next child observation",
                JobPhase::Stopped => "Stopped",
                JobPhase::Cancelling => "Cancelling",
                JobPhase::ExitPinned => "Exit observed; leader retained",
                JobPhase::Faulted => "Faulted",
            };
            native.push((
                format!("monitor/attempt/{}", job.attempt),
                format!(
                    "Session attempt {} · {phase} · admitted peak {} bytes ({:.1} GiB)",
                    job.attempt,
                    job.reserved_bytes,
                    job.reserved_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
                ),
            ));
        }
        if snapshot.jobs.is_empty() {
            native.push((
                "monitor/roots".into(),
                "No native job rows in this snapshot; epoch work may remain".into(),
            ));
        }
        native.push(("monitor/epochs".into(), "Private compile epochs".into()));
        let mut epochs: Vec<_> = snapshot.epochs.iter().collect();
        epochs.sort_unstable_by_key(|epoch| epoch.sequence);
        for epoch in epochs {
            native.extend(epoch_rows(epoch));
            native.extend(cache_work_rows(epoch));
        }
        if snapshot.epochs.is_empty() {
            native.push((
                "monitor/epochs-empty".into(),
                "No private compile epochs in this snapshot".into(),
            ));
        } else {
            native.push((
                "monitor/epochs-basis".into(),
                "Retired confirms helper/token retirement. CPU backing returns after group drain and durable cleanup, or genuine no-child cancellation. Root outcomes remain separate.".into(),
            ));
        }
        native.push((
            "monitor/completions".into(),
            "Recent delivered outcomes".into(),
        ));
        if last.omitted_terminals != 0 {
            native.push((
                "monitor/completions-omitted".into(),
                format!(
                    "Earlier delivered outcomes omitted locally: {}",
                    last.omitted_terminals
                ),
            ));
        }
        let completed = native.len();
        native.extend(last.recent.iter().filter_map(completion_row));
        if native.len() == completed {
            native.push((
                "monitor/completions-empty".into(),
                "No delivered outcomes retained".into(),
            ));
        }
        native.push(("monitor/limits".into(), "Session observations report original reservations and custody, not measured CPU use, current computer leadership or proof status.".into()));
        rows.splice(1..1, native);
    }
}

impl NativeAppModel for AllocationApp {
    type ViewState = AllocationViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Allocation,
            name: "Allocation",
            icon: AppIcon::SystemState,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::FocusChanged(focus) => view.common.last_focus = focus,
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll += lines as f32 * 28.0
            }
            AppEvent::ScrollPage(direction) => {
                view.scroll += direction as f32 * view.room.get().max(28.0)
            }
            _ => return EventResult::Bubble,
        }
        view.scroll = view.scroll.clamp(0.0, view.limit.get());
        cx.repaint(DamageRegion::All);
        EventResult::Handled
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        let scale = crate::native_appearance::text_scale();
        let width = (cx.viewport.width - 32.0).max(1.0);
        let mut children = vec![];
        let mut height = 0.0;
        for (key, text) in std::iter::once(("title".into(), "Allocation · This computer".into()))
            .chain(self.rows(Instant::now()))
        {
            let heading = matches!(
                key.as_str(),
                "title" | "monitor/epochs" | "monitor/completions"
            );
            let role = if heading {
                SemanticRole::Heading
            } else {
                SemanticRole::Text
            };
            let style = if heading {
                StyleRef::Plain
            } else {
                StyleRef::Secondary
            };
            let row = crate::native_about::wrapped(
                &format!("allocation/{key}"),
                &text,
                width,
                role,
                style,
            );
            let row_height = match row.layout.height {
                Length::Fixed(height) => height,
                _ => 0.0,
            };
            children.push(row);
            height += row_height + 10.0;
        }
        children.push(
            UiNode::new(
                "allocation/pause",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Pause admission unavailable: no host-service connection"),
                        ActionId::new("allocation/pause"),
                    )
                    .state(ControlState {
                        enabled: false,
                        ..ControlState::default()
                    }),
                ),
            )
            .layout(Layout::default().height(Length::Fixed(36.0 * scale))),
        );
        height += 36.0 * scale;
        let room = (cx.viewport.height - 32.0).max(0.0);
        let limit = (height - room).max(0.0);
        view.room.set(room);
        view.limit.set(limit);
        let list = UiNode::new(
            "allocation/content",
            UiContent::Group(GroupSpec::new("Resource observations")),
        )
        .layout(
            Layout::column()
                .gap(10.0)
                .height(Length::Fixed(room))
                .clipped()
                .scrolled(view.scroll.min(limit)),
        )
        .room(room, limit > 0.0)
        .lists_every_row()
        .children(children);
        let mut root = UiNode::new(
            "allocation/root",
            UiContent::Group(GroupSpec::new("Allocation")),
        )
        .layout(Layout::column().padding(Insets::all(16.0)).clipped())
        .children(vec![list]);
        if let UiContent::Group(spec) = &mut root.content {
            spec.role = SemanticRole::Application;
        }
        UiTree::new(root)
    }

    fn commands(&self, _: &Self::ViewState, _: &mut Vec<Command>) {}
    fn presentation(&self, _: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: "Allocation".into(),
            icon: AppIcon::SystemState,
            indicators: AppIndicators::default(),
            closable: true,
            tooltip: Some("Read-only resource allocation observations".into()),
        }
    }
    fn prepare_close(&mut self, _: CloseRequest, _: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "native_allocation_completion_tests.rs"]
mod completion_tests;

#[cfg(test)]
#[path = "native_allocation_tests.rs"]
mod tests;
