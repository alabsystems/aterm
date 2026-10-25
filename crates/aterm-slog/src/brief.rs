// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Token-budgeted session brief rollup, a pure function of the log window, fold state, budget, and `now_wall`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use aterm_codec::pct;

use crate::field::{Fields, Key, Tier, Val};
use crate::fold::{MailLink, Need, OpenNode, SessionState};
use crate::log::{Cursor, Gap};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, Op, Src};

/// Default budget for one session.
pub const DEFAULT_BUDGET: usize = 600;
/// Default budget for the instance-wide form.
pub const INSTANCE_BUDGET: usize = 1000;

/// The token estimate of `bytes`: bytes ÷ 4, rounded up.
#[must_use]
pub const fn tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// A rendered brief.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brief {
    /// The reply, header first, lines joined by `\n` (no trailing newline).
    pub text: String,
    /// Its token estimate, as `used=` prints it.
    pub used: usize,
    /// The shedding level taken (0: nothing shed … 4: one line).
    pub level: u8,
}

/// A duration for a node line: `40s`, `4m10s`, `3h12m`.
#[must_use]
pub fn dur(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// `[<src><suffix>]`.
fn reliability(src: Src, conf: Conf) -> String {
    format!("[{}{}]", src.word(), conf.suffix())
}

#[derive(Default)]
struct StepInfo<'a> {
    open: Option<&'a Rec>,
    close: Option<&'a Rec>,
    amends: Vec<&'a Rec>,
}

impl StepInfo<'_> {
    fn out(&self) -> Option<&'static str> {
        best(self.close, &self.amends, Key::Out).and_then(Rec::out)
    }

    /// Whether the step was opened by the transcript.
    fn transcript(&self) -> bool {
        self.open
            .or(self.close)
            .is_some_and(|r| r.src == Src::Transcript)
    }
}

/// How much transcript detail a rendering keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TxShed {
    /// All of it.
    Full,
    /// No closed transcript step lines; open transcript steps collapsed to the newest plus a count.
    Collapsed,
    /// None of it: rendered from [`scan_stripped`]'s window.
    Stripped,
}

#[derive(Default)]
struct TurnInfo<'a> {
    open: Option<&'a Rec>,
    close: Option<&'a Rec>,
    amends: Vec<&'a Rec>,
    steps: BTreeMap<u32, StepInfo<'a>>,
    /// Transcript steps omitted by [`scan_stripped`].
    tx_steps: u64,
    /// Transcript tools by class omitted by [`scan_stripped`].
    tx_tools: BTreeMap<&'static str, u64>,
}

/// Return the record from which a node's `key` is read (its close or a refining amend).
fn best<'a>(close: Option<&'a Rec>, amends: &[&'a Rec], key: Key) -> Option<&'a Rec> {
    let mut held = close.filter(|c| c.f.get(key).is_some());
    for a in amends.iter().copied().filter(|a| a.f.get(key).is_some()) {
        let wins = match held {
            None => close.is_some(),
            Some(h) if key == Key::Out => {
                crate::fold::amend_refines_out(h.conf, h.seq, a.conf, a.seq)
            }
            Some(h) => crate::fold::confidence_law(h.conf, h.seq, a.conf, a.seq),
        };
        if wins {
            held = Some(a);
        }
    }
    held
}

impl TurnInfo<'_> {
    fn out(&self) -> Option<&'static str> {
        best(self.close, &self.amends, Key::Out).and_then(Rec::out)
    }

    /// Whether a range may absorb this turn.
    fn ordinary(&self) -> bool {
        matches!(
            self.out(),
            Some("done" | "interrupted" | "asked" | "superseded")
        )
    }

    fn steps_total(&self) -> u64 {
        best(self.close, &self.amends, Key::Steps)
            .and_then(|c| c.f.get_u(Key::Steps))
            .map(|n| n.saturating_sub(self.tx_steps))
            .unwrap_or(self.steps.len() as u64)
    }

    /// Return the close record's `tools=` rollup minus stripped transcript tools.
    fn tools(&self) -> Option<String> {
        let tools =
            best(self.close, &self.amends, Key::Tools).and_then(|r| r.f.get_str(Key::Tools))?;
        let mut by: Vec<(u64, &str)> = tools
            .split(',')
            .filter_map(|item| {
                let (w, n) = item.split_once(':')?;
                let n = n.parse::<u64>().ok()?;
                let less = self.tx_tools.get(w).copied().unwrap_or(0);
                Some((n.saturating_sub(less), w))
            })
            .filter(|(n, _)| *n > 0)
            .collect();
        by.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let words: Vec<String> = by.iter().map(|(n, w)| format!("{w} {n}")).collect();
        (!words.is_empty()).then(|| words.join(" "))
    }
}

