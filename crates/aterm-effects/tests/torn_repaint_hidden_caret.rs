// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TORN FULL-SCREEN REPAINT MUST NOT SPEND THE HAND'S KEYS (2026-09-28).
//!
//! The owner, on the installed v0.98.0, typing into Claude Code 2.1.284's
//! FULLSCREEN composer (alt screen, caret drawn) while an Opus turn streamed
//! (`Moonwalking… · thinking with xhigh effort`, machine load ~75): the line
//! `❯ how is it that there was a bug in ay? did you fix▮` carried the band
//! under `how is it that there was a`, then ONE DARK CELL — the space — and
//! then a SECOND band, its hue restarted, from `bug` on.
//!
//! The owner's session cast (tab 2, `aterm ctl @s-8177… cast`, events
//! 13199–13206) holds the bytes, and the mechanism is in them:
//!
//! ```text
//! 21244.848  SYNC_ON HIDE CUP 41;28 SHOW SYNC_OFF        the space before `a`
//! 21244.880  SYNC_ON HIDE … `a` … CUP 41;29 SHOW SYNC_OFF caret after `a`
//! 21244.979  SYNC_ON HIDE HOME <transcript repaint…      ONE bracket, open
//! 21245.048  …transcript…                                 ~280 ms, the bytes
//! 21245.181  …transcript…                                 in four PTY reads
//! 21245.258  …composer rows… CUP 41;29 SHOW SYNC_OFF      closes on `a`'s caret
//!            SYNC_ON HIDE spinner CUP 41;30 SHOW SYNC_OFF the space's echo
//!            SYNC_ON HIDE spinner … `bu` CUP 41;32 …      `b` `u`, one frame
//! ```
//!
//! The host's frame hold releases a bracket open past `SYNC_HOLD_CAP`
//! (150 ms) and presents the terminal as it stands: the caret HIDDEN and
//! parked wherever Ink's partial write stopped — rows above the composer,
//! in the transcript. The single-pane present hands a hidden caret to the
//! glow engine as `cur` (`app_render.rs`, "A HIDDEN CURSOR IS STILL A
//! CARET", b473a0197 — for the pet), and only the minibuffer gate reads
//! `observe_caret_drawn`. So the seam judges Ink's WRITE CURSOR as the
//! caret: `(40,28) -> (18,57)` and `-> (29,0)` are licensed by the typed
//! keys in flight, cross-row, and `-> (40,31)` returns — the space the keys
//! paid for is laid by nobody and the band restarts. Replayed from those
//! bytes into a private headless aterm the ring reads exactly
//! `licensed key (40,28)->(18,57)`, `(18,57)->(29,0)`, `(29,0)->(40,32)`.
//!
//! Every other harness in this directory hands a hidden caret over as
//! `None` (`visible.then_some(..)`), which is the composed/headless paths'
//! contract and not the single-pane present's — so `phrase_pause_gaps.rs`'s
//! mid-turn Claude Code takes, which DO sample open brackets past the cap,
//! could not see this. This harness takes the host's contract as a dial.
//!
//! THE FIX: the single-pane present reads the motion engines' caret through
//! `aterm_effects::host::motion_caret`, which withholds a hidden cursor on a
//! frame the program has not finished (`Terminal::sync_frame_unfinished`:
//! its bracket still open by its own account, and written into), and the
//! pet keeps the unfiltered caret. [`Contract::SinglePane`] is that shipping
//! contract, and [`Contract::HandedOver`] is the one before it. Under the old
//! contract the take still reproduces the gap, so the fixed take cannot pass
//! vacuously.
//!
//! REVIEW ROUND 1: the first fix fed `motion_caret` the MODE level
//! (`synchronized_output()`), which the terminal force-clears when a bracket
//! outlives `sync_timeout_ms` (1 s) — with the program still mid-frame and
//! its hidden write cursor still parked in the transcript. A bracket held
//! past 1 s with keys typed late in it gapped again under that reading
//! ([`Contract::ModeLevel`], kept as the negative control of
//! [`a_torn_repaint_outliving_the_terminal_timeout_keeps_the_line_whole`]).

