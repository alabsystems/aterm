// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resident across a coordinate seam.
//!
//! A window resize (every frame of a live drag), a font step, a DPI change, a zoom,
//! an alternate-screen enter or exit, and a tab or pane switch are coordinate
//! seams. The pet crosses each with a cat on every frame: a regrid keeps the body;
//! a new surface crossfades, the old body fading as a ghost on the glass it was
//! drawn on while the arriver comes up; an incoherent world observation holds the
//! frame, and a sub-row scroll is not incoherent.
//!
//! The laws drive the shipping `CompanionOwner` over a real `Terminal` in the
//! native frame order (observe, prepare, tick, rescan) and conform each seam to
//! `PetSurfaceSeam` (`aterm_models_effects::pet_surface_seam_model`). Negative
//! controls replay the retiring seams (`retire_coordinate_space`, `retire_owner`)
//! on the same real state and reproduce the cut the model refuses.
//!
//! Ghost rules pinned here: every ghost knows its own pane and fades there; glass
//! that goes takes its ghost; unknown is not evidence; a refused ghost hands its
//! presence to the arriver; a ghost is drawn at its body's last stance, tile and
//! palette; and an arriver that adopts its ghost on a quick return goes on in that
//! look, and in its palette on the surface the ghost was drawn on.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_core::render::FreeZ;
use aterm_core::terminal::Terminal;
use aterm_effects::companion::{
    CompanionFrame, CompanionOwner, ContrastFallback, GlowOwnership, PetFacts, PetGlass,
};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{
    CaptureMode, FrameGeom, HostFrameInput, SingFacts, TerminalFacts, Visibility,
};
use aterm_effects::kitty_pet::{PetAction, PetFrame};
use aterm_effects::pet_glyphs_gen::PetGlyphId;
use aterm_effects::pet_world::{PetPane, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;
use aterm_models_effects::pet_surface_seam_model;
use aterm_spec::derive::Model;

const FRAME: Duration = Duration::from_millis(16);
/// The prompt the pet settles beside: `$ ` on row 10, the caret at (10, 2).
const PROMPT: &[u8] = b"\x1b]11;#000000\x07\x1b[11;1H$ ";

/// The other pane of a vertical split: its terminal, session and place.
struct Pane {
    term: Terminal,
    session: u64,
    /// Its cell `(0, 0)` in window-grid pixels.
    origin: (i32, i32),
}

struct Host {
    term: Terminal,
    /// The terminal's session (the world and the facts are keyed on it).
    session: u64,
    /// A composed host's focused pane: its cell `(0, 0)` in window-grid
    /// pixels (`PetFacts::pane`). `None` for a single grid.
    pane: Option<(i32, i32)>,
    /// The other pane of a split, unfocused and still on glass.
    other: Option<Pane>,
    /// A third pane beside them, unfocused and still on glass.
    third: Option<Pane>,
    /// Offer the split's panes as glass before each tick, as the composed
    /// host does (`CompanionOwner::offer_glass`).
    offer: bool,
    owner: CompanionOwner,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    now: Instant,
    cell: (u16, u16),
    origin: (i32, i32),
    /// Present the cells as a snapshot extracted before a scroll the facts
    /// already saw — an observation the world map must refuse.
    stale: bool,
    /// The renderer's sub-row scroll translate for the next presents.
    frac: i32,
}

/// One present, as the pet drew it.
#[derive(Clone, Copy, Debug)]
struct Shot {
    frame: CompanionFrame,
    hit: bool,
    observed: usize,
    reason: &'static str,
    /// Sprites the emitter drew, and how many of them beneath the text.
    sprites: usize,
    under_text: usize,
    /// The live body's rect in window-grid pixels `(x0, y0, x1, y1)`.
    body_win: Option<(i32, i32, i32, i32)>,
    /// The first ghost drawn, in window-grid pixels: a ghost on its own
    /// glass from the window channel, else the live lane's.
    ghost_win: Option<(i32, i32, i32, i32)>,
    /// Its opacity and z-order.
    ghost_alpha: u8,
    ghost_under: bool,
    /// Sprites drawn into the window channel: a ghost on its own glass.
    glass_sprites: usize,
    /// The window channel's first sprites: `(x0, alpha)` in window pixels.
    own: [Option<(i32, u8)>; 4],
    /// The focused pane's cell `(0, 0)` in window pixels.
    pane_at: (i32, i32),
    /// Every pet-sized body drawn this frame, either channel, in window
    /// pixels: the ghosts and the live body, with the atlas tile each wears.
    pets: [Option<Drawn>; 8],
}

/// One pet-sized sprite as drawn: its window rect `(x0, y0, x1, y1)`, the atlas
/// tile it wears (the bake of its pose, look and palette), its opacity, and whether
/// it is the frame's live body (the last sprite the live lane pushed, at
/// `body_win`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drawn {
    rect: (i32, i32, i32, i32),
    tile: (u16, u16),
    alpha: u8,
    body: bool,
}

impl Shot {
    /// Anything of the pet on glass: the live body or a ghost.
    fn cat(&self) -> bool {
        self.frame.on_glass && self.sprites + self.glass_sprites > 0
    }

    /// The most opaque body on glass this frame, ghost or arriver.
    fn presence(&self) -> u8 {
        if !self.cat() {
            return 0;
        }
        self.frame.pet.alpha.max(self.ghost_alpha)
    }
}

impl Host {
    fn new(rows: u16, cols: u16) -> Self {
        let mut term = Terminal::new(rows, cols);
        term.process(PROMPT);
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, 0x5EED);
        Self {
            term,
            session: 7,
            pane: None,
            other: None,
            third: None,
            offer: false,
            owner,
            decos: WordDecorations::default(),
            lexicon: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            now: Instant::now(),
            cell: (8, 16),
            origin: (0, 0),
            stale: false,
            frac: 0,
        }
    }

    /// The prompt, then three seconds of frames: the pet settled beside it.
    fn settled() -> Self {
        let mut h = Self::new(30, 100);
        let settled = h.run(Duration::from_secs(3));
        assert!(
            settled.frame.on_glass && settled.frame.pet.alpha == 255,
            "fixture: the resident settled on glass: {settled:?}"
        );
        assert!(!h.owner.needs_frames(), "fixture: and at rest");
        h
    }

    fn grid(&self) -> (u16, u16) {
        (self.term.grid().rows(), self.term.grid().cols())
    }

    /// One present in the native order: observe, prepare, tick, rescan.
    fn present(&mut self) -> Shot {
        self.now += FRAME;
        // A composed host opens every presented frame: the bake budget, the
        // atlas clock and the one-body door are per frame.
        self.decos.begin_host_frame();
        let (rows, cols) = self.grid();
        let mut input = self.term.cell_frame(usize::from(rows), usize::from(cols));
        if self.stale {
            input.base_y += 1;
        }
        input.scroll_frac_px = self.frac;
        let world = PetWorldFacts::read(&self.term, self.session);
        let observed = self
            .owner
            .observe_console(&input, &world, PetPane::full(&input), &[]);
        let facts = TerminalFacts::read(&self.term, self.session, false);
        let host = HostFrameInput {
            now: self.now,
            visibility: Visibility::Focused,
            reduced_motion: false,
            serious: false,
            shed_envelope: 1.0,
            shed_active: false,
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry: FrameGeom {
                rows,
                cols,
                cell_w: self.cell.0,
                cell_h: self.cell.1,
                origin_px: self.origin,
            },
        };
        if self.offer {
            let focused = PetGlass {
                session: self.session,
                origin: self.pane.unwrap_or((0, 0)),
                cols,
                rows,
            };
            let glass = |p: &Pane| PetGlass {
                session: p.session,
                origin: p.origin,
                cols: p.term.grid().cols(),
                rows: p.term.grid().rows(),
            };
            let other = self.other.as_ref().map(glass);
            let third = self.third.as_ref().map(glass);
            self.owner
                .offer_glass(std::iter::once(focused).chain(other).chain(third));
        }
        let tick = self.owner.prepare(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: GlowOwnership {
                    enabled: true,
                    style: GlowStyle::RainbowKitty,
                    style_raw_names_pet: true,
                },
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: self.pane.map(|origin| (self.session, origin)),
                room: None,
            },
            &mut self.decos,
        );
        let frame = self.owner.tick(tick);
        // The performance seam, as the hosts run it: it dresses the pet (the
        // worn look a re-sighting ghost carries) and draws the sprites — the
        // live lane into the focused surface's channel, a ghost on its own
        // glass into the window's, as the composed host does.
        let mut free = Vec::new();
        let mut window = Vec::new();
        let _ = self.owner.emit_on(
            &frame,
            &input.cells,
            ContrastFallback {
                bg: 0,
                cursor: 0x00FF_FFFF,
                accent: 0x0050_FA7B,
            },
            &mut self.decos,
            &mut free,
            Some(&mut window),
        );
        self.decos.rescan_from_cells(
            &input.cells,
            &input.line_sizes,
            usize::from(rows),
            usize::from(cols),
            &self.lexicon,
            &self.cfg,
            input.content_seq,
            self.now,
        );
        let pane = self.pane.unwrap_or((0, 0));
        let win = |s: &aterm_core::render::FreeSprite, at: (i32, i32)| {
            (
                s.x + at.0,
                s.y + at.1,
                s.x + at.0 + i32::from(s.w),
                s.y + at.1 + i32::from(s.h),
            )
        };
        // The live lane pushes its ghosts first.
        let lane_ghosts = frame
            .pet
            .departures
            .iter()
            .flatten()
            .filter(|d| !d.own_glass)
            .count();
        let body_win = frame
            .body_px
            .map(|(x0, x1, y0, y1)| (x0 + pane.0, y0 + pane.1, x1 + pane.0, y1 + pane.1));
        let mut pets = [None; 8];
        let pet_sized = |s: &&aterm_core::render::FreeSprite| s.ah > self.cell.1;
        let last = free.len().checked_sub(1);
        let drawn = free
            .iter()
            .enumerate()
            .filter(|(_, s)| pet_sized(s))
            .map(|(i, s)| {
                let rect = win(s, pane);
                (rect, s, Some(i) == last && Some(rect) == body_win)
            })
            .chain(
                window
                    .iter()
                    .filter(pet_sized)
                    .map(|s| (win(s, (0, 0)), s, false)),
            );
        for (slot, (rect, s, body)) in pets.iter_mut().zip(drawn) {
            *slot = Some(Drawn {
                rect,
                tile: (s.ax, s.ay),
                alpha: s.alpha,
                body,
            });
        }
        let ghost_sprite = window.first().map(|s| (win(s, (0, 0)), s)).or_else(|| {
            (lane_ghosts > 0)
                .then(|| free.first().map(|s| (win(s, pane), s)))
                .flatten()
        });
        Shot {
            frame,
            hit: self.owner.hit_rect().is_some(),
            observed,
            reason: self.owner.brain().console_reason(),
            sprites: free.len(),
            under_text: free
                .iter()
                .chain(&window)
                .filter(|s| s.z == FreeZ::UnderText)
                .count(),
            body_win,
            ghost_win: ghost_sprite.map(|(rect, _)| rect),
            ghost_alpha: ghost_sprite.map_or(0, |(_, s)| s.alpha),
            ghost_under: ghost_sprite.is_some_and(|(_, s)| s.z == FreeZ::UnderText),
            glass_sprites: window.len(),
            own: core::array::from_fn(|i| window.get(i).map(|s| (s.x, s.alpha))),
            pane_at: pane,
            pets,
        }
    }

    /// A vertical split of a 30x100 window: the left pane (session 8, cols 0..50,
    /// prompt on row 20) and the focused right pane (session 7, cols 51..100 past
    /// the divider, prompt on row 10) with the pet settled beside its prompt. Both
    /// are offered as glass.
    fn split_settled() -> Self {
        let mut h = Self::new(30, 49);
        let cw = i32::from(h.cell.0);
        h.pane = Some((51 * cw, 0));
        h.origin = (51 * cw, 0);
        let mut left = Terminal::new(30, 50);
        left.process(b"\x1b]11;#000000\x07\x1b[21;1H$ ");
        h.other = Some(Pane {
            term: left,
            session: 8,
            origin: (0, 0),
        });
        h.offer = true;
        let settled = h.run(Duration::from_secs(3));
        assert!(
            settled.frame.on_glass && settled.frame.pet.alpha == 255,
            "fixture: the resident settled on glass in the right pane: {settled:?}"
        );
        h
    }

    /// Three panes side by side in a 30x98 window, each 32 columns: left (session 8,
    /// cols 0..32, prompt on row 20), the focused middle (session 7, cols 33..65,
    /// prompt on row 10) with the pet settled beside its prompt, and right (session
    /// 9, cols 66..98, prompt on row 5). All three are offered as glass.
    fn three_settled() -> Self {
        let mut h = Self::new(30, 32);
        let cw = i32::from(h.cell.0);
        h.pane = Some((33 * cw, 0));
        h.origin = (33 * cw, 0);
        let mut left = Terminal::new(30, 32);
        left.process(b"\x1b]11;#000000\x07\x1b[21;1H$ ");
        h.other = Some(Pane {
            term: left,
            session: 8,
            origin: (0, 0),
        });
        let mut right = Terminal::new(30, 32);
        right.process(b"\x1b]11;#000000\x07\x1b[6;1H$ ");
        h.third = Some(Pane {
            term: right,
            session: 9,
            origin: (66 * cw, 0),
        });
        h.offer = true;
        let settled = h.run(Duration::from_secs(3));
        assert!(
            settled.frame.on_glass && settled.frame.pet.alpha == 255,
            "fixture: the resident settled on glass in the middle pane: {settled:?}"
        );
        h
    }

    /// Focus the split's other pane: the terminal, the session and the
    /// pixel frame the pet chases swap, and the hosts' two fences run — the
    /// layout fence (another view is focused) and the owner fence (another
    /// terminal), both through the re-sighting crossfade.
    fn focus_other(&mut self) {
        self.focus_pane(false);
    }

    /// Focus the third pane, as [`Self::focus_other`] focuses the other.
    fn focus_third(&mut self) {
        self.focus_pane(true);
    }

    fn focus_pane(&mut self, third: bool) {
        let slot = if third {
            &mut self.third
        } else {
            &mut self.other
        };
        let mut other = slot.take().expect("a split");
        std::mem::swap(&mut self.term, &mut other.term);
        std::mem::swap(&mut self.session, &mut other.session);
        let origin = other.origin;
        other.origin = self.pane.expect("a split pane");
        self.pane = Some(origin);
        self.origin = origin;
        if third {
            self.third = Some(other);
        } else {
            self.other = Some(other);
        }
        self.owner.resight_coordinate_space();
        self.owner.resight_owner();
    }

    /// Presents for `span`; the last one.
    fn run(&mut self, span: Duration) -> Shot {
        let mut last = self.present();
        let end = self.now + span;
        while self.now < end {
            last = self.present();
        }
        last
    }

    /// Presents for `span`, every one.
    fn shots(&mut self, span: Duration) -> Vec<Shot> {
        let end = self.now + span;
        let mut out = vec![self.present()];
        while self.now < end {
            out.push(self.present());
        }
        out
    }
}

