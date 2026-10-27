// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-keys [--vim] [--keep]`: whether the keys typed into the real Claude Code's composer
//! after each input boundary light Rainbow Kitty's trail.
//!
//! Claude Code runs fullscreen with its caret drawn (`CLAUDE_CODE_NO_FLICKER=1`,
//! `CLAUDE_CODE_NATIVE_CURSOR=1`). That is the setup where a boundary that doubts the caret's row
//! lets Claude's next repaint of another row (a selection highlight, a footer hint) park the
//! composer's row, and every key typed on it is then refused `program-row` (aterm-effects'
//! `parked-caret-row-ownership`, aterm-gui's `press-licence-law`). Each scenario types into the
//! composer, applies its boundary, types its keys, and reads their admissions from
//! `aterm ctl trail`. Each key waits for its echo to move the caret before a frame judges it, so
//! two keys never reach the glow as one move.
//!
//! A scenario fails unless its boundary happened: aterm took the key, Claude painted a selection
//! over the path the drag crossed, Escape or Ctrl-C stopped a stub turn that was streaming, or
//! the wheel scrolled a transcript that long stub turns made taller than the lab's screen (24
//! rows unless `LAB_LINES` says otherwise). `--vim` runs
//! Claude's vim editor mode instead: an Escape into NORMAL must leave its motions dark, and `i`
//! must light the typing again.
//!
//! The verdict is 0 when every scenario reads as expected and 1 when one does not. 3 is COULD
//! NOT RUN: the lab did not come up, an `aterm ctl` call was refused, a wait for something that
//! must happen hung for a minute, or what the scenarios stand on is missing (Claude Code
//! fullscreen with its caret drawn and the mouse tracked, Rainbow Kitty, vim's INSERT).

use std::time::Duration;

use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::Run;
use crate::{Fail, Res};

/// What a scenario's keys must show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Every key's admission is licensed.
    Lit,
    /// No key's admission is licensed.
    Dark,
}

impl Want {
    const fn word(self) -> &'static str {
        match self {
            Want::Lit => "lit",
            Want::Dark => "dark",
        }
    }
}

/// One `admission` line of `aterm ctl trail`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    /// Its `seq=`.
    pub seq: u64,
    /// Its `phase=` (`licensed`, `declined`, …).
    pub phase: String,
    /// Its `reason=` (`licensed`, `program-row`, …).
    pub reason: String,
    /// The whole line, printed under a scenario that failed.
    pub line: String,
}

/// The `admission` lines of `aterm ctl trail` output, oldest first.
#[must_use]
pub fn admissions(text: &str) -> Vec<Admission> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("admission ")?;
            let field = |key: &str| {
                rest.split_whitespace()
                    .find_map(|word| word.strip_prefix(key))
                    .map(str::to_string)
            };
            Some(Admission {
                seq: field("seq=")?.parse().ok()?,
                phase: field("phase=")?,
                reason: field("reason=")?,
                line: line.trim().to_string(),
            })
        })
        .collect()
}

/// Whether the last `keys` admissions after `since` show `want`: all of them licensed, or none.
#[must_use]
pub fn judged(all: &[Admission], since: u64, keys: usize, want: Want) -> (bool, Vec<String>) {
    let fresh: Vec<&Admission> = all.iter().filter(|a| a.seq > since).collect();
    let last = &fresh[fresh.len().saturating_sub(keys)..];
    let words = last.iter().map(|a| a.reason.clone()).collect();
    let licensed = last.iter().filter(|a| a.phase == "licensed").count();
    let ok = match want {
        Want::Lit => last.len() == keys && licensed == keys,
        Want::Dark => !last.is_empty() && licensed == 0,
    };
    (ok, words)
}

/// The first screen row holding a path (a word with a `/` in it, the welcome's working
/// directory), and the columns it spans: `(row, first, last)`.
#[must_use]
pub fn path_span(screen: &str) -> Option<(usize, usize, usize)> {
    screen.lines().enumerate().find_map(|(row, line)| {
        let cells: Vec<char> = line.chars().collect();
        let slash = cells.iter().position(|&c| c == '/')?;
        let word = |c: &char| !c.is_whitespace() && !matches!(c, '│' | '|' | '(' | ')');
        let first = cells[..slash]
            .iter()
            .rposition(|c| !word(c))
            .map_or(0, |i| i + 1);
        let last = cells[slash..]
            .iter()
            .position(|c| !word(c))
            .map_or(cells.len(), |i| slash + i)
            .saturating_sub(1);
        (last > first).then_some((row, first, last))
    })
}

