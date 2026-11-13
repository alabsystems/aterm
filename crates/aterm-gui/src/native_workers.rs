// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Workers tab (`docs/DESIGN-workers-tab-2026-11-02.md`): one native page per window listing
//! the window's parked worker tabs, grouped by manager (a fleet drive's under the Mac that asked,
//! ATERM-KI-1109-3), and under Finished the workers whose tabs
//! aterm closed once their tasks concluded ([`crate::retired_workers`]). The rows are published by
//! the host ([`crate::app_workers`]); the page holds no session and reads no socket. Pressing a
//! row asks the host through [`WorkersAction`].

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, Command, CommonViewState, DamageRegion, EventResult, NativeAppModel, UpdateCx,
    ViewCx,
};
use crate::native_ui::{
    ActionId, AuditedCustomNode, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout,
    Length, SemanticRole, StyleRef, TextSpec, UiContent, UiKey, UiNode, UiTree,
};
use crate::system_state::lines::Mark;
use crate::system_state::marks::{Ground, mark_value};
use crate::task_record::{Conclusion, TaskRecord, TaskState, failed_words};

/// The page's heading, the chip's name, and the words before a shown worker's label.
pub(crate) const WORKERS_TITLE: &str = "Workers";

/// What the chip reads while worker `label` shows in place of the list.
pub(crate) fn chip_title_of(label: &str) -> String {
    format!("{WORKERS_TITLE} \u{25b8} {label}")
}

/// The group the harness's own workers sit under, and any worker no session manages.
pub(crate) const ATERM_GROUP: &str = "aterm";

/// The group of the workers whose tabs aterm closed once their tasks concluded, last on the page.
pub(crate) const FINISHED_GROUP: &str = "Finished";

/// A row's state, in the order the summary line says them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RowState {
    Working,
    NeedsYou,
    Stalled,
    Failed,
    Finished,
    /// No task record: a shell a person split beside a worker.
    Idle,
}

impl RowState {
    const ALL: [Self; 6] = [
        Self::Working,
        Self::NeedsYou,
        Self::Stalled,
        Self::Failed,
        Self::Finished,
        Self::Idle,
    ];

    /// The row's state word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsYou => "Needs you",
            Self::Stalled => "Stalled",
            Self::Failed => "Failed",
            Self::Finished => "Finished",
            Self::Idle => "Idle",
        }
    }

    const fn summary_word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsYou => "need you",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
            Self::Finished => "finished",
            Self::Idle => "idle",
        }
    }

    /// Whether the worker's task is over: its row sits below the running ones.
    pub(crate) const fn concluded(self) -> bool {
        matches!(self, Self::Failed | Self::Finished | Self::Idle)
    }

    const fn mark(self) -> Mark {
        match self {
            Self::Working => Mark::Working,
            Self::NeedsYou => Mark::NeedsYou,
            Self::Stalled => Mark::Waiting,
            Self::Failed => Mark::Failed,
            Self::Finished => Mark::Done,
            Self::Idle => Mark::Pending,
        }
    }

    const fn style(self) -> StyleRef {
        match self {
            Self::Failed => StyleRef::Danger,
            Self::Finished => StyleRef::Success,
            Self::Working | Self::NeedsYou | Self::Stalled | Self::Idle => StyleRef::Secondary,
        }
    }
}

/// THE ROW'S STATE, THE ONE PLACE A TASK STATE BECOMES A WORD ON THIS PAGE. `need` is a person's
/// need standing on the session, which only a task still running can show as its state. A task that
/// ended reads as its record's conclusion ([`TaskRecord::conclusion`]), the word `await task` and
/// `status` give.
pub(crate) fn row_state(record: Option<&TaskRecord>, need: bool) -> RowState {
    let Some(record) = record else {
        return if need {
            RowState::NeedsYou
        } else {
            RowState::Idle
        };
    };
    match record.state {
        TaskState::Working | TaskState::Stalled if need => RowState::NeedsYou,
        TaskState::Working => RowState::Working,
        TaskState::Stalled => RowState::Stalled,
        TaskState::Done | TaskState::Failed | TaskState::Exited => match record.conclusion() {
            Some(Conclusion::Finished) => RowState::Finished,
            Some(Conclusion::Failed) | None => RowState::Failed,
        },
    }
}

