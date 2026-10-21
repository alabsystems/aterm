// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EDITING WIN: what predictive editing buys someone typing real command
//! lines over a link, and what it costs, MEASURED on the safety theorem's own
//! world (`common/world.rs`): its aterm-core engine, its far ends in the byte
//! forms measured from them, and its host wired the way aterm-gui wires the
//! predictor (the key-time row, the edit keys handed to `predict_edit`, a chain
//! painted all or none by aterm-core's span painter). The theorem says a
//! prediction never corrupts the view; this file says how much earlier the view
//! is right, on a typist's corpus rather than a generator's chaos, and checks
//! the theorem's clauses over every frame of it while it counts.
//!
//! Everything is a pure function of its seeds: the clock is injected, nothing
//! is timed, and every number below is a count. A rerun prints the same table.
//!
//! ## The corpus
//!
//! Three hundred command lines from a seeded generator over twenty real
//! commands (`git`, `ls`, `cd`, `grep`, a `targo` test, `make`, `docker`,
//! `kubectl`, `vim`, `tail`, `find | wc`, `ssh`, `cat`, `rg`, `du`, `echo`,
//! `history`), each typed in one of ten FAMILIES of real editing, by weight:
//! plain (35), a typo at the end fixed with Backspace (15), a typo several
//! glyphs back reached with `←`, rubbed out, retyped, and left by End, Ctrl-E or
//! `→` (12), Home or Ctrl-A and `sudo ` typed in front (6), Ctrl-A Ctrl-K and
//! another command (5), `←` to the first space and ` -v` inserted (6), a junk
//! `xx` typed first and removed by Home, Delete, Delete, End (4), a Tab
//! completing the first word (10), Up and a small edit inside the recalled line
//! (5), and Alt-B, a word motion, then a glyph (2). A typist's cadence: 60 to
//! 200 ms between glyphs, 350 to 1200 ms to notice a typo, 70 to 120 ms
//! between repeated arrows, and a pause of the link's round trip plus 300 to
//! 1200 ms before each line (the prompt has to be there). It is the region
//! design's probe corpus, ported generator call for generator call
//! (`the_corpus_is_the_region_probe_s`), so its numbers and these are of the
//! same 6999 keys.
//!
//! The far end is the world's line editor (`Editor::Zle`, zsh 5.9's byte forms,
//! and `Editor::Readline`, bash 3.2.57's), with a history Up recalls and a
//! completion table Tab completes from (`Sim::corpus_shell`), printing one line
//! of output and a fresh prompt for each Enter on a 24 by 80 screen. From the
//! twelfth line's Enter on, every Enter scrolls the screen, as at a real
//! shell's bottom row. INTEGRATION is either aterm's shell integration of a local shell (a
//! signed OSC 133 `B`, so Home, End, Ctrl-A, Ctrl-E and Delete are learned
//! keys), or none (a shell over ssh: those keys are never modelled).
//!
//! ## The link
//!
//! Each key reaches the far end half a round trip after its press, within
//! ±15 %, in order; its output comes back half a round trip later, within
//! ±15 %, in order, half the time in TWO PIECES 1 to 6 ms apart, split at a
//! byte drawn at random, so the frames a split read shows (a mid-line glyph
//! drawn, the tail it shifts not yet) happen. A frame is drawn after every press
//! and every piece delivered, and at every deadline of the predictor plus 1 ms
//! (its expiry sweep), as aterm-gui draws them.
//!
//! ## The metrics, per run and per key class
//!
//! The TRUTH is the screen a key's output settles to: a pass with prediction
//! off, every key read and its output delivered whole before the next, records
//! the row, its text and the cursor after each key. A key COUNTS unless it is a
//! line terminator or changed nothing (Up with no history yet, `←` at the start).
//!
//! * BEFORE ECHO: a counted key `k` is shown before its echo when some frame's
//!   displayed view `D` shows the truth after some key `j ≥ k` of its line,
//!   cells and drawn cursor, strictly earlier than the first frame whose
//!   authoritative screen `A` does.
//! * CORRECT AT PRESS: the frame drawn at `k`'s press already shows the truth
//!   after `k`.
//! * WRONG EPISODES on whole-line states: a frame that painted something (a
//!   cell of `D` other than `A`'s, or a cursor other than the engine's) whose
//!   row shows no state its line passes through, integrated until a frame is
//!   right again: the episodes, their total and longest time, the most cells
//!   wrong in one frame, and cells times time. The world's far ends here draw no
//!   decoration (no right prompt, no suggestion), so every wrong cell is a
//!   buffer cell.
//! * MISPREDICTED CELLS: the same frames counted by the cell. A cell of a wrong
//!   row that `D` shows other than both `A` and the line's newest state (the
//!   drawn cursor's cell, where that is all that is wrong) is a mispredicted
//!   cell from the first frame showing it wrong until the first that does not:
//!   the times a cell went wrong, and its LIFETIME on glass in injected time at
//!   the median, the 95th and the 99th percentile and the longest. An episode
//!   says how long some line was wrong; a lifetime says how long the eye saw
//!   one wrong cell, which TIMELINESS bounds by the glitch window.
//! * PERCEIVED LATENCY: from the press to the first frame that shows the key's
//!   effect, displayed or echoed: its median and 95th percentile.
//! * CHURN, per thousand presses: flushes, epoch ends, expiries and bindings
//!   unlearned (`PredictEvents`).
//! * COST, counted: the cells each frame's reconcile asked the host for, the
//!   ops the predictor folded each frame and each press, and the row cells each
//!   press read (`world::Cost`).
//!
//! ## The variants, all the shipping crate
//!
//! * `Baseline`: the edit lane off (`Sim::set_editing`, as aterm-gui's config
//!   switches it: the predictor's `set_editing`, and no row read for an edit
//!   key) and every rule switched off (`Predictor::disable_rule_for_test`):
//!   each rule's off branch is the code before its fix (`Rule`'s docs), so
//!   this is the glyph lane as it shipped before predictive editing began.
//!   On this corpus it is that predictor exactly: a cross-check drove the
//!   crate at 3d4c211fbb, the commit before the plan's first landing, through
//!   this world as aterm-gui drove it then, and every metric below of every
//!   `Baseline` run came out the same (not the churn, which it did not count,
//!   nor the cost: this press still reads the row, which that one did not).
//! * `Phase0`: the edit lane off, every rule in force: the glyph lane as it
//!   ships now. That is not what increment 1 bought alone: the glyph-lane
//!   rules the later increments' reviews added are in force too (THE FLOOR,
//!   `Rule::FloorBelow`, came with the review of increment 4, and so did the
//!   floor row a reconcile reads), so the gap between it and `Baseline` is
//!   every glyph-lane fix since, and the gap between it and `Full` the edit
//!   lane's own.
//! * `Full`: everything, as it ships.
//! * `Sticky`: `Full` with `Rule::ProofsEndWithTheEpoch` off, the class proofs
//!   kept across epochs for the session: the alternative the class LADDER
//!   (a motion or a shift shown only once one of its kind echoed on this line)
//!   was chosen over. It shows each line's first motion a round trip sooner,
//!   and shows a motion a cooked program then echoes as `^[[D`, which the
//!   switch corpus measures (a line at the shell, a line typed into `cat`).
//! * `ShutAtScroll`: `Full` with `Rule::ScrollKeepsFirstKey` off, THE FIRST
//!   KEY's gate shut at every coordinate move as it was before this file
//!   found it should not be; run beside the signed Adaptive runs of `Full`,
//!   for the lines each witnesses (`Stats::witnessed_lines`, printed a run).
//! * `PaintedOverText`: `Full` with `Rule::DarkOverText` off, a guess waiting
//!   on the screen as it was painted over the line's text, as before this file
//!   measured what that cost `Always`; run beside the 32 `Always` runs of
//!   `Full`, for the mispredicted cells and wrong episodes each shows.
//!
//! `Phase0`, `Full`, `ShutAtScroll` and `PaintedOverText` run with the
//! theorem's checker on, so every clause holds on every frame of every run of
//! them; `Baseline` and `Sticky` break clauses by design, and run with it off
//! (`Sim::without_checks`), what they show measured.
//!
//! ## What is asserted
//!
//! 1. `Full`, Adaptive: no wrong episode, at every link, far end and
//!    integration, and none on the switch corpus.
//! 2. `Full` shows at least as many keys before their echo as `Baseline`, in
//!    every run and every key class.
//! 3. `Full`'s count before echo in every run is at least the one recorded at
//!    landing (`FULL_FLOORS`, a ratchet: an improvement re-records it).
//! 4. Under a cooked tty no motion is ever displayed, and under readline's
//!    overwrite mode no shift (`Full`, Adaptive, on the corpus lines that
//!    use them).
//! 5. The cost: at most `2 * cols + 2` observe calls a reconcile, and at most
//!    `64 * 65 / 2` ops folded a frame (`OBSERVE_BOUND`, `FOLD_BOUND`).
//! 6. `Full` witnesses more lines than `ShutAtScroll` in every signed Adaptive
//!    run: the gate kept across a scroll is what witnesses them.
//! 7. No cell is shown wrong for longer than the longest glitch window the
//!    predictor can have (`LIFETIME_BOUND_US`, 2 s), in any run of the main
//!    matrix: every variant, mode and link.
//! 8. `Full`, `Always`, shows no more mispredicted cells and no more wrong
//!    episodes than `Baseline`, the predictor before the plan, in every run,
//!    and fewer mispredicted cells than `PaintedOverText` in every run.
//!
//! And the metric's own negative controls: `Sticky` shows wrong lines on the
//! switch corpus, and `Baseline` does on the main one, wrong episodes and
//! mispredicted cells both, so a count of zero is a measurement, not a metric
//! that cannot see.
//!
//! ## What it found
//!
//! That no line was witnessed at a shell on the bottom row: a coordinate move
//! reset the predictor, and the reset shut THE FIRST KEY's gate, so the scroll
//! every Enter's own output makes there left Home, End, Ctrl-A, Ctrl-E and
//! Delete unplaced for the rest of the session (`ShutAtScroll`: 11 of the 300
//! lines witnessed, all typed before the screen filled, at 1 to 160 ms; 10 at
//! 250 and 320 ms, 7 at 600 ms). A scroll lets no key past the predictor, and
//! the gate now stands across one (`observe_coordinates`,
//! `Rule::ScrollKeepsFirstKey`,
//! `a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate`): 282 of 300
//! are witnessed at 1 to 160 ms, 274 at 250 ms, 260 at 320 ms and 153 at
//! 600 ms, the same for zle and readline (the report prints each run's).
//! And that a reconcile reads two rows, not the one the plan bounded it by:
//! THE FLOOR under the line (`Rule::FloorBelow`) came after the plan.
//! `Baseline` reproduces the region probe's measurement of the predictor as it
//! was (Adaptive, 68.2 / 75.2 / 68.2 % of keys before echo at 30 / 80 / 160 ms,
//! there and here, and zsh's 9 wrong episodes at 80 ms).
//! And, by that cross-check, that `Baseline` was not yet the predictor as it
//! was: it showed 22 wrong episodes at 80 ms, Adaptive, and 188 where that
//! one showed 45 at 30 ms, `Always`. A glyph typed over text (`sudo ` after
//! Home or Ctrl-A) waited on the glyph its press read there, where the
//! predictor before the plan flushed it at the next frame: the wait the
//! review of phase 0 added beside a suggestion, which no rule switched off.
//! It is `Rule::ScreenAsItWas` now, off in `Baseline` with the rest.
//! And that the wait cost `Always` more wrong cells than the predictor before
//! the plan showed, over ssh most (123 against 45 at 30 ms, 152 against 67 at
//! 100 ms): `Always` painted the waiting glyph over the line's text, unshifted,
//! until its echo. The wait itself keeps a run typed a round trip after
//! Ctrl-K, whose glyphs land as appends once Ctrl-K's echo has erased the text
//! their presses read (the six keys readline shows at 250 ms in Adaptive), so
//! it stays; a guess waiting where the line's end is not known now waits off
//! the glass, with every guess typed after it (`Rule::DarkOverText`,
//! `PaintedOverText` beside it), and so does the run's first guess typed over
//! the line's text behind a first glyph that hides nothing (`sudo ` typed on
//! the `s` of `systemctl`): 2 and 35 mispredicted cells over ssh at 30 and
//! 100 ms, and no key fewer before its echo in any run of `Full`.
//!
//! Run: `targo --unverified test -p aterm-predict --test predict_suite editing_win -- --nocapture`;
//! the tables it prints are `docs/DESIGN-predictive-editing-2026-10-20.md`'s.

