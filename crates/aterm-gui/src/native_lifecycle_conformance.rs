// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for native Settings singleton activation and async routing.
//!
//! These traces drive the genuine [`crate::App`] Settings host, stable native runtime
//! identities and the versioned config reducer.  Projections are reconstructed from
//! those independent truth lanes; they are never copied from the model action or a
//! single router verdict.

#![cfg(test)]

use std::collections::BTreeSet;

use aterm_spec::derive::{Model, native_async_delivery_model, native_settings_singleton_model};
use aterm_spec::interp::{State, admits};

use crate::document_store::DocumentStore;
use crate::native_app::{
    AppEffect, AppEvent, AppKind, AppViewState, CompletionSink, ConfigPatch, ConfigPatchOutcome,
    ExternalOpenOutcome, MarkdownApp, MarkdownViewState, NativeApp, NativeRuntime, ReplyToken,
    SemanticInput, WorkOwner,
};
use crate::native_config_service::{
    ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ExpectedValue, VersionedConfigService,
};
use crate::native_settings::{SettingsApp, SettingsRoute, SettingsViewState};
use crate::native_ui::ActionId;
use crate::tab_model::{AppInstanceId, View, ViewId};
use crate::update_screen::UpdateState;
use crate::{App, WindowId};

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "shipping transition must conform specifically to {action}"
    );
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

fn settings_instance_for_view(app: &App, view: ViewId) -> Option<AppInstanceId> {
    let View::Native(link) = app.view_store.get(view).copied()? else {
        return None;
    };
    app.native_runtime
        .app(link.instance)
        .is_some_and(|native| native.kind() == AppKind::Settings)
        .then_some(link.instance)
}

fn settings_views_in_window(app: &App, wid: WindowId) -> usize {
    app.windows
        .get(&wid)
        .into_iter()
        .flat_map(|window| window.tab_set.tabs())
        .filter(|tab| settings_instance_for_view(app, tab.focus).is_some())
        .count()
}

fn settings_instances(app: &App) -> BTreeSet<AppInstanceId> {
    app.view_store
        .iter()
        .filter_map(|(view, _)| settings_instance_for_view(app, view))
        .collect()
}

fn active_is_settings(app: &App, wid: WindowId) -> bool {
    app.windows
        .get(&wid)
        .and_then(|window| window.tab_set.active())
        .is_some_and(|tab| settings_instance_for_view(app, tab.focus).is_some())
}

/// Headless twin of the production `WindowEvent::Focused(true)` branch.  Every
/// mutation here calls the same App bookkeeping seams; only the unavailable OS
/// event object is omitted.
fn focus_logical_window(app: &mut App, wid: WindowId) {
    assert!(app.windows.contains_key(&wid));
    if let Some(previous) = app.frontmost_window
        && previous != wid
    {
        app.on_focus(previous, false);
    }
    app.note_window_focused(wid);
    if app.frontmost_window != Some(wid) {
        app.frontmost_window = Some(wid);
        app.sync_active_session();
    }
    app.on_focus(wid, true);
}

#[derive(Clone, Copy)]
struct SingletonFacts {
    opens: i64,
    requesting_window: i64,
}

fn singleton_project(
    model: &Model,
    app: &App,
    windows: [WindowId; 2],
    facts: SingletonFacts,
) -> State {
    let mut state = model.init_state();
    state.insert("opens", facts.opens);
    state.insert(
        "settings_instances",
        i64::try_from(settings_instances(app).len()).expect("bounded Settings instances"),
    );
    state.insert(
        "window_one_implicit",
        i64::try_from(settings_views_in_window(app, windows[0])).expect("bounded views"),
    );
    state.insert(
        "window_two_implicit",
        i64::try_from(settings_views_in_window(app, windows[1])).expect("bounded views"),
    );
    state.insert("requesting_window", facts.requesting_window);
    // Read off the App, whoever asked: the frontmost window, when its active tab
    // is Settings. A request that raised another window projects as that window.
    let focused_window = (1..)
        .zip(windows)
        .find(|&(_, wid)| app.frontmost_window == Some(wid) && active_is_settings(app, wid))
        .map_or(0, |(number, _)| number);
    state.insert("focused_window", focused_window);
    state
}

