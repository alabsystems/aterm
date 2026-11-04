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
            Some(CpuObservation::Unreadable(error)) => rows.push((
                "lease-state".into(),
                format!("CPU leases: unreadable ({error})"),
            )),
            Some(CpuObservation::Ready { total, holders }) => {
                rows.push((
                    "lease-state".into(),
                    format!(
                        "CPU leases: {} held · {} available · {total} total",
                        holders.len(),
                        total.saturating_sub(holders.len())
                    ),
                ));
                rows.push((
                    "lease-basis".into(),
                    "A held lease is not measured CPU use or proof of process identity.".into(),
                ));
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
        rows.push(("limits".into(), "Build identities, demand, grants, wait reasons and arena accounting are not yet provided by the host service.".into()));
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
                "No owned native roots in this snapshot; retained work may remain".into(),
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
        native.push(("monitor/limits".into(), "Session observation does not establish CPU grants, current computer leadership or proof status.".into()));
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
            let heading = key == "title" || key == "monitor/completions";
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
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn monitor_age_seconds_enclose_fractional_bounds_and_refuse_overflow() {
        assert_eq!(
            whole_age_seconds(Duration::from_millis(100), Duration::from_millis(900)),
            Some((0, 1))
        );
        assert_eq!(
            whole_age_seconds(Duration::from_secs(1), Duration::from_secs(2)),
            Some((1, 2))
        );
        assert_eq!(
            whole_age_seconds(Duration::from_secs(2), Duration::from_secs(1)),
            None
        );
        assert_eq!(whole_age_seconds(Duration::ZERO, Duration::MAX), None);
    }
    use crate::native_app::{
        ActionInvocation, AppViewState, NativeApp, NativeRuntime, ViewId, ViewMotionCx,
    };
    use crate::native_ui::{LogicalRect, UiKey};

    #[test]
    fn lease_state_never_invents_capacity_or_running_work() {
        let now = Instant::now();
        let mut app = AllocationApp::default();
        for state in [
            CpuObservation::Absent,
            CpuObservation::Initializing,
            CpuObservation::Unreadable("refused".into()),
            CpuObservation::Unsupported,
        ] {
            app.leases = Some(Arc::new(ObservedCpu {
                at: now,
                value: state,
            }));
            let rows = app.rows(now);
            assert!(!rows.iter().any(|(_, text)| text.contains("0 held")));
            assert!(rows.iter().any(|(_, text)| text.contains("not connected")));
        }
        app.leases = Some(Arc::new(ObservedCpu {
            at: now,
            value: CpuObservation::Ready {
                total: 2,
                holders: vec![aterm_capacity::CpuHolder {
                    slot: 1,
                    reported_pid: None,
                    reported_birth: None,
                }],
            },
        }));
        let rows = app.rows(now + Duration::from_secs(20));
        assert!(
            rows.iter()
                .any(|(_, text)| text.contains("1 held · 1 available · 2 total"))
        );
        assert!(
            rows.iter()
                .any(|(_, text)| text.contains("Stale · 20s ago"))
        );
        assert!(
            rows.iter()
                .any(|(_, text)| text.contains("reported PID unknown"))
        );
        assert!(rows.iter().any(|(_, text)| text == "CPU use: unavailable"));
    }

    #[test]
    fn admission_control_is_disabled_and_dispatch_cannot_issue_work() {
        let mut runtime = NativeRuntime::new();
        let instance = runtime
            .insert_instance(NativeApp::Allocation(Box::default()))
            .unwrap();
        let view = ViewId::from_stored(712);
        runtime
            .attach_view(view, instance, AppViewState::Allocation(Box::default()))
            .unwrap();
        let cx = ViewCx {
            viewport: LogicalRect::new(0.0, 0.0, 400.0, 300.0),
            animation_phase_ms: 0,
            motion: ViewMotionCx::default(),
            terminal_font_px: 16.0,
            terminal_scale: 1.0,
            semantic_font: None,
            document: None,
        };
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("allocation/pause"))
                .unwrap()
                .state
                .is_some_and(|state| !state.enabled)
        );
        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("allocation/pause"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(outcome.effects.is_empty());
    }
}