use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::cursor_glow::{AdmissionPhase, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::host::motion_caret;
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_effects::rainbow_kitty::witness::WITNESS_ROWS;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

const ROWS: usize = 45;
const COLS: usize = 130;
const CW: usize = 8;
const CH: usize = 16;
/// Claude Code 2.1.284's fullscreen composer row at 130x45 (CUP 41).
const ROW: u16 = 40;
/// The text starts two cells in, after `❯ `.
const COL0: u16 = 2;
const FRAME_MS: u64 = 16;
/// `app_render.rs`'s `SYNC_HOLD_CAP`.
const SYNC_HOLD_CAP: Duration = Duration::from_millis(150);

const SYNC_ON: &str = "\x1b[?2026h";
const SYNC_OFF: &str = "\x1b[?2026l";
const HIDE: &str = "\x1b[?25l";
const SHOW: &str = "\x1b[?25h";

fn geom() -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    }
}

fn cfg() -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style: GlowStyle::RainbowKitty,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 1.0,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

/// `app_render.rs`'s `sync_frame_hold`, its logic verbatim.
fn sync_frame_hold(
    active: bool,
    open_dirty: bool,
    end_seq: u64,
    was_active: bool,
    armed: (Option<Instant>, u64),
    now: Instant,
) -> (Option<Instant>, bool, u64) {
    if !active {
        return (None, false, end_seq);
    }
    if was_active && end_seq != armed.1 {
        return (Some(now + SYNC_HOLD_CAP), open_dirty, end_seq);
    }
    let mut deadline = if was_active {
        armed.0
    } else {
        Some(now + SYNC_HOLD_CAP)
    };
    let hold = deadline.is_some_and(|d| now < d);
    if !hold {
        deadline = None;
    }
    (deadline, hold, end_seq)
}

/// How the host hands the caret to the glow tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Contract {
    /// The composed, focus and headless paths and `EffectsPipeline`: a
    /// hidden caret is `None`.
    Withheld,
    /// The single-pane present from b473a0197 until this fix: a hidden
    /// caret is always handed over, `observe_caret_drawn(false)` says so.
    HandedOver,
    /// The shipping single-pane present: the hidden caret handed over
    /// through `motion_caret` with the program's account of its frame, so
    /// an unfinished frame withholds it.
    SinglePane,
    /// The first fix (review round 1's finding): `motion_caret` fed the
    /// mode level, which the terminal's timeout force-clears mid-frame.
    ModeLevel,
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink_seen: u64,
    /// The host's caret contract.
    contract: Contract,
    sync_was_active: bool,
    sync_hold_until: Option<Instant>,
    sync_armed_seq: u64,
    torn_presents: usize,
}

