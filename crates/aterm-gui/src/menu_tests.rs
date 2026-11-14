// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    MENU_MODEL, MenuAction, MenuEntry, menu_chrome_lines, native_item_tip,
    native_menu_action_enabled, set_active_tab_is_terminal, staged_apply_label,
    version_menu_bar_title,
};

/// A new quit goes to the window whoever asks it, and one past its point of no return
/// terminates. AppKit is told to wait on the reply (`Later`) only for a quit the system or
/// Quit & Reopen asked for, which asks nothing; a person's quit, whose question stands as long
/// as they leave it, is taken up with AppKit's ask cancelled. A second ask of the pending quit
/// goes to the window only when the system or Quit & Reopen sends it, while a person's second
/// ask answers nothing and is cancelled. Where no reply could come (AppKit asking from inside
/// aterm's own alert or panel), a person's quit and a Quit & Reopen are still taken up, with
/// AppKit's ask cancelled, and a logout, restart or shutdown is refused.
///
/// NEGATIVE CONTROL: answer a person's new quit `Later` (the code at 0007e00dcf) and AppKit
/// waits inside `terminate:` for as long as the person leaves `Quit aterm?` up, terminating at
/// once, with no question, barrier or mark, on any second quit then. Make the DeferExisting
/// arm cancel for every asker (the code before 2026-11-06) and the system's and Quit &
/// Reopen's asks onto a pending quit are dropped. Refuse an unanswerable Quit & Reopen (the
/// code at 0007e00dcf) and it never quits once the alert or panel is answered.
#[test]
fn only_the_system_or_quit_and_reopen_wait_on_a_reply() {
    use super::{NativeTerminateDecision as Decision, TerminateReply, terminate_reply};
    use crate::system_quit::Asker;
    for answerable in [true, false] {
        for asker in [Asker::Person, Asker::System, Asker::Reopen] {
            assert_eq!(
                terminate_reply(Decision::AllowExit(3), asker, answerable),
                TerminateReply::Now,
                "{asker:?} {answerable}"
            );
        }
        assert_eq!(
            terminate_reply(Decision::Dispatch(3), Asker::Person, answerable),
            TerminateReply::TakeUp(3),
            "{answerable}"
        );
        assert_eq!(
            terminate_reply(Decision::DeferExisting(3), Asker::Person, answerable),
            TerminateReply::Cancel,
            "{answerable}"
        );
    }
    for decision in [Decision::Dispatch(3), Decision::DeferExisting(3)] {
        for asker in [Asker::System, Asker::Reopen] {
            assert_eq!(
                terminate_reply(decision, asker, true),
                TerminateReply::Later(3),
                "{decision:?} {asker:?}"
            );
        }
        assert_eq!(
            terminate_reply(decision, Asker::Reopen, false),
            TerminateReply::TakeUp(3),
            "{decision:?}"
        );
        assert_eq!(
            terminate_reply(decision, Asker::System, false),
            TerminateReply::Cancel,
            "{decision:?}"
        );
    }
}

#[test]
fn the_apply_row_can_never_read_as_the_version_already_running() {
    let running = crate::build_info::version_display();
    let badge = version_menu_bar_title(true);

    let same = staged_apply_label("\u{2B06}\u{FE0F}", 1_785_910_394, running, None, false);
    assert!(
        same.contains("build 1785910394"),
        "a same-version staged build must be identified by its build number: {same}"
    );
    assert!(
        !same.contains(&format!("v{running}")),
        "the offer must not name the version the badge already shows \
         (badge {badge:?}, offer {same:?})"
    );

    let newer = staged_apply_label("\u{2B06}\u{FE0F}", 1_785_910_394, "9.9.9", None, false);
    assert!(
        newer.contains("v9.9.9") && !newer.contains("build"),
        "a differing version is still named directly: {newer}"
    );
}

#[test]
fn the_apply_row_admits_when_applying_has_already_been_tried() {
    use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

    let label = |retry, held| {
        let trouble = ApplyTrouble::new(2, "handoff proof ended ChildDied", retry)
            .expect("two failed applies");
        staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", Some(&trouble), held)
    };
    let clean = staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", None, false);
    assert!(clean.ends_with(" now"), "{clean}");
    let scheduled = label(ApplyRetry::Scheduled, false);
    assert!(
        scheduled.contains("v9.9.9") && scheduled.contains("twice"),
        "{scheduled}"
    );
    assert!(!scheduled.contains("ChildDied"), "{scheduled}");
    assert!(!scheduled.ends_with(" now"), "{scheduled}");
    assert!(label(ApplyRetry::ManualOnly, false).ends_with(" now"));
    for held in [
        label(ApplyRetry::NewerRelease, true),
        staged_apply_label("\u{2191}", 1_787_699_398, "9.9.9", None, true),
    ] {
        assert!(held.contains("on hold") && !held.contains(" now"), "{held}");
    }
}

fn palette_only_actions() -> Vec<MenuAction> {
    super::PALETTE_ONLY.iter().map(|(_, _, a)| *a).collect()
}

fn offered_actions() -> Vec<MenuAction> {
    model_items()
        .into_iter()
        .map(|(_, action, _, _)| action)
        .chain(palette_only_actions())
        .chain([MenuAction::CopySessionId, MenuAction::CopyCwd])
        .collect()
}

const EARLIER_INVOKE_NAMES: &[&str] = &[
    "About",
    "SoftwareUpdate",
    "Version",
    "ApplyUpdate",
    "Preferences",
    "Quit",
    "NewWindow",
    "NewTab",
    "OpenEditor",
    "ReopenClosedTab",
    "ReopenClosedView",
    "MoveTabToNewWindow",
    "MoveTabToNextWindow",
    "ViewSessionInNewWindow",
    "NewControlledWindow",
    "NewControlledTab",
    "NewControllerWindow",
    "NewControllerTab",
    "CloseTab",
    "Copy",
    "Paste",
    "SelectAll",
    "Find",
    "FindNext",
    "FindPrev",
    "ToggleFullScreen",
    "FontIncrease",
    "FontDecrease",
    "FontActualSize",
    "SplitVertical",
    "SplitHorizontal",
    "ToggleMatrixRain",
    "FavouriteKitty",
    "NextKitty",
    "ToggleSeriousMode",
    "ToggleSettings",
    "Packages",
    "RenameSession",
    "OpenPalette",
    "Minimize",
    "Zoom",
    "NextTab",
    "PrevTab",
    "Help",
    "CopySessionId",
    "CopyCwd",
    "ConnectToSession",
    "ShowConnectionMap",
    "ConfigureConnection",
    "DisconnectSession",
    "Inbox",
    "ShowInbox",
    "LedgerForSession",
    "OpenLedger",
    "HoldSession",
    "LiftHold",
    "Fleet",
    "ShowFleet",
    "FabricStatus",
    "FabricOn",
    "FabricOff",
    "SetRole",
    "ShowIdentity",
    "TogglePresenceBand",
    "TogglePresenceRim",
];

fn model_items() -> Vec<(&'static str, MenuAction, &'static str, super::MenuMods)> {
    MENU_MODEL
        .iter()
        .flat_map(|s| s.entries.iter())
        .flat_map(MenuEntry::items)
        .collect()
}

