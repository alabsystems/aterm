// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `aterm_spec::derive::native_update_settings_draft_carry_model`
//! (`NativeUpdateSettingsDraftCarry`, plan P2-2): an unsaved Settings draft
//! rides a seamless self-update instead of holding it — and, beside it, the
//! same rule for a document whose last save failed.
//!
//! Each schedule drives two real `App`s through the shipping code: the
//! Settings view's own input, the update preflight
//! (`revalidate_native_update_safety`, the one the Commit admission calls) and
//! the close barrier it runs behind, the park's capture
//! (`capture_handoff_layout`) written to the real layout wire and parsed back
//! as the successor parses it (`RestoreManifest::from_toml`), the successor's
//! restore (`restore_into_window`, which reopens the Settings view and hands it
//! the carried drafts), the Commit's layout comparison
//! (`commit_layout_topology`) and the lane gate (`lane_carry_refusal`). After
//! every real step the machine is projected from real state — the outgoing
//! view's draft, the parked layout's draft, the successor view's draft or its
//! loss message — and must equal the model's state after the named actions;
//! the real verdicts must BE the model's guards; and at each refusal the
//! defect machine admits what the real code refused, so no pass is vacuous.

use aterm_spec::derive::{Model, native_update_settings_draft_carry_model};
use aterm_spec::interp::{self, State};

use crate::app_native::{NativeUpdateSafetyToken, SettingsDraftTally};
use crate::app_update_handoff::{commit_layout_topology, lane_carry_refusal};
use crate::native_app::{AppEvent, AppKind, AppViewState, CloseScope, TextInputEvent};
use crate::native_settings::{CarriedFieldDrafts, SettingsRoute};
use crate::native_update_admission::ApplyLane;
use crate::restore::{RestoreManifest, RestoredSplitTree, RestoredView, SettingsDraftRestore};
use crate::{App, WindowId};

/// The model's `Surfaced`: the successor said which draft it could not reopen.
const SURFACED: i64 = 9;
/// Any running build; the lane gate compares it with the target's.
const RUNNING: u64 = 5_000;
/// The Settings row every schedule types into: a plain text field.
const FIELD: &str = crate::prefs::EDIT_FONT_FAMILY;

fn wid() -> WindowId {
    WindowId(0)
}

/// One outgoing App with one Settings tab, the successor it hands to, and the
/// bookkeeping that names each draft version the model counts.
struct Rig {
    parent: App,
    view: crate::tab_model::ViewId,
    /// Version → the field's draft text; version 0 is no draft.
    texts: Vec<String>,
    phase: i64,
    older: i64,
    restored: i64,
    token: Option<NativeUpdateSafetyToken>,
    pending: Option<RestoreManifest>,
    successor: Option<App>,
    model: Model,
}

impl Rig {
    /// A windowed outgoing App — its successor reopens native tabs, so its
    /// drafts ride — with Settings open on the page that edits [`FIELD`].
    fn new() -> Self {
        let mut parent = App::headless_for_test();
        parent.carry_drafts_as_windowed_for_test();
        assert!(parent.open_settings_tab(SettingsRoute::TextFonts));
        let (_, view) = parent.active_native_view(wid()).expect("Settings tab");
        Self {
            parent,
            view,
            texts: vec![String::new()],
            phase: 0,
            older: 0,
            restored: 0,
            token: None,
            pending: None,
            successor: None,
            model: native_update_settings_draft_carry_model(),
        }
    }

    fn version_of(&self, text: &str) -> i64 {
        let index = self
            .texts
            .iter()
            .position(|known| known == text)
            .unwrap_or_else(|| panic!("a draft no version names: {text:?}"));
        i64::try_from(index).unwrap()
    }

    /// The outgoing view's draft of [`FIELD`], as the update would carry it.
    fn parent_drafts(&self) -> CarriedFieldDrafts {
        let Some(AppViewState::Settings(state)) = self.parent.native_runtime.view_state(self.view)
        else {
            panic!("the outgoing Settings view is installed");
        };
        state.carried_field_drafts()
    }

