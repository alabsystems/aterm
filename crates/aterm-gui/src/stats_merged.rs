// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The merged deck's painter (`DESIGN-top-deck-merged-2026-10-21.md` §3, §11): its skeleton,
//! where [`MergedPlanner`] lays it. The ground and the seam along its foot; the all-tabs and
//! this-tab lanes' tints from the rail's rule to the bands' end; the rules between the rail, the
//! blocks and the divider; row A's block names, their capacity words and the orange words past
//! capacity; the band names, once; and a hit region for each block in each lane. The blocks'
//! marks are drawn into these slots ([`merged_marks`](crate::merged_marks)), so nothing here
//! moves when one lands: so far the CPU's in the Mac lane.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use aterm_vitals::glance::cpu::{self, CpuGlance};
use aterm_vitals::glance::network;
use aterm_vitals::ink::{MergedInks, Rgb, VitalsInks};
use aterm_vitals::model::{HeatCell, Resource, Snapshot};
use aterm_vitals::words::{self, DeckScope, Platform};

#[cfg(test)]
use crate::deck_face::Boxed;
use crate::deck_face::{ASCENT_EM, BoxKind, Boxes, DESCENT_EM, FaceMetrics, deck_metrics};
use crate::merged_marks::{GaugeLook, Marks, RUN_BARS, Reading};
use crate::merged_plan::{
    self, Band, BlockKind, Grid, Lane, MachineShape, MergedForm, MergedPlan, MergedPlanner,
    RuleKind, Run, Span, Tiles,
};
use crate::stats_deck::{DeckGeom, DeckInput, MERGED_MIN_PT};
use crate::stats_paint::{Bar, Hit, Instrument, Painter, Rect, a, panel};
use crate::type_scale::{StepPx, TypeStep};
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// Row A's baseline sits this far above the row's foot (the mockups' 18 px of 23 at 2×), in
/// points at the mockups' height, like every length here.
const ROW_A_BASE: f64 = 2.5;
/// A lane's tint starts this far below its lane's top, and rounds its corners this much.
const TINT_INSET: f64 = 0.5;
const TINT_RADIUS: f64 = 1.5;
/// How far a rule stops inside the deck's top and its last lane's foot: the rail's, and the
/// blocks' (the mockups' 4 and 3 px, and 5 and 4 px, at 2×).
const RAIL_INSET: (f64, f64) = (2.0, 1.5);
const RULE_INSET: (f64, f64) = (2.5, 2.0);
/// A rule the bands run on under, or one past the divider, is this much fainter.
const RULE_FAINT: f32 = 0.8;
/// The tree's stroke from the Mac's name to all tabs' and from all tabs' to this tab's.
const TREE_W: f64 = 0.75;
/// Rule F's tolerance, in points.
const EPS: f64 = 0.01;
/// The seconds a 10-minute run covers, its bars dividing them.
const TEN_MINUTES_S: u32 = 600;

/// The resource a block reads.
const fn resource(kind: BlockKind) -> Resource {
    match kind {
        BlockKind::Processes => Resource::Processes,
        BlockKind::Cpu => Resource::Cpu,
        BlockKind::Memory => Resource::Memory,
        BlockKind::Gpu => Resource::Gpu,
        BlockKind::Disk => Resource::Disk,
        BlockKind::Network => Resource::Network,
    }
}

/// Whether a block reads each scope: the bands run under it (§3).
const fn scoped(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Processes | BlockKind::Cpu | BlockKind::Memory | BlockKind::Gpu
    )
}

/// The three-row deck a card `g` belongs to, in device px, and whether the card shows row A
/// alone: a card shorter than the least three-row deck is row A's, laid as the least deck.
fn deck_of(g: &DeckGeom) -> (usize, bool) {
    let least = (MERGED_MIN_PT * f64::from(g.scale)).round().max(0.0);
    let h = f64::from(g.h).max(0.0);
    if h >= least {
        (h as usize, false)
    } else {
        (least as usize, true)
    }
}

