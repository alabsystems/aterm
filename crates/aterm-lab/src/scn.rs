// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame-stream survey scenarios against real Claude Code, plus `selftest`, `watch`, and `catch`.

use std::fs;
use std::path::PathBuf;

use crate::LAB;
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::paths::paths;
use crate::pyjson::{J, Style};
use crate::record::{Event, Frame, Journal, Recorder, rec_s, rec_t};
use crate::run::Run;
use crate::screen::{field, is_card_row, is_idle, is_options_row, status_row};
use crate::stop::{self, Stopped};
use crate::util::{keep_tree, now, py_repr, stamp};

const BANNER: &str = aterm_phase::anchor!("launch.banner");
const PENDING: &str = aterm_phase::anchor!("survey.pending");
const THANKS: &str = aterm_phase::anchor!("survey.thanks");

/// Why a scenario stopped short.
#[derive(Debug)]
pub enum ScnErr {
    Stopped(Stopped),
    Failed(String),
}

impl From<Stopped> for ScnErr {
    fn from(s: Stopped) -> Self {
        ScnErr::Stopped(s)
    }
}

impl From<String> for ScnErr {
    fn from(s: String) -> Self {
        ScnErr::Failed(s)
    }
}

impl From<crate::Fail> for ScnErr {
    fn from(f: crate::Fail) -> Self {
        match f.stopped {
            Some(sig) => ScnErr::Stopped(Stopped(sig)),
            None => ScnErr::Failed(f.msg.unwrap_or_default()),
        }
    }
}

type R<T = ()> = Result<T, ScnErr>;

/// Lab launch options on top of `LAB_TRUST=1`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LabKw {
    /// The one-time first-run notice left on the screen.
    pub first_run: bool,
    /// The forced survey flag set at launch (`LAB_SURVEY=1`).
    pub survey: bool,
    /// The pane's rows.
    pub lines: Option<u32>,
    /// `LAB_INSTANCE_ENV` words the scenario's own lab adds to the caller's (a dev seam its
    /// instance reads; [`crate::lab::instance_env`] says which).
    pub instance_env: &'static str,
}

const SUMMARY: Style = Style {
    indent: Some(1),
    compact: false,
    ascii: false,
};

#[must_use]
pub fn results_dir() -> PathBuf {
    paths().state.join("results")
}

/// One scenario run on one lab.
pub struct Ctx {
    /// The run's number.
    pub k: u32,
    attached: bool,
    /// The lab is this run's own ([`crate::tape::end_loops`]).
    pub ends_loops: bool,
    /// Where the run's records go (`None`: kept nowhere).
    pub dir: Option<PathBuf>,
    tag: String,
    /// The lab.
    pub run: Run,
    /// The first tab's session id.
    pub sid: String,
    /// The frame stream.
    pub rec: Option<Recorder>,
    /// The journal tail.
    pub jr: Option<Journal>,
    /// `(wall, label)` marks.
    pub marks: Vec<(f64, String)>,
    /// The summary being measured.
    pub sumd: J,
    harness_keys: String,
    kw: LabKw,
}

/// The `up` options a run's own lab is made with: the caller's (`base`) with the folder trust
/// seeded and a lifeline to this process, the run's `[harness]` lines in place of the caller's,
/// and its [`LabKw`]: the instance's words after the caller's `LAB_INSTANCE_ENV` ones (a name
/// both give is refused at `up`, [`crate::lab::instance_env`]).
fn lab_opts(mut o: UpOpts, harness_keys: &str, kw: LabKw) -> UpOpts {
    o.trust = true;
    o.lifeline = true;
    o.first_run = kw.first_run;
    o.survey = kw.survey || o.survey;
    o.harness_keys = harness_keys.to_string();
    if !kw.instance_env.is_empty() {
        o.instance_env = format!("{} {}", o.instance_env, kw.instance_env)
            .trim()
            .to_string();
    }
    if let Some(l) = kw.lines {
        o.lines = l.to_string();
    }
    o
}

/// Why a run cannot take a lab it attaches to: the lab was made before the run, so the run's
/// `[harness]` lines and [`LabKw`] ([`lab_opts`]) never reach it. `None` for a run that asks for
/// neither.
fn unset_on_attach(harness_keys: &str, kw: LabKw) -> Option<String> {
    let asks = !harness_keys.is_empty()
        || kw.first_run
        || kw.survey
        || kw.lines.is_some()
        || !kw.instance_env.is_empty();
    asks.then(|| {
        "an attached lab keeps the [harness] lines and the instance it was made with; this run's \
         are not set"
            .to_string()
    })
}

fn r3(v: f64) -> J {
    J::Float((v * 1000.0).round() / 1000.0)
}

fn r2(v: f64) -> J {
    J::Float((v * 100.0).round() / 100.0)
}

fn repr_opt(s: Option<&str>) -> String {
    s.map_or_else(|| "None".into(), py_repr)
}

impl Ctx {
    #[must_use]
    pub fn new(
        name: &str,
        k: u32,
        tag: &str,
        harness_keys: &str,
        kw: LabKw,
        attached: bool,
        keep: bool,
    ) -> Ctx {
        let dir = keep.then(|| results_dir().join(format!("{name}-{k:02}")));
        if let Some(d) = &dir {
            let _ = fs::create_dir_all(d);
        }
        Ctx {
            k,
            attached,
            ends_loops: !attached,
            dir,
            tag: tag.into(),
            run: Run::default(),
            sid: String::new(),
            rec: None,
            jr: None,
            marks: Vec::new(),
            sumd: J::Obj(Vec::new()),
            harness_keys: harness_keys.into(),
            kw,
        }
    }

