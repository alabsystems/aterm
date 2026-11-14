// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical command identity and authority classification.

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
        M::Preferences => spec("config.open_source", S::Process, A::ConfigMutate, C::Any),
        M::Quit => spec("app.quit", S::Process, A::LocalUi, C::Any),
        M::NewWindow => spec("window.new", S::Process, A::LocalUi, C::Any),
        M::NewTab => spec("tab.new_terminal", S::Window, A::LocalUi, C::Any),
        M::OpenEditor => spec("document.open_file", S::Window, A::DocumentRead, C::Any),
        M::Save => spec("document.save", S::Document, A::DocumentWrite, C::Native),
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
        M::Overview => spec("view.overview", S::App, A::Owner, C::Any),
        M::Workers => spec("view.workers", S::App, A::Owner, C::Any),
        M::Allocation => spec("app.allocation.open", S::Window, A::LocalUi, C::Any),
        M::CloseTab => spec("view.close_focused", S::View, A::LocalUi, C::Any),
        M::Copy => spec("selection.copy", S::View, A::Clipboard, C::Any),
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
        M::FavouriteKitty => spec("effects.kitty.favourite", S::Process, A::LocalUi, C::Any),
        M::NextKitty => spec("effects.kitty.next", S::Process, A::LocalUi, C::Any),
        M::ToggleSeriousMode => spec(
            "effects.serious.toggle",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        M::ToggleSettings => spec("app.settings.open", S::App, A::ConfigMutate, C::Any),
        M::Packages => spec("app.settings.packages", S::App, A::ConfigMutate, C::Any),
        M::Messages => spec("app.settings.messages", S::App, A::ConfigMutate, C::Any),
        M::SystemState => spec("app.system_state.open", S::App, A::ConfigMutate, C::Any),
        M::Mail => spec("app.mail.open", S::App, A::ConfigMutate, C::Any),
        M::HarnessSettings => spec("app.settings.harness", S::App, A::ConfigMutate, C::Any),
        M::OpenPalette => spec("palette.open", S::Window, A::Owner, C::Any),
        M::ChooseTheme => spec("palette.choose_theme", S::Window, A::ConfigMutate, C::Any),
        M::BrowseThemes => spec("app.settings.themes", S::App, A::ConfigMutate, C::Any),
        M::Minimize => spec("window.minimize", S::Window, A::LocalUi, C::Any),
        M::Zoom => spec("window.maximize", S::Window, A::LocalUi, C::Any),
        M::NextTab => spec("tab.next", S::Window, A::LocalUi, C::Any),
        M::PrevTab => spec("tab.previous", S::Window, A::LocalUi, C::Any),
        M::RenameSession => spec("session.rename", S::Tab, A::LocalUi, C::Terminal),
        M::SetRole => spec("session.set_role", S::Tab, A::LocalUi, C::Terminal),
        M::Help => spec("app.help.open", S::App, A::ExternalOpen, C::Any),
        M::NextNeed => spec("agents.next_need", S::App, A::LocalUi, C::Any),
        M::HarnessLog => spec("session.ledger.open", S::Tab, A::ExternalOpen, C::Terminal),
        M::ReadThisMac => spec("agents.read_mac", S::Process, A::Owner, C::Any),
        M::ReadFleet => spec("agents.read_fleet", S::Process, A::Owner, C::Any),
        M::SummarizeFleet => spec("agents.summarize", S::Process, A::Owner, C::Any),
        M::FleetMachines => spec("agents.fleet_machines", S::Process, A::Owner, C::Any),
        M::TakeOver => spec("session.take_over", S::Tab, A::Owner, C::Terminal),
        M::HandBack => spec("session.hand_back", S::Tab, A::Owner, C::Terminal),
        M::HandBackAll => spec("agents.hand_back_all", S::Process, A::Owner, C::Any),
        M::FabricStatus => spec("fabric.status", S::Process, A::Owner, C::Any),
        M::FabricOn => spec("fabric.on", S::Process, A::Owner, C::Any),
        M::FabricOff => spec("fabric.off", S::Process, A::Owner, C::Any),
        M::StatsBarsDeck => spec("sys.stats_bars.deck", S::Process, A::ConfigMutate, C::Any),
        M::StatsBarsSill => spec("sys.stats_bars.sill", S::Process, A::ConfigMutate, C::Any),
        M::StatsBarsOff => spec("sys.stats_bars.off", S::Process, A::ConfigMutate, C::Any),
        M::DeckSizeAuto => spec(
            "sys.stats_deck_size.auto",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        M::DeckSizeFull => spec(
            "sys.stats_deck_size.full",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        M::DeckSizeFolded => spec(
            "sys.stats_deck_size.folded",
            S::Process,
            A::ConfigMutate,
            C::Any,
        ),
        M::ToggleStatsLens => spec("sys.stats_lens.toggle", S::Window, A::LocalUi, C::Any),
        M::NetworkState => spec("sys.network_state.open", S::Window, A::LocalUi, C::Any),
        M::ResetTerminal => spec("terminal.reset", S::View, A::LocalUi, C::Terminal),
        M::RewindPane => spec("pane.rewind", S::View, A::LocalUi, C::Terminal),
        M::ExportPaneHistory => spec("pane.export_history", S::View, A::LocalUi, C::Terminal),
        M::ForgetPaneHistory => spec("pane.forget_history", S::View, A::LocalUi, C::Terminal),
        M::ForgetAllHistory => spec("history.forget_all", S::Process, A::Owner, C::Any),
        M::RecordUnseenKeys => spec("aos.record_unseen", S::View, A::LocalUi, C::Terminal),
        M::ChooseTabTheme => spec("palette.choose_tab_theme", S::View, A::LocalUi, C::Terminal),
        M::ClearTabTheme => spec("terminal.theme.clear", S::View, A::LocalUi, C::Terminal),
        M::RenameMachine => spec("machine.name.change", S::Process, A::Owner, C::Any),
    }
}

/// Canonical identity/authority for native document semantic actions.
pub(crate) fn native_document_action(action: &str) -> Option<CommandSpec> {
    use ActionAuthority as A;
    use CommandScope as S;
    use ContentRequirement as C;

    let result = match action {
        "editor/markdown/read" => spec("document.mode.read", S::View, A::LocalUi, C::Native),
        "editor/markdown/edit" => spec("document.mode.edit", S::View, A::LocalUi, C::Native),
        "editor/markdown/split" => spec("document.mode.split", S::View, A::LocalUi, C::Native),
        "editor/markdown/toggle" => spec("document.mode.toggle", S::View, A::LocalUi, C::Native),
        "editor/markdown/focus-editor" => {
            spec("document.focus.editor", S::View, A::LocalUi, C::Native)
        }
        "editor/markdown/focus-reader" => {
            spec("document.focus.reader", S::View, A::LocalUi, C::Native)
        }
        "editor/markdown/resize" => spec("document.split.resize", S::View, A::LocalUi, C::Native),
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
        "editor/grid-toggle" => spec("document.grid_toggle", S::View, A::LocalUi, C::Native),
        "editor/grid-insert-row" => spec(
            "document.grid_insert_row",
            S::Document,
            A::DocumentWrite,
            C::Native,
        ),
        "editor/grid-delete-row" => spec(
            "document.grid_delete_row",
            S::Document,
            A::DocumentWrite,
            C::Native,
        ),
        "editor/keep-local" => spec(
            "document.keep_local",
            S::Document,
            A::DocumentWrite,
            C::Native,
        ),
        "editor/finish" => spec(
            "document.finish_edit",
            S::Document,
            A::DocumentWrite,
            C::Native,
        ),
        "editor/abort" => spec(
            "document.abort_edit",
            S::Document,
            A::DocumentWrite,
            C::Native,
        ),
        "editor/config-problem-next" | "editor/config-problem-previous" => {
            spec("config.problem.navigate", S::View, A::LocalUi, C::Native)
        }
        _ if action
            .strip_prefix("editor/completion/")
            .filter(|index| index.bytes().all(|b| b.is_ascii_digit()))
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
            .filter(|(t, c)| {
                t.bytes().all(|b| b.is_ascii_digit()) && c.bytes().all(|b| b.is_ascii_digit())
            })
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

/// The registry's row for an editor command
/// ([`EditorCommand::name`](crate::native_editor::EditorCommand::name),
/// [`EditorCommand::authority`](crate::native_editor::EditorCommand::authority)).
pub(crate) const fn editor_command(command: crate::native_editor::EditorCommand) -> CommandSpec {
    spec(
        command.name(),
        CommandScope::Document,
        editor_action_authority(command.authority()),
        ContentRequirement::Native,
    )
}

/// The host ceiling an editor authority class asks for: moving and reading is local UI.
pub(crate) const fn editor_action_authority(
    authority: crate::native_editor::CommandAuthority,
) -> ActionAuthority {
    use crate::native_editor::CommandAuthority as E;
    match authority {
        E::Read => ActionAuthority::LocalUi,
        E::Edit | E::Save => ActionAuthority::DocumentWrite,
        E::Clipboard => ActionAuthority::Clipboard,
        E::Files | E::Spawn => ActionAuthority::Owner,
    }
}

/// Whether a control client's gesture reaches `authority`.
const fn controller_reaches(authority: ActionAuthority) -> bool {
    matches!(
        authority,
        ActionAuthority::Observe
            | ActionAuthority::LocalUi
            | ActionAuthority::DocumentRead
            | ActionAuthority::DocumentWrite
    )
}

/// The editor authority classes a gesture carries, read off the host ceiling each class asks for;
/// a control client's reaches the clipboard class only where the clipboard is a lab file, nobody's
/// ([`crate::control::reaches_persons_clipboard`]).
pub(crate) fn editor_grant(controller: bool) -> Vec<crate::native_editor::CommandAuthority> {
    crate::native_editor::CommandAuthority::ALL
        .into_iter()
        .filter(|class| {
            !controller
                || controller_reaches(editor_action_authority(*class))
                || editor_action_authority(*class) == ActionAuthority::Clipboard
                    && !crate::control::reaches_persons_clipboard()
        })
        .collect()
}

#[cfg(test)]
#[path = "command_registry_tests.rs"]
mod tests;
