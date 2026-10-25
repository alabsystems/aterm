// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REMAINING §4.1 MARKS (plan §3.2).

use aterm_slog::{
    Amend, BoundedText, BoundedToken, Conf, Fields, Key, Kind, Mark, Node, Src, Stamp, Val,
};

use super::{SessionLog, name_fp, tok, with};

/// A bell is said at most this often (monotonic ms): a flood of BELs is one count a second.
pub(crate) const BELL_EVERY_MS: u64 = 1_000;

/// A program's moves written at once before its run's bucket is empty (6F-4, U17).
pub(crate) const PROGRAM_BURST: u64 = 20;
/// Past the burst, a program's run earns one program mark this often (monotonic ms).
pub(crate) const PROGRAM_REFILL_MS: u64 = 1_000;
const PROGRAM_FULL_MILLI: u64 = PROGRAM_BURST * 1_000;

/// A THROTTLED MARK CLASS'S BUCKET.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkBucket {
    milli: u64,
    at: u64,
}

impl Default for MarkBucket {
    fn default() -> Self {
        MarkBucket::full(0)
    }
}

impl MarkBucket {
    const fn full(at: u64) -> Self {
        MarkBucket {
            milli: PROGRAM_FULL_MILLI,
            at,
        }
    }

    fn spend(&mut self, now: u64, force: bool) -> bool {
        let now = now.max(self.at);
        let earned = (now - self.at).saturating_mul(1_000) / PROGRAM_REFILL_MS;
        self.milli = self.milli.saturating_add(earned).min(PROGRAM_FULL_MILLI);
        self.at = now;
        if self.milli < 1_000 && !force {
            return false;
        }
        self.milli = self.milli.saturating_sub(1_000);
        true
    }
}

/// What the program in front says about itself, as its mark words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProgramWords {
    pub(crate) state: &'static str,
    pub(crate) ask: Option<&'static str>,
}

impl ProgramWords {
    /// The words of the shown report `shown` (`None`: none shown).
    pub(crate) fn of(shown: Option<aterm_core::terminal::program_state::ProgramState>) -> Self {
        shown.map_or(
            ProgramWords {
                state: "none",
                ask: None,
            },
            |s| ProgramWords {
                state: s.state.as_str(),
                ask: s.kind.map(|k| k.as_str()),
            },
        )
    }
}

/// THE PROGRAM MARKS' BUCKET: the run in front's tokens, and the newest move not yet written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramMarks {
    /// The run the bucket is the share of (`None`: no move yet).
    run: Option<Node>,
    /// Its tokens.
    bucket: MarkBucket,
    /// The newest move not yet written, and how many states it replaced.
    pending: Option<(ProgramWords, u64)>,
}

/// WHAT THE PERSON IS SHOWN of a session's need: the printed tuple, as its `shown` mark words it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownTuple {
    pub(crate) need: Option<&'static str>,
    pub(crate) sev: Option<&'static str>,
    pub(crate) held: Option<&'static str>,
}

impl ShownTuple {
    /// The tuple verdict `v` prints; its `waiting` and `recheck_unix` are not shown in it.
    pub(crate) fn of(v: &crate::needs::Verdict) -> Self {
        ShownTuple {
            need: v.need.as_ref().map(|n| n.word.word()),
            sev: v.need.as_ref().map(|n| n.word.severity().word()),
            held: v.held.map(aterm_types::needs::HoldWord::word),
        }
    }

    /// The carry's token (`<need>.<sev>.<held>`, `-` for none).
    #[cfg(any(unix, test))]
    fn token(self) -> String {
        format!(
            "{}.{}.{}",
            self.need.unwrap_or("-"),
            self.sev.unwrap_or("-"),
            self.held.unwrap_or("-")
        )
    }

    fn read(token: &str) -> Option<Self> {
        let mut it = token.split('.');
        let word = |raw: Option<&str>, words: &[&'static str]| match raw? {
            "-" => Some(None),
            w => carried_word(words, w).map(Some),
        };
        let need = word(it.next(), aterm_slog::vocab::NEED_WORDS)?;
        let sev = word(it.next(), aterm_slog::vocab::SEV_WORDS)?;
        let held = word(it.next(), aterm_slog::vocab::HELD_WORDS)?;
        it.next()
            .is_none()
            .then_some(ShownTuple { need, sev, held })
    }
}

/// THE SHOWN MARKS' BUCKET.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownMarks {
    last: ShownTuple,
    bucket: MarkBucket,
    pending: Option<(ShownTuple, u64)>,
}

/// What the engine says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EngineRead {
    /// The shell integration's posture.
    pub(crate) integration: aterm_core::terminal::ShellIntegrationPosture,
    /// Notifications asked for ([`aterm_core::terminal::Terminal::notify_total`]).
    pub(crate) notify_total: u64,
    /// Bells that fired ([`aterm_core::terminal::Terminal::bell_total`]).
    pub(crate) bell_total: u64,
    /// The OSC 9;4 progress.
    pub(crate) progress: Option<aterm_types::osc::TaskbarProgress>,
}

impl EngineRead {
    /// Read off `t`: four field loads.
    pub(crate) const fn of(t: &aterm_core::terminal::Terminal) -> EngineRead {
        EngineRead {
            integration: t.shell_integration_posture(),
            notify_total: t.notify_total(),
            bell_total: t.bell_total(),
            progress: t.taskbar_progress(),
        }
    }
}

