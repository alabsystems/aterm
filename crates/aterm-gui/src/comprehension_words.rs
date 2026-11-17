// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE COMPREHENSION WORDS (`docs/DESIGN-comprehension-2026-11-14.md` §5.6): the one function from
//! a track, activity or event word to the form and ink every view draws it in ([`look`]), the
//! Custom node that paints that form and speaks its word ([`mark_node`]), and this Mac's activity
//! word ([`activity_here`]).
//!
//! A mark is drawn from a typed word, never from text, and paints no glyph of its word: a screen
//! reader hears the word from the node's label, and the form carries it on the screen. Each of
//! the four track marks (`none` draws nothing), the four activity icons and the event mark has its
//! own form, so every pair still differs with each ink replaced by grey. One hue exists, `danger`,
//! for `off-track`; every other mark is drawn in a text ink ("Grey is normal").

use std::sync::Arc;

use aterm_db::words::{Activity, Seen, State, Track, TrackWhy};

use crate::fleet_glance::{AgentWord, TaskWord, glance_of};
use crate::native_appearance::{ensure_contrast_on, mix_linear, mix_srgb};
use crate::native_ui::{
    AuditedCustomNode, COMPREHENSION_MARK_AUDIT, DISCLOSURE_HOVER_WASH, GroupSpec, Layout, Length,
    LogicalRect, SemanticRole, UiContent, UiNode,
};
use crate::settings::Roles;
use crate::system_state::marks::MARK_INK_CONTRAST;
use crate::widget::{DrawPrim, rgba};

/// A mark's slot beside a Title line (20 px): the track mark before a topic, the activity icon
/// at the row's right edge.
#[cfg_attr(
    not(debug_assertions),
    expect(
        dead_code,
        reason = "no view of a release build places a mark yet; the debug specimen does"
    )
)]
pub(crate) const MARK_TITLE: f32 = 16.0;

/// A mark's slot beside a Caption line (11 px): the event mark before a history line's time.
#[cfg_attr(
    not(debug_assertions),
    expect(
        dead_code,
        reason = "no view of a release build places a mark yet; the debug specimen does"
    )
)]
pub(crate) const MARK_CAPTION: f32 = 12.0;

/// What a mark stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Word<'a> {
    /// A track cell: its word, `None` where it is unread.
    Track(Option<Track>),
    /// An activity cell: its word, `None` where it is unread.
    Activity(Option<Activity>),
    /// A history line aterm's own writer reported, by its event word. The form is one for every
    /// event word; the word is what the mark speaks.
    Event(&'a str),
}

impl Word<'_> {
    /// The painter's value: `track:<word>` or `track:?`, `activity:<word>` or `activity:?`, or
    /// `event` (the painter needs no event word; the spoken label carries it).
    #[cfg_attr(
        not(debug_assertions),
        expect(
            dead_code,
            reason = "no view of a release build places a mark yet; the debug specimen does"
        )
    )]
    pub(crate) fn value(&self) -> String {
        match self {
            Self::Track(Some(track)) => format!("track:{}", track.word()),
            Self::Track(None) => "track:?".to_owned(),
            Self::Activity(Some(activity)) => format!("activity:{}", activity.word()),
            Self::Activity(None) => "activity:?".to_owned(),
            Self::Event(_) => "event".to_owned(),
        }
    }
}

impl Word<'static> {
    /// The word a painter's value names ([`Word::value`]); a value that names none is `None`,
    /// and paints nothing.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        if value == "event" {
            return Some(Self::Event(""));
        }
        let (column, word) = value.split_once(':')?;
        let unread = word == "?";
        match column {
            "track" if unread => Some(Self::Track(None)),
            "track" => Track::of(word).map(|track| Self::Track(Some(track))),
            "activity" if unread => Some(Self::Activity(None)),
            "activity" => Activity::of(word).map(|activity| Self::Activity(Some(activity))),
            _ => None,
        }
    }
}

/// How a word is drawn: its form and its ink, `None` for a word that draws nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Look {
    pub(crate) form: Form,
    pub(crate) ink: Option<Ink>,
}