impl Host {
    fn new(contract: Contract) -> Self {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, COLS);
        term.process(
            format!(
                "\x1b[?1049h\x1b[2J\x1b[{};1H❯\u{a0}\x1b[{};{}H",
                ROW + 1,
                ROW + 1,
                COL0 + 1
            )
            .as_bytes(),
        );
        let sync_armed_seq = term.sync_end_seq();
        let mut h = Self {
            term,
            glow,
            cfg: cfg(),
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink_seen: 0,
            contract,
            sync_was_active: false,
            sync_hold_until: None,
            sync_armed_seq,
            torn_presents: 0,
        };
        h.frame();
        h
    }

    fn process(&mut self, bytes: &str) {
        self.term.process_at(
            bytes.as_bytes(),
            ClockReading {
                monotonic: self.now,
                wall_ms: None,
            },
        );
    }

    /// The frame hold and the tick, as the single-pane present runs them.
    fn frame(&mut self) {
        let c = self.term.cursor();
        let visible = self.term.cursor_visible();
        let caret = Some((c.row, c.col));
        let cur = match self.contract {
            Contract::Withheld => caret.filter(|_| visible),
            Contract::HandedOver => caret,
            Contract::SinglePane => motion_caret(caret, visible, self.term.sync_frame_unfinished()),
            Contract::ModeLevel => {
                motion_caret(caret, visible, self.term.modes().synchronized_output())
            }
        };
        let epoch = self.term.repaint_blink_epoch();
        if epoch != self.blink_seen {
            self.blink_seen = epoch;
            self.glow.note_repaint_blink(self.now);
        }
        self.glow.note_context(self.term.is_alternate_screen());
        self.term
            .row_cols_into(usize::from(c.row), &mut self.row_buf);
        self.glow.observe_row(c.row, c.col, &self.row_buf, self.now);
        self.glow
            .observe_print_anchor_glyph(self.term.print_anchor(), self.term.print_anchor_glyph());
        if self.contract != Contract::Withheld {
            self.glow.observe_caret_drawn(visible);
        }
        self.glow.observe_ribbon_row(c.row, &self.row_buf);
        let mut rows = [0u16; WITNESS_ROWS];
        let n = self.glow.ribbon_rows(&mut rows);
        for &r in &rows[..n] {
            self.term.row_cols_into(usize::from(r), &mut self.row_buf);
            self.glow.observe_ribbon_row(r, &self.row_buf);
        }
        self.glow
            .tick(cur, self.now, &self.cfg, geom(), &mut self.out);
    }

    /// The host's present rule (SYNC-1): a held frame runs nothing; a
    /// bracket open past the cap is presented as it stands.
    fn present(&mut self) {
        let active = self.term.modes().synchronized_output();
        let (deadline, hold, seq) = sync_frame_hold(
            active,
            self.term.sync_open_dirty(),
            self.term.sync_end_seq(),
            self.sync_was_active,
            (self.sync_hold_until, self.sync_armed_seq),
            self.now,
        );
        self.sync_was_active = active;
        self.sync_hold_until = deadline;
        self.sync_armed_seq = seq;
        if hold {
            return;
        }
        // Torn: the program's frame is unfinished on glass — the hold ran
        // out with the mode set, or the terminal's timeout cleared it.
        if self.term.sync_frame_unfinished() {
            self.torn_presents += 1;
        }
        self.frame();
    }

    /// Run the 16 ms frame train up to `t0 + ms`.
    fn to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.now + Duration::from_millis(FRAME_MS) <= t {
            self.now += Duration::from_millis(FRAME_MS);
            self.present();
        }
        self.now = self.now.max(t);
    }

    fn key(&mut self, ms: u64, ch: char) {
        self.to(ms);
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        self.glow.note_typed_glyph(self.now, 1, false, class);
    }

    fn write(&mut self, ms: u64, bytes: &str) {
        self.to(ms);
        self.process(bytes);
    }

    /// The census of `cols`: lit (a live owner with coverage) or dark.
    fn lit(&self, col: u16) -> bool {
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let owned = rib
            .cells()
            .iter()
            .any(|c| c.row == ROW && c.col == col && !c.leaving());
        let cov = rib.plan_segments().iter().any(|s| {
            ((s.spine / CH as f32) - 0.5).floor() as i64 == i64::from(ROW)
                && (s.x / CW as f32).floor() as i64 == i64::from(col)
                && s.cov > 0
        });
        owned && cov
    }

    fn ring(&self) -> String {
        let mut s = String::new();
        for r in self.glow.admission_log() {
            let at = r.at.saturating_duration_since(self.t0).as_millis();
            let _ = write!(
                s,
                "\n  @{at:>5} {} {} ({},{})->({},{}) {}",
                if r.phase == AdmissionPhase::Declined {
                    "declined"
                } else {
                    "licensed"
                },
                r.reason,
                r.origin.0,
                r.origin.1,
                r.target.0,
                r.target.1,
                r.licence
            );
        }
        s
    }
}