/// How a seam is crossed: as the hosts do, or by retiring the resident.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rule {
    Shipping,
    /// The negative control: every seam retires the resident outright.
    Retired,
}

type State = BTreeMap<&'static str, i64>;

/// A drawn body more than a cell from where it stood.
fn jumped(from: &Shot, to: &Shot) -> bool {
    to.frame.on_glass
        && ((to.frame.pet.col - from.frame.pet.col).abs() > 1.0
            || (to.frame.pet.row - from.frame.pet.row).abs() > 1.0)
}

/// The resight ghost on this frame, if any: a departing body.
fn ghost(shot: &Shot) -> Option<aterm_effects::kitty_pet::PetDepartureSprite> {
    shot.frame
        .on_glass
        .then(|| shot.frame.pet.departures.iter().flatten().copied().next())
        .flatten()
}

/// `PetSurfaceSeam`'s projection of the real owner after `action`.
fn project(
    state: &State,
    action: &str,
    owner: &CompanionOwner,
    before: &Shot,
    shot: Option<&Shot>,
) -> State {
    let mut post = state.clone();
    match (action, shot) {
        ("Draw", Some(s)) => {
            post.insert("drawn", 1);
            post.insert("lit", i64::from(s.frame.on_glass));
            post.insert("hit", i64::from(s.hit));
        }
        ("Regrid" | "Replace" | "Blind", None) => {
            let seam = match action {
                "Regrid" => 1,
                "Replace" => 2,
                _ => 3,
            };
            post.insert("seam", seam);
            post.insert("lit", i64::from(owner.brain().is_active()));
            post.insert("hit", i64::from(owner.hit_rect().is_some()));
        }
        ("Shift", None) => {
            post.insert("shifted", 1);
        }
        ("Return", None) => {
            post.insert("returned", 1);
            post.insert("ticked", 0);
            post.insert("refused", 0);
            post.insert("lit", i64::from(owner.brain().is_active()));
            post.insert("hit", i64::from(owner.hit_rect().is_some()));
            // A dark boundary draws nothing: the ghosts still fading are
            // held by value for the next tick, not on glass.
            post.insert("ghost", 0);
        }
        ("Spend", None) => {
            post.insert("spent", 1);
        }
        ("Lose", None) => {
            post.insert("lost", 1);
        }
        ("Refuse", None) => {
            post.insert("refused", 1);
        }
        ("Handoff", Some(s)) => {
            // The breed handoff's leaver on glass beside the body: a ghost
            // in another coat than the body's.
            let worn = owner.brain().worn_pair().map(|(coat, _)| coat);
            let mut ghosts = s.frame.pet.departures.iter().flatten();
            let leaver = s.cat() && ghosts.any(|d| Some(d.coat) != worn);
            post.insert("leaver", i64::from(leaver));
        }
        ("Tick" | "BlindLong", Some(s)) => {
            if action == "Tick" {
                post.insert("ticked", 1);
            } else {
                post.insert("long", 1);
            }
            post.insert("lit", i64::from(s.frame.on_glass));
            post.insert("hit", i64::from(s.hit));
            let g = ghost(s);
            post.insert("ghost", i64::from(g.is_some()));
            // The old body stood at `before`'s cell, in `before`'s frame: a ghost
            // anywhere else on glass (window pixels, every edge), or at another
            // size, stands where the old body never was.
            let near = |a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)| a == b;
            post.insert(
                "ghost_moved",
                i64::from(g.is_some_and(|g| {
                    (g.col - before.frame.pet.col).abs() > 0.01
                        || (g.row - before.frame.pet.row).abs() > 0.01
                        || !s
                            .ghost_win
                            .zip(before.body_win)
                            .is_some_and(|(ghost, body)| near(ghost, body))
                })),
            );
            // Only the regrid and the blind frame keep the body's place.
            post.insert("moved", i64::from(post["seam"] != 2 && jumped(before, s)));
        }
        _ => panic!("no projection for {action}"),
    }
    post
}

/// Fire `action` on the model and check the real projection is its
/// successor, with every invariant holding. `Err` names the refusal.
fn conform(model: &Model, state: &mut State, action: &str, post: State) -> Result<(), String> {
    let lawful = model.successors(action, state).contains(&post)
        && model
            .invariants
            .iter()
            .all(|inv| model.check_invariant(inv.name, &post));
    if !lawful {
        return Err(format!("{action}: {state:?} -> {post:?}"));
    }
    *state = post;
    Ok(())
}

/// The retiring seam, projected, is exactly the mutant model's successor.
fn shipped_step(shipped: &Model, state: &State, action: &str, post: &State) -> Result<(), String> {
    if shipped.successors(action, state).contains(post) {
        Ok(())
    } else {
        Err(format!("not the mutant's {action}: {state:?} -> {post:?}"))
    }
}

/// A ghost's opacity as drawn: its fade envelope times the lane's presence
/// (`WordDecorations::pet_departure_sprite`).
fn drawn_alpha(shot: &Shot, d: &aterm_effects::kitty_pet::PetDepartureSprite) -> u8 {
    (u16::from(d.alpha) * u16::from(shot.frame.pet.lane_alpha) / 255) as u8
}

/// The ghost standing on its own glass this frame, if any.
fn own_ghost(shot: &Shot) -> Option<aterm_effects::kitty_pet::PetDepartureSprite> {
    shot.frame
        .on_glass
        .then(|| {
            shot.frame
                .pet
                .departures
                .iter()
                .flatten()
                .copied()
                .find(|d| d.own_glass)
        })
        .flatten()
}

/// [`project`] then [`conform`]: `action` as the real owner just took it.
fn step(
    model: &Model,
    state: &mut State,
    action: &str,
    owner: &CompanionOwner,
    before: &Shot,
    shot: Option<&Shot>,
) -> Result<(), String> {
    let post = project(state, action, owner, before, shot);
    conform(model, state, action, post)
}

/// A font step and a live window drag keep the cat: cells 8x16 to 9x18 with the grid
/// following, then the window a column narrower per frame, leave the resident on
/// glass on every frame within a cell of its seat (`Regrid`).
///
/// Negative control: the retiring seam (`retire_coordinate_space`) blanks the cat
/// after the font step and on every frame of the drag, and the model refuses it.
#[test]
fn a_font_step_and_a_live_drag_keep_the_resident_on_glass() {
    let model = pet_surface_seam_model();
    let shipped = aterm_spec::interp::with_buggy(&model, 1);
    for rule in [Rule::Shipping, Rule::Retired] {
        let mut h = Host::settled();
        let settled = h.present();
        let mut state = model.init_state();
        step(
            &model,
            &mut state,
            "Draw",
            &h.owner,
            &settled,
            Some(&settled),
        )
        .expect("the settled resident is drawn");

        // ⌘+ : the cells grow and the grid follows the fixed window.
        h.cell = (9, 18);
        h.term.resize(26, 88);
        match rule {
            Rule::Shipping => h.owner.rebase_geometry(),
            Rule::Retired => h.owner.retire_coordinate_space(),
        }
        let at_seam = project(&state, "Regrid", &h.owner, &settled, None);
        let after = h.shots(Duration::from_millis(500));
        let blank = after.iter().filter(|s| !s.frame.on_glass).count();
        if rule == Rule::Retired {
            assert!(
                blank > 0,
                "negative control: the shipped seam blanks the cat after a font step"
            );
            shipped_step(&shipped, &state, "Regrid", &at_seam)
                .expect("the shipped seam is the model's Buggy regrid");
            assert!(
                conform(&model, &mut state, "Regrid", at_seam).is_err(),
                "the model refuses the shipped regrid"
            );
        } else {
            assert_eq!(blank, 0, "a font step blanked the cat: {after:?}");
            conform(&model, &mut state, "Regrid", at_seam).expect("regrid seam");
            let post = project(&state, "Tick", &h.owner, &settled, Some(&after[0]));
            conform(&model, &mut state, "Tick", post).expect("the frame after a regrid");
            let far = after.iter().find(|s| jumped(&settled, s));
            assert!(far.is_none(), "the font step moved the cat: {far:?}");
        }

        // A live drag: one column narrower on every frame.
        let mut prev = h.present();
        let mut blank = 0;
        for k in 1..=30u16 {
            h.term.resize(26, 88 - k);
            match rule {
                Rule::Shipping => h.owner.rebase_geometry(),
                Rule::Retired => h.owner.retire_coordinate_space(),
            }
            let shot = h.present();
            blank += usize::from(!shot.frame.on_glass);
            if rule == Rule::Shipping {
                assert!(
                    shot.frame.on_glass && !jumped(&prev, &shot),
                    "drag frame {k}: {shot:?} after {prev:?}"
                );
            }
            prev = shot;
        }
        match rule {
            Rule::Shipping => assert_eq!(blank, 0),
            Rule::Retired => assert_eq!(
                blank, 30,
                "negative control: the shipped seam kept the cat off glass for the whole drag"
            ),
        }
    }
}

