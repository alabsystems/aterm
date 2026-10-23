// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AGENTS PANEL'S DOTS — what each background agent and workflow in Claude Code's agents panel is doing, folded over successive readings of the panel: running, quiet, in a tool call of its own, stuck, halted, waiting, settled, and how much work it is doing, as numbers a renderer turns into brightness, glow and saturation.

use aterm_phase::agents_panel::{
    AgentsPanel, Bullet, Elapsed, PANEL_ROWS_MAX, PanelRow, RowKind, Span,
};

use super::flight::{Flight, ToolName};

/// Quiet this long and a running dot turns [`DotState::Quiet`].
pub const QUIET_MS: u64 = 20_000;
/// Quiet this long and it turns [`DotState::Stuck`]: Bash's default two-minute timeout plus 30 s.
pub const STUCK_MS: u64 = 150_000;
/// A clock still for `2 × resolution` plus this is [`DotState::Halted`].
pub const HALT_SLACK_MS: u64 = 1_000;
/// The slack, past the clock's resolution, a row's implied start may differ from its track's and still be that track.
pub const MATCH_SLACK_MS: u64 = 1_500;
/// Tokens that step by more than this cannot show quiet by themselves.
pub const BLIND_QUANTUM: u64 = 200;
/// The window [`Dot::rate`] is measured over.
pub const WINDOW_MS: u64 = 60_000;
/// A track not seen this long is dropped.
pub const ABSENT_MS: u64 = 600_000;
/// The tracks a session keeps.
pub const TRACKS_MAX: usize = 16;
/// The token samples a track keeps.
pub const SAMPLES: usize = 16;
/// The spacing the kept samples keep: while the newest is younger than this past the one before it, a change replaces it, so [`SAMPLES`] span more than the window (15 gaps of 5 s).
pub const SAMPLE_GAP_MS: u64 = 5_000;
/// [`Dot::sat`] at [`STUCK_MS`] of quiet.
pub const SAT_QUIET_FLOOR: f32 = 0.15;
/// [`Dot::sat`] of a stuck dot.
pub const SAT_STUCK: f32 = 0.10;
/// While a look is still moving, the host is due back this soon.
pub const LOOK_TICK_MS: u64 = 1_000;

/// The looks [`PanelTrack::fingerprint`] tells apart, per unit.
const LOOK_STEPS: f32 = 32.0;

/// What the host saw of a bullet's colour: Claude's default or dim ink, or one of its status colours (success, error, warning).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tint {
    #[default]
    Plain,
    Marked,
}

/// A dot's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DotState {
    /// Evidence of work under [`QUIET_MS`] ago.
    Running,
    /// No evidence for [`QUIET_MS`], not yet [`STUCK_MS`].
    Quiet,
    /// No evidence for [`QUIET_MS`] or more, but its own file says it waits on a call of its own inside that call's budget ([`super::flight`]): working, not stuck.
    Tool,
    /// No evidence for [`STUCK_MS`] or more.
    Stuck,
    /// Its clock stopped moving.
    Halted,
    /// `waiting`: running, nothing in flight.
    Waiting,
    /// `idle`: a teammate idle, or a finished agent read.
    Idle,
    /// `awaiting approval`: a plan waits on its lead.
    Awaiting,
    /// `⏸`: a workflow paused on a rate limit.
    Paused,
    /// Claude coloured its bullet: done, failed or killed.
    Settled,
}

/// One agent's or workflow's dot, as of the last fold or retime.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dot {
    /// The pane row (as the panel's locator placed it).
    pub row: u16,
    /// The bullet's pane columns.
    pub bullet: Span,
    pub kind: RowKind,
    pub state: DotState,
    /// How much work, `0..=1`: [`level()`] of [`Dot::rate`].
    pub level: f32,
    /// How sure the dot is that the work goes on, `0..=1`.
    pub sat: f32,
    /// Tokens per second over [`WINDOW_MS`].
    pub rate: f32,
    /// Since the last evidence of work.
    pub quiet_ms: u64,
    /// Its tokens step too coarsely (or are not drawn) to show quiet.
    pub blind: bool,
    /// While [`DotState::Tool`]: the tool it waits on, and for how long.
    pub waits: Option<Waits>,
}