    /// Opens the lab's door.
    /// # Errors
    /// No aterm to test.
    pub fn door(&self) -> R<Door<'_>> {
        Ok(Door::new(&self.run)?)
    }

    /// `aterm ctl <args>` against the lab (an empty reply when it could not run).
    pub fn ctl(&self, args: &[&str]) -> crate::door::Reply {
        self.door()
            .ok()
            .and_then(|d| d.ctl(args).ok())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn drive(&self, args: &[&str]) -> String {
        self.door()
            .ok()
            .and_then(|d| d.drive(args).ok())
            .map(|r| format!("{}{}", r.stdout, r.stderr).trim().to_string())
            .unwrap_or_default()
    }

    /// The stub's mode.
    pub fn mode(&self, m: &str) {
        let _ = crate::verbs::set_mode(&self.run, m);
    }

    /// Brings the lab up (or attaches to it) and starts the frame stream and journal tail.
    /// # Errors
    /// The lab did not come up, or the stream could not start.
    pub fn start(&mut self) -> R {
        if self.attached {
            if let Some(why) = unset_on_attach(&self.harness_keys, self.kw) {
                return Err(ScnErr::Failed(why));
            }
        } else {
            let o = lab_opts(UpOpts::from_env(), &self.harness_keys, self.kw);
            crate::lab::up(&self.tag, &o).map_err(|f| {
                ScnErr::Failed(format!("lab up failed: {}", f.msg.unwrap_or_default()))
            })?;
        }
        self.run = Run::load(&self.tag)?;
        Door::new(&self.run)?;
        for _ in 0..50 {
            let ls = self.ctl(&["ls"]).stdout;
            if let Some(sid) = ls
                .split_whitespace()
                .find(|w| w.len() == 22 && w.starts_with("s-"))
            {
                self.sid = sid.to_string();
                break;
            }
            stop::sleep(std::time::Duration::from_millis(200))?;
        }
        if self.sid.is_empty() {
            return Err(ScnErr::Failed("no session id".into()));
        }
        let rec = Recorder::start(&self.door()?, &self.sid, self.dir.as_deref())?;
        self.rec = Some(rec);
        let jpath = self
            .run
            .state()
            .join("drive")
            .join(format!("{}.journal.jsonl", self.sid));
        self.jr = Some(Journal::start(jpath, self.dir.as_deref()));
        stop::sleep(std::time::Duration::from_millis(400))?;
        Ok(())
    }

    const fn rec(&self) -> &Recorder {
        self.rec.as_ref().expect("started")
    }

    /// The stub's scripted `bg` mode.
    pub fn setup_bg(&self, sleep_s: u32, slow_s: u32) {
        self.mode(&format!("bg:{sleep_s}:{slow_s}"));
    }

    /// Launches Claude Code in the tab and waits for its composer.
    /// # Errors
    /// The composer never showed.
    pub fn launch(&mut self, timeout: f64, extra: Launch) -> R {
        self.mark("launch claude", None);
        let l = Launch {
            no_wait: true,
            ..extra
        };
        crate::lab::launch_claude(&self.run, &l).map_err(|f| {
            ScnErr::Failed(format!("lab claude failed: {}", f.msg.unwrap_or_default()))
        })?;
        let fr = self.rec().wait_for(
            |f| f.comp().is_some() && f.rows.iter().any(|r| r.contains(BANNER)),
            timeout,
            0,
        )?;
        if fr.is_none() {
            return Err(ScnErr::Failed("the composer never showed".into()));
        }
        self.mark("composer up", None);
        stop::sleep(std::time::Duration::from_millis(2500))?;
        Ok(())
    }

    /// The cast, status and stub log for the record.
    pub fn finish(&mut self) {
        self.mark("finish", None);
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(d) = &self.dir {
            let cast = self.ctl(&["cast"]).stdout;
            let _ = fs::write(d.join("cast.jsonl"), cast);
            let _ = fs::write(d.join("status.txt"), self.ctl(&["status"]).stdout);
            let sl = self.run.w.join("stub.log");
            if let Ok(Some(t)) = crate::sys::scratch_read(&sl)
                .map(|b| b.map(|b| String::from_utf8_lossy(&b).into_owned()))
            {
                let kept: String = t
                    .lines()
                    .filter(|l| !l.contains("PROXY refused"))
                    .map(|l| format!("{l}\n"))
                    .collect();
                let _ = fs::write(d.join("stub.log"), kept);
            }
        }
        if let Some(mut r) = self.rec.take() {
            r.stop();
            let e = r.err();
            if !e.is_empty() {
                self.sumd.set("recorder_error", J::s(e));
            }
        }
        if let Some(mut j) = self.jr.take() {
            j.stop();
        }
        let mut tapes_from = self.run.state().join("drive").join("tapes");
        if self.dir.is_some() && self.ends_loops {
            let ended = self
                .door()
                .ok()
                .map(|door| crate::tape::end_loops(&door, &tapes_from));
            let why = match ended {
                Some(Ok(true)) => None,
                Some(Ok(false)) => Some("a loop did not end within a minute of its Autopilot off"),
                Some(Err(_)) => Some("interrupted before every loop ended"),
                None => Some("the lab's door did not open, so no loop was ended"),
            };
            if let Some(why) = why {
                self.sumd.set("tapes_unended", J::s(why));
            }
        }
        if !self.attached && !self.tag.is_empty() {
            let down = match crate::lab::down(&self.tag, false) {
                Ok(keep) => {
                    tapes_from = keep.join("drive").join("tapes");
                    format!(
                        "lab '{}' is down; logs kept in {}",
                        self.tag,
                        keep.display()
                    )
                }
                Err(f) => f.msg.unwrap_or_default(),
            };
            let tail: String = down
                .chars()
                .rev()
                .take(200)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            self.sumd.set("down", J::s(tail));
        }
        if let Some(d) = &self.dir {
            let kept = d.join("tapes");
            let _ = fs::remove_dir_all(&kept);
            keep_tree(&tapes_from, &kept);
            match crate::tape::summary(&kept) {
                Ok(cost) => {
                    let int = |n: u64| J::Int(i64::try_from(n).unwrap_or(i64::MAX));
                    self.sumd.set("tapes", int(cost.tapes));
                    self.sumd.set("tapes_open", int(cost.open));
                    self.sumd.set("tape_bytes", int(cost.bytes));
                    self.sumd.set("doors", int(cost.doors));
                    self.sumd.set("decisions", int(cost.decisions));
                }
                Err(error) => {
                    self.sumd.set("tape_summary_error", J::s(error));
                }
            }
        }
    }

    /// Note `label` at `t` (now when `None`).
    pub fn mark(&mut self, label: &str, t: Option<f64>) -> f64 {
        let t = t.unwrap_or_else(now);
        self.marks.push((t, label.to_string()));
        t
    }

    /// Arms `CLAUDE_FORCE_DISPLAY_SURVEY=1` in the running Claude's settings ([`crate::settings_witness`]).
    /// # Errors
    /// Returns an error if the settings could not be written, the run was interrupted, or no
    /// re-read arrives within [`crate::settings_witness::HANG`].
    pub fn arm(&mut self) -> R {
        self.arm_with_wait(&crate::settings_witness::WallWait::new())
    }

    fn arm_with_wait(&mut self, wait: &impl crate::settings_witness::Wait) -> R {
        let t = now();
        let r = crate::settings_witness::set_env_reread_with_wait(
            &self.run,
            "CLAUDE_FORCE_DISPLAY_SURVEY",
            Some("1"),
            wait,
        )
        .map_err(|f| match f.stopped {
            Some(_) => ScnErr::from(f),
            None => ScnErr::Failed(format!(
                "the survey flag was not armed: {}",
                f.msg.unwrap_or_default()
            )),
        })?;
        self.mark("survey flag written", Some(t));
        self.mark(
            &format!("survey flag on (re-read after {} write(s))", r.writes),
            None,
        );
        self.put("arm_writes", J::Int(i64::from(r.writes)));
        self.put("arm_reread_s", r2(r.after.as_secs_f64()));
        Ok(())
    }

    /// Types `text` character by character with `gap` seconds between keys.
    /// # Errors
    /// Interrupted.
    pub fn type_text(&mut self, text: &str, gap: f64) -> R<f64> {
        self.mark(&format!("type {} start", py_repr(text)), None);
        for ch in text.chars() {
            if ch == ' ' {
                self.ctl(&["key", "space"]);
            } else {
                self.ctl(&["send", &ch.to_string()]);
            }
            stop::sleep(std::time::Duration::from_secs_f64(gap))?;
        }
        Ok(self.mark(&format!("type {} end", py_repr(text)), None))
    }

    /// Sends `text` and presses Enter 0.3 s later.
    /// # Errors
    /// Interrupted.
    pub fn turn(&mut self, text: &str) -> R<f64> {
        let t = self.mark(&format!("turn {}: send", py_repr(text)), None);
        self.ctl(&["send", text]);
        stop::sleep(std::time::Duration::from_millis(300))?;
        self.ctl(&["key", "enter"]);
        self.mark(&format!("turn {}: enter", py_repr(text)), None);
        Ok(t)
    }

    /// Waits for the turn's done row with no spinner.
    /// # Errors
    /// Interrupted.
    pub fn wait_idle_row(&mut self, since: usize, timeout: f64) -> R<Option<Frame>> {
        let fr = self.rec().wait_for(|f| is_idle(&f.rows), timeout, since)?;
        if let Some(f) = &fr {
            self.mark("turn idle (done row)", Some(f.wall));
        }
        Ok(fr)
    }

    /// Waits for the survey card.
    /// # Errors
    /// Interrupted.
    pub fn wait_card(&mut self, since: usize, timeout: f64) -> R<Option<Frame>> {
        let fr = self.rec().wait_for(Frame::card, timeout, since)?;
        if let Some(f) = &fr {
            self.mark("card appears", Some(f.wall));
        }
        Ok(fr)
    }

    /// Waits for the first frame matching `pred` from `since` within `timeout` seconds.
    /// # Errors
    /// Interrupted.
    pub fn wait_for(
        &self,
        pred: impl Fn(&Frame) -> bool,
        timeout: f64,
        since: usize,
    ) -> R<Option<Frame>> {
        Ok(self.rec().wait_for(pred, timeout, since)?)
    }

    #[must_use]
    pub fn n_frames(&self) -> usize {
        self.rec().len()
    }

    #[must_use]
    pub fn frames(&self) -> Vec<Frame> {
        self.rec().snapshot().0
    }

    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.rec().snapshot().1
    }

    #[must_use]
    pub fn journal(&self) -> Vec<(f64, J)> {
        self.jr.as_ref().map(Journal::snapshot).unwrap_or_default()
    }

    /// Set a summary field.
    pub fn put(&mut self, key: &str, v: impl Into<J>) {
        self.sumd.set(key, v.into());
    }

    fn push(&mut self, key: &str, v: J) {
        let mut list = self
            .sumd
            .get(key)
            .and_then(J::as_arr)
            .map(<[J]>::to_vec)
            .unwrap_or_default();
        list.push(v);
        self.sumd.set(key, J::Arr(list));
    }

    /// A fixture of `frame`, named after the lab's Claude Code.
    pub fn fixture(&mut self, name: &str, what: &str, frame: &Frame) {
        let Ok(door) = self.door() else { return };
        if let Ok((p, _)) =
            crate::verbs::write_fixture_rows(&self.run, &door, name, what, &frame.rows)
        {
            self.push("fixtures", J::s(p.to_string_lossy().into_owned()));
        }
    }

    /// The frame as a PNG next to the fixtures.
    pub fn png(&mut self, name: &str) {
        let dir = paths().state.join("captures");
        let _ = fs::create_dir_all(&dir);
        let Ok(door) = self.door() else { return };
        if let Some(p) = crate::verbs::png(
            &door,
            &dir,
            &format!("claude-{}-{name}", self.run.claude_ver),
        ) {
            self.push("pngs", J::s(p.to_string_lossy().into_owned()));
        }
    }

    /// Formats every frame change, event, journal line, and mark on one clock from `t0`.
    #[must_use]
    pub fn timeline(&self, t0: Option<f64>) -> String {
        let (frames, events) = self
            .rec
            .as_ref()
            .map(Recorder::snapshot)
            .unwrap_or_default();
        let jl = self.journal();
        let t0 = t0
            .or_else(|| self.marks.first().map(|m| m.0))
            .or_else(|| frames.first().map(|f| f.wall))
            .unwrap_or(0.0);
        let mut lines: Vec<(f64, String)> = Vec::new();
        let mut last = None;
        for f in &frames {
            let key = frame_key(f);
            if Some(&key) != last.as_ref() {
                lines.push((f.wall, frame_line(f)));
                last = Some(key);
            }
        }
        for (w, _, l) in &events {
            lines.push((*w, format!("EVENT {}", l.get(8..).unwrap_or(""))));
        }
        for (w, rec) in &jl {
            let tj = rec_t(rec);
            let when = if tj > 0.0 { tj } else { *w };
            let seen = if tj > 0.0 { w - tj } else { 0.0 };
            let kind = rec.get("kind").map_or_else(
                || "None".to_string(),
                |k| k.as_str().map_or_else(|| k.py(), str::to_string),
            );
            let line = rec.get("line").map_or_else(
                || "None".to_string(),
                |k| k.as_str().map_or_else(|| k.py(), str::to_string),
            );
            lines.push((
                when,
                format!(
                    "JOURNAL(t={} seen+{seen:.3}) {kind} {line}",
                    if tj > 0.0 { stamp(tj) } else { "-".into() }
                ),
            ));
        }
        for (w, l) in &self.marks {
            lines.push((*w, format!("MARK {l}")));
        }
        lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        lines
            .iter()
            .map(|(w, l)| format!("{:8.3} {}  {l}", w - t0, stamp(*w)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `timeline.txt`.
    pub fn save_timeline(&self) {
        if let Some(d) = &self.dir {
            let _ = fs::write(d.join("timeline.txt"), format!("{}\n", self.timeline(None)));
        }
    }

    /// `summary.json`.
    pub fn save_summary(&self) {
        if let Some(d) = &self.dir {
            let _ = fs::write(
                d.join("summary.json"),
                format!("{}\n", self.sumd.dump(SUMMARY)),
            );
        }
    }
}

type FrameKey = (bool, Option<String>, Option<String>, Vec<String>);

fn rating_row(r: &str) -> bool {
    [
        PENDING.trim_end(),
        "Thanks for",
        "Can Anthropic",
        "Interrupted",
        "What went",
        "Tell us more",
        "y: Yes",
    ]
    .iter()
    .any(|n| r.contains(n))
}

fn frame_key(f: &Frame) -> FrameKey {
    (
        f.card(),
        f.comp(),
        status_row(&f.rows),
        f.rows.iter().filter(|r| rating_row(r)).cloned().collect(),
    )
}

#[must_use]
pub fn frame_line(f: &Frame) -> String {
    let (card, comp, st, rows) = frame_key(f);
    let status: String = st.unwrap_or_default().chars().take(50).collect();
    let rows_s = if rows.is_empty() {
        String::new()
    } else {
        format!(
            " rows=[{}]",
            rows.iter()
                .map(|r| py_repr(r))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "FRAME seq={} T={} card={} composer={} status={}{rows_s}",
        f.seq,
        f.t.map_or_else(|| "None".into(), |t| t.to_string()),
        i32::from(card),
        repr_opt(comp.as_deref()),
        py_repr(&status)
    )
}

fn opt_f(v: Option<f64>) -> J {
    v.map_or(J::Null, J::Float)
}

fn comp_j(c: Option<String>) -> J {
    c.map_or(J::Null, J::Str)
}

#[must_use]
pub fn cycle_stats(frames: &[Frame], journal: &[(f64, J)], t_turn: f64) -> J {
    let mut d = J::Obj(Vec::new());
    let idle = frames.iter().find(|f| is_idle(&f.rows) && f.wall >= t_turn);
    let card = frames.iter().find(|f| f.card() && f.wall >= t_turn);
    d.set("t_turn", J::Float(t_turn));
    d.set("t_idle", opt_f(idle.map(|f| f.wall)));
    d.set("t_card", opt_f(card.map(|f| f.wall)));
    if let Some(card) = card {
        let zero = frames.iter().find(|f| {
            f.wall > card.wall
                && f.card()
                && f.comp()
                    .is_some_and(|c| !c.trim().is_empty() && c.trim().chars().all(|x| x == '0'))
        });
        let gone = frames.iter().find(|f| f.wall > card.wall && !f.card());
        d.set("t_zero", opt_f(zero.map(|f| f.wall)));
        d.set("zero_text", zero.map_or(J::Null, |f| comp_j(f.comp())));
        d.set("t_gone", opt_f(gone.map(|f| f.wall)));
        d.set(
            "composer_after",
            comp_j(frames.last().and_then(Frame::comp)),
        );
        let mut seen: Vec<Option<String>> = Vec::new();
        for f in frames.iter().filter(|f| f.wall >= card.wall) {
            let c = f.comp();
            if seen.last() != Some(&c) {
                seen.push(c);
            }
        }
        d.set(
            "composer_states",
            J::Arr(seen.into_iter().map(comp_j).collect()),
        );
    }
    for (_, rec) in journal {
        if rec_s(rec, "line").contains("DISMISSED survey") {
            d.set("t_journal_dismissed", J::Float(rec_t(rec)));
            d.set(
                "journal_dismissed_line",
                rec.get("line").cloned().unwrap_or(J::Null),
            );
        }
    }
    let jl: Vec<J> = journal
        .iter()
        .map(|(_, rec)| {
            J::Arr(vec![
                r3(rec_t(rec) - t_turn),
                rec.get("kind").cloned().unwrap_or(J::Null),
                rec.get("line").cloned().unwrap_or(J::Null),
            ])
        })
        .collect();
    d.set("journal", J::Arr(jl));
    let dd = |d: &J, a: &str, b: &str| match (
        d.get(a).and_then(J::as_f64),
        d.get(b).and_then(J::as_f64),
    ) {
        (Some(x), Some(y)) if x != 0.0 && y != 0.0 => r3(y - x),
        _ => J::Null,
    };
    for (k, a, b) in [
        ("idle_to_card", "t_idle", "t_card"),
        ("card_to_zero", "t_card", "t_zero"),
        ("zero_to_gone", "t_zero", "t_gone"),
        ("card_to_gone", "t_card", "t_gone"),
        ("gone_to_journal", "t_gone", "t_journal_dismissed"),
    ] {
        let v = dd(&d, a, b);
        d.set(k, v);
    }
    d
}

#[must_use]
pub fn judge_cycle(d: &J) -> Vec<String> {
    if !d.get("t_card").is_some_and(J::truthy) {
        return vec!["no survey card was drawn".into()];
    }
    let mut bad = Vec::new();
    if !d.get("t_gone").is_some_and(J::truthy) {
        bad.push("the card never went".into());
    }
    if !d.get("t_journal_dismissed").is_some_and(J::truthy) {
        bad.push("the harness journaled no DISMISSED survey".into());
    }
    match d.get("composer_after") {
        Some(J::Str(s)) if s.is_empty() => {}
        other => bad.push(format!(
            "the composer was left holding {}",
            other.map_or_else(
                || "None".into(),
                |v| v.as_str().map_or_else(|| "None".into(), py_repr)
            )
        )),
    }
    bad
}

fn card_cycle(ctx: &mut Ctx, text: &str) -> R<(f64, Option<Frame>)> {
    ctx.arm()?;
    let n0 = ctx.n_frames();
    let t = ctx.turn(text)?;
    let card = ctx.wait_card(n0, 30.0)?;
    Ok((t, card))
}

fn plain_cycle(ctx: &mut Ctx, settle: f64) -> R<(f64, Option<Frame>)> {
    let (t, card) = card_cycle(ctx, "hi")?;
    let Some(card) = card else {
        ctx.put("error", "no card");
        return Ok((t, None));
    };
    ctx.wait_for(|f| !f.card(), 20.0, card.i)?;
    let end = now() + 20.0;
    while now() < end
        && !ctx
            .journal()
            .iter()
            .any(|(_, r)| rec_s(r, "line").contains("DISMISSED survey"))
    {
        stop::sleep(std::time::Duration::from_millis(100))?;
    }
    stop::sleep(std::time::Duration::from_secs_f64(settle))?;
    let stats = cycle_stats(&ctx.frames(), &ctx.journal(), t);
    merge_into(&mut ctx.sumd, &stats);
    Ok((t, Some(card)))
}

fn merge_into(sumd: &mut J, from: &J) {
    if let J::Obj(pairs) = from {
        for (k, v) in pairs {
            sumd.set(k, v.clone());
        }
    }
}

fn at(t: f64, dt: f64) -> R<f64> {
    stop::until(t + dt)?;
    Ok(now())
}

fn secs(s: f64) -> R {
    Ok(stop::sleep(std::time::Duration::from_secs_f64(s))?)
}

fn comp_states(ctx: &Ctx, t_from: f64, t_to: Option<f64>) -> J {
    let mut out = Vec::new();
    let mut last: Option<(bool, Option<String>)> = None;
    for f in ctx.frames() {
        if f.wall < t_from || t_to.is_some_and(|t| f.wall > t) {
            continue;
        }
        let k = (f.card(), f.comp());
        if last.as_ref() != Some(&k) {
            out.push(J::Arr(vec![
                r3(f.wall - t_from),
                J::Int(i64::from(k.0)),
                comp_j(k.1.clone()),
            ]));
            last = Some(k);
        }
    }
    J::Arr(out)
}

fn journal_lines(ctx: &Ctx, t_from: f64) -> J {
    J::Arr(
        ctx.journal()
            .iter()
            .filter(|(_, r)| rec_t(r) >= t_from - 0.5)
            .map(|(_, r)| {
                J::Arr(vec![
                    r3(rec_t(r) - t_from),
                    r.get("kind").cloned().unwrap_or(J::Null),
                    r.get("line").cloned().unwrap_or(J::Null),
                ])
            })
            .collect(),
    )
}

fn put_journal(ctx: &mut Ctx, jl: J) {
    let dismissed = dismissed_in(&jl);
    ctx.put("journal", jl);
    ctx.put("dismissed_journal", dismissed);
}

fn dismissed_in(j: &J) -> bool {
    j.as_arr().unwrap_or_default().iter().any(|e| {
        e.as_arr()
            .and_then(|a| a.get(2))
            .and_then(J::as_str)
            .is_some_and(|l| l.contains("DISMISSED"))
    })
}

fn finish_states(ctx: &mut Ctx, t_from: f64) {
    let frames = ctx.frames();
    let states = comp_states(ctx, t_from, None);
    let journal = journal_lines(ctx, t_from);
    let dismissed = dismissed_in(&journal);
    ctx.put("states", states);
    ctx.put("journal", journal);
    ctx.put(
        "composer_end",
        frames.last().map_or(J::Null, |f| comp_j(f.comp())),
    );
    ctx.put(
        "card_end",
        frames.last().map_or(J::Null, |f| J::Bool(f.card())),
    );
    ctx.put("dismissed_journal", dismissed);
}

fn no_card(ctx: &mut Ctx) -> R {
    ctx.put("error", "no card");
    Ok(())
}

fn stub_main_lines(ctx: &Ctx) -> Option<Vec<String>> {
    let t = crate::sys::scratch_read(&ctx.run.w.join("stub.log"))
        .ok()
        .flatten()
        .map(|b| String::from_utf8_lossy(&b).into_owned())?;
    Some(
        t.lines()
            .filter(|l| l.contains("POST /v1/messages") && l.contains("kind=main"))
            .map(str::to_string)
            .collect(),
    )
}

fn last_user_of(line: &str) -> Option<String> {
    let i = line.find("last_user=\"")?;
    let rest = &line[i + 11..];
    rest.find('"').map(|j| rest[..j].to_string())
}

fn status_of(ctx: &Ctx, key: &str) -> J {
    field(&ctx.ctl(&["status"]).stdout, key).map_or(J::Null, J::Str)
}

#[derive(Clone, Copy, Debug)]
pub enum Plan {
    S1Plain,
    XRating1,
    PersonText(f64),
    PersonDigit(f64, char),
    S3a,
    S3b,
    S4a,
    S4b,
    S5a,
    S5b,
    EarlyZero(f64),
    S6b,
    S6c,
    S6e,
    S2Lease,
    TextSweep(f64),
    S7,
    S7Early,
    S6f,
    Startup,
    StartupProbe,
    ReaderProbe(bool),
    Tall(u32, bool),
    S8BusyParked,
    S8Static,
    TallLong(u32),
    S8Lease(u32),
    TailDump(u32),
    CorpusApproval,
    CorpusQuestion,
    CorpusWall,
    CorpusRead,
    /// THE OCCUPANCY DOOR at a workflow's break, under `occupancy = "prompt"`
    /// ([`crate::occupancy_break`]).
    OccupancyBreak,
    /// Land or Cruise played to the act or hold the corpus keeps
    /// ([`crate::autopilot_scenes`]).
    Autopilot(crate::autopilot_scenes::Scene),
}

#[derive(Clone, Debug)]
pub struct Scenario {
    /// Its name.
    pub name: String,
    /// Its `[harness]` lines.
    pub harness_keys: &'static str,
    /// How its lab is made.
    pub kw: LabKw,
    /// What it does.
    pub plan: Plan,
}

const DISMISS_OFF: &str = "dismiss_surveys = false";
const QUIET: &str = "continue = false";
const HELD_QUIET: &str = "human_grace_s = 3\ncontinue = false";

#[must_use]
pub fn scenarios() -> Vec<Scenario> {
    let mut v = Vec::new();
    let mut add = |name: String, hk: &'static str, kw: LabKw, plan: Plan| {
        assert!(
            !v.iter().any(|s: &Scenario| s.name == name),
            "two scenarios are named {name}"
        );
        v.push(Scenario {
            name,
            harness_keys: hk,
            kw,
            plan,
        })
    };
    let none = LabKw::default();
    let survey = LabKw {
        survey: true,
        ..none
    };
    let notice = LabKw {
        first_run: true,
        survey: true,
        lines: None,
        instance_env: "",
    };
    let lines = |n: u32| LabKw {
        lines: Some(n),
        ..none
    };
    add("s1-plain".into(), "", none, Plan::S1Plain);
    add("x-rating-1".into(), DISMISS_OFF, none, Plan::XRating1);
    for off in [0.3, 1.0, 1.6, 1.9] {
        add(format!("s2-text-{off:.1}"), "", none, Plan::PersonText(off));
    }
    for (off, dg) in [
        (0.9, '1'),
        (0.9, '0'),
        (1.5, '0'),
        (1.8, '0'),
        (2.0, '0'),
        (2.2, '0'),
        (0.3, '0'),
    ] {
        add(
            format!("s2-digit{dg}-{off:.1}"),
            "",
            none,
            Plan::PersonDigit(off, dg),
        );
    }
    add("s3a-draft-standing".into(), "", none, Plan::S3a);
    add("s3b-draft-submit-slow".into(), QUIET, none, Plan::S3b);
    add("s4a-slow-first".into(), QUIET, none, Plan::S4a);
    add("s4b-esc-slow".into(), QUIET, none, Plan::S4b);
    add("s5a-double-zero".into(), DISMISS_OFF, none, Plan::S5a);
    add("s5b-zero-then-zero".into(), DISMISS_OFF, none, Plan::S5b);
    for off in [0.15, 0.4, 0.5, 0.6, 0.7, 0.8] {
        add(
            format!("s5d-zero-at-{off:.2}"),
            DISMISS_OFF,
            none,
            Plan::EarlyZero(off),
        );
    }
    add("s6b-esc-on-card".into(), DISMISS_OFF, none, Plan::S6b);
    add("s6c-numbered".into(), QUIET, none, Plan::S6c);
    add("s6e-rate-then-undo".into(), "", none, Plan::S6e);
    add(
        "s2-lease-hold".into(),
        "human_grace_s = 6",
        none,
        Plan::S2Lease,
    );
    for off in [1.5, 1.7, 1.78, 1.84, 1.9, 2.0, 2.15, 2.3, 2.5] {
        add(format!("s2c-text-{off:.2}"), "", none, Plan::TextSweep(off));
    }
    add("s7-zero-enter".into(), DISMISS_OFF, none, Plan::S7);
    add(
        "s7-zero-enter-early".into(),
        DISMISS_OFF,
        none,
        Plan::S7Early,
    );
    add("s6f-rate-then-turn".into(), QUIET, none, Plan::S6f);
    add("x-startup-notice".into(), "", notice, Plan::Startup);
    add("x-startup-nonotice".into(), "", survey, Plan::Startup);
    add("x-startup-probe".into(), "", notice, Plan::StartupProbe);
    add(
        "x-reader-notice-launch".into(),
        DISMISS_OFF,
        notice,
        Plan::ReaderProbe(true),
    );
    add(
        "x-reader-nonotice-launch".into(),
        DISMISS_OFF,
        survey,
        Plan::ReaderProbe(true),
    );
    add(
        "x-reader-nonotice-afterturn".into(),
        DISMISS_OFF,
        none,
        Plan::ReaderProbe(false),
    );
    for n in [46, 47, 48, 50, 52, 53, 54, 56] {
        add(
            format!("x-tall-post-{n}"),
            "",
            lines(n),
            Plan::Tall(n, false),
        );
    }
    for n in [46, 47, 48, 50] {
        add(
            format!("x-tall-startup-{n}"),
            "",
            LabKw {
                lines: Some(n),
                survey: true,
                first_run: false,
                instance_env: "",
            },
            Plan::Tall(n, true),
        );
    }
    for n in [49, 50, 52] {
        add(
            format!("x-tall-notice-{n}"),
            "",
            LabKw {
                lines: Some(n),
                ..notice
            },
            Plan::Tall(n, true),
        );
    }
    add(
        "s8-bg-busy-parked".into(),
        HELD_QUIET,
        none,
        Plan::S8BusyParked,
    );
    add("s8-bg-static".into(), QUIET, none, Plan::S8Static);
    for n in [53, 52] {
        add(format!("x-tall-long-{n}"), "", lines(n), Plan::TallLong(n));
    }
    for ttl in [20_000, 26_000, 31_000] {
        add(
            format!("s8-lease-ttl{}", ttl / 1000),
            HELD_QUIET,
            none,
            Plan::S8Lease(ttl),
        );
    }
    for n in [53, 50] {
        add(format!("x-tail-dump-{n}"), "", lines(n), Plan::TailDump(n));
    }
    add("c1-approval".into(), "", none, Plan::CorpusApproval);
    add("c2-question".into(), "", none, Plan::CorpusQuestion);
    add("c3-wall".into(), "", none, Plan::CorpusWall);
    add("c4-read".into(), "", none, Plan::CorpusRead);
    for scene in crate::autopilot_scenes::Scene::ALL {
        add(
            scene.name().into(),
            scene.harness_keys(),
            scene.lab_kw(),
            Plan::Autopilot(scene),
        );
    }
    add(
        crate::occupancy_break::NAME.into(),
        crate::occupancy_break::HARNESS_KEYS,
        LabKw {
            instance_env: crate::occupancy_break::INSTANCE_ENV,
            ..none
        },
        Plan::OccupancyBreak,
    );
    v
}

const WALL_500: &str = r#"{"status": 500, "headers": {"x-should-retry": "false"}, "json": {"type": "error", "error": {"type": "api_error", "message": "Internal server error"}}, "times": 1}"#;

pub(crate) fn journaled_after(
    ctx: &mut Ctx,
    from: usize,
    needle: &str,
    timeout: f64,
) -> R<Option<usize>> {
    let end = now() + timeout;
    loop {
        if let Some(at) = ctx
            .journal()
            .iter()
            .skip(from)
            .position(|(_, r)| rec_s(r, "line").contains(needle))
        {
            ctx.mark(&format!("journaled {needle}"), None);
            return Ok(Some(from + at));
        }
        if now() >= end {
            ctx.put("error", format!("no {needle} journaled"));
            return Ok(None);
        }
        stop::sleep(std::time::Duration::from_millis(200))?;
    }
}

fn corpus_decision(
    ctx: &mut Ctx,
    start: impl FnOnce(&mut Ctx) -> R,
    decided: &str,
    timeout: f64,
) -> R {
    let from = ctx.journal().len();
    start(ctx)?;
    let Some(at) = journaled_after(ctx, from, decided, timeout)? else {
        return Ok(());
    };
    if journaled_after(ctx, at + 1, "EVENT idle", 60.0)?.is_some() {
        ctx.put("decided", decided);
    }
    secs(2.0)
}

#[allow(clippy::too_many_lines)]
fn play(ctx: &mut Ctx, plan: Plan) -> R {
    match plan {
        Plan::S1Plain => {
            let (_, card) = plain_cycle(ctx, 3.5)?;
            if let Some(card) = card {
                let k = ctx.k;
                ctx.fixture(
                    &format!("survey-s1-card-run{k:02}"),
                    &format!("S1 plain case run {k}: the real card, first sight"),
                    &card,
                );
            }
        }
        Plan::XRating1 => {
            let (t, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 1.0)?;
            ctx.mark("press 1", None);
            ctx.ctl(&["send", "1"]);
            secs(9.0)?;
            let st = cycle_stats(&ctx.frames(), &ctx.journal(), t);
            merge_into(&mut ctx.sumd, &st);
        }
        Plan::PersonText(offset) => {
            let text = "hello";
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, offset)?;
            let t_type0 = ctx.type_text(text, 0.06)?;
            if ctx.k == 2 && offset >= 1.84 {
                secs(0.3)?;
                ctx.png("survey-race-0hello");
            }
            secs(7.0)?;
            let t_clear = ctx.mark("clear the draft (5 backspaces)", None);
            for _ in 0..text.len() {
                ctx.ctl(&["key", "backspace"]);
                secs(0.05)?;
            }
            secs(6.0)?;
            let frames = ctx.frames();
            ctx.put("offset", offset);
            ctx.put("card_at", card.wall);
            ctx.put("type_started_after_card", r3(t_type0 - card.wall));
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            ctx.put(
                "card_shown_after_first_char",
                frames.iter().any(|f| {
                    f.wall >= t_type0 && f.wall > t_type0 + 0.12 && f.wall < t_clear && f.card()
                }),
            );
            ctx.put(
                "card_returned_after_clear",
                frames.iter().any(|f| f.card() && f.wall > t_clear),
            );
            ctx.put(
                "composer_before_clear",
                frames
                    .iter()
                    .rev()
                    .find(|f| f.wall < t_clear)
                    .map_or(J::Null, |f| comp_j(f.comp())),
            );
            ctx.put(
                "composer_end",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            let jl = journal_lines(ctx, card.wall);
            put_journal(ctx, jl);
            if ctx.k == 1
                && let Some(fr) = frames.iter().find(|f| f.wall > t_type0 + 0.4 && !f.card())
            {
                let what = format!(
                    "a person typed {} {offset:.1} s after the card showed: the card is hidden by the text (Claude Code's abandon), the composer holds the text",
                    py_repr(text)
                );
                ctx.fixture("survey-person-typed-card-hidden", &what, fr);
            }
        }
        Plan::PersonDigit(offset, digit) => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, offset)?;
            let t_press = ctx.mark(&format!("person presses {digit}"), None);
            ctx.ctl(&["send", &digit.to_string()]);
            if ctx.k == 4
                && digit == '1'
                && ctx
                    .wait_for(
                        |f| f.rows.iter().any(|r| r.contains(PENDING.trim_end())),
                        5.0,
                        card.i,
                    )?
                    .is_some()
            {
                ctx.png("survey-after-1-pending");
            }
            secs(12.0)?;
            ctx.put("offset", offset);
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            let jl = journal_lines(ctx, card.wall);
            let dismissed = dismissed_in(&jl);
            ctx.put("journal", jl);
            let frames = ctx.frames();
            ctx.put(
                "composer_end",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            ctx.put("dismissed_journal", dismissed);
            if ctx.k == 1 {
                let steps: [(&str, &[&str], String); 4] = [
                    (
                        "pending",
                        &[PENDING],
                        format!("pending step after a person's {digit} (3 s, Esc undoes)"),
                    ),
                    (
                        "thanks",
                        &[THANKS],
                        format!("thanks step after a person's {digit}"),
                    ),
                    (
                        "transcript-ask",
                        &["Can Anthropic look"],
                        format!("transcript-share ask after a person's {digit}"),
                    ),
                    (
                        "followup",
                        &["What went wrong", "Tell us more", "What went well"],
                        format!("follow-up prompt after a person's {digit}"),
                    ),
                ];
                for (key, needles, what) in steps {
                    if let Some(fr) = frames.iter().find(|f| {
                        f.wall > t_press
                            && f.rows.iter().any(|r| needles.iter().any(|n| r.contains(n)))
                    }) {
                        ctx.fixture(&format!("survey-after-{digit}-{key}"), &what, fr);
                    }
                }
            }
        }
        Plan::S3a => {
            ctx.arm()?;
            let n0 = ctx.n_frames();
            let t = ctx.turn("hi")?;
            let Some(idle) = ctx.wait_idle_row(n0, 40.0)? else {
                ctx.put("error", "no idle row");
                return Ok(());
            };
            at(idle.wall, 0.4)?;
            ctx.type_text("draft", 0.06)?;
            let t_standing = now();
            secs(8.0)?;
            let frames = ctx.frames();
            ctx.put(
                "card_while_draft_stands",
                frames.iter().any(|f| f.card() && f.wall > t_standing),
            );
            ctx.put(
                "composer_while_draft_stands",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            let jl = journal_lines(ctx, idle.wall);
            ctx.put("journal_while_draft_stands", jl);
            let t_clear = ctx.mark("clear the draft (5 backspaces)", None);
            for _ in 0..5 {
                ctx.ctl(&["key", "backspace"]);
                secs(0.05)?;
            }
            let card = ctx.wait_for(|f| f.card() && f.wall > t_clear, 12.0, 0)?;
            if let Some(c) = &card {
                ctx.mark("card appears (after the draft was cleared)", Some(c.wall));
                ctx.put("card_after_clear_s", r3(c.wall - t_clear));
                ctx.wait_for(|f| !f.card(), 12.0, c.i)?;
            }
            secs(3.5)?;
            finish_states(ctx, idle.wall);
            if card.is_some() {
                let d = cycle_stats(&ctx.frames(), &ctx.journal(), t);
                ctx.put(
                    "card_to_zero",
                    d.get("card_to_zero").cloned().unwrap_or(J::Null),
                );
                ctx.put(
                    "zero_to_gone",
                    d.get("zero_to_gone").cloned().unwrap_or(J::Null),
                );
            }
            if ctx.k == 1
                && let Some(fr) = frames.iter().find(|f| {
                    f.wall > t_standing + 3.0 && !f.card() && f.comp().as_deref() == Some("draft")
                })
            {
                ctx.fixture(
                    "survey-draft-standing-no-card",
                    "a draft standing in the composer 6 s after the turn ended, with the forced survey open but invisible: no card is drawn",
                    fr,
                );
            }
        }
        Plan::S3b => {
            ctx.arm()?;
            let n0 = ctx.n_frames();
            ctx.turn("hi")?;
            let Some(idle) = ctx.wait_idle_row(n0, 40.0)? else {
                ctx.put("error", "no idle row");
                return Ok(());
            };
            at(idle.wall, 0.4)?;
            ctx.type_text("next", 0.06)?;
            secs(4.5)?;
            let before = ctx.frames().iter().any(|f| f.card() && f.wall > idle.wall);
            ctx.put("card_before_submit", before);
            ctx.mode("slow");
            let t_sub = ctx.mark("submit the draft (stub mode slow)", None);
            ctx.ctl(&["key", "enter"]);
            let card = ctx.wait_for(|f| f.card() && f.wall > t_sub, 45.0, 0)?;
            if let Some(c) = &card {
                ctx.mark("card appears over the turn", Some(c.wall));
                ctx.put("card_after_submit_s", r3(c.wall - t_sub));
                ctx.fixture(
                    "survey-card-over-a-streaming-turn",
                    "the forced card drawn while the stub streams a slow turn (a draft was submitted with the survey open but invisible)",
                    c,
                );
                if ctx.k == 4 {
                    ctx.png("survey-card-over-a-streaming-turn");
                }
                if let Some(g) = ctx.wait_for(|f| !f.card(), 60.0, c.i)? {
                    ctx.mark("card gone", Some(g.wall));
                    ctx.put("card_to_gone_s", r3(g.wall - c.wall));
                }
            }
            secs(if card.is_some() { 12.0 } else { 40.0 })?;
            ctx.mode("ok");
            finish_states(ctx, idle.wall);
        }
        Plan::S4a => {
            ctx.arm()?;
            ctx.mode("slow");
            let n0 = ctx.n_frames();
            let t = ctx.turn("hi")?;
            if let Some(c) = ctx.wait_for(Frame::card, 60.0, n0)? {
                ctx.mark("card appears", Some(c.wall));
                ctx.wait_for(|f| !f.card(), 15.0, c.i)?;
            }
            secs(3.5)?;
            ctx.mode("ok");
            let d = cycle_stats(&ctx.frames(), &ctx.journal(), t);
            merge_into(&mut ctx.sumd, &d);
            let last_tick = ctx
                .frames()
                .iter()
                .rev()
                .find(|f| status_row(&f.rows).is_some())
                .map_or(J::Null, |f| r2(f.wall - t));
            ctx.put("last_tick_frame_s", last_tick);
            finish_states(ctx, t);
        }
        Plan::S4b => {
            ctx.arm()?;
            ctx.mode("slow");
            ctx.turn("hi")?;
            secs(6.0)?;
            let t_esc = ctx.mark("Esc (interrupt the slow turn)", None);
            ctx.ctl(&["key", "escape"]);
            if let Some(c) = ctx.wait_for(|f| f.card() && f.wall > t_esc, 20.0, 0)? {
                ctx.mark("card appears", Some(c.wall));
                ctx.put("esc_to_card_s", r3(c.wall - t_esc));
                ctx.fixture(
                    "survey-card-after-esc",
                    "the forced card drawn 2 s after a slow turn was interrupted with Esc",
                    &c,
                );
                ctx.wait_for(|f| !f.card(), 15.0, c.i)?;
            }
            secs(4.0)?;
            ctx.mode("ok");
            let frames = ctx.frames();
            let intr = frames
                .iter()
                .find(|f| f.wall > t_esc && f.rows.iter().any(|r| r.contains("Interrupted")));
            if let Some(i) = intr
                && ctx.k == 1
            {
                ctx.fixture(
                    "interrupted-by-esc",
                    "a slow stub turn interrupted with Esc (the interrupted row)",
                    i,
                );
            }
            let rows: Vec<J> = intr
                .map(|f| {
                    f.rows
                        .iter()
                        .filter(|r| r.contains("Interrupted"))
                        .map(|r| J::s(r.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            ctx.put("interrupted_row", J::Arr(rows));
            finish_states(ctx, t_esc - 1.0);
        }
        Plan::S5a => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 0.9)?;
            ctx.mark("0 (first)", None);
            ctx.ctl(&["send", "0"]);
            secs(0.05)?;
            ctx.mark("0 (second)", None);
            ctx.ctl(&["send", "0"]);
            secs(6.0)?;
            let frames = ctx.frames();
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            ctx.put(
                "composer_end",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            ctx.put(
                "card_end",
                frames.last().map_or(J::Null, |f| J::Bool(f.card())),
            );
            let t_clear = ctx.mark("clear: 3 backspaces", None);
            for _ in 0..3 {
                ctx.ctl(&["key", "backspace"]);
                secs(0.05)?;
            }
            secs(5.0)?;
            let frames = ctx.frames();
            ctx.put(
                "card_returned_after_clear",
                frames.iter().any(|f| f.card() && f.wall > t_clear),
            );
            ctx.put(
                "composer_after_clear",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            if ctx.k == 1
                && let Some(fr) = frames
                    .iter()
                    .find(|f| f.wall > card.wall && f.comp().as_deref() == Some("00"))
            {
                ctx.fixture(
                    "survey-double-zero",
                    "two 0s typed 50 ms apart at card+0.9 s: the composer holds 00 and the card is gone (abandoned)",
                    fr,
                );
                ctx.png("survey-double-zero");
            }
            finish_states(ctx, card.wall);
        }
        Plan::S5b => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 0.9)?;
            ctx.mark("0 (first)", None);
            ctx.ctl(&["send", "0"]);
            at(card.wall, 2.2)?;
            ctx.mark("0 (second, the card is gone)", None);
            ctx.ctl(&["send", "0"]);
            secs(5.0)?;
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            ctx.put(
                "composer_end",
                ctx.frames().last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            finish_states(ctx, card.wall);
        }
        Plan::EarlyZero(offset) => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, offset)?;
            ctx.mark(&format!("0 at card+{offset:.2}"), None);
            ctx.ctl(&["send", "0"]);
            secs(4.5)?;
            let frames = ctx.frames();
            ctx.put("offset", offset);
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            ctx.put(
                "composer_end",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            ctx.put(
                "card_end",
                frames.last().map_or(J::Null, |f| J::Bool(f.card())),
            );
            let zero = frames
                .iter()
                .find(|f| f.wall > card.wall && f.comp().is_some_and(|c| c.trim() == "0"));
            let gone = frames.iter().find(|f| f.wall > card.wall && !f.card());
            ctx.put(
                "zero_to_gone",
                match (zero, gone) {
                    (Some(z), Some(g)) => r3(g.wall - z.wall),
                    _ => J::Null,
                },
            );
            ctx.put("taken", gone.is_some());
            if ctx.k == 1
                && gone.is_none()
                && let Some(last) = frames.last()
            {
                let what = format!(
                    "a 0 typed {offset:.2} s after the card showed is not taken: it stays in the composer as text under the open card"
                );
                ctx.fixture("survey-zero-dropped-in-mount-window", &what, last);
            }
            finish_states(ctx, card.wall);
        }
        Plan::S6b => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 1.0)?;
            ctx.mark("Esc on the open card", None);
            ctx.ctl(&["key", "escape"]);
            secs(5.0)?;
            let frames = ctx.frames();
            ctx.put(
                "card_end",
                frames.last().map_or(J::Null, |f| J::Bool(f.card())),
            );
            ctx.put(
                "composer_end",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            finish_states(ctx, card.wall);
        }
        Plan::S6c => {
            ctx.arm()?;
            ctx.mode("numbered");
            let n0 = ctx.n_frames();
            let t = ctx.turn("hi")?;
            let card = ctx.wait_for(Frame::card, 9.0, n0)?;
            ctx.put("card_after_numbered_reply", card.is_some());
            let frames = ctx.frames();
            if ctx.k == 1
                && let Some(fr) = frames
                    .iter()
                    .rev()
                    .find(|f| f.rows.iter().any(|r| r.contains("1. The first option")))
            {
                ctx.fixture(
                    "numbered-reply-no-survey",
                    "a reply holding a numbered list, 8 s after it ended, with the survey forced: no card is drawn",
                    fr,
                );
            }
            ctx.mode("ok");
            let n1 = ctx.n_frames();
            ctx.turn("again")?;
            let card2 = ctx.wait_for(Frame::card, 12.0, n1)?;
            ctx.put("card_after_plain_reply", card2.is_some());
            if let Some(c) = card2 {
                ctx.mark("card appears (second turn)", Some(c.wall));
                ctx.wait_for(|f| !f.card(), 12.0, c.i)?;
            }
            secs(3.5)?;
            finish_states(ctx, t);
        }
        Plan::S6e => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 0.9)?;
            ctx.mark("person presses 1", None);
            ctx.ctl(&["send", "1"]);
            if let Some(pend) = ctx.wait_for(
                |f| f.rows.iter().any(|r| r.contains(PENDING.trim_end())),
                5.0,
                card.i,
            )? {
                ctx.mark("pending step shown", Some(pend.wall));
                at(pend.wall, 1.0)?;
                let t_esc = ctx.mark("person presses Esc (undo)", None);
                ctx.ctl(&["key", "escape"]);
                let back = ctx.wait_for(|f| f.card() && f.wall > t_esc, 5.0, 0)?;
                ctx.put(
                    "card_back_after_undo_s",
                    back.as_ref().map_or(J::Null, |b| r3(b.wall - t_esc)),
                );
                if let Some(b) = back {
                    ctx.mark("card is back", Some(b.wall));
                    ctx.wait_for(|f| !f.card(), 12.0, b.i)?;
                }
            }
            secs(4.0)?;
            finish_states(ctx, card.wall);
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
        }
        Plan::S2Lease => {
            let (t, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 0.3)?;
            let r = ctx.ctl(&["lease", "acquire", "ttl=4000", "holder=person-sim"]);
            let said: String = format!("{}{}", r.stdout, r.stderr)
                .trim()
                .chars()
                .take(80)
                .collect();
            ctx.mark(
                &format!("lease acquire ttl=4000 holder=person-sim -> {said}"),
                None,
            );
            let mut samples = Vec::new();
            let t_end = now() + 16.0;
            while now() < t_end {
                let st = ctx.ctl(&["status"]).stdout;
                let f = |k| field(&st, k).map_or(J::Null, J::Str);
                samples.push(J::Arr(vec![
                    r2(now() - card.wall),
                    f("hand"),
                    f("human_ms"),
                ]));
                secs(1.0)?;
                if ctx.rec().last().is_some_and(|fr| !fr.card()) && now() - card.wall > 3.0 {
                    break;
                }
            }
            secs(2.0)?;
            ctx.put("status_hand_samples", J::Arr(samples));
            let d = cycle_stats(&ctx.frames(), &ctx.journal(), t);
            for k in ["card_to_zero", "zero_to_gone", "card_to_gone", "t_zero"] {
                if let Some(v) = d.get(k) {
                    ctx.put(k, v.clone());
                }
            }
            finish_states(ctx, card.wall);
        }
        Plan::TextSweep(offset) => {
            let text = "hello";
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, offset)?;
            ctx.type_text(text, 0.06)?;
            secs(3.5)?;
            let frames = ctx.frames();
            ctx.put("offset", offset);
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            ctx.put(
                "composer_end",
                frames.last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            let jl = journal_lines(ctx, card.wall);
            put_journal(ctx, jl);
            let first = frames
                .iter()
                .find(|f| f.wall > card.wall && !f.comp().as_deref().unwrap_or("").is_empty());
            ctx.put(
                "first_nonempty_composer",
                first.map_or(J::Null, |f| {
                    J::Arr(vec![r3(f.wall - card.wall), comp_j(f.comp())])
                }),
            );
            ctx.put(
                "stray_zero_head",
                frames
                    .last()
                    .and_then(Frame::comp)
                    .is_some_and(|c| c.starts_with('0')),
            );
        }
        Plan::S7 | Plan::S7Early => {
            let early = matches!(plan, Plan::S7Early);
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, if early { 0.3 } else { 0.9 })?;
            ctx.mark(
                if early {
                    "0 (inside the mount window)"
                } else {
                    "0"
                },
                None,
            );
            ctx.ctl(&["send", "0"]);
            secs(0.05)?;
            let guard = format!(
                "if=^●.{}",
                aterm_phase::anchors::guard_prefix("survey.question", 4)
            );
            let label = if early {
                "Enter, guarded by the card row".to_string()
            } else {
                format!("Enter, guarded by {guard}")
            };
            ctx.mark(&label, None);
            let r = ctx.ctl(&["key", &guard, "enter"]);
            ctx.put(
                "enter_reply",
                format!("{}{}", r.stdout, r.stderr).trim().to_string(),
            );
            secs(if early { 5.0 } else { 4.0 })?;
            let states = comp_states(ctx, card.wall, None);
            ctx.put("states_from_card", states);
            ctx.put(
                "composer_end",
                ctx.frames().last().map_or(J::Null, |f| comp_j(f.comp())),
            );
            let mains = stub_main_lines(ctx);
            ctx.put(
                "stub_main_requests",
                mains
                    .as_ref()
                    .map_or(J::Null, |m| J::Int(i64::try_from(m.len()).unwrap_or(0))),
            );
            if early {
                ctx.put(
                    "last_user_texts",
                    mains.map_or(J::Null, |m| {
                        J::Arr(
                            m.iter()
                                .filter_map(|l| last_user_of(l))
                                .map(J::Str)
                                .collect(),
                        )
                    }),
                );
            }
            finish_states(ctx, card.wall);
        }
        Plan::S6f => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            at(card.wall, 0.9)?;
            ctx.mark("person presses 1", None);
            ctx.ctl(&["send", "1"]);
            let pend = ctx.wait_for(
                |f| f.rows.iter().any(|r| r.contains(PENDING.trim_end())),
                5.0,
                card.i,
            )?;
            ctx.mode("slow");
            at(pend.map_or_else(now, |p| p.wall), 0.8)?;
            let t_turn2 = ctx.turn("go")?;
            let feedback = |r: &String| r.contains(PENDING.trim_end()) || r.contains(THANKS);
            let mut phases = Vec::new();
            for _ in 0..10 {
                secs(1.0)?;
                let fr = ctx.rec().last();
                let rows: Vec<J> = fr
                    .as_ref()
                    .map(|f| {
                        f.rows
                            .iter()
                            .filter(|r| feedback(r))
                            .take(1)
                            .map(|r| J::s(r.as_str()))
                            .collect()
                    })
                    .unwrap_or_default();
                let phase = ctx
                    .drive(&["phase", &format!("@{}", ctx.sid)])
                    .replace('\n', " | ");
                phases.push(J::Arr(vec![
                    J::Float(((now() - t_turn2) * 10.0).round() / 10.0),
                    J::s(phase),
                    J::Arr(rows),
                    fr.map_or(J::Null, |f| J::Bool(status_row(&f.rows).is_some())),
                ]));
            }
            ctx.put(
                "drive_phase_while_feedback_rows_and_spinner",
                J::Arr(phases),
            );
            secs(5.0)?;
            ctx.mode("ok");
            let frames = ctx.frames();
            let hidden: Vec<J> = frames
                .iter()
                .filter(|f| {
                    f.wall > t_turn2
                        && status_row(&f.rows).is_none()
                        && f.rows.iter().any(&feedback)
                })
                .take(10)
                .map(|f| r2(f.wall - t_turn2))
                .collect();
            ctx.put(
                "frames_with_feedback_rows_and_no_spinner_after_turn2",
                J::Arr(hidden),
            );
            let agent: Vec<J> = ctx
                .events()
                .iter()
                .filter(|(w, _, l)| l.contains(" agent ") && *w > card.wall)
                .map(|(w, _, l)| J::Arr(vec![r2(w - t_turn2), J::s(l.get(8..).unwrap_or(""))]))
                .collect();
            ctx.put("agent_events", J::Arr(agent));
            if ctx.k == 1
                && let Some(fr) = frames.iter().find(|f| {
                    f.wall > t_turn2
                        && status_row(&f.rows).is_some()
                        && f.rows.iter().any(&feedback)
                })
            {
                ctx.fixture(
                    "survey-thanks-row-over-a-busy-turn",
                    "the rating's thanks/pending rows drawn together with a running turn's spinner (a person rated 1, then sent a slow turn)",
                    fr,
                );
            }
            finish_states(ctx, card.wall);
        }
        Plan::Startup => {
            let first_run = ctx.kw.first_run;
            let t0 = now();
            ctx.mark("startup observation begins", None);
            let mut samples = Vec::new();
            for _ in 0..25 {
                let st = ctx.ctl(&["status"]).stdout;
                let cur = ctx.ctl(&["cursor"]).stdout.trim().to_string();
                let f = |k| field(&st, k).map_or(J::Null, J::Str);
                samples.push(J::Arr(vec![
                    r2(now() - t0),
                    f("agent"),
                    f("phase"),
                    J::s(cur),
                ]));
                secs(1.0)?;
            }
            ctx.put("samples", J::Arr(samples));
            let frames = ctx.frames();
            let card = frames.iter().find(|f| f.card());
            ctx.put("card_first_seen", card.map_or(J::Null, |c| r2(c.wall - t0)));
            let states = comp_states(ctx, t0, None);
            ctx.put("states", states);
            let jl = journal_lines(ctx, t0);
            ctx.put("journal", jl);
            let agent: Vec<J> = ctx
                .events()
                .iter()
                .filter(|(_, _, l)| l.contains(" agent "))
                .map(|(w, _, l)| J::Arr(vec![r2(w - t0), J::s(l.get(8..).unwrap_or(""))]))
                .collect();
            ctx.put("agent_events", J::Arr(agent));
            if let Some(c) = card
                && ctx.k == 1
            {
                let (name, seen) = if first_run {
                    ("survey-at-startup-with-notice", "standing")
                } else {
                    ("survey-at-startup-no-notice", "already seen")
                };
                ctx.fixture(
                    name,
                    &format!("startup with the force flag set at launch, first-run notice {seen}"),
                    c,
                );
            }
            finish_states(ctx, t0);
        }
        Plan::StartupProbe => {
            let mut out = Vec::new();
            for i in 0..6 {
                out.push(J::Arr(vec![
                    J::Float(f64::from(i)),
                    J::s(ctx.drive(&["phase", &format!("@{}", ctx.sid)])),
                ]));
                secs(1.0)?;
            }
            ctx.put("drive_phase", J::Arr(out));
            ctx.put("status", ctx.ctl(&["status"]).stdout);
            ctx.put("cursor", ctx.ctl(&["cursor"]).stdout.trim().to_string());
            ctx.put(
                "cell_13_2",
                ctx.ctl(&["cell", "13", "2"]).stdout.trim().to_string(),
            );
            ctx.put(
                "cell_12_0",
                ctx.ctl(&["cell", "12", "0"]).stdout.trim().to_string(),
            );
            let t0 = ctx.marks.first().map_or_else(now, |m| m.0);
            let jl = journal_lines(ctx, t0);
            ctx.put("journal", jl);
        }
        Plan::ReaderProbe(at_launch) => {
            if !at_launch {
                card_cycle(ctx, "hi")?;
            }
            secs(1.0)?;
            let at_sid = format!("@{}", ctx.sid);
            ctx.put("phase_1", ctx.drive(&["phase", &at_sid]));
            ctx.put("read", ctx.ctl(&["text"]).stdout.trim().to_string());
            ctx.put(
                "ctl_text_rows",
                J::Int(i64::try_from(ctx.ctl(&["text"]).stdout.matches('\n').count()).unwrap_or(0)),
            );
            secs(2.0)?;
            ctx.put("phase_2", ctx.drive(&["phase", &at_sid]));
            ctx.put("status_agent", status_of(ctx, "agent"));
            let t0 = ctx.marks.first().map_or_else(now, |m| m.0);
            let jl = journal_lines(ctx, t0);
            ctx.put("journal", jl);
        }
        Plan::Tall(lines, startup) => tall(ctx, lines, startup)?,
        Plan::TallLong(lines) => {
            ctx.mode("long");
            tall(ctx, lines, false)?;
            ctx.mode("ok");
        }
        Plan::S8BusyParked | Plan::S8Lease(_) => {
            let ttl_ms = if let Plan::S8Lease(t) = plan {
                t
            } else {
                26_000
            };
            let parked = matches!(plan, Plan::S8BusyParked);
            ctx.arm()?;
            let n0 = ctx.n_frames();
            ctx.turn("bgstart")?;
            let Some(card) = ctx.wait_card(n0, 60.0)? else {
                ctx.put("error", "no card");
                if parked {
                    ctx.put("screen", ctx.ctl(&["text", "trim"]).stdout);
                }
                return Ok(());
            };
            at(card.wall, 0.3)?;
            let r = ctx.ctl(&[
                "lease",
                "acquire",
                &format!("ttl={ttl_ms}"),
                "holder=person-sim",
            ]);
            let said: String = format!("{}{}", r.stdout, r.stderr)
                .trim()
                .chars()
                .take(60)
                .collect();
            let label = if parked {
                format!("lease acquire ttl=26000 -> {said}")
            } else {
                format!("lease acquire ttl={ttl_ms} -> {said}")
            };
            let t_lease = ctx.mark(&label, None);
            if let Some(b) = ctx.wait_for(
                |f| f.card() && status_row(&f.rows).is_some() && f.wall > card.wall + 1.0,
                60.0,
                card.i,
            )? {
                ctx.mark("card parked over a busy turn (first frame)", Some(b.wall));
                ctx.put("busy_with_card_after_card_s", r2(b.wall - card.wall));
                if parked {
                    ctx.fixture(
                        "survey-card-parked-over-a-background-wake-turn",
                        "the forced card parked over a turn Claude Code started by itself when its background Bash task ended (the stub answers the wake slowly)",
                        &b,
                    );
                }
            }
            let gone = ctx.wait_for(
                |f| !f.card() && f.wall > card.wall + 1.0,
                if parked { 75.0 } else { 90.0 },
                card.i,
            )?;
            let ttl = f64::from(ttl_ms) / 1000.0;
            if parked {
                if let Some(g) = &gone {
                    ctx.mark("card gone", Some(g.wall));
                    ctx.put("card_to_gone_s", r2(g.wall - card.wall));
                    ctx.put("lease_end_to_gone_s", r2(g.wall - (t_lease + 26.0)));
                    ctx.put("gone_while_busy", status_row(&g.rows).is_some());
                }
                secs(4.0)?;
                let tail = crate::sys::scratch_read(&ctx.run.w.join("stub.log"))
                    .ok()
                    .flatten()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .map(|t| {
                        let kept: Vec<String> = t
                            .lines()
                            .filter(|l| !l.contains("PROXY refused"))
                            .map(|l| l.trim_end().chars().take(200).collect())
                            .collect();
                        J::Arr(
                            kept[kept.len().saturating_sub(12)..]
                                .iter()
                                .map(|l| J::s(l.as_str()))
                                .collect(),
                        )
                    });
                ctx.put("stub_log_tail", tail.unwrap_or(J::Null));
            } else {
                let frames = ctx.frames();
                let zero = frames.iter().find(|f| {
                    f.wall > card.wall + 1.0
                        && f.card()
                        && f.comp().is_some_and(|c| c.trim() == "0")
                });
                if let Some(g) = &gone {
                    ctx.mark("card gone", Some(g.wall));
                    ctx.put("card_to_gone_s", r2(g.wall - card.wall));
                    ctx.put(
                        "card_to_zero_s",
                        zero.map_or(J::Null, |z| r2(z.wall - card.wall)),
                    );
                    ctx.put(
                        "lease_end_to_zero_s",
                        zero.map_or(J::Null, |z| r2(z.wall - (t_lease + ttl))),
                    );
                    ctx.put("gone_while_busy", status_row(&g.rows).is_some());
                }
                secs(3.0)?;
                let held: Vec<J> = ctx
                    .journal()
                    .iter()
                    .filter(|(_, r)| rec_s(r, "line").contains("HELD"))
                    .map(|(_, r)| r2(rec_t(r) - card.wall))
                    .collect();
                ctx.put("held_reads_s", J::Arr(held));
            }
            finish_states(ctx, card.wall);
        }
        Plan::S8Static => {
            ctx.arm()?;
            let n0 = ctx.n_frames();
            ctx.turn("bgstart")?;
            let Some(card) = ctx.wait_card(n0, 60.0)? else {
                return no_card(ctx);
            };
            ctx.fixture(
                "survey-card-over-a-background-shell",
                "the forced card 2 s after a turn that left a Bash task running in the background (footer `1 shell`, row `done … · 1 shell still running`)",
                &card,
            );
            let gone = ctx.wait_for(|f| !f.card(), 30.0, card.i)?;
            let frames = ctx.frames();
            let zero = frames.iter().find(|f| {
                f.wall > card.wall && f.card() && f.comp().is_some_and(|c| c.trim() == "0")
            });
            ctx.put(
                "card_to_zero",
                zero.map_or(J::Null, |z| r3(z.wall - card.wall)),
            );
            ctx.put(
                "zero_to_gone",
                match (zero, &gone) {
                    (Some(z), Some(g)) => r3(g.wall - z.wall),
                    _ => J::Null,
                },
            );
            let agent: Vec<J> = ctx
                .events()
                .iter()
                .filter(|(w, _, l)| l.contains(" agent ") && (w - card.wall).abs() < 3.0)
                .map(|(_, _, l)| J::s(l.get(8..).unwrap_or("")))
                .collect();
            ctx.put("agent_at_card", J::Arr(agent));
            ctx.put(
                "screen_status_row_at_zero",
                zero.and_then(|z| status_row(&z.rows))
                    .map_or(J::Null, J::Str),
            );
            ctx.put(
                "footer_at_card",
                J::Arr(
                    card.rows
                        .iter()
                        .filter(|r| r.contains("auto mode"))
                        .map(|r| J::s(r.as_str()))
                        .collect(),
                ),
            );
            secs(3.0)?;
            finish_states(ctx, card.wall);
        }
        Plan::TailDump(lines) => {
            let (_, card) = card_cycle(ctx, "hi")?;
            let Some(card) = card else {
                return no_card(ctx);
            };
            secs(0.6)?;
            let tail = ctx.ctl(&["text", "--json", "tail=40"]).stdout;
            let whole = ctx.ctl(&["text", "--json"]).stdout;
            let mut out = J::Obj(Vec::new());
            for (name, txt) in [("tail40", tail), ("whole", whole)] {
                let Ok(j) = J::parse(&txt) else {
                    out.set(
                        name,
                        J::obj([("error", J::s(txt.chars().take(200).collect::<String>()))]),
                    );
                    continue;
                };
                let rows: Vec<String> = j
                    .get("rows")
                    .and_then(J::as_arr)
                    .unwrap_or_default()
                    .iter()
                    .map(|r| r.as_str().unwrap_or("").to_string())
                    .collect();
                let first = j.get("first").and_then(J::as_i64).unwrap_or(0);
                let at_rows = |pred: &dyn Fn(&str) -> bool| {
                    J::Arr(
                        rows.iter()
                            .enumerate()
                            .filter(|(_, r)| pred(r))
                            .map(|(i, _)| J::Int(i64::try_from(i).unwrap_or(0) + first))
                            .collect(),
                    )
                };
                let mut keys: Vec<String> = match &j {
                    J::Obj(m) => m.iter().map(|(k, _)| k.clone()).collect(),
                    _ => Vec::new(),
                };
                keys.sort();
                out.set(
                    name,
                    J::obj([
                        ("first", j.get("first").cloned().unwrap_or(J::Null)),
                        ("n_rows", J::Int(i64::try_from(rows.len()).unwrap_or(0))),
                        ("rows_with_card", at_rows(&is_card_row)),
                        ("options_rows", at_rows(&is_options_row)),
                        (
                            "first_rows",
                            J::Arr(
                                rows.iter()
                                    .take(5)
                                    .map(|r| J::s(r.chars().take(60).collect::<String>()))
                                    .collect(),
                            ),
                        ),
                        ("keys", J::Arr(keys.into_iter().map(J::Str).collect())),
                    ]),
                );
            }
            ctx.put("reads", out);
            ctx.put("lines", J::Int(i64::from(lines)));
            ctx.put("phase", ctx.drive(&["phase", &format!("@{}", ctx.sid)]));
            secs(5.0)?;
            let jl = journal_lines(ctx, card.wall);
            put_journal(ctx, jl);
        }
        Plan::CorpusApproval => corpus_decision(
            ctx,
            |ctx| {
                crate::verbs::set_tool(
                    &ctx.run,
                    r#"[{"name": "Bash", "input": {"command": "touch @PROJ@/corpus-approval.txt", "description": "Make a file"}}, {"text": "Done."}]"#,
                )?;
                ctx.turn("toolstart").map(|_| ())
            },
            "APPROVED",
            60.0,
        )?,
        Plan::CorpusQuestion => corpus_decision(
            ctx,
            |ctx| {
                crate::verbs::set_tool(
                    &ctx.run,
                    r#"[{"name": "AskUserQuestion", "input": {"questions": [{"question": "Which color should the file be?", "header": "Color", "multiSelect": false, "options": [{"label": "Red", "description": "A warm color"}, {"label": "Blue", "description": "A cool color"}]}]}}, {"text": "Done."}]"#,
                )?;
                ctx.turn("toolstart").map(|_| ())
            },
            "CHOSE",
            60.0,
        )?,
        Plan::CorpusWall => corpus_decision(
            ctx,
            |ctx| {
                let spec =
                    J::parse(WALL_500).map_err(|e| ScnErr::Failed(format!("WALL_500: {e}")))?;
                crate::verbs::set_custom_mode(&ctx.run, "wall500", &spec)?;
                ctx.turn("say hello").map(|_| ())
            },
            "rule=api-retry@v1",
            120.0,
        )?,
        Plan::CorpusRead => corpus_decision(
            ctx,
            |ctx| {
                crate::verbs::set_tool(
                    &ctx.run,
                    r#"[{"name": "Read", "input": {"file_path": "/etc/hosts"}}, {"text": "Done."}]"#,
                )?;
                ctx.turn("toolstart").map(|_| ())
            },
            "APPROVED",
            60.0,
        )?,
        Plan::OccupancyBreak => crate::occupancy_break::play(ctx)?,
        Plan::Autopilot(scene) => crate::autopilot_scenes::play(ctx, scene)?,
    }
    Ok(())
}