/// Launching and quitting a full-screen program crossfades. The alternate screen
/// enters (a TUI paints text where the pet sat) and exits, each through the owner
/// fence (`CompanionOwner::resight_owner`). The boundary is dark and owns no hit
/// target; the next frame has the replacement at its new station and the old body
/// as a ghost exactly where it was drawn, beneath the TUI's text, fading over a
/// quarter second while the arriver comes up. No frame is without a cat.
///
/// Negative control: the retiring fence (`retire_owner`) cuts the cat, and the model
/// refuses it.
#[test]
fn an_alt_screen_enter_and_exit_crossfade_instead_of_cutting() {
    let model = pet_surface_seam_model();
    let shipped = aterm_spec::interp::with_buggy(&model, 1);
    let tui = tui_bytes();
    for rule in [Rule::Shipping, Rule::Retired] {
        let mut h = Host::settled();
        for (leg, bytes) in [
            ("enter", tui.as_slice()),
            ("exit", b"\x1b[?1049l".as_slice()),
        ] {
            let label = format!("{rule:?} {leg}");
            let before = h.present();
            assert!(before.frame.on_glass, "{label}: on glass first");
            let mut state = model.init_state();
            step(&model, &mut state, "Draw", &h.owner, &before, Some(&before))
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            h.term.process(bytes);
            match rule {
                Rule::Shipping => h.owner.resight_owner(),
                Rule::Retired => h.owner.retire_owner(),
            }
            let at_seam = project(&state, "Replace", &h.owner, &before, None);
            conform(&model, &mut state, "Replace", at_seam)
                .unwrap_or_else(|e| panic!("{label}: the boundary is dark: {e}"));
            let after = h.shots(Duration::from_millis(600));
            let post = project(&state, "Tick", &h.owner, &before, Some(&after[0]));
            if rule == Rule::Retired {
                assert!(
                    !after[0].frame.on_glass,
                    "{label}: negative control: the shipped fence cuts the cat"
                );
                shipped_step(&shipped, &state, "Tick", &post)
                    .unwrap_or_else(|e| panic!("{label}: the shipped cut is Buggy: {e}"));
                assert!(conform(&model, &mut state, "Tick", post).is_err());
                h.run(Duration::from_secs(3));
                continue;
            }
            conform(&model, &mut state, "Tick", post).unwrap_or_else(|e| panic!("{label}: {e}"));
            let first = after[0];
            let g = ghost(&first).expect("the old body fades as a ghost");
            assert_eq!(
                (g.col, g.row),
                (before.frame.pet.col, before.frame.pet.row),
                "{label}: the ghost stands exactly where the body was drawn"
            );
            assert_eq!(
                g.under_ink,
                leg == "enter",
                "{label}: beneath the new screen's text, over blank ground"
            );
            assert!(
                first.sprites >= 2 && (leg != "enter" || first.under_text >= 1),
                "{label}: both bodies are drawn, the ghost beneath the TUI's text: {first:?}"
            );
            assert!(
                first.frame.pet.alpha > 0 && first.frame.pet.alpha < 64,
                "{label}: the arriver comes up from its floor: {}",
                first.frame.pet.alpha
            );
            let blank = after.iter().position(|s| !s.frame.on_glass);
            assert_eq!(blank, None, "{label}: a frame with no cat: {after:?}");
            let last = after.last().expect("frames");
            assert!(
                ghost(last).is_none() && last.frame.pet.alpha == 255,
                "{label}: the crossfade completes: {last:?}"
            );
            let caret = h.term.cursor();
            assert!(
                (last.frame.pet.row - f32::from(caret.row)).abs() <= 1.0,
                "{label}: the replacement stands at the new caret's row: {last:?} caret {caret:?}"
            );
            h.run(Duration::from_secs(3));
        }
    }

    // Glass that is gone spends no ghost: the grid's origin moves before the next
    // frame and no glass is offered where the old body was drawn.
    let mut h = Host::settled();
    let before = h.present();
    let mut state = model.init_state();
    step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
    h.term.process(&tui);
    h.owner.resight_owner();
    let at_seam = project(&state, "Replace", &h.owner, &before, None);
    conform(&model, &mut state, "Replace", at_seam).expect("dark boundary");
    h.origin = (400, 0);
    let lost = project(&state, "Lose", &h.owner, &before, None);
    conform(&model, &mut state, "Lose", lost).expect("lose");
    let next = h.present();
    let post = project(&state, "Tick", &h.owner, &before, Some(&next));
    conform(&model, &mut state, "Tick", post).expect("no ghost on glass that is gone");
    assert!(ghost(&next).is_none());
}

/// A split-pane focus switch crossfades across panes. The pet settled in the right
/// pane; the focus moves left and the layout and owner fences run. The boundary is
/// dark and owns no hit target; the next frame has the arriver at the left pane's
/// caret on its homecoming ramp and the old body as a ghost on its own glass (the
/// window channel, at the window pixels it was drawn at, beneath that pane's text),
/// fading over a quarter second. No frame is without a cat.
///
/// Negative control: an owner offered no glass drops the ghost and cuts the cat,
/// which the model refuses (`PaneCut`). Controls: the ghost goes when its glass
/// goes.
#[test]
fn a_split_pane_focus_switch_crossfades_across_panes() {
    let model = pet_surface_seam_model();
    // The `PaneCut` mutant: a same-glass replacement crossfades, a shifted one cuts.
    let shipped = aterm_spec::interp::with_consts(&model, &[("PaneCut", 1)]);
    for offer in [true, false] {
        let label = if offer { "after" } else { "before" };
        let mut h = Host::split_settled();
        let cw = i32::from(h.cell.0);
        let before = h.present();
        assert!(before.frame.on_glass, "{label}: on glass first");
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before))
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        h.offer = offer;
        h.focus_other();
        let at_seam = project(&state, "Replace", &h.owner, &before, None);
        conform(&model, &mut state, "Replace", at_seam)
            .unwrap_or_else(|e| panic!("{label}: the boundary is dark: {e}"));
        let shifted = project(&state, "Shift", &h.owner, &before, None);
        conform(&model, &mut state, "Shift", shifted).expect("shift");
        let after = h.shots(Duration::from_millis(600));
        let post = project(&state, "Tick", &h.owner, &before, Some(&after[0]));
        let no_cat = after.iter().filter(|s| !s.cat()).count();
        let faint = after.iter().filter(|s| s.presence() < 64).count();
        let floor = after.iter().map(Shot::presence).min().unwrap_or(0);
        let done = after
            .iter()
            .position(|s| s.frame.pet.alpha == 255 && ghost(s).is_none())
            .map(|i| (i + 1) * 16);
        eprintln!(
            "pane switch, {label}: {no_cat}/{} frames with no cat, {faint} under a quarter \
             presence, presence floor {floor}/255, arriver full and ghost gone at {done:?} ms",
            after.len()
        );
        if !offer {
            assert!(
                no_cat > 0,
                "negative control: the shipped rule cut the cat at the switch"
            );
            shipped_step(&shipped, &state, "Tick", &post)
                .expect("the shipped cut is the model's PaneCut shifted tick");
            assert!(
                conform(&model, &mut state, "Tick", post).is_err(),
                "the model refuses the shipped cut"
            );
            continue;
        }
        conform(&model, &mut state, "Tick", post).expect("a pane switch crossfades");
        assert_eq!(no_cat, 0, "a frame with no cat: {after:?}");
        let first = after[0];
        let g = ghost(&first).expect("the old body fades as a ghost");
        assert!(g.own_glass && g.under_ink, "{g:?}");
        assert_eq!(
            first.frame.glass.iter().flatten().collect::<Vec<_>>(),
            [&PetGlass {
                session: 7,
                origin: (51 * cw, 0),
                cols: 49,
                rows: 30,
            }],
            "the ghost carries the glass it was drawn on"
        );
        assert_eq!(
            first.glass_sprites, 1,
            "drawn in the window channel, never placed by the focused pane: {first:?}"
        );
        let (ghost_px, body_px) = (first.ghost_win.unwrap(), before.body_win.unwrap());
        assert!(
            (ghost_px.0 - body_px.0).abs() <= 1 && (ghost_px.3 - body_px.3).abs() <= 1,
            "the ghost stands where the body was drawn, in window pixels: {ghost_px:?} vs {body_px:?}"
        );
        assert!(
            ghost_px.0 >= 51 * cw,
            "…inside the pane it was drawn in: {ghost_px:?}"
        );
        assert!(first.ghost_under, "beneath that pane's text");
        assert!(
            first.ghost_alpha > 200,
            "the ghost starts at the old body's presence: {}",
            first.ghost_alpha
        );
        assert!(
            first.frame.pet.alpha > 0 && first.frame.pet.alpha < 64,
            "the arriver comes up from its floor: {}",
            first.frame.pet.alpha
        );
        assert!(
            first.body_win.is_some_and(|b| b.2 <= 50 * cw),
            "the arriver stands in the left pane: {first:?}"
        );
        let last = after.last().expect("frames");
        assert!(
            ghost(last).is_none() && last.glass_sprites == 0 && last.frame.pet.alpha == 255,
            "the crossfade completes: {last:?}"
        );
        assert!(
            (last.frame.pet.row - 20.0).abs() <= 1.0,
            "the arriver stands at the left pane's caret row: {last:?}"
        );
    }

    // Glass that goes takes its ghost: the old pane closes at the switch, or a font
    // step lands with it, and the arriver fades in alone; the old pane closing
    // mid-fade drops the ghost on the next frame.
    for lose in ["closed", "font step"] {
        let mut h = Host::split_settled();
        let before = h.present();
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
        h.focus_other();
        let at_seam = project(&state, "Replace", &h.owner, &before, None);
        conform(&model, &mut state, "Replace", at_seam).expect("dark boundary");
        match lose {
            "closed" => h.other = None,
            _ => h.cell = (9, 18),
        }
        let lost = project(&state, "Lose", &h.owner, &before, None);
        conform(&model, &mut state, "Lose", lost).expect("lose");
        let next = h.present();
        let post = project(&state, "Tick", &h.owner, &before, Some(&next));
        conform(&model, &mut state, "Tick", post)
            .unwrap_or_else(|e| panic!("{lose}: no ghost on glass that is gone: {e}"));
        assert!(
            ghost(&next).is_none() && next.glass_sprites == 0,
            "{lose}: {next:?}"
        );
    }
    let mut h = Host::split_settled();
    h.focus_other();
    let fading = h.shots(Duration::from_millis(64));
    assert!(
        fading.iter().all(|s| s.glass_sprites == 1),
        "fixture: the ghost is fading on its own glass: {fading:?}"
    );
    h.other = None;
    let next = h.present();
    assert!(
        ghost(&next).is_none()
            && next.glass_sprites == 0
            && next.frame.glass.iter().all(Option::is_none),
        "the ghost goes with its pane: {next:?}"
    );
    assert!(!h.owner.brain().has_own_glass_ghost());
}

/// The same seat in one surface's cells: on the row, within a cell
/// (`kitty_pet::ADOPT_REACH`).
fn at_seat(col: f32, row: f32, seat: (f32, f32)) -> bool {
    (col - seat.0).abs() <= 1.0 && (row - seat.1).abs() < 0.5
}

/// The cat at `seat` this frame: how many bodies stand there, and the
/// presence the eye sees there — their composite coverage, `0..=255`. On the
/// focused surface (`own == false`) that is the arriver and the ghosts of the
/// live lane; on the pane the focus left (`own == true`), its own-glass
/// ghosts.
fn seat_cat(shot: &Shot, seat: (f32, f32), own: bool) -> (usize, u8) {
    let mut alphas: Vec<u8> = shot
        .frame
        .pet
        .departures
        .iter()
        .flatten()
        .filter(|d| d.own_glass == own && at_seat(d.col, d.row, seat))
        .map(|d| drawn_alpha(shot, d))
        .collect();
    if !own && shot.cat() && at_seat(shot.frame.pet.col, shot.frame.pet.row, seat) {
        alphas.push(shot.frame.pet.alpha);
    }
    alphas.retain(|&a| a > 0);
    let clear: f32 = alphas.iter().map(|&a| 1.0 - f32::from(a) / 255.0).product();
    (alphas.len(), ((1.0 - clear) * 255.0).round() as u8)
}

/// Everything of the cat on glass this frame, summed: the arriver and every
/// ghost, as drawn. One cat is 255.
fn total_presence(shot: &Shot) -> u16 {
    if !shot.cat() {
        return 0;
    }
    let ghosts: u16 = shot
        .frame
        .pet
        .departures
        .iter()
        .flatten()
        .map(|d| u16::from(drawn_alpha(shot, d)))
        .sum();
    u16::from(shot.frame.pet.alpha) + ghosts
}

/// For every ghost that leaves glass during `shots` — the first frame it is
/// no longer drawn — the alpha it should still have had there under the
/// crossfade's complement law: its starting presence times what its arriver
/// was still missing on that frame (`255 − alpha`). Zero when it leaves on
/// the arriver's full frame; up to one for the bytes' rounding.
fn tails(shots: &[Shot]) -> Vec<u8> {
    type Key = (bool, i32, i32);
    let drawn = |shot: &Shot| -> Vec<(Key, u8)> {
        shot.frame
            .pet
            .departures
            .iter()
            .flatten()
            .map(|d| {
                let key = (
                    d.own_glass,
                    (d.col * 100.0).round() as i32,
                    (d.row * 100.0).round() as i32,
                );
                (key, drawn_alpha(shot, d))
            })
            .filter(|&(_, alpha)| alpha > 0)
            .collect()
    };
    let mut peaks: BTreeMap<Key, u8> = BTreeMap::new();
    let mut out = Vec::new();
    for pair in shots.windows(2) {
        let next = drawn(&pair[1]);
        for (ghost, alpha) in drawn(&pair[0]) {
            let peak = *peaks.entry(ghost).or_insert(alpha);
            if !next.iter().any(|&(key, _)| key == ghost) {
                let missing = u16::from(255 - pair[1].frame.pet.alpha);
                out.push((u16::from(peak) * missing / 255) as u8);
            }
        }
    }
    out
}