/// A mark's form ([`paint_form`]). The design's sketches stand for these by `◆ ◇ ✓ ● ▶ ‖ – ▪ ▹ ?`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Form {
    /// A filled diamond: off track.
    Diamond,
    /// A hollow diamond: at risk.
    HollowDiamond,
    /// A check: done.
    Check,
    /// A disc: on track.
    Disc,
    /// A filled triangle pointing right: working.
    Play,
    /// Two bars: quiet, and stuck.
    Pause,
    /// A level stroke: idle.
    Dash,
    /// A small square: ended.
    Square,
    /// A hollow triangle pointing right, in the Caption slot: a history line aterm recorded. It
    /// fills more of its slot than working's filled one does of its own ([`EVENT`]), so its
    /// hollow stays open under High Contrast's 2 pt outline at 1x and its form differs from
    /// working's at either size.
    Event,
    /// A drawn question mark: the cell is unread.
    Unread,
    /// No prim: a session with no track.
    Nothing,
}

/// A mark's ink, by the [`crate::native_appearance::NativeRoles`] field it is drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ink {
    /// `danger`: off track, the one hue.
    Danger,
    /// `text_primary`: at risk, a second form and no second hue.
    Primary,
    /// `text_secondary`: every word that is no judgement against the session.
    Secondary,
    /// `text_tertiary`: an unread cell.
    Tertiary,
}

/// How `word` is drawn (§5.6's track table and activity table): the one function every view
/// calls, so a mark looks the same on the card, the navigator and a sheet. `done` takes the
/// secondary ink and never `success`, so a hue always means off track; an activity word is never
/// a judgement, so it never takes the hue (`stuck` included: its hue is its track mark's).
pub(crate) const fn look(word: Word<'_>) -> Look {
    let (form, ink) = match word {
        Word::Track(Some(Track::OffTrack)) => (Form::Diamond, Some(Ink::Danger)),
        Word::Track(Some(Track::AtRisk)) => (Form::HollowDiamond, Some(Ink::Primary)),
        Word::Track(Some(Track::Done)) => (Form::Check, Some(Ink::Secondary)),
        Word::Track(Some(Track::OnTrack)) => (Form::Disc, Some(Ink::Secondary)),
        Word::Track(Some(Track::Untracked)) => (Form::Nothing, None),
        Word::Track(None) | Word::Activity(None) => (Form::Unread, Some(Ink::Tertiary)),
        Word::Activity(Some(Activity::Working)) => (Form::Play, Some(Ink::Secondary)),
        Word::Activity(Some(Activity::Quiet | Activity::Stuck)) => {
            (Form::Pause, Some(Ink::Secondary))
        }
        Word::Activity(Some(Activity::Idle)) => (Form::Dash, Some(Ink::Secondary)),
        Word::Activity(Some(Activity::Ended)) => (Form::Square, Some(Ink::Secondary)),
        Word::Event(_) => (Form::Event, Some(Ink::Secondary)),
    };
    Look { form, ink }
}

/// The colour `ink` paints in `roles`: its role's colour held to [`MARK_INK_CONTRAST`] on every
/// ground a view sets a mark on (the page, a card or popover, a grouped form's card), and on each
/// of those under the wash a hovered row lays over it. One ink per word per palette, so the paint
/// value carries no ground and a mark reads the same wherever it sits.
pub(crate) fn ink_rgb(ink: Ink, roles: &Roles) -> [u8; 3] {
    let inks = HELD_INKS.with(|kept| match kept.get() {
        Some((at, inks)) if at == *roles => inks,
        _ => {
            let inks = held_inks(roles);
            kept.set(Some((*roles, inks)));
            inks
        }
    });
    inks[match ink {
        Ink::Danger => 0,
        Ink::Primary => 1,
        Ink::Secondary => 2,
        Ink::Tertiary => 3,
    }]
}

/// The four inks of `roles`, each held ([`held`]), in the order [`Ink`] lists them.
fn held_inks(roles: &Roles) -> [[u8; 3]; 4] {
    [
        roles.danger,
        roles.text_primary,
        roles.text_secondary,
        roles.text_tertiary,
    ]
    .map(|role| held(role, roles))
}

thread_local! {
    /// The palette this thread last painted marks in, and its held inks. A tray paints every
    /// mark in one `Roles`, and holding an ink is a contrast search that costs more than the
    /// mark it paints, so the search runs once a palette, never once a mark.
    static HELD_INKS: Box<std::cell::Cell<Option<(Roles, [[u8; 3]; 4])>>> =
        Box::new(std::cell::Cell::new(None));
}

