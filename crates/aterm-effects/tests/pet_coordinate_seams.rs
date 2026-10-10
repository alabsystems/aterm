// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESIDENT ACROSS A COORDINATE SEAM (2026-10-01).
//!
//! The owner saw the cursor kitty pet blink: gone for a frame, back 0.3 s
//! later at another station, often in another pose. Two mechanisms, both on
//! the pet's coordinate seams:
//!
//! * every seam RETIRED the resident (`PetBrain::retire_coordinate_space`:
//!   alpha zero, a fresh sighting) — a window resize (on every frame of a
//!   live drag), a font step, a DPI change, a zoom, an alt-screen enter or
//!   exit (launching or quitting a full-screen program), a tab or pane
//!   switch;
//! * an INCOHERENT world observation cut the frame to alpha zero with no
//!   fade, and a sub-row scroll offset (a trackpad glide's tail, the
//!   overscroll bounce at the live bottom) was refused as incoherent.
//!
//! The laws below drive the shipping `CompanionOwner` over a real `Terminal`
//! in the native frame order (observe, prepare, tick, rescan) and conform each
//! seam to `PetSurfaceSeam` (`aterm_spec::derive::pet_surface_seam_model`).
//! NEGATIVE CONTROLS: the shipped seams (`retire_coordinate_space`,
//! `retire_owner`) are replayed on the same real state and reproduce the cut
//! — a frame with no cat — which the model refuses.
//!
//! The residual those laws left (2026-10-01): a split-pane focus switch moved
//! the pet's pixel frame to the other pane, the re-sighting ghost was dropped
//! with it, and the cat vanished from the old pane in one frame. Its ghost now
//! carries the glass it was drawn on and fades there.
//!
//! The residuals after the quick return (2026-10-08): an arriver that adopted
//! its own ghost changed pose in place on the adoption frame, and with three
//! panes the owner kept one own glass, so a ghost in the first pane the focus
//! left was cut at the second switch. Every ghost now knows its own pane, and
//! an adopted body goes on in the look the user saw.
//!
//! The residuals after that (2026-10-08): on a tall screen the new world
//! could not see where the old body stood and culled its ghost; a ghost a
//! real guarantee refuses took its presence with it; the breed handoff's
//! leaver vanished at any boundary; and a carried ghost whose bake was
//! deferred was not drawn. Unknown is not evidence (a protected cell the
//! world sees still is), a refused ghost hands its presence to its arriver,
//! every ghost on glass is carried, and a ghost's held tile is its own.

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
use aterm_spec::derive::{Model, pet_surface_seam_model};

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
            body_win: frame
                .body_px
                .map(|(x0, x1, y0, y1)| (x0 + pane.0, y0 + pane.1, x1 + pane.0, y1 + pane.1)),
            ghost_win: ghost_sprite.map(|(rect, _)| rect),
            ghost_alpha: ghost_sprite.map_or(0, |(_, s)| s.alpha),
            ghost_under: ghost_sprite.is_some_and(|(_, s)| s.z == FreeZ::UnderText),
            glass_sprites: window.len(),
            own: core::array::from_fn(|i| window.get(i).map(|s| (s.x, s.alpha))),
            pane_at: pane,
        }
    }

    /// A vertical split of a 30x100 window: the LEFT pane (session 8, cols
    /// 0..50, its prompt on row 20) and the RIGHT pane (session 7, cols
    /// 51..100 past the divider, its prompt on row 10), focused, with the pet
    /// settled beside its prompt. The composed host offers both as glass.
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

    /// Three panes side by side in a 30x98 window, each 32 columns: LEFT
    /// (session 8, cols 0..32, its prompt on row 20), the focused MIDDLE
    /// (session 7, cols 33..65, its prompt on row 10) with the pet settled
    /// beside its prompt, and RIGHT (session 9, cols 66..98, its prompt on
    /// row 5). The composed host offers all three as glass.
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

/// How a seam is crossed: as the hosts now do, or as they did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rule {
    Shipping,
    /// Before 2026-10-01: every seam retired the resident outright.
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
            // The old body stood at `before`'s cell, in `before`'s frame: a
            // ghost anywhere else ON GLASS — window pixels, a pixel of
            // rounding allowed — stands where the old body never was.
            let near = |a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)| {
                (a.0 - b.0).abs() <= 1 && (a.3 - b.3).abs() <= 1 && (a.2 - b.2).abs() <= 1
            };
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

/// The shipped seam, projected, is exactly the mutant model's successor.
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