    fn successor_draft(&self) -> Option<String> {
        let successor = self.successor.as_ref()?;
        let (_, view) = successor.active_native_view(wid())?;
        let Some(AppViewState::Settings(state)) = successor.native_runtime.view_state(view) else {
            return None;
        };
        state
            .carried_field_drafts()
            .carried
            .into_iter()
            .find(|draft| draft.key == FIELD)
            .map(|draft| draft.text)
    }

    fn successor_said_a_loss(&self) -> bool {
        self.successor
            .as_ref()
            .is_some_and(|successor| drafts_loss_row(successor).is_some())
    }

    /// Every model variable, read from real state.
    fn project(&self) -> State {
        let mut state = self.model.init_state();
        let draft = self
            .parent_drafts()
            .carried
            .into_iter()
            .find(|draft| draft.key == FIELD)
            .map_or(0, |draft| self.version_of(&draft.text));
        state.insert("draft", draft);
        state.insert("phase", self.phase);
        let carried = self
            .pending
            .as_ref()
            .and_then(|layout| settings_leaf(layout).cloned())
            .and_then(|leaf| {
                leaf.settings_drafts
                    .into_iter()
                    .find(|draft| draft.key == FIELD)
            })
            .map_or(0, |draft| self.version_of(&draft.text));
        state.insert("carried", carried);
        state.insert("older", self.older);
        state.insert("restored", self.restored);
        let succ = if self.successor_said_a_loss() {
            SURFACED
        } else {
            self.successor_draft()
                .map_or(0, |text| self.version_of(&text))
        };
        state.insert("succ", succ);
        state
    }

    /// The real step `op` IS the model's `actions`, fired in order from the
    /// state it started in, and the invariant holds after it.
    fn step(&mut self, label: &str, actions: &[&str], op: impl FnOnce(&mut Self)) {
        let before = self.project();
        op(self);
        let after = self.project();
        let mut expected = before.clone();
        for action in actions {
            assert!(
                self.model.fire(action, &mut expected),
                "{label}: the model cannot fire {action} from {expected:?} (real: {before:?} -> \
                 {after:?})"
            );
        }
        assert_eq!(
            after, expected,
            "{label}: the real step is not {actions:?} (from {before:?})"
        );
        for invariant in &self.model.invariants {
            assert!(
                self.model.check_invariant(invariant.name, &after),
                "{label}: {} broken at {after:?}",
                invariant.name
            );
        }
    }

    /// Type `text` into [`FIELD`] of the outgoing Settings view, as the
    /// person does: focus the field, select what is there, type over it.
    fn type_draft(&mut self, label: &str, text: &str) {
        let text = text.to_string();
        self.step(label, &["Type"], |rig| {
            type_into_field(&mut rig.parent, rig.view, &text);
            rig.texts.push(text.clone());
        });
    }

    /// The real preflight must BE the model's Start guard, and the close
    /// barrier it runs behind must let a carried draft through.
    fn start(&mut self, label: &str) -> bool {
        let before = self.project();
        let barrier = self
            .parent
            .prepare_all_native_shutdown(
                self.parent.update_close_scope(),
                crate::app_tabs::ClosePreflightVisibility::Quiet,
            )
            .expect("the close barrier runs");
        let real = self.parent.revalidate_native_update_safety();
        assert_eq!(
            barrier && real.is_ok(),
            self.model.action_enabled("Start", &before),
            "{label}: the real preflight is not the model's Start guard at {before:?} \
             (barrier {barrier}, preflight {:?})",
            real.as_ref().err()
        );
        let Ok(token) = real else {
            return false;
        };
        assert_eq!(
            token.carried_settings_drafts(),
            usize::from(before["draft"] > 0),
            "{label}: the token counts the carried draft"
        );
        self.step(label, &["Start"], |rig| {
            rig.token = Some(token);
            rig.phase = 1;
        });
        true
    }

    fn stand_down(&mut self, label: &str) {
        self.step(label, &["StandDown"], |rig| {
            rig.token = None;
            rig.phase = 0;
        });
    }

