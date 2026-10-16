// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The band under Codex's composer is one run, on every frame, through word hops and edits.
//!
//! Codex (ratatui, per-cell diff) paints an ambient Braille particle field (`⠁ ⠄ ⠈ ⢀`, dim
//! grey) into its composer's blank cells every ~150 ms inside a `?2026` bracket. The particles
//! drift, so a typed space flips `' '` → `'⠈'` → `' '` while the words either side stand; a
//! caret parked on such a cell (an Alt+Right hop lands on the space after the word) clears the
//! particle and the next frame paints it back, and a typed space can even echo as a particle.
//!
//! This file replays Codex's real bytes (`fixtures/codex-particles-2026-09-16.ptylog`: a line,
//! Alt+Left ×3, a word, Alt+Right ×3, two letters) through a real [`Terminal`] sampled as
//! `app_render.rs`'s frame hold samples it, fed to [`CursorGlow`] with the app's hints, and
//! censuses the plan's coverage on the composer row every 8 ms.

use crate::common::{
    LIT_COV, caret_sample, coverage, geom, holes_of, is_arrow, note_blink, observe_caret_row,
    observe_ribbon_rows, rainbow_cfg, sync_scroll, unhex,
};
use aterm_core::render::GlowQuad;
use aterm_core::terminal::{ContentScrollState, Terminal};
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig};
use aterm_effects::rainbow_kitty::TypedClass;
use aterm_spec::derive::{rainbow_typed_continuity_model, same_caret_typed_echo_model};
use std::time::{Duration, Instant};

/// The take's grid and cell: `resize 32 100` in a headless instance, 7×14 px.
const ROWS: usize = 32;
const COLS: usize = 100;
const CW: usize = 7;
const CH: usize = 14;
/// The frame train.
const FRAME_MS: u64 = 8;
/// Replay stops before the take's exit keys (Ctrl+C ×2, then Ctrl+D).
const END_MS: u64 = 9_500;

const FIXTURE: &str = include_str!("fixtures/codex-particles-2026-09-16.ptylog");
/// Codex's wrapped composer at 56×137, 24 px. The first `a` key is at
/// 8893 ms and its complete synchronized echo at 8916 ms; the `and ` run
/// retires before the `introspection` continuation at 10887 ms.
const WRAPPED_FIXTURE: &str = include_str!("fixtures/codex-wrapped-composer-2026-09-22.ptylog");
/// A second 24 px take where the host first presented `and ` in one batch:
/// `a` was 391 ms old at the 10.0 s frame while the trailing space was fresh.
const STALLED_WRAPPED_FIXTURE: &str =
    include_str!("fixtures/codex-wrapped-composer-stalled-2026-09-22.ptylog");
/// A take whose ordinary 22:2→3 `a` admission must stay cached for the first resumed frame.
const FIRST_FRAME_FIXTURE: &str =
    include_str!("fixtures/codex-wrapped-composer-first-frame-2026-09-22.ptylog");

enum Event {
    /// Bytes the program wrote to the terminal.
    Out(Vec<u8>),
    /// Bytes the keyboard sent to the program.
    In(Vec<u8>),
    /// Controller `turn` fences the previous turn's movement licence.
    ClearLicence,
}

fn events(fixture: &str) -> Vec<(u64, Event)> {
    fixture
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, ' ');
            let ms: u64 = it.next().unwrap().parse().unwrap();
            let kind = it.next().unwrap();
            let bytes = unhex(it.next().unwrap_or(""));
            (
                ms,
                match kind {
                    "O" => Event::Out(bytes),
                    "I" => Event::In(bytes),
                    "C" => Event::ClearLicence,
                    k => panic!("bad kind {k}"),
                },
            )
        })
        .collect()
}

/// The config the take ran: rainbow kitty, tall body, intensity 1.0, dark.
fn cfg() -> GlowConfig {
    GlowConfig {
        duration: Duration::from_millis(260),
        intensity: 1.0,
        ..rainbow_cfg()
    }
}

/// One frame's census of one row: the lit columns as a map, with the span's
/// interior dark runs, and where the caret stood.
struct RowCensus {
    ms: u64,
    row: u16,
    caret: u16,
    map: String,
    lit: usize,
    holes: Vec<usize>,
    /// Interior gaps in the ribbon's own live cells on the row: a column
    /// between two live cells that no live cell owns. The plan's coverage
    /// feathers across a one-cell gap, so this catches a dark column the
    /// coverage census does not.
    cell_gaps: Vec<usize>,
}

impl RowCensus {
    fn worst(&self) -> usize {
        self.holes.iter().copied().max().unwrap_or(0)
    }
}

struct Host {
    term: Terminal,
    glow: CursorGlow,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    out: Vec<GlowQuad>,
    row_buf: Vec<char>,
    blink: u64,
    scroll: Option<ContentScrollState>,
    /// The composer row, learnt from the caret at the first typed key.
    composer_row: Option<u16>,
    /// Reproduce a host frame slip while Codex echoes two keys separately.
    suppress_frames_ms: Option<(u64, u64)>,
    suppress_extra_ms: Option<(u64, u64)>,
    /// The frame cadence: ordinary fixture tests keep the dense train, the
    /// screenshot regression uses the glass capture's 5 Hz.
    frame_ms: u64,
    present_on_output: bool,
    census: Vec<RowCensus>,
}