use std::fmt::Write as _;
use std::time::Duration;

use aterm_core::render::RenderInput;
use aterm_predict::{EditKey, PredictMode, Rule};
use aterm_spec::rng::XorShift;

use crate::world::{Config, Cost, Editor, Host, Key, Keymap, Sim, Stats, Unmodelled};

/// The region design's probe seed and size, which this corpus reproduces.
const CORPUS_SEED: u64 = 0xED17_2026_0001;
const LINES: usize = 300;
/// The keys the region probe's corpus held, and its lines per family: the
/// port's own check that it is the same corpus.
const REGION_KEYS: usize = 6999;
const REGION_FAMILIES: [usize; 10] = [101, 37, 29, 20, 18, 19, 20, 33, 17, 6];

/// The screen: a standard terminal, wide enough for every line of the corpus
/// (the longest, `sudo ` before the `targo` test, is 47 cells with the prompt).
const ROWS: u16 = 24;
const COLS: u16 = 80;

/// THE COST BOUNDS (assertion 5). A reconcile asks the host for at most two
/// rows and two cells: the line's row (the tail a proof stands on, a chain's
/// cells to the margin) and the row under it, THE FLOOR (`Rule::FloorBelow`,
/// every cell its run's press read blank), added by the review of increment 4
/// after the plan bounded a reconcile by one row (`cols + 2`); the shipping
/// predictor reads both on every frame a standing proof is above a floor.
const OBSERVE_BOUND: u64 = 2 * COLS as u64 + 2;
/// The plan's bound on the ops a frame folds: every state of a chain of
/// `EDIT_CHAIN_MAX` (64) ops folded from scratch, `64 * 65 / 2`. The predictor
/// folds each state from the one before, in at most seven passes a frame (the
/// match, the part-way echoes, three in-place worlds counted twice, the
/// retirement, the net-null tail, the transit wait, the overlay), under 800.
const FOLD_BOUND: u64 = 64 * 65 / 2;
/// THE LIFETIME BOUND (assertion 7): the longest glitch window the predictor
/// can have, its own function at the slowest link and the deepest backoff (the
/// crate's documented 2 s ceiling). TIMELINESS says every ghost on glass was
/// typed no longer ago than the window, so a cell shown wrong is wrong at most
/// that long, in every mode and with every rule switched off: the expiry is
/// older than predictive editing.
const LIFETIME_BOUND_US: u64 =
    aterm_predict::compute_glitch_window_ms(Some(u32::MAX), u8::MAX) * 1000;

/// The links measured: a local shell, a LAN, a continent, an ocean, two; and
/// 100 and 250 ms, the links the comparison of the predictor before the plan
/// with the one shipping is quoted at (30, 100 and 250 ms), 250 ms being the
/// glitch window's floor, where a link's echo first arrives later than the
/// shortest window the predictor can have.
const RTTS_MS: [u32; 8] = [1, 30, 80, 100, 160, 250, 320, 600];
/// The links the controls and the switch corpus run on.
const CONTROL_RTTS_MS: [u32; 3] = [80, 160, 320];
/// The link's jitter seed (the region probe's), per round trip.
const LINK_SEED: u64 = 7;

/// What Enter runs: a command printing one line (`Program::run_command`'s
/// choice 8), and, on the switch corpus, `cat` (choice 11), a cooked program
/// reading the tty until Ctrl-C.
const RUNS_A_COMMAND: u32 = 8;
const STARTS_CAT: u32 = 11;
/// What every other key reads: a choice no echo draws a suggestion for.
const NO_SUGGESTION: u32 = 0;

const CMDS: [&str; 20] = [
    "git status",
    "git diff --stat",
    "git commit -m \"fix the typo\"",
    "git push origin main",
    "ls -la /tmp",
    "cd ~/src/aterm",
    "grep -rn predict crates",
    "targo --unverified test -p aterm-predict",
    "make -j8 all",
    "docker ps -a",
    "kubectl get pods -n prod",
    "vim notes.md",
    "tail -f /var/log/system.log",
    "find . -name '*.rs' | wc -l",
    "ssh build-box uptime",
    "cat Cargo.toml",
    "rg -n coordinate_generation",
    "du -sh target",
    "echo $PATH",
    "history | tail -20",
];
/// The words Tab completes.
const VOCAB: [&str; 7] = [
    "git", "grep", "kubectl", "docker", "targo", "tail", "history",
];
const TYPOS: [char; 7] = ['q', 'w', 'x', 'z', 'p', 'k', 'm'];
const FAMILY_WEIGHTS: [u64; 10] = [35, 15, 12, 6, 5, 6, 4, 10, 5, 2];
const FAMILY_NAMES: [&str; 10] = [
    "plain",
    "typo-end",
    "typo-back",
    "prefix",
    "retype",
    "insert-flag",
    "delete-fwd",
    "tab",
    "history",
    "word-motion",
];
/// The families whose lines move the cursor or edit inside the text: the
/// corpus the cooked and overwrite controls type.
const EDITING_FAMILIES: [usize; 5] = [2, 3, 5, 6, 8];

/// A key of the corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    G(char),
    Backspace,
    Left,
    Right,
    Home,
    End,
    CtrlA,
    CtrlE,
    Delete,
    CtrlK,
    Tab,
    Up,
    AltB,
    Enter,
    /// Ctrl-C: how a line typed into `cat` ends (the switch corpus).
    CtrlC,
}

impl Press {
    /// The press the world's host sees.
    fn key(self) -> Key {
        match self {
            Self::G(c) => Key::Glyph(c),
            Self::Backspace => Key::Backspace,
            Self::Left => Key::Left,
            Self::Right => Key::Right,
            Self::Home => Key::Learned(EditKey::Home),
            Self::End => Key::Learned(EditKey::End),
            Self::CtrlA => Key::Learned(EditKey::CtrlA),
            Self::CtrlE => Key::Learned(EditKey::CtrlE),
            Self::Delete => Key::Learned(EditKey::Delete),
            Self::CtrlK => Key::Unmodelled(Unmodelled::KillToEnd),
            Self::Tab => Key::Unmodelled(Unmodelled::Tab),
            Self::Up => Key::Unmodelled(Unmodelled::Up),
            Self::AltB => Key::Unmodelled(Unmodelled::WordLeft),
            Self::Enter => Key::Enter,
            Self::CtrlC => Key::Unmodelled(Unmodelled::Interrupt),
        }
    }

    fn terminates(self) -> bool {
        matches!(self, Self::Enter | Self::CtrlC)
    }
}

/// One press of the corpus: the key, the pause before it, and whether it is
/// its line's first (the typist waits for the prompt, a round trip more).
#[derive(Clone, Copy, Debug)]
struct Stroke {
    key: Press,
    gap_ms: u32,
    first: bool,
}

/// One line of the corpus: its family and its presses, the terminator last.
#[derive(Clone, Debug)]
struct Line {
    family: usize,
    strokes: Vec<Stroke>,
}

/// The typist: the generator's state and the line being typed.
struct Typist<'r> {
    rng: &'r mut XorShift,
    strokes: Vec<Stroke>,
}

