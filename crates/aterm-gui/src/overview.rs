// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overview card (`⌘⇧O` / `Ctrl+Shift+O`): session chips, fabric needs, and manager lines.

#[cfg(test)]
use aterm_render::Theme;
use aterm_session::SessionId;

#[cfg(a11y_tree)]
use crate::palette::{a11y_epoch_from, a11y_slot_for, list_overlay_a11y};
use crate::palette::{finish_overlay_card, overlay_card_frame, row_text_writer};
use crate::settings::{Roles, SettingsGeom};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TrayInput, rgba};

/// The most body lines shown at once; beyond this the card scrolls.
const MAX_BODY_LINES: usize = 16;
/// Chrome rows framing the scrolling band: title on top, hint footer below.
const CHROME_ROWS: usize = 2;
/// Maximum logical width of the floating card (the palette's rule).
const MAX_CARD_WIDTH: f32 = 760.0;

/// What a chip's leading mark says, most pressing first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChipMark {
    /// A stuck need (`◆`).
    Stuck,
    /// A decide need (`◇`).
    Decide,
    /// The person holds it (`✋`).
    Yours,
    /// Its task is done (`✓`).
    Done,
    /// Its agent or task is working (`●`).
    Working,
    /// Nothing to say (`·`).
    Quiet,
}

impl ChipMark {
    /// The glyph the line opens with.
    pub(crate) const fn glyph(self) -> &'static str {
        match self {
            Self::Stuck => "\u{25c6}",
            Self::Decide => "\u{25c7}",
            Self::Yours => "\u{270b}",
            Self::Done => "\u{2713}",
            Self::Working => "\u{25cf}",
            Self::Quiet => "\u{00b7}",
        }
    }

    /// The word `controls overview` prints.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Stuck => "stuck",
            Self::Decide => "decide",
            Self::Yours => "yours",
            Self::Done => "done",
            Self::Working => "working",
            Self::Quiet => "quiet",
        }
    }
}

/// Who drives a session, as its line says it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Driver {
    /// The person took it over (`you have it`).
    You,
    /// A manager here started its task (`worker of <label>`).
    WorkerOf(String),
    /// A manager outside this aterm started it (`worker of <sid> (not here)`).
    WorkerOfElsewhere(String),
    /// aterm's harness drives it under this Autopilot wording.
    Aterm(String),
}

impl Driver {
    /// The words on the line.
    pub(crate) fn words(&self) -> String {
        match self {
            Self::You => "you have it".to_string(),
            Self::WorkerOf(manager) => format!("worker of {manager}"),
            Self::WorkerOfElsewhere(sid) => format!("worker of {sid} (not here)"),
            Self::Aterm(autopilot) => format!("driven by aterm ({autopilot})"),
        }
    }

    /// The word `controls overview` prints.
    pub(crate) const fn word(&self) -> &'static str {
        match self {
            Self::You => "person",
            Self::WorkerOf(_) | Self::WorkerOfElsewhere(_) => "worker",
            Self::Aterm(_) => "aterm",
        }
    }
}

/// One session line of the Overview: plain words the App composed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverviewChip {
    pub(crate) sid: SessionId,
    /// Registry local id.
    pub(crate) local_id: u64,
    /// Tab label as shown in the tab strip.
    pub(crate) label: String,
    pub(crate) mark: ChipMark,
    /// The need's word (`frozen`, `ask`, …), when one stands.
    pub(crate) need: Option<&'static str>,
    /// Need words, or what the session is doing and for how long.
    pub(crate) doing: String,
    /// Task record state word (`working`, `done`, `stalled`, …).
    pub(crate) task: Option<&'static str>,
    /// The task's words (`task working 12m`, `task done 4m · 2.1 KB`).
    pub(crate) task_words: Option<String>,
    /// Who drives it: one word, the person's hold first, so it is no count.
    pub(crate) driver: Option<Driver>,
    /// Reason Land still holds, rendered on a line under the chip.
    pub(crate) held: Option<String>,
    /// Who last typed into the session over the socket and when.
    pub(crate) typed: Option<(String, String)>,
    /// Whether the session's task `from=` names a manager.
    pub(crate) worker_of: bool,
    /// Whether the person's hold stands (`t` hands back).
    pub(crate) held_by_person: bool,
    /// Its task is done with a result to read (`r`).
    pub(crate) has_result: bool,
    /// Whether `⌫` can stop this session's running worker task.
    pub(crate) stoppable: bool,
}

/// One window group of chips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverviewGroup {
    /// The window header the group renders under.
    pub(crate) label: String,
    pub(crate) chips: Vec<OverviewChip>,
}

/// Remote fabric session whose bus log reports a person need.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FabricChip {
    /// Its node id.
    pub(crate) node: String,
    /// Its face on the bus: the session's sid there.
    pub(crate) sid: String,
    /// Leading mark (`◇` decide or `◆` stuck).
    pub(crate) mark: ChipMark,
    /// The need's word (`approval:bash`): `controls overview`'s `need=`.
    pub(crate) need: String,
    /// The need's words as the clause says them (`approval · bash`).
    pub(crate) words: String,
    /// Age of its log's newest record in seconds as of the compose.
    pub(crate) age_s: Option<u64>,
    /// Whether its read is older than `COUNT_FRESH` or its node is gone.
    pub(crate) older: bool,
}

/// Fabric group chips and optional read-failure word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FabricGroup {
    /// The chips, in the brief's order.
    pub(crate) chips: Vec<FabricChip>,
    /// Why the last read failed (`overview_fabric::ReadFail::word`).
    pub(crate) failed: Option<&'static str>,
}

/// The fabric group's header.
pub(crate) const FABRIC_GROUP: &str = "On the fabric";

/// One "manages" line: a manager and the workers its tasks started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Manages {
    /// The manager's sid (`from=`).
    pub(crate) manager: SessionId,
    /// Its local id when this aterm hosts it (⏎ goes there), else `None`.
    pub(crate) manager_local: Option<u64>,
    /// The manager's label, or its sid with `(not here)`.
    pub(crate) manager_label: String,
    /// Its workers' labels, in session order.
    pub(crate) workers: Vec<(SessionId, String)>,
}

/// Selectable element in navigation order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverviewItem {
    Chip {
        group: usize,
        chip: usize,
    },
    /// A [`FabricChip`], by its index.
    Fabric {
        chip: usize,
    },
    Manages {
        line: usize,
    },
}

/// Action resolved from a key or click on the current selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum OverviewAct {
    /// Nothing to act on.
    None,
    /// Go to this session (its local id) and close the card.
    Raise(u64),
    /// Take this session over, or hand it back when the person holds it.
    TakeOverOrHandBack(u64),
    /// Read this done worker's result.
    ReadResult(u64),
    /// Stop Worker confirm armed on this worker's local id.
    Armed(u64),
    /// Stop this worker (the armed confirm resolved).
    StopWorker(u64),
}

