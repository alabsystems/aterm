// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `HolderFence` (`aterm_models_core::holder_fence_model`).

use std::collections::{BTreeSet, VecDeque};

use aterm_grid::{FenceKnob, Grid, with_knob};
use aterm_models_core::holder_fence_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, bmc, with_buggy};

use super::holder_fence::Mark;
use super::{CutKind, HolderCut, Terminal};
use crate::scrollback::Scrollback;

/// Tall enough that nothing a trace draws scrolls into history.
const ROWS: u16 = 48;
const COLS: u16 = 16;
/// The width a reflow toggles to: the banner (12 cells) wraps there, so
/// every row below it is renumbered.
const NARROW: u16 = 10;
const BANNER: &[u8] = b"xxxxxxxxxxxx\r\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    LaunchA,
    LaunchB,
    StopA,
    StopB,
    FgA,
    FgB,
    ExitA,
    ExitB,
    KillA,
    ExecBA,
    ExecAB,
    Rewind,
    Climb,
    Settle,
    Scribble,
    Draw,
    BandMove,
    Splice,
    Reflow,
    Renumber,
    Ed3,
    FullClear,
    Alt1049,
    Alt1047,
    AltDraw,
    AltClear,
    AltExit,
    Admit,
}

impl Step {
    const ALL: [Self; 28] = [
        Self::LaunchA,
        Self::LaunchB,
        Self::StopA,
        Self::StopB,
        Self::FgA,
        Self::FgB,
        Self::ExitA,
        Self::ExitB,
        Self::KillA,
        Self::ExecBA,
        Self::ExecAB,
        Self::Rewind,
        Self::Climb,
        Self::Settle,
        Self::Scribble,
        Self::Draw,
        Self::BandMove,
        Self::Splice,
        Self::Reflow,
        Self::Renumber,
        Self::Ed3,
        Self::FullClear,
        Self::Alt1049,
        Self::Alt1047,
        Self::AltDraw,
        Self::AltClear,
        Self::AltExit,
        Self::Admit,
    ];

    /// The model action this real step is.
    const fn action(self) -> &'static str {
        match self {
            Self::LaunchA => "LaunchA",
            Self::LaunchB => "LaunchB",
            Self::StopA => "StopA",
            Self::StopB => "StopB",
            Self::FgA => "FgA",
            Self::FgB => "FgB",
            Self::ExitA => "ExitA",
            Self::ExitB => "ExitB",
            Self::KillA => "KillA",
            Self::ExecBA => "ExecBA",
            Self::ExecAB => "ExecAB",
            Self::Rewind => "Rewind",
            Self::Climb => "Climb",
            Self::Settle => "Settle",
            Self::Scribble => "Scribble",
            Self::Draw => "Draw",
            Self::BandMove => "BandMove",
            Self::Splice => "Splice",
            Self::Reflow => "Reflow",
            Self::Renumber => "Renumber",
            Self::Ed3 => "Ed3",
            Self::FullClear => "FullClear",
            Self::Alt1049 => "Alt1049",
            Self::Alt1047 => "Alt1047",
            Self::AltDraw => "AltDraw",
            Self::AltClear => "AltClear",
            Self::AltExit => "AltExit",
            Self::Admit => "Admit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fg {
    Shell,
    A,
    B,
}

/// One running drawer instance.
#[derive(Clone, Copy, Debug)]
struct Drawer {
    token: u64,
    /// Its lines' glyph.
    plain: char,
    /// Its card's glyph (`A` only draws a card).
    card: char,
}

impl Drawer {
    fn line(self) -> String {
        std::iter::repeat_n(self.plain, 4).collect()
    }

    fn card_line(self) -> String {
        std::iter::repeat_n(self.card, 4).collect()
    }