impl Typist<'_> {
    fn push(&mut self, key: Press, gap_ms: u32) {
        let first = self.strokes.is_empty();
        self.strokes.push(Stroke { key, gap_ms, first });
    }

    fn draw(&mut self, base: u32, spread: u64) -> u32 {
        base + u32::try_from(self.rng.below(spread)).unwrap_or(0)
    }

    fn typing(&mut self) -> u32 {
        self.draw(60, 140)
    }

    fn notice(&mut self) -> u32 {
        self.draw(350, 850)
    }

    fn repeat(&mut self) -> u32 {
        self.draw(70, 50)
    }

    fn type_str(&mut self, s: &[char]) {
        for &c in s {
            let gap = self.typing();
            self.push(Press::G(c), gap);
        }
    }

    /// `n` presses of `key`, the first after `gap`, the rest at the arrow's
    /// repeat.
    fn repeated(&mut self, key: Press, n: usize, gap: u32) {
        let mut gap = gap;
        for _ in 0..n {
            self.push(key, gap);
            gap = self.repeat();
        }
    }

    /// One line of family `family` typing `cmd`, in the region probe's order
    /// of draws (so the corpus is its corpus).
    fn line(&mut self, family: usize, cmd: &[char]) {
        let start = self.draw(300, 900);
        let len = cmd.len();
        let at = |i: usize| cmd.get(i).copied().unwrap_or(' ');
        let from = |i: usize| cmd.get(i..).unwrap_or_default();
        match family {
            // A typo at the end, rubbed out with Backspace and the rest typed.
            1 => {
                let p = self.rng.range(len / 2, len);
                self.push(Press::G(at(0)), start);
                self.type_str(cmd.get(1..p).unwrap_or_default());
                let more = self.rng.range(0, 3).min(len - p);
                let gap = self.typing();
                let typo = *self.rng.pick(&TYPOS);
                self.push(Press::G(typo), gap);
                self.type_str(cmd.get(p..p + more).unwrap_or_default());
                let gap = self.notice();
                self.push(Press::Backspace, gap);
                for _ in 0..more {
                    let gap = self.repeat();
                    self.push(Press::Backspace, gap);
                }
                self.type_str(from(p));
            }
            // A typo several glyphs back: `←` to it, rubbed out, retyped, and
            // left by End, Ctrl-E or `→` back to the end.
            2 => {
                let p = self.rng.range(1, len - 2);
                self.push(Press::G(at(0)), start);
                for (i, &c) in cmd.iter().enumerate().skip(1) {
                    let gap = self.typing();
                    let c = if i == p { *self.rng.pick(&TYPOS) } else { c };
                    self.push(Press::G(c), gap);
                }
                let notice = self.notice();
                let lefts = len - p - 1;
                self.repeated(Press::Left, lefts, notice);
                let gap = self.typing();
                self.push(Press::Backspace, if lefts == 0 { notice } else { gap });
                let gap = self.typing();
                self.push(Press::G(at(p)), gap);
                match self.rng.below(3) {
                    0 => {
                        let gap = self.typing();
                        self.push(Press::End, gap);
                    }
                    1 => {
                        let gap = self.typing();
                        self.push(Press::CtrlE, gap);
                    }
                    _ => {
                        let gap = self.typing();
                        self.repeated(Press::Right, lefts, gap);
                    }
                }
            }
            // A prefix: Home or Ctrl-A, and `sudo ` in front.
            3 => {
                self.push(Press::G(at(0)), start);
                self.type_str(from(1));
                let gap = self.notice();
                let home = if self.rng.below(2) == 0 {
                    Press::Home
                } else {
                    Press::CtrlA
                };
                self.push(home, gap);
                self.type_str(&"sudo ".chars().collect::<Vec<char>>());
            }
            // Retyped: Ctrl-A Ctrl-Press, and another command.
            4 => {
                self.push(Press::G(at(0)), start);
                self.type_str(from(1));
                let gap = self.notice();
                self.push(Press::CtrlA, gap);
                let gap = self.repeat();
                self.push(Press::CtrlK, gap);
                let other: Vec<char> = self.rng.pick(&CMDS).chars().collect();
                self.type_str(&other);
            }
            // A flag inserted after the first word.
            5 => {
                let word = cmd.iter().position(|&c| c == ' ').unwrap_or(len);
                self.push(Press::G(at(0)), start);
                self.type_str(from(1));
                let notice = self.notice();
                let lefts = len - word.min(len);
                if lefts != 0 {
                    self.repeated(Press::Left, lefts, notice);
                    self.type_str(&" -v".chars().collect::<Vec<char>>());
                }
            }
            // A junk prefix removed: Home, Delete, Delete, End.
            6 => {
                self.push(Press::G('x'), start);
                let gap = self.typing();
                self.push(Press::G('x'), gap);
                self.type_str(cmd);
                let gap = self.notice();
                self.push(Press::Home, gap);
                let gap = self.repeat();
                self.push(Press::Delete, gap);
                let gap = self.repeat();
                self.push(Press::Delete, gap);
                let gap = self.typing();
                self.push(Press::End, gap);
            }
            // The first word completed with Tab, where the table has it.
            7 => {
                let word = cmd.iter().position(|&c| c == ' ').unwrap_or(len);
                let w: String = cmd.get(..word).unwrap_or_default().iter().collect();
                self.push(Press::G(at(0)), start);
                if VOCAB.contains(&w.as_str()) && word > 3 {
                    for i in 1..3 {
                        let gap = self.typing();
                        self.push(Press::G(at(i)), gap);
                    }
                    let gap = self.typing();
                    self.push(Press::Tab, gap);
                    self.type_str(from((word + 1).min(len)));
                } else {
                    self.type_str(from(1));
                }
            }
            // A line recalled with Up and edited inside.
            8 => {
                self.push(Press::Up, start);
                let notice = self.notice();
                let m = self.rng.range(2, 6);
                self.repeated(Press::Left, m, notice);
                let gap = self.typing();
                self.push(Press::Backspace, gap);
                let gap = self.typing();
                self.push(Press::G('2'), gap);
            }
            // A word motion (never modelled) and a glyph.
            9 => {
                self.push(Press::G(at(0)), start);
                self.type_str(from(1));
                let gap = self.notice();
                self.push(Press::AltB, gap);
                let gap = self.typing();
                self.push(Press::G('x'), gap);
            }
            // Plain.
            _ => {
                self.push(Press::G(at(0)), start);
                self.type_str(from(1));
            }
        }
        let gap = self.typing() + 80;
        self.push(Press::Enter, gap);
    }
}

/// THE CORPUS: [`LINES`] lines from [`CORPUS_SEED`].
fn corpus() -> Vec<Line> {
    let mut rng = XorShift::new(CORPUS_SEED);
    let total: u64 = FAMILY_WEIGHTS.iter().sum();
    (0..LINES)
        .map(|_| {
            let mut pick = rng.below(total);
            let family = FAMILY_WEIGHTS
                .iter()
                .position(|&w| {
                    if pick < w {
                        return true;
                    }
                    pick -= w;
                    false
                })
                .unwrap_or(0);
            let cmd: Vec<char> = rng.pick(&CMDS).chars().collect();
            let mut typist = Typist {
                rng: &mut rng,
                strokes: Vec::new(),
            };
            typist.line(family, &cmd);
            Line {
                family,
                strokes: typist.strokes,
            }
        })
        .collect()
}

/// The screen a key's output settles to: the cursor's row, its text, the
/// cursor, and the far end's buffer (its length and the cursor's index in it).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Truth {
    row: u16,
    text: Vec<char>,
    cursor: (u16, u16),
    buffer: (usize, usize),
}

/// A corpus as one far end types it: the presses, what each terminator runs,
/// the bytes each key's output takes, and the TRUTH after each key.
struct Script {
    strokes: Vec<Stroke>,
    /// The line each press belongs to, and each line's first press.
    line_of: Vec<usize>,
    line_start: Vec<usize>,
    /// The press index of each key's line terminator.
    line_end: Vec<usize>,
    choices: Vec<u32>,
    bytes: Vec<usize>,
    initial: Truth,
    truth: Vec<Truth>,
    /// Keys the metrics count: no terminator, and a change of the truth.
    counted: Vec<bool>,
    /// Each counted key's class (`CLASSES`).
    class: Vec<usize>,
}

/// The key classes the metrics are kept per.
const CLASSES: [&str; 8] = [
    "glyph-end",
    "glyph-mid",
    "bksp-end",
    "bksp-mid",
    "motion",
    "line-edge",
    "delete",
    "unmodelled",
];

/// The class of `key` pressed with the far end's buffer `(len, cur)`.
fn class_of(key: Press, (len, cur): (usize, usize)) -> usize {
    match key {
        Press::G(_) if cur == len => 0,
        Press::G(_) => 1,
        Press::Backspace if cur == len => 2,
        Press::Backspace => 3,
        Press::Left | Press::Right => 4,
        Press::Home | Press::End | Press::CtrlA | Press::CtrlE => 5,
        Press::Delete => 6,
        Press::CtrlK | Press::Tab | Press::Up | Press::AltB | Press::Enter | Press::CtrlC => 7,
    }
}

/// A far end's configuration: the world's host, faithful, in `mode`.
fn config(editor: Editor, integration: bool, mode: PredictMode) -> Config {
    Config {
        mode,
        // A local shell under the integration; ssh without it.
        remote: !integration,
        host: Host::Faithful,
        forced_wrap: false,
        wide: false,
        rprompt: false,
        editor,
        bidi: false,
        integration,
        keymap: Keymap::Canonical,
    }
}

/// The row `row` of `frame`, `cols` wide (a cell past a trimmed row is blank).
fn row_text(frame: &RenderInput, row: u16, cols: usize, out: &mut Vec<char>) {
    out.clear();
    let cells = frame
        .cells
        .get(usize::from(row))
        .map_or(&[][..], Vec::as_slice);
    out.extend((0..cols).map(|c| cells.get(c).map_or(' ', |cell| cell.ch)));
}