/// The grounds a mark sits on in `roles`: the page, a card or popover, a grouped form's card,
/// each bare and under a hovered row's wash of the primary ink, blended in sRGB and in linear
/// light.
fn grounds(roles: &Roles) -> [[u8; 3]; 9] {
    let wash = DISCLOSURE_HOVER_WASH;
    let ink = roles.text_primary;
    let [page, card, grouped] = [roles.surface, roles.elevated, roles.grouped().card];
    let under = |ground| {
        [
            ground,
            mix_srgb(ground, ink, wash),
            mix_linear(ground, ink, wash),
        ]
    };
    let ([a, b, c], [d, e, f], [g, h, i]) = (under(page), under(card), under(grouped));
    [a, b, c, d, e, f, g, h, i]
}

/// `rgb` moved in lightness, its hue kept, until it reads [`MARK_INK_CONTRAST`] on every one of
/// [`grounds`].
fn held(rgb: [u8; 3], roles: &Roles) -> [u8; 3] {
    ensure_contrast_on(rgb, &grounds(roles), MARK_INK_CONTRAST)
}

/// The width of a form's outline: 1.5 pt, 2 under High Contrast (as the setup screen's rims).
fn stroke(high_contrast: bool) -> f32 {
    if high_contrast { 2.0 } else { 1.5 }
}

/// The dash's width in every look. A lone level line shows its ink on a 1x display only where
/// both its edges fall on whole pixels: about a centre on a whole point, a 1.5 pt line covers
/// each of its two pixel rows three quarters, and no pixel reaches the ink.
const DASH_WIDTH: f32 = 2.0;

/// How far apart a filled form's rows of fill are.
const FILL_STEP: f32 = 0.5;

/// How wide each row of fill is. The raster covers a pixel by how far its centre lies from a
/// row's middle line, so a pixel is solid only within `width / 2 - 0.5` px of a row. Rows
/// [`FILL_STEP`] apart leave every pixel centre within `FILL_STEP / 2` of one, so the width must
/// be at least `1 + FILL_STEP` device px. That holds at 1x and at every scale of 1 or more,
/// wherever the rect lands. The width must also be no more than `stroke(false)` (1.5), so each
/// row's round cap stays within the outline's own reach.
const FILL_WIDTH: f32 = 1.0 + FILL_STEP;

/// The event mark's corners, in units of its radius: a right-pointing triangle wider and taller
/// for its slot than working's ([`Form::Play`]), so that in the 12 pt Caption slot under High
/// Contrast's 2 pt outline its inradius (0.44 r, 2.6 pt) leaves a hollow of several pixels at 1x
/// wherever it lands, and its form differs from working's filled one in grey at either size. It
/// reaches 5.5 pt from the centre with the outline's round caps, inside the 6 pt half-slot.
const EVENT: [(f32, f32); 3] = [(-0.6, -0.75), (-0.6, 0.75), (0.75, 0.0)];

