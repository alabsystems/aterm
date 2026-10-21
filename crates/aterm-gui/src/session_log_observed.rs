// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REMAINING §4.1 MARKS (session log Phase 3B, plan §3.2): what the
//! engine, the status classifier, the screen reader, the resize ledger and
//! the launch card say, each one typed call into [`SessionLog`] from a
//! timeline hold its site already takes (no payload is formatted to be parsed
//! back, design:775). [`Observed`] holds what each last wrote, so a value
//! written twice writes one mark, and crosses a seamless update as additive
//! tokens on the carry's `state` host line (fingerprints only, ruling S5), so
//! a successor does not restate them.
//!
//! What each writes (the node `-` is the session, `<run>` the run in front):
//! - `mark - integration state=on|off|degraded`, when the shell
//!   integration's posture moves;
//! - `mark - notify n=<delta>`, at most once a sweep, the notifications the
//!   program asked for since the last (counted with notifications off too,
//!   ruling S19; never their text), the first read a baseline;
//! - `mark - bell n=<delta>`, at most once a second;
//! - `mark <run> progress state=none|normal|error|indeterminate|paused
//!   pct=<n>`, on a state change or each ten points;
//! - `mark <run> status phase=<w> out=ok|fail|signal code=<n>`, each
//!   published status transition, at the classifier's confidence;
//! - `mark <run> ctx pct=<n>` when Claude Code's context left moves a
//!   tenth, and `mark <run> goal state=on|off` when its goal does;
//! - `amend r<n> agent ver= model= effort= src=screen conf=strong`, the
//!   launch card's names (a transcript's exact amend outranks it);
//! - `mark - render state=desync-risk|healed|unjudged`, each transition of
//!   the resize ledger's verdict;
//! - `mark - cwd cwd=<path>` when the OSC 7 directory moves (`cwd=` absent
//!   for a cleared one), and `mark - title title=<informative title>`
//!   (content tier: never journaled, never carried) when the title moves
//!   past a spinner frame, from the drift flush;
//! - `mark - modes n=<reverted>`, the modes a handback or a manual reset
//!   put back;
//! - `mark - in-doubt verb=<w>`, a keyed write whose outcome is unknown
//!   (the producer's verb, never its bytes);
//! - `mark - mail dir=in|out …`, `mail-seen`, `mail-landed` and `hold`, from
//!   the fabric's one typed hook (`FabricFact`), never a body;
//! - `mark - meta field=<f> value=<v>` by its writer, from the one
//!   `meta-change` writer (`SessionTimeline::record_meta_change`) — for
//!   `attention`, whose text can hold a box's command or a question's words
//!   (the supervisor's escalation), `fp=<16 hex>` in place of the value,
//!   never the text — when the field's value moved from what this log last
//!   said of it (a successor restoring a carried value says nothing), and
//!   the `need`/`clear word=escalated src=meta` it derives `by=aterm`;
//! - `mark - story word=<told verb>` by its teller, never its text;
//! - `mark - usage h5=<pct> d7=<pct>` `by=vendor src=aterm`, each usage tap
//!   reading the harness worker takes while `[harness] usage_tap` is on;
//! - `mark - task rev= state= attention= [why=] [code=] src=aterm
//!   conf=exact`, each move of a worker's task record (phase 5, 5C-3), in
//!   the hold that records its `task-change` row.
//!
//! None of it is input, and none of it names a PTY path (slog-inert).

use aterm_slog::{
    Amend, BoundedText, BoundedToken, Conf, Fields, Key, Kind, Mark, Node, Src, Stamp, Val,
};

use super::{SessionLog, name_fp, with};

/// A bell is said at most this often (monotonic ms): a flood of BELs is one
/// count a second.
pub(crate) const BELL_EVERY_MS: u64 = 1_000;

/// What the engine says, read under the status sweep's terminal guard
/// ([`Self::of`]) and written once that guard is released.
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
    pub(crate) fn of(t: &aterm_core::terminal::Terminal) -> EngineRead {
        EngineRead {
            integration: t.shell_integration_posture(),
            notify_total: t.notify_total(),
            bell_total: t.bell_total(),
            progress: t.taskbar_progress(),
        }
    }
}