/// Type `lines` into `editor` with prediction off, every key read and its
/// output delivered whole before the next: the truth after each key. On the
/// switch corpus (`switch`) every other line's Enter starts `cat`, and the line
/// after it, typed into `cat`, ends with Ctrl-C.
fn script(lines: &[Line], editor: Editor, integration: bool, switch: bool) -> Script {
    let mut strokes = Vec::new();
    let mut line_of = Vec::new();
    let mut line_start = Vec::new();
    let mut choices = Vec::new();
    for (l, line) in lines.iter().enumerate() {
        line_start.push(strokes.len());
        let into_cat = switch && l % 2 == 1;
        for s in &line.strokes {
            let key = if s.key == Press::Enter && into_cat {
                Press::CtrlC
            } else {
                s.key
            };
            strokes.push(Stroke { key, ..*s });
            line_of.push(l);
            // The choice is the terminator's alone: a glyph's echo reads it too
            // (`Program::suggest_after`, an autosuggestion now and then), and
            // this corpus draws no decoration, which the wrong-episode metric
            // does not excuse.
            choices.push(match key {
                Press::Enter if switch && l % 2 == 0 => STARTS_CAT,
                Press::Enter => RUNS_A_COMMAND,
                _ => NO_SUGGESTION,
            });
        }
    }
    let mut line_end = vec![strokes.len(); strokes.len()];
    let mut next = strokes.len();
    for (i, s) in strokes.iter().enumerate().rev() {
        if s.key.terminates() {
            next = i;
        }
        if let Some(end) = line_end.get_mut(i) {
            *end = next;
        }
    }
    let mut sim = Sim::sized(config(editor, integration, PredictMode::Off), ROWS, COLS);
    sim.corpus_shell(&VOCAB);
    sim.fresh_prompt().expect("prediction off breaks no clause");
    let cols = usize::from(COLS);
    let mut text = Vec::new();
    let mut snapshot = |sim: &Sim| {
        let cursor = sim.term().cursor();
        row_text(sim.screen(), cursor.row, cols, &mut text);
        Truth {
            row: cursor.row,
            text: text.clone(),
            cursor: (cursor.row, cursor.col),
            buffer: sim.buffer(),
        }
    };
    let initial = snapshot(&sim);
    let mut bytes = Vec::with_capacity(strokes.len());
    let mut truth: Vec<Truth> = Vec::with_capacity(strokes.len());
    for (s, &choice) in strokes.iter().zip(&choices) {
        sim.press_and_draw(s.key.key())
            .expect("prediction off breaks no clause");
        let n = sim.read_key(choice);
        sim.deliver_and_draw(n)
            .expect("prediction off breaks no clause");
        bytes.push(n);
        truth.push(snapshot(&sim));
    }
    let mut counted = Vec::with_capacity(strokes.len());
    let mut class = Vec::with_capacity(strokes.len());
    for (i, s) in strokes.iter().enumerate() {
        let before = i
            .checked_sub(1)
            .and_then(|j| truth.get(j))
            .unwrap_or(&initial);
        let after = truth.get(i).unwrap_or(&initial);
        counted.push(!s.key.terminates() && after != before);
        class.push(class_of(s.key, before.buffer));
    }
    Script {
        strokes,
        line_of,
        line_start,
        line_end,
        choices,
        bytes,
        initial,
        truth,
        counted,
        class,
    }
}

/// One piece of a key's output in flight: when it lands and how many bytes.
#[derive(Clone, Copy, Debug)]
struct Piece {
    at: Duration,
    len: usize,
}

/// THE LINK for a round trip of `rtt_ms`: when each key is pressed, when the
/// far end reads it, and the pieces its output comes back in, each in order.
struct Timeline {
    press: Vec<Duration>,
    arrive: Vec<Duration>,
    pieces: Vec<Piece>,
    /// When each key's last piece lands.
    landed: Vec<Duration>,
}

fn timeline(script: &Script, rtt_ms: u32) -> Timeline {
    let mut rng = XorShift::new(LINK_SEED ^ u64::from(rtt_ms).wrapping_mul(0x9E37_79B9));
    let half_us = u64::from(rtt_ms) * 500;
    // Half a round trip, within ±15 %.
    let jitter = |rng: &mut XorShift| half_us * (85_000 + rng.below(1000) * 30) / 100_000;
    let rtt = Duration::from_millis(u64::from(rtt_ms));
    let n = script.strokes.len();
    let (mut press, mut arrive, mut deliver) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let (mut t, mut arr, mut del) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    for s in &script.strokes {
        t +=
            Duration::from_millis(u64::from(s.gap_ms)) + if s.first { rtt } else { Duration::ZERO };
        press.push(t);
        arr = arr.max(t + Duration::from_micros(jitter(&mut rng)));
        del = del.max(arr + Duration::from_micros(jitter(&mut rng)));
        arrive.push(arr);
        deliver.push(del);
    }
    let mut pieces = Vec::new();
    let mut landed = Vec::with_capacity(n);
    let mut last = Duration::ZERO;
    for (&len, &at) in script.bytes.iter().zip(&deliver) {
        let at = at.max(last);
        if len >= 2 && rng.below(2) == 0 {
            let split = 1 + rng.index(len - 1);
            let second = at + Duration::from_millis(1 + rng.below(6));
            pieces.push(Piece { at, len: split });
            pieces.push(Piece {
                at: second,
                len: len - split,
            });
            last = second;
        } else {
            if len > 0 {
                pieces.push(Piece { at, len });
            }
            last = at;
        }
        landed.push(last);
    }
    Timeline {
        press,
        arrive,
        pieces,
        landed,
    }
}

/// The predictor a run measures (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    Baseline,
    Phase0,
    Full,
    Sticky,
    /// `Full` with `Rule::ScrollKeepsFirstKey` off: THE FIRST KEY's gate shut
    /// at every coordinate move, as before this file found it should not be.
    /// Run beside `Full` at the signed runs only, for the lines witnessed.
    ShutAtScroll,
    /// `Full` with `Rule::DarkOverText` off: a guess waiting on the screen as
    /// it was painted over the line's text, as before this file measured what
    /// that cost `Always`. Run beside `Full` at the `Always` runs only, for the
    /// mispredicted cells and wrong episodes each shows.
    PaintedOverText,
}

/// The variants of the main matrix (`ShutAtScroll` runs beside it).
const VARIANTS: [Variant; 4] = [
    Variant::Baseline,
    Variant::Phase0,
    Variant::Full,
    Variant::Sticky,
];

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Phase0 => "phase0",
            Self::Full => "full",
            Self::Sticky => "sticky",
            Self::ShutAtScroll => "shut-at-scroll",
            Self::PaintedOverText => "painted-over-text",
        }
    }

    /// Configure `sim`'s predictor (and checker) before the first key. The
    /// edit lane is switched off as aterm-gui's config switches it, through the
    /// host (`Sim::set_editing`), which then reads no row for an edit key.
    fn prepare(self, sim: &mut Sim) {
        match self {
            Self::Baseline => {
                sim.set_editing(false);
                let p = sim.predictor_mut();
                for &rule in Rule::ALL {
                    p.disable_rule_for_test(rule);
                }
                sim.without_checks();
            }
            Self::Phase0 => sim.set_editing(false),
            Self::Full => {}
            Self::Sticky => {
                sim.predictor_mut()
                    .disable_rule_for_test(Rule::ProofsEndWithTheEpoch);
                sim.without_checks();
            }
            // Less is shown with the gate shut, so every clause still holds.
            Self::ShutAtScroll => sim
                .predictor_mut()
                .disable_rule_for_test(Rule::ScrollKeepsFirstKey),
            // More is painted, but only in `Always`, which claims no fidelity.
            Self::PaintedOverText => sim
                .predictor_mut()
                .disable_rule_for_test(Rule::DarkOverText),
        }
    }
}

/// One run's numbers.
#[derive(Clone, Debug, Default)]
struct Tally {
    keys: u64,
    presses: u64,
    before_echo: u64,
    at_press: u64,
    class_keys: [u64; 8],
    class_before: [u64; 8],
    class_at_press: [u64; 8],
    wrong_runs: u64,
    wrong_us: u64,
    max_wrong_us: u64,
    max_wrong_cells: u64,
    wrong_cell_us: u64,
    /// MISPREDICTED CELLS: the times a cell went wrong on glass, and how long
    /// each stayed wrong (`Meter::open_cells`), at the median, the 95th and 99th
    /// percentiles and the longest.
    wrong_cells: u64,
    cell_life_p50_us: u64,
    cell_life_p95_us: u64,
    cell_life_p99_us: u64,
    cell_life_max_us: u64,
    p50_us: u64,
    p95_us: u64,
    flushes: u64,
    epoch_ends: u64,
    expiries: u64,
    unlearned: u64,
    cost: Cost,
    stats: Stats,
}

/// What a run is: the far end, its integration, the link, the mode and the
/// variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    editor: Editor,
    integration: bool,
    rtt_ms: u32,
    mode: PredictMode,
    variant: Variant,
}

impl Run {
    fn label(self) -> String {
        format!(
            "{:<8} {:<6} rtt={:>3} {:<8} {:<8}",
            format!("{:?}", self.editor),
            if self.integration { "signed" } else { "remote" },
            self.rtt_ms,
            format!("{:?}", self.mode),
            self.variant.name()
        )
    }
}

/// Whether row `text` with the cursor drawn at `cursor` shows `t`: its text,
/// and its cursor where the cursor is on its row (a cursor on another row, just
/// after a terminator, says nothing about this one).
fn shows(text: &[char], cursor: (u16, u16), t: &Truth) -> bool {
    text == t.text.as_slice() && (cursor.0 != t.row || cursor == t.cursor)
}