/// `form` in `ink`, centred in `rect` with a radius of half its shorter side. A filled form is
/// rows of [`DrawPrim::Line`]s under its outline, since no prim fills a polygon. No form moves.
pub(crate) fn paint_form(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    form: Form,
    ink: [u8; 3],
    high_contrast: bool,
) {
    let r = rect.width.min(rect.height) / 2.0;
    if r <= 0.0 || !r.is_finite() {
        return;
    }
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let s = stroke(high_contrast);
    let color = rgba(ink, 255);
    let at = |(dx, dy): (f32, f32)| (cx + dx * r, cy + dy * r);
    let line = |prims: &mut Vec<DrawPrim>, (x1, y1): (f32, f32), (x2, y2): (f32, f32), width| {
        prims.push(DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            color,
        });
    };
    let outline = |prims: &mut Vec<DrawPrim>, corners: &[(f32, f32)]| {
        for (i, &from) in corners.iter().enumerate() {
            let to = corners[(i + 1) % corners.len()];
            line(prims, at(from), at(to), s);
        }
    };
    match form {
        Form::Diamond | Form::HollowDiamond => {
            let tip = 0.9;
            if form == Form::Diamond {
                let reach = tip * r;
                let mut dy = -reach;
                while dy <= reach {
                    let half = reach - dy.abs();
                    line(
                        prims,
                        (cx - half, cy + dy),
                        (cx + half, cy + dy),
                        FILL_WIDTH,
                    );
                    dy += FILL_STEP;
                }
            }
            outline(prims, &[(0.0, -tip), (tip, 0.0), (0.0, tip), (-tip, 0.0)]);
        }
        Form::Check => {
            let width = s.max(0.22 * r);
            line(prims, at((-0.6, 0.0)), at((-0.15, 0.45)), width);
            line(prims, at((-0.15, 0.45)), at((0.65, -0.45)), width);
        }
        Form::Disc => prims.push(DrawPrim::Dot {
            cx,
            cy,
            r: 0.55 * r,
            color,
        }),
        Form::Play => {
            // The upright side's outer edge on a whole point, so at 1x its outline fills whole
            // pixel columns (-0.53125 r at r = 8 with a 1.5 pt outline; -0.5 r under High
            // Contrast, whose 2 pt outline already lands there).
            let left = ((cx - 0.5 * r - s / 2.0).round() + s / 2.0 - cx) / r;
            let (right, half) = (0.65 * r, 0.65 * r);
            let mut dx = left * r;
            while dx <= right {
                let reach = half * (right - dx) / (right - left * r);
                line(
                    prims,
                    (cx + dx, cy - reach),
                    (cx + dx, cy + reach),
                    FILL_WIDTH,
                );
                dx += FILL_STEP;
            }
            outline(prims, &[(left, -0.65), (left, 0.65), (0.65, 0.0)]);
        }
        Form::Pause => {
            // Whole-point bars as panels, which the raster puts on the device grid: on a 1x
            // display a bar shows hard edges only where both its edges fall on whole pixels, as
            // the dash's do.
            let bar = (0.26 * r).round().max(s.round()); // 2.0 at r = 8, in every look
            let reach = (0.6 * r).round(); // 5.0 at r = 8
            for x in [-bar, bar] {
                // Bar centres at cx ± bar: a gap of one bar between them.
                prims.push(DrawPrim::Panel {
                    x: cx + x - bar / 2.0,
                    y: cy - reach,
                    w: bar,
                    h: 2.0 * reach,
                    radius: 0.5,
                    fill: color,
                });
            }
        }
        Form::Dash => line(prims, at((-0.55, 0.0)), at((0.55, 0.0)), s.max(DASH_WIDTH)),
        Form::Square => {
            let side = 0.7 * r;
            prims.push(DrawPrim::Panel {
                x: cx - side / 2.0,
                y: cy - side / 2.0,
                w: side,
                h: side,
                radius: 0.5,
                fill: color,
            });
        }
        Form::Event => outline(prims, &EVENT),
        Form::Unread => {
            let (bowl_x, bowl_y, bowl_r) = (cx, cy - 0.2 * r, 0.32 * r);
            let (from, to) = (-0.25_f32, 0.375_f32);
            prims.push(DrawPrim::Arc {
                cx: bowl_x,
                cy: bowl_y,
                r: bowl_r,
                width: s,
                from,
                to,
                color,
            });
            let turn = to * std::f32::consts::TAU;
            let end = (bowl_x + turn.sin() * bowl_r, bowl_y - turn.cos() * bowl_r);
            line(prims, end, at((0.0, 0.2)), s);
            prims.push(DrawPrim::Dot {
                cx,
                cy: cy + 0.55 * r,
                r: (0.12 * r).max(1.0),
                color,
            });
        }
        Form::Nothing => {}
    }
}

/// What a mark of `word` speaks: its word, as a person reads it (`off track`, `quiet`), and
/// `blocked on a person` for the event `blocked`. A track mark speaks the track word alone: its
/// reason is text the view draws and speaks beside it. `None` for a session with no track, whose
/// slot speaks nothing.
#[cfg_attr(
    not(debug_assertions),
    expect(
        dead_code,
        reason = "no view of a release build places a mark yet; the debug specimen does"
    )
)]
pub(crate) fn spoken(word: Word<'_>) -> Option<String> {
    let said = match word {
        Word::Track(Some(Track::OffTrack)) => "off track",
        Word::Track(Some(Track::AtRisk)) => "at risk",
        Word::Track(Some(Track::Done)) => "done",
        Word::Track(Some(Track::OnTrack)) => "on track",
        Word::Track(Some(Track::Untracked)) => return None,
        Word::Track(None) => "track unread",
        Word::Activity(Some(activity)) => activity.word(),
        Word::Activity(None) => "activity unread",
        Word::Event("blocked") => "blocked on a person",
        Word::Event(event) => event,
    };
    Some(said.to_owned())
}