impl Host {
    fn new() -> Self {
        Self::new_with(geom(ROWS, COLS, CW, CH), None)
    }

    fn new_with(g: Geom, composer_row: Option<u16>) -> Self {
        let now = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_pane_columns(0, g.cols);
        Self {
            term: Terminal::new(g.rows as u16, g.cols as u16),
            glow,
            cfg: cfg(),
            g,
            t0: now,
            now,
            out: Vec::new(),
            row_buf: Vec::new(),
            blink: 0,
            scroll: None,
            composer_row,
            suppress_frames_ms: None,
            suppress_extra_ms: None,
            frame_ms: FRAME_MS,
            present_on_output: true,
            census: Vec::new(),
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The host's order: `sync_cursor_effect_scroll`, the frame's samples, then the tick.
    fn frame(&mut self) {
        sync_scroll(&self.term, &mut self.glow, &mut self.scroll, self.now);
        let (c, cur) = caret_sample(&self.term);
        note_blink(&self.term, &mut self.glow, &mut self.blink, self.now);
        self.glow.note_context(self.term.is_alternate_screen());
        self.glow.observe_print_anchor(self.term.print_anchor());
        observe_caret_row(&self.term, &mut self.glow, &mut self.row_buf, c, self.now);
        observe_ribbon_rows(&self.term, &mut self.glow, &mut self.row_buf);
        self.glow
            .tick(cur, self.now, &self.cfg, self.g, &mut self.out);
        self.record(c.row, c.col);
        self.dump_cells();
    }

    /// `CODEX_REPLAY_CELLS=<ms-from>:<ms-to>` prints every ribbon cell on the
    /// composer row for the frames in that window: column, layer, cohort,
    /// whether it is leaving, and its cohort's clock.
    fn dump_cells(&self) {
        let Some(row) = self.composer_row else { return };
        let Some(win) = std::env::var("CODEX_REPLAY_CELLS").ok() else {
            return;
        };
        let Some((a, b)) = win.split_once(':') else {
            return;
        };
        let (a, b): (u64, u64) = (a.parse().unwrap_or(0), b.parse().unwrap_or(0));
        let ms = self.ms();
        if ms < a || ms > b {
            return;
        }
        let rib = self.glow.v2_ribbon().expect("rainbow kitty owns the frame");
        let cov = coverage(&self.glow, self.g, row);
        let mut cells: Vec<_> = rib.cells().iter().filter(|c| c.row == row).collect();
        cells.sort_by_key(|c| c.col);
        let mut s = format!("t={ms:6} cells:");
        for c in cells {
            let coh = rib.cohorts().iter().find(|k| k.id == c.cohort);
            let alive = coh
                .map(|k| k.alive_at.saturating_duration_since(self.t0).as_millis() as i64)
                .unwrap_or(-1);
            let ab = coh.map(|k| k.abandoned).unwrap_or(false);
            s.push_str(&format!(
                " {}:{:?}/c{}{}{}@{}={}b{}{}",
                c.col,
                c.layer,
                c.cohort,
                if c.leaving() { "L" } else { "" },
                if ab { "A" } else { "" },
                alive,
                cov[usize::from(c.col).min(self.g.cols - 1)],
                c.born.saturating_duration_since(self.t0).as_millis(),
                if c.typing { "T" } else { "" }
            ));
        }
        if let Some(last) = self.glow.admission_log().last() {
            s.push_str(&format!("\n   ring: {}", last.line(self.now)));
        }
        // Every cell in the pool born in the last 60 ms, whatever its row.
        let fresh: Vec<String> = rib
            .cells()
            .iter()
            .filter(|c| self.now.saturating_duration_since(c.born).as_millis() < 60)
            .map(|c| {
                format!(
                    "({},{}) b{} {:?}",
                    c.row,
                    c.col,
                    c.born.saturating_duration_since(self.t0).as_millis(),
                    c.layer
                )
            })
            .collect();
        s.push_str(&format!(
            "\n   fresh: {fresh:?} pool={} cohorts={}",
            rib.cells().len(),
            rib.cohorts().len()
        ));
        eprintln!("{s}");
    }

    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        if !self.present_on_output {
            // A live window presents on its own cadence even while PTY
            // packets arrive more often than frames. Keep that cadence
            // anchored to the fixture clock, not each packet's timestamp.
            let mut next = (self.ms() / self.frame_ms + 1) * self.frame_ms;
            while next <= ms {
                self.now = self.t0 + Duration::from_millis(next);
                if !self.term.sync_open_dirty() && self.frame_allowed() {
                    self.frame();
                }
                next += self.frame_ms;
            }
            self.now = t;
            return;
        }
        while self.now + Duration::from_millis(self.frame_ms) <= t {
            self.now += Duration::from_millis(self.frame_ms);
            if !self.term.sync_open_dirty() && self.frame_allowed() {
                self.frame();
            }
        }
        self.now = t;
    }

    fn frame_allowed(&self) -> bool {
        self.suppress_frames_ms
            .is_none_or(|(start, end)| !(start..end).contains(&self.ms()))
            && self
                .suppress_extra_ms
                .is_none_or(|(start, end)| !(start..end).contains(&self.ms()))
    }

    /// The hint the app stamps for what the keyboard sent: a printable byte is
    /// a typed key, Backspace erases one glyph, an arrow (plain or Alt) is a
    /// navigation key. The terminal's replies to Codex's queries and the
    /// take's exit chords stamp nothing.
    fn hint(&mut self, bytes: &[u8]) {
        match bytes {
            [b] if (0x20..0x7f).contains(b) => {
                if self.composer_row.is_none() {
                    self.composer_row = Some(self.term.cursor().row);
                }
                self.glow.note_typed_expected(
                    self.now,
                    1,
                    false,
                    TypedClass::Glyph,
                    char::from(*b),
                );
            }
            [0x7f] => self.glow.note_backspace_erasing(self.now, Some(1)),
            b if is_arrow(b) => self.glow.note_motion(self.now),
            _ => {}
        }
    }

    /// This frame's census of the plan's coverage on the composer row.
    fn record(&mut self, caret_row: u16, caret_col: u16) {
        let Some(row) = self.composer_row else { return };
        let cov = coverage(&self.glow, self.g, row);
        let lit: Vec<usize> = (0..self.g.cols).filter(|&i| cov[i] >= LIT_COV).collect();
        if lit.len() < 2 {
            return;
        }
        let holes = holes_of(&cov).into_iter().map(|(_, len)| len).collect();
        let caret = if caret_row == row {
            caret_col
        } else {
            u16::MAX
        };
        let mut live: Vec<u16> = self
            .glow
            .v2_ribbon()
            .expect("rainbow kitty owns the frame")
            .cells()
            .iter()
            .filter(|c| c.row == row && !c.leaving())
            .map(|c| c.col)
            .collect();
        live.sort_unstable();
        live.dedup();
        let cell_gaps: Vec<usize> = live
            .windows(2)
            .filter(|w| w[1] > w[0] + 1)
            .map(|w| usize::from(w[1] - w[0] - 1))
            .collect();
        self.census.push(RowCensus {
            ms: self.ms(),
            row,
            caret,
            cell_gaps,
            map: cov
                .iter()
                .map(|&v| if v >= LIT_COV { '#' } else { '.' })
                .collect(),
            lit: lit.len(),
            holes,
        });
    }

    fn broken(&self) -> Vec<&RowCensus> {
        self.census.iter().filter(|c| !c.holes.is_empty()).collect()
    }

    fn report(&self, what: &str) -> String {
        let broken = self.broken();
        let worst = broken.iter().map(|c| c.worst()).max().unwrap_or(0);
        let mut s = format!(
            "{what}: {} of {} row-frames carry an interior dark run, worst {worst} cells",
            broken.len(),
            self.census.len()
        );
        for c in broken.iter().take(12) {
            s.push_str(&format!(
                "\n  t={:6} r={:2} caret={:3} lit={:2} {}",
                c.ms, c.row, c.caret, c.lit, c.map
            ));
        }
        s
    }
}

fn replay() -> Host {
    replay_with(FIXTURE, END_MS, Host::new())
}

fn replay_with(fixture: &str, end_ms: u64, mut h: Host) -> Host {
    let dump = std::env::var_os("CODEX_REPLAY_DUMP");
    for (ms, ev) in events(fixture) {
        if ms > end_ms {
            break;
        }
        h.run_to(ms);
        match ev {
            Event::Out(bytes) => {
                h.term.process(&bytes);
                if h.present_on_output && !h.term.sync_open_dirty() && h.frame_allowed() {
                    h.frame();
                }
            }
            Event::In(bytes) => h.hint(&bytes),
            Event::ClearLicence => h.glow.clear_typed(h.now),
        }
    }
    h.run_to(end_ms);
    if let Some(path) = dump {
        let mut s = String::new();
        for c in &h.census {
            s.push_str(&format!(
                "t={:6} r={:2} caret={:3} lit={:2} holes={:?} {}\n",
                c.ms, c.row, c.caret, c.lit, c.holes, c.map
            ));
        }
        std::fs::write(path, s).expect("dump");
    }
    h
}

/// A line, three word hops back, a word typed inside it, three word hops
/// forward, two letters at the end: the two spaces the forward hops parked
/// the caret on stay lit with the words either side of them.
#[test]
fn a_backward_edit_in_codex_never_punches_a_hole_in_the_band() {
    let h = replay();
    assert!(
        h.census.iter().any(|c| c.lit >= 9),
        "the take never lit a band of 9 cells: {}",
        h.report("codex-g1")
    );
    assert!(h.broken().is_empty(), "{}", h.report("codex-g1"));
}

/// A space echoed as a particle is still the user's space. While the line
/// is typed, with no navigation and nothing erased, the light under the
/// first word still stands when the last word is typed, as it does in zsh.
#[test]
fn plain_typing_under_codex_keeps_the_whole_line_lit() {
    let h = replay();
    // The line's last key lands at 5432 ms in the fixture; the first at 2356.
    let at_end: Vec<&RowCensus> = h
        .census
        .iter()
        .filter(|c| (5400..5500).contains(&c.ms))
        .collect();
    assert!(
        !at_end.is_empty(),
        "no census frames at the end of the line"
    );
    let widest = at_end.iter().map(|c| c.lit).max().unwrap_or(0);
    assert!(
        widest >= 30,
        "at the end of a 38-key line only {widest} cells were lit: {}",
        at_end
            .iter()
            .take(3)
            .map(|c| format!("\n  t={:6} lit={:2} {}", c.ms, c.lit, c.map))
            .collect::<String>()
    );
}

/// The hand's own cells are one interval while it types. The plan feathers
/// coverage across a missing column, so this checks the ribbon's live cells:
/// the first key of the mid-line insert, typed over a lit cell the hops
/// brought the caret back onto, re-lays a cell there (`Ribbon::sweep`), and
/// no frame from the insert to the last key has a gap at the insert column.
#[test]
fn a_key_typed_over_the_band_gets_its_own_cell() {
    let h = replay();
    // From the insert's first echo (6289 ms) to the last key of the take,
    // while the hand is on the row: the suffix's melting cells are
    // `leaving` and excluded, so the live set is the band under the hand.
    let broken: Vec<&RowCensus> = h
        .census
        .iter()
        .filter(|c| (6400..7900).contains(&c.ms) && !c.cell_gaps.is_empty())
        .collect();
    assert!(
        broken.is_empty(),
        "{} frames with a gap in the ribbon's live cells under the hand, first: t={} gaps={:?}",
        broken.len(),
        broken.first().map_or(0, |c| c.ms),
        broken.first().map_or(&Vec::new(), |c| &c.cell_gaps)
    );
}

fn first_a_reowned(h: &Host) -> bool {
    h.glow.v2_ribbon().unwrap().cells().iter().any(|c| {
        c.row == 22 && c.col == 2 && c.typing && c.born >= h.t0 + Duration::from_millis(8_893)
    })
}

fn first_echo_model_verdict(evidence: &str, typeahead: bool) -> bool {
    let model = same_caret_typed_echo_model();
    let mut state = model.init_state();
    if matches!(evidence, "KeylessPaint" | "BatchKeyless") {
        assert!(model.fire(evidence, &mut state));
    } else {
        assert!(model.fire("BankA", &mut state));
        if typeahead {
            assert!(model.fire("BankN", &mut state));
        }
        assert!(model.fire(evidence, &mut state));
    }
    assert!(model.fire("Resolve", &mut state));
    state["admitted"] == 1
}

/// Codex rewrites the first wrapped-row glyph under a same-position caret
/// while an unrelated status row may become the last print anchor. The exact
/// key glyph and changed, hand-owned cell make this one-key re-lay licensed.
#[test]
fn first_codex_key_on_wrapped_row_reowns_its_cell() {
    let h = replay_warm_wrapped(&wrapped_with_licence_clear(), 9_000);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("ExactA", false)
    );
    assert!(
        !h.glow
            .v2_ribbon()
            .unwrap()
            .cells()
            .iter()
            .any(|c| c.row == 18)
    );
}