/// The measurement's view of a frame: per row asked for, `A`'s text and `D`'s.
struct View {
    rows: Vec<(u16, Vec<char>, Vec<char>)>,
}

impl View {
    fn row(&mut self, sim: &Sim, row: u16) -> (&[char], &[char]) {
        let i = match self.rows.iter().position(|(r, _, _)| *r == row) {
            Some(i) => i,
            None => {
                let cols = usize::from(COLS);
                let mut a = Vec::new();
                row_text(sim.screen(), row, cols, &mut a);
                let mut d = Vec::new();
                match sim.on_glass() {
                    Some(glass) => row_text(&glass.frame, row, cols, &mut d),
                    None => d.clone_from(&a),
                }
                self.rows.push((row, a, d));
                self.rows.len() - 1
            }
        };
        let (_, a, d) = &self.rows[i];
        (a, d)
    }
}

/// The metrics' state across a run's frames.
struct Meter<'s> {
    script: &'s Script,
    timeline: &'s Timeline,
    pressed: usize,
    displayed: Vec<Option<Duration>>,
    echoed: Vec<Option<Duration>>,
    at_press: Vec<bool>,
    /// The oldest key still to be echoed, as far as the metrics care.
    lo: usize,
    /// The cells wrong at the last frame, while an episode runs.
    wrong: Option<u64>,
    wrong_run: Duration,
    /// Each cell wrong at the last frame, and the frame it went wrong at: a
    /// cell's LIFETIME runs from the first frame that shows it wrong to the
    /// first that does not, whatever glyphs it shows wrong meanwhile (a cell
    /// wrong under one guess and then under the next is one wrong cell to the
    /// eye). A row shown wrong by its drawn cursor alone counts the cursor's
    /// cell. Few at once: a frame shows at most a line's worth.
    open_cells: Vec<((u16, u16), Duration)>,
    /// The lifetimes of the cells that went right again, in microseconds.
    cell_lives: Vec<u64>,
    last: Duration,
    tally: Tally,
}

/// `d` in microseconds, as the tallies keep injected time.
fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// The `q`th percentile of `sorted` (ascending), nearest rank rounded up: the
/// median and the 95th percentile the perceived latency always quoted, and the
/// lifetimes' 99th.
fn percentile(sorted: &[u64], q: usize) -> u64 {
    sorted
        .get((sorted.len().saturating_sub(1) * q).div_ceil(100))
        .copied()
        .unwrap_or(0)
}

impl Meter<'_> {
    /// The states the line on row `row` passes through, as far as it has been
    /// typed: from the screen its first key met (the terminator before it, or
    /// the first prompt) to the newest key pressed whose truth is on that row,
    /// searched back no further than the line before the newest key's.
    fn line_states(&self, row: u16) -> (Vec<&Truth>, &Truth) {
        let s = self.script;
        let newest = self.pressed.checked_sub(1);
        let floor = newest.and_then(|k| s.line_of.get(k)).map_or(0, |&l| {
            s.line_start.get(l.saturating_sub(1)).copied().unwrap_or(0)
        });
        let m = newest.and_then(|k| {
            (floor..=k)
                .rev()
                .find(|&j| s.truth.get(j).is_some_and(|t| t.row == row))
        });
        let Some(m) = m else {
            return (vec![&s.initial], &s.initial);
        };
        let start = s
            .line_of
            .get(m)
            .and_then(|&l| s.line_start.get(l))
            .copied()
            .unwrap_or(0);
        let mut states: Vec<&Truth> = Vec::with_capacity(m + 2 - start);
        states.push(
            start
                .checked_sub(1)
                .and_then(|j| s.truth.get(j))
                .unwrap_or(&s.initial),
        );
        states.extend(s.truth.get(start..=m).unwrap_or_default());
        (states, s.truth.get(m).unwrap_or(&s.initial))
    }

    /// Measure the frame `sim` just drew, at `now`; `press` names the key
    /// whose press drew it, if one did.
    fn frame(&mut self, sim: &Sim, now: Duration, press: Option<usize>) {
        // The episode in progress, integrated up to this frame.
        let dt = now.saturating_sub(self.last);
        if let Some(cells) = self.wrong {
            let us = u64::try_from(dt.as_micros()).unwrap_or(u64::MAX);
            self.tally.wrong_us += us;
            self.tally.wrong_cell_us += us * cells;
            self.wrong_run += dt;
        }
        self.last = now;
        let engine = sim.term().cursor();
        let engine = (engine.row, engine.col);
        let drawn = sim.on_glass().and_then(|g| g.cursor).unwrap_or(engine);
        let mut view = View { rows: Vec::new() };
        let s = self.script;
        // WRONG: a painted row showing no state of its line.
        let mut rows: Vec<u16> = sim.on_glass().map(|g| g.rows.clone()).unwrap_or_default();
        rows.push(
            self.pressed
                .checked_sub(1)
                .and_then(|k| s.truth.get(k))
                .unwrap_or(&s.initial)
                .row,
        );
        rows.push(engine.0);
        rows.sort_unstable();
        rows.dedup();
        let mut wrong_cells = None::<u64>;
        let mut cells_now: Vec<(u16, u16)> = Vec::new();
        for &row in &rows {
            let (a, d) = view.row(sim, row);
            let painted = a != d || (drawn != engine && drawn.0 == row);
            if !painted {
                continue;
            }
            let (states, target) = self.line_states(row);
            if states.iter().any(|t| shows(d, drawn, t)) {
                continue;
            }
            let before = cells_now.len();
            cells_now.extend(
                a.iter()
                    .zip(d)
                    .zip(&target.text)
                    .enumerate()
                    .filter(|&(_, ((a, d), t))| d != a && d != t)
                    .map(|(c, _)| (row, u16::try_from(c).unwrap_or(u16::MAX))),
            );
            if cells_now.len() == before {
                // Every painted cell is one of the line's own glyphs: the drawn
                // cursor is what no state of the line shows.
                cells_now.push((row, if drawn.0 == row { drawn.1 } else { 0 }));
            }
            let cells = (cells_now.len() - before) as u64;
            wrong_cells = Some(wrong_cells.unwrap_or(0) + cells);
        }
        // MISPREDICTED CELLS: a cell wrong at the last frame and right at this
        // one lived from the frame it went wrong at to this one; a cell wrong
        // now and not at the last frame is a mispredicted cell shown.
        let lives = &mut self.cell_lives;
        self.open_cells.retain(|&(cell, since)| {
            let open = cells_now.contains(&cell);
            if !open {
                lives.push(micros(now.saturating_sub(since)));
            }
            open
        });
        for cell in cells_now {
            if !self.open_cells.iter().any(|&(c, _)| c == cell) {
                self.open_cells.push((cell, now));
                self.tally.wrong_cells += 1;
            }
        }
        match (wrong_cells, self.wrong) {
            (Some(cells), prev) => {
                if prev.is_none() {
                    self.tally.wrong_runs += 1;
                    self.wrong_run = Duration::ZERO;
                }
                self.tally.max_wrong_cells = self.tally.max_wrong_cells.max(cells);
                self.wrong = Some(cells);
            }
            (None, Some(_)) => {
                self.end_episode();
            }
            (None, None) => {}
        }
        // AT PRESS: the press's own frame already shows the key's truth.
        if let Some(k) = press
            && s.counted.get(k) == Some(&true)
            && let Some(t) = s.truth.get(k)
        {
            let (_, d) = view.row(sim, t.row);
            if d == t.text.as_slice()
                && drawn == t.cursor
                && let Some(hit) = self.at_press.get_mut(k)
            {
                *hit = true;
            }
        }
        // BEFORE ECHO: the first frame `D` shows a state at or past each key
        // in flight, and the first `A` does.
        for i in self.lo..self.pressed {
            if s.counted.get(i) != Some(&true) {
                continue;
            }
            let Some(t) = s.truth.get(i) else {
                continue;
            };
            let hi = self.pressed.min(s.line_end.get(i).copied().unwrap_or(0));
            let ahead = s.truth.get(i..hi).unwrap_or_default();
            let (a, d) = view.row(sim, t.row);
            if self.displayed.get(i) == Some(&None) && ahead.iter().any(|t| shows(d, drawn, t)) {
                self.displayed[i] = Some(now);
            }
            if self.echoed.get(i) == Some(&None) && ahead.iter().any(|t| shows(a, engine, t)) {
                self.echoed[i] = Some(now);
            }
        }
        while self.lo < self.pressed
            && (s.counted.get(self.lo) != Some(&true)
                || self.echoed.get(self.lo).is_some_and(Option::is_some)
                || self
                    .timeline
                    .landed
                    .get(self.lo)
                    .is_some_and(|&t| t + Duration::from_secs(3) < now))
        {
            self.lo += 1;
        }
    }

    fn end_episode(&mut self) {
        let us = u64::try_from(self.wrong_run.as_micros()).unwrap_or(u64::MAX);
        self.tally.max_wrong_us = self.tally.max_wrong_us.max(us);
        self.wrong = None;
    }

    /// The per-key numbers, once every key has settled.
    fn finish(mut self) -> Tally {
        if self.wrong.is_some() {
            self.end_episode();
        }
        // A cell still wrong at the run's last frame lived until it.
        let last = self.last;
        self.cell_lives.extend(
            self.open_cells
                .drain(..)
                .map(|(_, since)| micros(last.saturating_sub(since))),
        );
        self.cell_lives.sort_unstable();
        self.tally.cell_life_p50_us = percentile(&self.cell_lives, 50);
        self.tally.cell_life_p95_us = percentile(&self.cell_lives, 95);
        self.tally.cell_life_p99_us = percentile(&self.cell_lives, 99);
        self.tally.cell_life_max_us = self.cell_lives.last().copied().unwrap_or(0);
        let s = self.script;
        let mut latency = Vec::new();
        for i in 0..s.strokes.len() {
            if s.counted.get(i) != Some(&true) {
                continue;
            }
            let class = s.class.get(i).copied().unwrap_or(7);
            let displayed = self.displayed.get(i).copied().flatten();
            let echoed = self.echoed.get(i).copied().flatten();
            self.tally.keys += 1;
            if let Some(n) = self.tally.class_keys.get_mut(class) {
                *n += 1;
            }
            let before = match (displayed, echoed) {
                (Some(d), Some(e)) => d < e,
                (Some(_), None) => true,
                _ => false,
            };
            if before {
                self.tally.before_echo += 1;
                if let Some(n) = self.tally.class_before.get_mut(class) {
                    *n += 1;
                }
            }
            if self.at_press.get(i) == Some(&true) {
                self.tally.at_press += 1;
                if let Some(n) = self.tally.class_at_press.get_mut(class) {
                    *n += 1;
                }
            }
            let seen = match (displayed, echoed) {
                (Some(d), Some(e)) => d.min(e),
                (d, e) => d
                    .or(e)
                    .unwrap_or_else(|| self.timeline.landed.get(i).copied().unwrap_or_default()),
            };
            let pressed = self.timeline.press.get(i).copied().unwrap_or_default();
            latency.push(u64::try_from(seen.saturating_sub(pressed).as_micros()).unwrap_or(0));
        }
        latency.sort_unstable();
        self.tally.p50_us = percentile(&latency, 50);
        self.tally.p95_us = percentile(&latency, 95);
        self.tally
    }
}

