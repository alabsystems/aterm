// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Modified, Manual, Reset, Undo, and what a change writes to aterm.toml.

use super::*;

/// A Settings view over a real config service holding `source`.
pub(super) struct Lab {
    runtime: NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    service: VersionedConfigService,
}

impl Lab {
    pub(super) fn new(source: &str) -> Self {
        let service = VersionedConfigService::new(source.to_string()).expect("valid fixture");
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        **state = SettingsViewState::from_snapshot(&service.snapshot()).expect("projects");
        Self {
            runtime,
            instance,
            view,
            service,
        }
    }

    pub(super) fn state(&self) -> &SettingsViewState {
        let Some(AppViewState::Settings(state)) = self.runtime.view_state(self.view) else {
            panic!("Settings view");
        };
        state
    }

    pub(super) fn state_mut(&mut self) -> &mut SettingsViewState {
        let Some(AppViewState::Settings(state)) = self.runtime.view_state_mut(self.view) else {
            panic!("Settings view");
        };
        state
    }

    /// Dispatch `event` to the view.
    pub(super) fn dispatch(&mut self, event: AppEvent) {
        self.runtime
            .dispatch(self.instance, self.view, event)
            .unwrap();
    }

    pub(super) fn text(&self) -> String {
        self.service.snapshot().text.to_string()
    }

    /// Invoke `action`; the patch it emits, if any.
    pub(super) fn invoke(
        &mut self,
        action: &str,
        value: Option<SemanticInput>,
    ) -> Option<(ConfigPatch, OperationId)> {
        let outcome = self
            .runtime
            .dispatch(
                self.instance,
                self.view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(action),
                    value,
                }),
            )
            .unwrap();
        outcome.effects.iter().find_map(|effect| match effect {
            AppEffect::ConfigPatch { patch, reply } => Some((patch.clone(), reply.operation)),
            _ => None,
        })
    }

    /// Invoke `action` and carry the patch it emits through the real service,
    /// as the host does: the result, then the new snapshot.
    pub(super) fn commit(&mut self, action: &str, value: Option<SemanticInput>) -> ConfigPatch {
        let (patch, operation) = self
            .invoke(action, value)
            .unwrap_or_else(|| panic!("{action} writes aterm.toml"));
        let (snapshot, undo) = apply_real_config_patch(&mut self.service, &patch);
        self.runtime
            .dispatch(
                self.instance,
                self.view,
                AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo,
                    },
                },
            )
            .unwrap();
        self.runtime
            .dispatch(self.instance, self.view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
        patch
    }

    /// Dispatch `event` and carry the patch it emits through the real
    /// service, as [`Self::commit`] does for an action.
    pub(super) fn commit_event(&mut self, event: AppEvent) -> ConfigPatch {
        let outcome = self
            .runtime
            .dispatch(self.instance, self.view, event)
            .unwrap();
        let (patch, operation) = outcome
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { patch, reply } => Some((patch.clone(), reply.operation)),
                _ => None,
            })
            .expect("the event writes aterm.toml");
        let (snapshot, undo) = apply_real_config_patch(&mut self.service, &patch);
        self.runtime
            .dispatch(
                self.instance,
                self.view,
                AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo,
                    },
                },
            )
            .unwrap();
        self.runtime
            .dispatch(self.instance, self.view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
        patch
    }

    /// [`Self::commit`], with the new snapshot reaching the view before the
    /// patch's result does, as the host may deliver them.
    fn commit_snapshot_first(&mut self, action: &str, value: Option<SemanticInput>) {
        let (patch, operation) = self
            .invoke(action, value)
            .unwrap_or_else(|| panic!("{action} writes aterm.toml"));
        let (snapshot, undo) = apply_real_config_patch(&mut self.service, &patch);
        let revision = snapshot.revision;
        self.runtime
            .dispatch(self.instance, self.view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
        self.runtime
            .dispatch(
                self.instance,
                self.view,
                AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Applied { revision, undo },
                },
            )
            .unwrap();
    }

    /// Press Undo (or Cmd-Z, with `event`) and carry it through the real
    /// service; `None` when nothing was offered to undo.
    fn undo_with(&mut self, event: AppEvent) -> Option<()> {
        let outcome = self
            .runtime
            .dispatch(self.instance, self.view, event)
            .unwrap();
        let (token, operation) = outcome.effects.iter().find_map(|effect| match effect {
            AppEffect::ConfigUndo { token, reply } => Some((*token, reply.operation)),
            _ => None,
        })?;
        let result = self
            .service
            .undo(crate::native_config_service::UndoToken::from_stored(token));
        let ConfigPatchResult::Applied { snapshot, undo } = result else {
            panic!("the service takes the Undo: {result:?}");
        };
        self.runtime
            .dispatch(
                self.instance,
                self.view,
                AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo: Some(undo.get()),
                    },
                },
            )
            .unwrap();
        self.runtime
            .dispatch(self.instance, self.view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
        Some(())
    }

    fn undo(&mut self) -> Option<()> {
        self.undo_with(AppEvent::Action(ActionInvocation {
            id: ActionId::new("settings/undo"),
            value: None,
        }))
    }

    fn undo_offered(&self) -> bool {
        self.compile(1_200.0, 820.0)
            .semantic(&UiKey::new("settings/undo"))
            .is_some()
    }

    pub(super) fn compile(&self, width: f32, height: f32) -> crate::native_ui::CompiledUi {
        self.compile_in(&view_cx_at(width, height))
    }

    /// [`Self::compile`] in host context `cx`.
    pub(super) fn compile_in(&self, cx: &ViewCx<'_>) -> crate::native_ui::CompiledUi {
        compile_settings_view(&self.runtime, self.instance, self.view, cx)
    }
}