    /// The seamless lane to a successor at least this build (`older == false`)
    /// or older: the real lane gate must BE the model's guard, and an admitted
    /// park captures the layout exactly as the update's park does.
    fn park(&mut self, label: &str, older: bool) -> bool {
        let before = self.project();
        let action = if older { "ParkOlder" } else { "Park" };
        let target = if older { RUNNING - 1 } else { RUNNING };
        let refusal = lane_carry_refusal(
            self.token.as_ref().expect("a started attempt"),
            ApplyLane::Seamless,
            RUNNING,
            target,
        );
        assert_eq!(
            refusal.is_none(),
            self.model.action_enabled(action, &before),
            "{label}: the real lane gate is not the model's {action} guard at {before:?}: \
             {refusal:?}"
        );
        if let Some(refusal) = refusal {
            assert!(
                refusal.starts_with(crate::app_native::SETTINGS_DRAFTS_BLOCK),
                "{label}: the refusal is the person-facing shape: {refusal}"
            );
            assert!(
                interp::with_buggy(&self.model, 3).action_enabled(action, &before),
                "{label}: NEGATIVE CONTROL: handing the draft to an older build (Buggy = 3) \
                 would have parked here"
            );
            return false;
        }
        self.step(label, &[action], |rig| {
            rig.pending = Some(rig.parent.capture_handoff_layout());
            rig.older = i64::from(older);
            rig.phase = 2;
        });
        true
    }

    /// The cold lane: the real gate must BE the model's `ColdExec` guard. An
    /// admitted exec replaces the app with one that reopens nothing.
    fn cold_exec(&mut self, label: &str) -> bool {
        let before = self.project();
        let refusal = lane_carry_refusal(
            self.token.as_ref().expect("a started attempt"),
            ApplyLane::Cold,
            RUNNING,
            RUNNING,
        );
        assert_eq!(
            refusal.is_none(),
            self.model.action_enabled("ColdExec", &before),
            "{label}: the real cold gate is not the model's ColdExec guard at {before:?}: \
             {refusal:?}"
        );
        if let Some(refusal) = refusal {
            assert!(
                refusal.starts_with(crate::app_native::SETTINGS_DRAFTS_BLOCK),
                "{label}: the refusal is the person-facing shape: {refusal}"
            );
            assert!(
                interp::with_buggy(&self.model, 2).action_enabled("ColdExec", &before),
                "{label}: NEGATIVE CONTROL: a cold lane that spends the carried token \
                 (Buggy = 2) would have exec'd here"
            );
            return false;
        }
        self.step(label, &["ColdExec"], |rig| {
            // The exec'd build restores no layout: its Settings, if any, is a
            // fresh view.
            let mut successor = App::headless_for_test();
            assert!(successor.open_settings_tab(SettingsRoute::TextFonts));
            rig.successor = Some(successor);
            rig.restored = 1;
            rig.phase = 3;
        });
        true
    }

    /// The successor reads the layout the park captured off the real wire —
    /// `wire` may rewrite the bytes, as a damaged sidecar would be — and
    /// restores the Settings leaf exactly as the update's successor does. An
    /// older successor's struct has no `settings_drafts`: it reads the same
    /// layout without them.
    fn successor_restores(&mut self, label: &str, action: &str, wire: impl Fn(String) -> String) {
        let wire = wire(
            self.pending
                .as_ref()
                .expect("a parked layout")
                .to_toml()
                .expect("the layout serializes"),
        );
        let mut layout = RestoreManifest::from_toml(&wire)
            .expect("the successor parses the layout it is handed");
        if self.older == 1 {
            for_each_settings_leaf(&mut layout, &mut |leaf| leaf.settings_drafts.clear());
        }
        self.step(label, &[action], move |rig| {
            let mut successor = App::headless_for_test();
            successor.incoming_handoff_pending = true;
            successor.restore_into_window(wid(), settings_only_layout(&layout));
            // The end of the restore, as `apply_pending_restore` ends it: only
            // now is what did not survive said.
            successor.settle_carried_settings_drafts();
            let (instance, _) = successor
                .active_native_view(wid())
                .expect("the successor reopened the Settings leaf");
            assert_eq!(
                successor.native_runtime.app(instance).map(|app| app.kind()),
                Some(AppKind::Settings),
                "{label}: a carry is never what makes the leaf a Recovery tab"
            );
            rig.successor = Some(successor);
            rig.restored = 1;
        });
    }