/// The planner for a three-row deck `deck_px` tall at `scale` on `shape` in the face `m`, kept
/// for the last four such decks: it measures every template, so it is built once, not per
/// reading.
fn planner_for(
    deck_px: usize,
    scale: f64,
    shape: MachineShape,
    m: &Arc<FaceMetrics>,
) -> Rc<MergedPlanner> {
    type Key = (usize, u64, MachineShape, Arc<FaceMetrics>);
    thread_local! {
        static HELD: RefCell<Vec<(Key, Rc<MergedPlanner>)>> = const { RefCell::new(Vec::new()) };
    }
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        let found = held.iter().find(|((px, s, sh, face), _)| {
            *px == deck_px && *s == scale.to_bits() && *sh == shape && Arc::ptr_eq(face, m)
        });
        if let Some((_, planner)) = found {
            return Rc::clone(planner);
        }
        let planner = Rc::new(MergedPlanner::new(deck_px, scale, shape.clone(), m));
        if held.len() >= 4 {
            held.remove(0);
        }
        held.push((
            (deck_px, scale.to_bits(), shape, Arc::clone(m)),
            Rc::clone(&planner),
        ));
        planner
    })
}

/// Paint the merged deck: its skeleton where the plan for the card's width lays it, and its hit
/// regions.
pub(crate) fn merged_paint(g: &DeckGeom, input: &DeckInput<'_>) -> (Vec<DrawPrim>, Vec<Hit>) {
    let m = deck_metrics();
    let (deck_px, _) = deck_of(g);
    let shape = MachineShape::of(input.snap, input.locale.platform);
    let planner = planner_for(deck_px, f64::from(g.scale), shape, &m);
    let mut card = Card::new(g, input, &m, &planner, true);
    card.paint(input.snap);
    (card.p.prims, card.hits)
}

/// A merged deck laid out as the tests read it.
#[cfg(test)]
pub(crate) struct MergedLaid {
    pub(crate) prims: Vec<DrawPrim>,
    pub(crate) hits: Vec<Hit>,
    pub(crate) boxes: Vec<Boxed>,
    pub(crate) plan: MergedPlan,
}

/// [`merged_paint`] with `planner` (built for the card's three-row deck) and a face's metrics,
/// recording every box; `draw: false` pushes no prims.
#[cfg(test)]
pub(crate) fn merged_lay(
    planner: &MergedPlanner,
    g: &DeckGeom,
    input: &DeckInput<'_>,
    m: &FaceMetrics,
    draw: bool,
) -> MergedLaid {
    let mut card = Card::new(g, input, m, planner, draw);
    card.boxes.record();
    card.paint(input.snap);
    MergedLaid {
        boxes: card.boxes.take(),
        prims: card.p.prims,
        hits: card.hits,
        plan: card.plan,
    }
}

/// One card being painted.
struct Card<'a> {
    p: Painter<'a>,
    inks: &'a VitalsInks,
    /// The merged inks, Increase Contrast's under it.
    k: MergedInks,
    /// Increase Contrast.
    ic: bool,
    m: &'a FaceMetrics,
    plan: MergedPlan,
    /// Device px per point, and the card in device px.
    s: f64,
    w_px: f32,
    h_px: f32,
    platform: Platform,
    draw: bool,
    hits: Vec<Hit>,
    boxes: Boxes,
}

impl<'a> Card<'a> {
    fn new(
        g: &DeckGeom,
        input: &'a DeckInput<'a>,
        m: &'a FaceMetrics,
        planner: &MergedPlanner,
        draw: bool,
    ) -> Self {
        let s = f64::from(g.scale.max(0.5));
        let (_, row_a) = deck_of(g);
        Self {
            p: Painter::new(&input.inks, g.scale, g.font_px, input.dwc),
            inks: &input.inks,
            k: if input.contrast {
                input.inks.merged_ic
            } else {
                input.inks.merged
            },
            ic: input.contrast,
            m,
            plan: planner.plan(f64::from(g.w) / s, row_a),
            s,
            w_px: g.w.max(0.0),
            h_px: g.h.max(0.0),
            platform: input.locale.platform,
            draw,
            hits: Vec::new(),
            boxes: Boxes::default(),
        }
    }