pub(super) fn rect_of(compiled: &crate::native_ui::CompiledUi, key: &str) -> LogicalRect {
    compiled
        .semantic(&UiKey::new(key))
        .unwrap_or_else(|| panic!("{key} on glass"))
        .rect
}

/// An Undo pressed on a Cursor Styles row puts the keyboard, and so the
/// preview, on the style it restores: the row the change was made on is no
/// longer tried once its change is gone.
#[test]
fn undo_takes_the_cursor_style_highlight_to_the_restored_style() {
    let mut lab = Lab::new("cursor_trail_style = \"comet\"\n");
    lab.state_mut().navigate(SettingsRoute::CursorStyles);
    let _ = lab.compile(1_074.0, 684.0);
    let water = UiKey::new(cursor_style_row_key("water"));
    lab.dispatch(AppEvent::FocusChanged(Some(water.clone())));
    let _ = lab.compile(1_074.0, 684.0);
    lab.commit(water.as_str(), None);
    assert_eq!(saved_cursor_style(lab.state()), "water");
    lab.undo_with(AppEvent::TextInput(TextInputEvent::Undo))
        .expect("Undo is offered");
    let _ = lab.compile(1_074.0, 684.0);
    let state = lab.state();
    assert_eq!(saved_cursor_style(state), "comet");
    assert_eq!(
        state.common.last_focus.as_ref().map(UiKey::as_str),
        Some(cursor_style_row_key("comet").as_str())
    );
    assert_eq!(cursor_style_candidate(state).as_deref(), Some("comet"));
}

/// Keyword kitties off, then on again (its default): aterm.toml is back to
/// what it was, with no empty `[sparkle_words.feline]` left for Modified to
/// list as a change nothing can reset.
#[test]
fn a_switch_turned_back_leaves_no_empty_table_and_nothing_modified() {
    let mut lab = Lab::new("font_px = 13.0\n");
    let key = KEYWORD_KITTIES_KEY;
    lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Bool(false)),
    );
    assert!(
        lab.text().contains("[sparkle_words.feline]"),
        "{}",
        lab.text()
    );
    lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Bool(true)),
    );
    assert_eq!(lab.text(), "font_px = 13.0\n");

    lab.state_mut().navigate(SettingsRoute::Modified);
    assert!(authored_manual_override_keys(lab.state()).is_empty());
    assert_eq!(
        settings_field_result_count(lab.state()),
        1,
        "only Font size"
    );
}

/// A known table with nothing in it is where settings would live, not a
/// change: Modified lists none of them.
#[test]
fn modified_never_lists_an_empty_table() {
    let lab = Lab::new("[update]\n[machine]\n[sparkle_words.feline]\n[matrix_rain]\n");
    assert!(
        authored_manual_override_keys(lab.state()).is_empty(),
        "{:?}",
        authored_manual_override_keys(lab.state())
    );
}

/// A switch turned back to its default removes its key: no `= true` left
/// behind, no "Changed", nothing on Modified.
#[test]
fn a_switch_turned_back_to_its_default_removes_its_key() {
    let mut lab = Lab::new("");
    let key = prefs::EDIT_CURSOR_BLINK;
    lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Bool(false)),
    );
    assert!(lab.state().is_explicit(key));
    let patch = lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Bool(true)),
    );
    assert_eq!(
        patch.edits[0].value, None,
        "on is the default: the key goes"
    );
    assert_eq!(lab.text(), "");
    assert!(!lab.state().is_explicit(key));
    assert_eq!(
        SettingsState::display_value(lab.state().field_by_key(key).unwrap()),
        "true"
    );
    // A click with no value toggles back the same way.
    lab.commit(&format!("settings/set/{key}"), None);
    let patch = lab.commit(&format!("settings/set/{key}"), None);
    assert_eq!(patch.edits[0].value, None);
    assert_eq!(lab.text(), "");
}

/// A slider moved back to its default removes its key, as a switch does: no
/// `window_padding = 12.0` left behind, no "Changed", nothing on Modified —
/// by a set value (a drag or click) and by the arrow keys alike. CONTROL: a
/// value off the default is written.
#[test]
fn a_slider_moved_back_to_its_default_removes_its_key() {
    let key = prefs::EDIT_WINDOW_PADDING;
    let default = f64::from(crate::PAD_LOGICAL_PX);
    let mut lab = Lab::new("");
    let patch = lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Number(default + 1.0)),
    );
    assert!(patch.edits[0].value.is_some(), "off the default: written");
    assert!(lab.state().is_explicit(key));
    let patch = lab.commit(
        &format!("settings/set/{key}"),
        Some(SemanticInput::Number(default)),
    );
    assert_eq!(patch.edits[0].value, None, "the default: the key goes");
    assert_eq!(lab.text(), "");
    assert!(!lab.state().is_explicit(key));

    // The arrow keys on the focused slider, Right then Left.
    lab.state_mut().navigate(SettingsRoute::WindowTabs);
    lab.state_mut().common.last_focus = Some(UiKey::new(format!("settings/control/{key}")));
    let patch = lab.commit_event(AppEvent::TextInput(TextInputEvent::Right { extend: false }));
    assert!(patch.edits[0].value.is_some(), "one step off: written");
    assert!(lab.state().is_explicit(key), "{}", lab.text());
    let patch = lab.commit_event(AppEvent::TextInput(TextInputEvent::Left { extend: false }));
    assert_eq!(
        patch.edits[0].value, None,
        "back on the default: the key goes"
    );
    assert_eq!(lab.text(), "");
    assert!(!lab.state().is_explicit(key));
}