/// A loaded child may emit only the first wrapped-row glyph after the short
/// 250 ms key hint has expired. The press is still in flight, and the exact
/// blank-to-`a` caret-row probe plus the new print generation identify its
/// echo even though Codex returns the visible caret to the same cell and
/// leaves its status row as the last print anchor. An ambient particle or a
/// keyless same-row print must not borrow that in-flight credit.
#[test]
fn delayed_single_same_caret_echo_reowns_only_the_exact_typed_glyph() {
    let base = wrapped_with_licence_clear();
    let first_echo = base
        .lines()
        .find_map(|line| line.strip_prefix("8917 O "))
        .expect("captured first wrapped-row echo");
    let mut before_echo = base
        .lines()
        .filter(|line| line.split_once(' ').unwrap().0.parse::<u64>().unwrap() < 8917)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    before_echo.push(format!("9300 O {first_echo}"));
    let delayed = before_echo.join("\n");
    let h = replay_warm_wrapped(&delayed, 9_400);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("DelayedExactA", false),
        "real delayed echo disagrees with the derived licence model"
    );
    assert!(
        first_a_reowned(&h),
        "the exact single echo at 507 ms must re-lay the first wrapped-row cell"
    );

    let particle = delayed.replacen("1b5b32333b334861", "1b5b32333b3348e2a081", 1);
    assert_ne!(particle, delayed, "the ambient-glyph control must differ");
    assert_eq!(
        first_a_reowned(&replay_warm_wrapped(&particle, 9_400)),
        first_echo_model_verdict("DelayedAmbientOtherGlyph", false),
        "ambient same-row particle cannot spend the delayed key"
    );
    let keyless = delayed.replacen("8893 I 61\n", "", 1);
    assert_ne!(
        keyless, delayed,
        "the keyless control must remove the press"
    );
    assert_eq!(
        first_a_reowned(&replay_warm_wrapped(&keyless, 9_400)),
        first_echo_model_verdict("KeylessPaint", false),
        "keyless same-row program output cannot create a typed ribbon cell"
    );
}