/// Whether `record`'s agent has left its tab: it exited, or it never started (a stall at the
/// shell, at startup, for a sign-in or an install, or a launch a shutdown stopped). The tab's title
/// is its shell's by then, a folder that names no worker. A `why=held` worker has started and may
/// yet move on.
pub(crate) fn agent_gone(record: &TaskRecord) -> bool {
    match record.state {
        TaskState::Exited => true,
        TaskState::Stalled => matches!(
            record.why,
            Some(
                "startup"
                    | "shell"
                    | crate::task_record::WHY_SIGNIN
                    | crate::task_record::WHY_INSTALL
                    | crate::task_record::WHY_CANCELLED
            )
        ),
        TaskState::Working | TaskState::Done | TaskState::Failed => false,
    }
}

/// Whether the worker left a result to read, still there after its agent exits: a landing's or a
/// failure's. A stall that may yet move on has none.
pub(crate) fn result_to_read(record: &TaskRecord) -> bool {
    record.state.has_an_end_to_read() && record.result_bytes > 0 && record.result_path.is_some()
}

/// Why a stalled or concluded task reads as it does, in a person's words.
pub(crate) fn why_words(record: &TaskRecord) -> Option<String> {
    let code = record
        .exit
        .filter(|code| *code != 0)
        .map(|code| format!("exit {code}"));
    match record.state {
        TaskState::Working => None,
        TaskState::Stalled => Some(crate::task_record::stall_words(record.why)),
        TaskState::Done => record.why.map(str::to_owned),
        TaskState::Failed => Some(failed_words(record.state, record.why)),
        // An exit after a finished task needs no reason beside the state word but a bad code.
        TaskState::Exited if record.finished => code,
        TaskState::Exited => {
            let words = failed_words(record.state, record.why);
            Some(code.map_or_else(|| words.clone(), |code| format!("{words}, {code}")))
        }
    }
}

/// Whom a worker's row sits under, by identity: what [`grouped`] keys on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowGroup {
    /// A manager on this Mac, by its sid (its task's `from=`).
    Manager(String),
    /// Another Mac's fleet drive, by the requester's node id: one group per requesting Mac.
    From(String),
    /// The harness's own (`from=ctl`), and any worker nothing manages.
    Aterm,
}

/// The heading of a fleet drive's group: `From <host>`, or the node's head where the Mac's name
/// is not known.
pub(crate) fn from_label(host: &str, node: &str) -> String {
    if host.is_empty() {
        format!("From {}", aterm_requests::id::head_of(node))
    } else {
        format!("From {host}")
    }
}

/// One worker as the page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerRow {
    /// Its session, by local id.
    pub(crate) session: u64,
    /// Its manager's label, [`from_label`], or [`ATERM_GROUP`]: the heading of its group, never
    /// its key.
    pub(crate) group: String,
    /// Whom it sits under, by identity, which [`grouped`] keys on.
    pub(crate) under: RowGroup,
    pub(crate) state: RowState,
    pub(crate) title: String,
    /// The agent's name (`claude`), when one was asked for.
    pub(crate) agent: Option<String>,
    /// How long it has run, or how long ago it concluded.
    pub(crate) elapsed: String,
    /// Its last words: redacted and capped as `survey` does.
    pub(crate) last_words: Option<String>,
    /// [`why_words`].
    pub(crate) why: Option<String>,
    pub(crate) has_result: bool,
    pub(crate) stoppable: bool,
    /// A worker whose tab aterm closed once its task concluded ([`crate::retired_workers`]): its
    /// row sits under [`FINISHED_GROUP`] and offers what it left, not its tab.
    pub(crate) finished: bool,
}

impl WorkerRow {
    /// The state as the row words it: `Failed: exit 1`.
    fn state_words(&self) -> String {
        match &self.why {
            Some(why) if self.state != RowState::Working => {
                format!("{}: {why}", self.state.word())
            }
            _ => self.state.word().to_owned(),
        }
    }

    /// The agent and the time, as the row's second column says them.
    fn meta_words(&self) -> String {
        match &self.agent {
            Some(agent) => format!("{agent} \u{00b7} {}", self.elapsed),
            None => self.elapsed.clone(),
        }
    }

    /// The whole row in words, for assistive technology (the painted lines are paint only): its
    /// press opens a running worker's tab, and reads a Finished one's result.
    fn spoken(&self) -> String {
        let press = if self.finished { "Read" } else { "Open" };
        let mut words = format!(
            "{press} {}: {}, {}",
            self.title,
            self.state_words(),
            self.meta_words()
        );
        if let Some(said) = &self.last_words {
            words.push_str(". ");
            words.push_str(said);
        }
        words
    }
}