    /// The real Commit — the layout comparison and the native revalidation —
    /// must BE the model's Commit guard.
    fn commit(&mut self, label: &str) -> bool {
        let before = self.project();
        let pending = self.pending.as_ref().expect("a parked layout");
        let same_layout = commit_layout_topology(&self.parent.capture_handoff_layout())
            == commit_layout_topology(pending);
        let real = same_layout && self.parent.revalidate_native_update_safety().is_ok();
        assert_eq!(
            real,
            self.model.action_enabled("Commit", &before),
            "{label}: the real Commit check is not the model's Commit guard at {before:?}"
        );
        if !real {
            assert!(
                interp::with_buggy(&self.model, 1).action_enabled("Commit", &before),
                "{label}: NEGATIVE CONTROL: the blind Commit (Buggy = 1) would have committed \
                 here"
            );
            return false;
        }
        self.step(label, &["Commit"], |rig| rig.phase = 3);
        true
    }

    fn rollback(&mut self, label: &str) {
        self.step(label, &["Rollback"], |rig| {
            rig.successor = None;
            rig.pending = None;
            rig.token = None;
            rig.older = 0;
            rig.restored = 0;
            rig.phase = 0;
        });
    }

    fn successor(&self) -> &App {
        self.successor.as_ref().expect("a successor")
    }
}

fn type_into_field(app: &mut App, view: crate::tab_model::ViewId, text: &str) {
    for event in [
        AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new(format!(
            "settings/control/{FIELD}"
        )))),
        AppEvent::TextInput(TextInputEvent::SelectAll),
        AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
    ] {
        app.dispatch_native_view_event(wid(), view, event).unwrap();
    }
}

/// The successor's loss row for carried Settings drafts, if it posted one.
fn drafts_loss_row(app: &App) -> Option<aterm_messages::Message> {
    app.messages
        .live_rows()
        .find(|row| row.msg.key.as_deref() == Some(crate::update_words::KEY_SETTINGS_DRAFTS))
        .map(|row| row.msg.clone())
}

fn settings_leaf(layout: &RestoreManifest) -> Option<&crate::restore::NativeLeafRestore> {
    layout.windows.iter().find_map(|window| {
        window.restored_tabs.iter().find_map(|tab| match &tab.root {
            RestoredSplitTree::Leaf {
                view: RestoredView::Native(native),
            } if native.restore_tag == "settings" => Some(native),
            _ => None,
        })
    })
}

fn for_each_settings_leaf(
    layout: &mut RestoreManifest,
    each: &mut dyn FnMut(&mut crate::restore::NativeLeafRestore),
) {
    for window in &mut layout.windows {
        for tab in &mut window.restored_tabs {
            if let RestoredSplitTree::Leaf {
                view: RestoredView::Native(native),
            } = &mut tab.root
                && native.restore_tag == "settings"
            {
                each(native);
            }
        }
    }
}

/// Window 0 of `manifest` with only its Settings tab: what the successor's
/// restore does with a Settings leaf, without spawning shells for the rest.
fn settings_only_layout(manifest: &RestoreManifest) -> crate::restore::WindowLayout {
    tab_only_layout(manifest, "settings")
}

fn tab_only_layout(manifest: &RestoreManifest, restore_tag: &str) -> crate::restore::WindowLayout {
    let window = manifest.windows.first().expect("window 0");
    let tab = window
        .restored_tabs
        .iter()
        .find(|tab| {
            matches!(
                &tab.root,
                RestoredSplitTree::Leaf {
                    view: RestoredView::Native(native),
                } if native.restore_tag == restore_tag
            )
        })
        .unwrap_or_else(|| panic!("the capture holds the {restore_tag} leaf"))
        .clone();
    crate::restore::WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        show: crate::restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        restored_tabs: vec![tab],
    }
}