/// A row whose key is written in aterm.toml says Changed and has a Reset on
/// its own page, after its control; a row at its default has neither. Reset
/// removes exactly that key.
#[test]
fn a_changed_row_offers_reset_on_its_own_page() {
    let mut lab = Lab::new("cursor_blink = false\nscrollback_lines = 5000\n");
    lab.state_mut().navigate(SettingsRoute::CursorMotion);
    let compiled = lab.compile(1_200.0, 820.0);
    let label = &compiled
        .semantic(&UiKey::new("settings/label/cursor_blink"))
        .unwrap()
        .label;
    assert!(label.contains("Changed"), "{label}");
    let reset = rect_of(&compiled, "settings/reset/cursor_blink");
    let control = rect_of(&compiled, "settings/control/cursor_blink");
    assert!(
        reset.x >= control.x + control.width,
        "Reset follows the switch"
    );
    assert!(
        compiled
            .semantic(&UiKey::new("settings/reset/cursor_momentum_glow"))
            .is_none(),
        "a default row offers no Reset"
    );
    let patch = lab.commit("settings/reset/cursor_blink", None);
    assert_eq!(
        patch
            .edits
            .iter()
            .map(|edit| (edit.key.as_str(), edit.value.as_deref()))
            .collect::<Vec<_>>(),
        [("cursor_blink", None)]
    );
    assert_eq!(lab.text(), "scrollback_lines = 5000\n");
}

/// Compact rows keep Reset on the trailing side as every other row: after
/// the control on a one-line row, and ending the label line over a stacked
/// row's control, so the control keeps its whole width.
#[test]
fn compact_reset_follows_the_switch() {
    let mut lab = Lab::new("cursor_blink = false\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    for (width, height) in [(420.0, 820.0), (724.0, 490.0)] {
        let compiled = lab.compile(width, height);
        let reset = rect_of(&compiled, "settings/reset/cursor_blink");
        let control = rect_of(&compiled, "settings/control/cursor_blink");
        let stacked = reset.y + reset.height <= control.y + 0.01;
        assert!(
            if stacked {
                (reset.right() - control.right()).abs() < 0.01
            } else {
                reset.x >= control.x + control.width
            },
            "{width}x{height}: reset {reset:?} control {control:?}"
        );
    }
}

/// The special pages' switches follow the same rule: Software Update's and
/// Packages' rows say Changed and offer Reset while their key is written.
#[test]
fn special_page_switches_offer_reset_while_written() {
    let mut lab = Lab::new("[update]\nauto_apply = false\n[packages]\nauto_install = false\n");
    for (route, key, quiet) in [
        (
            SettingsRoute::SoftwareUpdate,
            "update.auto_apply",
            prefs::EDIT_UPDATE_ENABLED,
        ),
        (
            SettingsRoute::Packages,
            prefs::EDIT_PACKAGES_AUTO_INSTALL,
            prefs::EDIT_PACKAGES_ENABLED,
        ),
    ] {
        lab.state_mut().navigate(route);
        let compiled = lab.compile(1_200.0, 1_400.0);
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/reset/{key}")))
                .is_some(),
            "{route:?} offers Reset for {key}"
        );
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/reset/{quiet}")))
                .is_none(),
            "{route:?} offers none for {quiet}"
        );
    }
    lab.commit("settings/reset/update.auto_apply", None);
    assert_eq!(lab.text(), "[packages]\nauto_install = false\n");

    // Messages' one switch too.
    let key = prefs::EDIT_EXPLAIN_HEAVY_LOAD;
    let mut lab = Lab::new(&format!("{key} = false\n"));
    lab.state_mut().navigate(SettingsRoute::Messages);
    for (width, height) in [(1_200.0, 820.0), (420.0, 820.0)] {
        let compiled = lab.compile(width, height);
        let label = &compiled
            .semantic(&UiKey::new(format!("settings/label/{key}")))
            .unwrap()
            .label;
        assert!(label.contains("Changed"), "{label}");
        let reset = rect_of(&compiled, &format!("settings/reset/{key}"));
        let control = rect_of(&compiled, &format!("settings/control/{key}"));
        assert!(
            reset.x >= control.x + control.width,
            "{width}: Reset follows the switch"
        );
    }
    lab.commit(&format!("settings/reset/{key}"), None);
    assert_eq!(lab.text(), "");
    assert!(
        lab.compile(1_200.0, 820.0)
            .semantic(&UiKey::new(format!("settings/reset/{key}")))
            .is_none()
    );
}

/// The old shared word-effects switch is its own row on Modified, so a
/// toy's row is listed only when the toy's own key is written.
#[test]
fn modified_lists_the_old_word_effects_switch_as_its_own_row() {
    let mut lab = Lab::new("[sparkle_words]\nenabled = false\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(1_200.0, 820.0);
    assert!(
        compiled
            .semantic(&UiKey::new(format!(
                "settings/control/{SPARKLE_MASTER_KEY}"
            )))
            .is_some()
    );
    for toy in [SPARKLE_WORDS_KEY, KEYWORD_KITTIES_KEY] {
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/control/{toy}")))
                .is_none(),
            "{toy} is not written"
        );
    }
    lab.commit(&format!("settings/reset/{SPARKLE_MASTER_KEY}"), None);
    assert_eq!(lab.text(), "");
}