/// One published status transition, typed ([`crate::session_status::Status`]'s
/// words): the phase, the outcome with its code, the classifier's confidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusFacts {
    pub(crate) phase: &'static str,
    /// `ok`, `fail` or `signal`, and the exit code or signal number.
    pub(crate) out: Option<(&'static str, Option<i64>)>,
    pub(crate) conf: Conf,
}

/// What each kind of mark last wrote ([`SessionLog`]'s `observed`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Observed {
    integration: Option<&'static str>,
    /// The context left, in tenths.
    ctx: Option<u8>,
    goal: Option<bool>,
    /// The notify and bell totals last counted from (this process's engine:
    /// neither crosses an update, so the first read is a baseline).
    notify: Option<u64>,
    bell: Option<u64>,
    /// When a bell was last said (monotonic ms).
    bell_at: Option<u64>,
    /// The progress state and its tenth.
    progress: Option<(&'static str, Option<u8>)>,
    render: Option<&'static str>,
    /// The cwd and the informative title last said, as fingerprints
    /// ([`name_fp`]; 0: cleared): the carry holds no text (ruling S5).
    cwd: Option<u64>,
    title: Option<u64>,
    /// Whether the effective attention owner was the supervisor's when a
    /// meta write last said (the `escalated` need's standing).
    escalated: Option<bool>,
    /// Each meta field's value as the log last said it, a fingerprint
    /// ([`name_fp`]; 0: unset), at most [`META_FIELDS_MAX`] fields.
    meta: Vec<(String, u64)>,
}

/// The most meta fields [`Observed`] remembers: every user field and the
/// supervisor's claim (`SUPERVISOR_FIELD`).
const META_FIELDS_MAX: usize = crate::session_timeline::MetaField::ALL.len() + 1;

/// A meta field whose value is free text that can hold what the log never
/// stores (a box's command, a question's words, the screen's last line:
/// the supervisor's escalation): its mark says a fingerprint, never the
/// text.
const FINGERPRINTED_META: &[&str] = &["attention"];

/// A title's fingerprint, its leading busy-spinner frame left out
/// ([`crate::toolbar::busy_spinner_title_parts`]): a move of the frame alone
/// (`⠋ Build` → `⠙ Build`) is no move. 0 for no title.
pub(crate) fn title_fp(title: &str) -> u64 {
    if title.is_empty() {
        return 0;
    }
    match crate::toolbar::busy_spinner_title_parts(title) {
        Some((_, rest)) => name_fp(Some(&format!("\u{1}{rest}"))),
        None => name_fp(Some(title)),
    }
}

/// The `state` word of a shell integration posture.
fn integration_word(p: aterm_core::terminal::ShellIntegrationPosture) -> &'static str {
    use aterm_core::terminal::ShellIntegrationPosture as P;
    match p {
        P::Off => "off",
        P::On => "on",
        P::Degraded => "degraded",
    }
}

/// The `state` word and percent of an OSC 9;4 progress (`none` for none).
fn progress_of(p: Option<aterm_types::osc::TaskbarProgress>) -> (&'static str, Option<u8>) {
    use aterm_types::osc::TaskbarProgress as P;
    match p {
        None | Some(P::Hidden) => ("none", None),
        Some(P::Normal(n)) => ("normal", Some(n)),
        Some(P::Error(n)) => ("error", Some(n)),
        Some(P::Indeterminate) => ("indeterminate", None),
        Some(P::Paused(n)) => ("paused", Some(n)),
        // A state a later engine names that this build does not know.
        Some(_) => ("unknown", None),
    }
}

/// A word of `words` a carried token names, as the `&'static` word.
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
const RENDER_WORDS: &[&str] = &["desync-risk", "healed", "unjudged"];

impl Observed {
    /// The carry's tokens for the `state` host line (additive: a reader
    /// without them reads the line as it did): what a successor must not
    /// restate. Its one caller, the carry's `host_lines`, is Unix's and the
    /// tests' (the seamless update's handoff), so it carries that gate.
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
        if let Some(w) = self.render {
            s.push_str(&format!(" obs_render={w}"));
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
        s
    }