/// One painted body line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OverviewLine {
    Group(usize),
    Chip {
        item: usize,
        group: usize,
        chip: usize,
    },
    /// Why Land still holds a chip ([`OverviewChip::held`]), under its
    Held {
        item: usize,
        group: usize,
        chip: usize,
    },
    /// The fabric group's header ([`FABRIC_GROUP`]).
    FabricHeader,
    Fabric {
        item: usize,
        chip: usize,
    },
    ManagesHeader,
    Manages {
        item: usize,
        line: usize,
    },
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OverviewLayout {
    card: (f32, f32, f32, f32),
    card_rows: usize,
    body_rows: usize,
}

/// Transient state of the open Overview overlay.
pub(crate) struct OverviewState {
    /// The hosting (frontmost-at-open) window.
    pub(crate) window: crate::WindowId,
    groups: Vec<OverviewGroup>,
    /// The fabric group's chips ([`FabricChip`]), empty where the fabric is
    fabric: Vec<FabricChip>,
    /// Why the last read of the fabric's log failed ([`FabricGroup::failed`]).
    fabric_failed: Option<&'static str>,
    manages: Vec<Manages>,
    /// Cursor over [`Self::items`].
    selected: usize,
    /// Scroll over body lines (headers included), clamped by moves.
    scroll: usize,
    /// The chip item whose Stop Worker confirm is armed.
    confirm: Option<usize>,
    /// The armed confirm is a CONTROL CLIENT'S (its ⌫ came through the `key`
    confirm_by_controller: bool,
    /// Exact item index under the pointer (the palette's distinct rule).
    pointer_over: Option<usize>,
    pointer_armed: Option<usize>,
    /// How many sessions another aterm on this Mac holds, as the last sibling
    elsewhere: usize,
}

/// Selection identity that survives a refresh (retarget re-finds it).
#[derive(Clone, PartialEq, Eq, Debug)]
enum Identity {
    Chip(SessionId),
    /// A fabric chip, by its node and face.
    Fabric(String, String),
    Manages(SessionId),
}

impl OverviewState {
    pub(crate) fn new(
        window: crate::WindowId,
        groups: Vec<OverviewGroup>,
        manages: Vec<Manages>,
    ) -> Self {
        Self {
            window,
            groups,
            fabric: Vec::new(),
            fabric_failed: None,
            manages,
            selected: 0,
            scroll: 0,
            confirm: None,
            confirm_by_controller: false,
            pointer_over: None,
            pointer_armed: None,
            elsewhere: 0,
        }
    }

    /// Update the sibling aterm session count and return whether it changed.
    pub(crate) fn set_elsewhere(&mut self, elsewhere: usize) -> bool {
        std::mem::replace(&mut self.elsewhere, elsewhere) != elsewhere
    }

    /// Every selectable element in navigation order.
    fn items(&self) -> Vec<OverviewItem> {
        let mut out = Vec::new();
        for (g, group) in self.groups.iter().enumerate() {
            for c in 0..group.chips.len() {
                out.push(OverviewItem::Chip { group: g, chip: c });
            }
        }
        for chip in 0..self.fabric.len() {
            out.push(OverviewItem::Fabric { chip });
        }
        for line in 0..self.manages.len() {
            out.push(OverviewItem::Manages { line });
        }
        out
    }

    /// The painted body lines: group headers and chips, then the manages
    fn lines(&self) -> Vec<OverviewLine> {
        let mut out = Vec::new();
        let mut item = 0;
        for (g, group) in self.groups.iter().enumerate() {
            out.push(OverviewLine::Group(g));
            for (c, chip) in group.chips.iter().enumerate() {
                out.push(OverviewLine::Chip {
                    item,
                    group: g,
                    chip: c,
                });
                if chip.held.is_some() {
                    out.push(OverviewLine::Held {
                        item,
                        group: g,
                        chip: c,
                    });
                }
                item += 1;
            }
        }
        if self.fabric_shown() {
            out.push(OverviewLine::FabricHeader);
            for chip in 0..self.fabric.len() {
                out.push(OverviewLine::Fabric { item, chip });
                item += 1;
            }
        }
        if !self.manages.is_empty() {
            out.push(OverviewLine::ManagesHeader);
            for line in 0..self.manages.len() {
                out.push(OverviewLine::Manages { item, line });
                item += 1;
            }
        }
        if out.is_empty() {
            out.push(OverviewLine::Empty);
        }
        out
    }

    fn line_of_item(&self, idx: usize) -> Option<usize> {
        self.lines().iter().position(|l| match l {
            OverviewLine::Chip { item, .. }
            | OverviewLine::Fabric { item, .. }
            | OverviewLine::Manages { item, .. } => *item == idx,
            _ => false,
        })
    }

    /// A chip of THIS aterm's: what every act on the card acts on. A
    fn chip(&self, item: OverviewItem) -> Option<&OverviewChip> {
        match item {
            OverviewItem::Chip { group, chip } => self.groups.get(group)?.chips.get(chip),
            OverviewItem::Fabric { .. } | OverviewItem::Manages { .. } => None,
        }
    }

    /// The fabric chip `item` is, when it is one.
    fn fabric_chip(&self, item: OverviewItem) -> Option<&FabricChip> {
        match item {
            OverviewItem::Fabric { chip } => self.fabric.get(chip),
            OverviewItem::Chip { .. } | OverviewItem::Manages { .. } => None,
        }
    }

    /// Whether the card shows the fabric group: it lists a chip, or the last
    fn fabric_shown(&self) -> bool {
        !self.fabric.is_empty() || self.fabric_failed.is_some()
    }

    /// Selected fabric chip, when the selection is on one.
    pub(crate) fn selected_fabric(&self) -> Option<&FabricChip> {
        self.fabric_chip(self.selected_item()?)
    }

    /// The selected element, or `None` on an empty card.
    pub(crate) fn selected_item(&self) -> Option<OverviewItem> {
        self.items().get(self.selected).copied()
    }

    /// The selected chip, when the selection is one.
    pub(crate) fn selected_chip(&self) -> Option<&OverviewChip> {
        self.chip(self.selected_item()?)
    }

    /// How many sessions the card lists, how many need the person, and how
    fn counts(&self) -> (usize, usize, usize) {
        let chips = self.groups.iter().flat_map(|g| g.chips.iter());
        let (mut sessions, mut needs, mut workers) = (0, 0, 0);
        for chip in chips {
            sessions += 1;
            needs += usize::from(chip.need.is_some());
            workers += usize::from(chip.worker_of);
        }
        needs += self.fabric.iter().filter(|c| !c.older).count();
        (sessions, needs, workers)
    }