fn open_settings_conformant(
    model: &Model,
    app: &mut App,
    windows: [WindowId; 2],
    facts: &mut SingletonFacts,
    window_number: i64,
    route: SettingsRoute,
) -> (State, State) {
    let before = singleton_project(model, app, windows, *facts);
    let target = windows[usize::try_from(window_number - 1).expect("window one or two")];
    focus_logical_window(app, target);
    assert!(app.open_settings_tab(route));
    facts.opens += 1;
    facts.requesting_window = window_number;
    let after = singleton_project(model, app, windows, *facts);
    assert_transition(
        model,
        &before,
        &after,
        if window_number == 1 {
            "OpenOne"
        } else {
            "OpenTwo"
        },
    );
    (before, after)
}

#[test]
fn real_app_settings_activation_conforms_in_two_windows_and_rejects_duplicates() {
    let model = native_settings_singleton_model();
    let mut app = App::headless_for_test();
    let one = WindowId(0);
    let sid = app.next_session_id;
    let two = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    let windows = [one, two];
    focus_logical_window(&mut app, one);
    let mut facts = SingletonFacts {
        opens: 0,
        requesting_window: 0,
    };
    assert_eq!(
        singleton_project(&model, &app, windows, facts),
        model.init_state()
    );

    open_settings_conformant(
        &model,
        &mut app,
        windows,
        &mut facts,
        1,
        SettingsRoute::Appearance,
    );
    open_settings_conformant(
        &model,
        &mut app,
        windows,
        &mut facts,
        1,
        SettingsRoute::About,
    );
    open_settings_conformant(
        &model,
        &mut app,
        windows,
        &mut facts,
        2,
        SettingsRoute::SoftwareUpdate,
    );
    let (before_repeat_two, after_repeat_two) = open_settings_conformant(
        &model,
        &mut app,
        windows,
        &mut facts,
        2,
        SettingsRoute::Home,
    );

    assert_eq!(settings_instances(&app).len(), 1);
    assert_eq!(settings_views_in_window(&app, one), 1);
    assert_eq!(settings_views_in_window(&app, two), 1);
    assert!(app.structural_invariants_ok());

    // Negative control: historical "open means allocate" behavior creates both
    // a second controller and a second implicit view in the requesting window.
    let mut duplicate = before_repeat_two.clone();
    duplicate.insert("opens", before_repeat_two["opens"] + 1);
    duplicate.insert("settings_instances", 2);
    duplicate.insert("window_two_implicit", 2);
    duplicate.insert("requesting_window", 2);
    duplicate.insert("focused_window", 2);
    assert_eq!(admits(&model, &before_repeat_two, &duplicate), None);
    assert!(!model.check_invariant("SingletonInstance", &duplicate));
    assert!(!model.check_invariant("OneImplicitViewWindowTwo", &duplicate));

    // A cross-window focus steal (the model's `Buggy=1` OpenTwo: raising the
    // window that already shows Settings instead of the requester) leaves window
    // one frontmost over its Settings tab while window two asked, which the
    // projection reads as `(requesting 2, focused 1)`. It cannot masquerade as a
    // successful OpenTwo.
    let mut stolen_focus = after_repeat_two;
    stolen_focus.insert("focused_window", 1);
    assert_eq!(admits(&model, &before_repeat_two, &stolen_focus), None);
    assert!(!model.check_invariant("RequestingWindowFocused", &stolen_focus));
}

#[derive(Clone)]
enum PendingReply {
    View(ReplyToken<ExternalOpenOutcome>),
    Config(ReplyToken<ConfigPatchOutcome>),
}

impl PendingReply {
    fn work_owner(&self) -> WorkOwner {
        match self {
            Self::View(reply) => reply.work_owner,
            Self::Config(reply) => reply.work_owner,
        }
    }

    fn sink(&self) -> CompletionSink {
        match self {
            Self::View(reply) => reply.sink,
            Self::Config(reply) => reply.sink,
        }
    }

    fn operation(&self) -> crate::native_app::OperationId {
        match self {
            Self::View(reply) => reply.operation,
            Self::Config(reply) => reply.operation,
        }
    }

    fn is_current(&self, runtime: &NativeRuntime) -> bool {
        match self {
            Self::View(reply) => runtime.completion_is_current(reply),
            Self::Config(reply) => runtime.completion_is_current(reply),
        }
    }
}