/// Modified lists its settings rows first, then every key only aterm.toml
/// holds as a row of ONE group: no card per key, no inner counter, and each
/// row opens Manual at its own key.
#[test]
fn modified_groups_the_keys_only_in_aterm_toml_in_one_list() {
    let source = "agents_auto_prime = false\ncursor_blink = false\nfuture_thing = 5\nkeybindings = { \"cmd+k\" = \"clear\" }\n[matrix_rain]\nenabled = true\n";
    let mut lab = Lab::new(source);
    lab.state_mut().navigate(SettingsRoute::Modified);
    let overrides = authored_manual_overrides(lab.state());
    assert_eq!(
        overrides
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        ["agents_auto_prime", "future_thing", "keybindings"]
    );
    let compiled = lab.compile(1_200.0, 1_600.0);
    let group = compiled
        .semantics
        .iter()
        .find(|node| node.label == MANUAL_ONLY_GROUP)
        .expect("one group of keys only in aterm.toml");
    for authored in &overrides {
        let row = compiled
            .semantic(&UiKey::new(format!(
                "settings/modified/manual/row/{}",
                key_fragment(&authored.key)
            )))
            .unwrap_or_else(|| panic!("{} has a row", authored.key));
        assert_eq!(row.parent.as_ref(), Some(&group.key), "{}", authored.key);
    }
    let blink = rect_of(&compiled, "settings/control/cursor_blink");
    assert!(blink.y < group.rect.y, "settings rows come first");
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.label.contains("Manual override") && !node.label.contains("of 3")),
        "no counter"
    );
    let unknown = compiled
        .semantic(&UiKey::new("settings/modified/manual/label/future-thing"))
        .unwrap();
    assert_eq!(
        unknown.label, "future_thing",
        "a key with no name shows the key"
    );
    assert_eq!(
        compiled
            .semantic(&UiKey::new(
                "settings/modified/manual/value/agents-auto-prime"
            ))
            .unwrap()
            .label,
        "Off",
        "a value in plain words"
    );
    let open = compiled
        .semantic(&UiKey::new("settings/modified/manual/open/keybindings"))
        .unwrap();
    let ordinal = overrides
        .iter()
        .position(|entry| entry.key == "keybindings")
        .unwrap();
    assert_eq!(
        open.action.as_ref().map(ActionId::as_str),
        Some(format!("settings/manual/open-override/{ordinal}").as_str())
    );
    assert_eq!(
        compiled
            .semantic(&UiKey::new("settings/results-range"))
            .unwrap()
            .label,
        "2 settings · 3 only in aterm.toml"
    );
}

/// Modified carries Reset All beside its count; it asks first, names how
/// many settings it resets, and is off when nothing would reset.
#[test]
fn modified_offers_reset_all_and_names_what_it_resets() {
    let mut lab = Lab::new("cursor_blink = false\ntheme = \"Nord\"\nfuture_thing = 5\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(1_200.0, 820.0);
    let button = compiled
        .semantic(&UiKey::new("settings/modified/reset-all"))
        .expect("Reset All on Modified");
    assert!(button.state.is_some_and(|state| state.enabled));
    assert!(
        lab.invoke("settings/reset-all", None).is_none(),
        "it asks first"
    );
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some(
            "Reset 2 settings to their defaults? Lists, maps, and other keys only in aterm.toml stay."
        )
    );
    lab.commit("settings/reset-all-confirm", None);
    assert_eq!(lab.text(), "future_thing = 5\n");

    let compiled = lab.compile(1_200.0, 820.0);
    let button = compiled
        .semantic(&UiKey::new("settings/modified/reset-all"))
        .unwrap();
    assert!(
        button.state.is_some_and(|state| !state.enabled),
        "nothing left for Reset All"
    );
    assert!(lab.invoke("settings/reset-all", None).is_none());
    assert!(!lab.state().reset_all_confirmation);
    // `future_thing` is still listed with its own Reset, so "nothing to
    // reset" would be false: the words say what Reset All leaves.
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Reset All leaves keys only in aterm.toml. Use Reset on each row.")
    );
    assert_eq!(
        button.label,
        "Reset All: keys only in aterm.toml stay; use Reset on each row"
    );
    // The palette's Reset All is off with the button.
    assert!(
        lab.runtime
            .commands(lab.instance, lab.view)
            .unwrap()
            .iter()
            .any(|command| command.id.as_str() == "settings/reset-all" && !command.enabled)
    );

    // Once nothing at all is changed, it says so.
    let mut empty = Lab::new("");
    empty.state_mut().navigate(SettingsRoute::Modified);
    assert!(empty.invoke("settings/reset-all", None).is_none());
    assert_eq!(empty.state().feedback.as_deref(), Some("Nothing to reset."));
}