/// A FONT STEP AND A LIVE WINDOW DRAG KEEP THE CAT. The same terminal
/// re-gridded — cells 8x16 to 9x18 with the grid following, then a drag
/// shrinking the window a column per frame — leaves the resident on glass on
/// every frame, within a cell of where it sat, and conforms to `Regrid`.
///
/// NEGATIVE CONTROL: the shipped seam (`retire_coordinate_space`) blanks the
/// cat on the frame after the font step and on EVERY frame of the drag, and
/// the model refuses it.
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

/// LAUNCHING AND QUITTING A FULL-SCREEN PROGRAM CROSSFADES. The alternate
/// screen enters (a TUI paints text where the pet sat, its caret at (24,4))
/// and exits again, each through the owner fence the hosts run
/// (`CompanionOwner::resight_owner`). The boundary is dark and owns no hit
/// target, as the owner-lifecycle law requires; the very next frame has the
/// replacement on glass at its new station AND the old body as a ghost
/// exactly where it was drawn — beneath the TUI's text, which stays in front
/// — fading out over a quarter second while the arriver comes up to full.
/// No frame between them is without a cat. A pixel frame that moved before
/// that frame (another pane's origin) spends no ghost.
///
/// NEGATIVE CONTROL: the shipped fence (`retire_owner`) leaves the frame
/// after the switch with no cat at all, and the model refuses it.
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

    // GLASS THAT IS GONE SPENDS NO GHOST: the same switch with the grid's
    // origin moved before the next frame and no glass offered where the old
    // body was drawn — a single grid shows one surface, so its old pixels
    // are gone.
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

