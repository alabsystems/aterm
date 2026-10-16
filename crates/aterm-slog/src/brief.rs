// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `brief` (design §7.4): a token-budgeted rollup for AI readers, a pure
//! function of the log window, the fold state, the budget and `now_wall`.
//!
//! ```text
//! OK 8 brief sid=s-7bd6 v=2 as_of=4123 ep=2 budget=600 used=412
//! - claude r3 3h12m · need=approval:bash 40s [screen+]
//! t17 open 4m10s by=harness · p204 approval bash open 40s [screen+]
//! t16 done 6m02s · 23 steps · appr 3 [screen+]
//! t1-t14 14 turns: 13 done 1 interrupted · 310 steps
//! mail in 2 · out 1
//! claim verdict on-track by=s-ops 12m ago [?]
//! more=t16,t1-t14
//! ```
//!
//! Tokens are estimated as bytes ÷ 4 ([`tokens`]). When the whole does not
//! fit, it sheds in this order: (1) step lines of older turns; (2) runs of
//! ordinary turns, collapsed into ranges; (3) marks other than needs and
//! claims, then the oldest turn lines, newest kept; (4) finally, the session
//! down to one line. Every line after the header starts with a node id (a
//! mark's id is its seq) until the trailer: `need` lines for the needs the
//! session line does not name, `mail`, then one `claim` line per live claim
//! (a verdict, a label, a need raised by a mark). `more=` names what was
//! left out. A suffix gives the source's reliability: `!` exact, `+`
//! strong, `~` heuristic, `?` claim; a phase nothing has observed yet has
//! none (`- session · -`).
//!
//! A `gap why=<w> lost=<n> resync_to=<seq>` line before `more=` says the
//! window does not reach back to where the reader stands: the caller passes
//! the [`Gap`] its read crossed ([`Log::read`]), never one inferred from the
//! window's first seq, so a window narrowed to a node or past a cursor
//! reports no loss that did not happen.
//!
//! The open turn and step come from the fold when the ring has evicted
//! their open records: the line the reader most needs is never lost to
//! eviction.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use aterm_codec::pct;

use crate::field::{Fields, Key, Tier, Val};
use crate::fold::{Need, OpenNode, SessionState};
use crate::log::{Cursor, Gap};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, Op, Src};

/// The default budget for one session (§7.4).
pub const DEFAULT_BUDGET: usize = 600;
/// The default budget for the instance-wide form.
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
    /// The shedding level it took (0: nothing shed … 3: marks and the
    /// oldest turn lines shed … 4: one line).
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
}

#[derive(Default)]
struct TurnInfo<'a> {
    open: Option<&'a Rec>,
    close: Option<&'a Rec>,
    amends: Vec<&'a Rec>,
    steps: BTreeMap<u32, StepInfo<'a>>,
}

impl TurnInfo<'_> {
    fn out(&self) -> Option<&'static str> {
        self.close.and_then(Rec::out)
    }

    /// A turn a range may absorb: closed `done`, `interrupted`, `asked` or
    /// `superseded`.
    fn ordinary(&self) -> bool {
        matches!(
            self.out(),
            Some("done" | "interrupted" | "asked" | "superseded")
        )
    }

    fn steps_total(&self) -> u64 {
        self.close
            .and_then(|c| c.f.get_u(Key::Steps))
            .unwrap_or(self.steps.len() as u64)
    }
}

/// The window, sorted into turns, their steps and the marks worth a line.
struct Scan<'a> {
    turns: BTreeMap<u32, TurnInfo<'a>>,
    marks: Vec<&'a Rec>,
}