/// Display face on Modified is a toggle per face, as on its own page: no
/// pop-up of raw mixes, and a toggle changes only that face.
#[test]
fn modified_display_face_is_a_toggle_per_face() {
    let mut lab = Lab::new("display_font = \"pixel+bubble\"\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    for (width, height) in [(1_200.0, 820.0), (420.0, 820.0)] {
        let compiled = lab.compile(width, height);
        for (id, on) in [("chunky", false), ("pixel", true), ("bubble", true)] {
            let toggle = compiled
                .semantic(&UiKey::new(format!("settings/display-face/{id}")))
                .unwrap_or_else(|| panic!("{id} toggle at {width}"));
            assert_eq!(toggle.value, SemanticValue::Bool(on), "{id}");
        }
        assert!(
            compiled.semantics.iter().all(|node| node
                .action
                .as_ref()
                .is_none_or(|action| action.as_str() != "settings/set/display_font")),
            "no pop-up of raw mixes"
        );
    }
    lab.commit("settings/display-face/chunky", None);
    assert_eq!(lab.text(), "display_font = \"pixel+bubble+chunky\"\n");
}

/// A Modified row too long for its line keeps its name beside the control
/// and puts the rest — when it applies, what Reset returns it to — on the
/// line under it, whole.
#[test]
fn a_long_modified_row_puts_its_facts_on_a_second_line() {
    let mut lab = Lab::new("[machine]\nuniversal_control = \"leave\"\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(900.0, 820.0);
    let key = prefs::EDIT_MACHINE_UNIVERSAL_CONTROL;
    let label = rect_of(&compiled, &format!("settings/label/{key}"));
    let detail = compiled
        .paint
        .iter()
        .filter(|node| {
            node.key
                .as_str()
                .starts_with(&format!("settings/detail/{key}/line-"))
        })
        .collect::<Vec<_>>();
    assert!(!detail.is_empty(), "the facts are under the name");
    assert!(detail.iter().all(|line| line.rect.y >= label.bottom()));
    let lines = detail
        .iter()
        .filter_map(|line| match &line.content {
            UiContent::Text(spec) => Some(spec.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    // When the change applies is always the second line's; the default is
    // on whichever line has the room, whole.
    assert!(
        lines.contains("Applies at the next ALab tools check"),
        "{lines}"
    );
    let visual = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == format!("settings/label/{key}/visual"))
        .and_then(|node| match &node.content {
            UiContent::Text(spec) => Some(spec.text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        lines.contains("Default: Off") || visual.contains("Default: Off"),
        "{visual} / {lines}"
    );
    assert!(
        !lines.contains('…') && !visual.contains('…'),
        "nothing cut off: {lines}"
    );
}

/// A Follow-system slot on Modified names its default as a theme: aterm's
/// own dark theme is called Default, so "Default: Default" would read as if
/// nothing had changed.
#[test]
fn a_modified_theme_slot_names_its_default_as_a_theme() {
    for source in [
        "theme = \"dark:Nord,light:GitHub Light\"\n",
        "theme = \"dark:Default,light:Default Light\"\n",
    ] {
        let mut lab = Lab::new(source);
        lab.state_mut().navigate(SettingsRoute::Modified);
        let compiled = lab.compile(1_074.0, 684.0);
        let label = |side: &str| {
            compiled
                .semantic(&UiKey::new(format!("settings/label/theme-slot/{side}")))
                .unwrap_or_else(|| panic!("the {side} slot is on Modified: {source}"))
                .label
                .clone()
        };
        for (side, appearance) in [
            ("dark", aterm_types::Appearance::Dark),
            ("light", aterm_types::Appearance::Light),
        ] {
            let label = label(side);
            // It still names the theme Reset returns the slot to.
            assert!(
                label.ends_with(&format!(
                    ": {}",
                    crate::app_config::unset_theme_name(appearance)
                )),
                "{side}: {label}"
            );
            assert!(!label.contains("Default: Default"), "{side}: {label}");
        }
    }
}

/// The status names the change, and Undo names what it put back — then
/// offers to undo that, named too.
#[test]
fn undo_names_the_change_and_what_it_restored() {
    let mut lab = Lab::new("theme = \"Dracula\"\n");
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_THEME),
        Some(SemanticInput::Text("Nord".to_string())),
    );
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Color theme set to Nord")
    );
    assert!(lab.undo_offered());
    lab.undo().expect("Undo is offered");
    assert_eq!(lab.text(), "theme = \"Dracula\"\n");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Color theme: Dracula")
    );
    assert!(lab.undo_offered(), "the Undo can itself be undone");
    assert_eq!(
        lab.undo_button_label().as_deref(),
        Some("Redo"),
        "and its button says so"
    );
    lab.undo().expect("and that is offered");
    assert_eq!(lab.text(), "theme = \"Nord\"\n");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Color theme: Nord")
    );

    // A Follow system pair reads as its two sides, not its raw value.
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_THEME),
        Some(SemanticInput::Text(
            "dark:Dracula,light:GitHub Light".to_string(),
        )),
    );
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Color theme set to Dracula (dark) and GitHub Light (light)")
    );
    lab.undo().expect("Undo is offered");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Color theme: Nord")
    );
    // No theme at all follows the system too, and says so.
    lab.commit(&format!("settings/reset/{}", prefs::EDIT_THEME), None);
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Color theme reset to Follow system")
    );
    lab.undo().expect("Undo is offered");
    // One theme on both sides is named once.
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_THEME),
        Some(SemanticInput::Text(
            "dark:Aterm Day,light:Aterm Day".to_string(),
        )),
    );
    let said = lab.state().feedback.clone().unwrap_or_default();
    assert_eq!(said.matches("Aterm Day").count(), 1, "{said}");
    lab.undo().expect("Undo is offered");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Color theme: Nord")
    );

    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK),
        Some(SemanticInput::Bool(false)),
    );
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Cursor blink turned off")
    );
}