/// The look of the most present body at `seat` this frame — `(pose,
/// facing_left)` as drawn — on the focused surface (`own == false`: the
/// arriver and the live lane's ghosts) or on the pane the focus left (`own`:
/// its own-glass ghosts).
fn seat_look(shot: &Shot, seat: (f32, f32), own: bool) -> Option<(PetGlyphId, bool)> {
    let mut bodies: Vec<(u8, (PetGlyphId, bool))> = shot
        .frame
        .pet
        .departures
        .iter()
        .flatten()
        .filter(|d| d.own_glass == own && at_seat(d.col, d.row, seat))
        .map(|d| (drawn_alpha(shot, d), (d.pose, d.facing_left)))
        .collect();
    let pet = &shot.frame.pet;
    if !own && shot.cat() && at_seat(pet.col, pet.row, seat) {
        bodies.push((pet.alpha, (pet.pose, pet.facing_left)));
    }
    bodies.retain(|&(alpha, _)| alpha > 0);
    bodies
        .iter()
        .max_by_key(|&&(alpha, _)| alpha)
        .map(|&(_, look)| look)
}

/// The seat a return brings the cat back to, as the frames after it draw it:
/// `(restarted, most bodies there, presence floor there, what the user saw
/// there before the return)`. The seat is where the arriver stands on the
/// first frame; before the return it was on the pane the focus left
/// (`own`, a ghost on its own glass) or on the same glass. It `restarted` if
/// a second body of the cat stood there, the presence there fell below what
/// the user saw (a frame's rounding allowed), or the body there changed its
/// look — pose or facing — on the frame it came back.
fn return_seat(seen: &Shot, after: &[Shot], own: bool) -> (bool, usize, u8, u8) {
    let seat = (after[0].frame.pet.col, after[0].frame.pet.row);
    let (_, there) = seat_cat(seen, seat, own);
    let story: Vec<(usize, u8)> = after.iter().map(|s| seat_cat(s, seat, false)).collect();
    let most = story.iter().map(|&(n, _)| n).max().unwrap_or(0);
    let floor = story.iter().map(|&(_, p)| p).min().unwrap_or(0);
    let relooked = seat_look(seen, seat, own)
        .zip(seat_look(&after[0], seat, false))
        .is_some_and(|(was, is)| was != is);
    (
        most > 1 || floor.saturating_add(2) < there || relooked,
        most,
        floor,
        there,
    )
}

/// A full-screen program's screen: twenty rows of text, its caret at (24, 4).
fn tui_bytes() -> Vec<u8> {
    let mut b = b"\x1b[?1049h\x1b[2J".to_vec();
    for row in 1..=20 {
        b.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        b.extend_from_slice(&[b'x'; 60]);
    }
    b.extend_from_slice(b"\x1b[25;5H");
    b
}

/// A quick return continues the one cat it comes back to. The focus leaves a surface
/// and comes back two frames later, while the body it left still fades as a ghost:
/// a split pane (the ghost on its own glass) and a full-screen program that quits at
/// once (the same glass). The arriver is seated at the ghost's seat in its look and
/// adopts the ghost's cell and presence: one body at the seat on every frame, its
/// presence never below what the user saw, and the pane the focus left keeps a
/// ghost no brighter than its body was.
#[test]
fn a_quick_return_continues_the_one_cat_it_comes_back_to() {
    let model = pet_surface_seam_model();
    for glass in ["split", "screen"] {
        let (mut h, own) = match glass {
            "split" => (Host::split_settled(), true),
            _ => (Host::settled(), false),
        };
        let before = h.present();
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
        match glass {
            "split" => h.focus_other(),
            _ => {
                h.term.process(&tui_bytes());
                h.owner.resight_owner();
            }
        }
        step(&model, &mut state, "Replace", &h.owner, &before, None).expect("dark boundary");
        if own {
            step(&model, &mut state, "Shift", &h.owner, &before, None).expect("shift");
        }
        let first = h.shots(Duration::from_millis(32));
        step(
            &model,
            &mut state,
            "Tick",
            &h.owner,
            &before,
            Some(&first[0]),
        )
        .unwrap_or_else(|e| panic!("{glass}: {e}"));
        let seen = *first.last().expect("frames");
        let old = if own { own_ghost(&seen) } else { ghost(&seen) }
            .expect("fixture: the body the focus left is fading as a ghost");
        let seat = (old.col, old.row);
        let (_, seen_there) = seat_cat(&seen, seat, own);
        let left_seen = seen.frame.pet.alpha;
        assert!(
            left_seen < 64 && seen_there > 192,
            "fixture: mid-crossfade, the arriver faint and the ghost bright: {left_seen} \
             {seen_there}"
        );

        match glass {
            "split" => h.focus_other(),
            _ => {
                h.term.process(b"\x1b[?1049l");
                h.owner.resight_owner();
            }
        }
        step(&model, &mut state, "Return", &h.owner, &seen, None)
            .unwrap_or_else(|e| panic!("{glass}: the second boundary is dark: {e}"));
        let after = h.shots(Duration::from_millis(400));
        let next = after[0];
        assert!(
            at_seat(next.frame.pet.col, next.frame.pet.row, seat),
            "{glass}: fixture: the arriver comes back to its seat: {next:?} {seat:?}"
        );
        let (restarted, most, floor, there) = return_seat(&seen, &after, own);
        assert_eq!(
            there, seen_there,
            "{glass}: fixture: the seat is the ghost's"
        );
        eprintln!(
            "quick return, {glass}: the seat showed {seen_there}; after the return the arriver \
             starts at {}, at most {most} bodies there, its presence floor {floor}/255",
            next.frame.pet.alpha
        );
        let mut post = project(&state, "Tick", &h.owner, &seen, Some(&next));
        post.insert("restarted", i64::from(restarted));
        conform(&model, &mut state, "Tick", post)
            .unwrap_or_else(|e| panic!("{glass}: one body continues: {e}"));
        assert_eq!(most, 1, "{glass}: one body at the seat");
        assert!(
            next.frame.pet.alpha.saturating_add(2) >= seen_there,
            "{glass}: the arriver continues from what the user saw: {} after {seen_there}",
            next.frame.pet.alpha
        );
        if own {
            let left = own_ghost(&next).expect("the left pane's body fades on its own glass");
            assert!(
                drawn_alpha(&next, &left) <= left_seen.saturating_add(1) && next.glass_sprites == 1,
                "its ghost is never brighter than its body was: {} after {left_seen}",
                drawn_alpha(&next, &left)
            );
        }
        assert!(
            after.iter().all(Shot::cat),
            "{glass}: a frame with no cat: {after:?}"
        );
        let last = after.last().expect("frames");
        assert!(
            ghost(last).is_none() && last.glass_sprites == 0 && last.frame.pet.alpha == 255,
            "{glass}: the crossfade completes: {last:?}"
        );
        let mut post = project(&state, "Spend", &h.owner, &seen, None);
        post.insert("tail", i64::from(tails(&after).iter().any(|&t| t > 1)));
        conform(&model, &mut state, "Spend", post).unwrap_or_else(|e| panic!("{glass}: {e}"));
        assert!(!h.owner.brain().has_own_glass_ghost());
    }
}

/// Every crossfade ghost leaves glass on its arriver's full frame: a ghost's envelope
/// runs on its arriver's ramp from the presence it had on glass, so on every frame
/// the arriver and the ghosts sum to about one cat (255), the arriver's 6 %
/// first-frame floor the only excess. Cases: a split-pane switch, a full-screen
/// program, and a return to the pane the focus left after its caret moved a row
/// (its ghost then fades apart from the arriver's seat), two and eight frames into
/// the first crossfade.
#[test]
fn every_crossfade_ghost_leaves_glass_on_its_arrivers_full_frame() {
    let model = pet_surface_seam_model();
    for (case, back) in [
        ("split", None),
        ("screen", None),
        ("split, back to a moved caret", Some(2)),
        ("split, back to a moved caret late", Some(8)),
    ] {
        let mut h = if case == "screen" {
            Host::settled()
        } else {
            Host::split_settled()
        };
        let before = h.present();
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
        if case == "screen" {
            h.term.process(&tui_bytes());
            h.owner.resight_owner();
        } else {
            h.focus_other();
        }
        step(&model, &mut state, "Replace", &h.owner, &before, None).expect("dark boundary");
        if case != "screen" {
            step(&model, &mut state, "Shift", &h.owner, &before, None).expect("shift");
        }
        let mut after = h.shots(Duration::ZERO);
        step(
            &model,
            &mut state,
            "Tick",
            &h.owner,
            &before,
            Some(&after[0]),
        )
        .unwrap_or_else(|e| panic!("{case}: {e}"));
        if let Some(frames) = back {
            for _ in 1..frames {
                after.push(h.present());
            }
            let seen = *after.last().expect("frames");
            // Output in the pane the focus left moves its caret a row.
            h.other.as_mut().expect("a split").term.process(b"\r\n$ ");
            h.focus_other();
            step(&model, &mut state, "Return", &h.owner, &seen, None)
                .unwrap_or_else(|e| panic!("{case}: the second boundary is dark: {e}"));
            after = h.shots(Duration::from_millis(400));
            let (restarted, ..) = return_seat(&seen, &after, true);
            let mut post = project(&state, "Tick", &h.owner, &seen, Some(&after[0]));
            post.insert("restarted", i64::from(restarted));
            conform(&model, &mut state, "Tick", post).unwrap_or_else(|e| panic!("{case}: {e}"));
            assert_eq!(
                after[0].frame.pet.departures.iter().flatten().count(),
                2,
                "{case}: fixture: the carried ghost stands apart from the arriver: {:?}",
                after[0]
            );
        } else {
            after.extend(h.shots(Duration::from_millis(400)));
        }
        let tails = tails(&after);
        let low = after.iter().map(total_presence).min().unwrap_or(0);
        let high = after.iter().map(total_presence).max().unwrap_or(0);
        eprintln!(
            "{case}: each ghost's alpha owed as it left glass: {tails:?}; the cat summed \
             {low}..={high}/255"
        );
        assert!(!tails.is_empty(), "{case}: fixture: a ghost left glass");
        assert!(
            tails.iter().all(|&t| t <= 1),
            "{case}: a ghost left glass before its arriver was full: {tails:?}"
        );
        assert!(
            low >= 245 && high <= 272,
            "{case}: the cat dipped or doubled: {low}..={high}"
        );
        let mut post = project(&state, "Spend", &h.owner, &before, None);
        post.insert("tail", i64::from(tails.iter().any(|&t| t > 1)));
        conform(&model, &mut state, "Spend", post).unwrap_or_else(|e| panic!("{case}: {e}"));
        let last = after.last().expect("frames");
        assert!(
            ghost(last).is_none() && last.frame.pet.alpha == 255,
            "{case}: the crossfade completes: {last:?}"
        );
    }
}

