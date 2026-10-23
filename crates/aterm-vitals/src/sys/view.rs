// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each `sys` view as text lines and as JSON.

use super::disk::{self, RootSize};
use super::jobs_view;
use super::json::Jv;
use super::query::{ScopeArg, SysQuery, View};
use super::why;
use super::{Ctx, Freshness, clock_ms, exe_word, pct};
use crate::events::{EventKind, LoadScope, millicores};
use crate::model::{ProcView, RowScope, ScopeView, Snapshot};
use crate::words;

/// One body line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Line key compared by a delta.
    pub key: String,
    /// The line as printed.
    pub text: String,
    /// Whether it states a value a delta compares.
    pub value: bool,
}

/// A reply before its header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The view.
    pub view: View,
    /// The scope read, for the views that take one.
    pub scope: Option<ScopeArg>,
    /// Subject of a `why` reply, or `None` for every other view.
    pub subject: Option<&'static str>,
    /// Body lines.
    pub lines: Vec<Line>,
    /// The JSON body, when the request asked for it.
    pub json: Option<Jv>,
}

impl Reply {
    /// Format the reply header line without a trailing newline.
    #[must_use]
    pub fn header(&self, ctx: &Ctx<'_>, delta: bool) -> String {
        let n = if self.json.is_some() {
            1
        } else {
            self.lines.len()
        };
        header_line(self.view, self.scope, self.subject, n, ctx, delta)
    }
}

/// [`Reply::header`] of a reply of `n` lines.
pub(super) fn header_line(
    view: View,
    scope: Option<ScopeArg>,
    subject: Option<&str>,
    n: usize,
    ctx: &Ctx<'_>,
    delta: bool,
) -> String {
    let mut h = format!("OK {n} sys v=1 view={}", view.word());
    if let Some(s) = scope {
        h.push_str(&format!(" scope={}", s.word()));
    }
    if let Some(s) = subject {
        h.push_str(&format!(" subject={s}"));
    }
    match as_of(view, ctx) {
        Some((unix_ms, age_ms, window)) => {
            h.push_str(&format!(
                " as_of={} age_ms={age_ms} window_ms={}",
                clock_ms(unix_ms),
                window.map_or_else(|| "-".to_owned(), |w| w.to_string())
            ));
        }
        None => h.push_str(" as_of=- age_ms=- window_ms=-"),
    }
    h.push_str(&format!(" next={}", ctx.cursor()));
    if ctx.state != Freshness::Fresh {
        h.push_str(&format!(" state={}", ctx.state.word()));
    }
    if delta {
        h.push_str(" delta=1");
    }
    h
}

fn as_of(view: View, ctx: &Ctx<'_>) -> Option<(u64, u64, Option<u64>)> {
    let snap = ctx.snap?;
    match snap.why.as_deref().filter(|_| view == View::Why) {
        Some(f) => {
            let older = snap.at_ns.saturating_sub(f.at_ns) / 1_000_000;
            Some((
                ctx.as_of_unix_ms.saturating_sub(older),
                ctx.age_ms.saturating_add(older),
                Some(f.window_ns / 1_000_000),
            ))
        }
        None => Some((ctx.as_of_unix_ms, ctx.age_ms, window_ms(snap))),
    }
}

/// Rate window covered by a snapshot's figures, in milliseconds.
#[must_use]
pub fn window_ms(s: &Snapshot) -> Option<u64> {
    s.procs_window_ns
        .or(s.cores_window_ns)
        .map(|ns| ns / 1_000_000)
}

/// A scope's figures, summed over its sessions where it has several.
#[derive(Clone, Debug, Default)]
struct Fig {
    present: bool,
    cores: f32,
    footprint: Option<u64>,
    procs: u32,
    wait: Option<f32>,
    write_bps: Option<f64>,
    refused: u32,
    program: String,
}

impl Fig {
    fn of(sv: &ScopeView) -> Self {
        Self {
            present: true,
            cores: sv.cores.raw.max(0.0),
            footprint: sv.footprint,
            procs: sv.procs,
            wait: sv.wait,
            write_bps: sv.write_bps,
            refused: sv.refused,
            program: String::new(),
        }
    }

    fn add(&mut self, o: &Self) {
        if !o.present {
            return;
        }
        if !self.present {
            *self = o.clone();
            return;
        }
        self.cores += o.cores;
        self.footprint = match (self.footprint, o.footprint) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (a, b) => a.or(b),
        };
        self.procs += o.procs;
        self.wait = match (self.wait, o.wait) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.write_bps = match (self.write_bps, o.write_bps) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.refused += o.refused;
        if self.program.is_empty() {
            self.program.clone_from(&o.program);
        }
    }

    fn starved(&self) -> Option<f32> {
        let w = self.wait?;
        let d = w + self.cores;
        Some(if d > 1e-3 {
            (w / d).clamp(0.0, 1.0)
        } else {
            0.0
        })
    }
}

/// The sessions a scope word covers for the target.
pub(super) fn scope_keys(ctx: &Ctx<'_>, scope: ScopeArg) -> Vec<u64> {
    match scope {
        ScopeArg::Pane => vec![ctx.target.key],
        ScopeArg::Tab => {
            if ctx.target.panes.is_empty() {
                vec![ctx.target.key]
            } else {
                ctx.target.panes.clone()
            }
        }
        ScopeArg::Window => {
            if ctx.target.window_sessions.is_empty() {
                vec![ctx.target.key]
            } else {
                ctx.target.window_sessions.clone()
            }
        }
        ScopeArg::Aterm | ScopeArg::Global => ctx.tabs.iter().map(|t| t.key).collect(),
    }
}

fn tabs_fig(s: &Snapshot, keys: &[u64], target: u64) -> Fig {
    let mut f = Fig::default();
    let mut ordered: Vec<u64> = keys.to_vec();
    ordered.sort_by_key(|k| u8::from(*k != target));
    for k in ordered {
        if let Some(t) = s.tab(k) {
            let mut one = Fig::of(&t.scope);
            one.program.clone_from(&t.program);
            f.add(&one);
        }
    }
    f
}

fn c1(x: f32) -> String {
    format!("{}c", words::cores(x))
}

/// Cores-worth to two decimals with `c`: `4.13c`.
pub(super) fn c2(x: f32) -> String {
    format!("{:.2}c", x.max(0.0))
}

fn mc(x: f32) -> u64 {
    u64::from(millicores(x))
}

fn mc_opt(x: Option<f32>) -> Option<u64> {
    x.map(mc)
}

fn bps_word(b: f64) -> String {
    let b = b.max(0.0);
    if b >= 1e9 {
        format!("{:.1} GB/s", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB/s", b / 1e3)
    } else {
        format!("{b:.0} B/s")
    }
}

/// `unavailable(<word>)`.
pub(super) fn unavailable(word: &str) -> String {
    format!("unavailable({word})")
}

/// A record value or `unavailable(<why>)`.
pub(super) fn rec<T: std::fmt::Display>(v: Option<T>, why: &str) -> String {
    v.map_or_else(|| unavailable(why), |v| v.to_string())
}

fn global_clauses(ctx: &Ctx<'_>, s: &Snapshot) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(cpu) = s.cpu.as_ref() {
        let motion = cpu
            .busy
            .motion(words::CORES_GATE)
            .word()
            .map(|w| format!(", {w}"))
            .unwrap_or_default();
        parts.push(format!(
            "{} of {} cores busy{motion}",
            words::cores(cpu.busy.raw),
            cpu.total()
        ));
    } else {
        parts.push("cores measuring (the first sample has no interval)".into());
    }
    if let Some(t) = s.thermal {
        parts.push(if t.level == crate::model::Throttle::None {
            "throttle none".into()
        } else {
            format!("throttled: {}", t.level.word())
        });
    }
    if let Some(w) = s.global.wait.filter(|w| *w >= 0.1) {
        parts.push(format!("≥{} waiting for a core (own processes)", c1(w)));
    }
    if let Some(g) = s.gpu.as_ref() {
        parts.push(format!("GPU {:.0}%", (g.load.raw * 100.0).round()));
    }
    if let Some(m) = s.mem.as_ref() {
        parts.extend(mem_clauses(m));
    }
    if let Some(v) = s.volume.as_ref() {
        parts.push(format!("disk {} free", words::disk(v.available)));
    }
    let _ = ctx;
    parts
}

/// Memory's words for a sentence: `memory 16.6 of 128 GiB used, pressure
/// normal` and `swap 0` (with `, swapping <rate>` while it moves).
pub(super) fn mem_clauses(m: &crate::model::MemView) -> [String; 2] {
    let mut w = format!(
        "memory {} of {} GiB used",
        words::gib_figure(m.sample.used()),
        words::gib_total(m.sample.total)
    );
    if m.sample.pressure != crate::model::Pressure::Unknown {
        w.push_str(&format!(", pressure {}", m.sample.pressure.word()));
    }
    let mut sw = if m.sample.swap_used == 0 {
        "swap 0".to_owned()
    } else {
        format!("swap {}", words::mem(m.sample.swap_used))
    };
    if let Some(r) = m
        .rates
        .filter(|r| r.swap_bps() >= crate::events::SWAP_ACTIVE_BPS)
    {
        sw.push_str(&format!(", swapping {}", bps_word(r.swap_bps())));
    }
    [w, sw]
}

fn aterm_clauses(s: &Snapshot) -> Vec<String> {
    let Some(a) = s.aterm.as_ref() else {
        return vec!["measuring (one process sweep has no interval)".into()];
    };
    let mut first = format!("{} cores", words::cores(a.cores.raw));
    if let Some(busy) = s.cpu.as_ref().map(|c| c.busy.raw).filter(|b| *b > 0.05) {
        let share = (a.cores.raw / busy * 100.0).round().clamp(0.0, 100.0);
        first.push_str(&format!(", {share:.0}% of busy"));
    }
    let mut parts = vec![first];
    if let Some(f) = a.footprint {
        parts.push(words::mem(f));
    }
    parts.push(format!(
        "{} process{}",
        a.procs,
        if a.procs == 1 { "" } else { "es" }
    ));
    parts
}

fn tab_clauses(f: &Fig) -> Vec<String> {
    if !f.present {
        return vec!["measuring (one process sweep has no interval)".into()];
    }
    let mut parts = Vec::new();
    if !f.program.is_empty() {
        parts.push(format!("program {}", exe_word(&f.program)));
    }
    parts.push(format!("{} cores", words::cores(f.cores)));
    if let Some(b) = f.footprint {
        parts.push(words::mem(b));
    }
    if let Some(st) = f.starved().filter(|s| *s >= 0.25) {
        parts.push(format!("waiting for a core {:.0}%", st * 100.0));
    }
    parts
}

fn tab_label(ctx: &Ctx<'_>) -> String {
    match ctx.target.tab {
        Some(n) => format!("tab {n}"),
        None => "tab -".into(),
    }
}

fn sentence(key: &str, label: &str, clauses: &[String]) -> Line {
    Line {
        key: key.into(),
        text: format!("{label:<8}{}", clauses.join("; ")),
        value: true,
    }
}

fn line(key: impl Into<String>, text: impl Into<String>) -> Line {
    Line {
        key: key.into(),
        text: text.into(),
        value: true,
    }
}