#[test]
fn tab_context_actions_round_trip_and_stay_out_of_the_bar() {
    for a in [MenuAction::CopySessionId, MenuAction::CopyCwd] {
        let name = format!("{a:?}");
        assert_eq!(MenuAction::from_invoke_name(&name), Some(a));
        assert_eq!(
            a.invoke_authority(),
            super::InvokeAuthority::ClipboardWrite,
            "{a:?} boundary"
        );
        assert!(
            !model_items().iter().any(|(_, action, _, _)| *action == a),
            "{a:?} is context-menu-only, never a bar item"
        );
    }
}

#[test]
fn every_earlier_invoke_name_resolves_or_is_retired() {
    for name in EARLIER_INVOKE_NAMES {
        match (
            MenuAction::from_invoke_name(name),
            super::retired_invoke_name(name),
        ) {
            (Some(action), None) => assert_eq!(
                format!("{action:?}"),
                super::canonical_invoke_name(name),
                "{name} names the action its canonical spelling does"
            ),
            (None, Some(instead)) => assert!(!instead.is_empty(), "{name}"),
            other => panic!("{name}: resolves or is retired, never both or neither: {other:?}"),
        }
    }
    for (old, new) in [
        ("HoldSession", MenuAction::TakeOver),
        ("LiftHold", MenuAction::HandBack),
        ("LedgerForSession", MenuAction::HarnessLog),
        ("OpenLedger", MenuAction::HarnessLog),
        ("ShowConnectionMap", MenuAction::Overview),
        ("Fleet", MenuAction::Overview),
        ("ShowFleet", MenuAction::Overview),
    ] {
        assert_eq!(MenuAction::from_invoke_name(old), Some(new), "{old}");
    }
    for retired in [
        "Inbox",
        "ShowInbox",
        "ConnectToSession",
        "ConfigureConnection",
        "DisconnectSession",
        "NewControlledWindow",
        "NewControlledTab",
        "NewControllerWindow",
        "NewControllerTab",
        "TogglePresenceBand",
    ] {
        let refusal = crate::control::control_media::invoke_refusal(retired)
            .unwrap_or_else(|| panic!("{retired} is refused before the hop"));
        assert!(refusal.starts_with("ERR retired: "), "{retired}: {refusal}");
        assert!(
            crate::palette::PaletteState::new()
                .action_by_name(retired)
                .is_err_and(|e| e.starts_with("retired: ")),
            "{retired}: the palette's lookup says so too"
        );
    }
    assert_eq!(
        crate::control::control_media::invoke_refusal("NewTab"),
        None
    );
}

#[test]
fn the_agents_menu_holds_its_rows_in_order_with_their_classes() {
    use super::{InvokeAuthority, MenuMods};
    let titles: Vec<&str> = MENU_MODEL.iter().map(|s| s.title).collect();
    let agents_at = titles
        .iter()
        .position(|t| *t == "Agents")
        .expect("an Agents menu");
    assert_eq!(titles[agents_at - 1], "View");
    assert_eq!(titles[agents_at + 1], "Window");
    assert!(!titles.contains(&"Fabric"), "the Fabric menu is gone");
    let agents = &MENU_MODEL[agents_at];
    let rows: Vec<(MenuAction, &str, MenuMods)> = agents
        .entries
        .iter()
        .flat_map(MenuEntry::items)
        .map(|(_, action, key, mods)| (action, key, mods))
        .collect();
    assert_eq!(
        rows,
        [
            (MenuAction::NextNeed, "j", MenuMods::CommandShift),
            (MenuAction::Overview, "o", MenuMods::CommandControl),
            (MenuAction::Workers, "", MenuMods::None),
            (MenuAction::TakeOver, "", MenuMods::None),
            (MenuAction::HandBack, "", MenuMods::None),
            (MenuAction::HandBackAll, "", MenuMods::None),
            (MenuAction::HarnessLog, "l", MenuMods::CommandShift),
            (MenuAction::SetRole, "", MenuMods::None),
            (MenuAction::ReadThisMac, "", MenuMods::None),
            (MenuAction::ReadFleet, "", MenuMods::None),
            (MenuAction::SummarizeFleet, "", MenuMods::None),
        ]
    );
    assert_eq!(
        agents
            .entries
            .iter()
            .filter(|e| matches!(e, MenuEntry::Separator))
            .count(),
        3
    );
    for (action, ..) in &rows {
        let want = match action {
            MenuAction::TakeOver | MenuAction::HandBack | MenuAction::HandBackAll => {
                InvokeAuthority::PersonOnly
            }
            MenuAction::NextNeed | MenuAction::SetRole => InvokeAuthority::WriteInput,
            _ => InvokeAuthority::OwnerOnly,
        };
        assert_eq!(action.invoke_authority(), want, "{action:?}");
        let session_row = matches!(
            action,
            MenuAction::TakeOver
                | MenuAction::HandBack
                | MenuAction::HarnessLog
                | MenuAction::SetRole
        );
        assert_eq!(
            super::requires_terminal_tab(*action),
            session_row,
            "{action:?}"
        );
    }
    for (_, _, action) in super::PALETTE_ONLY
        .iter()
        .filter(|(section, ..)| *section == "Agents")
    {
        assert!(
            !model_items().iter().any(|(_, a, _, _)| a == action),
            "{action:?} is palette-only"
        );
        assert_eq!(
            action.invoke_authority(),
            InvokeAuthority::OwnerOnly,
            "{action:?}"
        );
    }
}

#[test]
fn take_over_and_hand_back_follow_the_front_person_hold() {
    set_active_tab_is_terminal(true);
    super::set_front_person_hold(false);
    assert!(native_menu_action_enabled(MenuAction::TakeOver));
    assert!(!native_menu_action_enabled(MenuAction::HandBack));
    super::set_front_person_hold(true);
    assert!(!native_menu_action_enabled(MenuAction::TakeOver));
    assert!(native_menu_action_enabled(MenuAction::HandBack));
    super::set_front_remote_hold(true);
    assert!(native_menu_action_enabled(MenuAction::TakeOver));
    assert!(native_menu_action_enabled(MenuAction::HandBack));
    super::set_front_remote_hold(false);
    super::set_front_person_hold(false);
    set_active_tab_is_terminal(false);
    assert!(!native_menu_action_enabled(MenuAction::TakeOver));
    assert!(!native_menu_action_enabled(MenuAction::HarnessLog));
    set_active_tab_is_terminal(true);
}

#[test]
fn the_agents_rows_grey_from_their_live_bits() {
    use super::{AgentsLive, native_item_tip, native_live_title, set_agents_live};
    set_active_tab_is_terminal(true);
    let quiet = AgentsLive::default();
    set_agents_live(quiet.clone());
    for a in [
        MenuAction::NextNeed,
        MenuAction::Overview,
        MenuAction::ReadFleet,
        MenuAction::FleetMachines,
    ] {
        assert!(
            !native_menu_action_enabled(a),
            "{a:?} greys without its fact"
        );
    }
    assert!(native_menu_action_enabled(MenuAction::SummarizeFleet));
    assert!(
        native_menu_action_enabled(MenuAction::ReadThisMac),
        "always live"
    );
    assert_eq!(native_item_tip(MenuAction::SummarizeFleet), None);
    set_agents_live(AgentsLive {
        needs_local: true,
        in_fleet: true,
        any_window: true,
        summarize_block: Some("(claude not found)".to_string()),
    });
    for a in [
        MenuAction::NextNeed,
        MenuAction::Overview,
        MenuAction::ReadFleet,
        MenuAction::FleetMachines,
    ] {
        assert!(native_menu_action_enabled(a), "{a:?} live with its fact");
    }
    assert!(!native_menu_action_enabled(MenuAction::SummarizeFleet));
    assert!(
        native_item_tip(MenuAction::SummarizeFleet)
            .is_some_and(|tip| tip.ends_with("(claude not found)")),
        "the greyed row says why"
    );
    assert_eq!(
        native_live_title(MenuAction::SummarizeFleet),
        Some(super::summarize_title(true))
    );
    set_agents_live(quiet);
    assert_eq!(
        native_live_title(MenuAction::SummarizeFleet),
        Some(super::summarize_title(false))
    );
    set_agents_live(AgentsLive::UNPUBLISHED);
}

