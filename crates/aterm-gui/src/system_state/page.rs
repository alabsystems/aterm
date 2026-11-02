// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One rendering of the setup screen (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.1): the
//! hero (the lockup as the run's progress, the phase headline and the sub-line), the pinned
//! Needs-you card while something needs the person, one inset grouped list of five lines, and
//! the footer while nothing runs. Collapsed, it fits a 900×644 window with no scrolling; a line
//! the person opens shows its rows inside the list, which scrolls where they do not fit, and the
//! page itself never scrolls. The layout is regular where the page's content fits the viewport
//! at the regular measures, and compact otherwise (§4.1's breakpoint is on content height).

use std::sync::Arc;

use aterm_effects::pet_glyphs_gen::PetGlyphId;

use super::lines::{self, CardItem, Hero, Line, LineView, Mark};
use super::lockup::LockupSpec;
use super::marks::{self, Ground};
use super::{
    Chip, FADE_STRIP, PHRASE_FIELD, PHRASE_JOIN, QUEUED_RESTORE_NOTE, RESTORE_FINISHED,
    RESTORE_WAITS, RowValue, RowView, SetupOperation, SetupStatus, Span, Subject, SystemStateApp,
    SystemStateViewState, row_slug, rows, rows_as_they_stand, whole_at,
};
use crate::native_app::ViewCx;
use crate::native_text_input::TextInputProjection;
use crate::native_ui::{
    ActionId, AuditedCustomNode, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout,
    Length, NodeMotion, SemanticRole, SemanticValue, StyleRef, SwitchSpec, TextFieldSpec, TextSpec,
    UiContent, UiNode, UiTree,
};
use crate::system_state_host::PhraseOutcome;
use aterm_grapheme::GraphemeClusters as _;

/// The width of a line's switch: its track alone, the phrase beside it saying where it stands.
const SWITCH_W: f32 = 58.0;

/// The room a line's disclosure chevron takes before the switch.
const CHEVRON_W: f32 = 28.0;

/// The least room (at 1x text) a line's words take beside its switch; narrower, the switch goes
/// under them.
const STACK_BELOW: f32 = 72.0;

/// The gap between a mark's slot and its title, in the list and in the card alike, so their
/// titles line up.
const MARK_GAP: f32 = 12.0;

/// The page's vertical rhythm (§4.9): every top and height a multiple of it.
const RHYTHM: f32 = 4.0;

/// A hairline's seat: the rule itself is one point, the seat keeps the rhythm.
const RULE_H: f32 = RHYTHM;

/// `v` up to the rhythm's next step (a hundredth of a point's slack, so a measure already on it
/// stays).
fn on_grid(v: f32) -> f32 {
    ((v - 0.01) / RHYTHM).ceil().max(0.0) * RHYTHM
}

/// `v` down to the rhythm's step below it.
fn grid_below(v: f32) -> f32 {
    ((v + 0.01) / RHYTHM).floor().max(0.0) * RHYTHM
}

/// The page's measures at one viewport, text scale and density.
#[derive(Clone, Copy, Debug)]
pub(super) struct Metrics {
    pub(super) compact: bool,
    pad_x: f32,
    pad_y: f32,
    /// The content's width.
    pub(super) inner: f32,
    lockup_h: f32,
    headline_h: f32,
    sub_h: f32,
    /// The space between the hero, the card, the list and the footer.
    gap: f32,
    /// A collapsed line's height.
    pub(super) line_h: f32,
    /// An expanded row's height.
    row_h: f32,
    /// A wrapped Body line.
    text_line: f32,
    /// A wrapped Caption line (a note).
    note_line: f32,
    control_h: f32,
    /// The inset inside the list's and the card's surfaces.
    card_pad: f32,
    /// The card's inset above its first item and below its last.
    card_pad_y: f32,
    /// Where a line's title starts: past the mark's slot.
    title_x: f32,
    scale: f32,
}

impl Metrics {
    pub(super) fn new(width: f32, compact: bool) -> Self {
        let scale = crate::native_settings::settings_text_scale().max(1.0);
        let narrow = width < 480.0;
        let pad_x = if narrow {
            12.0
        } else if compact {
            16.0
        } else {
            24.0
        };
        // Every vertical measure on the rhythm, at any text scale.
        let s = |regular: f32, small: f32| on_grid(if compact { small } else { regular } * scale);
        Self {
            compact,
            pad_x,
            pad_y: s(16.0, 8.0),
            inner: (width - pad_x * 2.0).max(1.0),
            lockup_h: s(48.0, 40.0),
            headline_h: s(32.0, 28.0),
            sub_h: s(24.0, 20.0),
            gap: s(12.0, 8.0),
            line_h: s(44.0, 36.0),
            row_h: s(36.0, 28.0),
            text_line: on_grid((20.0 * scale).max(20.0)),
            note_line: on_grid((16.0 * scale).max(16.0)),
            control_h: s(28.0, 28.0),
            card_pad: if narrow { 10.0 } else { 14.0 },
            card_pad_y: RHYTHM * 2.0,
            title_x: marks::MARK_SLOT + MARK_GAP,
            scale,
        }
    }

    /// The width `words` paint at in the face and size `role` and `style` give them.
    fn words_w(role: SemanticRole, style: StyleRef, words: &str) -> f32 {
        let (px, face) = crate::native_ui::text_paint_metrics(role, style);
        face_width(face, words, px).ceil() + 1.0
    }

    /// A button's width for `words` (padding either side).
    fn button_w(words: &str) -> f32 {
        Self::words_w(SemanticRole::Text, StyleRef::Plain, words) + 28.0
    }

    /// The hero's height: §4.1's 120 [96].
    fn hero_h(&self) -> f32 {
        let least: f32 = if self.compact { 96.0 } else { 120.0 };
        least.max(
            self.lockup_h + self.headline_h + self.sub_h + if self.compact { 8.0 } else { 16.0 },
        )
    }
}

/// `text` set in a box `width` wide in the face and size `role` and `style` paint: whole words,
/// a word broken only where it cannot fit a line ([`crate::native_settings::wrap_to_box`]).
pub(super) fn lines_in(text: &str, role: SemanticRole, style: StyleRef, width: f32) -> Vec<String> {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    let measure = |line: &str| face_width(face, line, px);
    crate::native_settings::wrap_to_box(text, width.max(1.0), &measure)
}

/// How wide `words` paint in `face` at `px`: the mono face (a command) by its own advances, as
/// the painter sets it, the UI faces proportionally.
fn face_width(face: crate::widget::TextFace, words: &str, px: f32) -> f32 {
    if face == crate::widget::TextFace::Mono {
        crate::tray_raster::measure_text(words, px, crate::widget::TextWeight::Regular)
    } else {
        crate::tray_raster::ui_text_width_for(face, words, px)
    }
}

/// `text` as a node `width` wide on its [`lines_in`], `line_h` apart, with its height: one line
/// is the text node `key`, more are `key/line-N` under a group `key` that names it whole.
pub(super) fn words_node(
    key: &str,
    text: &str,
    role: SemanticRole,
    style: StyleRef,
    width: f32,
    line_h: f32,
) -> (UiNode, f32) {
    let line = |key: String, text: String| {
        UiNode::new(key, UiContent::Text(TextSpec { text, role, style })).layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(line_h)),
        )
    };
    let lines = lines_in(text, role, style, width);
    if lines.len() <= 1 {
        return (line(key.to_owned(), text.to_owned()), line_h);
    }
    let height = line_h * lines.len() as f32;
    let children = lines
        .into_iter()
        .enumerate()
        .map(|(index, words)| line(format!("{key}/line-{index}"), words).paint_only())
        .collect();
    (
        UiNode::new(
            key.to_owned(),
            UiContent::Group(GroupSpec {
                label: Some(text.to_owned()),
                role,
                style: StyleRef::Plain,
            }),
        )
        .layout(
            Layout::column()
                .gap(0.0)
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .children(children),
        height,
    )
}

/// A group with no words of its own.
fn group(key: impl Into<Arc<str>>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
}

/// A spacer that takes what is left of a row.
fn spacer(key: impl Into<Arc<str>>) -> UiNode {
    group(key)
        .layout(Layout::default().width(Length::Fill))
        .paint_only()
}

/// A paint-only custom node.
fn painted(key: impl Into<Arc<str>>, audit_id: &'static str, paint: String) -> UiNode {
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id,
            role: SemanticRole::Group,
            label: String::new(),
            paint: Some(paint),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: None,
        }),
    )
    .paint_only()
}

/// A painted node that moves as `motion` says.
fn moving(mut node: UiNode, motion: Option<NodeMotion>) -> UiNode {
    if let UiContent::Custom(custom) = &mut node.content {
        custom.motion = motion;
    }
    node
}

/// A state's mark, in a slot `slot` square, centred down a band `band` tall, moving as `motion`
/// says where it moves (the working spinner, [`Page::spins`]).
fn mark_node(
    key: &str,
    (mark, ground): (Mark, Ground),
    motion: Option<NodeMotion>,
    slot: f32,
    band: f32,
) -> UiNode {
    let top = ((band - slot) / 2.0).max(0.0);
    group(format!("{key}/seat"))
        .layout(
            Layout::column()
                .width(Length::Fixed(slot))
                .height(Length::Fixed(band))
                .padding(Insets {
                    top,
                    ..Insets::default()
                }),
        )
        .children(vec![
            moving(
                painted(
                    format!("{key}/mark"),
                    crate::native_ui::SYSTEM_STATE_MARK_AUDIT,
                    marks::mark_value(mark, ground),
                ),
                motion,
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(slot))
                    .height(Length::Fixed(slot)),
            ),
        ])
        .paint_only_tree()
}

/// Centre `node` (of `height`) down a band `band` tall.
fn centred(key: String, node: UiNode, width: Length, height: f32, band: f32) -> UiNode {
    let top = ((band - height) / 2.0).max(0.0);
    group(key)
        .layout(
            Layout::column()
                .width(width)
                .height(Length::Fixed(band))
                .padding(Insets {
                    top,
                    ..Insets::default()
                }),
        )
        .children(vec![node])
}