fn note(key: &str, text: impl Into<String>) -> Line {
    Line {
        key: key.into(),
        text: text.into(),
        value: false,
    }
}

fn rest_line(ctx: &Ctx<'_>, s: &Snapshot, tab: &Fig) -> Option<Line> {
    let Some(sp) = s.split else {
        return s.procs_window_ns.map(|_| {
            line(
                "rest",
                format!("rest    split {}", unavailable("not_sampled")),
            )
        });
    };
    let rest_of_aterm = (sp.aterm - tab.cores).max(0.0);
    let mut text = format!(
        "rest    tab {} · rest of aterm {} · others {} · {} ≈{}",
        c1(tab.cores),
        c1(rest_of_aterm),
        c1(sp.others),
        ctx.machine.services_word(),
        c1(sp.services)
    );
    if sp.unaccounted < -0.05 {
        text.push_str(&format!(" · unaccounted -{}", c1(-sp.unaccounted)));
    } else {
        text.push_str(" (by subtraction, unaccounted work included)");
    }
    if let Some(w) = window_ms(s) {
        text.push_str(&format!(" · window_ms={w}"));
    }
    Some(line("rest", text))
}

fn event_unix_ms(ctx: &Ctx<'_>, s: &Snapshot, at_ns: u64) -> u64 {
    match at_ns.checked_sub(s.at_ns) {
        Some(after) => ctx.as_of_unix_ms.saturating_add(after / 1_000_000),
        None => ctx
            .as_of_unix_ms
            .saturating_sub((s.at_ns - at_ns) / 1_000_000),
    }
}

fn event_shown(ctx: &Ctx<'_>, e: &crate::events::Event) -> bool {
    if ctx.owner_class {
        return true;
    }
    let own = |k: &u64| ctx.target.panes.contains(k);
    match &e.kind {
        EventKind::Load {
            scope: LoadScope::Tab(k),
            ..
        } => own(k),
        EventKind::Tab { key, .. } | EventKind::Orphan { key, .. } => own(key),
        EventKind::Job { tab, .. } => own(tab),
        _ => true,
    }
}

fn load_top_shown(ctx: &Ctx<'_>, scope: &LoadScope) -> bool {
    ctx.owner_class || matches!(scope, LoadScope::Tab(k) if ctx.target.panes.contains(k))
}

fn event_line(ctx: &Ctx<'_>, s: &Snapshot, e: &crate::events::Event) -> Line {
    let at = super::clock_s(event_unix_ms(ctx, s, e.at_ns));
    let mut t = format!("e seq={} at={at} kind={}", e.seq, e.kind.word());
    match &e.kind {
        EventKind::Pressure { from, to } => {
            t.push_str(&format!(" from={} to={}", from.word(), to.word()));
        }
        EventKind::Throttle { from, to } => {
            t.push_str(&format!(" from={} to={}", from.word(), to.word()));
        }
        EventKind::Swap { started, bps } => {
            t.push_str(&format!(
                " to={} swap_bps={bps}",
                if *started { "started" } else { "stopped" }
            ));
        }
        EventKind::Disk {
            mount,
            line,
            below,
            free,
        } => {
            t.push_str(&format!(
                " mount={} line={} to={} free_bytes={free}",
                pct(mount),
                line.word(),
                if *below { "below" } else { "above" }
            ));
        }
        EventKind::Load {
            scope,
            from_mc,
            to_mc,
            top,
        } => {
            let sc = match scope {
                LoadScope::Global => "scope=global".to_owned(),
                LoadScope::Aterm => "scope=aterm".to_owned(),
                LoadScope::Tab(k) => match ctx.tab_number(*k) {
                    Some(n) => format!("scope=tab tab={n}"),
                    None => format!("scope=tab local={k}"),
                },
            };
            t.push_str(&format!(" {sc} from_mc={from_mc} to_mc={to_mc}"));
            if let Some(top) = top.as_deref().filter(|_| load_top_shown(ctx, scope)) {
                t.push_str(&format!(" top={}", exe_word(top)));
            }
        }
        EventKind::Tab { key, opened, .. } => {
            t.push_str(&format!(
                " local={key} to={}",
                if *opened { "opened" } else { "closed" }
            ));
            if let Some(n) = ctx.tab_number(*key) {
                t.push_str(&format!(" tab={n}"));
            }
        }
        EventKind::Lane { lane, available } => {
            t.push_str(&format!(
                " lane={lane} to={}",
                if *available {
                    "available"
                } else {
                    "unavailable"
                }
            ));
        }
        EventKind::Orphan {
            key, procs, top, ..
        } => {
            t.push_str(&format!(" local={key} procs={procs}"));
            if let Some(top) = top {
                t.push_str(&format!(" top={}", exe_word(top)));
            }
        }
        EventKind::Job {
            tab,
            id,
            from,
            to,
            class,
            task,
            exit,
        } => {
            match ctx.tab_number(*tab) {
                Some(n) => t.push_str(&format!(" tab={n}")),
                None => t.push_str(&format!(" local={tab}")),
            }
            t.push_str(&format!(
                " job={} from={} to={} class={}",
                pct(id),
                from.map_or("new", |v| v.word()),
                to.word(),
                class.word()
            ));
            if let Some(task) = task.as_deref().filter(|_| ctx.target.panes.contains(tab)) {
                t.push_str(&format!(" task={}", exe_word(task)));
            }
            if let Some(code) = exit {
                t.push_str(&format!(" code={code}"));
            }
        }
    }
    note("e", t)
}

fn event_json(ctx: &Ctx<'_>, s: &Snapshot, e: &crate::events::Event) -> Jv {
    let mut o = Jv::obj([
        ("seq", Jv::u(e.seq)),
        ("at_unix_ms", Jv::u(event_unix_ms(ctx, s, e.at_ns))),
        ("kind", Jv::s(e.kind.word())),
    ]);
    match &e.kind {
        EventKind::Pressure { from, to } => {
            o.set("from", Jv::s(from.word()));
            o.set("to", Jv::s(to.word()));
        }
        EventKind::Throttle { from, to } => {
            o.set("from", Jv::s(from.word()));
            o.set("to", Jv::s(to.word()));
        }
        EventKind::Swap { started, bps } => {
            o.set("to", Jv::s(if *started { "started" } else { "stopped" }));
            o.set("swap_bytes_per_s", Jv::u(*bps));
        }
        EventKind::Disk {
            mount,
            line,
            below,
            free,
        } => {
            o.set("mount", Jv::s(mount.clone()));
            o.set("line", Jv::s(line.word()));
            o.set("to", Jv::s(if *below { "below" } else { "above" }));
            o.set("free_bytes", Jv::u(*free));
        }
        EventKind::Load {
            scope,
            from_mc,
            to_mc,
            top,
        } => {
            match scope {
                LoadScope::Global => o.set("scope", Jv::s("global")),
                LoadScope::Aterm => o.set("scope", Jv::s("aterm")),
                LoadScope::Tab(k) => {
                    o.set("scope", Jv::s("tab"));
                    o.set("local", Jv::u(*k));
                    if let Some(n) = ctx.tab_number(*k) {
                        o.set("tab", Jv::u(n as u64));
                    }
                }
            }
            o.set("from_mc", Jv::u(u64::from(*from_mc)));
            o.set("to_mc", Jv::u(u64::from(*to_mc)));
            if let Some(top) = top.as_deref().filter(|_| load_top_shown(ctx, scope)) {
                o.set("top", Jv::s(exe_word(top)));
            }
        }
        EventKind::Tab { key, opened, .. } => {
            o.set("local", Jv::u(*key));
            o.set("to", Jv::s(if *opened { "opened" } else { "closed" }));
            if let Some(n) = ctx.tab_number(*key) {
                o.set("tab", Jv::u(n as u64));
            }
        }
        EventKind::Lane { lane, available } => {
            o.set("lane", Jv::s(*lane));
            o.set(
                "to",
                Jv::s(if *available {
                    "available"
                } else {
                    "unavailable"
                }),
            );
        }
        EventKind::Orphan {
            key, procs, top, ..
        } => {
            o.set("local", Jv::u(*key));
            o.set("procs", Jv::u(u64::from(*procs)));
            if let Some(top) = top {
                o.set("top", Jv::s(exe_word(top)));
            }
        }
        EventKind::Job {
            tab,
            id,
            from,
            to,
            class,
            task,
            exit,
        } => {
            o.set("local", Jv::u(*tab));
            if let Some(n) = ctx.tab_number(*tab) {
                o.set("tab", Jv::u(n as u64));
            }
            o.set("job", Jv::s(id.clone()));
            o.set("from", Jv::s(from.map_or("new", |v| v.word())));
            o.set("to", Jv::s(to.word()));
            o.set("class", Jv::s(class.word()));
            if let Some(task) = task.as_deref().filter(|_| ctx.target.panes.contains(tab)) {
                o.set("task", Jv::s(exe_word(task)));
            }
            if let Some(code) = exit {
                o.set("code", Jv::Int(i64::from(*code)));
            }
        }
    }
    o
}

/// Render the reply's text lines for `q`.
#[must_use]
pub fn render_text(q: &SysQuery, ctx: &Ctx<'_>) -> Reply {
    let scope = q.effective_scope();
    let mut lines = Vec::new();
    if let Some(g) = ctx.gap {
        lines.push(note("gap", format!("gap why={}", g.word())));
    }
    let events: Vec<Line> = ctx.snap.map_or_else(Vec::new, |s| {
        ctx.events
            .iter()
            .filter(|e| event_shown(ctx, e))
            .map(|e| event_line(ctx, s, e))
            .collect()
    });
    match ctx.snap {
        None => lines.push(note(
            "state",
            "state  warming: the probe has not answered yet; every figure is unavailable(warming)",
        )),
        Some(s) => match q.view {
            View::Overview => overview_text(ctx, s, &mut lines),
            View::Tab => tab_text(ctx, s, &mut lines),
            View::Aterm => aterm_text(ctx, s, &mut lines),
            View::Global => global_text(q, ctx, s, &mut lines),
            View::Jobs => jobs_text(q, ctx, s, &mut lines),
            View::Cores => cores_text(ctx, s, &mut lines),
            View::Gpu => gpu_text(ctx, s, &mut lines),
            View::Mem => mem_text(q, ctx, s, &mut lines),
            View::Disk => disk_text(q, ctx, s, &mut lines),
            View::Procs => procs_text(q, ctx, s, &mut lines),
            View::Why => why::why_text(q, ctx, s, &mut lines, &events),
        },
    }
    lines.extend(events);
    Reply {
        view: q.view,
        scope: q.view.default_scope().and(scope),
        subject: why::subject_word(q, ctx.snap),
        lines,
        json: None,
    }
}

fn overview_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    lines.push(sentence("global", "global", &global_clauses(ctx, s)));
    lines.push(sentence("aterm", "aterm", &aterm_clauses(s)));
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
    let label = tab_label(ctx);
    lines.push(sentence(&label, &label, &tab_clauses(&tab)));
    if let Some(r) = rest_line(ctx, s, &tab) {
        lines.push(r);
    }
    lines.push(note(
        "more",
        "more=views:tab,aterm,global,jobs,cores,gpu,mem,disk,procs,why · aterm ctl @self sys --json",
    ));
}

fn exact_words() -> &'static str {
    "live processes (work that ended between samples is not counted)"
}

