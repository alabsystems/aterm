// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live predictive-editing harness over a real aterm and link relay
//! ([`crate::link`], [`MUST_PAINT_RTT_MS`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::instance::{self, Instance, Launch, World};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// One key of the corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable character.
    Ch(char),
    /// A named key (`hwkey`'s and `key`'s names: `left`, `backspace`, `enter`…).
    Named(&'static str),
    /// Control and a letter.
    Ctrl(char),
}

impl Key {
    /// The `hwkey` request that presses it (the OS event queue's path).
    #[must_use]
    pub fn hwkey(self) -> String {
        match self {
            Key::Ch(' ') => "hwkey space".to_string(),
            Key::Ch(c) => format!("hwkey {c}"),
            Key::Named(n) => format!("hwkey {n}"),
            Key::Ctrl(c) => format!("hwkey {c} mods=ctrl"),
        }
    }

    /// The `key` request that presses it.
    #[must_use]
    pub fn key(self) -> String {
        match self {
            Key::Ch(' ') => "key space".to_string(),
            Key::Ch(c) => format!("key {c}"),
            Key::Named(n) => format!("key {n}"),
            Key::Ctrl(c) => format!("key ctrl+{c}"),
        }
    }

    const fn is_glyph(self) -> bool {
        matches!(self, Key::Ch(_))
    }
}

/// One command line of the corpus.
#[derive(Clone, Debug)]
pub struct Line {
    pub family: &'static str,
    pub keys: Vec<Key>,
}

/// The plan's editing kinds, each a harmless command line whatever the line editor's bindings.
#[must_use]
pub fn corpus(rounds: usize) -> Vec<Line> {
    use Key::{Ch, Ctrl, Named};
    let t = |s: &str| s.chars().map(Ch).collect::<Vec<_>>();
    let (l, r, bs) = (Named("left"), Named("right"), Named("backspace"));
    let cat = |parts: Vec<Vec<Key>>| {
        let mut keys: Vec<Key> = parts.into_iter().flatten().collect();
        keys.push(Named("enter"));
        keys
    };
    let one = || {
        vec![
            Line {
                family: "plain",
                keys: cat(vec![t("echo hello world")]),
            },
            Line {
                family: "typo-end",
                keys: cat(vec![t("echo helo"), vec![bs], t("lo world")]),
            },
            Line {
                family: "left-right",
                keys: cat(vec![
                    t("echo wrold"),
                    vec![l, l, l, bs, r, Ch('r'), r, r],
                    t(" ok"),
                ]),
            },
            Line {
                family: "mid-insert",
                keys: cat(vec![t("echo ac"), vec![l, Ch('b'), r], t(" d")]),
            },
            Line {
                family: "mid-erase",
                keys: cat(vec![t("echo abxc"), vec![l, bs, r], t(" d")]),
            },
            Line {
                family: "ctrl-d",
                keys: cat(vec![t("echo abxc"), vec![l, l, Ctrl('d'), r], t(" d")]),
            },
            Line {
                family: "delete",
                keys: cat(vec![
                    t("echo abxc"),
                    vec![l, l, Named("delete"), r],
                    t(" d"),
                ]),
            },
            Line {
                family: "home-end",
                keys: cat(vec![
                    t("echo hi"),
                    vec![Named("home")],
                    t("true; "),
                    vec![Named("end")],
                    t(" there"),
                ]),
            },
            Line {
                family: "ctrl-a-e",
                keys: cat(vec![
                    t("echo b"),
                    vec![Ctrl('a')],
                    t("true; "),
                    vec![Ctrl('e')],
                    t(" c"),
                ]),
            },
            Line {
                family: "typo-back",
                keys: cat(vec![
                    t("echo predicton"),
                    vec![l, l, Ch('i'), Ctrl('e')],
                    t(" ok"),
                ]),
            },
        ]
    };
    (0..rounds).flat_map(|_| one()).collect()
}

/// How long after the previous key a key is pressed.
#[must_use]
pub fn gap(prev: Option<Key>, key: Key, n: usize) -> Duration {
    const CADENCE: [u64; 4] = [90, 130, 110, 150];
    let base = CADENCE[n % CADENCE.len()];
    match (prev, key) {
        (None, _) => Duration::ZERO,
        (Some(p), k) if p.is_glyph() && !k.is_glyph() => Duration::from_millis(350),
        (_, Key::Named("enter")) => Duration::from_millis(300),
        _ => Duration::from_millis(base),
    }
}

/// A press the predictor was handed (`aterm-predict press`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Press {
    pub t: u64,
    pub key: String,
    pub armed: Option<u64>,
}

/// A frame that painted a prediction (`aterm-predict frame`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub t: u64,
    pub mode: String,
    pub newest: Option<u64>,
    /// The engine's cursor, and the one drawn.
    pub cursor: (usize, usize),
    pub shown: (usize, usize),
    /// Each row the paint touched.
    pub rows: Vec<(usize, String, String)>,
}

/// One trace line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Press(Press),
    Frame(Frame),
    /// The frame after a painted one that painted none.
    Clear(u64),
}