/// A key the list scrolls to, where its span starts in the list's content, and its height.
type Reveal = (String, f32, f32);

/// A piece of the list: its node, its height and the keys it reveals.
type ListPiece = (UiNode, f32, Vec<Reveal>);

/// One rendering of the page.
pub(super) struct Page<'a, 'b> {
    app: &'a SystemStateApp,
    view: &'a SystemStateViewState,
    cx: &'a ViewCx<'b>,
    m: Metrics,
    card: Vec<CardItem>,
    lines: Vec<LineView>,
    hero: Hero,
    /// Revealable keys and where they sit in the list's content.
    spans: Vec<Span>,
    /// The list scrolls inside its room.
    scrolls: bool,
    /// A window too short for the hero's lockup beside the card, the footer and one line: the
    /// sub-line's count carries the progress, and the lockup is left out.
    tight: bool,
    /// A window too short even for that: the footer rides at the list's end, scrolled to as a
    /// line is, so the card stays whole above it.
    footer_in_list: bool,
    /// A window too short for the card itself (several things need the person at once): the
    /// card rides at the list's top, where the list opens, so none of its items is cut.
    card_in_list: bool,
}

/// Whether `node`, or a node under it, has `key`: the footer spans only the controls it holds.
fn holds(node: &UiNode, key: &str) -> bool {
    node.key.as_str() == key || node.children.iter().any(|child| holds(child, key))
}

/// The rows the page reads: until the page knows the plan in force it cannot say which rows aterm
/// manages; a newer aterm's setup names none it can know, so its rows read as they stand.
pub(super) fn page_rows(app: &SystemStateApp) -> Vec<RowView> {
    if app.plan_known {
        app.reviewed_rows()
    } else if app.status.newer() {
        rows_as_they_stand(&app.status)
    } else {
        rows(&SetupStatus::default(), super::Selection::all())
    }
}

impl<'a, 'b> Page<'a, 'b> {
    pub(super) fn new(
        app: &'a SystemStateApp,
        view: &'a SystemStateViewState,
        cx: &'a ViewCx<'b>,
    ) -> Self {
        let rows = page_rows(app);
        let card = lines::card_items(app, &rows);
        let lines = Line::ALL
            .into_iter()
            .map(|line| lines::line_view(app, line, &rows, &card))
            .collect();
        let hero = lines::hero(app, &rows, &card);
        Self {
            m: Metrics::new(cx.viewport.width.max(64.0), false),
            app,
            view,
            cx,
            card,
            lines,
            hero,
            spans: Vec::new(),
            scrolls: false,
            tight: false,
            footer_in_list: false,
            card_in_list: false,
        }
    }

    /// The tree at regular measures where the page's content fits the viewport, else compact.
    pub(super) fn tree(mut self) -> UiTree {
        let height = self.cx.viewport.height.max(120.0);
        let width = self.cx.viewport.width.max(64.0);
        self.m = Metrics::new(width, false);
        if self.content_height() > height {
            self.m = Metrics::new(width, true);
            // Compact and still too short for the hero, the card, the footer and one line: the
            // lockup goes before any control leaves the window, then the footer joins the
            // list's end, and then the card its top.
            let least = self.least_list();
            self.tight = self.fixed_height() + least > height;
            self.footer_in_list = self.tight && self.fixed_height() + least > height;
            self.card_in_list = self.footer_in_list && self.fixed_height() + least > height;
        }
        self.build(height)
    }

    /// The least the list is given: one line whole above the strip a scrolling list fades out in.
    fn least_list(&self) -> f32 {
        on_grid(self.m.line_h + FADE_STRIP.min(self.m.line_h / 3.0))
    }

    /// The page's height without its list.
    fn fixed_height(&mut self) -> f32 {
        let m = self.m;
        let hero = self.hero_node().1;
        let notes = self.notes().map_or(0.0, |(_, h)| h + m.gap);
        let card = if self.card_in_list {
            0.0
        } else {
            self.card_node(None).map_or(0.0, |(_, h)| h + m.gap)
        };
        let footer = if self.footer_in_list {
            0.0
        } else {
            self.footer(m.inner).map_or(0.0, |(_, h)| h + m.gap)
        };
        self.spans.clear();
        m.pad_y * 2.0 + hero + m.gap + notes + card + footer
    }

    /// The page's height with every line at its own height, the list unscrolled.
    fn content_height(&mut self) -> f32 {
        let list = self.list_content(0.0).1;
        self.fixed_height() + list
    }

    fn build(mut self, height: f32) -> UiTree {
        let m = self.m;
        let (hero, hero_h) = self.hero_node();
        let notes = self.notes();
        let card = if self.card_in_list {
            None
        } else {
            self.card_node(None)
        };
        let footer = if self.footer_in_list {
            None
        } else {
            self.footer(m.inner)
        };
        let notes_h = notes.as_ref().map_or(0.0, |(_, h)| h + m.gap);
        let card_h = card.as_ref().map_or(0.0, |(_, h)| h + m.gap);
        let footer_h = footer.as_ref().map_or(0.0, |(_, h)| h + m.gap);
        // The list takes what is left, never less than one line: it scrolls inside, the page
        // never does.
        let room =
            grid_below(height - m.pad_y * 2.0 - hero_h - m.gap - notes_h - card_h - footer_h)
                .max(self.least_list());
        let list = self.list(room);
        let mut children = vec![hero, gap("system-state/hero-gap", m.gap)];
        if let Some((notes, _)) = notes {
            children.push(notes);
            children.push(gap("system-state/notes-gap", m.gap));
        }
        if let Some((card, _)) = card {
            children.push(card);
            children.push(gap("system-state/card-gap", m.gap));
        }
        children.push(list);
        if let Some((footer, _)) = footer {
            children.push(gap("system-state/footer-gap", m.gap));
            children.push(footer);
        }
        *self.view.spans.borrow_mut() = std::mem::take(&mut self.spans);
        UiTree::new(
            UiNode::new(
                "system-state/app",
                // A grouped form: in light mode its cards rise over a page that sinks (§4.4).
                UiContent::Group(
                    GroupSpec::unlabeled(SemanticRole::Application).style(StyleRef::Secondary),
                ),
            )
            .layout(
                Layout::column()
                    .padding(Insets::symmetric(m.pad_x, m.pad_y))
                    .gap(0.0)
                    .clipped(),
            )
            .children(children),
        )
    }

    /// How a mark moves: the working spinner steps at 12 Hz where the page may move in place
    /// (Reduce Motion keeps it: rotation in place is not travel), and stands still as its still
    /// glyph under Serious Mode, the shed, Low Power or in an unfocused window
    /// (`docs/DESIGN-init-on-by-default-2026-10-25.md` §5.5). No other mark moves.
    fn spins(&self, mark: Mark) -> Option<NodeMotion> {
        (mark == Mark::Working
            && self
                .cx
                .motion
                .page()
                .animate(crate::motion::MotionEffect::NativeInPlace))
        .then(|| NodeMotion::endless(0, aterm_messages::motion::Cadence::SPINNER))
    }

    /// Whether the kitty and the ribbon show: not in Serious Mode, not under High Contrast.
    fn kitty_shows(&self) -> bool {
        !self.cx.motion.serious && !marks::Looks::now().high_contrast
    }

    /// The pose of the moment (§4.5's table; SCR-2 jumps to it, SCR-3 glides).
    fn wanted_pose(&self) -> PetGlyphId {
        let hero = &self.hero;
        if self.card.iter().any(|item| item.mark == Mark::NeedsYou) {
            PetGlyphId::PetSitLookup
        } else if self.card.iter().any(|item| item.mark == Mark::Failed) {
            PetGlyphId::PetSitEar
        } else if hero.total > 0 && hero.done == hero.total && !self.app.status.busy {
            PetGlyphId::PetLoaf
        } else if self.app.status.running() {
            PetGlyphId::PetStand
        } else {
            PetGlyphId::PetSit
        }
    }

