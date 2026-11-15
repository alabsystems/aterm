// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Settings ▸ This Mac (`docs/DESIGN-machine-names-2026-11-04.md` §5.6): this machine's NAME and
//! nickname, how the NAME was claimed, its node, its fleet membership and its earlier numbers,
//! over a Number and a Nickname field that Rename This Mac…'s judges and live line judge
//! ([`crate::machine_rename_sheet`]), then LAST CHANGE, which says what Undo Last Rename gives
//! back or why it cannot, and HISTORY, `aterm name history`'s lines. The page reads for itself on
//! the window's name writer ([`MachineRequest::Read`]), and the host resolves each write against
//! the read the page showed.
//! Every `name/press/` action is a person's own press (`app_control`'s `PERSON_ONLY_PREFIXES`),
//! refused as it is dispatched to any press no person stands behind; field text is not. On
//! Linux the page names `aterm name` instead, and on other platforms says they keep no machine
//! name yet; off macOS it offers no field and no press.

use super::harness_page::{metadata_cards, row_cards};
use super::lab_page::button;
use super::*;
use crate::native_app::MachineRequest;
use aterm_types::machine_name::OS_NOUN;

/// Every action of this page.
pub(crate) const NAME_ACTION_PREFIX: &str = "name/";
/// The page's writes: only a person's own click or key presses them.
pub(crate) const NAME_PRESS_PREFIX: &str = "name/press/";
pub(super) const NAME_FIELD_NUMBER: &str = "name/field/number";
pub(super) const NAME_FIELD_NICK: &str = "name/field/nick";
#[cfg(target_os = "macos")]
pub(super) const NAME_SAVE: &str = "name/press/save";
#[cfg(target_os = "macos")]
pub(super) const NAME_SYNC: &str = "name/press/sync";
#[cfg(target_os = "macos")]
pub(super) const NAME_AUTHORIZE: &str = "name/press/authorize";
#[cfg(target_os = "macos")]
pub(super) const NAME_RANDOM: &str = "name/press/random";
/// Undo Last Rename: its button's key and its write, as New Random Name's are one. It is this
/// page's undo of a rename, never Settings' own Undo Last Change (`settings/undo`, Cmd-Z).
#[cfg(target_os = "macos")]
pub(super) const NAME_UNDO: &str = "name/press/undo";
pub(super) const NAME_REVERT: &str = "name/revert";
pub(super) const NAME_REFRESH: &str = "name/refresh";
/// The primary button's key: its words and action follow the read (Save, Retry Sync or
/// Authorize & Sync), so the keyboard stays on it as they change.
pub(super) const NAME_PRIMARY: &str = "name/primary";
/// Authorize Naming…, beside Retry Sync where the naming rule could not be read.
#[cfg(target_os = "macos")]
pub(super) const NAME_AUTHORIZE_BESIDE: &str = "name/authorize";

/// What Settings ▸ This Mac shows, as the host last published it. What only the editor reads
/// (a write out, windows) is macOS's, the one platform with an editor; the read carries its own
/// sequence, which each write names (`MachineRequest::Save`'s `rev`).
#[derive(Clone, Debug, Default)]
pub(crate) struct MachinePageState {
    pub(crate) read: PageRead,
    /// Why the page's last write took no effect, or what of it is pending, kept until its next.
    pub(crate) notice: Option<String>,
    /// A write the page asked for has not been read back yet.
    #[cfg(target_os = "macos")]
    pub(crate) busy: bool,
    /// Whether this instance can open Authorize Naming's tab: it has windows.
    #[cfg(target_os = "macos")]
    pub(crate) windowed: bool,
}

/// The page's read.
#[derive(Clone, Debug, Default)]
pub(crate) enum PageRead {
    /// No read has landed yet.
    #[default]
    Reading,
    /// This platform's page edits nothing: what does.
    #[cfg_attr(
        all(target_os = "macos", not(test)),
        expect(
            dead_code,
            reason = "macOS edits its name here; only other platforms say what does"
        )
    )]
    Unsupported(&'static str),
    /// The read failed and no name was ever shown here: why.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(
            dead_code,
            reason = "only macOS's name writer reads; elsewhere the page says what changes the name"
        )
    )]
    Unread(String),
    /// What the read found, and what the fields are judged against.
    #[cfg(target_os = "macos")]
    Read(std::sync::Arc<crate::machine_title::page::PageSnapshot>),
}