/// Marks a brief never gives a line: the raw signals the tree is built from,
/// the content tier, and needs and claims (shown elsewhere).
fn mark_worth_a_line(k: Kind) -> bool {
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

/// The fold's open turn and step whose open records the window no longer
/// holds, as records the scan reads after the window.
fn open_path_seeds(st: &SessionState, window: &[&Rec]) -> Vec<Rec> {
    [st.turn(), st.step()]
        .into_iter()
        .flatten()
        .filter(|o| !window.iter().any(|r| r.op == Op::Open && r.node == o.node))
        .map(seed_of)
        .collect()
}

fn scan<'a>(window: &[&'a Rec], seeds: &'a [Rec]) -> Scan<'a> {
    let mut turns: BTreeMap<u32, TurnInfo<'a>> = BTreeMap::new();
    let mut step_turn: BTreeMap<u32, u32> = BTreeMap::new();
    let mut marks = Vec::new();
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
            (Op::Mark, _) if mark_worth_a_line(r.k) => marks.push(r),
            _ => {}
        }
    }
    turns.retain(|_, t| t.open.is_some() || t.close.is_some());
    Scan { turns, marks }
}

/// The needs the brief names as observed: every one but a claim (a need
/// raised by a mark), most urgent first.
fn observed_needs(st: &SessionState) -> impl Iterator<Item = &Need> {
    st.needs.iter().flatten().filter(|n| n.conf != Conf::Claim)
}