/// The trace's text field, percent-decoded (`predict_trace::encode`).
#[must_use]
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn pair(s: &str) -> Option<(usize, usize)> {
    let (a, b) = s.split_once(',')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn seq(s: &str) -> Option<u64> {
    s.parse().ok()
}

/// Every trace line of `log`, in order.
#[must_use]
pub fn parse_trace(log: &str) -> Vec<Event> {
    let mut out = Vec::new();
    for line in log.lines() {
        let Some(rest) = line.strip_prefix("aterm-predict ") else {
            continue;
        };
        let mut words = rest.split(' ');
        let kind = words.next().unwrap_or("");
        let fields: Vec<(&str, &str)> = words.filter_map(|w| w.split_once('=')).collect();
        let get = |k: &str| fields.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        let t = get("t").and_then(|v| v.parse().ok()).unwrap_or(0);
        match kind {
            "press" => out.push(Event::Press(Press {
                t,
                key: decode(get("key").unwrap_or("")),
                armed: get("armed").and_then(seq),
            })),
            "frame" => {
                let mut rows = Vec::new();
                let mut row = None;
                let mut under = None;
                for (k, v) in &fields {
                    match *k {
                        "row" => row = v.parse().ok(),
                        "under" => under = Some(decode(v)),
                        "over" => {
                            if let (Some(r), Some(u)) = (row.take(), under.take()) {
                                rows.push((r, u, decode(v)));
                            }
                        }
                        _ => {}
                    }
                }
                out.push(Event::Frame(Frame {
                    t,
                    mode: get("mode").unwrap_or("").to_string(),
                    newest: get("newest").and_then(seq),
                    cursor: get("cursor").and_then(pair).unwrap_or_default(),
                    shown: get("shown").and_then(pair).unwrap_or_default(),
                    rows,
                }));
            }
            "clear" => out.push(Event::Clear(t)),
            _ => {}
        }
    }
    out
}

/// One line of a link take ([`crate::link`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// Input the far end was handed.
    In(Vec<u8>),
    /// Output it wrote.
    Out(Vec<u8>),
    /// The window it was told.
    Size(u16, u16),
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A take's lines, in order.
#[must_use]
pub fn parse_take(text: &str) -> Vec<Chunk> {
    text.lines()
        .filter_map(|line| {
            let mut w = line.splitn(3, ' ');
            let _t = w.next()?;
            let way = w.next()?;
            let body = w.next().unwrap_or("");
            match way {
                "I" => unhex(body).map(Chunk::In),
                "O" => unhex(body).map(Chunk::Out),
                "W" => {
                    let (r, c) = body.split_once('x')?;
                    Some(Chunk::Size(r.parse().ok()?, c.parse().ok()?))
                }
                _ => None,
            }
        })
        .collect()
}

/// A screen: its visible rows (trailing blanks trimmed) and its cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    pub rows: Vec<String>,
    pub cursor: (usize, usize),
}

impl Screen {
    fn of(t: &aterm_core::terminal::Terminal) -> Screen {
        let rows = usize::from(t.rows());
        let c = t.cursor();
        Screen {
            rows: (0..rows)
                .map(|r| trim(&t.row_text(r).unwrap_or_default()))
                .collect(),
            cursor: (usize::from(c.row), usize::from(c.col)),
        }
    }

    fn row(&self, r: usize) -> &str {
        self.rows.get(r).map_or("", String::as_str)
    }
}

fn trim(s: &str) -> String {
    s.trim_end_matches([' ', '\0']).to_string()
}

/// The far end's screen just before it read each input of `take` (one per `In` chunk, in order).
#[must_use]
pub fn replay(take: &[Chunk]) -> Vec<Screen> {
    let mut term: Option<aterm_core::terminal::Terminal> = None;
    let mut out = Vec::new();
    for chunk in take {
        match chunk {
            Chunk::Size(r, c) => match &mut term {
                Some(t) => t.resize(*r, *c),
                None => term = Some(aterm_core::terminal::Terminal::new(*r, *c)),
            },
            Chunk::Out(bytes) => {
                term.get_or_insert_with(|| aterm_core::terminal::Terminal::new(24, 80))
                    .process(bytes);
            }
            Chunk::In(_) => out.push(term.as_ref().map(Screen::of).unwrap_or_default()),
        }
    }
    out.push(term.as_ref().map(Screen::of).unwrap_or_default());
    out
}

/// Frame verdicts and timing metrics for one phase.
#[derive(Debug, Default)]
pub struct Verdict {
    /// Frames that painted, and of them the `adaptive` ones.
    pub frames: usize,
    pub adaptive: usize,
    /// Frames held to their settled screen.
    pub checked: usize,
    /// Frames whose newest key no press names.
    pub unattributed: usize,
    /// Each wrong frame, described.
    pub wrong: Vec<String>,
    /// Correct frames ahead of the echo.
    pub ahead: usize,
    /// Keys counted (every press but a line terminator).
    pub keys: HashMap<String, (usize, usize)>,
    /// The press to the first frame ahead of the echo showing it, µs.
    pub perceived_us: Vec<u64>,
    /// Painted frames still on glass when a line had settled ([`left_on_glass`]).
    pub left_on_glass: Vec<String>,
}

/// Eventual, on glass: once a line's output has landed and the screen has settled.
#[must_use]
pub fn left_on_glass(events: &[Event]) -> Vec<String> {
    let mut out = Vec::new();
    let mut shown: Option<&Frame> = None;
    let mut line_start = true;
    for e in events {
        match e {
            Event::Frame(f) => shown = Some(f),
            Event::Clear(_) => shown = None,
            Event::Press(p) => {
                if line_start && let Some(f) = shown {
                    out.push(format!(
                        "the frame of t={}us (rows {:?}) was still on glass at the next line's first key (t={}us)",
                        f.t, f.rows, p.t
                    ));
                }
                line_start = p.key == "submit";
            }
        }
    }
    if let Some(f) = shown {
        out.push(format!(
            "the frame of t={}us (rows {:?}) was still on glass at the end",
            f.t, f.rows
        ));
    }
    out
}

/// A press's class: `glyph`, an edit key's name, `submit`, `unmodelled`.
#[must_use]
pub fn class(key: &str) -> String {
    if key.starts_with("glyph") {
        "glyph".to_string()
    } else if let Some(k) = key.strip_prefix("edit ") {
        k.to_string()
    } else {
        key.split(' ').next().unwrap_or(key).to_string()
    }
}

/// Whether `frame`'s engine side (the rows it touched as the engine had them,
/// and the engine's cursor) is already `screen`.
fn engine_is(frame: &Frame, screen: &Screen) -> bool {
    frame.cursor == screen.cursor
        && frame
            .rows
            .iter()
            .all(|(r, under, _)| trim(under) == screen.row(*r))
}

