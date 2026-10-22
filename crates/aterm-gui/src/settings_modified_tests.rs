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

    /// Dispatch `event` to the view; what it did.
    pub(super) fn dispatch_outcome(
        &mut self,
        event: AppEvent,
    ) -> crate::native_app::DispatchOutcome {
        self.runtime
            .dispatch(self.instance, self.view, event)
            .unwrap()
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
            AppEffect::ConfigPatch { patch, reply, .. } => Some((patch.clone(), reply.operation)),
            _ => None,
        })
    }

    /// Invoke `action` and carry the patch it emits through the real service,
    /// as the host does: the result, then the new snapshot.
    pub(super) fn commit(&mut self, action: &str, value: Option<SemanticInput>) -> ConfigPatch {
        let (patch, operation) = self
            .invoke(action, value)
            .unwrap_or_else(|| panic!("{action} writes aterm.toml"));
        self.carry(patch, operation)
    }

    /// Invoke `action` as a press does, and carry the patch it emits, if it
    /// writes, through the real service as [`Self::commit`] does.
    pub(super) fn press(&mut self, action: &str) -> Option<ConfigPatch> {
        let (patch, operation) = self.invoke(action, None)?;
        Some(self.carry(patch, operation))
    }

    /// Carry `patch` through the real service, as the host does: the
    /// result, then the new snapshot.
    pub(super) fn carry(&mut self, patch: ConfigPatch, operation: OperationId) -> ConfigPatch {
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
                AppEffect::ConfigPatch { patch, reply, .. } => {
                    Some((patch.clone(), reply.operation))
                }
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
            AppEffect::ConfigUndo {
                target: crate::native_app::UndoTarget::Token(token),
                reply,
            } => Some((*token, reply.operation)),
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

    pub(super) fn undo(&mut self) -> Option<()> {
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

    /// Every screen of the list in a `width`x`height` window, from its top
    /// to the last one it scrolls to ([`each_screen_of`]).
    pub(super) fn each_screen(
        &mut self,
        width: f32,
        height: f32,
        each: impl FnMut(&SettingsViewState, &crate::native_ui::CompiledUi, usize),
    ) -> usize {
        each_screen_of(
            &mut self.runtime,
            self.instance,
            self.view,
            &view_cx_at(width, height),
            each,
        )
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
    let mut lab = Lab::new(&trail_drawn("cursor_trail_style = \"comet\"\n"));
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

    // The theme's own pair, written (what Follow system writes over an unset
    // theme): neither slot is off its side's default, so the Follow system
    // row carries the theme's Reset, on its page and on Modified.
    for route in [SettingsRoute::Appearance, SettingsRoute::Modified] {
        let mut lab = Lab::new("theme = \"dark:Default,light:Default Light\"\n");
        lab.state_mut().navigate(route);
        let compiled = lab.compile(1_200.0, 1_400.0);
        if route == SettingsRoute::Appearance {
            let label = &compiled
                .semantic(&UiKey::new("settings/label/theme-mode"))
                .unwrap()
                .label;
            assert!(label.contains("Changed"), "{label}");
        }
        let reset = rect_of(&compiled, &format!("settings/reset/{}", prefs::EDIT_THEME));
        let control = rect_of(&compiled, THEME_MODE_ACTION);
        assert!(
            reset.x >= control.x + control.width,
            "{route:?}: Reset follows the switch"
        );
        lab.commit(&format!("settings/reset/{}", prefs::EDIT_THEME), None);
        assert_eq!(lab.text(), "", "{route:?}");
    }
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
}

/// The old shared word-effects switch has no Settings row anywhere (the Top
/// card's two toys are the switches), so Modified lists it among the keys
/// only aterm.toml holds, as Search does, with its Reset; a toy's row is
/// listed only when the toy's own key is written.
#[test]
fn modified_lists_the_old_word_effects_switch_with_the_aterm_toml_keys() {
    let mut lab = Lab::new("[sparkle_words]\nenabled = false\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(1_200.0, 820.0);
    assert!(
        compiled
            .semantic(&UiKey::new(format!(
                "settings/control/{SPARKLE_MASTER_KEY}"
            )))
            .is_none(),
        "no row: it is not a setting"
    );
    assert!(
        authored_manual_overrides(lab.state())
            .iter()
            .any(|entry| entry.key == SPARKLE_MASTER_KEY)
    );
    for toy in [SPARKLE_WORDS_KEY, KEYWORD_KITTIES_KEY] {
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/control/{toy}")))
                .is_none(),
            "{toy} is not written"
        );
    }
    let reset = manual_override_reset_action(SPARKLE_MASTER_KEY).expect("its Reset");
    lab.commit(reset.as_str(), None);
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

/// DISPLAY FACE ON MODIFIED IS THE SWITCHES TEXT & FONTS DRAWS, from the one
/// builder ([`display_face_rows`]): a Switch per face, its value on or off and
/// never "selected" as a single choice is, saying what its own page's switch
/// says (name, value, credit or why a fourth is refused); no pop-up of raw
/// mixes; inside its row at a phone width; and a switch changes only its
/// face. On the old builder they were single-choice segment buttons with no
/// description.
#[test]
fn modified_display_face_is_a_toggle_per_face() {
    for source in [
        "display_font = \"pixel+bubble\"\n",
        "display_font = \"chunky+pixel+bubble\"\n",
    ] {
        let mut lab = Lab::new(source);
        for (width, height) in [(1_200.0, 820.0), (420.0, 820.0)] {
            lab.state_mut().navigate(SettingsRoute::TextFonts);
            let mut own = std::collections::BTreeMap::new();
            for top in 0..40 {
                lab.state_mut().scroll_page_to(top);
                let compiled = lab.compile(width, 2_400.0);
                for font in aterm_render::DISPLAY_FACES {
                    if let Some(node) =
                        compiled.semantic(&UiKey::new(format!("settings/display-face/{}", font.id)))
                    {
                        own.entry(font.id).or_insert_with(|| node.clone());
                    }
                }
            }
            lab.state_mut().navigate(SettingsRoute::Modified);
            lab.state_mut().scroll_page_to(0);
            let compiled = lab.compile(width, height);
            let row = compiled
                .semantics
                .iter()
                .find(|node| {
                    node.key.as_str().starts_with("settings/row/")
                        && node.key.as_str().ends_with("/display_font")
                })
                .map(|node| node.rect)
                .expect("the Display face row");
            for font in aterm_render::DISPLAY_FACES {
                let key = UiKey::new(format!("settings/display-face/{}", font.id));
                let toggle = compiled
                    .semantic(&key)
                    .unwrap_or_else(|| panic!("{source} {width}: {} on Modified", font.id));
                let mine = own
                    .get(font.id)
                    .unwrap_or_else(|| panic!("{} on Text & Fonts", font.id));
                assert_eq!(toggle.role, SemanticRole::Switch, "{}", font.id);
                assert!(
                    toggle.state.is_some_and(|state| !state.selected),
                    "{}: a switch, not a chosen segment",
                    font.id
                );
                assert_eq!(
                    (&toggle.label, &toggle.value, &toggle.description),
                    (&mine.label, &mine.value, &mine.description),
                    "{source} {width}: {} says what its own page says",
                    font.id
                );
                assert!(
                    toggle.rect.y >= row.y - 0.5 && toggle.rect.bottom() <= row.bottom() + 0.5,
                    "{width}: {} {:?} inside its row {row:?}",
                    font.id,
                    toggle.rect
                );
            }
            assert!(
                compiled.semantics.iter().all(|node| node
                    .action
                    .as_ref()
                    .is_none_or(|action| action.as_str() != "settings/set/display_font")),
                "no pop-up of raw mixes"
            );
        }
    }
    let mut lab = Lab::new("display_font = \"pixel+bubble\"\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    lab.commit("settings/display-face/chunky", None);
    assert_eq!(lab.text(), "display_font = \"pixel+bubble+chunky\"\n");
}

/// A Modified row too long for its line keeps its name beside the control
/// and puts the rest — when it applies, what Reset returns it to — on the
/// line under it, whole.
#[test]
fn a_long_modified_row_puts_its_facts_on_a_second_line() {
    let mut lab = Lab::new("[machine]\nuniversal_control = \"off\"\n");
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
    // on whichever line has the room, whole. Where Universal Control does
    // nothing (every platform but macOS, `row_available_here`), the row says
    // that there instead, in the platform note's own words, and claims no
    // timing.
    let availability = SettingsAvailability::for_state(lab.state());
    let first_fact = if row_available_here(key, availability) {
        "Applies at the next ALab tools check".to_string()
    } else {
        platform_unavailability(lab.state(), None, key, availability)
            .expect("a row that does nothing here says why")
            .visual
    };
    assert!(lines.contains(&first_fact), "{first_fact:?} in {lines}");
    let visual = painted_row_label(&compiled, key).unwrap_or_default();
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
                    crate::app_config::default_pair_side(appearance)
                )),
                "{side}: {label}"
            );
            assert!(!label.contains("Default: Default"), "{side}: {label}");
        }
    }
}