fn tall(ctx: &mut Ctx, lines: u32, startup: bool) -> R {
    let t0 = now();
    let card = if startup {
        secs(1.0)?;
        ctx.wait_for(Frame::card, 5.0, 0)?
    } else {
        card_cycle(ctx, "hi")?.1
    };
    ctx.put("lines", J::Int(i64::from(lines)));
    ctx.put("card_on_screen", card.is_some());
    if let Some(c) = &card {
        ctx.mark("card appears", Some(c.wall));
    }
    secs(1.0)?;
    let at_sid = format!("@{}", ctx.sid);
    ctx.put("phase_1", ctx.drive(&["phase", &at_sid]));
    secs(7.0)?;
    let frames = ctx.frames();
    ctx.put(
        "card_end",
        frames.last().map_or(J::Null, |f| J::Bool(f.card())),
    );
    ctx.put("phase_2", ctx.drive(&["phase", &at_sid]));
    ctx.put("agent", status_of(ctx, "agent"));
    ctx.put(
        "composer_end",
        frames.last().map_or(J::Null, |f| comp_j(f.comp())),
    );
    ctx.put("card_row_index", J::Null);
    let jl = journal_lines(ctx, t0);
    put_journal(ctx, jl);
    let text = ctx.ctl(&["text"]).stdout;
    if let Some(i) = text
        .split('\n')
        .enumerate()
        .filter(|(_, r)| is_card_row(r))
        .map(|(i, _)| i)
        .last()
    {
        ctx.put("card_row_index", J::Int(i64::try_from(i).unwrap_or(0)));
    }
    ctx.put("tail_first_row", J::Int(i64::from(lines) - 40));
    if ctx.k == 3 && frames.last().is_some_and(Frame::card) {
        ctx.png(&format!("survey-card-above-the-tail-144x{lines}"));
    }
    Ok(())
}