/// Window records sorted into turns, steps, marks, and mark counts.
struct Scan<'a> {
    turns: BTreeMap<u32, TurnInfo<'a>>,
    marks: Vec<&'a Rec>,
    acts: usize,
    stories: usize,
    shown: usize,
}

impl Scan<'_> {
    /// Whether any count line has a non-zero count.
    const fn counts_any(&self, totals: &crate::fold::Totals) -> bool {
        self.acts + self.stories + self.shown > 0 || totals.mail_in > 0 || totals.mail_out > 0
    }
}

/// Whether a mark of kind `k` renders its own line in a brief.
const fn mark_worth_a_line(k: Kind) -> bool {
    !matches!(
        k,
        Kind::Agent
            | Kind::Status
            | Kind::Title
            | Kind::Need
            | Kind::Clear
            | Kind::Verdict
            | Kind::Label
            | Kind::Cwd
            | Kind::Meta
            | Kind::Source
            | Kind::MailLanded
            | Kind::Integration
            | Kind::Ctx
            | Kind::Goal
            | Kind::Notify
            | Kind::Progress
            | Kind::Bell
            | Kind::Render
            | Kind::Modes
            | Kind::InDoubt
            | Kind::Usage
            | Kind::Config
            | Kind::MailSeen
            | Kind::Mail
            | Kind::Hold
            | Kind::Act
            | Kind::Story
            | Kind::Task
            | Kind::State
            | Kind::Policy
            | Kind::Topic
            | Kind::Advisory
            | Kind::Program
            | Kind::MailFetch
            | Kind::Shown
            | Kind::Row
            | Kind::MailGap
    )
}

/// An open record for `o`, as the fold holds it (its seq is not kept: 0).
fn seed_of(o: &OpenNode) -> Rec {
    let mut f = Fields::new();
    for h in o.f.iter() {
        let _ = f.set(h.key, h.val.clone());
    }
    Rec {
        seq: 0,
        t: o.t,
        op: Op::Open,
        node: o.node,
        par: o.par,
        k: o.kind,
        src: o.src,
        conf: o.conf,
        by: o.by,
        f,
    }
}

/// Return open turn and step records from the fold whose open records were evicted.
fn open_path_seeds(st: &SessionState, window: &[&Rec]) -> Vec<Rec> {
    [st.turn(), st.step()]
        .into_iter()
        .flatten()
        .filter(|o| !window.iter().any(|r| r.op == Op::Open && r.node == o.node))
        .map(seed_of)
        .collect()
}

/// Scan `window` without transcript records and return whether any were stripped.
fn scan_stripped<'a>(window: &[&'a Rec], seeds: &'a [Rec]) -> (Scan<'a>, bool) {
    let mut tx_steps: BTreeMap<u32, u64> = BTreeMap::new();
    let mut tx_tools: BTreeMap<u32, BTreeMap<&'static str, u64>> = BTreeMap::new();
    let mut tx_nodes: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    let mut kept: Vec<&'a Rec> = Vec::with_capacity(window.len());
    let mut any = false;
    for r in window.iter().copied() {
        let tx = r.src == Src::Transcript;
        let drop = match (r.op, r.node.lvl) {
            (Op::Open, Lvl::Step) if tx => {
                tx_nodes.insert(r.node.n);
                if let Some(p) = r.par.filter(|p| p.lvl == Lvl::Turn)
                    && r.f.get(Key::Cont).is_none()
                {
                    *tx_steps.entry(p.n).or_default() += 1;
                    if r.k == Kind::Tool
                        && let Some(cls) = r.f.get_word(Key::Cls)
                    {
                        *tx_tools.entry(p.n).or_default().entry(cls).or_default() += 1;
                    }
                }
                true
            }
            (Op::Close, Lvl::Step) => tx_nodes.contains(&r.node.n),
            (Op::Amend, Lvl::Step) => tx || tx_nodes.contains(&r.node.n),
            (Op::Amend, Lvl::Turn) => tx,
            _ => false,
        };
        any |= tx && drop;
        if !drop {
            kept.push(r);
        }
    }
    let mut s = scan(&kept, seeds);
    for (n, t) in &mut s.turns {
        t.tx_steps = tx_steps.get(n).copied().unwrap_or(0);
        t.tx_tools = tx_tools.remove(n).unwrap_or_default();
    }
    (s, any)
}