#[derive(Clone, Copy)]
struct AsyncFacts {
    owner: i64,
    sink: i64,
    token_generation: i64,
    view_generation: i64,
    /// Reductions of the latest issued operation's reply, OBSERVED: each time
    /// the Settings reducer changed the view's feedback, or the config service
    /// published a new revision, for that operation.
    reduced: i64,
}

struct AsyncHarness {
    runtime: NativeRuntime,
    config: VersionedConfigService,
    config_baseline: u64,
    settings_instance: AppInstanceId,
    settings_view: ViewId,
    markdown_instance: AppInstanceId,
    markdown_view: ViewId,
    pending: Option<PendingReply>,
    config_work: Option<ConfigPatch>,
    facts: AsyncFacts,
}

impl AsyncHarness {
    fn new() -> Self {
        let config = VersionedConfigService::new("copy_on_select = true\n".into())
            .expect("matching config baseline");
        let config_snapshot = config.snapshot();
        let config_baseline = config_snapshot.revision;
        let mut runtime = NativeRuntime::new();
        let update = UpdateState::from_status(1, "0.1.0", None, false);
        let settings_instance = runtime
            .insert_instance(NativeApp::Settings(SettingsApp::new(update)))
            .expect("Settings instance");
        let settings_view = ViewId::from_stored(1);
        let mut settings_state = SettingsViewState::from_snapshot(&config_snapshot)
            .expect("Settings consumes the same canonical config snapshot");
        // Shipping ALab provenance intentionally has no fabricated website.
        // This conformance harness supplies one explicitly because its subject
        // is the real optional OpenExternal reply lifecycle.
        settings_state.about.add_test_site("example.test");
        runtime
            .attach_view(
                settings_view,
                settings_instance,
                AppViewState::Settings(Box::new(settings_state)),
            )
            .expect("Settings view");

        // A second live view of a different app: the crossed-sink control
        // needs an identity that is live on its own and still not this reply's.
        let mut documents = DocumentStore::new();
        let document = documents.open("mem://async-conformance".into(), "alpha".into());
        let markdown_instance = runtime
            .insert_instance(NativeApp::Markdown(MarkdownApp::new(
                document,
                "Async.md".into(),
                "alpha",
            )))
            .expect("Markdown instance");
        let markdown_view = ViewId::from_stored(2);
        runtime
            .attach_view(
                markdown_view,
                markdown_instance,
                AppViewState::Markdown(MarkdownViewState::default()),
            )
            .expect("Markdown view");

        Self {
            runtime,
            config,
            config_baseline,
            settings_instance,
            settings_view,
            markdown_instance,
            markdown_view,
            pending: None,
            config_work: None,
            facts: AsyncFacts {
                owner: 0,
                sink: 0,
                token_generation: 0,
                view_generation: 1,
                reduced: 0,
            },
        }
    }

    fn project(&self, model: &Model) -> State {
        let mut state = model.init_state();
        state.insert("owner", self.facts.owner);
        state.insert("sink", self.facts.sink);
        state.insert("token_generation", self.facts.token_generation);
        state.insert("pending", i64::from(self.pending.is_some()));
        state.insert("view_generation", self.facts.view_generation);
        state.insert("reduced", self.facts.reduced);
        state.insert("wrong_delivery", 0);
        state.insert("service_dropped_with_view", 0);
        state
    }

    fn set_pending(&mut self, pending: PendingReply) {
        let (owner, generation) = match pending.work_owner() {
            WorkOwner::View { generation, .. } => (1, generation),
            WorkOwner::Service { generation, .. } => (4, generation),
        };
        let sink = match pending.sink() {
            CompletionSink::View { .. } => 1,
            CompletionSink::ServiceReducer { .. } => 4,
        };
        self.facts.owner = owner;
        self.facts.sink = sink;
        self.facts.token_generation = i64::try_from(generation).expect("bounded token generation");
        self.facts.reduced = 0;
        self.pending = Some(pending);
    }

    fn settings_feedback(&self) -> Option<String> {
        match self.runtime.view_state(self.settings_view) {
            Some(AppViewState::Settings(view)) => view.feedback.clone(),
            _ => panic!("live Settings view"),
        }
    }