/// What a [`DotState::Tool`] dot waits on: what its title says (`in Bash 4m12s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    pub tool: ToolName,
    /// Since its call was written.
    pub for_ms: u64,
}

/// A track's own call in flight, on the panel's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InTool {
    tool: ToolName,
    /// When the host last said so, and how old the call was then: its age
    /// is kept, not when it was written, since a call may be older than the
    /// panel's clock (written before this aterm, or this window's first
    /// panel, began counting).
    flown: u64,
    age_ms: u64,
    /// When it outlives its budget.
    until: u64,
}

impl InTool {
    /// How long its call has been in flight at `now`.
    const fn for_ms(&self, now: u64) -> u64 {
        self.age_ms.saturating_add(now.saturating_sub(self.flown))
    }
}

const NO_DOT: Dot = Dot {
    row: 0,
    bullet: Span { start: 0, end: 0 },
    kind: RowKind::Agent,
    state: DotState::Running,
    level: 0.0,
    sat: 1.0,
    rate: 0.0,
    quiet_ms: 0,
    blind: false,
    waits: None,
};

/// One row's history across folds.
#[derive(Debug, Clone, Copy)]
pub struct RowTrack {
    kind: RowKind,
    name_hash: u64,
    /// Its place among the rows of its name, when last seen.
    ordinal: u8,
    /// The fold it was last matched in.
    seen_ms: u64,
    /// Whether the last fold matched it.
    present: bool,
    /// The clock as last drawn, `(ms, res_ms)`.
    clock: Option<(u64, u32)>,
    /// When the clock last moved (or was first seen).
    clock_ms: u64,
    /// The elapsed slot's word, where it says one.
    word: Option<Elapsed>,
    tokens: Option<(u64, u64)>,
    activity_hash: u64,
    progress: Option<(u32, u32)>,
    /// The last evidence of work.
    evidence_ms: u64,
    /// Token change points, oldest first: `(t_ms, count)`.
    samples: [(u64, u64); SAMPLES],
    n_samples: u8,
    /// Where it was drawn last.
    row: u16,
    bullet: Span,
    paused: bool,
    tint: Tint,
    /// Its own call in flight, as the host last told it.
    tool: Option<InTool>,
    /// The call the host was last sure of (`Some(None)`: none in flight):
    /// another one is evidence.
    known_call: Option<Option<u64>>,
}

impl RowTrack {
    fn new(row: &PanelRow, ordinal: u8, tint: Tint, now: u64) -> RowTrack {
        let mut track = RowTrack {
            kind: row.kind,
            name_hash: row.name_hash,
            ordinal,
            seen_ms: now,
            present: true,
            clock: None,
            clock_ms: now,
            word: None,
            tokens: None,
            activity_hash: row.activity_hash,
            progress: None,
            evidence_ms: now,
            samples: [(0, 0); SAMPLES],
            n_samples: 0,
            row: row.row,
            bullet: Span { start: 0, end: 0 },
            paused: false,
            tint,
            tool: None,
            known_call: None,
        };
        track.observe(row, ordinal, tint, now);
        track
    }

    /// The start `now − elapsed` this track's clock implies, as of `now`.
    fn start(&self) -> Option<i128> {
        let (ms, _) = self.clock?;
        Some(i128::from(self.clock_ms) - i128::from(ms))
    }

    /// How well `row` (drawn at `now`, with `ordinal` among its name) fits
    /// this track: `None` when it cannot be this track, else a cost, lower
    /// is better.
    fn fit(&self, row: &PanelRow, ordinal: u8, now: u64) -> Option<u64> {
        if self.kind != row.kind || self.name_hash != row.name_hash {
            return None;
        }
        let drawn = row.elapsed.and_then(|(_, e)| e.clock());
        let (class, err) = match (drawn, self.clock) {
            (Some((ms, res)), Some((was, was_res))) => {
                let tol = u64::from(res.max(was_res)) + MATCH_SLACK_MS;
                let start = i128::from(now) - i128::from(ms);
                let err = (start - self.start()?).unsigned_abs();
                let err = u64::try_from(err).unwrap_or(u64::MAX);
                let wall = now.saturating_sub(self.seen_ms);
                if ms == was {
                    (0, 0)
                } else if err <= tol {
                    (0, err)
                } else if ms > was && ms - was <= wall + tol {
                    (1, err)
                } else {
                    return None;
                }
            }
            _ if self.ordinal == ordinal => (2, 0),
            _ => return None,
        };
        let activity = u64::from(row.activity_hash != self.activity_hash);
        let fell = u64::from(
            row.tokens
                .zip(self.tokens)
                .is_some_and(|((_, t), (count, _))| t.count < count),
        );
        let moved = u64::from(self.ordinal != ordinal);
        Some(
            (class << 60)
                | (activity << 59)
                | (fell << 58)
                | (moved << 57)
                | err.min((1 << 56) - 1),
        )
    }