/// `3 working, 1 failed, 2 finished`: the states that have a row, in [`RowState::ALL`]'s order.
pub(crate) fn summary_words(rows: &[WorkerRow]) -> String {
    let words: Vec<String> = RowState::ALL
        .iter()
        .filter_map(|state| {
            let count = rows.iter().filter(|row| row.state == *state).count();
            (count > 0).then(|| {
                let word = if *state == RowState::NeedsYou && count == 1 {
                    "needs you"
                } else {
                    state.summary_word()
                };
                format!("{count} {word}")
            })
        })
        .collect();
    if words.is_empty() {
        "No workers are running.".to_owned()
    } else {
        words.join(", ")
    }
}

/// The rows as the page lists them: one group per manager or requesting Mac, by its identity, in
/// order of first appearance with the harness's own last, and in each group the running rows
/// above the concluded ones, otherwise in tab order; then the Finished rows, under
/// [`FINISHED_GROUP`], in the order their tabs closed. A group's heading is its label, and where
/// two groups' labels match (two managers both titled `Claude Code`, one titled `aterm`, two Macs
/// both named `m114`) each manager's carries its short sid and each Mac's its node's head, so no
/// heading stands for two of them.
pub(crate) fn grouped(rows: &[WorkerRow]) -> Vec<(String, Vec<&WorkerRow>)> {
    let mut groups: Vec<(&RowGroup, &str, Vec<&WorkerRow>)> = Vec::new();
    for row in rows.iter().filter(|row| !row.finished) {
        match groups.iter_mut().find(|(under, ..)| **under == row.under) {
            Some((.., members)) => members.push(row),
            None => groups.push((&row.under, row.group.as_str(), vec![row])),
        }
    }
    groups.sort_by_key(|(under, ..)| matches!(under, RowGroup::Aterm));
    let shared = |label: &str| {
        groups
            .iter()
            .filter(|(_, other, _)| *other == label)
            .count()
            > 1
    };
    let short = |sid: &str| {
        let short = aterm_messages::presence::short_sid(sid);
        let clashes = groups.iter().any(|(other, ..)| {
            matches!(other, RowGroup::Manager(other) if other != sid
                && aterm_messages::presence::short_sid(other) == short)
        });
        if clashes { sid.to_owned() } else { short }
    };
    let headings: Vec<String> = groups
        .iter()
        .map(|(under, label, _)| match under {
            RowGroup::Manager(sid) if shared(label) => format!("{label} ({})", short(sid)),
            RowGroup::From(node) if shared(label) => {
                format!("{label} ({})", aterm_requests::id::head_of(node))
            }
            _ => (*label).to_owned(),
        })
        .collect();
    let mut listed: Vec<(String, Vec<&WorkerRow>)> = groups
        .into_iter()
        .zip(headings)
        .map(|((.., mut members), heading)| {
            members.sort_by_key(|row| row.state.concluded());
            (heading, members)
        })
        .collect();
    let finished: Vec<&WorkerRow> = rows.iter().filter(|row| row.finished).collect();
    if !finished.is_empty() {
        listed.push((FINISHED_GROUP.to_owned(), finished));
    }
    listed
}

/// What a press on the page asks of the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkersAction {
    /// A running worker's tab in place, or a Finished worker's result.
    Open(u64),
    Stop(u64),
    ReadResult(u64),
    /// Keep as Tab: the worker's tab becomes the person's (`App::keep_as_tab`).
    Keep(u64),
    /// A Finished row goes, and the result it kept.
    Dismiss(u64),
    /// Every Finished row of the window goes.
    ClearFinished,
}

const OPEN: &str = "workers/open/";
const STOP: &str = "workers/stop/";
const RESULT: &str = "workers/result/";
const KEEP: &str = "workers/keep/";
const DISMISS: &str = "workers/dismiss/";
const CLEAR_FINISHED: &str = "workers/clear-finished";

/// Parse one of the page's action ids.
fn action_of(id: &str) -> Option<WorkersAction> {
    if id == CLEAR_FINISHED {
        return Some(WorkersAction::ClearFinished);
    }
    let session = |prefix: &str| id.strip_prefix(prefix)?.parse::<u64>().ok();
    session(OPEN)
        .map(WorkersAction::Open)
        .or_else(|| session(STOP).map(WorkersAction::Stop))
        .or_else(|| session(RESULT).map(WorkersAction::ReadResult))
        .or_else(|| session(KEEP).map(WorkersAction::Keep))
        .or_else(|| session(DISMISS).map(WorkersAction::Dismiss))
}