fn tab_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let f = tabs_fig(s, &keys, ctx.target.key);
    let mut head = format!("{} {}", tab_label(ctx), pct(&ctx.target.sid));
    if let Some(w) = ctx.target.window {
        head.push_str(&format!(" · window {w}"));
    }
    head.push_str(&format!(
        " · {} pane{}",
        keys.len(),
        if keys.len() == 1 { "" } else { "s" }
    ));
    if !f.program.is_empty() {
        head.push_str(&format!(" · program {}", exe_word(&f.program)));
    }
    head.push_str(&format!(" · {}", exact_words()));
    lines.push(line("tab", head));
    if !f.present {
        lines.push(line(
            "cpu",
            "cpu    measuring (one process sweep has no interval)",
        ));
    } else {
        let mut cpu = format!("cpu    {}", c2(f.cores));
        match f.starved() {
            Some(st) => cpu.push_str(&format!(
                " · waiting for a core {:.0}% ({})",
                st * 100.0,
                c2(f.wait.unwrap_or(0.0))
            )),
            None => cpu.push_str(&format!(
                " · waiting for a core {}",
                unavailable("not_sampled")
            )),
        }
        cpu.push_str(&format!(
            " · core type split {}",
            unavailable("not_sampled")
        ));
        lines.push(line("cpu", cpu));
        lines.push(line(
            "mem",
            format!(
                "mem    {} footprint ({} procs, {} unread) · slope {}",
                f.footprint
                    .map_or_else(|| unavailable("refused"), words::mem),
                f.procs,
                f.refused,
                unavailable("not_sampled")
            ),
        ));
    }
    lines.push(line(
        "gpu",
        format!(
            "gpu    per-tab GPU time {} · the device is {}",
            unavailable("lane_off:gpu_clients"),
            s.gpu.as_ref().map_or_else(
                || unavailable("unsupported"),
                |g| format!("{:.0}% busy", (g.load.raw * 100.0).round())
            )
        ),
    ));
    lines.push(line(
        "jobs",
        match s.jobs.as_deref() {
            Some(v) => format!(
                "jobs   {} · aterm ctl @self sys jobs",
                jobs_view::summary(&jobs_view::rows_in(v, &keys))
            ),
            None => format!("jobs   {}", unavailable(jobs_view::missing_why(s))),
        },
    ));
    let roots = ctx.roots.len();
    lines.push(line(
        "disk",
        format!(
            "disk   {roots} root{}{} · {} · writes {} · aterm ctl @self sys disk",
            if roots == 1 { "" } else { "s" },
            root_kinds_words(ctx),
            disk::totals_clause(ctx.disk, bps_word),
            f.write_bps
                .map_or_else(|| unavailable("not_sampled"), bps_word),
        ),
    ));
    lines.push(line(
        "procs",
        format!(
            "procs  {} · aterm ctl @self sys procs",
            proc_census(s, &keys)
        ),
    ));
}

fn root_kinds_words(ctx: &Ctx<'_>) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for k in ctx.roots.iter().flat_map(|r| r.kinds.iter()) {
        if !kinds.contains(k) {
            kinds.push(k);
        }
    }
    if kinds.is_empty() {
        String::new()
    } else {
        format!(": {}", kinds.join(", "))
    }
}

fn proc_census(s: &Snapshot, keys: &[u64]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for r in s
        .procs
        .iter()
        .filter(|r| matches!(r.scope, RowScope::Tab(k) if keys.contains(&k)))
    {
        let w = exe_word(&r.name).to_owned();
        match counts.iter_mut().find(|(n, _)| *n == w) {
            Some(slot) => slot.1 += 1,
            None => counts.push((w, 1)),
        }
    }
    if counts.is_empty() {
        return "none read".into();
    }
    let shown = counts.len().min(6);
    let rest: usize = counts[shown..].iter().map(|(_, n)| n).sum();
    let mut parts: Vec<String> = counts[..shown]
        .iter()
        .map(|(n, c)| format!("{n} {c}"))
        .collect();
    if rest > 0 {
        parts.push(format!("other {rest}"));
    }
    parts.join(" · ")
}

fn aterm_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    lines.push(line(
        "aterm",
        format!(
            "aterm   instance {} · {} session{} · {} · coalitions {}",
            s.self_pid,
            ctx.tabs.len(),
            if ctx.tabs.len() == 1 { "" } else { "s" },
            exact_words(),
            unavailable("lane_off:coalitions")
        ),
    ));
    let Some(a) = s.aterm.as_ref() else {
        lines.push(line(
            "cpu",
            "cpu     measuring (one process sweep has no interval)",
        ));
        return;
    };
    let busy = s.cpu.as_ref().map(|c| c.busy.raw);
    let mut cpu = format!("cpu     {}", c2(a.cores.raw));
    if let Some(b) = busy {
        cpu.push_str(&format!(" of {} busy", c2(b)));
        if b > 0.05 {
            cpu.push_str(&format!(
                " ({:.0}%)",
                (a.cores.raw / b * 100.0).clamp(0.0, 100.0)
            ));
        }
    }
    cpu.push_str(&format!(" · {} live processes", a.procs));
    lines.push(line("cpu", cpu));
    let mut tab_sum = 0.0f32;
    for t in &s.tabs {
        tab_sum += t.scope.cores.raw.max(0.0);
        let tabn = ctx
            .tab_number(t.key)
            .map_or_else(|| "-".to_owned(), |n| n.to_string());
        let sid = ctx
            .tabs
            .iter()
            .find(|f| f.key == t.key)
            .map_or_else(|| "-".to_owned(), |f| pct(&f.sid));
        lines.push(line(
            format!("part tab {}", t.key),
            format!(
                "part name=tab tab={tabn} local={} sid={sid} program={} cpu_mc={} wait_mc={} fp_bytes={} procs={} exact=live",
                t.key,
                exe_word(&t.program),
                mc(t.scope.cores.raw),
                rec(mc_opt(t.scope.wait), "not_sampled"),
                rec(t.scope.footprint, "refused"),
                t.scope.procs
            ),
        ));
    }
    let me = s.procs.iter().find(|r| r.scope == RowScope::SelfProc);
    let self_cores = me.and_then(|r| r.cores).unwrap_or(0.0);
    lines.push(line(
        "part self",
        format!(
            "part name=self pid={} cpu_mc={} fp_bytes={}",
            s.self_pid,
            rec(me.and_then(|r| r.cores).map(mc), "warming"),
            rec(me.and_then(|r| r.footprint), "refused")
        ),
    ));
    let helpers: Vec<&ProcView> = s
        .procs
        .iter()
        .filter(|r| r.scope == RowScope::Helper)
        .collect();
    let helpers_cores = (a.cores.raw - tab_sum - self_cores).max(0.0);
    let mut h = format!(
        "part name=helpers cpu_mc={} procs={}",
        mc(helpers_cores),
        helpers.len()
    );
    if let Some(top) = helpers.first() {
        h.push_str(&format!(" top={}", exe_word(&top.name)));
    }
    lines.push(line("part helpers", h));
    lines.push(line(
        "part unattributed",
        format!(
            "part name=unattributed cpu_mc={}",
            unavailable("lane_off:coalitions")
        ),
    ));
    lines.push(line(
        "gpu",
        format!(
            "gpu     aterm's GPU time {}",
            unavailable("lane_off:gpu_clients")
        ),
    ));
    lines.push(line(
        "mem",
        format!(
            "mem     {} footprint (live members) · writes {}",
            a.footprint
                .map_or_else(|| unavailable("refused"), words::mem),
            a.write_bps
                .map_or_else(|| unavailable("not_sampled"), bps_word)
        ),
    ));
}

fn machine_line(ctx: &Ctx<'_>, s: &Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(c) = ctx.machine.cpu.as_ref() {
        parts.push(device_word(c));
    }
    if let Some(cpu) = s.cpu.as_ref() {
        let cl: Vec<String> = cpu
            .clusters
            .iter()
            .map(|c| format!("{} {}", c.cores.len(), device_word(&c.name)))
            .collect();
        parts.push(cl.join(" + "));
    }
    if let Some(g) = s.gpu.as_ref() {
        match g.cores {
            Some(n) => parts.push(format!("{n}-core GPU")),
            None => parts.push("GPU".into()),
        }
    }
    if let Some(m) = s.mem.as_ref() {
        parts.push(format!("{} GiB", words::gib_total(m.sample.total)));
    }
    if let Some(os) = ctx.machine.os.as_ref() {
        parts.push(device_word(os));
    }
    format!("machine  {}", parts.join(" · "))
}

fn throttle_words(s: &Snapshot) -> String {
    match s.thermal {
        Some(t) => {
            let mut w = if t.exact {
                format!("{} (thermal state {})", t.level.word(), t.raw)
            } else {
                format!(
                    "{} (estimate: thermal zones against their trip points, and the throttle counters)",
                    t.level.word()
                )
            };
            if let Some(lp) = t.low_power {
                w.push_str(&format!(
                    " · Low Power Mode {}",
                    if lp { "on" } else { "off" }
                ));
            }
            w
        }
        None => unavailable("unsupported"),
    }
}

fn global_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    lines.push(line("machine", machine_line(ctx, s)));
    match s.cpu.as_ref() {
        Some(cpu) => {
            let mut t = format!(
                "cpu      {} of {} cores busy",
                words::cores(cpu.busy.raw),
                cpu.total()
            );
            for c in &cpu.clusters {
                let busy: f32 = c.cores.iter().map(|&i| cpu.cores[i].load.raw).sum();
                t.push_str(&format!(
                    " · {} {} of {}",
                    c.name,
                    words::cores(busy),
                    c.cores.len()
                ));
            }
            t.push_str(&format!(
                " · requested MHz {}",
                unavailable("lane_off:ioreport")
            ));
            lines.push(line("cpu", t));
        }
        None => lines.push(line(
            "cpu",
            "cpu      measuring (the first sample has no interval)",
        )),
    }
    if let Some(sp) = s.split {
        let mut t = format!(
            "split    aterm {} · others {} · {} ≈{}",
            c1(sp.aterm),
            c1(sp.others),
            ctx.machine.services_word(),
            c1(sp.services)
        );
        if sp.unaccounted < -0.05 {
            t.push_str(&format!(" · unaccounted -{}", c1(-sp.unaccounted)));
        } else {
            t.push_str(" (by subtraction, unaccounted work included)");
        }
        t.push_str(" · live (parent chains)");
        lines.push(line("split", t));
    } else if s.procs_window_ns.is_some() {
        lines.push(line(
            "split",
            format!("split    {}", unavailable("not_sampled")),
        ));
    }
    let mut th = format!(
        "threads  waiting for a core {}",
        s.global
            .wait
            .map_or_else(|| unavailable("not_sampled"), |w| format!("≥{}", c2(w)))
    );
    th.push_str(&format!(
        " (own processes; {} other-user processes unread)",
        s.global.refused
    ));
    if let Some(l) = s.load {
        th.push_str(&format!(" · load {:.2} / {:.2} / {:.2}", l[0], l[1], l[2]));
    }
    lines.push(line("threads", th));
    lines.push(line("throttle", format!("throttle {}", throttle_words(s))));
    lines.push(line("gpu", format!("gpu      {}", gpu_words(s))));
    if let Some(m) = s.mem.as_ref() {
        lines.push(line(
            "mem",
            format!("mem      {}", mem_class_words(m, exact_partition(ctx))),
        ));
        if let Some(c) = m.sample.commit.as_ref() {
            lines.push(line("commit", format!("commit   {}", commit_words(c))));
        }
        lines.push(line("paging", format!("paging   {}", paging_words(m))));
    }
    if let Some(v) = s.volume.as_ref() {
        lines.push(line(
            "disk",
            format!(
                "disk     {} {} free of {}",
                pct(&v.mount),
                words::disk(v.available),
                words::disk(v.total)
            ),
        ));
    }
    top_rows(
        q,
        ctx,
        s,
        lines,
        |r| !r.scope.in_aterm() && r.cores.is_some_and(|c| c >= 0.1),
        "top",
    );
}