fn scan<'a>(window: &[&'a Rec], seeds: &'a [Rec]) -> Scan<'a> {
    let mut turns: BTreeMap<u32, TurnInfo<'a>> = BTreeMap::new();
    let mut step_turn: BTreeMap<u32, u32> = BTreeMap::new();
    let mut marks = Vec::new();
    let (mut acts, mut stories, mut shown) = (0, 0, 0);
    for r in window.iter().copied().chain(seeds) {
        let turn_of = |node: Node, step_turn: &BTreeMap<u32, u32>| match node.lvl {
            Lvl::Turn => Some(node.n),
            Lvl::Step => step_turn.get(&node.n).copied(),
            _ => None,
        };
        match (r.op, r.node.lvl) {
            (Op::Open, Lvl::Turn) => turns.entry(r.node.n).or_default().open = Some(r),
            (Op::Close, Lvl::Turn) => turns.entry(r.node.n).or_default().close = Some(r),
            (Op::Amend, Lvl::Turn) => turns.entry(r.node.n).or_default().amends.push(r),
            (Op::Open, Lvl::Step) => {
                if let Some(t) = r.par.and_then(|p| turn_of(p, &step_turn)) {
                    step_turn.insert(r.node.n, t);
                    turns
                        .entry(t)
                        .or_default()
                        .steps
                        .entry(r.node.n)
                        .or_default()
                        .open = Some(r);
                }
            }
            (Op::Close, Lvl::Step) => {
                if let Some(t) = step_turn.get(&r.node.n).copied() {
                    turns
                        .entry(t)
                        .or_default()
                        .steps
                        .entry(r.node.n)
                        .or_default()
                        .close = Some(r);
                }
            }
            (Op::Amend, Lvl::Step) => {
                if let Some(t) = step_turn.get(&r.node.n).copied() {
                    turns
                        .entry(t)
                        .or_default()
                        .steps
                        .entry(r.node.n)
                        .or_default()
                        .amends
                        .push(r);
                }
            }
            (Op::Mark, _) if mark_worth_a_line(r.k) => marks.push(r),
            (Op::Mark, _) if r.k == Kind::Act => acts += 1,
            (Op::Mark, _) if r.k == Kind::Story => stories += 1,
            (Op::Mark, _) if r.k == Kind::Shown => shown += 1,
            _ => {}
        }
    }
    turns.retain(|_, t| t.open.is_some() || t.close.is_some());
    Scan {
        turns,
        marks,
        acts,
        stories,
        shown,
    }
}

/// Observed needs (excluding claims), most urgent first.
fn observed_needs(st: &SessionState) -> impl Iterator<Item = &Need> {
    st.needs.iter().flatten().filter(|n| n.observed())
}

/// The needs raised by a mark: claims, each on a `claim need` line.
fn claimed_needs(st: &SessionState) -> impl Iterator<Item = &Need> {
    st.needs.iter().flatten().filter(|n| !n.observed())
}