/// The window's list of workers: the published rows and what the page has asked for.
#[derive(Debug, Default)]
pub(crate) struct WorkersApp {
    rows: Vec<WorkerRow>,
    actions: VecDeque<WorkersAction>,
}

impl WorkersApp {
    /// Take the rows the host read. Whether anything the page shows moved.
    pub(crate) fn publish(&mut self, rows: Vec<WorkerRow>) -> bool {
        if self.rows == rows {
            return false;
        }
        self.rows = rows;
        true
    }

    /// The published rows' sessions, in tab order.
    pub(crate) fn sessions(&self) -> Vec<u64> {
        self.rows.iter().map(|row| row.session).collect()
    }

    pub(crate) fn take_action(&mut self) -> Option<WorkersAction> {
        self.actions.pop_front()
    }

    /// Each row's session, in the order the page lists them.
    fn listed(&self) -> Vec<u64> {
        grouped(&self.rows)
            .iter()
            .flat_map(|(_, members)| members.iter().map(|row| row.session))
            .collect()
    }
}

/// One view of the list: its scroll and which row the keyboard is on.
#[derive(Debug, Default)]
pub(crate) struct WorkersViewState {
    pub(crate) common: CommonViewState,
    scroll: f32,
    scroll_limit: Cell<f32>,
    /// The list's room, as the last render laid it out.
    room: Cell<f32>,
    /// `(session, top, height)` of each row in the list, as the last render laid it out.
    row_spans: RefCell<Vec<(u64, f32, f32)>>,
}

impl WorkersViewState {
    /// The session of the row the keyboard is on.
    fn selected(&self) -> Option<u64> {
        let key = self.common.last_focus.as_ref()?;
        key.as_str().strip_prefix(OPEN)?.parse().ok()
    }

    /// The inspector's reveal of a listed control ([`crate::app_control`]): a row or one of its
    /// buttons. Whether the key names a row on this page.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        let Some(session) = [OPEN, STOP, RESULT, KEEP, DISMISS]
            .into_iter()
            .find_map(|prefix| key.as_str().strip_prefix(prefix))
            .and_then(|session| session.parse().ok())
        else {
            return false;
        };
        let before = self.scroll;
        self.reveal(session);
        self.scroll != before
    }

    fn select(&mut self, session: u64) {
        self.common.last_focus = Some(UiKey::new(format!("{OPEN}{session}")));
        self.common.focus_visible = true;
        self.reveal(session);
    }

    /// Scroll so `session`'s row shows, once the page has been laid out.
    fn reveal(&mut self, session: u64) {
        let spans = self.row_spans.borrow();
        let Some((_, top, height)) = spans.iter().find(|(row, ..)| *row == session).copied() else {
            return;
        };
        drop(spans);
        let room = self.room.get();
        if top < self.scroll {
            self.scroll = top;
        } else if top + height > self.scroll + room {
            self.scroll = top + height - room;
        }
        self.scroll = self.scroll.clamp(0.0, self.scroll_limit.get().max(0.0));
    }

    fn scroll_by(&mut self, delta: f32) {
        self.scroll = (self.scroll + delta).clamp(0.0, self.scroll_limit.get().max(0.0));
    }
}

/// One line of a wheel step.
const LINE_PX: f32 = 40.0;