fn top_rows(
    q: &SysQuery,
    ctx: &Ctx<'_>,
    s: &Snapshot,
    lines: &mut Vec<Line>,
    keep: impl Fn(&ProcView) -> bool,
    key: &str,
) {
    let rows: Vec<&ProcView> = s.procs.iter().filter(|r| keep(r)).collect();
    if rows.is_empty() {
        return;
    }
    if !ctx.owner_class {
        lines.push(note(
            "withheld",
            format!(
                "withheld={} (machine-wide rows are the owner's)",
                rows.len()
            ),
        ));
        return;
    }
    for r in rows.iter().take(q.top) {
        lines.push(line(
            format!("{key} {}", r.pid),
            format!(
                "{key} pid={} exe={} scope={} cpu_mc={} fp_bytes={}",
                r.pid,
                exe_word(&r.name),
                r.scope.word(),
                proc_cpu(r),
                rec(r.footprint, "refused")
            ),
        ));
    }
}

fn procs_total(rows: &[&ProcView]) -> Option<f32> {
    let measured = rows.iter().any(|r| r.cores.is_some());
    let readable = rows.iter().any(|r| !r.refused);
    (measured || !readable).then(|| rows.iter().filter_map(|r| r.cores).sum())
}

fn proc_cpu(r: &ProcView) -> String {
    match r.cores {
        Some(c) => mc(c).to_string(),
        None if r.refused => unavailable("refused"),
        None => unavailable("warming"),
    }
}

fn gpu_words(s: &Snapshot) -> String {
    let Some(g) = s.gpu.as_ref() else {
        return unavailable("unsupported");
    };
    let mut t = format!("{:.0}% busy, whole percent", (g.load.raw * 100.0).round());
    if let (Some(r), Some(ti)) = (g.sample.renderer_pct, g.sample.tiler_pct) {
        t.push_str(&format!(" (renderer {r}, tiler {ti})"));
    }
    if let Some(b) = g.sample.in_use_bytes {
        t.push_str(&format!(" · {} in use", words::mem(b)));
    }
    t
}

/// Whether the memory classes partition physical memory exactly (macOS,
/// where the remainder is what the VM never manages), or leave memory no
/// class names (Linux, Windows; design §9.6).
pub(super) fn exact_partition(ctx: &Ctx<'_>) -> bool {
    ctx.machine.family == "macOS"
}

/// The classes, bytes; `None` for one the OS does not say (Windows'
/// compression store), never a false zero. The last is the remainder:
/// `not_in_vm` where the partition is exact, else `unnamed`.
pub(super) fn mem_classes(
    m: &crate::model::MemView,
    exact: bool,
) -> [(&'static str, Option<u64>); 6] {
    let x = &m.sample;
    let counted = x
        .app
        .saturating_add(x.wired)
        .saturating_add(x.compressed.unwrap_or(0))
        .saturating_add(x.cached)
        .saturating_add(x.free);
    [
        ("app", Some(x.app)),
        ("wired", Some(x.wired)),
        ("compressed", x.compressed),
        ("cache", Some(x.cached)),
        ("free", Some(x.free)),
        (
            if exact { "not_in_vm" } else { "unnamed" },
            Some(x.total.saturating_sub(counted)),
        ),
    ]
}

fn g2(b: u64) -> String {
    format!("{:.2}", b as f64 / f64::from(1u32 << 30))
}

fn class_g2(b: Option<u64>) -> String {
    b.map_or_else(|| unavailable("not_sampled"), g2)
}

fn mem_sum_words(m: &crate::model::MemView, exact: bool) -> String {
    let c = mem_classes(m, exact);
    format!(
        "used {} = app {} + wired {} + compressed {} · cache {} · free {} · {} {} GiB",
        g2(m.sample.used()),
        class_g2(c[0].1),
        class_g2(c[1].1),
        class_g2(c[2].1),
        class_g2(c[3].1),
        class_g2(c[4].1),
        if exact { "not in VM" } else { "no class" },
        class_g2(c[5].1)
    )
}

fn mem_class_words(m: &crate::model::MemView, exact: bool) -> String {
    let mut t = m.sample.pressure.word().to_owned();
    if let Some(k) = m.sample.kernel_available_pct {
        t.push_str(&format!(" (kernel: {k}% available)"));
    }
    t.push_str(" · ");
    t.push_str(&mem_sum_words(m, exact));
    t
}

fn commit_words(c: &crate::model::CommitSample) -> String {
    let pct = c
        .pct()
        .map_or_else(|| unavailable("unsupported"), |p| format!("{p}%"));
    let how = if c.exact {
        "enforced: an allocation fails at the limit"
    } else {
        "advisory: the kernel overcommits"
    };
    format!("{} of {} GiB ({pct}) · {how}", g2(c.charge), g2(c.limit))
}

fn paging_why(m: &crate::model::MemView) -> &'static str {
    if m.sample.counters.is_some() {
        "warming"
    } else {
        "not_sampled"
    }
}

fn thrashing_why(m: &crate::model::MemView) -> &'static str {
    if m.sample.pressure.rank().is_none() {
        "unsupported"
    } else {
        paging_why(m)
    }
}

fn paging_words(m: &crate::model::MemView) -> String {
    let mut t = format!(
        "swap {} used of {}",
        words::mem(m.sample.swap_used),
        words::mem(m.sample.swap_total)
    );
    match m.rates {
        Some(r) => t.push_str(&format!(
            " · per s: swap in {}, swap out {}, decompressions {:.0}, compressions {:.0}, page-ins {:.0}",
            bps_word(r.swapin_bps),
            bps_word(r.swapout_bps),
            r.decompress_ps,
            r.compress_ps,
            r.pagein_ps
        )),
        None => t.push_str(&format!(" · rates {}", unavailable(paging_why(m)))),
    }
    t
}

fn jobs_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
    let keys = scope_keys(ctx, scope);
    let who = match scope {
        ScopeArg::Pane | ScopeArg::Tab => tab_label(ctx),
        ScopeArg::Window => format!(
            "window {}",
            ctx.target
                .window
                .map_or_else(|| "-".to_owned(), |w| w.to_string())
        ),
        ScopeArg::Aterm => "aterm".into(),
        ScopeArg::Global => "global".into(),
    };
    let Some(v) = s.jobs.as_deref() else {
        lines.push(line(
            "jobs",
            format!(
                "jobs  {who} · {}: the stuck engine has not read this scope yet",
                unavailable(jobs_view::missing_why(s))
            ),
        ));
        return;
    };
    let rows = if scope == ScopeArg::Global {
        jobs_view::rows_all(v)
    } else {
        jobs_view::rows_in(v, &keys)
    };
    lines.push(line(
        "jobs",
        format!("jobs  {who} · {}", jobs_view::summary(&rows)),
    ));
    let wide = !matches!(scope, ScopeArg::Pane | ScopeArg::Tab);
    let findings = jobs_view::findings_in(&v.findings, &keys, scope == ScopeArg::Global);
    lines.extend(jobs_view::row_lines(
        v,
        &rows,
        &findings,
        wide,
        q.top,
        &|k| ctx.tab_number(k),
    ));
    let (at, wait_on) = rows
        .iter()
        .find(|r| r.verdict != crate::jobs::Verdict::Exited)
        .map_or_else(
            || ("@self".to_owned(), "<id>".to_owned()),
            |r| {
                let at = if ctx.target.panes.contains(&r.tab) {
                    "@self".to_owned()
                } else {
                    ctx.tabs
                        .iter()
                        .find(|t| t.key == r.tab)
                        .map_or_else(|| "@self".to_owned(), |t| format!("@{}", t.sid))
                };
                (at, r.id.clone())
            },
        );
    let wider = match scope {
        ScopeArg::Pane | ScopeArg::Tab | ScopeArg::Window => " · sys jobs scope=aterm (owner)",
        ScopeArg::Aterm => " · sys jobs scope=global (owner)",
        ScopeArg::Global => "",
    };
    let left = jobs_view::overflow(rows.len(), q.top).map_or_else(String::new, |w| w + " · ");
    lines.push(note(
        "more",
        format!(
            "more={left}aterm ctl {at} await sys job={wait_on} done,stuck (wait for one){wider} · sys procs"
        ),
    ));
}

fn core_labels(cpu: &crate::model::CpuView) -> Vec<(usize, String, usize)> {
    let mut out = Vec::new();
    for (level, c) in cpu.clusters.iter().enumerate() {
        let letter = if c.short == "CPU" {
            "C"
        } else {
            c.short.as_str()
        };
        for (i, &core) in c.cores.iter().enumerate() {
            out.push((core, format!("{letter}{}", i + 1), level));
        }
    }
    out
}

fn cores_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let Some(cpu) = s.cpu.as_ref() else {
        lines.push(line(
            "cores",
            "cores  measuring (the first sample has no interval)",
        ));
        return;
    };
    let labels = core_labels(cpu);
    for (level, c) in cpu.clusters.iter().enumerate() {
        let busy: f32 = c.cores.iter().map(|&i| cpu.cores[i].load.raw).sum();
        lines.push(line(
            format!("cluster {}", c.name),
            format!(
                "cluster name={} level=l{level} cpus={} busy_mc={} req_mhz={} split_mc={} exact=exact",
                pct(&c.name),
                c.cores.len(),
                mc(busy),
                unavailable("lane_off:ioreport"),
                unavailable("not_sampled")
            ),
        ));
        for (core, label, _) in labels.iter().filter(|(_, _, l)| *l == level) {
            lines.push(line(
                format!("core {label}"),
                format!(
                    "core name={label} cpu={core} busy_pm={}",
                    (cpu.cores[*core].load.raw * 1000.0)
                        .round()
                        .clamp(0.0, 1000.0) as u32
                ),
            ));
        }
    }
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
    lines.push(line(
        "threads",
        format!(
            "threads runnable={} waiting_mc={} unread={} tab_waiting_mc={} tab_starved_pm={}",
            unavailable("not_sampled"),
            s.global
                .wait
                .map_or_else(|| unavailable("not_sampled"), |w| format!("≥{}", mc(w))),
            s.global.refused,
            rec(mc_opt(tab.wait), "not_sampled"),
            rec(
                tab.starved().map(|x| (x * 1000.0).round() as u32),
                "not_sampled"
            )
        ),
    ));
    lines.push(note(
        "note",
        if ctx.machine.family == "macOS" {
            "note   macOS does not say which core a process runs on without root: sys procs gives \
             how many cores' worth each process uses, not placement"
        } else {
            "note   placement (which core a process runs on) is not sampled: sys procs gives how \
             many cores' worth each process uses"
        },
    ));
    lines.push(line("throttle", format!("throttle {}", throttle_words(s))));
}