/// Hold every frame of a phase to the screen its newest key settles to.
#[must_use]
pub fn judge(presses: &[Press], frames: &[Frame], settled: &[Screen]) -> Verdict {
    let mut v = Verdict::default();
    let by_seq: HashMap<u64, usize> = presses
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.armed.map(|s| (s, i)))
        .collect();
    let mut line_start = Vec::with_capacity(presses.len());
    let mut start = 0;
    for (i, p) in presses.iter().enumerate() {
        line_start.push(start);
        if p.key == "submit" {
            start = i + 1;
        }
    }
    let mut early: Vec<Option<u64>> = vec![None; presses.len()];
    for f in frames {
        v.frames += 1;
        if f.mode == "adaptive" {
            v.adaptive += 1;
        }
        let Some(&k) = f.newest.and_then(|s| by_seq.get(&s)) else {
            v.unattributed += 1;
            continue;
        };
        let Some(screen) = settled.get(k) else {
            v.unattributed += 1;
            continue;
        };
        v.checked += 1;
        let mut cells = Vec::new();
        for (r, _, over) in &f.rows {
            let want = screen.row(*r);
            let got = trim(over);
            if got != want {
                cells.push(format!("row {r}: drawn {got:?}, settles to {want:?}"));
            }
        }
        if f.shown != screen.cursor {
            cells.push(format!(
                "cursor drawn at {:?}, settles to {:?}",
                f.shown, screen.cursor
            ));
        }
        if !cells.is_empty() {
            v.wrong.push(format!(
                "t={}us {} key {k} ({}): {}",
                f.t,
                f.mode,
                presses[k].key,
                cells.join("; ")
            ));
            continue;
        }
        if engine_is(f, screen) {
            continue;
        }
        v.ahead += 1;
        for q in (line_start[k]..=k).rev() {
            if presses[q].key == "submit" || settled.get(q).is_none_or(|s| engine_is(f, s)) {
                break;
            }
            if early[q].is_none() && f.t >= presses[q].t {
                early[q] = Some(f.t - presses[q].t);
            }
        }
    }
    for (p, e) in presses.iter().zip(&early) {
        if p.key == "submit" {
            continue;
        }
        let entry = v.keys.entry(class(&p.key)).or_default();
        entry.0 += 1;
        if let Some(us) = e {
            entry.1 += 1;
            v.perceived_us.push(*us);
        }
    }
    v
}

/// The `q`-quantile of `xs` (nearest rank), `0` when empty.
#[must_use]
pub fn quantile(xs: &[u64], q: f64) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    let mut s = xs.to_vec();
    s.sort_unstable();
    let rank = ((q * s.len() as f64).ceil() as usize).clamp(1, s.len());
    s[rank - 1]
}

/// One configuration of a run.
#[derive(Clone, Debug)]
struct Config {
    shell: String,
    rtt: u64,
    on: bool,
}

/// Whether the desk is busy: a person touched the keyboard or pointer within `since`.
fn person_since(since: Instant) -> bool {
    crate::meta_keys::hid_idle().is_none_or(|idle| idle < since.elapsed())
}

fn sleep(d: Duration) -> Result<(), Stopped> {
    stop::sleep(d)
}

/// The metrics a phase reports, from `metrics percentiles`.
const METRIC_KEYS: [&str; 13] = [
    "n_input",
    "input_predicted",
    "input_p50_ms",
    "input_p95_ms",
    "input_p99_ms",
    "n_present",
    "present_p50_ms",
    "present_p95_ms",
    "present_p99_ms",
    "n_echo",
    "echo_p50_ms",
    "echo_p95_ms",
    "echo_p99_ms",
];

/// The value of field `key` of a `metrics` reply, as printed.
fn metric<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

fn metric_line(reply: &str) -> String {
    METRIC_KEYS
        .iter()
        .map(|k| format!("{k}={}", metric(reply, k).unwrap_or("?")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The round trip at or above which a windowed phase with prediction on must
/// paint: twice the 20 ms at which `adaptive` opens its display gate
/// (aterm-predict's `DISPLAY_OPEN_MS`), so the link stands clear of the
/// gate's own threshold.
const MUST_PAINT_RTT_MS: u64 = 40;

/// The instance's screen as `text` prints it: its rows, trailing blanks trimmed.
fn text_rows(inst: &Instance, sid: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(s) = sid {
        args.push(format!("@{s}"));
    }
    args.push("text".to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    inst.ctl(&argv).stdout.lines().map(trim).collect()
}

/// Whether `rows` (the instance's `text`) are the replay's `screen`.
fn same_screen(rows: &[String], screen: &Screen) -> bool {
    let n = rows.len().max(screen.rows.len());
    (0..n).all(|r| rows.get(r).map_or("", String::as_str) == screen.row(r))
}

/// Every take in `dir`, read now: (its path, its lines).
fn read_takes(dir: &Path) -> Vec<(PathBuf, Vec<Chunk>)> {
    let mut takes: Vec<(PathBuf, Vec<Chunk>)> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "take"))
                .map(|p| {
                    let lines = parse_take(&std::fs::read_to_string(&p).unwrap_or_default());
                    (p, lines)
                })
                .collect()
        })
        .unwrap_or_default();
    takes.sort_by(|a, b| a.0.cmp(&b.0));
    takes
}

fn inputs(take: &[Chunk]) -> usize {
    take.iter().filter(|c| matches!(c, Chunk::In(_))).count()
}

/// The one take whose far end read input between `before` and `after` (the
/// tab the corpus was typed into: aterm also keeps a spare shell, behind a
/// relay of its own, which reads nothing), with its lines at the start.
fn grown(
    before: &[(PathBuf, Vec<Chunk>)],
    after: Vec<(PathBuf, Vec<Chunk>)>,
) -> Result<(Vec<Chunk>, Vec<Chunk>), String> {
    let mut grew: Vec<(Vec<Chunk>, Vec<Chunk>)> = after
        .into_iter()
        .filter_map(|(path, now)| {
            let then = before
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, t)| t.clone())
                .unwrap_or_default();
            (inputs(&now) > inputs(&then)).then_some((then, now))
        })
        .collect();
    match grew.len() {
        1 => Ok(grew.remove(0)),
        n => Err(format!("{n} far ends read the corpus's input, not one")),
    }
}

/// What one phase measured, for the report.
struct Phase {
    name: &'static str,
    verdict: Option<Verdict>,
    metrics: String,
    /// `input_predicted`: the keys whose input prediction on glass closed, before their echo.
    predicted: Option<u64>,
    oracle: String,
    notes: Vec<String>,
    /// Prediction off: whether the phase held.
    off_held: bool,
}