    /// Format the Overview card's title line.
    pub(crate) fn title(&self) -> String {
        let (sessions, needs, workers) = self.counts();
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("{n} {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let mut title = format!(
            "This aterm \u{2014} {}",
            plural(sessions, "session", "sessions")
        );
        if self.elsewhere > 0 {
            title.push_str(&format!(" (+{} in another aterm)", self.elsewhere));
        }
        if needs > 0 {
            title.push_str(&format!(" \u{00b7} {needs} need you"));
        }
        if workers > 0 {
            title.push_str(&format!(
                " \u{00b7} {}",
                plural(workers, "worker", "workers")
            ));
        }
        title
    }

    /// A chip's display line (paint == a11y == `controls`).
    fn chip_text(&self, item: usize, chip: &OverviewChip) -> String {
        let mut label = crate::session_timeline::sanitize_presentation_line(&chip.label, 40);
        if label.is_empty() {
            label = "(untitled)".to_string();
        }
        let mut parts = vec![format!(
            "{} {label} \u{2014} {}",
            chip.mark.glyph(),
            chip.doing
        )];
        parts.extend(chip.task_words.clone());
        parts.extend(chip.driver.as_ref().map(Driver::words));
        parts.extend(
            chip.typed
                .as_ref()
                .map(|(by, when)| format!("typed by {by} {when}")),
        );
        let mut s = parts.join(" \u{00b7} ");
        if self.confirm == Some(item) {
            s.push_str(if self.confirm_by_controller {
                "  a script asks to stop it \u{00b7} \u{232b} twice stops it \u{00b7} esc keeps"
            } else {
                "  stop worker? \u{232b} again stops it \u{00b7} esc keeps"
            });
        }
        s
    }

    /// A fabric chip's display line (paint == a11y): `◇ approval · bash —
    fn fabric_text(chip: &FabricChip) -> String {
        let mut s = format!(
            "{} {} \u{2014} {} on {}",
            chip.mark.glyph(),
            chip.words,
            aterm_messages::presence::short_sid(&chip.sid),
            chip.node,
        );
        if let Some(age_s) = chip.age_s {
            s.push_str(" \u{00b7} ");
            s.push_str(&crate::app_overview::coarse_age(age_s.saturating_mul(1000)));
        }
        if chip.older {
            s.push_str(" \u{00b7} older");
        }
        s
    }

    /// Whether `t` hands the selected tab back rather than taking it over.
    pub(crate) fn t_hands_back(&self) -> bool {
        self.selected_chip().is_some_and(|c| c.held_by_person)
    }

    /// A manages line: `lab-sweep ─manages→ bench, fmt, lint`.
    fn manages_text(&self, manages: &Manages) -> String {
        let workers: Vec<&str> = manages.workers.iter().map(|(_, l)| l.as_str()).collect();
        format!(
            "{} \u{2500}manages\u{2192} {}",
            manages.manager_label,
            workers.join(", ")
        )
    }

    /// TEST READ: the painted body lines' words, in order.
    #[cfg(test)]
    pub(crate) fn line_texts(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .map(|line| self.line_text(line))
            .collect()
    }

    fn line_text(&self, line: OverviewLine) -> String {
        match line {
            OverviewLine::Group(g) => format!("\u{25b8} {}", self.groups[g].label),
            OverviewLine::Chip { item, group, chip } => {
                self.chip_text(item, &self.groups[group].chips[chip])
            }
            OverviewLine::Held { group, chip, .. } => self.groups[group].chips[chip]
                .held
                .clone()
                .unwrap_or_default(),
            OverviewLine::FabricHeader => match self.fabric_failed {
                Some(_) => format!("\u{25b8} {FABRIC_GROUP} \u{00b7} the last read failed"),
                None => format!("\u{25b8} {FABRIC_GROUP}"),
            },
            OverviewLine::Fabric { chip, .. } => Self::fabric_text(&self.fabric[chip]),
            OverviewLine::ManagesHeader => format!("manages ({})", self.manages.len()),
            OverviewLine::Manages { line, .. } => self.manages_text(&self.manages[line]),
            OverviewLine::Empty => "no sessions".to_string(),
        }
    }

    fn identity(&self, idx: usize) -> Option<Identity> {
        match *self.items().get(idx)? {
            item @ OverviewItem::Chip { .. } => {
                self.chip(item).map(|c| Identity::Chip(c.sid.clone()))
            }
            OverviewItem::Fabric { chip } => self
                .fabric
                .get(chip)
                .map(|c| Identity::Fabric(c.node.clone(), c.sid.clone())),
            OverviewItem::Manages { line } => self
                .manages
                .get(line)
                .map(|m| Identity::Manages(m.manager.clone())),
        }
    }

    /// Replace the snapshot while preserving selection and confirm by identity.
    pub(crate) fn retarget(&mut self, groups: Vec<OverviewGroup>, manages: Vec<Manages>) -> bool {
        if self.groups == groups && self.manages == manages {
            return false;
        }
        self.reshape(|state| {
            state.groups = groups;
            state.manages = manages;
        });
        true
    }

    /// Replace the fabric group and report whether the displayed words changed.
    pub(crate) fn set_fabric(&mut self, group: FabricGroup) -> bool {
        let FabricGroup {
            chips: fabric,
            failed,
        } = group;
        if self.fabric == fabric && self.fabric_failed == failed {
            return false;
        }
        let same_items = self.fabric.len() == fabric.len()
            && self
                .fabric
                .iter()
                .zip(&fabric)
                .all(|(a, b)| a.node == b.node && a.sid == b.sid);
        let shown_moved = self.fabric_failed != failed
            || !same_items
            || self
                .fabric
                .iter()
                .zip(&fabric)
                .any(|(a, b)| Self::fabric_text(a) != Self::fabric_text(b));
        if same_items && self.fabric_failed == failed {
            self.fabric = fabric;
        } else {
            self.reshape(|state| {
                state.fabric = fabric;
                state.fabric_failed = failed;
            });
        }
        shown_moved
    }

    /// Apply `change` to the lists, then re-find the selection and the armed
    fn reshape(&mut self, change: impl FnOnce(&mut Self)) {
        let keep_selected = self.identity(self.selected);
        let keep_confirm = self.confirm.and_then(|idx| self.identity(idx));
        change(self);
        let find = |state: &Self, id: &Identity| -> Option<usize> {
            state.items().iter().position(|it| match (it, id) {
                (OverviewItem::Chip { .. }, Identity::Chip(sid)) => {
                    state.chip(*it).is_some_and(|c| c.sid == *sid)
                }
                (OverviewItem::Fabric { .. }, Identity::Fabric(node, sid)) => state
                    .fabric_chip(*it)
                    .is_some_and(|c| c.node == *node && c.sid == *sid),
                (OverviewItem::Manages { line }, Identity::Manages(sid)) => {
                    state.manages.get(*line).is_some_and(|m| m.manager == *sid)
                }
                _ => false,
            })
        };
        self.selected = keep_selected
            .as_ref()
            .and_then(|id| find(self, id))
            .unwrap_or(0)
            .min(self.items().len().saturating_sub(1));
        self.confirm = keep_confirm
            .as_ref()
            .and_then(|id| find(self, id))
            .filter(|idx| {
                self.items()
                    .get(*idx)
                    .and_then(|it| self.chip(*it))
                    .is_some_and(|c| c.stoppable)
            });
        self.pointer_over = None;
        self.pointer_armed = None;
        self.clamp_scroll();
    }