impl NativeAppModel for WorkersApp {
    type ViewState = WorkersViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Workers,
            name: WORKERS_TITLE,
            icon: AppIcon::Workers,
            singleton: false,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        match event {
            AppEvent::FocusChanged(focus) => view.common.last_focus = focus,
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll_by(lines as f32 * LINE_PX);
            }
            AppEvent::ScrollPage(direction) => {
                let page = (view.room.get() - LINE_PX).max(LINE_PX);
                view.scroll_by(direction as f32 * page);
            }
            AppEvent::StepFocus(step) => {
                let listed = self.listed();
                let at = view
                    .selected()
                    .and_then(|session| listed.iter().position(|row| *row == session));
                let next = match (at, step.signum()) {
                    (None, -1) => listed.len().checked_sub(1),
                    (None, _) => (!listed.is_empty()).then_some(0),
                    (Some(at), -1) => Some(at.saturating_sub(1)),
                    (Some(at), _) => Some((at + 1).min(listed.len().saturating_sub(1))),
                };
                if let Some(session) = next.and_then(|next| listed.get(next).copied()) {
                    view.select(session);
                }
            }
            AppEvent::Action(action) => {
                let Some(asked) = action_of(action.id.as_str()) else {
                    return EventResult::Bubble;
                };
                self.actions.push_back(asked);
            }
            _ => return EventResult::Bubble,
        }
        view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
        cx.repaint(DamageRegion::All);
        EventResult::Handled
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        page(self, view, cx.viewport.width, cx.viewport.height)
    }

    fn commands(&self, _view: &Self::ViewState, _out: &mut Vec<Command>) {}

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: WORKERS_TITLE.to_owned(),
            icon: AppIcon::Workers,
            indicators: AppIndicators {
                dirty: false,
                busy: self.rows.iter().any(|row| !row.state.concluded()),
                attention: false,
            },
            closable: self.rows.iter().all(|row| row.finished),
            tooltip: Some(format!("{WORKERS_TITLE}: {}", summary_words(&self.rows))),
        }
    }

    fn prepare_close(&mut self, _close: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        // The host refuses a close of this tab while workers remain, before it asks here, and a
        // close of it with only Finished rows clears them; a window that closes takes its workers
        // with it.
        CloseReadiness::Ready
    }
}

fn plain_group(key: impl Into<std::sync::Arc<str>>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
}

fn words(key: String, text: String, role: SemanticRole, style: StyleRef, width: Length) -> UiNode {
    UiNode::new(key, UiContent::Text(TextSpec { text, role, style }))
        .layout(Layout::default().width(width).height(Length::Fill))
}

/// How wide `text` paints as `role` and `style` paint it.
pub(crate) fn text_width(text: &str, role: SemanticRole, style: StyleRef) -> f32 {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    if face == crate::widget::TextFace::Mono {
        crate::tray_raster::measure_text(text, px, crate::widget::TextWeight::Regular)
    } else {
        crate::tray_raster::ui_text_width_for(face, text, px)
    }
    .ceil()
        + 1.0
}

/// `text` on one line of at most `width`, its tail an ellipsis where it does not fit.
pub(crate) fn one_line(text: &str, role: SemanticRole, style: StyleRef, width: f32) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if width <= 0.0 {
        return String::new();
    }
    if text_width(&flat, role, style) <= width {
        return flat;
    }
    // By halves over the characters: a capped excerpt is a few hundred of them, each paint.
    let chars: Vec<char> = flat.chars().collect();
    let (mut fits, mut over) = (0, chars.len());
    while over - fits > 1 {
        let mid = (fits + over) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "\u{2026}";
        if text_width(&candidate, role, style) <= width {
            fits = mid;
        } else {
            over = mid;
        }
    }
    let kept: String = chars[..fits].iter().collect();
    format!("{}\u{2026}", kept.trim_end())
}

/// The page's measures at one width and text scale.
#[derive(Clone, Copy)]
struct Measure {
    scale: f32,
    pad: f32,
    inner: f32,
    title_line: f32,
    detail_line: f32,
    control_h: f32,
    row_pad: f32,
    /// A row too narrow for its buttons beside its words carries them on a line of their own.
    stacked: bool,
}

impl Measure {
    fn at(width: f32) -> Self {
        let scale = crate::native_appearance::text_scale();
        let pad = if width < 480.0 { 12.0 } else { 24.0 };
        let inner = (width - pad * 2.0).max(1.0);
        Self {
            scale,
            pad,
            inner,
            title_line: 22.0_f32.max(20.0 * scale),
            detail_line: 18.0_f32.max(16.0 * scale),
            control_h: 28.0 * scale.max(1.0),
            row_pad: 10.0,
            stacked: inner < 620.0,
        }
    }

    fn row_h(self) -> f32 {
        let lines = self.title_line + self.detail_line + self.row_pad * 2.0;
        if self.stacked {
            lines + self.control_h + 6.0
        } else {
            lines
        }
    }
}

fn action_button(key: String, label: &str, of_title: &str, style: StyleRef) -> UiNode {
    let width = crate::native_ui::native_button_label_width(label) + 24.0;
    UiNode::new(
        key.clone(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(format!("{label}: {of_title}")).visual_label(label),
                ActionId::new(key),
            )
            .state(ControlState {
                enabled: true,
                ..ControlState::default()
            })
            .style(style),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fill),
    )
}