/// Runs scenario `s` once as run number `k` on a fresh lab.
/// # Errors
/// Interrupted (the lab is taken down first).
pub fn run_one(s: &Scenario, k: u32, tag: &str) -> Result<Ctx, Stopped> {
    let mut ctx = Ctx::new(&s.name, k, tag, s.harness_keys, s.kw, false, true);
    let result = (|| -> R {
        ctx.start()?;
        if s.name.starts_with("s8-") {
            ctx.setup_bg(14, 40);
        }
        let launch = match s.plan {
            Plan::CorpusApproval | Plan::CorpusRead => Launch {
                args: vec!["--permission-mode".into(), "manual".into()],
                ..Launch::default()
            },
            Plan::Autopilot(scene) => crate::autopilot_scenes::prepare(&ctx, scene)?,
            _ => Launch::default(),
        };
        ctx.launch(60.0, launch)?;
        play(&mut ctx, s.plan)
    })();
    let stopped = match result {
        Ok(()) => None,
        Err(ScnErr::Stopped(st)) => Some(st),
        Err(ScnErr::Failed(e)) => {
            ctx.put("exception", e.clone());
            eprintln!("EXCEPTION in {}-{k:02}: {e}", s.name);
            None
        }
    };
    ctx.save_timeline();
    ctx.finish();
    ctx.save_summary();
    stopped.map_or(Ok(ctx), Err)
}