/// Undo goes with the status that named its change: an unrelated message, a
/// new page, or a failed save takes the button and Cmd-Z with it, and the
/// earlier change stays.
#[test]
fn undo_goes_with_the_status_that_named_it() {
    let mut lab = Lab::new("");
    let blink = format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK);
    lab.commit(&blink, Some(SemanticInput::Bool(false)));
    assert!(lab.undo_offered());

    // Another message replaces the status: no Undo, and Cmd-Z does nothing.
    lab.state_mut().feedback = Some("Discarded 1 unsaved Settings draft.".to_string());
    assert!(!lab.undo_offered());
    assert!(
        lab.undo_with(AppEvent::TextInput(TextInputEvent::Undo))
            .is_none()
    );
    assert!(lab.undo().is_none());
    assert_eq!(lab.text(), "cursor_blink = false\n");

    // A new page clears it.
    lab.commit(&blink, Some(SemanticInput::Bool(true)));
    assert!(lab.undo_offered());
    lab.state_mut().navigate(SettingsRoute::Terminal);
    assert!(lab.state().last_undo.is_none());
    assert!(!lab.undo_offered());

    // While named, Cmd-Z is the same Undo.
    lab.commit(&blink, Some(SemanticInput::Bool(false)));
    lab.undo_with(AppEvent::TextInput(TextInputEvent::Undo))
        .expect("Cmd-Z undoes the named change");
    assert_eq!(lab.text(), "");
}

/// A value that cannot be saved writes nothing and offers no Undo; the row
/// says why in its own words, never its config key, and keeps the typed text
/// to correct. A good value then saves and clears the error.
#[test]
fn an_invalid_value_keeps_the_draft_and_says_why_under_its_row() {
    let mut lab = Lab::new("");
    let key = prefs::EDIT_SEARCH_HISTORY_LINES;
    let set = format!("settings/set/{key}");
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK),
        Some(SemanticInput::Bool(false)),
    );
    assert!(lab.undo_offered());
    lab.state_mut().navigate(SettingsRoute::Terminal);
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK),
        Some(SemanticInput::Bool(true)),
    );
    for (raw, why) in [
        ("lots", "must be a whole number, 0 or more"),
        ("-5", "must be 0 or more"),
        ("5000000000", "must be at most 4,294,967,295"),
        // Too long for i64 is still a whole number: it says which end it
        // passed, never that it is not a number.
        ("99999999999999999999", "must be at most 4,294,967,295"),
        ("-99999999999999999999", "must be 0 or more"),
        ("99999999999999999999x", "must be a whole number, 0 or more"),
    ] {
        assert!(
            lab.invoke(&set, Some(SemanticInput::Text(raw.to_string())))
                .is_none(),
            "{raw} writes nothing"
        );
        let state = lab.state();
        let label = state.field_by_key(key).unwrap().label;
        assert_eq!(state.field_inputs[key].value(), raw, "the typed text stays");
        let error = &state.field_errors[key];
        assert!(error.starts_with(label) && error.contains(why), "{error}");
        assert!(!error.contains(key), "no config key: {error}");
        if raw
            .trim_start_matches('-')
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            assert!(
                !error.contains("whole number"),
                "{raw} is a whole number: {error}"
            );
        }
        assert_eq!(
            state.feedback.as_deref(),
            Some(format!("{label} was not saved").as_str())
        );
        assert!(!lab.undo_offered(), "nothing was written to undo");
        let compiled = lab.compile(1_200.0, 1_600.0);
        let control = compiled
            .semantic(&UiKey::new(format!("settings/control/{key}")))
            .unwrap();
        assert!(control.state.is_some_and(|state| state.invalid));
        let field = rect_of(&compiled, &format!("settings/row-with-error/{key}"));
        assert!(
            compiled.paint.iter().any(|node| node.key.as_str()
                == format!("settings/error/{key}/line-0")
                && node.rect.y > field.y),
            "the reason is under the row"
        );
    }
    lab.commit(&set, Some(SemanticInput::Text("2500".to_string())));
    assert_eq!(lab.text(), "search_history_lines = 2500\n");
    assert!(!lab.state().field_errors.contains_key(key));
}