/// The buttons a row offers now, and how wide they stand together: a running worker's result,
/// Stop Worker and Keep as Tab; a Finished one's result and Dismiss.
fn row_actions(row: &WorkerRow) -> (Vec<UiNode>, f32) {
    let mut buttons = Vec::new();
    if row.has_result {
        buttons.push(action_button(
            format!("{RESULT}{}", row.session),
            "Read Result",
            &row.title,
            StyleRef::Secondary,
        ));
    }
    if row.stoppable && !row.finished {
        buttons.push(action_button(
            format!("{STOP}{}", row.session),
            "Stop Worker",
            &row.title,
            StyleRef::Quiet,
        ));
    }
    buttons.push(if row.finished {
        action_button(
            format!("{DISMISS}{}", row.session),
            "Dismiss",
            &row.title,
            StyleRef::Quiet,
        )
    } else {
        action_button(
            format!("{KEEP}{}", row.session),
            crate::session_chrome::KEEP_AS_TAB,
            &row.title,
            StyleRef::Quiet,
        )
    });
    let gap = 6.0 * buttons.len().saturating_sub(1) as f32;
    let width = buttons
        .iter()
        .map(|button| match button.layout.width {
            Length::Fixed(width) => width,
            Length::Fill | Length::Fraction(_) => 0.0,
        })
        .sum::<f32>()
        + gap;
    (buttons, width)
}

const MARK_SLOT: f32 = crate::system_state::marks::MARK_SLOT;
const MARK_GAP: f32 = 12.0;
const ROW_INSET: f32 = 14.0;