/// A themes-folder theme goes by the name its card shows, not its file id,
/// in the status and in Undo's words. Negative control: a built-in's id is
/// its name (`undo_names_the_change_and_what_it_restored`).
#[test]
fn a_kept_folder_theme_is_named_by_its_display_name() {
    let mut lab = Lab::new("theme = \"Dracula\"\n");
    let parsed = aterm_types::scheme::parse_theme_str(
        "name = Studio Type\nforeground = #e0e6f0\nbackground = #141821\n",
    )
    .expect("a theme");
    let mut assets = (*lab.state().assets).clone();
    assets.themes =
        crate::app_config::ThemeCatalog::from_parsed([("studio-type".to_string(), parsed)]);
    lab.state_mut().assets = std::sync::Arc::new(assets);
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_THEME),
        Some(SemanticInput::Text("studio-type".to_string())),
    );
    assert_eq!(lab.text(), "theme = \"studio-type\"\n");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Color theme set to Studio Type")
    );
    lab.undo().expect("Undo is offered");
    lab.undo().expect("and Redo");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Color theme: Studio Type")
    );
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
    // No theme at all is Default, and says so.
    lab.commit(&format!("settings/reset/{}", prefs::EDIT_THEME), None);
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Color theme reset to Default")
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
        ("5000000000", "must be at most 4294967295"),
        // Too long for i64 is still a whole number: it says which end it
        // passed, never that it is not a number.
        ("99999999999999999999", "must be at most 4294967295"),
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

/// SHOW IN FINDER SHOWS THE FILE AND SAYS ONLY WHAT IT SHOWED: with the
/// file there, the file itself — never its folder — goes to the file
/// viewer to be selected, and only its yes is "Showed aterm.toml"; before
/// the file exists, its folder is shown and the status says so; a viewer
/// that selects nothing leaves the path named. The live arm's seams are the
/// real reveal and folder open.
#[test]
fn show_in_finder_selects_the_file_and_says_only_what_it_showed() {
    use std::sync::Mutex;
    static REVEALED: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());
    static OPENED: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());
    fn reveal(path: &std::path::Path) -> bool {
        REVEALED.lock().unwrap().push(path.to_path_buf());
        true
    }
    fn refuse(path: &std::path::Path) -> bool {
        REVEALED.lock().unwrap().push(path.to_path_buf());
        false
    }
    fn open_folder(path: &std::path::Path) -> bool {
        OPENED.lock().unwrap().push(path.to_path_buf());
        true
    }
    let root = aterm_tempfile::TempDir::new().unwrap();
    let path = root.path().join("aterm.toml");
    std::fs::write(&path, "").unwrap();
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Manual);
    lab.state_mut().arm_config_file(
        Some(path.clone()),
        None,
        ConfigFileArm::with_seams(reveal, open_folder),
    );
    lab.invoke("settings/manual/show-file", None);
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some(format!("Showed aterm.toml in {}", file_viewer_name()).as_str())
    );
    assert_eq!(
        REVEALED.lock().unwrap().as_slice(),
        std::slice::from_ref(&path),
        "the file, not its folder"
    );
    assert!(OPENED.lock().unwrap().is_empty());

    std::fs::remove_file(&path).unwrap();
    lab.invoke("settings/manual/show-file", None);
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some(
            format!(
                "aterm.toml does not exist yet; showed its folder in {}",
                file_viewer_name()
            )
            .as_str()
        )
    );
    assert_eq!(REVEALED.lock().unwrap().len(), 1, "nothing to select");
    assert_eq!(*OPENED.lock().unwrap(), [root.path().to_path_buf()]);

    std::fs::write(&path, "").unwrap();
    lab.state_mut().arm_config_file(
        Some(path.clone()),
        None,
        ConfigFileArm::with_seams(refuse, open_folder),
    );
    lab.invoke("settings/manual/show-file", None);
    let status = lab.state().feedback.clone().unwrap();
    assert!(status.starts_with("aterm.toml is at "), "{status}");

    assert!(ConfigFileArm::live().seams_are_live());
    assert!(!ConfigFileArm::inert().seams_are_live());
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
        Some("Color theme reset to Default")
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
    assert_eq!(restored, source, "Undo gives back the file, byte for byte");
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