struct Run<'a> {
    inst: Instance,
    client: aterm_agent::RelayClient<aterm_uds::CtlStream>,
    headless: bool,
    /// Prediction on (`adaptive`).
    on: bool,
    rtt: u64,
    rounds: usize,
    log: PathBuf,
    takes: PathBuf,
    out: &'a Path,
    tag: String,
}

/// Why a phase was not judged.
enum Halt {
    Stopped(Stopped),
    NotMeasured(String),
    Broken(String),
}

impl From<Stopped> for Halt {
    fn from(s: Stopped) -> Self {
        Halt::Stopped(s)
    }
}

impl Run<'_> {
    fn req(&mut self, line: &str) -> Result<String, Halt> {
        self.client
            .request_line(line)
            .map_err(|e| Halt::Broken(format!("{line}: {e}")))
    }

    /// [`Run::req`] for a step the phase counts on: any reply but an `OK` (an `ERR`) is BROKEN,
    /// never the step taken as done.
    fn req_ok(&mut self, line: &str) -> Result<String, Halt> {
        let reply = self.req(line)?;
        if reply.starts_with("OK") {
            Ok(reply)
        } else {
            Err(Halt::Broken(format!("{line}: {reply}")))
        }
    }

    /// Press `key`; a press the window did not take is NOT MEASURED.
    fn press(&mut self, key: Key) -> Result<(), Halt> {
        let line = if self.headless {
            key.key()
        } else {
            key.hwkey()
        };
        let reply = self.req_ok(&line)?;
        if !self.headless && !reply.contains("key=1") {
            return Err(Halt::NotMeasured(format!(
                "{line}: {reply}: the window was not the key window of the active app"
            )));
        }
        Ok(())
    }

    /// Wait until the tab has written nothing for `ms`.
    fn settle(&mut self, ms: u64) -> Result<(), Halt> {
        self.req_ok(&format!("await idle {ms} timeout=60000"))?;
        Ok(())
    }

    /// Type the corpus: each line's keys at a typist's cadence, then wait for
    /// its output and the next prompt.
    fn type_corpus(&mut self, image: bool) -> Result<(), Halt> {
        let lines = corpus(self.rounds);
        let mut n = 0;
        for (i, line) in lines.iter().enumerate() {
            let started = Instant::now();
            let mut prev = None;
            for (j, &key) in line.keys.iter().enumerate() {
                sleep(gap(prev, key, n))?;
                self.press(key)?;
                prev = Some(key);
                n += 1;
                if image && i == 10 && j == 9 {
                    self.capture()?;
                }
            }
            self.settle(400)?;
            sleep(Duration::from_millis(self.rtt))?;
            self.settle(300)?;
            if !self.headless && person_since(started) {
                return Err(Halt::NotMeasured(format!(
                    "a person touched the keyboard or pointer during line {i} ({})",
                    line.family
                )));
            }
        }
        Ok(())
    }

    /// `image` now, mid-line, copied to the output directory.
    fn capture(&mut self) -> Result<(), Halt> {
        let reply = self.inst.ctl(&["image"]).first();
        let path = reply
            .split_whitespace()
            .nth(3)
            .unwrap_or_default()
            .to_string();
        if reply.starts_with("OK") && !path.is_empty() {
            let dest = self.out.join(format!("frame-{}.png", self.tag));
            let _ = std::fs::copy(&path, &dest);
            println!(
                "      image mid-line: {} -> {}",
                reply.trim(),
                dest.display()
            );
        }
        Ok(())
    }

    fn log_len(&self) -> usize {
        std::fs::read_to_string(&self.log).map_or(0, |s| s.len())
    }

    /// One phase: settle, read the oracle's start, reset the metrics, type,
    /// settle, read the metrics, the trace and the take, and judge. A reset
    /// the instance refused is BROKEN: the counters would still hold the
    /// phase before, whose `input_predicted` would stand in for this one's.
    fn phase(&mut self, name: &'static str, sid: Option<&str>, image: bool) -> Result<Phase, Halt> {
        self.settle(800)?;
        let mut notes = Vec::new();
        let takes_at_start = read_takes(&self.takes);
        let text_at_start = text_rows(&self.inst, sid);
        let log_offset = self.log_len();
        self.req_ok("metrics reset")?;
        self.type_corpus(image)?;
        self.settle(800)?;
        let percentiles = self.req("metrics percentiles")?;
        let metrics = metric_line(&percentiles);
        let predicted = metric(&percentiles, "input_predicted").and_then(|v| v.parse().ok());
        let (at_start, whole) =
            grown(&takes_at_start, read_takes(&self.takes)).map_err(Halt::Broken)?;
        let start_ok = same_screen(&text_at_start, &replay(&at_start).pop().unwrap_or_default());
        let in_offset = inputs(&at_start);
        let screens = replay(&whole);
        let end_ok = same_screen(
            &text_rows(&self.inst, sid),
            screens.last().unwrap_or(&Screen::default()),
        );
        let oracle = format!(
            "the replay of the far end's take is the instance's own screen at the start: {}, at the end: {}",
            if start_ok { "yes" } else { "NO" },
            if end_ok { "yes" } else { "NO" }
        );
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let events = parse_trace(log.get(log_offset..).unwrap_or(""));
        let presses: Vec<Press> = events
            .iter()
            .filter_map(|e| match e {
                Event::Press(p) => Some(p.clone()),
                Event::Frame(_) | Event::Clear(_) => None,
            })
            .collect();
        let frames: Vec<Frame> = events
            .iter()
            .filter_map(|e| match e {
                Event::Frame(f) => Some(f.clone()),
                Event::Press(_) | Event::Clear(_) => None,
            })
            .collect();
        let inputs = screens.len().saturating_sub(1) - in_offset.min(screens.len() - 1);
        notes.push(format!(
            "{} presses traced, {} inputs the far end read, {} frames painted",
            presses.len(),
            inputs,
            frames.len()
        ));
        let off_held = !self.on && frames.is_empty() && start_ok && end_ok;
        let verdict = if !self.on {
            notes.push(
                "prediction off: the control (nothing is predicted, so no frame may paint one)"
                    .to_string(),
            );
            None
        } else if presses.len() == inputs && start_ok && end_ok {
            let settled: Vec<Screen> = (0..presses.len())
                .map(|k| screens[in_offset + k + 1].clone())
                .collect();
            let mut v = judge(&presses, &frames, &settled);
            v.left_on_glass = left_on_glass(&events);
            Some(v)
        } else {
            notes.push(
                "NOT JUDGED: the presses and the far end's inputs do not pair one to one, \
                 or the replay is not the instance's screen"
                    .to_string(),
            );
            None
        };
        Ok(Phase {
            name,
            verdict,
            metrics,
            predicted,
            oracle,
            notes,
            off_held,
        })
    }
}