    /// Deliver one `ExternalOpenFinished` through the genuine router and
    /// reducer, and count a reduction only if the reducer changed the view.
    fn deliver_external(
        &mut self,
        operation: crate::native_app::OperationId,
        outcome: ExternalOpenOutcome,
    ) {
        let before = self.settings_feedback();
        self.runtime
            .dispatch(
                self.settings_instance,
                self.settings_view,
                AppEvent::ExternalOpenFinished { operation, outcome },
            )
            .expect("the router delivers the view completion");
        if self.settings_feedback() != before {
            self.facts.reduced += 1;
        }
    }

    fn view_reply(&mut self) -> ReplyToken<ExternalOpenOutcome> {
        self.runtime
            .dispatch(
                self.settings_instance,
                self.settings_view,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: ActionId::new(format!("settings/route{}", SettingsRoute::About.path())),
                    value: None,
                }),
            )
            .expect("route Settings to About");
        let outcome = self
            .runtime
            .dispatch(
                self.settings_instance,
                self.settings_view,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: ActionId::new("about/open-site"),
                    value: None,
                }),
            )
            .expect("issue view-owned external open");
        outcome
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                AppEffect::OpenExternal { reply, .. } => Some(reply),
                _ => None,
            })
            .expect("real Settings view reply")
    }

    fn issue_view(&mut self) {
        assert!(self.pending.is_none());
        let reply = self.view_reply();
        self.set_pending(PendingReply::View(reply));
    }

    fn issue_service(&mut self) {
        let outcome = self
            .runtime
            .dispatch(
                self.settings_instance,
                self.settings_view,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: ActionId::new("settings/set/copy_on_select"),
                    value: Some(SemanticInput::Bool(false)),
                }),
            )
            .expect("issue service-owned config patch");
        let (patch, reply) = outcome
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { patch, reply } => Some((patch, reply)),
                _ => None,
            })
            .expect("real Settings config reply");
        self.config_work = Some(patch);
        self.set_pending(PendingReply::Config(reply));
    }

    fn close_requester_view(&mut self) {
        assert!(self.runtime.remove_view(self.settings_view).is_some());
        self.facts.view_generation += 1;
    }

    fn complete_view(&mut self) -> PendingReply {
        let reply = self.pending.take().expect("pending view completion");
        assert!(reply.is_current(&self.runtime));
        let reduced = self.facts.reduced;
        self.deliver_external(reply.operation(), ExternalOpenOutcome::Opened);
        assert_eq!(
            self.facts.reduced,
            reduced + 1,
            "the current reply reduces the view"
        );
        reply
    }

    fn complete_service(&mut self) {
        let reply = self.pending.take().expect("pending service completion");
        assert!(reply.is_current(&self.runtime));
        let patch = self.config_work.take().expect("pending config work");
        let revision_before = self.config.snapshot().revision;
        let result = self.config.patch(ConfigPatchRequest {
            base_revision: patch.base_revision,
            edits: patch
                .edits
                .into_iter()
                .map(|edit| ConfigKeyEdit {
                    key: edit.key,
                    expected: match edit.expected {
                        crate::native_app::ExpectedConfigValue::Any => ExpectedValue::Any,
                        crate::native_app::ExpectedConfigValue::Exact(value) => {
                            ExpectedValue::Exact(value)
                        }
                    },
                    value: edit.value,
                })
                .collect(),
        });
        assert!(
            matches!(
                result,
                ConfigPatchResult::Applied { .. } | ConfigPatchResult::Unchanged { .. }
            ),
            "service-owned patch must apply against its matching baseline: {result:?}"
        );
        assert_eq!(self.config.snapshot().revision, self.config_baseline + 1);
        if self.config.snapshot().revision > revision_before {
            self.facts.reduced += 1;
        }
    }
}

fn async_issue(
    model: &Model,
    harness: &mut AsyncHarness,
    action: &'static str,
    issue: impl FnOnce(&mut AsyncHarness),
) {
    let before = harness.project(model);
    issue(harness);
    let after = harness.project(model);
    assert_transition(model, &before, &after, action);
}

fn async_step(
    model: &Model,
    harness: &mut AsyncHarness,
    action: &'static str,
    step: impl FnOnce(&mut AsyncHarness),
) -> (State, State) {
    let before = harness.project(model);
    step(harness);
    let after = harness.project(model);
    assert_transition(model, &before, &after, action);
    (before, after)
}