    /// The lockup (§4.5): the page's progress indicator.
    fn lockup(&self) -> UiNode {
        let m = &self.m;
        let kitty = self.kitty_shows();
        let scale = self.cx.terminal_scale.max(1.0);
        let wanted = self.wanted_pose();
        let mut spec = LockupSpec {
            kitty,
            px: if m.compact { 17.0 } else { 22.0 },
            scale,
            alab: lines::alab_lockup(&self.app.status, self.app.plan.selection),
            done: u32::try_from(self.hero.done).unwrap_or(u32::MAX),
            total: u32::try_from(self.hero.total).unwrap_or(u32::MAX),
            pose: wanted,
        };
        // A narrow page sets the whole lockup smaller rather than cut it.
        while spec.least_width() > m.inner && spec.px > 8.0 {
            spec.px -= 1.0;
        }
        if kitty {
            let h = spec.kitty_height();
            super::kitty::prewarm(wanted, h, scale);
            spec.pose = super::kitty::pose_to_draw(wanted, self.view.kitty_pose.get(), h, scale);
            self.view.kitty_pose.set(Some(spec.pose));
        }
        let (done, total) = (spec.done, spec.total);
        UiNode::new(
            "system-state/progress",
            UiContent::Custom(AuditedCustomNode {
                audit_id: crate::native_ui::SYSTEM_STATE_BRAND_AUDIT,
                role: SemanticRole::ProgressIndicator,
                label: "Setup progress".to_owned(),
                paint: Some(spec.value()),
                action: None,
                focusable: false,
                focused: false,
                focus_visible: false,
                // Before anything was read there is no progress to claim, never `0 of 0`.
                progress: (total > 0).then_some((done, total)),
                motion: None,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(m.lockup_h.max(on_grid(spec.needed_height())))),
        )
    }

    /// A text button: quiet words, as Stop is in the hero.
    fn button(
        key: &str,
        label: String,
        visual: Option<&str>,
        style: StyleRef,
        enabled: bool,
        h: f32,
    ) -> UiNode {
        let width = Metrics::button_w(visual.unwrap_or(&label));
        let mut spec = ButtonSpec::new(label);
        spec.visual_label = visual.map(str::to_owned);
        UiNode::new(
            key.to_owned(),
            UiContent::Button(
                Control::new(spec, ActionId::new(key.to_owned()))
                    .state(ControlState {
                        enabled,
                        ..ControlState::default()
                    })
                    .style(style),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(h)),
        )
    }

    /// [`Self::button`] held to `room`: its face as it is where it fits, else wrapped onto the
    /// lines it needs, else `short` where that fits one line. With its width and height.
    fn fitted_button(
        key: &str,
        label: String,
        short: Option<&str>,
        style: StyleRef,
        enabled: bool,
        h: f32,
        room: f32,
    ) -> (UiNode, f32, f32) {
        let width = Metrics::button_w(&label);
        if width <= room {
            return (Self::button(key, label, None, style, enabled, h), width, h);
        }
        let face = label.clone();
        let mut node = Self::button(key, label, Some(&face), style, enabled, h);
        let mut height = h;
        if let UiContent::Button(control) = &mut node.content {
            let words_room = crate::native_ui::text_button_label_room(&control.spec, room);
            match crate::native_ui::native_button_label_lines(&face, words_room) {
                Some(lines) if lines.len() > 1 => {
                    height = on_grid(crate::native_ui::native_button_height_for_lines(
                        lines.len(),
                        h,
                    ));
                    control.spec.visual_label = Some(lines.join("\n"));
                }
                None if short.is_some_and(|short| Metrics::button_w(short) <= room) => {
                    control.spec.visual_label = short.map(str::to_owned);
                }
                _ => {}
            }
        }
        (
            node.layout(
                Layout::default()
                    .width(Length::Fixed(room))
                    .height(Length::Fixed(height)),
            ),
            room,
            height,
        )
    }

    /// The hero (§4.1): lockup, headline and sub-line, with Stop beside the sub-line while a run
    /// goes. 120 [96] tall; a headline or sub-line the width cannot hold on one line takes the
    /// lines it needs.
    fn hero_node(&self) -> (UiNode, f32) {
        let m = &self.m;
        let hero = &self.hero;
        let lockup = (!self.tight).then(|| self.lockup());
        let lockup_h = match lockup.as_ref().map(|lockup| lockup.layout.height) {
            Some(Length::Fixed(h)) => h,
            Some(_) => m.lockup_h,
            None => 0.0,
        };
        // Display 24 [Title 20]; a headline wider than the page steps down to Title, then wraps.
        let mut style = if m.compact {
            StyleRef::Primary
        } else {
            StyleRef::Hero
        };
        if style == StyleRef::Hero
            && Metrics::words_w(SemanticRole::Heading, style, &hero.headline) > m.inner
        {
            style = StyleRef::Primary;
        }
        let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, style);
        let line_h = m.headline_h.max(on_grid(px * 1.3));
        let (headline, headline_h) = words_node(
            "system-state/headline",
            &hero.headline,
            SemanticRole::Heading,
            style,
            m.inner,
            line_h,
        );
        // The live region: the phase alone, so a count that moves is never announced (§4.7).
        let status = UiNode::new(
            "system-state/phase",
            UiContent::Group(GroupSpec {
                label: Some(hero.phase.clone()),
                role: SemanticRole::Status,
                style: StyleRef::Plain,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        let headline = group("system-state/headline-seat")
            .layout(Layout::default().height(Length::Fixed(headline_h)))
            .children(vec![headline, status]);
        let (sub, sub_h) = self.sub_row();
        let height = if self.tight {
            headline_h + sub_h + 4.0
        } else {
            m.hero_h()
                .max(lockup_h + headline_h + sub_h + if m.compact { 8.0 } else { 16.0 })
        };
        let lead = (height - lockup_h - headline_h - sub_h).max(0.0);
        let column = || {
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fixed(height))
                .padding(Insets {
                    top: grid_below(lead * 0.25),
                    ..Insets::default()
                })
                .gap(0.0)
        };
        // The headline is the page's H1 and the first thing VoiceOver reads (§4.6, §4.7), the
        // lockup's progress after it: the words and the lockup are two layers of the hero, the
        // lockup's drawn above the words where it stands.
        let words = group("system-state/hero/words")
            .layout(column())
            .children(vec![
                gap("system-state/hero/lockup-room", lockup_h),
                gap("system-state/hero/lead", grid_below(lead * 0.5)),
                headline,
                sub,
            ]);
        let mut layers = vec![words];
        if let Some(lockup) = lockup {
            layers.push(
                group("system-state/hero/lockup")
                    .layout(column())
                    .children(vec![lockup, spacer("system-state/hero/lockup-space")]),
            );
        }
        (
            group("system-state/hero")
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fixed(height)),
                )
                .children(layers),
            height,
        )
    }

