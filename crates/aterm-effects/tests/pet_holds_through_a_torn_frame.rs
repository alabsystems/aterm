// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet holds through a torn frame.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{
    CaptureMode, FrameGeom, HostFrameInput, SingFacts, TerminalFacts, Visibility,
};
use aterm_effects::kitty_pet::{PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;
use aterm_models_effects::console_cursor_home_model;
use aterm_spec::derive::Model;

use crate::common::FrameHold;

const ROWS: u16 = 40;
const COLS: u16 = 120;
const CW: u16 = 7;
const CH: u16 = 14;
const ROW: u16 = 37;
const FRAME_MS: u64 = 16;
const OWNER_MS: u64 = 280;
const PAST_TIMEOUT_MS: u64 = 1_300;

const FIXTURES: [(&str, &str); 4] = [
    (
        "insert",
        include_str!("fixtures/claude-composer-2026-09-21.ptylog"),
    ),
    (
        "end",
        include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
    ),
    (
        "scrub3",
        include_str!("fixtures/claude-composer-2026-09-21-scrub3.ptylog"),
    ),
    (
        "popup",
        include_str!("fixtures/claude-composer-2026-09-21-popup.ptylog"),
    ),
];

const SYNC_ON: &[u8] = b"\x1b[?2026h";
const SYNC_OFF: &[u8] = b"\x1b[?2026l";
const SHOW: &[u8] = b"\x1b[?25h";

/// How the host hands the pet a hidden cursor on an unfinished frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Contract {
    /// `TerminalFacts::read` as it ships, `frame_unfinished` included.
    Shipping,
    /// `frame_unfinished` cleared: the write position handed over as the caret.
    Raw,
}

enum Rec {
    Out(Vec<u8>),
    In(Vec<u8>),
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn recording(src: &str) -> Vec<(u64, Rec)> {
    let mut t0 = None;
    src.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let ns: u64 = it.next().expect("stamp").parse().expect("stamp");
            let kind = it.next().expect("kind");
            let bytes = unhex(it.next().expect("bytes"));
            let ms = (ns - *t0.get_or_insert(ns)) / 1_000_000;
            match kind {
                "O" => (ms, Rec::Out(bytes)),
                "I" => (ms, Rec::In(bytes)),
                k => panic!("bad kind {k}"),
            }
        })
        .collect()
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn torn_pieces(bytes: &[u8], open_ms: u64) -> Vec<(u64, Vec<u8>)> {
    if open_ms == 0 {
        return vec![(0, bytes.to_vec())];
    }
    let mut out = Vec::new();
    let (mut at, mut i) = (0, 0);
    while i < bytes.len() {
        let Some((open, close)) =
            find(bytes, SYNC_ON, i).and_then(|open| Some((open, find(bytes, SYNC_OFF, open)?)))
        else {
            out.push((at, bytes[i..].to_vec()));
            break;
        };
        if open > i {
            out.push((at, bytes[i..open].to_vec()));
        }
        let end = find(bytes, SHOW, open)
            .filter(|&s| s < close)
            .unwrap_or(close);
        let mut cuts = vec![open];
        let mut j = open + SYNC_ON.len();
        while let Some(cr) = find(bytes, b"\r", j).filter(|&cr| cr + 1 < end) {
            cuts.push(cr + 1);
            j = cr + 1;
        }
        cuts.push(end);
        let n = cuts.len() as u64 - 1;
        for (k, w) in cuts.windows(2).enumerate() {
            out.push((at + open_ms * k as u64 / n, bytes[w[0]..w[1]].to_vec()));
        }
        at += open_ms;
        out.push((at, bytes[end..close + SYNC_OFF.len()].to_vec()));
        i = close + SYNC_OFF.len();
    }
    out
}

fn full_repaint(src: &str) -> Vec<u8> {
    let mut all = Vec::new();
    for (_, ev) in recording(src) {
        if let Rec::Out(b) = ev {
            all.extend_from_slice(&b);
        }
    }
    let open = find(&all, SYNC_ON, 0).expect("a bracket");
    let close = find(&all, SYNC_OFF, open).expect("its close");
    let mut paint = SYNC_ON.to_vec();
    paint.extend_from_slice(b"\x1b[?25l");
    paint.extend_from_slice(&all[open + SYNC_ON.len()..close + SYNC_OFF.len()]);
    paint
}