    /// Whether `text` holds a glyph neither of this instance's.
    fn foreign(self, text: &str) -> bool {
        text.chars()
            .any(|c| c != ' ' && c != '\0' && c != self.plain && c != self.card)
    }
}

/// The terminal and the process world around it.
struct World {
    t: Terminal,
    fg: Fg,
    a: Option<Drawer>,
    b: Option<Drawer>,
    /// Instances launched: each takes a fresh token, and each role
    /// alternates its glyph pair, so an earlier instance's rows are another
    /// writer's.
    launched: u64,
    a_instances: u64,
    b_instances: u64,
}

impl World {
    /// History under the screen (for the unscroll to pull back), a clear
    /// screen and the shell's banner on its first row.
    fn new() -> Self {
        let scrollback = Scrollback::new(16, 256, 1_000_000);
        let mut t = Terminal::with_scrollback(ROWS, COLS, 64, scrollback);
        for _ in 0..ROWS + 12 {
            t.process(b"xxxx\r\n");
        }
        t.process(b"\x1b[H\x1b[2J");
        t.process(BANNER);
        Self {
            t,
            fg: Fg::Shell,
            a: None,
            b: None,
            launched: 0,
            a_instances: 0,
            b_instances: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.t.process(bytes);
    }

    fn cut(&mut self, to: Option<u64>, kind: CutKind, from_alive: bool) {
        self.t.note_holder_cut(HolderCut {
            to,
            kind,
            from_alive,
        });
    }

    fn launch_a(&mut self) -> Drawer {
        self.launched += 1;
        self.a_instances += 1;
        let (plain, card) = if self.a_instances % 2 == 1 {
            ('A', 'a')
        } else {
            ('C', 'c')
        };
        let d = Drawer {
            token: 1000 + self.launched,
            plain,
            card,
        };
        self.a = Some(d);
        d
    }

    fn launch_b(&mut self) -> Drawer {
        self.launched += 1;
        self.b_instances += 1;
        let (plain, card) = if self.b_instances % 2 == 1 {
            ('B', 'b')
        } else {
            ('D', 'd')
        };
        let d = Drawer {
            token: 1000 + self.launched,
            plain,
            card,
        };
        self.b = Some(d);
        d
    }

    /// A drawer's first draw: its card row and a line, from the cursor.
    fn draw_launch(&mut self, d: Drawer) -> Result<(), String> {
        let bytes = format!("{}\r\n{}\r\n", d.card_line(), d.line());
        self.write(bytes.as_bytes());
        self.settle()
    }

    /// A drawer's line at the cursor.
    fn draw_line(&mut self, d: Drawer) -> Result<(), String> {
        let bytes = format!("{}\r\n", d.line());
        self.write(bytes.as_bytes());
        self.settle()
    }

    /// The main cursor to the frontier: column 0 of the first row with
    /// nothing drawn from it on.
    fn settle(&mut self) -> Result<(), String> {
        let main = self.t.main_grid();
        let frontier = (0..main.rows())
            .rev()
            .find(|&r| row_inked(main, r, 0))
            .map_or(0, |r| r + 1);
        if frontier >= main.rows() {
            return Err("the screen filled: the trace outgrew the walk's geometry".into());
        }
        self.write(format!("\x1b[{};1H", frontier + 1).as_bytes());
        Ok(())
    }

    fn a(&self) -> Result<Drawer, String> {
        self.a.ok_or_else(|| "no A runs".to_string())
    }

    fn b(&self) -> Result<Drawer, String> {
        self.b.ok_or_else(|| "no B runs".to_string())
    }

    /// `A`'s mark.
    fn mark_a(&self) -> Result<&Mark, String> {
        let a = self.a()?;
        self.t
            .holder_fence()
            .marks
            .iter()
            .find(|m| m.token == a.token)
            .ok_or_else(|| "A holds no mark".to_string())
    }

    /// The screen row of absolute `row` on the main screen.
    fn screen_row(&self, row: u64) -> Result<u16, String> {
        let main = self.t.main_grid();
        row.checked_sub(main.visible_to_absolute(0))
            .and_then(|r| u16::try_from(r).ok())
            .filter(|&r| r < main.rows())
            .ok_or_else(|| format!("row {row} is off the screen"))
    }

    /// Drive one real step.
    fn drive(&mut self, step: Step) -> Result<(), String> {
        match step {
            Step::LaunchA => {
                let d = self.launch_a();
                self.fg = Fg::A;
                self.cut(Some(d.token), CutKind::Group, true);
                self.draw_launch(d)?;
            }
            Step::LaunchB => {
                let d = self.launch_b();
                self.fg = Fg::B;
                self.cut(Some(d.token), CutKind::Group, true);
                self.draw_launch(d)?;
            }
            Step::StopA | Step::StopB => {
                self.fg = Fg::Shell;
                self.cut(None, CutKind::Group, true);
            }
            Step::FgA => {
                let d = self.a()?;
                self.fg = Fg::A;
                self.cut(Some(d.token), CutKind::Group, true);
                if !self.t.modes().alternate_screen {
                    self.draw_line(d)?;
                }
            }
            Step::FgB => {
                let d = self.b()?;
                self.fg = Fg::B;
                self.cut(Some(d.token), CutKind::Group, true);
                self.draw_line(d)?;
            }
            Step::ExitA => {
                self.a = None;
                self.fg = Fg::Shell;
                self.cut(None, CutKind::Group, false);
            }
            Step::ExitB => {
                self.b = None;
                self.fg = Fg::Shell;
                self.cut(None, CutKind::Group, false);
            }
            Step::KillA => {
                let d = self.a()?;
                self.a = None;
                self.t.retire_holder_mark(d.token);
            }
            Step::ExecBA => {
                self.b = None;
                let d = self.launch_a();
                self.fg = Fg::A;
                self.cut(Some(d.token), CutKind::Exec, false);
                self.draw_launch(d)?;
            }
            Step::ExecAB => {
                self.a = None;
                let d = self.launch_b();
                self.fg = Fg::B;
                self.cut(Some(d.token), CutKind::Exec, false);
                self.draw_launch(d)?;
            }
            Step::Rewind => self.write(b"xxxxxxxx\r"),
            Step::Climb => self.write(b"\r\nxxxxxxxx\x1b[A\r"),
            Step::Settle => self.settle()?,
            Step::Scribble => {
                let first = {
                    let mark = self.mark_a()?;
                    let span = mark.spans.first().ok_or("A holds no span")?;
                    self.t
                        .main_grid()
                        .anchors()
                        .row(span.start)
                        .ok_or("A's oldest span has no start")?
                };
                let row = self.screen_row(first)?;
                self.write(format!("\x1b7\x1b[{};1Hx\x1b8", row + 1).as_bytes());
            }
            Step::Draw => {
                let d = self.a()?;
                self.draw_line(d)?;
            }
            Step::BandMove => {
                let start = {
                    let mark = self.mark_a()?;
                    let open = mark
                        .spans
                        .iter()
                        .find(|s| s.end.is_none())
                        .ok_or("A has no open span")?;
                    self.t
                        .main_grid()
                        .anchors()
                        .row(open.start)
                        .ok_or("A's open span has no start")?
                };
                let row = self.screen_row(start)?.saturating_sub(1);
                self.write(format!("\x1b[{};1H\x1b[L", row + 1).as_bytes());
                self.settle()?;
            }
            Step::Splice => {
                let end = {
                    let mark = self.mark_a()?;
                    let span = mark.spans.last().ok_or("A holds no span")?;
                    let end = span.end.ok_or("A's newest span is open")?;
                    self.t
                        .main_grid()
                        .anchors()
                        .row(end)
                        .ok_or("A's newest span has no end")?
                };
                let bottom = self.screen_row(end)?;
                if bottom < 2 {
                    return Err(format!("A's newest span ends on screen row {bottom}"));
                }
                self.write(
                    format!("\x1b7\x1b[1;{bottom}r\x1b[{bottom};1H\n\x1b[r\x1b8").as_bytes(),
                );
            }
            Step::Reflow => {
                let cols = if self.t.cols() == COLS { NARROW } else { COLS };
                self.t.resize(ROWS, cols);
            }
            Step::Renumber => self.write(b"\x1b[1+T\x1b[B"),
            Step::Ed3 => self.write(b"\x1b[3J"),
            Step::FullClear => {
                let d = self.a()?;
                self.write(b"\x1b[H\x1b[2J");
                self.draw_launch(d)?;
            }
            Step::Alt1049 => self.write(b"\x1b[?1049h"),
            Step::Alt1047 => self.write(b"\x1b[?1047h"),
            Step::AltDraw => {
                let glyph = match self.fg {
                    Fg::B => self.b()?.plain,
                    _ => 'x',
                };
                let cursor = self.t.grid().cursor();
                let line: String = std::iter::repeat_n(glyph, 4).collect();
                self.write(
                    format!("\x1b[1;1H{line}\x1b[{};{}H", cursor.row + 1, cursor.col + 1)
                        .as_bytes(),
                );
            }
            Step::AltClear => self.write(b"\x1b[2J"),
            Step::AltExit => self.write(b"\x1b[?47l"),
            Step::Admit => {}
        }
        Ok(())
    }
}

fn inked(cell: &crate::grid::Cell) -> bool {
    let c = cell.char_data();
    (c != 0 && c != u16::from(b' ')) || !cell.colors().bg_is_default()
}

/// Whether screen row `row` of `g` holds ink from column `col` on.
fn row_inked(g: &Grid, row: u16, col: u16) -> bool {
    (col..g.cols()).any(|c| g.cell(row, c).is_some_and(inked))
}

/// The text of absolute row `row` of `g`, on its screen or in its history.
fn row_text(g: &Grid, row: u64) -> Option<String> {
    let base = g.visible_to_absolute(0);
    if row >= base {
        let screen = u16::try_from(row - base).ok().filter(|&r| r < g.rows())?;
        return g.row_text_screen(screen);
    }
    let back = usize::try_from(base - row - 1).ok()?;
    g.history_line_rev(back)
        .map(|line| String::from_utf8_lossy(line.as_bytes()).into_owned())
}

/// The rows `mark`'s spans hold, `[start, end)` each, an open span to the
/// screen's bottom, bounded to the rows the main grid keeps.
fn span_rows(g: &Grid, mark: &Mark, closed_only: bool) -> Vec<u64> {
    let anchors = g.anchors();
    let screen_end = g.visible_to_absolute(0) + u64::from(g.rows());
    let oldest = g.oldest_absolute_row();
    let mut rows = Vec::new();
    for span in &mark.spans {
        if closed_only && span.end.is_none() {
            continue;
        }
        let Some(start) = anchors.row(span.start) else {
            continue;
        };
        let end = match span.end {
            None => Some(screen_end),
            Some(e) => anchors.row(e),
        };
        let Some(end) = end else { continue };
        rows.extend(start.max(oldest)..end.min(screen_end));
    }
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// What lies from the main cursor on: 0 nothing, 1 on its own row, 2 only
/// below it.
fn ink_from_cursor(g: &Grid) -> i64 {
    let cursor = g.cursor();
    let row = cursor.row.min(g.rows().saturating_sub(1));
    if row_inked(g, row, cursor.col) {
        1
    } else if (row + 1..g.rows()).any(|r| row_inked(g, r, 0)) {
        2
    } else {
        0
    }
}

/// The real state as the model's variables, `hurt` (a ghost) left out.
fn project(w: &World) -> Result<State, String> {
    let t = &w.t;
    let fence = t.holder_fence();
    let main = t.main_grid();
    let lost = main.anchors().lost();
    let mark_of =
        |d: Option<Drawer>| d.and_then(|d| fence.marks.iter().find(|m| m.token == d.token));
    let tainted = |m: &Mark| m.tainted || (lost && !m.spans.is_empty());
    let ma = mark_of(w.a);
    let mb = mark_of(w.b);
    let ta = ma.is_some_and(tainted);
    let (mut fa, mut ka, mut ga, mut idle) = (false, false, false, false);
    if let (Some(a), Some(m)) = (w.a, ma) {
        let rows = span_rows(main, m, false);
        let texts: Vec<(u64, String)> = rows
            .iter()
            .filter_map(|&r| row_text(main, r).map(|text| (r, text)))
            .collect();
        fa = !ta && texts.iter().any(|(_, text)| a.foreign(text));
        ka = !ta
            && texts
                .iter()
                .any(|(_, text)| text.starts_with(&a.card_line()));
        if w.fg != Fg::A
            && let Some(seal) = &m.seal
        {
            ga = span_rows(main, m, true).into_iter().any(|r| {
                r >= seal.base && seal.rows.binary_search_by_key(&r, |&(row, _)| row).is_err()
            });
        }
        if w.fg == Fg::A {
            let cursor = main.visible_to_absolute(main.cursor().row);
            idle = m
                .spans
                .iter()
                .find(|s| s.end.is_none())
                .and_then(|s| main.anchors().row(s.start))
                .is_some_and(|start| start >= cursor);
            gate_agrees(t, a, &rows, ta)?;
        }
    }
    let alt_screen = t.modes().alternate_screen;
    let alt_buffer = if alt_screen {
        Some(t.grid())
    } else {
        t.alt_grid.as_ref()
    };
    let aw = alt_buffer.is_some_and(|g| (0..g.rows()).any(|r| row_inked(g, r, 0)));
    let fg = match w.fg {
        Fg::Shell => 0,
        Fg::A => 1,
        Fg::B => 2,
    };
    Ok(State::from([
        ("fg", fg),
        ("ra", i64::from(w.a.is_some())),
        ("ma", i64::from(ma.is_some())),
        ("ta", i64::from(ta)),
        ("fa", i64::from(fa)),
        ("ga", i64::from(ga)),
        ("ka", i64::from(ka)),
        ("spans", ma.map_or(0, |m| m.spans.len() as i64)),
        ("idle", i64::from(idle)),
        ("rb", i64::from(w.b.is_some())),
        ("mb", i64::from(mb.is_some())),
        ("tb", i64::from(mb.is_some_and(tainted))),
        ("marks", fence.marks.len() as i64),
        ("ink", ink_from_cursor(main)),
        ("alt", i64::from(alt_screen)),
        ("aa", i64::from(ma.is_some_and(|m| m.alt_fresh))),
        ("aw", i64::from(aw)),
    ]))
}

/// The gate admits exactly the rows of `A`'s spans, and none while `A` is
/// tainted, over every row the main grid keeps.
fn gate_agrees(t: &Terminal, a: Drawer, rows: &[u64], tainted: bool) -> Result<(), String> {
    let view = t.holder_view();
    if view.token != a.token {
        return Err(format!(
            "the gate reads token {}, A is {}",
            view.token, a.token
        ));
    }
    let main = t.main_grid();
    let screen_end = main.visible_to_absolute(0) + u64::from(main.rows());
    for row in main.oldest_absolute_row()..screen_end {
        let admitted = view.admits_rows(row, row + 1);
        let held = !tainted && rows.binary_search(&row).is_ok();
        if admitted != held {
            return Err(format!(
                "the gate {} row {row}, A's spans {} it: {view:?}",
                if admitted { "admits" } else { "refuses" },
                if held { "hold" } else { "do not hold" },
            ));
        }
    }
    Ok(())
}

/// The variables `a` and `b` differ in, for a rejection's message.
fn diff(a: &State, b: &State) -> String {
    a.iter()
        .filter(|&(k, v)| b.get(k) != Some(v))
        .map(|(k, v)| format!("{k}: real {v}, model {:?}", b.get(k)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn replay(path: &[Step]) -> Result<World, String> {
    let mut w = World::new();
    for &step in path {
        w.drive(step)
            .map_err(|why| format!("replaying {path:?}: {step:?}: {why}"))?;
    }
    Ok(w)
}

/// What the walk covered.
struct Coverage {
    states: usize,
    edges: usize,
    actions: BTreeSet<&'static str>,
}

/// Walk `model`'s whole reachable space on the real terminal. `Err` names
/// the first real step the model does not admit under its own action: the
/// trace, the step and the variables that differ.
fn walk(model: &Model) -> Result<Coverage, String> {
    let mut init = project(&World::new())?;
    init.insert("hurt", 0);
    if init != model.init_state() {
        return Err(format!(
            "the fixture projects to {}",
            diff(&init, &model.init_state())
        ));
    }
    let mut seen: BTreeSet<State> = BTreeSet::from([init.clone()]);
    let mut queue: VecDeque<(State, Vec<Step>)> = VecDeque::from([(init, Vec::new())]);
    let mut coverage = Coverage {
        states: 0,
        edges: 0,
        actions: BTreeSet::new(),
    };
    while let Some((state, path)) = queue.pop_front() {
        coverage.states += 1;
        for step in Step::ALL {
            let action = step.action();
            let expected = model.successors(action, &state);
            let [want] = expected.as_slice() else {
                if expected.is_empty() {
                    continue;
                }
                return Err(format!("{action} is not deterministic at {state:?}"));
            };
            let mut w = replay(&path)?;
            w.drive(step)
                .map_err(|why| format!("{step:?} after {path:?}: {why}"))?;
            let mut after = project(&w)
                .map_err(|why| format!("{}: real {step:?} after {path:?}: {why}", model.name))?;
            after.insert("hurt", want["hurt"]);
            if &after != want {
                return Err(format!(
                    "{}: real {step:?} after {path:?} is not the model's {action} : {}",
                    model.name,
                    diff(&after, want)
                ));
            }
            coverage.edges += 1;
            coverage.actions.insert(action);
            if seen.insert(after.clone()) {
                let mut next = path.clone();
                next.push(step);
                queue.push_back((after, next));
            }
        }
    }
    Ok(coverage)
}

#[test]
fn the_real_fence_conforms_to_the_model_over_its_whole_reachable_space() {
    let model = holder_fence_model();
    let coverage = walk(&model).unwrap_or_else(|why| panic!("{why}"));
    let reachable = bmc(&model).expect("the healthy model holds");
    assert_eq!(
        coverage.states, reachable,
        "the real traces must reach every state the model can"
    );
    assert_eq!(
        coverage.actions,
        model
            .actions
            .iter()
            .map(|a| a.name)
            .collect::<BTreeSet<_>>(),
        "every model action must be driven on the real terminal"
    );
    assert!(
        coverage.edges > coverage.states,
        "every state is left more than one way"
    );
}

/// Negative control: each knob switched off is rejected at the model action
/// whose law it breaks.
#[test]
fn each_knob_is_rejected_at_its_own_action() {
    let model = holder_fence_model();
    for (knob, at) in [
        (FenceKnob::NoBandTransform, &["BandMove"][..]),
        (FenceKnob::SpliceEndAsStart, &["Splice"][..]),
        (FenceKnob::NoReflowLend, &["Reflow"][..]),
        (FenceKnob::UnlentRenumberKeeps, &["Renumber"][..]),
        (FenceKnob::SwapLoses, &["Alt1049", "Alt1047", "AltExit"][..]),
        (FenceKnob::NoSealCheck, &["FgA"][..]),
        (
            FenceKnob::NoLayTaint,
            &["LaunchA", "LaunchB", "FgA", "FgB"][..],
        ),
        (
            FenceKnob::LayTaintSkipsCursorRow,
            &["LaunchA", "LaunchB", "FgA", "FgB"][..],
        ),
        (FenceKnob::ExecInherits, &["ExecBA", "ExecAB"][..]),
        (
            FenceKnob::CountEviction,
            &["LaunchA", "LaunchB", "FgA", "FgB"][..],
        ),
        (FenceKnob::StopKeepsAltFresh, &["StopA"][..]),
    ] {
        let rejected = with_knob(knob, || walk(&model))
            .err()
            .unwrap_or_else(|| panic!("{knob:?}: the walk conformed"));
        assert!(
            at.iter()
                .any(|action| rejected.contains(&format!("the model's {action} :"))),
            "{knob:?} must be rejected at {at:?}: {rejected}"
        );
    }
}

/// Negative control, the other way: the shipped fence matches no mutant of
/// the model.
#[test]
fn the_shipped_fence_matches_no_mutant() {
    let model = holder_fence_model();
    for dial in model.mutant_dials() {
        let rejected = walk(&with_buggy(&model, dial))
            .err()
            .unwrap_or_else(|| panic!("the shipped fence conforms to Buggy = {dial}"));
        assert!(
            rejected.contains("the model's"),
            "Buggy = {dial}: {rejected}"
        );
    }
}