/// An ambient particle at the same cell is a different glyph, and a matching
/// program print before the press is too old to claim its credit.
#[test]
fn first_a_recovery_refuses_particle_and_pre_key_program_paint() {
    let base = wrapped_with_licence_clear();
    let particle = base.replacen("1b5b32333b334861", "1b5b32333b3348e2a081", 1);
    assert!(particle.contains("1b5b32333b3348e2a081"));
    let h = replay_warm_wrapped(&particle, 9_000);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("AmbientOtherGlyph", false),
        "an ambient Braille cell spent the `a` key"
    );

    let first_echo = base
        .lines()
        .find_map(|line| line.strip_prefix("8917 O "))
        .expect("captured `a` echo");
    let mut earlier = base
        .lines()
        .filter(|line| !line.starts_with("8917 O "))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    earlier.push(format!("8400 O {first_echo}"));
    earlier.sort_by_key(|line| line.split_once(' ').unwrap().0.parse::<u64>().unwrap());
    let earlier = earlier.join("\n");
    let h = replay_warm_wrapped(&earlier, 9_000);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("EarlierMatchingPaint", false),
        "pre-key program paint spent a later key"
    );

    let outputs_only = base
        .lines()
        .filter(|line| *line != "8893 I 61")
        .collect::<Vec<_>>()
        .join("\n");
    let h = replay_warm_wrapped(&outputs_only, 9_000);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("KeylessPaint", false)
    );

    let typeahead = base.replacen("8893 I 61\n", "8893 I 61\n8895 I 6e\n", 1);
    let h = replay_warm_wrapped(&typeahead, 9_000);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("ExactA", true)
    );
}