    /// The sub-line and its quiet button, side by side where they fit, else the button under it.
    fn sub_row(&self) -> (UiNode, f32) {
        let m = &self.m;
        let hero = &self.hero;
        let mut buttons: Vec<UiNode> = Vec::new();
        if hero.stop {
            buttons.push(Self::button(
                "system-state/stop",
                "Stop".to_owned(),
                None,
                StyleRef::Quiet,
                !self.app.stopping,
                m.control_h,
            ));
        }
        let buttons_w: f32 = buttons
            .iter()
            .map(|button| match button.layout.width {
                Length::Fixed(w) => w + 8.0,
                _ => 0.0,
            })
            .sum();
        let words_w = Metrics::words_w(SemanticRole::Text, StyleRef::Secondary, &hero.sub);
        let line = m.sub_h;
        if words_w + buttons_w <= m.inner {
            let mut children = vec![
                UiNode::new(
                    "system-state/sub",
                    UiContent::Text(TextSpec {
                        text: hero.sub.clone(),
                        role: SemanticRole::Text,
                        style: StyleRef::Secondary,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            ];
            children.extend(buttons.into_iter().map(|button| {
                let height = m.control_h;
                centred(
                    format!("{}/seat", button.key.as_str()),
                    button.layout(
                        Layout::default()
                            .width(Length::Fill)
                            .height(Length::Fixed(height)),
                    ),
                    Length::Fixed(buttons_w - 8.0),
                    height,
                    line.max(height),
                )
            }));
            let height = line.max(if buttons_w > 0.0 { m.control_h } else { 0.0 });
            return (
                group("system-state/sub-row")
                    .layout(Layout::row().gap(8.0).height(Length::Fixed(height)))
                    .children(children),
                height,
            );
        }
        let (words, words_h) = words_node(
            "system-state/sub",
            &hero.sub,
            SemanticRole::Text,
            StyleRef::Secondary,
            m.inner,
            m.text_line,
        );
        let mut height = words_h;
        let mut children = vec![words];
        if !buttons.is_empty() {
            let mut row = buttons;
            row.push(spacer("system-state/sub-row/space"));
            children.push(
                group("system-state/sub-row/buttons")
                    .layout(Layout::row().gap(8.0).height(Length::Fixed(m.control_h)))
                    .children(row),
            );
            height += 4.0 + m.control_h;
        }
        (
            group("system-state/sub-row")
                .layout(Layout::column().gap(4.0).height(Length::Fixed(height)))
                .children(children),
            height,
        )
    }

    /// The calm lines an observation cannot carry (the notes rule, [`SystemStateApp::notes`]):
    /// a result someone should know, a choice that waits for another setup, a plan aterm set up
    /// afresh or widened, a setting back only at the next login. Every line of each: a note cut
    /// short would leave a sentence half said.
    fn notes(&self) -> Option<(UiNode, f32)> {
        let m = &self.m;
        let notes = self.app.notes(&self.app.reviewed_plan());
        if notes.is_empty() {
            return None;
        }
        let mut height = 0.0;
        let children: Vec<UiNode> = notes
            .iter()
            .enumerate()
            .map(|(index, note)| {
                let (node, h) = words_node(
                    &format!("system-state/note/{index}"),
                    note,
                    SemanticRole::Text,
                    StyleRef::Secondary,
                    m.inner,
                    m.text_line,
                );
                height += h + if index > 0 { 4.0 } else { 0.0 };
                node
            })
            .collect();
        Some((
            group("system-state/notes")
                .layout(
                    Layout::column()
                        .gap(4.0)
                        .width(Length::Fill)
                        .height(Length::Fixed(height)),
                )
                .children(children),
            height,
        ))
    }

    /// The pinned Needs-you card (§4.8): a Group VoiceOver names "Needs you", one item per thing
    /// the person does, each with its reason and its one button.
    /// The card (§4.8); at `list_top` in the list's content, its items and their controls listed
    /// there so scrolling and focus reach them.
    fn card_node(&mut self, list_top: Option<f32>) -> Option<(UiNode, f32)> {
        if self.card.is_empty() {
            return None;
        }
        let m = self.m;
        let ground = if self.card.iter().all(|item| item.mark == Mark::Failed) {
            Ground::Failed
        } else {
            Ground::Needs
        };
        let room = m.inner - m.card_pad * 2.0;
        let mut children = Vec::new();
        let mut height = 0.0;
        let items = self.card.clone();
        // One default action (§4.8): the first button the card holds; any after it is quiet.
        let default = items.iter().position(|item| item.button.is_some());
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                children.push(rule(format!("system-state/card/{}/rule", item.id)));
                height += RULE_H;
            }
            let (node, h) = self.card_item(item, ground, room, default == Some(index));
            if let Some(top) = list_top {
                let at = top + m.card_pad_y + height;
                let mut keys = vec![format!("system-state/card/{}", item.id)];
                keys.extend(item.button.as_ref().map(|button| button.key.clone()));
                if item.phrase {
                    keys.extend([
                        super::PHRASE_FIELD.to_owned(),
                        super::PHRASE_JOIN.to_owned(),
                    ]);
                }
                if item.vault {
                    keys.extend(
                        [
                            "system-state/url",
                            "system-state/password",
                            "system-state/vault/join",
                        ]
                        .map(str::to_owned),
                    );
                }
                for key in keys {
                    self.span(key, at, h);
                }
            }
            children.push(node);
            height += h;
        }
        let height = height + m.card_pad_y * 2.0;
        Some((
            UiNode::new(
                "system-state/card",
                UiContent::Group(GroupSpec {
                    label: Some("Needs you".to_owned()),
                    role: SemanticRole::Group,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(vec![
                painted(
                    "system-state/card/surface",
                    crate::native_ui::SYSTEM_STATE_SURFACE_AUDIT,
                    marks::surface_value(ground),
                ),
                group("system-state/card/items")
                    .layout(Layout::column().padding(Insets {
                        top: m.card_pad_y,
                        right: m.card_pad,
                        bottom: m.card_pad_y,
                        left: m.card_pad,
                    }))
                    .children(children),
            ]),
            height,
        ))
    }

    /// One card item: its mark, what to do and its step, its button (the card's default where
    /// `default`), its reason, what the walk reaches next, and the phrase or vault fields it
    /// holds.
    fn card_item(
        &self,
        item: &CardItem,
        ground: Ground,
        room: f32,
        default: bool,
    ) -> (UiNode, f32) {
        let face_style = if default {
            StyleRef::Accent
        } else {
            StyleRef::Secondary
        };
        let m = &self.m;
        let key = format!("system-state/card/{}", item.id);
        let band = m.line_h.min(44.0 * m.scale);
        let words_room = (room - m.title_x).max(1.0);
        // What goes under the title (the step, the button, the reason, the fields) sits under
        // the title's words, or, in a card too narrow to spare that indent, under the mark.
        let indent = if words_room < STACK_BELOW * m.scale {
            0.0
        } else {
            m.title_x
        };
        let below_room = (room - indent).max(1.0);
        let button_w = item
            .button
            .as_ref()
            .map_or(0.0, |button| Metrics::button_w(&button.face) + 12.0);
        // Its one button: its face where it fits, else wrapped to the room under the title.
        let button = item.button.as_ref().map(|button| {
            let button_key = &button.key;
            let label = &button.label;
            let face = &button.face;
            let (mut node, w, h) = if Metrics::button_w(face) <= below_room {
                let visual = (face != label).then_some(face.as_str());
                let w = Metrics::button_w(face);
                (
                    Self::button(
                        button_key,
                        label.clone(),
                        visual,
                        face_style,
                        true,
                        m.control_h,
                    ),
                    w,
                    m.control_h,
                )
            } else {
                let (mut node, w, h) = Self::fitted_button(
                    button_key,
                    face.clone(),
                    None,
                    face_style,
                    true,
                    m.control_h,
                    below_room,
                );
                // VoiceOver hears the whole label; the face is what fits.
                if let UiContent::Button(control) = &mut node.content {
                    control.spec.visual_label = Some(
                        control
                            .spec
                            .visual_label
                            .clone()
                            .unwrap_or_else(|| face.clone()),
                    );
                    control.spec.label = label.clone();
                }
                (node, w, h)
            };
            // The reason is its own line, read next: the button carries none, so it is heard once.
            if let UiContent::Button(control) = &mut node.content {
                control.action = ActionId::new(button.action.clone());
            }
            node = node.layout(
                Layout::default()
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(h)),
            );
            (node, h)
        });
        let step_w = item.step.as_deref().map_or(0.0, |step| {
            Metrics::words_w(SemanticRole::Text, StyleRef::Secondary, step) + 12.0
        });
        let title_w = Metrics::words_w(SemanticRole::Heading, StyleRef::Plain, &item.title);
        let one_line = title_w + step_w + button_w <= words_room;
        let mut head = vec![mark_node(
            &key,
            (item.mark, ground),
            self.spins(item.mark),
            marks::MARK_SLOT,
            band,
        )];
        let mut height;
        let mut rest: Vec<UiNode> = Vec::new();
        if one_line {
            head.push(
                UiNode::new(
                    format!("{key}/title"),
                    UiContent::Text(TextSpec {
                        text: item.title.clone(),
                        role: SemanticRole::Heading,
                        style: StyleRef::Plain,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(title_w))
                        .height(Length::Fill),
                )
                .paint_only(),
            );
            if let Some(step) = &item.step {
                // The step stands apart from the title by the 12 points its width carries.
                head.push(
                    group(format!("{key}/step-seat"))
                        .layout(
                            Layout::row()
                                .width(Length::Fixed(step_w))
                                .height(Length::Fill)
                                .padding(Insets {
                                    left: 12.0,
                                    ..Insets::default()
                                }),
                        )
                        .children(vec![
                            UiNode::new(
                                format!("{key}/step"),
                                UiContent::Text(TextSpec {
                                    text: step.clone(),
                                    role: SemanticRole::Text,
                                    style: StyleRef::Secondary,
                                }),
                            )
                            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                        ])
                        .paint_only_tree(),
                );
            }
            head.push(spacer(format!("{key}/space")));
            if let Some((button, _)) = button {
                let w = button_w - 12.0;
                head.push(centred(
                    format!("{key}/button-seat"),
                    button,
                    Length::Fixed(w),
                    m.control_h,
                    band,
                ));
            }
            height = band;
        } else {
            // The title wraps beside the mark; the step and the button go under it.
            let (title, title_h) = words_node(
                &format!("{key}/title"),
                &item.title,
                SemanticRole::Heading,
                StyleRef::Plain,
                words_room,
                m.text_line,
            );
            let top = ((band - m.text_line) / 2.0).max(0.0);
            head.push(
                group(format!("{key}/title-seat"))
                    .layout(
                        Layout::column()
                            .height(Length::Fixed(title_h + top * 2.0))
                            .padding(Insets {
                                top,
                                ..Insets::default()
                            }),
                    )
                    .children(vec![title.paint_only_tree()]),
            );
            height = band.max(title_h + top * 2.0);
            if let Some(step) = &item.step {
                let (step, step_h) = words_node(
                    &format!("{key}/step"),
                    step,
                    SemanticRole::Text,
                    StyleRef::Secondary,
                    below_room,
                    m.text_line,
                );
                rest.push(indented(
                    &format!("{key}/step-seat"),
                    step.paint_only_tree(),
                    indent,
                    step_h,
                ));
            }
            if let Some((button, button_h)) = button {
                rest.push(indented(
                    &format!("{key}/button-seat"),
                    group(format!("{key}/button-row"))
                        .layout(Layout::row().height(Length::Fixed(button_h)))
                        .children(vec![button, spacer(format!("{key}/button-space"))]),
                    indent,
                    button_h,
                ));
            }
        }
        let head_h = height;
        let mut column = vec![
            group(format!("{key}/head"))
                .layout(Layout::row().gap(0.0).height(Length::Fixed(head_h)))
                .children(interleave_gap(head, &key)),
        ];
        for node in rest {
            height += RHYTHM + node_h(&node);
            column.push(node);
        }
        if let Some(reason) = &item.reason {
            let style = if item.code {
                StyleRef::Code
            } else {
                StyleRef::Note
            };
            let (words, words_h) = if item.code {
                words_node(
                    &format!("{key}/reason"),
                    reason,
                    SemanticRole::Text,
                    style,
                    below_room,
                    m.text_line,
                )
            } else {
                words_node(
                    &format!("{key}/reason"),
                    reason,
                    SemanticRole::Text,
                    style,
                    below_room,
                    m.note_line,
                )
            };
            column.push(indented(
                &format!("{key}/reason-seat"),
                words,
                indent,
                words_h,
            ));
            height += RHYTHM + words_h;
        }
        if let Some(next) = &item.next {
            let (words, words_h) = words_node(
                &format!("{key}/next"),
                next,
                SemanticRole::Text,
                StyleRef::Secondary,
                below_room,
                m.note_line,
            );
            column.push(indented(
                &format!("{key}/next-seat"),
                words,
                indent,
                words_h,
            ));
            height += RHYTHM + words_h;
        }
        if item.phrase {
            let (node, h) = self.phrase_block(&key, below_room);
            column.push(indented(&format!("{key}/phrase-seat"), node, indent, h));
            height += RHYTHM + h;
        }
        if item.vault {
            let (node, h) = self.vault_fields(&key, below_room);
            column.push(indented(&format!("{key}/vault-seat"), node, indent, h));
            height += RHYTHM + h;
        }
        // What stands under the head keeps a step of room below it, as the head's band keeps
        // above its title.
        let tail = if column.len() > 1 { RHYTHM * 2.0 } else { 0.0 };
        height += tail;
        let mut label = item.title.clone();
        if let Some(step) = &item.step {
            label = format!("{label}, {step}");
        }
        (
            UiNode::new(
                key,
                UiContent::Group(GroupSpec {
                    label: Some(label),
                    role: SemanticRole::Group,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::column()
                    .gap(RHYTHM)
                    .height(Length::Fixed(height))
                    .padding(Insets {
                        bottom: tail,
                        ..Insets::default()
                    }),
            )
            .children(column),
            height,
        )
    }

    /// The phrase field, its Join and the line under them (lead decision 6).
    fn phrase_block(&self, key: &str, width: f32) -> (UiNode, f32) {
        let m = &self.m;
        let enabled = self.app.phrase_editable();
        let help = self.app.phrase_help();
        let input = self.app.phrase.projection();
        let field_w = (width - Metrics::button_w("Joining…") - 8.0).clamp(40.0, 360.0);
        // The phrase is a secret: the field draws its words, but what a control client's dump
        // and a screen reader read of it is a bullet a character ([`Self::field`]).
        let field = self.field(
            PHRASE_FIELD,
            "Phrase from a Mac in ALab",
            Some("three words"),
            input,
            true,
            Some(help),
            enabled,
            field_w,
        );
        let join = Self::button(
            PHRASE_JOIN,
            if self.app.phrase_sent.is_some() {
                "Joining…"
            } else {
                "Join"
            }
            .to_owned(),
            None,
            StyleRef::Secondary,
            enabled && !self.app.phrase.value().trim().is_empty(),
            m.control_h,
        );
        let side = field_w + 8.0 + Metrics::button_w("Joining…") <= width;
        let (controls, controls_h) = if side {
            (
                group(format!("{key}/phrase-row"))
                    .layout(Layout::row().gap(8.0).height(Length::Fixed(m.control_h)))
                    .children(vec![field, join, spacer(format!("{key}/phrase-space"))]),
                m.control_h,
            )
        } else {
            (
                group(format!("{key}/phrase-row"))
                    .layout(
                        Layout::column()
                            .gap(4.0)
                            .height(Length::Fixed(m.control_h * 2.0 + 4.0)),
                    )
                    .children(vec![field, join]),
                m.control_h * 2.0 + 4.0,
            )
        };
        let mut children = vec![controls];
        let mut height = controls_h;
        // What the last phrase came to, where it let nobody in or reached nobody, stands under
        // the field; the words of what to type are the field's own description.
        if self
            .app
            .phrase_came_to
            .is_some_and(|outcome| outcome != PhraseOutcome::Joined)
        {
            let (line, line_h) = words_node(
                "system-state/vault/phrase-help",
                help,
                SemanticRole::Text,
                StyleRef::Note,
                width,
                m.note_line,
            );
            children.push(line);
            height += 4.0 + line_h;
        }
        (
            group(format!("{key}/phrase"))
                .layout(Layout::column().gap(4.0).height(Length::Fixed(height)))
                .children(children),
            height,
        )
    }

    /// A text field. A `secret` one's semantic value, what a control client's dump and a screen
    /// reader carry, is a bullet a character, so its words are kept only where it draws them.
    #[allow(
        clippy::too_many_arguments,
        reason = "a field's key, words, input, look and room, as the vault and phrase take them"
    )]
    fn field(
        &self,
        key: &str,
        label: &str,
        placeholder: Option<&str>,
        input: TextInputProjection,
        secret: bool,
        description: Option<&str>,
        enabled: bool,
        width: f32,
    ) -> UiNode {
        UiNode::new(
            key.to_owned(),
            UiContent::TextField(
                Control::new(
                    TextFieldSpec {
                        label: label.to_owned(),
                        placeholder: placeholder.map(str::to_owned),
                        secret,
                        visual_value: None,
                        input: Some(input.clone()),
                        swatch: None,
                        swatch_line: false,
                        trailing_hint: None,
                        description: description.map(str::to_owned),
                        end_button: false,
                    },
                    ActionId::new(key.to_owned()),
                )
                .value(SemanticValue::Text(if secret {
                    "•".repeat(input.text.graphemes().count())
                } else {
                    input.text
                }))
                .state(ControlState {
                    enabled,
                    focused: self
                        .view
                        .common
                        .last_focus
                        .as_ref()
                        .is_some_and(|focus| focus.as_str() == key),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(self.m.control_h)),
        )
    }

    /// The vault's address and password fields and its Join, each field under its caption.
    fn vault_fields(&self, key: &str, width: f32) -> (UiNode, f32) {
        let (node, height, _) = self.vault_fields_at(key, width);
        (node, height)
    }

    /// [`Self::vault_fields`], with where the address field, the password field and Join sit in
    /// it (top, height), so focus scrolls the list to the field itself, never only its caption.
    fn vault_fields_at(&self, key: &str, width: f32) -> (UiNode, f32, [(f32, f32); 3]) {
        let m = &self.m;
        let enabled = self.app.vault_editable();
        let field_w = width.clamp(40.0, 420.0);
        let caption = |caption_key: &str, words: &str| {
            words_node(
                caption_key,
                words,
                SemanticRole::Text,
                StyleRef::Secondary,
                width,
                m.text_line,
            )
        };
        let (address_caption, address_h) =
            caption("system-state/vault/address-label", "Vault address");
        let (password_caption, password_h) =
            caption("system-state/vault/password-label", "Password (optional)");
        let blank = "Leave the password blank to join without one.";
        let column = vec![
            address_caption.paint_only_tree(),
            self.field(
                "system-state/url",
                "Vault address",
                Some(aterm_types::macos_setup::DEFAULT_VAULT_URL),
                self.app.url.projection(),
                false,
                None,
                enabled,
                field_w,
            ),
            password_caption.paint_only_tree(),
            self.field(
                "system-state/password",
                "Password (optional)",
                None,
                self.app.password.projection(),
                true,
                Some(blank),
                enabled,
                field_w,
            ),
            group(format!("{key}/join-row"))
                .layout(Layout::row().height(Length::Fixed(m.control_h)))
                .children(vec![
                    Self::button(
                        "system-state/vault/join",
                        "Join".to_owned(),
                        None,
                        StyleRef::Secondary,
                        enabled && self.app.plan_known,
                        m.control_h,
                    ),
                    spacer(format!("{key}/join-space")),
                ]),
        ];
        let height = address_h + password_h + m.control_h * 3.0 + 4.0 * 4.0;
        let at = [
            (address_h + 4.0, m.control_h),
            (
                address_h + 4.0 + m.control_h + 4.0 + password_h + 4.0,
                m.control_h,
            ),
            (
                address_h + password_h + m.control_h * 2.0 + 4.0 * 4.0,
                m.control_h,
            ),
        ];
        (
            group(format!("{key}/vault"))
                .layout(Layout::column().gap(4.0).height(Length::Fixed(height)))
                .children(column),
            height,
            at,
        )
    }

    /// The list (§4.1): one inset grouped surface, five lines with hairlines between them, an
    /// open line's rows under it. Where its content is taller than `room` it scrolls inside,
    /// with its last [`FADE_STRIP`] a strip its rows fade out in.
    fn list(&mut self, room: f32) -> UiNode {
        let (items, content) = self.list_content(0.0);
        let scrolls = content > room + 0.5;
        self.scrolls = scrolls;
        let height = if scrolls { room } else { content };
        let live = if scrolls {
            room - FADE_STRIP.min(room / 4.0)
        } else {
            room
        };
        let limit = (content - live).max(0.0);
        let mut offset = self.view.scroll.get().clamp(0.0, limit);
        // A room that shrank, or a list whose rows above the focused control changed height,
        // keeps the focused control in view.
        let (was, before) = (self.view.room.get(), self.view.scroll.get());
        const EDGE: f32 = crate::native_ui::CLIP_EDGE_EPSILON;
        if let Some(focus) = self.view.common.last_focus.as_ref()
            && let Some((old_top, old_bottom)) = self
                .view
                .spans
                .borrow()
                .iter()
                .find(|span| *span.key == *focus.as_str())
                .map(|span| (span.top, span.bottom))
            && let Some(span) = self.spans.iter().find(|span| *span.key == *focus.as_str())
            && (live + EDGE < was || (span.top - old_top).abs() > EDGE)
            && old_bottom.min(before + was) - old_top.max(before) + EDGE
                >= crate::native_ui::LIST_PEEK_MIN.min(old_bottom - old_top)
        {
            offset = whole_at(offset, live, span.top, span.bottom).clamp(0.0, limit);
        }
        self.view.scroll.set(offset);
        self.view.height.set(height);
        self.view.room.set(live);
        self.view.limit.set(limit);
        let mut y = 0.0;
        let children: Vec<UiNode> = items
            .into_iter()
            .map(|(node, h)| {
                let (top, bottom) = (y - offset, y - offset + h);
                y += h;
                if bottom <= 0.01 || top >= height - 0.01 {
                    node.offscreen()
                } else {
                    node
                }
            })
            .collect();
        let list = UiNode::new(
            "system-state/groups",
            UiContent::Group(GroupSpec {
                label: Some("Mac setup".to_owned()),
                role: if scrolls {
                    SemanticRole::List
                } else {
                    SemanticRole::Group
                },
                style: StyleRef::Plain,
            }),
        )
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fixed(height))
                .scrolled(offset)
                .clipped(),
        )
        .room(live, scrolls)
        .lists_every_row()
        .children(children);
        let mut layers = vec![
            painted(
                "system-state/list/surface",
                crate::native_ui::SYSTEM_STATE_SURFACE_AUDIT,
                marks::surface_value(Ground::List),
            ),
            list,
        ];
        if scrolls {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "points of a list a few thousand tall, rounded"
            )]
            let share = format!(
                "{},{},{}",
                offset.round() as u32,
                live.round() as u32,
                content.round() as u32
            );
            layers.push(
                painted(
                    "system-state/scroll/thumb",
                    crate::native_ui::PAGE_SCROLL_AUDIT,
                    share,
                )
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fixed(live)),
                ),
            );
        }
        group("system-state/list")
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(layers)
    }