/// Why a phase did not hold, one line each; none when it held.
fn failures(cfg: &Config, windowed: bool, phase: &Phase) -> Vec<String> {
    let predicted = phase
        .predicted
        .map_or_else(|| "?".to_string(), |n| n.to_string());
    let mut out = Vec::new();
    if !cfg.on {
        if !phase.off_held {
            out.push(
                "prediction off: a frame painted a prediction, or the far end's replay is not \
                 the instance's screen"
                    .to_string(),
            );
        }
        if phase.predicted != Some(0) {
            out.push(format!(
                "prediction off: input_predicted={predicted}, where no key's slice may close at a \
                 prediction"
            ));
        }
        return out;
    }
    let Some(v) = &phase.verdict else {
        out.push("NOT JUDGED (the notes above say why)".to_string());
        return out;
    };
    if !v.wrong.is_empty() {
        out.push(format!("{} WRONG frames", v.wrong.len()));
    }
    if !v.left_on_glass.is_empty() {
        out.push(format!(
            "{} predictions left on glass once a line settled",
            v.left_on_glass.len()
        ));
    }
    if v.unattributed > 0 {
        out.push(format!(
            "{} painted frames name no press, so they were not judged",
            v.unattributed
        ));
    }
    if windowed && cfg.rtt >= MUST_PAINT_RTT_MS {
        let shown: usize = v.keys.values().map(|(_, s)| s).sum();
        if v.frames == 0 {
            out.push(format!(
                "no frame painted a prediction over a {} ms link",
                cfg.rtt
            ));
        }
        if shown == 0 {
            out.push("no key was shown before its echo".to_string());
        }
        if !phase.predicted.is_some_and(|n| n > 0) {
            out.push(format!(
                "input_predicted={predicted}: no key's input slice closed at its prediction"
            ));
        }
    }
    out
}

/// Print a phase and say whether it held ([`failures`]).
fn report(cfg: &Config, windowed: bool, phase: &Phase) -> bool {
    println!(
        "  -- {} / {} ms / prediction {} / {}",
        cfg.shell,
        cfg.rtt,
        if cfg.on { "on (adaptive)" } else { "off" },
        phase.name
    );
    println!("      metrics: {}", phase.metrics);
    println!("      oracle:  {}", phase.oracle);
    for n in &phase.notes {
        println!("      {n}");
    }
    if let Some(v) = &phase.verdict {
        print_verdict(v);
    }
    let failed = failures(cfg, windowed, phase);
    for f in &failed {
        println!("      NOT HELD: {f}");
    }
    failed.is_empty()
}