/// The session line: the most urgent observed need, else the phase.
fn session_line(st: &SessionState, now: u64) -> String {
    let age = |t: u64| dur(now.saturating_sub(t));
    if let Some(c) = &st.closed {
        return format!("- closed out={} {} ago", c.out, age(c.t));
    }
    let mut s = String::from("-");
    if let Some(run) = st.run() {
        let who = run
            .f
            .str(Key::Agent)
            .or(run.f.str(Key::Prog))
            .or(run.f.str(Key::Detail))
            .map_or_else(|| run.kind.word().to_string(), pct::encode);
        let _ = write!(s, " {who} {} {}", run.node, age(run.t));
    } else {
        s.push_str(" session");
    }
    if let Some(n) = observed_needs(st).next() {
        let _ = write!(
            s,
            " · need={} {} {}",
            n.word,
            age(n.t),
            reliability(n.src, n.conf)
        );
    } else {
        let _ = write!(s, " · {}", st.phase.word);
        if st.phase.conf != Conf::Unknown {
            let _ = write!(s, " {}", reliability(st.phase.src, st.phase.conf));
        }
    }
    s
}

/// The word a step line names it by: its box, wall or class.
fn step_word(r: &Rec) -> Option<&'static str> {
    r.f.get_word(Key::Box)
        .map(|b| crate::words::BoxWord::from_wire(b).kind())
        .or(r.f.get_word(Key::Wall))
        .or(r.f.get_word(Key::Cls))
}

fn step_line(n: u32, s: &StepInfo<'_>, now: u64) -> Option<String> {
    let head = s.open.or(s.close)?;
    let mut line = format!("p{n} {}", head.k.word());
    if let Some(w) = s.open.and_then(step_word) {
        let _ = write!(line, " {w}");
    }
    match (s.open, s.close) {
        (_, Some(c)) => {
            let _ = write!(
                line,
                " {} {}",
                s.out().unwrap_or("unknown"),
                dur(c.f.get_u(Key::DurMs).unwrap_or(0))
            );
        }
        (Some(o), None) => {
            let _ = write!(line, " open {}", dur(now.saturating_sub(o.t)));
        }
        (None, None) => {}
    }
    Some(line)
}

fn turn_line(n: u32, t: &TurnInfo<'_>, link: Option<MailLink>, shed: TxShed, now: u64) -> String {
    let mut line = format!("t{n}");
    let by = t.open.map_or(Actor::Nobody, |o| o.by);
    match t.close {
        None => {
            let since = t.open.map_or(now, |o| o.t);
            let _ = write!(line, " open {}", dur(now.saturating_sub(since)));
        }
        Some(c) => {
            let _ = write!(
                line,
                " {} {}",
                t.out().unwrap_or("unknown"),
                dur(c.f.get_u(Key::DurMs).unwrap_or(0))
            );
        }
    }
    if by != Actor::Nobody {
        let _ = write!(line, " by={}", by.word());
    } else if t.open.is_none()
        && let Some(Val::Id(opener)) = t.close.and_then(|c| c.f.get(Key::OpBy))
    {
        let _ = write!(line, " by={opener}");
    }
    let (from, keys): (Option<&Rec>, &[Key]) = match t.open {
        Some(o) => (Some(o), &[Key::Ledger, Key::Cause, Key::Cont]),
        None => (t.close, &[Key::Ledger]),
    };
    if let Some(r) = from {
        for key in keys {
            match r.f.get(*key) {
                Some(Val::U(v)) => {
                    let _ = write!(line, " {}={v}", key.word());
                }
                Some(Val::Id(v)) => {
                    let _ = write!(line, " {}={v}", key.word());
                }
                _ => {}
            }
        }
    }
    if let Some(l) = link {
        let _ = write!(line, " mail=@{}{}", l.off, l.conf.suffix());
    }
    if t.close.is_none() {
        let open_step = |line: &mut String, sn: u32, o: &Rec| {
            let _ = write!(line, " · p{sn} {}", o.k.word());
            if let Some(w) = step_word(o) {
                let _ = write!(line, " {w}");
            }
            let _ = write!(line, " open {}", dur(now.saturating_sub(o.t)));
        };
        let mut tx_open: Vec<(u32, &Rec)> = Vec::new();
        for (sn, s) in &t.steps {
            if s.close.is_none()
                && let Some(o) = s.open
            {
                if shed == TxShed::Collapsed && s.transcript() {
                    tx_open.push((*sn, o));
                } else {
                    open_step(&mut line, *sn, o);
                }
            }
        }
        if let Some((sn, o)) = tx_open.last() {
            open_step(&mut line, *sn, o);
            if tx_open.len() > 1 {
                let _ = write!(line, " · +{} open", tx_open.len() - 1);
            }
        }
    } else if let Some(c) = t.close {
        let steps = t.steps_total();
        if steps > 0 {
            let _ = write!(
                line,
                " · {steps} {}",
                if steps == 1 { "step" } else { "steps" }
            );
            if let Some(tools) = t.tools() {
                let _ = write!(line, " {tools}");
            }
        }
        for (key, word) in [
            (Key::Appr, "appr"),
            (Key::Qs, "questions"),
            (Key::Walls, "walls"),
            (Key::Stalls, "stalls"),
            (Key::Lost, "lost"),
        ] {
            if let Some(n) = c.f.get_u(key) {
                let _ = write!(line, " · {word} {n}");
            }
        }
        for a in &t.amends {
            if let (Some(i), Some(o)) = (a.f.get_u(Key::TokIn), a.f.get_u(Key::TokOut)) {
                let _ = write!(line, " · tok ↑{i} ↓{o}");
            }
        }
    }
    let rel = t
        .amends
        .iter()
        .max_by_key(|a| a.conf.rank())
        .filter(|a| t.close.is_none_or(|c| a.conf.rank() > c.conf.rank()))
        .map(|a| (a.src, a.conf))
        .or(t.close.map(|c| (c.src, c.conf)))
        .or(t.open.map(|o| (o.src, o.conf)));
    if let Some((src, conf)) = rel {
        let _ = write!(line, " {}", reliability(src, conf));
    }
    line
}