/// One present, as the pet drew it.
#[derive(Clone, Copy, Debug)]
struct Shot {
    ms: u64,
    /// A hidden cursor on a frame the program has not finished.
    torn: bool,
    frame: PetFrame,
    needs_frames: bool,
    /// The terminal's own cursor (on a torn present, the write position).
    cursor: (u16, u16),
    /// The caret the last finished present painted.
    painted: Option<(u16, u16)>,
}

struct Host {
    term: Terminal,
    owner: CompanionOwner,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    contract: Contract,
    t0: Instant,
    now: Instant,
    hold: FrameHold,
    painted: Option<(u16, u16)>,
    shots: Vec<Shot>,
    /// The last present's cell snapshot.
    input: Option<(u64, RenderInput)>,
}

impl Host {
    fn new(contract: Contract) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, 0x5EED);
        let now = Instant::now();
        Self {
            term,
            owner,
            decos: WordDecorations::default(),
            lexicon: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            contract,
            t0: now,
            now,
            hold: FrameHold::default(),
            painted: None,
            shots: Vec::new(),
            input: None,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    fn feed(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.now,
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    fn present(&mut self) {
        if self.hold.withholds(&self.term, self.now) {
            return;
        }
        let mutations = self.term.screen_mutations();
        if self.input.as_ref().is_none_or(|(at, _)| *at != mutations) {
            let fresh = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
            self.input = Some((mutations, fresh));
        }
        let (_, input) = self.input.as_ref().expect("snapshot taken above");
        let world = PetWorldFacts::read(&self.term, 7);
        self.owner
            .observe_console(input, &world, PetPane::full(input), &[]);
        let mut facts = TerminalFacts::read(&self.term, 7, false);
        let torn = self.term.sync_frame_unfinished() && !facts.cursor_visible;
        if self.contract == Contract::Raw {
            facts.frame_unfinished = false;
        }
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
                rows: ROWS,
                cols: COLS,
                cell_w: CW,
                cell_h: CH,
                origin_px: (0, 0),
            },
        };
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
                pane: None,
                room: None,
            },
            &mut self.decos,
        );
        let frame = self.owner.tick(tick).pet;
        self.decos.rescan_from_cells(
            &input.cells,
            &input.line_sizes,
            usize::from(ROWS),
            usize::from(COLS),
            &self.lexicon,
            &self.cfg,
            input.content_seq,
            self.now,
        );
        let c = self.term.cursor();
        if !torn && self.term.cursor_visible() {
            self.painted = Some((c.row, c.col));
        }
        self.shots.push(Shot {
            ms: self.ms(),
            torn,
            frame,
            needs_frames: self.owner.needs_frames(),
            cursor: (c.row, c.col),
            painted: self.painted,
        });
    }

    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.now += Duration::from_millis(FRAME_MS);
            self.present();
        }
        self.now = self.now.max(t);
    }

    fn deliver(&mut self, bytes: &[u8], open_ms: u64) -> u64 {
        let start = self.ms();
        let mut span = 0;
        for (off, piece) in torn_pieces(bytes, open_ms) {
            self.run_to(start + off);
            self.feed(&piece);
            span = off;
        }
        span
    }

    fn start(src: &str, contract: Contract, settle_ms: u64) -> Self {
        let mut h = Self::new(contract);
        let closes = h.term.sync_end_seq();
        for (ms, ev) in recording(src) {
            if let Rec::Out(b) = ev {
                h.run_to(ms);
                h.feed(&b);
                if h.term.sync_end_seq() != closes {
                    break;
                }
            }
        }
        let end = h.ms() + settle_ms;
        h.run_to(end);
        h
    }

    fn replay(src: &str, open_ms: u64, contract: Contract) -> Self {
        let mut h = Self::new(contract);
        let mut shift = 0;
        for (ms, ev) in recording(src) {
            match ev {
                Rec::In(b) if b.as_slice() == b"\x1b[99;5u" => break,
                Rec::In(b) => {
                    h.run_to(ms + shift);
                    let kind = match b.as_slice() {
                        [c] if (0x20..0x7f).contains(c) => Some(PetInputKind::Text),
                        [0x7f] => Some(PetInputKind::Delete),
                        b"\x1b[1;3D" | b"\x1b[1;3C" | b"\x1b[F" | b"\x1b[H" => {
                            Some(PetInputKind::Navigate)
                        }
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        h.owner.note_console_input(h.now, kind);
                    }
                }
                Rec::Out(b) => {
                    h.run_to(ms + shift);
                    shift += h.deliver(&b, open_ms);
                }
            }
        }
        let end = h.ms() + 1_000;
        h.run_to(end);
        h
    }
}