/// Claude Code's letter frame: hide, home, CR, CUF, CUD, the glyph(s), park
/// at the last row, the caret after them, show.
fn letter_frame(col: u16, glyphs: &str, caret: u16) -> String {
    format!(
        "{SYNC_ON}{HIDE}\x1b[H\r\x1b[{col}C\x1b[{ROW}B{glyphs}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
        ROW + 1,
        caret + 1
    )
}

/// Claude Code's space frame: hide, the caret one on, show.
fn space_frame(caret: u16) -> String {
    format!(
        "{SYNC_ON}{HIDE}\x1b[{};{}H{SHOW}{SYNC_OFF}",
        ROW + 1,
        caret + 1
    )
}

/// Its spinner frame three rows up, the caret returned to `caret`.
fn spinner_frame(glyph: char, caret: u16) -> String {
    format!(
        "{SYNC_ON}{HIDE}\x1b[H\r\x1b[{}B{glyph}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
        ROW - 3,
        ROW + 1,
        caret + 1
    )
}

/// One transcript row's repaint: home, CR, CUD to the row, the text.
fn transcript_row(row: u16, text: &str) -> String {
    format!("\x1b[H\r\x1b[{row}B{text}\x1b[K")
}

/// The owner's gesture: `was a` typed and echoed, then ` bu` typed while a
/// transcript repaint's bracket stays open ~280 ms — presented torn at the
/// cap with Ink's write cursor parked in the transcript — and echoed after
/// it closes, the space folded into a spinner frame and `bu` into the next.
/// Returns the host after the settle.
fn take(contract: Contract, open_ms: u64) -> Host {
    let gap = ((open_ms - 20) / 3).min(100);
    take_shaped(
        contract,
        Shape {
            open_ms,
            keys: [30, 30 + gap, 30 + 2 * gap],
            middle: 40 + open_ms / 2,
        },
    )
}

/// When the torn bracket's reads and the keys typed into it land, each in
/// ms after the bracket's `open` instant. The head read lands at +40 and
/// the close at `+40 + open_ms`.
struct Shape {
    open_ms: u64,
    /// ` `, `b`, `u`.
    keys: [u64; 3],
    /// The middle transcript read.
    middle: u64,
}

fn take_shaped(contract: Contract, shape: Shape) -> Host {
    let Shape {
        open_ms,
        keys: key_ms,
        middle: middle_ms,
    } = shape;
    let mut h = Host::new(contract);
    // `was a` at 10 cps, each echoed 20 ms after its key.
    let typed = "was a";
    let mut t = 100;
    for (i, ch) in typed.chars().enumerate() {
        let col = COL0 + i as u16;
        h.key(t, ch);
        let f = if ch == ' ' {
            space_frame(col + 1)
        } else {
            letter_frame(col, &ch.to_string(), col + 1)
        };
        h.write(t + 20, &f);
        t += 100;
    }
    let caret = COL0 + typed.len() as u16; // after `a`
    // The transcript repaint opens; its bytes arrive in three reads. The
    // first two end mid-transcript, the caret hidden where Ink stopped.
    let open = t;
    // The three keys of ` bu`, typed while the bracket is open.
    let keys = [
        (open + key_ms[0], ' '),
        (open + key_ms[1], 'b'),
        (open + key_ms[2], 'u'),
    ];
    let head = format!(
        "{SYNC_ON}{HIDE}{}{}",
        transcript_row(1, "It resumed successfully. The crash report is deleted."),
        transcript_row(18, "The cause is a live bug on the other branch's executor")
    );
    let middle = transcript_row(29, "- Row D: 48 of 155 sampled queries now prove unsat.");
    // The close: the composer's caret back after `a`, shown — then, in the
    // same read, the space's echo in a spinner frame and `bu` in the next.
    let close = format!(
        "{}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}{}{SYNC_ON}{HIDE}\x1b[H\r\x1b[{}B·\r\x1b[{}C\x1b[3B{}\x1b[{ROWS};1H\x1b[{};{}H{SHOW}{SYNC_OFF}",
        transcript_row(33, "  ⎿  Running in background"),
        ROW + 1,
        caret + 1,
        spinner_frame('✢', caret + 1),
        ROW - 3,
        caret + 1,
        "bu",
        ROW + 1,
        caret + 4
    );
    // Every event in time order: the head read, the middle read, the close.
    let mut events: Vec<(u64, Option<char>, String)> = keys
        .iter()
        .map(|&(at, ch)| (at, Some(ch), String::new()))
        .collect();
    events.push((open + 40, None, head));
    events.push((open + middle_ms, None, middle));
    events.push((open + 40 + open_ms, None, close));
    events.sort_by_key(|e| e.0);
    for (at, key, bytes) in events {
        match key {
            Some(ch) => h.key(at, ch),
            None => h.write(at, &bytes),
        }
    }
    // `g`, then the settle.
    let after = open + 40 + open_ms;
    h.key(after + 60, 'g');
    h.write(after + 80, &letter_frame(caret + 3, "g", caret + 4));
    h.to(after + 280);
    h
}