/// One worker's row: a whole-row press under its painted lines, and its buttons on top.
fn row_node(row: &WorkerRow, m: Measure, selected: bool, focus_visible: bool) -> UiNode {
    let key = format!("workers/row/{}", row.session);
    let open = format!("{OPEN}{}", row.session);
    let (actions, actions_w) = row_actions(row);
    let beside = if m.stacked { 0.0 } else { actions_w + 12.0 };
    let text_w = (m.inner - ROW_INSET * 2.0 - MARK_SLOT - MARK_GAP - beside).max(1.0);

    let meta = row.meta_words();
    let meta_w = text_width(&meta, SemanticRole::Text, StyleRef::Note).min(text_w * 0.45);
    let title_room = (text_w - meta_w - 10.0).max(1.0);
    let title = words(
        format!("{key}/title"),
        one_line(
            &row.title,
            SemanticRole::Text,
            StyleRef::Primary,
            title_room,
        ),
        SemanticRole::Text,
        StyleRef::Primary,
        Length::Fill,
    );
    let meta = words(
        format!("{key}/meta"),
        one_line(&meta, SemanticRole::Text, StyleRef::Note, meta_w),
        SemanticRole::Text,
        StyleRef::Note,
        Length::Fixed(meta_w),
    );
    let first = plain_group(format!("{key}/first"))
        .layout(Layout::row().gap(10.0).height(Length::Fixed(m.title_line)))
        .children(vec![title, meta]);

    let style = row.state.style();
    let state_words = row.state_words();
    let state_w = text_width(&state_words, SemanticRole::Text, style).min(text_w * 0.6);
    let said_room = (text_w - state_w - 10.0).max(0.0);
    let mut second = vec![words(
        format!("{key}/state"),
        one_line(&state_words, SemanticRole::Text, style, state_w),
        SemanticRole::Text,
        style,
        Length::Fixed(state_w),
    )];
    if let Some(said) = &row.last_words {
        second.push(words(
            format!("{key}/said"),
            one_line(said, SemanticRole::Text, StyleRef::Note, said_room),
            SemanticRole::Text,
            StyleRef::Note,
            Length::Fill,
        ));
    }
    let second = plain_group(format!("{key}/second"))
        .layout(Layout::row().gap(10.0).height(Length::Fixed(m.detail_line)))
        .children(second);

    let lines = plain_group(format!("{key}/lines"))
        .layout(
            Layout::column()
                .width(Length::Fixed(text_w))
                .height(Length::Fixed(m.title_line + m.detail_line)),
        )
        .children(vec![first, second]);

    let mark_top = ((m.title_line - MARK_SLOT) / 2.0).max(0.0);
    let mark = plain_group(format!("{key}/seat"))
        .layout(
            Layout::column()
                .width(Length::Fixed(MARK_SLOT))
                .height(Length::Fixed(m.title_line + m.detail_line))
                .padding(Insets {
                    top: mark_top,
                    ..Insets::default()
                }),
        )
        .children(vec![
            UiNode::new(
                format!("{key}/mark"),
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::SYSTEM_STATE_MARK_AUDIT,
                    role: SemanticRole::Group,
                    label: String::new(),
                    paint: Some(mark_value(row.state.mark(), Ground::List)),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                    progress: None,
                    motion: None,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(MARK_SLOT))
                    .height(Length::Fixed(MARK_SLOT)),
            ),
        ]);

    let painted = plain_group(format!("{key}/painted"))
        .layout(
            Layout::row()
                .gap(MARK_GAP)
                .height(Length::Fixed(m.title_line + m.detail_line)),
        )
        .children(vec![mark, lines])
        .paint_only_tree();

    let press = UiNode::new(
        open.clone(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(row.spoken()).visual_label(String::new()),
                ActionId::new(open),
            )
            .state(ControlState {
                enabled: true,
                selected,
                focused: selected,
                focus_visible: selected && focus_visible,
                ..ControlState::default()
            })
            .style(StyleRef::Navigation),
        ),
    );

    let buttons = plain_group(format!("{key}/actions"))
        .layout(
            Layout::row()
                .gap(6.0)
                .width(Length::Fixed(actions_w))
                .height(Length::Fixed(m.control_h)),
        )
        .children(actions);

    let face: Vec<UiNode> = if m.stacked {
        let band = plain_group(format!("{key}/band"))
            .layout(
                Layout::column()
                    .gap(6.0)
                    .padding(Insets::symmetric(ROW_INSET, m.row_pad)),
            )
            .children(vec![
                painted,
                plain_group(format!("{key}/actions-line"))
                    .layout(
                        Layout::row()
                            .height(Length::Fixed(m.control_h))
                            .padding(Insets {
                                left: MARK_SLOT + MARK_GAP,
                                ..Insets::default()
                            }),
                    )
                    .children(vec![buttons]),
            ]);
        vec![press, band]
    } else {
        let button_top = ((m.title_line + m.detail_line - m.control_h) / 2.0).max(0.0);
        let band = plain_group(format!("{key}/band"))
            .layout(
                Layout::row()
                    .gap(12.0)
                    .padding(Insets::symmetric(ROW_INSET, m.row_pad)),
            )
            .children(vec![
                painted.layout(
                    Layout::row()
                        .gap(MARK_GAP)
                        .width(Length::Fixed(MARK_SLOT + MARK_GAP + text_w))
                        .height(Length::Fixed(m.title_line + m.detail_line)),
                ),
                plain_group(format!("{key}/actions-seat"))
                    .layout(
                        Layout::column()
                            .width(Length::Fixed(actions_w))
                            .height(Length::Fixed(m.title_line + m.detail_line))
                            .padding(Insets {
                                top: button_top,
                                ..Insets::default()
                            }),
                    )
                    .children(vec![buttons]),
            ]);
        vec![press, band]
    };

    plain_group(key)
        .layout(Layout::default().height(Length::Fixed(m.row_h())))
        .children(face)
}

/// A group's card: a hairline between rows.
fn group_card(
    index: usize,
    members: &[&WorkerRow],
    m: Measure,
    selected: Option<u64>,
    focus_visible: bool,
) -> (UiNode, f32) {
    let mut children = Vec::with_capacity(members.len() * 2);
    let mut height = 0.0;
    for (at, row) in members.iter().enumerate() {
        if at > 0 {
            children.push(
                UiNode::new(
                    format!("workers/group/{index}/rule/{at}"),
                    UiContent::Custom(AuditedCustomNode {
                        audit_id: crate::native_ui::MESSAGES_RULE_AUDIT,
                        role: SemanticRole::Group,
                        label: String::new(),
                        paint: None,
                        action: None,
                        focusable: false,
                        focused: false,
                        focus_visible: false,
                        progress: None,
                        motion: None,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(1.0)))
                .paint_only(),
            );
            height += 1.0;
        }
        children.push(row_node(
            row,
            m,
            selected == Some(row.session),
            focus_visible,
        ));
        height += m.row_h();
    }
    (
        UiNode::new(
            format!("workers/group/{index}/card"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::List).style(StyleRef::Secondary)),
        )
        .layout(Layout::column().height(Length::Fixed(height)))
        .children(children),
        height,
    )
}