fn reacting(action: PetAction) -> bool {
    matches!(
        action,
        PetAction::Perk | PetAction::Crouch | PetAction::Leap | PetAction::Startle
    )
}

fn disturbed(settled: &Shot, shot: &Shot) -> bool {
    (shot.frame.col - settled.frame.col).abs() > 0.01
        || (shot.frame.row - settled.frame.row).abs() > 0.01
        || shot.frame.lift != settled.frame.lift
        || shot.frame.alpha != settled.frame.alpha
        || reacting(shot.frame.action)
}

fn off_home(shot: &Shot) -> Option<(f32, f32)> {
    let (row, col) = shot.painted?;
    let (x0, x1, _, y1) = shot.frame.body_px(CW, CH, COLS, ROWS)?;
    let (left, right) = (x0 as f32 / f32::from(CW), x1 as f32 / f32::from(CW));
    let col = f32::from(col);
    let dx = (left - col).max(col - right).max(0.0);
    let feet = y1 as f32 / f32::from(CH) - 1.0;
    Some((dx, (feet - f32::from(row)).abs()))
}

type State = BTreeMap<&'static str, i64>;

fn project(shot: &Shot, seat: &Shot, event: i64) -> State {
    let home = (shot.frame.col - seat.frame.col).abs() <= 0.5
        && (shot.frame.row - seat.frame.row).abs() <= 0.5;
    State::from([
        ("visible", i64::from(shot.frame.alpha > 0)),
        ("home", i64::from(home || shot.needs_frames)),
        ("moving", i64::from(shot.needs_frames)),
        ("hidden", i64::from(shot.torn)),
        ("protected", 0),
        ("event", event),
    ])
}

fn conform(model: &Model, seat: &Shot, shots: &[Shot]) -> Result<usize, String> {
    let mut state = model.init_state();
    let mut stepped = 0;
    for shot in shots {
        let (action, event) = match (state["hidden"] == 1, shot.torn) {
            (false, true) => ("HideCursor", 7),
            (true, true) => ("RepaintMove", 9),
            (true, false) => ("ShowCursor", 8),
            (false, false) => continue,
        };
        let post = project(shot, seat, event);
        let lawful = model.successors(action, &state).contains(&post)
            && model
                .invariants
                .iter()
                .all(|inv| model.check_invariant(inv.name, &post));
        if !lawful {
            return Err(format!(
                "@{} {action}: {state:?} -> {post:?}; cursor={:?} pet=({:.1},{:.1}) {:?}",
                shot.ms, shot.cursor, shot.frame.col, shot.frame.row, shot.frame.action
            ));
        }
        state = post;
        stepped += 1;
    }
    Ok(stepped)
}

#[test]
fn a_torn_full_repaint_neither_moves_nor_fades_the_settled_pet() {
    let model = console_cursor_home_model();
    for (name, src) in FIXTURES {
        let paint = full_repaint(src);
        for (open_ms, contract) in [
            (OWNER_MS, Contract::Shipping),
            (PAST_TIMEOUT_MS, Contract::Shipping),
            (OWNER_MS, Contract::Raw),
            (PAST_TIMEOUT_MS, Contract::Raw),
        ] {
            let label = format!("{name} {contract:?} {open_ms} ms");
            let mut h = Host::start(src, contract, 3_000);
            let settled = *h.shots.last().expect("presents");
            assert!(
                settled.frame.alpha == 255 && !settled.needs_frames,
                "{label}: the pet settled first: {settled:?}"
            );
            assert_eq!(
                settled.painted,
                Some((ROW, 2)),
                "{label}: the empty composer"
            );
            let from = h.shots.len();
            h.deliver(&paint, open_ms);
            let end = h.ms() + 1_000;
            h.run_to(end);
            let after = &h.shots[from..];
            let torn = after.iter().filter(|s| s.torn).count();
            assert!(
                torn as u64 >= (open_ms - 150) / (2 * FRAME_MS),
                "{label}: the repaint must present torn ({torn})"
            );
            let broken = after.iter().find(|s| disturbed(&settled, s));
            let conformed = conform(&model, &settled, after);
            if contract == Contract::Raw && open_ms == PAST_TIMEOUT_MS {
                assert!(
                    broken.is_some() && conformed.is_err(),
                    "{label}: negative control: the write cursor must disturb the pet \
                     (broken {broken:?}, model {conformed:?})"
                );
                continue;
            }
            assert!(
                broken.is_none(),
                "{label}: the pet moved, faded or reacted: {broken:?}"
            );
            let stepped = conformed.unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_eq!(
                stepped,
                torn + 1,
                "{label}: every torn present and the show"
            );
        }
    }
}