    fn paint(&mut self, snap: Option<&Snapshot>) {
        let cpu = snap.and_then(|s| CpuGlance::of(s, self.platform));
        self.ground();
        self.tints();
        self.rules();
        self.row_a(snap, cpu.as_ref());
        self.band_names();
        self.cpu(snap, cpu.as_ref());
        self.network(snap);
        self.hit_regions();
    }

    /// `pt` points at the mockups' height, at this deck's.
    fn sp(&self, pt: f64) -> f64 {
        pt * self.plan.k
    }

    fn px(&self, v: f64) -> f32 {
        (v * self.s + 0.5).floor() as f32
    }

    fn px_rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        let (x, y) = (self.px(x0), self.px(y0));
        (x, y, self.px(x1) - x, self.px(y1) - y)
    }

    /// A stroke `pt` wide in whole device px, never under one.
    fn stroke_px(&self, pt: f64) -> f32 {
        ((pt * self.s).floor() as f32).max(1.0)
    }

    fn fill(&mut self, (x, y, w, h): Rect, radius: f32, ink: Rgb, alpha: f32) {
        if self.draw && w > 0.0 && h > 0.0 {
            self.p.prims.push(panel(x, y, w, h, radius, a(ink, alpha)));
        }
    }

    /// `s` at `(x, base)` in points, in the deck's face; answers its advance. `room` is the
    /// column it must stay inside.
    #[expect(
        clippy::too_many_arguments,
        reason = "a text run's place, size, weight, ink and column"
    )]
    fn text(
        &mut self,
        x: f64,
        base: f64,
        s: &str,
        pt: f64,
        bold: bool,
        ink: Rgb,
        room: Option<(f64, f64)>,
    ) -> f64 {
        if s.is_empty() {
            return 0.0;
        }
        let w = self.m.width(s, pt, bold);
        self.boxes.note(
            BoxKind::Text,
            [x, base - ASCENT_EM * pt, x + w, base + DESCENT_EM * pt],
            s,
            room,
        );
        if self.draw {
            let px = (pt * self.s) as f32;
            self.p.prims.push(text_prim(
                self.px(x),
                self.px(base),
                s.to_owned(),
                TypeStep::Caption.px_clamped(px, px, px),
                TextWeight::Regular,
                if bold { TextFace::UiBold } else { TextFace::Ui },
                rgba(ink, 0xFF),
            ));
        }
        w
    }

    /// The lane `lane` of the plan, where it lays one.
    fn lane(&self, lane: Lane) -> Option<Band> {
        let scopes = self.plan.lanes.scopes;
        match lane {
            Lane::A => Some(self.plan.lanes.a),
            Lane::Mac => scopes.map(|s| s[0]),
            Lane::AllTabs => scopes.map(|s| s[1]),
            Lane::ThisTab => scopes.map(|s| s[2]),
        }
    }

    /// The ground over the card, and the seam along its foot.
    fn ground(&mut self) {
        let (w, h) = (self.w_px, self.h_px);
        let seam = ((self.plan.lanes.seam.h * self.s).round() as f32)
            .max(1.0)
            .min(h);
        self.fill((0.0, 0.0, w, h - seam), 0.0, self.inks.ground, 1.0);
        self.fill((0.0, h - seam, w, seam), 0.0, self.inks.seam, 1.0);
    }

    /// The all-tabs and this-tab lanes' tints, from the rail's rule to the bands' end.
    fn tints(&mut self) {
        let (Some(scopes), Some(bands)) = (self.plan.lanes.scopes, self.plan.bands) else {
            return;
        };
        let inset = self.sp(TINT_INSET);
        let radius = (self.sp(TINT_RADIUS) * self.s) as f32;
        for (lane, ink) in [
            (scopes[1], self.k.all_tabs_tint),
            (scopes[2], self.k.this_tab_tint),
        ] {
            let r = self.px_rect(bands.x, lane.y + inset, bands.end(), lane.end());
            self.fill(r, radius, ink, 1.0);
        }
    }

    /// Each rule from its top lane to its bottom one, inside the deck's top and its last lane's
    /// foot; under row A alone, only the rules row A crosses, down to its foot.
    fn rules(&mut self) {
        let row_a = self.plan.lanes.scopes.is_none();
        let bands_end = self.plan.bands.map(|b| b.end());
        for r in self.plan.rules.clone() {
            let Some(top) = self.lane(r.top) else {
                continue;
            };
            let (bottom, last) = if row_a {
                (self.plan.lanes.a, true)
            } else {
                match self.lane(r.bottom) {
                    Some(b) => (b, r.bottom == Lane::ThisTab),
                    None => continue,
                }
            };
            let (in_top, in_bottom) = if r.kind == RuleKind::Rail {
                RAIL_INSET
            } else {
                RULE_INSET
            };
            let y0 = top.y
                + if r.top == Lane::A {
                    self.sp(in_top)
                } else {
                    0.0
                };
            let y1 = bottom.end() - if last { self.sp(in_bottom) } else { 0.0 };
            let faint = r.kind == RuleKind::Block
                && (r.bottom == Lane::Mac || bands_end.is_some_and(|end| r.at.x >= end));
            let x = self.px(r.at.x);
            let (y0p, y1p) = (self.px(y0), self.px(y1));
            let w = self.stroke_px(r.at.w);
            let alpha = if faint { RULE_FAINT } else { 1.0 };
            self.fill((x, y0p, w, y1p - y0p), 0.0, self.inks.grid, alpha);
            self.boxes
                .note(BoxKind::Bar, [r.at.x, y0, r.at.end(), y1], "rule", None);
        }
    }

    /// Row A: each block's name in bold and the words after it (its capacity in secondary
    /// ink, or Processes' key), then its orange words while it is past capacity (§4: CPU's
    /// `~8 waiting`), in the room the plan gives its head; words that do not fit are not drawn.
    fn row_a(&mut self, snap: Option<&Snapshot>, cpu: Option<&CpuGlance>) {
        let base = self.plan.lanes.a.end() - self.sp(ROW_A_BASE);
        let (words_pt, names_pt) = (self.plan.text.words, self.plan.text.names);
        let laid = self.plan.laid;
        let heads: Vec<(BlockKind, Span)> =
            self.plan.blocks.iter().map(|b| (b.kind, b.head)).collect();
        for (kind, head) in heads {
            let (name, after) = head_words(kind, laid, snap);
            let room = (head.x, head.end());
            let name = self.m.fit(&[name], head.w, names_pt, true);
            let ink = self.inks.words;
            let mut end = head.x + self.text(head.x, base, &name, names_pt, true, ink, Some(room));
            if let Some(after) = after {
                let x = end + self.sp(merged_plan::after_name(kind, laid));
                if x + self.m.width(&after, words_pt, false) <= room.1 + EPS {
                    let ink = self.k.secondary;
                    end = x + self.text(x, base, &after, words_pt, false, ink, Some(room));
                }
            }
            let over = match kind {
                BlockKind::Cpu => cpu.and_then(|g| g.words.clone()),
                _ => None,
            };
            let Some(over) = over else {
                continue;
            };
            let x = end + self.sp(merged_plan::OVER_GAP);
            if x + self.m.width(&over, words_pt, false) <= room.1 + EPS {
                let ink = self.k.over_text;
                self.text(x, base, &over, words_pt, false, ink, Some(room));
            }
        }
    }

    /// The bands' names, once: Full's in the Processes counts' column, indented as a tree
    /// (all tabs under the Mac, this tab under all tabs); Mid's in the Processes names' column;
    /// Narrow's all tabs and this tab under the CPU's cores.
    fn band_names(&mut self) {
        let Some(scopes) = self.plan.lanes.scopes else {
            return;
        };
        let column = |kind: BlockKind, counts: bool| {
            self.plan
                .block(kind)
                .and_then(|b| if counts { b.counts } else { b.names })
        };
        let (col, from, tree) = match self.plan.laid {
            MergedForm::Full => (column(BlockKind::Processes, true), 0, true),
            MergedForm::Mid => (column(BlockKind::Processes, false), 0, false),
            MergedForm::Narrow => (column(BlockKind::Cpu, false), 1, false),
            MergedForm::Rail | MergedForm::RowA => (None, 0, false),
        };
        let Some(col) = col else {
            return;
        };
        let pt = self.plan.text.words;
        let base = |lane: Band| lane.y + lane.h / 2.0 + ASCENT_EM * pt / 2.0;
        let indent = self.sp(merged_plan::INDENT);
        for (depth, scope) in DeckScope::ALL.into_iter().enumerate().skip(from) {
            let lane = scopes[depth];
            let ink = match scope {
                DeckScope::Mac => self.inks.words,
                DeckScope::AllTabs => self.k.all_tabs,
                DeckScope::ThisTab => self.k.this_tab,
            };
            let depth_f = if tree { depth as f64 } else { 0.0 };
            let x = col.x + depth_f * indent;
            let word = scope.word(self.platform);
            self.text(x, base(lane), word, pt, true, ink, Some((col.x, col.end())));
            if tree && depth > 0 {
                let above = base(scopes[depth - 1]) + DESCENT_EM * pt;
                self.tree(x - indent / 2.0, x, above, lane.y + lane.h / 2.0);
            }
        }
    }

    /// Network's score in the Mac lane (§5): its gauge, jagged at its top while the last 20 s
    /// swing, and its figure; then its 10-minute run, the run's ends named beside it. Until the
    /// row is read against a rate, its gauge is an absence (dashed, no figure) and its bars hollow.
    fn network(&mut self, snap: Option<&Snapshot>) {
        let (Some(b), Some(scopes)) = (self.plan.block(BlockKind::Network), self.plan.lanes.scopes)
        else {
            return;
        };
        let (Some(gauge), Some(fig), Some(run)) = (b.gauge, b.fig, b.run) else {
            return;
        };
        let mac = scopes[0];
        let view = snap.map(|s| (&s.resources[Resource::Network.index()], s.at_unix_ms));
        let level = view.and_then(|(v, _)| v.heat_now).filter(|l| l.is_finite());
        let pt = self.plan.text.words;
        let base = mac.y + mac.h / 2.0 + ASCENT_EM * pt / 2.0;
        let r = [gauge.x, mac.y, gauge.end(), mac.end()];
        self.boxes.note(BoxKind::Mark, r, "Network's gauge", None);
        if self.draw {
            let g = self.px_rect(r[0], r[1], r[2], r[3]);
            let mut mk = Marks {
                p: &mut self.p,
                k: self.k,
            };
            match level {
                Some(level) => {
                    let spread = view.map_or(0.0, |(v, now)| network::swing(v, now));
                    let m = Reading {
                        spread,
                        ..Reading::still(level)
                    };
                    mk.gauge(g, m, GaugeLook::default());
                }
                None => mk.hollow(g),
            }
        }
        if let Some(level) = level {
            let (figure, ink) = (words::pct(Some(level)), self.inks.words);
            self.text(
                fig.x,
                base,
                &figure,
                pt,
                true,
                ink,
                Some((fig.x, fig.end())),
            );
        }
        // A run whose ends cannot both be named is not drawn (§4).
        let ((oldest, newest), [at_oldest, at_newest]) = (run.span.ends(), run.ends);
        let fits = |s: &str, room: Span| self.m.width(s, pt, false) <= room.w + EPS;
        if !(fits(oldest, at_oldest) && fits(newest, at_newest)) {
            return;
        }
        let axis = self.k.axis;
        for (s, at) in [(oldest, at_oldest), (newest, at_newest)] {
            self.text(at.x, base, s, pt, false, axis, Some((at.x, at.end())));
        }
        let r = [run.at.x, mac.y, run.at.end(), mac.end()];
        self.boxes.note(BoxKind::Mark, r, "Network's run", None);
        if self.draw {
            let per_s = TEN_MINUTES_S / u32::try_from(run.bars.max(1)).unwrap_or(u32::MAX);
            let cells = view.map_or_else(
                || vec![HeatCell::default(); run.bars],
                |(v, now)| v.heat_columns(per_s, run.bars, now),
            );
            let bars = self.px_rect(r[0], r[1], r[2], r[3]);
            Marks {
                p: &mut self.p,
                k: self.k,
            }
            .run(bars, &cells);
        }
    }

    /// A tree's corner: down at `xv` from `y0` to `y1`, then right to the name at `x1`.
    fn tree(&mut self, xv: f64, x1: f64, y0: f64, y1: f64) {
        if y1 <= y0 {
            return;
        }
        let t = self.sp(TREE_W);
        let tp = self.stroke_px(t);
        let vx = self.px(xv - t / 2.0);
        let (vy, hy) = (self.px(y0), self.px(y1 - t / 2.0));
        let ink = self.k.secondary;
        self.fill((vx, vy, tp, hy + tp - vy), 0.0, ink, 1.0);
        self.fill((vx, hy, self.px(x1) - vx, tp), 0.0, ink, 1.0);
        self.boxes.note(
            BoxKind::Mark,
            [xv - t / 2.0, y0, x1, y1 + t / 2.0],
            "tree",
            None,
        );
    }

    /// The CPU block's Mac lane (§5): a tile per core, kind by kind as the OS lists them, each
    /// kind led by its letter where there are several (a tank across the parts where tiles would
    /// be too thin); the dashed wall and a slat per core's worth waiting past it; in Full the
    /// score gauge; the figure; and in Full the 10-minute run, drawn only where its two ends are
    /// named over it. Before the CPU is read its parts and gauge are dashed, its run hollow, and
    /// nothing of it is written (§11).
    fn cpu(&mut self, snap: Option<&Snapshot>, g: Option<&CpuGlance>) {
        let (Some(scopes), Some(b)) = (self.plan.lanes.scopes, self.plan.block(BlockKind::Cpu))
        else {
            return;
        };
        let b = b.clone();
        let mac = scopes[0];
        let pt = self.plan.text.words;
        let (top, foot) = (self.px(mac.y), self.px(mac.end()));
        let lane_rect = |c: &Self, s: Span| {
            let x = c.px(s.x);
            (x, top, c.px(s.end()) - x, foot - top)
        };
        let mark = |c: &mut Self, s: Span, what: &str| {
            c.boxes
                .note(BoxKind::Mark, [s.x, mac.y, s.end(), mac.end()], what, None);
        };
        // Each tile with its reading (`None`: unread), and the parts drawn whole where they
        // are not divided into tiles.
        let mut tiles: Vec<(Rect, Option<Reading>)> = Vec::new();
        let mut whole: Option<(Rect, Option<Reading>)> = None;
        let busy = g.map(|g| Reading::of(&g.busy, g.cores as f32, words::CORES_GATE));
        if let Some(parts) = b.parts {
            match (&b.grid, g) {
                (Some(Grid::Tiles { groups, .. }), Some(g)) => {
                    for (i, t) in groups.iter().enumerate() {
                        if let Some((letter, at)) = &t.letter {
                            let base = mac.end() - mac.h * t.tall / 2.0 + ASCENT_EM * pt / 2.0;
                            let (ink, room) = (self.k.secondary, Some((at.x, at.end())));
                            self.text(at.x, base, letter, pt, true, ink, room);
                        }
                        let loads = g.kinds.get(i).map_or(&[][..], |k| &k.loads[..]);
                        let read = |col: usize| {
                            loads
                                .get(col)
                                .map(|l| Reading::of(l, 1.0, words::LOAD_GATE))
                        };
                        tiles.extend(
                            self.tile_rects(t, mac)
                                .into_iter()
                                .enumerate()
                                .map(|(col, r)| (r, read(col))),
                        );
                        mark(self, Span::new(t.x, t.end() - t.x), "CPU tiles");
                    }
                }
                _ => {
                    whole = Some((lane_rect(self, parts), busy));
                    mark(self, parts, "CPU parts");
                }
            }
        }
        let wall = b.wall.filter(|_| g.is_some()).map(|x| {
            let w = self.sp(0.5);
            mark(self, Span::new(x - w, 2.0 * w), "CPU wall");
            self.px(x)
        });
        let slats = b.over.zip(g.filter(|g| g.waiting > 0.0)).map(|(at, g)| {
            mark(self, at, "CPU slats");
            (lane_rect(self, at), g.waiting)
        });
        let gauge = b.gauge.map(|at| {
            mark(self, at, "CPU gauge");
            lane_rect(self, at)
        });
        if let (Some(fig), Some(score)) = (b.fig, g.and_then(|g| g.score)) {
            let base = mac.y + mac.h / 2.0 + ASCENT_EM * pt / 2.0;
            let ink = self.inks.words;
            self.text(
                fig.x,
                base,
                &words::pct(Some(score)),
                pt,
                true,
                ink,
                Some((fig.x, fig.end())),
            );
        }
        let run = b.run.map(|run| {
            mark(self, run.at, "CPU run");
            let base = self.plan.lanes.a.end() - self.sp(ROW_A_BASE);
            let (left, right) = run.span.ends();
            for (at, word) in run.ends.into_iter().zip([left, right]) {
                let r = [
                    at.x,
                    base - ASCENT_EM * pt,
                    at.end(),
                    base + DESCENT_EM * pt,
                ];
                self.boxes.note(BoxKind::Text, r, word, None);
            }
            let cells = snap.map_or_else(
                || vec![HeatCell::default(); RUN_BARS],
                |s| cpu::run(s, self.platform, RUN_BARS),
            );
            (self.run_ends_at(&run), lane_rect(self, run.at), cells)
        });
        if !self.draw {
            return;
        }
        let size = self.words_px();
        let m = self.m;
        let mut mk = Marks::new(&mut self.p, self.ic);
        for (r, reading) in tiles.into_iter().chain(whole) {
            match reading {
                Some(reading) => mk.part_tile(r, reading, None),
                None => mk.hollow(r),
            }
        }
        if let Some(x) = wall {
            mk.wall(x, top, foot);
        }
        if let Some((r, units)) = slats {
            mk.slats(r, units);
        }
        if let Some(r) = gauge {
            match (busy, g) {
                (Some(reading), Some(g)) => {
                    let look = GaugeLook {
                        over: g.over(),
                        ..GaugeLook::default()
                    };
                    mk.gauge(r, reading, look);
                }
                _ => mk.hollow(r),
            }
        }
        if let Some(((run_x, lead, base, ends), r, cells)) = run
            && mk.run_ends(run_x, lead, base, ends, size, m)
        {
            mk.run(r, &cells);
        }
    }

    /// How [`Marks::run_ends`] names `run`'s two ends over it: the run's edges and how far its
    /// oldest word leads its first bar, in device px, row A's baseline, and the two words.
    fn run_ends_at(&self, run: &Run) -> ((f32, f32), f32, f32, (&'static str, &'static str)) {
        let base = self.plan.lanes.a.end() - self.sp(ROW_A_BASE);
        let lead = ((run.at.x - run.ends[0].x) * self.s) as f32;
        let x = (self.px(run.at.x), self.px(run.at.end()));
        (x, lead, self.px(base), run.span.ends())
    }

    /// The words' size in device px, as [`Self::text`] draws them at the plan's words size.
    fn words_px(&self) -> StepPx {
        let px = (self.plan.text.words * self.s) as f32;
        TypeStep::Caption.px_clamped(px, px, px)
    }

    /// The tiles of `t` in `lane`, in whole device px: each `t.tall` of the lane on its foot,
    /// and a whole px narrower than its pitch, so no two neighbours touch.
    fn tile_rects(&self, t: &Tiles, lane: Band) -> Vec<Rect> {
        let pitch = t.pitch * self.s;
        let w = (t.w * self.s).round().min(pitch.floor() - 1.0).max(1.0) as f32;
        let (top, foot) = (self.px(lane.y), self.px(lane.end()));
        let h = ((foot - top) * t.tall as f32).round();
        (0..t.cols)
            .map(|c| (self.px(t.x + c as f64 * t.pitch), foot - h, w, h))
            .collect()
    }

    /// A hit region for each block in each lane it lays: row A and the Mac lane name the Mac,
    /// a scoped block's all-tabs lane aterm and its this-tab lane the front tab; Disk and Network
    /// read no scope, so theirs name the Mac in every lane.
    fn hit_regions(&mut self) {
        for b in &self.plan.blocks {
            let instrument = Instrument::of(resource(b.kind));
            for lane in Lane::ALL {
                let Some(band) = self.lane(lane) else {
                    continue;
                };
                let bar = match lane {
                    Lane::AllTabs if scoped(b.kind) => Bar::Aterm,
                    Lane::ThisTab if scoped(b.kind) => Bar::Tab,
                    _ => Bar::Global,
                };
                let r = self.px_rect(b.at.x, band.y, b.at.end(), band.end());
                self.hits.push(Hit::row(r, instrument, bar));
            }
        }
    }
}