/// Run `script` over `timeline` as `run` says: the theorem's clauses checked on
/// every frame for a variant with every rule in force. `Err` names a clause a
/// frame broke.
fn measure(script: &Script, timeline: &Timeline, run: Run) -> Result<Tally, String> {
    let mut sim = Sim::sized(config(run.editor, run.integration, run.mode), ROWS, COLS);
    sim.corpus_shell(&VOCAB);
    run.variant.prepare(&mut sim);
    let n = script.strokes.len();
    let mut meter = Meter {
        script,
        timeline,
        pressed: 0,
        displayed: vec![None; n],
        echoed: vec![None; n],
        at_press: vec![false; n],
        lo: 0,
        wrong: None,
        wrong_run: Duration::ZERO,
        open_cells: Vec::new(),
        cell_lives: Vec::new(),
        last: Duration::ZERO,
        tally: Tally::default(),
    };
    let broke = |what: &str, k: usize, v: crate::world::Violation| {
        format!(
            "{}: {what} at key {k} {:?}: `{}`: {}",
            run.label(),
            script.strokes.get(k).map(|s| s.key),
            v.clause,
            v.detail
        )
    };
    sim.fresh_prompt()
        .map_err(|v| broke("the first prompt", 0, v))?;
    let start = sim.now();
    meter.frame(&sim, Duration::ZERO, None);
    let (mut pi, mut ai, mut di) = (0usize, 0usize, 0usize);
    // The far end reads keys in order; each read is a key whose output the
    // pieces then carry, `di` counting pieces.
    let mut guard = 0u64;
    loop {
        guard += 1;
        assert!(
            guard < 10_000_000,
            "{}: the event loop never drained",
            run.label()
        );
        // The next event, deliveries first, then reads, then presses at the same
        // instant; a deadline of the predictor's 1 ms late, before any of them.
        let next = [
            timeline.pieces.get(di).map(|p| (p.at, 0)),
            timeline.arrive.get(ai).map(|&t| (t, 1)),
            timeline.press.get(pi).map(|&t| (t, 2)),
        ]
        .into_iter()
        .flatten()
        .min();
        let deadline = sim
            .next_deadline()
            .map(|d| d.saturating_duration_since(start) + Duration::from_millis(1));
        if let Some(due) = deadline.filter(|&due| next.is_none_or(|(t, _)| due < t)) {
            if sim
                .advance_to(start + due)
                .map_err(|v| broke("the expiry sweep", pi, v))?
            {
                meter.frame(&sim, due, None);
            }
            continue;
        }
        let Some((at, what)) = next else {
            break;
        };
        if sim
            .advance_to(start + at)
            .map_err(|v| broke("the expiry sweep", pi, v))?
        {
            meter.frame(&sim, at, None);
        }
        match what {
            0 => {
                let len = timeline.pieces.get(di).map_or(0, |p| p.len);
                sim.deliver_and_draw(len)
                    .map_err(|v| broke("a delivery", ai.saturating_sub(1), v))?;
                di += 1;
                meter.frame(&sim, at, None);
            }
            1 => {
                let choice = script.choices.get(ai).copied().unwrap_or(0);
                let n = sim.read_key(choice);
                debug_assert_eq!(
                    Some(&n),
                    script.bytes.get(ai),
                    "the far end is deterministic"
                );
                ai += 1;
            }
            _ => {
                let key = script.strokes.get(pi).map_or(Press::Enter, |s| s.key);
                sim.press_and_draw(key.key())
                    .map_err(|v| broke("a press", pi, v))?;
                pi += 1;
                meter.pressed = pi;
                meter.frame(&sim, at, Some(pi - 1));
            }
        }
    }
    let events = sim.predictor_mut().events_for_test();
    let mut tally = meter.finish();
    tally.presses = n as u64;
    tally.flushes = events.flushes;
    tally.epoch_ends = events.epoch_ends;
    tally.expiries = events.expiries;
    tally.unlearned = events.bindings_unlearned;
    tally.cost = sim.cost();
    tally.stats = sim.stats();
    Ok(tally)
}

fn pct(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        100.0 * a as f64 / b as f64
    }
}

fn per_k(a: u64, presses: u64) -> f64 {
    if presses == 0 {
        0.0
    } else {
        1000.0 * a as f64 / presses as f64
    }
}

/// One run's line of the report.
fn report_line(run: Run, t: &Tally) -> String {
    let mut by = String::new();
    for (c, name) in CLASSES.iter().enumerate() {
        let _ = write!(
            by,
            " {name}={:.0}%",
            pct(t.class_before[c], t.class_keys[c])
        );
    }
    let ms = |us: u64| us as f64 / 1000.0;
    format!(
        "{} before-echo {:>5.1}% ({}/{}) at-press {:>5.1}% wrong: runs={} total={}ms max={}ms \
         max-cells={} cell-ms={} cells: shown={} life p50={:.1}ms p95={:.1}ms p99={:.1}ms \
         max={:.1}ms perceived p50={}ms p95={}ms churn/1k: flush={:.1} epoch={:.1} \
         expire={:.1} unlearn={:.1} |{by}",
        run.label(),
        pct(t.before_echo, t.keys),
        t.before_echo,
        t.keys,
        pct(t.at_press, t.keys),
        t.wrong_runs,
        t.wrong_us / 1000,
        t.max_wrong_us / 1000,
        t.max_wrong_cells,
        t.wrong_cell_us / 1000,
        t.wrong_cells,
        ms(t.cell_life_p50_us),
        ms(t.cell_life_p95_us),
        ms(t.cell_life_p99_us),
        ms(t.cell_life_max_us),
        t.p50_us / 1000,
        t.p95_us / 1000,
        per_k(t.flushes, t.presses),
        per_k(t.epoch_ends, t.presses),
        per_k(t.expiries, t.presses),
        per_k(t.unlearned, t.presses),
    )
}

/// The cost line of a set of runs.
fn cost_line(name: &str, c: &Cost) -> String {
    let mean = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    format!(
        "cost {name}: observe/reconcile mean={:.2} max={} | folds/frame mean={:.3} max={} | \
         folds/press mean={:.3} max={} | row cells/press mean={:.1} max={} ({} of {} presses \
         read a row)",
        mean(c.observe_calls, c.reconciles),
        c.observe_max,
        mean(c.frame_folds, c.reconciles),
        c.frame_folds_max,
        mean(c.press_folds, c.presses),
        c.press_folds_max,
        mean(c.row_cells, c.presses),
        c.row_cells_max,
        c.row_samples,
        c.presses,
    )
}

#[test]
fn the_corpus_is_the_region_probe_s() {
    let lines = corpus();
    let keys: usize = lines.iter().map(|l| l.strokes.len()).sum();
    let mut families = [0usize; 10];
    for l in &lines {
        if let Some(n) = families.get_mut(l.family) {
            *n += 1;
        }
    }
    assert_eq!(
        (keys, families),
        (REGION_KEYS, REGION_FAMILIES),
        "the port draws as the region probe drew (families {FAMILY_NAMES:?})"
    );
}

/// Every run of the main matrix, in report order: each far end and
/// integration, each link, each mode, each variant.
fn main_runs() -> Vec<Run> {
    let mut runs = Vec::new();
    for editor in [Editor::Zle, Editor::Readline] {
        for integration in [true, false] {
            for rtt_ms in RTTS_MS {
                for mode in [PredictMode::Adaptive, PredictMode::Always] {
                    for variant in VARIANTS {
                        runs.push(Run {
                            editor,
                            integration,
                            rtt_ms,
                            mode,
                            variant,
                        });
                    }
                }
            }
        }
    }
    runs
}

/// `runs` over `lines`, every core busy, each with the script of its far end
/// and integration (`switch`: the switch corpus). Results in `runs`' order.
fn measure_all(lines: &[Line], runs: &[Run], switch: bool) -> Vec<Result<Tally, String>> {
    let mut worlds: Vec<(Editor, bool)> = runs.iter().map(|r| (r.editor, r.integration)).collect();
    worlds.sort_by_key(|&(e, i)| (format!("{e:?}"), i));
    worlds.dedup();
    let scripts = aterm_spec::check::each(&worlds, |&(editor, integration)| {
        script(lines, editor, integration, switch)
    });
    aterm_spec::check::each(runs, |&run| {
        let at = worlds
            .iter()
            .position(|&w| w == (run.editor, run.integration))
            .expect("every run's world is scripted");
        let script = &scripts[at];
        measure(script, &timeline(script, run.rtt_ms), run)
    })
}