/// A loaded host can miss the individual `a` frame and first observe the
/// `a,n,d` bytes as one forward hop. The exact first glyph is re-laid before
/// the ordinary batch spends its own credits; a Braille particle cannot be
/// treated as that glyph.
#[test]
fn coalesced_wrapped_prefix_keeps_first_key_and_refuses_particle() {
    let base = wrapped_with_licence_clear();
    let mut host = Host::new_with(geom(56, 137, 15, 28), Some(22));
    host.frame_ms = 200;
    host.present_on_output = false;
    host.suppress_frames_ms = Some((0, 8_000));
    host.suppress_extra_ms = Some((8_894, 9_190));
    let h = replay_with(&base, 9_200, host);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("ExactA", false)
    );
    for (col, key_ms) in [(3, 8_981), (4, 9_063)] {
        assert!(
            h.glow.v2_ribbon().unwrap().cells().iter().any(|c| {
                c.row == 22
                    && c.col == col
                    && c.typing
                    && c.born >= h.t0 + Duration::from_millis(key_ms)
            }),
            "coalesced prefix lost column {col}"
        );
    }

    let particle = base.replacen("1b5b32333b334861", "1b5b32333b3348e2a081", 1);
    let mut host = Host::new_with(geom(56, 137, 15, 28), Some(22));
    host.frame_ms = 200;
    host.present_on_output = false;
    host.suppress_frames_ms = Some((0, 8_000));
    host.suppress_extra_ms = Some((8_894, 9_190));
    let h = replay_with(&particle, 9_200, host);
    assert_eq!(
        first_a_reowned(&h),
        first_echo_model_verdict("AmbientOtherGlyph", false)
    );
}

/// The captured wrapped-composer screen just before its first `a`, then one longer
/// synchronized echo. The 5 Hz train samples the blank row before the keys and misses every
/// intermediate echo, so at the frame that sees the phrase the first key is past the hint
/// window and the last is fresh.
fn long_wrapped_batch(
    typed: &str,
    painted: &str,
    omit_first: bool,
    interleave: bool,
) -> (String, u64, u64) {
    assert_eq!(typed.len(), painted.len());
    assert!((9..=128).contains(&typed.len()));
    assert!(typed.is_ascii() && painted.is_ascii());
    // The captured setup ends at 8287 ms. A full 128-key run needs 3810 ms
    // before its first presentation, so move that replay's echo later while
    // preserving the same 5 Hz frame train and the captured repaint bytes.
    let first_frame = if typed.len() <= 32 { 10_600 } else { 14_600 };
    let first = first_frame - 61 - (typed.len() as u64 - 1) * 30;
    let last = first + (typed.len() as u64 - 1) * 30;
    assert!(
        first_frame - first > 250,
        "the first key must be past the hint window"
    );
    assert!(
        first_frame - last < 250,
        "the exact tail must still be fresh"
    );
    let mut lines: Vec<String> = STALLED_WRAPPED_FIXTURE
        .lines()
        .filter(|line| line.split_once(' ').unwrap().0.parse::<u64>().unwrap() <= 8_287)
        .map(str::to_owned)
        .collect();
    lines.push(format!("{} C", first - 15));
    for (i, byte) in typed.bytes().enumerate() {
        let at = first + i as u64 * 30;
        if !(omit_first && i == 0) {
            lines.push(format!("{at} I {byte:02x}"));
        }
        if interleave && i == 4 {
            lines.push(format!("{} I 21", at + 10)); // An unrelated `!` key.
        }
    }
    let repaint = format!(
        "\x1b[?2026h\x1b[?25l\x1b[23;3H{painted}\x1b[23;{}H\x1b[?25h\x1b[?2026l",
        3 + painted.len()
    );
    let hex = repaint
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    lines.push(format!("{} O {hex}", first_frame - 50));
    (lines.join("\n"), first, first_frame)
}