fn gpu_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let name = s
        .gpu
        .as_ref()
        .and_then(|g| g.name.clone())
        .or_else(|| ctx.machine.cpu.clone())
        .unwrap_or_else(|| "GPU".into());
    let cores = s
        .gpu
        .as_ref()
        .and_then(|g| g.cores)
        .map(|n| format!(", {n} cores"))
        .unwrap_or_default();
    lines.push(line(
        "gpu",
        format!(
            "gpu     {} GPU{cores}, reported as one device · {}",
            device_word(&name),
            gpu_words(s)
        ),
    ));
    lines.push(line(
        "split",
        format!(
            "split   per-scope GPU time {} · clients {}",
            unavailable("lane_off:gpu_clients"),
            unavailable("lane_off:gpu_clients")
        ),
    ));
    lines.push(note(
        "note",
        "note    the device reports one whole-percent figure; there is no per-core GPU use",
    ));
}

fn device_word(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '+' | '(' | ')'))
        .take(48)
        .collect();
    let kept = kept.trim();
    if kept.is_empty() {
        "-".into()
    } else {
        kept.into()
    }
}

fn mem_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let Some(m) = s.mem.as_ref() else {
        lines.push(line(
            "mem",
            format!("mem     {}", unavailable("unsupported")),
        ));
        return;
    };
    let thrash = match m.thrashing() {
        Some(true) => "yes".to_owned(),
        Some(false) => "no".to_owned(),
        None => unavailable(thrashing_why(m)),
    };
    let mut head = format!(
        "mem     {} GiB · pressure {}",
        words::gib_total(m.sample.total),
        m.sample.pressure.word()
    );
    if let Some(k) = m.sample.kernel_available_pct {
        head.push_str(&format!(" (kernel: {k}% available)"));
    }
    let exact = exact_partition(ctx);
    head.push_str(&format!(
        " · thrashing: {thrash} · {}",
        if exact {
            "classes add up to physical memory"
        } else {
            "the classes leave memory no class names"
        }
    ));
    lines.push(line("mem", head));
    lines.push(line(
        "classes",
        format!("classes {}", mem_sum_words(m, exact)),
    ));
    if let Some(c) = m.sample.commit.as_ref() {
        lines.push(line("commit", format!("commit  {}", commit_words(c))));
    }
    lines.push(line("paging", format!("paging  {}", paging_words(m))));
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
    let aterm_fp = s.aterm.as_ref().and_then(|a| a.footprint);
    let others_fp: u64 = s
        .procs
        .iter()
        .filter(|r| r.scope == RowScope::Others)
        .filter_map(|r| r.footprint)
        .sum();
    let mut owners = format!(
        "owners  {} {}",
        tab_label(ctx),
        tab.footprint
            .map_or_else(|| unavailable("refused"), words::mem)
    );
    if let (Some(a), Some(t)) = (aterm_fp, tab.footprint) {
        owners.push_str(&format!(
            " · rest of aterm {}",
            words::mem(a.saturating_sub(t))
        ));
    }
    if s.procs.iter().any(|r| r.scope == RowScope::Unknown) {
        owners.push_str(&format!(" · others {}", unavailable("not_sampled")));
    } else {
        owners.push_str(&format!(
            " · others ≥{} (the busiest and largest rows only)",
            words::mem(others_fp)
        ));
    }
    owners.push_str(" · footprints are shares, not a split of RAM");
    lines.push(line("owners", owners));
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let mut rows: Vec<&ProcView> = s.procs.iter().filter(|r| r.footprint.is_some()).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.footprint));
    let mut withheld = 0usize;
    let mut shown = 0usize;
    for r in rows {
        let own = matches!(r.scope, RowScope::Tab(k) if keys.contains(&k));
        if !own && !ctx.owner_class {
            withheld += 1;
            continue;
        }
        if shown >= q.top {
            continue;
        }
        shown += 1;
        let scope = match r.scope {
            RowScope::Tab(k) => ctx.tab_number(k).map_or_else(
                || format!("scope=tab local={k}"),
                |n| format!("scope=tab tab={n}"),
            ),
            other => format!("scope={}", other.word()),
        };
        lines.push(line(
            format!("top {}", r.pid),
            format!(
                "top pid={} exe={} {scope} fp_bytes={} peak_bytes={}",
                r.pid,
                exe_word(&r.name),
                rec(r.footprint, "refused"),
                rec(r.peak, "not_sampled")
            ),
        ));
    }
    if withheld > 0 {
        lines.push(note(
            "withheld",
            format!("withheld={withheld} (rows outside this tab are the owner's)"),
        ));
    }
}

fn disk_writers<'a>(ctx: &Ctx<'_>, s: &'a Snapshot, scope: ScopeArg) -> (Vec<&'a ProcView>, usize) {
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let mut withheld = 0usize;
    let mut writers: Vec<&ProcView> = procs_in_scope(ctx, s, scope)
        .into_iter()
        .filter(|r| r.write_bps.is_some_and(|b| b >= 1.0))
        .filter(|r| {
            let own = matches!(r.scope, RowScope::Tab(k) if keys.contains(&k));
            if own || ctx.owner_class {
                true
            } else {
                withheld += 1;
                false
            }
        })
        .collect();
    writers.sort_by(|a, b| {
        b.write_bps
            .unwrap_or(0.0)
            .total_cmp(&a.write_bps.unwrap_or(0.0))
    });
    (writers, withheld)
}

fn disk_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
    let keys = scope_keys(ctx, scope);
    let f = tabs_fig(s, &keys, ctx.target.key);
    let who = match scope {
        ScopeArg::Pane | ScopeArg::Tab => format!("{} {}", tab_label(ctx), pct(&ctx.target.sid)),
        ScopeArg::Window => format!(
            "window {}",
            ctx.target
                .window
                .map_or_else(|| "-".to_owned(), |w| w.to_string())
        ),
        ScopeArg::Aterm => "aterm".into(),
        ScopeArg::Global => "global".into(),
    };
    lines.push(line(
        "disk",
        format!(
            "disk {who} · {} root{} · {} · writes {} (storage I/O of the scope's processes)",
            ctx.roots.len(),
            if ctx.roots.len() == 1 { "" } else { "s" },
            disk::totals_clause(ctx.disk, bps_word),
            f.write_bps
                .map_or_else(|| unavailable("not_sampled"), bps_word)
        ),
    ));
    let depth = usize::from(q.depth.unwrap_or(DISK_DEPTH_DEFAULT));
    let mut dirs_left = q.top;
    for (i, r) in ctx.roots.iter().enumerate() {
        let n = i + 1;
        let path_ok = ctx.path_ok(&r.keys);
        let place = if path_ok {
            format!("path={}", pct(&r.path))
        } else {
            format!("under={}", r.under(ctx.home))
        };
        let mut rec = format!("root n={n} kinds={}", r.kinds.join(","));
        if let Some(t) = r.tab {
            rec.push_str(&format!(" tab={t}"));
        }
        rec.push(' ');
        rec.push_str(&place);
        if let Some(state) = r.size.state_word() {
            rec.push_str(&format!(" state={state}"));
        }
        rec.push_str(&format!(
            " alloc_bytes={} entries={} growth_bps={}",
            r.size.alloc_text(),
            r.size.entries_text(),
            r.size.growth_text()
        ));
        match &r.size {
            RootSize::Walking {
                walked_entries,
                last,
                ..
            } => {
                rec.push_str(&format!(" walked_entries={walked_entries}"));
                if let Some(l) = last {
                    rec.push_str(&format!(" last_alloc_bytes={}", l.alloc));
                }
            }
            RootSize::Complete(c) => {
                if let Some(age) = c.age_s {
                    rec.push_str(&format!(" walked_s={age}"));
                }
            }
            _ => {}
        }
        if let Some(inside) = r.inside {
            rec.push_str(&format!(" inside={inside}"));
        }
        if !r.shared_with.is_empty() {
            let tabs: Vec<String> = r.shared_with.iter().map(ToString::to_string).collect();
            rec.push_str(&format!(" shared_with={}", tabs.join(",")));
        }
        if let Some(t) = r.counted_in {
            rec.push_str(&format!(" counted_in=tab{t}"));
        }
        lines.push(line(format!("root {n}"), rec));
        for (rank, d) in r
            .dirs
            .iter()
            .filter(|d| d.depth() <= depth)
            .enumerate()
            .take(dirs_left)
        {
            dirs_left -= 1;
            let place = if path_ok {
                format!("path={}", pct(&d.rel))
            } else {
                format!("rank={}", rank + 1)
            };
            lines.push(line(
                format!("dir {n}.{}", rank + 1),
                format!(
                    "dir root={n} {place} alloc_bytes={} entries={}",
                    d.alloc, d.entries
                ),
            ));
        }
    }
    let (writers, withheld) = disk_writers(ctx, s, scope);
    for r in writers.iter().take(q.top) {
        lines.push(line(
            format!("writer {}", r.pid),
            format!(
                "writer pid={} exe={} write_bps={} kind=disk_io",
                r.pid,
                exe_word(&r.name),
                r.write_bps.unwrap_or(0.0).round() as u64
            ),
        ));
    }
    if withheld > 0 {
        lines.push(note(
            "withheld",
            format!("withheld={withheld} (writer rows outside this tab are the owner's)"),
        ));
    }
    match ctx.volume {
        Some(v) => lines.push(line(
            "volume",
            format!(
                "volume mount={} fs={} free_bytes={} available_bytes={} total_bytes={} level={} slope_bps={} purgeable_bytes={}",
                pct(&v.mount),
                pct(&v.fs),
                v.free,
                v.available,
                v.total,
                v.level.as_deref().map_or_else(|| unavailable("not_sampled"), str::to_owned),
                unavailable("not_sampled"),
                unavailable("not_sampled")
            ),
        )),
        None => lines.push(line(
            "volume",
            format!("volume {}", unavailable("not_sampled")),
        )),
    }
    lines.push(note("summary", disk_summary(ctx)));
}

/// Default subtree depth listed by `sys disk` under each root.
pub const DISK_DEPTH_DEFAULT: u8 = 3;

fn disk_summary(ctx: &Ctx<'_>) -> String {
    let Some(t) = ctx.disk else {
        return format!(
            "summary folder sizes, entry counts and growth are {} here (no disk walker measures them); free space and writers are measured",
            unavailable("not_sampled")
        );
    };
    let warnings = if t.warnings.is_empty() {
        "none".to_owned()
    } else {
        let n = t.warnings.len();
        let worst = t.warnings.iter().map(|w| w.entries).max().unwrap_or(0);
        format!(
            "{n} folder{} past 100,000 entries (largest {worst})",
            if n == 1 { "" } else { "s" }
        )
    };
    let not_measured: Vec<&str> = ctx
        .roots
        .iter()
        .filter_map(|r| match r.size {
            RootSize::NotMeasured(why) => Some(why),
            _ => None,
        })
        .collect();
    let not_measured = if not_measured.is_empty() {
        "none".to_owned()
    } else {
        not_measured.join(",")
    };
    let mut out = format!("summary warnings {warnings} · not measured {not_measured}");
    if t.walking {
        out.push_str(" · still walking: sizes are lower bounds");
    }
    out
}