/// An adopted body goes on in the look the user saw. A quick return adopts the ghost
/// at the arriver's seat, and the body goes on in the ghost's pose and facing while
/// it rests there: through the homecoming ramp and, for a still pose, until the
/// brain's own look steps. Every handover rides a step the brain takes anyway (its
/// look stepping at rest, a mid-stride pose folding at the ramp's end, or the body
/// set in motion), and the held look never travels or flies. Cases: mid-stretch
/// after a key (the brain walks on at once), standing, sitting face-on, asleep,
/// mid-stride, and standing with Enter two frames after the return.
#[test]
fn an_adopted_body_goes_on_in_the_look_the_user_saw() {
    let model = pet_surface_seam_model();
    for (case, keys, wait, input) in [
        // Inside the stretch's lead: the body drawn stretched, and its
        // ghost at that stretch (`ghost_moved` reads its pixels).
        ("mid-stretch", 1usize, 4usize, None),
        ("standing", 3, 30, None),
        ("face-on", 6, 120, None),
        ("asleep", 0, 0, None),
        ("mid-stride", 2, 6, None),
        ("standing, then Enter", 3, 30, Some(&b"\r\n$ "[..])),
    ] {
        let mut h = Host::split_settled();
        for _ in 0..keys {
            h.term.process(b"l");
            h.present();
            h.present();
        }
        for _ in 0..wait {
            h.present();
        }
        let before = h.present();
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
        h.focus_other();
        step(&model, &mut state, "Replace", &h.owner, &before, None).expect("dark boundary");
        step(&model, &mut state, "Shift", &h.owner, &before, None).expect("shift");
        let first = h.shots(Duration::from_millis(32));
        step(
            &model,
            &mut state,
            "Tick",
            &h.owner,
            &before,
            Some(&first[0]),
        )
        .unwrap_or_else(|e| panic!("{case}: {e}"));
        let seen = *first.last().expect("frames");
        let old = own_ghost(&seen).expect("fixture: the body the focus left fades in its pane");
        let look = (old.pose, old.facing_left);

        h.focus_other();
        step(&model, &mut state, "Return", &h.owner, &seen, None)
            .unwrap_or_else(|e| panic!("{case}: the second boundary is dark: {e}"));
        let mut after = Vec::new();
        let quiet = if let Some(bytes) = input {
            after.extend([h.present(), h.present()]);
            h.term.process(bytes);
            2
        } else {
            usize::MAX
        };
        after.extend(h.shots(Duration::from_millis(3_000)));
        let next = after[0];
        let a0 = next.frame.pet;
        assert!(
            at_seat(a0.col, a0.row, (old.col, old.row))
                && a0.alpha.saturating_add(2) >= drawn_alpha(&seen, &old),
            "{case}: fixture: the arriver adopts the ghost at its seat: {next:?}"
        );
        let looks: Vec<(PetGlyphId, bool)> = after
            .iter()
            .map(|s| (s.frame.pet.pose, s.frame.pet.facing_left))
            .collect();
        let worn = looks.iter().take_while(|&&l| l == look).count();
        let ramp = after.iter().take_while(|s| s.frame.pet.alpha < 255).count();
        let handover = after.get(worn).map(|s| s.frame.pet);
        eprintln!(
            "{case}: the ghost showed {look:?}; the adopted body wore it {} ms (the ramp ran \
             {} ms), then {:?}",
            worn * 16,
            ramp * 16,
            handover.map(|p| (p.pose, p.action, p.col, p.row, p.lift))
        );
        let (restarted, most, ..) = return_seat(&seen, &after[..quiet.min(after.len())], true);
        let mut post = project(&state, "Tick", &h.owner, &seen, Some(&next));
        post.insert("restarted", i64::from(restarted));
        conform(&model, &mut state, "Tick", post)
            .unwrap_or_else(|e| panic!("{case}: one body goes on in the look the user saw: {e}"));
        assert_eq!(most, 1, "{case}: one body at the seat");
        // The held look rests at its seat: it never slides, lifts or rides a
        // gather, a flight or a landing.
        let flying = |p: &PetFrame| {
            p.lift.abs() > 0.05
                || matches!(
                    p.action,
                    PetAction::Crouch | PetAction::Leap | PetAction::Land
                )
        };
        let travelled = after[..worn]
            .iter()
            .map(|s| {
                (s.frame.pet.col - a0.col)
                    .abs()
                    .max((s.frame.pet.row - a0.row).abs())
            })
            .fold(0.0f32, f32::max);
        assert!(
            travelled < 0.25 && !after[..worn].iter().any(|s| flying(&s.frame.pet)),
            "{case}: the held look travelled {travelled:.2} cells or flew: {:?}",
            after[..worn]
                .iter()
                .map(|s| s.frame.pet)
                .collect::<Vec<_>>()
        );
        // Every handover rides a step the brain takes anyway: after the ramp
        // (a still look until the brain's own steps — the standing cat's own
        // stand may creep a little before it folds — and a mid-stride one at
        // the ramp's end, at rest), or the brain setting the body in motion.
        let handover = handover.unwrap_or_else(|| panic!("{case}: the look hands over: {looks:?}"));
        let moving = flying(&handover)
            || (handover.col - a0.col)
                .abs()
                .max((handover.row - a0.row).abs())
                > 0.05;
        match case {
            "mid-stretch" | "standing, then Enter" => assert!(
                worn < ramp && moving,
                "{case}: the look hands over as the brain sets the body moving: {handover:?}"
            ),
            "mid-stride" => assert!(
                worn >= ramp.min(16) && worn <= ramp.max(16) + 1 && !moving,
                "{case}: a pose caught mid-stride folds at the ramp's end, at rest: {worn} \
                 frames, the ramp {ramp}: {handover:?}"
            ),
            _ => assert!(
                worn > ramp,
                "{case}: the look is worn through the whole ramp: {worn} frames, the ramp {ramp}"
            ),
        }
        let last = after.last().expect("frames");
        assert!(
            ghost(last).is_none()
                && last.glass_sprites == 0
                && last.frame.pet.alpha == 255
                && last.frame.pet.action.settled(),
            "{case}: the brain's behaviour takes over and the cat settles: {last:?}"
        );
    }
}

/// Every ghost fades in its own pane across three panes. A quick cycle M → L → R
/// inside the first crossfade leaves two ghosts on two own glasses (M's and L's);
/// each fades where it was drawn and reaches zero on the arriver's full frame, so no
/// pane loses its cat in one frame and the cat never dips. Coming back to M or L,
/// the ghost there is the cat the arriver adopts.
#[test]
fn every_ghost_fades_in_its_own_pane_across_three_panes() {
    let model = pet_surface_seam_model();
    for gap in [1usize, 2, 4, 8] {
        for back in ["none", "M", "L"] {
            let case = format!("{gap} frames apart, back to {back}");
            let mut h = Host::three_settled();
            let cw = i32::from(h.cell.0);
            let before = h.present();
            let mut state = model.init_state();
            step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
            h.focus_other();
            step(&model, &mut state, "Replace", &h.owner, &before, None).expect("dark boundary");
            step(&model, &mut state, "Shift", &h.owner, &before, None).expect("shift");
            let mut all = vec![before];
            for _ in 0..gap {
                all.push(h.present());
            }
            step(&model, &mut state, "Tick", &h.owner, &before, Some(&all[1]))
                .unwrap_or_else(|e| panic!("{case}: {e}"));
            let seen = *all.last().expect("frames");

            h.focus_third();
            step(&model, &mut state, "Return", &h.owner, &seen, None)
                .unwrap_or_else(|e| panic!("{case}: the second boundary is dark: {e}"));
            let mut after = vec![h.present()];
            let (restarted, ..) = return_seat(&seen, &after, true);
            let mut post = project(&state, "Tick", &h.owner, &seen, Some(&after[0]));
            post.insert("restarted", i64::from(restarted));
            conform(&model, &mut state, "Tick", post).unwrap_or_else(|e| panic!("{case}: {e}"));
            let next = after[0];
            match back {
                "none" => {
                    after.extend(h.shots(Duration::from_millis(400)));
                    // From the frame before the switch to R: a ghost cut at
                    // that boundary leaves glass there.
                    let tails = tails(&[&[seen][..], &after[..]].concat());
                    let mut post = project(&state, "Spend", &h.owner, &seen, None);
                    post.insert("tail", i64::from(tails.iter().any(|&t| t > 1)));
                    conform(&model, &mut state, "Spend", post).unwrap_or_else(|e| {
                        panic!("{case}: every ghost leaves at full presence: {e} {tails:?}")
                    });
                    all.extend(after);
                }
                _ => {
                    for _ in 1..gap {
                        after.push(h.present());
                    }
                    let seen = *after.last().expect("frames");
                    if back == "M" {
                        h.focus_other();
                    } else {
                        h.focus_third();
                    }
                    let home = h.shots(Duration::from_millis(400));
                    let (restarted, most, floor, there) = return_seat(&seen, &home, true);
                    assert!(
                        !restarted && most == 1 && there > 0,
                        "{case}: the arriver adopts the ghost in the pane the focus came back \
                         to: {most} bodies, floor {floor} after {there}"
                    );
                    all.extend(after);
                    all.extend(home);
                }
            }
            let story: Vec<[u16; 3]> = all.iter().map(|s| by_pane(s, cw)).collect();
            let mut cut = [0u16; 3];
            for pair in story.windows(2) {
                for (worst, (was, is)) in cut.iter_mut().zip(pair[0].iter().zip(&pair[1])) {
                    *worst = (*worst).max(was.saturating_sub(*is));
                }
            }
            let low = all.iter().map(total_presence).min().unwrap_or(0);
            eprintln!(
                "three panes, {case}: the largest one-frame fall in L/M/R {cut:?}, the cat summed \
                 at least {low}/255"
            );
            assert!(
                cut.iter().all(|&c| c <= 20),
                "{case}: a pane lost its cat mid-fade (one fade step is at most 17): {cut:?}"
            );
            assert!(low >= 245, "{case}: the cat dipped to {low}");
            let own = next
                .frame
                .pet
                .departures
                .iter()
                .flatten()
                .filter(|d| d.own_glass)
                .count();
            assert!(
                own == 2 && next.glass_sprites == 2,
                "{case}: two ghosts after the switch to R, each drawn in its own pane: {next:?}"
            );
            let last = all.last().expect("frames");
            assert!(
                ghost(last).is_none() && last.glass_sprites == 0 && last.frame.pet.alpha == 255,
                "{case}: the crossfade completes: {last:?}"
            );
            assert!(!h.owner.brain().has_own_glass_ghost());
        }
    }
}

/// The cat in each of `Host::three_settled`'s panes (L, M, R by window x)
/// this frame, summed as drawn: the focused pane's arriver and its
/// same-glass ghosts, and each window-channel ghost in the pane it stands in.
fn by_pane(shot: &Shot, cw: i32) -> [u16; 3] {
    let pane = |x: i32| ((x / cw) / 33).clamp(0, 2) as usize;
    let mut out = [0u16; 3];
    if !shot.cat() {
        return out;
    }
    let focused = pane(shot.pane_at.0);
    out[focused] += u16::from(shot.frame.pet.alpha);
    for d in shot
        .frame
        .pet
        .departures
        .iter()
        .flatten()
        .filter(|d| !d.own_glass)
    {
        out[focused] += u16::from(drawn_alpha(shot, d));
    }
    for &(x, alpha) in shot.own.iter().flatten() {
        out[pane(x)] += u16::from(alpha);
    }
    out
}

/// An incoherent frame holds, and a sub-row scroll is not incoherent. A stale
/// snapshot (cells extracted before a scroll the facts saw) certifies nothing, so
/// the pet is held where it was drawn, at full opacity, for the visibility hold's
/// blind grace, then faded. A sub-row scroll offset (a glide's tail, the overscroll
/// bounce) is observed coherently: the renderer translates the grid band and the
/// sprite together, so the pet does not change.
///
/// Controls: the stale snapshot is refused, and so is the model's `Buggy` blind
/// frame.
#[test]
fn an_incoherent_frame_holds_the_pet_and_a_sub_row_scroll_moves_nothing() {
    let model = pet_surface_seam_model();
    let mut h = Host::settled();
    let before = h.present();
    assert!(before.observed > 0, "fixture: a coherent observation");

    for frac in [-5, 3, -11, 7] {
        h.frac = frac;
        let shot = h.present();
        assert!(
            shot.observed > 0 && shot.reason != "incoherent-surface",
            "a sub-row offset of {frac} px is coherent: {shot:?}"
        );
        assert_eq!(
            (
                shot.frame.on_glass,
                shot.frame.pet.alpha,
                shot.frame.pet.col,
                shot.frame.pet.row
            ),
            (true, 255, before.frame.pet.col, before.frame.pet.row),
            "a sub-row offset of {frac} px changed the pet"
        );
    }
    h.frac = 0;

    let mut state = model.init_state();
    step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
    h.stale = true;
    let at_seam = project(&state, "Blind", &h.owner, &before, None);
    conform(&model, &mut state, "Blind", at_seam).expect("blind");
    let blind = h.present();
    assert_eq!(blind.observed, 0, "control: the stale snapshot is refused");
    assert_eq!(blind.reason, "incoherent-surface");
    let post = project(&state, "Tick", &h.owner, &before, Some(&blind));
    let mut cut = post.clone();
    cut.insert("lit", 0);
    cut.insert("hit", 0);
    assert!(
        conform(&model, &mut state.clone(), "Tick", cut).is_err(),
        "control: the shipped cut is refused"
    );
    conform(&model, &mut state, "Tick", post).expect("a blind frame holds");
    assert_eq!(
        (
            blind.frame.pet.alpha,
            blind.frame.pet.col,
            blind.frame.pet.row
        ),
        (255, before.frame.pet.col, before.frame.pet.row)
    );
    let grace = h.shots(Duration::from_millis(400));
    assert!(
        grace.iter().all(|s| s.frame.on_glass),
        "held through the grace: {grace:?}"
    );
    let gone = h.run(Duration::from_millis(1_000));
    let post = project(&state, "BlindLong", &h.owner, &before, Some(&gone));
    conform(&model, &mut state, "BlindLong", post).expect("the grace is bounded");
    assert!(!gone.frame.on_glass);

    // Coherence returns: the cat comes back where it belongs.
    h.stale = false;
    let back = h.run(Duration::from_secs(2));
    assert!(back.frame.on_glass && back.frame.pet.alpha == 255);
    assert!(!jumped(&before, &back), "{back:?}");
}