/// One published status transition, typed ([`crate::session_status::Status`]'s words).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusFacts {
    pub(crate) phase: &'static str,
    /// `ok`, `fail` or `signal`, and the exit code or signal number.
    pub(crate) out: Option<(&'static str, Option<i64>)>,
    pub(crate) conf: Conf,
}

/// What put a session's modes back ([`SessionLog::observe_modes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModesFrom<'a> {
    /// A manual reset, by its source word (`manual_reset::ResetSource`'s `ctl|menu`).
    Reset(&'static str),
    /// A foreground handback.
    Handback {
        from: i32,
        to: i32,
        program: Option<&'a str>,
    },
}

/// What each kind of mark last wrote ([`SessionLog`]'s `observed`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Observed {
    integration: Option<&'static str>,
    /// The context left, in tenths.
    ctx: Option<u8>,
    goal: Option<bool>,
    /// The notify and bell totals last counted from (this process's engine: neither crosses an
    /// update, so the first read is a baseline).
    notify: Option<u64>,
    bell: Option<u64>,
    /// When a bell was last said (monotonic ms).
    bell_at: Option<u64>,
    /// The progress state and its tenth.
    progress: Option<(&'static str, Option<u8>)>,
    /// The cwd and the informative title last said.
    cwd: Option<u64>,
    title: Option<u64>,
    /// Whether the effective attention owner was the supervisor's when a meta write last said.
    escalated: Option<bool>,
    /// Each meta field's value as the log last said it.
    meta: Vec<(String, u64)>,
    /// The program marks' bucket.
    program: ProgramMarks,
    /// The shown marks' tuple and bucket (6F-9).
    shown: ShownMarks,
}

const META_FIELDS_MAX: usize = crate::session_timeline::MetaField::ALL.len() + 1;

const FINGERPRINTED_META: &[&str] = &["attention"];

/// A title's fingerprint.
pub(crate) fn title_fp(title: &str) -> u64 {
    if title.is_empty() {
        return 0;
    }
    match crate::toolbar::busy_spinner_title_parts(title) {
        Some((_, rest)) => name_fp(Some(&format!("\u{1}{rest}"))),
        None => name_fp(Some(title)),
    }
}

const fn integration_word(p: aterm_core::terminal::ShellIntegrationPosture) -> &'static str {
    use aterm_core::terminal::ShellIntegrationPosture as P;
    match p {
        P::Off => "off",
        P::On => "on",
        P::Degraded => "degraded",
    }
}

const fn progress_of(p: Option<aterm_types::osc::TaskbarProgress>) -> (&'static str, Option<u8>) {
    use aterm_types::osc::TaskbarProgress as P;
    match p {
        None | Some(P::Hidden) => ("none", None),
        Some(P::Normal(n)) => ("normal", Some(n)),
        Some(P::Error(n)) => ("error", Some(n)),
        Some(P::Indeterminate) => ("indeterminate", None),
        Some(P::Paused(n)) => ("paused", Some(n)),
        Some(_) => ("unknown", None),
    }
}