/// The needs raised by a mark: claims, each on a `claim need` line.
fn claimed_needs(st: &SessionState) -> impl Iterator<Item = &Need> {
    st.needs.iter().flatten().filter(|n| n.conf == Conf::Claim)
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
        // A phase nothing has observed yet says so by carrying no source:
        // `?` is a claim's, and no one claimed anything.
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
                c.out().unwrap_or("unknown"),
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

fn turn_line(n: u32, t: &TurnInfo<'_>, now: u64) -> String {
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
                c.out().unwrap_or("unknown"),
                dur(c.f.get_u(Key::DurMs).unwrap_or(0))
            );
        }
    }
    if by != Actor::Nobody {
        let _ = write!(line, " by={}", by.word());
    }
    if let Some(o) = t.open {
        for key in [Key::Ledger, Key::Cause, Key::Cont] {
            match o.f.get(key) {
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
    if t.close.is_none() {
        for (sn, s) in &t.steps {
            if s.close.is_none()
                && let Some(o) = s.open
            {
                let _ = write!(line, " · p{sn} {}", o.k.word());
                if let Some(w) = step_word(o) {
                    let _ = write!(line, " {w}");
                }
                let _ = write!(line, " open {}", dur(now.saturating_sub(o.t)));
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
            if let Some(tools) = c.f.get_str(Key::Tools) {
                let _ = write!(line, " {}", tools.replace([':', ','], " "));
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

/// A rendering: the body lines, and how many turn units (a turn with its
/// step lines, or a range of turns) it could shed oldest first.
struct Rendered {
    lines: Vec<String>,
    units: usize,
}

/// What `more=` names: the turns left out (by number), and how many marks,
/// need lines and claim lines were.
#[derive(Default)]
struct More {
    turns: std::collections::BTreeSet<u32>,
    marks: usize,
    needs: usize,
    claims: usize,
}

/// The trailer's need and claim lines: every observed need but the one the
/// session line names, then the live claims (verdict, label, needs raised
/// by a mark).
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
    if let Some(v) = &st.verdict
        && let Some(w) = v.word
    {
        claims.push(format!(
            "claim verdict {w} by={} {} ago [?]",
            v.by.word(),
            ago(v.t)
        ));
    }
    if let Some(l) = &st.label
        && l.until.is_some_and(|u| u > now)
        && let Some(text) = &l.text
    {
        claims.push(format!(
            "claim label {} by={} {} ago [?]",
            pct::encode(text),
            l.by.word(),
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

/// The `gap` line for the gap a read crossed.
fn gap_line(g: &Gap) -> String {
    format!(
        "gap why={} lost={} resync_to={}",
        g.why.word(),
        g.lost,
        g.resync_to
    )
}

/// Render at shedding `level`, dropping the `drop` oldest turn units at
/// level 3 (the newest is always kept): the body lines and the `more=`
/// items.
fn render(
    scan: &Scan<'_>,
    gap: Option<&Gap>,
    st: &SessionState,
    level: u8,
    drop: usize,
    now: u64,
) -> Rendered {
    let mut lines = vec![session_line(st, now)];
    let mut more = More::default();
    // Newest first.
    let turns: Vec<(u32, &TurnInfo<'_>)> = scan.turns.iter().rev().map(|(n, t)| (*n, t)).collect();
    let newest = turns.first().map(|t| t.0);
    let (needs, claims) = trailer(st, now);
    let mut units_len = 0;
    if level < 4 {
        // Each unit: its lines and the turns it covers.
        let mut units: Vec<(Vec<String>, Vec<u32>)> = Vec::new();
        let mut i = 0;
        while i < turns.len() {
            let (n, t) = turns[i];
            // Level 2 collapses runs of ordinary closed turns, keeping the
            // newest turn whole.
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
            let mut unit = vec![turn_line(n, t, now)];
            let detail = level == 0 || Some(n) == newest;
            if detail {
                for (sn, s) in &t.steps {
                    // An open turn's open steps are already on its line.
                    if t.close.is_none() && s.close.is_none() {
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
        // Past everything else, level 3 sheds the oldest turn units, the
        // newest kept, each named in `more=`.
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
        if t.mail_in > 0 || t.mail_out > 0 {
            lines.push(format!("mail in {} · out {}", t.mail_in, t.mail_out));
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

/// `more=`: the turns left out, newest first, a run of three or more as a
/// range (`more=t17,t16,t1-t14`), then `marks:<n>`, `needs:<n>` and
/// `claims:<n>`; `more=-` when nothing was.
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

/// Assemble the reply: the header's `used=` is the whole reply's token
/// estimate, header included.
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

/// The brief of session `sid` (§7.4): the least-shed rendering whose token
/// estimate fits `budget`, or the one-line form when none does. At level 3
/// the oldest turn lines go one unit at a time (the fewest that fit), so the
/// brief reaches the one-line form only when even the session line and the
/// newest turn do not fit.
///
/// `window` is the records the reader asked for (the whole ring, one node's
/// subtree, or those past its cursor) and `gap` the gap that read crossed
/// ([`crate::log::Read::gap`]): the one `gap` line says exactly that, never
/// a loss inferred from where a narrowed window starts.
pub fn brief<'a>(
    sid: &str,
    window: impl IntoIterator<Item = &'a Rec>,
    gap: Option<Gap>,
    st: &SessionState,
    budget: usize,
    now_wall: u64,
) -> Brief {
    let window: Vec<&Rec> = window.into_iter().collect();
    let seeds = open_path_seeds(st, &window);
    let scan = scan(&window, &seeds);
    let at = |level: u8, drop: usize| {
        let r = render(&scan, gap.as_ref(), st, level, drop, now_wall);
        let (text, used) = assemble(sid, st, budget, &r.lines);
        (Brief { text, used, level }, r.units)
    };
    let mut units = 0;
    for level in 0..=3u8 {
        let (b, n) = at(level, 0);
        if b.used <= budget {
            return b;
        }
        units = n;
    }
    // Level 3, shedding the oldest turn units: the fewest drops that fit
    // (fewer units only ever shorten the body).
    if units > 1 {
        let (most, _) = at(3, units - 1);
        if most.used <= budget {
            let (mut lo, mut hi, mut best) = (1, units - 1, most);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let (b, _) = at(3, mid);
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
    at(4, 0).0
}

impl crate::log::Log {
    /// [`brief`] over this log's whole window and fold: its gap is the ring's
    /// own eviction (a reader that holds nothing, `since=0`).
    #[must_use]
    pub fn brief(&self, sid: &str, budget: usize, now_wall: u64) -> Brief {
        let read = self.read(Cursor::new(self.ep(), 0), usize::MAX);
        brief(sid, read.recs, read.gap, self.state(), budget, now_wall)
    }
}