fn replay_long_wrapped_batch(
    fixture: &str,
    first_key_ms: u64,
    first_frame_ms: u64,
    end_ms: u64,
) -> Host {
    let mut host = Host::new_with(geom(56, 137, 15, 28), Some(22));
    host.frame_ms = 200;
    host.present_on_output = false;
    host.suppress_frames_ms = Some((0, 8_000));
    host.suppress_extra_ms = Some((first_key_ms, first_frame_ms));
    replay_with(fixture, end_ms, host)
}

fn long_batch_first_cell_reowned(h: &Host, first_frame_ms: u64) -> bool {
    h.glow.v2_ribbon().unwrap().cells().iter().any(|cell| {
        cell.row == 22
            && cell.col == 2
            && cell.typing
            && !cell.leaving()
            && cell.born >= h.t0 + Duration::from_millis(first_frame_ms - 250)
    })
}

#[test]
fn nine_to_128_key_wrapped_batches_reown_the_first_glyph_on_their_first_frame() {
    let phrase = "abcdefghijklmnopqrstuvwxyzABCDEF".repeat(4);
    for len in [9, 32, 33, 128] {
        let (fixture, first, frame_ms) =
            long_wrapped_batch(&phrase[..len], &phrase[..len], false, false);
        let before = replay_long_wrapped_batch(&fixture, first, frame_ms, first - 1);
        assert!(
            !long_batch_first_cell_reowned(&before, frame_ms),
            "{len} keys: a newly born cell must not mask the missing first-glyph repair"
        );
        let after = replay_long_wrapped_batch(&fixture, first, frame_ms, frame_ms);
        assert!(
            long_batch_first_cell_reowned(&after, frame_ms),
            "{len} keys: the first wrapped glyph was dark after the exact delayed echo"
        );
        let ribbon = after.glow.v2_ribbon().unwrap();
        for col in 2..2 + len as u16 {
            assert!(
                ribbon.cells().iter().any(|cell| cell.row == 22
                    && cell.col == col
                    && cell.typing
                    && !cell.leaving()),
                "{len} keys: coalesced glyph cell {col} was absent on its first frame"
            );
        }
        let first_frame = after
            .census
            .iter()
            .find(|frame| frame.ms == frame_ms)
            .expect("the loaded host presented the delayed echo at 5 Hz");
        assert_eq!(
            first_frame.map.as_bytes()[2],
            b'#',
            "{len} keys: the reowned first cell still left a visible gap"
        );
    }
}

#[test]
fn long_wrapped_batch_refuses_keyless_mismatch_and_interleaved_prints() {
    let phrase = "abcdefghi";
    let mut changed = phrase.to_owned();
    changed.replace_range(4..5, "X");
    for (label, painted, omit_first, interleave) in [
        ("keyless first glyph", phrase, true, false),
        ("different middle glyph", changed.as_str(), false, false),
        ("interleaved unrelated key", phrase, false, true),
    ] {
        let (fixture, first, frame_ms) =
            long_wrapped_batch(phrase, painted, omit_first, interleave);
        let after = replay_long_wrapped_batch(&fixture, first, frame_ms, frame_ms);
        assert!(
            !long_batch_first_cell_reowned(&after, frame_ms),
            "{label}: the exceptional prefix repair accepted unrelated output"
        );
    }
}

#[test]
fn wide_wrapped_batch_still_requires_every_exact_key() {
    let phrase = "abcdefghijklmnopqrstuvwxyzABCDEF".repeat(4);
    for len in [33, 128] {
        let mut changed = phrase[..len].to_owned();
        changed.replace_range(len / 2..len / 2 + 1, "X");
        let (fixture, first, frame_ms) = long_wrapped_batch(&phrase[..len], &changed, false, false);
        let after = replay_long_wrapped_batch(&fixture, first, frame_ms, frame_ms);
        assert!(
            !long_batch_first_cell_reowned(&after, frame_ms),
            "{len} keys: a changed middle glyph claimed the missing first cell"
        );
    }
}