#[test]
fn the_native_bar_checks_and_greys_what_the_palette_does() {
    use super::{MenuLive, menu_live, native_live_title, native_menu_checked, set_menu_live};
    set_active_tab_is_terminal(true);
    let before = menu_live();
    set_menu_live(MenuLive::UNPUBLISHED);
    assert_eq!(menu_live(), MenuLive::UNPUBLISHED);
    for a in [
        MenuAction::Copy,
        MenuAction::NextTab,
        MenuAction::PrevTab,
        MenuAction::ReopenClosedTab,
        MenuAction::ReopenClosedView,
    ] {
        assert!(native_menu_action_enabled(a), "{a:?}");
    }
    assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Tab"));
    for action in [MenuAction::Find, MenuAction::FindNext, MenuAction::FindPrev] {
        assert!(!native_menu_action_enabled(action), "{action:?}");
    }

    let idle = MenuLive {
        has_selection: false,
        can_save: false,
        can_find: false,
        can_find_again: false,
        multi_tab: false,
        can_reopen_closed_tab: false,
        can_reopen_closed_view: false,
        serious_mode: true,
        rain_on: true,
        kitty_favourited: true,
        front_tab_split: true,
        can_rewind: false,
        aos_attached: false,
        aos_unseen: false,
    };
    set_menu_live(idle);
    assert_eq!(menu_live(), idle, "every bit round-trips");
    assert!(
        !native_menu_action_enabled(MenuAction::RecordUnseenKeys),
        "Record Keys Not Echoed… needs a recording"
    );
    assert_eq!(
        native_menu_checked(MenuAction::RecordUnseenKeys),
        Some(false)
    );
    set_menu_live(MenuLive {
        aos_attached: true,
        aos_unseen: true,
        ..idle
    });
    assert_eq!(
        (menu_live().aos_attached, menu_live().aos_unseen),
        (true, true),
        "the recording's bits round-trip"
    );
    assert!(native_menu_action_enabled(MenuAction::RecordUnseenKeys));
    assert_eq!(
        native_menu_checked(MenuAction::RecordUnseenKeys),
        Some(true),
        "its checkmark is the grant"
    );
    assert_eq!(
        MenuAction::RecordUnseenKeys.invoke_authority(),
        super::InvokeAuthority::PersonOnly,
        "only the person at the window grants it"
    );
    assert!(!MenuAction::RecordUnseenKeys.writes_pty_input());
    assert!(super::requires_terminal_tab(MenuAction::RecordUnseenKeys));
    assert_eq!(
        MenuAction::RecordUnseenKeys.offered_on_this_platform(),
        cfg!(target_os = "macos"),
        "aterm's alert, the one door to the grant, is macOS's"
    );
    assert!(!native_menu_action_enabled(MenuAction::Save));
    assert!(!native_menu_action_enabled(MenuAction::RewindPane));
    assert!(!native_menu_action_enabled(MenuAction::ForgetPaneHistory));
    assert!(
        native_menu_action_enabled(MenuAction::ForgetAllHistory),
        "Forget All History needs no front pane that records"
    );
    assert_eq!(
        native_item_tip(MenuAction::RewindPane).as_deref(),
        Some(crate::rewind::REWIND_OFF_TIP),
        "a pane that keeps no history says what turns Rewind on"
    );
    assert_eq!(
        native_item_tip(MenuAction::ForgetPaneHistory).as_deref(),
        Some(crate::rewind::REWIND_OFF_TIP)
    );
    set_menu_live(MenuLive {
        can_rewind: true,
        ..idle
    });
    assert!(menu_live().can_rewind, "the Rewind bit round-trips");
    assert_eq!(native_item_tip(MenuAction::RewindPane), None);
    assert!(native_menu_action_enabled(MenuAction::ForgetPaneHistory));
    assert_eq!(
        MenuAction::ForgetPaneHistory.invoke_authority(),
        super::InvokeAuthority::WriteInput
    );
    assert_eq!(
        MenuAction::ForgetAllHistory.invoke_authority(),
        super::InvokeAuthority::OwnerOnly,
        "one session's scope never drops every pane's history"
    );
    set_menu_live(MenuLive {
        can_save: true,
        ..idle
    });
    assert!(native_menu_action_enabled(MenuAction::Save));
    for (open, again) in [(true, false), (true, true), (false, false)] {
        set_menu_live(MenuLive {
            can_find: open,
            can_find_again: again,
            ..idle
        });
        assert_eq!(native_menu_action_enabled(MenuAction::Find), open);
        assert_eq!(native_menu_action_enabled(MenuAction::FindNext), again);
        assert_eq!(native_menu_action_enabled(MenuAction::FindPrev), again);
        assert_eq!(menu_live().can_find, open);
        assert_eq!(menu_live().can_find_again, again);
    }
    set_menu_live(idle);
    for a in [
        MenuAction::Copy,
        MenuAction::NextTab,
        MenuAction::PrevTab,
        MenuAction::ReopenClosedTab,
        MenuAction::ReopenClosedView,
    ] {
        assert!(!native_menu_action_enabled(a), "{a:?} greys");
    }
    assert!(native_menu_action_enabled(MenuAction::Paste), "untouched");
    for a in [
        MenuAction::ToggleSeriousMode,
        MenuAction::ToggleMatrixRain,
        MenuAction::FavouriteKitty,
    ] {
        assert_eq!(native_menu_checked(a), Some(true), "{a:?}");
    }
    assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Pane"));
    assert_eq!(native_live_title(MenuAction::Copy), None);

    let off = MenuLive {
        serious_mode: false,
        rain_on: false,
        kitty_favourited: false,
        front_tab_split: false,
        ..MenuLive::UNPUBLISHED
    };
    set_menu_live(off);
    for a in [
        MenuAction::ToggleSeriousMode,
        MenuAction::ToggleMatrixRain,
        MenuAction::FavouriteKitty,
    ] {
        assert_eq!(native_menu_checked(a), Some(false), "{a:?}");
    }
    assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Tab"));
    set_menu_live(before);
}

