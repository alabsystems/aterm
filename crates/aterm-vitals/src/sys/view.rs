// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each `sys` view as text lines and as JSON.

use super::disk::{self, RootSize};
use super::held;
use super::jobs_view;
use super::json::Jv;
use super::query::{DiskForm, ScopeArg, SysQuery, View};
use super::stores;
use super::why;
use super::{BudgetFacts, BudgetRead, Ctx, Freshness, clock_ms, exe_word, pct};
use crate::blockage::{DScope, DState, Zombies};
use crate::events::{EventKind, LoadScope};
use crate::heat::{Cut, NoHeat};
use crate::jobs::ZombieParent;
use crate::model::{ProcScope, ProcView, ProgramGroup, RowScope, ScopeView, Snapshot};
use crate::runq::Runnable;
use crate::situation::Reading;
use crate::words;
use crate::writes::ProgramHour;

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
    /// The disk view's form (`disk held`), or `None`: its lines differ from the plain view's.
    pub form: Option<DiskForm>,
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
        header_line(
            self.view,
            self.form,
            self.scope,
            self.subject,
            n,
            ctx,
            delta,
        )
    }
}

/// [`Reply::header`] of a reply of `n` lines.
pub(super) fn header_line(
    view: View,
    form: Option<DiskForm>,
    scope: Option<ScopeArg>,
    subject: Option<&str>,
    n: usize,
    ctx: &Ctx<'_>,
    delta: bool,
) -> String {
    let mut h = format!("OK {n} sys v=1 view={}", view.word());
    if let Some(f) = form {
        h.push_str(&format!(" form={}", f.word()));
    }
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
    if let Some(s) = ctx.snap.filter(|s| !s.stalled.is_empty()) {
        let lanes: Vec<&str> = s.stalled.iter().map(|l| l.lane).collect();
        h.push_str(&format!(" stalled={}", lanes.join(",")));
    }
    if delta {
        h.push_str(" delta=1");
    }
    h
}

/// When the figures `view` prints were read, their age and their window. A view a snapshot
/// carries from an earlier reading speaks as of that reading: `why`'s fragment, and the jobs
/// view of the engine's last pass (a reading the engine did not run on keeps its view), aged by
/// [`Snapshot::jobs_age_ms`], which the host reads off the one clock the pass and the reading
/// share.
fn as_of(view: View, ctx: &Ctx<'_>) -> Option<(u64, u64, Option<u64>)> {
    let snap = ctx.snap?;
    let carried = match view {
        View::Why => snap.why.as_deref().map(|f| {
            (
                snap.at_ns.saturating_sub(f.at_ns) / 1_000_000,
                Some(f.window_ns / 1_000_000),
            )
        }),
        View::Jobs => snap
            .jobs
            .as_deref()
            .map(|_| (snap.jobs_age_ms, window_ms(snap))),
        View::Boot => Some((0, None)),
        _ => None,
    };
    let (older, window) = carried.unwrap_or((0, window_ms(snap)));
    Some((
        ctx.as_of_unix_ms.saturating_sub(older),
        ctx.age_ms.saturating_add(older),
        window,
    ))
}

/// Rate window covered by a snapshot's figures, in milliseconds.
#[must_use]
pub fn window_ms(s: &Snapshot) -> Option<u64> {
    s.procs_window_ns
        .or(s.cores_window_ns)
        .map(|ns| ns / 1_000_000)
}

/// CPU ownership has one window and one account in every resource view. A process sweep's
/// live rates remain useful for its rows, but omit work that ended between sweeps.
fn cpu_facts(s: &Snapshot) -> Option<&crate::explain::WhyFacts> {
    s.why.as_deref().filter(|f| {
        f.lane.on() && f.split.via == crate::explain::Via::Coalitions && s.coal_lane.on()
    })
}

fn cpu_timing(s: &Snapshot) -> Option<(u64, u64, &'static str)> {
    s.scoped_cpu
        .as_ref()
        .map(|cpu| (cpu.read_at_ns, cpu.window_ns, "process_and_reap"))
        .or_else(|| {
            s.procs_window_ns
                .map(|span| (s.at_ns, span, "live_processes"))
        })
}

fn busy_cpu(s: &Snapshot) -> Option<f32> {
    s.cpu.as_ref().and_then(|c| c.current_busy())
}

fn aterm_cpu(s: &Snapshot) -> Option<f32> {
    s.scoped_cpu
        .as_ref()
        .map(|cpu| cpu.process_reported() as f32)
        .or_else(|| s.aterm.as_ref().map(|a| a.cores.raw))
}

fn tab_cpu(s: &Snapshot, keys: &[u64]) -> Option<(i64, bool)> {
    if let Some(cpu) = s.scoped_cpu.as_ref() {
        let rows: Vec<_> = cpu.tabs.iter().filter(|t| keys.contains(&t.key)).collect();
        return (!rows.is_empty()).then(|| {
            (
                (rows.iter().map(|t| t.reported).sum::<f64>() * 1000.0).round() as i64,
                rows.iter().any(|t| t.missed),
            )
        });
    }
    None
}

fn cpu_words(_s: &Snapshot) -> &'static str {
    "CPU is reported counter growth; updates can arrive late. Memory is live footprint"
}

fn cpu_coverage(s: &Snapshot) -> &'static str {
    match s.scoped_cpu.as_ref() {
        Some(cpu) if !cpu.missed && !cpu.tabs.iter().any(|tab| tab.missed) => "complete",
        _ => "partial",
    }
}

fn cpu_read_span_json(s: &Snapshot, span: Option<u64>) -> Jv {
    span.map_or_else(|| unknown_json(s, "procs", "warming"), Jv::u)
}

fn cpu_metadata(o: &mut Jv, read_span: Jv) {
    o.set("cpu_basis", Jv::s("published_counters"));
    o.set("wait_basis", Jv::s("runnable_minus_cpu_counter_growth"));
    o.set(
        "cpu_execution_window",
        Jv::why("unavailable:execution_window_unknown"),
    );
    o.set("cpu_read_span_ns", read_span);
    o.set(
        "share_of_busy_pm",
        Jv::why("unavailable:execution_window_unknown"),
    );
}

fn tab_cpu_json(s: &Snapshot, keys: &[u64], o: &mut Jv) {
    cpu_metadata(
        o,
        cpu_read_span_json(s, cpu_timing(s).map(|(_, span, _)| span)),
    );
    if let Some((reported, incomplete)) = tab_cpu(s, keys) {
        o.set("cpu_mc", Jv::Int(reported));
        o.set(
            "cpu_coverage",
            Jv::s(if incomplete { "partial" } else { "complete" }),
        );
        if let Some(cpu) = &s.scoped_cpu {
            let rows: Vec<_> = cpu.tabs.iter().filter(|t| keys.contains(&t.key)).collect();
            o.set(
                "cpu_charge_mc",
                Jv::Int((rows.iter().map(|t| t.charge).sum::<f64>() * 1000.0).round() as i64),
            );
            o.set("cpu_ambiguous", Jv::Bool(rows.iter().any(|t| t.ambiguous)));
            o.set("cpu_via", Jv::s("process_and_reap"));
        } else if let Some(f) = cpu_facts(s) {
            o.set("cpu_charge_mc", Jv::Int(f.split.tab_charge_of(keys)));
            o.set("cpu_via", Jv::s("process_counters"));
        }
    }
}

/// A scope's figures, summed over its sessions where it has several.
#[derive(Clone, Debug, Default)]
struct Fig {
    present: bool,
    cores: f32,
    footprint: Option<u64>,
    footprint_unread: u32,
    procs: u32,
    wait: Option<f32>,
    write_bps: Option<f64>,
    refused: u32,
    program: String,
    gpu: Option<f32>,
}

impl Fig {
    fn of(sv: &ScopeView) -> Self {
        Self {
            present: true,
            cores: sv.cores.raw.max(0.0),
            footprint: sv.footprint,
            footprint_unread: sv.footprint_unread,
            procs: sv.procs,
            wait: sv.wait,
            write_bps: sv.write_bps,
            refused: sv.refused,
            program: String::new(),
            gpu: sv.gpu,
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
        self.footprint_unread = self.footprint_unread.saturating_add(o.footprint_unread);
        self.wait = match (self.wait, o.wait) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.write_bps = match (self.write_bps, o.write_bps) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.gpu = match (self.gpu, o.gpu) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.refused += o.refused;
        if self.program.is_empty() {
            self.program.clone_from(&o.program);
        }
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
        ScopeArg::Request => match ctx.target.request.as_ref() {
            Some(request) if !request.sessions.is_empty() => request.sessions.clone(),
            _ => vec![ctx.target.key],
        },
        ScopeArg::Aterm | ScopeArg::Global => ctx.tabs.iter().map(|t| t.key).collect(),
    }
}

/// `scope=request`'s label: `request <id> (<n> tabs)`.
fn request_label(ctx: &Ctx<'_>) -> String {
    ctx.target.request.as_ref().map_or_else(
        || "request -".to_owned(),
        |request| {
            let n = request.tabs;
            format!(
                "request {} ({n} tab{})",
                request.id,
                if n == 1 { "" } else { "s" }
            )
        },
    )
}

/// `scope=request`'s totals: what the request's tabs' trees spent and what its claimed folders
/// hold, each `unavailable` where nothing was read, never 0.
fn request_line(ctx: &Ctx<'_>) -> Option<Line> {
    let request = ctx.target.request.as_ref()?;
    let fig = |v: Option<u64>| v.map_or_else(|| unavailable("not_sampled"), |v| v.to_string());
    Some(line(
        "request",
        format!(
            "request={} tabs={} cpu_s={} peak_mb={} disk_written_mb={} claims={} claimed_mb={} \
             (spend: a floor; a process that ended between reads is missed)",
            request.id,
            request.tabs,
            fig(request.cpu_s),
            fig(request.peak_mb),
            fig(request.written_mb),
            request.claims,
            fig(request.claimed_mb),
        ),
    ))
}

/// `scope=request`'s totals as JSON.
fn request_json(ctx: &Ctx<'_>) -> Option<Jv> {
    let request = ctx.target.request.as_ref()?;
    let fig = |v: Option<u64>| v.map_or_else(|| Jv::why("not_sampled"), Jv::u);
    Some(Jv::obj([
        ("id", Jv::s(request.id.clone())),
        ("tabs", Jv::u(request.tabs as u64)),
        ("cpu_s", fig(request.cpu_s)),
        ("peak_mb", fig(request.peak_mb)),
        ("disk_written_mb", fig(request.written_mb)),
        ("claims", Jv::u(request.claims as u64)),
        ("claimed_mb", fig(request.claimed_mb)),
    ]))
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

/// Cores-worth to two decimals with `c`: `4.13c`.
pub(super) fn c2(x: f32) -> String {
    let x = if x.is_finite() { x.max(0.0) } else { 0.0 };
    format!("{x:.2}c")
}

fn mc(x: f32) -> u64 {
    (f64::from(x) * 1000.0).round() as u64
}

fn mc_opt(x: Option<f32>) -> Option<i64> {
    x.map(|v| (f64::from(v) * 1000.0).round() as i64)
}

fn wait_json(value: Option<f32>, why: &str) -> Jv {
    mc_opt(value).map_or_else(|| Jv::why(why), Jv::Int)
}

/// The GPU lane's word while it holds no figure ([`crate::gpu_clients`]).
const GPU_LANE_OFF: &str = "lane_off:gpu_clients";

/// A share of the device (`1.0` the whole GPU) in per-mille.
fn gpu_pm(share: f32) -> u64 {
    if share.is_finite() && share > 0.0 {
        (f64::from(share) * 1000.0).round() as u64
    } else {
        0
    }
}

/// A share of the device in parts per million.
fn gpu_ppm(share: f32) -> u64 {
    if share.is_finite() && share > 0.0 {
        (f64::from(share) * 1e6).round() as u64
    } else {
        0
    }
}

/// A share of the device as a sentence says it: `1.5%`.
fn gpu_share(share: f32) -> String {
    let s = if share.is_finite() {
        share.max(0.0)
    } else {
        0.0
    };
    format!("{:.1}%", s * 100.0)
}

fn bps_word(b: f64) -> String {
    let b = if b.is_finite() { b.max(0.0) } else { 0.0 };
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

/// `n`, with `≤` before it when it is only a bound (a counter that counts more than its state).
fn at_most_word(n: u32, bound: bool) -> String {
    if bound {
        format!("\u{2264}{n}")
    } else {
        n.to_string()
    }
}

/// A record value or `unavailable(<why>)`.
pub(super) fn rec<T: std::fmt::Display>(v: Option<T>, why: &str) -> String {
    v.map_or_else(|| unavailable(why), |v| v.to_string())
}

fn global_clauses(ctx: &Ctx<'_>, s: &Snapshot) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some((cpu, busy)) = s.cpu.as_ref().and_then(|c| busy_cpu(s).map(|b| (c, b))) {
        let motion = cpu
            .busy
            .motion(words::CORES_GATE)
            .word()
            .map(|w| format!(", {w}"))
            .unwrap_or_default();
        parts.push(format!(
            "{} of {} cores busy{motion}",
            words::cores(busy),
            cpu.total()
        ));
    } else if let Some(total) = s.total_cores() {
        let why = if s.cpu.is_some() {
            "counters did not advance"
        } else {
            "the first sample has no interval"
        };
        parts.push(format!(
            "{total} cores total, utilization unavailable ({why})"
        ));
    } else {
        parts.push("cores measuring (the first sample has no interval)".into());
    }
    if let Some(t) = s.thermal {
        parts.push(t.phrase());
    }
    if let Some(w) = s.global.wait.filter(|w| w.abs() >= 0.1) {
        parts.push(format!("{w:.2}c runnable-minus-CPU reported difference"));
    }
    if s.gpu_sample().is_some() {
        parts.push(format!("GPU {}", gpu_words(s)));
    }
    if let Some(m) = s.mem.as_ref() {
        parts.extend(mem_clauses(m));
    }
    if let Some(v) = s.volume.as_ref() {
        parts.push(format!("disk {}", words::disk_space_line(v)));
    }
    if let Some(net) = s.net.as_ref() {
        parts.extend(net_clause(net));
    }
    let _ = ctx;
    parts
}

fn net_clause(net: &crate::model::NetView) -> Option<String> {
    let mut sub = Vec::new();
    if let Some(r) = net.rates {
        sub.push(format!(
            "down {}, up {}",
            bps_word(r.rx_bps),
            bps_word(r.tx_bps)
        ));
    }
    if let Some(tcp) = net.tcp {
        sub.push(match (tcp.known_total(), tcp.other(tcp.listen)) {
            (Some(total), Some(listen)) => format!(
                "{total} TCP ({} established, {listen} listen)",
                tcp.established
            ),
            _ => format!("{} TCP established or close-wait", tcp.established),
        });
    }
    (!sub.is_empty()).then(|| format!("net {}", sub.join(", ")))
}

/// Memory's used, available and total capacities with pressure, followed by swap
/// (with `, swapping <rate>` while it moves).
pub(super) fn mem_clauses(m: &crate::model::MemView) -> [String; 2] {
    let mut w = format!("memory {}", words::memory_space_line(&m.sample));
    if m.sample.pressure != crate::model::Pressure::Unknown {
        w.push_str(&format!(", pressure {}", m.sample.pressure.word()));
    }
    let mut sw = words::swap_used(m.sample.swap_used);
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
        if stalled_lane(s, "aterm").is_some() {
            return vec![unavailable("stalled")];
        }
        return vec!["measuring (one process sweep has no interval)".into()];
    };
    let used = aterm_cpu(s).unwrap_or(a.cores.raw);
    let first = format!("{} reported cores", words::cores(used));
    let mut parts = vec![first];
    parts.push(memory_words(a.footprint, a.footprint_unread));
    parts.push(format!(
        "{} process{}",
        a.procs,
        if a.procs == 1 { "" } else { "es" }
    ));
    parts
}

fn tab_clauses(f: &Fig, _cpu_incomplete: bool) -> Vec<String> {
    if !f.present {
        return vec!["measuring (one process sweep has no interval)".into()];
    }
    let mut parts = Vec::new();
    if !f.program.is_empty() {
        parts.push(format!("program {}", exe_word(&f.program)));
    }
    parts.push(format!("{} reported cores", words::cores(f.cores)));
    parts.push(memory_words(f.footprint, f.footprint_unread));
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

pub(super) fn line(key: impl Into<String>, text: impl Into<String>) -> Line {
    Line {
        key: key.into(),
        text: text.into(),
        value: true,
    }
}

pub(super) fn note(key: &str, text: impl Into<String>) -> Line {
    Line {
        key: key.into(),
        text: text.into(),
        value: false,
    }
}

fn rest_line(ctx: &Ctx<'_>, s: &Snapshot, tab: &Fig) -> Option<Line> {
    if s.split.is_none() && cpu_facts(s).is_none() {
        return None;
    }
    let app = aterm_cpu(s)?;
    let used =
        tab_cpu(s, &scope_keys(ctx, ScopeArg::Tab)).map_or(tab.cores, |(mc, _)| mc as f32 / 1000.0);
    let (others, services) = cpu_facts(s)
        .map(|f| {
            (
                f.split.others_mc as f32 / 1000.0,
                f.split.services_mc as f32 / 1000.0,
            )
        })
        .or_else(|| s.split.map(|sp| (sp.others, sp.services)))?;
    Some(line(
        "reported_cpu",
        format!(
            "reported_cpu tab {used:.3}c · aterm {app:.3}c · others {others:.3}c · services {services:.3}c · execution_window=unavailable:execution_window_unknown"
        ),
    ))
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
        EventKind::Thermal { from, to } => {
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
        EventKind::Thermal { from, to } => {
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
    if let Some(line) = stale_line(q.view, ctx) {
        lines.push(line);
    }
    if let Some(s) = ctx.snap {
        lines.extend(stalled_lines(s));
    }
    let events: Vec<Line> = ctx
        .snap
        .filter(|_| q.view != View::Boot)
        .map_or_else(Vec::new, |s| {
            ctx.events
                .iter()
                .filter(|e| event_shown(ctx, e))
                .map(|e| event_line(ctx, s, e))
                .collect()
        });
    let mut hw_notes: Vec<Line> = match (ctx.snap, q.view) {
        (Some(s), View::Cores | View::Why) => super::hardware::lines(s)
            .into_iter()
            .map(|s| note("cpu_hardware", s))
            .collect(),
        _ => Vec::new(),
    };
    if matches!(
        q.view,
        View::Overview | View::Tab | View::Aterm | View::Global
    ) && let Some(s) = ctx.snap
        && let Some((at, window, via)) = cpu_timing(s)
    {
        let older = s.at_ns.saturating_sub(at) / 1_000_000;
        lines.push(line(
            "cpu_reading",
            format!(
                "cpu_reading as_of={} age_ms={} window_ms={} via={via} includes_finished=1",
                clock_ms(ctx.as_of_unix_ms.saturating_sub(older)),
                ctx.age_ms.saturating_add(older),
                window / 1_000_000
            ),
        ));
    }
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
            View::Gpu => gpu_text(q, ctx, s, &mut lines),
            View::Mem => mem_text(q, ctx, s, &mut lines),
            View::Disk => disk_text(q, ctx, s, &mut lines),
            View::Net => net_text(q, s, &mut lines),
            View::Procs => procs_text(q, ctx, s, &mut lines),
            View::Why => why::why_text(q, ctx, s, &mut lines, &mut hw_notes, &events),
            View::Left => super::left::left_text(q, ctx, s, &mut lines),
            View::Report => {
                lines.push(note("report", "CPU diagnostic report; cause not established. Use --json to export raw evidence."));
                lines.extend(
                    super::hardware::lines(s)
                        .into_iter()
                        .map(|s| note("report", s)),
                );
            }
            View::Boot => lines.extend(super::boot::text(s.boot_cost.as_deref(), q.top)),
        },
    }
    if let Some(s) = ctx.snap
        && matches!(q.view, View::Cores | View::Procs)
    {
        scheduling_context_text(ctx, s, &mut lines);
    }
    lines.extend(hw_notes);
    lines.extend(events);
    Reply {
        view: q.view,
        form: q.form,
        scope: q.view.default_scope().and(scope),
        subject: why::subject_word(q, ctx.snap),
        lines,
        json: None,
    }
}

/// The lane stalled at `s` that feeds `figure` ([`crate::lanes::stalls`]).
pub(super) fn stalled_lane(s: &Snapshot, figure: &str) -> Option<&'static str> {
    crate::lanes::stalls(&s.stalled, figure)
}

/// Why `figure` reads unknown at `s`: `stalled` while a lane that feeds it is, else `why`.
pub(super) fn unknown_why(s: &Snapshot, figure: &str, why: &'static str) -> &'static str {
    if stalled_lane(s, figure).is_some() {
        "stalled"
    } else {
        why
    }
}

/// `figure` unknown at `s`, in JSON: `{"why":"stalled","lane":…}` while a lane that feeds it is
/// stalled, else `{"why": why}`.
pub(super) fn unknown_json(s: &Snapshot, figure: &str, why: &'static str) -> Jv {
    stalled_lane(s, figure).map_or_else(|| Jv::why(why), Jv::stalled)
}

/// `v`, or `figure` unknown at `s` ([`unknown_json`]).
fn known_or_stalled(s: &Snapshot, v: Option<u64>, figure: &str, why: &'static str) -> Jv {
    v.map_or_else(|| unknown_json(s, figure, why), Jv::u)
}

/// One head line per lane stalled at `s`: the read it is in, since when, and the figures it
/// leaves unknown.
fn stalled_lines(s: &Snapshot) -> Vec<Line> {
    s.stalled
        .iter()
        .map(|l| {
            let mut t = format!("stalled {}: {}", l.lane, l.call);
            if let Some(pid) = l.pid {
                t.push_str(&format!(" pid={pid}"));
                if let Some(name) = l.name.as_deref() {
                    t.push_str(&format!(" ({})", exe_word(name)));
                }
            }
            t.push_str(&format!(
                " since {} for {} · figures {}",
                clock_ms(u64::try_from(l.since_unix_ms).unwrap_or(0)),
                span_word(l.for_ms),
                l.figures().join(",")
            ));
            note(&format!("stalled.{}", l.lane), t)
        })
        .collect()
}

/// A span, ms, in its largest units: `850ms`, `12.4s`, `3m12s`, `2h05m`.
fn span_word(ms: u64) -> String {
    let s = ms / 1000;
    if ms < 1000 {
        format!("{ms}ms")
    } else if s < 60 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s / 60) % 60)
    }
}