/// The Manual page is worth landing on: one line of what Manual is, where
/// aterm.toml lives with Open, Show and Copy path, and the keys only it
/// holds. A view nobody armed shows no folder; it names it.
#[test]
fn the_manual_page_names_the_file_and_lists_the_keys_only_it_holds() {
    let mut lab = Lab::new("agents_auto_prime = false\nfuture_thing = 5\n");
    let home = std::path::Path::new("/Users//someone");
    let path = home.join(".config/aterm/aterm.toml");
    lab.state_mut().navigate(SettingsRoute::Manual);
    lab.state_mut()
        .arm_config_file(Some(path.clone()), Some(home), ConfigFileArm::inert());
    let compiled = lab.compile(1_200.0, 820.0);
    assert_eq!(
        compiled
            .semantic(&UiKey::new("settings/page-subtitle/manual"))
            .unwrap()
            .label,
        "Edit aterm.toml directly, with completion and error checking."
    );
    assert_eq!(
        compiled
            .semantic(&UiKey::new("settings/manual/path"))
            .unwrap()
            .label,
        "~/.config/aterm/aterm.toml"
    );
    // A compact page shows no subtitle, so the card says what Manual is.
    let compact = lab.compile(744.0, 420.0);
    assert!(
        compact
            .semantic(&UiKey::new("settings/page-subtitle/manual"))
            .is_none()
    );
    assert!(
        compact
            .paint
            .iter()
            .any(|node| node.key.as_str().starts_with("settings/manual/explanation")),
        "the card says what Manual is"
    );
    assert!(
        compiled
            .paint
            .iter()
            .all(|node| !node.key.as_str().starts_with("settings/manual/explanation")),
        "said once"
    );
    for key in [
        "settings/manual/open",
        "settings/manual/show-file",
        "settings/manual/copy-path",
        "settings/modified/manual/open/agents-auto-prime",
        "settings/modified/manual/reset/agents-auto-prime",
        "settings/modified/manual/open/future-thing",
    ] {
        assert!(compiled.semantic(&UiKey::new(key)).is_some(), "{key}");
    }

    assert!(lab.invoke("settings/manual/show-file", None).is_none());
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("aterm.toml is at ~/.config/aterm/aterm.toml"),
        "the inert arm names the folder"
    );
    lab.state_mut()
        .arm_config_file(Some(path.clone()), Some(home), ConfigFileArm::showing());
    lab.invoke("settings/manual/show-file", None);
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some(format!("Showed aterm.toml in {}", file_viewer_name()).as_str())
    );

    let outcome = lab
        .runtime
        .dispatch(
            lab.instance,
            lab.view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("settings/manual/copy-path"),
                value: None,
            }),
        )
        .unwrap();
    let (text, operation) = outcome
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::Clipboard {
                request: ClipboardRequest::CopyText { text, .. },
                reply,
            } => Some((text.clone(), reply.operation)),
            _ => None,
        })
        .expect("Copy path copies");
    assert_eq!(text, path.display().to_string());
    lab.runtime
        .dispatch(
            lab.instance,
            lab.view,
            AppEvent::ClipboardFinished {
                operation,
                outcome: ClipboardOutcome::Copied,
            },
        )
        .unwrap();
    assert_eq!(lab.state().feedback.as_deref(), Some("Path copied"));

    // Unarmed: no path to show or copy.
    let mut bare = Lab::new("");
    bare.state_mut().navigate(SettingsRoute::Manual);
    let compiled = bare.compile(1_200.0, 820.0);
    let show = compiled
        .semantic(&UiKey::new("settings/manual/show-file"))
        .unwrap();
    assert!(show.state.is_some_and(|state| !state.enabled));
}

/// A suffix used to distinguish two normalized keys may already be another
/// authored key. Both pages still compile and show every row, without moving
/// the plain keys that were distinct to begin with.
#[test]
fn manual_key_suffix_collisions_keep_every_authored_row_renderable() {
    for route in [SettingsRoute::Manual, SettingsRoute::Modified] {
        let mut lab = Lab::new("a-b = 1\na-b-2 = 2\na_b = 3\n");
        lab.state_mut().navigate(route);
        let compiled = lab.compile(1_200.0, 1_600.0);
        for name in ["a-b", "a-b-2", "a_b"] {
            assert!(
                compiled.semantics.iter().any(|node| {
                    node.key
                        .as_str()
                        .starts_with("settings/modified/manual/label/")
                        && node.label == name
                }),
                "{route:?} keeps the authored {name} row"
            );
        }
        for plain in ["a-b", "a-b-2"] {
            assert!(
                compiled
                    .semantic(&UiKey::new(format!(
                        "settings/modified/manual/open/{plain}"
                    )))
                    .is_some(),
                "the unambiguous {plain} semantic key is preserved"
            );
        }
        let opens = compiled.semantics.iter().filter(|node| {
            node.key
                .as_str()
                .starts_with("settings/modified/manual/open/")
        });
        assert_eq!(
            opens.count(),
            3,
            "{route:?} retains three separate controls"
        );
    }
}

/// The words do not depend on the order the host delivers a change in: a
/// snapshot that reaches the view before the patch's result does still
/// leaves Undo naming the value it puts back.
#[test]
fn undo_names_the_old_value_when_the_snapshot_arrives_first() {
    let mut lab = Lab::new("[sparkle_words.feline]\nenabled = false\n");
    lab.commit_snapshot_first(&format!("settings/reset/{KEYWORD_KITTIES_KEY}"), None);
    assert_eq!(lab.text(), "");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Keyword kitties turned on")
    );
    lab.undo().expect("Undo is offered");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Keyword kitties: Off")
    );
    assert_eq!(lab.text(), "[sparkle_words.feline]\nenabled = false\n");

    // Reset All of one setting names it the same way.
    let mut lab = Lab::new("theme = \"Nord\"\n");
    lab.invoke("settings/reset-all", None);
    lab.commit_snapshot_first("settings/reset-all-confirm", None);
    assert_eq!(lab.text(), "");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Color theme reset to Follow system")
    );
    lab.undo().expect("Undo is offered");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Color theme: Nord")
    );
}