/// In the stalled captured take the first presentation after `and ` is one
/// 22:3→6 hop. The oldest `a` is past the short hint window, but the exact
/// chronological a/n/d/space suffix is still in flight and the tail is fresh.
/// A stale wrap-fill Space before that run must be retired, never reused.
#[test]
fn stalled_codex_batch_reowns_first_a_from_exact_inflight_run() {
    let base = STALLED_WRAPPED_FIXTURE
        .replacen("9609 I 61\n", "9580 C\n9609 I 61\n", 1)
        .replacen("11593 I 69\n", "11575 C\n11593 I 69\n", 1);
    assert!(base.contains("9580 C\n9609 I 61\n"));
    let host = |extra| {
        let mut host = Host::new_with(geom(56, 137, 15, 28), Some(22));
        host.frame_ms = 400;
        host.present_on_output = false;
        host.suppress_frames_ms = Some((0, 8_000));
        host.suppress_extra_ms = Some(extra);
        host
    };
    // The loaded live take declined wrap-fill before `a`, so this branch
    // must work without an old hand-owned cell at column 2.
    let before = replay_with(&base, 9_600, host((8_200, 8_800)));
    assert!(
        !before
            .glow
            .v2_ribbon()
            .unwrap()
            .cells()
            .iter()
            .any(|c| { c.row == 22 && c.col == 2 && c.typing && !c.leaving() }),
        "the control must actually enter the no-owner branch"
    );
    let h = replay_with(&base, 10_000, host((8_200, 8_800)));
    let admitted = h.glow.v2_ribbon().unwrap().cells().iter().any(|c| {
        c.row == 22 && c.col == 2 && c.typing && c.born >= h.t0 + Duration::from_millis(9_609)
    });
    assert_eq!(
        admitted,
        first_echo_model_verdict("ExactBatch", true),
        "stalled first a was not re-owned at the coalesced presentation"
    );

    let first_a_born = |h: &Host| {
        h.glow.v2_ribbon().unwrap().cells().iter().any(|c| {
            c.row == 22 && c.col == 2 && c.typing && c.born >= h.t0 + Duration::from_millis(9_609)
        })
    };
    let particle = base.replacen(
        "9632 O 1b5b3f32303236681b5b32333b334861",
        "9632 O 1b5b3f32303236681b5b32333b3348e2a081",
        1,
    );
    assert_ne!(particle, base, "captured first a echo must be mutated");
    assert_eq!(
        first_a_born(&replay_with(&particle, 10_000, host((8_200, 8_800)))),
        first_echo_model_verdict("BatchOtherGlyph", true),
        "a Braille program glyph claimed the exact a key"
    );
    let keyless = base
        .lines()
        .filter(|line| *line != "9609 I 61")
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        first_a_born(&replay_with(&keyless, 10_000, host((8_200, 8_800)))),
        first_echo_model_verdict("BatchKeyless", false),
        "program output with no a key was admitted"
    );
    assert_eq!(
        first_a_born(&replay_with(&base, 10_000, host((8_200, 10_000)))),
        first_echo_model_verdict("BatchNoPriorProbe", true),
        "a batch with no previous row sample was admitted"
    );

    let resumed = replay_with(&base, 12_000, host((8_200, 8_800)));
    let first = resumed
        .census
        .iter()
        .find(|c| c.ms >= 11_593 && c.caret >= 8)
        .expect("stalled continuation frame");
    for col in 2..6 {
        assert_eq!(
            first.map.as_bytes()[col],
            b'#',
            "stalled continuation left old col {col} dark on its first frame t={}",
            first.ms
        );
    }
}

#[test]
fn ordinary_first_a_reanchor_is_cached_for_the_first_resumed_frame() {
    let fixture = FIRST_FRAME_FIXTURE
        .replacen("8875 I 61\n", "8860 C\n8875 I 61\n", 1)
        .replacen("10868 I 69\n", "10850 C\n10868 I 69\n", 1);
    assert!(fixture.contains("8860 C\n8875 I 61\n"));
    let h = replay_warm_wrapped(&fixture, 11_200);
    assert!(
        h.glow
            .admission_log()
            .any(|a| { a.origin == (22, 2) && a.target == (22, 3) }),
        "captured a admission was not replayed"
    );
    let first = h
        .census
        .iter()
        .find(|c| c.ms >= 10_868 && c.caret >= 8)
        .expect("first resumed frame");
    for col in 2..6 {
        assert_eq!(
            first.map.as_bytes()[col],
            b'#',
            "ordinary re-anchor left prefix col {col} dark at t={}",
            first.ms
        );
    }
}

/// The `and ` prefix, lit on the wrapped row, swooshes out after a 1.74 s key gap while Codex
/// leaves its text in place; the first resumed key rejoins that text on its own frame. The
/// 5 Hz train with two warm-up frames matches the live cadence: presenting each PTY packet
/// would hide the expiry under extra frames.
#[test]
fn codex_same_row_continuation_restores_the_exact_retired_prefix() {
    let separate_turns = wrapped_with_licence_clear();
    let h = replay_warm_wrapped(&separate_turns, 11_200);
    assert_eq!(
        renewed_prefix_count(&h, 10_887) > 0,
        continuity_model_verdict(None, true),
        "captured Codex continuation disagrees with the derived model"
    );
    let ribbon = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
    for col in 2..6 {
        assert!(
            ribbon.cells().iter().any(|c| c.row == 22
                && c.col == col
                && c.typing
                && !c.leaving()
                && c.born >= h.t0 + Duration::from_millis(10_887)),
            "typed prefix cell {col} did not rejoin after the pause"
        );
    }
    let first = h
        .census
        .iter()
        .find(|c| c.ms >= 10_887 && c.caret >= 8)
        .expect("captured continuation frame");
    for col in 2..6 {
        assert_eq!(
            first.map.as_bytes()[col],
            b'#',
            "col {col} has a cell but no visible ribbon at the first continuation frame t={}",
            first.ms
        );
    }
}