#[test]
fn native_runtime_accepts_a_live_view_owner_once() {
    let model = native_async_delivery_model();
    let mut harness = AsyncHarness::new();
    assert_eq!(harness.project(&model), model.init_state());
    async_issue(&model, &mut harness, "IssueView", |h| h.issue_view());
    let (_, completed) = async_step(&model, &mut harness, "CompleteView", |h| {
        let _ = h.complete_view();
    });
    assert_eq!(completed["reduced"], 1);
}

#[test]
fn service_completion_outlives_requester_and_stale_generations_drop() {
    let model = native_async_delivery_model();
    let mut service = AsyncHarness::new();
    async_issue(&model, &mut service, "IssueService", |h| h.issue_service());
    async_step(&model, &mut service, "NavigateView", |h| {
        h.close_requester_view();
    });
    assert!(
        service
            .pending
            .as_ref()
            .expect("service work remains pending")
            .is_current(&service.runtime),
        "service ownership is independent of the initiating view"
    );
    async_step(&model, &mut service, "CompleteService", |h| {
        h.complete_service();
    });

    let mut view = AsyncHarness::new();
    async_issue(&model, &mut view, "IssueView", |h| h.issue_view());
    async_step(&model, &mut view, "NavigateView", |h| {
        h.close_requester_view();
    });
    assert!(!view.pending.as_ref().unwrap().is_current(&view.runtime));
    async_step(&model, &mut view, "DropStaleView", |h| {
        h.pending = None;
    });
}

#[test]
fn router_rejects_crossed_sink_and_model_catches_wrong_or_duplicate_delivery() {
    let model = native_async_delivery_model();
    let mut harness = AsyncHarness::new();
    async_issue(&model, &mut harness, "IssueView", |h| h.issue_view());
    let legitimate_pending = harness.project(&model);

    let Some(PendingReply::View(reply)) = harness.pending.as_ref() else {
        panic!("view reply");
    };
    // Both halves live on their own: the Markdown view's identity, as owner
    // AND sink, is a current reply proof...
    let markdown = CompletionSink::View {
        instance: harness.markdown_instance,
        view: harness.markdown_view,
        generation: 1,
    };
    let mut aligned = reply.clone();
    aligned.work_owner = WorkOwner::View {
        instance: harness.markdown_instance,
        view: harness.markdown_view,
        generation: 1,
    };
    aligned.sink = markdown;
    assert!(
        harness.runtime.completion_is_current(&aligned),
        "the crossed sink's identity is live by itself"
    );
    // ...and crossing it with the Settings owner is not.
    let mut crossed = reply.clone();
    crossed.sink = markdown;
    assert!(
        !harness.runtime.completion_is_current(&crossed),
        "two independently live identities are not one coherent reply proof"
    );

    let mut wrong_delivery = legitimate_pending.clone();
    wrong_delivery.insert("pending", 0);
    wrong_delivery.insert("reduced", 1);
    wrong_delivery.insert("wrong_delivery", 1);
    assert_eq!(admits(&model, &legitimate_pending, &wrong_delivery), None);
    assert!(!model.check_invariant("IdentityAndGenerationChecked", &wrong_delivery));

    let reply = harness.complete_view();
    let completed = harness.project(&model);
    assert_eq!(
        admits(&model, &legitimate_pending, &completed),
        Some("CompleteView")
    );
    let feedback = harness.settings_feedback();
    let PendingReply::View(reply) = reply else {
        unreachable!();
    };
    // A second delivery of the consumed reply, through the genuine router and
    // reducer, with an outcome that WOULD change the feedback were it reduced:
    // `finish_external`'s `pending.remove` guard finds no operation, and the
    // view is untouched — no model step at all.
    harness.deliver_external(
        reply.operation,
        ExternalOpenOutcome::Denied {
            message: "duplicate".into(),
        },
    );
    assert_eq!(harness.settings_feedback(), feedback);
    assert_eq!(harness.project(&model), completed);

    // Negative control: the reducer without that guard reduces the reply a
    // second time.
    let mut duplicate_reduction = completed.clone();
    duplicate_reduction.insert("reduced", 2);
    assert_eq!(
        aterm_spec::interp::with_buggy(&model, 1).successors("RedeliverView", &completed),
        vec![duplicate_reduction.clone()]
    );
    assert_eq!(admits(&model, &completed, &duplicate_reduction), None);
    assert!(!model.check_invariant("ReducedAtMostOnce", &duplicate_reduction));
}