impl Lab {
    /// aterm.toml changed outside this page (a hand edit, a control verb,
    /// another window): the new text, as the watcher delivers it.
    fn external(&mut self, text: &str) {
        let snapshot = self
            .service
            .replace_external(text.to_string())
            .expect("valid fixture");
        self.runtime
            .dispatch(self.instance, self.view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
    }

    fn undo_button_label(&self) -> Option<String> {
        self.compile(1_200.0, 820.0)
            .semantic(&UiKey::new("settings/undo"))
            .map(|node| node.label.clone())
    }
}

/// Undo after undoing Reset All never quietly resets everything again: the
/// offer that follows an Undo is a Redo, says so on its button, is not what
/// Cmd-Z (an undo) presses, and each press's status says what it did.
#[test]
fn the_offer_after_an_undo_is_a_redo_that_says_so() {
    let source = "cursor_blink = false\ntheme = \"Nord\"\nfont_px = 15.0\n";
    let mut lab = Lab::new(source);
    lab.state_mut().navigate(SettingsRoute::Modified);
    assert!(lab.invoke("settings/reset-all", None).is_none(), "it asks");
    lab.commit("settings/reset-all-confirm", None);
    assert_eq!(lab.text(), "");
    assert_eq!(lab.state().feedback.as_deref(), Some("Reset 3 settings"));
    assert_eq!(lab.undo_button_label().as_deref(), Some("Undo"));

    lab.undo().expect("Undo is offered");
    let restored = lab.text();
    for line in source.lines() {
        assert!(restored.contains(line), "{line} is back: {restored}");
    }
    assert_eq!(lab.state().feedback.as_deref(), Some("Restored 3 settings"));
    assert_eq!(lab.undo_button_label().as_deref(), Some("Redo"));
    // Cmd-Z asks for an undo; what is offered is a redo, so nothing happens.
    assert!(
        lab.undo_with(AppEvent::TextInput(TextInputEvent::Undo))
            .is_none(),
        "Cmd-Z does not reset everything again"
    );
    assert_eq!(lab.text(), restored);

    // The Redo button, pressed, redoes the reset and says so.
    lab.undo().expect("Redo is offered");
    assert_eq!(lab.text(), "");
    assert_eq!(lab.state().feedback.as_deref(), Some("Reset 3 settings"));
    assert_eq!(lab.undo_button_label().as_deref(), Some("Undo"));
}

/// A change made elsewhere to a key the offered change wrote takes the
/// status and its Undo away: the status no longer says what the file holds.
/// A change to another key leaves them, as that Undo still works.
#[test]
fn an_outside_change_to_its_keys_takes_the_undo_away() {
    let mut lab = Lab::new("cursor_blink = false\n");
    lab.state_mut().navigate(SettingsRoute::CursorStyles);
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_CURSOR_TRAIL_STYLE),
        Some(SemanticInput::Text("comet".to_string())),
    );
    assert!(lab.undo_offered());
    let status = lab.state().feedback.clone().expect("the change is named");

    // Another key: the offer stands.
    let unrelated = format!("{}theme = \"Nord\"\n", lab.text());
    lab.external(&unrelated);
    assert!(lab.undo_offered(), "an unrelated change keeps the Undo");
    assert_eq!(lab.state().feedback.as_ref(), Some(&status));

    // The style itself, changed in aterm.toml: the offer goes.
    let restyled = lab.text().replace("\"comet\"", "\"lumen\"");
    assert_ne!(restyled, lab.text(), "the fixture changes the style");
    lab.external(&restyled);
    assert!(!lab.undo_offered(), "its Undo could not put it back");
    let now = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        !now.contains("Comet") && now.contains("changed in aterm.toml"),
        "{now}"
    );
}

/// A STYLE ROUND TRIP FROM EFFECTS OFF LEAVES NO REDUNDANT MASTER: turning
/// cursor effects on by choosing a style writes the switch in its canonical
/// ON form (no key where the platform default draws), so choosing the
/// default style afterwards leaves nothing behind, and Modified lists no
/// "Cursor style" whose value is its default.
#[test]
fn a_style_round_trip_from_effects_off_leaves_no_redundant_master() {
    let mut lab = Lab::new("cursor_trail = false\n");
    lab.state_mut().navigate(SettingsRoute::CursorStyles);
    lab.compile(1_024.0, 768.0);
    lab.commit("settings/cursor-style/comet", None);
    lab.compile(1_024.0, 768.0);
    lab.commit(
        &format!(
            "settings/cursor-style/{}",
            key_fragment(prefs::DEFAULT_CURSOR_TRAIL_STYLE)
        ),
        None,
    );
    let config: Config = aterm_toml::from_str(&lab.text()).unwrap();
    assert!(config.cursor_trail_or_default(), "the trail is on");
    if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
        assert_eq!(lab.text(), "", "no cursor key is left behind");
        assert!(
            !lab.state().is_explicit(prefs::EDIT_CURSOR_TRAIL_STYLE),
            "Modified lists no Cursor style row"
        );
    } else {
        assert_eq!(lab.text(), "cursor_trail = true\n");
    }
}

/// A reset says the value in the words its row shows, never the config's:
/// the selected tab color goes back to Automatic, and Remove on Wallpaper
/// keeps its own words once the change is saved.
#[test]
fn resets_name_the_default_in_plain_words() {
    let mut lab = Lab::new("active_tab_color = \"#FF8800\"\nwallpaper = \"/tmp/none.png\"\n");
    lab.commit(
        &format!("settings/reset/{}", prefs::EDIT_ACTIVE_TAB_COLOR),
        None,
    );
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Selected tab color reset to Automatic")
    );
    lab.commit("settings/wallpaper/detach", None);
    assert_eq!(lab.text(), "");
    assert_eq!(lab.state().feedback.as_deref(), Some("Wallpaper removed"));
    // Modified names the default the same way.
    assert_eq!(default_value_label(prefs::EDIT_WALLPAPER), Some("No image"));
}