    /// The list's items, each with its height, and the content's height; `top` is where the
    /// list's content starts.
    fn list_content(&mut self, top: f32) -> (Vec<(UiNode, f32)>, f32) {
        let mut items = Vec::new();
        let mut y = top;
        if self.card_in_list
            && let Some((card, h)) = self.card_node(Some(y))
        {
            items.push((card, h));
            items.push((rule("system-state/card/rule".to_owned()), RULE_H));
            y += h + RULE_H;
        }
        // Over the switch column, once: what the switches say (§4.2), so a switch reads as what
        // aterm sets up, never as the thing it names turned on.
        if !self.stacked() && self.lines.iter().any(|line| line.switch.is_some()) {
            let m = self.m;
            let words = "aterm sets up";
            let words_w = Metrics::words_w(SemanticRole::Text, StyleRef::Note, words) + 2.0;
            let h = m.note_line + RHYTHM;
            items.push((
                group("system-state/switch-caption")
                    .layout(Layout::row().height(Length::Fixed(h)).padding(Insets {
                        top: RHYTHM,
                        right: m.card_pad - 8.0,
                        ..Insets::default()
                    }))
                    .children(vec![
                        spacer("system-state/switch-caption/space"),
                        UiNode::new(
                            "system-state/switch-caption/words",
                            UiContent::Text(TextSpec {
                                text: words.to_owned(),
                                role: SemanticRole::Text,
                                style: StyleRef::Note,
                            }),
                        )
                        .layout(
                            Layout::default()
                                .width(Length::Fixed(words_w))
                                .height(Length::Fixed(m.note_line)),
                        ),
                    ])
                    .paint_only_tree(),
                h,
            ));
            y += h;
        }
        let lines = self.lines.clone();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                items.push((
                    rule(format!("system-state/line/{}/rule", line.line.slug())),
                    RULE_H,
                ));
                y += RULE_H;
            }
            let (node, h) = self.line(line, y);
            items.push((node, h));
            y += h;
        }
        if self.footer_in_list
            && let Some((footer, h)) = self.footer(self.m.inner - self.m.card_pad * 2.0)
        {
            items.push((rule("system-state/footer/rule".to_owned()), RULE_H));
            y += RULE_H;
            let pad = self.m.card_pad_y * 2.0;
            for key in [
                "system-state/footer",
                "system-state/headline/start",
                "system-state/check",
                "system-state/restore",
            ] {
                if holds(&footer, key) {
                    self.span(key, y, h + pad);
                }
            }
            items.push((
                group("system-state/footer-seat")
                    .layout(
                        Layout::column()
                            .height(Length::Fixed(h + pad))
                            .padding(Insets {
                                top: pad / 2.0,
                                left: self.m.card_pad,
                                right: self.m.card_pad,
                                bottom: pad / 2.0,
                            }),
                    )
                    .children(vec![footer]),
                h + pad,
            ));
            y += h + pad;
        }
        (items, y - top)
    }

    /// A pane too narrow for a line's words beside its switch: the switch goes under them, and
    /// an open line's rows take the width the switch's column leaves elsewhere.
    fn stacked(&self) -> bool {
        let m = &self.m;
        m.inner - m.card_pad * 2.0 - SWITCH_W - CHEVRON_W - m.title_x < STACK_BELOW * m.scale
    }

    /// An open line's row insets: under the title on the left, clear of the switch's column on
    /// the right (its small mark just before it); a stacked line's rows take the list's width,
    /// which a pane that narrow cannot spare to an indent.
    fn row_insets(&self) -> (f32, f32) {
        let m = &self.m;
        if self.stacked() {
            (m.card_pad, m.card_pad)
        } else {
            (
                m.card_pad + m.title_x,
                m.card_pad + CHEVRON_W + SWITCH_W - 12.0 * m.scale - 8.0,
            )
        }
    }

    fn span(&mut self, key: impl Into<Arc<str>>, top: f32, height: f32) {
        self.spans.push(Span {
            key: key.into(),
            top,
            bottom: top + height,
        });
    }

    /// Whether `line` is the one the person opened.
    fn open(&self, line: &LineView) -> bool {
        #[cfg(test)]
        if self.view.every_line_open.get() {
            return line.expandable();
        }
        line.expandable() && self.view.open_line == Some(line.line)
    }

    /// One line (§4.1): the mark, the title, the live phrase, the disclosure and the switch;
    /// opened, its rows under it.
    fn line(&mut self, line: &LineView, top: f32) -> (UiNode, f32) {
        let m = self.m;
        let slug = line.line.slug();
        let key = format!("system-state/line/{slug}");
        let open = self.open(line);
        let looks = marks::Looks::now();
        let inner = m.inner - m.card_pad * 2.0;
        let stacked = self.stacked();
        let content_w = if stacked {
            (inner - CHEVRON_W - m.title_x).max(1.0)
        } else {
            inner - SWITCH_W - CHEVRON_W - m.title_x
        };
        let title_style = StyleRef::Plain;
        // High Contrast says the state in words on every line (§4.4). A key's name stays whole
        // on one line where the phrase wraps.
        let phrase = if looks.high_contrast && !line.phrase.to_lowercase().contains(line.word) {
            format!("{} · {}", line.phrase, line.word)
        } else {
            line.phrase.clone()
        }
        .replace("Caps Lock", "Caps\u{a0}Lock");
        let phrase_style = if line.quiet {
            StyleRef::Quiet
        } else {
            StyleRef::Secondary
        };
        let phrase_w = Metrics::words_w(SemanticRole::Text, phrase_style, &phrase);
        // A fixed title column where it fits, so the phrases line up.
        let title_col = Line::ALL
            .iter()
            .map(|line| Metrics::words_w(SemanticRole::Heading, title_style, line.title()))
            .fold(0.0, f32::max)
            + 16.0;
        let one_line = !stacked && title_col + phrase_w <= content_w;
        let band = m.line_h;
        let ground = if line.mark == Mark::Working {
            Ground::Working
        } else {
            Ground::List
        };
        let mut info = vec![mark_node(
            &key,
            (line.mark, ground),
            self.spins(line.mark),
            marks::MARK_SLOT,
            band,
        )];
        info.push(
            gap(format!("{key}/mark-gap"), MARK_GAP).layout(
                Layout::default()
                    .width(Length::Fixed(MARK_GAP))
                    .height(Length::Fill),
            ),
        );
        let head_h;
        if one_line {
            info.push(
                UiNode::new(
                    format!("{key}/title"),
                    UiContent::Text(TextSpec {
                        text: line.line.title().to_owned(),
                        role: SemanticRole::Heading,
                        style: title_style,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(title_col))
                        .height(Length::Fill),
                ),
            );
            info.push(
                UiNode::new(
                    format!("{key}/phrase"),
                    UiContent::Text(TextSpec {
                        text: phrase.clone(),
                        role: SemanticRole::Text,
                        style: phrase_style,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            );
            head_h = band;
        } else {
            // Narrow: the title on its line, the phrase wrapped under it.
            let words_room = content_w;
            let title_line = m.text_line;
            let (phrase_node, phrase_h) = words_node(
                &format!("{key}/phrase"),
                &phrase,
                SemanticRole::Text,
                phrase_style,
                words_room,
                m.text_line,
            );
            let (title_node, title_h) = words_node(
                &format!("{key}/title"),
                line.line.title(),
                SemanticRole::Heading,
                title_style,
                words_room,
                title_line,
            );
            let pad = ((band - title_line - m.text_line) / 2.0).max(4.0);
            head_h = (pad * 2.0 + title_h + phrase_h).max(band);
            info.push(
                group(format!("{key}/words"))
                    .layout(
                        Layout::column()
                            .width(Length::Fill)
                            .height(Length::Fixed(head_h))
                            .padding(Insets {
                                top: pad,
                                ..Insets::default()
                            }),
                    )
                    .children(vec![
                        title_node.layout(
                            Layout::column()
                                .width(Length::Fill)
                                .height(Length::Fixed(title_h)),
                        ),
                        phrase_node,
                    ]),
            );
        }
        let info = group(format!("{key}/info"))
            .layout(
                Layout::row()
                    .width(Length::Fill)
                    .height(Length::Fixed(head_h))
                    .padding(Insets {
                        left: m.card_pad,
                        right: CHEVRON_W,
                        ..Insets::default()
                    }),
            )
            .children(info)
            .paint_only_tree();
        // The disclosure spans the line's words, its chevron just before the switch.
        let expandable = line.expandable();
        let mut head_layers = Vec::new();
        // VoiceOver hears the line's name once, from its switch with where it stands now and its
        // phrase, its value saying what is wanted (lead decision 1); the disclosure inside the
        // line's group is its details.
        if expandable {
            let mut spec = ButtonSpec::new("Details");
            spec.visual_label = Some(String::new());
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a line's height in points"
            )]
            {
                spec.chevron_row = Some(band.round() as u16);
            }
            head_layers.push(
                UiNode::new(
                    format!("{key}/disclose"),
                    UiContent::Button(
                        Control::new(spec, ActionId::new(format!("{key}/disclose")))
                            .state(ControlState {
                                expanded: Some(open),
                                ..ControlState::default()
                            })
                            .style(StyleRef::Plain),
                    ),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            );
        }
        head_layers.push(info);
        // No switch while the plan, or whether the line applies here, is still being read: a
        // pending answer never reads as off (§2.2). Its seat stays, so nothing moves when it
        // comes; the line's group says its state meanwhile.
        let switch = match line.switch {
            Some(on) => UiNode::new(
                format!("{key}/switch"),
                UiContent::Switch(
                    Control::new(
                        SwitchSpec {
                            label: line.label.clone(),
                            description: None,
                        },
                        ActionId::new(format!("{key}/switch")),
                    )
                    .value(SemanticValue::Bool(on))
                    .state(ControlState {
                        enabled: line.offered,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Quiet),
                ),
            ),
            None => spacer(format!("{key}/no-switch")),
        }
        .layout(
            Layout::default()
                .width(Length::Fixed(SWITCH_W))
                .height(Length::Fixed(m.control_h.min(band))),
        );
        let switch_h = m.control_h.min(band);
        let hit = group(format!("{key}/hit"))
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(head_h)),
            )
            .children(head_layers);
        let (head, words_h) = if stacked {
            // The words across the line, the switch under them where the title starts.
            let switch_row = group(format!("{key}/switch-seat"))
                .layout(
                    Layout::row()
                        .height(Length::Fixed(switch_h))
                        .padding(Insets {
                            left: m.card_pad + m.title_x,
                            ..Insets::default()
                        }),
                )
                .children(vec![switch, spacer(format!("{key}/switch-space"))]);
            let height = head_h + switch_h + 8.0;
            (
                group(format!("{key}/head"))
                    .layout(Layout::column().height(Length::Fixed(height)))
                    .children(vec![hit, switch_row]),
                height,
            )
        } else {
            (
                group(format!("{key}/head"))
                    .layout(Layout::row().height(Length::Fixed(head_h)).padding(Insets {
                        right: m.card_pad - 8.0,
                        ..Insets::default()
                    }))
                    .children(vec![
                        hit.layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                        centred(
                            format!("{key}/switch-seat"),
                            switch,
                            Length::Fixed(SWITCH_W),
                            switch_h,
                            band,
                        ),
                    ]),
                head_h,
            )
        };
        // A working line's wash: a rounded band a step inside the list's edges, under the
        // whole head, its switch included, clear of the list's own rounded corners.
        let head = if line.mark == Mark::Working && !looks.solid {
            group(format!("{key}/band"))
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fixed(words_h)),
                )
                .children(vec![
                    group(format!("{key}/wash-seat"))
                        .layout(
                            Layout::column()
                                .width(Length::Fill)
                                .height(Length::Fixed(words_h))
                                .padding(Insets::symmetric(RHYTHM, RHYTHM)),
                        )
                        .children(vec![
                            painted(
                                format!("{key}/wash"),
                                crate::native_ui::SYSTEM_STATE_SURFACE_AUDIT,
                                marks::surface_value(Ground::Working),
                            )
                            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                        ])
                        .paint_only_tree(),
                    head,
                ])
        } else {
            head
        };
        if expandable {
            self.span(format!("{key}/disclose"), top, head_h);
        }
        self.span(format!("{key}/switch"), top, words_h);
        let mut children = vec![head];
        let mut height = words_h;
        if open {
            let rows = line.rows.clone();
            // "Join a different vault…" goes under the Secrets vault's row, or, where the card
            // holds that row, at the end of the Secrets line.
            let vault_row = Subject::Setting(super::Setting::Secrets);
            let other_vault_last =
                line.line == Line::Secrets && !rows.iter().any(|row| row.subject == vault_row);
            for row in &rows {
                children.push(rule_inset(
                    format!("system-state/row/{}/rule", row_slug(row.subject)),
                    self.row_insets().0,
                ));
                height += RULE_H;
                let (node, h) = self.row(row);
                let slug = row_slug(row.subject);
                self.span(format!("system-state/row/{slug}"), top + height, h);
                if row.turn_on_sync {
                    self.span(super::TURN_ON_SYNC.to_owned(), top + height, h);
                } else if row.install_tools {
                    self.span(super::INSTALL_TOOLS.to_owned(), top + height, h);
                }
                children.push(node);
                height += h;
                if row.subject == vault_row {
                    height += self.push_other_vault(&mut children, top + height, inner);
                }
            }
            if other_vault_last {
                height += self.push_other_vault(&mut children, top + height, inner);
            }
            height += RHYTHM * 2.0;
        }
        self.span(key.clone(), top, height);
        // The line's name is said once: by its switch, with its state and phrase; where no switch
        // says them yet, by the group itself.
        let group_label = line.switch.is_none().then(|| line.label.clone());
        (
            UiNode::new(
                key,
                UiContent::Group(GroupSpec {
                    label: group_label,
                    role: SemanticRole::Group,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(children),
            height,
        )
    }

    /// An open line's row: its name, `now → wanted` in secondary ink, its small mark, its quiet
    /// button where it has one (Turn On Sync), and its note in Caption under it.
    fn row(&self, row: &RowView) -> (UiNode, f32) {
        let m = self.m;
        let (left, right) = self.row_insets();
        let room = (m.inner - left - right).max(1.0);
        let slug = row_slug(row.subject);
        let key = format!("system-state/row/{slug}");
        let name = row.subject.name();
        let mark = row_mark(row);
        let pieces: Vec<(String, StyleRef)> = match &row.value {
            RowValue::Change { now, wanted } => vec![
                (now.clone(), StyleRef::Secondary),
                ("→".to_owned(), StyleRef::Quiet),
                (wanted.clone(), StyleRef::Primary),
            ],
            RowValue::One(value) => vec![(value.clone(), StyleRef::Secondary)],
            RowValue::Muted(value) => vec![(value.clone(), StyleRef::Quiet)],
        };
        let piece_w = |pieces: &[(String, StyleRef)]| {
            pieces
                .iter()
                .map(|(words, style)| Metrics::words_w(SemanticRole::Text, *style, words))
                .sum::<f32>()
                + 6.0 * pieces.len().saturating_sub(1) as f32
        };
        // Its one quiet button: Turn On Sync for a kind turned off, Install… for the tools a
        // wait (Low Data Mode) holds back, which a person may start now.
        let button = if row.turn_on_sync {
            Some((super::TURN_ON_SYNC, "Turn On Sync"))
        } else if row.install_tools {
            Some((super::INSTALL_TOOLS, super::INSTALL_TOOLS_LABEL))
        } else {
            None
        }
        .map(|(key, words)| {
            Self::fitted_button(
                key,
                words.to_owned(),
                None,
                StyleRef::Quiet,
                true,
                m.control_h.min(m.row_h),
                room,
            )
        });
        let slot = 12.0 * m.scale;
        let name_w = Metrics::words_w(SemanticRole::Text, StyleRef::Primary, name);
        let name_col = (room * 0.42)
            .min(220.0 * m.scale)
            .max(name_w.min(room * 0.5));
        let value_w = piece_w(&pieces);
        let one_line = name_w <= name_col && name_col + value_w + slot + 12.0 <= room;
        let value_nodes = |pieces: Vec<(String, StyleRef)>, first: usize| -> Vec<UiNode> {
            pieces
                .into_iter()
                .enumerate()
                .map(|(index, (words, style))| {
                    let width = Metrics::words_w(SemanticRole::Text, style, &words);
                    UiNode::new(
                        format!("{key}/value/{}", first + index),
                        UiContent::Text(TextSpec {
                            text: words,
                            role: SemanticRole::Text,
                            style,
                        }),
                    )
                    .layout(
                        Layout::default()
                            .width(Length::Fixed(width))
                            .height(Length::Fill),
                    )
                })
                .collect()
        };
        let (info, info_h) = if one_line {
            let mut children = vec![
                UiNode::new(
                    format!("{key}/name"),
                    UiContent::Text(TextSpec {
                        text: name.to_owned(),
                        role: SemanticRole::Text,
                        style: StyleRef::Primary,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(name_col))
                        .height(Length::Fill),
                ),
            ];
            children.extend(value_nodes(pieces, 0));
            children.push(spacer(format!("{key}/space")));
            children.push(mark_node(
                &key,
                (mark, Ground::List),
                self.spins(mark),
                slot,
                m.row_h,
            ));
            (
                group(format!("{key}/words"))
                    .layout(
                        Layout::row()
                            .gap(6.0)
                            .width(Length::Fill)
                            .height(Length::Fixed(m.row_h)),
                    )
                    .children(children),
                m.row_h,
            )
        } else {
            // The value under the name; each wraps where its line cannot hold it.
            let line = m.text_line;
            // The name takes the line's room up to the mark: a seat of its own, never a share
            // of the room with the spacer before the mark.
            let name_room = (room - slot - 12.0).max(1.0);
            let (name_node, name_h) = words_node(
                &format!("{key}/name"),
                name,
                SemanticRole::Text,
                StyleRef::Primary,
                name_room,
                line,
            );
            let name_seat = group(format!("{key}/name-seat"))
                .layout(
                    Layout::column()
                        .width(Length::Fixed(name_room))
                        .height(Length::Fixed(name_h)),
                )
                .children(vec![name_node]);
            let first = group(format!("{key}/line-0"))
                .layout(Layout::row().gap(6.0).height(Length::Fixed(name_h)))
                .children(vec![
                    name_seat,
                    spacer(format!("{key}/space")),
                    mark_node(&key, (mark, Ground::List), self.spins(mark), slot, line),
                ]);
            let words = pieces
                .iter()
                .map(|(words, _)| words.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let (value, value_h) = if value_w <= room {
                (
                    group(format!("{key}/line-1"))
                        .layout(Layout::row().gap(6.0).height(Length::Fixed(line)))
                        .children({
                            let mut nodes = value_nodes(pieces, 0);
                            nodes.push(spacer(format!("{key}/value-space")));
                            nodes
                        }),
                    line,
                )
            } else {
                words_node(
                    &format!("{key}/value"),
                    &words,
                    SemanticRole::Text,
                    StyleRef::Secondary,
                    room,
                    line,
                )
            };
            let height = name_h + value_h + 8.0;
            (
                group(format!("{key}/words"))
                    .layout(
                        Layout::column()
                            .width(Length::Fill)
                            .height(Length::Fixed(height))
                            .padding(Insets {
                                top: 4.0,
                                bottom: 4.0,
                                ..Insets::default()
                            }),
                    )
                    .children(vec![first, value]),
                height,
            )
        };
        let info = UiNode::new(
            format!("{key}/info"),
            UiContent::Group(GroupSpec {
                label: Some(row.label.clone()),
                role: SemanticRole::Text,
                style: StyleRef::Plain,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(info_h)),
        )
        .children(vec![info.paint_only_tree()]);
        let mut height = info_h;
        let mut column = vec![info];
        if let Some((button, button_w, button_h)) = button {
            column.push(
                group(format!("{key}/actions"))
                    .layout(Layout::row().height(Length::Fixed(button_h)))
                    .children(vec![
                        button.layout(
                            Layout::default()
                                .width(Length::Fixed(button_w))
                                .height(Length::Fixed(button_h)),
                        ),
                        spacer(format!("{key}/actions/space")),
                    ]),
            );
            height += button_h + 4.0;
        }
        if let Some(note) = row.note.as_deref().filter(|note| !note.trim().is_empty()) {
            let (note, note_h) = words_node(
                &format!("{key}/note"),
                note,
                SemanticRole::Text,
                StyleRef::Note,
                room,
                m.note_line,
            );
            column.push(note);
            height += note_h + RHYTHM * 2.0;
        }
        (
            group(key)
                .layout(
                    Layout::column()
                        .gap(0.0)
                        .height(Length::Fixed(height))
                        .padding(Insets {
                            left,
                            right,
                            bottom: if row.note.is_some() {
                                RHYTHM * 2.0
                            } else {
                                0.0
                            },
                            ..Insets::default()
                        }),
                )
                .children(column),
            height,
        )
    }

    /// [`Self::other_vault`]'s nodes pushed onto `children` at `top`, their keys listed; the
    /// height they take.
    fn push_other_vault(&mut self, children: &mut Vec<UiNode>, top: f32, inner: f32) -> f32 {
        let mut height = 0.0;
        let indent = self.row_insets().0 - self.m.card_pad;
        for (node, h, keys) in self.other_vault(top, inner - indent) {
            for (key, at, span_h) in keys {
                self.span(key, at, span_h);
            }
            children.push(node);
            height += h;
        }
        height
    }

    /// Under the Secrets row, "Join a different vault…" and, open, its fields (§4.3); where the
    /// card holds the vault's fields, nothing.
    fn other_vault(&self, top: f32, room: f32) -> Vec<ListPiece> {
        let m = self.m;
        if self.app.status.newer() {
            // A newer aterm's setup is in force (§3.2): this aterm joins nothing, so the vault
            // reads as it stands, its address in words, with no field and no Join.
            let (address, h) = words_node(
                "system-state/vault/address",
                &self.app.vault_address_as_it_stands(),
                SemanticRole::Text,
                StyleRef::Secondary,
                room.max(40.0),
                m.text_line,
            );
            let height = h + 8.0;
            return vec![(
                indented(
                    "system-state/vault/seat",
                    address,
                    self.row_insets().0,
                    height,
                ),
                height,
                Vec::new(),
            )];
        }
        if self.card.iter().any(|item| item.vault) {
            return Vec::new();
        }
        let open = self.app.vault_shown();
        // Nothing stands right of it: the row's mark and the switch's column are not in its way.
        let left = (self.row_insets().0 - 10.0).max(0.0);
        let right = m.card_pad;
        let avail = (m.inner - left - right).max(1.0);
        let (mut button, button_w, button_h) = Self::fitted_button(
            "system-state/row/secrets/disclose",
            "Join a different vault…".to_owned(),
            Some("Other vault…"),
            StyleRef::Plain,
            true,
            m.row_h,
            (avail - 24.0).max(1.0),
        );
        // Its disclosure mark takes a little more after the words.
        let button_w = (button_w + 24.0).min(avail);
        if let UiContent::Button(control) = &mut button.content {
            control.state.expanded = Some(open);
        }
        let disclose_h = button_h.max(m.row_h);
        let disclose = group("system-state/row/secrets/disclose-seat")
            .layout(
                Layout::row()
                    .height(Length::Fixed(disclose_h))
                    .padding(Insets {
                        left,
                        right,
                        ..Insets::default()
                    }),
            )
            .children(vec![
                button.layout(
                    Layout::default()
                        .width(Length::Fixed(button_w))
                        .height(Length::Fixed(button_h)),
                ),
                spacer("system-state/row/secrets/disclose-space"),
            ]);
        let mut out = vec![(
            disclose,
            disclose_h,
            vec![(
                "system-state/row/secrets/disclose".to_owned(),
                top,
                disclose_h,
            )],
        )];
        if !open {
            return out;
        }
        let (fields, fields_h, at) = self.vault_fields_at("system-state/vault", room.min(420.0));
        let keys = [
            "system-state/url",
            "system-state/password",
            "system-state/vault/join",
        ]
        .into_iter()
        .zip(at)
        .map(|(key, (field_top, field_h))| (key.to_owned(), top + disclose_h + field_top, field_h))
        .collect();
        out.push((
            group("system-state/vault/seat")
                .layout(
                    Layout::column()
                        .height(Length::Fixed(fields_h + 8.0))
                        .padding(Insets {
                            left: self.row_insets().0,
                            bottom: 8.0,
                            ..Insets::default()
                        }),
                )
                .children(vec![fields]),
            fields_h + 8.0,
            keys,
        ));
        out
    }

    /// While nothing runs (§4.1), once something was read: Set up now while a hold stands (a
    /// Stop, an older aterm's Cancel), Check again, and, at the right, Restore previous
    /// settings… once aterm saved preferences it can put back.
    fn footer(&self, room: f32) -> Option<(UiNode, f32)> {
        let status = &self.app.status;
        let restore_waits = status.queued && status.operation == Some(SetupOperation::Restore);
        if status.busy && !restore_waits && !status.queued {
            return None;
        }
        // Nothing read yet: nothing to check again (Restore stays where aterm saved preferences,
        // whatever can be read now).
        let read = (!status.observed.is_empty() && self.app.plan_known()) || status.newer();
        let m = &self.m;
        let mut left: Vec<(UiNode, f32, f32)> = Vec::new();
        if read && self.hero.start {
            left.push(Self::fitted_button(
                "system-state/headline/start",
                "Set up now".to_owned(),
                None,
                StyleRef::Quiet,
                !status.busy,
                m.control_h,
                room,
            ));
        }
        if read && self.app.can_check() {
            left.push(Self::fitted_button(
                "system-state/check",
                "Check again".to_owned(),
                Some("Check"),
                StyleRef::Quiet,
                true,
                m.control_h,
                room,
            ));
        }
        let restore = if restore_waits {
            Some(Self::fitted_button(
                "system-state/restore",
                RESTORE_WAITS.to_owned(),
                Some("Waiting…"),
                StyleRef::Quiet,
                false,
                m.control_h,
                room,
            ))
        } else if status.restorable && self.app.can_restore() {
            Some(Self::fitted_button(
                "system-state/restore",
                format!("{RESTORE_FINISHED}…"),
                Some("Restore…"),
                StyleRef::Quiet,
                true,
                m.control_h,
                room,
            ))
        } else {
            None
        };
        let restore = restore.map(|(mut node, w, h)| {
            if let UiContent::Button(control) = &mut node.content {
                control.spec.description = Some(if restore_waits {
                    QUEUED_RESTORE_NOTE.to_owned()
                } else {
                    "Puts back the Mac preferences aterm saved before changing them. Sign-outs, \
                     Rust docs, repositories, credentials and permissions stay."
                        .to_owned()
                });
            }
            (node, w, h)
        });
        if left.is_empty() && restore.is_none() {
            return None;
        }
        let left_w: f32 = left.iter().map(|(_, w, _)| w + 8.0).sum();
        // Side by side with Restore's short face where its whole face leaves no room for all.
        let short_w = Metrics::button_w("Restore…");
        let restore = restore.map(|(node, w, h)| {
            if left_w + 8.0 + w > room && !restore_waits && left_w + 8.0 + short_w <= room {
                let mut node = node.layout(
                    Layout::default()
                        .width(Length::Fixed(short_w))
                        .height(Length::Fixed(m.control_h)),
                );
                if let UiContent::Button(control) = &mut node.content {
                    control.spec.visual_label = Some("Restore…".to_owned());
                }
                (node, short_w, m.control_h)
            } else {
                (node, w, h)
            }
        });
        let restore_w = restore.as_ref().map_or(0.0, |(_, w, _)| *w);
        if left_w + 8.0 + restore_w <= room {
            let height = left
                .iter()
                .chain(restore.iter())
                .map(|(_, _, h)| *h)
                .fold(0.0, f32::max);
            let mut children: Vec<UiNode> = left.into_iter().map(|(node, _, _)| node).collect();
            children.push(spacer("system-state/footer/space"));
            children.extend(restore.map(|(node, _, _)| node));
            return Some((
                group("system-state/footer")
                    .layout(Layout::row().gap(8.0).height(Length::Fixed(height)))
                    .children(children),
                height,
            ));
        }
        // One to a row, each at its own width where it fits, the rest of its row empty.
        let seat = |node: UiNode, w: f32, h: f32| {
            let key = format!("{}/row", node.key.as_str());
            let space = format!("{key}/space");
            group(key)
                .layout(Layout::row().height(Length::Fixed(h)))
                .children(vec![
                    node.layout(
                        Layout::default()
                            .width(Length::Fixed(w))
                            .height(Length::Fixed(h)),
                    ),
                    spacer(space),
                ])
        };
        let all: Vec<(UiNode, f32, f32)> = left.into_iter().chain(restore).collect();
        let height = all.iter().map(|(_, _, h)| *h).sum::<f32>() + 4.0 * (all.len() - 1) as f32;
        Some((
            group("system-state/footer")
                .layout(Layout::column().gap(4.0).height(Length::Fixed(height)))
                .children(
                    all.into_iter()
                        .map(|(node, w, h)| seat(node, w, h))
                        .collect(),
                ),
            height,
        ))
    }
}

/// A row's small mark (§4.1's 12 pt glyph): its chip, as the row rule decided it.
fn row_mark(row: &RowView) -> Mark {
    match row.chip {
        Some(Chip::InPlace) => Mark::Done,
        Some(Chip::WillChange { busy: true }) => Mark::Working,
        Some(Chip::WillChange { busy: false }) => Mark::Pending,
        Some(Chip::Waiting) => Mark::Waiting,
        Some(Chip::NeedsYou) => Mark::NeedsYou,
        Some(Chip::Failed) => Mark::Failed,
        Some(Chip::LeftAsIs) => Mark::Off,
        // As the person left it, after aterm set it: theirs, and settled.
        None if row.changed_by_you => Mark::Done,
        None if !row.managed => Mark::Off,
        None => Mark::Pending,
    }
}

/// A fixed gap in a column.
fn gap(key: impl Into<Arc<str>>, height: f32) -> UiNode {
    group(key)
        .layout(Layout::default().height(Length::Fixed(height)))
        .paint_only()
}

/// A hairline across the list.
fn rule(key: String) -> UiNode {
    rule_inset(key, 0.0)
}

/// A hairline from `inset` on.
fn rule_inset(key: String, inset: f32) -> UiNode {
    // The hairline two points down its seat: whole pixels at 1x, the seat on the rhythm.
    group(format!("{key}-seat"))
        .layout(Layout::row().height(Length::Fixed(RULE_H)).padding(Insets {
            left: inset,
            top: (RULE_H / 2.0).floor(),
            bottom: RULE_H - (RULE_H / 2.0).floor() - 1.0,
            ..Insets::default()
        }))
        .children(vec![
            UiNode::new(
                key,
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
            .layout(Layout::default().height(Length::Fixed(1.0))),
        ])
        .paint_only_tree()
}

/// `node`, `height` tall, set `inset` in.
fn indented(key: &str, node: UiNode, inset: f32, height: f32) -> UiNode {
    group(key.to_owned())
        .layout(
            Layout::column()
                .height(Length::Fixed(height))
                .padding(Insets {
                    left: inset,
                    ..Insets::default()
                }),
        )
        .children(vec![node])
}

/// A node's fixed height.
fn node_h(node: &UiNode) -> f32 {
    match node.layout.height {
        Length::Fixed(h) => h,
        _ => 0.0,
    }
}

/// A head row's pieces with [`MARK_GAP`] after the mark, so a card's title starts where a
/// line's does.
fn interleave_gap(mut head: Vec<UiNode>, key: &str) -> Vec<UiNode> {
    if head.len() > 1 {
        head.insert(
            1,
            group(format!("{key}/mark-gap"))
                .layout(
                    Layout::default()
                        .width(Length::Fixed(MARK_GAP))
                        .height(Length::Fill),
                )
                .paint_only(),
        );
    }
    head
}