fn procs_in_scope<'a>(ctx: &Ctx<'_>, s: &'a Snapshot, scope: ScopeArg) -> Vec<&'a ProcView> {
    let keys = scope_keys(ctx, scope);
    s.procs
        .iter()
        .filter(|r| match scope {
            ScopeArg::Aterm => r.scope.in_aterm(),
            ScopeArg::Global => true,
            _ => matches!(r.scope, RowScope::Tab(k) if keys.contains(&k)),
        })
        .collect()
}

fn proc_label(ctx: &Ctx<'_>, s: &Snapshot, r: &ProcView) -> &'static str {
    match r.scope {
        RowScope::Tab(k) => {
            if s.tab(k).is_some_and(|t| t.shell_pid == r.pid) {
                "root"
            } else if k == ctx.target.key && ctx.target.fg_pgid.is_some_and(|g| g == r.pgid) {
                "fg"
            } else {
                "job"
            }
        }
        other => other.word(),
    }
}

fn procs_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
    let rows = procs_in_scope(ctx, s, scope);
    let total = procs_total(&rows).map_or_else(|| unavailable("warming"), c2);
    let unread = rows.iter().filter(|r| r.refused).count();
    let who = match scope {
        ScopeArg::Pane | ScopeArg::Tab => tab_label(ctx),
        ScopeArg::Window => format!(
            "window {}",
            ctx.target
                .window
                .map_or_else(|| "-".to_owned(), |w| w.to_string())
        ),
        ScopeArg::Aterm => "aterm".into(),
        ScopeArg::Global => "global (the busiest and largest rows)".into(),
    };
    if s.procs_window_ns.is_none() {
        lines.push(line(
            "procs",
            format!(
                "procs  {who} · {}: one process sweep has no interval",
                unavailable("warming")
            ),
        ));
        return;
    }
    lines.push(line(
        "procs",
        format!(
            "procs  {who} · {} live ({unread} unread) · {total} · {}",
            rows.len(),
            exact_words()
        ),
    ));
    for r in rows.iter().take(q.top) {
        let mut t = format!(
            "proc pid={} exe={} label={} pgid={} cpu_mc={} wait_mc={} fp_bytes={} peak_bytes={} write_bps={} stat={}",
            r.pid,
            exe_word(&r.name),
            proc_label(ctx, s, r),
            r.pgid,
            proc_cpu(r),
            rec(
                mc_opt(r.wait),
                if r.refused { "refused" } else { "warming" }
            ),
            rec(r.footprint, "refused"),
            rec(r.peak, "refused"),
            rec(
                r.write_bps.map(|b| b.round() as u64),
                if r.refused { "refused" } else { "warming" }
            ),
            r.state.word()
        );
        if let RowScope::Tab(k) = r.scope
            && !matches!(scope, ScopeArg::Pane | ScopeArg::Tab)
            && let Some(n) = ctx.tab_number(k)
        {
            t.push_str(&format!(" tab={n}"));
        }
        lines.push(line(format!("proc {}", r.pid), t));
    }
    if rows.len() > q.top {
        lines.push(note(
            "more",
            format!(
                "more={} rows · top={}",
                rows.len() - q.top,
                rows.len().min(64)
            ),
        ));
    }
}

fn as_of_json(view: View, ctx: &Ctx<'_>) -> Jv {
    let (unix, age, window) = match as_of(view, ctx) {
        Some((unix, age, window)) => (
            Jv::u(unix),
            Jv::u(age),
            window.map_or_else(|| Jv::why("warming"), Jv::u),
        ),
        None => (Jv::why("warming"), Jv::why("warming"), Jv::why("warming")),
    };
    Jv::obj([
        ("unix_ms", unix),
        ("age_ms", age),
        ("window_ms", window),
        ("cursor", Jv::s(ctx.cursor().to_string())),
        ("state", Jv::s(ctx.state.word())),
    ])
}

fn machine_json(ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let mut o = Jv::obj([] as [(&str, Jv); 0]);
    if let Some(cpu) = s.cpu.as_ref() {
        o.set("cpus", Jv::u(cpu.total() as u64));
        o.set(
            "levels",
            Jv::Arr(
                cpu.clusters
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        Jv::obj([
                            ("id", Jv::s(format!("l{i}"))),
                            ("name", Jv::s(c.name.clone())),
                            ("cpus", Jv::u(c.cores.len() as u64)),
                        ])
                    })
                    .collect(),
            ),
        );
    }
    o.set(
        "gpu_cores",
        Jv::known_or(
            s.gpu.as_ref().and_then(|g| g.cores).map(u64::from),
            "unsupported",
        ),
    );
    if let Some(m) = s.mem.as_ref() {
        o.set("memory_bytes", Jv::u(m.sample.total));
    }
    if let (Some(cpu), Some(m)) = (s.cpu.as_ref(), s.mem.as_ref()) {
        o.set(
            "capacity",
            Jv::obj([
                ("cpu_mc", Jv::u(cpu.total() as u64 * 1000)),
                ("mem_bytes", Jv::u(m.sample.total)),
                ("source", Jv::s("host")),
            ]),
        );
    }
    if let Some(c) = ctx.machine.cpu.as_ref() {
        o.set("cpu_name", Jv::s(c.clone()));
    }
    if let Some(os) = ctx.machine.os.as_ref() {
        o.set("os", Jv::s(os.clone()));
    }
    o
}

fn global_json(s: &Snapshot) -> Jv {
    let mut o = Jv::obj([(
        "cpu_mc",
        s.cpu
            .as_ref()
            .map_or_else(|| Jv::why("warming"), |c| Jv::u(mc(c.busy.raw))),
    )]);
    if let Some(cpu) = s.cpu.as_ref() {
        o.set(
            "cpu_by_level_mc",
            Jv::Arr(
                cpu.clusters
                    .iter()
                    .map(|c| Jv::u(mc(c.cores.iter().map(|&i| cpu.cores[i].load.raw).sum())))
                    .collect(),
            ),
        );
    }
    o.set("runnable_threads", Jv::why("not_sampled"));
    o.set(
        "wait_mc",
        s.global.wait.map_or_else(
            || Jv::why("not_sampled"),
            |w| Jv::at_least_refused(mc(w), s.global.refused),
        ),
    );
    o.set(
        "load_m",
        s.load.map_or_else(
            || Jv::why("unsupported"),
            |l| {
                Jv::Arr(
                    l.iter()
                        .map(|v| Jv::u((*v * 1000.0).round() as u64))
                        .collect(),
                )
            },
        ),
    );
    o.set(
        "throttle",
        s.thermal
            .map_or_else(|| Jv::why("unsupported"), |t| Jv::s(t.level.word())),
    );
    o.set(
        "gpu_pm",
        s.gpu.as_ref().map_or_else(
            || Jv::why("unsupported"),
            |g| Jv::u((g.load.raw * 1000.0).round().max(0.0) as u64),
        ),
    );
    if let Some(m) = s.mem.as_ref() {
        o.set("mem_pressure", Jv::s(m.sample.pressure.word()));
        o.set("mem_used_bytes", Jv::u(m.sample.used()));
        o.set("swap_used_bytes", Jv::u(m.sample.swap_used));
    } else {
        o.set("mem_pressure", Jv::why("unsupported"));
    }
    o.set(
        "disk_free_bytes",
        Jv::known_or(s.volume.as_ref().map(|v| v.available), "not_sampled"),
    );
    o.set(
        "disk_write_bytes_per_s",
        s.global.write_bps.map_or_else(
            || Jv::why("not_sampled"),
            |b| Jv::at_least_refused(b.round() as u64, s.global.refused),
        ),
    );
    o.set("procs", Jv::Int(i64::from(s.global.procs)));
    o.set("exact", Jv::s("exact"));
    o
}

fn fig_json(f: &Fig) -> Jv {
    let mut o = Jv::obj([] as [(&str, Jv); 0]);
    if !f.present {
        o.set("cpu_mc", Jv::why("warming"));
        o.set("exact", Jv::s("live"));
        return o;
    }
    o.set("cpu_mc", Jv::u(mc(f.cores)));
    o.set("cpu_by_level_mc", Jv::why("not_sampled"));
    o.set("procs", Jv::Int(i64::from(f.procs)));
    o.set("unread", Jv::Int(i64::from(f.refused)));
    o.set("wait_mc", Jv::known_or(f.wait.map(mc), "not_sampled"));
    o.set(
        "starved_pm",
        Jv::known_or(
            f.starved().map(|x| (x * 1000.0).round() as u64),
            "not_sampled",
        ),
    );
    o.set("footprint_bytes", Jv::known_or(f.footprint, "refused"));
    o.set(
        "write_bytes_per_s",
        Jv::known_or(f.write_bps.map(|b| b.round() as u64), "not_sampled"),
    );
    if !f.program.is_empty() {
        o.set("program", Jv::s(exe_word(&f.program)));
    }
    o.set("exact", Jv::s("live"));
    o
}

fn tab_obj(ctx: &Ctx<'_>, s: &Snapshot, keys: &[u64], f: &Fig) -> Jv {
    let mut o = Jv::obj([
        (
            "tab",
            ctx.target
                .tab
                .map_or_else(|| Jv::why("gone"), |t| Jv::u(t as u64)),
        ),
        ("local", Jv::u(ctx.target.key)),
        ("sid", Jv::s(ctx.target.sid.clone())),
    ]);
    if let Some(w) = ctx.target.window {
        o.set("window", Jv::u(w as u64));
    }
    if let Jv::Obj(pairs) = fig_json(f) {
        for (k, v) in pairs {
            o.set(&k, v);
        }
    }
    o.set(
        "jobs",
        s.jobs.as_deref().map_or_else(
            || Jv::why(jobs_view::missing_why(s)),
            |v| jobs_view::summary_json(&jobs_view::rows_in(v, keys)),
        ),
    );
    o
}

fn split_json(s: &Snapshot, tab: &Fig) -> Jv {
    match s.split {
        Some(sp) => {
            let rest = (sp.aterm - tab.cores).max(0.0);
            let whole = mc(sp.global) as i64;
            let t = mc(tab.cores) as i64;
            let r = mc(rest) as i64;
            let o = mc(sp.others) as i64;
            let remainder = whole - t - r - o;
            let (services, unaccounted) = if remainder >= 0 {
                (Jv::Int(remainder), Jv::why("lane_off:coalitions"))
            } else {
                (Jv::Int(0), Jv::Int(remainder))
            };
            Jv::obj([
                ("tab", Jv::Int(t)),
                ("aterm_rest", Jv::Int(r)),
                ("others", Jv::Int(o)),
                ("services", services),
                ("unaccounted", unaccounted),
            ])
        }
        None => Jv::why(if s.procs_window_ns.is_some() {
            "not_sampled"
        } else {
            "warming"
        }),
    }
}