/// THE ROUND TRIP: a Settings draft typed and never saved rides the update.
/// The close barrier lets it through and the preflight is Ready (before plan
/// P2-2 both refused: "Review Settings Drafts"); the successor's view holds the
/// text, still unsaved — its tab dirty, its close held for review — and says
/// nothing was lost.
#[test]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateSettingsDraftCarry",
    action = "Type",
    reason = "The person's keystroke: every Settings text reducer edits the draft, and each \
              schedule here drives the real view's focus, select-all and commit to make one."
)]
fn a_settings_draft_rides_the_update_and_reopens_unsaved() {
    let mut rig = Rig::new();
    rig.type_draft("type", "Carried Mono");
    assert!(
        rig.start("start"),
        "a carried draft no longer holds the update"
    );
    assert!(rig.park("park", false));
    assert!(
        rig.parent
            .capture_restore_manifest()
            .to_toml()
            .unwrap()
            .find("Carried Mono")
            .is_none(),
        "the DURABLE capture never carries unsaved Settings text"
    );
    rig.successor_restores("restore", "Restore", |wire| wire);
    assert_eq!(rig.successor_draft().as_deref(), Some("Carried Mono"));
    let successor = rig.successor();
    let tab = successor.windows[&wid()].tab_set.active().unwrap();
    assert!(
        tab.presentation.indicators.dirty && !tab.presentation.closable,
        "the successor's tab shows the draft unsaved from its first frame: {:?}",
        tab.presentation
    );
    assert!(drafts_loss_row(successor).is_none(), "nothing was lost");
    assert!(rig.commit("commit"));
    let successor = rig.successor.as_mut().unwrap();
    assert!(
        !successor
            .prepare_all_native_shutdown(
                CloseScope::AppQuit,
                crate::app_tabs::ClosePreflightVisibility::Quiet,
            )
            .unwrap(),
        "the draft is still unsaved in the successor: quitting it holds for review"
    );
}

/// A key typed into the field while the successor boots holds the Commit —
/// the successor reopened the older draft — and the next attempt carries the
/// newer one.
#[test]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateSettingsDraftCarry",
    action = "Rollback",
    reason = "The handoff's own step: a refused Commit rolls the overlap back and the outgoing \
              process keeps its view untouched; nothing of the Settings carry is undone, so the \
              step is the rig forgetting the killed successor and its layout."
)]
fn typing_into_settings_during_the_overlap_holds_the_commit() {
    let mut rig = Rig::new();
    rig.type_draft("type", "First Mono");
    assert!(rig.start("start"));
    assert!(rig.park("park", false));
    rig.successor_restores("restore", "Restore", |wire| wire);
    rig.type_draft("typed while the successor booted", "Second Mono");
    assert!(
        !rig.commit("commit refused"),
        "the successor holds the older draft"
    );
    rig.rollback("rollback");
    assert!(rig.start("second start"));
    assert!(rig.park("second park", false));
    rig.successor_restores("second restore", "Restore", |wire| wire);
    assert!(rig.commit("second commit"));
    assert_eq!(rig.successor_draft().as_deref(), Some("Second Mono"));
}

/// The cold lane reopens nothing and an older successor ignores the carry:
/// both refuse a token that carries a draft, in the person-facing shape, and
/// a desk with no draft takes either.
#[test]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateSettingsDraftCarry",
    action = "StandDown",
    reason = "The handoff's own step: a lane the gate refused starts nothing, so standing down \
              is dropping the token; the outgoing view is untouched."
)]
fn the_cold_lane_and_an_older_successor_refuse_a_carried_draft() {
    let mut rig = Rig::new();
    rig.type_draft("type", "Cold Mono");
    assert!(rig.start("start"));
    assert!(!rig.cold_exec("cold refused"));
    rig.stand_down("stand down");
    assert!(rig.start("start again"));
    assert!(!rig.park("older refused", true));
    rig.stand_down("stand down again");
    assert_eq!(rig.parent_drafts().carried.len(), 1, "the draft is kept");

    let mut clean = Rig::new();
    assert!(clean.start("clean start"));
    assert!(clean.park("a clean desk parks toward an older build", true));
    clean.successor_restores("older restore", "Restore", |wire| wire);
    assert!(clean.commit("clean commit"));

    let mut cold = Rig::new();
    assert!(cold.start("cold start"));
    assert!(cold.cold_exec("a clean desk execs cold"));
}