fn range_line(run: &[(u32, &TurnInfo<'_>)]) -> String {
    let first = run.first().map_or(0, |r| r.0);
    let last = run.last().map_or(0, |r| r.0);
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    let mut steps = 0u64;
    for (_, t) in run {
        *counts.entry(t.out().unwrap_or("unknown")).or_default() += 1;
        steps += t.steps_total();
    }
    let mut by: Vec<(&str, u32)> = counts.into_iter().collect();
    by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let outs: Vec<String> = by.iter().map(|(w, n)| format!("{n} {w}")).collect();
    let mut line = format!("t{first}-t{last} {} turns: {}", run.len(), outs.join(" "));
    if steps > 0 {
        let _ = write!(line, " · {steps} steps");
    }
    line
}

fn mark_line(r: &Rec) -> String {
    let mut line = format!("{} {}", r.seq, r.k.word());
    for (k, v) in r.f.iter() {
        if k.tier() > Tier::WireMeta {
            continue;
        }
        let _ = write!(line, " {}=", k.word());
        match v {
            Val::Word(w) => line.push_str(w),
            Val::U(n) => {
                let _ = write!(line, "{n}");
            }
            Val::I(n) => {
                let _ = write!(line, "{n}");
            }
            Val::Id(id) => line.push_str(id.as_str()),
            Val::Tok(t) => pct::encode_into(&mut line, t.as_str()),
            Val::Text(t) => pct::encode_into(&mut line, t.as_str()),
        }
    }
    if r.by != Actor::Nobody {
        let _ = write!(line, " by={}", r.by.word());
    }
    let _ = write!(line, " {}", reliability(r.src, r.conf));
    line
}

/// Rendered body lines and the number of turn units available to shed.
struct Rendered {
    lines: Vec<String>,
    units: usize,
}

/// Omitted turns and mark/need/claim counts for the `more=` line.
#[derive(Default)]
struct More {
    turns: std::collections::BTreeSet<u32>,
    marks: usize,
    needs: usize,
    claims: usize,
}

/// Build the trailer's need and claim lines.
fn trailer(st: &SessionState, now: u64) -> (Vec<String>, Vec<String>) {
    let ago = |t: u64| dur(now.saturating_sub(t));
    let needs = observed_needs(st)
        .skip(1)
        .map(|n| {
            format!(
                "need {} {} {} {}",
                n.word,
                n.node,
                ago(n.t),
                reliability(n.src, n.conf)
            )
        })
        .collect();
    let mut claims = Vec::new();
    for v in crate::fold::shown_verdicts(st) {
        if let Some(w) = v.word {
            claims.push(format!(
                "claim verdict {w} by={}{} {} ago [?]",
                v.by.word(),
                claim_refs(v),
                ago(v.t)
            ));
        }
    }
    if let Some(l) = crate::fold::shown_label(st, now)
        && let Some(text) = &l.text
    {
        claims.push(format!(
            "claim label {} by={}{} {} ago [?]",
            pct::encode(text),
            l.by.word(),
            claim_refs(l),
            ago(l.t)
        ));
    }
    for n in claimed_needs(st) {
        claims.push(format!(
            "claim need {} by={} {} ago [?]",
            n.word,
            n.by.word(),
            ago(n.t)
        ));
    }
    (needs, claims)
}

/// Format a claim's `as=` and `re=` fields when present.
fn claim_refs(c: &crate::fold::Claim) -> String {
    let mut s = String::new();
    if let Some(a) = &c.as_name {
        s.push_str(" as=");
        pct::encode_into(&mut s, a);
    }
    if let Some(re) = &c.re {
        let _ = write!(s, " re={re}");
    }
    s
}

/// The `gap` line for the gap a read crossed.
fn gap_line(g: &Gap) -> String {
    format!(
        "gap why={} lost={} resync_to={}",
        g.why.word(),
        g.lost,
        g.resync_to
    )
}

/// Render body lines at shedding `level`, dropping the `drop` oldest turn units at level 3.
fn render(
    scan: &Scan<'_>,
    gap: Option<&Gap>,
    st: &SessionState,
    links: &[(Node, MailLink)],
    (level, drop, shed): (u8, usize, TxShed),
    counts: bool,
    now: u64,
) -> Rendered {
    let mut lines = vec![session_line(st, now)];
    let mut more = More::default();
    let turns: Vec<(u32, &TurnInfo<'_>)> = scan.turns.iter().rev().map(|(n, t)| (*n, t)).collect();
    let newest = turns.first().map(|t| t.0);
    let (needs, claims) = trailer(st, now);
    let mut units_len = 0;
    if level < 4 {
        let mut units: Vec<(Vec<String>, Vec<u32>)> = Vec::new();
        let mut i = 0;
        while i < turns.len() {
            let (n, t) = turns[i];
            if level >= 2 && Some(n) != newest && t.ordinary() {
                let mut j = i;
                while j < turns.len() && Some(turns[j].0) != newest && turns[j].1.ordinary() {
                    j += 1;
                }
                if j - i >= 2 {
                    let mut run: Vec<(u32, &TurnInfo<'_>)> = turns[i..j].to_vec();
                    run.reverse();
                    let covers: Vec<u32> = run.iter().map(|r| r.0).collect();
                    more.turns.extend(&covers);
                    units.push((vec![range_line(&run)], covers));
                    i = j;
                    continue;
                }
            }
            let link = links
                .iter()
                .find(|(node, _)| *node == Node::new(Lvl::Turn, n))
                .map(|(_, l)| *l);
            let mut unit = vec![turn_line(n, t, link, shed, now)];
            let detail = level == 0 || Some(n) == newest;
            if detail {
                for (sn, s) in &t.steps {
                    // An open turn's open steps are already on its line; collapsed, a closed
                    // transcript step has none.
                    if (t.close.is_none() && s.close.is_none())
                        || (shed == TxShed::Collapsed && s.transcript())
                    {
                        continue;
                    }
                    if let Some(l) = step_line(*sn, s, now) {
                        unit.push(l);
                    }
                }
            } else if !t.steps.is_empty() {
                more.turns.insert(n);
            }
            units.push((unit, vec![n]));
            i += 1;
        }
        units_len = units.len();
        let keep = if level >= 3 {
            units
                .len()
                .saturating_sub(drop)
                .max(usize::from(!units.is_empty()))
        } else {
            units.len()
        };
        for (at, (unit, covers)) in units.into_iter().enumerate() {
            if at < keep {
                lines.extend(unit);
            } else {
                more.turns.extend(covers);
            }
        }
        if level < 3 {
            lines.extend(scan.marks.iter().map(|r| mark_line(r)));
        } else {
            more.marks = scan.marks.len();
        }
        lines.extend(needs);
        let t = &st.totals;
        if counts && (t.mail_in > 0 || t.mail_out > 0) {
            lines.push(format!("mail in {} · out {}", t.mail_in, t.mail_out));
        }
        if counts && scan.acts + scan.stories > 0 {
            lines.push(format!("acts {} · stories {}", scan.acts, scan.stories));
        }
        if counts && scan.shown > 0 {
            lines.push(format!("shown {}", scan.shown));
        }
        lines.extend(claims);
    } else {
        more.turns.extend(turns.iter().map(|t| t.0));
        more.marks = scan.marks.len();
        more.needs = needs.len();
        more.claims = claims.len();
    }
    if let Some(g) = gap {
        lines.push(gap_line(g));
    }
    lines.push(more_line(&more));
    Rendered {
        lines,
        units: units_len,
    }
}

/// Format the `more=` trailer line.
fn more_line(more: &More) -> String {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &n in &more.turns {
        match runs.last_mut() {
            Some((_, hi)) if *hi + 1 == n => *hi = n,
            _ => runs.push((n, n)),
        }
    }
    let mut items: Vec<String> = Vec::new();
    for &(lo, hi) in runs.iter().rev() {
        if hi - lo >= 2 {
            items.push(format!("t{lo}-t{hi}"));
        } else {
            items.extend((lo..=hi).rev().map(|n| format!("t{n}")));
        }
    }
    for (word, n) in [
        ("marks", more.marks),
        ("needs", more.needs),
        ("claims", more.claims),
    ] {
        if n > 0 {
            items.push(format!("{word}:{n}"));
        }
    }
    if items.is_empty() {
        "more=-".to_string()
    } else {
        format!("more={}", items.join(","))
    }
}

/// Assemble the header and body with the total token estimate.
fn assemble(sid: &str, st: &SessionState, budget: usize, body: &[String]) -> (String, usize) {
    let body_text = body.join("\n");
    let mut used = 0usize;
    for _ in 0..4 {
        let header = format!(
            "OK {} brief sid={} v=2 as_of={} ep={} budget={budget} used={used}",
            body.len(),
            pct::encode(sid),
            st.as_of,
            st.ep
        );
        let total = header.len() + 1 + body_text.len();
        let next = tokens(total);
        if next == used {
            return (format!("{header}\n{body_text}"), used);
        }
        used = next;
    }
    let header = format!(
        "OK {} brief sid={} v=2 as_of={} ep={} budget={budget} used={used}",
        body.len(),
        pct::encode(sid),
        st.as_of,
        st.ep
    );
    (format!("{header}\n{body_text}"), used)
}

/// Pre-scan inputs for rendering a brief at any budget.
pub struct BriefInput<'a> {
    window: Vec<&'a Rec>,
    seeds: Vec<Rec>,
    gap: Option<Gap>,
    st: &'a SessionState,
    links: &'a [(Node, MailLink)],
}

impl<'a> BriefInput<'a> {
    /// Construct a [`BriefInput`] from a record window, read gap, fold state, and mail links.
    #[must_use]
    pub fn new(
        window: impl IntoIterator<Item = &'a Rec>,
        gap: Option<Gap>,
        st: &'a SessionState,
        links: &'a [(Node, MailLink)],
    ) -> Self {
        let window: Vec<&Rec> = window.into_iter().collect();
        let seeds = open_path_seeds(st, &window);
        BriefInput {
            window,
            seeds,
            gap,
            st,
            links,
        }
    }

    /// Scan the window with and without transcript records.
    #[must_use]
    pub fn scanned(&self) -> Briefing<'_> {
        #[cfg(any(test, debug_assertions))]
        SCANS.with(|n| n.set(n.get() + 1));
        let (stripped, transcript) = scan_stripped(&self.window, &self.seeds);
        Briefing {
            scan: scan(&self.window, &self.seeds),
            stripped,
            transcript,
            gap: self.gap.as_ref(),
            st: self.st,
            links: self.links,
        }
    }
}

/// Scanned [`BriefInput`] ready to render at any budget.
pub struct Briefing<'b> {
    scan: Scan<'b>,
    /// The window without the transcript's records ([`TxShed::Stripped`]).
    stripped: Scan<'b>,
    /// The window held a transcript record.
    transcript: bool,
    gap: Option<&'b Gap>,
    st: &'b SessionState,
    links: &'b [(Node, MailLink)],
}

impl Briefing<'_> {
    /// Render at shedding `level` with `drop` oldest turn units omitted.
    fn at_with(
        &self,
        (sid, budget, now): (&str, usize, u64),
        (level, drop): (u8, usize),
        counts: bool,
        shed: TxShed,
    ) -> (Brief, usize) {
        let scan = if shed == TxShed::Stripped {
            &self.stripped
        } else {
            &self.scan
        };
        let r = render(
            scan,
            self.gap,
            self.st,
            self.links,
            (level, drop, shed),
            counts,
            now,
        );
        let (text, used) = assemble(sid, self.st, budget, &r.lines);
        (Brief { text, used, level }, r.units)
    }

    /// Render [`Self::at_with`] without count lines or transcript records.
    fn at(&self, sid: &str, budget: usize, now: u64, level: u8, drop: usize) -> (Brief, usize) {
        self.at_with((sid, budget, now), (level, drop), false, TxShed::Stripped)
    }

    /// Fit the brief of session `sid` within `budget` tokens at `now_wall`.
    #[must_use]
    pub fn fit(&self, sid: &str, budget: usize, now_wall: u64) -> Brief {
        let whole = (sid, budget, now_wall);
        if self.scan.counts_any(&self.st.totals) {
            let (b, _) = self.at_with(whole, (0, 0), true, TxShed::Full);
            if b.used <= budget {
                return b;
            }
        }
        if self.transcript {
            for shed in [TxShed::Full, TxShed::Collapsed] {
                let (b, _) = self.at_with(whole, (0, 0), false, shed);
                if b.used <= budget {
                    return b;
                }
            }
        }
        let mut units = 0;
        for level in 0..=3u8 {
            let (b, n) = self.at(sid, budget, now_wall, level, 0);
            if b.used <= budget {
                return b;
            }
            units = n;
        }
        if units > 1 {
            let (most, _) = self.at(sid, budget, now_wall, 3, units - 1);
            if most.used <= budget {
                let (mut lo, mut hi, mut best) = (1, units - 1, most);
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let (b, _) = self.at(sid, budget, now_wall, 3, mid);
                    if b.used <= budget {
                        best = b;
                        hi = mid;
                    } else {
                        lo = mid + 1;
                    }
                }
                return best;
            }
        }
        self.at(sid, budget, now_wall, 4, 0).0
    }

    /// Render the one-line floor brief with `budget=0` in its header.
    #[must_use]
    pub fn floor(&self, sid: &str, now_wall: u64) -> Brief {
        self.at(sid, 0, now_wall, 4, 0).0
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Number of windows [`BriefInput::scanned`] has sorted on this thread.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn scans_on_this_thread() -> u64 {
    SCANS.with(std::cell::Cell::get)
}

/// Render the least-shed brief of session `sid` that fits `budget`, or the one-line floor.
pub fn brief<'a>(
    sid: &str,
    window: impl IntoIterator<Item = &'a Rec>,
    gap: Option<Gap>,
    st: &'a SessionState,
    links: &'a [(Node, MailLink)],
    budget: usize,
    now_wall: u64,
) -> Brief {
    BriefInput::new(window, gap, st, links)
        .scanned()
        .fit(sid, budget, now_wall)
}

impl crate::log::Log {
    /// Render [`brief`] over this log's whole window and fold.
    #[must_use]
    pub fn brief(&self, sid: &str, budget: usize, now_wall: u64) -> Brief {
        let read = self.read(Cursor::new(self.ep(), 0), usize::MAX);
        brief(
            sid,
            read.recs,
            read.gap,
            self.reader_state(),
            self.reader_fold().mail_links(),
            budget,
            now_wall,
        )
    }
}