    /// Read back the tokens of [`Self::carry_tokens`] off a `state` line;
    /// what is absent or unreadable stays unknown.
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
        self.render = kv("obs_render").and_then(|w| carried_word(RENDER_WORDS, &w));
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
    }

    /// Note that the log says meta field `field` holds the value of
    /// fingerprint `fp`: whether that moved it from what the log last said.
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
    /// The run in front: the agent's, else the program's, else the
    /// command's; the session when none is open.
    fn front_run(&self) -> Node {
        let open = |n: Node| self.log.is_open(n).then_some(n);
        self.agent
            .as_ref()
            .and_then(|a| open(a.node))
            .or_else(|| self.fg.as_ref().and_then(|f| open(f.node)))
            .or_else(|| self.cmd.as_ref().and_then(|c| open(c.node)))
            .unwrap_or(Node::SESSION)
    }

    /// The open agent run of Claude Code, the one reader whose screen facts
    /// the log keeps (plan §3.2; Codex is Phase 5).
    fn claude_run(&self) -> Option<Node> {
        self.agent
            .as_ref()
            .filter(|a| {
                self.log.is_open(a.node)
                    && match a.who {
                        aterm_phase::Program::Claude => true,
                        aterm_phase::Program::Codex
                        | aterm_phase::Program::Agy
                        | aterm_phase::Program::Muse
                        | aterm_phase::Program::Gemini
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

    /// THE ENGINE'S MARKS, from one read under the sweep's guard: the
    /// integration posture on a move, the notifications and bells counted
    /// since the last (bells at most once a second), the progress on a
    /// state change or each ten points.
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
        // The first read in a process is the baseline: what came before it
        // was another engine's, or said by the process that counted it.
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
            // A session that never showed progress says nothing.
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

    /// `mark <run> status …`: one published status transition (the status
    /// sweep dedups transitions; every call writes).
    pub(crate) fn observe_status(&mut self, s: StatusFacts, at: Stamp) {
        let mut f = with(Fields::new(), Key::Phase, Some(Val::Word(s.phase)));
        if let Some((out, code)) = s.out {
            f = with(f, Key::Out, Some(Val::Word(out)));
            f = with(f, Key::Code, code.map(Val::I));
        }
        let run = self.front_run();
        self.put(Kind::Status, Src::Status, s.conf, run, f, at);
    }

    /// The screen saw a person stop the turn (Esc): its idle close reads
    /// `out=interrupted` (called before the verdict is published).
    pub(crate) fn agent_interrupted(&mut self) {
        if let Some(a) = self.agent.as_mut() {
            a.tracker.note_interrupted();
        }
    }

    /// Claude Code's context left (`pct`, when its screen shows it) and its
    /// goal (`None`: not read), after the verdict is published: `mark <run>
    /// ctx pct=` when the tenth moves, `mark <run> goal state=` when the
    /// goal does (a first `off` is no news).
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

    /// THE LAUNCH CARD's names, kept anew for the process in front
    /// (`SessionTimeline::note_claude_card`): an amend of the agent run with
    /// what it does not already hold at least as surely.
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

    /// `mark - render state=…`: the resize ledger's verdict moved
    /// (`SessionTimeline::record_render`).
    pub(crate) fn observe_render(&mut self, state: &'static str, at: Stamp) {
        if self.observed.render == Some(state) {
            return;
        }
        self.observed.render = Some(state);
        let f = with(Fields::new(), Key::State, Some(Val::Word(state)));
        self.put(Kind::Render, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - cwd cwd=<path>`: the OSC 7 directory the drift flush read
    /// (`SessionTimeline::record_cwd_change`), when it moved; a cleared one
    /// is the mark with no `cwd=`.
    pub(crate) fn observe_cwd(&mut self, cwd: Option<&str>, at: Stamp) {
        let cwd = cwd.filter(|c| !c.is_empty());
        let fp = name_fp(cwd);
        if self.observed.cwd == Some(fp) {
            return;
        }
        self.observed.cwd = Some(fp);
        let mut f = Fields::new();
        if let Some(t) = cwd.and_then(|c| BoundedText::new(Key::Cwd, c)) {
            let _ = f.set(Key::Cwd, Val::Text(t));
        }
        self.put(Kind::Cwd, Src::Engine, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - title title=<informative title>`: the drift flush's title
    /// ([`crate::tab_label::informative`]) moved, a spinner frame's move
    /// aside ([`title_fp`]). The title is content tier: `log` prints it
    /// only at `tier=content`, and neither the journal nor the carry holds
    /// it.
    pub(crate) fn observe_title(&mut self, title: &str, at: Stamp) {
        let fp = title_fp(title);
        if self.observed.title == Some(fp) {
            return;
        }
        // The first read of a session with no title is no news.
        let first = self.observed.title.is_none();
        self.observed.title = Some(fp);
        if first && fp == 0 {
            return;
        }
        let mut f = Fields::new();
        if let Some(t) = BoundedText::new(Key::Title, title) {
            let _ = f.set(Key::Title, Val::Text(t));
        }
        self.put(Kind::Title, Src::Engine, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - modes n=<reverted>`: a foreground handback or a manual reset
    /// put `reverted` modes back (`by=human` for Edit ▸ Reset Terminal).
    pub(crate) fn observe_modes(&mut self, reverted: usize, by: aterm_slog::Actor, at: Stamp) {
        let n = u64::try_from(reverted).unwrap_or(u64::MAX);
        let f = with(Fields::new(), Key::N, Some(Val::U(n)));
        let _ = self.log.mark(
            Mark::new(Kind::Modes, Src::Engine, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }

    /// THE FABRIC'S MARKS ([`crate::fabric::FabricFact`], through the one
    /// hook `fabric_bind` implements, in the timeline row's hold): `mail
    /// dir=in|out`, `mail-seen`, `mail-landed` and `hold`. Ids, words and
    /// offsets only — a body is never in a fact, so it is in no record.
    pub(crate) fn observe_fabric(&mut self, fact: crate::fabric::FabricFact<'_>, at: Stamp) {
        use crate::fabric::FabricFact as F;
        // A person's key the timeline's hold kept from the log stands
        // before the mail (ruling T2).
        self.reconcile_person(at);
        let word = |f: Fields, k: Key, w: &str| {
            let fallback = f.clone();
            f.word(k, w)
                .or_else(|_| fallback.word(k, "unknown"))
                .unwrap_or_default()
        };
        let text = |mut f: Fields, k: Key, t: &str| {
            if let Some(t) = BoundedText::new(k, t) {
                let _ = f.set(k, Val::Text(t));
            }
            f
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
            F::MailOut { kind, to, post } => {
                let f = word(Fields::new(), Key::Dir, "out");
                let f = word(f, Key::MailKind, kind);
                let f = text(f, Key::To, to);
                let f = with(f, Key::Inbox, Some(Val::U(post)));
                (Kind::Mail, aterm_slog::Actor::Nobody, f)
            }
            F::MailSeen { inbox, verdict } => {
                let f = with(Fields::new(), Key::Inbox, Some(Val::U(inbox)));
                let f = word(f, Key::Word, verdict.unwrap_or("handled"));
                (Kind::MailSeen, aterm_slog::Actor::Nobody, f)
            }
            F::MailLanded { post, off } => {
                let f = with(Fields::new(), Key::Inbox, Some(Val::U(post)));
                let f = with(f, Key::Off, Some(Val::U(off)));
                (Kind::MailLanded, aterm_slog::Actor::Bridge, f)
            }
            F::Hold { held, bridge } => {
                let f = word(Fields::new(), Key::State, if held { "on" } else { "off" });
                let by = if bridge {
                    aterm_slog::Actor::Bridge
                } else {
                    aterm_slog::Actor::Nobody
                };
                (Kind::Hold, by, f)
            }
        };
        // A `mail dir=in` mark sets the fold's mail latch as it folds: the
        // person's next input is marked (rulings T39, U19).
        let _ = self.log.mark(
            Mark::new(kind, Src::Fabric, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }

    /// `mark - meta field=<f> value=<v>` by its writer (`value=` absent for
    /// a key unset; host tier; `fp=` its fingerprint in place of the text
    /// for a [`FINGERPRINTED_META`] field), from the one `meta-change`
    /// writer ([`crate::session_timeline::SessionTimeline::record_meta_change`]),
    /// when the value moved from what this log last said of the field (a
    /// carried log's included: a successor's restore of the same value is
    /// no news, ruling S5), and the `escalated` need that write moves
    /// (addendum A16):
    /// `escalated` is whether the effective attention owner is now the
    /// supervisor's, and on a move it is `need` (raised) or `clear`
    /// (lifted) `word=escalated src=meta conf=strong`, always `by=aterm`,
    /// whoever wrote — so a second writer raises no second need, and
    /// aterm's own seed, supersede or restore can lift what any writer's
    /// write raised (a need is its raiser's, `fold::clear_lifts`).
    pub(crate) fn observe_meta(
        &mut self,
        change: crate::session_timeline::MetaChange<'_>,
        escalated: bool,
        at: Stamp,
    ) {
        let fp = name_fp(change.value);
        if self.observed.meta_moved(change.field, fp) {
            let mut f = Fields::new();
            if let Some(t) = BoundedToken::new(Key::Field, change.field) {
                let _ = f.set(Key::Field, Val::Tok(t));
            }
            if FINGERPRINTED_META.contains(&change.field) {
                if let Some(id) = change
                    .value
                    .and_then(|_| aterm_slog::ShortId::new(&format!("{fp:016x}")))
                {
                    let _ = f.set(Key::Fp, Val::Id(id));
                }
            } else if let Some(t) = change.value.and_then(|v| BoundedText::new(Key::Value, v)) {
                let _ = f.set(Key::Value, Val::Text(t));
            }
            let _ = self.log.mark(
                Mark::new(Kind::Meta, Src::Meta, Conf::Exact)
                    .at(Node::SESSION)
                    .by(change.by)
                    .fields(f),
                at,
            );
        }
        // The first write to a session that is not escalated is no news.
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

    /// A WORKER'S TASK MOVED (plan 5C-3; ruling T21): `mark - task rev=<n>
    /// state=<working|done|exited|stalled> attention=<0|1> [why=<w>]
    /// [code=<exit>] src=aterm conf=exact`, the fact of the timeline's
    /// `task-change` row ([`crate::task_record::TaskRecord::event_payload`]),
    /// written in the hold that records the row. Closed words and counters
    /// only: never the result's path, size or text, and never who started it.
    /// Local: never bus class, so no fleet reader learns it.
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
        self.put(Kind::Task, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - story word=<verb>`: what `aterm ctl story` told the window
    /// ([`aterm_messages::presence::StoryVerb::word`]) by its teller (the
    /// wire's `log_actor`), never its text.
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

    /// `mark - usage h5=<pct> d7=<pct>`: a usage tap reading the harness
    /// worker took (`harness_host::log_usage`), the account's five-hour and
    /// seven-day windows as the vendor reported them — `by=vendor
    /// src=aterm conf=strong` (the owner's default 8: aterm read what the
    /// vendor said). A window the reading lacks has no key.
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

    /// `mark - act word=relaunch rule=<row>`: a relaunch or upgrade row of
    /// the window's harness host that moved the session
    /// (`harness_host::log_act`, plan 5F-2, rulings T25 and T28) — `by=harness
    /// src=aterm conf=strong`: aterm saw its own harness act. `rule=` is the
    /// row's name ([`aterm_agent::harness::acts::relaunch_act`]); the row's
    /// pid, conversation and versions stay in the row. The turn the pass
    /// typed keeps its transport's `by=ctl`; this mark says the pass was the
    /// harness's. Counted in `brief`'s one `acts N` line, as every act is.
    ///
    /// Once the act is written, and only when the row's step brought a new
    /// agent process up (`started`,
    /// [`aterm_agent::harness::upgrade_drive::started`]: never a turn typed
    /// into the running agent, a stop or a wait), the failure the restart
    /// caused is lifted with `mark - seen by=harness` when it is the
    /// session's only one, an agent ran under its run, and the relaunched
    /// run is open ([`SessionLog::harness_saw_failure`], Phase 6 step 6C-1).
    pub(crate) fn observe_relaunch_act(&mut self, rule: &str, started: bool, at: Stamp) {
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
        }
    }

    /// `mark - in-doubt verb=<w>`: a keyed write whose outcome nobody knows
    /// (`pty_idem::record_in_doubt`), named by its verb (`unknown` for one
    /// the vocabulary does not hold), never by its bytes or its reply.
    pub(crate) fn observe_in_doubt(&mut self, verb: &str, at: Stamp) {
        let f = Fields::new()
            .word(Key::Verb, verb)
            .or_else(|_| Fields::new().word(Key::Verb, "unknown"))
            .unwrap_or_default();
        self.put(Kind::InDoubt, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }
}

#[cfg(test)]
#[path = "session_log_observed_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_log_overlap_tests.rs"]
mod overlap_tests;