/// A SPLIT-PANE FOCUS SWITCH CROSSFADES ACROSS PANES. The pet settled in
/// the right pane of a vertical split; the focus moves to the left pane, and
/// the hosts' layout and owner fences run. The boundary is dark and owns no
/// hit target; the very next frame has the arriver on glass at the LEFT
/// pane's caret, on its homecoming ramp, AND the old body as a ghost on its
/// OWN glass — in the window channel, at exactly the window pixels it was
/// drawn at, inside the right pane, beneath that pane's text — fading out
/// over a quarter second. No frame between them is without a cat.
///
/// NEGATIVE CONTROL: the shipped rule before this fix — the moved pixel
/// frame dropped the ghost, which is what an owner offered no glass still
/// does — leaves the frames after the switch with no cat, and the model
/// refuses it. CONTROLS: the ghost goes when its glass goes — its pane
/// closed at the switch or mid-fade, or a font step at the switch.
#[test]
fn a_split_pane_focus_switch_crossfades_across_panes() {
    let model = pet_surface_seam_model();
    // As shipped before this fix: a same-glass replacement crossfaded, a
    // shifted one cut.
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

    // GLASS THAT GOES TAKES ITS GHOST: the old pane closes at the switch, or
    // a font step lands with it — no pixels of the old body are left — and
    // the arriver fades in alone; the old pane closing mid-fade drops the
    // ghost on the next frame.
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

/// A QUICK RETURN CONTINUES THE ONE CAT IT COMES BACK TO (the live
/// verifier's residual B). The focus leaves a surface and comes back two
/// frames later, while the body it left is still fading there as a ghost and
/// the arriver it went to is still on its ramp — a split pane (the ghost on
/// its own glass) and a full-screen program that quits at once (the same
/// glass: the shell's screen comes back). The arriver for the surface the
/// focus returns to is seated at that ghost's seat in that ghost's look: it
/// IS that cat, so it adopts the ghost's cell and presence and continues.
/// One body at the seat on every frame, the presence there never below what
/// the user saw, and the pane the focus left keeps its faint ghost no
/// brighter than its body was (measured 32 → 255 before e5d71c594).
///
/// Before this fix (`PetSurfaceSeam`'s `Restart` mutant), measured here: the
/// split's arriver restarted at the ramp's floor (15) beside its own ghost
/// (238) — two bodies at the seat, the coverage there dipping to 183 before
/// it recovered — and the same glass cut the ghost at the boundary, the seat
/// falling from 238 to 15 in one frame and the whole cat to 30/255.
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

/// EVERY CROSSFADE GHOST LEAVES GLASS ON ITS ARRIVER'S FULL FRAME (the live
/// verifier's residual A). A crossfade ghost's envelope runs on the ramp of
/// the arriver it hands over to, from the presence it had on glass, so it
/// reaches zero on exactly the frame the arriver reaches full presence: no
/// ghost leaves while its arriver is still coming up, and on every frame the
/// arriver and the ghosts sum to about one cat (255) — never a dip, never
/// visibly more (the arriver's 6 % floor on the first frame is the only
/// excess).
///
/// Cases: a split-pane focus switch, a full-screen program's screen, and a
/// return to the pane the focus left after its caret moved a row — its ghost
/// then stands apart from the arriver's seat and keeps fading there beside
/// it — two frames and eight frames into the first crossfade. (A return to
/// the same seat is `a_quick_return_continues_the_one_cat_it_comes_back_to`.)
///
/// Before this fix (`PetSurfaceSeam`'s `OwnClock` mutant), measured here: a
/// ghost carried across the second boundary kept its OLD clock and left glass
/// owing 10/255 (back two frames in) and 59/255 (eight frames in) while its
/// arriver was still coming up, the cat summing to 239 and 194/255. The single
/// switches already left on the arriver's full frame (owing 0, summing
/// 254..=270), and still do.
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

/// AN ADOPTED BODY GOES ON IN THE LOOK THE USER SAW (2026-10-08). A quick
/// return adopts the ghost standing at the arriver's seat
/// (`a_quick_return_continues_the_one_cat_it_comes_back_to`), and that body
/// is the one the user was looking at: it goes on in the ghost's look — its
/// pose and facing — while it rests at that seat, through the homecoming
/// ramp and, for a still pose, on until the brain's own look steps. Every
/// handover rides a step the brain takes anyway: its look stepping at rest,
/// a pose caught mid-stride folding at the ramp's end, or the brain setting
/// the body in motion (a walk, the gather of a leap). The held look never
/// travels and never rides a flight. Cases: a cat woken by a key and caught
/// mid-stretch (the brain walks on to the caret at once), one standing by
/// its prompt after a few keys, one sitting face-on after them, one asleep,
/// one caught mid-stride, and the standing one with Enter pressed two frames
/// after the return (the brain leaps to the next row).
///
/// Before this fix, measured here: the adopted arriver wore the fresh
/// brain's look — the retire reset it to a sit or a sleep — so the
/// stretching cat became a walk frame at the same cell on the adoption
/// frame (`PetStretch` to `PetLwalk0` at (3, 10)). And before the seat gate:
/// the held standing look glided 3.47 cells through the leap, backwards, at
/// no lift, then became the landing frame in mid-air; a stretch slid 0.62
/// cells under a walk; and a walk frame stood frozen for 1.28 s on a
/// motionless cat.
#[test]
fn an_adopted_body_goes_on_in_the_look_the_user_saw() {
    let model = pet_surface_seam_model();
    for (case, keys, wait, input) in [
        // Past the stretch's lead, so the live body is drawn at the unit
        // scale a still ghost is drawn at (`ghost_moved` reads its pixels).
        ("mid-stretch", 1usize, 12usize, None),
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

/// EVERY GHOST FADES IN ITS OWN PANE, HOWEVER MANY PANES THE FOCUS CROSSES
/// (2026-10-08). A quick cycle across three panes — M to L to R inside the
/// first crossfade — leaves two ghosts of the cat on two own glasses: M's,
/// still fading where the focus first left it, and L's. Each is judged by
/// its own pane, so each keeps fading where it was drawn and reaches zero on
/// the frame the arriver reaches full; no pane loses its cat in one frame
/// and the cat never dips. When the focus comes back to one of those panes —
/// M or L — the ghost there is the cat the arriver adopts.
///
/// Before this fix, measured here: the owner tracked ONE own glass, so M's
/// ghost was dropped at the second switch. Switching 1, 2, 4 and 8 frames
/// apart, that pane fell from 255, 238, 206 and 140/255 to nothing in one
/// frame, the cat summed down to 30, 30, 60 and 122/255, and coming back to
/// M restarted the cat at the ramp's floor.
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

/// AN INCOHERENT FRAME HOLDS, A SUB-ROW SCROLL IS NOT INCOHERENT. A stale
/// snapshot (the cells extracted before a scroll the facts saw) certifies
/// nothing, so the pet is held exactly where it was drawn, at full opacity,
/// for the visibility hold's blind grace — then faded, never stranded. A
/// sub-row scroll offset at the live bottom (a glide's tail, the overscroll
/// bounce) is observed coherently: the renderer translates the grid band and
/// the pet's sprite together, so nothing about the pet changes at all.
///
/// CONTROLS: the stale snapshot really is refused (no cells observed, the
/// reason says so), and the model's Buggy blind frame — the shipped cut — is
/// refused.
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

/// A TALL SCREEN CROSSFADES THE BODY IT REPLACED (2026-10-08). The world a
/// surface is judged in is a bounded window of rows around its caret
/// (`pet_world::MAX_WORLD_ROWS`): on a 90-row terminal, a full-screen program
/// whose caret is at the bottom cannot see the rows where the shell's body
/// stood, nor the shell the program's. Unknown is not evidence: the ghost
/// keeps the verdict its own surface gave it at the boundary, beneath
/// whatever ink is there, and fades where it was drawn while the arriver
/// comes up — entering the program and quitting it.
///
/// THE ONE GUARANTEE THAT FORBIDS THE GHOST (`PetSurfaceSeam`'s `Refuse`):
/// the program's screen is protected under the old body — here double-width
/// rows, which the world never admits a body over: two rows the world sees
/// whole, or on the tall screen one row it sees under the ghost's feet while
/// its head is outside the window (a protected cell the world sees refuses
/// what it cannot see the rest of). No ghost is drawn there, and its presence
/// is not lost: the arriver carries it, so the cat stands at its new station
/// at the presence the user saw.
///
/// Before this fix, measured here: the tall screen culled the ghost as if
/// protected, and the first frame had the arriver alone at its ramp's floor
/// (15/255) on entering and on quitting; both protected screens did the same
/// (`Cull`).
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

/// A REFUSED GHOST IS HANDED OVER ONCE (2026-10-08). A full-screen program
/// whose expanded rows lie under the old body refuses that body's ghost, and
/// the arriver carries its presence — for good: the ghost leaves the lane on
/// the frame it is refused. Then, a few frames into that crossfade, the
/// program quits (the old screen comes back), the focus moves to the other
/// pane of a split, or the program rewrites the rows single width (the
/// protection lifts). Through all of it the cat sums to one cat, never a
/// dip and never a second cat, and the arriver never falls in one frame.
///
/// Before this fix, measured here: the refusal was judged afresh every frame
/// with the ghost kept in the lane, so the next boundary carried it beside
/// the boosted arriver — 510/255 on the first frame after a quit one frame
/// in, 492 two frames in — and a lifted protection re-admitted it while the
/// arriver fell 206/255 in one frame.
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

/// THE BREED HANDOFF'S LEAVER CROSSES A BOUNDARY WHERE IT WAS DRAWN
/// (2026-10-08). A new cat takes over (`PetArrival::Ceremony`: the old coat
/// runs off toward its edge while the new one fades up; `Quiet`: the old coat
/// fades in place), and mid-handoff the surface is replaced — a full-screen
/// program starts, or the focus moves to the other pane of a split. The
/// leaver is the same pet's body on glass: it is carried with the crossfade
/// ghosts, stopped where the user saw it, and fades there on the arriver's
/// ramp (in its own pane for the split) — no frame where it falls by more
/// than one step of that fade, and it leaves glass on the arriver's full
/// frame (`GhostLeavesAtFullPresence`).
///
/// Before this fix, measured here: the leaver vanished at the boundary, from
/// 255 to 0 in one frame (ceremony, alternate screen and split), and from
/// 229 at a homecoming.
///
/// WHAT IS CARRIED IS WHAT WAS DRAWN. A selection over the leaver, clear of
/// the body, culls the leaver on every frame before the seam: nothing of it
/// is on glass, so nothing of it crosses. After the boundary the old coat is
/// never brighter than on the last frame before it, and on the pane the
/// focus left nothing unseen is handed to the arriver. Measured before this
/// fix: the carry read the brain's record instead of the frame, and a leaver
/// the user never saw popped in at 255 after an alternate-screen enter (the
/// cat 126 → 396/255 in one frame).
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
            // Nor is it handed to the arriver: the cat is one cat, the old
            // body's ghost and its arriver (the crossfade's rounding allowed,
            // as `a_refused_ghost_is_handed_over_once` allows it).
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

/// A GHOST ITS OWN GLASS REFUSES HANDS ITS PRESENCE TO THE ARRIVER
/// (2026-10-08). A split-pane switch on the frame after an incoherent
/// observation of the pane the focus leaves (a stale snapshot: the body is
/// held lit there, and nothing under it is certified): that pane cannot admit
/// the old body as a ghost (`PetSurfaceSeam`'s `Refuse` after a `Shift`), so
/// none is drawn there — and the arriver comes up from the presence the user
/// saw instead of from nothing.
///
/// Before this fix, measured here: the refused ghost was dropped with its
/// presence, and the first frame after the switch had no cat at all (the
/// arriver at 0/255).
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

/// A CARRIED GHOST KEEPS ITS OWN TILE (2026-10-08). The emitter bakes at most
/// two new tiles a frame and re-emits a ghost's held tile when its bake is
/// deferred; the hold is the ghost's (`PetDepartureSprite::id`), wherever the
/// lane puts it. A breed homecoming's leaver carried to a tab of another
/// background is the third new tile of the frame after the switch (the
/// arriver and the body the user saw take the budget): it is drawn in the
/// tile it wore a frame ago, and every ghost on the lane is drawn on every
/// frame.
///
/// Before this fix, measured here: the hold was keyed on the ghost's birth
/// stamp, which a carried ghost re-bases (and every crossfade ghost shared,
/// 0.0), so the deferred leaver had no hold and was not drawn on that frame.
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