impl PageRead {
    /// What an untouched Number and Nickname field hold: the read's number (empty for a number
    /// that holds no claim yet) and nickname; `None` before a read with a name lands.
    fn held_texts(&self) -> Option<(&str, &str)> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Read(read) => Some((read.held.number_text(), read.held.word.as_str())),
            Self::Reading | Self::Unsupported(_) | Self::Unread(_) => None,
        }
    }
}

/// Which of the page's two fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MachineText {
    Number,
    Nick,
}

impl MachineText {
    pub(super) fn for_key(key: &str) -> Option<Self> {
        match key {
            NAME_FIELD_NUMBER => Some(Self::Number),
            NAME_FIELD_NICK => Some(Self::Nick),
            _ => None,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Number => NAME_FIELD_NUMBER,
            Self::Nick => NAME_FIELD_NICK,
        }
    }
}

/// One field the person put the keyboard in: its text, and the text the read put there, which
/// it follows while its text is that ([`crate::machine_rename_sheet::Field`]'s rule).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MachineField {
    pub(super) input: crate::native_text_input::TextInputState,
    pub(super) opened: String,
}

impl MachineField {
    fn new(held: &str) -> Self {
        Self {
            input: crate::native_text_input::TextInputState::new(held.to_owned()),
            opened: held.to_owned(),
        }
    }

    /// Follow a read that found `fresh`: an untouched field takes it, and a typed one keeps its
    /// text but is judged against `fresh` from now on.
    fn follow(&mut self, fresh: &str) {
        let untouched = self.opened == self.input.value();
        fresh.clone_into(&mut self.opened);
        if untouched && self.input.value() != fresh {
            self.input = crate::native_text_input::TextInputState::new(fresh.to_owned());
        }
    }
}

/// What the Number and Nickname fields hold: `None` for one the keyboard never reached, which
/// shows the read's own text and follows each read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MachineInputs {
    pub(super) number: Option<MachineField>,
    pub(super) nick: Option<MachineField>,
}

impl MachineInputs {
    const fn slot(&self, target: MachineText) -> Option<&MachineField> {
        match target {
            MachineText::Number => self.number.as_ref(),
            MachineText::Nick => self.nick.as_ref(),
        }
    }

    const fn slot_mut(&mut self, target: MachineText) -> &mut Option<MachineField> {
        match target {
            MachineText::Number => &mut self.number,
            MachineText::Nick => &mut self.nick,
        }
    }

    pub(super) fn input(
        &self,
        target: MachineText,
    ) -> Option<&crate::native_text_input::TextInputState> {
        self.slot(target).map(|field| &field.input)
    }

    pub(super) fn input_mut(
        &mut self,
        target: MachineText,
    ) -> Option<&mut crate::native_text_input::TextInputState> {
        self.slot_mut(target).as_mut().map(|field| &mut field.input)
    }

    /// `target`'s input, made from what the read put in it where the keyboard had not reached
    /// it yet; `None` before a read with a name lands, when the page draws no field.
    fn seeded(
        &mut self,
        machine: &MachinePageState,
        target: MachineText,
    ) -> Option<&mut crate::native_text_input::TextInputState> {
        let (number, nick) = machine.read.held_texts()?;
        let held = match target {
            MachineText::Number => number,
            MachineText::Nick => nick,
        };
        Some(
            &mut self
                .slot_mut(target)
                .get_or_insert_with(|| MachineField::new(held))
                .input,
        )
    }

    /// Follow the read `machine` shows ([`MachineField::follow`]).
    pub(super) fn follow(&mut self, machine: &MachinePageState) {
        let Some((number, nick)) = machine.read.held_texts() else {
            return;
        };
        if let Some(field) = &mut self.number {
            field.follow(number);
        }
        if let Some(field) = &mut self.nick {
            field.follow(nick);
        }
    }
}

impl SettingsViewState {
    /// The page field the keyboard is in, if any.
    pub(super) fn machine_text_focus(&self) -> Option<MachineText> {
        if self.route != SettingsRoute::Machine {
            return None;
        }
        MachineText::for_key(self.common.last_focus.as_ref()?.as_str())
    }
}

