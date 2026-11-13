// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn menu_faces_carry_stable_identities() {
    assert_eq!(
        menu_command(menu::MenuAction::CloseTab).id.as_str(),
        "view.close_focused"
    );
    assert_eq!(
        menu_command(menu::MenuAction::SplitVertical).content,
        ContentRequirement::Any
    );
    let map = menu_command(menu::MenuAction::Overview);
    assert_eq!(map.id.as_str(), "view.overview");
    assert_eq!(map.authority, ActionAuthority::Owner);
    for action in [
        menu::MenuAction::TakeOver,
        menu::MenuAction::HandBack,
        menu::MenuAction::ReadThisMac,
        menu::MenuAction::ReadFleet,
        menu::MenuAction::SummarizeFleet,
        menu::MenuAction::FleetMachines,
    ] {
        assert_eq!(
            menu_command(action).authority,
            ActionAuthority::Owner,
            "{action:?}"
        );
    }
    assert_eq!(
        menu_command(menu::MenuAction::NextNeed).authority,
        ActionAuthority::LocalUi
    );
    let system_state = menu_command(menu::MenuAction::SystemState);
    assert_eq!(system_state.id.as_str(), "app.system_state.open");
    assert_eq!(system_state.scope, CommandScope::App);
    assert_eq!(
        system_state.authority,
        menu_command(menu::MenuAction::ToggleSettings).authority
    );
    assert_eq!(system_state.content, ContentRequirement::Any);

    let allocation = menu_command(menu::MenuAction::Allocation);
    assert_eq!(allocation.id.as_str(), "app.allocation.open");
    assert_eq!(allocation.scope, CommandScope::Window);
    assert_eq!(allocation.authority, ActionAuthority::LocalUi);
    assert_eq!(allocation.content, ContentRequirement::Any);

    let all_actions: Vec<menu::MenuAction> =
        (0..=128).filter_map(menu::MenuAction::from_tag).collect();
    let mut seen_ids = std::collections::HashSet::with_capacity(all_actions.len());
    for action in all_actions {
        let cmd = menu_command(action);
        assert!(
            !cmd.id.as_str().is_empty(),
            "{action:?} has empty command id"
        );
        assert!(
            seen_ids.insert(cmd.id),
            "duplicate CommandId {:?} on {action:?}",
            cmd.id
        );
    }
}

#[test]
fn document_dynamic_actions_cannot_smuggle_authority_in_their_suffix() {
    let section = native_document_action("markdown/outline/42").unwrap();
    assert_eq!(section.id.as_str(), "markdown.section.goto");
    assert_eq!(section.authority, ActionAuthority::LocalUi);
    let page = native_document_action("markdown/page/42").unwrap();
    assert_eq!(page.id.as_str(), "markdown.page.goto");
    assert_eq!(page.authority, ActionAuthority::LocalUi);
    let link = native_document_action("markdown/link/7").unwrap();
    assert_eq!(link.id.as_str(), "markdown.resource.open");
    assert_eq!(link.authority, ActionAuthority::ExternalOpen);
    let completion = crate::native_config_language::ConfigCompletionEdit {
        replacement: 0..3,
        expected: "win".to_string(),
        insertion: "window_theme = \"auto\"".to_string(),
        post_insert_selection: 16..20,
        display: "window_theme".to_string(),
        help: "System appearance".to_string(),
    };
    let context = crate::native_config_language::ConfigCompletionContext::new(1, 2, 3);
    let config_action =
        crate::native_config_language::config_completion_action(context, 7, &completion);
    let config = native_document_action(&config_action).unwrap();
    assert_eq!(config.id.as_str(), "config.completion.choose");
    assert_eq!(config.authority, ActionAuthority::DocumentWrite);
    let out_of_range =
        crate::native_config_language::config_completion_action(context, 8, &completion);
    assert!(native_document_action(&out_of_range).is_none());
    assert!(native_document_action("editor/config-completion/0/save").is_none());
    assert!(native_document_action("markdown/unknown/7").is_none());
}

#[test]
fn editor_rows_carry_their_authority() {
    use crate::native_editor::{CommandAuthority as E, EditorCommand};
    let mut ids = std::collections::BTreeSet::new();
    for command in EditorCommand::ALL {
        let row = editor_command(command);
        assert_eq!(row.id.as_str(), command.name());
        assert!(ids.insert(row.id), "{command:?} shares an id");
        assert_eq!(row.scope, CommandScope::Document);
        assert_eq!(row.content, ContentRequirement::Native);
        assert_eq!(
            row.authority,
            editor_action_authority(command.authority()),
            "{command:?}"
        );
    }
    let ceilings = E::ALL.map(editor_action_authority);
    assert_eq!(
        ceilings,
        [
            ActionAuthority::LocalUi,
            ActionAuthority::DocumentWrite,
            ActionAuthority::DocumentWrite,
            ActionAuthority::Owner,
            ActionAuthority::Clipboard,
            ActionAuthority::Owner,
        ]
    );
    assert_eq!(editor_grant(false), E::ALL);
    assert_eq!(editor_grant(true), [E::Read, E::Edit, E::Save]);
    assert!(!controller_reaches(ActionAuthority::Clipboard));
    assert!(!controller_reaches(ActionAuthority::Owner));
    let kill_ring_save = editor_command(EditorCommand::KillRingSave);
    assert_eq!(kill_ring_save.id.as_str(), "kill-ring-save");
    assert_eq!(kill_ring_save.authority, ActionAuthority::Clipboard);
    assert!(!editor_grant(true).contains(&EditorCommand::KillRingSave.authority()));
}

#[test]
fn host_keybindings_shadow_only_the_listed_editor_chords() {
    use crate::native_editor::{KeyChord, Keymap, Platform, Profile};
    use aterm_window::keyboard::{Key as WinitKey, ModifiersState, NamedKey, SmolStr};
    let host = crate::keybinding::Keybindings::platform_defaults();
    let winit = |chord: &KeyChord| {
        let key = match chord.key.as_str() {
            "space" => WinitKey::Named(NamedKey::Space),
            "backspace" => WinitKey::Named(NamedKey::Backspace),
            "delete" => WinitKey::Named(NamedKey::Delete),
            "enter" => WinitKey::Named(NamedKey::Enter),
            "escape" => WinitKey::Named(NamedKey::Escape),
            "tab" => WinitKey::Named(NamedKey::Tab),
            "left" => WinitKey::Named(NamedKey::ArrowLeft),
            "right" => WinitKey::Named(NamedKey::ArrowRight),
            key => {
                assert_eq!(key.chars().count(), 1, "unmapped editor key {key}");
                WinitKey::Character(SmolStr::new(key))
            }
        };
        let mut mods = ModifiersState::empty();
        mods.set(ModifiersState::CONTROL, chord.control);
        mods.set(ModifiersState::ALT, chord.meta);
        mods.set(ModifiersState::SHIFT, chord.shift);
        (key, mods)
    };
    let mut shadowed = Vec::new();
    for (keys, command) in Keymap::emacs(Profile::Gui(Platform::HOST)).bindings() {
        for chord in &keys {
            let (key, mods) = winit(chord);
            if let Some(action) = host.lookup(&key, mods) {
                shadowed.push(format!(
                    "{} {} ({action:?})",
                    chord.display(),
                    command.name()
                ));
            }
        }
    }
    let expected: &[&str] = if cfg!(target_os = "macos") {
        &[]
    } else {
        &[
            "M-left backward-word (FocusPaneLeft)",
            "M-right forward-word (FocusPaneRight)",
        ]
    };
    assert_eq!(shadowed, expected);
}