/// THE LENIENT CONSUMER: a carry the successor cannot read costs that draft —
/// never the layout, never the Settings leaf, never the handoff — and the
/// successor says so on glass.
#[test]
fn an_unreadable_carry_degrades_without_refusing() {
    let mut rig = Rig::new();
    rig.type_draft("type", "Damaged Mono");
    assert!(rig.start("start"));
    assert!(rig.park("park", false));
    let key_line = format!("key = \"{FIELD}\"");
    rig.successor_restores("unreadable restore", "RestoreUnreadable", |wire| {
        assert!(
            wire.contains(&key_line),
            "the wire carries the draft: {wire}"
        );
        wire.replace(&key_line, "key = 7")
    });
    assert_eq!(
        rig.successor_draft(),
        None,
        "nothing it could not read is reopened"
    );
    let row = drafts_loss_row(rig.successor()).expect("the loss is said on glass");
    assert_eq!(row.title, "Couldn't reopen a Settings draft");
    assert!(
        row.detail
            .iter()
            .any(|line| line == "1 draft could not be read"),
        "{:?}",
        row.detail
    );
    assert!(
        rig.commit("commit"),
        "a successor that said its loss commits"
    );
}

/// A draft for a row the successor's build does not edit as text is said on
/// glass WITH its text, so it can be typed again; drafts it can take land.
#[test]
fn a_draft_the_successor_cannot_reopen_is_said_with_its_text() {
    let mut rig = Rig::new();
    rig.type_draft("type", "Kept Mono");
    let mut layout = rig.parent.capture_handoff_layout();
    for_each_settings_leaf(&mut layout, &mut |leaf| {
        leaf.settings_drafts.push(SettingsDraftRestore {
            key: "no_such_row".to_string(),
            text: "words to keep".to_string(),
        });
    });
    let layout = RestoreManifest::from_toml(&layout.to_toml().unwrap()).unwrap();
    let mut successor = App::headless_for_test();
    successor.restore_into_window(wid(), settings_only_layout(&layout));
    successor.settle_carried_settings_drafts();
    let row = drafts_loss_row(&successor).expect("the loss is said on glass");
    assert_eq!(row.detail[0], "no_such_row: words to keep");
    let (_, view) = successor.active_native_view(wid()).unwrap();
    let Some(AppViewState::Settings(state)) = successor.native_runtime.view_state(view) else {
        panic!("the Settings leaf reopened");
    };
    assert_eq!(
        state.carried_field_drafts().carried,
        vec![SettingsDraftRestore {
            key: FIELD.to_string(),
            text: "Kept Mono".to_string(),
        }],
        "the draft it can take landed"
    );
}

/// A successor that reopens nothing — a headless one here, the Windows replace
/// alike — keeps the pre-carry hold, word for word, and so does the close
/// barrier: the draft would close with the process.
#[test]
fn a_successor_that_reopens_nothing_keeps_the_settings_hold() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(SettingsRoute::TextFonts));
    let (_, view) = app.active_native_view(wid()).unwrap();
    type_into_field(&mut app, view, "Headless Mono");
    assert_eq!(app.update_close_scope(), CloseScope::Relaunch);
    assert!(
        !app.prepare_all_native_shutdown(
            app.update_close_scope(),
            crate::app_tabs::ClosePreflightVisibility::Quiet,
        )
        .unwrap()
    );
    let reasons = app
        .revalidate_native_update_safety()
        .err()
        .expect("the draft holds the update");
    assert!(
        reasons.contains(&format!(
            "{} 1 Settings view(s) have unsaved text",
            crate::app_native::SETTINGS_DRAFTS_BLOCK
        )),
        "{reasons:?}"
    );
}