#[test]
fn dormant_typed_run_adds_no_idle_work_after_natural_exit() {
    let after_and = wrapped_with_licence_clear()
        .lines()
        .filter(|line| line.split_once(' ').unwrap().0.parse::<u64>().unwrap() < 10_887)
        .collect::<Vec<_>>()
        .join("\n");
    let h = replay_warm_wrapped(&after_and, 11_800);
    let ribbon = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
    assert!(ribbon.at_rest(), "retired prefix kept the ribbon active");
    assert!(
        ribbon.next_change_deadline(h.now).is_none(),
        "dormant typed content scheduled another frame"
    );
}

/// The live `ctl turn` path clears movement credits between the two typing
/// bursts. Content evidence survives that boundary, and still requires a
/// fresh typed echo to rejoin it.
fn wrapped_with_licence_clear() -> String {
    let fixture = WRAPPED_FIXTURE
        .replacen("8893 I 61\n", "8880 C\n8893 I 61\n", 1)
        .replacen("10887 I 69\n", "10870 C\n10887 I 69\n", 1);
    assert!(fixture.contains("8880 C\n"));
    assert!(fixture.contains("10870 C\n"));
    fixture
}

fn replay_warm_wrapped(fixture: &str, end_ms: u64) -> Host {
    let mut host = Host::new_with(geom(56, 137, 15, 28), Some(22));
    host.frame_ms = 200;
    host.present_on_output = false;
    host.suppress_frames_ms = Some((0, 8_000));
    replay_with(fixture, end_ms, host)
}

fn renewed_prefix_count(h: &Host, after_ms: u64) -> usize {
    let ribbon = h.glow.v2_ribbon().expect("rainbow kitty owns the frame");
    ribbon
        .cells()
        .iter()
        .filter(|c| {
            c.row == 22
                && (2..6).contains(&c.col)
                && c.born >= h.t0 + Duration::from_millis(after_ms)
        })
        .count()
}

fn continuity_model_verdict(interruption: Option<&str>, typed: bool) -> bool {
    let model = rainbow_typed_continuity_model();
    let mut state = model.init_state();
    assert!(model.fire("TypeRun", &mut state));
    assert!(model.fire("RetireNaturally", &mut state));
    assert!(model.fire("ClearLicence", &mut state));
    if let Some(action) = interruption {
        assert!(model.fire(action, &mut state));
    }
    assert!(model.fire(
        if typed { "ResumeTyped" } else { "ProgramPaint" },
        &mut state
    ));
    state["revived"] == 1
}

/// Same content, same position and a typed continuation are all required.
/// A keyless paint, a row whose saved glyph changed, a navigation key with
/// no observed motion, and a continuation past the chain window must not
/// resurrect the old prefix.
#[test]
fn retired_prefix_rejoin_refuses_changed_keyless_navigated_and_stale_rows() {
    let base = wrapped_with_licence_clear();
    let altered = base.replacen(
        "10887 I 69\n",
        "10870 O 1b5b32333b3348581b5b32333b3748\n10887 I 69\n",
        1,
    );
    assert!(altered.contains("10870 O"));
    assert_eq!(
        renewed_prefix_count(&replay_warm_wrapped(&altered, 11_200), 10_887) > 0,
        continuity_model_verdict(Some("ChangeGlyph"), true)
    );

    let keyless = base
        .lines()
        .filter(|line| {
            let (ms, rest) = line.split_once(' ').expect("fixture line");
            ms.parse::<u64>().expect("fixture clock") < 10_887 || !rest.starts_with("I ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        renewed_prefix_count(&replay_warm_wrapped(&keyless, 11_200), 10_887) > 0,
        continuity_model_verdict(None, false)
    );

    let navigated = base.replacen("10887 I 69\n", "10870 I 1b5b44\n10887 I 69\n", 1);
    assert_eq!(
        renewed_prefix_count(&replay_warm_wrapped(&navigated, 11_200), 10_887) > 0,
        continuity_model_verdict(Some("Navigation"), true)
    );

    let stale = base
        .lines()
        .map(|line| {
            let (ms, rest) = line.split_once(' ').expect("fixture line");
            let ms = ms.parse::<u64>().expect("fixture clock");
            format!("{} {rest}", if ms >= 10_887 { ms + 5_000 } else { ms })
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        renewed_prefix_count(&replay_warm_wrapped(&stale, 16_200), 15_887) > 0,
        continuity_model_verdict(Some("AgePastChain"), true)
    );
}