#[test]
fn every_bracket_torn_keeps_the_pet_at_the_composer_while_typing() {
    let takes = [
        (0, OWNER_MS, Contract::Shipping),
        (1, OWNER_MS, Contract::Shipping),
        (2, OWNER_MS, Contract::Shipping),
        (3, OWNER_MS, Contract::Shipping),
        (3, PAST_TIMEOUT_MS, Contract::Shipping),
        (3, PAST_TIMEOUT_MS, Contract::Raw),
    ];
    crate::common::par_map(&takes, |&(fixture, open_ms, contract)| {
        every_bracket_torn_keeps_the_pet_at_the_composer(fixture, open_ms, contract);
    });
}

fn every_bracket_torn_keeps_the_pet_at_the_composer(
    fixture: usize,
    open_ms: u64,
    contract: Contract,
) {
    let (name, src) = FIXTURES[fixture];
    let label = format!("{name} {contract:?} {open_ms} ms");
    let h = Host::replay(src, open_ms, contract);
    let first = h
        .shots
        .iter()
        .find(|s| s.painted.is_some_and(|(row, _)| row == ROW))
        .expect("a painted composer caret")
        .ms;
    let shots: Vec<&Shot> = h.shots.iter().filter(|s| s.ms >= first + 2_000).collect();
    let mut torn = 0;
    let mut far = Vec::new();
    let mut dimmed = Vec::new();
    let mut reactions = Vec::new();
    for w in shots.windows(2) {
        let (prev, s) = (w[0], w[1]);
        if !s.torn {
            continue;
        }
        torn += 1;
        if let Some((_, dy)) = off_home(s)
            && dy > 2.0
        {
            far.push((s.ms, dy, s.cursor));
        }
        if s.frame.alpha < prev.frame.alpha {
            dimmed.push((s.ms, prev.frame.alpha, s.frame.alpha));
        }
        if reacting(s.frame.action) && s.frame.action != prev.frame.action {
            reactions.push((s.ms, prev.frame.action, s.frame.action, s.cursor));
        }
    }
    assert!(torn > 200, "{label}: the take must present torn ({torn})");
    if contract == Contract::Raw {
        assert!(
            !far.is_empty() && !reactions.is_empty(),
            "{label}: negative control: the write cursor must be chased \
                 ({} of {torn} torn presents far, {} reactions)",
            far.len(),
            reactions.len()
        );
        return;
    }
    assert!(
        far.is_empty(),
        "{label}: left the composer: {:?}",
        &far[..far.len().min(4)]
    );
    assert!(
        dimmed.is_empty(),
        "{label}: dimmed: {:?}",
        &dimmed[..dimmed.len().min(4)]
    );
    assert!(
        reactions.is_empty(),
        "{label}: reacted to a torn frame: {:?}",
        &reactions[..reactions.len().min(4)]
    );
}

#[test]
fn a_finished_frames_hidden_caret_is_held_for_a_repaint_and_then_followed() {
    let mut h = Host::start(FIXTURES[0].1, Contract::Shipping, 3_000);
    let settled = *h.shots.last().expect("presents");
    assert!(!settled.needs_frames && settled.frame.alpha == 255);
    let from = h.shots.len();
    h.feed(b"\x1b[?25l\x1b[3;10H");
    let t = h.ms() + 200;
    h.run_to(t);

    h.feed(b"\x1b[38;3H\x1b[?25h");
    let t = h.ms() + 1_000;
    h.run_to(t);
    let moved = h.shots[from..].iter().find(|s| disturbed(&settled, s));
    assert!(
        moved.is_none(),
        "a repaint's hidden caret moved the pet: {moved:?}"
    );

    h.feed(b"\x1b[?25l\x1b[3;10H");
    let t = h.ms() + 2_500;
    h.run_to(t);
    let followed = h.shots.last().expect("presents").frame;
    assert!(
        followed.alpha > 0 && (followed.row - 2.0).abs() <= 3.0,
        "control: a caret hidden past the bound is followed: {followed:?}"
    );
}