/// A field mid-composition (an input method's marked text on screen, not yet
/// the field's) is HELD, not carried as its committed value — by the close
/// barrier under the carried scope and by the preflight.
#[test]
fn a_draft_still_being_composed_holds_the_update() {
    let mut rig = Rig::new();
    type_into_field(&mut rig.parent, rig.view, "Composed");
    rig.parent
        .dispatch_native_view_event(
            wid(),
            rig.view,
            AppEvent::TextInput(TextInputEvent::Preedit {
                text: "\u{3042}".to_string(),
                selection: None,
            }),
        )
        .unwrap();
    assert_eq!(rig.parent_drafts().held, 1);
    assert!(
        !rig.parent
            .prepare_all_native_shutdown(
                CloseScope::CarriedRelaunch,
                crate::app_tabs::ClosePreflightVisibility::Quiet,
            )
            .unwrap()
    );
    let reasons = rig.parent.revalidate_native_update_safety().err().unwrap();
    assert!(
        reasons.iter().any(
            |reason| reason.starts_with(crate::app_native::SETTINGS_DRAFTS_BLOCK)
                && reason.contains("still being composed")
        ),
        "{reasons:?}"
    );
}

/// The tally's lane rule and cap, without a window.
#[test]
fn the_settings_tally_rides_only_where_the_successor_reopens_and_within_the_cap() {
    let view = |text: &str| CarriedFieldDrafts {
        carried: vec![SettingsDraftRestore {
            key: FIELD.to_string(),
            text: text.to_string(),
        }],
        held: 0,
    };
    let rides = SettingsDraftTally::of([view("a"), CarriedFieldDrafts::default()], true, 1024);
    assert_eq!(
        (rides.carried_views, rides.carried_drafts, rides.held_views),
        (1, 1, 0)
    );
    let nowhere = SettingsDraftTally::of([view("a")], false, 1024);
    assert_eq!((nowhere.carried_drafts, nowhere.held_views), (0, 1));
    assert_eq!(nowhere.held_why, None, "the pre-carry words, unchanged");
    let over = SettingsDraftTally::of([view(&"x".repeat(600)), view(&"y".repeat(600))], true, 1024);
    assert_eq!(
        (over.carried_drafts, over.held_views),
        (0, 2),
        "past the cap nothing rides: {over:?}"
    );
    assert!(over.held_why.unwrap().contains("more than the 1 KiB"));
}

/// Every Settings row fits one view's carry: a real capture never meets the
/// per-view cap the layout's sanitize enforces.
#[test]
fn every_settings_row_fits_one_views_carry() {
    let rows = crate::prefs::editable_fields(&crate::app_config::Config::default()).len();
    assert!(
        rows <= crate::restore::MAX_SETTINGS_DRAFTS_PER_VIEW,
        "{rows} Settings rows exceed the {} drafts one view carries",
        crate::restore::MAX_SETTINGS_DRAFTS_PER_VIEW
    );
}