/// Render the reply's JSON for `q`.
#[must_use]
pub fn render_json(q: &SysQuery, ctx: &Ctx<'_>) -> Jv {
    let scope = q.effective_scope();
    let mut o = Jv::obj([
        ("schema", Jv::Int(1)),
        ("kind", Jv::s(format!("sys.{}", q.view.word()))),
    ]);
    if let Some(sc) = q.view.default_scope().and(scope) {
        o.set("scope", Jv::s(sc.word()));
    }
    o.set("as_of", as_of_json(q.view, ctx));
    if let Some(g) = ctx.gap {
        o.set("gap", Jv::s(g.word()));
    }
    let Some(s) = ctx.snap else {
        o.set("events", Jv::Arr(Vec::new()));
        return o;
    };
    match q.view {
        View::Overview => {
            let keys = scope_keys(ctx, ScopeArg::Tab);
            let tab = tabs_fig(s, &keys, ctx.target.key);
            o.set("machine", machine_json(ctx, s));
            o.set("global", global_json(s));
            o.set("aterm", aterm_json(s));
            o.set("tab", tab_obj(ctx, s, &keys, &tab));
            o.set("split_mc", split_json(s, &tab));
            o.set("split_exact", Jv::s("heuristic"));
        }
        View::Tab => {
            let keys = scope_keys(ctx, ScopeArg::Tab);
            let tab = tabs_fig(s, &keys, ctx.target.key);
            let mut t = tab_obj(ctx, s, &keys, &tab);
            t.set("panes", Jv::u(keys.len() as u64));
            t.set("gpu_pm", Jv::why("lane_off:gpu_clients"));
            t.set("roots", Jv::u(ctx.roots.len() as u64));
            t.set(
                "root_bytes",
                ctx.disk.map_or_else(
                    || Jv::why("not_sampled"),
                    |d| {
                        if d.walking {
                            Jv::at_least(d.exclusive, "walking")
                        } else {
                            Jv::u(d.exclusive)
                        }
                    },
                ),
            );
            t.set("footprint_slope_bytes_per_min", Jv::why("not_sampled"));
            o.set("tab", t);
            o.set("procs", procs_json(q, ctx, s, ScopeArg::Tab));
        }
        View::Aterm => {
            o.set("aterm", aterm_json(s));
            o.set("parts", parts_json(ctx, s));
        }
        View::Global => {
            o.set("machine", machine_json(ctx, s));
            o.set("global", global_json(s));
            o.set("cores", cores_json(s));
            o.set("mem", mem_json(q, ctx, s));
            o.set("gpu", gpu_json(s));
            let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
            o.set("split_mc", split_json(s, &tab));
            o.set(
                "aterm_mc",
                Jv::known_or(s.aterm.as_ref().map(|a| mc(a.cores.raw)), "warming"),
            );
            o.set("split_exact", Jv::s("heuristic"));
            o.set(
                "throttle",
                s.thermal.map_or_else(
                    || Jv::why("unsupported"),
                    |t| {
                        let mut th = Jv::obj([
                            ("level", Jv::s(t.level.word())),
                            ("raw", Jv::u(u64::from(t.raw))),
                            ("exact", Jv::Bool(t.exact)),
                        ]);
                        if let Some(lp) = t.low_power {
                            th.set("low_power", Jv::Bool(lp));
                        }
                        th
                    },
                ),
            );
            if ctx.owner_class {
                let top: Vec<Jv> = s
                    .procs
                    .iter()
                    .filter(|r| !r.scope.in_aterm() && r.cores.is_some_and(|c| c >= 0.1))
                    .take(q.top)
                    .map(|r| proc_json(ctx, s, r))
                    .collect();
                o.set("top", Jv::Arr(top));
            } else {
                o.set("top", Jv::why("needs_privilege:owner"));
            }
        }
        View::Jobs => {
            o.set(
                "tab",
                ctx.target
                    .tab
                    .map_or_else(|| Jv::why("gone"), |t| Jv::u(t as u64)),
            );
            let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
            let keys = scope_keys(ctx, scope);
            match s.jobs.as_deref() {
                Some(v) => {
                    let rows = if scope == ScopeArg::Global {
                        jobs_view::rows_all(v)
                    } else {
                        jobs_view::rows_in(v, &keys)
                    };
                    o.set("summary", jobs_view::summary_json(&rows));
                    o.set(
                        "jobs",
                        Jv::Arr(
                            rows.iter()
                                .take(q.top)
                                .map(|r| jobs_view::row_json(r, ctx.tab_number(r.tab)))
                                .collect(),
                        ),
                    );
                    let f = jobs_view::findings_in(&v.findings, &keys, scope == ScopeArg::Global);
                    o.set("findings", jobs_view::findings_json(&f));
                    o.set(
                        "lanes_off",
                        Jv::Arr(v.lanes_off.iter().map(|w| Jv::s(*w)).collect()),
                    );
                }
                None => {
                    let why = jobs_view::missing_why(s);
                    o.set("summary", Jv::why(why));
                    o.set("jobs", Jv::why(why));
                }
            }
        }
        View::Cores => {
            let c = cores_json(s);
            if let Jv::Obj(pairs) = c {
                for (k, v) in pairs {
                    o.set(&k, v);
                }
            }
            let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
            o.set(
                "threads",
                Jv::obj([
                    ("runnable", Jv::why("not_sampled")),
                    (
                        "wait_mc",
                        s.global.wait.map_or_else(
                            || Jv::why("not_sampled"),
                            |w| Jv::at_least_refused(mc(w), s.global.refused),
                        ),
                    ),
                    ("tab_wait_mc", Jv::known_or(tab.wait.map(mc), "not_sampled")),
                    (
                        "tab_starved_pm",
                        Jv::known_or(
                            tab.starved().map(|x| (x * 1000.0).round() as u64),
                            "not_sampled",
                        ),
                    ),
                ]),
            );
            o.set(
                "placement",
                Jv::why(if ctx.machine.family == "macOS" {
                    "needs_privilege:root"
                } else {
                    "not_sampled"
                }),
            );
            o.set(
                "throttle",
                s.thermal
                    .map_or_else(|| Jv::why("unsupported"), |t| Jv::s(t.level.word())),
            );
        }
        View::Gpu => o.set("gpu", gpu_json(s)),
        View::Mem => {
            if let Jv::Obj(pairs) = mem_json(q, ctx, s) {
                for (k, v) in pairs {
                    o.set(&k, v);
                }
            }
        }
        View::Disk => o.set("disk", disk_json(q, ctx, s)),
        View::Procs => {
            let sc = scope.unwrap_or(ScopeArg::Tab);
            o.set("procs", procs_json(q, ctx, s, sc));
        }
        View::Why => why::why_json(q, ctx, s, &mut o),
    }
    o.set(
        "events",
        Jv::Arr(
            ctx.events
                .iter()
                .filter(|e| event_shown(ctx, e))
                .map(|e| event_json(ctx, s, e))
                .collect(),
        ),
    );
    o
}

fn aterm_json(s: &Snapshot) -> Jv {
    match s.aterm.as_ref() {
        Some(a) => {
            let mut f = Fig::of(a);
            f.program.clear();
            let mut o = fig_json(&f);
            if let Some(b) = s.cpu.as_ref().map(|c| c.busy.raw).filter(|b| *b > 0.05) {
                o.set(
                    "share_of_busy_pm",
                    Jv::u(((a.cores.raw / b) * 1000.0).round().clamp(0.0, 1000.0) as u64),
                );
            }
            o.set("instance", Jv::u(u64::from(s.self_pid)));
            o.set("coalitions", Jv::why("lane_off:coalitions"));
            o.set("via", Jv::s("parent_chains"));
            o
        }
        None => Jv::obj([("cpu_mc", Jv::why("warming")), ("exact", Jv::s("live"))]),
    }
}

fn parts_json(ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let Some(a) = s.aterm.as_ref() else {
        return Jv::why("warming");
    };
    let mut parts = Vec::new();
    let mut tab_sum = 0.0f32;
    for t in &s.tabs {
        tab_sum += t.scope.cores.raw.max(0.0);
        let mut p = Jv::obj([
            ("name", Jv::s("tab")),
            ("local", Jv::u(t.key)),
            (
                "tab",
                ctx.tab_number(t.key)
                    .map_or_else(|| Jv::why("gone"), |n| Jv::u(n as u64)),
            ),
        ]);
        if let Some(f) = ctx.tabs.iter().find(|f| f.key == t.key) {
            p.set("sid", Jv::s(f.sid.clone()));
        }
        let mut fig = Fig::of(&t.scope);
        fig.program.clone_from(&t.program);
        if let Jv::Obj(pairs) = fig_json(&fig) {
            for (k, v) in pairs {
                p.set(&k, v);
            }
        }
        parts.push(p);
    }
    let me = s.procs.iter().find(|r| r.scope == RowScope::SelfProc);
    let self_cores = me.and_then(|r| r.cores).unwrap_or(0.0);
    parts.push(Jv::obj([
        ("name", Jv::s("self")),
        ("pid", Jv::u(u64::from(s.self_pid))),
        (
            "cpu_mc",
            Jv::known_or(me.and_then(|r| r.cores).map(mc), "warming"),
        ),
        (
            "footprint_bytes",
            Jv::known_or(me.and_then(|r| r.footprint), "refused"),
        ),
    ]));
    let helpers = s
        .procs
        .iter()
        .filter(|r| r.scope == RowScope::Helper)
        .count();
    parts.push(Jv::obj([
        ("name", Jv::s("helpers")),
        (
            "cpu_mc",
            Jv::u(mc((a.cores.raw - tab_sum - self_cores).max(0.0))),
        ),
        ("procs", Jv::u(helpers as u64)),
    ]));
    parts.push(Jv::obj([
        ("name", Jv::s("unattributed")),
        ("cpu_mc", Jv::why("lane_off:coalitions")),
    ]));
    Jv::Arr(parts)
}

fn cores_json(s: &Snapshot) -> Jv {
    let Some(cpu) = s.cpu.as_ref() else {
        return Jv::obj([
            ("clusters", Jv::why("warming")),
            ("cores", Jv::why("warming")),
        ]);
    };
    let labels = core_labels(cpu);
    let clusters = cpu
        .clusters
        .iter()
        .enumerate()
        .map(|(level, c)| {
            Jv::obj([
                ("name", Jv::s(c.name.clone())),
                ("level", Jv::s(format!("l{level}"))),
                ("cpus", Jv::u(c.cores.len() as u64)),
                (
                    "busy_mc",
                    Jv::u(mc(c.cores.iter().map(|&i| cpu.cores[i].load.raw).sum())),
                ),
                ("req_mhz", Jv::why("lane_off:ioreport")),
                ("split_mc", Jv::why("not_sampled")),
                ("exact", Jv::s("exact")),
            ])
        })
        .collect();
    let mut cores: Vec<(usize, Jv)> = labels
        .iter()
        .map(|(core, label, _)| {
            (
                *core,
                Jv::obj([
                    ("name", Jv::s(label.clone())),
                    ("cpu", Jv::u(*core as u64)),
                    (
                        "busy_pm",
                        Jv::u(
                            (cpu.cores[*core].load.raw * 1000.0)
                                .round()
                                .clamp(0.0, 1000.0) as u64,
                        ),
                    ),
                    ("req_mhz", Jv::why("lane_off:ioreport")),
                ]),
            )
        })
        .collect();
    cores.sort_by_key(|(c, _)| *c);
    Jv::obj([
        ("clusters", Jv::Arr(clusters)),
        (
            "cores",
            Jv::Arr(cores.into_iter().map(|(_, j)| j).collect()),
        ),
    ])
}