/// The space between a group's label and its card, and on either side of the space between groups.
const LIST_GAP: f32 = 6.0;
/// The space between one group's card and the next group's label.
const GROUP_GAP: f32 = 10.0;

/// The page: its heading and summary over the groups' cards.
fn page(app: &WorkersApp, view: &WorkersViewState, width: f32, height: f32) -> UiTree {
    let m = Measure::at(width);
    let heading_h = 34.0 * m.scale.max(1.0);
    let summary_h = 22.0_f32.max(20.0 * m.scale);
    let header_h = heading_h + summary_h + 4.0;
    let header = plain_group("workers/header")
        .layout(Layout::column().gap(4.0).height(Length::Fixed(header_h)))
        .children(vec![
            UiNode::new(
                "workers/title",
                UiContent::Text(TextSpec::heading(WORKERS_TITLE)),
            )
            .layout(Layout::default().height(Length::Fixed(heading_h))),
            UiNode::new(
                "workers/summary",
                UiContent::Text(TextSpec {
                    text: summary_words(&app.rows),
                    role: SemanticRole::Status,
                    style: StyleRef::Note,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(summary_h))),
        ]);

    let room = (height - m.pad * 2.0 - header_h - 16.0).max(0.0);
    let selected = view.selected();
    let label_h = 20.0_f32.max(18.0 * m.scale);
    let mut spans = Vec::with_capacity(app.rows.len());
    let mut children = Vec::new();
    let mut top = 0.0;
    for (index, (name, members)) in grouped(&app.rows).into_iter().enumerate() {
        if index > 0 {
            children.push(
                plain_group(format!("workers/gap/{index}"))
                    .layout(Layout::default().height(Length::Fixed(GROUP_GAP)))
                    .paint_only(),
            );
            top += GROUP_GAP + LIST_GAP * 2.0;
        }
        let finished = members.iter().all(|row| row.finished);
        let label = UiNode::new(
            format!("workers/group/{index}/label"),
            UiContent::Text(TextSpec {
                text: name,
                role: SemanticRole::Text,
                style: StyleRef::Plain,
            }),
        );
        // The Finished group's heading carries Clear Finished, on a line as tall as its button.
        let line_h = if finished {
            label_h.max(m.control_h)
        } else {
            label_h
        };
        children.push(if finished {
            let clear = action_button(
                CLEAR_FINISHED.to_owned(),
                "Clear Finished",
                FINISHED_GROUP,
                StyleRef::Quiet,
            );
            plain_group(format!("workers/group/{index}/heading"))
                .layout(Layout::row().gap(12.0).height(Length::Fixed(line_h)))
                .children(vec![
                    label.layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                    clear,
                ])
        } else {
            label.layout(Layout::default().height(Length::Fixed(line_h)))
        });
        top += line_h + LIST_GAP;
        let mut row_top = top;
        for row in &members {
            spans.push((row.session, row_top, m.row_h()));
            row_top += m.row_h() + 1.0;
        }
        let (card, card_h) = group_card(index, &members, m, selected, view.common.focus_visible);
        children.push(card);
        top += card_h;
    }
    *view.row_spans.borrow_mut() = spans;
    view.room.set(room);
    view.scroll_limit.set((top - room).max(0.0));

    // A list longer than its room fades where it is cut, so a cut row reads as more to scroll to,
    // and gives the tree every row, so the arrows and a control client reach those it cut.
    let limit = view.scroll_limit.get();
    let list = plain_group("workers/list")
        .layout(
            Layout::column()
                .height(Length::Fixed(room))
                .gap(LIST_GAP)
                .clipped()
                .scrolled(view.scroll.min(limit)),
        )
        .room(room, limit > 0.0)
        .lists_every_row()
        .children(children);

    let mut root = UiNode::new(
        "workers/root",
        UiContent::Group(GroupSpec::new(WORKERS_TITLE)),
    )
    .layout(
        Layout::column()
            .padding(Insets::all(m.pad))
            .gap(16.0)
            .clipped(),
    )
    .children(vec![header, list]);
    if let UiContent::Group(spec) = &mut root.content {
        spec.role = SemanticRole::Application;
    }
    UiTree::new(root)
}

#[cfg(test)]
#[path = "native_workers_tests.rs"]
mod tests;