    /// Move the cursor (wrapping); movement cancels an armed confirm.
    pub(crate) fn move_selection(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.items().len();
        if n == 0 {
            return;
        }
        let next = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        if next != self.selected {
            self.confirm = None;
        }
        self.selected = next;
        self.clamp_scroll();
    }

    /// Move the cursor to item `idx` (a11y Focus / pointer land here).
    pub(crate) fn select(&mut self, idx: usize) {
        let n = self.items().len();
        if n == 0 {
            return;
        }
        let next = idx.min(n - 1);
        if next != self.selected {
            self.confirm = None;
        }
        self.selected = next;
        self.clamp_scroll();
    }

    /// Move the cursor to `session` (its manages line when `as_manager` and present, else its chip).
    pub(crate) fn select_session(&mut self, session: u64, as_manager: bool) -> bool {
        let items = self.items();
        let line = items.iter().position(|item| {
            as_manager
                && matches!(item, OverviewItem::Manages { line }
                    if self.manages.get(*line).and_then(|m| m.manager_local) == Some(session))
        });
        let chip = || {
            items
                .iter()
                .position(|item| self.chip(*item).is_some_and(|c| c.local_id == session))
        };
        let Some(idx) = line.or_else(chip) else {
            return false;
        };
        self.select(idx);
        true
    }