    /// Take `row`'s reading at `now`.
    fn observe(&mut self, row: &PanelRow, ordinal: u8, tint: Tint, now: u64) {
        let slot = |t: &RowTrack| (t.clock.is_some(), t.word.is_some());
        let slot_before = slot(self);
        self.ordinal = ordinal;
        self.seen_ms = now;
        self.present = true;
        self.row = row.row;
        self.bullet = row
            .bullet
            .map_or(Span { start: 0, end: 0 }, |(span, _)| span);
        self.paused = row.bullet.is_some_and(|(_, b)| b == Bullet::Paused);
        self.tint = tint;
        let mut evidence = false;
        match row.elapsed.map(|(_, e)| e) {
            Some(Elapsed::Clock { ms, res_ms }) => {
                if self.clock.is_none_or(|(was, _)| was != ms) {
                    self.clock_ms = now;
                }
                self.clock = Some((ms, res_ms));
                self.word = None;
            }
            Some(word) => {
                self.word = Some(word);
                self.clock = None;
                self.clock_ms = now;
            }
            None => {
                self.word = None;
                self.clock = None;
                self.clock_ms = now;
            }
        }
        if let Some((_, t)) = row.tokens {
            match self.tokens {
                Some((count, _)) if t.count < count => {
                    self.n_samples = 0;
                    self.push_sample(now, t.count);
                    evidence = true;
                }
                Some((count, _)) if t.count > count => {
                    self.push_sample(now, t.count);
                    evidence = true;
                }
                Some(_) => {}
                None => {
                    self.push_sample(now, t.count);
                    evidence = true;
                }
            }
            self.tokens = Some((t.count, t.quantum));
        } else {
            self.tokens = None;
        }
        if row.activity_hash != self.activity_hash {
            self.activity_hash = row.activity_hash;
            evidence = true;
        }
        let progress = row.progress.map(|(_, p)| (p.done, p.total));
        if progress != self.progress {
            evidence |= self.progress.is_some();
            self.progress = progress;
        }
        if evidence || slot(self) != slot_before || self.word.is_some() {
            self.evidence_ms = now;
        }
    }

    /// Take what the host says of its own calls at `now`: a call answered or
    /// a new one written is evidence (the agent moved on), a row no file is
    /// certainly its keeps the screen's rules alone.
    fn fly(&mut self, flight: Flight, now: u64) {
        let known = match flight {
            Flight::Unknown => None,
            Flight::Model => Some(None),
            Flight::Tool { call, .. } => Some(Some(call)),
        };
        if let Some(known) = known {
            if self.known_call.is_some_and(|was| was != known) {
                self.evidence_ms = now;
            }
            self.known_call = Some(known);
        }
        self.tool = match flight {
            Flight::Tool {
                tool,
                age_ms,
                left_ms,
                ..
            } => Some(InTool {
                tool,
                flown: now,
                age_ms,
                until: now.saturating_add(left_ms),
            }),
            Flight::Unknown | Flight::Model => None,
        };
    }

    /// Its call in flight at `now`, while inside its budget.
    fn in_tool(&self, now: u64) -> Option<InTool> {
        self.tool.filter(|t| now < t.until)
    }

    /// Record `count` at `now`: in place of the newest sample while that one
    /// is under [`SAMPLE_GAP_MS`] past the one before it, else after it.
    fn push_sample(&mut self, now: u64, count: u64) {
        let n = usize::from(self.n_samples);
        if n >= 2 && self.samples[n - 1].0 - self.samples[n - 2].0 < SAMPLE_GAP_MS {
            self.samples[n - 1] = (now, count);
            return;
        }
        if n == SAMPLES {
            self.samples.copy_within(1.., 0);
            self.samples[SAMPLES - 1] = (now, count);
        } else {
            self.samples[n] = (now, count);
            self.n_samples += 1;
        }
    }