fn brief(ctx: &Ctx) -> J {
    let mut b = J::Obj(Vec::new());
    for k in [
        "idle_to_card",
        "card_to_zero",
        "zero_to_gone",
        "gone_to_journal",
        "composer_after",
        "composer_states",
        "error",
    ] {
        if let Some(v) = ctx.sumd.get(k) {
            b.set(k, v.clone());
        }
    }
    if let Some(J::Str(e)) = ctx.sumd.get("exception") {
        let tail: String = e
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        b.set("exception", J::s(tail));
    }
    b
}

const fn pyline() -> Style {
    Style {
        indent: None,
        compact: false,
        ascii: false,
    }
}

/// `scn list|run|show|agg`.
pub fn cmd_scn(args: &[String]) -> crate::Res {
    let tag = std::env::var("LAB_TAG")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "survey".into());
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            let mut names: Vec<String> = scenarios().into_iter().map(|s| s.name).collect();
            names.sort();
            println!("{}", names.join("\n"));
            Ok(0)
        }
        "show" => {
            let run = args
                .get(1)
                .ok_or_else(|| crate::Fail::usage("scn show <run>"))?;
            let p = results_dir().join(run).join("summary.json");
            let text = fs::read_to_string(&p)
                .map_err(|e| crate::Fail::die(format!("{}: {e}", p.display())))?;
            let j = J::parse(&text).map_err(crate::Fail::die)?;
            if let J::Obj(pairs) = j {
                for (k, v) in pairs {
                    if !["cast_error", "down", "fixtures", "pngs"].contains(&k.as_str()) {
                        let s: String = v.dump(pyline()).chars().take(1200).collect();
                        println!("  {k} = {s}");
                    }
                }
            }
            Ok(0)
        }
        "agg" => {
            let pat = args.get(1).cloned().unwrap_or_else(|| "s1-plain".into());
            agg(&pat);
            Ok(0)
        }
        "run" => {
            let name = args
                .get(1)
                .ok_or_else(|| crate::Fail::usage("scn run <scenario> [--n N] [--start K]"))?;
            let s = scenarios()
                .into_iter()
                .find(|s| s.name == *name)
                .ok_or_else(|| {
                    crate::Fail::die(format!("no scenario '{name}' ({LAB} scn list)"))
                })?;
            let opt = |flag: &str| {
                args.iter()
                    .position(|a| a == flag)
                    .and_then(|i| args.get(i + 1))
                    .and_then(|v| v.parse::<u32>().ok())
            };
            let (n, start) = (opt("--n").unwrap_or(1), opt("--start").unwrap_or(1));
            stop::install();
            for k in start..start + n {
                let ctx = run_one(&s, k, &tag)?;
                println!("== {} run {k}: {}", s.name, brief(&ctx).dump(pyline()));
            }
            Ok(0)
        }
        other => Err(crate::Fail::usage(format!(
            "scn: unknown command '{other}' (list, run, show, agg)"
        ))),
    }
}