fn report(h: &Host) -> (Vec<u16>, String) {
    let line = "was a bug";
    let dark: Vec<u16> = (COL0..COL0 + line.len() as u16)
        .filter(|&c| !h.lit(c))
        .collect();
    let glyphs: Vec<String> = dark
        .iter()
        .map(|&c| {
            format!(
                "{c}:{:?}",
                line.chars().nth(usize::from(c - COL0)).unwrap_or('?')
            )
        })
        .collect();
    (
        dark,
        format!(
            "torn presents={} dark={}{}",
            h.torn_presents,
            glyphs.join(","),
            h.ring()
        ),
    )
}

/// The licensed rows that left the composer row: a move judged from, or
/// to, the torn frame's write cursor.
fn cross_row_licences(h: &Host) -> Vec<String> {
    h.glow
        .admission_log()
        .filter(|r| r.phase != AdmissionPhase::Declined && (r.origin.0 != ROW || r.target.0 != ROW))
        .map(|r| {
            format!(
                "{} ({},{})->({},{})",
                r.reason, r.origin.0, r.origin.1, r.target.0, r.target.1
            )
        })
        .collect()
}

/// CONTROL: the same bytes and keys with the bracket closing inside the
/// cap — no torn present — light every cell under both single-pane
/// contracts, so a hole in the takes below is the torn present's and not
/// the gesture's.
#[test]
fn a_repaint_closing_inside_the_cap_keeps_the_boundary_space_lit() {
    for contract in [Contract::HandedOver, Contract::SinglePane] {
        let h = take(contract, 120);
        let (dark, rep) = report(&h);
        assert_eq!(
            h.torn_presents, 0,
            "{contract:?}: control must present no torn frame: {rep}"
        );
        assert!(dark.is_empty(), "{contract:?} control: {rep}");
    }
}

/// CONTROL: a host that hands a hidden caret over as `None` (every other
/// harness here, the composed and headless paths) keeps the line whole
/// through the same torn present.
#[test]
fn a_torn_repaint_with_the_hidden_caret_withheld_keeps_the_boundary_space_lit() {
    let h = take(Contract::Withheld, 280);
    let (dark, rep) = report(&h);
    assert!(h.torn_presents > 0, "the take must present torn: {rep}");
    assert!(dark.is_empty(), "withheld-caret control: {rep}");
}

/// NEGATIVE CONTROL, the owner's gap: under the contract the single-pane
/// present shipped with until this fix, the torn present's hidden write
/// cursor is judged as the caret. The keys in flight license the
/// cross-row moves and the boundary cells go dark. If this stops
/// reproducing, the fixed take below proves nothing.
#[test]
fn the_pre_fix_contract_spends_the_keys_on_the_write_cursor() {
    let h = take(Contract::HandedOver, 280);
    let (dark, rep) = report(&h);
    assert!(h.torn_presents > 0, "the take must present torn: {rep}");
    assert!(!dark.is_empty(), "the gap must reproduce: {rep}");
    assert!(
        !cross_row_licences(&h).is_empty(),
        "the write cursor must be licensed as the caret: {rep}"
    );
}