/// A tall screen crossfades the body it replaced. A surface is judged in a bounded
/// window of rows around its caret (`pet_world::MAX_WORLD_ROWS`), so on a 90-row
/// terminal a program with its caret at the bottom cannot see where the shell's body
/// stood, nor the shell the program's. Unknown is not evidence: the ghost keeps its
/// own surface's verdict at the boundary and fades where it was drawn, beneath
/// whatever ink is there, entering the program and quitting it.
///
/// The one guarantee that forbids the ghost (`Refuse`): protected cells under the
/// old body, here double-width rows, whether the world sees both rows or only the
/// feet row with the head outside its window. No ghost is drawn and the arriver
/// carries its presence.
#[test]
fn a_tall_screen_crossfades_the_body_it_replaced() {
    let model = pet_surface_seam_model();
    for case in ["enter", "quit", "protected", "half-seen"] {
        let mut h = match case {
            "protected" => Host::new(30, 100),
            _ => Host::new(90, 100),
        };
        if case == "quit" {
            // The program is up, its caret at the bottom; the cat settled
            // there.
            h.term.process(b"\x1b[?1049h\x1b[2J\x1b[90;5H");
            h.owner.resight_owner();
        }
        let before = h.run(Duration::from_secs(3));
        assert!(
            before.frame.on_glass && before.frame.pet.alpha == 255,
            "{case}: fixture: the cat settled: {before:?}"
        );
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
        match case {
            "enter" => h.term.process(b"\x1b[?1049h\x1b[2J\x1b[90;5H"),
            "quit" => h.term.process(b"\x1b[?1049l"),
            "half-seen" => {
                // The world's window starts at the old body's feet row (its
                // caret 32 rows below it), which is double-width; the head's
                // row above is outside the window.
                let feet = before.frame.pet.row as u16;
                let screen = format!(
                    "\x1b[?1049h\x1b[2J\x1b[{};1H\x1b#6\x1b[{};5H",
                    feet + 1,
                    feet + 33
                );
                h.term.process(screen.as_bytes());
            }
            _ => h
                .term
                .process(&protected_screen(before.frame.pet.row as u16 + 1, 26)),
        }
        h.owner.resight_owner();
        step(&model, &mut state, "Replace", &h.owner, &before, None)
            .unwrap_or_else(|e| panic!("{case}: the boundary is dark: {e}"));
        let refused = case.starts_with("protected") || case == "half-seen";
        if refused {
            let post = project(&state, "Refuse", &h.owner, &before, None);
            conform(&model, &mut state, "Refuse", post).expect("refuse");
        }
        let after = h.shots(Duration::from_millis(400));
        let first = after[0];
        let low = after.iter().map(total_presence).min().unwrap_or(0);
        let high = after.iter().map(total_presence).max().unwrap_or(0);
        eprintln!(
            "tall screen, {case}: the first frame's arriver {}, ghost {:?}; the cat summed \
             {low}..={high}/255",
            first.frame.pet.alpha,
            ghost(&first).map(|g| drawn_alpha(&first, &g))
        );
        let mut post = project(&state, "Tick", &h.owner, &before, Some(&first));
        post.insert("dipped", i64::from(total_presence(&first) < 240));
        conform(&model, &mut state, "Tick", post).unwrap_or_else(|e| panic!("{case}: {e}"));
        if refused {
            assert!(
                ghost(&first).is_none() && first.frame.pet.alpha >= 240,
                "{case}: no ghost over protected cells, and the arriver carries its presence: \
                 {first:?}"
            );
        } else {
            let g = ghost(&first).expect("the old body fades as a ghost");
            assert!(
                g.under_ink && (g.col, g.row) == (before.frame.pet.col, before.frame.pet.row),
                "{case}: where the body was drawn, beneath whatever ink the new world cannot \
                 see: {g:?}"
            );
            let tails = tails(&after);
            assert!(
                tails.iter().all(|&t| t <= 1),
                "{case}: the ghost left glass early: {tails:?}"
            );
        }
        assert!(
            low >= 245 && high <= 275,
            "{case}: the cat dipped or doubled: {low}..={high}/255: {after:?}"
        );
        let mut post = project(&state, "Spend", &h.owner, &before, None);
        post.insert("tail", i64::from(tails(&after).iter().any(|&t| t > 1)));
        conform(&model, &mut state, "Spend", post).unwrap_or_else(|e| panic!("{case}: {e}"));
        let last = after.last().expect("frames");
        assert!(
            ghost(last).is_none() && last.frame.pet.alpha == 255,
            "{case}: the crossfade completes: {last:?}"
        );
    }
}

/// A full-screen program whose rows `feet - 1..=feet` are double width (an
/// expanded row: protected, a ghost may not fade over it), its caret on
/// `caret_row`.
fn protected_screen(feet: u16, caret_row: u16) -> Vec<u8> {
    let mut screen = b"\x1b[?1049h\x1b[2J".to_vec();
    for row in feet - 1..=feet {
        screen.extend_from_slice(format!("\x1b[{row};1H\x1b#6").as_bytes());
    }
    screen.extend_from_slice(format!("\x1b[{caret_row};5H").as_bytes());
    screen
}

/// A refused ghost is handed over once. A full-screen program with expanded rows
/// under the old body refuses its ghost, and the arriver carries its presence for
/// good: the ghost leaves the lane on the frame it is refused. A few frames later
/// the program quits, the focus moves to the other pane, or the rows go single width
/// (the protection lifts); throughout, the cat sums to one cat and the arriver never
/// falls in one frame.
#[test]
fn a_refused_ghost_is_handed_over_once() {
    for (case, k) in [
        ("quit", 1usize),
        ("quit", 2),
        ("quit", 4),
        ("switch", 2),
        ("lift", 3),
    ] {
        let label = format!("{case} after {k}");
        let mut h = if case == "switch" {
            Host::split_settled()
        } else {
            Host::new(30, 100)
        };
        let before = h.run(Duration::from_secs(3));
        assert!(
            before.frame.on_glass && before.frame.pet.alpha == 255,
            "{label}: fixture: the cat settled: {before:?}"
        );
        let feet = before.frame.pet.row as u16 + 1;
        h.term.process(&protected_screen(feet, 26));
        h.owner.resight_owner();
        let mut shots: Vec<Shot> = (0..k).map(|_| h.present()).collect();
        assert!(
            ghost(&shots[0]).is_none() && shots[0].frame.pet.alpha >= 240,
            "{label}: fixture: the ghost refused, its presence on the arriver: {:?}",
            shots[0]
        );
        match case {
            "quit" => {
                h.term.process(b"\x1b[?1049l");
                h.owner.resight_owner();
            }
            "switch" => h.focus_other(),
            _ => h.term.process(
                format!("\x1b[{};1H\x1b#5\x1b[{feet};1H\x1b#5\x1b[26;5H", feet - 1).as_bytes(),
            ),
        }
        shots.extend(h.shots(Duration::from_millis(400)));
        let sums: Vec<u16> = shots.iter().map(total_presence).collect();
        let (low, high) = (
            sums.iter().copied().min().unwrap_or(0),
            sums.iter().copied().max().unwrap_or(0),
        );
        eprintln!("refused, then {label}: the cat summed {low}..={high}/255: {sums:?}");
        assert!(
            low >= 245 && high <= 275,
            "{label}: the cat dipped or doubled: {sums:?}"
        );
        if case == "lift" {
            let arriver: Vec<u8> = shots.iter().map(|s| s.frame.pet.alpha).collect();
            assert!(
                arriver.windows(2).all(|w| w[1] >= w[0])
                    && shots.iter().all(|s| ghost(s).is_none()),
                "{label}: the refused ghost came back: {arriver:?}"
            );
        }
    }
}

/// The drawn presence of every ghost in `coat` this frame, summed.
fn coat_presence(shot: &Shot, coat: u8) -> u16 {
    if !shot.cat() {
        return 0;
    }
    shot.frame
        .pet
        .departures
        .iter()
        .flatten()
        .filter(|d| d.coat == coat)
        .map(|d| u16::from(drawn_alpha(shot, d)))
        .sum()
}

/// The breed handoff's leaver crosses a boundary where it was drawn. A new cat takes
/// over (`PetArrival::Ceremony`: the old coat runs off while the new fades up;
/// `Quiet`: the old coat fades in place), and mid-handoff a full-screen program
/// starts or the focus moves to the other pane. The leaver is carried with the
/// crossfade ghosts, stopped where the user saw it, and fades there on the arriver's
/// ramp (in its own pane for the split): no fall larger than one fade step, and it
/// leaves glass on the arriver's full frame (`GhostLeavesAtFullPresence`).
///
/// What is carried is what was drawn: a selection over the leaver, clear of the
/// body, culls it before the seam, so nothing of it crosses and the old coat is
/// never brighter after the boundary than before it.
#[test]
fn a_breed_leaver_crosses_a_boundary_where_it_was_drawn() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_effects::companion::CompanionRung;
    use aterm_effects::kitty_pet::PetArrival;
    use aterm_effects::kitty_registry::coat_distance;
    let model = pet_surface_seam_model();
    for (case, arrival, frames_in, selected) in [
        ("screen", PetArrival::Ceremony, 10usize, false),
        ("split", PetArrival::Ceremony, 30, false),
        ("screen", PetArrival::Quiet, 3, false),
        ("screen", PetArrival::Ceremony, 25, true),
        ("split", PetArrival::Ceremony, 30, true),
    ] {
        let label = format!(
            "{case} {arrival:?}{}",
            if selected { " under a selection" } else { "" }
        );
        let mut h = if case == "split" {
            Host::split_settled()
        } else {
            Host::settled()
        };
        let old = h.owner.brain().worn_pair().expect("dressed");
        let coat = (0..16u8)
            .find(|&c| coat_distance(old.0, c) >= 40.0)
            .expect("a far coat");
        h.owner
            .set_look((coat, old.1), arrival, CompanionRung::Program);
        // The handoff fires after its debounce, at a cat kept awake.
        let mut fired = false;
        for k in 0..400 {
            if k % 60 == 0 {
                h.term.process(b"l");
            }
            if coat_presence(&h.present(), old.0) > 0 {
                fired = true;
                break;
            }
        }
        assert!(fired, "{label}: fixture: the handoff fired");
        let mut shots = vec![];
        for _ in 0..frames_in {
            shots.push(h.present());
        }
        if selected {
            // A block selection over the leaver's side of the body's rows,
            // clear of the body: the console culls the leaver on every frame
            // and keeps the body.
            let shot = *shots.last().expect("frames");
            let (cw, ch) = (i32::from(h.cell.0), i32::from(h.cell.1));
            let (x0, y0, x1, y1) = shot.body_win.expect("fixture: the body on glass");
            let (px, py) = shot.pane_at;
            let leaver = shot
                .frame
                .pet
                .departures
                .iter()
                .flatten()
                .find(|d| d.coat == old.0)
                .expect("fixture: the leaver");
            let (from, to) = if leaver.col * h.cell.0 as f32 >= (x1 - px) as f32 {
                ((x1 - px) / cw + 1, i32::from(h.grid().1) - 1)
            } else {
                (0, (x0 - px) / cw - 1)
            };
            let selection = h.term.text_selection_mut();
            selection.start_selection(
                (y0 - py) / ch,
                from as u16,
                SelectionSide::Left,
                SelectionType::Block,
            );
            selection.update_selection((y1 - 1 - py) / ch, to as u16, SelectionSide::Right);
            for _ in 0..3 {
                shots.push(h.present());
            }
        }
        let before = *shots.last().expect("frames");
        let seen = coat_presence(&before, old.0);
        if selected {
            assert!(
                seen == 0 && before.frame.pet.alpha > 0,
                "{label}: fixture: the selection culls the leaver and not the body: {before:?}"
            );
        } else {
            assert!(
                seen > 192,
                "{label}: fixture: the leaver on glass at the boundary: {before:?}"
            );
        }
        let mut state = model.init_state();
        step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
        if selected {
            let post = project(&state, "Handoff", &h.owner, &before, Some(&before));
            assert_eq!(post["leaver"], 0, "{label}: fixture: no leaver on glass");
        } else {
            step(
                &model,
                &mut state,
                "Handoff",
                &h.owner,
                &before,
                Some(&before),
            )
            .expect("leaver");
            assert_eq!(
                state["leaver"], 1,
                "{label}: fixture: the leaver is on glass"
            );
        }
        if case == "split" {
            h.focus_other();
        } else {
            h.term.process(&tui_bytes());
            h.owner.resight_owner();
        }
        step(&model, &mut state, "Replace", &h.owner, &before, None).expect("dark boundary");
        if case == "split" {
            step(&model, &mut state, "Shift", &h.owner, &before, None).expect("shift");
        }
        let after = h.shots(Duration::from_millis(400));
        if selected {
            let carried: Vec<u16> = after.iter().map(|s| coat_presence(s, old.0)).collect();
            let sums: Vec<u16> = after.iter().map(total_presence).collect();
            eprintln!(
                "breed leaver, {label}: the cat {} before the boundary, the old coat {seen}; \
                 after it the old coat {carried:?}, the cat {sums:?}",
                total_presence(&before)
            );
            assert!(
                carried.iter().all(|&a| a <= seen),
                "{label}: a leaver nobody saw came onto glass: {seen} before, {carried:?} after"
            );
            // Nor is it handed to the arriver: the cat sums to one cat, the
            // crossfade's rounding allowed.
            assert!(
                sums.iter().all(|&s| s <= 275),
                "{label}: a leaver nobody saw doubled the cat: {sums:?}"
            );
        }
        step(
            &model,
            &mut state,
            "Tick",
            &h.owner,
            &before,
            Some(&after[0]),
        )
        .unwrap_or_else(|e| panic!("{label}: {e}"));
        if selected {
            // The selection cut the leaver, not the boundary: what it owed
            // as it left glass is the selection's, and nothing crossed.
            assert!(after.iter().all(Shot::cat), "{label}: a frame with no cat");
            continue;
        }
        shots.extend(after.iter().copied());
        let leaver: Vec<u16> = shots.iter().map(|s| coat_presence(s, old.0)).collect();
        let fall = leaver
            .windows(2)
            .map(|w| w[0].saturating_sub(w[1]))
            .max()
            .unwrap_or(0);
        // What the leaver still owed when it left glass: its presence then
        // times what its arriver was still missing.
        let gone = leaver.iter().rposition(|&a| a > 0).expect("drawn") + 1;
        let tail = shots.get(gone).map_or(0, |s| {
            u32::from(leaver[gone - 1]) * u32::from(255 - s.frame.pet.alpha) / 255
        });
        eprintln!(
            "breed leaver, {label}: {} at the boundary, its largest one-frame fall {fall}, owed \
             {tail} as it left",
            leaver[frames_in - 1]
        );
        assert!(
            fall <= 20,
            "{label}: the leaver fell by {fall} in one frame: {leaver:?}"
        );
        let mut post = project(&state, "Spend", &h.owner, &before, None);
        post.insert("tail", i64::from(tail > 1));
        conform(&model, &mut state, "Spend", post)
            .unwrap_or_else(|e| panic!("{label}: the leaver left glass early: {e} {leaver:?}"));
        assert!(after.iter().all(Shot::cat), "{label}: a frame with no cat");
    }
}