fn agg(pat: &str) {
    let keys = [
        "idle_to_card",
        "card_to_zero",
        "zero_to_gone",
        "card_to_gone",
        "gone_to_journal",
    ];
    let mut rows: Vec<(String, J)> = fs::read_dir(results_dir())
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let num = n.strip_prefix(pat)?.strip_prefix('-')?;
                    (num.len() == 2 && num.chars().all(|c| c.is_ascii_digit())).then_some(())?;
                    let j =
                        J::parse(&fs::read_to_string(e.path().join("summary.json")).ok()?).ok()?;
                    Some((n, j))
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let show = |v: Option<&J>| {
        v.map_or_else(
            || "None".to_string(),
            |v| {
                v.as_f64()
                    .map_or_else(|| v.dump(pyline()), crate::pyjson::py_float)
            },
        )
    };
    println!(
        "{:<14} {}  composer_states",
        "run",
        keys.iter()
            .map(|k| format!("{k:<13}"))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for (n, s) in &rows {
        println!(
            "{n:<14} {}  {}",
            keys.iter()
                .map(|k| format!("{:<13}", show(s.get(k))))
                .collect::<Vec<_>>()
                .join("  "),
            s.get("composer_states")
                .map_or_else(|| "None".into(), |v| v.dump(pyline()))
        );
    }
    for k in keys {
        let mut v: Vec<f64> = rows
            .iter()
            .filter_map(|(_, s)| s.get(k).and_then(J::as_f64))
            .collect();
        if v.is_empty() {
            continue;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len();
        let median = if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        };
        println!(
            "{k:<16} n={n} min={:.3} median={median:.3} max={:.3}",
            v[0],
            v[n - 1]
        );
    }
}

/// Runs `selftest`'s cycle.
/// # Errors
/// Interrupted.
pub fn run_selftest(tag: &str) -> Result<i32, Stopped> {
    let mut ctx = Ctx::new("selftest", 1, tag, "", LabKw::default(), true, true);
    ctx.ends_loops = true;
    let result = (|| -> R<i32> {
        ctx.start()?;
        if let Err(e) = ctx.launch(120.0, Launch::default()) {
            match e {
                ScnErr::Failed(m) => {
                    println!("selftest: {m}");
                    return Ok(2);
                }
                ScnErr::Stopped(s) => return Err(ScnErr::Stopped(s)),
            }
        }
        plain_cycle(&mut ctx, 1.5)?;
        let d = ctx.sumd.clone();
        let sec = |k: &str| {
            d.get(k)
                .and_then(J::as_f64)
                .map_or_else(|| "n/a".into(), |v| format!("{v:.2}s"))
        };
        let trust = ctx
            .journal()
            .iter()
            .any(|(_, r)| rec_s(r, "kind") == "approved");
        println!(
            "trust dialog answered by the harness : {}",
            if trust { "yes" } else { "not seen" }
        );
        println!(
            "survey flag written -> re-read       : {}   (Claude Code's ConfigChange hook; {} write(s))",
            sec("arm_reread_s"),
            d.get("arm_writes")
                .and_then(J::as_i64)
                .map_or_else(|| "?".into(), |n| n.to_string())
        );
        println!(
            "turn idle -> card shown              : {}   (Claude's gate: 2 s idle)",
            sec("idle_to_card")
        );
        println!(
            "card shown -> harness 0 in composer  : {}   (the press: printed, not required)",
            sec("card_to_zero")
        );
        println!(
            "0 in composer -> card gone           : {}   (Claude's digit debounce: 0.4 s)",
            sec("zero_to_gone")
        );
        println!(
            "card gone -> DISMISSED survey        : {}",
            sec("gone_to_journal")
        );
        println!(
            "card on screen in total              : {}",
            sec("card_to_gone")
        );
        println!(
            "left in the composer                 : {}",
            d.get("composer_after").map_or_else(
                || "None".into(),
                |v| v.as_str().map_or_else(|| "None".into(), py_repr)
            )
        );
        let bad = judge_cycle(&d);
        for b in &bad {
            println!("PROBLEM: {b}");
        }
        Ok(i32::from(!bad.is_empty()))
    })();
    ctx.save_timeline();
    ctx.finish();
    ctx.save_summary();
    match result {
        Ok(c) => Ok(c),
        Err(ScnErr::Stopped(s)) => Err(s),
        Err(ScnErr::Failed(e)) => {
            println!("selftest: {e}");
            Ok(1)
        }
    }
}

/// `watch [tag] [secs]`.
pub fn cmd_watch(args: &[String]) -> crate::Res {
    let mut a = args.to_vec();
    let tag = crate::run::take_tag(&mut a)?;
    let secs_total: f64 = a.first().and_then(|v| v.parse().ok()).unwrap_or(30.0);
    stop::install();
    let mut ctx = Ctx::new("watch", 1, &tag, "", LabKw::default(), true, false);
    if let Err(e) = ctx.start() {
        ctx.finish();
        return Err(match e {
            ScnErr::Stopped(s) => s.into(),
            ScnErr::Failed(m) => crate::Fail::die(m),
        });
    }
    println!(
        "{} watching {} for {secs_total:.0}s",
        stamp(now()),
        ctx.run.sock
    );
    let (mut nf, mut ne, mut nj) = (0, 0, 0);
    let mut last: Option<FrameKey> = None;
    let end = now() + secs_total;
    let mut stopped = None;
    while now() < end {
        let (frames, events) = ctx.rec().snapshot();
        let jl = ctx.journal();
        let mut out: Vec<(f64, String)> = Vec::new();
        for f in &frames[nf.min(frames.len())..] {
            let key = frame_key(f);
            if Some(&key) != last.as_ref() {
                out.push((f.wall, frame_line(f)));
                last = Some(key);
            }
        }
        for (w, _, line) in &events[ne.min(events.len())..] {
            out.push((*w, format!("EVENT {}", line.get(6..).unwrap_or(""))));
        }
        for (w, r) in &jl[nj.min(jl.len())..] {
            let t = rec_t(r);
            let kind = r.get("kind").and_then(J::as_str).unwrap_or("?");
            out.push((
                if t > 0.0 { t } else { *w },
                format!("JOURNAL {kind:<9} {}", rec_s(r, "line")),
            ));
        }
        out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (w, text) in out {
            println!("{} {text}", stamp(w));
        }
        (nf, ne, nj) = (frames.len(), events.len(), jl.len());
        if let Err(s) = stop::sleep(std::time::Duration::from_millis(50)) {
            stopped = Some(s);
            break;
        }
    }
    ctx.finish();
    if let Some(s) = stopped {
        return Err(s.into());
    }
    println!("{} done", stamp(now()));
    Ok(0)
}

/// `catch [tag] <egrep> <name> "<what>" [secs]`.
pub fn cmd_catch(args: &[String]) -> crate::Res {
    let mut a = args.to_vec();
    let tag = crate::run::take_tag(&mut a)?;
    let pat = a
        .first()
        .cloned()
        .ok_or_else(|| crate::Fail::die("catch: need an egrep"))?;
    let name = a
        .get(1)
        .cloned()
        .ok_or_else(|| crate::Fail::die("catch: need a name"))?;
    let what = a.get(2).cloned().unwrap_or_default();
    let secs_total: f64 = a.get(3).and_then(|v| v.parse().ok()).unwrap_or(60.0);
    let re = aterm_regex::Regex::new(&pat).map_err(|e| crate::Fail::die(format!("catch: {e}")))?;
    stop::install();
    let mut ctx = Ctx::new("catch", 1, &tag, "", LabKw::default(), true, false);
    let result = (|| -> R<i32> {
        ctx.start()?;
        let Some(fr) = ctx.wait_for(|f| f.rows.iter().any(|r| re.is_match(r)), secs_total, 0)?
        else {
            println!("not caught");
            return Ok(1);
        };
        let (p, png) =
            crate::verbs::write_fixture_rows(&ctx.run, &ctx.door()?, &name, &what, &fr.rows)?;
        println!(
            "caught {} -> {}{}",
            stamp(fr.wall),
            p.display(),
            png.map(|p| format!("  +  {}", p.display()))
                .unwrap_or_default()
        );
        Ok(0)
    })();
    ctx.finish();
    match result {
        Ok(c) => Ok(c),
        Err(ScnErr::Stopped(s)) => Err(s.into()),
        Err(ScnErr::Failed(e)) => Err(crate::Fail::die(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(wall: f64, rows: &str, i: usize) -> Frame {
        Frame {
            wall,
            t: Some(1),
            seq: i64::try_from(i).unwrap(),
            rows: rows.lines().map(str::to_string).collect(),
            i,
        }
    }

    #[test]
    fn the_idle_a_decision_lets_go_on_is_found_journaled_with_it() {
        use std::time::{Duration, Instant};
        let owned = aterm_tempfile::tempdir().expect("owned scratch");
        let path = std::fs::canonicalize(owned.path())
            .expect("a physical scratch")
            .join("journal");
        let line = |kind: &str, line: &str| {
            format!(
                "{}\n",
                J::obj([("t", J::Int(1)), ("kind", J::s(kind)), ("line", J::s(line))]).py()
            )
        };
        std::fs::write(&path, line("event", "EVENT idle seq=1")).expect("a journal");
        let mut ctx = Ctx::new("t", 1, "lab", "", LabKw::default(), true, false);
        ctx.jr = Some(Journal::start(path.clone(), None));
        let held = |ctx: &Ctx, n: usize| {
            let by = Instant::now() + Duration::from_secs(60);
            while ctx.journal().len() < n {
                assert!(Instant::now() < by, "the tail never held {n} line(s)");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        held(&ctx, 1);
        let start = |ctx: &mut Ctx| -> R {
            let both = format!(
                "{}{}{}",
                line("event", "EVENT idle seq=1"),
                line("chose", "CHOSE seq=2 rule=answer-recommended@v1"),
                line("event", "EVENT idle seq=3 ⏺ Done.")
            );
            let mut journal = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("the journal");
            std::io::Write::write_all(&mut journal, both.as_bytes()).expect("appended");
            held(ctx, 4);
            Ok(())
        };
        corpus_decision(&mut ctx, start, "CHOSE", 60.0).expect("not interrupted");
        assert_eq!(ctx.sumd.get("error"), None, "{}", ctx.sumd.py());
        assert_eq!(ctx.sumd.get("decided").and_then(J::as_str), Some("CHOSE"));
    }

    #[test]
    fn the_survey_arm_returns_only_once_claude_code_reported_a_reread() {
        use crate::settings_witness::{HANG, LOG, REWRITE_EVERY, SteppedWait, Wait as _};
        use std::cell::Cell;
        let owned = aterm_tempfile::tempdir().expect("owned scratch");
        let w = std::fs::canonicalize(owned.path()).expect("a physical scratch");
        for (what, answers, same_file) in [
            ("reread", true, true),
            ("no line", false, true),
            ("another file", true, false),
        ] {
            let run = Run {
                w: w.join(what),
                ..Run::default()
            };
            std::fs::create_dir_all(run.w.join("home/.claude")).expect("the settings' dir");
            std::fs::write(run.settings(), "{}\n").expect("the settings");
            let settings = run.settings().to_string_lossy().into_owned();
            let names = if same_file {
                settings.clone()
            } else {
                run.w
                    .join("proj/.claude/settings.json")
                    .to_string_lossy()
                    .into_owned()
            };
            let log = run.w.join(LOG);
            let pauses = Cell::new(0);
            let wait = SteppedWait::new(REWRITE_EVERY / 3, || {
                let settings = J::parse(&crate::util::scratch_text(&run.settings()))
                    .expect("the settings just written");
                assert_eq!(
                    settings
                        .get("env")
                        .and_then(|env| env.get("CLAUDE_FORCE_DISPLAY_SURVEY")),
                    Some(&J::s("1")),
                    "the actual writer set the flag before pausing"
                );
                pauses.set(pauses.get() + 1);
                if answers && pauses.get() == 4 {
                    let line = J::obj([
                        ("hook_event_name", J::s("ConfigChange")),
                        ("file_path", J::s(&names)),
                    ]);
                    std::fs::write(&log, format!("{}\n", line.py())).expect("the witness's log");
                }
            });
            let mut ctx = Ctx::new("t", 1, "lab", "", LabKw::default(), true, false);
            ctx.run = run.clone();
            let armed = ctx.arm_with_wait(&wait);
            if answers && same_file {
                armed.expect("armed");
                let logged = crate::util::scratch_text(&log);
                assert_eq!(
                    crate::settings_witness::rereads(&logged, &settings),
                    1,
                    "the line was in the log when the arm returned: {logged:?}"
                );
                let writes = ctx.sumd.get("arm_writes").and_then(J::as_i64).unwrap_or(0);
                assert_eq!(
                    writes,
                    2,
                    "the missed write was made again: {}",
                    ctx.sumd.py()
                );
                assert!(wait.elapsed() < HANG);
                continue;
            }
            let made = HANG.as_millis() / REWRITE_EVERY.as_millis();
            match armed {
                Err(ScnErr::Failed(m)) => assert!(
                    m.starts_with("the survey flag was not armed: ")
                        && m.contains("no re-read")
                        && m.contains(&format!("({made} writes)")),
                    "{what}: one write every REWRITE_EVERY until HANG: {m}"
                ),
                other => panic!("{what}: {other:?}"),
            }
            assert_eq!(ctx.sumd.get("arm_writes"), None, "{what}");
            assert_eq!(
                crate::settings_witness::rereads(&crate::util::scratch_text(&log), &names),
                usize::from(answers),
                "{what}: the controlled watcher supplied exactly its intended evidence"
            );
            assert_eq!(
                wait.elapsed(),
                HANG,
                "{what}: the simulated hang detector ended the wait"
            );
        }
    }

    #[test]
    fn the_journal_goes_before_whether_it_holds_a_dismissal() {
        let mut ctx = Ctx::new("t", 1, "lab", "", LabKw::default(), true, false);
        put_journal(
            &mut ctx,
            J::Arr(vec![J::Arr(vec![
                J::Float(0.4),
                J::s("dismissed"),
                J::s("DISMISSED survey seq=1"),
            ])]),
        );
        let J::Obj(m) = &ctx.sumd else {
            panic!("summary is an object")
        };
        let keys: Vec<&str> = m.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["journal", "dismissed_journal"]);
        assert_eq!(ctx.sumd.get("dismissed_journal"), Some(&J::Bool(true)));
    }

    #[test]
    fn the_cycle_is_measured_from_frames_and_journal_and_judged() {
        let rule = "─".repeat(50);
        let card =
            format!("● How is Claude doing this session? (optional)\n{rule}\n❯\u{a0}\n{rule}");
        let zero =
            format!("● How is Claude doing this session? (optional)\n{rule}\n❯\u{a0}0\n{rule}");
        let after = format!("✻ Worked for 0s · done 12:16 PM\n{rule}\n❯\u{a0}\n{rule}");
        let frames = vec![
            frame(10.5, "✻ Worked for 0s · done 12:16 PM", 0),
            frame(12.5, &card, 1),
            frame(14.3, &zero, 2),
            frame(14.7, &after, 3),
        ];
        let journal = vec![(
            15.3,
            J::parse(r#"{"t":15200,"kind":"dismissed","line":"DISMISSED survey seq=1"}"#).unwrap(),
        )];
        let d = cycle_stats(&frames, &journal, 10.0);
        assert_eq!(d.get("idle_to_card"), Some(&J::Float(2.0)));
        assert_eq!(d.get("card_to_zero"), Some(&J::Float(1.8)));
        assert_eq!(d.get("zero_to_gone"), Some(&J::Float(0.4)));
        assert_eq!(d.get("gone_to_journal"), Some(&J::Float(0.5)));
        assert_eq!(
            d.get("composer_states").unwrap().dump(crate::pyjson::PY),
            r#"["", "0", ""]"#
        );
        assert!(judge_cycle(&d).is_empty());
        let no_card = cycle_stats(&frames[..1], &[], 10.0);
        assert_eq!(
            judge_cycle(&no_card),
            vec!["no survey card was drawn".to_string()]
        );
    }

    #[test]
    fn a_frame_line_says_what_changed_in_python_repr() {
        let f = frame(1.0, "Feedback: Bad · Esc to undo", 7);
        assert_eq!(
            frame_line(&f),
            "FRAME seq=7 T=1 card=0 composer=None status='' rows=['Feedback: Bad · Esc to undo']"
        );
    }

    /// A RUN'S OWN LAB IS MADE WITH THE RUN'S KEYS AND WORDS: its `[harness]` lines in place of the
    /// caller's, and its instance's words after the caller's `LAB_INSTANCE_ENV`, so the
    /// occupancy scenario's lab reads `prompt` and the nudge's seam at its minute. NEGATIVE
    /// CONTROL: a run with no words of its own leaves the caller's instance words as they were.
    #[test]
    fn a_runs_own_lab_gets_its_keys_and_its_instance_words() {
        use aterm_agent::supervise::occupancy::rules::NUDGE_SEAM;
        let caller = || UpOpts {
            harness_keys: "continue = false".into(),
            instance_env: "TMPDIR=/s".into(),
            ..UpOpts::default()
        };
        let listed = scenarios()
            .into_iter()
            .find(|s| s.name == crate::occupancy_break::NAME)
            .expect("the occupancy scenario is listed");
        let o = lab_opts(caller(), listed.harness_keys, listed.kw);
        assert!(o.trust && o.lifeline);
        assert_eq!(o.harness_keys, crate::occupancy_break::HARNESS_KEYS);
        assert_eq!(
            crate::lab::instance_env(&o.instance_env),
            Ok(vec![
                ("TMPDIR".to_string(), "/s".to_string()),
                (
                    NUDGE_SEAM.to_string(),
                    crate::occupancy_break::NUDGE_S.to_string()
                ),
            ])
        );
        let alone = lab_opts(UpOpts::default(), listed.harness_keys, listed.kw);
        assert_eq!(alone.instance_env, crate::occupancy_break::INSTANCE_ENV);
        let plain = lab_opts(caller(), "", LabKw::default());
        assert_eq!(plain.instance_env, "TMPDIR=/s");
        assert_eq!(plain.harness_keys, "");
    }

    /// A RUN THAT ASKS ITS LAB FOR KEYS OR WORDS REFUSES A LAB IT ATTACHES TO, which was made
    /// without them. NEGATIVE CONTROL: a run that asks for none (`watch`, `catch`, the nudge
    /// trials) attaches.
    #[test]
    fn a_run_that_asks_its_lab_for_keys_refuses_an_attached_one() {
        let listed = scenarios()
            .into_iter()
            .find(|s| s.name == crate::occupancy_break::NAME)
            .expect("the occupancy scenario is listed");
        assert!(unset_on_attach(listed.harness_keys, listed.kw).is_some());
        let words = LabKw {
            instance_env: crate::occupancy_break::INSTANCE_ENV,
            ..LabKw::default()
        };
        assert!(unset_on_attach("", words).is_some());
        assert!(unset_on_attach("continue = false", LabKw::default()).is_some());
        assert_eq!(unset_on_attach("", LabKw::default()), None);
    }
}