/// A view that came to this page from route `from`, by whatever way it came (the rail, its
/// arrow keys, a search hit, a palette command, `open app settings /machine`), asks the host
/// for one read; a view that was on the page already, or went elsewhere, asks for none here.
/// The route's own action asks too, so a view opened already on the route
/// (`App::open_settings_tab_in_window` navigates a new view before it dispatches the action)
/// and a press of the route the view is on read as well; `UpdateCx::machine` keeps one read
/// per update. A restored tab is navigated without an update, so its host reads for it
/// instead (`App::restore_native_leaf`).
pub(super) fn arrived(from: SettingsRoute, view: &SettingsViewState, cx: &mut UpdateCx<'_>) {
    if from != SettingsRoute::Machine && view.route == SettingsRoute::Machine {
        cx.machine(MachineRequest::Read);
    }
}

/// The page's commands: Save Name, New Random Name and Undo Last Rename where the read lets
/// them, and Refresh.
pub(super) fn machine_commands(
    view: &SettingsViewState,
    machine: &MachinePageState,
) -> Vec<Command> {
    let mut out = Vec::new();
    #[cfg(target_os = "macos")]
    if let PageRead::Read(read) = &machine.read {
        let judged = edit::Judged::of(machine, read, &view.machine);
        out.push(Command {
            id: ActionId::new(NAME_SAVE),
            title: format!("{THIS_MACHINE}: Save Name"),
            shortcut: None,
            enabled: judged.primary.action == NAME_SAVE && judged.primary.enabled,
        });
        out.push(Command {
            id: ActionId::new(NAME_RANDOM),
            title: format!("{THIS_MACHINE}: New Random Name"),
            shortcut: None,
            enabled: !machine.busy,
        });
        // No Settings undo: `settings/undo` ("Undo Last Change", Cmd-Z) takes back a settings
        // edit and this takes back a rename, so it shares neither those words nor a key.
        out.push(Command {
            id: ActionId::new(NAME_UNDO),
            title: format!("{THIS_MACHINE}: Undo Last Rename"),
            shortcut: None,
            enabled: edit::undoes(machine, read),
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = view;
    out.push(Command {
        id: ActionId::new(NAME_REFRESH),
        title: format!("{THIS_MACHINE}: Refresh"),
        shortcut: None,
        enabled: !matches!(machine.read, PageRead::Unsupported(_)),
    });
    out
}

impl SettingsApp {
    pub(super) fn reduce_machine_action(
        &mut self,
        view: &mut SettingsViewState,
        action: &str,
        value: Option<&SemanticInput>,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if action == NAME_REFRESH {
            if !matches!(self.machine.read, PageRead::Unsupported(_)) {
                cx.machine(MachineRequest::Read);
            }
        } else if action == NAME_REVERT {
            view.machine = MachineInputs::default();
        } else if let Some(target) = MachineText::for_key(action) {
            // A press puts the keyboard in the field; a value (an accessibility client's set,
            // `act`'s words) replaces its text.
            if let Some(input) = view.machine.seeded(&self.machine, target)
                && let Some(SemanticInput::Text(text)) = value
            {
                *input = crate::native_text_input::TextInputState::new(text.clone());
            }
            view.common.last_focus = Some(UiKey::new(target.key()));
        } else if action.starts_with(NAME_PRESS_PREFIX) {
            #[cfg(target_os = "macos")]
            if let Some(request) = edit::press(&self.machine, &view.machine, action) {
                cx.machine(request);
            }
        } else {
            return EventResult::Bubble;
        }
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        cx.repaint(crate::native_app::DamageRegion::All);
        EventResult::Handled
    }

    /// Text editing in the Number or Nickname field. Return moves the keyboard to the primary
    /// button, whose own press (a person's, the dispatch fence judges) saves: the reducer cannot
    /// tell whose key a text event was. Escape gives the field back to the read.
    pub(super) fn reduce_machine_text(
        &mut self,
        view: &mut SettingsViewState,
        target: MachineText,
        event: TextInputEvent,
        cx: &mut UpdateCx<'_>,
    ) {
        match event {
            TextInputEvent::Submit => {
                view.common.last_focus = Some(UiKey::new(NAME_PRIMARY));
                view.common.focus_visible = true;
            }
            TextInputEvent::Cancel => *view.machine.slot_mut(target) = None,
            event => {
                let Some(input) = view.machine.seeded(&self.machine, target) else {
                    return;
                };
                apply_text_input_event(input, event);
            }
        }
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        cx.repaint(crate::native_app::DamageRegion::All);
    }

    /// Text an assistive technology inserts into a page field.
    #[cfg(any(a11y_tree, test))]
    pub(super) fn insert_machine_text(
        &self,
        view: &mut SettingsViewState,
        target: MachineText,
        text: &str,
    ) {
        if let Some(input) = view.machine.seeded(&self.machine, target) {
            input.insert(text);
        }
    }

    /// A pointer position in a page field: whether `action` is one.
    pub(super) fn reduce_machine_pointer(
        &self,
        view: &mut SettingsViewState,
        action: &str,
        byte: usize,
        extend: bool,
        cx: &mut UpdateCx<'_>,
    ) -> bool {
        let Some(target) = MachineText::for_key(action) else {
            return false;
        };
        if let Some(input) = view.machine.seeded(&self.machine, target) {
            apply_pointer_selection(input, byte, extend);
            view.common.last_focus = Some(UiKey::new(target.key()));
            view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
            cx.repaint(crate::native_app::DamageRegion::All);
        }
        true
    }
}

/// The height of one wrapped line of the page's own words.
fn words_line_height() -> f32 {
    20.0_f32.max(18.0 * settings_text_scale())
}

/// `text` wrapped whole to `width`, in `style`.
fn words(key: &str, text: &str, style: StyleRef, width: f32) -> (UiNode, f32) {
    wrapped_words_node(
        key,
        text,
        (SemanticRole::Status, style),
        width,
        words_line_height(),
    )
}

pub(super) fn machine_page(
    state: &SettingsViewState,
    machine: &MachinePageState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let gap = settings_page_gap(state, width, cx.viewport);
    let page_width =
        settings_page_content_width(cx.viewport.width, width, PageLayout::Form.maximum(width));
    let subtitle = format!("Number, nickname, and how this {OS_NOUN} holds them.");
    let (mut heading, heading_height) =
        pinned_heading(state, width, cx.viewport, gap, THIS_MACHINE, &subtitle);
    let room = page_list_room(state, width, cx.viewport, heading_height);
    let inner = card_text_width(page_width);
    let mut cards = Vec::new();
    if let Some(notice) = &machine.notice {
        cards.extend(row_cards(
            "name/last",
            ("Last attempt", ("LAST ATTEMPT", "LAST")),
            vec![words("name/notice", notice, StyleRef::Danger, inner)],
            room,
            page_width,
        ));
    }
    let status = |why: String| vec![("Status".to_owned(), why)];
    let refresh = |enabled: bool| {
        button(
            NAME_REFRESH,
            "Refresh",
            ActionId::new(NAME_REFRESH),
            inner,
            enabled,
        )
    };
    let facts = |rows: &[(String, String)]| {
        metadata_cards(
            "name/facts",
            ("NAME", "NAME"),
            rows,
            page_width,
            fact_label_width(rows, inner),
        )
    };
    match &machine.read {
        PageRead::Reading => {
            cards.extend(facts(&status(format!(
                "Reading this {OS_NOUN}'s name\u{2026}"
            ))));
            cards.extend(row_cards(
                "name/actions",
                ("Name actions", ("ACTIONS", "ACTIONS")),
                vec![refresh(true)],
                room,
                page_width,
            ));
        }
        PageRead::Unsupported(why) => cards.extend(facts(&status((*why).to_owned()))),
        PageRead::Unread(why) => {
            cards.extend(facts(&status(format!(
                "This {OS_NOUN}'s name could not be read: {why}"
            ))));
            cards.extend(row_cards(
                "name/actions",
                ("Name actions", ("ACTIONS", "ACTIONS")),
                vec![refresh(true)],
                room,
                page_width,
            ));
        }
        #[cfg(target_os = "macos")]
        PageRead::Read(read) => {
            cards.extend(facts(&read.facts));
            cards.extend(edit::cards(state, machine, read, (page_width, inner), room));
        }
    }
    heading.push(scroll::list_page_peeking(
        state,
        "settings/list",
        THIS_MACHINE,
        cards
            .into_iter()
            .map(|(node, height)| scroll::ListItem::of(node, height))
            .collect(),
        room,
        gap,
        settings_list_overhang(state, width, cx.viewport),
    ));
    heading
}

/// The editor over a read: the fields, their live line and the page's buttons, each judged as
/// Rename This Mac… judges its own ([`crate::machine_rename_sheet::request`] and
/// [`crate::machine_rename_sheet::verdict`], its live line without the OS names this page does
/// not read yet).
#[cfg(target_os = "macos")]
pub(super) mod edit {
    use super::super::lab_page::styled_button;
    use super::*;
    use crate::machine_rename_sheet::{Field, Line, RenameTo, request, sentence, verdict};
    use crate::machine_title::page::PageSnapshot;
    use aterm_agent::machine_name::file::NO_HISTORY;
    use aterm_types::machine_name::{Nickname, Number, Refusal};

    /// The primary's words where Authorize & Sync is what Save would ask, in an instance that
    /// can open no tab for it.
    pub(crate) const AUTHORIZE_NEEDS_A_WINDOW: &str = "Authorize Naming needs a window";

    /// The page's primary button: its words, its action, and whether it is available.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Primary {
        pub(crate) label: &'static str,
        pub(crate) action: &'static str,
        pub(crate) enabled: bool,
    }

    /// What the fields ask of the read, judged once per draw and press.
    pub(crate) struct Judged {
        pub(crate) number: Field,
        pub(crate) nick: Field,
        pub(crate) line: Line,
        pub(crate) primary: Primary,
        /// Authorize Naming…: `None` where it is not shown, else whether it is available.
        pub(crate) beside: Option<bool>,
    }

    fn field(input: Option<&MachineField>, held: &str) -> Field {
        match input {
            None => Field::open(None, held),
            Some(field) => Field::at(field.input.value().to_owned(), Some(field.opened.clone())),
        }
    }

    impl Judged {
        pub(crate) fn of(
            machine: &MachinePageState,
            read: &PageSnapshot,
            inputs: &MachineInputs,
        ) -> Self {
            let number = field(inputs.number.as_ref(), read.held.number_text());
            let nick = field(inputs.nick.as_ref(), &read.held.word);
            let line = verdict(&read.snapshot, &read.held, &number, &nick);
            let asked = request(&read.snapshot, &read.held, &number, &nick);
            let (label, action) = match &asked {
                Ok(RenameTo::Authorize) if machine.windowed => ("Authorize & Sync", NAME_AUTHORIZE),
                Ok(RenameTo::Authorize) => (AUTHORIZE_NEEDS_A_WINDOW, NAME_AUTHORIZE),
                Ok(RenameTo::Synchronize(_)) => ("Retry Sync", NAME_SYNC),
                Ok(RenameTo::Keep | RenameTo::Nick(_) | RenameTo::Name(_)) | Err(_) => {
                    ("Save", NAME_SAVE)
                }
            };
            let enabled = !machine.busy
                && line.renames
                && asked.as_ref().is_ok_and(|to| *to != RenameTo::Keep)
                && (action != NAME_AUTHORIZE || machine.windowed);
            let beside = read.snapshot.authorization_unknown(&read.held).then(|| {
                machine.windowed
                    && !machine.busy
                    && read
                        .snapshot
                        .authorize_saved(&read.held, &number, &nick)
                        .is_some()
            });
            Self {
                number,
                nick,
                line,
                primary: Primary {
                    label,
                    action,
                    enabled,
                },
                beside,
            }
        }

        /// Whether either field holds words the person typed.
        fn typed(&self) -> bool {
            self.number.typed().is_some() || self.nick.typed().is_some()
        }
    }

    /// Write `action` as a button drawn over read `rev` carries it (`name/press/save@7`). A
    /// pointer's press is dispatched with the action of the frame it was made over, and dispatch
    /// takes it only while the live tree draws the same one (`App::dispatch_native_control_action`
    /// checks the key and action pair), so a press over a frame drawn before a newer read landed
    /// is dropped, never resolved against a read the person did not see.
    pub(crate) fn drawn_over(action: &str, rev: u64) -> ActionId {
        ActionId::new(format!("{action}@{rev}"))
    }

    /// `action` without the read it was drawn over, and that read's sequence: `None` for a bare
    /// action, a palette command's, which acts on the read shown when it runs.
    pub(crate) fn split_drawn(action: &str) -> (&str, Option<u64>) {
        match action
            .rsplit_once('@')
            .map(|(write, rev)| (write, rev.parse::<u64>()))
        {
            Some((write, Ok(rev))) => (write, Some(rev)),
            _ => (action, None),
        }
    }

    /// Whether Undo Last Rename is available over `read`: the read offers an undo
    /// ([`crate::machine_title::page::undo_offer`]) and no page write is out.
    pub(crate) const fn undoes(machine: &MachinePageState, read: &PageSnapshot) -> bool {
        read.undo.seen.is_some() && !machine.busy
    }

    /// What press `action` asks of the host, where the page offers it now; `None` for a press
    /// of a button that is not there or not available. The request names the read the button
    /// was drawn over ([`drawn_over`]), else the read shown now; the host refuses one that is
    /// not the read it holds.
    pub(crate) fn press(
        machine: &MachinePageState,
        inputs: &MachineInputs,
        action: &str,
    ) -> Option<MachineRequest> {
        let PageRead::Read(read) = &machine.read else {
            return None;
        };
        let (action, drawn) = split_drawn(action);
        let rev = drawn.unwrap_or(read.rev);
        if action == NAME_RANDOM {
            return (!machine.busy).then_some(MachineRequest::Random { rev });
        }
        if action == NAME_UNDO {
            return undoes(machine, read).then_some(MachineRequest::Undo { rev });
        }
        let judged = Judged::of(machine, read, inputs);
        let primary = judged.primary.action == action && judged.primary.enabled;
        match action {
            NAME_SAVE if primary => Some(MachineRequest::Save {
                rev,
                number: judged.number.typed().map(str::to_owned),
                nick: judged.nick.typed().map(str::to_owned),
            }),
            NAME_SYNC if primary => Some(MachineRequest::Sync { rev }),
            NAME_AUTHORIZE if primary || judged.beside == Some(true) => {
                Some(MachineRequest::Authorize { rev })
            }
            _ => None,
        }
    }

    /// A field whose text is no number (or no nickname) the read could take.
    fn refused(target: MachineText, field: &Field) -> bool {
        let Some(typed) = field.typed().map(str::trim) else {
            return false;
        };
        match target {
            MachineText::Number => {
                Number::parse(typed).is_err_and(|refusal| refusal != Refusal::Empty)
            }
            MachineText::Nick => !typed.is_empty() && Nickname::parse(typed).is_err(),
        }
    }

    /// One labelled field: its caption over the field.
    fn field_row(
        state: &SettingsViewState,
        target: MachineText,
        label: &str,
        shown: &Field,
        inputs: &MachineInputs,
        inner: f32,
    ) -> (UiNode, f32) {
        let key = target.key();
        let (caption, caption_height) = wrapped_words_node(
            &format!("{key}/label"),
            label,
            (SemanticRole::Text, StyleRef::Quiet),
            inner,
            words_line_height(),
        );
        let focused = state
            .common
            .last_focus
            .as_ref()
            .is_some_and(|focus| focus.as_str() == key);
        // As a Settings row's field: the keyboard's field scrolls its caret's end into view, and
        // one it is not in shows its words whole or marks what it cannot show.
        let untouched;
        let input = match (focused, inputs.input(target)) {
            (false, _) => None,
            (true, Some(input)) => Some(input),
            (true, None) => {
                untouched = crate::native_text_input::TextInputState::new(shown.text().to_owned());
                Some(&untouched)
            }
        };
        let value = input.map_or_else(
            || shown.text().to_owned(),
            |input| input.display_projection().0,
        );
        let field_height = scaled_control_height();
        let node = UiNode::new(
            key,
            UiContent::TextField(
                Control::new(
                    TextFieldSpec {
                        label: label.to_owned(),
                        placeholder: None,
                        secret: false,
                        visual_value: input.is_none().then(|| value.clone()),
                        input: input.map(crate::native_text_input::TextInputState::projection),
                        swatch: None,
                        swatch_line: false,
                        trailing_hint: None,
                        description: None,
                        end_button: false,
                    },
                    ActionId::new(key),
                )
                .value(SemanticValue::Text(value))
                .state(ControlState {
                    focused,
                    invalid: refused(target, shown),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(field_height)),
        );
        let height = caption_height + 4.0 + field_height;
        (
            UiNode::new(
                format!("{key}/row"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(height))
                    .gap(4.0),
            )
            .children(vec![caption.paint_only(), node]),
            height,
        )
    }

    pub(crate) fn cards(
        state: &SettingsViewState,
        machine: &MachinePageState,
        read: &PageSnapshot,
        (page_width, inner): (f32, f32),
        room: f32,
    ) -> Vec<(UiNode, f32)> {
        let judged = Judged::of(machine, read, &state.machine);
        let rows = vec![
            field_row(
                state,
                MachineText::Number,
                "Number",
                &judged.number,
                &state.machine,
                inner,
            ),
            field_row(
                state,
                MachineText::Nick,
                "Nickname",
                &judged.nick,
                &state.machine,
                inner,
            ),
            words("name/line", &judged.line.text, StyleRef::Quiet, inner),
        ];
        let mut cards = row_cards(
            "name/edit",
            ("Rename", ("RENAME", "RENAME")),
            rows,
            room,
            page_width,
        );
        // The page's default only while it acts on words the person typed: drawn in the accent,
        // and Return presses it where the keyboard is in no control (a person's key, which
        // dispatch's fence judges). One that acts on nothing typed (an unchanged seeded or
        // automatic number's Save, Retry Sync) renames or publishes this Mac with no word of the
        // person's, so a Return on arrival must not press it: it is drawn as the rest are and is
        // no default, as is one that cannot act (Settings ▸ Messages' rule for a dead first
        // intent).
        let mut buttons = vec![styled_button(
            NAME_PRIMARY,
            judged.primary.label,
            drawn_over(judged.primary.action, read.rev),
            inner,
            judged.primary.enabled,
            if judged.primary.enabled && judged.typed() {
                StyleRef::Primary
            } else {
                StyleRef::Secondary
            },
        )];
        if let Some(enabled) = judged.beside {
            buttons.push(button(
                NAME_AUTHORIZE_BESIDE,
                crate::machine_rename_sheet::AUTHORIZE_NAMING,
                drawn_over(NAME_AUTHORIZE, read.rev),
                inner,
                enabled,
            ));
        }
        buttons.push(button(
            NAME_RANDOM,
            "New Random Name",
            drawn_over(NAME_RANDOM, read.rev),
            inner,
            !machine.busy,
        ));
        buttons.push(button(
            NAME_REVERT,
            "Revert",
            ActionId::new(NAME_REVERT),
            inner,
            judged.typed(),
        ));
        buttons.push(button(
            NAME_REFRESH,
            "Refresh",
            ActionId::new(NAME_REFRESH),
            inner,
            true,
        ));
        cards.extend(row_cards(
            "name/actions",
            ("Name actions", ("ACTIONS", "ACTIONS")),
            buttons,
            room,
            page_width,
        ));
        // Undo stands in its own card, apart from Revert, which only gives the fields back:
        // this writes the name.
        cards.extend(row_cards(
            "name/undo",
            ("Last change", ("LAST CHANGE", "LAST")),
            vec![
                words("name/undo/line", &read.undo.line, StyleRef::Quiet, inner),
                button(
                    NAME_UNDO,
                    "Undo Last Rename",
                    drawn_over(NAME_UNDO, read.rev),
                    inner,
                    undoes(machine, read),
                ),
            ],
            room,
            page_width,
        ));
        if let Some(history) = &read.history {
            // Keyed by place, newest first: two changes in one second share their words.
            let rows = if history.is_empty() {
                vec![words(
                    "name/history/none",
                    &sentence(NO_HISTORY),
                    StyleRef::Quiet,
                    inner,
                )]
            } else {
                history
                    .iter()
                    .enumerate()
                    .map(|(at, line)| {
                        words(
                            &format!("name/history/{at}"),
                            line,
                            StyleRef::Primary,
                            inner,
                        )
                    })
                    .collect()
            };
            cards.extend(row_cards(
                "name/history",
                ("History", ("HISTORY", "HISTORY")),
                rows,
                room,
                page_width,
            ));
        }
        cards
    }
}