/// THE OWNER'S SHAPE, the shipping single-pane contract: the torn
/// present's hidden write cursor is not judged as the caret, no licence
/// leaves the composer row, and the line stays one band.
#[test]
fn a_torn_full_screen_repaint_keeps_the_boundary_space_lit() {
    let h = take(Contract::SinglePane, 280);
    let (dark, rep) = report(&h);
    assert!(h.torn_presents > 0, "the take must present torn: {rep}");
    assert!(dark.is_empty(), "owner's shape: {rep}");
    let off_row = cross_row_licences(&h);
    assert!(
        off_row.is_empty(),
        "licensed off the composer row: {off_row:?}{rep}"
    );
}

/// A bracket the terminal times out mid-frame (review round 1): open
/// 1.3 s, the middle read landing at +1.06 s — past the terminal's 1 s
/// `sync_timeout_ms`, which force-clears the MODE with Ink's hidden write
/// cursor still parked in the transcript — and ` bu` typed late in it, at
/// +0.9..1.02 s.
fn outliving_the_timeout() -> Shape {
    Shape {
        open_ms: 1_260,
        keys: [900, 960, 1_020],
        middle: 1_060,
    }
}

/// THE OWNER'S SHAPE, HELD PAST THE TERMINAL'S TIMEOUT (review round 1):
/// the program's frame is still unfinished after the terminal clears the
/// mode, its hidden write cursor is still no caret, and the line stays one
/// band. CONTROL: the withheld contract keeps it whole too. NEGATIVE
/// CONTROL: the first fix's mode-level reading hands the write cursor over
/// once the mode is cleared, and the gap comes back — so this take does
/// reach the case it is written for.
#[test]
fn a_torn_repaint_outliving_the_terminal_timeout_keeps_the_line_whole() {
    let withheld = take_shaped(Contract::Withheld, outliving_the_timeout());
    let (dark, rep) = report(&withheld);
    assert!(
        withheld.torn_presents > 0,
        "withheld: must present torn: {rep}"
    );
    assert!(dark.is_empty(), "withheld control: {rep}");

    let mode_level = take_shaped(Contract::ModeLevel, outliving_the_timeout());
    let (dark, rep) = report(&mode_level);
    assert!(
        !dark.is_empty(),
        "negative control: the mode-level reading must gap past the timeout: {rep}"
    );

    let h = take_shaped(Contract::SinglePane, outliving_the_timeout());
    let (dark, rep) = report(&h);
    assert!(h.torn_presents > 0, "the take must present torn: {rep}");
    assert!(dark.is_empty(), "past the timeout: {rep}");
    let off_row = cross_row_licences(&h);
    assert!(
        off_row.is_empty(),
        "licensed off the composer row: {off_row:?}{rep}"
    );
}

/// The bracket-length sweep the review ran against the first fix (which
/// darkened the whole band at 2.5 s): from inside the hold cap to well past
/// the terminal's timeout, with the keys typed early and late, the
/// shipping contract keeps the line whole wherever the withheld contract
/// does.
#[test]
fn every_bracket_length_keeps_the_line_whole_under_the_shipping_contract() {
    for open_ms in [200_u64, 500, 900, 1_100, 1_500, 2_500] {
        for late in [false, true] {
            let keys = if late {
                let k = open_ms.saturating_sub(360).max(30);
                [k, k + 60, k + 120]
            } else {
                [30, 90, 150]
            };
            let shape = || Shape {
                open_ms,
                keys,
                middle: (keys[2] + 40).min(open_ms),
            };
            let (withheld_dark, _) = report(&take_shaped(Contract::Withheld, shape()));
            let h = take_shaped(Contract::SinglePane, shape());
            let (dark, rep) = report(&h);
            assert_eq!(
                dark, withheld_dark,
                "open {open_ms} ms, late keys {late}: {rep}"
            );
            assert!(
                dark.is_empty(),
                "open {open_ms} ms, late keys {late}: {rep}"
            );
        }
    }
}