/// A mark of `word` in a square `slot` across ([`MARK_TITLE`], [`MARK_CAPTION`]): a Custom node
/// that paints its form and speaks its word, as Text and never as Status (a status is a live
/// region, which would announce every change of a mark; a screen reader hears the word when it
/// reaches the mark). It paints no glyph of its word: a view that draws the word beside it draws
/// that word paint-only, so it is heard once. A session with no track gets a silent paint-only
/// slot of the same size, so the topic beside it keeps its column.
#[cfg_attr(
    not(debug_assertions),
    expect(
        dead_code,
        reason = "no view of a release build places a mark yet; the debug specimen does"
    )
)]
pub(crate) fn mark_node(key: impl Into<Arc<str>>, word: Word<'_>, slot: f32) -> UiNode {
    let layout = Layout::default()
        .width(Length::Fixed(slot))
        .height(Length::Fixed(slot));
    let Some(label) = spoken(word) else {
        return UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(layout)
        .paint_only();
    };
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id: COMPREHENSION_MARK_AUDIT,
            role: SemanticRole::Text,
            label,
            paint: Some(word.value()),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: None,
        }),
    )
    .layout(layout)
}

/// A [`COMPREHENSION_MARK_AUDIT`] node's paint: the word its `value` names, in its form and its
/// ink, drawn in `rect`; a value that names no word paints nothing.
pub(crate) fn paint_mark(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let Some(word) = value.and_then(Word::parse) else {
        return;
    };
    let look = look(word);
    let Some(ink) = look.ink else {
        return;
    };
    let high_contrast = crate::native_appearance::current_preferences().high_contrast;
    paint_form(prims, rect, look.form, ink_rgb(ink, roles), high_contrast);
}

/// What a survey row says of one session, in today's wire words, each as the row spells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowWords<'a> {
    /// `state=`: `spawning`, `alive` or `exited`.
    pub(crate) state: Option<&'a str>,
    /// `program_kind=`: `agent` for an agent's row.
    pub(crate) program_kind: Option<&'a str>,
    /// `agent=`: the agent's own word.
    pub(crate) agent: Option<&'a str>,
    /// `needs=`, raw: `-` where the row read its needs and found none for a person.
    pub(crate) needs: Option<&'a str>,
    /// `task=`.
    pub(crate) task: Option<&'a str>,
    /// `task_conclusion=`.
    pub(crate) task_conclusion: Option<&'a str>,
}

/// The row as the fold reads it: its glance word as the glance reads it ([`glance_of`]), the
/// same row with its need set aside (`needs=-`, so an agent at a prompt or a question is at
/// work, never waiting), its task word, and whether it has ended. A row that is no agent's has
/// no glance word: it is `unknown`, and the fallback leaves its activity unread.
pub(crate) fn seen_of_row(row: &RowWords<'_>) -> Seen {
    let task = TaskWord::of(row.task, row.task_conclusion);
    let glance = |needs: Option<&str>| {
        AgentWord::of_row(row.program_kind, row.agent, needs)
            .map_or(State::Unknown, |agent| glance_of(&agent, task).0)
    };
    Seen {
        glance: glance(row.needs),
        aside: glance(Some("-")),
        task: task.map(TaskWord::fleet),
        ended: row.state == Some("exited"),
    }
}

/// This Mac's activity word for one of its sessions, under the track rule's `verdict` (`None`
/// until this Mac derives one): the reader's fallback ([`aterm_db::words::activity`]), so a
/// session reads the same word here as on every Mac that reads its row. The navigator's
/// row-state function, where a build has one, gives this Mac's sessions their word in place of
/// the fallback.
pub(crate) fn activity_here(row: &RowWords<'_>, verdict: Option<TrackWhy>) -> Option<Activity> {
    aterm_db::words::activity(&seen_of_row(row), verdict)
}

#[cfg(test)]
#[path = "comprehension_words_tests.rs"]
mod tests;
