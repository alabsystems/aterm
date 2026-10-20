// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical command identity and authority classification.
//!
//! Menus, native semantic actions and editor commands are adapters onto this
//! registry.  The adapter matches are exhaustive: a new shipping action cannot
//! compile until it receives a stable command id and an authority decision.

use crate::menu;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CommandId(&'static str);

impl CommandId {
    pub(crate) const fn new(id: &'static str) -> Self {
        Self(id)
    }

    #[cfg(test)]
    pub(crate) const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandScope {
    Process,
    Window,
    Tab,
    View,
    Document,
    App,
}

/// Maximum capability a command may request from the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionAuthority {
    Observe,
    LocalUi,
    Clipboard,
    ConfigMutate,
    DocumentRead,
    DocumentWrite,
    ExternalOpen,
    UpdateStage,
    UpdateApply,
    Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentRequirement {
    Any,
    Terminal,
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    pub(crate) id: CommandId,
    pub(crate) scope: CommandScope,
    pub(crate) authority: ActionAuthority,
    pub(crate) content: ContentRequirement,
}

const fn spec(
    id: &'static str,
    scope: CommandScope,
    authority: ActionAuthority,
    content: ContentRequirement,
) -> CommandSpec {
    CommandSpec {
        id: CommandId::new(id),
        scope,
        authority,
        content,
    }
}

/// Exhaustive adapter for the platform menu and palette action vocabulary.
pub(crate) const fn menu_command(action: menu::MenuAction) -> CommandSpec {
    use ActionAuthority as A;
    use CommandScope as S;
    use ContentRequirement as C;
    use menu::MenuAction as M;

    match action {
        M::About => spec("app.settings.about", S::App, A::LocalUi, C::Any),
        M::SoftwareUpdate => spec("app.settings.updates", S::App, A::UpdateStage, C::Any),
        M::Version => spec("app.settings.version", S::App, A::Observe, C::Any),
        M::ApplyUpdate => spec("update.apply", S::Process, A::UpdateApply, C::Any),
        // A stable id; the config source opens in native Manual.
        M::Preferences => spec("config.open_source", S::Process, A::ConfigMutate, C::Any),
        M::Quit => spec("app.quit", S::Process, A::LocalUi, C::Any),
        M::NewWindow => spec("window.new", S::Process, A::LocalUi, C::Any),
        M::NewTab => spec("tab.new_terminal", S::Window, A::LocalUi, C::Any),
        M::OpenMarkdown => spec("document.open_markdown", S::Window, A::DocumentRead, C::Any),
        M::OpenEditor => spec("document.open_editor", S::Window, A::DocumentRead, C::Any),
        M::ReopenClosedTab => spec("tab.reopen_closed", S::Window, A::LocalUi, C::Any),
        M::ReopenClosedView => spec("view.reopen_closed", S::View, A::LocalUi, C::Any),
        M::MoveTabToNewWindow => spec("tab.move_new_window", S::Tab, A::LocalUi, C::Any),
        M::MoveTabToNextWindow => spec("tab.move_next_window", S::Tab, A::LocalUi, C::Any),
        M::ViewSessionInNewWindow => spec(
            "terminal.share_new_window",
            S::View,
            A::LocalUi,
            C::Terminal,
        ),
        // The Overview lists every session of the instance: Owner, the
        // aggregate-disclosure class of the `flows` verb.
        M::Overview => spec("view.overview", S::App, A::Owner, C::Any),
        // `CloseTab` closes the focused pane/view, hence the view id.
        M::CloseTab => spec("view.close_focused", S::View, A::LocalUi, C::Any),
        M::Copy => spec("selection.copy", S::View, A::Clipboard, C::Any),
        // Tab-context copies of session identity / cwd: clipboard authority, terminal content
        // only (a native tab owns no session).
        M::CopySessionId => spec("session.copy_id", S::Tab, A::Clipboard, C::Terminal),
        M::CopyCwd => spec("session.copy_cwd", S::Tab, A::Clipboard, C::Terminal),
        M::Paste => spec("selection.paste", S::View, A::Clipboard, C::Any),
        M::SelectAll => spec("selection.select_all", S::View, A::Clipboard, C::Any),
        M::Find => spec("view.find", S::View, A::LocalUi, C::Any),
        M::FindNext => spec("view.find_next", S::View, A::LocalUi, C::Any),
        M::FindPrev => spec("view.find_previous", S::View, A::LocalUi, C::Any),
        M::ToggleFullScreen => spec("window.fullscreen", S::Window, A::LocalUi, C::Any),
        M::FontIncrease => spec("window.text_scale_increase", S::Window, A::LocalUi, C::Any),
        M::FontDecrease => spec("window.text_scale_decrease", S::Window, A::LocalUi, C::Any),
        M::FontActualSize => spec("window.text_scale_reset", S::Window, A::LocalUi, C::Any),
        M::SplitVertical => spec("view.split_vertical", S::View, A::LocalUi, C::Any),
        M::SplitHorizontal => spec("view.split_horizontal", S::View, A::LocalUi, C::Any),
        M::ToggleMatrixRain => spec("effects.rain.toggle", S::Process, A::LocalUi, C::Any),
        // Promotes the front window's promotable kitty (its tenured program cat, else the launch
        // kitty) into the durable registry and pins it. Process-scoped and context-free: the
        // launch kitty is one cat for the whole process. `LocalUi` because the only durable write
        // is the machine-owned toy ledger, not config.
        M::FavouriteKitty => spec("effects.kitty.favourite", S::Process, A::LocalUi, C::Any),
        M::NextKitty => spec("effects.kitty.next", S::Process, A::LocalUi, C::Any),
        M::ToggleSeriousMode => spec(
            "effects.serious.toggle",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        M::ToggleSettings => spec("app.settings.open", S::App, A::ConfigMutate, C::Any),
        // Settings at the Packages route: the class of ToggleSettings (it raises the
        // durable-config Settings tab; the page's switches write [packages] through the OCC
        // editor).
        M::Packages => spec("app.settings.packages", S::App, A::ConfigMutate, C::Any),
        // Settings at the Messages route: the same class as Packages.
        M::Messages => spec("app.settings.messages", S::App, A::ConfigMutate, C::Any),
        // Settings ▸ Harness (the Autopilot design): the same Settings tab at
        // its Harness route, the same class as its siblings.
        M::HarnessSettings => spec("app.settings.harness", S::App, A::ConfigMutate, C::Any),
        M::OpenPalette => spec("palette.open", S::Window, A::Owner, C::Any),
        // The palette scoped to its Theme section: its Enter writes `theme`,
        // so it carries the config-write ceiling, not the gateway's Owner one.
        M::ChooseTheme => spec("palette.choose_theme", S::Window, A::ConfigMutate, C::Any),
        // Settings ▸ Themes, the Theme Showcase: the same Settings tab at its
        // Themes route, the same class as its siblings.
        M::BrowseThemes => spec("app.settings.themes", S::App, A::ConfigMutate, C::Any),
        M::Minimize => spec("window.minimize", S::Window, A::LocalUi, C::Any),
        M::Zoom => spec("window.maximize", S::Window, A::LocalUi, C::Any),
        M::NextTab => spec("tab.next", S::Window, A::LocalUi, C::Any),
        M::PrevTab => spec("tab.previous", S::Window, A::LocalUi, C::Any),
        // The inline pin editor. Tab-scoped because the gesture names a tab and the editor is
        // tab chrome (the write targets that tab's focused session); `LocalUi` because nothing
        // durable on disk moves, as the control layer classifies `meta set` as `WriteInput`.
        M::RenameSession => spec("session.rename", S::Tab, A::LocalUi, C::Terminal),
        // Same identity as the pin's face, one field over: the role is session
        // metadata written through the same typed API (`meta set role`).
        M::SetRole => spec("session.set_role", S::Tab, A::LocalUi, C::Terminal),
        // Identity rows: showing a session's identity discloses the roster row the `identities`
        // verb does (Owner-only on the wire); the two spawns may create an identity, which only
        // `spawn identity=` (Owner) may do. Neither spawn needs a focused session.
        M::ShowIdentity => spec("session.identity.show", S::Tab, A::Owner, C::Terminal),
        M::NewWindowWithIdentity => spec("window.new_with_identity", S::App, A::Owner, C::Any),
        M::NewTabWithIdentity => spec("tab.new_with_identity", S::Window, A::Owner, C::Any),
        M::Help => spec("app.help.open", S::App, A::ExternalOpen, C::Any),
        // The Agents menu. Next That Needs You moves focus (a local act).
        M::NextNeed => spec("agents.next_need", S::App, A::LocalUi, C::Any),
        // Converges on `App::open_session_ledger`; the id keeps the ledger's.
        M::HarnessLog => spec("session.ledger.open", S::Tab, A::ExternalOpen, C::Terminal),
        // The reads, Fleet Machines and Summarize run a child or start a
        // worker that sees the whole Mac or fleet: Owner, process-scoped.
        M::ReadThisMac => spec("agents.read_mac", S::Process, A::Owner, C::Any),
        M::ReadFleet => spec("agents.read_fleet", S::Process, A::Owner, C::Any),
        M::SummarizeFleet => spec("agents.summarize", S::Process, A::Owner, C::Any),
        M::FleetMachines => spec("agents.fleet_machines", S::Process, A::Owner, C::Any),
        // The person's hold pair (Take Over / Hand Back): the window's own act
        // on the person's slot, at the highest ceiling the registry names; the
        // `invoke` fence refuses every socket scope (`InvokeAuthority::PersonOnly`).
        M::TakeOver => spec("session.take_over", S::Tab, A::Owner, C::Terminal),
        M::HandBack => spec("session.hand_back", S::Tab, A::Owner, C::Terminal),
        // The message bus's three commands (`aterm fabric`) run as a child of
        // this process: Owner, process-scoped, no content requirement.
        M::FabricStatus => spec("fabric.status", S::Process, A::Owner, C::Any),
        M::FabricOn => spec("fabric.on", S::Process, A::Owner, C::Any),
        M::FabricOff => spec("fabric.off", S::Process, A::Owner, C::Any),
        // Show Needs in Windows writes `[presence] rim` — durable config, the
        // Serious Mode class; the id keeps the rim's.
        M::ToggleNeedsInWindows => spec("presence.rim.toggle", S::Process, A::ConfigMutate, C::Any),
        // The manual reset of the focused view's terminal to its host defaults: runtime engine
        // state only (`LocalUi`, the class of the font zoom), and it needs a terminal leaf.
        M::ResetTerminal => spec("terminal.reset", S::View, A::LocalUi, C::Terminal),
        // A tab's own theme: runtime state of the focused view's session, the
        // `theme session` verbs' class; it needs a terminal leaf.
        M::ChooseTabTheme => spec("palette.choose_tab_theme", S::View, A::LocalUi, C::Terminal),
        M::ClearTabTheme => spec("terminal.theme.clear", S::View, A::LocalUi, C::Terminal),
    }
}

/// Canonical identity/authority for native document semantic actions. Dynamic
/// outline/link/image/source-range suffixes deliberately collapse onto one
/// stable command identity; indices remain reducer parameters, never command
/// names that can acquire different authority.
pub(crate) fn native_document_action(action: &str) -> Option<CommandSpec> {
    use ActionAuthority as A;
    use CommandScope as S;
    use ContentRequirement as C;

    let result = match action {
        "markdown/back" => spec("markdown.history.back", S::View, A::LocalUi, C::Native),
        "markdown/forward" => spec("markdown.history.forward", S::View, A::LocalUi, C::Native),
        "markdown/previous-section" => {
            spec("markdown.section.previous", S::View, A::LocalUi, C::Native)
        }
        "markdown/next-section" => spec("markdown.section.next", S::View, A::LocalUi, C::Native),
        "markdown/select-all" => spec("markdown.selection.all", S::View, A::LocalUi, C::Native),
        "markdown/clear-selection" => {
            spec("markdown.selection.clear", S::View, A::LocalUi, C::Native)
        }
        "markdown/copy" => spec("markdown.selection.copy", S::View, A::Clipboard, C::Native),
        "markdown/mode/preview" => spec("markdown.mode.preview", S::View, A::LocalUi, C::Native),
        "markdown/mode/source" => spec("markdown.mode.source", S::View, A::LocalUi, C::Native),
        "markdown/mode/split" => spec("markdown.mode.split", S::View, A::LocalUi, C::Native),
        "markdown/edit" => spec("document.edit", S::Document, A::DocumentWrite, C::Native),
        "editor/save" => spec("document.save", S::Document, A::DocumentWrite, C::Native),
        "editor/undo" => spec("document.undo", S::Document, A::DocumentWrite, C::Native),
        "editor/redo" => spec("document.redo", S::Document, A::DocumentWrite, C::Native),
        "editor/find" => spec("document.find", S::Document, A::LocalUi, C::Native),
        "editor/goto-line" => spec("document.goto_line", S::Document, A::LocalUi, C::Native),
        "editor/commands" => spec("document.commands", S::Document, A::LocalUi, C::Native),
        "editor/revert" => spec("document.revert", S::Document, A::DocumentWrite, C::Native),
        "editor/config-problem-next" | "editor/config-problem-previous" => {
            spec("config.problem.navigate", S::View, A::LocalUi, C::Native)
        }
        _ if action
            .strip_prefix("editor/completion/")
            .and_then(|index| index.parse::<usize>().ok())
            .is_some_and(|index| index < 8) =>
        {
            spec("document.command.choose", S::View, A::LocalUi, C::Native)
        }
        _ if crate::native_config_language::is_config_completion_action(action) => spec(
            "config.completion.choose",
            S::Document,
            A::DocumentWrite,
            C::Native,
        ),
        _ if action
            .strip_prefix("editor/config-page/")
            .and_then(|suffix| suffix.split_once('/'))
            .and_then(|(target, candidates)| {
                Some((
                    target.parse::<usize>().ok()?,
                    candidates.parse::<usize>().ok()?,
                ))
            })
            .is_some_and(|(target, candidates)| {
                candidates > 0 && candidates <= 8 && target < candidates
            }) =>
        {
            spec("config.completion.page", S::View, A::LocalUi, C::Native)
        }
        _ if action.starts_with("markdown/outline/") => {
            spec("markdown.section.goto", S::View, A::LocalUi, C::Native)
        }
        _ if action.starts_with("markdown/page/") => {
            spec("markdown.page.goto", S::View, A::LocalUi, C::Native)
        }
        _ if action.starts_with("markdown/select-block/")
            || action.starts_with("markdown/select-range/") =>
        {
            spec("markdown.selection.range", S::View, A::LocalUi, C::Native)
        }
        _ if action.starts_with("markdown/link/") || action.starts_with("markdown/image/") => spec(
            "markdown.resource.open",
            S::View,
            A::ExternalOpen,
            C::Native,
        ),
        _ => return None,
    };
    Some(result)
}

pub(crate) fn editor_command(command: &crate::native_editor::EditorCommand) -> CommandSpec {
    use crate::native_editor::EditorCommand as E;
    use ActionAuthority as A;
    use CommandScope as S;
    use ContentRequirement as C;

    let authority = match command {
        E::Save | E::RevertBuffer => A::DocumentWrite,
        E::DeleteBackward
        | E::DeleteForward
        | E::IndentForTab
        | E::KillRegion
        | E::KillLine
        | E::Yank
        | E::YankPop
        | E::Undo
        | E::Redo
        | E::StartMacro
        | E::EndMacro
        | E::PlayMacro => A::DocumentWrite,
        E::MoveBackward
        | E::MoveForward
        | E::MoveLineUp
        | E::MoveLineDown
        | E::ScrollPageDown
        | E::ScrollPageUp
        | E::MoveLineStart
        | E::MoveLineEnd
        | E::MoveWordBackward
        | E::MoveWordForward
        | E::SetMark
        | E::Abort
        | E::UniversalArgument
        | E::ExecuteCommand
        | E::IncrementalSearch
        | E::GotoLine
        | E::SwitchBuffer => A::LocalUi,
    };
    spec(command.name(), S::Document, authority, C::Native)
}

#[cfg(test)]
mod tests {
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
        // The Overview's id: stable, Owner (it lists every session).
        let map = menu_command(menu::MenuAction::Overview);
        assert_eq!(map.id.as_str(), "view.overview");
        assert_eq!(map.authority, ActionAuthority::Owner);
        // The person's pair and the Agents rows that see the whole Mac or
        // fleet carry the Owner ceiling; Next That Needs You is a local act.
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
    fn editor_mutations_and_navigation_have_distinct_authority() {
        assert_eq!(
            editor_command(&crate::native_editor::EditorCommand::GotoLine).authority,
            ActionAuthority::LocalUi
        );
        assert_eq!(
            editor_command(&crate::native_editor::EditorCommand::RevertBuffer).authority,
            ActionAuthority::DocumentWrite
        );
    }
}