    /// Tokens per second from the base sample to `now`: the base is the
    /// last sample at or before `now −` [`WINDOW_MS`] (the count stood there
    /// until the next change), else the oldest.
    fn rate(&self, now: u64) -> f32 {
        let samples = &self.samples[..usize::from(self.n_samples)];
        let (Some(&(_, newest)), Some(&oldest)) = (samples.last(), samples.first()) else {
            return 0.0;
        };
        let from = now.saturating_sub(WINDOW_MS);
        let base = samples
            .iter()
            .rev()
            .find(|s| s.0 <= from)
            .copied()
            .unwrap_or(oldest);
        let span = now.saturating_sub(base.0);
        if span == 0 || newest <= base.1 {
            return 0.0;
        }
        (newest - base.1) as f32 * 1000.0 / span as f32
    }

    /// Whether quiet cannot show in its tokens.
    fn blind(&self) -> bool {
        self.tokens
            .is_none_or(|(_, quantum)| quantum > BLIND_QUANTUM)
    }

    /// The clock's halt time: when a still clock turns [`DotState::Halted`].
    fn halt_at(&self) -> Option<u64> {
        let (_, res) = self.clock?;
        Some(self.clock_ms + 2 * u64::from(res) + HALT_SLACK_MS)
    }

    fn dot(&self, now: u64) -> Dot {
        let quiet = now.saturating_sub(self.evidence_ms);
        let blind = self.blind();
        let state = if self.tint == Tint::Marked {
            DotState::Settled
        } else if let Some(word) = self.word {
            match word {
                Elapsed::AwaitingApproval => DotState::Awaiting,
                Elapsed::Idle => DotState::Idle,
                _ => DotState::Waiting,
            }
        } else if self.paused {
            DotState::Paused
        } else if self.halt_at().is_some_and(|at| now >= at) {
            DotState::Halted
        } else if quiet < QUIET_MS {
            DotState::Running
        } else if self.in_tool(now).is_some() {
            DotState::Tool
        } else if quiet < STUCK_MS || blind {
            DotState::Quiet
        } else {
            DotState::Stuck
        };
        let rate = self.rate(now);
        Dot {
            row: self.row,
            bullet: self.bullet,
            kind: self.kind,
            state,
            level: level(rate),
            sat: sat(state, quiet),
            rate,
            quiet_ms: quiet,
            blind,
            waits: self
                .in_tool(now)
                .filter(|_| state == DotState::Tool)
                .map(|t| Waits {
                    tool: t.tool,
                    for_ms: t.for_ms(now),
                }),
        }
    }