/// The first line of a view whose figures come from a snapshot the reader has not refreshed
/// ([`Freshness::Stale`]): when they were read and how long ago, in words, so a reader of the
/// lines alone never takes an old `cores busy` or `easing` for the present (the header's
/// `state=stale` is the only other mark). `None` for a fresh or warming view, and for one with
/// no snapshot, which says it has none.
fn stale_line(view: View, ctx: &Ctx<'_>) -> Option<Line> {
    if ctx.state != Freshness::Stale || ctx.snap.is_none() {
        return None;
    }
    let (unix_ms, age_ms, _) = as_of(view, ctx)?;
    Some(note(
        "state",
        format!(
            "state  stale: these figures were read {} ago, at {} UTC, and have not been read \
             since",
            words::duration(age_ms),
            clock_ms(unix_ms)
        ),
    ))
}

fn overview_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    lines.push(sentence("global", "global", &global_clauses(ctx, s)));
    lines.push(sentence("aterm", "aterm", &aterm_clauses(s)));
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let mut tab = tabs_fig(s, &keys, ctx.target.key);
    if let Some((cpu, _)) = tab_cpu(s, &keys) {
        tab.cores = cpu as f32 / 1000.0;
    }
    let label = tab_label(ctx);
    let clauses = match stalled_lane(s, "tab").filter(|_| !tab.present) {
        Some(_) => vec![unavailable("stalled")],
        None => tab_clauses(&tab, tab_cpu(s, &keys).is_some_and(|(_, low)| low)),
    };
    lines.push(sentence(&label, &label, &clauses));
    if let Some(f) = cpu_facts(s)
        && f.split.ambiguous_tabs.iter().any(|k| keys.contains(k))
    {
        lines.push(line(
            "cpu_charge",
            format!(
                "cpu_charge tab_mc={} own_reported_mc={} reap_accounting=ambiguous",
                f.split.tab_charge_of(&keys),
                f.split
                    .tab_reported
                    .iter()
                    .filter(|(k, _)| keys.contains(k))
                    .map(|(_, mc)| *mc)
                    .sum::<i64>()
            ),
        ));
    }
    if let Some(r) = rest_line(ctx, s, &tab) {
        lines.push(r);
    }
    let views: Vec<&str> = View::ALL
        .iter()
        .filter(|v| **v != View::Overview)
        .map(|v| v.word())
        .collect();
    lines.push(note(
        "more",
        format!(
            "more=views:{} · aterm ctl @self sys --json · aterm help sys",
            views.join(",")
        ),
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
    head.push_str(&format!(" · {}", cpu_words(s)));
    lines.push(line("tab", head));
    if !f.present {
        lines.push(line(
            "cpu",
            if stalled_lane(s, "tab").is_some() {
                format!("cpu    {}", unavailable("stalled"))
            } else {
                "cpu    measuring (one process sweep has no interval)".to_owned()
            },
        ));
    } else {
        let used = tab_cpu(s, &keys).map_or(f.cores, |(mc, _)| mc as f32 / 1000.0);
        let mut cpu = format!(
            "cpu    {} reported · runnable-minus-CPU difference {}",
            c2(used),
            rec(mc_opt(f.wait), "not_sampled")
        );
        cpu.push_str(&format!(
            " · core type split {}",
            unavailable("not_sampled")
        ));
        if let Some(facts) = cpu_facts(s)
            && facts.split.ambiguous_tabs.iter().any(|k| keys.contains(k))
        {
            cpu.push_str(&format!(
                " · counter charge {} (inherited estimate)",
                c2(facts.split.tab_charge_of(&keys) as f32 / 1000.0)
            ));
        }
        lines.push(line("cpu", cpu));
        lines.push(line(
            "mem",
            format!(
                "mem    {} footprint ({} procs, {} unread) · slope {}",
                memory_words(f.footprint, f.footprint_unread),
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
            f.gpu
                .filter(|_| f.present)
                .map_or_else(|| unavailable(GPU_LANE_OFF), gpu_share),
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
            f.write_bps.map_or_else(
                || unavailable(unknown_why(s, "writes", "not_sampled")),
                bps_word
            ),
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
            "aterm   app work · instance {} · {} session{} · {} · coalitions {}",
            s.self_pid,
            ctx.tabs.len(),
            if ctx.tabs.len() == 1 { "" } else { "s" },
            cpu_words(s),
            if cpu_facts(s).is_some() {
                "measured".into()
            } else {
                unavailable("lane_off:coalitions")
            }
        ),
    ));
    let Some(a) = s.aterm.as_ref() else {
        lines.push(line(
            "cpu",
            if stalled_lane(s, "aterm").is_some() {
                format!("cpu     {}", unavailable("stalled"))
            } else {
                "cpu     measuring (one process sweep has no interval)".to_owned()
            },
        ));
        return;
    };
    lines.push(line("cpu", format!("cpu     {:.3} reported cores · read_span_ns={} · execution_window=unavailable:execution_window_unknown", aterm_cpu(s).unwrap_or(a.cores.raw), cpu_timing(s).map_or(0, |(_, span, _)| span))));
    for t in &s.tabs {
        let number = ctx
            .tab_number(t.key)
            .map_or_else(|| "-".into(), |n| n.to_string());
        let reported =
            tab_cpu(s, &[t.key]).map_or(t.scope.cores.raw as f64, |(mc, _)| mc as f64 / 1000.0);
        lines.push(line(
            format!("part tab {}", t.key),
            format!(
                "reported name=tab tab={number} local={} program={} cpu_mc={} fp_bytes={} procs={}",
                t.key,
                exe_word(&t.program),
                (reported * 1000.0).round() as i64,
                memory_record(t.scope.footprint, t.scope.footprint_unread),
                t.scope.procs
            ),
        ));
    }
    if let Some(cpu) = &s.scoped_cpu {
        lines.push(line(
            "part shared",
            format!(
                "reported name=shared cpu_mc={} cpu_charge_mc={} reap_ambiguous={}",
                (cpu.own_reported * 1000.0).round() as i64,
                (cpu.own_charge * 1000.0).round() as i64,
                cpu.own_ambiguous
            ),
        ));
        if let Some(coalition) = cpu.coalition_reported {
            lines.push(line(
                "coalition counter",
                format!(
                    "coalition_reported_mc={} (independent counter account)",
                    (coalition * 1000.0).round() as i64
                ),
            ));
        }
    }
    if let Some(shared) = &s.shared {
        lines.push(line(
            "shared app live",
            format!(
                "reported name=shared_app included_in=aterm cpu_mc={} fp_bytes={} procs={}",
                mc(shared.cores.raw),
                memory_record(shared.footprint, shared.footprint_unread),
                shared.procs
            ),
        ));
    }
    lines.push(line(
        "gpu",
        format!(
            "gpu     aterm's GPU time {}",
            a.gpu.map_or_else(
                || unavailable(GPU_LANE_OFF),
                |g| format!("{} of the GPU", gpu_share(g))
            )
        ),
    ));
    lines.push(line(
        "mem",
        format!(
            "mem     {} footprint (live members) · writes {}",
            memory_words(a.footprint, a.footprint_unread),
            a.write_bps
                .map_or_else(|| unavailable("not_sampled"), bps_word)
        ),
    ));
    if !s.native_terminals.is_empty() {
        let known = s
            .native_terminals
            .iter()
            .filter_map(|t| t.wait_cpu_ns)
            .fold(0_u64, u64::saturating_add);
        let unread = s
            .native_terminals
            .iter()
            .filter(|t| t.wait_cpu_ns.is_none())
            .count();
        lines.push(line("native waits", format!("native_waits completed={} wait_cpu_ns={} unread={} scope=owning_wait_total; includes waited descendants; not a current CPU rate", s.native_terminals.len(), if unread > 0 { format!("≥{known}") } else { known.to_string() }, unread)));
        for terminal in s.native_terminals.iter().take(8) {
            lines.push(line(
                format!("native wait {} {}", terminal.host.pid, terminal.seq),
                format!(
                    "native_wait pid={} start={} local={} wait_cpu_ns={} own_cpu_ns={} child_cpu_ns={} reaps_unwaited={} exited_ns={} previous_cut_ns={} current_cut_ns={}",
                    terminal.process.pid,
                    rec(terminal.process.start, "not_sampled"),
                    terminal.tab.map_or_else(|| "-".into(), |t| t.to_string()),
                    rec(terminal.wait_cpu_ns, "not_sampled"),
                    rec(terminal.process.cpu_ns, "not_sampled"),
                    rec(terminal.process.child_cpu_ns, "not_sampled"),
                    terminal.process.reaps_unwaited.map_or_else(|| unavailable("not_sampled"), |v| u8::from(v).to_string()),
                    rec(terminal.timing.exited_ns, "not_sampled"),
                    rec(terminal.timing.previous.continuous_ns, "not_sampled"),
                    rec(terminal.timing.current.continuous_ns, "not_sampled"),
                ),
            ));
        }
    }
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
    } else if let Some(total) = s.total_cores() {
        parts.push(format!("{total} cores"));
    }
    if let Some(g) = s.gpu_sample() {
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

/// The JSON `thermal` object: `{word, raw, exact, low_power}`, `low_power` unavailable where
/// the OS says nothing of it.
pub(super) fn thermal_json(t: crate::model::Thermal) -> Jv {
    Jv::obj([
        ("word", Jv::s(t.word())),
        ("raw", Jv::u(u64::from(t.raw))),
        ("exact", Jv::Bool(t.exact)),
        (
            "low_power",
            t.low_power.map_or_else(|| Jv::why("unsupported"), Jv::Bool),
        ),
    ])
}

/// The `thermal` line's words: the OS's word and its raw level, or the counter heuristic's
/// clause and what it estimates from.
fn thermal_words(s: &Snapshot) -> String {
    match s.thermal {
        Some(t) => {
            let mut w = if t.exact {
                format!("{} (OS level {})", t.word(), t.raw)
            } else {
                format!(
                    "{} (estimate: thermal zones against their trip points, and the throttle counters)",
                    t.phrase()
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
                rec(busy_cpu(s).map(words::cores), "no_counter_delta"),
                cpu.total()
            );
            if let Some(busy) = busy_cpu(s) {
                t.push_str(&format!(
                    " · {} cores idle",
                    words::cores((cpu.total() as f32 - busy).max(0.0))
                ));
            }
            for c in &cpu.clusters {
                let busy = cpu.current_cluster_busy(c);
                t.push_str(&format!(
                    " · {} {} of {}",
                    c.name,
                    rec(busy.map(words::cores), "no_counter_delta"),
                    c.cores.len()
                ));
            }
            t.push_str(&format!(
                " · requested MHz {}",
                unavailable("not_sampled:requested_frequency")
            ));
            lines.push(line("cpu", t));
        }
        None => lines.push(line(
            "cpu",
            "cpu      measuring (the first sample has no interval)",
        )),
    }
    if let Some(app) = aterm_cpu(s) {
        lines.push(line("reported_cpu", format!("reported_cpu aterm {app:.3}c · coverage={} · execution_window=unavailable:execution_window_unknown", cpu_coverage(s))));
    }
    let mut th = format!(
        "threads  reported runnable-minus-CPU difference {}",
        s.global.wait.map_or_else(
            || unavailable(unknown_why(s, "wait", "not_sampled")),
            |w| format!("{w:.2}c reported counter difference")
        )
    );
    th.push_str(&format!(
        " (your processes; {} other-user processes unread)",
        s.global.refused
    ));
    if let Some(l) = s.load {
        th.push_str(&format!(" · load {:.2} / {:.2} / {:.2}", l[0], l[1], l[2]));
    }
    lines.push(line("threads", th));
    lines.push(line("thermal", format!("thermal  {}", thermal_words(s))));
    lines.push(line("gpu", format!("gpu      {}", gpu_words(s))));
    if let Some(m) = s.mem.as_ref() {
        lines.push(line(
            "mem",
            format!(
                "mem      {}",
                mem_class_words(m, exact_partition(ctx), class_why(ctx))
            ),
        ));
        if let Some(c) = m.sample.commit.as_ref() {
            lines.push(line("commit", format!("commit   {}", commit_words(c))));
        }
        lines.push(line("paging", format!("paging   {}", paging_words(m))));
    }
    if let Some(v) = s.volume.as_ref() {
        lines.push(line(
            "disk",
            format!("disk     {} {}", pct(&v.mount), words::disk_space_line(v)),
        ));
    }
    if let Some(net) = s.net.as_ref() {
        lines.push(line(
            "net",
            format!("net      {}", net_summary_words(s, net)),
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
    if ctx.owner_class && stalled_lane(s, "top").is_some() {
        lines.push(line("top", format!("top      {}", unavailable("stalled"))));
    }
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
    let Some(g) = s.gpu_sample() else {
        return unavailable("unsupported");
    };
    let mut t = g.device_pct.map_or_else(
        || format!("{} busy", unavailable("not_sampled:device_utilization")),
        |pct| format!("{}% busy device-wide", pct.min(100)),
    );
    if let Some(name) = g.name.as_deref() {
        t.push_str(&format!(" · {}", device_word(name)));
    }
    if let Some(cores) = g.cores {
        t.push_str(&format!(" · {cores} cores"));
    }
    if let (Some(r), Some(ti)) = (g.renderer_pct, g.tiler_pct) {
        t.push_str(&format!(" · renderer {r}%, tiler {ti}%"));
    }
    if let Some(b) = g.in_use_bytes {
        t.push_str(&format!(" · {} in use", words::mem(b)));
    }
    if let Some(b) = g.alloc_bytes {
        t.push_str(&format!(" · {} allocated", words::mem(b)));
    }
    t.push_str(" · GPU memory total not reported · available not reported");
    if g.unified {
        t.push_str(" (unified system memory)");
    }
    t
}

/// Whether the memory classes partition physical memory exactly (macOS,
/// where the remainder is what the VM never manages), or leave memory no
/// class names (Linux, Windows; design §9.6).
pub(super) fn exact_partition(ctx: &Ctx<'_>) -> bool {
    let platform = match ctx.machine.family {
        "macOS" => Some(words::Platform::MacOs),
        "Linux" => Some(words::Platform::Linux),
        "Windows" => Some(words::Platform::Windows),
        _ => None,
    };
    ctx.snap.and_then(|s| s.mem.as_ref()).is_some_and(|m| {
        m.sample.physical_classes(platform).partition == crate::model::MemoryPartition::Exact
    })
}

/// A memory charge remains a lower bound whenever any member could not be read.
pub(super) fn memory_words(bytes: Option<u64>, unread: u32) -> String {
    match bytes {
        Some(bytes) if unread > 0 => format!("≥{}", words::mem(bytes)),
        Some(bytes) => words::mem(bytes),
        None => unavailable("refused"),
    }
}

fn memory_record(bytes: Option<u64>, unread: u32) -> String {
    match bytes {
        Some(bytes) if unread > 0 => format!("≥{bytes}"),
        _ => rec(bytes, "refused"),
    }
}

fn memory_json(bytes: Option<u64>, unread: u32) -> Jv {
    match bytes {
        Some(bytes) if unread > 0 => Jv::at_least_refused(bytes, unread),
        _ => Jv::known_or(bytes, "refused"),
    }
}

struct MemoryReading {
    owners: crate::model::MemoryOwners,
    via: &'static str,
    age_ms: u64,
}

impl MemoryReading {
    fn of(s: &Snapshot) -> Option<Self> {
        if let Some(f) = s
            .why
            .as_ref()
            .filter(|f| f.lane.on() && !f.split.rows.is_empty())
        {
            let mut owners = crate::model::MemoryOwners::of_coalitions(
                &f.split,
                f.memory_provenance_incomplete
                    || s.memory_owners.is_none_or(|o| o.ownership_incomplete),
            );
            owners.ownership_incomplete |= f.at_ns == s.at_ns
                && s.aterm
                    .as_ref()
                    .and_then(|a| a.footprint)
                    .is_some_and(|observed| observed > owners.aterm.bytes);
            return Some(Self {
                owners,
                via: "coalitions",
                age_ms: s.at_ns.saturating_sub(f.at_ns) / 1_000_000,
            });
        }
        s.memory_owners.map(|owners| Self {
            owners,
            via: "processes",
            age_ms: 0,
        })
    }

    fn charges(&self) -> [(&'static str, crate::model::MemoryCharge); 4] {
        [
            ("aterm", self.owners.aterm),
            ("others", self.owners.others),
            ("services", self.owners.services),
            ("unknown", self.owners.unknown),
        ]
    }

    fn words(&self, charge: crate::model::MemoryCharge) -> String {
        if self.owners.ownership_incomplete {
            return format!(
                "{} (observed {})",
                unavailable("ownership_incomplete"),
                words::mem(charge.bytes)
            );
        }
        if charge.members > 0 && charge.unread == charge.members {
            return unavailable("refused");
        }
        let mark = if charge.unread > 0 || self.owners.truncated {
            "≥"
        } else {
            ""
        };
        format!("{mark}{}", words::mem(charge.bytes))
    }

    fn json(&self) -> Jv {
        let mut result = Jv::obj([
            ("via", Jv::s(self.via)),
            ("age_ms", Jv::u(self.age_ms)),
            ("truncated", Jv::Bool(self.owners.truncated)),
            (
                "ownership_incomplete",
                Jv::Bool(self.owners.ownership_incomplete),
            ),
            (
                "units",
                Jv::s(if self.via == "coalitions" {
                    "charge_groups"
                } else {
                    "processes"
                }),
            ),
        ]);
        for (name, charge) in self.charges() {
            let bytes = if charge.members > 0 && charge.unread == charge.members {
                Jv::why("refused")
            } else if charge.unread > 0 {
                Jv::at_least_refused(charge.bytes, charge.unread)
            } else if self.owners.truncated {
                Jv::at_least(charge.bytes, "truncated")
            } else {
                Jv::u(charge.bytes)
            };
            let mut object = Jv::obj([
                (
                    "footprint_bytes",
                    if self.owners.ownership_incomplete {
                        Jv::why("ownership_incomplete")
                    } else {
                        bytes.clone()
                    },
                ),
                ("members", Jv::u(u64::from(charge.members))),
                ("unread", Jv::u(u64::from(charge.unread))),
            ]);
            if self.owners.ownership_incomplete {
                object.set("observed_footprint_bytes", bytes);
            }
            result.set(name, object);
        }
        result
    }
}

/// The classes, bytes; `None` for one the OS does not say (Windows'
/// compressed class with compression off, or on a reading that swept no
/// processes, where its store is read), never a false zero. The last is the remainder:
/// `not_in_vm` where the partition is exact, else `unnamed`.
pub(super) fn mem_classes(
    m: &crate::model::MemView,
    exact: bool,
) -> [(&'static str, Option<u64>); 6] {
    use crate::model::MemoryClassKind;
    // `exact_partition` supplies the naming proof; the physical counts do not depend on it.
    m.sample.physical_classes(None).classes.map(|class| {
        let name = match class.kind {
            MemoryClassKind::App => "app",
            MemoryClassKind::Wired => "wired",
            MemoryClassKind::Compressed => "compressed",
            MemoryClassKind::Cache => "cache",
            MemoryClassKind::Free => "free",
            MemoryClassKind::Residual if exact => "not_in_vm",
            MemoryClassKind::Residual => "unnamed",
        };
        (name, class.bytes)
    })
}

fn g2(b: u64) -> String {
    format!("{:.2}", b as f64 / f64::from(1u32 << 30))
}

/// Why a class the OS does not say reads unavailable: on Linux only the compressed class can be
/// missing, and only with zswap off or absent (design §9.6, "only with zswap"); elsewhere it is
/// not read.
pub(super) fn class_why(ctx: &Ctx<'_>) -> &'static str {
    if ctx.machine.family == "Linux" {
        "not_enabled:zswap"
    } else {
        "not_sampled"
    }
}

/// Why a process's lifetime peak footprint reads unavailable ([`ProcView::peak`]): macOS reads
/// `ri_lifetime_max_phys_footprint` (refused for another user's process) and Windows reads
/// `PeakPagefileUsage` for every process; Linux keeps no lifetime peak footprint (`VmHWM` is a
/// peak RSS), so it is unsupported there, never refused.
fn peak_why(ctx: &Ctx<'_>, mac: &'static str) -> &'static str {
    match ctx.machine.family {
        "macOS" => mac,
        "Windows" => "not_sampled",
        _ => "unsupported",
    }
}

/// Why a volume's purgeable space reads unavailable: APFS keeps such space (not sampled here yet);
/// Linux's and Windows' file systems have none.
fn purgeable_why(ctx: &Ctx<'_>) -> &'static str {
    if ctx.machine.family == "macOS" {
        "not_sampled"
    } else {
        "unsupported"
    }
}

fn class_g2(b: Option<u64>, why: &str) -> String {
    b.map_or_else(|| unavailable(why), g2)
}

fn mem_sum_words(m: &crate::model::MemView, exact: bool, why: &str) -> String {
    let c = mem_classes(m, exact);
    format!(
        "used {} = app {} + wired {} + compressed {} · cache {} · free {} · {} {} GiB",
        g2(m.sample.used()),
        class_g2(c[0].1, why),
        class_g2(c[1].1, why),
        class_g2(c[2].1, why),
        class_g2(c[3].1, why),
        class_g2(c[4].1, why),
        if exact { "not in VM" } else { "no class" },
        class_g2(c[5].1, why)
    )
}

fn mem_class_words(m: &crate::model::MemView, exact: bool, why: &str) -> String {
    let mut t = format!(
        "{} · pressure {}",
        words::memory_space_line(&m.sample),
        m.sample.pressure.word()
    );
    if let Some(k) = m.sample.memorystatus_level_pct {
        t.push_str(&format!(" (memorystatus_level {k}%)"));
    }
    t.push_str(" · ");
    t.push_str(&mem_sum_words(m, exact, why));
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
        m.sample
            .swap_used
            .map_or_else(|| unavailable("not_read"), words::mem),
        m.sample
            .swap_total
            .map_or_else(|| unavailable("not_read"), words::mem)
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
        ScopeArg::Request => request_label(ctx),
        ScopeArg::Aterm => "aterm".into(),
        ScopeArg::Global => "global".into(),
    };
    if scope == ScopeArg::Request {
        lines.extend(request_line(ctx));
    }
    let Some(v) = s.jobs.as_deref() else {
        lines.push(line(
            "jobs",
            format!(
                "jobs  {who} · {}: the stuck engine has not read this scope yet",
                unavailable(jobs_view::missing_why(s))
            ),
        ));
        lines.push(line(
            "machine",
            jobs_view::capacity_text(s, ctx.as_of_unix_ms, ctx.age_ms),
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
    lines.push(line(
        "machine",
        jobs_view::capacity_text(s, ctx.as_of_unix_ms, ctx.age_ms),
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
        ScopeArg::Pane | ScopeArg::Tab | ScopeArg::Window | ScopeArg::Request => {
            " · sys jobs scope=aterm (owner)"
        }
        ScopeArg::Aterm => " · sys jobs scope=global (owner)",
        ScopeArg::Global => "",
    };
    let left = jobs_view::overflow(rows.len(), q.top).map_or_else(String::new, |w| w + " · ");
    lines.push(note(
        "scheduling",
        "Use the job CPU and wait state with these machine readings to choose independent work. Free slots are observations, not reservations; run heavy work through build-slot in its own worktree. Keep one waiter per job.",
    ));
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
        let busy = cpu.current_cluster_busy(c);
        lines.push(line(
            format!("cluster {}", c.name),
            format!(
                "cluster name={} level=l{level} cpus={} busy_mc={} req_mhz={} split_mc={} exact={}",
                pct(&c.name),
                c.cores.len(),
                rec(busy.map(mc), "no_counter_delta"),
                unavailable("not_sampled:requested_frequency"),
                unavailable("not_sampled"),
                rec(busy.map(|_| "exact"), "no_counter_delta")
            ),
        ));
        for (core, label, _) in labels.iter().filter(|(_, _, l)| *l == level) {
            lines.push(line(
                format!("core {label}"),
                format!(
                    "core name={label} cpu={core} busy_pm={}",
                    rec(
                        cpu.cores
                            .get(*core)
                            .and_then(crate::model::CoreView::current)
                            .map(|load| { (load * 1000.0).round().clamp(0.0, 1000.0) as u32 }),
                        "no_counter_delta"
                    )
                ),
            ));
        }
    }
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
    lines.push(line(
        "threads",
        format!(
            "threads runnable_minus_cpu_mc={} unread={} tab_runnable_minus_cpu_mc={} tab_starved_pm={}",
            s.global.wait.map_or_else(
                || unavailable(unknown_why(s, "wait", "not_sampled")),
                |w| ((f64::from(w) * 1000.0).round() as i64).to_string()
            ),
            s.global.refused,
            rec(mc_opt(tab.wait), unknown_why(s, "tab", "not_sampled")),
            unavailable("execution_window_unknown")
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
    lines.push(line("thermal", format!("thermal {}", thermal_words(s))));
}

fn gpu_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    lines.push(line(
        "gpu",
        format!("gpu     {} · reported as one device", gpu_words(s)),
    ));
    lines.push(line("split", gpu_split_words(ctx, s)));
    let (clients, withheld) = gpu_clients(ctx, s);
    for r in clients.iter().take(q.top) {
        lines.push(line(
            format!("client {}", r.pid),
            format!(
                "client pid={} exe={} {} share_ppm={}",
                r.pid,
                exe_word(&r.name),
                row_scope_words(ctx, r),
                gpu_ppm(r.gpu.unwrap_or(0.0))
            ),
        ));
    }
    if withheld > 0 {
        lines.push(note(
            "withheld",
            format!("withheld={withheld} (client rows outside this tab are the owner's)"),
        ));
    }
    lines.push(note(
        "note",
        "note    the device reports one whole-percent figure; there is no per-core GPU use",
    ));
}

/// The `sys gpu` split line: client time by owner against the device's whole percent, the
/// residual printed only from 2 quanta (2 %).
fn gpu_split_words(ctx: &Ctx<'_>, s: &Snapshot) -> String {
    let Some((g, sp)) = s.gpu.as_ref().and_then(|g| Some((g, g.split?))) else {
        return format!(
            "split   per-scope GPU time {} · clients {}",
            unavailable(GPU_LANE_OFF),
            unavailable(GPU_LANE_OFF)
        );
    };
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key)
        .gpu
        .unwrap_or(0.0);
    let residual = g.load.raw - sp.clients;
    let mut t = format!(
        "split   tab {} · rest of aterm {} · others {} · {} {} · client time {}",
        gpu_share(tab),
        gpu_share((sp.aterm - tab).max(0.0)),
        gpu_share(sp.others),
        ctx.machine.services_word(),
        gpu_share(sp.services),
        gpu_share(sp.clients)
    );
    if residual.abs() < 0.02 {
        t.push_str(", within 2% of the device's whole percent, so no residual");
    } else {
        t.push_str(&format!(" · residual {:+.1}%", residual * 100.0));
    }
    t.push_str(" · live processes");
    t
}

/// Processes holding GPU time, most first, with how many outside the target tab a non-owner
/// may not see.
fn gpu_clients<'a>(ctx: &Ctx<'_>, s: &'a Snapshot) -> (Vec<&'a ProcView>, usize) {
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let mut withheld = 0usize;
    let mut rows: Vec<&ProcView> = s
        .procs
        .iter()
        .filter(|r| r.gpu.is_some_and(|g| g > 0.0))
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
    rows.sort_by(|a, b| {
        b.gpu
            .unwrap_or(0.0)
            .total_cmp(&a.gpu.unwrap_or(0.0))
            .then(a.pid.cmp(&b.pid))
    });
    (rows, withheld)
}

/// A row's `scope=` words, with its tab's number where it has one.
fn row_scope_words(ctx: &Ctx<'_>, r: &ProcView) -> String {
    match r.scope {
        RowScope::Tab(k) => ctx.tab_number(k).map_or_else(
            || format!("scope=tab local={k}"),
            |n| format!("scope=tab tab={n}"),
        ),
        other => format!("scope={}", other.word()),
    }
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
        "mem     {} · pressure {}",
        words::memory_space_line(&m.sample),
        m.sample.pressure.word()
    );
    if let Some(k) = m.sample.memorystatus_level_pct {
        head.push_str(&format!(" (memorystatus_level {k}%)"));
    }
    head.push_str(" · available is what the kernel can hand out without paging");
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
        format!("classes {}", mem_sum_words(m, exact, class_why(ctx))),
    ));
    if let Some(c) = m.sample.commit.as_ref() {
        lines.push(line("commit", format!("commit  {}", commit_words(c))));
    }
    lines.push(line("paging", format!("paging  {}", paging_words(m))));
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
    let aterm_fp = s.aterm.as_ref().and_then(|a| a.footprint);
    let mut owners = format!(
        "owners  {} {}",
        tab_label(ctx),
        memory_words(tab.footprint, tab.footprint_unread)
    );
    if let (Some(a), Some(t)) = (aterm_fp, tab.footprint)
        && tab.footprint_unread == 0
        && s.aterm.as_ref().is_some_and(|a| a.footprint_unread == 0)
    {
        owners.push_str(&format!(
            " · rest of aterm {}",
            words::mem(a.saturating_sub(t))
        ));
    }
    owners.push_str(" · live process footprints include compressed charges");
    lines.push(line("owners", owners));
    if let Some(reading) = MemoryReading::of(s) {
        let mut text = format!(
            "charges via={} age_ms={} truncated={}",
            reading.via,
            reading.age_ms,
            u8::from(reading.owners.truncated)
        );
        for (name, charge) in reading.charges() {
            text.push_str(&format!(" · {name} {}", reading.words(charge)));
        }
        let unread = reading
            .charges()
            .iter()
            .fold(0_u32, |sum, (_, c)| sum.saturating_add(c.unread));
        text.push_str(&format!(" · unread={unread}"));
        text.push_str(" · memory charges are not a partition of physical RAM");
        lines.push(line("charges", text));
    }
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
                rec(r.peak, peak_why(ctx, "not_sampled"))
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

/// The pane lines a caller may read, and how many outside its tab were withheld from a caller
/// that is not the owner (as [`disk_writers`]).
fn disk_panes<'a>(ctx: &Ctx<'_>, t: &'a disk::DiskTotals) -> (Vec<&'a disk::PaneLine>, usize) {
    let keys = scope_keys(ctx, ScopeArg::Tab);
    let mut withheld = 0usize;
    let shown = t
        .panes
        .iter()
        .filter(|p| {
            let own = keys.contains(&p.key);
            if own || ctx.owner_class {
                true
            } else {
                withheld += 1;
                false
            }
        })
        .collect();
    (shown, withheld)
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

/// The scope's programs of the last hour of writes, busiest first, and how many outside the
/// caller's tab were withheld from a caller that is not the owner (as [`disk_writers`]). The
/// hour is kept per tab (`ProcScope::Tab` is a tab's key, its least pane's), so a pane's scope
/// reads its tab's programs.
fn disk_programs<'a>(
    ctx: &Ctx<'_>,
    s: &'a Snapshot,
    scope: ScopeArg,
) -> (Vec<&'a ProgramHour>, usize) {
    let keys = scope_keys(
        ctx,
        if scope == ScopeArg::Pane {
            ScopeArg::Tab
        } else {
            scope
        },
    );
    let mine = scope_keys(ctx, ScopeArg::Tab);
    let mut withheld = 0usize;
    let programs = s
        .facts
        .hour_writers
        .iter()
        .filter(|p| match scope {
            ScopeArg::Global => true,
            ScopeArg::Aterm => p.scope != ProcScope::Other,
            _ => matches!(p.scope, ProcScope::Tab(k) if keys.contains(&k)),
        })
        .filter(|p| {
            let own = matches!(p.scope, ProcScope::Tab(k) if mine.contains(&k));
            if own || ctx.owner_class {
                true
            } else {
                withheld += 1;
                false
            }
        })
        .collect();
    (programs, withheld)
}

/// A program's `scope=` word: `tab`, `aterm` (aterm and its helpers, and tabs since closed) or
/// `others` (the user's other processes).
fn program_scope_word(scope: ProcScope) -> &'static str {
    match scope {
        ProcScope::Tab(_) => "tab",
        ProcScope::Aterm => "aterm",
        ProcScope::Shared => "shared-aterm",
        ProcScope::Other => "others",
    }
}

/// A program's bytes per hour over the hour's covered time; `None` under a minute of it
/// ([`ProgramHour::per_hour`]).
fn program_per_hour(p: &ProgramHour, covered_s: f64) -> Option<u64> {
    p.per_hour(covered_s)
        .filter(|b| b.is_finite() && *b >= 0.0)
        .map(|b| b.round() as u64)
}

/// The hour's covered time in whole seconds, rounded down, so `covered_s=60` never stands
/// beside a figure still warming toward its minute.
fn covered_whole_s(covered_s: f64) -> u64 {
    if covered_s.is_finite() && covered_s > 0.0 {
        covered_s.floor() as u64
    } else {
        0
    }
}

fn disk_text(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    if q.form == Some(DiskForm::Held) {
        held::text(ctx, s, lines);
        return;
    }
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
        ScopeArg::Request => request_label(ctx),
        ScopeArg::Aterm => "aterm".into(),
        ScopeArg::Global => "global".into(),
    };
    if scope == ScopeArg::Request {
        lines.extend(request_line(ctx));
    }
    if let Some(v) = ctx.volume {
        let volume = crate::model::VolumeSample {
            total: v.total,
            free: v.free,
            reserved: v.reserved,
            mount: v.mount.clone(),
        };
        lines.push(line(
            "space",
            format!("space {}", words::disk_space_line(&volume)),
        ));
    }
    if let Some(t) = ctx.disk {
        for sc in &t.scopes {
            let key = match sc {
                disk::ScopeLine::Global { .. } => "scope global".to_owned(),
                disk::ScopeLine::Bytes(b) => format!("scope {}", b.name),
            };
            lines.push(line(key, sc.text(&|t| pct(t), &unavailable)));
        }
        let (panes, withheld) = disk_panes(ctx, t);
        for p in panes {
            lines.push(line(
                format!("pane {}", p.key),
                p.text(ctx.path_ok(&[p.key]), ctx.home, &|t| pct(t), &|e| {
                    exe_word(e).to_owned()
                }),
            ));
        }
        if withheld > 0 {
            lines.push(note(
                "panes withheld",
                format!("withheld={withheld} (pane rows outside this tab are the owner's)"),
            ));
        }
    }
    lines.push(line(
        "disk",
        format!(
            "disk {who} · {} root{} · {} · writes {} (storage I/O of the scope's processes)",
            ctx.roots.len(),
            if ctx.roots.len() == 1 { "" } else { "s" },
            disk::totals_clause(ctx.disk, bps_word),
            f.write_bps.map_or_else(
                || unavailable(unknown_why(s, "writes", "not_sampled")),
                bps_word
            )
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
        if !r.scopes.is_empty() {
            rec.push_str(&format!(" scopes={}", r.scopes.join(",")));
        }
        if let Some(pool) = r.pool {
            rec.push_str(&format!(" pool={pool}"));
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
                    "dir root={n} {place} alloc_bytes={} entries={} walked_s={}",
                    d.alloc,
                    d.entries,
                    r.size
                        .sizes()
                        .and_then(|s| s.age_s)
                        .map_or_else(|| "unavailable(not_sampled)".to_owned(), |s| s.to_string())
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
    let covered_s = s.facts.writes_covered_s;
    let (programs, withheld) = disk_programs(ctx, s, scope);
    for (rank, p) in programs.iter().take(q.top).enumerate() {
        let scope = match p.scope {
            ProcScope::Tab(k) => ctx.tab_number(k).map_or_else(
                || format!("scope=tab local={k}"),
                |n| format!("scope=tab tab={n}"),
            ),
            other => format!("scope={}", program_scope_word(other)),
        };
        lines.push(line(
            format!("program {}", rank + 1),
            format!(
                "program exe={} n={} {scope} write_bytes_per_h={} covered_s={}",
                exe_word(&p.name),
                p.n_max,
                rec(program_per_hour(p, covered_s), "warming"),
                covered_whole_s(covered_s)
            ),
        ));
    }
    if withheld > 0 {
        lines.push(note(
            "programs withheld",
            format!("withheld={withheld} (program rows outside this tab are the owner's)"),
        ));
    }
    match ctx.volume {
        Some(v) => lines.push(line(
            "volume",
            format!(
                "volume mount={} fs={} free_bytes={} reserved_bytes={} total_bytes={} level={} slope_bps={} purgeable_bytes={}",
                pct(&v.mount),
                pct(&v.fs),
                v.free,
                v.reserved,
                v.total,
                v.level.as_deref().map_or_else(|| unavailable("not_sampled"), str::to_owned),
                unavailable("not_sampled"),
                unavailable(purgeable_why(ctx))
            ),
        )),
        None => lines.push(line(
            "volume",
            format!("volume {}", unavailable("not_sampled")),
        )),
    }
    for store in ctx.stores {
        lines.push(line(store.key(), store.text()));
    }
    lines.push(line(
        "budget",
        format!("budget {}", budget_words(ctx.volume.map(|v| v.budget))),
    ));
    lines.push(note("summary", disk_summary(ctx)));
}

/// How far a sized budget's rebuildable bytes are from its target, as the one field that says
/// so: `over_bytes` where they are over it, else `under_bytes` (`0` at the target).
fn budget_gap(b: &BudgetFacts) -> (&'static str, u64) {
    let over = b.rebuildable.saturating_sub(b.target);
    if over > 0 {
        ("over_bytes", over)
    } else {
        ("under_bytes", b.target.saturating_sub(b.rebuildable))
    }
}

/// `complete`, or `partial` where the sizing was cut short: the rebuildable bytes, the target and
/// `over_bytes` are then floors and the kept bytes a ceiling ([`BudgetFacts`]).
fn budget_walk(b: &BudgetFacts) -> &'static str {
    if b.partial { "partial" } else { "complete" }
}

/// `aterm.toml` where the cap is the figure `disk.cap_used_gib` wrote, `share` where it is the
/// volume's share.
fn budget_cap_from(b: &BudgetFacts) -> &'static str {
    if b.cap_written { "aterm.toml" } else { "share" }
}

/// Why a disk view prints no budget figures (`read` is `None` where the view has no volume).
fn budget_why(read: Option<BudgetRead>) -> &'static str {
    match read {
        Some(BudgetRead::Warming) => "warming",
        Some(BudgetRead::Off) => "not_enabled:disk.cap_used_gib",
        Some(BudgetRead::NotSampled | BudgetRead::Sized(_)) | None => "not_sampled",
    }
}

/// A disk view's `budget` record after its key: the figures of a sized budget, or why there
/// are none (`read` is `None` where the view has no volume).
pub(super) fn budget_words(read: Option<BudgetRead>) -> String {
    match read {
        Some(BudgetRead::Sized(b)) => {
            let (gap, by) = budget_gap(&b);
            format!(
                "cap_bytes={} cap_from={} kept_bytes={} target_bytes={} rebuildable_bytes={} {gap}={by} walk={} sized_s={}",
                b.cap,
                budget_cap_from(&b),
                b.kept,
                b.target,
                b.rebuildable,
                budget_walk(&b),
                b.sized_s
            )
        }
        other => unavailable(budget_why(other)),
    }
}

/// [`budget_words`] as the JSON `budget` value: the same figures (`sized_age_s` for the age),
/// or `{"why": <word>}`.
pub(super) fn budget_json(read: Option<BudgetRead>) -> Jv {
    match read {
        Some(BudgetRead::Sized(b)) => {
            let (gap, by) = budget_gap(&b);
            Jv::obj([
                ("cap_bytes", Jv::u(b.cap)),
                ("cap_from", Jv::s(budget_cap_from(&b))),
                ("kept_bytes", Jv::u(b.kept)),
                ("target_bytes", Jv::u(b.target)),
                ("rebuildable_bytes", Jv::u(b.rebuildable)),
                (gap, Jv::u(by)),
                ("walk", Jv::s(budget_walk(&b))),
                ("sized_age_s", Jv::u(b.sized_s)),
            ])
        }
        other => Jv::why(budget_why(other)),
    }
}

fn net_summary_words(s: &Snapshot, net: &crate::model::NetView) -> String {
    let mut parts = Vec::new();
    match net.rates {
        Some(r) => {
            let mut rate = format!(
                "down {} ({:.0} pkt/s), up {} ({:.0} pkt/s)",
                bps_word(r.rx_bps),
                r.rx_pps,
                bps_word(r.tx_bps),
                r.tx_pps
            );
            let errs = r.rx_err_ps + r.tx_err_ps;
            let drops = r.rx_drop_ps + r.tx_drop_ps.unwrap_or(0.0);
            if errs > 0.0 || drops > 0.0 {
                rate.push_str(&format!(" · err {errs:.1}/s, drop {drops:.1}/s"));
            }
            parts.push(rate);
        }
        None if !net.links.is_empty() => {
            parts.push(format!(
                "rates {}",
                unavailable(net.rates_why.map_or("warming", |why| why.word()))
            ));
        }
        None => {}
    }
    if let Some(l) = s.facts.link.as_ref() {
        let mut p = format!("primary {} ({})", pct(&l.name), l.kind.word());
        if let Some(bps) = l.rate_bps {
            p.push_str(&format!(", link {}", bps_word(bps as f64 / 8.0)));
        }
        parts.push(p);
    }
    if let Some(tcp) = net.tcp {
        parts.push(if tcp.counters_only {
            format!(
                "tcp {} established or close_wait, {} time_wait or orphaned fin_wait2 (other states not_sampled)",
                tcp.established, tcp.time_wait
            )
        } else {
            format!(
                "tcp {} total ({} established, {} listen, {} time_wait, {} close_wait, {} connecting)",
                tcp.total(),
                tcp.established,
                tcp.listen,
                tcp.time_wait,
                tcp.close_wait,
                tcp.connecting()
            )
        });
    }
    if let Some(ms) = net.srtt_ms {
        parts.push(format!("rtt {ms:.1} ms"));
    }
    if let Some(sock) = net.aterm_sockets {
        parts.push(format!("aterm {sock} sockets"));
    }
    if parts.is_empty() {
        unavailable("not_sampled")
    } else {
        parts.join(" · ")
    }
}

fn net_text(q: &SysQuery, s: &Snapshot, lines: &mut Vec<Line>) {
    if let Some(place) = s.place.as_ref() {
        lines.push(line("place", format!("place   {}", place.line)));
    }
    let Some(net) = s.net.as_ref() else {
        lines.push(line(
            "net",
            format!("net     {}", unavailable("not_sampled")),
        ));
        return;
    };
    lines.push(line(
        "net",
        format!("net     {}", net_summary_words(s, net)),
    ));
    match net.tcp {
        Some(tcp) => lines.push(line(
            "tcp",
            format!(
                "tcp     total={} established={} listen={} time_wait={} close_wait={} syn_sent={} syn_recv={} fin_wait1={} fin_wait2={} closing={} last_ack={} close={} srtt_us={}",
                rec(tcp.known_total(), "not_sampled"),
                at_most_word(tcp.established, tcp.counters_only),
                rec(tcp.other(tcp.listen), "not_sampled"),
                at_most_word(tcp.time_wait, tcp.counters_only),
                rec(tcp.other(tcp.close_wait), "not_sampled"),
                rec(tcp.other(tcp.syn_sent), "not_sampled"),
                rec(tcp.other(tcp.syn_recv), "not_sampled"),
                rec(tcp.other(tcp.fin_wait1), "not_sampled"),
                rec(tcp.other(tcp.fin_wait2), "not_sampled"),
                rec(tcp.other(tcp.closing), "not_sampled"),
                rec(tcp.other(tcp.last_ack), "not_sampled"),
                rec(tcp.other(tcp.close), "not_sampled"),
                rec(srtt_us(net.srtt_ms), "not_sampled"),
            ),
        )),
        None => lines.push(line(
            "tcp",
            format!("tcp     {}", unavailable("not_sampled")),
        )),
    }
    for l in net.links.iter().take(q.top) {
        let x = &l.sample;
        let primary = u8::from(s.facts.link.as_ref().is_some_and(|p| p.name == x.name));
        let rate_why = l.rates_why.map_or("warming", |why| why.word());
        let tx_drop_ps = match l.rates {
            Some(r) => rec(r.tx_drop_ps.map(|v| v.round() as u64), "unsupported"),
            None if x.tx_drops.is_none() => unavailable("unsupported"),
            None => unavailable(rate_why),
        };
        lines.push(line(
            format!("link {}", x.name),
            format!(
                "link name={} kind={} up={} loopback={} counted={} primary={primary} rx_bps={} tx_bps={} rx_pps={} tx_pps={} rx_err_ps={} tx_err_ps={} rx_drop_ps={} tx_drop_ps={tx_drop_ps} rx_bytes={} tx_bytes={} byte_counter={} byte_counter_bits={} byte_counter_quantum_bytes={} rx_packets={} tx_packets={} rx_errors={} tx_errors={} rx_drops={} tx_drops={} link_bps={}",
                pct(&x.name),
                x.kind.word(),
                u8::from(x.up),
                u8::from(x.loopback),
                u8::from(x.counts),
                rec(l.rates.map(|r| r.rx_bps.round() as u64), rate_why),
                rec(l.rates.map(|r| r.tx_bps.round() as u64), rate_why),
                rec(l.rates.map(|r| r.rx_pps.round() as u64), rate_why),
                rec(l.rates.map(|r| r.tx_pps.round() as u64), rate_why),
                rec(l.rates.map(|r| r.rx_err_ps.round() as u64), rate_why),
                rec(l.rates.map(|r| r.tx_err_ps.round() as u64), rate_why),
                rec(l.rates.map(|r| r.rx_drop_ps.round() as u64), rate_why),
                x.rx,
                x.tx,
                x.byte_counter.word(),
                rec(x.byte_counter.bits(), "not_sampled"),
                rec(x.byte_counter.quantum(), "not_sampled"),
                x.rx_packets,
                x.tx_packets,
                x.rx_errors,
                x.tx_errors,
                x.rx_drops,
                rec(x.tx_drops, "unsupported"),
                rec(x.rate_bps, "unsupported"),
            ),
        ));
    }
    if net.links.len() > q.top {
        lines.push(note(
            "more",
            format!(
                "more={} links · top={}",
                net.links.len() - q.top,
                net.links.len().min(64)
            ),
        ));
    }
    lines.push(note(
        "note",
        "note    interface counters and TCP state counts are machine-wide; per-tab TCP waits appear in sys jobs",
    ));
}

/// Default subtree depth listed by `sys disk` under each root.
pub const DISK_DEPTH_DEFAULT: u8 = 3;

/// `sys disk`'s summary when no walker's sizes came with the reading. On Windows the walker
/// starts when first asked in a headless instance, so it may yet measure them, and no process's
/// disk writes are read there (a process's handle I/O counts pipes, consoles and sockets too, so
/// no disk figure is made of it); elsewhere the walker measures none here and the writers are.
fn disk_unmeasured(windows: bool) -> String {
    let unread = unavailable("not_sampled");
    if windows {
        format!(
            "summary folder sizes, entry counts and growth are {unread} here (the disk walker has measured none yet; a headless instance starts it at the first ask); free space is measured, each process's disk writes are not"
        )
    } else {
        format!(
            "summary folder sizes, entry counts and growth are {unread} here (no disk walker measures them); free space and writers are measured"
        )
    }
}

fn disk_summary(ctx: &Ctx<'_>) -> String {
    let store = stores::summary_clause(ctx.stores);
    let Some(t) = ctx.disk else {
        let mut out = disk_unmeasured(ctx.machine.family == "Windows");
        if let Some(store) = store {
            out.push_str(&format!(" · warnings {store}"));
        }
        return out;
    };
    let folders = (!t.warnings.is_empty()).then(|| {
        let n = t.warnings.len();
        let worst = t.warnings.iter().map(|w| w.entries).max().unwrap_or(0);
        format!(
            "{n} folder{} past 100,000 entries (largest {worst})",
            if n == 1 { "" } else { "s" }
        )
    });
    let warnings = match (folders, store) {
        (None, None) => "none".to_owned(),
        (Some(one), None) | (None, Some(one)) => one,
        (Some(folders), Some(store)) => format!("{folders}, {store}"),
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

/// Why a process's age is unknown: its birth was not read on this reading (the OS gave none:
/// on macOS its kernel record missed or named another program, as after an exec).
const BIRTH_UNREAD: &str = "not_sampled";

/// [`Snapshot::programs`] in `scope`, as [`procs_in_scope`] takes the rows. A program groups
/// by tab ([`ProcScope::Tab`] is the tab's key, its least pane's), so a pane's programs are its
/// tab's: the target pane's key alone would miss them in any pane of a split tab but that one.
fn programs_in_scope<'a>(ctx: &Ctx<'_>, s: &'a Snapshot, scope: ScopeArg) -> Vec<&'a ProgramGroup> {
    let by_tab = if scope == ScopeArg::Pane {
        ScopeArg::Tab
    } else {
        scope
    };
    let keys = scope_keys(ctx, by_tab);
    s.programs
        .iter()
        .filter(|g| match scope {
            ScopeArg::Aterm => g.scope != ProcScope::Other,
            ScopeArg::Global => true,
            _ => matches!(g.scope, ProcScope::Tab(k) if keys.contains(&k)),
        })
        .collect()
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
        ScopeArg::Request => request_label(ctx),
        ScopeArg::Aterm => "aterm".into(),
        ScopeArg::Global => "global (the busiest and largest rows)".into(),
    };
    if scope == ScopeArg::Request {
        lines.extend(request_line(ctx));
    }
    if s.procs_window_ns.is_none() {
        lines.push(line(
            "procs",
            if stalled_lane(s, "procs").is_some() {
                format!(
                    "procs  {who} · {}: the process sweep did not answer",
                    unavailable("stalled")
                )
            } else {
                format!(
                    "procs  {who} · {}: one process sweep has no interval",
                    unavailable("warming")
                )
            },
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
            "proc pid={} exe={} label={} pgid={} cpu_mc={} cpu_mc_scope=process_self cpu_basis=published_counters wait_mc={} fp_bytes={} peak_bytes={} write_bps={} gpu_ppm={} stat={} scheduling={} age_ms={}",
            r.pid,
            exe_word(&r.name),
            proc_label(ctx, s, r),
            r.pgid,
            proc_cpu(r),
            rec(mc_opt(r.wait), proc_rate_why(ctx, r)),
            rec(r.footprint, "refused"),
            rec(r.peak, peak_why(ctx, "refused")),
            rec(r.write_bps.map(|b| b.round() as u64), proc_rate_why(ctx, r)),
            rec(r.gpu.map(gpu_ppm), GPU_LANE_OFF),
            r.state.word(),
            scheduling_json(r.scheduling, ctx.machine.family).to_text(),
            rec(r.age_ms(s.at_unix_ms), unknown_why(s, "ages", BIRTH_UNREAD))
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
    if view == View::Boot {
        return Jv::obj([
            (
                "unix_ms_decimal",
                ctx.snap.map_or_else(
                    || Jv::why("warming"),
                    |_| Jv::s(ctx.as_of_unix_ms.to_string()),
                ),
            ),
            (
                "age_ms_decimal",
                ctx.snap
                    .map_or_else(|| Jv::why("warming"), |_| Jv::s(ctx.age_ms.to_string())),
            ),
            ("window_ms", Jv::why("unavailable:execution_window_unknown")),
            ("cursor", Jv::s(ctx.cursor().to_string())),
            ("state", Jv::s(ctx.state.word())),
        ]);
    }
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
    if let Some(total) = s.total_cores() {
        o.set("cpus", Jv::u(total as u64));
    }
    if let Some(cpu) = s.cpu.as_ref() {
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
            s.gpu_sample().and_then(|g| g.cores).map(u64::from),
            "unsupported",
        ),
    );
    if let Some(name) = s.gpu_sample().and_then(|g| g.name.as_ref()) {
        o.set("gpu_name", Jv::s(name.clone()));
    }
    if let Some(m) = s.mem.as_ref() {
        o.set("memory_bytes", Jv::u(m.sample.total));
    }
    if let (Some(total), Some(m)) = (s.total_cores(), s.mem.as_ref()) {
        o.set(
            "capacity",
            Jv::obj([
                ("cpu_mc", Jv::u(total as u64 * 1000)),
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
    let kind = &s.situation.machine;
    for (key, field) in [
        ("type", kind.map(|m| m.name.clone())),
        ("model", kind.map(|m| m.model.clone())),
        ("model_id", kind.map(|m| m.model_id.clone())),
    ] {
        o.set(
            key,
            match field {
                Reading::Read(Some(text)) => Jv::s(text),
                Reading::Read(None) => Jv::why(TYPE_NOT_GIVEN),
                unread => reading_why(&unread),
            },
        );
    }
    o
}

/// Why a field of the computer's type holds no text although the type was read: the firmware
/// gave none for it, or none that passed the name ladder (`crate::situation::MachineType`).
const TYPE_NOT_GIVEN: &str = "not_sampled:firmware_gave_none";

/// Why a heat value read no number although its chain read: the window held under
/// [`crate::heat::MIN_CLOCK_NS`] of the core time it divides by, or no energy.
const HEAT_TOO_LITTLE: &str = "not_sampled:too_little_core_time";

/// A situational reading that is not one, as its unavailable word ([`Reading::why`]):
/// `not_sampled` or `unsupported`.
fn reading_why<T>(r: &Reading<T>) -> Jv {
    Jv::why(r.why().unwrap_or("not_sampled"))
}

/// A heat value's absence as an unavailable word: `unsupported:<reason>` where this system has no
/// reader of it, `not_sampled:<reason>` where its reader gave nothing this reading. A reason the
/// word rule does not take (`super::is_unavailable_word`) is left off, so the reply keeps its
/// contract whatever a reader names.
fn heat_why(why: NoHeat) -> Jv {
    let (head, reason) = match why {
        NoHeat::NotSupported(reason) => ("unsupported", reason),
        NoHeat::Unread(reason) => ("not_sampled", reason),
    };
    let word = format!("{head}:{reason}");
    Jv::why(if super::is_unavailable_word(&word) {
        &word
    } else {
        head
    })
}

/// `v × scale` as a whole figure; `unread_format` for a value that is no count (not finite, or
/// under zero), never a 0 in its place.
fn scaled(v: f64, scale: f64) -> Jv {
    let x = v * scale;
    if x.is_finite() && x >= 0.0 {
        Jv::u(x.round() as u64)
    } else {
        Jv::why("unread_format")
    }
}

/// The overview's `heat` object (A4 §12.1), from [`Snapshot::heat`] and the thermal state's word
/// as `global.thermal` prints it (the OS's where exact, the counter heuristic's where not):
/// `{pressure, thermal_state, clock {p_mhz, e_mhz, e_verdict, basis, p_usual_mhz, cut}, watts
/// {cpu_mw, gpu_mw, ane_mw}, ipj, p_share {time_pm, instructions_pm}}`. Each value that is not a
/// reading is its [`NoHeat`]'s word ([`heat_why`]); with no chain, `clock`, `ipj` and `p_share`
/// are each that one word.
fn heat_json(s: &Snapshot) -> Jv {
    let h = &s.heat;
    let watt = |w: Result<f64, NoHeat>| w.map_or_else(heat_why, |w| scaled(w, 1000.0));
    let (clock, ipj, p_share) = match h.chain {
        Err(why) => (heat_why(why), heat_why(why), heat_why(why)),
        Ok(c) => {
            // A clock the chain did not give: no table to judge it against, else too little time.
            let no_clock = || {
                c.range
                    .err()
                    .map_or_else(|| Jv::why(HEAT_TOO_LITTLE), heat_why)
            };
            let mhz = |v: Option<f64>| v.map_or_else(no_clock, |v| scaled(v, 1.0));
            // A share is at most the whole: the Super time and the QoS time are two accounts, so
            // their quotient may pass 1 by a tick's rounding.
            let share = |v: Option<f64>| match v {
                None => Jv::why(HEAT_TOO_LITTLE),
                Some(v) if v.is_nan() => Jv::why("unread_format"),
                Some(v) => scaled(v.min(1.0), 1000.0),
            };
            let clock = Jv::obj([
                ("p_mhz", mhz(c.judged.p_mhz)),
                (
                    "e_mhz",
                    if c.judged.e_refused {
                        Jv::why("not_sampled:past_table")
                    } else {
                        mhz(c.judged.e_mhz)
                    },
                ),
                ("e_verdict", Jv::s(c.e_verdict.word())),
                ("basis", Jv::s("coalition_counters")),
                (
                    "p_usual_mhz",
                    c.usual_mhz.map_or_else(
                        || {
                            Jv::why(if c.decile.is_some() {
                                "not_sampled:unlearned"
                            } else {
                                "not_sampled:no_busy_decile"
                            })
                        },
                        |v| scaled(v, 1.0),
                    ),
                ),
                ("cut", cut_json(c.cut)),
            ]);
            let ipj = c
                .judged
                .ipj
                .map_or_else(|| Jv::why(HEAT_TOO_LITTLE), |v| scaled(v, 1.0));
            let p_share = Jv::obj([
                ("time_pm", share(c.judged.p_time_share)),
                ("instructions_pm", share(c.judged.p_instruction_share)),
            ]);
            (clock, ipj, p_share)
        }
    };
    Jv::obj([
        (
            "pressure",
            h.pressure.map_or_else(heat_why, |p| Jv::s(p.word())),
        ),
        (
            "thermal_state",
            s.thermal
                .map_or_else(|| Jv::why("unsupported"), |t| Jv::s(t.word())),
        ),
        ("clock", clock),
        (
            "watts",
            Jv::obj([
                ("cpu_mw", watt(h.watts.cpu)),
                ("gpu_mw", watt(h.watts.gpu)),
                ("ane_mw", watt(h.watts.ane)),
            ]),
        ),
        ("ipj", ipj),
        ("p_share", p_share),
    ])
}

/// The Super clock's cut as `{state, …}`: `not_judged` (pressure under Moderate, or unread),
/// `gated` with the decile's M-PCLK `verdict` (which claims nothing), `unlearned`, and `within`
/// or `cut` with `usual_over_clock_m` (the usual clock ÷ the clock, in thousandths: over 1000
/// where the clock runs under its usual); `not_sampled` with no Super clock or decile this reading.
fn cut_json(cut: Cut) -> Jv {
    let state = |word: &str| Jv::obj([("state", Jv::s(word))]);
    let judged = |word: &str, ratio: f64| {
        Jv::obj([
            ("state", Jv::s(word)),
            ("usual_over_clock_m", scaled(ratio, 1000.0)),
        ])
    };
    match cut {
        Cut::NotJudged => state("not_judged"),
        Cut::Unread => Jv::why("not_sampled"),
        Cut::Gated(verdict) => Jv::obj([
            ("state", Jv::s("gated")),
            ("verdict", Jv::s(verdict.word())),
        ]),
        Cut::Unlearned => state("unlearned"),
        Cut::Within(ratio) => judged("within", ratio),
        Cut::Cut(ratio) => judged("cut", ratio),
    }
}

/// `global.queue` (A4 §12.1): the queue the over rule reads ([`crate::model::RowFacts::queue`]),
/// `{threads_m, basis}`: P10, the machine's runnable threads over 10 s (`runnable_10s`), or the
/// 1-minute load average only where P10 is unread (`load1`), in thousandths as `load_m` is. With
/// neither, why P10 is unread.
fn queue_json(s: &Snapshot) -> Jv {
    match s.facts.queue {
        Some(q) => Jv::obj([
            ("threads_m", scaled(q.threads, 1000.0)),
            ("basis", Jv::s(q.basis.word())),
        ]),
        None => Jv::why(match s.facts.runnable {
            Runnable::Unread(why) => why.word(),
            Runnable::Read(_) => "unread_format",
        }),
    }
}

/// The overview's `procs` object (A4 §12.1): blockage, from [`crate::model::RowFacts::zombies`]
/// and [`crate::model::RowFacts::dstate`]. `zombies` is each parent's pile `{parent, exe, count,
/// per_min_deci}` (with `local` and `tab` for a parent in a tab), largest first, and
/// `zombie_count` the machine's, a lower bound where the sweep stopped at its cap; `dstate` is
/// each process found in an uninterruptible wait `{pid, exe, threads, held}`, the held first,
/// with `dstate_count`, `dstate_held` and whose processes they cover (`dstate_scope`: `own` or
/// `machine`). A census pass that did not read every listed process (its cover takes more than
/// one pass) counts at least what it found, so both counts are then lower bounds
/// (`partial_census`); so are they where a read was cut at a cap (`truncated`: the sweep stopped
/// at its process cap, or the census cut a process's thread list and found none uninterruptible
/// in what it read), and a row's `threads` where its own list was cut.
///
/// A row names a process: a zombie pile outside the target's tab, and every `dstate` row, prints
/// only for the owner class and is counted in `zombies_withheld` or `dstate_withheld` otherwise.
/// Each list prints at most `top` rows, the rest counted in `zombies_more` or `dstate_more`. The
/// counts are narrowed by neither: `zombie_count` is the machine's, and `dstate_count` and
/// `dstate_held` cover the processes `dstate_scope` names. A reader that gave nothing is its
/// word, never an empty list.
fn blockage_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let mut o = Jv::obj([] as [(&str, Jv); 0]);
    // The rows of `key` a caller may read, cut at `top`, with what the gate and the cut left out.
    let list = |o: &mut Jv, key: &str, rows: Vec<Jv>, of: usize| {
        let (withheld, more) = (of - rows.len(), rows.len().saturating_sub(q.top));
        o.set(key, Jv::Arr(rows.into_iter().take(q.top).collect()));
        for (suffix, n) in [("withheld", withheld), ("more", more)] {
            if n > 0 {
                o.set(&format!("{key}_{suffix}"), Jv::u(n as u64));
            }
        }
    };
    match &s.facts.zombies {
        Zombies::Unread(why) => {
            o.set("zombies", Jv::why(why.word()));
            o.set("zombie_count", Jv::why(why.word()));
        }
        Zombies::Read(v) => {
            let shown = |z: &&ZombieParent| {
                ctx.owner_class || z.tab.is_some_and(|t| ctx.target.panes.contains(&t))
            };
            let piles: Vec<Jv> = v
                .piles
                .iter()
                .filter(shown)
                .map(|z| {
                    let mut p = Jv::obj([
                        ("parent", Jv::u(u64::from(z.ppid))),
                        ("exe", Jv::s(exe_word(&z.name))),
                        ("count", Jv::u(u64::from(z.count))),
                        ("per_min_deci", Jv::u(z.per_min_deci)),
                    ]);
                    if let Some(k) = z.tab {
                        p.set("local", Jv::u(k));
                        if let Some(n) = ctx.tab_number(k) {
                            p.set("tab", Jv::u(n as u64));
                        }
                    }
                    p
                })
                .collect();
            list(&mut o, "zombies", piles, v.piles.len());
            let count = u64::from(v.count);
            o.set(
                "zombie_count",
                if v.whole {
                    Jv::u(count)
                } else {
                    Jv::at_least(count, "truncated")
                },
            );
            o.set("zombies_age_ms", Jv::u(v.age_ms));
        }
    }
    match &s.facts.dstate {
        DState::Unread(why) => {
            for key in ["dstate", "dstate_count", "dstate_held", "dstate_scope"] {
                o.set(key, Jv::why(why.word()));
            }
        }
        DState::Read(v) => {
            let mut procs: Vec<&crate::blockage::DStuck> = v.procs.iter().collect();
            procs.sort_by_key(|d| (!d.held, d.pid));
            let rows: Vec<Jv> = procs
                .into_iter()
                .filter(|_| ctx.owner_class)
                .map(|d| {
                    Jv::obj([
                        ("pid", Jv::u(u64::from(d.pid))),
                        ("exe", Jv::s(exe_word(&d.name))),
                        (
                            "threads",
                            match d.threads.map(u64::from) {
                                Some(n) if d.threads_cut => Jv::at_least(n, "truncated"),
                                n => Jv::known_or(n, "unsupported"),
                            },
                        ),
                        ("held", Jv::Bool(d.held)),
                    ])
                })
                .collect();
            list(&mut o, "dstate", rows, v.procs.len());
            // The census reads this user's processes a budget's worth a pass: only a pass that
            // was its whole cover read every process it listed. Any other leaves out a mark its
            // budget did not re-read and whatever waits in the part of the list it did not reach.
            // A read cut at a cap (`!v.whole`) leaves out what lay past the cut. Either way the
            // counts are at least; the open cover is named first, as the wider gap.
            let partial = v
                .pass
                .as_ref()
                .is_some_and(|p| !(p.cover_done && p.passes_in_cover == 1));
            let count = |n: usize| match (partial, v.whole) {
                (true, _) => Jv::at_least(n as u64, "partial_census"),
                (false, false) => Jv::at_least(n as u64, "truncated"),
                (false, true) => Jv::u(n as u64),
            };
            o.set("dstate_count", count(v.procs.len()));
            o.set("dstate_held", count(v.held()));
            if v.scope == DScope::Machine {
                o.set("dstate_blocked", Jv::known_or(v.blocked, "not_sampled"));
            }
            o.set("dstate_age_ms", Jv::u(v.age_ms));
            o.set("dstate_scope", Jv::s(v.scope.word()));
        }
    }
    o
}

/// The overview's `situation` object (A4 §8, §12.1), from [`Snapshot::situation`]: `{lid, power
/// {source, battery_pct, charging}, docked, awake_since_ms, last_sleep {from_ms, to_ms, cause,
/// wake}}`. What the machine lacks is the reading `none` (a desktop's lid, battery and docked; a
/// machine that has not slept since it booted), and a value that is not a reading is its word
/// ([`Reading::why`]). The network place is not here: the reply's `place` prints it
/// ([`place_json`]), and Low Power Mode stays the global view's `thermal.low_power`.
fn situation_json(s: &Snapshot) -> Jv {
    let sit = &s.situation;
    let none = || Jv::s("none");
    let text = |t: &Option<String>| {
        t.as_ref()
            .map_or_else(|| Jv::why("not_sampled"), |t| Jv::s(t.clone()))
    };
    let lid = match &sit.lid {
        Reading::Read(l) => Jv::s(l.lid.word()),
        unread => reading_why(unread),
    };
    let power = match &sit.power {
        Reading::Read(p) => Jv::obj([
            ("source", Jv::s(p.source.word())),
            (
                "battery_pct",
                p.battery.map_or_else(none, |b| Jv::u(u64::from(b.pct))),
            ),
            (
                "charging",
                p.battery.map_or_else(none, |b| Jv::Bool(b.charging)),
            ),
        ]),
        unread => reading_why(unread),
    };
    let docked = match sit.docked() {
        Reading::Read(Some(docked)) => Jv::Bool(docked),
        Reading::Read(None) => none(),
        unread => reading_why(&unread),
    };
    let (awake_since, last_sleep) = match &sit.awake {
        Reading::Read(a) => (
            Jv::Int(a.since_ms()),
            a.last_sleep.as_ref().map_or_else(none, |sleep| {
                Jv::obj([
                    ("from_ms", Jv::Int(sleep.from_ms)),
                    ("to_ms", Jv::Int(sleep.to_ms)),
                    ("cause", text(&sleep.cause)),
                    ("wake", text(&sleep.wake)),
                ])
            }),
        ),
        unread => (reading_why(unread), reading_why(unread)),
    };
    Jv::obj([
        ("lid", lid),
        ("power", power),
        ("docked", docked),
        ("awake_since_ms", awake_since),
        ("last_sleep", last_sleep),
    ])
}

fn global_json(s: &Snapshot) -> Jv {
    let mut o = Jv::obj([(
        "cpu_mc",
        s.cpu.as_ref().map_or_else(
            || Jv::why("warming"),
            |_| Jv::known_or(busy_cpu(s).map(mc), "no_counter_delta"),
        ),
    )]);
    o.set(
        "cpu_cores",
        Jv::known_or(s.total_cores().map(|n| n as u64), "warming"),
    );
    if let Some(cpu) = s.cpu.as_ref() {
        o.set(
            "cpu_by_level_mc",
            Jv::Arr(
                cpu.clusters
                    .iter()
                    .map(|c| Jv::known_or(cpu.current_cluster_busy(c).map(mc), "no_counter_delta"))
                    .collect(),
            ),
        );
    }
    o.set(
        "wait_mc",
        s.global.wait.map_or_else(
            || unknown_json(s, "wait", "not_sampled"),
            |w| Jv::Int((f64::from(w) * 1000.0).round() as i64),
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
    o.set("queue", queue_json(s));
    o.set(
        "thermal",
        s.thermal
            .map_or_else(|| Jv::why("unsupported"), |t| Jv::s(t.word())),
    );
    o.set("gpu_pm", gpu_device_json(s));
    if let Some(m) = s.mem.as_ref() {
        o.set("mem_pressure", Jv::s(m.sample.pressure.word()));
        o.set("mem_used_bytes", Jv::u(m.sample.used()));
        o.set("mem_available_bytes", Jv::u(m.sample.available));
        o.set("mem_total_bytes", Jv::u(m.sample.total));
        o.set(
            "swap_used_bytes",
            Jv::known_or(m.sample.swap_used, "not_read"),
        );
    } else {
        o.set("mem_pressure", Jv::why("unsupported"));
        o.set("mem_available_bytes", Jv::why("unsupported"));
        o.set("mem_total_bytes", Jv::why("unsupported"));
    }
    for (name, value) in [
        (
            "disk_used_bytes",
            s.volume.as_ref().map(crate::model::VolumeSample::used),
        ),
        ("disk_total_bytes", s.volume.as_ref().map(|v| v.total)),
    ] {
        o.set(name, Jv::known_or(value, "not_sampled"));
    }
    o.set(
        "disk_free_bytes",
        Jv::known_or(s.volume.as_ref().map(|v| v.free), "not_sampled"),
    );
    o.set(
        "disk_write_bytes_per_s",
        s.global.write_bps.map_or_else(
            || unknown_json(s, "writes", "not_sampled"),
            |b| Jv::at_least_refused(b.round() as u64, s.global.refused),
        ),
    );
    match s.net.as_ref().and_then(|n| n.rates) {
        Some(r) => {
            o.set("net_rx_bytes_per_s", Jv::u(r.rx_bps.round() as u64));
            o.set("net_tx_bytes_per_s", Jv::u(r.tx_bps.round() as u64));
        }
        None => {
            let why = s.net.as_ref().and_then(|n| n.rates_why).map_or_else(
                || {
                    if s.net.as_ref().is_some_and(|n| !n.links.is_empty()) {
                        "warming"
                    } else {
                        "not_sampled"
                    }
                },
                |why| why.word(),
            );
            o.set("net_rx_bytes_per_s", Jv::why(why));
            o.set("net_tx_bytes_per_s", Jv::why(why));
        }
    }
    o.set(
        "tcp_established",
        s.net.as_ref().and_then(|n| n.tcp).map_or_else(
            || Jv::why("not_sampled"),
            |t| tcp_counter_json(t.established, t.counters_only, "close_wait_counted"),
        ),
    );
    o.set(
        "tcp_listen",
        Jv::known_or(
            s.net
                .as_ref()
                .and_then(|n| n.tcp)
                .and_then(|t| t.other(t.listen))
                .map(u64::from),
            "not_sampled",
        ),
    );
    o.set(
        "procs",
        stalled_lane(s, "procs").map_or_else(|| Jv::Int(i64::from(s.global.procs)), Jv::stalled),
    );
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
    o.set("wait_mc", wait_json(f.wait, "not_sampled"));
    o.set(
        "starved_pm",
        Jv::why("unavailable:execution_window_unknown"),
    );
    cpu_metadata(&mut o, Jv::why("warming"));
    o.set(
        "footprint_bytes",
        memory_json(f.footprint, f.footprint_unread),
    );
    o.set("footprint_unread", Jv::u(u64::from(f.footprint_unread)));
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
    tab_cpu_json(s, keys, &mut o);
    if let Some(lane) = stalled_lane(s, "tab").filter(|_| !f.present) {
        o.set("cpu_mc", Jv::stalled(lane));
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

fn split_json(ctx: &Ctx<'_>, s: &Snapshot, tab: &Fig) -> Jv {
    let Some(app) = aterm_cpu(s) else {
        return unknown_json(s, "split", "warming");
    };
    let used = tab_cpu(s, &scope_keys(ctx, ScopeArg::Tab))
        .map_or(tab.cores as f64, |(mc, _)| mc as f64 / 1000.0);
    let mut o = Jv::obj([
        ("aterm", Jv::Int((app as f64 * 1000.0).round() as i64)),
        ("tab", Jv::Int((used * 1000.0).round() as i64)),
    ]);
    if let Some(f) = cpu_facts(s) {
        o.set("coalition_aterm", Jv::Int(f.split.aterm_mc));
        o.set("others", Jv::Int(f.split.others_mc));
        o.set("services", Jv::Int(f.split.services_mc));
    } else if let Some(sp) = s.split {
        o.set(
            "others",
            Jv::Int((sp.others as f64 * 1000.0).round() as i64),
        );
        o.set(
            "services",
            Jv::Int((sp.services as f64 * 1000.0).round() as i64),
        );
    }
    if let Some(sp) = s.split {
        o.set(
            "process_unknown_owner",
            Jv::u(mc(sp.unknown_owner_reported)),
        );
    }
    cpu_metadata(
        &mut o,
        cpu_read_span_json(s, cpu_timing(s).map(|(_, span, _)| span)),
    );
    o
}

/// Render the reply's JSON for `q`.
#[must_use]
pub fn render_json(q: &SysQuery, ctx: &Ctx<'_>) -> Jv {
    let scope = q.effective_scope();
    let mut o = Jv::obj([
        // `sys.disk` replies carry its scope and pane arrays (flow record §7.12); every other
        // kind keeps 1.
        ("schema", Jv::Int(if q.view == View::Disk { 2 } else { 1 })),
        ("kind", Jv::s(format!("sys.{}", q.view.word()))),
    ]);
    if let Some(f) = q.form {
        o.set("form", Jv::s(f.word()));
    }
    if let Some(sc) = q.view.default_scope().and(scope) {
        o.set("scope", Jv::s(sc.word()));
        if sc == ScopeArg::Request
            && let Some(request) = request_json(ctx)
        {
            o.set("request", request);
        }
    }
    o.set("as_of", as_of_json(q.view, ctx));
    if let Some(g) = ctx.gap {
        o.set("gap", Jv::s(g.word()));
    }
    if let Some(s) = ctx.snap.filter(|s| !s.stalled.is_empty()) {
        o.set("lanes", lanes_json(s));
    }
    let Some(s) = ctx.snap else {
        o.set("events", Jv::Arr(Vec::new()));
        return o;
    };
    if matches!(
        q.view,
        View::Overview | View::Tab | View::Aterm | View::Global
    ) && let Some((at, window, _)) = cpu_timing(s)
    {
        let older = s.at_ns.saturating_sub(at) / 1_000_000;
        o.set(
            "cpu_as_of",
            Jv::obj([
                ("unix_ms", Jv::u(ctx.as_of_unix_ms.saturating_sub(older))),
                ("age_ms", Jv::u(ctx.age_ms.saturating_add(older))),
                ("window_ms", Jv::u(window / 1_000_000)),
            ]),
        );
    }
    match q.view {
        View::Overview => {
            let keys = scope_keys(ctx, ScopeArg::Tab);
            let tab = tabs_fig(s, &keys, ctx.target.key);
            o.set("machine", machine_json(ctx, s));
            o.set("global", global_json(s));
            o.set("aterm", aterm_json(s));
            o.set("tab", tab_obj(ctx, s, &keys, &tab));
            o.set("reported_cpu_mc", split_json(ctx, s, &tab));
            o.set("cpu_coverage", Jv::s(cpu_coverage(s)));
            o.set("heat", heat_json(s));
            o.set("procs", blockage_json(q, ctx, s));
            o.set("situation", situation_json(s));
            o.set("place", place_json(s));
        }
        View::Tab => {
            let keys = scope_keys(ctx, ScopeArg::Tab);
            let tab = tabs_fig(s, &keys, ctx.target.key);
            let mut t = tab_obj(ctx, s, &keys, &tab);
            t.set("panes", Jv::u(keys.len() as u64));
            t.set("gpu_pm", Jv::known_or(tab.gpu.map(gpu_pm), GPU_LANE_OFF));
            t.set("roots", Jv::u(ctx.roots.len() as u64));
            t.set(
                "root_bytes",
                ctx.disk
                    .map_or_else(|| Jv::why("not_sampled"), |d| d.figure_json(d.exclusive)),
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
            o.set("gpu", gpu_json(q, ctx, s));
            o.set("place", place_json(s));
            o.set("net", net_json(q, s));
            let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key);
            o.set("reported_cpu_mc", split_json(ctx, s, &tab));
            o.set(
                "aterm_mc",
                if let Some(lane) = stalled_lane(s, "aterm").filter(|_| s.aterm.is_none()) {
                    Jv::stalled(lane)
                } else {
                    known_or_stalled(s, aterm_cpu(s).map(mc), "aterm", "warming")
                },
            );
            o.set("cpu_coverage", Jv::s(cpu_coverage(s)));
            o.set(
                "thermal",
                s.thermal
                    .map_or_else(|| Jv::why("unsupported"), thermal_json),
            );
            if let Some(lane) = stalled_lane(s, "top").filter(|_| ctx.owner_class) {
                o.set("top", Jv::stalled(lane));
            } else if ctx.owner_class {
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
                "machine",
                jobs_view::capacity_json(s, ctx.as_of_unix_ms, ctx.age_ms),
            );
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
                    (
                        "wait_mc",
                        s.global.wait.map_or_else(
                            || unknown_json(s, "wait", "not_sampled"),
                            |w| Jv::Int((f64::from(w) * 1000.0).round() as i64),
                        ),
                    ),
                    (
                        "tab_wait_mc",
                        stalled_lane(s, "tab")
                            .map_or_else(|| wait_json(tab.wait, "not_sampled"), Jv::stalled),
                    ),
                    (
                        "tab_starved_pm",
                        Jv::why("unavailable:execution_window_unknown"),
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
                "thermal",
                s.thermal
                    .map_or_else(|| Jv::why("unsupported"), |t| Jv::s(t.word())),
            );
        }
        View::Gpu => o.set("gpu", gpu_json(q, ctx, s)),
        View::Mem => {
            if let Jv::Obj(pairs) = mem_json(q, ctx, s) {
                for (k, v) in pairs {
                    o.set(&k, v);
                }
            }
        }
        View::Disk => o.set("disk", disk_json(q, ctx, s)),
        View::Net => {
            o.set("place", place_json(s));
            o.set("net", net_json(q, s));
        }
        View::Procs => {
            let sc = scope.unwrap_or(ScopeArg::Tab);
            o.set("procs", procs_json(q, ctx, s, sc));
        }
        View::Why => why::why_json(q, ctx, s, &mut o),
        View::Report => o.set("report", super::hardware::report_json(s)),
        View::Boot => o.set("boot", super::boot::json(s.boot_cost.as_deref())),
        View::Left => super::left::left_json(q, ctx, s, &mut o),
    }
    if q.view == View::Boot {
        o.set("events", Jv::Arr(Vec::new()));
        return o;
    }
    if matches!(q.view, View::Cores | View::Procs) {
        o.set("scheduling_context", scheduling_context_json(ctx, s));
    }
    if matches!(q.view, View::Cores | View::Why) {
        o.set(
            "cpu_hardware",
            s.hardware
                .as_ref()
                .map_or_else(|| Jv::why("not_sampled"), super::hardware::sample_json),
        );
        if let Some(b) = &s.cpu_diagnostics {
            o.set("cpu_diagnostic_bundle", super::hardware::bundle_json(b));
        }
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

/// Every lane stalled at `s`: `{"lane","state","call","pid","name","since","since_unix_ms","for_ms","figures"}`.
fn lanes_json(s: &Snapshot) -> Jv {
    Jv::Arr(
        s.stalled
            .iter()
            .map(|l| {
                let mut o = Jv::obj([
                    ("lane", Jv::s(l.lane)),
                    ("state", Jv::s("stalled")),
                    ("call", Jv::s(l.call)),
                ]);
                if let Some(pid) = l.pid {
                    o.set("pid", Jv::u(u64::from(pid)));
                }
                if let Some(name) = l.name.as_deref() {
                    o.set("name", Jv::s(exe_word(name)));
                }
                let since = u64::try_from(l.since_unix_ms).unwrap_or(0);
                o.set("since", Jv::s(clock_ms(since)));
                o.set("since_unix_ms", Jv::u(since));
                o.set("for_ms", Jv::u(l.for_ms));
                o.set(
                    "figures",
                    Jv::Arr(l.figures().iter().map(|f| Jv::s(*f)).collect()),
                );
                o
            })
            .collect(),
    )
}

fn aterm_json(s: &Snapshot) -> Jv {
    let Some(a) = s.aterm.as_ref() else {
        return unknown_json(s, "aterm", "warming");
    };
    let mut o = fig_json(&Fig::of(a));
    o.set("cpu_mc", Jv::u(mc(aterm_cpu(s).unwrap_or(a.cores.raw))));
    cpu_metadata(
        &mut o,
        cpu_read_span_json(s, cpu_timing(s).map(|(_, span, _)| span)),
    );
    o.set("cpu_coverage", Jv::s(cpu_coverage(s)));
    if let Some(cpu) = &s.scoped_cpu {
        o.set(
            "cpu_charge_mc",
            Jv::Int((cpu.all_charge() * 1000.0).round() as i64),
        );
        o.set(
            "cpu_ambiguous",
            Jv::Bool(cpu.own_ambiguous || cpu.tabs.iter().any(|t| t.ambiguous)),
        );
        o.set(
            "coalition_reported_mc",
            cpu.coalition_reported.map_or_else(
                || Jv::why("not_sampled"),
                |v| Jv::Int((v * 1000.0).round() as i64),
            ),
        );
    }
    o.set("gpu_pm", Jv::known_or(a.gpu.map(gpu_pm), GPU_LANE_OFF));
    o.set("instance", Jv::u(u64::from(s.self_pid)));
    if !s.native_terminals.is_empty() {
        o.set("native_waits", native_waits_json(s));
    }
    if let Some(shared) = &s.shared {
        let mut slice = fig_json(&Fig::of(shared));
        slice.set("included_in", Jv::s("aterm"));
        cpu_metadata(&mut slice, cpu_read_span_json(s, s.procs_window_ns));
        o.set("shared_app", slice);
    }
    o
}

fn native_waits_json(s: &Snapshot) -> Jv {
    let known = s
        .native_terminals
        .iter()
        .filter_map(|t| t.wait_cpu_ns)
        .fold(0_u64, u64::saturating_add);
    let unread = s
        .native_terminals
        .iter()
        .filter(|t| t.wait_cpu_ns.is_none())
        .count();
    Jv::obj([
        ("scope", Jv::s("owning_wait_total")),
        ("quantity", Jv::s("kernel_counter_charge")),
        ("completed", Jv::u(s.native_terminals.len() as u64)),
        (
            "wait_cpu_ns",
            if unread > 0 {
                Jv::at_least_refused(known, unread as u32)
            } else {
                Jv::u(known)
            },
        ),
        (
            "rows",
            Jv::Arr(
                s.native_terminals
                    .iter()
                    .take(8)
                    .map(|t| {
                        Jv::obj([
                            ("pid", Jv::u(u64::from(t.process.pid))),
                            ("start", Jv::known_or(t.process.start, "not_sampled")),
                            ("seq", Jv::u(t.seq)),
                            ("local", Jv::known_or(t.tab, "gone")),
                            (
                                "timing",
                                Jv::obj([
                                    ("clock", Jv::s("continuous_boot_ns")),
                                    ("exited_ns", Jv::known_or(t.timing.exited_ns, "not_sampled")),
                                    (
                                        "previous_cut_ns",
                                        Jv::known_or(
                                            t.timing.previous.continuous_ns,
                                            "not_sampled",
                                        ),
                                    ),
                                    (
                                        "current_cut_ns",
                                        Jv::known_or(t.timing.current.continuous_ns, "not_sampled"),
                                    ),
                                ]),
                            ),
                            ("wait_cpu_ns", Jv::known_or(t.wait_cpu_ns, "not_sampled")),
                            ("own_cpu_ns", Jv::known_or(t.process.cpu_ns, "not_sampled")),
                            (
                                "child_cpu_ns",
                                Jv::known_or(t.process.child_cpu_ns, "not_sampled"),
                            ),
                            (
                                "reaps_unwaited",
                                t.process
                                    .reaps_unwaited
                                    .map_or_else(|| Jv::why("not_sampled"), Jv::Bool),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "more",
            Jv::u(s.native_terminals.len().saturating_sub(8) as u64),
        ),
    ])
}

fn parts_json(ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    if s.aterm.is_none() {
        return unknown_json(s, "aterm", "warming");
    }
    let mut parts = Vec::new();
    for t in &s.tabs {
        let mut p = fig_json(&Fig::of(&t.scope));
        p.set("name", Jv::s("tab"));
        p.set("local", Jv::u(t.key));
        p.set(
            "tab",
            ctx.tab_number(t.key)
                .map_or_else(|| Jv::why("gone"), |n| Jv::u(n as u64)),
        );
        if let Some(f) = ctx.tabs.iter().find(|f| f.key == t.key) {
            p.set("sid", Jv::s(f.sid.clone()));
        }
        tab_cpu_json(s, &[t.key], &mut p);
        parts.push(p);
    }
    if let Some(cpu) = &s.scoped_cpu {
        let mut own = Jv::obj([
            ("name", Jv::s("shared")),
            (
                "cpu_mc",
                Jv::Int((cpu.own_reported * 1000.0).round() as i64),
            ),
            (
                "cpu_charge_mc",
                Jv::Int((cpu.own_charge * 1000.0).round() as i64),
            ),
            ("cpu_ambiguous", Jv::Bool(cpu.own_ambiguous)),
        ]);
        cpu_metadata(&mut own, Jv::u(cpu.window_ns));
        parts.push(own);
    }
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
                    Jv::known_or(cpu.current_cluster_busy(c).map(mc), "no_counter_delta"),
                ),
                ("req_mhz", Jv::why("not_sampled:requested_frequency")),
                ("split_mc", Jv::why("not_sampled")),
                (
                    "exact",
                    cpu.current_cluster_busy(c)
                        .map_or_else(|| Jv::why("no_counter_delta"), |_| Jv::s("exact")),
                ),
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
                        Jv::known_or(
                            cpu.cores
                                .get(*core)
                                .and_then(crate::model::CoreView::current)
                                .map(|load| (load * 1000.0).round().clamp(0.0, 1000.0) as u64),
                            "no_counter_delta",
                        ),
                    ),
                    ("req_mhz", Jv::why("not_sampled:requested_frequency")),
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

fn gpu_device_json(s: &Snapshot) -> Jv {
    s.gpu_sample().map_or_else(
        || Jv::why("unsupported"),
        |g| {
            Jv::known_or(
                g.device_pct.map(|pct| u64::from(pct.min(100)) * 10),
                "not_sampled:device_utilization",
            )
        },
    )
}

fn gpu_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let Some(g) = s.gpu_sample() else {
        return Jv::why("unsupported");
    };
    let pm = |p: Option<u8>| Jv::known_or(p.map(|v| u64::from(v.min(100)) * 10), "unsupported");
    let mut o = Jv::obj([
        ("device_pm", gpu_device_json(s)),
        ("renderer_pm", pm(g.renderer_pct)),
        ("tiler_pm", pm(g.tiler_pct)),
        ("in_use_bytes", Jv::known_or(g.in_use_bytes, "unsupported")),
        ("cores", Jv::known_or(g.cores.map(u64::from), "unsupported")),
        ("split_pm", gpu_split_json(ctx, s)),
        ("clients", Jv::why(GPU_LANE_OFF)),
        (
            "exact",
            if g.device_pct.is_some() {
                Jv::s("exact")
            } else {
                Jv::why("not_sampled:device_utilization")
            },
        ),
    ]);
    if let Some(n) = g.name.as_ref() {
        o.set("name", Jv::s(n.clone()));
    }
    if s.gpu.as_ref().is_some_and(|g| g.split.is_some()) {
        let (rows, withheld) = gpu_clients(ctx, s);
        o.set(
            "clients",
            Jv::Arr(
                rows.iter()
                    .take(q.top)
                    .map(|r| {
                        let mut c = Jv::obj([
                            ("pid", Jv::u(u64::from(r.pid))),
                            ("exe", Jv::s(exe_word(&r.name))),
                            ("scope", Jv::s(r.scope.word())),
                            ("share_ppm", Jv::u(gpu_ppm(r.gpu.unwrap_or(0.0)))),
                        ]);
                        if let RowScope::Tab(k) = r.scope {
                            c.set("local", Jv::u(k));
                            if let Some(n) = ctx.tab_number(k) {
                                c.set("tab", Jv::u(n as u64));
                            }
                        }
                        c
                    })
                    .collect(),
            ),
        );
        if withheld > 0 {
            o.set("withheld", Jv::u(withheld as u64));
        }
    }
    o
}

/// The device's busy time split by owner, per-mille: client time by owner, and the device less
/// the clients' sum as `unaccounted` (signed).
fn gpu_split_json(ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let Some((g, sp)) = s.gpu.as_ref().and_then(|g| Some((g, g.split?))) else {
        return Jv::why(GPU_LANE_OFF);
    };
    let tab = tabs_fig(s, &scope_keys(ctx, ScopeArg::Tab), ctx.target.key)
        .gpu
        .unwrap_or(0.0);
    let signed = |x: f32| (x * 1000.0).round() as i64;
    Jv::obj([
        ("tab", Jv::u(gpu_pm(tab))),
        ("aterm_rest", Jv::u(gpu_pm(sp.aterm - tab))),
        ("others", Jv::u(gpu_pm(sp.others))),
        ("services", Jv::u(gpu_pm(sp.services))),
        ("unaccounted", Jv::Int(signed(g.load.raw - sp.clients))),
    ])
}

/// [`mem_classes`] as JSON: `[{class, bytes}]`, an unread class's bytes an
/// unknown.
pub(super) fn classes_json(m: &crate::model::MemView, exact: bool, why: &str) -> Jv {
    Jv::Arr(
        mem_classes(m, exact)
            .iter()
            .map(|(k, b)| Jv::obj([("class", Jv::s(*k)), ("bytes", Jv::known_or(*b, why))]))
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
        ("classes", classes_json(m, exact, class_why(ctx))),
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
                    "memorystatus_level_pm",
                    Jv::known_or(
                        m.sample.memorystatus_level_pct.map(|p| u64::from(p) * 10),
                        "unsupported",
                    ),
                ),
            ]),
        ),
        (
            "swap",
            Jv::obj([
                ("used_bytes", Jv::known_or(m.sample.swap_used, "not_read")),
                ("total_bytes", Jv::known_or(m.sample.swap_total, "not_read")),
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
            (
                "footprint_bytes",
                memory_json(tab.footprint, tab.footprint_unread),
            ),
            ("footprint_unread", Jv::u(u64::from(tab.footprint_unread))),
            ("slope_bytes_per_min", Jv::why("not_sampled")),
            ("exact", Jv::s("live")),
        ]),
    );
    o.set(
        "aterm",
        Jv::obj([
            (
                "footprint_bytes",
                s.aterm.as_ref().map_or_else(
                    || unknown_json(s, "aterm", "warming"),
                    |a| memory_json(a.footprint, a.footprint_unread),
                ),
            ),
            (
                "footprint_unread",
                s.aterm.as_ref().map_or_else(
                    || unknown_json(s, "aterm", "warming"),
                    |a| Jv::u(u64::from(a.footprint_unread)),
                ),
            ),
            ("exact", Jv::s("live")),
            (
                "via",
                Jv::s(
                    if s.why.as_deref().is_some_and(|facts| {
                        facts.lane.on() && facts.at_ns == s.at_ns && !facts.split.rows.is_empty()
                    }) {
                        "coalitions_and_processes"
                    } else {
                        "processes"
                    },
                ),
            ),
        ]),
    );
    o.set(
        "owners",
        MemoryReading::of(s).map_or_else(
            || unknown_json(s, "procs", "warming"),
            |reading| reading.json(),
        ),
    );
    o.set(
        "class_overflow_bytes",
        match i64::try_from(m.sample.physical_classes(None).excess) {
            Ok(bytes) => Jv::Int(bytes),
            Err(_) => Jv::at_least(i64::MAX.unsigned_abs(), "integer_range"),
        },
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
    o.set("cpu_mc_scope", Jv::s("process_self"));
    cpu_metadata(&mut o, cpu_read_span_json(s, r.cpu_read_span_ns));
    o.set(
        "cpu_accounting",
        cpu_accounting_json(
            &r.cpu_accounting,
            ctx.machine.family,
            stalled_lane(s, "reaping"),
        ),
    );
    o.set("wait_mc", wait_json(r.wait, proc_rate_why(ctx, r)));
    o.set("footprint_bytes", Jv::known_or(r.footprint, "refused"));
    o.set("peak_bytes", Jv::known_or(r.peak, peak_why(ctx, "refused")));
    o.set(
        "write_bytes_per_s",
        Jv::known_or(r.write_bps.map(|b| b.round() as u64), proc_rate_why(ctx, r)),
    );
    o.set("gpu_ppm", Jv::known_or(r.gpu.map(gpu_ppm), GPU_LANE_OFF));
    o.set("stat", Jv::s(r.state.word()));
    o.set(
        "age_ms",
        known_or_stalled(s, r.age_ms(s.at_unix_ms), "ages", BIRTH_UNREAD),
    );
    o.set(
        "scheduling",
        scheduling_json(r.scheduling, ctx.machine.family),
    );
    o
}

fn scheduling_read<T>(read: Result<T, i32>, value: impl FnOnce(T) -> Jv) -> Jv {
    read.map_or_else(
        |errno| {
            Jv::obj([
                (
                    "unavailable",
                    Jv::why(&crate::scheduling::error_word(errno)),
                ),
                ("errno", Jv::Int(i64::from(errno))),
            ])
        },
        value,
    )
}

/// Why a process's time waiting for a core or write rate is unread ([`ProcView::wait`],
/// [`ProcView::write_bps`]): macOS (`proc_pid_rusage`) and Linux (`/proc/<pid>/io` and each
/// thread's `schedstat`) read them for this user's own processes and need a second sweep for a
/// rate (`warming`), refusing another user's (`refused`, including [`RowScope::Services`] on
/// Linux where `stat` still gives CPU time); elsewhere neither is sampled (`not_sampled`).
fn proc_rate_why(ctx: &Ctx<'_>, r: &ProcView) -> &'static str {
    match ctx.machine.family {
        "macOS" | "Linux" => {
            if r.refused || r.scope == RowScope::Services {
                "refused"
            } else {
                "warming"
            }
        }
        _ => "not_sampled",
    }
}

/// `c` as JSON; `reaping` is the lane stalled that reads whether the kernel reaps its children
/// for it, if one is.
pub(super) fn cpu_accounting_json(
    c: &crate::model::CpuAccounting,
    family: &str,
    reaping: Option<&'static str>,
) -> Jv {
    // Match raw hardware counters: decimal strings preserve every u64 bit.
    let raw = |value: Option<u64>, why: &str| {
        value.map_or_else(|| Jv::why(why), |v| Jv::s(v.to_string()))
    };
    let child_why = if family == "macOS" {
        "not_sampled"
    } else {
        "unsupported"
    };
    Jv::obj([
        ("scope", Jv::s("lifetime_counters")),
        ("self_ns", raw(c.self_ns, "refused")),
        ("reaped_children_ns", raw(c.reaped_children_ns, child_why)),
        ("child_scope", Jv::s("reaped_descendants")),
        (
            "auto_reaps_children",
            c.auto_reaps_children.map_or_else(
                || reaping.map_or_else(|| Jv::why(child_why), Jv::stalled),
                Jv::Bool,
            ),
        ),
        ("counter_at_ns", raw(c.sampled_at_ns, "not_sampled")),
        ("pid_start", raw(c.start, "not_sampled")),
        (
            "exec_version",
            c.exec_version
                .map_or_else(|| Jv::why("not_sampled"), |v| Jv::Int(i64::from(v))),
        ),
    ])
}

pub(super) fn scheduling_json(s: Option<crate::scheduling::Scheduling>, family: &str) -> Jv {
    let Some(s) = s else {
        return Jv::why(if family == "macOS" {
            "not_sampled"
        } else {
            "unsupported"
        });
    };
    Jv::obj([
        ("nice", scheduling_read(s.nice, |v| Jv::Int(i64::from(v)))),
        ("darwin_application", Jv::Bool(s.application)),
        (
            "darwin_background_requested",
            Jv::obj([
                ("internal", Jv::Bool(s.background_internal)),
                ("external", Jv::Bool(s.background_external)),
            ]),
        ),
        ("darwin_suppressed", Jv::Bool(s.suppressed)),
        (
            "darwin_role",
            scheduling_read(s.role, |v| Jv::Int(i64::from(v))),
        ),
        (
            "qos_lifetime_cpu_ns",
            s.qos_cpu_ns.map_or_else(
                || Jv::why("not_sampled"),
                |read| {
                    scheduling_read(read, |ns| {
                        Jv::obj(
                            crate::scheduling::QOS_CLASSES
                                .into_iter()
                                .zip(ns)
                                .map(|(k, v)| (k, Jv::u(v))),
                        )
                    })
                },
            ),
        ),
        ("placement", Jv::why("not_sampled")),
    ])
}

pub(crate) fn scheduling_context_json(ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    let clusters = s.cpu.as_ref().map_or_else(
        || Jv::why("warming"),
        |cpu| {
            Jv::Arr(
                cpu.clusters
                    .iter()
                    .map(|c| {
                        Jv::obj([
                            ("name", Jv::s(&c.name)),
                            ("cpus", Jv::u(c.cores.len() as u64)),
                            (
                                "busy_mc",
                                Jv::known_or(
                                    cpu.current_cluster_busy(c).map(mc),
                                    "no_counter_delta",
                                ),
                            ),
                        ])
                    })
                    .collect(),
            )
        },
    );
    Jv::obj([
        ("clusters", clusters),
        (
            "wait_mc",
            s.global.wait.map_or_else(
                || unknown_json(s, "wait", "not_sampled"),
                |w| Jv::Int((f64::from(w) * 1000.0).round() as i64),
            ),
        ),
        (
            "cores_window_ns",
            Jv::known_or(s.cores_window_ns, "warming"),
        ),
        (
            "process_window_ns",
            known_or_stalled(s, s.procs_window_ns, "procs", "warming"),
        ),
        ("finding", Jv::why("unavailable:execution_window_unknown")),
        (
            "diagnostic_executable",
            ctx.machine
                .executable
                .as_deref()
                .map_or_else(|| Jv::why("not_sampled"), Jv::s),
        ),
    ])
}

pub(crate) fn scheduling_context_text(ctx: &Ctx<'_>, s: &Snapshot, lines: &mut Vec<Line>) {
    lines.push(line(
        "scheduling-context",
        format!(
            "scheduling_context {}",
            scheduling_context_json(ctx, s).to_text()
        ),
    ));
    if let Some(exe) = &ctx.machine.executable {
        lines.push(note("diagnostic-client", format!("diagnostic executable={} (this running instance); use this executable for ctl if PATH names an older development copy", pct(exe))));
    }
}

/// One program in its scope: its oldest member's age, a lower bound while a member's birth
/// went unread.
fn program_json(ctx: &Ctx<'_>, s: &Snapshot, g: &ProgramGroup) -> Jv {
    let mut o = Jv::obj([
        ("exe", Jv::s(exe_word(&g.name))),
        (
            "scope",
            Jv::s(match g.scope {
                ProcScope::Tab(_) => "tab",
                ProcScope::Aterm => "aterm",
                ProcScope::Shared => "shared-aterm",
                ProcScope::Other => "other",
            }),
        ),
    ]);
    if let ProcScope::Tab(k) = g.scope {
        o.set("local", Jv::u(k));
        if let Some(n) = ctx.tab_number(k) {
            o.set("tab", Jv::u(n as u64));
        }
    }
    o.set("n", Jv::u(u64::from(g.n)));
    o.set(
        "age_ms",
        match (g.age_ms(s.at_unix_ms), g.unborn) {
            (Some(age), 0) => Jv::u(age),
            (Some(age), _) => Jv::at_least(age, unknown_why(s, "ages", BIRTH_UNREAD)),
            (None, _) => unknown_json(s, "ages", BIRTH_UNREAD),
        },
    );
    o
}

fn procs_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot, scope: ScopeArg) -> Jv {
    if s.procs_window_ns.is_none() {
        return stalled_lane(s, "procs").map_or_else(|| Jv::why("warming"), Jv::stalled);
    }
    let rows = procs_in_scope(ctx, s, scope);
    let unread = rows.iter().filter(|r| r.refused).count();
    Jv::obj([
        ("scope", Jv::s(scope.word())),
        ("cpu_mc_scope", Jv::s("process_self")),
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
        (
            "programs",
            Jv::Arr(
                programs_in_scope(ctx, s, scope)
                    .into_iter()
                    .take(q.top)
                    .map(|g| program_json(ctx, s, g))
                    .collect(),
            ),
        ),
    ])
}

fn disk_json(q: &SysQuery, ctx: &Ctx<'_>, s: &Snapshot) -> Jv {
    if q.form == Some(DiskForm::Held) {
        return Jv::obj([("held", held::json(ctx, s))]);
    }
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
            o.set(
                "scopes",
                Jv::Arr(r.scopes.iter().map(|s| Jv::s(s.clone())).collect()),
            );
            if let Some(pool) = r.pool {
                o.set("pool", Jv::s(pool));
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
                        (
                            "walked_age_s",
                            Jv::known_or(r.size.sizes().and_then(|s| s.age_s), "not_sampled"),
                        ),
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
                ("reserved_bytes", Jv::u(v.reserved)),
                ("total_bytes", Jv::u(v.total)),
                ("slope_bytes_per_s", Jv::why("not_sampled")),
                ("eta_full_s", Jv::why("not_sampled")),
                ("purgeable_bytes", Jv::why(purgeable_why(ctx))),
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
    if let Some(t) = ctx.disk {
        o.set(
            "scopes",
            Jv::Arr(t.scopes.iter().map(disk::ScopeLine::json).collect()),
        );
        let (panes, withheld) = disk_panes(ctx, t);
        o.set(
            "panes",
            Jv::Arr(
                panes
                    .iter()
                    .map(|p| p.json(ctx.path_ok(&[p.key]), ctx.home, &|e| exe_word(e).to_owned()))
                    .collect(),
            ),
        );
        if withheld > 0 {
            o.set("panes_withheld", Jv::u(withheld as u64));
        }
    }
    disk::totals_json(ctx.disk, &mut o);
    o.set(
        "walking",
        ctx.disk
            .map_or_else(|| Jv::why("not_sampled"), |t| Jv::Bool(t.walking)),
    );
    o.set(
        "write_bytes_per_s",
        known_or_stalled(
            s,
            f.write_bps.map(|b| b.round() as u64),
            "writes",
            "not_sampled",
        ),
    );
    o.set("writers", Jv::Arr(writers));
    if withheld > 0 {
        o.set("withheld", Jv::u(withheld as u64));
    }
    let covered_s = s.facts.writes_covered_s;
    let (programs, withheld) = disk_programs(ctx, s, scope);
    let programs = programs
        .iter()
        .take(q.top)
        .map(|p| {
            let mut row = Jv::obj([
                ("exe", Jv::s(exe_word(&p.name))),
                ("n", Jv::u(u64::from(p.n_max))),
                ("scope", Jv::s(program_scope_word(p.scope))),
            ]);
            if let ProcScope::Tab(k) = p.scope {
                row.set("local", Jv::u(k));
                if let Some(n) = ctx.tab_number(k) {
                    row.set("tab", Jv::u(n as u64));
                }
            }
            row.set(
                "write_bytes_per_h",
                Jv::known_or(program_per_hour(p, covered_s), "warming"),
            );
            row.set("covered_s", Jv::u(covered_whole_s(covered_s)));
            row
        })
        .collect();
    o.set("programs", Jv::Arr(programs));
    if withheld > 0 {
        o.set("programs_withheld", Jv::u(withheld as u64));
    }
    o.set("volume", volume);
    o.set(
        "stores",
        Jv::Arr(ctx.stores.iter().map(stores::StoreFact::json).collect()),
    );
    o.set("store_warnings", stores::warnings_json(ctx.stores));
    o.set("budget", budget_json(ctx.volume.map(|v| v.budget)));
    o
}

fn link_rates_json(r: crate::model::LinkRates) -> Jv {
    Jv::obj([
        ("rx_bytes_per_s", Jv::u(r.rx_bps.round() as u64)),
        ("tx_bytes_per_s", Jv::u(r.tx_bps.round() as u64)),
        ("rx_packets_per_s", Jv::u(r.rx_pps.round() as u64)),
        ("tx_packets_per_s", Jv::u(r.tx_pps.round() as u64)),
        ("rx_errors_per_s", Jv::u(r.rx_err_ps.round() as u64)),
        ("tx_errors_per_s", Jv::u(r.tx_err_ps.round() as u64)),
        ("rx_drops_per_s", Jv::u(r.rx_drop_ps.round() as u64)),
        (
            "tx_drops_per_s",
            Jv::known_or(r.tx_drop_ps.map(|v| v.round() as u64), "unsupported"),
        ),
    ])
}

/// `n` of a counter-read state: exact, or, with only the counters read
/// ([`crate::model::TcpCounts::counters_only`]), at most `n`, since the counter also counts what
/// `cause` names (`CurrEstab` the `CLOSE_WAIT` connections, `tw` the orphaned `FIN_WAIT2` sockets
/// held as timewait ones).
fn tcp_counter_json(n: u32, counters_only: bool, cause: &str) -> Jv {
    if counters_only {
        Jv::at_most(u64::from(n), cause)
    } else {
        Jv::u(u64::from(n))
    }
}

/// A reading's TCP counts; with only the counters read, every other state is `not_sampled` and
/// the two counted ones are bounds ([`tcp_counter_json`]).
fn tcp_counts_json(t: crate::model::TcpCounts) -> Jv {
    let other = |n: u32| Jv::known_or(t.other(n).map(u64::from), "not_sampled");
    Jv::obj([
        ("total", other(t.total())),
        (
            "established",
            tcp_counter_json(t.established, t.counters_only, "close_wait_counted"),
        ),
        ("syn_sent", other(t.syn_sent)),
        ("syn_recv", other(t.syn_recv)),
        ("fin_wait1", other(t.fin_wait1)),
        ("fin_wait2", other(t.fin_wait2)),
        (
            "time_wait",
            tcp_counter_json(t.time_wait, t.counters_only, "fin_wait2_counted"),
        ),
        ("close", other(t.close)),
        ("close_wait", other(t.close_wait)),
        ("last_ack", other(t.last_ack)),
        ("listen", other(t.listen)),
        ("closing", other(t.closing)),
        ("connecting", other(t.connecting())),
        ("closing_total", other(t.closing_total())),
    ])
}

/// A [`crate::model::PlaceValue`] as JSON.
fn place_value_json(value: &crate::model::PlaceValue) -> Jv {
    use crate::model::PlaceValue;
    match value {
        PlaceValue::Text(text) => Jv::s(text.clone()),
        PlaceValue::Count(n) => Jv::u(*n),
        PlaceValue::Flag(flag) => Jv::Bool(*flag),
        PlaceValue::Absent(why) => Jv::why(why),
        PlaceValue::Object(fields) => Jv::obj(
            fields
                .iter()
                .map(|(key, value)| (*key, place_value_json(value))),
        ),
    }
}

/// The `place` object of `sys --json`'s overview, global and net views: the host's
/// [`crate::model::PlaceView`] as it came, or `{"why":"not_sampled"}` with none.
fn place_json(s: &Snapshot) -> Jv {
    s.place.as_ref().map_or_else(
        || Jv::why("not_sampled"),
        |place| {
            Jv::obj(
                place
                    .fields
                    .iter()
                    .map(|(key, value)| (*key, place_value_json(value))),
            )
        },
    )
}

fn net_json(q: &SysQuery, s: &Snapshot) -> Jv {
    let Some(net) = s.net.as_ref() else {
        return Jv::why("not_sampled");
    };
    let rates_why = net.rates_why.map_or_else(
        || {
            if net.links.is_empty() {
                "not_sampled"
            } else {
                "warming"
            }
        },
        |why| why.word(),
    );
    let primary = s
        .facts
        .link
        .as_ref()
        .map_or_else(|| Jv::why("not_sampled"), |l| Jv::s(l.name.clone()));
    let links: Vec<Jv> = net
        .links
        .iter()
        .take(q.top)
        .map(|l| {
            let x = &l.sample;
            Jv::obj([
                ("name", Jv::s(x.name.clone())),
                ("kind", Jv::s(x.kind.word())),
                ("up", Jv::Bool(x.up)),
                ("loopback", Jv::Bool(x.loopback)),
                ("counted", Jv::Bool(x.counts)),
                (
                    "primary",
                    Jv::Bool(s.facts.link.as_ref().is_some_and(|p| p.name == x.name)),
                ),
                ("rx_bytes", Jv::u(x.rx)),
                ("tx_bytes", Jv::u(x.tx)),
                ("byte_counter", Jv::s(x.byte_counter.word())),
                (
                    "byte_counter_bits",
                    Jv::known_or(x.byte_counter.bits().map(u64::from), "not_sampled"),
                ),
                (
                    "byte_counter_quantum_bytes",
                    Jv::known_or(x.byte_counter.quantum(), "not_sampled"),
                ),
                ("rx_packets", Jv::u(x.rx_packets)),
                ("tx_packets", Jv::u(x.tx_packets)),
                ("rx_errors", Jv::u(x.rx_errors)),
                ("tx_errors", Jv::u(x.tx_errors)),
                ("rx_drops", Jv::u(x.rx_drops)),
                ("tx_drops", Jv::known_or(x.tx_drops, "unsupported")),
                ("link_bps", Jv::known_or(x.rate_bps, "unsupported")),
                (
                    "per_s",
                    l.rates.map_or_else(
                        || Jv::why(l.rates_why.map_or("warming", |why| why.word())),
                        link_rates_json,
                    ),
                ),
            ])
        })
        .collect();
    Jv::obj([
        ("state", Jv::s(net.state.word())),
        ("primary_link", primary),
        ("srtt_us", Jv::known_or(srtt_us(net.srtt_ms), "not_sampled")),
        (
            "aterm_sockets",
            Jv::known_or(net.aterm_sockets.map(u64::from), "not_sampled"),
        ),
        (
            "per_s",
            net.rates
                .map_or_else(|| Jv::why(rates_why), link_rates_json),
        ),
        (
            "tcp",
            net.tcp
                .map_or_else(|| Jv::why("not_sampled"), tcp_counts_json),
        ),
        ("links", Jv::Arr(links)),
        ("more", Jv::u(net.links.len().saturating_sub(q.top) as u64)),
    ])
}

fn srtt_us(srtt_ms: Option<f32>) -> Option<u64> {
    srtt_ms
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map(|ms| (f64::from(ms) * 1000.0).round() as u64)
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