fn gpu_json(s: &Snapshot) -> Jv {
    let Some(g) = s.gpu.as_ref() else {
        return Jv::why("unsupported");
    };
    let pm = |p: Option<u8>| Jv::known_or(p.map(|v| u64::from(v) * 10), "unsupported");
    let mut o = Jv::obj([
        (
            "device_pm",
            Jv::u((g.load.raw * 1000.0).round().max(0.0) as u64),
        ),
        ("renderer_pm", pm(g.sample.renderer_pct)),
        ("tiler_pm", pm(g.sample.tiler_pct)),
        (
            "in_use_bytes",
            Jv::known_or(g.sample.in_use_bytes, "unsupported"),
        ),
        ("cores", Jv::known_or(g.cores.map(u64::from), "unsupported")),
        ("split_pm", Jv::why("lane_off:gpu_clients")),
        ("clients", Jv::why("lane_off:gpu_clients")),
        ("exact", Jv::s("exact")),
    ]);
    if let Some(n) = g.name.as_ref() {
        o.set("name", Jv::s(n.clone()));
    }
    o
}

/// [`mem_classes`] as JSON: `[{class, bytes}]`, an unread class's bytes an
/// unknown.
pub(super) fn classes_json(m: &crate::model::MemView, exact: bool) -> Jv {
    Jv::Arr(
        mem_classes(m, exact)
            .iter()
            .map(|(k, b)| {
                Jv::obj([
                    ("class", Jv::s(*k)),
                    ("bytes", Jv::known_or(*b, "not_sampled")),
                ])
            })
            .collect(),
    )
}

fn mem_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let Some(m) = s.mem.as_ref() else {
        return Jv::obj([("classes", Jv::why("unsupported"))]);
    };
    let exact = exact_partition(ctx);
    let mut o = Jv::obj([
        ("total_bytes", Jv::u(m.sample.total)),
        (
            "partition",
            Jv::s(if exact { "exact" } else { "remainder" }),
        ),
        ("classes", classes_json(m, exact)),
        ("used_bytes", Jv::u(m.sample.used())),
        (
            "pressure",
            Jv::obj([
                (
                    "level",
                    if m.sample.pressure == crate::model::Pressure::Unknown {
                        Jv::why("unsupported")
                    } else {
                        Jv::s(m.sample.pressure.word())
                    },
                ),
                (
                    "kernel_available_pm",
                    Jv::known_or(
                        m.sample.kernel_available_pct.map(|p| u64::from(p) * 10),
                        "unsupported",
                    ),
                ),
            ]),
        ),
        (
            "swap",
            Jv::obj([
                ("used_bytes", Jv::u(m.sample.swap_used)),
                ("total_bytes", Jv::u(m.sample.swap_total)),
            ]),
        ),
        (
            "commit",
            m.sample.commit.as_ref().map_or_else(
                || Jv::why("unsupported"),
                |c| {
                    Jv::obj([
                        ("charge_bytes", Jv::u(c.charge)),
                        ("limit_bytes", Jv::u(c.limit)),
                        ("enforced", Jv::Bool(c.exact)),
                    ])
                },
            ),
        ),
    ]);
    o.set(
        "per_s",
        match m.rates {
            Some(r) => Jv::obj([
                ("swap_in_bytes", Jv::u(r.swapin_bps.round() as u64)),
                ("swap_out_bytes", Jv::u(r.swapout_bps.round() as u64)),
                ("decompressions", Jv::u(r.decompress_ps.round() as u64)),
                ("compressions", Jv::u(r.compress_ps.round() as u64)),
                ("pageins", Jv::u(r.pagein_ps.round() as u64)),
            ]),
            None => Jv::why(paging_why(m)),
        },
    );
    o.set(
        "thrashing",
        m.thrashing()
            .map_or_else(|| Jv::why(thrashing_why(m)), Jv::Bool),
    );
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let tab = tabs_fig(s, &keys, ctx.target.key);
    o.set(
        "tab",
        Jv::obj([
            (
                "tab",
                ctx.target
                    .tab
                    .map_or_else(|| Jv::why("gone"), |t| Jv::u(t as u64)),
            ),
            ("footprint_bytes", Jv::known_or(tab.footprint, "refused")),
            ("slope_bytes_per_min", Jv::why("not_sampled")),
            ("exact", Jv::s("live")),
        ]),
    );
    o.set(
        "aterm",
        Jv::obj([
            (
                "footprint_bytes",
                Jv::known_or(s.aterm.as_ref().and_then(|a| a.footprint), "warming"),
            ),
            ("exact", Jv::s("live")),
            ("via", Jv::s("parent_chains")),
        ]),
    );
    let mut rows: Vec<&ProcView> = s.procs.iter().filter(|r| r.footprint.is_some()).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.footprint));
    let mut withheld = 0u64;
    let mut top = Vec::new();
    for r in rows {
        let own = matches!(r.scope, RowScope::Tab(k) if keys.contains(&k));
        if !own && !ctx.owner_class {
            withheld += 1;
            continue;
        }
        if top.len() < q.top {
            top.push(proc_json(ctx, s, r));
        }
    }
    o.set("top", Jv::Arr(top));
    if withheld > 0 {
        o.set("withheld", Jv::u(withheld));
    }
    o
}

fn proc_json(ctx: &Ctx<'_>, s: &Snapshot, r: &ProcView) -> Jv {
    let why = if r.refused { "refused" } else { "warming" };
    let mut o = Jv::obj([
        ("pid", Jv::u(u64::from(r.pid))),
        ("ppid", Jv::u(u64::from(r.ppid))),
        ("pgid", Jv::u(u64::from(r.pgid))),
        ("exe", Jv::s(exe_word(&r.name))),
        ("label", Jv::s(proc_label(ctx, s, r))),
        ("scope", Jv::s(r.scope.word())),
    ]);
    if let RowScope::Tab(k) = r.scope {
        o.set("local", Jv::u(k));
        if let Some(n) = ctx.tab_number(k) {
            o.set("tab", Jv::u(n as u64));
        }
    }
    o.set("cpu_mc", Jv::known_or(r.cores.map(mc), why));
    o.set("wait_mc", Jv::known_or(r.wait.map(mc), why));
    o.set("footprint_bytes", Jv::known_or(r.footprint, "refused"));
    o.set("peak_bytes", Jv::known_or(r.peak, "refused"));
    o.set(
        "write_bytes_per_s",
        Jv::known_or(r.write_bps.map(|b| b.round() as u64), why),
    );
    o.set("stat", Jv::s(r.state.word()));
    o
}

fn procs_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, scope: ScopeArg) -> Jv {
    if s.procs_window_ns.is_none() {
        return Jv::why("warming");
    }
    let rows = procs_in_scope(ctx, s, scope);
    let unread = rows.iter().filter(|r| r.refused).count();
    Jv::obj([
        ("scope", Jv::s(scope.word())),
        ("live", Jv::u(rows.len() as u64)),
        ("unread", Jv::u(unread as u64)),
        (
            "cpu_mc",
            procs_total(&rows).map_or_else(|| Jv::why("warming"), |t| Jv::u(mc(t))),
        ),
        ("exact", Jv::s("live")),
        (
            "rows",
            Jv::Arr(
                rows.iter()
                    .take(q.top)
                    .map(|r| proc_json(ctx, s, r))
                    .collect(),
            ),
        ),
        ("more", Jv::u(rows.len().saturating_sub(q.top) as u64)),
    ])
}

fn disk_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
    let keys = scope_keys(ctx, scope);
    let f = tabs_fig(s, &keys, ctx.target.key);
    let depth = usize::from(q.depth.unwrap_or(DISK_DEPTH_DEFAULT));
    let roots = ctx
        .roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let n = i as u64 + 1;
            let mut o = Jv::obj([
                ("n", Jv::u(n)),
                (
                    "kinds",
                    Jv::Arr(r.kinds.iter().map(|k| Jv::s(*k)).collect()),
                ),
            ]);
            let path_ok = ctx.path_ok(&r.keys);
            if path_ok {
                o.set("path", Jv::s(r.path.clone()));
            } else {
                o.set(
                    "path",
                    Jv::obj([("root", Jv::u(n)), ("under", Jv::s(r.under(ctx.home)))]),
                );
            }
            if let Some(t) = r.tab {
                o.set("tab", Jv::u(t as u64));
            }
            if let Some(state) = r.size.state_word() {
                o.set("state", Jv::s(state));
            }
            r.size.json_fields(&mut o);
            if let Some(inside) = r.inside {
                o.set("inside", Jv::u(inside as u64));
            }
            o.set(
                "shared_with_tabs",
                Jv::Arr(r.shared_with.iter().map(|t| Jv::u(*t as u64)).collect()),
            );
            if let Some(t) = r.counted_in {
                o.set("counted_in_tab", Jv::u(t as u64));
            }
            let dirs: Vec<Jv> = r
                .dirs
                .iter()
                .filter(|d| d.depth() <= depth)
                .take(q.top)
                .enumerate()
                .map(|(rank, d)| {
                    let mut o = Jv::obj([
                        ("rank", Jv::u(rank as u64 + 1)),
                        ("alloc_bytes", Jv::u(d.alloc)),
                        ("entries", Jv::u(d.entries)),
                    ]);
                    if path_ok {
                        o.set("path", Jv::s(d.rel.clone()));
                    }
                    o
                })
                .collect();
            o.set("dirs", Jv::Arr(dirs));
            o
        })
        .collect();
    let (writers, withheld) = disk_writers(ctx, s, scope);
    let writers = writers
        .iter()
        .take(q.top)
        .map(|r| {
            Jv::obj([
                ("pid", Jv::u(u64::from(r.pid))),
                ("exe", Jv::s(exe_word(&r.name))),
                (
                    "write_bytes_per_s",
                    Jv::u(r.write_bps.unwrap_or(0.0).round() as u64),
                ),
                ("kind", Jv::s("disk_io")),
            ])
        })
        .collect();
    let volume = ctx.volume.map_or_else(
        || Jv::why("not_sampled"),
        |v| {
            let mut o = Jv::obj([
                ("mount", Jv::s(v.mount.clone())),
                ("fs", Jv::s(v.fs.clone())),
                ("free_bytes", Jv::u(v.free)),
                ("available_bytes", Jv::u(v.available)),
                ("total_bytes", Jv::u(v.total)),
                ("slope_bytes_per_s", Jv::why("not_sampled")),
                ("eta_full_s", Jv::why("not_sampled")),
                ("purgeable_bytes", Jv::why("not_sampled")),
            ]);
            o.set(
                "level",
                v.level
                    .as_ref()
                    .map_or_else(|| Jv::why("not_sampled"), |l| Jv::s(l.clone())),
            );
            o
        },
    );
    let mut o = Jv::obj([("roots", Jv::Arr(roots))]);
    disk::totals_json(ctx.disk, &mut o);
    o.set(
        "walking",
        ctx.disk
            .map_or_else(|| Jv::why("not_sampled"), |t| Jv::Bool(t.walking)),
    );
    o.set(
        "write_bytes_per_s",
        Jv::known_or(f.write_bps.map(|b| b.round() as u64), "not_sampled"),
    );
    o.set("writers", Jv::Arr(writers));
    if withheld > 0 {
        o.set("withheld", Jv::u(withheld as u64));
    }
    o.set("volume", volume);
    o
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