    /// Scroll the band without moving the cursor (wheel).
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let total = self.lines().len();
        let body = total.min(MAX_BODY_LINES);
        let max_scroll = total.saturating_sub(body);
        let before = self.scroll;
        self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
        self.pointer_over = None;
        self.pointer_armed = None;
        before != self.scroll
    }

    fn clamp_scroll(&mut self) {
        let total = self.lines().len();
        let body = total.min(MAX_BODY_LINES);
        if body == 0 {
            self.scroll = 0;
            return;
        }
        if let Some(line) = self.line_of_item(self.selected) {
            let last = match self.lines().get(line + 1) {
                Some(OverviewLine::Held { item, .. }) if *item == self.selected => line + 1,
                _ => line,
            };
            if line < self.scroll {
                self.scroll = line;
            } else if last >= self.scroll + body {
                self.scroll = (last + 1 - body).min(line);
            }
        }
        self.scroll = self.scroll.min(total.saturating_sub(body));
    }

    /// Activate the selected chip or manages line (`⏎` or click).
    pub(crate) fn activate(&mut self) -> OverviewAct {
        self.confirm = None;
        match self.selected_item() {
            None | Some(OverviewItem::Fabric { .. }) => OverviewAct::None,
            Some(item @ OverviewItem::Chip { .. }) => self
                .chip(item)
                .map_or(OverviewAct::None, |c| OverviewAct::Raise(c.local_id)),
            Some(OverviewItem::Manages { line }) => self
                .manages
                .get(line)
                .and_then(|m| m.manager_local)
                .map_or(OverviewAct::None, OverviewAct::Raise),
        }
    }

    /// `t`: take the selected session over, or hand it back.
    pub(crate) fn take_over_pressed(&mut self) -> OverviewAct {
        self.confirm = None;
        self.selected_chip().map_or(OverviewAct::None, |c| {
            OverviewAct::TakeOverOrHandBack(c.local_id)
        })
    }

    /// `r`: read the selected done worker's result.
    pub(crate) fn read_result_pressed(&mut self) -> OverviewAct {
        self.confirm = None;
        self.selected_chip()
            .filter(|c| c.has_result)
            .map_or(OverviewAct::None, |c| OverviewAct::ReadResult(c.local_id))
    }

    /// Handle `⌫` on a stoppable worker (arm confirm, or resolve when armed by the same hand).
    pub(crate) fn stop_pressed(&mut self, controller: bool) -> OverviewAct {
        let Some(chip) = self.selected_chip().filter(|c| c.stoppable) else {
            self.confirm = None;
            return OverviewAct::None;
        };
        let local = chip.local_id;
        if self.confirm == Some(self.selected) && self.confirm_by_controller == controller {
            self.confirm = None;
            return OverviewAct::StopWorker(local);
        }
        self.confirm = Some(self.selected);
        self.confirm_by_controller = controller;
        OverviewAct::Armed(local)
    }

    /// Select `session`'s chip and arm its Stop Worker confirm.
    pub(crate) fn arm_stop_on(&mut self, session: u64, controller: bool) -> bool {
        let Some(idx) = self
            .items()
            .iter()
            .position(|it| self.chip(*it).is_some_and(|c| c.local_id == session))
        else {
            return false;
        };
        self.select(idx);
        self.confirm = None;
        matches!(self.stop_pressed(controller), OverviewAct::Armed(_))
    }

    /// Drop an armed confirm (the App refused the arm).
    pub(crate) fn cancel_confirm(&mut self) {
        self.confirm = None;
    }

    /// Cancel an armed confirm on `Esc`, or return `true` to close the card.
    pub(crate) fn escape(&mut self) -> bool {
        if self.confirm.is_some() {
            self.confirm = None;
            return false;
        }
        true
    }

    /// Hover an item, dragging the selection with the pointer.
    pub(crate) fn pointer_hover(&mut self, idx: Option<usize>) -> bool {
        let before = (self.selected, self.confirm, self.pointer_over);
        if let Some(idx) = idx {
            self.select(idx);
        }
        self.pointer_over = idx;
        before != (self.selected, self.confirm, self.pointer_over)
    }

    /// Arm the hovered item on left press.
    pub(crate) fn pointer_press(&mut self, idx: Option<usize>) -> bool {
        let mut changed = self.pointer_hover(idx);
        changed |= self.pointer_armed != self.pointer_over;
        self.pointer_armed = self.pointer_over;
        changed
    }

    /// Settle a left pointer release into `(changed, activate)`.
    pub(crate) fn pointer_release(&mut self, idx: Option<usize>) -> (bool, bool) {
        let mut changed = self.pointer_hover(idx);
        let armed = self.pointer_armed.take();
        changed |= armed.is_some();
        (changed, idx.is_some() && armed == idx)
    }

    pub(crate) fn pointer_over_item(&self) -> bool {
        self.pointer_over.is_some()
    }

    /// The overlay height: chrome + the (capped) body band, never `0`.
    pub(crate) fn wanted_rows(&self) -> usize {
        CHROME_ROWS + self.lines().len().clamp(1, MAX_BODY_LINES)
    }

    /// `(scroll, total, visible)` for `controls front`.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        let total = self.lines().len();
        (self.scroll, total, total.min(MAX_BODY_LINES))
    }

    /// Machine-readable lines for `controls overview`.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let (sessions, needs, workers) = self.counts();
        let mut out = vec![format!(
            "overview window={} sessions={sessions} needs={needs} workers={workers} \
             manages={} selected={} confirm={} confirm_by={} fabric={} fabric_error={}",
            self.window.0,
            self.manages.len(),
            self.selected,
            self.confirm
                .map_or_else(|| "-".to_string(), |i| i.to_string()),
            match (self.confirm, self.confirm_by_controller) {
                (None, _) => "-",
                (Some(_), true) => "controller",
                (Some(_), false) => "person",
            },
            self.fabric.len(),
            self.fabric_failed.unwrap_or("-"),
        )];
        let mut item = 0usize;
        for group in &self.groups {
            out.push(format!("overview group label={:?}", group.label));
            for chip in &group.chips {
                out.push(format!(
                    "overview chip sid={} local={} mark={} need={} task_state={} driver={} \
                     typed_by={} result={} stoppable={} selected={} confirm={} held={}",
                    chip.sid.as_str(),
                    chip.local_id,
                    chip.mark.word(),
                    chip.need.unwrap_or("-"),
                    chip.task.unwrap_or("-"),
                    chip.driver.as_ref().map_or("-", Driver::word),
                    chip.typed
                        .as_ref()
                        .map_or_else(|| "-".to_string(), |(by, _)| crate::control::pct_encode(by)),
                    u8::from(chip.has_result),
                    u8::from(chip.stoppable),
                    item == self.selected,
                    self.confirm == Some(item),
                    u8::from(chip.held.is_some()),
                ));
                item += 1;
            }
        }
        if self.fabric_shown() {
            out.push(format!("overview group label={FABRIC_GROUP:?}"));
        }
        for chip in &self.fabric {
            out.push(format!(
                "overview fabric chip node={} sid={} need={} age={} mark={} older={} selected={}",
                chip.node,
                chip.sid,
                chip.need,
                chip.age_s
                    .map_or_else(|| "-".to_string(), |a| a.to_string()),
                chip.mark.word(),
                u8::from(chip.older),
                item == self.selected,
            ));
            item += 1;
        }
        for manages in &self.manages {
            let workers: Vec<&str> = manages.workers.iter().map(|(s, _)| s.as_str()).collect();
            out.push(format!(
                "overview manages manager={} local={} workers={} selected={}",
                manages.manager.as_str(),
                manages
                    .manager_local
                    .map_or_else(|| "-".to_string(), |l| l.to_string()),
                workers.join(","),
                item == self.selected,
            ));
            item += 1;
        }
        out
    }

    /// Repaint fingerprint (never `0` while open).
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.window.0.hash(&mut h);
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
        self.confirm.hash(&mut h);
        self.confirm_by_controller.hash(&mut h);
        self.elsewhere.hash(&mut h);
        for group in &self.groups {
            group.label.hash(&mut h);
            for chip in &group.chips {
                chip.sid.as_str().hash(&mut h);
                chip.local_id.hash(&mut h);
                chip.label.hash(&mut h);
                chip.mark.hash(&mut h);
                chip.need.hash(&mut h);
                chip.doing.hash(&mut h);
                chip.task_words.hash(&mut h);
                chip.driver.hash(&mut h);
                chip.held.hash(&mut h);
                chip.typed.hash(&mut h);
                chip.worker_of.hash(&mut h);
                chip.held_by_person.hash(&mut h);
            }
        }
        for chip in &self.fabric {
            Self::fabric_text(chip).hash(&mut h);
        }
        self.fabric_failed.hash(&mut h);
        for manages in &self.manages {
            manages.manager.as_str().hash(&mut h);
            manages.manager_label.hash(&mut h);
            for (sid, label) in &manages.workers {
                sid.as_str().hash(&mut h);
                label.hash(&mut h);
            }
        }
        h.finish() | 1
    }

    /// A11y row-identity epoch over the item list (the palette's scheme).
    #[cfg(a11y_tree)]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for item in self.items() {
            match item {
                OverviewItem::Chip { .. } => {
                    0u8.hash(&mut h);
                    if let Some(c) = self.chip(item) {
                        c.sid.as_str().hash(&mut h);
                    }
                }
                OverviewItem::Fabric { chip } => {
                    2u8.hash(&mut h);
                    if let Some(c) = self.fabric.get(chip) {
                        c.node.hash(&mut h);
                        c.sid.hash(&mut h);
                    }
                }
                OverviewItem::Manages { line } => {
                    1u8.hash(&mut h);
                    self.manages[line].manager.as_str().hash(&mut h);
                }
            }
        }
        a11y_epoch_from(h.finish())
    }

    /// Decode a node minted by the current epoch to its item index.
    #[cfg(a11y_tree)]
    pub(crate) fn a11y_item_index(&self, node: aterm_a11y::model::NodeId) -> Option<usize> {
        a11y_slot_for(self.a11y_epoch(), node, self.items().len())
    }
}

/// One pure geometry projection shared by paint and hit-testing — the
fn overview_layout(state: &OverviewState, g: &SettingsGeom) -> OverviewLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let tray_h = (g.panel_rows as f32 * g.ch).max(0.0);
    let desired_margin = (g.cw * 2.0).max(16.0);
    let max_margin = (tray_w * 0.5 - g.cw.max(0.0)).max(0.0);
    let margin = desired_margin.min(max_margin);
    let card_w = (tray_w - margin * 2.0).clamp(0.0, MAX_CARD_WIDTH);
    let card_rows = state.wanted_rows().min(g.panel_rows);
    let card_h = (card_rows as f32 * g.ch).clamp(0.0, tray_h);
    OverviewLayout {
        card: (
            ((tray_w - card_w) * 0.5).max(0.0),
            ((tray_h - card_h) * 0.5).max(0.0),
            card_w,
            card_h,
        ),
        card_rows,
        body_rows: state
            .lines()
            .len()
            .min(MAX_BODY_LINES)
            .min(card_rows.saturating_sub(CHROME_ROWS)),
    }
}

/// Exact painted selection/hit rectangle for one visible body slot.
fn overview_row_rect_in(
    layout: &OverviewLayout,
    g: &SettingsGeom,
    slot: usize,
) -> Option<(f32, f32, f32, f32)> {
    if slot >= layout.body_rows {
        return None;
    }
    let (card_x, card_y, card_w, _) = layout.card;
    Some((
        card_x + g.cw,
        card_y + (1 + slot) as f32 * g.ch + 1.0,
        (card_w - g.cw * 2.0).max(0.0),
        (g.ch - 2.0).max(0.0),
    ))
}