#[test]
fn chrome_reads_the_close_row_live_not_as_last_stamped() {
    use super::{MenuLive, menu_live, native_row_title, set_menu_live};
    let before = menu_live();
    let close = MenuAction::CloseTab.tag();
    set_menu_live(MenuLive {
        front_tab_split: false,
        ..MenuLive::UNPUBLISHED
    });
    assert_eq!(
        native_row_title(close, "Close Pane".to_string()),
        "Close Tab"
    );
    set_menu_live(MenuLive {
        front_tab_split: true,
        ..MenuLive::UNPUBLISHED
    });
    assert_eq!(
        native_row_title(close, "Close Tab".to_string()),
        "Close Pane"
    );
    let copy = MenuAction::Copy.tag();
    assert_eq!(native_row_title(copy, "Copy".to_string()), "Copy");
    assert_eq!(
        native_row_title(0, "Emoji & Symbols".to_string()),
        "Emoji & Symbols"
    );
    set_menu_live(before);
}

#[test]
fn every_action_decodes_from_its_tag_and_its_invoke_name() {
    let mut by_tag = std::collections::HashMap::new();
    for a in offered_actions() {
        if let Some(other) = by_tag.insert(a.tag(), a) {
            assert_eq!(other, a, "{a:?} and {other:?} share tag {}", a.tag());
        }
        assert_eq!(MenuAction::from_tag(a.tag()), Some(a), "{a:?}");
        assert_eq!(
            MenuAction::from_invoke_name(&format!("{a:?}")),
            Some(a),
            "the invoke fence must decode {a:?}"
        );
    }
    for tag in 0..=255isize {
        if let Some(a) = MenuAction::from_tag(tag) {
            assert_eq!(a.tag(), tag, "tag {tag} decodes to {a:?}");
        }
    }
    assert_eq!(MenuAction::from_tag(0), None);
    assert_eq!(MenuAction::from_tag(-1), None);
    assert_eq!(MenuAction::from_tag(9999), None);
    assert_eq!(MenuAction::from_invoke_name("NopeNotAnAction"), None);
    assert_eq!(MenuAction::from_invoke_name(""), None);
}