/// A block's name in row A in the form `laid`, and the words after it: its capacity in Full
/// where it is read (Network's, the rate its basis names as 100%), or Processes' key.
fn head_words(
    kind: BlockKind,
    laid: MergedForm,
    snap: Option<&Snapshot>,
) -> (&'static str, Option<String>) {
    let full = laid == MergedForm::Full;
    let read = |f: fn(&Snapshot) -> Option<String>| snap.filter(|_| full).and_then(f);
    let name = words::resource_name(resource(kind));
    match kind {
        BlockKind::Processes => {
            let (name, key) = merged_plan::PROCESSES_HEAD[match laid {
                MergedForm::Full => 0,
                MergedForm::Mid => 1,
                _ => 2,
            }];
            (name, (!key.is_empty()).then(|| key.to_owned()))
        }
        BlockKind::Cpu => (
            name,
            read(|s| {
                s.cpu
                    .as_ref()
                    .filter(|c| c.total() > 0)
                    .map(words::cpu_capacity)
            }),
        ),
        BlockKind::Memory if !full => (merged_plan::MEMORY_SHORT, None),
        BlockKind::Memory => (
            name,
            read(|s| {
                s.mem
                    .as_ref()
                    .map(|m| m.sample.total)
                    .filter(|&t| t > 0)
                    .map(words::memory_capacity)
            }),
        ),
        BlockKind::Gpu => (
            name,
            read(|s| {
                s.gpu
                    .as_ref()
                    .and_then(|g| g.cores)
                    .filter(|&n| n > 0)
                    .map(words::gpu_capacity)
            }),
        ),
        BlockKind::Disk => (
            name,
            read(|s| {
                s.volume
                    .as_ref()
                    .map(|v| v.total)
                    .filter(|&t| t > 0)
                    .map(words::disk_capacity)
            }),
        ),
        BlockKind::Network => (
            name,
            read(|s| network::capacity_words(&s.resources[Resource::Network.index()])),
        ),
    }
}

/// The merged deck's accessibility group's name.
#[cfg(a11y_tree)]
const A11Y_TITLE: &str = "Stats deck";

/// The merged deck as a reader meets it: what needs the owner first (filled by the needs rail),
/// then a meter for each block in the deck's order, then the cores.
#[cfg(a11y_tree)]
pub(crate) fn merged_a11y_group(
    snap: Option<&Snapshot>,
    panes: &[u64],
    loc: &words::Locale,
) -> crate::a11y_tree::StatsGroup {
    let needs = crate::a11y_tree::StatsNeeds {
        name: words::NEEDS_RAIL_CALM.into(),
        needs: Vec::new(),
    };
    let blocks = MergedForm::Full.blocks().iter().map(|&kind| resource(kind));
    crate::stats_deck::stats_group(A11Y_TITLE, Some(needs), blocks, snap, panes, loc)
}

#[cfg(test)]
#[path = "stats_merged_tests.rs"]
mod tests;