/// A DOCUMENT WHOSE SAVE FAILED rides the update too (plan P2-2). Before, any
/// failed checkpoint held the update ("Retry: … previously failed") even
/// though its unsaved text was durable in the draft journal the successor
/// replays. Now the seamless preflight is Ready and the successor reopens the
/// editor holding the text, unsaved; the cold lane — which reopens nothing —
/// and a successor that reopens nothing still hold it.
#[test]
fn a_document_whose_save_failed_rides_the_seamless_update_only() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-failed-checkpoint-carry-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    // One file and one journal directory per App under test, so the headless
    // case never reopens the windowed case's journal.
    let desk = |name: &str| {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("notes.md");
        std::fs::write(&file, "on disk\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&file).unwrap();
        (file, uri, root.join("drafts"))
    };

    let failed_save = |windowed: bool, uri: &str, journals: &std::path::Path| {
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(journals.to_path_buf());
        if windowed {
            app.carry_drafts_as_windowed_for_test();
        }
        app.open_document_tab(AppKind::Editor, uri).unwrap();
        let (instance, view) = app.active_native_view(wid()).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid(),
            AppEvent::TextInput(TextInputEvent::Commit("unsaved ".into())),
        )
        .unwrap();
        // The real reducer path for a save the disk refused: begin it, then
        // complete it with the host's failure.
        let snapshot = app.document_store.snapshot(document).unwrap();
        let pending = app.native_documents.persistence.begin(&snapshot).unwrap();
        app.native_runtime.set_document_saving(document, true);
        let error = app
            .finish_native_document_save(
                document,
                view,
                pending.plan.generation,
                pending.plan.bytes,
                crate::native_document_io::AtomicSaveResult::Failed {
                    stage: crate::native_document_io::AtomicSaveStage::WriteTemporary,
                    message: "No space left on device".to_string(),
                },
                None,
            )
            .unwrap_err();
        assert!(error.contains("No space left on device"), "{error}");
        assert!(matches!(
            app.document_store.phase(document),
            Some(crate::document_store::DocumentPhase::Blocked { .. })
        ));
        (app, document)
    };

    let (file, uri, journals) = desk("windowed");
    let (mut app, document) = failed_save(true, &uri, &journals);
    app.dispatch_native_event(
        wid(),
        AppEvent::TextInput(TextInputEvent::Commit("more ".into())),
    )
    .unwrap();
    assert_eq!(
        &*app.document_store.snapshot(document).unwrap().text,
        "unsaved on disk\n",
        "the failed checkpoint froze the outgoing editor at the head it could not save"
    );
    let token = app
        .revalidate_native_update_safety()
        .expect("a failed checkpoint whose draft is journaled no longer holds the update");
    assert_eq!((token.failed_checkpoints(), token.carried_drafts()), (1, 1));
    let cold = lane_carry_refusal(&token, ApplyLane::Cold, RUNNING, RUNNING)
        .expect("the cold lane reopens no editor");
    assert!(
        cold.starts_with(crate::app_native::DIRTY_DOCUMENTS_BLOCK),
        "{cold}"
    );
    let clean_but_failed = NativeUpdateSafetyToken::carrying_for_test(0, 0, 1);
    let cold = lane_carry_refusal(&clean_but_failed, ApplyLane::Cold, RUNNING, RUNNING).unwrap();
    assert!(
        cold.starts_with(crate::app_native::FAILED_CHECKPOINTS_BLOCK),
        "the cold lane keeps the failed-checkpoint hold: {cold}"
    );
    assert_eq!(
        lane_carry_refusal(&token, ApplyLane::Seamless, RUNNING, RUNNING),
        None
    );

    let layout = app.capture_handoff_layout();
    let mut successor = App::headless_for_test();
    successor.use_document_journal_root_for_test(journals);
    successor.incoming_handoff_pending = true;
    successor.restore_into_window(wid(), tab_only_layout(&layout, "editor"));
    let (instance, _) = successor.active_native_view(wid()).unwrap();
    let reopened = successor.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        &*successor.document_store.snapshot(reopened).unwrap().text,
        "unsaved on disk\n",
        "the successor holds the text the failed save could not write"
    );
    assert_eq!(successor.document_store.dirty(reopened), Some(true));
    successor
        .dispatch_native_event(
            wid(),
            AppEvent::TextInput(TextInputEvent::Commit("more ".into())),
        )
        .unwrap();
    assert!(
        successor
            .document_store
            .snapshot(reopened)
            .unwrap()
            .text
            .contains("more "),
        "the successor's editor takes edits again, to be saved there"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "on disk\n",
        "the file is untouched"
    );
    drop((app, document, successor));

    let (_, uri, journals) = desk("headless");
    let (headless, _) = failed_save(false, &uri, &journals);
    let reasons = headless
        .revalidate_native_update_safety()
        .err()
        .expect("a successor that reopens nothing keeps the hold");
    assert!(
        reasons.iter().any(|reason| reason
            == &format!(
                "{} 1 document checkpoint(s) previously failed",
                crate::app_native::FAILED_CHECKPOINTS_BLOCK
            )),
        "{reasons:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