#[test]
fn staged_update_menu_action_is_selectable_on_every_active_tab_kind() {
    let model = aterm_models_misc::native_update_menu_activation_model();
    let mut state = model.init_state();
    for action in ["StageUpdate", "RefreshStagedVersionMenu"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(
        model_items()
            .iter()
            .any(|(_, action, _, _)| *action == MenuAction::ApplyUpdate),
        "the portable/native shared menu description carries ApplyUpdate"
    );
    set_active_tab_is_terminal(false);
    assert!(
        native_menu_action_enabled(MenuAction::ApplyUpdate),
        "the update row must remain clickable while Settings is frontmost"
    );
    assert!(
        !native_menu_action_enabled(MenuAction::SplitVertical),
        "the fixture still disables genuinely terminal-only commands"
    );
    assert_eq!(
        MenuAction::from_tag(MenuAction::ApplyUpdate.tag()),
        Some(MenuAction::ApplyUpdate),
        "the enabled native row must decode to the exact apply command"
    );
    assert!(model.fire("DecodeApplyTag", &mut state));
    assert!(model.fire("DispatchApply", &mut state));
    assert_eq!(state["apply_dispatched"], 1);

    set_active_tab_is_terminal(true);
    assert!(native_menu_action_enabled(MenuAction::ApplyUpdate));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut disabled = buggy.init_state();
    assert!(buggy.fire("StageUpdate", &mut disabled));
    assert!(buggy.fire("RefreshStagedVersionMenu", &mut disabled));
    assert_eq!(disabled["terminal_tab"], 0);
    assert_eq!(disabled["row_enabled"], 0);
    assert!(buggy.successors("DecodeApplyTag", &disabled).is_empty());
}

#[test]
fn chrome_lines_render_titled_sections() {
    let lines = menu_chrome_lines(None, false);
    let titles: Vec<&str> = MENU_MODEL.iter().map(|s| s.title).collect();
    assert_eq!(
        titles,
        [
            "aterm", "File", "Edit", "View", "Agents", "Window", "Help", "Version", "Machine"
        ]
    );
    let shown = usize::from(cfg!(target_os = "macos")) + titles.len() - 1;
    assert_eq!(
        lines.len(),
        shown + 1,
        "sections this platform offers, plus the Find submenu"
    );
    for section in MENU_MODEL {
        let head = format!("menu {:?}: ", section.title);
        let Some(line) = lines.iter().find(|line| line.starts_with(&head)) else {
            assert!(
                section.title == super::MACHINE_SECTION && !cfg!(target_os = "macos"),
                "{head} is missing"
            );
            continue;
        };
        let offered = section
            .entries
            .iter()
            .filter(|entry| match entry {
                MenuEntry::Item { action, .. } => {
                    action.offered_on_this_platform() && *action != MenuAction::ApplyUpdate
                }
                MenuEntry::Submenu { .. } => true,
                MenuEntry::Separator => false,
            })
            .count();
        let listed = line[head.len()..]
            .split(", ")
            .filter(|label| !label.is_empty())
            .count();
        assert_eq!(listed, offered, "{line}");
    }
    assert!(
        !lines.iter().any(|l| l.contains(", ,")),
        "no separator artifact: {lines:?}"
    );
}

#[test]
fn find_submenu_keeps_commands_shortcuts_and_palette_access() {
    let edit = super::MENU_MODEL
        .iter()
        .find(|section| section.title == "Edit")
        .unwrap();
    let nested = edit
        .entries
        .iter()
        .find_map(|entry| match entry {
            MenuEntry::Submenu {
                label: "Find",
                entries,
            } => Some(*entries),
            _ => None,
        })
        .expect("one Find submenu");
    assert!(
        !edit.entries.iter().any(|entry| matches!(
            entry,
            MenuEntry::Item {
                action: MenuAction::Find | MenuAction::FindNext | MenuAction::FindPrev,
                ..
            }
        )),
        "search commands are grouped, not repeated beside their submenu"
    );
    let commands: Vec<_> = nested
        .iter()
        .flat_map(MenuEntry::items)
        .map(|(_, action, key, mods)| (action, key, mods))
        .collect();
    assert_eq!(
        commands,
        [
            (MenuAction::Find, "f", super::MenuMods::Command),
            (MenuAction::FindNext, "g", super::MenuMods::Command),
            (MenuAction::FindPrev, "g", super::MenuMods::CommandShift),
        ]
    );
    let chrome = super::chrome_lines_for("Edit", edit.entries, None, false);
    assert!(chrome[0].contains("Find ▸"));
    let submenu_rows = chrome[1]
        .strip_prefix("menu \"Edit ▸ Find\": ")
        .expect("the nested menu has its own inspection path");
    assert_eq!(submenu_rows.split(", ").count(), commands.len());
    let palette = crate::palette::PaletteState::new();
    for name in ["Find", "FindNext", "FindPrev"] {
        let action = MenuAction::from_invoke_name(name).unwrap();
        assert_eq!(palette.action_by_name(name), Ok(action));
        assert_eq!(
            model_items()
                .iter()
                .filter(|(_, found, _, _)| *found == action)
                .count(),
            1
        );
    }
}

#[test]
fn update_rows_are_not_offered_where_no_updater_exists() {
    let offered = |label: &str| {
        menu_chrome_lines(None, false)
            .iter()
            .any(|l| l.contains(label))
    };
    let updater = aterm_update::enabled();
    if cfg!(windows) {
        assert!(!updater, "Windows has no in-app updater");
    }
    if cfg!(target_os = "macos") {
        assert!(updater, "macOS has the in-app updater");
    }
    assert_eq!(
        MenuAction::SoftwareUpdate.offered_on_this_platform(),
        updater
    );
    // The apply row follows the updater, and Windows, which has none, still installs a build
    // staged on the machine.
    assert_eq!(
        MenuAction::ApplyUpdate.offered_on_this_platform(),
        updater || aterm_update::staged_notice_enabled()
    );
    assert_eq!(aterm_update::staged_notice_enabled(), cfg!(windows));
    assert!(!offered("Check for Updates…"));
    assert!(!offered("Install"), "{:?}", menu_chrome_lines(None, false));
    for section in MENU_MODEL {
        for (_, action, _, _) in section.entries.iter().flat_map(MenuEntry::items) {
            // The updater's rows follow the updater; the macOS-only rows are checked where
            // they are offered.
            let macos_only = matches!(
                action,
                MenuAction::SystemState | MenuAction::RecordUnseenKeys | MenuAction::RenameMachine
            );
            if macos_only {
                assert_eq!(
                    action.offered_on_this_platform(),
                    cfg!(target_os = "macos"),
                    "{action:?}"
                );
            } else if !matches!(action, MenuAction::SoftwareUpdate | MenuAction::ApplyUpdate) {
                assert!(action.offered_on_this_platform(), "{action:?}");
            }
        }
    }
}

/// Windows offers "Install … now" exactly while a newer build is staged on the machine: the
/// row is absent with nothing staged (the negative control), present with a staged build, and
/// the network checker's row stays off Windows either way.
#[test]
fn the_apply_row_is_offered_on_windows_only_while_a_build_is_staged() {
    let install_rows = |apply_row: Option<&str>| -> Vec<String> {
        menu_chrome_lines(apply_row, false)
            .into_iter()
            .filter(|line| line.contains("Install"))
            .collect()
    };
    assert!(
        install_rows(None).is_empty(),
        "nothing staged, no row: {:?}",
        install_rows(None)
    );
    let label = staged_apply_label("\u{2191}", 101, "9.9.0", None, false);
    assert_eq!(label, "\u{2191} Install aterm v9.9.0 now");
    let staged = install_rows(Some(&label));
    if MenuAction::ApplyUpdate.offered_on_this_platform() {
        assert_eq!(staged.len(), 1, "{staged:?}");
        assert!(
            staged[0].starts_with("menu \"Version\": ") && staged[0].contains(&label),
            "{staged:?}"
        );
    } else {
        assert!(staged.is_empty(), "{staged:?}");
    }
    #[cfg(windows)]
    {
        assert!(MenuAction::ApplyUpdate.offered_on_this_platform());
        assert_eq!(staged.len(), 1, "a staged build is offered: {staged:?}");
        assert!(
            !MenuAction::SoftwareUpdate.offered_on_this_platform(),
            "there is no checker to open on Windows"
        );
        assert!(
            !menu_chrome_lines(Some(&label), false)
                .iter()
                .any(|line| line.contains("Check for Updates")),
            "the staged build adds one row and nothing else"
        );
    }
}

#[test]
fn allocation_is_a_read_only_native_app_action_on_every_platform() {
    let app = MENU_MODEL
        .iter()
        .find(|section| section.title == "aterm")
        .unwrap();
    let rows: Vec<_> = app
        .entries
        .iter()
        .flat_map(MenuEntry::items)
        .filter(|(_, action, _, _)| *action == MenuAction::Allocation)
        .collect();
    assert_eq!(
        rows,
        [(
            "Allocation…",
            MenuAction::Allocation,
            "",
            super::MenuMods::None
        )]
    );
    assert!(MenuAction::Allocation.offered_on_this_platform());
    assert_eq!(MenuAction::Allocation.tag(), 100);
    assert_eq!(MenuAction::Workers.tag(), 99);
    assert_eq!(MenuAction::from_tag(100), Some(MenuAction::Allocation));
    assert_eq!(
        MenuAction::from_invoke_name("Allocation"),
        Some(MenuAction::Allocation)
    );
    assert_eq!(
        MenuAction::Allocation.invoke_authority(),
        super::InvokeAuthority::OwnerOnly
    );
    assert!(!MenuAction::Allocation.writes_pty_input());
    assert!(!super::requires_terminal_tab(MenuAction::Allocation));
    let mut palette = crate::palette::PaletteState::new();
    palette.resolve(&crate::palette::PaletteLive::default());
    assert!(palette.action_by_name("Allocation").is_ok());
}

#[test]
fn system_state_is_an_app_menu_row_where_its_setup_runs() {
    let app = MENU_MODEL.iter().find(|s| s.title == "aterm").unwrap();
    let rows: Vec<_> = app
        .entries
        .iter()
        .flat_map(MenuEntry::items)
        .filter(|(_, action, _, _)| *action == MenuAction::SystemState)
        .collect();
    assert_eq!(
        rows,
        [(
            "System State…",
            MenuAction::SystemState,
            "",
            super::MenuMods::None
        )]
    );
    let macos = cfg!(target_os = "macos");
    assert_eq!(MenuAction::SystemState.offered_on_this_platform(), macos);
    let chrome = menu_chrome_lines(None, false);
    assert_eq!(
        chrome.iter().any(|l| l.contains("System State…")),
        macos,
        "{chrome:?}"
    );
    let mut palette = crate::palette::PaletteState::new();
    palette.resolve(&crate::palette::PaletteLive::default());
    assert_eq!(palette.action_by_name("SystemState").is_ok(), macos);
    assert_eq!(
        MenuAction::from_invoke_name("SystemState"),
        Some(MenuAction::SystemState)
    );
    assert_eq!(
        MenuAction::SystemState.invoke_authority(),
        MenuAction::ToggleSettings.invoke_authority()
    );
    assert!(!MenuAction::SystemState.writes_pty_input());
    assert!(!super::requires_terminal_tab(MenuAction::SystemState));
}

#[test]
fn critical_key_equivalents_are_correct() {
    let item = |action: MenuAction| {
        model_items()
            .into_iter()
            .find_map(|(_, a, key, mods)| (a == action).then_some((key, mods)))
            .unwrap_or_else(|| panic!("{action:?} not in MENU_MODEL"))
    };
    assert_eq!(
        item(MenuAction::ViewSessionInNewWindow),
        ("o", super::MenuMods::CommandShift)
    );
    assert_eq!(
        item(MenuAction::ReopenClosedTab),
        ("t", super::MenuMods::CommandShift)
    );
    assert_eq!(
        item(MenuAction::SplitVertical),
        ("d", super::MenuMods::Command)
    );
    assert_eq!(
        item(MenuAction::SplitHorizontal),
        ("d", super::MenuMods::CommandShift)
    );
    assert_eq!(
        item(MenuAction::ToggleFullScreen),
        ("f", super::MenuMods::CommandControl)
    );
    assert_eq!(
        item(MenuAction::ToggleSettings),
        (",", super::MenuMods::Command)
    );
    assert_eq!(
        item(MenuAction::OpenEditor),
        ("o", super::MenuMods::Command)
    );
    for (action, key, mods) in [
        (MenuAction::HarnessLog, "l", super::MenuMods::CommandShift),
        (MenuAction::NextNeed, "j", super::MenuMods::CommandShift),
        (MenuAction::Overview, "o", super::MenuMods::CommandControl),
    ] {
        assert_eq!(item(action), (key, mods), "{action:?}");
        let claimed: Vec<MenuAction> = model_items()
            .into_iter()
            .filter(|(_, _, k, m)| *k == key && *m == mods)
            .map(|(_, a, _, _)| a)
            .collect();
        assert_eq!(claimed, [action], "{key} {mods:?}");
    }
}

#[test]
fn version_signature_composes_every_build_kind() {
    use super::{VersionFacts, VersionRung, version_signature};
    let facts = |release: bool, dev_commits: &'static str, commit: &'static str| VersionFacts {
        release,
        version: "0.101.0",
        dev_commits,
        commit,
    };
    let release = facts(true, "1576", "25a5963a4c21");
    assert_eq!(
        version_signature(release, VersionRung::Full).as_deref(),
        Some("v0.101")
    );
    assert_eq!(
        version_signature(release, VersionRung::Short),
        None,
        "a release has no short rung"
    );
    let dev = facts(false, "1576", "25a5963a4c21");
    assert_eq!(
        version_signature(dev, VersionRung::Full).as_deref(),
        Some("v0.101.1576+g25a5963 \u{00B7} DEV")
    );
    assert_eq!(
        version_signature(dev, VersionRung::Short).as_deref(),
        Some("v0.101.1576 \u{00B7} DEV")
    );
    let dirty = facts(false, "1576", "25a5963a4c21-dirty");
    assert_eq!(
        version_signature(dirty, VersionRung::Full).as_deref(),
        Some("v0.101.1576+g25a5963* \u{00B7} DEV")
    );
    assert_eq!(
        version_signature(dirty, VersionRung::Short).as_deref(),
        Some("v0.101.1576* \u{00B7} DEV"),
        "the short rung keeps the dirty mark"
    );
    let no_git = facts(false, "0", "unknown");
    assert_eq!(
        version_signature(no_git, VersionRung::Full).as_deref(),
        Some("v0.101.0+gunknown \u{00B7} DEV")
    );
    assert_eq!(
        version_signature(no_git, VersionRung::Short).as_deref(),
        Some("v0.101.0 \u{00B7} DEV")
    );
}

#[test]
fn the_bar_title_is_the_full_signature_plus_its_colour_glyphs() {
    use super::{
        VersionFacts, VersionRung, strip_version_label, version_menu_bar_title, version_signature,
    };
    let plain =
        version_signature(VersionFacts::this_build(), VersionRung::Full).expect("a full rung");
    let wrench = if crate::build_info::is_release_build() {
        ""
    } else {
        "\u{1F6E0}\u{FE0F} "
    };
    for attention in [false, true] {
        let arrow = if attention { " \u{2B06}\u{FE0F}" } else { "" };
        assert_eq!(
            version_menu_bar_title(attention),
            format!("{wrench}{plain}{arrow}"),
            "attention={attention}"
        );
    }
    let quiet = strip_version_label(VersionRung::Full, false).expect("a full rung");
    assert_eq!(
        strip_version_label(VersionRung::Full, true),
        Some(format!("{quiet} \u{2191}")),
        "the strip's attention mark is a plain U+2191 after one space"
    );
}

#[test]
fn the_strip_label_is_the_quiet_version() {
    use super::{VersionFacts, VersionRung, strip_version_text};
    let dirty = VersionFacts {
        release: false,
        version: "0.106.0",
        dev_commits: "3605",
        commit: "8b6f0a8c1d2e-dirty",
    };
    assert_eq!(
        strip_version_text(dirty, VersionRung::Full).as_deref(),
        Some("v0.106.3605")
    );
    assert_eq!(
        strip_version_text(dirty, VersionRung::Short).as_deref(),
        Some("v0.106")
    );
    let release = VersionFacts {
        release: true,
        ..dirty
    };
    assert_eq!(
        strip_version_text(release, VersionRung::Full).as_deref(),
        Some("v0.106")
    );
    assert_eq!(strip_version_text(release, VersionRung::Short), None);
}

#[test]
fn the_strip_label_carries_no_colour_emoji() {
    use super::{VersionRung, strip_version_label};
    for rung in [VersionRung::Full, VersionRung::Short] {
        for attention in [false, true] {
            let Some(label) = strip_version_label(rung, attention) else {
                assert!(
                    crate::build_info::is_release_build() && rung == VersionRung::Short,
                    "only a release's short rung is absent"
                );
                continue;
            };
            for c in label.chars() {
                assert!(u32::from(c) <= 0xFFFF, "{c:?} is past the BMP in {label:?}");
                assert!(
                    !matches!(c, '\u{FE0F}' | '\u{1F6E0}' | '\u{2B06}'),
                    "{c:?} is a colour glyph in {label:?}"
                );
            }
            assert!(
                !label.contains("DEV"),
                "the strip label is quiet: {label:?}"
            );
        }
    }
}

#[test]
fn version_slot_click_opens_about_and_details_only_where_offered() {
    use super::{MenuAction, version_slot_click};
    assert_eq!(version_slot_click(false), MenuAction::Version);
    let staged = version_slot_click(true);
    if MenuAction::SoftwareUpdate.offered_on_this_platform() {
        assert_eq!(staged, MenuAction::SoftwareUpdate);
    } else {
        assert_eq!(staged, MenuAction::Version);
    }
    #[cfg(windows)]
    assert_eq!(
        staged,
        MenuAction::Version,
        "Windows has no updater, so the label is always About"
    );
    for attention in [false, true] {
        assert_ne!(
            version_slot_click(attention),
            MenuAction::ApplyUpdate,
            "a version label never installs"
        );
    }
}

#[test]
fn the_handoff_target_title_is_the_successor_s_plain_version() {
    let b = 1_790_278_596;
    assert_eq!(
        super::handoff_target_bar_title(b, false, Some((b, "0.92.0"))),
        Some("v0.92.0".to_string())
    );
    assert_eq!(
        super::handoff_target_bar_title(b, true, Some((b, "0.92.0"))),
        None
    );
    assert_eq!(
        super::handoff_target_bar_title(b, false, Some((b + 1, "0.93.0"))),
        None
    );
    assert_eq!(
        super::handoff_target_bar_title(b, false, Some((b, ""))),
        None
    );
    assert_eq!(super::handoff_target_bar_title(b, false, None), None);
}

#[test]
fn the_version_menu_never_offers_the_running_build() {
    let running = 1_790_278_596;
    for stale in [running, running - 158_101] {
        let staged = super::staged_for_version_menu(Some((stale, "0.92.0")), running);
        assert_eq!(staged, None, "build {stale} is not newer than {running}");
        let title = super::version_menu_bar_title(super::bar_title_attention(
            staged.is_some(),
            false,
            false,
        ));
        assert_eq!(
            title,
            super::version_menu_bar_title(false),
            "no arrow over the running build"
        );
        assert!(!title.contains('\u{2B06}'), "{title}");
    }
    assert_eq!(
        super::staged_for_version_menu(Some((running + 1, "0.93.0")), running),
        Some((running + 1, "0.93.0")),
        "a strictly newer build is still the offer"
    );
    assert_eq!(
        super::staged_for_version_menu(Some((running, "0.92.0")), 0),
        Some((running, "0.92.0")),
        "an unnamed running build (0) offers any positive staged build"
    );
    assert_eq!(
        super::staged_for_version_menu(Some((0, "0.92.0")), 0),
        None,
        "staged_for_version_menu keeps strictly build > running_build"
    );
    assert_eq!(super::staged_for_version_menu(None, running), None);
}

#[test]
fn menu_bar_arrow_tracks_staged_not_realized() {
    assert!(
        super::bar_title_attention(true, false, false),
        "staged shows the arrow"
    );
    assert!(
        !super::bar_title_attention(false, false, true),
        "realized alone must not light the persistent bar arrow"
    );
    assert!(
        !super::bar_title_attention(false, false, false),
        "quiet steady state: no arrow"
    );
    assert!(
        super::bar_title_attention(true, false, true),
        "staged wins over realized"
    );
    assert!(
        !super::bar_title_attention(true, true, false),
        "a staged build on hold shows no arrow"
    );
    let realized_only =
        super::version_menu_bar_title(super::bar_title_attention(false, false, true));
    assert!(
        !realized_only.contains('\u{2B06}'),
        "realized-only bar title carries no arrow: {realized_only}"
    );
}

#[test]
fn native_terminate_arbiter_deduplicates_and_allows_only_after_completion() {
    let mut arbiter = super::NativeTerminateArbiter::new();
    let super::NativeTerminateDecision::Dispatch(first) = arbiter.request() else {
        panic!("first request must dispatch");
    };
    assert_eq!(
        arbiter.request(),
        super::NativeTerminateDecision::DeferExisting(first)
    );
    assert!(arbiter.is_current(first));
    assert!(arbiter.complete(first));
    assert_eq!(
        arbiter.request(),
        super::NativeTerminateDecision::AllowExit(first)
    );
}

#[test]
#[cfg(target_os = "macos")]
fn a_dropped_candidate_terminate_frees_the_arbiter() {
    let _serial = super::terminate_test_serial();
    let super::NativeTerminateDecision::Dispatch(logout) =
        super::admit_native_terminate(crate::system_quit::Asker::System)
    else {
        panic!("a first request dispatches");
    };
    assert!(crate::system_quit::asked_for(logout));
    let mut app = crate::App::headless_for_test();
    app.incoming_handoff_pending = true;
    assert!(
        app.drops_wake_before_commit(&crate::Wake::NativeTerminateRequested { generation: logout }),
        "before Commit the wake is still swallowed"
    );
    assert!(
        !crate::menu::native_termination_is_current(logout),
        "the swallowed quit is resolved"
    );
    assert!(
        !crate::system_quit::asked(),
        "a cancelled logout marks no later quit"
    );
    let next = super::admit_native_terminate(crate::system_quit::Asker::Person);
    assert!(
        matches!(next, super::NativeTerminateDecision::Dispatch(g) if g != logout),
        "the next logout reaches the event loop: {next:?}"
    );
    app.incoming_handoff_pending = false;
    assert!(
        !app.drops_wake_before_commit(&crate::Wake::NativeTerminateRequested {
            generation: next.generation()
        })
    );
    assert!(crate::menu::native_termination_is_current(
        next.generation()
    ));
    assert!(super::cancel_current_native_termination());
}

#[test]
fn native_terminate_arbiter_rejects_stale_generations_after_retry() {
    let mut arbiter = super::NativeTerminateArbiter::new();
    let super::NativeTerminateDecision::Dispatch(first) = arbiter.request() else {
        panic!("first request must dispatch");
    };
    assert!(arbiter.cancel(first));
    let super::NativeTerminateDecision::Dispatch(second) = arbiter.request() else {
        panic!("retry must dispatch a fresh generation");
    };
    assert_ne!(first, second);
    assert!(!arbiter.cancel(first));
    assert!(!arbiter.complete(first));
    assert!(arbiter.is_current(second));
    assert_eq!(arbiter.cancel_current(), Some(second));
    assert!(!arbiter.is_current(second));
}

/// AppKit waits on one reply per quit it was told to wait on: a cancel of that quit owes it
/// one NO, and nothing after; a quit it was never told to wait on owes it none; a completed
/// quit is answered YES by the event loop's exit alone, so no cancel refuses it, and AppKit
/// still waits until the process ends.
///
/// NEGATIVE CONTROL: make `answer_cancelled` return before its NO and the logout AppKit
/// waits on is never refused.
#[test]
#[cfg(unix)]
fn appkit_hears_one_reply_per_quit_it_waits_on() {
    use super::{TerminateReply, appkit_awaits_quit_reply, ask_native_terminate, offer_terminate};
    use crate::system_quit::Asker;
    let _serial = super::terminate_test_serial();
    let taken = |asker| {
        let TerminateReply::Later(generation) =
            offer_terminate(ask_native_terminate(asker, true), |_| true)
        else {
            panic!("{asker:?}: AppKit waits on the quit");
        };
        generation
    };
    let unawaited = super::admit_native_terminate(Asker::System).generation();
    assert!(super::cancel_native_termination(unawaited));
    assert!(
        super::take_refusals_for_test().is_empty(),
        "AppKit waited on nothing"
    );

    let refused = taken(Asker::System);
    assert!(super::cancel_native_termination(refused));
    assert!(!super::cancel_native_termination(refused));
    assert!(!super::cancel_current_native_termination());
    assert_eq!(
        super::take_refusals_for_test(),
        [refused],
        "one NO, never a second"
    );
    assert!(!appkit_awaits_quit_reply());

    let current = taken(Asker::Reopen);
    assert!(super::cancel_current_native_termination());
    assert_eq!(super::take_refusals_for_test(), [current]);

    let done = taken(Asker::System);
    assert!(super::complete_native_termination(done));
    assert!(!super::cancel_native_termination(done));
    assert!(!super::cancel_current_native_termination());
    assert!(
        super::take_refusals_for_test().is_empty(),
        "no NO for a quit that goes"
    );
    assert!(
        appkit_awaits_quit_reply(),
        "AppKit waits on the completed quit until the loop's exit answers YES"
    );
    assert!(
        !std::thread::spawn(appkit_awaits_quit_reply)
            .join()
            .expect("another thread"),
        "what AppKit waits on is the main thread's fact"
    );
}

/// A quit the system or Quit & Reopen asks for has AppKit wait on its reply, and a cancel of it
/// gives AppKit one NO; a person's quit is taken up with AppKit's ask cancelled, so AppKit
/// waits on nothing while the person's question stands and their Cancel owes it no NO; a quit
/// no event loop takes is cancelled at once and terminates now, AppKit never told to wait.
///
/// NEGATIVE CONTROL: answer the person's quit `Later` too (the code at 0007e00dcf) and AppKit
/// waits inside `terminate:` on the person's answer. Before 2026-11-06 every quit the window
/// took up answered AppKit `NSTerminateCancel`, so a logout, restart or shutdown was refused
/// while aterm quit on its own path.
#[test]
#[cfg(unix)]
fn a_logout_or_quit_and_reopen_waits_on_its_reply_and_a_persons_quit_on_none() {
    use super::{TerminateReply, appkit_awaits_quit_reply, ask_native_terminate, offer_terminate};
    use crate::system_quit::Asker;
    for asker in [Asker::Person, Asker::System, Asker::Reopen] {
        let _serial = super::terminate_test_serial();
        let mut offered = None;
        let reply = offer_terminate(ask_native_terminate(asker, true), |generation| {
            offered = Some(generation);
            true
        });
        let generation = offered.unwrap_or_else(|| panic!("{asker:?}: the window has it"));
        let waits = asker != Asker::Person;
        assert_eq!(
            reply,
            if waits {
                TerminateReply::Later(generation)
            } else {
                TerminateReply::TakeUp(generation)
            },
            "{asker:?}"
        );
        assert!(
            super::native_termination_is_current(generation),
            "{asker:?}"
        );
        assert_eq!(appkit_awaits_quit_reply(), waits, "{asker:?}");
        assert!(super::cancel_current_native_termination());
        assert_eq!(
            super::take_refusals_for_test(),
            if waits { vec![generation] } else { vec![] },
            "{asker:?}: a NO only for a quit AppKit waits on"
        );
        assert!(!appkit_awaits_quit_reply());
        assert!(!super::cancel_current_native_termination());
        assert!(
            super::take_refusals_for_test().is_empty(),
            "{asker:?}: one reply"
        );
    }
    let _serial = super::terminate_test_serial();
    let reply = ask_native_terminate(Asker::System, true);
    let TerminateReply::Later(generation) = reply else {
        panic!("a logout is taken up: {reply:?}");
    };
    assert_eq!(offer_terminate(reply, |_| false), TerminateReply::Now);
    assert!(!super::native_termination_is_current(generation));
    assert!(!appkit_awaits_quit_reply());
    assert!(
        super::take_refusals_for_test().is_empty(),
        "AppKit waited on nothing"
    );
    assert!(
        !crate::system_quit::asked(),
        "the quit nobody took is forgotten"
    );
}

/// A quit AppKit asks about from inside aterm's own handler (a modal alert or panel aterm
/// runs, whose loop dispatched it), where no reply could come from AppKit's wait, has AppKit
/// wait on nothing. A person's quit and a Quit & Reopen are still taken up, their wake posted,
/// so they quit once the alert or panel is answered (a Quit & Reopen leaving its reopen
/// helper, its mark kept). A logout, restart or shutdown is refused unadmitted: no generation
/// pending, no system mark, and a person's quit pending under it stands as it was, not
/// made the system's.
///
/// NEGATIVE CONTROL: cancel every unanswerable quit (the code at 0007e00dcf) and the Quit &
/// Reopen and the person's quit are dropped for good, and a logout sent onto the person's
/// quit cancels that quit too. Note the logout before refusing it and the person's quit,
/// still pending, reads as the system's: it would keep agents and give up account claims for
/// a logout macOS abandoned.
#[test]
#[cfg(unix)]
fn a_quit_asked_from_inside_the_handler_waits_on_nothing() {
    use super::{TerminateReply, appkit_awaits_quit_reply, ask_native_terminate, offer_terminate};
    use crate::system_quit::Asker;
    for asker in [Asker::Person, Asker::Reopen] {
        let _serial = super::terminate_test_serial();
        let mut offered = None;
        let reply = offer_terminate(ask_native_terminate(asker, false), |generation| {
            offered = Some(generation);
            true
        });
        let generation = offered.unwrap_or_else(|| panic!("{asker:?}: the window has it"));
        assert_eq!(reply, TerminateReply::TakeUp(generation), "{asker:?}");
        assert!(
            super::native_termination_is_current(generation),
            "{asker:?}"
        );
        assert!(!appkit_awaits_quit_reply(), "{asker:?}");
        assert_eq!(
            crate::system_quit::reopens(),
            asker == Asker::Reopen,
            "{asker:?}: the reopen goes with the quit"
        );
    }

    let _serial = super::terminate_test_serial();
    assert_eq!(
        offer_terminate(ask_native_terminate(Asker::System, false), |_| {
            panic!("nobody takes a refused logout up")
        }),
        TerminateReply::Cancel
    );
    assert!(!appkit_awaits_quit_reply());
    assert!(
        !crate::system_quit::asked(),
        "the refused logout marks nothing"
    );
    let super::NativeTerminateDecision::Dispatch(person) =
        super::admit_native_terminate(Asker::Person)
    else {
        panic!("the refused logout left nothing pending");
    };
    assert_eq!(
        ask_native_terminate(Asker::System, false),
        TerminateReply::Cancel,
        "a logout onto the person's quit"
    );
    assert!(
        super::native_termination_is_current(person),
        "the person's quit stands"
    );
    assert!(
        !crate::system_quit::asked() && !crate::system_quit::explicit(person),
        "the person's quit is not the abandoned logout's"
    );
    assert!(super::take_refusals_for_test().is_empty());
}

#[test]
fn the_help_destination_passes_the_url_gate() {
    let url = super::help_url();
    assert!(
        crate::is_safe_url(&url),
        "{url} must satisfy the same allowlist a clicked link does"
    );
}

#[test]
fn every_settings_deep_link_uses_the_dotted_scheme() {
    for pane in super::PrivacyPane::ALL {
        for url in super::privacy_settings_urls(pane) {
            assert!(
                url.starts_with("x-apple.systempreferences:"),
                "the scheme is dotted: {url:?}"
            );
            assert!(
                !url.contains("x-apple-systempreferences"),
                "the hyphenated spelling is a different scheme: {url:?}"
            );
        }
    }
    assert!(
        super::SETTINGS_FULL_DISK_ACCESS.ends_with("?Privacy_AllFiles"),
        "Full Disk Access is anchored at Privacy_AllFiles"
    );
    assert!(
        super::SETTINGS_FILES_AND_FOLDERS.ends_with("?Privacy_FilesAndFolders"),
        "the per-app repair route is anchored at Privacy_FilesAndFolders"
    );
    assert!(
        super::SETTINGS_DEVELOPER_TOOLS.ends_with("?Privacy_DevTools"),
        "the Developer Tools list is anchored at Privacy_DevTools"
    );
}

#[test]
fn the_last_candidate_is_the_unanchored_pane_root() {
    let fda = super::privacy_settings_urls(super::PrivacyPane::FullDiskAccess);
    for urls in super::PrivacyPane::ALL.map(super::privacy_settings_urls) {
        assert_eq!(urls[2], fda[2], "every pane degrades to the same page");
        let (root, anchored) = urls.split_last().expect("three candidates");
        assert!(
            !root.contains('?'),
            "the degradation target carries no anchor: {root:?}"
        );
        for url in anchored {
            assert!(
                url.contains('?'),
                "the first candidates are anchored: {url:?}"
            );
        }
    }
    for url in super::PrivacyPane::ALL
        .iter()
        .flat_map(|pane| super::privacy_settings_urls(*pane))
    {
        let anchor = url.split_once('?').map(|(_, a)| a);
        assert!(
            matches!(
                anchor,
                None | Some("Privacy_AllFiles" | "Privacy_FilesAndFolders" | "Privacy_DevTools")
            ),
            "only a REGISTERED anchor may ship: {url:?}"
        );
    }
}

#[test]
fn program_output_still_cannot_open_a_settings_pane() {
    for pane in super::PrivacyPane::ALL {
        for url in super::privacy_settings_urls(pane) {
            assert!(
                !crate::is_safe_url(url),
                "a clicked link must never be able to open Settings: {url:?}"
            );
        }
    }
}