fn print_verdict(v: &Verdict) {
    let mut classes: Vec<(&String, &(usize, usize))> = v.keys.iter().collect();
    classes.sort();
    let (keys, shown) = v
        .keys
        .values()
        .fold((0, 0), |(a, b), (k, s)| (a + k, b + s));
    println!(
        "      frames painted {} (adaptive {}), held to their settled screen {}, unattributed {}, ahead of the echo {}, WRONG {}",
        v.frames,
        v.adaptive,
        v.checked,
        v.unattributed,
        v.ahead,
        v.wrong.len()
    );
    println!(
        "      keys shown before their echo: {shown} of {keys}; by class: {}",
        classes
            .iter()
            .map(|(c, (k, s))| format!("{c} {s}/{k}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !v.perceived_us.is_empty() {
        let ms = |q| quantile(&v.perceived_us, q) as f64 / 1000.0;
        println!(
            "      press to the first frame showing it, those keys: p50 {:.1} ms, p95 {:.1} ms, p99 {:.1} ms, max {:.1} ms",
            ms(0.5),
            ms(0.95),
            ms(0.99),
            ms(1.0)
        );
    }
    for w in v.wrong.iter().take(20) {
        println!("      WRONG {w}");
    }
    println!(
        "      predictions left on glass once a line settled: {}",
        v.left_on_glass.len()
    );
    for w in v.left_on_glass.iter().take(20) {
        println!("      LEFT {w}");
    }
}

fn copy_out(out: &Path, tag: &str, log: &Path, takes: &Path) {
    let _ = std::fs::copy(log, out.join(format!("aterm-{tag}.log")));
    if let Ok(dir) = std::fs::read_dir(takes) {
        for (i, e) in dir.filter_map(Result::ok).enumerate() {
            let _ = std::fs::copy(e.path(), out.join(format!("take-{tag}-{i}.take")));
        }
    }
}

/// One configuration: its instance, its phases, its report. `Ok(true)` held.
fn run_one(
    aterm: &Path,
    cfg: &Config,
    headless: bool,
    rounds: usize,
    split: bool,
    out: &Path,
) -> Result<Result<bool, String>, Stopped> {
    let tag = format!(
        "{}-{}ms-{}",
        cfg.shell,
        cfg.rtt,
        if cfg.on { "on" } else { "off" }
    );
    let mut world = match World::new(instance::CONFIG_OFF) {
        Ok(w) => w,
        Err(e) => return Ok(Err(e)),
    };
    let laid = (|| -> std::io::Result<(PathBuf, PathBuf)> {
        let bin = world.dir.join("link");
        let takes = world.dir.join("takes");
        std::fs::create_dir_all(&bin)?;
        std::fs::create_dir_all(&takes)?;
        let shell = bin.join(&cfg.shell);
        std::fs::copy(&crate::paths::paths().exe, &shell)?;
        let config = instance::config_with(&[
            format!(
                "predictive_echo = \"{}\"",
                if cfg.on { "adaptive" } else { "off" }
            ),
            format!("shell = \"{}\"", shell.display()),
        ]);
        std::fs::write(
            world.cfg.join("aterm/aterm.toml"),
            instance::bell_off(&config),
        )?;
        std::fs::write(world.home.join(".zshrc"), "PROMPT='$ '\nRPROMPT=''\n")?;
        std::fs::write(world.home.join(".bashrc"), "PS1='$ '\n")?;
        Ok((shell, takes))
    })();
    let (_, takes) = match laid {
        Ok(p) => p,
        Err(e) => return Ok(Err(format!("cannot lay out the world: {e}"))),
    };
    // the instance starts the shell's copy: it stays until its take shows it ran
    world.keep();
    let mut env = world.env();
    env.push((crate::link::ENV_RTT.into(), cfg.rtt.to_string()));
    env.push((
        crate::link::ENV_TAKE.into(),
        takes.to_string_lossy().into_owned(),
    ));
    env.push(("ATERM_TRACE_PREDICT".into(), "1".into()));
    let launch = Launch {
        aterm: aterm.to_path_buf(),
        headless,
        args: ["--columns", "80", "--lines", "24"]
            .map(str::to_string)
            .to_vec(),
        env,
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    };
    let started = if headless {
        Instance::start_in(&world, launch, crate::band::Band::App)
    } else {
        Instance::start(&world, launch)
    };
    let mut inst = match started {
        Ok(i) => i,
        Err(e) => return Ok(Err(e)),
    };
    println!(
        "\n== {tag}: {} instance pid={} sock={}",
        if headless { "headless" } else { "windowed" },
        inst.pid,
        inst.sock.display()
    );
    if let Err(f) = inst.await_socket(instance::BOOT) {
        return Ok(Err(f.msg.unwrap_or_default()));
    }
    let _ = inst.ctl(&["await", "idle", "1500", "timeout=60000"]);
    let sid = inst
        .ctl(&["sessions"])
        .stdout
        .split_whitespace()
        .find(|w| w.len() == 22 && w.starts_with("s-"))
        .map(str::to_string);
    let Some(sid) = sid else {
        return Ok(Err("the tab has no session id".into()));
    };
    let client = match inst.connect() {
        Ok(c) => c,
        Err(e) => return Ok(Err(e)),
    };
    let log = inst.log.clone();
    let mut run = Run {
        inst,
        client,
        headless,
        on: cfg.on,
        rtt: cfg.rtt,
        rounds,
        log: log.clone(),
        takes: takes.clone(),
        out,
        tag: tag.clone(),
    };
    if read_takes(&takes).is_empty() {
        return Ok(Err(
            "the link relay kept no take: is the shell behind it?".into()
        ));
    }
    world.release();
    if !headless {
        match crate::meta_keys::await_idle()? {
            Ok(()) => {}
            Err(why) => return Ok(Err(format!("NOT MEASURED: {why}"))),
        }
        println!(
            "      raise {sid}: {}",
            run.inst.ctl(&["raise", &sid]).first()
        );
        sleep(Duration::from_secs(1))?;
    }
    let mut held = true;
    let mut phases = vec![("single pane (the single-pane render path)", None::<String>)];
    let mut i = 0;
    while i < phases.len() {
        let (name, pane) = phases[i].clone();
        match run.phase(name, pane.as_deref(), cfg.on && i == 0 && !headless) {
            Ok(p) => {
                held &= report(cfg, !headless, &p);
            }
            Err(Halt::Stopped(s)) => return Err(s),
            Err(Halt::NotMeasured(why)) => {
                copy_out(out, &tag, &log, &takes);
                return Ok(Err(format!("NOT MEASURED: {why}")));
            }
            Err(Halt::Broken(why)) => {
                copy_out(out, &tag, &log, &takes);
                return Ok(Err(why));
            }
        }
        if i == 0 && split && cfg.on && !headless {
            let reply = run.inst.ctl(&["spawn", "split=v"]);
            let new_sid = reply.first().split_whitespace().nth(1).map(str::to_string);
            let _ = run.inst.ctl(&["await", "idle", "1500", "timeout=60000"]);
            match new_sid {
                Some(s) if reply.first().starts_with("OK") => {
                    println!("      spawn split=v: {}", reply.first());
                    phases.push(("split pane (the composed render path)", Some(s)));
                }
                _ => {
                    println!("      spawn split=v: {} (no split phase)", reply.first());
                    held = false;
                }
            }
        }
        i += 1;
    }
    copy_out(out, &tag, &log, &run.takes);
    run.inst.stop();
    Ok(Ok(held))
}

/// `predict-live` (module docs).
///
/// # Errors
/// Usage, or nothing to run.
pub fn cmd_predict_live(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let window = instance::take_flag(&mut a, "--window");
    let headless = instance::take_flag(&mut a, "--headless");
    let no_split = instance::take_flag(&mut a, "--no-split");
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let shells = instance::take_values(&mut a, "--shell")?;
    let rtts = instance::take_values(&mut a, "--rtt")?;
    let modes = instance::take_values(&mut a, "--mode")?;
    let rounds = instance::take_value(&mut a, "--rounds")?;
    let out = instance::take_value(&mut a, "--out")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "predict-live: unexpected '{extra}' ({LAB} help)"
        )));
    }
    if window == headless {
        return Err(Fail::usage(format!(
            "predict-live opens a window on your desktop and presses keys in it: pass --window \
             to say you want that, or --headless for the press path alone ({LAB} help)"
        )));
    }
    let shells = if shells.is_empty() {
        vec!["zsh".to_string(), "bash".to_string()]
    } else {
        shells
    };
    if let Some(s) = shells
        .iter()
        .find(|s| !matches!(s.as_str(), "zsh" | "bash"))
    {
        return Err(Fail::usage(format!(
            "predict-live: --shell zsh|bash, not '{s}'"
        )));
    }
    let rtts: Vec<u64> = if rtts.is_empty() {
        vec![100]
    } else {
        rtts.iter()
            .map(|r| instance::count("--rtt", r).map(u64::from))
            .collect::<Result<_, _>>()?
    };
    let modes: Vec<bool> = if modes.is_empty() {
        vec![true, false]
    } else {
        modes
            .iter()
            .map(|m| match m.as_str() {
                "on" => Ok(true),
                "off" => Ok(false),
                other => Err(Fail::usage(format!(
                    "predict-live: --mode on|off, not '{other}'"
                ))),
            })
            .collect::<Result<_, _>>()?
    };
    let rounds = match rounds {
        Some(r) => instance::count("--rounds", &r)? as usize,
        None => 2,
    };
    let out = PathBuf::from(out.map_or_else(
        || {
            crate::sys::make_temp_dir("/tmp/predict-live.XXXXXX")
                .unwrap_or_else(|_| "/tmp".to_string())
        },
        |o| crate::paths::from_caller(&o),
    ));
    let _ = std::fs::create_dir_all(&out);
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    println!(
        "== predict-live: {} ; out {} ; {} rounds of {} lines",
        aterm.display(),
        out.display(),
        rounds,
        corpus(1).len()
    );
    let mut held = true;
    let mut unmeasured = false;
    for shell in &shells {
        for &rtt in &rtts {
            for &on in &modes {
                let cfg = Config {
                    shell: shell.clone(),
                    rtt,
                    on,
                };
                match run_one(&aterm, &cfg, headless, rounds, !no_split, &out) {
                    Ok(Ok(ok)) => held &= ok,
                    Ok(Err(why)) => {
                        println!("  {}: {why}", cfg.shell);
                        if why.starts_with("NOT MEASURED") {
                            unmeasured = true;
                        } else {
                            held = false;
                        }
                    }
                    Err(Stopped(sig)) => {
                        println!("predict-live: ended (signal {sig})");
                        return Ok(3);
                    }
                }
            }
        }
    }
    Ok(if !held {
        1
    } else if unmeasured {
        3
    } else {
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: &[&str], cursor: (usize, usize)) -> Screen {
        Screen {
            rows: rows.iter().map(|r| (*r).to_string()).collect(),
            cursor,
        }
    }

    fn press(t: u64, key: &str, armed: Option<u64>) -> Press {
        Press {
            t,
            key: key.to_string(),
            armed,
        }
    }

    fn frame(t: u64, newest: u64, under: &str, over: &str, cursor: usize, shown: usize) -> Frame {
        Frame {
            t,
            mode: "adaptive".to_string(),
            newest: Some(newest),
            cursor: (0, cursor),
            shown: (0, shown),
            rows: vec![(0, under.to_string(), over.to_string())],
        }
    }

    /// The trace's lines parse back to what aterm-gui's `predict_trace` wrote.
    #[test]
    fn the_trace_parses_back_into_presses_and_frames() {
        let log = "noise\n\
            aterm-predict press t=10 key=glyph%20x armed=3 pending=1\n\
            aterm-predict press t=11 key=submit armed=- pending=0\n\
            aterm-predict frame t=12 mode=adaptive engaged=1 visible=2 newest=3 cursor=0,2 shown=0,3 row=0 under=$ over=$%20x\n\
            aterm-predict clear t=13\n";
        let ev = parse_trace(log);
        assert_eq!(
            ev,
            vec![
                Event::Press(press(10, "glyph x", Some(3))),
                Event::Press(press(11, "submit", None)),
                Event::Frame(Frame {
                    t: 12,
                    mode: "adaptive".to_string(),
                    newest: Some(3),
                    cursor: (0, 2),
                    shown: (0, 3),
                    rows: vec![(0, "$".to_string(), "$ x".to_string())],
                }),
                Event::Clear(13),
            ]
        );
        assert_eq!(decode("a%25b%20%C3%A9"), "a%b é");
    }

    /// A take replays through aterm-core at the size it was told: the screen
    /// before each input it read, and after the last output.
    #[test]
    fn a_take_replays_to_the_screen_before_each_input() {
        let take = parse_take("1 W 3x10\n2 O 2420\n3 I 61\n4 O 61\n5 I 0d\n6 O 0d0a6f6b\n");
        assert_eq!(take.len(), 6);
        let screens = replay(&take);
        assert_eq!(screens.len(), 3);
        assert_eq!(screens[0], screen(&["$", "", ""], (0, 2)));
        assert_eq!(screens[1], screen(&["$ a", "", ""], (0, 3)));
        assert_eq!(screens[2].rows[1], "ok");
    }

    /// A frame drawing the screen its newest key settles to is held and, its
    /// engine still short of that screen, counts as AHEAD of the echo for every
    /// key of its line it shows; one drawing another cell, or another cursor,
    /// is WRONG (the negative controls).
    #[test]
    fn a_frame_is_held_to_its_newest_keys_settled_screen() {
        let presses = [
            press(100, "glyph a", Some(0)),
            press(200, "glyph b", Some(1)),
            press(300, "submit", None),
        ];
        let settled = [
            screen(&["$ a"], (0, 3)),
            screen(&["$ ab"], (0, 4)),
            screen(&["$ ab"], (1, 0)),
        ];
        let good = frame(250, 1, "$", "$ ab", 2, 4);
        let v = judge(&presses, std::slice::from_ref(&good), &settled);
        assert!(v.wrong.is_empty(), "{:?}", v.wrong);
        assert_eq!((v.checked, v.ahead), (1, 1));
        assert_eq!(v.keys.get("glyph"), Some(&(2, 2)), "both keys shown early");
        assert_eq!(v.perceived_us, vec![150, 50]);

        let caught = frame(250, 1, "$", "$ ax", 2, 4);
        let v = judge(&presses, &[caught], &settled);
        assert_eq!(v.wrong.len(), 1, "a wrong cell is caught");
        assert!(v.wrong[0].contains("settles to \"$ ab\""), "{}", v.wrong[0]);
        let misplaced = frame(250, 1, "$", "$ ab", 2, 3);
        assert_eq!(
            judge(&presses, &[misplaced], &settled).wrong.len(),
            1,
            "a wrong cursor is caught"
        );

        let late = frame(260, 0, "$ a", "$ a", 3, 3);
        let v = judge(&presses, &[late], &settled);
        assert!(v.wrong.is_empty());
        assert_eq!((v.checked, v.ahead), (1, 0));
        assert_eq!(v.keys.get("glyph"), Some(&(2, 0)));

        let stray = Frame {
            newest: Some(9),
            ..good
        };
        assert_eq!(judge(&presses, &[stray], &settled).unattributed, 1);
    }

    /// A painted frame its `clear` erased is gone by the next line; one never
    /// erased is still on glass there and at the end (the negative control).
    #[test]
    fn a_prediction_must_be_erased_before_the_next_line() {
        let f = frame(10, 0, "$", "$ a", 2, 3);
        let erased = [
            Event::Press(press(1, "glyph a", Some(0))),
            Event::Frame(f.clone()),
            Event::Press(press(20, "submit", None)),
            Event::Clear(30),
            Event::Press(press(40, "glyph b", Some(1))),
        ];
        assert!(left_on_glass(&erased).is_empty());
        let mid_line = [
            Event::Press(press(1, "glyph a", Some(0))),
            Event::Frame(f.clone()),
            Event::Press(press(5, "glyph b", Some(1))),
            Event::Clear(30),
        ];
        assert!(
            left_on_glass(&mid_line).is_empty(),
            "a frame on glass as the line goes on is the line's own"
        );
        let stale = [
            Event::Press(press(1, "glyph a", Some(0))),
            Event::Frame(f),
            Event::Press(press(20, "submit", None)),
            Event::Press(press(40, "glyph b", Some(1))),
        ];
        let left = left_on_glass(&stale);
        assert_eq!(
            left.len(),
            2,
            "at the next line's first key, and at the end: {left:?}"
        );
        assert!(left[0].contains("t=40us"), "{}", left[0]);
    }

    /// The corpus is the plan's kinds, every line ending in Enter, and its
    /// cadence puts a notice before an edit key and before Enter.
    #[test]
    fn the_corpus_holds_the_plans_kinds_and_a_typists_cadence() {
        let lines = corpus(2);
        assert_eq!(lines.len(), 20);
        assert!(
            lines
                .iter()
                .all(|l| l.keys.last() == Some(&Key::Named("enter")))
        );
        let families: Vec<&str> = corpus(1).iter().map(|l| l.family).collect();
        assert_eq!(
            families,
            [
                "plain",
                "typo-end",
                "left-right",
                "mid-insert",
                "mid-erase",
                "ctrl-d",
                "delete",
                "home-end",
                "ctrl-a-e",
                "typo-back"
            ]
        );
        assert_eq!(Key::Ctrl('a').hwkey(), "hwkey a mods=ctrl");
        assert_eq!(Key::Ch(' ').key(), "key space");
        assert_eq!(gap(None, Key::Ch('e'), 0), Duration::ZERO);
        assert_eq!(
            gap(Some(Key::Ch('e')), Key::Named("left"), 3),
            Duration::from_millis(350)
        );
        assert_eq!(
            gap(Some(Key::Ch('e')), Key::Ch('c'), 1),
            Duration::from_millis(130)
        );
        assert_eq!(quantile(&[5, 1, 9, 3], 0.5), 3);
        assert_eq!(quantile(&[], 0.5), 0);
    }

    fn phase(verdict: Option<Verdict>, predicted: Option<u64>, off_held: bool) -> Phase {
        Phase {
            name: "single pane",
            verdict,
            metrics: String::new(),
            predicted,
            oracle: String::new(),
            notes: Vec::new(),
            off_held,
        }
    }

    fn on(rtt: u64) -> Config {
        Config {
            shell: "zsh".to_string(),
            rtt,
            on: true,
        }
    }

    /// A judged phase that showed what it is run to show: frames painted and
    /// held, keys shown before their echo, and their input slices booked at
    /// their prediction.
    fn shown() -> Verdict {
        let presses = [press(100, "glyph a", Some(0)), press(300, "submit", None)];
        let settled = [screen(&["$ a"], (0, 3)), screen(&["$ a"], (1, 0))];
        judge(&presses, &[frame(150, 0, "$", "$ a", 2, 3)], &settled)
    }

    /// The `metrics` reply's fields are read whole, never a prefix of another.
    #[test]
    fn a_metric_is_read_by_its_whole_name() {
        let reply = "OK n_input=537 input_predicted=401 input_p50_ms=1.44";
        assert_eq!(metric(reply, "input_predicted"), Some("401"));
        assert_eq!(metric(reply, "input_p50_ms"), Some("1.44"));
        assert_eq!(metric(reply, "input"), None);
        assert_eq!(
            metric_line(reply).split(' ').nth(1),
            Some("input_predicted=401")
        );
    }

    /// NOT VACUOUSLY.
    #[test]
    fn a_phase_holds_only_when_it_showed_what_it_is_run_to_show() {
        let held = phase(Some(shown()), Some(1), false);
        assert_eq!(failures(&on(100), true, &held), Vec::<String>::new());

        let quiet = || phase(Some(Verdict::default()), Some(0), false);
        let failed = failures(&on(100), true, &quiet());
        assert_eq!(failed.len(), 3, "{failed:?}");
        assert!(failed[0].contains("no frame painted"), "{failed:?}");
        assert!(
            failures(&on(100), false, &quiet()).is_empty(),
            "headless never presents"
        );
        assert!(
            failures(&on(MUST_PAINT_RTT_MS - 1), true, &quiet()).is_empty(),
            "below the gate's round trip, painting is its call"
        );

        let unwired = phase(Some(shown()), Some(0), false);
        assert_eq!(
            failures(&on(100), true, &unwired),
            vec!["input_predicted=0: no key's input slice closed at its prediction".to_string()]
        );

        let mut stray = shown();
        stray.unattributed = 1;
        let failed = failures(&on(100), true, &phase(Some(stray), Some(1), false));
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("name no press"), "{failed:?}");

        let off = Config {
            on: false,
            ..on(100)
        };
        assert!(failures(&off, true, &phase(None, Some(0), true)).is_empty());
        let failed = failures(&off, true, &phase(None, Some(3), true));
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("input_predicted=3"), "{failed:?}");
        assert_eq!(
            failures(&off, true, &phase(None, Some(0), false)).len(),
            1,
            "a frame painted with prediction off"
        );
    }
}