    /// The next time after `now` this track's dot changes with no new
    /// reading: a state's turn, or the next look tick while its looks move.
    fn due(&self, now: u64) -> Option<u64> {
        if self.tint == Tint::Marked || self.word.is_some() || self.paused {
            return None;
        }
        let mut next: Option<u64> = None;
        let mut take = |at: u64| {
            if at > now {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        };
        take(self.evidence_ms + QUIET_MS);
        if !self.blind() {
            take(self.evidence_ms + STUCK_MS);
        }
        if let Some(at) = self.halt_at() {
            take(at);
        }
        let in_tool = self.in_tool(now);
        if let Some(t) = in_tool {
            take(t.until);
        }
        let quiet = now.saturating_sub(self.evidence_ms);
        let fading = in_tool.is_none() && (QUIET_MS..STUCK_MS).contains(&quiet);
        let newest = self.samples[..usize::from(self.n_samples)]
            .last()
            .map(|s| s.0);
        let decaying = self.rate(now) > 0.0 && newest.is_some_and(|t| now < t + WINDOW_MS);
        if fading || decaying {
            take(now + LOOK_TICK_MS);
        }
        next
    }
}

/// `ln(1 + rate/2) / ln(501)`, clamped to `0..=1`.
#[must_use]
pub fn level(rate: f32) -> f32 {
    ((1.0 + rate.max(0.0) / 2.0).ln() / 501.0f32.ln()).clamp(0.0, 1.0)
}

/// A dot's saturation at `quiet_ms` without evidence.
#[must_use]
pub fn sat(state: DotState, quiet_ms: u64) -> f32 {
    if state == DotState::Stuck {
        return SAT_STUCK;
    }
    if matches!(state, DotState::Settled | DotState::Tool) || quiet_ms <= QUIET_MS {
        return 1.0;
    }
    let t = (quiet_ms.min(STUCK_MS) - QUIET_MS) as f32 / (STUCK_MS - QUIET_MS) as f32;
    1.0 - t * (1.0 - SAT_QUIET_FLOOR)
}

/// One session's panel history: its row tracks and its dots as of the last fold or retime.
#[derive(Debug, Clone)]
pub struct PanelTrack {
    tracks: [Option<RowTrack>; TRACKS_MAX],
    dots: [Dot; PANEL_ROWS_MAX],
    /// Which track each dot shows.
    dot_track: [u8; PANEL_ROWS_MAX],
    /// Which of the panel's rows each dot is (its index in `rows()`).
    dot_row: [u8; PANEL_ROWS_MAX],
    n_dots: u8,
    now_ms: u64,
}

impl Default for PanelTrack {
    fn default() -> Self {
        PanelTrack {
            tracks: [None; TRACKS_MAX],
            dots: [NO_DOT; PANEL_ROWS_MAX],
            dot_track: [0; PANEL_ROWS_MAX],
            dot_row: [0; PANEL_ROWS_MAX],
            n_dots: 0,
            now_ms: 0,
        }
    }
}

impl PanelTrack {
    /// Fold the panel as read at `now_ms`: `tints[i]` is what the host saw of `panel.rows()[i]`'s bullet colour (missing: [`Tint::Plain`]).
    pub fn fold(&mut self, panel: &AgentsPanel, tints: &[Tint], now_ms: u64) {
        self.fold_flown(panel, tints, &[], now_ms);
    }

    /// [`Self::fold`], `flights[i]` what the host knows of `panel.rows()[i]`'s own calls ([`super::flight::place`]; missing: [`Flight::Unknown`]).
    pub fn fold_flown(
        &mut self,
        panel: &AgentsPanel,
        tints: &[Tint],
        flights: &[Flight],
        now_ms: u64,
    ) {
        let now = now_ms.max(self.now_ms);
        self.now_ms = now;
        for track in &mut self.tracks {
            if track.is_some_and(|t| now.saturating_sub(t.seen_ms) >= ABSENT_MS) {
                *track = None;
            }
        }
        for track in self.tracks.iter_mut().flatten() {
            track.present = false;
        }
        let mut tasks: [(usize, u8); PANEL_ROWS_MAX] = [(0, 0); PANEL_ROWS_MAX];
        let mut n_tasks = 0;
        for (i, row) in panel.rows().iter().enumerate() {
            if !row.is_task() || row.bullet.is_none() {
                continue;
            }
            let ordinal = panel.rows()[..i]
                .iter()
                .filter(|r| r.is_task() && r.kind == row.kind && r.name_hash == row.name_hash)
                .count();
            tasks[n_tasks] = (i, u8::try_from(ordinal).unwrap_or(u8::MAX));
            n_tasks += 1;
        }
        let mut matched: [Option<usize>; PANEL_ROWS_MAX] = [None; PANEL_ROWS_MAX];
        let mut claimed = [false; TRACKS_MAX];
        loop {
            let mut best: Option<(u64, usize, usize)> = None;
            for (k, &(i, ordinal)) in tasks[..n_tasks].iter().enumerate() {
                if matched[k].is_some() {
                    continue;
                }
                for (t, track) in self.tracks.iter().enumerate() {
                    let Some(track) = track else { continue };
                    if claimed[t] {
                        continue;
                    }
                    if let Some(cost) = track.fit(&panel.rows()[i], ordinal, now)
                        && best.is_none_or(|(c, _, _)| cost < c)
                    {
                        best = Some((cost, k, t));
                    }
                }
            }
            let Some((_, k, t)) = best else { break };
            matched[k] = Some(t);
            claimed[t] = true;
        }
        self.n_dots = 0;
        for (k, &(i, ordinal)) in tasks[..n_tasks].iter().enumerate() {
            let row = &panel.rows()[i];
            let tint = tints.get(i).copied().unwrap_or_default();
            let flight = flights.get(i).copied().unwrap_or_default();
            let t = match matched[k] {
                Some(t) if self.tracks[t].is_some() => {
                    if let Some(track) = self.tracks[t].as_mut() {
                        track.observe(row, ordinal, tint, now);
                    }
                    t
                }
                _ => {
                    let t = self.free_slot(&claimed);
                    self.tracks[t] = Some(RowTrack::new(row, ordinal, tint, now));
                    claimed[t] = true;
                    t
                }
            };
            if let Some(track) = self.tracks[t].as_mut() {
                track.fly(flight, now);
            }
            self.dot_track[usize::from(self.n_dots)] = u8::try_from(t).unwrap_or(0);
            self.dot_row[usize::from(self.n_dots)] = u8::try_from(i).unwrap_or(u8::MAX);
            self.n_dots += 1;
        }
        self.redot();
    }