/// Every run's tally, or every broken clause at once.
fn tallies(runs: &[Run], results: Vec<Result<Tally, String>>) -> Vec<(Run, Tally)> {
    let mut broken = Vec::new();
    let mut out = Vec::new();
    for (&run, result) in runs.iter().zip(results) {
        match result {
            Ok(t) => out.push((run, t)),
            Err(e) => broken.push(e),
        }
    }
    assert!(
        broken.is_empty(),
        "the theorem's checker found {} broken clause(s) on the corpus:\n{}",
        broken.len(),
        broken.join("\n")
    );
    out
}

/// `Full`'s count of keys shown before their echo in each run of the main
/// matrix at landing (assertion 3, a ratchet): `(editor, integration, rtt,
/// mode, keys)`. Deterministic, so an improvement re-records it.
const FULL_FLOORS: [(Editor, bool, u32, PredictMode, u64); 64] = [
    (Editor::Zle, true, 1, PredictMode::Adaptive, 0),
    (Editor::Zle, true, 1, PredictMode::Always, 6538),
    (Editor::Zle, true, 30, PredictMode::Adaptive, 6037),
    (Editor::Zle, true, 30, PredictMode::Always, 6538),
    (Editor::Zle, true, 80, PredictMode::Adaptive, 6041),
    (Editor::Zle, true, 80, PredictMode::Always, 6527),
    (Editor::Zle, true, 100, PredictMode::Adaptive, 6009),
    (Editor::Zle, true, 100, PredictMode::Always, 6502),
    (Editor::Zle, true, 160, PredictMode::Adaptive, 5932),
    (Editor::Zle, true, 160, PredictMode::Always, 6441),
    (Editor::Zle, true, 250, PredictMode::Adaptive, 5433),
    (Editor::Zle, true, 250, PredictMode::Always, 5888),
    (Editor::Zle, true, 320, PredictMode::Adaptive, 5324),
    (Editor::Zle, true, 320, PredictMode::Always, 5798),
    (Editor::Zle, true, 600, PredictMode::Adaptive, 4446),
    (Editor::Zle, true, 600, PredictMode::Always, 5202),
    (Editor::Zle, false, 1, PredictMode::Adaptive, 0),
    (Editor::Zle, false, 1, PredictMode::Always, 6310),
    (Editor::Zle, false, 30, PredictMode::Adaptive, 5829),
    (Editor::Zle, false, 30, PredictMode::Always, 6310),
    (Editor::Zle, false, 80, PredictMode::Adaptive, 5835),
    (Editor::Zle, false, 80, PredictMode::Always, 6301),
    (Editor::Zle, false, 100, PredictMode::Adaptive, 5803),
    (Editor::Zle, false, 100, PredictMode::Always, 6276),
    (Editor::Zle, false, 160, PredictMode::Adaptive, 5730),
    (Editor::Zle, false, 160, PredictMode::Always, 6219),
    (Editor::Zle, false, 250, PredictMode::Adaptive, 5262),
    (Editor::Zle, false, 250, PredictMode::Always, 5698),
    (Editor::Zle, false, 320, PredictMode::Adaptive, 5201),
    (Editor::Zle, false, 320, PredictMode::Always, 5659),
    (Editor::Zle, false, 600, PredictMode::Adaptive, 4438),
    (Editor::Zle, false, 600, PredictMode::Always, 5192),
    (Editor::Readline, true, 1, PredictMode::Adaptive, 0),
    (Editor::Readline, true, 1, PredictMode::Always, 6538),
    (Editor::Readline, true, 30, PredictMode::Adaptive, 6037),
    (Editor::Readline, true, 30, PredictMode::Always, 6538),
    (Editor::Readline, true, 80, PredictMode::Adaptive, 6041),
    (Editor::Readline, true, 80, PredictMode::Always, 6530),
    (Editor::Readline, true, 100, PredictMode::Adaptive, 6009),
    (Editor::Readline, true, 100, PredictMode::Always, 6506),
    (Editor::Readline, true, 160, PredictMode::Adaptive, 5932),
    (Editor::Readline, true, 160, PredictMode::Always, 6459),
    (Editor::Readline, true, 250, PredictMode::Adaptive, 5439),
    (Editor::Readline, true, 250, PredictMode::Always, 5959),
    (Editor::Readline, true, 320, PredictMode::Adaptive, 5324),
    (Editor::Readline, true, 320, PredictMode::Always, 5798),
    (Editor::Readline, true, 600, PredictMode::Adaptive, 4446),
    (Editor::Readline, true, 600, PredictMode::Always, 5202),
    (Editor::Readline, false, 1, PredictMode::Adaptive, 0),
    (Editor::Readline, false, 1, PredictMode::Always, 6310),
    (Editor::Readline, false, 30, PredictMode::Adaptive, 5829),
    (Editor::Readline, false, 30, PredictMode::Always, 6310),
    (Editor::Readline, false, 80, PredictMode::Adaptive, 5835),
    (Editor::Readline, false, 80, PredictMode::Always, 6304),
    (Editor::Readline, false, 100, PredictMode::Adaptive, 5803),
    (Editor::Readline, false, 100, PredictMode::Always, 6280),
    (Editor::Readline, false, 160, PredictMode::Adaptive, 5730),
    (Editor::Readline, false, 160, PredictMode::Always, 6237),
    (Editor::Readline, false, 250, PredictMode::Adaptive, 5268),
    (Editor::Readline, false, 250, PredictMode::Always, 5769),
    (Editor::Readline, false, 320, PredictMode::Adaptive, 5201),
    (Editor::Readline, false, 320, PredictMode::Always, 5659),
    (Editor::Readline, false, 600, PredictMode::Adaptive, 4438),
    (Editor::Readline, false, 600, PredictMode::Always, 5192),
];