/// A STYLE ROUND TRIP FROM THE RETIRED SWITCH LEAVES NO STRAY KEY: from an old
/// `cursor_trail = false`, choosing a style takes the retired switch out of
/// the file (the typing glow kept as it drew), and choosing the default style
/// afterwards writes no style key, so Modified lists no "Cursor style" whose
/// value is its default.
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
    assert!(config.cursor_trail_drawn(), "the trail is on");
    assert!(config.cursor_trail.is_none(), "the retired switch is gone");
    assert!(
        !config.cursor_momentum_glow_or_default(),
        "the typing glow stays off, as the old switch drew it"
    );
    if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
        assert_eq!(
            lab.text(),
            "cursor_momentum_glow = false\n",
            "no cursor style key is left behind"
        );
        assert!(
            !lab.state().is_explicit(prefs::EDIT_CURSOR_TRAIL_STYLE),
            "Modified lists no Cursor style row"
        );
    } else {
        assert_eq!(
            lab.text(),
            format!(
                "cursor_trail_style = \"{}\"\n",
                prefs::DEFAULT_CURSOR_TRAIL_STYLE
            )
        );
    }
}

/// AN OLD `cursor_trail = false` NEVER SWALLOWS A CHOICE OF A DEFAULT: the
/// default style and the typing glow turned on are writes of no value, yet
/// the writer takes the retired switch out for them (the other successor
/// pinned to what the file drew), so the page sends them, names what
/// changed on screen, and its Undo puts the old switch back. Both rows the
/// old switch holds off show as changed, and their Reset takes it out too.
#[test]
fn the_default_style_and_the_glow_are_chosen_over_the_retired_switch() {
    if !crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
        // Where effects are off by default `cursor_trail = false` reads as
        // the defaults, so there is nothing to choose.
        return;
    }
    let legacy = "cursor_trail = false\n";
    let style = prefs::EDIT_CURSOR_TRAIL_STYLE;
    let glow = prefs::EDIT_CURSOR_MOMENTUM_GLOW;

    // The default style, from the Cursor Styles list.
    let mut lab = Lab::new(legacy);
    assert!(
        lab.state().is_explicit(style),
        "the old switch holds it Off"
    );
    assert!(lab.state().is_explicit(glow), "and the glow off");
    lab.state_mut().navigate(SettingsRoute::CursorStyles);
    lab.compile(1_024.0, 768.0);
    lab.commit(
        &format!(
            "settings/cursor-style/{}",
            key_fragment(prefs::DEFAULT_CURSOR_TRAIL_STYLE)
        ),
        None,
    );
    assert_eq!(lab.text(), "cursor_momentum_glow = false\n");
    let status = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        status.starts_with("Cursor style set to Rainbow kitty"),
        "{status}"
    );
    assert!(!lab.state().is_explicit(style));
    lab.undo().expect("its Undo is offered");
    assert_eq!(lab.text(), legacy, "Undo puts the old switch back");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Cursor style: Off")
    );

    // The typing glow turned on.
    let mut lab = Lab::new(legacy);
    lab.commit(
        &format!("settings/set/{glow}"),
        Some(SemanticInput::Bool(true)),
    );
    let glowing = |text: &str| {
        let config: Config = aterm_toml::from_str(text).unwrap();
        assert!(
            config.cursor_trail.is_none(),
            "the old switch is gone: {text}"
        );
        assert!(!config.cursor_trail_drawn(), "the style stays Off: {text}");
        assert!(
            config.cursor_momentum_glow_or_default(),
            "the glow is on: {text}"
        );
    };
    glowing(&lab.text());
    let status = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        status.starts_with("Typing momentum glow turned on"),
        "{status}"
    );

    // Each row's Reset takes the old switch out, the other row kept.
    for key in [style, glow] {
        let mut lab = Lab::new(legacy);
        lab.state_mut().navigate(SettingsRoute::CursorMotion);
        let compiled = lab.compile(1_200.0, 2_400.0);
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/reset/{key}")))
                .is_some(),
            "{key} offers Reset"
        );
        lab.commit(&format!("settings/reset/{key}"), None);
        if key == style {
            assert_eq!(
                lab.text(),
                "cursor_momentum_glow = false\n",
                "{key}'s Reset"
            );
        } else {
            glowing(&lab.text());
        }
    }

    // Negative control: `cursor_trail = true` over the default style reads
    // as the defaults, so neither row is changed.
    let lab = Lab::new("cursor_trail = true\n");
    assert!(!lab.state().is_explicit(style));
    assert!(!lab.state().is_explicit(glow));
}

/// A RETIRED `[packages]` SPELLING NEVER SWALLOWS A CHOICE OF A DEFAULT
/// EITHER: over an old `auto_update = false`, turning Keep ALab tools up to
/// date on (its default, a write of no value) takes the retired key out, as
/// the writer does for any write of the key that replaced it; the row shows
/// as changed before, and says it turned on.
#[test]
fn keep_tools_up_to_date_is_turned_on_over_the_retired_switch() {
    let mut lab = Lab::new("[packages]\nauto_update = false\n");
    assert!(lab.state().is_explicit(prefs::EDIT_PACKAGES_ENABLED));
    lab.commit(
        &format!("settings/set/{}", prefs::EDIT_PACKAGES_ENABLED),
        Some(SemanticInput::Bool(true)),
    );
    assert!(!lab.text().contains("auto_update"), "{}", lab.text());
    let config: Config = aterm_toml::from_str(&lab.text()).unwrap();
    assert!(config.packages_enabled());
    let status = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        status.starts_with("Keep ALab tools up to date turned on"),
        "{status}"
    );
}