    /// Take what the host now knows of the last reading's rows' own calls (`flights[i]` of `panel.rows()[i]`, as [`Self::fold_flown`] takes them) at `now_ms`, with no new reading: the session's files moved while the screen did not.
    pub fn fly(&mut self, flights: &[Flight], now_ms: u64) {
        let now = now_ms.max(self.now_ms);
        self.now_ms = now;
        for k in 0..usize::from(self.n_dots) {
            let flight = flights
                .get(usize::from(self.dot_row[k]))
                .copied()
                .unwrap_or_default();
            if let Some(track) = self.tracks[usize::from(self.dot_track[k])].as_mut() {
                track.fly(flight, now);
            }
        }
        self.redot();
    }

    /// Recompute the dots at `now_ms` with no new reading (at [`Self::due`]).
    pub fn retime(&mut self, now_ms: u64) {
        self.now_ms = now_ms.max(self.now_ms);
        self.redot();
    }

    /// The dots, in the panel's order, as of the last fold or retime.
    #[must_use]
    pub fn dots(&self) -> &[Dot] {
        &self.dots[..usize::from(self.n_dots)]
    }

    /// When a dot next changes with nothing new on the screen: its state's next turn, or [`LOOK_TICK_MS`] on while a look still fades.
    #[must_use]
    pub fn due(&self) -> Option<u64> {
        self.shown()
            .filter_map(|track| track.due(self.now_ms))
            .min()
    }

    /// A hash of what the dots look like: place, state, blindness, and level and saturation in steps of 1/32.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            for b in v.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for dot in self.dots() {
            mix(u64::from(dot.row));
            mix(u64::from(dot.bullet.start));
            mix(dot.state as u64);
            mix(u64::from(dot.blind));
            mix(quantise(dot.level));
            mix(quantise(dot.sat));
        }
        h
    }

    /// How many tracks it holds (present or absent).
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.tracks.iter().flatten().count()
    }

    fn shown(&self) -> impl Iterator<Item = &RowTrack> {
        self.dot_track[..usize::from(self.n_dots)]
            .iter()
            .filter_map(|&t| self.tracks[usize::from(t)].as_ref())
    }

    fn redot(&mut self) {
        let now = self.now_ms;
        for k in 0..usize::from(self.n_dots) {
            if let Some(track) = &self.tracks[usize::from(self.dot_track[k])] {
                self.dots[k] = track.dot(now);
            }
        }
    }

    /// A slot for a new track: an empty one, else the least recently seen
    /// track this fold did not claim.
    fn free_slot(&self, claimed: &[bool; TRACKS_MAX]) -> usize {
        if let Some(t) = self.tracks.iter().position(Option::is_none) {
            return t;
        }
        (0..TRACKS_MAX)
            .filter(|&t| !claimed[t])
            .min_by_key(|&t| self.tracks[t].map_or(0, |track| track.seen_ms))
            .unwrap_or(0)
    }
}

/// `v` in `0..=1`, in steps of 1/32.
fn quantise(v: f32) -> u64 {
    (v.clamp(0.0, 1.0) * LOOK_STEPS).round() as u64
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