/// The boundary a scenario applies between its warm-up and its keys.
#[derive(Clone, Copy, Debug)]
enum Boundary {
    /// Nothing: plain typing.
    None,
    /// A left-button drag across the first path on the screen.
    DragPath,
    /// The wheel, three notches up and three down, over a transcript taller than the screen, so
    /// Claude scrolls it and repaints the rows above the composer.
    Wheel,
    /// One `aterm ctl key` press.
    Key(&'static str),
    /// One `aterm ctl key` press that interrupts the turn [`Prep::Streaming`] started: Claude's
    /// busy row must show before it and go after it.
    Interrupt(&'static str),
}

/// What a scenario sets up before its warm-up keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prep {
    /// Nothing.
    None,
    /// Stub turns with the long canned reply, until the transcript overflows the screen.
    LongTurns,
    /// A slow stub turn, still streaming (a tick a second) when the boundary lands.
    Streaming,
}

/// One scenario: warm-up keys, its boundary, the keys judged, and what they must show.
struct Scenario {
    name: &'static str,
    /// What runs before the warm-up keys.
    prep: Prep,
    warm: &'static [&'static str],
    boundary: Boundary,
    keys: &'static [&'static str],
    want: Want,
}

/// Claude's default editor: every boundary keeps the composer's keys lit.
const PLAIN: &[Scenario] = &[
    Scenario {
        prep: Prep::None,
        name: "typing",
        warm: &["a", "b"],
        boundary: Boundary::None,
        keys: &["c", "d", "e"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::None,
        name: "drag over a path",
        warm: &["f"],
        boundary: Boundary::DragPath,
        keys: &["g", "h", "i"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::None,
        name: "escape",
        warm: &["n"],
        boundary: Boundary::Key("esc"),
        keys: &["o", "p", "q"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::None,
        name: "ctrl+l",
        warm: &["r"],
        boundary: Boundary::Key("ctrl+l"),
        keys: &["s", "t", "u"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::None,
        name: "ctrl+c (clears the composer)",
        warm: &["v"],
        boundary: Boundary::Key("ctrl+c"),
        keys: &["w", "x", "y"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::Streaming,
        name: "escape interrupting a turn",
        warm: &[],
        boundary: Boundary::Interrupt("esc"),
        keys: &["a", "b", "c"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::Streaming,
        name: "ctrl+c interrupting a turn",
        warm: &[],
        boundary: Boundary::Interrupt("ctrl+c"),
        keys: &["d", "e", "f"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::LongTurns,
        name: "wheel over a long transcript",
        warm: &["j"],
        boundary: Boundary::Wheel,
        keys: &["k", "l", "m"],
        want: Want::Lit,
    },
];

/// Claude's vim editor mode: NORMAL's motions stay dark, INSERT's typing lights.
const VIM: &[Scenario] = &[
    Scenario {
        prep: Prep::None,
        name: "typing in INSERT",
        warm: &["a", "b"],
        boundary: Boundary::None,
        keys: &["c", "d", "e"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::None,
        name: "escape, NORMAL motions",
        warm: &[],
        boundary: Boundary::Key("esc"),
        keys: &["h", "h", "h"],
        want: Want::Dark,
    },
    Scenario {
        prep: Prep::None,
        name: "i, typing in INSERT again",
        warm: &[],
        boundary: Boundary::Key("i"),
        keys: &["f", "g", "h"],
        want: Want::Lit,
    },
    Scenario {
        prep: Prep::None,
        name: "ctrl+[, NORMAL motions",
        warm: &[],
        boundary: Boundary::Key("ctrl+["),
        keys: &["h", "h", "h"],
        want: Want::Dark,
    },
    Scenario {
        prep: Prep::None,
        name: "a, typing in INSERT again",
        warm: &[],
        boundary: Boundary::Key("a"),
        keys: &["j", "k", "l"],
        want: Want::Lit,
    },
];

/// How long a wait for something that must happen may hang before the lane gives up on it: a
/// hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// How many long stub turns may run before the lane concludes the transcript cannot outgrow the
/// screen.
const MOST_TURNS: usize = 12;

/// The exit-3 refusal: the lane could not reach a verdict.
fn could_not(msg: impl std::fmt::Display) -> Fail {
    Fail::exit(3, format!("trail-keys: COULD NOT RUN: {msg}"))
}

/// The caret a `cursor` reply names, `(row, col)`, or `None` while a synchronized frame is
/// still open (its position is not the program's last word yet) or the reply is not one.
#[must_use]
pub fn caret(reply: &str) -> Option<(u16, u16)> {
    if reply.contains("frame=open") {
        return None;
    }
    let mut words = reply.split_whitespace().skip_while(|w| *w == "OK");
    Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
}

/// The transcript of a fullscreen agent's screen: the rows above the composer's top rule, which
/// sits on the row above the caret's.
#[must_use]
pub fn transcript(screen: &str, caret_row: u16) -> String {
    let rows: Vec<&str> = screen
        .lines()
        .take(usize::from(caret_row.saturating_sub(1)))
        .collect();
    rows.join("\n")
}

/// `toml` with Rainbow Kitty as its trail style: the style goes first, so a `LAB_ATERM_TOML` that
/// ends inside a table cannot capture it, and a top-level style of its own is dropped.
#[must_use]
pub fn styled(toml: &str) -> String {
    let mut in_table = false;
    let kept: Vec<&str> = toml
        .lines()
        .filter(|l| {
            in_table |= l.trim_start().starts_with('[');
            in_table
                || l.split_once('=')
                    .is_none_or(|(k, _)| k.trim() != "cursor_trail_style")
        })
        .collect();
    format!(
        "cursor_trail_style = \"rainbow kitty\"\n{}",
        kept.join("\n")
    )
}

/// Drives one lab's tab. Every `aterm ctl` call must answer OK; one that does not ends the run
/// with exit 3, since nothing after a refused key or a dead instance would mean anything.
struct Probe<'a> {
    door: Door<'a>,
}

impl Probe<'_> {
    /// `aterm ctl <args>`'s output, which must answer OK.
    fn must(&self, args: &[&str]) -> Result<String, Fail> {
        let what = args.join(" ");
        let r = self
            .door
            .ctl(args)
            .map_err(|e| could_not(format!("aterm ctl {what}: {e}")))?;
        if r.code != 0 {
            return Err(could_not(format!(
                "aterm ctl {what} answered {}",
                r.stderr.trim()
            )));
        }
        Ok(r.stdout)
    }

    /// One frame: a capture is what steps the headless instance's glow.
    fn frame(&self) -> Result<(), Fail> {
        self.must(&["image", "trail-keys.png"]).map(drop)
    }

    /// The caret, once no synchronized frame is open.
    fn caret(&self) -> Result<(u16, u16), Fail> {
        let start = std::time::Instant::now();
        loop {
            let reply = self.must(&["cursor"])?;
            if let Some(at) = caret(&reply) {
                return Ok(at);
            }
            if start.elapsed() > HANG {
                return Err(could_not(format!(
                    "the caret's frame stayed open for {} s: {}",
                    HANG.as_secs(),
                    reply.trim()
                )));
            }
            crate::stop::sleep(Duration::from_millis(30))?;
        }
    }

    /// Wait until `changed` holds, polling; whether it did before the hang detector ran out.
    fn until(&self, mut changed: impl FnMut(&Self) -> Result<bool, Fail>) -> Result<bool, Fail> {
        let start = std::time::Instant::now();
        loop {
            if changed(self)? {
                return Ok(true);
            }
            if start.elapsed() > HANG {
                return Ok(false);
            }
            crate::stop::sleep(Duration::from_millis(50))?;
        }
    }

    /// Press `key`, wait for its echo to move the caret along its row, and let two frames judge
    /// that one move: a frame per echo keeps two keys' moves from reaching the glow as one.
    fn press(&self, key: &str) -> Result<(), Fail> {
        let before = self.caret()?;
        self.must(&["key", key])?;
        let moved = self.until(|p| {
            let now = p.caret()?;
            Ok(now.0 == before.0 && now.1 != before.1 && p.caret()? == now)
        })?;
        if !moved {
            return Err(could_not(format!(
                "key {key} moved no caret along row {} within {} s",
                before.0,
                HANG.as_secs()
            )));
        }
        self.frame()?;
        self.frame()
    }

    /// Let the program's answer to a boundary land and be judged.
    fn settle(&self) -> Result<(), Fail> {
        for _ in 0..6 {
            self.frame()?;
        }
        crate::stop::sleep(Duration::from_millis(400))?;
        self.frame()?;
        self.frame()
    }

    fn admissions(&self, n: usize) -> Result<Vec<Admission>, Fail> {
        Ok(admissions(&self.must(&["trail", &n.to_string()])?))
    }

    fn last_seq(&self) -> Result<u64, Fail> {
        Ok(self.admissions(1)?.last().map_or(0, |a| a.seq))
    }

    fn mouse(&self, args: &[&str]) -> Result<(), Fail> {
        let mut v = vec!["mouse"];
        v.extend_from_slice(args);
        self.must(&v)?;
        self.frame()
    }

    /// Apply `b`: why it did not happen, when it did not (the scenario then fails rather than
    /// pass on plain typing). A key boundary happened when aterm took the key; a drag when
    /// Claude painted its selection over the path; the wheel when Claude scrolled its transcript.
    fn apply(&self, b: Boundary) -> Result<Option<String>, Fail> {
        match b {
            Boundary::None => Ok(None),
            Boundary::Key(key) => self.must(&["key", key]).map(|_| None),
            Boundary::Interrupt(key) => {
                let busy = busy_row();
                if !self.door.await_match(&busy, 1)? {
                    return Ok(Some(format!("the turn had ended before {key}")));
                }
                self.must(&["key", key])?;
                let stopped = self.door.await_gone(&busy, 60)?;
                Ok((!stopped).then(|| format!("Claude kept streaming after {key}")))
            }
            Boundary::Wheel => {
                let (row, _) = self.caret()?;
                let before = transcript(&self.must(&["text"])?, row);
                for _ in 0..3 {
                    self.mouse(&["wheelup", "left", "2", "10"])?;
                }
                let scrolled =
                    self.until(|p| Ok(transcript(&p.must(&["text"])?, row) != before))?;
                for _ in 0..3 {
                    self.mouse(&["wheeldown", "left", "2", "10"])?;
                }
                Ok((!scrolled).then(|| "the wheel scrolled no transcript row".to_string()))
            }
            Boundary::DragPath => {
                let Some((row, first, last)) = path_span(&self.must(&["text"])?) else {
                    return Ok(Some("no path on the screen to drag over".to_string()));
                };
                let (r, c0, c1) = (row.to_string(), first.to_string(), last.to_string());
                let cell = |p: &Self| p.must(&["cell", &r, &c0]);
                let before = cell(self)?;
                self.mouse(&["press", "left", &r, &c0])?;
                let mut col = first;
                while col < last {
                    col = (col + 4).min(last);
                    self.mouse(&["move", "left", &r, &col.to_string()])?;
                }
                let painted = self.until(|p| Ok(cell(p)? != before))?;
                self.mouse(&["release", "left", &r, &c1])?;
                Ok((!painted).then(|| "Claude painted no selection over the path".to_string()))
            }
        }
    }

    /// The lab's `modes`, which must still show Claude Code fullscreen with its caret drawn.
    fn fullscreen(&self, when: &str) -> Result<String, Fail> {
        let modes = self.must(&["modes"])?;
        if modes.contains("alt_screen=true") && modes.contains("cursor_visible=true") {
            return Ok(modes);
        }
        Err(could_not(format!(
            "Claude Code is not fullscreen with its caret drawn {when}, so no row can park: {}",
            modes.split_whitespace().collect::<Vec<_>>().join(" ")
        )))
    }
}

/// Launch the managed Claude Code fullscreen with its caret drawn, wait for its footer, and
/// check what the scenarios stand on: the mouse reaches Claude, the trail is Rainbow Kitty, and
/// (`--vim`) the composer starts in INSERT.
fn launch(run: &Run, p: &Probe<'_>, vim: bool) -> Result<(), Fail> {
    let l = Launch {
        no_wait: true,
        envs: vec![
            "CLAUDE_CODE_NO_FLICKER=1".into(),
            "CLAUDE_CODE_NATIVE_CURSOR=1".into(),
        ],
        ..Launch::default()
    };
    crate::lab::launch_claude(run, &l)?;
    let door = &p.door;
    if door.await_match("shift.tab|for.agents|shortcuts|Yes,.I.accept", 60)?
        && p.must(&["text"])?.contains("Yes, I accept")
    {
        p.must(&["key", "down"])?;
        p.must(&["key", "enter"])?;
    }
    if !door.await_match("shift.tab|for.agents|shortcuts", 60)? {
        return Err(could_not("Claude Code drew no footer within 60 s"));
    }
    door.await_idle(2000, 30)?;
    let modes = p.fullscreen("after its launch")?;
    if modes.contains("mouse_mode=none") || !modes.contains("mouse_mode=") {
        return Err(could_not(
            "Claude Code is not tracking the mouse, so a drag or the wheel never reaches it",
        ));
    }
    let status = p.must(&["trail", "status"])?;
    if !status.contains("resolved=rainbow-kitty") {
        return Err(could_not(format!(
            "the trail is not Rainbow Kitty: {}",
            status
                .split_whitespace()
                .filter(|w| w.starts_with("style=") || w.starts_with("resolved="))
                .collect::<Vec<_>>()
                .join(" ")
        )));
    }
    if vim && !door.await_match(&aterm_phase::anchors::guard_prefix("vim.insert", 3), 30)? {
        return Err(could_not(
            "Claude Code shows no -- INSERT -- although its editorMode is vim",
        ));
    }
    Ok(())
}

/// Claude's busy row (`esc to interrupt`), as an `await` pattern.
fn busy_row() -> String {
    aterm_phase::anchors::guard_prefix("busy.interrupt", 3)
}

/// Erase what the scenarios before typed, so the composer holds only a turn's prompt.
fn clear_composer(run: &Run, p: &Probe<'_>) -> Result<(), Fail> {
    let typed = crate::screen::composer_row(run.agent(), &p.must(&["text"])?).unwrap_or_default();
    for _ in 0..typed.chars().count() + 2 {
        p.must(&["key", "backspace"])?;
    }
    Ok(())
}

/// Start a slow stub turn and wait for Claude's busy row; the stub goes back to its short reply
/// once the scenario's boundary has interrupted it.
fn streaming_turn(run: &Run, p: &Probe<'_>) -> Result<(), Fail> {
    clear_composer(run, p)?;
    crate::verbs::set_mode(run, "slow")?;
    if !crate::verbs::turn(&p.door, "count slowly")? {
        return Err(could_not("a slow turn was not submitted"));
    }
    if !p.door.await_match(&busy_row(), 60)? {
        return Err(could_not("a slow turn showed no busy row within 60 s"));
    }
    Ok(())
}

/// Stub turns with the long canned reply until the welcome banner has scrolled off the screen,
/// then the stub back to its short reply; why the transcript is still short, when it is.
fn long_turns(run: &Run, p: &Probe<'_>) -> Result<Option<String>, Fail> {
    clear_composer(run, p)?;
    crate::verbs::set_mode(run, "long")?;
    let busy = busy_row();
    let mut short = true;
    for _ in 0..MOST_TURNS {
        short = p.must(&["text"])?.contains("Claude Code v");
        if !short {
            break;
        }
        if !crate::verbs::turn(&p.door, "tell me something long")? {
            return Err(could_not("a long turn was not submitted"));
        }
        if !p.door.await_gone(&busy, 120)? || !p.door.await_idle(1000, 60)? {
            return Err(could_not("a long turn did not finish within 120 s"));
        }
        short = p.must(&["text"])?.contains("Claude Code v");
    }
    crate::verbs::set_mode(run, "ok")?;
    Ok(
        short
            .then(|| format!("the transcript never outgrew the screen in {MOST_TURNS} long turns")),
    )
}

fn probe(run: &Run, vim: bool, keep: bool) -> Res {
    if vim {
        crate::stats::set_claude_json(run, "editorMode", J::s("vim"))?;
    }
    let p = Probe {
        door: Door::new(run)?,
    };
    launch(run, &p, vim)?;
    println!(
        "trail-keys: Claude Code {}, fullscreen with its caret drawn, {} editor",
        run.claude_ver,
        if vim { "vim" } else { "default" }
    );
    let mut failed = 0;
    for s in if vim { VIM } else { PLAIN } {
        let short = match s.prep {
            Prep::None => None,
            Prep::LongTurns => long_turns(run, &p)?,
            Prep::Streaming => {
                streaming_turn(run, &p)?;
                None
            }
        };
        for key in s.warm {
            p.press(key)?;
        }
        p.settle()?;
        let idle = match short {
            Some(why) => Some(why),
            None => p.apply(s.boundary)?,
        };
        if s.prep == Prep::Streaming {
            crate::verbs::set_mode(run, "ok")?;
        }
        p.settle()?;
        // Only the keys' own admissions: a boundary's answer (vim's caret step back on Escape)
        // books one of its own.
        let since = p.last_seq()?;
        for key in s.keys {
            p.press(key)?;
        }
        // A refused move is booked a moment after its frames.
        p.settle()?;
        p.fullscreen(&format!("after '{}'", s.name))?;
        let all = p.admissions(32)?;
        let (keys_ok, words) = judged(&all, since, s.keys.len(), s.want);
        let ok = keys_ok && idle.is_none();
        println!(
            "  {:<5} {:<30} want {:<4}  {}{}",
            if ok { "ok" } else { "FAIL" },
            s.name,
            s.want.word(),
            if words.is_empty() {
                "no admissions".to_string()
            } else {
                words.join(" ")
            },
            idle.map(|why| format!("  (not exercised: {why})"))
                .unwrap_or_default()
        );
        if !ok {
            failed += 1;
            for a in all.iter().filter(|a| a.seq > since) {
                println!("          {}", a.line);
            }
        }
    }
    if failed == 0 {
        println!("trail-keys: every scenario reads as expected");
        return Ok(0);
    }
    let tag = &run.tag;
    println!(
        "trail-keys: {failed} scenario(s) did not; {}",
        if keep {
            format!("the lab is kept: targo --unverified lab ctl {tag} trail 32")
        } else {
            "rerun with --keep to look at the lab".to_string()
        }
    );
    Ok(1)
}

/// Whether `LAB_LINES` leaves the screen's height to this lane (unset or empty, as
/// [`UpOpts::from_env`] reads it).
fn lines_unset(v: Option<&str>) -> bool {
    v.is_none_or(str::is_empty)
}

/// Runs `lab trail-keys`.
pub fn cmd_trail_keys(args: &[String]) -> Res {
    let mut vim = false;
    let mut keep = false;
    for a in args {
        match a.as_str() {
            "--vim" => vim = true,
            "--keep" => keep = true,
            other => {
                return Err(Fail::usage(format!(
                    "trail-keys: unknown argument {other} (usage: trail-keys [--vim] [--keep])"
                )));
            }
        }
    }
    crate::stop::install();
    let tag = format!("trail-keys-{}", std::process::id());
    let mut u = UpOpts::from_env();
    u.trust = true;
    u.harness_keys = "enabled = false".into();
    u.lifeline = !keep;
    if lines_unset(std::env::var("LAB_LINES").ok().as_deref()) {
        // Short enough that a long stub turn or two overflows it, so the wheel scrolls.
        u.lines = "24".into();
    }
    u.aterm_toml = styled(&u.aterm_toml);
    // Every refusal before a verdict is the lane's, not the build's: exit 3, not 1.
    let infra = |f: Fail| {
        if f.stopped.is_some() || f.code == 3 {
            f
        } else {
            could_not(f.msg.unwrap_or_default())
        }
    };
    let run = crate::lab::up(&tag, &u).map_err(infra)?;
    let rc = probe(&run, vim, keep).map_err(infra);
    if keep {
        eprintln!("trail-keys: kept lab '{tag}' (targo --unverified lab down {tag})");
    } else {
        let _ = crate::lab::down(&tag, false);
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAIL: &str = "aterm-ctl: OK 3 trail\n\
        admission seq=7 phase=licensed reason=licensed age_ms=213 origin=47,4 target=47,5 alt=1 licence=key source=bound source_view=- source_session=-\n\
        admission seq=8 phase=declined reason=program-row age_ms=103 origin=47,5 target=47,6 alt=1 licence=none source=bound source_view=- source_session=-\n\
        admission seq=9 phase=licensed reason=licensed age_ms=3 origin=47,6 target=47,7 alt=1 licence=key source=bound source_view=- source_session=-\n";

    #[test]
    fn the_admission_lines_are_read_in_order() {
        let all = admissions(TRAIL);
        assert_eq!(all.iter().map(|a| a.seq).collect::<Vec<_>>(), [7, 8, 9]);
        assert_eq!(all[1].phase, "declined");
        assert_eq!(all[1].reason, "program-row");
        assert!(admissions("aterm-ctl: OK 0 trail\n").is_empty());
    }

    #[test]
    fn lit_wants_every_key_licensed_and_dark_wants_none() {
        let all = admissions(TRAIL);
        assert!(judged(&all, 8, 1, Want::Lit).0);
        assert!(!judged(&all, 6, 3, Want::Lit).0, "one key was refused");
        assert!(
            !judged(&all, 8, 2, Want::Lit).0,
            "one admission where two keys were typed"
        );
        assert!(judged(&all, 7, 1, Want::Lit).0, "the last key after seq 7");
        assert!(!judged(&all, 6, 3, Want::Dark).0, "two keys lit");
        assert!(judged(&all[..2], 7, 1, Want::Dark).0);
        assert!(
            !judged(&all, 9, 1, Want::Dark).0,
            "no admission is no verdict"
        );
        assert_eq!(
            judged(&all, 6, 3, Want::Lit).1,
            ["licensed", "program-row", "licensed"]
        );
    }

    #[test]
    fn a_failed_scenario_keeps_each_admission_s_whole_line() {
        let all = admissions(TRAIL);
        assert!(all[1].line.starts_with("admission seq=8 phase=declined"));
        assert!(all[1].line.ends_with("source_session=-"));
    }

    #[test]
    fn the_caret_is_read_once_its_frame_is_closed() {
        assert_eq!(caret("OK 21 6 1 blinking_block\n"), Some((21, 6)));
        assert_eq!(caret("21 6 1 steady_bar"), Some((21, 6)));
        assert_eq!(caret("OK 21 6 1 blinking_block frame=open\n"), None);
        assert_eq!(caret("ERR no focused window"), None);
        assert_eq!(caret(""), None);
    }

    #[test]
    fn the_transcript_is_the_rows_above_the_composer_s_rule() {
        let screen = "a\nb\nc\n────\n❯ typed\n────\nfooter\n";
        assert_eq!(transcript(screen, 4), "a\nb\nc");
        assert_eq!(transcript(screen, 0), "");
        assert_eq!(transcript(screen, 1), "");
    }

    #[test]
    fn rainbow_kitty_leads_the_lab_s_toml_and_replaces_its_own_style() {
        assert_eq!(styled(""), "cursor_trail_style = \"rainbow kitty\"\n");
        let toml =
            "theme = \"x\"\ncursor_trail_style = \"comet\"\n[render]\ncursor_trail_style = 1";
        assert_eq!(
            styled(toml),
            "cursor_trail_style = \"rainbow kitty\"\ntheme = \"x\"\n[render]\ncursor_trail_style = 1"
        );
    }

    #[test]
    fn an_empty_lab_lines_leaves_the_height_to_the_lane() {
        assert!(lines_unset(None));
        assert!(lines_unset(Some("")));
        assert!(!lines_unset(Some("50")));
    }

    #[test]
    fn the_welcome_s_path_is_found_with_its_columns() {
        let screen = "\n │ ✻ Welcome to Claude Code!   │\n │   cwd: /tmp/lab/work        │\n";
        let (row, first, last) = path_span(screen).unwrap();
        assert_eq!(row, 2);
        let line: Vec<char> = screen.lines().nth(2).unwrap().chars().collect();
        assert_eq!(
            line[first..=last].iter().collect::<String>(),
            "/tmp/lab/work"
        );
        let boxed = "│~/src/aterm│";
        let (_, first, last) = path_span(boxed).unwrap();
        let cells: Vec<char> = boxed.chars().collect();
        assert_eq!(
            cells[first..=last].iter().collect::<String>(),
            "~/src/aterm"
        );
        assert_eq!(path_span("no path here\n"), None);
    }
}