/// WHAT A STYLE'S PREVIEW SHOWS IS WHAT ITS PRESS SAVES, from every old
/// Classic spelling: for each listed style, the style and trail colour the
/// preview draws while the row is tried are the ones the saved file draws.
/// Leaving a `classic mono` for Comet saves Comet in its own pale blue, as
/// the preview showed, never a cursor colour the person never chose.
#[test]
fn a_tried_style_saves_what_its_preview_shows_from_an_old_classic() {
    use aterm_effects::cursor_glow::GlowStyle;
    // What a cursor draws: its style, Classic's one-colour face, its colour
    // (Off draws none, so no colour).
    let drawn = |raw: &str, color: aterm_effects::cursor_fx::TrailColor| {
        let presentation = crate::app_config::resolve_trail_presentation(raw);
        let style = presentation.style.style;
        let mono =
            style == Some(GlowStyle::Classic) && (presentation.classic_mono || color.chosen());
        let color = if style.is_none() {
            aterm_effects::cursor_fx::TrailColor::Automatic
        } else if presentation.classic_mono && !color.chosen() {
            // The old spelling says the cursor's colour.
            aterm_effects::cursor_fx::TrailColor::Cursor
        } else {
            color
        };
        (style, mono, color)
    };
    let mut checked = 0;
    for old in ["classic mono", "classic lumen", "mono classic"] {
        let file = format!("cursor_trail_style = \"{old}\"\n");
        for style in listed_cursor_style_values() {
            let mut lab = Lab::new(&file);
            lab.state_mut().navigate(SettingsRoute::CursorStyles);
            lab.compile(1_024.0, 768.0);
            if cursor_style_is_saved(lab.state(), style) {
                continue;
            }
            lab.dispatch(AppEvent::FocusChanged(Some(UiKey::new(
                cursor_style_row_key(style),
            ))));
            let spec = renderer_preview_spec(
                lab.state(),
                100,
                crate::native_app::ViewMotionCx::default(),
                13.0,
                1.0,
            )
            .expect("the Cursor Styles preview");
            // Classic over its old spelling is no trial: the list's check is
            // on it, and the preview shows what pressing it keeps.
            let shown = drawn(&spec.cursor.trail_style_raw, spec.cursor.color);
            lab.commit(&cursor_style_row_key(style), None);
            let saved: Config = aterm_toml::from_str(&lab.text()).unwrap();
            assert_eq!(
                drawn(saved.cursor_trail_style_raw(), saved.trail_color()),
                shown,
                "{old} to {style}: {}",
                lab.text()
            );
            checked += 1;
        }
    }
    assert!(checked > 30, "{checked}");
}

/// WHAT `aterm ctl settings unset` SAYS A KEY NOW READS AS IS WHAT ITS ROW
/// SAYS: for every choice and switch Settings draws, unset, the words after
/// "now" ([`effective_value_words`]) are its row control's value — a choice
/// in its picker's or segment's words with "(default)", a switch On or Off.
/// Negative control: a config token placeholder ("auto (default)") is no
/// row's words.
#[test]
fn every_unset_choice_and_switch_reads_now_as_its_row_says() {
    let mut lab = Lab::new("");
    let snapshot = lab.service.snapshot();
    let fields: Vec<(&'static str, EditKind)> = prefs::editable_fields(&Config::default())
        .iter()
        .filter(|field| matches!(field.kind, EditKind::Enum { .. } | EditKind::Bool))
        .map(|field| (field.key, field.kind))
        .collect();
    let mut checked = 0;
    for (key, kind) in fields {
        lab.dispatch(AppEvent::Action(ActionInvocation {
            id: ActionId::new("settings/search"),
            value: Some(SemanticInput::Text(key.to_string())),
        }));
        let compiled = lab.compile(1_200.0, 8_000.0);
        let Some(control) = compiled.semantic(&UiKey::new(format!("settings/control/{key}")))
        else {
            continue;
        };
        let now = effective_value_words(&snapshot, key, false, None)
            .unwrap_or_else(|| panic!("{key} has a row"));
        match (&control.value, kind) {
            (SemanticValue::Text(row), EditKind::Enum { .. }) => {
                assert_eq!(&now, row, "{key}");
                checked += 1;
            }
            (SemanticValue::Bool(on), EditKind::Bool) => {
                let words = if *on { "On" } else { "Off" };
                assert!(
                    now == format!("{words} (default)")
                        || now.starts_with(&format!("{words} (from theme")),
                    "{key}: {now} for a switch that is {words}"
                );
                checked += 1;
            }
            _ => {}
        }
    }
    assert!(checked > 60, "{checked}");
    assert_ne!(
        effective_value_words(&snapshot, prefs::EDIT_WINDOW_THEME, false, None).as_deref(),
        Some("auto (default)")
    );
}

/// A RESET THAT LEAVES A KEYWORD TOY HELD OFF SAYS SO IN THE SWITCH'S WORDS:
/// with all word effects switched off, Reset on Keyword kitties takes its
/// own `false` out, and its switch reads Off before and after, so the status
/// says it was reset and is still Off and why, never "turned on" or
/// "inactive"; Undo and Redo name what the switch shows, Off. Negative
/// control: with the word effects on, the same Reset turns it on.
#[test]
fn a_reset_that_leaves_a_keyword_toy_held_off_says_it_is_still_off() {
    let source = "[sparkle_words]\nenabled = false\n[sparkle_words.feline]\nenabled = false\n";
    let mut lab = Lab::new(source);
    lab.state_mut().navigate(SettingsRoute::Modified);
    lab.commit(&format!("settings/reset/{KEYWORD_KITTIES_KEY}"), None);
    assert!(!lab.text().contains("feline"), "{}", lab.text());
    let status = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        status.starts_with("Keyword kitties reset to its default")
            && status.contains("Still Off: all word effects are switched off in aterm.toml")
            && !status.contains("turned on")
            && !status.to_lowercase().contains("inactive"),
        "{status}"
    );
    lab.undo().expect("Undo is offered");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Keyword kitties: Off")
    );
    lab.undo().expect("Redo is offered");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Restored Keyword kitties: Off")
    );

    let mut lab = Lab::new("[sparkle_words.feline]\nenabled = false\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    lab.commit(&format!("settings/reset/{KEYWORD_KITTIES_KEY}"), None);
    let status = lab.state().feedback.clone().unwrap_or_default();
    assert!(status.starts_with("Keyword kitties turned on"), "{status}");
}