/// Item index under a card-local point `(px, py)`.
pub(crate) fn overview_item_hit(
    state: &OverviewState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<usize> {
    let lines = state.lines();
    let layout = overview_layout(state, g);
    for slot in 0..layout.body_rows {
        let (rx, ry, rw, rh) = overview_row_rect_in(&layout, g, slot)?;
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            return match lines.get(state.scroll + slot) {
                Some(
                    OverviewLine::Chip { item, .. }
                    | OverviewLine::Held { item, .. }
                    | OverviewLine::Fabric { item, .. }
                    | OverviewLine::Manages { item, .. },
                ) => Some(*item),
                _ => None,
            };
        }
    }
    None
}

/// `s` as the card paints it in `room` px at `px`: whole where it fits, else
fn fit_body(s: &str, room: f32, px: f32) -> String {
    let cells = crate::tab_menu::display_cells(s);
    let whole = crate::settings::text_w(s, px);
    if whole <= room {
        return crate::tab_menu::ellipsize(s, cells);
    }
    let advance = whole / cells.max(1) as f32;
    let mut fit = if advance > 0.0 {
        ((room / advance).floor().max(0.0) as usize).min(cells)
    } else {
        0
    };
    loop {
        let cut = crate::tab_menu::ellipsize(s, fit);
        if fit == 0 || crate::settings::text_w(&cut, px) <= room {
            return cut;
        }
        fit -= 1;
    }
}

/// Paint the Overview card into a [`SettingsTray`].
pub(crate) fn overview_tray(
    state: &OverviewState,
    g: &SettingsGeom,
    theme: impl Into<crate::chrome_band::ChromePalette>,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let layout = overview_layout(state, g);
    let (card_x, card_y, _, _) = layout.card;
    let mut prims = overlay_card_frame(layout.card, ch, &r, false);
    let text_at = row_text_writer(px, ch);
    text_at(
        &mut prims,
        card_x + cw,
        card_y,
        TypeStep::Body,
        state.title(),
        rgba(r.text_primary, 0xFF),
    );
    let lines = state.lines();
    let body_px = TypeStep::Body.px(px).get();
    for slot in 0..layout.body_rows {
        let Some(line) = lines.get(state.scroll + slot).copied() else {
            break;
        };
        let y0 = card_y + (1 + slot) as f32 * ch;
        let (indent, color) = match line {
            OverviewLine::Group(_) | OverviewLine::FabricHeader | OverviewLine::ManagesHeader => {
                (cw * 1.5, r.text_secondary)
            }
            OverviewLine::Chip { .. }
            | OverviewLine::Fabric { .. }
            | OverviewLine::Manages { .. } => (cw * 2.5, r.text_primary),
            OverviewLine::Held { .. } => (cw * 4.5, r.text_secondary),
            OverviewLine::Empty => (cw * 2.0, r.text_tertiary),
        };
        if let OverviewLine::Chip { item, .. }
        | OverviewLine::Fabric { item, .. }
        | OverviewLine::Manages { item, .. } = line
            && item == state.selected
        {
            let (x, y, w, mut h) = overview_row_rect_in(&layout, g, slot)
                .expect("painted overview slot has a row rectangle");
            if slot + 1 < layout.body_rows
                && matches!(
                    lines.get(state.scroll + slot + 1),
                    Some(OverviewLine::Held { item: under, .. }) if *under == item
                )
            {
                h += ch;
            }
            let ring = if state.confirm == Some(item) {
                r.danger
            } else {
                r.accent
            };
            prims.push(DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                fill: rgba(ring, 0x22),
            });
            prims.push(DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                width: 1.5,
                color: rgba(ring, 0xCC),
            });
        }
        text_at(
            &mut prims,
            card_x + indent,
            y0,
            TypeStep::Body,
            fit_body(&state.line_text(line), layout.card.2 - indent - cw, body_px),
            rgba(color, 0xFF),
        );
    }
    let fabric_hint;
    let hint = match (state.confirm.is_some(), state.confirm_by_controller) {
        (true, false) => "\u{232b} again stop worker   \u{23ce} go   esc keep",
        (true, true) => "\u{232b}\u{232b} stop worker   \u{23ce} go   esc keep",
        (false, _) if state.selected_fabric().is_some() => {
            fabric_hint = fabric_hint_of(state.selected_fabric().map_or("", |c| &c.node));
            fabric_hint.as_str()
        }
        (false, _) if state.t_hands_back() => {
            "\u{2191}\u{2193} move   \u{23ce} go   t hand back   r read result   \u{232b} stop worker   \
             esc close"
        }
        (false, _) => {
            "\u{2191}\u{2193} move   \u{23ce} go   t take over   r read result   \u{232b} stop worker   \
             esc close"
        }
    };
    finish_overlay_card(prims, hint, layout.card, layout.card_rows, g, &r)
}

/// The key hint on a fabric chip: its node, and nothing to do here.
pub(crate) fn fabric_hint_of(node: &str) -> String {
    format!("\u{2191}\u{2193} move   on {node}, answered there   esc close")
}