/// THE EDITING WIN, the main matrix: the report, and assertions 1, 2, 3, 5, 6, 7
/// and 8 (module docs).
#[test]
fn predictive_editing_wins_and_never_shows_a_wrong_line() {
    let lines = corpus();
    let runs = main_runs();
    let all = tallies(&runs, measure_all(&lines, &runs, false));
    let mut report = String::new();
    for (run, t) in &all {
        let _ = writeln!(report, "{}", report_line(*run, t));
    }
    for variant in VARIANTS {
        let mut cost = Cost::default();
        for (_, t) in all.iter().filter(|(r, _)| r.variant == variant) {
            cost.add(t.cost);
        }
        let _ = writeln!(report, "{}", cost_line(variant.name(), &cost));
    }
    if let Some((_, t)) = all.first() {
        let sizes: Vec<String> = CLASSES
            .iter()
            .zip(t.class_keys)
            .map(|(name, n)| format!("{name}={n}"))
            .collect();
        let _ = writeln!(report, "class sizes: {}", sizes.join(" "));
    }
    for variant in [Variant::Phase0, Variant::Full] {
        let mut s = Stats::default();
        for (_, t) in all
            .iter()
            .filter(|(r, _)| r.variant == variant && r.mode == PredictMode::Adaptive)
        {
            s.add(t.stats);
        }
        let _ = writeln!(
            report,
            "checked {} Adaptive: frames compared whole {}, echoes compared {}, credits \
             attributed {}, chain frames shown {} (motions {}, shifts {}, line edges {}, deletes \
             {}), oracle folds {}, lines witnessed {}, bindings learned {}",
            variant.name(),
            s.frame_fidelity_checks,
            s.fidelity_checks,
            s.attribution_checks,
            s.edit_frames_displaying,
            s.motions_displayed,
            s.shifts_displayed,
            s.line_edge_displayed,
            s.delete_displayed,
            s.oracle_checks,
            s.witnessed_lines,
            s.bindings_learned,
        );
    }
    // WHAT THIS FILE FOUND, per run: the lines witnessed at a signed prompt
    // (the integration's authenticated `B`, the first key typed there) by
    // `Full`, and by `Full` with the gate shut at every scroll, as it was.
    let shut_runs: Vec<Run> = all
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| r.variant == Variant::Full && r.integration && r.mode == PredictMode::Adaptive)
        .map(|r| Run {
            variant: Variant::ShutAtScroll,
            ..r
        })
        .collect();
    let shut = tallies(&shut_runs, measure_all(&lines, &shut_runs, false));
    let mut witness_failures = Vec::new();
    for (run, t) in &shut {
        let full = all
            .iter()
            .find(|(r, _)| {
                *r == Run {
                    variant: Variant::Full,
                    ..*run
                }
            })
            .map_or(0, |(_, t)| t.stats.witnessed_lines);
        let _ = writeln!(
            report,
            "lines witnessed, {:?} signed Adaptive rtt={}: full {} of {LINES}, {} {} of {LINES}",
            run.editor,
            run.rtt_ms,
            full,
            run.variant.name(),
            t.stats.witnessed_lines
        );
        // The measurement's own control: the gate kept across a scroll
        // witnesses more lines than the gate shut there.
        if full <= t.stats.witnessed_lines {
            witness_failures.push(format!(
                "{}: full witnessed {full} lines, the gate shut at a scroll {}",
                run.label(),
                t.stats.witnessed_lines
            ));
        }
    }
    let find = |run: Run| {
        all.iter()
            .find(|(r, _)| *r == run)
            .map(|(_, t)| t)
            .expect("every run of the matrix ran")
    };
    // WHAT THIS FILE FOUND IN `Always`, per run: the mispredicted cells and
    // wrong episodes of the predictor before the plan (`Baseline`), of `Full`,
    // and of `Full` painting a guess that waits over the line's text, as it did
    // (`PaintedOverText`).
    let painted_runs: Vec<Run> = all
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| r.variant == Variant::Full && r.mode == PredictMode::Always)
        .map(|r| Run {
            variant: Variant::PaintedOverText,
            ..r
        })
        .collect();
    let painted = tallies(&painted_runs, measure_all(&lines, &painted_runs, false));
    let mut always_failures = Vec::new();
    for (run, t) in &painted {
        let full = find(Run {
            variant: Variant::Full,
            ..*run
        });
        let base = find(Run {
            variant: Variant::Baseline,
            ..*run
        });
        let _ = writeln!(
            report,
            "mispredicted cells (wrong episodes), {:?} {} Always rtt={}: baseline {} ({}), full \
             {} ({}), {} {} ({})",
            run.editor,
            if run.integration { "signed" } else { "remote" },
            run.rtt_ms,
            base.wrong_cells,
            base.wrong_runs,
            full.wrong_cells,
            full.wrong_runs,
            run.variant.name(),
            t.wrong_cells,
            t.wrong_runs
        );
        // 8. `Always` shows no more wrong than the predictor before the plan.
        if full.wrong_cells > base.wrong_cells || full.wrong_runs > base.wrong_runs {
            always_failures.push(format!(
                "{}: {} mispredicted cells in {} wrong episodes, before the plan {} in {}",
                run.label(),
                full.wrong_cells,
                full.wrong_runs,
                base.wrong_cells,
                base.wrong_runs
            ));
        }
        // The measurement's own control: a guess painted over the line's text
        // shows more cells wrong.
        if full.wrong_cells >= t.wrong_cells {
            always_failures.push(format!(
                "{}: full showed {} mispredicted cells, painted over the text {}",
                run.label(),
                full.wrong_cells,
                t.wrong_cells
            ));
        }
    }
    eprintln!("{report}");
    assert!(
        witness_failures.is_empty() && always_failures.is_empty(),
        "{}\n{}",
        witness_failures.join("\n"),
        always_failures.join("\n")
    );
    let mut failures = Vec::new();
    // 7. No cell shown wrong for longer than the longest glitch window, in any
    // run: every variant, mode and link.
    for (run, t) in &all {
        if t.cell_life_max_us > LIFETIME_BOUND_US {
            failures.push(format!(
                "{}: a cell shown wrong for {} ms, the longest window {} ms",
                run.label(),
                t.cell_life_max_us / 1000,
                LIFETIME_BOUND_US / 1000
            ));
        }
    }
    for (run, t) in all.iter().filter(|(r, _)| r.variant == Variant::Full) {
        // 1. No wrong line, Adaptive, and so no cell shown wrong.
        if run.mode == PredictMode::Adaptive && (t.wrong_runs != 0 || t.wrong_cells != 0) {
            failures.push(format!(
                "{}: {} wrong episodes ({} ms), {} cells shown wrong",
                run.label(),
                t.wrong_runs,
                t.wrong_us / 1000,
                t.wrong_cells
            ));
        }
        // 2. At least the baseline's keys before their echo, in every class.
        let base = find(Run {
            variant: Variant::Baseline,
            ..*run
        });
        for (c, name) in CLASSES.iter().enumerate() {
            if t.class_before[c] < base.class_before[c] {
                failures.push(format!(
                    "{}: {name} shown before echo {} times, the baseline {}",
                    run.label(),
                    t.class_before[c],
                    base.class_before[c]
                ));
            }
        }
        // 3. The ratchet.
        if let Some(&(.., floor)) = FULL_FLOORS.iter().find(|&&(e, i, rtt, mode, _)| {
            (e, i, rtt, mode) == (run.editor, run.integration, run.rtt_ms, run.mode)
        }) && t.before_echo < floor
        {
            failures.push(format!(
                "{}: {} keys before echo, the floor {floor}",
                run.label(),
                t.before_echo
            ));
        }
        // 5. The cost.
        if t.cost.observe_max > OBSERVE_BOUND || t.cost.frame_folds_max > FOLD_BOUND {
            failures.push(format!(
                "{}: {}",
                run.label(),
                cost_line("over its bound", &t.cost)
            ));
        }
    }
    let recorded: Vec<String> = all
        .iter()
        .filter(|(r, _)| r.variant == Variant::Full)
        .map(|(r, t)| {
            format!(
                "    (Editor::{:?}, {}, {}, PredictMode::{:?}, {}),",
                r.editor, r.integration, r.rtt_ms, r.mode, t.before_echo
            )
        })
        .collect();
    assert_eq!(
        FULL_FLOORS.len(),
        recorded.len(),
        "record the floors:\n{}",
        recorded.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // The checker ran, and the edit lane was exercised: a clause held on
    // frames that showed what this increment shows.
    let full: Stats = {
        let mut s = Stats::default();
        for (_, t) in all
            .iter()
            .filter(|(r, _)| r.variant == Variant::Full && r.mode == PredictMode::Adaptive)
        {
            s.add(t.stats);
        }
        s
    };
    for (name, n) in [
        ("frame-fidelity checks", full.frame_fidelity_checks),
        ("chain frames displayed", full.edit_frames_displaying),
        ("motions displayed", full.motions_displayed),
        ("shifts displayed", full.shifts_displayed),
        ("line edges displayed", full.line_edge_displayed),
        ("deletes displayed", full.delete_displayed),
    ] {
        assert!(n > 0, "Full, Adaptive: no {name} on the corpus: {full:?}");
    }
    // The metric's negative control: the shipped glyph lane before predictive
    // editing shows wrong lines on this corpus, so the zeros above are seen.
    let (baseline_wrong, baseline_cells) = all
        .iter()
        .filter(|(r, _)| r.variant == Variant::Baseline)
        .fold((0, 0), |(runs, cells), (_, t)| {
            (runs + t.wrong_runs, cells + t.wrong_cells)
        });
    assert!(
        baseline_wrong > 0 && baseline_cells > 0,
        "the wrong-episode metric saw nothing to catch ({baseline_wrong} episodes, \
         {baseline_cells} cells)"
    );
}

/// Assertion 4: a cooked tty echoes `^[[D` for `←`, and readline's overwrite
/// mode replaces where the others shift, so under either the class ladder never
/// proves the class, and nothing of it is shown, on the corpus lines that use
/// arrows and edit inside the text (`EDITING_FAMILIES`).
#[test]
fn a_cooked_tty_shows_no_motion_and_overwrite_mode_no_shift() {
    let lines: Vec<Line> = corpus()
        .into_iter()
        .filter(|l| EDITING_FAMILIES.contains(&l.family))
        .collect();
    let mut runs = Vec::new();
    for editor in [Editor::Cooked, Editor::Overwrite] {
        for rtt_ms in CONTROL_RTTS_MS {
            runs.push(Run {
                editor,
                integration: false,
                rtt_ms,
                mode: PredictMode::Adaptive,
                variant: Variant::Full,
            });
        }
    }
    let all = tallies(&runs, measure_all(&lines, &runs, false));
    let mut report = String::new();
    for (run, t) in &all {
        let _ = writeln!(report, "{}", report_line(*run, t));
        let s = t.stats;
        let _ = writeln!(
            report,
            "    motions armed under cooked={} displayed={} | shifts armed under overwrite={} \
             displayed={}",
            s.cooked_motions_armed,
            s.motions_displayed,
            s.overwrite_shifts_armed,
            s.overwrite_shifts_displayed
        );
    }
    eprintln!("{report}");
    for (run, t) in &all {
        let s = t.stats;
        assert_eq!(t.wrong_runs, 0, "{}: a wrong line\n{report}", run.label());
        match run.editor {
            Editor::Cooked => {
                assert!(
                    s.cooked_motions_armed > 0,
                    "{}: no motion typed\n{report}",
                    run.label()
                );
                assert_eq!(
                    s.motions_displayed,
                    0,
                    "{}: a motion shown\n{report}",
                    run.label()
                );
            }
            _ => {
                assert!(
                    s.overwrite_shifts_armed > 0,
                    "{}: no shift typed\n{report}",
                    run.label()
                );
                assert_eq!(
                    s.overwrite_shifts_displayed,
                    0,
                    "{}: a shift shown\n{report}",
                    run.label()
                );
            }
        }
    }
}

/// THE LADDER'S DECISION, measured: the class proofs end with their epoch
/// (`Full`), or stay for the session (`Sticky`). On the SWITCH corpus every
/// other line is typed into `cat`, a cooked program the line before started,
/// whose tty echoes `^[[D` for `←`: a motion proven at the shell and kept
/// shows `cat`'s line wrong. `Full` shows none; `Sticky` must show some, or
/// the ladder's cost (each line's first motion and first shift a round trip
/// dark, the main matrix's `full` against `sticky` rows) buys nothing.
#[test]
fn class_proofs_end_with_their_line() {
    let lines = corpus();
    let mut runs = Vec::new();
    for editor in [Editor::Zle, Editor::Readline] {
        for rtt_ms in CONTROL_RTTS_MS {
            for variant in [Variant::Full, Variant::Sticky] {
                runs.push(Run {
                    editor,
                    integration: false,
                    rtt_ms,
                    mode: PredictMode::Adaptive,
                    variant,
                });
            }
        }
    }
    let all = tallies(&runs, measure_all(&lines, &runs, true));
    let mut report = String::new();
    for (run, t) in &all {
        let _ = writeln!(report, "{}", report_line(*run, t));
    }
    eprintln!("{report}");
    let sticky_wrong: u64 = all
        .iter()
        .filter(|(r, _)| r.variant == Variant::Sticky)
        .map(|(_, t)| t.wrong_runs)
        .sum();
    for (run, t) in all.iter().filter(|(r, _)| r.variant == Variant::Full) {
        assert_eq!(t.wrong_runs, 0, "{}: a wrong line\n{report}", run.label());
    }
    assert!(
        sticky_wrong > 0,
        "session-sticky proofs showed no wrong line\n{report}"
    );
}