/// EVERY TOP EFFECT SWITCH UNDER SERIOUS MODE SAYS WHAT ITS PRESS DOES: on
/// Top Settings, Sound, search and Modified, saved On or Off, a Music
/// effects, Sparkle words or Keyword kitties switch that reads Off says that
/// turning it on turns Serious Mode off, and its press writes Serious Mode
/// away, where nothing can sound as well (Linux, Windows: the note beside
/// "Audio unavailable"). Negative control: Music effects saved On on
/// Modified reads On, is "inactive" because of Serious Mode (or of no audio
/// output), never says its press turns Serious Mode off, and its press keeps
/// Serious Mode.
#[test]
fn every_top_effect_switch_under_serious_mode_says_what_its_press_does() {
    let leaves = |patch: &ConfigPatch| {
        patch
            .edits
            .iter()
            .any(|edit| edit.key == prefs::EDIT_SERIOUS_MODE && edit.value.is_none())
    };
    let mut checked = 0;
    for key in [
        prefs::EDIT_TRAIL_SOUNDS,
        SPARKLE_WORDS_KEY,
        KEYWORD_KITTIES_KEY,
    ] {
        let saved_off = if key == prefs::EDIT_TRAIL_SOUNDS {
            "trail_sounds = false\n".to_string()
        } else {
            let table = key.trim_end_matches(".enabled");
            format!("[{table}]\nenabled = false\n")
        };
        for saved in [String::new(), saved_off] {
            for route in [
                SettingsRoute::Home,
                SettingsRoute::Sound,
                SettingsRoute::Modified,
            ] {
                for search in [false, true] {
                    let mut lab = Lab::new(&format!("serious_mode = true\n{saved}"));
                    lab.state_mut().navigate(route);
                    if search {
                        lab.dispatch(AppEvent::Action(ActionInvocation {
                            id: ActionId::new("settings/search"),
                            value: Some(SemanticInput::Text(key.to_string())),
                        }));
                    }
                    let state = lab.state();
                    if top_effect_shown(state, None, key) {
                        continue;
                    }
                    let effect = setting_effect_projection(
                        state,
                        None,
                        key,
                        SettingsAvailability::for_state(state),
                        state.presented_motion.get(),
                    );
                    let context = format!("{key} saved {saved:?} on {route:?} search={search}");
                    assert!(
                        effect_note_contains(&effect, "turning this on turns Serious Mode off"),
                        "{context}: {effect:?}"
                    );
                    let (patch, _) = lab
                        .invoke(&format!("settings/set/{key}"), None)
                        .unwrap_or_else(|| panic!("{context}: the press writes"));
                    assert!(leaves(&patch), "{context}: {patch:?}");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 20, "{checked}");

    let mut lab = Lab::new("serious_mode = true\ntrail_sounds = true\n");
    lab.state_mut().navigate(SettingsRoute::Modified);
    let state = lab.state();
    assert!(top_effect_shown(state, None, prefs::EDIT_TRAIL_SOUNDS));
    let effect = setting_effect_projection(
        state,
        None,
        prefs::EDIT_TRAIL_SOUNDS,
        SettingsAvailability::for_state(state),
        state.presented_motion.get(),
    );
    // Held back by Serious Mode — or, where nothing can sound
    // (`trail_audio::output_available`), by that, the one reason a setting
    // does nothing that its row gives.
    assert!(
        effect_note_contains(
            &effect,
            if crate::trail_audio::output_available() {
                "Currently inactive because Serious Mode"
            } else {
                "Cursor-trail audio output is unavailable on this platform"
            }
        ) && !effect_note_contains(&effect, "turns Serious Mode off"),
        "{effect:?}"
    );
    let (patch, _) = lab
        .invoke(&format!("settings/set/{}", prefs::EDIT_TRAIL_SOUNDS), None)
        .expect("the press writes");
    assert!(!leaves(&patch), "{patch:?}");
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

/// WHEN UNIVERSAL CONTROL'S CHANGE LANDS IS READ FROM WHAT IS SAVED: Off at
/// the next ALab tools check while Keep ALab tools up to date is on, and with
/// it off (a retired `auto_update = false` too) once a day, at a new window
/// or session, since no package pass runs; Leave as is when saved, so it says
/// no timing, and its segment none either. The row, its control, the status
/// after a save and Manual's help say it in the same words. Negative
/// control: Keep on keeps the tools-check words.
#[test]
fn universal_control_says_when_off_lands_by_the_keep_switch() {
    use prefs::ApplicationTiming::{DailyLaunchApply, NextToolsCheck};
    let key = prefs::EDIT_MACHINE_UNIVERSAL_CONTROL;
    let row_words = |lab: &Lab| {
        let compiled = lab.compile(1_074.0, 820.0);
        let painted = compiled
            .paint
            .iter()
            .filter(|node| {
                node.key
                    .as_str()
                    .starts_with(&format!("settings/detail/{key}/line-"))
            })
            .filter_map(|node| match &node.content {
                UiContent::Text(spec) => Some(spec.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let visual = painted_row_label(&compiled, key).unwrap_or_default();
        let control = compiled
            .semantic(&UiKey::new(format!("settings/control/{key}")))
            .map(|node| node.label.clone())
            .unwrap_or_default();
        let segments: Vec<String> = compiled
            .semantics
            .iter()
            .filter(|node| {
                node.key
                    .as_str()
                    .starts_with(&format!("settings/segment/{key}/"))
            })
            .map(|node| node.label.clone())
            .collect();
        (format!("{visual} {painted}"), control, segments)
    };
    // The row and its control say when Off lands only where Universal Control
    // does anything (macOS, `row_available_here`); elsewhere they say it does
    // nothing here and name no timing, while the timing itself, the status
    // after a save and the Keep switch's flip below hold on every platform.
    let actuates = row_available_here(key, SettingsAvailability::for_state(Lab::new("").state()));
    for (source, timing) in [
        (
            "[machine]\nuniversal_control = \"off\"\n",
            Some(NextToolsCheck),
        ),
        (
            "[packages]\nenabled = true\n[machine]\nuniversal_control = \"off\"\n",
            Some(NextToolsCheck),
        ),
        (
            "[packages]\nenabled = false\n[machine]\nuniversal_control = \"off\"\n",
            Some(DailyLaunchApply),
        ),
        (
            "[packages]\nauto_update = false\n[machine]\nuniversal_control = \"off\"\n",
            Some(DailyLaunchApply),
        ),
        ("[machine]\nuniversal_control = \"leave\"\n", None),
    ] {
        let mut lab = Lab::new(source);
        lab.state_mut().navigate(SettingsRoute::Modified);
        let (painted, control, segments) = row_words(&lab);
        if !actuates {
            let availability = SettingsAvailability::for_state(lab.state());
            let note = platform_unavailability(lab.state(), None, key, availability)
                .expect("a row that does nothing here says why")
                .visual;
            assert!(
                painted.contains(&note),
                "{source}: where it does nothing the row says so ({note:?}): {painted:?}"
            );
        }
        for phrase in [NextToolsCheck, DailyLaunchApply].map(prefs::ApplicationTiming::words) {
            let says = actuates && timing.map(prefs::ApplicationTiming::words) == Some(phrase);
            assert_eq!(
                painted.contains(phrase),
                says,
                "{source}: painted {painted:?}"
            );
            assert_eq!(
                control.contains(phrase),
                says,
                "{source}: control {control:?}"
            );
        }
        // The segment not saved says when choosing it would land: Off its
        // timing (by the Keep switch), Leave as is none.
        for label in &segments {
            if label.contains("Leave") {
                assert!(
                    !label.contains("Applies"),
                    "{source}: Leave as is lands when saved: {label}"
                );
            }
        }
        // The status after saving Off says the same words.
        let state = lab.state();
        let patch = ConfigPatch {
            edits: vec![ConfigEdit {
                key: key.to_string(),
                expected: ExpectedConfigValue::Exact(None),
                value: Some("off".to_string()),
            }],
            base_revision: 1,
        };
        let projection =
            config_application_projection(state, &patch, SettingsAvailability::for_state(state));
        let off_timing = if source.contains("enabled = false") || source.contains("auto_update") {
            DailyLaunchApply
        } else {
            NextToolsCheck
        };
        if actuates {
            assert!(
                projection.application_timings.contains(&off_timing) || !native_advanced_key(key),
                "{source}: saving Off says {off_timing:?}: {projection:?}"
            );
        } else {
            assert!(
                projection.application_timings.is_empty()
                    && !projection.unavailable_notes.is_empty(),
                "{source}: where it does nothing, saving Off says so and no timing: {projection:?}"
            );
        }
    }
    // An unsaved patch that turns Keep on with it flips the words.
    let lab = Lab::new("[packages]\nenabled = false\n[machine]\nuniversal_control = \"leave\"\n");
    let state = lab.state();
    let patch = ConfigPatch {
        edits: vec![
            ConfigEdit {
                key: key.to_string(),
                expected: ExpectedConfigValue::Exact(None),
                value: Some("off".to_string()),
            },
            ConfigEdit {
                key: prefs::EDIT_PACKAGES_ENABLED.to_string(),
                expected: ExpectedConfigValue::Exact(None),
                value: Some("true".to_string()),
            },
        ],
        base_revision: 1,
    };
    assert_eq!(
        application_timing_in(state, Some(&patch), key),
        Some(NextToolsCheck)
    );
}

/// A NUMBER SETTINGS NAMES FOR A ROW IS ONE THE ROW TAKES BACK: the bound a
/// refusal names is spelled as the field takes it (plain digits), so typing
/// that bound saves, and the status, Reset and placeholder spell their
/// numbers the same way. Negative control: the grouped spelling the refusal
/// used to show is still refused, so grouping cannot come back without the
/// writer taking it.
#[test]
fn a_number_a_row_names_is_one_it_takes_back() {
    let key = prefs::EDIT_SEARCH_HISTORY_LINES;
    let set = format!("settings/set/{key}");
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Terminal);
    let too_many = (i64::from(u32::MAX) + 1).to_string();
    assert!(
        lab.invoke(&set, Some(SemanticInput::Text(too_many)))
            .is_none()
    );
    let error = lab.state().field_errors[key].clone();
    let bound = error
        .split("at most ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .unwrap_or_else(|| panic!("the refusal names its bound: {error}"))
        .to_string();
    assert!(
        prefs::check_edit_value(key, &bound).is_ok(),
        "the named bound {bound:?} is one the row takes"
    );
    assert!(
        prefs::check_edit_value(key, "4,294,967,295").is_err(),
        "the grouped spelling is still refused"
    );
    let label = lab.state().field_by_key(key).unwrap().label;
    lab.commit(&set, Some(SemanticInput::Text(bound.clone())));
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some(format!("{label} set to {bound}").as_str()),
        "the status spells the number as the refusal did"
    );
    lab.commit(&format!("settings/reset/{key}"), None);
    let feedback = lab.state().feedback.clone().unwrap_or_default();
    let reset_to = feedback
        .split(" reset to ")
        .nth(1)
        .unwrap_or_else(|| panic!("a reset says what to: {feedback}"))
        .to_string();
    let placeholder = lab.state().field_by_key(key).unwrap().placeholder.clone();
    assert_eq!(
        placeholder.strip_suffix(" (default)"),
        Some(reset_to.as_str()),
        "Reset and the placeholder spell the default alike"
    );
}

/// A COUNTED VALUE CARRIES ITS UNIT ON ITS NUMBER, never in its row's name
/// ([`prefs::numeric_unit`]): the padding, tab-status and typography rows
/// read "20 px" and "3000 ms" on their sliders and in their spoken value,
/// Modified's default reads "Default: 12 px", and no label says "(px)".
#[test]
fn counted_values_read_with_their_units() {
    let mut lab = Lab::new(
        "window_padding = 20\nwindow_padding_top = 18\ntab_status_quiet_after_ms = 3000\n\
         adjust_baseline = 2\n",
    );
    lab.state_mut().navigate(SettingsRoute::Modified);
    let compiled = lab.compile(1_200.0, 2_400.0);
    for (key, reading, default) in [
        (prefs::EDIT_WINDOW_PADDING, "20 px", "12 px"),
        (prefs::EDIT_WINDOW_PADDING_TOP, "18 px", "2 px"),
    ] {
        let control = compiled
            .semantic(&UiKey::new(format!("settings/control/{key}")))
            .unwrap_or_else(|| panic!("{key}'s slider"));
        assert_eq!(control.value_text.as_deref(), Some(reading), "{key}");
        assert_eq!(default_value_label(key), Some(default), "{key}");
        let painted = compiled
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::Slider(slider) if node.key == control.key => {
                    Some(slider.spec.display_value.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(painted, reading, "{key}: the readout");
    }
    for field in prefs::editable_fields(&crate::app_config::Config::default()) {
        assert!(
            !field.label.contains("(px)")
                && !field.label.contains("(ms)")
                && !field.label.contains("(s)")
                && !field.label.contains("(%)"),
            "{}: {}",
            field.key,
            field.label
        );
    }
    for (key, value, reading) in [
        (prefs::EDIT_TAB_STATUS_QUIET_AFTER_MS, 3000.0, "3000 ms"),
        (prefs::EDIT_ADJUST_BASELINE, 2.0, "2 px"),
        ("matrix_rain.idle_secs", 4.0, "4 s"),
        ("sparkle_words.profanity.supernova_chance", 35.0, "35%"),
    ] {
        assert_eq!(
            prefs::numeric_reading(key, value, &compact_f64(value)).as_deref(),
            Some(reading),
            "{key}"
        );
    }
}

/// Open a font row's picker on Text & Fonts and choose the option labelled
/// `label`: the patch that writes, if any.
fn choose_font(lab: &mut Lab, label: &str) -> Option<(ConfigPatch, OperationId)> {
    lab.invoke("settings/font-picker/font_family", None);
    let index = {
        let picker = lab
            .state()
            .choice_picker
            .as_ref()
            .expect("the font picker opens");
        picker
            .options
            .iter()
            .position(|option| option.label.starts_with(label))
            .unwrap_or_else(|| panic!("{label} is offered"))
    };
    lab.invoke(&format!("settings/choice/font_family/{index}"), None)
}

/// A COMMIT THAT CHANGES NOTHING WRITES NOTHING AND SAYS NOTHING: Return in
/// "Other…" with the field empty while the font is unset, choosing
/// Automatic while unset, choosing the saved font again, and Return in an
/// empty unset number field send no patch, leave the file alone, claim no
/// "reset to", and close what they opened. Negative controls: a name typed
/// into Other… is written and named, and clearing a saved font removes it
/// and says so.
#[test]
fn a_commit_of_what_the_file_holds_writes_nothing_and_claims_nothing() {
    let mut lab = Lab::new("");
    lab.state_mut().navigate(SettingsRoute::TextFonts);
    lab.state_mut()
        .replace_font_families(injected_font_families());
    assert!(
        choose_font(&mut lab, "Other").is_none(),
        "Other… opens a field"
    );
    assert_eq!(
        lab.state().font_text_entry.as_deref(),
        Some(prefs::EDIT_FONT_FAMILY)
    );
    let submitted = lab
        .runtime
        .dispatch(
            lab.instance,
            lab.view,
            AppEvent::TextInput(TextInputEvent::Submit),
        )
        .unwrap();
    assert!(
        submitted
            .effects
            .iter()
            .all(|effect| !matches!(effect, AppEffect::ConfigPatch { .. })),
        "an empty Other… on an unset font writes nothing"
    );
    assert_eq!(lab.text(), "");
    assert!(
        lab.state()
            .feedback
            .as_deref()
            .is_none_or(|status| !status.contains("reset") && !status.contains("set to")),
        "{:?}",
        lab.state().feedback
    );
    assert_eq!(lab.state().font_text_entry, None, "the list button is back");
    assert_eq!(lab.state().editing_field, None);

    assert!(
        choose_font(&mut lab, "Automatic").is_none(),
        "Automatic while unset"
    );
    assert!(lab.state().choice_picker.is_none(), "the picker closes");
    assert_eq!(lab.state().feedback, None);

    // An empty, unset number field: Return writes nothing.
    lab.invoke(&format!("settings/set/{}", prefs::EDIT_SCROLLBACK), None);
    let submitted = lab
        .runtime
        .dispatch(
            lab.instance,
            lab.view,
            AppEvent::TextInput(TextInputEvent::Submit),
        )
        .unwrap();
    assert!(
        submitted
            .effects
            .iter()
            .all(|effect| !matches!(effect, AppEffect::ConfigPatch { .. })),
        "an empty unset field writes nothing"
    );
    assert_eq!(lab.state().feedback, None);

    // Negative control: a typed name is written, and its status names it.
    choose_font(&mut lab, "Other");
    lab.dispatch(AppEvent::TextInput(TextInputEvent::Commit(
        "Menlo".to_string(),
    )));
    lab.commit_event(AppEvent::TextInput(TextInputEvent::Submit));
    assert_eq!(lab.text(), "font_family = \"Menlo\"\n");
    assert_eq!(
        lab.state().feedback.as_deref(),
        Some("Font family set to Menlo")
    );

    let mut lab = Lab::new("font_family = \"Menlo\"\n");
    lab.state_mut().navigate(SettingsRoute::TextFonts);
    lab.state_mut()
        .replace_font_families(injected_font_families());
    assert!(
        choose_font(&mut lab, "Menlo").is_none(),
        "the saved font again"
    );
    assert!(lab.state().choice_picker.is_none());
    // Negative control: Automatic over a saved font removes it, and says so.
    let (patch, _) = choose_font(&mut lab, "Automatic").expect("a real change writes");
    assert_eq!(patch.edits[0].value, None);
}

/// The words of a patch that changes no row never say "reset to" or "set
/// to": they say what the setting already is.
#[test]
fn a_patch_that_changes_nothing_is_never_worded_as_a_change() {
    let lab = Lab::new("font_family = \"Menlo\"\n");
    let words = change_words(
        lab.state(),
        &ConfigPatch {
            base_revision: 1,
            edits: vec![ConfigEdit {
                key: prefs::EDIT_FONT_FAMILY.to_string(),
                expected: ExpectedConfigValue::Exact(Some("Menlo".to_string())),
                value: Some("Menlo".to_string()),
            }],
        },
        &BTreeMap::new(),
    );
    assert!(
        !words.status.contains("reset to") && !words.status.contains("set to"),
        "{}",
        words.status
    );
    assert_eq!(words.status, "Font family is already Menlo");
}

/// A VALUE EQUAL TO ITS DEFAULT IS A REMOVAL ON THE PAGE TOO: setting
/// Window appearance to auto (by value, as automation and assistive
/// technology do) writes no key, and Modified's count does not move.
#[test]
fn the_page_writes_a_default_value_as_a_removal() {
    let mut lab = Lab::new("window_theme = \"dark\"\n");
    lab.state_mut().navigate(SettingsRoute::Appearance);
    let patch = lab.commit(
        &format!("settings/set/{}", prefs::EDIT_WINDOW_THEME),
        Some(SemanticInput::Text("auto".to_string())),
    );
    assert_eq!(patch.edits[0].value, None, "the page sends the removal");
    assert_eq!(lab.text(), "");
    assert_eq!(lab.state().raw_value(prefs::EDIT_WINDOW_THEME), None);
    // Already absent: the default again writes nothing.
    assert!(
        lab.invoke(
            &format!("settings/set/{}", prefs::EDIT_WINDOW_THEME),
            Some(SemanticInput::Text("auto".to_string())),
        )
        .is_none()
    );
}

/// A COMMENT UNDER A TABLE'S LAST KEY STAYS WITH ITS TABLE: turning Keyword
/// kitties back on (the default, so its key goes) keeps the
/// `[sparkle_words.feline]` header with the comment written under its key,
/// never leaving that comment under the table before it; Undo gives the file
/// back byte for byte.
#[test]
fn a_reset_keeps_the_comment_under_a_tables_last_key_with_its_table() {
    let source = "[machine]\nuniversal_control = \"leave\"\n\n[sparkle_words.feline]\nenabled = false\n# kitties: off until the demo is over\n\n[harness]\nenabled = true\n";
    let mut lab = Lab::new(source);
    lab.state_mut().navigate(SettingsRoute::Home);
    lab.commit(
        &format!("settings/set/{KEYWORD_KITTIES_KEY}"),
        Some(SemanticInput::Bool(true)),
    );
    let text = lab.text();
    assert!(
        text.contains("[sparkle_words.feline]\n# kitties: off until the demo is over\n"),
        "{text}"
    );
    assert!(
        !text.contains("universal_control = \"leave\"\n# kitties"),
        "{text}"
    );
    lab.undo().expect("Undo is offered");
    assert_eq!(lab.text(), source);
}

/// ONE KEY, ONE CLASSIFICATION ON EVERY SURFACE: a written key Modified
/// draws as a row is a row Search draws too, and a written key Modified
/// lists among the keys only aterm.toml holds is in Search's aterm.toml card
/// too — never a row on one and an aterm.toml key on the other. The keys
/// with an audited consumer and a home are rows (the renderer, replay, the
/// shell's priority, the window's size and badge); the ones without
/// (typography tuning, the old word-effects master, rain tuning) are
/// aterm.toml keys on both. (Modified used to admit every registry key while
/// Search admitted only the audited ones.)
#[test]
fn a_written_setting_is_the_same_row_in_search_and_modified() {
    let source = "gpu = false\nfocus_boost = false\ntemporal_recording = true\ncolumns = 96\nshow_build_badge = false\nstem_gamma = 1.2\nsparkle_words.enabled = false\n[matrix_rain]\nfps = 20\n";
    let controls = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .semantics
            .iter()
            .filter_map(|node| node.key.as_str().strip_prefix("settings/control/"))
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    };
    let mut lab = Lab::new(source);
    lab.state_mut().navigate(SettingsRoute::Modified);
    let modified_rows = controls(&lab.compile(1_200.0, 8_000.0));
    let overrides: BTreeSet<String> = authored_manual_overrides(lab.state())
        .iter()
        .map(|entry| entry.key.clone())
        .collect();
    let keys = [
        prefs::EDIT_GPU,
        prefs::EDIT_FOCUS_BOOST,
        prefs::EDIT_TEMPORAL_RECORDING,
        prefs::EDIT_COLUMNS,
        prefs::EDIT_SHOW_BUILD_BADGE,
        prefs::EDIT_STEM_GAMMA,
        SPARKLE_MASTER_KEY,
        "matrix_rain.fps",
    ];
    for key in keys {
        let row = modified_rows.contains(key);
        let toml = overrides.contains(key);
        assert!(row != toml, "{key}: a row or an aterm.toml key on Modified");
        lab.dispatch(AppEvent::Action(ActionInvocation {
            id: ActionId::new("settings/search"),
            value: Some(SemanticInput::Text(key.to_string())),
        }));
        let searched = controls(&lab.compile(1_200.0, 8_000.0));
        assert_eq!(
            searched.contains(key),
            row,
            "{key}: Search draws a row iff Modified does"
        );
        assert_eq!(
            manual_search_matching_keys(lab.state(), SearchQuery::of(lab.state(), key))
                .iter()
                .any(|listed| listed == key),
            toml,
            "{key}: Search's aterm.toml card lists it iff Modified does"
        );
    }
    for key in &keys[..5] {
        assert!(modified_rows.contains(*key), "{key} is a setting");
    }
    for key in &keys[5..] {
        assert!(overrides.contains(*key), "{key} has no Settings control");
    }
}