fn carried_word(words: &[&'static str], raw: &str) -> Option<&'static str> {
    words.iter().copied().find(|w| *w == raw)
}

const INTEGRATION_WORDS: &[&str] = &["off", "on", "degraded"];
const PROGRESS_WORDS: &[&str] = &[
    "none",
    "normal",
    "error",
    "indeterminate",
    "paused",
    "unknown",
];

impl Observed {
    /// The carry's tokens for the `state` host line.
    #[cfg(any(unix, test))]
    pub(crate) fn carry_tokens(&self) -> String {
        let mut s = String::new();
        if let Some(w) = self.integration {
            s.push_str(&format!(" obs_int={w}"));
        }
        if let Some(d) = self.ctx {
            s.push_str(&format!(" obs_ctx={d}"));
        }
        if let Some(g) = self.goal {
            s.push_str(&format!(" obs_goal={}", u8::from(g)));
        }
        if let Some((w, d)) = self.progress {
            match d {
                Some(d) => s.push_str(&format!(" obs_prog={w}.{d}")),
                None => s.push_str(&format!(" obs_prog={w}")),
            }
        }
        if let Some(fp) = self.cwd {
            s.push_str(&format!(" cwd_fp={fp:x}"));
        }
        if let Some(fp) = self.title {
            s.push_str(&format!(" title_fp={fp:x}"));
        }
        if let Some(e) = self.escalated {
            s.push_str(&format!(" obs_esc={}", u8::from(e)));
        }
        if !self.meta.is_empty() {
            let fields: Vec<String> = self
                .meta
                .iter()
                .map(|(f, fp)| format!("{f}.{fp:x}"))
                .collect();
            s.push_str(&format!(" meta_fp={}", fields.join(",")));
        }
        if self.shown.last != ShownTuple::default() {
            s.push_str(&format!(" obs_shown={}", self.shown.last.token()));
        }
        s
    }

    /// Read back the tokens of [`Self::carry_tokens`] off a `state` line.
    pub(crate) fn read_carry(&mut self, kv: impl Fn(&str) -> Option<String>) {
        self.integration = kv("obs_int").and_then(|w| carried_word(INTEGRATION_WORDS, &w));
        self.ctx = kv("obs_ctx")
            .and_then(|d| d.parse().ok())
            .filter(|d| *d <= 10);
        let flag = |k: &str| {
            kv(k).and_then(|g| match g.as_str() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            })
        };
        self.goal = flag("obs_goal");
        self.escalated = flag("obs_esc");
        self.progress = kv("obs_prog").and_then(|v| {
            let (w, d) = match v.split_once('.') {
                Some((w, d)) => (w.to_string(), Some(d.parse::<u8>().ok()?)),
                None => (v, None),
            };
            Some((carried_word(PROGRESS_WORDS, &w)?, d))
        });
        let fp = |k: &str| kv(k).and_then(|h| u64::from_str_radix(&h, 16).ok());
        self.cwd = fp("cwd_fp");
        self.title = fp("title_fp");
        self.meta = kv("meta_fp").map_or_else(Vec::new, |v| {
            v.split(',')
                .filter_map(|pair| {
                    let (f, h) = pair.split_once('.')?;
                    BoundedToken::new(Key::Field, f)?;
                    Some((f.to_string(), u64::from_str_radix(h, 16).ok()?))
                })
                .take(META_FIELDS_MAX)
                .collect()
        });
        self.shown.last = kv("obs_shown")
            .and_then(|t| ShownTuple::read(&t))
            .unwrap_or_default();
    }

    fn meta_moved(&mut self, field: &str, fp: u64) -> bool {
        match self.meta.iter_mut().find(|(f, _)| f == field) {
            Some((_, held)) if *held == fp => false,
            Some((_, held)) => {
                *held = fp;
                true
            }
            None => {
                if self.meta.len() < META_FIELDS_MAX {
                    self.meta.push((field.to_string(), fp));
                }
                true
            }
        }
    }
}

impl SessionLog {
    fn front_run(&self) -> Node {
        let open = |n: Node| self.log.is_open(n).then_some(n);
        self.agent
            .as_ref()
            .and_then(|a| open(a.node))
            .or_else(|| self.fg.as_ref().and_then(|f| open(f.node)))
            .or_else(|| self.cmd.as_ref().and_then(|c| open(c.node)))
            .unwrap_or(Node::SESSION)
    }

    fn claude_run(&self) -> Option<Node> {
        self.agent
            .as_ref()
            .filter(|a| {
                self.log.is_open(a.node)
                    && match a.who {
                        aterm_phase::Program::Claude => true,
                        aterm_phase::Program::Codex
                        | aterm_phase::Program::Grok
                        | aterm_phase::Program::Kimi
                        | aterm_phase::Program::Agy
                        | aterm_phase::Program::Muse
                        | aterm_phase::Program::Generic => false,
                    }
            })
            .map(|a| a.node)
    }

    fn put(&mut self, kind: Kind, src: Src, conf: Conf, node: Node, f: Fields, at: Stamp) {
        let _ = self
            .log
            .mark(Mark::new(kind, src, conf).at(node).fields(f), at);
    }

    /// THE ENGINE'S MARKS, from one read under the sweep's guard.
    pub(crate) fn observe_engine(&mut self, e: &EngineRead, at: Stamp) {
        let integration = integration_word(e.integration);
        if self.observed.integration != Some(integration) {
            self.observed.integration = Some(integration);
            let f = with(Fields::new(), Key::State, Some(Val::Word(integration)));
            self.put(
                Kind::Integration,
                Src::Engine,
                Conf::Exact,
                Node::SESSION,
                f,
                at,
            );
        }
        match self.observed.notify {
            Some(base) if e.notify_total > base => {
                let f = with(Fields::new(), Key::N, Some(Val::U(e.notify_total - base)));
                self.put(Kind::Notify, Src::Engine, Conf::Exact, Node::SESSION, f, at);
                self.observed.notify = Some(e.notify_total);
            }
            Some(base) if e.notify_total >= base => {}
            _ => self.observed.notify = Some(e.notify_total),
        }
        match self.observed.bell {
            Some(base) if e.bell_total > base => {
                let due = self
                    .observed
                    .bell_at
                    .is_none_or(|t| at.mono_ms.saturating_sub(t) >= BELL_EVERY_MS);
                if due {
                    let f = with(Fields::new(), Key::N, Some(Val::U(e.bell_total - base)));
                    self.put(Kind::Bell, Src::Engine, Conf::Exact, Node::SESSION, f, at);
                    self.observed.bell = Some(e.bell_total);
                    self.observed.bell_at = Some(at.mono_ms);
                }
            }
            Some(base) if e.bell_total >= base => {}
            _ => self.observed.bell = Some(e.bell_total),
        }
        let (word, pct) = progress_of(e.progress);
        let now = (word, pct.map(|p| p / 10));
        match self.observed.progress {
            Some(held) if held == now => {}
            None if word == "none" => self.observed.progress = Some(now),
            _ => {
                self.observed.progress = Some(now);
                let f = with(Fields::new(), Key::State, Some(Val::Word(word)));
                let f = with(f, Key::Pct, pct.map(|p| Val::U(u64::from(p))));
                let run = self.front_run();
                self.put(Kind::Progress, Src::Engine, Conf::Exact, run, f, at);
            }
        }
    }

    /// `mark <run> program state= [ask=] [coalesced=]`.
    pub(crate) fn observe_program(
        &mut self,
        shown: Option<aterm_core::terminal::program_state::ProgramState>,
        at: Stamp,
    ) {
        let words = ProgramWords::of(shown);
        let run = self.front_run();
        if self.observed.program.run != Some(run) {
            self.write_program(at, true);
            self.observed.program = ProgramMarks {
                run: Some(run),
                bucket: MarkBucket::full(at.mono_ms),
                pending: None,
            };
        }
        let p = &mut self.observed.program;
        p.pending = Some(match p.pending {
            Some((_, replaced)) => (words, replaced.saturating_add(1)),
            None => (words, 0),
        });
        self.write_program(at, false);
    }

    /// THE PROGRAM BUCKET'S TICK.
    pub(crate) fn program_tick(&mut self, at: Stamp) {
        self.write_program(at, false);
    }

    /// Write the pending program state now.
    pub(crate) fn flush_program(&mut self, at: Stamp) {
        self.write_program(at, true);
    }

    /// The run `closing` is about to close.
    pub(super) fn settle_program(&mut self, closing: Node, at: Stamp) {
        let Some(run) = self.observed.program.run else {
            return;
        };
        if run == closing || self.descends(run, closing) {
            self.write_program(at, true);
        }
    }

    fn write_program(&mut self, at: Stamp, force: bool) {
        let open = self.observed.program.run.filter(|n| self.log.is_open(*n));
        let p = &mut self.observed.program;
        let Some((words, replaced)) = p.pending else {
            return;
        };
        if !p.bucket.spend(at.mono_ms, force) {
            return;
        }
        p.pending = None;
        let node = open.unwrap_or(Node::SESSION);
        let f = with(Fields::new(), Key::State, Some(Val::Word(words.state)));
        let f = with(f, Key::Ask, words.ask.map(Val::Word));
        let f = with(
            f,
            Key::Coalesced,
            (replaced > 0).then_some(Val::U(replaced)),
        );
        self.put(Kind::Program, Src::Engine, Conf::Claim, node, f, at);
    }

    /// `mark - shown [need= sev=] [held=] [coalesced=]`.
    pub(crate) fn observe_shown(&mut self, tuple: ShownTuple, at: Stamp) {
        let s = &mut self.observed.shown;
        if s.last == tuple {
            return;
        }
        s.last = tuple;
        s.pending = Some(match s.pending {
            Some((_, replaced)) => (tuple, replaced.saturating_add(1)),
            None => (tuple, 0),
        });
        self.write_shown(at, false);
    }

    /// THE SHOWN BUCKET'S TICK.
    pub(crate) fn shown_tick(&mut self, at: Stamp) {
        self.write_shown(at, false);
    }

    /// Write the pending shown tuple now.
    pub(crate) fn flush_shown(&mut self, at: Stamp) {
        self.write_shown(at, true);
    }

    fn write_shown(&mut self, at: Stamp, force: bool) {
        let s = &mut self.observed.shown;
        let Some((tuple, replaced)) = s.pending else {
            return;
        };
        if !s.bucket.spend(at.mono_ms, force) {
            return;
        }
        s.pending = None;
        let f = with(Fields::new(), Key::Need, tuple.need.map(Val::Word));
        let f = with(f, Key::Sev, tuple.sev.map(Val::Word));
        let f = with(f, Key::Held, tuple.held.map(Val::Word));
        let f = with(
            f,
            Key::Coalesced,
            (replaced > 0).then_some(Val::U(replaced)),
        );
        let _ = self.log.mark(
            Mark::new(Kind::Shown, Src::Aterm, Conf::Strong)
                .at(Node::SESSION)
                .by(aterm_slog::Actor::Aterm)
                .fields(f),
            at,
        );
    }

    /// `mark <run> status …`.
    pub(crate) fn observe_status(&mut self, s: StatusFacts, at: Stamp) {
        let mut f = with(Fields::new(), Key::Phase, Some(Val::Word(s.phase)));
        if let Some((out, code)) = s.out {
            f = with(f, Key::Out, Some(Val::Word(out)));
            f = with(f, Key::Code, code.map(Val::I));
        }
        let run = self.front_run();
        self.put(Kind::Status, Src::Status, s.conf, run, f, at);
    }

    /// The screen saw a person stop the turn (Esc).
    pub(crate) const fn agent_interrupted(&mut self) {
        if let Some(a) = self.agent.as_mut() {
            a.tracker.note_interrupted();
        }
    }

    /// Claude Code's context left (`pct`, when its screen shows it) and its goal.
    pub(crate) fn observe_screen_facts(
        &mut self,
        ctx_pct: Option<u8>,
        goal: Option<bool>,
        at: Stamp,
    ) {
        let Some(run) = self.claude_run() else {
            return;
        };
        if let Some(pct) = ctx_pct {
            let tenth = pct.min(100) / 10;
            if self.observed.ctx != Some(tenth) {
                self.observed.ctx = Some(tenth);
                let f = with(Fields::new(), Key::Pct, Some(Val::U(u64::from(pct))));
                self.put(Kind::Ctx, Src::Screen, Conf::Strong, run, f, at);
            }
        }
        if let Some(goal) = goal
            && self.observed.goal != Some(goal)
        {
            let first = self.observed.goal.is_none();
            self.observed.goal = Some(goal);
            if goal || !first {
                let word = if goal { "on" } else { "off" };
                let f = with(Fields::new(), Key::State, Some(Val::Word(word)));
                self.put(Kind::Goal, Src::Screen, Conf::Strong, run, f, at);
            }
        }
    }

    /// THE LAUNCH CARD's names.
    pub(crate) fn observe_card(&mut self, card: &aterm_phase::LaunchCard, at: Stamp) {
        let Some(run) = self.claude_run() else {
            return;
        };
        let held = self
            .log
            .state()
            .run()
            .filter(|o| o.node == run)
            .map(|o| o.f.clone())
            .unwrap_or_default();
        let mut f = Fields::new();
        for (k, v) in [
            (Key::Ver, Some(card.version.as_str())),
            (Key::Model, Some(card.model.as_str())),
            (Key::Effort, card.effort.as_deref()),
        ] {
            if let Some(t) = v.and_then(|v| BoundedToken::new(k, v))
                && held.str(k) != Some(t.as_str())
            {
                let _ = f.set(k, Val::Tok(t));
            }
        }
        if f.iter().next().is_some() {
            let _ = self.log.amend(
                run,
                Amend::new(Kind::Agent, Src::Screen, Conf::Strong, f),
                at,
            );
        }
    }

    /// `mark - render state=desync-risk|healed …`.
    pub(crate) fn observe_render(&mut self, t: &crate::resize_ledger::Transition, at: Stamp) {
        use crate::resize_ledger::Transition as T;
        let u = |f: Fields, k: Key, n: u64| with(f, k, Some(Val::U(n)));
        let i = |f: Fields, k: Key, n: i32| with(f, k, Some(Val::I(i64::from(n))));
        let f = with(Fields::new(), Key::State, Some(Val::Word(t.state_word())));
        let f = match *t {
            T::Risk {
                run,
                displaced,
                net_zero,
                at_ms,
            } => {
                let f = i(u(f, Key::Run, run), Key::Displaced, displaced);
                let net = if net_zero { "zero" } else { "changed" };
                u(with(f, Key::Net, Some(Val::Word(net))), Key::At, at_ms)
            }
            T::Healed { run, after_ms } => u(u(f, Key::Run, run), Key::AfterMs, after_ms),
            T::EpochRisk {
                epoch,
                shift,
                pushed,
                cut,
                drew,
                class,
                at_ms,
            } => {
                let f = i(u(f, Key::ResizeEpoch, epoch), Key::Displaced, shift);
                let f = u(i(f, Key::Pushed, pushed), Key::Cut, u64::from(cut));
                let f = u(f, Key::Drew, u64::from(drew));
                let f = f.clone().word(Key::Class, class.wire()).unwrap_or(f);
                u(f, Key::At, at_ms)
            }
            T::EpochHealed {
                epoch,
                after_ms,
                by,
            } => {
                let f = u(u(f, Key::ResizeEpoch, epoch), Key::AfterMs, after_ms);
                f.clone().word(Key::HealedBy, by.wire()).unwrap_or(f)
            }
        };
        self.put(Kind::Render, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - cwd cwd=<path>`: the OSC 7 directory the drift flush read (`SessionTimeline::record_cwd_change`), when it moved; returns whether it marked.
    pub(crate) fn observe_cwd(&mut self, cwd: Option<&str>, at: Stamp) -> bool {
        let cwd = cwd.filter(|c| !c.is_empty());
        let fp = name_fp(cwd);
        if self.observed.cwd == Some(fp) {
            return false;
        }
        let first = self.observed.cwd.is_none();
        self.observed.cwd = Some(fp);
        if first && cwd.is_none() {
            return false;
        }
        let mut f = Fields::new();
        if let Some(t) = cwd.and_then(|c| BoundedText::new(Key::Cwd, c)) {
            let _ = f.set(Key::Cwd, Val::Text(t));
        }
        self.put(Kind::Cwd, Src::Engine, Conf::Exact, Node::SESSION, f, at);
        true
    }

    /// `mark - title title=<informative title>`, when it moved; returns whether it marked.
    pub(crate) fn observe_title(&mut self, title: &str, at: Stamp) -> bool {
        let fp = title_fp(title);
        if self.observed.title == Some(fp) {
            return false;
        }
        let first = self.observed.title.is_none();
        self.observed.title = Some(fp);
        if first && fp == 0 {
            return false;
        }
        let mut f = Fields::new();
        if let Some(t) = BoundedText::new(Key::Title, title) {
            let _ = f.set(Key::Title, Val::Text(t));
        }
        self.put(Kind::Title, Src::Engine, Conf::Exact, Node::SESSION, f, at);
        true
    }

    /// `mark - modes n=<k> …`.
    pub(crate) fn observe_modes(
        &mut self,
        from: ModesFrom<'_>,
        reverted: &[&str],
        bytes: usize,
        by: aterm_slog::Actor,
        at: Stamp,
    ) {
        let count = |n: usize| Some(Val::U(u64::try_from(n).unwrap_or(u64::MAX)));
        let f = with(Fields::new(), Key::N, count(reverted.len()));
        let f = match from {
            ModesFrom::Reset(source) => f.clone().word(Key::Source, source).unwrap_or(f),
            ModesFrom::Handback { from, to, program } => {
                let f = with(f, Key::FromPg, Some(Val::I(i64::from(from))));
                let f = with(f, Key::ToPg, Some(Val::I(i64::from(to))));
                with(f, Key::Prog, program.and_then(|p| tok(Key::Prog, p)))
            }
        };
        let modes = reverted.join(",");
        let f = with(f, Key::Reverted, tok(Key::Reverted, &modes));
        let f = with(f, Key::Bytes, count(bytes));
        let _ = self.log.mark(
            Mark::new(Kind::Modes, Src::Engine, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }

    /// THE FABRIC'S MARKS.
    pub(crate) fn observe_fabric(&mut self, fact: crate::fabric::FabricFact<'_>, at: Stamp) {
        use crate::fabric::FabricFact as F;
        self.reconcile_person(at);
        let word = |f: Fields, k: Key, w: &str| {
            let kept = f.clone();
            f.word(k, w)
                .or_else(|_| kept.clone().word(k, "unknown"))
                .unwrap_or(kept)
        };
        let text = |mut f: Fields, k: Key, t: &str| {
            if let Some(t) = BoundedText::new(k, t) {
                let _ = f.set(k, Val::Text(t));
            }
            f
        };
        let some_text = |f: Fields, k: Key, t: Option<&str>| match t {
            Some(t) => text(f, k, t),
            None => f,
        };
        let (kind, by, f) = match fact {
            F::MailIn {
                kind,
                from,
                off,
                inbox,
            } => {
                let f = word(Fields::new(), Key::Dir, "in");
                let f = word(f, Key::MailKind, kind);
                let f = text(f, Key::From, from);
                let f = with(f, Key::Off, Some(Val::U(off)));
                let f = with(f, Key::Inbox, Some(Val::U(inbox)));
                (Kind::Mail, aterm_slog::Actor::Bridge, f)
            }
            F::MailOut {
                kind,
                to,
                post,
                re,
                dl,
                via,
                idem,
            } => {
                let f = word(Fields::new(), Key::Dir, "out");
                let f = word(f, Key::MailKind, kind);
                let f = match to.strip_prefix("say:") {
                    Some(topic) => {
                        let fp = format!("say:{:016x}", name_fp(Some(topic)));
                        text(text(f, Key::To, &fp), Key::TopicName, topic)
                    }
                    None => text(f, Key::To, to),
                };
                let f = with(f, Key::Inbox, Some(Val::U(post)));
                let f = with(
                    f,
                    Key::Re,
                    re.and_then(|r| aterm_slog::ShortId::new(&r.to_string()))
                        .map(Val::Id),
                );
                let f = with(f, Key::Dl, dl.map(Val::U));
                let f = some_text(f, Key::Via, via);
                let f = some_text(f, Key::Idem, idem);
                (Kind::Mail, aterm_slog::Actor::Nobody, f)
            }
            F::MailSeen {
                inbox,
                off,
                floor,
                seen,
                verdict,
                kind,
                from,
                moved,
            } => {
                let f = with(Fields::new(), Key::Inbox, Some(Val::U(inbox)));
                let f = with(f, Key::Off, Some(Val::U(off)));
                let f = with(f, Key::Floor, floor.map(Val::U));
                let f = with(f, Key::Seen, Some(Val::U(seen)));
                let f = with(f, Key::Moved, Some(Val::U(u64::from(moved))));
                let f = match verdict {
                    Some(v) => text(
                        word(word(f, Key::Word, v), Key::MailKind, kind),
                        Key::From,
                        from,
                    ),
                    None => f,
                };
                (Kind::MailSeen, aterm_slog::Actor::Nobody, f)
            }
            F::MailLanded { post, off, why } => {
                let f = with(Fields::new(), Key::Inbox, Some(Val::U(post)));
                let f = with(f, Key::Off, off.map(Val::U));
                let (f, by) = match why {
                    Some(w) => (word(f, Key::Why, w), aterm_slog::Actor::Aterm),
                    None => (f, aterm_slog::Actor::Bridge),
                };
                (Kind::MailLanded, by, f)
            }
            F::Hold {
                held,
                bridge,
                origin,
                hold_origin,
                reason,
                carried,
            } => {
                let f = word(Fields::new(), Key::State, if held { "on" } else { "off" });
                let mut f = word(f, Key::Origin, origin);
                if let Some(t) = (hold_origin != "-")
                    .then(|| BoundedToken::new(Key::HoldOrigin, hold_origin))
                    .flatten()
                {
                    let _ = f.set(Key::HoldOrigin, Val::Tok(t));
                }
                let f = some_text(f, Key::Reason, reason);
                let f = with(f, Key::Carried, carried.then_some(Val::U(1)));
                let by = if bridge {
                    aterm_slog::Actor::Bridge
                } else {
                    aterm_slog::Actor::Nobody
                };
                (Kind::Hold, by, f)
            }
            F::Topic { add, name, since } => {
                let f = word(Fields::new(), Key::Change, if add { "add" } else { "drop" });
                let f = with(
                    f,
                    Key::Fp,
                    aterm_slog::ShortId::new(&format!("{:016x}", name_fp(Some(name)))).map(Val::Id),
                );
                let f = text(f, Key::TopicName, name);
                let f = match since {
                    Some(s) => text(f, Key::Since, s),
                    None => f,
                };
                (Kind::Topic, aterm_slog::Actor::Nobody, f)
            }
            F::Advisory {
                origin,
                reason,
                generation,
            } => {
                let f = word(Fields::new(), Key::Origin, origin);
                let f = with(
                    f,
                    Key::Gen,
                    aterm_slog::ShortId::new(&generation.to_string()).map(Val::Id),
                );
                let f = text(f, Key::Reason, reason);
                (Kind::Advisory, aterm_slog::Actor::Aterm, f)
            }
            F::Fetch { off } => {
                let f = with(Fields::new(), Key::Off, Some(Val::U(off)));
                (Kind::MailFetch, aterm_slog::Actor::Nobody, f)
            }
            F::MailGap { from, to, records } => {
                let f = with(Fields::new(), Key::Off, Some(Val::U(from)));
                let f = with(f, Key::Floor, Some(Val::U(to)));
                let f = with(f, Key::Lost, records.map(Val::U));
                (Kind::MailGap, aterm_slog::Actor::Bridge, f)
            }
        };
        let elastic = (kind == Kind::Mail).then_some(Key::Via);
        let mark = |f: Fields| {
            Mark::new(kind, Src::Fabric, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f)
        };
        let _ = self.mark_within_record(mark, f, elastic, at);
    }

    fn mark_within_record(
        &mut self,
        mark: impl Fn(Fields) -> Mark,
        f: Fields,
        elastic: Option<Key>,
        at: Stamp,
    ) -> Option<u64> {
        let over = match self.log.mark(mark(f.clone()), at) {
            Ok(seq) => return Some(seq),
            Err(aterm_slog::Refused::Encode(aterm_slog::EncodeError::TooLarge(n))) => {
                n.saturating_sub(aterm_slog::MAX_RECORD_BYTES)
            }
            Err(_) => return None,
        };
        let key = elastic?;
        let text = f.get_str(key)?;
        let room = aterm_slog::field::encoded_len(text).checked_sub(over)?;
        let kept = aterm_slog::field::clamp_encoded(text, room).to_string();
        let mut f = f;
        match BoundedText::new(key, &kept) {
            Some(t) => {
                let _ = f.set(key, Val::Text(t));
            }
            None => f.remove(key),
        }
        self.log.mark(mark(f), at).ok()
    }

    /// `mark - meta field=<f> value=<v>` from
    /// [`crate::session_timeline::SessionTimeline::record_meta_change`].
    pub(crate) fn observe_meta(
        &mut self,
        change: crate::session_timeline::MetaChange<'_>,
        escalated: bool,
        at: Stamp,
    ) {
        let land_at = change.land_at.map(|ms| ms.div_euclid(1000));
        let fp = match land_at {
            None => name_fp(change.value),
            Some(s) => name_fp(Some(&format!("{}\u{0}{s}", change.value.unwrap_or("")))),
        };
        let moved = self.observed.meta_moved(change.field, fp);
        let mut f = Fields::new();
        if let Some(t) = BoundedToken::new(Key::Field, change.field) {
            let _ = f.set(Key::Field, Val::Tok(t));
        }
        if FINGERPRINTED_META.contains(&change.field) {
            if let Some(v) = change.value {
                let shown = name_fp(Some(v));
                if let Some(id) = aterm_slog::ShortId::new(&format!("{shown:016x}")) {
                    let _ = f.set(Key::Fp, Val::Id(id));
                }
                if let Some(t) = BoundedText::new(Key::AttnText, v) {
                    let _ = f.set(Key::AttnText, Val::Text(t));
                }
            }
        } else if let Some(t) = change.value.and_then(|v| BoundedText::new(Key::Value, v)) {
            let _ = f.set(Key::Value, Val::Text(t));
        }
        let f = with(f, Key::LandAt, land_at.map(Val::I));
        let f = with(f, Key::Carried, (!moved).then_some(Val::U(1)));
        let _ = self.log.mark(
            Mark::new(Kind::Meta, Src::Meta, Conf::Exact)
                .at(Node::SESSION)
                .by(change.by)
                .fields(f),
            at,
        );
        let was = self.observed.escalated.replace(escalated).unwrap_or(false);
        if was == escalated {
            return;
        }
        let kind = if escalated { Kind::Need } else { Kind::Clear };
        let f = Fields::new()
            .word(Key::Word, "escalated")
            .unwrap_or_default();
        let _ = self.log.mark(
            Mark::new(kind, Src::Meta, Conf::Strong)
                .at(Node::SESSION)
                .by(aterm_slog::Actor::Aterm)
                .fields(f),
            at,
        );
    }

    /// A WORKER'S TASK MOVED.
    pub(crate) fn observe_task(&mut self, task: &crate::task_record::TaskRecord, at: Stamp) {
        let f = Fields::new();
        let f = with(f, Key::Rev, Some(Val::U(task.rev)));
        let f = with(
            f,
            Key::State,
            aterm_slog::vocab::lookup(aterm_slog::vocab::STATE_WORDS, task.state.as_str())
                .map(Val::Word),
        );
        let f = with(f, Key::Attention, Some(Val::U(u64::from(task.attention))));
        let f = with(
            f,
            Key::Why,
            task.why
                .and_then(|w| aterm_slog::vocab::lookup(aterm_slog::vocab::WHY_WORDS, w))
                .map(Val::Word),
        );
        let f = with(f, Key::Code, task.exit.map(|c| Val::I(i64::from(c))));
        let edited = task.fact_rev > 0;
        let f = with(f, Key::FactRev, edited.then_some(Val::U(task.fact_rev)));
        let f = with(
            f,
            Key::FactRule,
            task.fact
                .and_then(|w| {
                    aterm_slog::vocab::lookup(aterm_slog::vocab::FACT_RULE_WORDS, w.rule.name())
                })
                .map(Val::Word),
        );
        let f = with(
            f,
            Key::FactN,
            task.fact.map(|w| Val::U(u64::from(w.ordinal.get()))),
        );
        self.put(Kind::Task, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - story word=<verb>`.
    pub(crate) fn observe_story(&mut self, verb: &str, by: aterm_slog::Actor, at: Stamp) {
        let f = Fields::new()
            .word(Key::Word, verb)
            .or_else(|_| Fields::new().word(Key::Word, "unknown"))
            .unwrap_or_default();
        let _ = self.log.mark(
            Mark::new(Kind::Story, Src::Ctl, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }

    /// `mark - usage h5=<pct> d7=<pct>`.
    pub(crate) fn observe_usage(&mut self, h5: Option<u32>, d7: Option<u32>, at: Stamp) {
        let f = with(Fields::new(), Key::H5, h5.map(|p| Val::U(u64::from(p))));
        let f = with(f, Key::D7, d7.map(|p| Val::U(u64::from(p))));
        let _ = self.log.mark(
            Mark::new(Kind::Usage, Src::Aterm, Conf::Strong)
                .at(Node::SESSION)
                .by(aterm_slog::Actor::Vendor)
                .fields(f),
            at,
        );
    }

    /// `mark - act word=relaunch rule=<row>`; once a new agent came up (`started`), the lift of
    /// the failure the relaunch caused ([`SessionLog::harness_saw_failure`]), then the harness's
    /// own `label` ([`Self::harness_label`]).
    pub(crate) fn observe_relaunch_act(
        &mut self,
        rule: &str,
        started: bool,
        label: Option<&str>,
        at: Stamp,
    ) {
        let Ok(f) = Fields::new()
            .word(Key::Word, aterm_agent::harness::acts::RELAUNCH_WORD)
            .and_then(|f| f.id(Key::Rule, rule))
        else {
            return;
        };
        let acted = self
            .log
            .mark(
                Mark::new(Kind::Act, Src::Aterm, Conf::Strong)
                    .at(Node::SESSION)
                    .by(aterm_slog::Actor::Harness)
                    .fields(f),
                at,
            )
            .is_ok();
        if acted && started {
            self.harness_saw_failure(at);
            if let Some(words) = label {
                self.harness_label(words, at);
            }
        }
    }

    /// THE HARNESS'S OWN LABEL: `mark - label text=<words> ttl=<s> by=harness src=aterm
    /// conf=strong`, written only while no need is open. A need's row comes before any label
    /// (§7.3), so a label written under one would show only once the need closed, out of its
    /// moment; the words are the harness's closed templates
    /// ([`aterm_agent::harness::acts::relaunch_label`]).
    pub(crate) fn harness_label(&mut self, words: &str, at: Stamp) -> bool {
        if self.log.state().top_need().is_some() {
            return false;
        }
        let Ok(f) = Fields::new()
            .text(Key::Text, words)
            .and_then(|f| f.u(Key::Ttl, aterm_agent::harness::acts::HARNESS_LABEL_TTL_S))
        else {
            return false;
        };
        self.log
            .mark(
                Mark::new(Kind::Label, Src::Aterm, Conf::Strong)
                    .at(Node::SESSION)
                    .by(aterm_slog::Actor::Harness)
                    .fields(f),
                at,
            )
            .is_ok()
    }

    /// `mark - state state=<spawning|alive|exited>`.
    pub(crate) fn observe_state(&mut self, state: &str, at: Stamp) {
        let f = Fields::new()
            .word(Key::State, state)
            .or_else(|_| Fields::new().word(Key::State, "unknown"))
            .unwrap_or_default();
        self.put(Kind::State, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - policy gen=<n>`.
    pub(crate) fn observe_policy(&mut self, generation: u64, at: Stamp) {
        let f = with(
            Fields::new(),
            Key::Gen,
            aterm_slog::ShortId::new(&generation.to_string()).map(Val::Id),
        );
        self.put(Kind::Policy, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - in-doubt verb=<w> producer=<n> key_seq=<n> reply=<w-w>`.
    pub(crate) fn observe_in_doubt(
        &mut self,
        verb: &str,
        producer: u64,
        key_seq: u64,
        reply: &str,
        at: Stamp,
    ) {
        let f = Fields::new()
            .word(Key::Verb, verb)
            .or_else(|_| Fields::new().word(Key::Verb, "unknown"))
            .unwrap_or_default();
        let f = with(f, Key::Producer, Some(Val::U(producer)));
        let f = with(f, Key::KeySeq, Some(Val::U(key_seq)));
        let f = with(
            f,
            Key::Reply,
            BoundedText::new(Key::Reply, reply).map(Val::Text),
        );
        self.put(Kind::InDoubt, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - row row=<kind> row_text=<payload>`.
    pub(crate) fn observe_row(&mut self, kind: &str, payload: &str, at: Stamp) {
        let f = with(
            Fields::new(),
            Key::RowKind,
            tok(Key::RowKind, &crate::session_timeline_view::row_token(kind)),
        );
        let f = with(
            f,
            Key::RowText,
            BoundedText::new(Key::RowText, payload).map(Val::Text),
        );
        let _ = self.mark_within_record(
            |f| {
                Mark::new(Kind::Row, Src::Aterm, Conf::Exact)
                    .at(Node::SESSION)
                    .fields(f)
            },
            f,
            Some(Key::RowText),
            at,
        );
    }
}

#[cfg(test)]
#[path = "session_log_observed_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_log_overlap_tests.rs"]
pub(crate) mod overlap_tests;

#[cfg(test)]
#[path = "session_log_program_tests.rs"]
mod program_tests;

#[cfg(test)]
#[path = "session_log_shown_tests.rs"]
mod shown_tests;

#[cfg(test)]
#[path = "session_log_ring_facts_tests.rs"]
mod ring_facts_tests;