/// A ghost its own glass refuses hands its presence to the arriver: a split-pane
/// switch the frame after a stale snapshot of the pane the focus leaves (the body
/// held lit, nothing under it certified). That pane cannot admit the ghost (`Refuse`
/// after a `Shift`), so none is drawn there, and the arriver comes up from the
/// presence the user saw.
#[test]
fn a_ghost_its_own_glass_refuses_hands_its_presence_to_the_arriver() {
    let model = pet_surface_seam_model();
    let mut h = Host::split_settled();
    h.stale = true;
    let before = h.present();
    h.stale = false;
    assert!(
        before.frame.on_glass && before.frame.pet.alpha == 255 && before.observed == 0,
        "fixture: the body held lit over an observation that certifies nothing: {before:?}"
    );
    let mut state = model.init_state();
    step(&model, &mut state, "Draw", &h.owner, &before, Some(&before)).expect("drawn");
    h.focus_other();
    step(&model, &mut state, "Replace", &h.owner, &before, None).expect("dark boundary");
    step(&model, &mut state, "Shift", &h.owner, &before, None).expect("shift");
    let post = project(&state, "Refuse", &h.owner, &before, None);
    conform(&model, &mut state, "Refuse", post).expect("refuse");
    let after = h.shots(Duration::from_millis(400));
    let first = after[0];
    let low = after.iter().map(total_presence).min().unwrap_or(0);
    eprintln!(
        "own glass refused: the first frame's arriver {}, {} ghosts; the cat summed at least \
         {low}/255",
        first.frame.pet.alpha,
        first.frame.pet.departures.iter().flatten().count()
    );
    let mut post = project(&state, "Tick", &h.owner, &before, Some(&first));
    post.insert("dipped", i64::from(total_presence(&first) < 240));
    conform(&model, &mut state, "Tick", post).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        first.glass_sprites == 0 && first.frame.pet.alpha >= 240,
        "no ghost on the refusing glass, and the arriver carries its presence: {first:?}"
    );
    assert!(low >= 245, "the cat dipped to {low}/255: {after:?}");
}

/// A carried ghost keeps its own tile. The emitter bakes at most two new tiles a
/// frame and re-emits a ghost's held tile when its bake is deferred, the hold keyed
/// on the ghost (`PetDepartureSprite::id`). A breed homecoming's leaver carried to a
/// tab of another background is the third new tile of the frame after the switch,
/// so it is drawn in the tile it wore a frame ago; every ghost on the lane is drawn
/// on every frame.
#[test]
fn a_carried_ghost_keeps_its_own_tile() {
    use aterm_effects::companion::CompanionRung;
    use aterm_effects::kitty_pet::PetArrival;
    use aterm_effects::kitty_registry::coat_distance;
    let mut h = Host::settled();
    let old = h.owner.brain().worn_pair().expect("dressed");
    let coat = (0..16u8)
        .find(|&c| coat_distance(old.0, c) >= 40.0)
        .expect("a far coat");
    h.owner
        .set_look((coat, old.1), PetArrival::Quiet, CompanionRung::Program);
    let mut fired = false;
    for k in 0..400 {
        if k % 60 == 0 {
            h.term.process(b"l");
        }
        if coat_presence(&h.present(), old.0) > 0 {
            fired = true;
            break;
        }
    }
    assert!(fired, "fixture: the homecoming fired");
    // The other tab: a light background under its prompt.
    let mut tab = Terminal::new(30, 100);
    let mut paint = b"\x1b]11;#f0f0f0\x07\x1b[47m".to_vec();
    for row in 1..=30 {
        paint.extend_from_slice(format!("\x1b[{row};1H\x1b[2K").as_bytes());
    }
    paint.extend_from_slice(b"\x1b[21;1H$ ");
    tab.process(&paint);
    std::mem::swap(&mut h.term, &mut tab);
    h.session = 9;
    h.owner.resight_coordinate_space();
    h.owner.resight_owner();
    let after = h.shots(Duration::from_millis(400));
    assert!(
        coat_presence(&after[0], old.0) > 0,
        "fixture: the leaver is carried to the tab: {:?}",
        after[0]
    );
    for (k, shot) in after.iter().enumerate() {
        if shot.frame.pet.motes.iter().any(Option::is_some) {
            continue;
        }
        let ghosts = shot
            .frame
            .pet
            .departures
            .iter()
            .flatten()
            .filter(|d| !d.own_glass && drawn_alpha(shot, d) > 0)
            .count();
        let body = usize::from(shot.frame.pet.alpha > 0);
        assert_eq!(
            shot.sprites,
            ghosts + body,
            "frame {k}: every body on the lane is drawn: {shot:?}"
        );
    }
}

/// A drawn rect `(x0, y0, x1, y1)` as `(w, h, x0, y0)`, for the prints.
fn dims(r: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    (r.2 - r.0, r.3 - r.1, r.0, r.1)
}

/// Drive a settled host until its body is drawn `caught`: mid-breath (a fresh pet
/// sleeps and breathes), mid-stretch (a key wakes it), or mid-hop (awake, Enter
/// sends it to the next row). Returns that frame, the last before the boundary.
fn caught(h: &mut Host, caught: &str) -> Shot {
    let drawn = |s: &Shot| s.frame.on_glass && s.frame.pet.alpha == 255;
    let (want, frames): (fn(&PetFrame) -> bool, usize) = match caught {
        "mid-breath" => (
            |p: &PetFrame| p.action == PetAction::Sleep && (p.scale_y - 1.0).abs() > 0.025,
            200,
        ),
        "mid-stretch" => {
            h.term.process(b"l");
            (
                |p: &PetFrame| p.pose == PetGlyphId::PetStretch && p.scale_x > 1.05,
                40,
            )
        }
        _ => {
            for _ in 0..3 {
                h.term.process(b"l");
                h.present();
                h.present();
            }
            h.run(Duration::from_millis(500));
            h.term.process(b"\r\n$ ");
            (|p: &PetFrame| p.lift > 0.3, 60)
        }
    };
    for _ in 0..frames {
        let shot = h.present();
        if drawn(&shot) && want(&shot.frame.pet) {
            return shot;
        }
    }
    panic!("fixture: the cat {caught} on glass");
}