/// Build the Overview card's accessibility tree.
#[cfg(a11y_tree)]
pub(crate) fn overview_a11y(state: &OverviewState) -> aterm_a11y::model::TreeUpdate {
    use aterm_a11y::model::{Action, Node, Role};

    let items = state.items();
    let rows = items.iter().enumerate().map(|(slot, item)| {
        let mut node = Node::new(Role::MenuItem);
        let label = match *item {
            OverviewItem::Chip { group, chip } => {
                let chip = &state.groups[group].chips[chip];
                let text = state.chip_text(slot, chip);
                match &chip.held {
                    Some(held) => format!("{text} \u{00b7} {held}"),
                    None => text,
                }
            }
            OverviewItem::Fabric { chip } => OverviewState::fabric_text(&state.fabric[chip]),
            OverviewItem::Manages { line } => state.manages_text(&state.manages[line]),
        };
        node.set_label(label);
        node.add_action(Action::Focus);
        node.add_action(Action::Click);
        node
    });
    list_overlay_a11y("Overview", state.a11y_epoch(), state.selected, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s)
    }

    fn chip(s: &str, local: u64) -> OverviewChip {
        OverviewChip {
            sid: sid(s),
            local_id: local,
            label: format!("t-{s}"),
            mark: ChipMark::Quiet,
            need: None,
            doing: "idle".to_string(),
            task: None,
            task_words: None,
            driver: None,
            held: None,
            typed: None,
            worker_of: false,
            held_by_person: false,
            has_result: false,
            stoppable: false,
        }
    }

    fn worker(s: &str, local: u64, manager: &str) -> OverviewChip {
        OverviewChip {
            mark: ChipMark::Working,
            task: Some("working"),
            task_words: Some("task working 2m".to_string()),
            driver: Some(Driver::WorkerOf(manager.to_string())),
            worker_of: true,
            stoppable: true,
            ..chip(s, local)
        }
    }

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 160,
            panel_rows: 48,
        }
    }

    fn state() -> OverviewState {
        let mut needy = chip("a", 1);
        needy.mark = ChipMark::Stuck;
        needy.need = Some("frozen");
        let mut done = worker("c", 3, "t-a");
        done.mark = ChipMark::Done;
        done.task = Some("done");
        done.has_result = true;
        done.stoppable = false;
        OverviewState::new(
            crate::WindowId(0),
            vec![OverviewGroup {
                label: "Window 1".to_string(),
                chips: vec![needy, worker("b", 2, "t-a"), done],
            }],
            vec![Manages {
                manager: sid("a"),
                manager_local: Some(1),
                manager_label: "t-a".to_string(),
                workers: vec![(sid("b"), "t-b".to_string()), (sid("c"), "t-c".to_string())],
            }],
        )
    }

    /// The card's counts and words are the chips': sessions, needs, workers,
    #[test]
    fn the_card_counts_needs_and_workers_and_lists_who_manages_whom() {
        let s = state();
        let lines = s.controls_lines();
        assert!(
            lines[0].contains("sessions=3")
                && lines[0].contains("needs=1")
                && lines[0].contains("workers=2")
                && lines[0].contains("manages=1"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("sid=a")
                && l.contains("mark=stuck")
                && l.contains("need=frozen"))
        );
        assert!(lines.iter().any(|l| l.contains("sid=b")
            && l.contains("task_state=working")
            && l.contains("driver=worker")
            && l.contains("stoppable=1")));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("overview manages manager=a local=1 workers=b,c"))
        );
        let tray = overview_tray(&s, &geom(), Theme::default());
        let texts = tray
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Text { .. }))
            .count();
        assert!(texts > 1 + 1 + 3 + 1, "{texts}");
    }

    /// WHO TYPED rides a chip's line after its driver, and `controls` names
    #[test]
    fn a_chip_says_who_typed_after_its_driver() {
        let mut s = state();
        let typed = Some(("lab-sweep".to_string(), "4m ago".to_string()));
        s.groups[0].chips[1].typed = typed;
        let text = s.chip_text(1, &s.groups[0].chips[1]);
        assert!(
            text.ends_with("worker of t-a \u{b7} typed by lab-sweep 4m ago"),
            "{text}"
        );
        assert!(!s.chip_text(0, &s.groups[0].chips[0]).contains("typed"));
        let lines = s.controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("sid=b ") && l.contains(" typed_by=lab-sweep "))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("sid=a ") && l.contains(" typed_by=- "))
        );
    }

    /// AN ENTRANCE OPENS THE CARD AT A SESSION: a manager's lands on its
    #[test]
    fn select_session_lands_on_the_manages_line_or_the_chip() {
        let mut s = state();
        s.move_selection(1);
        assert!(s.select_session(1, true));
        assert_eq!(s.selected_item(), Some(OverviewItem::Manages { line: 0 }));
        assert_eq!(s.activate(), OverviewAct::Raise(1), "its manager");
        assert!(s.select_session(1, false));
        assert_eq!(s.selected_chip().map(|c| c.local_id), Some(1));
        assert!(s.select_session(2, true), "control: no manages line");
        assert_eq!(s.selected_chip().map(|c| c.local_id), Some(2));
        assert!(!s.select_session(9, false));
        assert_eq!(s.selected_chip().map(|c| c.local_id), Some(2), "unmoved");
    }

    /// The keys resolve to the acts the design names: ⏎ goes to the chip's
    #[test]
    fn the_keys_resolve_to_their_acts_and_stop_is_two_steps() {
        let mut s = state();
        assert_eq!(s.activate(), OverviewAct::Raise(1));
        assert_eq!(s.take_over_pressed(), OverviewAct::TakeOverOrHandBack(1));
        assert_eq!(s.read_result_pressed(), OverviewAct::None, "no result on a");
        assert_eq!(s.stop_pressed(false), OverviewAct::None, "a is no worker");
        s.move_selection(1);
        assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
        assert!(s.controls_lines()[0].contains("confirm=1 confirm_by=person"));
        assert_eq!(s.stop_pressed(false), OverviewAct::StopWorker(2));
        assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
        assert_eq!(s.activate(), OverviewAct::Raise(2));
        assert!(s.controls_lines()[0].contains("confirm=-"));
        assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
        s.move_selection(1);
        assert!(s.controls_lines()[0].contains("confirm=-"));
        assert_eq!(s.read_result_pressed(), OverviewAct::ReadResult(3));
        assert_eq!(s.stop_pressed(false), OverviewAct::None);
        s.move_selection(1);
        assert_eq!(s.activate(), OverviewAct::Raise(1));
        assert_eq!(s.take_over_pressed(), OverviewAct::None);
        s.move_selection(-2);
        assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
        assert!(!s.escape());
        assert!(s.escape());
    }

    /// ONE HAND'S ⌫ NEVER RESOLVES THE OTHER'S CONFIRM: a control client's ⌫
    #[test]
    fn a_confirm_is_resolved_only_by_the_hand_that_armed_it() {
        let mut s = state();
        s.move_selection(1);
        assert_eq!(s.stop_pressed(true), OverviewAct::Armed(2));
        assert!(s.controls_lines()[0].contains("confirm_by=controller"));
        let by_controller = s.fingerprint();
        assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2), "re-armed");
        assert!(s.controls_lines()[0].contains("confirm_by=person"));
        assert_ne!(
            s.fingerprint(),
            by_controller,
            "a re-arm by the other hand repaints its words"
        );
        assert_eq!(s.stop_pressed(true), OverviewAct::Armed(2), "re-armed");
        assert_eq!(s.stop_pressed(true), OverviewAct::StopWorker(2));
        s.move_selection(-1);
        assert!(s.arm_stop_on(2, false));
        assert!(
            s.controls_lines()
                .iter()
                .any(|l| l.contains("sid=b") && l.contains("selected=true confirm=true"))
        );
        assert_eq!(s.stop_pressed(false), OverviewAct::StopWorker(2));
        assert!(!s.arm_stop_on(3, false), "c's task is done");
        assert!(!s.arm_stop_on(42, false), "no such chip");
        assert!(s.controls_lines()[0].contains("confirm=-"));
    }

    /// A WORKER THE PERSON HOLDS IS STILL A WORKER, AND `t` HANDS IT BACK.
    #[test]
    fn a_held_worker_still_counts_and_t_hands_it_back() {
        let mut s = state();
        s.move_selection(1);
        assert!(!s.t_hands_back(), "b is not held");
        let mut groups = state().groups;
        groups[0].chips[1].driver = Some(Driver::You);
        groups[0].chips[1].held_by_person = true;
        groups[0].chips[1].mark = ChipMark::Yours;
        assert!(s.retarget(groups, state().manages));
        assert!(s.t_hands_back(), "b is the person's");
        assert!(
            s.controls_lines()[0].contains("workers=2"),
            "{:?}",
            s.controls_lines()[0]
        );
    }

    /// A refresh keeps the selection and the armed confirm by identity, and
    #[test]
    fn retarget_keeps_selection_identity_and_drops_a_dead_confirm() {
        let mut s = state();
        s.move_selection(1);
        assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
        let mut groups = state().groups;
        groups[0].chips.insert(0, chip("z", 9));
        assert!(s.retarget(groups.clone(), state().manages));
        assert!(s.controls_lines().iter().any(|l| l.contains("sid=b")
            && l.contains("selected=true")
            && l.contains("confirm=true")));
        assert!(
            !s.retarget(groups.clone(), state().manages),
            "same words: no change"
        );
        groups[0].chips[2].stoppable = false;
        assert!(s.retarget(groups, state().manages));
        assert!(s.controls_lines()[0].contains("confirm=-"));
    }

    /// WHY LAND HOLDS IS A LINE OF ITS OWN UNDER ITS CHIP (Autopilot v5
    #[test]
    fn a_held_chip_says_why_on_a_line_of_its_own_cut_with_an_ellipsis() {
        let held = "not landed yet: needs you; 3 changed files, 1 untracked file, 1 stash, 4 \
                    commits not on the remote main; open work in its report: follow-up, not \
                    pushed, local, a part; no \u{201c}Nothing left:\u{201d} defense yet; a reply \
                    too tall to read whole \u{b7} 3 nudges";
        let mut landing = chip("h", 4);
        landing.label = "docs pass".to_string();
        landing.mark = ChipMark::Working;
        landing.task = Some("working");
        landing.task_words = Some("task working 2m".to_string());
        landing.driver = Some(Driver::Aterm("land".to_string()));
        landing.typed = Some(("aterm".to_string(), "2m ago".to_string()));
        landing.held = Some(held.to_string());
        let mut s = OverviewState::new(
            crate::WindowId(0),
            vec![OverviewGroup {
                label: "Window 1".to_string(),
                chips: vec![chip("a", 1), landing, chip("z", 5)],
            }],
            Vec::new(),
        );
        let g = geom();
        let layout = overview_layout(&s, &g);
        assert_eq!(layout.card.2, MAX_CARD_WIDTH, "the card at its widest");
        assert_eq!(s.lines().len(), 5);
        assert_eq!(s.wanted_rows(), CHROME_ROWS + 5);
        let texts = |s: &OverviewState| -> Vec<(f32, String)> {
            overview_tray(s, &g, Theme::default())
                .prims
                .into_iter()
                .filter_map(|p| match p {
                    DrawPrim::Text { x, s, .. } => Some((x, s)),
                    _ => None,
                })
                .collect()
        };
        let painted = texts(&s);
        let row = painted
            .iter()
            .find(|(_, t)| t.contains("docs pass"))
            .expect("the chip's line");
        assert!(
            row.1
                .ends_with("driven by aterm (land) \u{b7} typed by aterm 2m ago"),
            "the chip's own words whole: {}",
            row.1
        );
        let under = painted
            .iter()
            .find(|(_, t)| t.starts_with("not landed yet: "))
            .expect("the held line");
        assert!(under.0 > row.0, "past the chip's mark");
        assert!(
            under.1.ends_with('\u{2026}') && held.starts_with(under.1.trim_end_matches('\u{2026}')),
            "cut with `…`: {}",
            under.1
        );
        let px = TypeStep::Body.px(g.font_px).get();
        let narrow = SettingsGeom {
            cw: crate::settings::text_w("0", px) * 0.9,
            ..g
        };
        for g in [g, narrow] {
            let card = overview_layout(&s, &g).card;
            let ring_right = card.0 + card.2 - g.cw;
            let painted = overview_tray(&s, &g, Theme::default()).prims;
            let body: Vec<(f32, String)> = painted
                .into_iter()
                .filter_map(|p| match p {
                    DrawPrim::Text { x, s, .. } if !s.contains("esc close") => Some((x, s)),
                    _ => None,
                })
                .collect();
            assert!(
                body.iter()
                    .any(|(_, t)| t.starts_with("not landed yet: ") && t.ends_with('\u{2026}')),
                "the held line cut: {body:?}"
            );
            for (x, t) in &body {
                let right = x + crate::settings::text_w(t, px);
                assert!(
                    right <= ring_right + 0.01,
                    "{t:?} ends at {right} past the ring's {ring_right} (cw {})",
                    g.cw
                );
            }
        }
        let (x, y, w, h) = overview_row_rect_in(&layout, &g, 3).expect("its slot");
        assert_eq!(overview_item_hit(&s, &g, x + w * 0.5, y + h * 0.5), Some(1));
        assert!(
            s.controls_lines()
                .iter()
                .any(|l| l.contains("sid=h") && l.ends_with(" held=1")),
            "{:?}",
            s.controls_lines()
        );
        s.select(1);
        let ring = overview_tray(&s, &g, Theme::default())
            .prims
            .into_iter()
            .find_map(|p| match p {
                DrawPrim::Stroke { x, h, .. } if x == layout.card.0 + g.cw => Some(h),
                _ => None,
            })
            .expect("the ring");
        assert!(ring > g.ch * 1.5, "two rows tall: {ring}");
    }

    /// Painter and hit-test share one row rectangle; the fingerprint moves
    #[test]
    fn hit_test_paint_and_fingerprint_share_one_state() {
        let mut s = state();
        let g = geom();
        let layout = overview_layout(&s, &g);
        let (x, y, w, h) = overview_row_rect_in(&layout, &g, 1).unwrap();
        assert_eq!(overview_item_hit(&s, &g, x + w * 0.5, y + h * 0.5), Some(0));
        let (hx, hy, hw, hh) = overview_row_rect_in(&layout, &g, 0).unwrap();
        assert_eq!(
            overview_item_hit(&s, &g, hx + hw * 0.5, hy + hh * 0.5),
            None
        );
        let tray = overview_tray(&s, &g, Theme::default());
        assert_eq!(tray.card, layout.card);
        let before = s.fingerprint();
        assert_ne!(before, 0);
        let mut groups = state().groups;
        groups[0].chips[1].doing = "busy 4m".to_string();
        assert!(s.retarget(groups, state().manages));
        assert_ne!(s.fingerprint(), before);
    }
}
