// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical command identity and authority classification.
//!
//! Menus, keybindings, the palette, native semantic actions, and control verbs
//! are adapters onto this registry.  The adapter matches are exhaustive: a new
//! shipping action cannot compile until it receives a stable command id and an
//! authority decision.

#![allow(
    dead_code,
    reason = "registry adapters replace legacy command surfaces incrementally"
)]

use crate::{keybinding, menu};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CommandId(&'static str);

impl CommandId {
    pub(crate) const fn new(id: &'static str) -> Self {
        Self(id)
    }

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

/// Maximum capability a command may request from the host. Reducer effects are
/// checked again against this ceiling, so a benign action cannot tunnel a file,
/// clipboard, process, or update operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionAuthority {
    Observe,
    LocalUi,
    Clipboard,
    ConfigMutate,
    DocumentRead,
    DocumentWrite,
    ExternalOpen,
    Spawn,
    UpdateStage,
    UpdateApply,
    Owner,
}

impl ActionAuthority {
    #[must_use]
    pub(crate) fn permits(self, required: Self) -> bool {
        self == required
            || matches!(self, Self::Owner)
            || matches!(required, Self::Observe)
            || matches!(
                (self, required),
                (Self::DocumentWrite, Self::DocumentRead) | (Self::UpdateApply, Self::UpdateStage)
            )
    }
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
        // Keep the stable command identity: the source now opens in native Manual.
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
        // Session-connection spawn presets (design §2.3): peer/place ride as
        // reducer parameters never id names (`command_registry.rs` rule — one
        // fieldless id per parameterless act). `A::Owner` because each MINTS
        // standing session-connection authority over the focused session (the
        // OwnerOnly `invoke` fence twin); terminal content because the origin
        // is the FOCUSED session.
        M::NewControlledWindow => spec(
            "session.connect_controlled_window",
            S::Process,
            A::Owner,
            C::Terminal,
        ),
        M::NewControlledTab => spec(
            "session.connect_controlled_tab",
            S::Window,
            A::Owner,
            C::Terminal,
        ),
        M::NewControllerWindow => spec(
            "session.connect_controller_window",
            S::Process,
            A::Owner,
            C::Terminal,
        ),
        M::NewControllerTab => spec(
            "session.connect_controller_tab",
            S::Window,
            A::Owner,
            C::Terminal,
        ),
        // The connection PICKER (§2.5; parameterless — the chosen peer is a
        // reducer parameter) and the instance-wide MAP (§5). Owner for the
        // same mint-standing-authority / aggregated-disclosure reasons as the
        // presets and the `flows` verb.
        M::ConnectToSession => spec("session.connect_to", S::Window, A::Owner, C::Terminal),
        M::ShowConnectionMap => spec("view.connections", S::App, A::Owner, C::Any),
        // Configure/disconnect an EXISTING connection (§2.3): the peer is a
        // reducer parameter (the picker/sheet resolve it), never an id name.
        // Owner because both rewrite/dissolve standing session-connection
        // authority; terminal content because the subject is the focused (or
        // clicked-tab) session.
        M::ConfigureConnection => spec(
            "session.configure_connection",
            S::Window,
            A::Owner,
            C::Terminal,
        ),
        M::DisconnectSession => spec("session.disconnect", S::Window, A::Owner, C::Terminal),
        // Compatibility spelling: the shipping action still closes the focused
        // pane/view. New UI uses `tab.close_tree` for whole-tree close.
        M::CloseTab => spec("view.close_focused", S::View, A::LocalUi, C::Any),
        M::Copy => spec("selection.copy", S::View, A::Clipboard, C::Any),
        // Tab-context copies (session-metadata stage 2): session identity / cwd
        // text onto the pasteboard — clipboard authority, terminal content only
        // (a native tab owns no session to copy from).
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
        // Same identity as the keybinding face (K::ToggleMatrixRain below):
        // one command, two faces, converging on the per-session toggle.
        M::ToggleMatrixRain => spec("effects.rain.toggle", S::Process, A::LocalUi, C::Any),
        // Promotes the front window's promotable kitty (its tenured program
        // cat, else the LAUNCH kitty) into the durable registry and pins it.
        // Process-scoped and context-free: the launch kitty is one cat for
        // the whole process (owner ruling, 2026-08-17), so it needs no
        // focused leaf and no terminal; `LocalUi` because the only durable
        // write is the machine-owned toy ledger, not config.
        M::FavouriteKitty => spec("effects.kitty.favourite", S::Process, A::LocalUi, C::Any),
        M::ToggleSeriousMode => spec(
            "effects.serious.toggle",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        M::ToggleSettings => spec("app.settings.open", S::App, A::ConfigMutate, C::Any),
        // Settings opened AT the Packages route — same surface class as
        // ToggleSettings (it raises the durable-config Settings tab; the page's
        // own switches do the actual [packages] writes through the OCC editor).
        M::Packages => spec("app.settings.packages", S::App, A::ConfigMutate, C::Any),
        M::OpenPalette => spec("palette.open", S::Window, A::Owner, C::Any),
        M::Minimize => spec("window.minimize", S::Window, A::LocalUi, C::Any),
        M::Zoom => spec("window.maximize", S::Window, A::LocalUi, C::Any),
        M::NextTab => spec("tab.next", S::Window, A::LocalUi, C::Any),
        M::PrevTab => spec("tab.previous", S::Window, A::LocalUi, C::Any),
        // Same identity as the keybinding face (K::RenameSession below): one
        // command, two faces, converging on the inline pin editor. Scoped to
        // the Tab because the gesture names a tab and the editor is tab chrome;
        // the WRITE it eventually performs targets that tab's FOCUSED session.
        // `LocalUi` (not `ConfigMutate`) for the same reason the control layer
        // classifies `meta set` as `WriteInput`: nothing durable on disk moves.
        M::RenameSession => spec("session.rename", S::Tab, A::LocalUi, C::Terminal),
        M::Help => spec("app.help.open", S::App, A::ExternalOpen, C::Any),
    }
}

/// Exhaustive adapter for user-configurable keybindings.
pub(crate) const fn keybinding_command(action: keybinding::Action) -> CommandSpec {
    use ActionAuthority as A;
    use CommandScope as S;
    use ContentRequirement as C;
    use keybinding::Action as K;

    match action {
        K::NewTab => spec("tab.new_terminal", S::Window, A::LocalUi, C::Any),
        K::ReopenClosedTab => spec("tab.reopen_closed", S::Window, A::LocalUi, C::Any),
        K::CloseTab => spec("view.close_focused", S::View, A::LocalUi, C::Any),
        K::NewWindow => spec("window.new", S::Process, A::LocalUi, C::Any),
        K::NextTab => spec("tab.next", S::Window, A::LocalUi, C::Any),
        K::PrevTab => spec("tab.previous", S::Window, A::LocalUi, C::Any),
        K::SwitchTab(_) => spec("tab.select_index", S::Window, A::LocalUi, C::Any),
        K::SplitVertical => spec("view.split_vertical", S::View, A::LocalUi, C::Any),
        K::SplitHorizontal => spec("view.split_horizontal", S::View, A::LocalUi, C::Any),
        K::Copy => spec("selection.copy", S::View, A::Clipboard, C::Any),
        K::Paste => spec("selection.paste", S::View, A::Clipboard, C::Any),
        K::Find => spec("view.find", S::View, A::LocalUi, C::Any),
        K::FontIncrease => spec("window.text_scale_increase", S::Window, A::LocalUi, C::Any),
        K::FontReset => spec("window.text_scale_reset", S::Window, A::LocalUi, C::Any),
        K::FontDecrease => spec("window.text_scale_decrease", S::Window, A::LocalUi, C::Any),
        K::FocusPaneLeft => spec("view.focus_left", S::Tab, A::LocalUi, C::Any),
        K::FocusPaneRight => spec("view.focus_right", S::Tab, A::LocalUi, C::Any),
        K::FocusPaneUp => spec("view.focus_up", S::Tab, A::LocalUi, C::Any),
        K::FocusPaneDown => spec("view.focus_down", S::Tab, A::LocalUi, C::Any),
        K::TogglePaneZoom => spec("view.zoom_toggle", S::Tab, A::LocalUi, C::Any),
        K::ScrollPageUp => spec("view.scroll_page_up", S::View, A::LocalUi, C::Any),
        K::ScrollPageDown => spec("view.scroll_page_down", S::View, A::LocalUi, C::Any),
        K::ScrollLineUp => spec("view.scroll_line_up", S::View, A::LocalUi, C::Any),
        K::ScrollLineDown => spec("view.scroll_line_down", S::View, A::LocalUi, C::Any),
        K::ScrollToTop => spec("view.scroll_top", S::View, A::LocalUi, C::Any),
        K::ScrollToBottom => spec("view.scroll_bottom", S::View, A::LocalUi, C::Any),
        K::JumpPrevPrompt => spec("terminal.prompt_previous", S::View, A::LocalUi, C::Terminal),
        K::JumpNextPrompt => spec("terminal.prompt_next", S::View, A::LocalUi, C::Terminal),
        K::ToggleSettings => spec("app.settings.open", S::App, A::ConfigMutate, C::Any),
        K::ToggleAbout => spec("app.settings.about", S::App, A::LocalUi, C::Any),
        K::ToggleMatrixRain => spec("effects.rain.toggle", S::Process, A::LocalUi, C::Any),
        K::ToggleSeriousMode => spec(
            "effects.serious.toggle",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        K::OpenPalette => spec("palette.open", S::Window, A::Owner, C::Any),
        K::ToggleViMode => spec("terminal.vi.toggle", S::View, A::LocalUi, C::Terminal),
        K::RenameSession => spec("session.rename", S::Tab, A::LocalUi, C::Terminal),
        // Same identities as the menu faces (M::SelectAll / M::ToggleFullScreen
        // / M::FindNext / M::FindPrev above): one command each, two faces,
        // converging on the same verbs — the join `menu_binding` (app_palette)
        // rides to label a palette row with the chord that actually fires it.
        K::SelectAll => spec("selection.select_all", S::View, A::Clipboard, C::Any),
        K::ToggleFullscreen => spec("window.fullscreen", S::Window, A::LocalUi, C::Any),
        K::FindNext => spec("view.find_next", S::View, A::LocalUi, C::Any),
        K::FindPrev => spec("view.find_previous", S::View, A::LocalUi, C::Any),
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
    fn shared_menu_and_keybinding_faces_have_one_identity() {
        assert_eq!(
            menu_command(menu::MenuAction::NewTab).id,
            keybinding_command(keybinding::Action::NewTab).id
        );
        assert_eq!(
            menu_command(menu::MenuAction::CloseTab).id.as_str(),
            "view.close_focused"
        );
        assert_eq!(
            menu_command(menu::MenuAction::SplitVertical).content,
            ContentRequirement::Any
        );
        // The session-connection presets: one fieldless id per parameterless
        // act (peer/place are parameters, never id names — the §2.3 [v5]
        // command-id rule), Owner authority (they mint standing
        // session-connection authority), terminal content (the origin is the
        // focused session).
        for (action, id) in [
            (
                menu::MenuAction::NewControlledWindow,
                "session.connect_controlled_window",
            ),
            (
                menu::MenuAction::NewControlledTab,
                "session.connect_controlled_tab",
            ),
            (
                menu::MenuAction::NewControllerWindow,
                "session.connect_controller_window",
            ),
            (
                menu::MenuAction::NewControllerTab,
                "session.connect_controller_tab",
            ),
        ] {
            let spec = menu_command(action);
            assert_eq!(spec.id.as_str(), id);
            assert_eq!(
                spec.authority,
                ActionAuthority::Owner,
                "{id} mints authority"
            );
            assert_eq!(spec.content, ContentRequirement::Terminal);
        }
        // The picker/map ids (§2.3 [v5]): stable fieldless identities, Owner
        // authority (the picker mints, the map aggregates).
        let connect_to = menu_command(menu::MenuAction::ConnectToSession);
        assert_eq!(connect_to.id.as_str(), "session.connect_to");
        assert_eq!(connect_to.authority, ActionAuthority::Owner);
        let map = menu_command(menu::MenuAction::ShowConnectionMap);
        assert_eq!(map.id.as_str(), "view.connections");
        assert_eq!(map.authority, ActionAuthority::Owner);
        // The configure/disconnect ids (§2.3): the peer stays a parameter —
        // one stable fieldless id each — and both are Owner (they rewrite or
        // dissolve standing session-connection authority).
        let configure = menu_command(menu::MenuAction::ConfigureConnection);
        assert_eq!(configure.id.as_str(), "session.configure_connection");
        assert_eq!(configure.authority, ActionAuthority::Owner);
        let disconnect = menu_command(menu::MenuAction::DisconnectSession);
        assert_eq!(disconnect.id.as_str(), "session.disconnect");
        assert_eq!(disconnect.authority, ActionAuthority::Owner);
    }

    #[test]
    fn authority_is_monotone_and_owner_is_the_ceiling() {
        assert!(ActionAuthority::Owner.permits(ActionAuthority::UpdateApply));
        assert!(ActionAuthority::DocumentWrite.permits(ActionAuthority::DocumentRead));
        assert!(!ActionAuthority::LocalUi.permits(ActionAuthority::Clipboard));
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