/// A ghost is drawn as the body the user last saw: at a split-pane switch or an
/// alternate-screen enter the body becomes a still ghost at its cell, pose, facing
/// and the squash, stretch and lift its last frame drew (`Departure::stance0`), so
/// its rect is the body's to the pixel on every frame of its fade, mid-breath,
/// mid-stretch and mid-hop. An arriver adopting it on a quick return is drawn at the
/// ghost's rect on the adoption frame and eases from there, at most a pixel an edge
/// per frame while it wears the adopted look.
#[test]
fn a_ghost_is_drawn_as_the_body_the_user_last_saw() {
    let mut wrong = Vec::new();
    for case in ["mid-breath", "mid-stretch", "mid-hop"] {
        for glass in ["split", "screen"] {
            let label = format!("{case}, {glass}");
            let mut h = match glass {
                "split" => Host::split_settled(),
                _ => Host::settled(),
            };
            let before = caught(&mut h, case);
            let body = before.body_win.expect("fixture: the body on glass");
            match glass {
                "split" => h.focus_other(),
                _ => {
                    h.term.process(&tui_bytes());
                    h.owner.resight_owner();
                }
            }
            let after = h.shots(Duration::from_millis(300));
            let ghosts: Vec<(i32, i32, i32, i32)> = after
                .iter()
                .filter(|s| s.ghost_alpha > 0)
                .filter_map(|s| s.ghost_win)
                .collect();
            let p = before.frame.pet;
            eprintln!(
                "{label}: the body {:?} at scale ({:.3}, {:.3}) lift {:.3}, as {:?}; its ghost \
                 per frame {:?}",
                dims(body),
                p.scale_x,
                p.scale_y,
                p.lift,
                p.pose,
                ghosts.iter().map(|&g| dims(g)).collect::<Vec<_>>()
            );
            assert!(
                ghosts.len() >= 8,
                "{label}: fixture: the ghost fades on glass: {after:?}"
            );
            if !ghosts.iter().all(|&g| g == body) {
                wrong.push(format!("{label}: the ghost is not the body the user saw"));
            }
        }
    }

    // The adopted arriver: a quick return to the ghost's seat.
    for case in ["mid-breath", "mid-stretch"] {
        for glass in ["split", "screen"] {
            let label = format!("{case}, {glass}, back");
            let (mut h, own) = match glass {
                "split" => (Host::split_settled(), true),
                _ => (Host::settled(), false),
            };
            caught(&mut h, case);
            match glass {
                "split" => h.focus_other(),
                _ => {
                    h.term.process(&tui_bytes());
                    h.owner.resight_owner();
                }
            }
            let seen = *h.shots(Duration::from_millis(16)).last().expect("frames");
            let old = if own { own_ghost(&seen) } else { ghost(&seen) }
                .expect("fixture: the body the focus left fades as a ghost");
            let ghost_px = seen.ghost_win.expect("fixture: the ghost drawn");
            match glass {
                "split" => h.focus_other(),
                _ => {
                    h.term.process(b"\x1b[?1049l");
                    h.owner.resight_owner();
                }
            }
            let after = h.shots(Duration::from_millis(400));
            let a0 = after[0].frame.pet;
            assert!(
                at_seat(a0.col, a0.row, (old.col, old.row)),
                "{label}: fixture: the arriver adopts the ghost at its seat: {:?}",
                after[0]
            );
            let worn: Vec<(i32, i32, i32, i32)> = after
                .iter()
                .take_while(|s| {
                    (s.frame.pet.pose, s.frame.pet.facing_left) == (old.pose, old.facing_left)
                })
                .filter_map(|s| s.body_win)
                .collect();
            eprintln!(
                "{label}: the ghost {:?}; the adopted arriver per frame while it wears the \
                 ghost's look {:?}",
                dims(ghost_px),
                worn.iter().map(|&b| dims(b)).collect::<Vec<_>>()
            );
            assert!(!worn.is_empty(), "{label}: fixture: the look is worn");
            if worn[0] != ghost_px {
                wrong.push(format!(
                    "{label}: the arriver is not drawn as the ghost it adopted"
                ));
            }
            let step = worn
                .windows(2)
                .map(|w| {
                    (w[1].0 - w[0].0)
                        .abs()
                        .max((w[1].1 - w[0].1).abs())
                        .max((w[1].2 - w[0].2).abs())
                        .max((w[1].3 - w[0].3).abs())
                })
                .max()
                .unwrap_or(0);
            if step > 1 {
                wrong.push(format!(
                    "{label}: the adopted arriver snapped {step} px in one frame"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A screen on a light ground: every row painted in an SGR background (not
/// erase-in-colour), the caret at `(caret_row, 5)`; `alt` enters the alternate
/// screen first.
fn light_screen(rows: u16, cols: u16, caret_row: u16, alt: bool) -> Vec<u8> {
    let mut b = if alt {
        b"\x1b[?1049h\x1b[2J".to_vec()
    } else {
        Vec::new()
    };
    b.extend_from_slice(b"\x1b[48;2;236;236;236m\x1b[38;2;40;40;40m");
    for row in 1..=rows {
        b.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        b.extend(std::iter::repeat_n(b' ', usize::from(cols)));
    }
    b.extend_from_slice(format!("\x1b[{caret_row};5H").as_bytes());
    b
}

/// A ghost on `after` standing where a body stood on `before` (the same window rect)
/// in another atlas tile than every body there: `(rect, tile before, tile now)`.
/// The tile bakes pose, look and palette, so this is that body repainted.
type Repaint = ((i32, i32, i32, i32), (u16, u16), (u16, u16));

fn repainted(before: &Shot, after: &Shot) -> Vec<Repaint> {
    after
        .pets
        .iter()
        .flatten()
        .filter(|d| !d.body && d.alpha > 0)
        .filter_map(|d| {
            let was: Vec<&Drawn> = before
                .pets
                .iter()
                .flatten()
                .filter(|b| b.rect == d.rect)
                .collect();
            (!was.is_empty() && was.iter().all(|b| b.tile != d.tile))
                .then(|| (d.rect, was[0].tile, d.tile))
        })
        .collect()
}

/// The ghosts on `after` standing where a body of the cat stood on `before`.
fn ghosts_in_place(before: &Shot, after: &Shot) -> usize {
    after
        .pets
        .iter()
        .flatten()
        .filter(|d| !d.body && d.alpha > 0)
        .filter(|d| before.pets.iter().flatten().any(|b| b.rect == d.rect))
        .count()
}

/// A ghost wears the palette its body was drawn in. The pet's contrast palette
/// (`CatColorKey`) is frozen per appearance, sampled under the body, and an arriver
/// on a new surface samples its own. On every frame of its fade a ghost wears the
/// atlas tile (pose, look and palette) its body wore on the last frame before the
/// boundary: alternate-screen enter and leave, a tab switch, a split-pane switch, a
/// tall screen, the breed handoff's leaver, and a quick return, where the adopting
/// arriver goes on in that tile too (on the ghost's own surface:
/// [`an_arriver_resumes_a_ghosts_palette_only_on_the_surface_it_was_drawn_on`]).
///
/// Negative control: a theme change at the boundary
/// (`CompanionOwner::invalidate_colors`) retires every frozen palette, and the
/// repainted ghost is seen.
#[test]
fn a_ghost_wears_the_palette_its_body_was_drawn_in() {
    use aterm_effects::companion::CompanionRung;
    use aterm_effects::kitty_pet::PetArrival;
    use aterm_effects::kitty_registry::coat_distance;
    let mut wrong = Vec::new();
    for case in [
        "alt enter",
        "alt leave",
        "tab switch",
        "split switch",
        "tall",
        "breed leaver",
        "quick return, screen",
        "quick return, split",
        "quick return, repainted ground",
        "theme change (negative control)",
    ] {
        let mut h = match case {
            "tall" => Host::new(90, 100),
            "split switch" | "quick return, split" => Host::split_settled(),
            _ => Host::settled(),
        };
        if let Some(other) = h.other.as_mut() {
            // The other pane on a light ground, its prompt where it was.
            other.term.process(&light_screen(30, 50, 21, false));
            other.term.process(b"$ ");
        }
        if case == "alt leave" {
            h.term.process(&light_screen(30, 100, 25, true));
            h.owner.resight_owner();
        }
        let mut before = h.run(Duration::from_secs(3));
        if case == "quick return, repainted ground" {
            // The ground under the settled cat turns light: its appearance keeps
            // the palette it was first drawn in, where a fresh sample would pick
            // another.
            let mut paint = b"\x1b7\x1b[48;2;236;236;236m".to_vec();
            for row in 9..=10 {
                paint.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
                paint.extend(std::iter::repeat_n(b' ', 100));
            }
            paint.extend_from_slice(b"\x1b[11;3H");
            paint.extend(std::iter::repeat_n(b' ', 98));
            paint.extend_from_slice(b"\x1b[0m\x1b8");
            h.term.process(&paint);
            before = h.run(Duration::from_secs(1));
        }
        if case == "breed leaver" {
            let old = h.owner.brain().worn_pair().expect("dressed");
            let coat = (0..16u8)
                .find(|&c| coat_distance(old.0, c) >= 40.0)
                .expect("a far coat");
            h.owner
                .set_look((coat, old.1), PetArrival::Ceremony, CompanionRung::Program);
            let mut fired = false;
            for k in 0..400 {
                if k % 60 == 0 {
                    h.term.process(b"l");
                }
                if coat_presence(&h.present(), old.0) > 0 {
                    fired = true;
                    break;
                }
            }
            assert!(fired, "{case}: fixture: the handoff fired");
            before = *h.shots(Duration::from_millis(160)).last().expect("frames");
            assert!(
                coat_presence(&before, old.0) > 192,
                "{case}: fixture: the leaver on glass: {before:?}"
            );
        }
        assert!(
            before.frame.on_glass && before.frame.pet.alpha > 0,
            "{case}: fixture: the cat on glass: {before:?}"
        );
        let mut was = h.owner.brain().appearance_colors();
        let boundary = |h: &mut Host| match case {
            "alt enter"
            | "breed leaver"
            | "quick return, screen"
            | "quick return, repainted ground" => {
                h.term.process(&light_screen(30, 100, 25, true));
                h.owner.resight_owner();
            }
            "theme change (negative control)" => {
                h.term.process(&light_screen(30, 100, 25, true));
                h.owner.resight_owner();
                h.owner.invalidate_colors();
            }
            "alt leave" => {
                h.term.process(b"\x1b[?1049l");
                h.owner.resight_owner();
            }
            "tab switch" => {
                let mut tab = Terminal::new(30, 100);
                tab.process(&light_screen(30, 100, 21, false));
                tab.process(b"$ ");
                std::mem::swap(&mut h.term, &mut tab);
                h.session = 9;
                h.owner.resight_owner();
            }
            "tall" => {
                h.term.process(&light_screen(90, 100, 90, true));
                h.owner.resight_owner();
            }
            _ => h.focus_other(),
        };
        boundary(&mut h);
        let born = h.shots(Duration::from_millis(16));
        if !case.contains("negative control") && !case.starts_with("quick return") {
            // Every ghost the boundary made carries the palette its body was
            // drawn in.
            let carried: Vec<_> = born[0]
                .frame
                .pet
                .departures
                .iter()
                .flatten()
                .map(|d| d.colors)
                .collect();
            if carried.is_empty() || carried.iter().any(|c| c.is_none()) {
                wrong.push(format!("{case}: a ghost without its palette: {carried:?}"));
            }
        }
        if case.starts_with("quick return") {
            // Two frames into the first crossfade, the focus comes back.
            before = *born.last().expect("frames");
            was = h.owner.brain().appearance_colors();
            match case {
                "quick return, split" => h.focus_other(),
                _ => {
                    h.term.process(b"\x1b[?1049l");
                    h.owner.resight_owner();
                }
            }
        }
        let after = if case.starts_with("quick return") {
            h.shots(Duration::from_millis(300))
        } else {
            let mut after = born;
            after.extend(h.shots(Duration::from_millis(270)));
            after
        };
        let now = h.owner.brain().appearance_colors();
        let ghost_tiles: Vec<Vec<(u16, u16)>> = after
            .iter()
            .take(3)
            .map(|s| {
                s.pets
                    .iter()
                    .flatten()
                    .filter(|d| !d.body && d.alpha > 0)
                    .map(|d| d.tile)
                    .collect()
            })
            .collect();
        let body_tiles: Vec<(u16, u16)> = before
            .pets
            .iter()
            .flatten()
            .filter(|d| d.alpha > 0)
            .map(|d| d.tile)
            .collect();
        let bad: Vec<Repaint> = after.iter().flat_map(|s| repainted(&before, s)).collect();
        eprintln!(
            "{case}: the palette before {was:?}, the arriver's after {now:?}; the bodies' tiles \
             before {body_tiles:?}, the ghosts' on the first frames {ghost_tiles:?}; {} \
             repainted ghost frames",
            bad.len()
        );
        assert!(
            ghosts_in_place(&before, &after[0]) >= 1,
            "{case}: fixture: a ghost stands where the body was drawn: {:?}",
            after[0]
        );
        if case.contains("negative control") {
            assert!(!bad.is_empty(), "{case}: the check sees a repainted ghost");
            continue;
        }
        if !bad.is_empty() {
            wrong.push(format!(
                "{case}: a ghost repainted on {} frames, first {:?}",
                bad.len(),
                bad[0]
            ));
        }
        if case.starts_with("quick return") {
            // On the adoption frame the arriver is that body, in the tile the
            // user saw.
            let body = after[0]
                .pets
                .iter()
                .flatten()
                .find(|d| d.body)
                .expect("the arriver drawn");
            let seen: Vec<(u16, u16)> = before
                .pets
                .iter()
                .flatten()
                .filter(|b| b.rect == body.rect)
                .map(|b| b.tile)
                .collect();
            assert!(
                !seen.is_empty(),
                "{case}: fixture: the arriver adopts the ghost at its seat"
            );
            if !seen.contains(&body.tile) {
                wrong.push(format!(
                    "{case}: the adopted arriver is repainted: {seen:?} -> {:?}",
                    body.tile
                ));
            }
        } else if now == was {
            wrong.push(format!(
                "{case}: fixture or law: the arriver did not pick its own surface's palette"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An arriver that adopts a carried ghost goes on in the ghost's palette only on the
/// surface the ghost was drawn on. A quick tab cycle A (dark) → B (dark) → C
/// (light), C's caret on A's cell: C's arriver adopts A's ghost there and wears the
/// palette a cat arriving on C straight from A picks, not A's dark-ground palette,
/// whose pale outline all but vanishes on a light ground.
#[test]
fn an_arriver_resumes_a_ghosts_palette_only_on_the_surface_it_was_drawn_on() {
    let tab = |rows, cols, light: bool| {
        let mut t = Terminal::new(rows, cols);
        if light {
            // A light ground, `$ ` on row 11: the caret on A's cell.
            t.process(&light_screen(rows, cols, 11, false));
            t.process(b"\x1b[11;1H$ ");
        } else {
            t.process(b"\x1b]11;#000000\x07\x1b[21;1H$ ");
        }
        t
    };
    let switch = |h: &mut Host, to: Terminal, session: u64| {
        h.term = to;
        h.session = session;
        h.owner.resight_owner();
    };
    // The palette a cat coming to C straight from A picks.
    let direct = {
        let mut h = Host::settled();
        switch(&mut h, tab(30, 100, true), 11);
        h.run(Duration::from_secs(1));
        h.owner.brain().appearance_colors()
    };
    let mut wrong = Vec::new();
    for frames_on_b in [1, 2, 4, 8] {
        let mut h = Host::settled();
        let seen = h.run(Duration::from_millis(16));
        let on_a = h.owner.brain().appearance_colors();
        assert!(
            on_a.is_some() && on_a != direct,
            "fixture: A's ground and C's pick different palettes: {on_a:?} {direct:?}"
        );
        switch(&mut h, tab(30, 100, false), 9);
        for _ in 0..frames_on_b {
            h.present();
        }
        switch(&mut h, tab(30, 100, true), 11);
        let arrived = h.present();
        assert_eq!(
            arrived.body_win, seen.body_win,
            "{frames_on_b}: fixture: C's arriver adopts A's ghost at its seat"
        );
        let mut palettes = Vec::new();
        for span in [300, 1000, 3000, 6000] {
            h.run(Duration::from_millis(span));
            palettes.push(h.owner.brain().appearance_colors());
        }
        eprintln!("{frames_on_b} frames on B: C's arriver wears {palettes:?}, direct {direct:?}");
        if palettes.iter().any(|p| *p != direct) {
            wrong.push(format!(
                "{frames_on_b} frames on B: C's arriver wears {palettes:?}, not C's {direct:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
