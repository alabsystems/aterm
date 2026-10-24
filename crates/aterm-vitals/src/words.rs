// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Human-readable formatting, amount ladders, Details clauses, and spoken sentences for system vitals.

use crate::explain::{Context, deck_cause};
use crate::history::Filling;
use crate::jobs::Verdict;
use crate::model::{
    CapBasis, CpuView, DiskLevel, HeatCell, LinkKind, NoFigure, Pressure, ProcScope, Resource,
    ResourceView, RowFacts, ScopeFig, Snapshot, State, TabView, VolumeSample,
};
use crate::peaks::{NET_LEARN_S, TOKENS_LEARN_S};
use crate::ring::BUCKET_MS;
use crate::trend::Motion;

/// Noise gate for CPU core-count motion (`0.1` cores).
pub const CORES_GATE: f32 = 0.1;
/// Noise gate for memory footprint motion (`0.05` GiB).
pub const FOOTPRINT_GATE_GIB: f32 = 0.05;
/// Noise gate for single-core or GPU busy fraction (`0.04`).
pub const LOAD_GATE: f32 = 0.04;

/// Format a CPU core count to one decimal place.
#[must_use]
pub fn cores(c: f32) -> String {
    format!("{:.1}", c.max(0.0))
}

/// Format memory bytes in binary `MiB` or `GiB`.
#[must_use]
pub fn mem(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.0} MiB", b / MIB)
    } else {
        format!("{:.0} KiB", b / 1024.0)
    }
}

/// Format bytes as `GiB` to one decimal place without a unit suffix.
#[must_use]
pub fn gib_figure(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / f64::from(1u32 << 30))
}

/// Format physical memory total in `GiB` (integer when exact).
#[must_use]
pub fn gib_total(bytes: u64) -> String {
    let g = bytes as f64 / f64::from(1u32 << 30);
    if (g - g.round()).abs() < 0.05 {
        format!("{g:.0}")
    } else {
        format!("{g:.1}")
    }
}

/// Format disk bytes in decimal `GB` or `TB`.
#[must_use]
pub fn disk(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 100e9 {
        format!("{:.0} GB", b / 1e9)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else {
        format!("{:.0} MB", b / 1e6)
    }
}

fn motion_suffix(m: Motion) -> String {
    m.word().map(|w| format!(", {w}")).unwrap_or_default()
}

/// Format the Global scope summary sentence.
#[must_use]
pub fn global_sentence(s: &Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(cpu) = s.cpu.as_ref() {
        parts.push(format!(
            "{} of {} cores busy{}",
            cores(cpu.busy.level),
            cpu.total(),
            motion_suffix(cpu.busy.motion(CORES_GATE))
        ));
    }
    if let Some(g) = s.gpu.as_ref() {
        parts.push(format!("GPU {:.0}%", (g.load.raw * 100.0).round()));
    }
    if let Some(m) = s.mem.as_ref() {
        let used = m.sample.used();
        let mut w = format!(
            "memory {} of {} GiB used",
            gib_figure(used),
            gib_total(m.sample.total)
        );
        if m.sample.pressure != Pressure::Unknown {
            w.push_str(&format!(", pressure {}", m.sample.pressure.word()));
        }
        parts.push(w);
        if m.sample.swap_used == 0 {
            parts.push("swap 0".into());
        } else {
            parts.push(format!("swap {}", mem(m.sample.swap_used)));
        }
    }
    if let Some(v) = s.volume.as_ref() {
        parts.push(format!("disk {} free", disk(v.available)));
    }
    if parts.is_empty() {
        return "Global: not read yet.".into();
    }
    format!("Global: {}.", parts.join("; "))
}

/// Format the `aterm` scope summary sentence.
#[must_use]
pub fn aterm_sentence(s: &Snapshot) -> String {
    let Some(a) = s.aterm.as_ref() else {
        return "aterm: measuring.".into();
    };
    let mut first = format!("{} cores", cores(a.cores.level));
    if let Some(busy) = s.cpu.as_ref().map(|c| c.busy.level).filter(|b| *b > 0.05) {
        let share = (a.cores.level / busy * 100.0).round().clamp(0.0, 100.0);
        first.push_str(&format!(", {share:.0}% of busy"));
    }
    let mut parts = vec![first];
    if let Some(f) = a.footprint {
        parts.push(format!(
            "{}{}",
            mem(f),
            motion_suffix(a.footprint_gib.motion(FOOTPRINT_GATE_GIB))
        ));
    }
    parts.push(format!(
        "{} process{}",
        a.procs,
        if a.procs == 1 { "" } else { "es" }
    ));
    format!("aterm: {}.", parts.join("; "))
}

/// Format a single tab's summary sentence.
#[must_use]
pub fn tab_sentence(t: &TabView) -> String {
    let mut parts = Vec::new();
    if !t.program.is_empty() {
        parts.push(t.program.clone());
    }
    parts.push(format!(
        "{} cores{}",
        cores(t.scope.cores.level),
        motion_suffix(t.scope.cores.motion(CORES_GATE))
    ));
    if let Some(f) = t.scope.footprint {
        parts.push(mem(f));
    }
    if let Some((name, c)) = t.scope.top.as_ref().filter(|(_, c)| *c >= 0.1)
        && *name != t.program
    {
        parts.push(format!("busiest {name} {}c", cores(*c)));
    }
    format!("{}: {}.", t.label, parts.join("; "))
}

/// Format the compact Deck CPU figure (`3.1c/10▴`).
#[must_use]
pub fn cores_figure(level: f32, total: Option<usize>, motion: Motion) -> String {
    let mut s = format!("{}c", cores(level));
    if let Some(n) = total {
        s.push_str(&format!("/{n}"));
    }
    if let Some(g) = motion.glyph() {
        s.push(g);
    }
    s
}

const GIB: f64 = 1_073_741_824.0;
const MIB: f64 = 1_048_576.0;
const DEC_UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];

/// Target OS platform for platform-specific resource wording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Platform {
    /// macOS.
    #[default]
    MacOs,
    /// Linux: the load average counts threads waiting on disk too, the process limits count threads, and no process's write counter is read.
    Linux,
    /// Windows.
    Windows,
}

impl Platform {
    /// Return the [`Platform`] of the current build target.
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::MacOs
        }
    }

    /// Platform display name (`"macOS"`, `"Linux"`, `"Windows"`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        }
    }

    /// Machine noun phrase (`"this Mac"` or `"this computer"`).
    #[must_use]
    pub const fn machine(self) -> &'static str {
        match self {
            Self::MacOs => "this Mac",
            Self::Linux | Self::Windows => "this computer",
        }
    }

    const fn counts_threads(self) -> bool {
        matches!(self, Self::Linux)
    }
}

/// What other users' processes are called on the OS family `family` (`macOS`, [`Platform::name`]).
#[must_use]
pub fn services_word(family: &str) -> &'static str {
    match family {
        "macOS" => "macOS services",
        "Windows" => "Windows services",
        _ => "system services",
    }
}

/// Platform wording and local UTC offset for formatting vitals text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Locale {
    /// Whose wording.
    pub platform: Platform,
    /// The local time's offset from UTC, seconds east: times print as local `HH:MM`.
    pub utc_offset_s: i64,
}

/// Semantic ink role for a vitals phrase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InkKind {
    /// Primary text.
    #[default]
    Words,
    /// Secondary text: a qualifier, or a fact with nothing to act on.
    Words2,
    /// A warning.
    Warn,
    /// A critical state.
    Critical,
    /// A job's attention marks (⊘ ◔ ◇).
    Stuck,
    /// Over the resource's maximum.
    Over,
}

/// Vector status mark drawn before a phrase or in a row's well.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    /// ◢: the disk is low, filling or stuck.
    Low,
    /// ●: memory pressure, or a GPU reset.
    Dot,
    /// ⊘: a stuck job.
    Stuck,
    /// ◔: a job gone quiet.
    Quiet,
    /// ☠: zombies left unreaped.
    Zombie,
    /// ◇: a job waiting for the person.
    Waiting,
    /// A drawn pause: a job stopped by job control.
    Stopped,
    /// ↑: sending.
    Up,
    /// ↓: receiving.
    Down,
    /// ∵: the cause aterm names (`explain`'s Deck clause).
    Cause,
}

/// Amount-column width ladder (`Det`, `Cols`, `NoCols`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ladder {
    /// Details shows (wide, mid with Details, fwide with Details).
    Det,
    /// The aterm and Tab columns show but Details does not (mid, narrow, fwide).
    Cols,
    /// No scope columns (slim, fmid, fnarrow).
    NoCols,
}

/// Single Details column clause with priority, folding forms, spoken text, and mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause {
    /// Its forms, longest first.
    pub forms: Vec<String>,
    /// The clause as a sentence a screen reader says and ⌘C copies.
    pub spoken: String,
    /// Its priority, 0 first: rule D folds the highest number first.
    pub prio: u8,
    /// A state clause: it leads Details and never folds.
    pub keep: bool,
    /// The mark drawn before it.
    pub mark: Option<Mark>,
    /// Its ink.
    pub ink: InkKind,
}

impl Clause {
    fn new(prio: u8, forms: Vec<String>) -> Self {
        let spoken = sentence(&speak(forms.first().map_or("", String::as_str)));
        Self {
            forms,
            spoken,
            prio,
            keep: false,
            mark: None,
            ink: InkKind::Words,
        }
    }

    fn kept(mut self) -> Self {
        self.keep = true;
        self
    }

    fn marked(mut self, mark: Option<Mark>) -> Self {
        self.mark = mark;
        self
    }

    fn inked(mut self, ink: InkKind) -> Self {
        self.ink = ink;
        self
    }

    fn said(mut self, spoken: String) -> Self {
        self.spoken = spoken;
        self
    }
}

/// Row state summary words for layouts without Details and for the `fslim` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateWords {
    /// The state the words say: the row's worst.
    pub state: State,
    /// The state's clause, longest first (rule S tiers 1 and 3).
    pub clauses: Vec<String>,
    /// The state in a word or two (tiers 2 and 4): `warn`, `low`, `full ~34 min`, `1 stuck`.
    pub word: String,
    /// The fslim header's label, drawn after the mark: `Disk low`, `Memory warn`, `1 stuck`.
    pub label: String,
    /// The amount forms drawn before the state, longest first.
    pub amount_forms: Vec<String>,
    /// The mark.
    pub mark: Mark,
    /// The ink of the mark and its words.
    pub ink: InkKind,
}

/// Candidate text forms (longest first) and ink for an `aterm` or `Tab` scope cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeCell {
    /// The forms, longest first.
    pub forms: Vec<String>,
    /// Its ink: words for a figure, words2 for a phrase saying there is none.
    pub ink: InkKind,
}

/// Scope cells for a resource row (`Each` column or a single `Span`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeForms {
    /// A cell in each column.
    Each {
        /// aterm's.
        aterm: ScopeCell,
        /// The tab's.
        tab: ScopeCell,
    },
    /// One phrase spanning both columns, where no scope can have a figure.
    Span(ScopeCell),
}

/// Whole and part figures rounded to a shared decimal unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parts {
    /// The whole's figure: `2.00`.
    pub whole: String,
    /// Each part's figure, in order: `1.11`, `0.89`.
    pub parts: Vec<String>,
    /// The unit they share: `TB`.
    pub unit: &'static str,
}

fn half_up(v: f64) -> f64 {
    (v + 0.5 + 1e-9).floor()
}

fn fixed(v: f64, dec: usize) -> String {
    let scale = 10f64.powi(dec as i32);
    let r = half_up(v.max(0.0) * scale) / scale;
    format!("{r:.dec$}")
}

fn point(n: u64, dec: u32) -> String {
    if dec == 0 {
        return n.to_string();
    }
    let p = 10u64.pow(dec);
    format!("{}.{:0w$}", n / p, n % p, w = dec as usize)
}

fn sig3(bytes: u64) -> (u64, u32) {
    if bytes < 100 {
        return (bytes, 0);
    }
    let e = bytes.ilog10() - 2;
    let q = 10u128.pow(e);
    let sig = u64::try_from((u128::from(bytes) + q / 2) / q).unwrap_or(u64::MAX);
    if sig >= 1000 {
        (sig / 10, e + 1)
    } else {
        (sig, e)
    }
}

fn sig_unit(sig: u64, e: u32) -> (u32, &'static str) {
    if sig < 100 && e == 0 {
        return (0, DEC_UNITS[0]);
    }
    let lead = e + 2;
    (2 - lead % 3, DEC_UNITS[(lead / 3) as usize])
}

/// Format an integer with comma thousands separators.
#[must_use]
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn rate_figure(v: f64) -> String {
    let one = half_up(v.max(0.0) * 10.0) / 10.0;
    if one < 100.0 {
        format!("{one:.1}")
    } else {
        grouped(half_up(v) as u64)
    }
}

fn rate_lone(v: f64) -> String {
    let one = half_up(v.max(0.0) * 10.0) / 10.0;
    if one < 10.0 {
        format!("{one:.1}")
    } else {
        grouped(half_up(v) as u64)
    }
}

/// Format a standalone byte rate in decimal `MB/s`.
#[must_use]
pub fn rate_mb_lone(bytes_per_s: f64) -> String {
    format!("{} MB/s", rate_lone(bytes_per_s / 1e6))
}

/// Format a byte rate against capacity in decimal `MB/s`.
#[must_use]
pub fn rate_mb(bytes_per_s: f64) -> String {
    format!("{} MB/s", rate_figure(bytes_per_s / 1e6))
}

/// Format bytes in decimal units to three significant digits.
#[must_use]
pub fn bytes_dec(bytes: u64) -> String {
    let (sig, e) = sig3(bytes);
    let (dec, unit) = sig_unit(sig, e);
    format!("{} {unit}", point(sig, dec))
}

/// Format standalone bytes in decimal units to two significant digits.
#[must_use]
pub fn bytes_lone(bytes: u64) -> String {
    let (figure, unit) = lone(bytes);
    format!("{figure} {unit}")
}

/// `value` to two significant digits (three from 100 of its unit: `40`, `1.2`, `880`) and its
/// decimal unit ([`DEC_UNITS`]).
fn lone(value: u64) -> (String, &'static str) {
    let (sig, e) = sig3(value);
    let (dec, unit) = sig_unit(sig, e);
    if dec == 0 {
        return (point(sig, 0), unit);
    }
    let q = 10u128.pow(e + 1);
    let mut n = u64::try_from((u128::from(value) + q / 2) / q).unwrap_or(u64::MAX);
    let mut dec = dec - 1;
    if n >= 100 && dec > 0 {
        n /= 10;
        dec -= 1;
    }
    (point(n, dec), unit)
}

/// Format `whole` and `parts` in `whole`'s decimal unit so the parts sum to the whole.
#[must_use]
pub fn parts_of(whole: u64, parts: &[u64]) -> Parts {
    let (sig, e) = sig3(whole);
    let (dec, unit) = sig_unit(sig, e);
    let q = 10u128.pow(e);
    let mut n: Vec<u64> = parts
        .iter()
        .map(|&p| u64::try_from(u128::from(p) / q).unwrap_or(u64::MAX))
        .collect();
    let mut left = sig.saturating_sub(n.iter().fold(0u64, |a, &b| a.saturating_add(b)));
    let mut by_remainder: Vec<usize> = (0..parts.len()).collect();
    by_remainder.sort_by_key(|&i| std::cmp::Reverse(u128::from(parts[i]) % q));
    for i in by_remainder {
        if left == 0 {
            break;
        }
        n[i] += 1;
        left -= 1;
    }
    if let Some((last, others)) = n.split_last_mut() {
        *last = sig.saturating_sub(others.iter().fold(0u64, |a, &b| a.saturating_add(b)));
    }
    Parts {
        whole: point(sig, dec),
        parts: n.into_iter().map(|n| point(n, dec)).collect(),
        unit,
    }
}

/// Format a token or event count with `k` or `M` suffixes.
#[must_use]
pub fn tokens_k(n: f64) -> String {
    let n = n.max(0.0);
    if half_up(n) < 1000.0 {
        return format!("{:.0}", half_up(n));
    }
    for (unit, size) in [("k", 1e3), ("M", 1e6)] {
        let v = n / size;
        let one = half_up(v * 10.0) / 10.0;
        if one < 100.0 {
            return format!("{one:.1}{unit}");
        }
        if half_up(v) < 1000.0 {
            return format!("{:.0}{unit}", half_up(v));
        }
    }
    format!("{:.0}G", half_up(n / 1e9))
}

/// Format an approximate duration (`~34 min`, `~5 h`, `~2 days`).
#[must_use]
pub fn duration_approx(secs: f64) -> String {
    let secs = secs.max(0.0);
    let min = half_up(secs / 60.0).max(1.0);
    if min < 60.0 {
        return format!("~{min:.0} min");
    }
    let h = half_up(secs / 3600.0);
    if h < 24.0 {
        return format!("~{h:.0} h");
    }
    let d = half_up(secs / 86_400.0);
    format!("~{d:.0} {}", if d < 1.5 { "day" } else { "days" })
}

/// Format an elapsed time floored to the minute: `under 1 min`, `12 min`, `3 h`, `3 h 5 min`,
/// `1 day`, `2 days 4 h`. Minutes are always `min`, never a bare `m`, which reads as megabytes
/// (`sys jobs` keeps its own `12m`, [`crate::jobs::wire::age_words`]).
#[must_use]
pub fn duration(ms: u64) -> String {
    let min = ms / 60_000;
    let (d, h, m) = (min / 1440, min / 60 % 24, min % 60);
    let days = |d: u64| format!("{d} {}", plural(d, "day", "days"));
    match (d, h, m) {
        (0, 0, 0) => "under 1 min".into(),
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => days(d),
        (d, h, _) => format!("{} {h} h", days(d)),
    }
}

/// How long a program must have run before its chip gives its age ([`up_words`]): 10 minutes.
pub const LONG_RUN_MS: u64 = 10 * 60_000;

/// A program's age in words once it has run long ([`LONG_RUN_MS`]): `up 47 min`, `up 3 h 5 min`,
/// never a bare clock and minutes, which read as time left. `partial` (a member's birth went
/// unread, so the oldest may be older) says `up ≥ 47 min`. `None` while younger or unread.
#[must_use]
pub fn up_words(age_ms: Option<u64>, partial: bool) -> Option<String> {
    let age = age_ms.filter(|a| *a >= LONG_RUN_MS)?;
    Some(format!(
        "up {}{}",
        if partial { "≥ " } else { "" },
        duration(age)
    ))
}

/// A program group's age as its chip says it at `at_unix_ms` ([`up_words`]).
#[must_use]
pub fn program_age(g: &crate::model::ProgramGroup, at_unix_ms: i64) -> Option<String> {
    up_words(g.age_ms(at_unix_ms), g.unborn > 0)
}

/// Format a `[0.0, 1.0]` share for the `%` column (`0%`, `<1%`, `12%`).
#[must_use]
pub fn pct(share: Option<f32>) -> String {
    let Some(p) = share.map(|s| f64::from(s) * 100.0).filter(|p| !p.is_nan()) else {
        return "–".into();
    };
    if p <= 0.0 {
        "0%".into()
    } else if p < 0.5 {
        "<1%".into()
    } else {
        format!("{:.0}%", half_up(p).min(100.0))
    }
}

/// Format `at_unix_ms` as local `HH:MM` (with weekday prefix when on a different local day).
#[must_use]
pub fn clock(at_unix_ms: i64, now_unix_ms: i64, utc_offset_s: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let local = |ms: i64| ms.div_euclid(1000).saturating_add(utc_offset_s);
    let at = local(at_unix_ms);
    let day = at.div_euclid(86_400);
    let secs = at.rem_euclid(86_400);
    let hm = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    if day == local(now_unix_ms).div_euclid(86_400) {
        hm
    } else {
        format!("{} {hm}", WEEKDAYS[(day + 3).rem_euclid(7) as usize])
    }
}

/// Display name for resource row `res`.
#[must_use]
pub const fn resource_name(res: Resource) -> &'static str {
    match res {
        Resource::Cpu => "CPU",
        Resource::Gpu => "GPU",
        Resource::Memory => "Memory",
        Resource::Disk => "Disk",
        Resource::Network => "Network",
        Resource::Tokens => "Tokens",
        Resource::Processes => "Processes",
    }
}

fn plural(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

fn per_minute(bytes_per_s: f64) -> String {
    let per_min = bytes_per_s * 60.0;
    if per_min >= 0.95e8 {
        format!("{} GB/min", rate_figure(per_min / 1e9))
    } else {
        format!("{} MB/min", rate_figure(per_min / 1e6))
    }
}

fn of_slash(ladder: Ladder, of: String, slash: String) -> Vec<String> {
    match ladder {
        Ladder::Det => vec![of],
        Ladder::Cols => vec![of, slash],
        Ladder::NoCols => vec![slash],
    }
}

/// Candidate text forms (longest first) for a row's amount cell under `ladder`.
#[must_use]
pub fn amount_forms(
    res: Resource,
    view: &ResourceView,
    facts: &RowFacts,
    ladder: Ladder,
    loc: &Locale,
) -> Vec<String> {
    match res {
        Resource::Cpu => cpu_amount(view, ladder),
        Resource::Gpu => gpu_amount(view, ladder),
        Resource::Memory => memory_amount(view, ladder),
        Resource::Disk => disk_amount(view, freed_today(facts), ladder),
        Resource::Network => network_amount(view, ladder),
        Resource::Tokens => tokens_amount(view, ladder),
        Resource::Processes => processes_amount(view, ladder, loc.platform),
    }
}

fn cpu_amount(view: &ResourceView, ladder: Ladder) -> Vec<String> {
    let Some(now) = view.now else {
        return Vec::new();
    };
    let busy = fixed(now, 1);
    match view.capacity {
        Some(n) => of_slash(
            ladder,
            format!("{busy} of {n:.0} cores"),
            format!("{busy}/{n:.0} cores"),
        ),
        None => vec![format!("{busy} cores")],
    }
}

fn gpu_amount(view: &ResourceView, ladder: Ladder) -> Vec<String> {
    if view.now.is_none() {
        return Vec::new();
    }
    let p = pct(view.heat_now);
    let long = format!("busy {p} of the time");
    let mid = format!("busy {p} of time");
    let short = format!("busy {p}");
    match ladder {
        Ladder::Det => vec![long],
        Ladder::Cols => vec![long, mid, short],
        Ladder::NoCols => vec![mid, short],
    }
}

fn memory_amount(view: &ResourceView, ladder: Ladder) -> Vec<String> {
    let Some(now) = view.now else {
        return Vec::new();
    };
    let used = fixed(now / GIB, 1);
    let Some(total) = view.capacity else {
        return vec![format!("{used} GiB in use")];
    };
    let total = gib_total(total as u64);
    let slash = format!("{used}/{total} GiB");
    match ladder {
        Ladder::Det => vec![format!("{used} of {total} GiB in use")],
        Ladder::Cols => vec![
            format!("{used} of {total} GiB in use"),
            format!("{used} of {total} GiB"),
            slash,
        ],
        Ladder::NoCols => vec![slash],
    }
}

fn freed_today(facts: &RowFacts) -> Option<u64> {
    facts
        .disk
        .as_ref()
        .filter(|d| d.watch)
        .and_then(|d| d.freed_today.as_ref())
        .map(|f| f.0)
        .filter(|&b| b > 0)
}

fn disk_shapes(view: &ResourceView) -> Option<(Option<(String, String)>, String)> {
    let used = view.now?.max(0.0) as u64;
    let alone = format!("{} used", bytes_dec(used));
    let both = view
        .capacity
        .map(|c| c as u64)
        .filter(|&t| t > 0)
        .map(|total| {
            let p = parts_of(total, &[used, total.saturating_sub(used)]);
            let (u, t, unit) = (&p.parts[0], &p.whole, p.unit);
            (
                format!("{u} of {t} {unit} used"),
                format!("{u}/{t} {unit} used"),
            )
        });
    Some((both, alone))
}

fn disk_amount(view: &ResourceView, freed: Option<u64>, ladder: Ladder) -> Vec<String> {
    let Some((both, alone)) = disk_shapes(view) else {
        return Vec::new();
    };
    let Some((of, slash)) = both else {
        return vec![alone];
    };
    let Some(f) = freed.map(|b| format!(" · freed {}", bytes_lone(b))) else {
        return of_slash(ladder, of, slash);
    };
    match ladder {
        Ladder::Det => vec![of],
        Ladder::Cols => vec![
            format!("{of}{f}"),
            format!("{slash}{f}"),
            format!("{alone}{f}"),
            of,
            slash,
        ],
        Ladder::NoCols => vec![format!("{slash}{f}"), format!("{alone}{f}"), slash],
    }
}

fn basis_word(basis: CapBasis) -> Option<&'static str> {
    match basis {
        CapBasis::Peak => Some("peak"),
        CapBasis::Link => Some("link"),
        CapBasis::Fixed | CapBasis::Learning => None,
    }
}

fn second_binds(view: &ResourceView) -> bool {
    view.second.as_ref().is_some_and(|s| s.binds)
}

fn network_amount(view: &ResourceView, ladder: Ladder) -> Vec<String> {
    let b = view.binding();
    let Some(now) = b.now else {
        return Vec::new();
    };
    let n = rate_figure(now / 1e6);
    match (basis_word(view.basis), b.capacity) {
        (Some(word), Some(cap)) => {
            let c = rate_figure(cap / 1e6);
            of_slash(
                ladder,
                format!("{n} of {c} MB/s {word}"),
                format!("{n}/{c} MB/s {word}"),
            )
        }
        (Some("peak"), None) => vec![format!("{n} MB/s, no peak in 7 days")],
        _ => vec![format!("{n} MB/s, learning peak")],
    }
}

fn tokens_amount(view: &ResourceView, ladder: Ladder) -> Vec<String> {
    let now = view.now.map(tokens_k);
    match (view.basis, view.capacity.map(tokens_k)) {
        (CapBasis::Peak, Some(k)) => match (ladder, now) {
            (Ladder::Det, _) => vec![
                format!("aterm only, peak {k}/min"),
                format!("aterm only, peak {k}"),
            ],
            (Ladder::Cols, _) => vec![
                format!("aterm only, peak {k}/min"),
                format!("aterm only, peak {k}"),
                format!("peak {k}/min"),
            ],
            (Ladder::NoCols, Some(n)) => vec![
                format!("aterm {n}/min, peak {k}"),
                format!("aterm {n}/{k} peak"),
                format!("{n}/{k} peak"),
            ],
            (Ladder::NoCols, None) => vec![format!("aterm only, peak {k}"), format!("peak {k}")],
        },
        (CapBasis::Peak, None) => vec![
            "aterm only, no peak in 7 days".into(),
            "no peak in 7 days".into(),
        ],
        (CapBasis::Learning | CapBasis::Link, _) => match (ladder, now) {
            (Ladder::NoCols, Some(n)) => vec![
                format!("aterm {n}/min, learning"),
                format!("{n}/min, learning"),
                format!("{n}/min"),
            ],
            _ => vec![
                "aterm only · learning its peak".into(),
                "aterm only, learning peak".into(),
                "learning peak".into(),
                "learning".into(),
            ],
        },
        (CapBasis::Fixed, _) => Vec::new(),
    }
}

fn processes_amount(view: &ResourceView, ladder: Ladder, platform: Platform) -> Vec<String> {
    let b = view.binding();
    let Some(now) = b.now else {
        return Vec::new();
    };
    let n = half_up(now) as u64;
    let threads = platform.counts_threads();
    let unit = if threads { "threads" } else { "processes" };
    let Some(cap) = b.capacity.map(|c| half_up(c) as u64) else {
        return vec![format!("{} {unit}", grouped(n))];
    };
    let (n_s, c_s) = (grouped(n), grouped(cap));
    if second_binds(view) {
        let tail = if threads { "your threads" } else { "yours" };
        return of_slash(
            ladder,
            format!("{n_s} of {c_s} {tail}"),
            format!("{n_s}/{c_s} {tail}"),
        );
    }
    let of = format!("{n_s} of {c_s} {unit}");
    if threads {
        let slash = format!("{n_s}/{c_s} threads");
        let compact = format!("{}/{} threads", tokens_k(n as f64), tokens_k(cap as f64));
        match ladder {
            Ladder::Det => vec![of],
            Ladder::Cols => vec![of, slash, compact],
            Ladder::NoCols => vec![slash, compact],
        }
    } else {
        let slash = format!("{n_s}/{c_s}");
        match ladder {
            Ladder::Det => vec![of],
            Ladder::Cols => vec![of, format!("{n_s} of {c_s}"), slash],
            Ladder::NoCols => vec![slash],
        }
    }
}

/// Candidate text forms for a row's `aterm` and `Tab` scope cells.
#[must_use]
pub fn scope_forms(res: Resource, view: &ResourceView, loc: &Locale) -> ScopeForms {
    let span = |forms: Vec<String>| {
        ScopeForms::Span(ScopeCell {
            forms,
            ink: InkKind::Words2,
        })
    };
    match view.aterm.missing {
        Some(NoFigure::NotPerApp) => {
            return span(vec!["no per-app figure".into(), "not per app".into()]);
        }
        Some(NoFigure::NotReadHere) => {
            let what = if res == Resource::Disk {
                "writes"
            } else {
                "figures"
            };
            return span(vec![
                format!("{what} not read on {}", loc.platform.name()),
                format!("{what} not read"),
            ]);
        }
        Some(NoFigure::NoAgent) | None => {}
    }
    ScopeForms::Each {
        aterm: scope_cell(res, &view.aterm),
        tab: scope_cell(res, &view.tab),
    }
}

fn scope_cell(res: Resource, fig: &ScopeFig) -> ScopeCell {
    let words2 = |s: &str| ScopeCell {
        forms: vec![s.into()],
        ink: InkKind::Words2,
    };
    if fig.missing == Some(NoFigure::NoAgent) {
        return words2("no agent");
    }
    let Some(v) = fig.now else {
        return words2("–");
    };
    let forms = match res {
        Resource::Cpu => vec![format!("{} cores", fixed(v, 1))],
        Resource::Memory => {
            let g = v / GIB;
            vec![format!("{} GiB", fixed(g, if g < 0.995 { 2 } else { 1 }))]
        }
        Resource::Disk => writes_forms(v),
        Resource::Tokens => vec![format!("{}/min", tokens_k(v))],
        Resource::Processes => {
            let n = half_up(v) as u64;
            let unit = plural(n, "process", "processes");
            vec![format!("{} {unit}", grouped(n)), grouped(n)]
        }
        Resource::Gpu | Resource::Network => return words2("not per app"),
    };
    ScopeCell {
        forms,
        ink: InkKind::Words,
    }
}

fn writes_forms(bytes_per_s: f64) -> Vec<String> {
    let mb = bytes_per_s / 1e6;
    if mb < 0.05 {
        return vec!["writes 0".into()];
    }
    let mut forms = vec![format!("writes {}", rate_mb_lone(bytes_per_s))];
    if (1.0..10.0).contains(&(half_up(mb * 10.0) / 10.0)) {
        forms.push(format!("writes {} MB/s", grouped(half_up(mb) as u64)));
    }
    if half_up(mb * 10.0) >= 1000.0 {
        let gb = mb / 1000.0;
        let (long, short) = if gb < 9.995 { (2, 1) } else { (1, 0) };
        forms.push(format!("writes {} GB/s", fixed(gb, long)));
        forms.push(format!("writes {} GB/s", fixed(gb, short)));
    }
    forms
}

fn state_mark(res: Resource, state: State) -> (Mark, InkKind) {
    let level = if res == Resource::Disk {
        Mark::Low
    } else {
        Mark::Dot
    };
    match state {
        State::Critical => (level, InkKind::Critical),
        State::Warn => (level, InkKind::Warn),
        State::Stuck => (Mark::Stuck, InkKind::Stuck),
        State::Zombies => (Mark::Zombie, InkKind::Warn),
        State::Quiet => (Mark::Quiet, InkKind::Stuck),
        State::Waiting => (Mark::Waiting, InkKind::Stuck),
        State::Stopped => (Mark::Stopped, InkKind::Words2),
    }
}

/// Well mark and ink for resource row `res`.
#[must_use]
pub fn well(res: Resource, view: &ResourceView) -> Option<(Mark, InkKind)> {
    if let Some(s) = view.state {
        return Some(state_mark(res, s));
    }
    (res == Resource::Network && view.binding().now.is_some()).then(|| {
        let arrow = if second_binds(view) {
            Mark::Up
        } else {
            Mark::Down
        };
        (arrow, InkKind::Words2)
    })
}

/// Strip-spanning status sentence forms when a resource has no live cells.
#[must_use]
pub fn strip_words(res: Resource, snap: &Snapshot) -> Option<Vec<String>> {
    let view = &snap.resources[res.index()];
    match res {
        Resource::Gpu if snap.seq > 0 && view.ring.iter().all(|c| c.mean.is_none()) => Some(vec![
            "no GPU reading on this system".into(),
            "no GPU reading".into(),
        ]),
        Resource::Tokens if view.basis == CapBasis::Learning => {
            let (n, of) = (snap.facts.tokens_read_s / 60, TOKENS_LEARN_S / 60);
            Some(vec![
                format!("learning aterm's token peak: {n} of {of} min"),
                format!("learning token peak: {n} of {of} min"),
                format!("learning peak: {n} of {of} min"),
                format!("learning: {n}/{of} min"),
                "learning".into(),
            ])
        }
        _ => None,
    }
}

struct DiskSaid {
    state: State,
    forms: Vec<String>,
    spoken: String,
    word: String,
    label: String,
    prio: u8,
    mark: Option<Mark>,
}

fn disk_free(snap: &Snapshot) -> Option<u64> {
    let view = &snap.resources[Resource::Disk.index()];
    snap.volume
        .as_ref()
        .filter(|v| v.total > 0)
        .map(|v| v.available)
        .or_else(|| Some((view.capacity? - view.now?).max(0.0) as u64))
}

fn level_words(level: DiskLevel, free: Option<u64>) -> Option<(State, String, String)> {
    let free = free.map(|b| format!(", {} free", bytes_lone(b)));
    let free = free.as_deref().unwrap_or("");
    let (state, form, word) = match level {
        DiskLevel::Ok => return None,
        DiskLevel::Low => (State::Warn, format!("low{free}"), "low"),
        DiskLevel::Short => (
            State::Warn,
            format!("short{free}, aterm is cleaning"),
            "short",
        ),
        DiskLevel::Emergency => (State::Critical, format!("emergency{free}"), "emergency"),
        DiskLevel::EmergencyFiles => (
            State::Critical,
            "emergency, out of file slots".into(),
            "emergency",
        ),
    };
    Some((state, form, word.into()))
}

fn disk_states(snap: &Snapshot, loc: &Locale) -> Vec<DiskSaid> {
    let facts = &snap.facts;
    let mut out = Vec::new();
    if let Some(f) = facts.filling.filter(Filling::outranks_level) {
        let d = duration_approx(f.secs);
        let r = per_minute(f.rate);
        out.push(DiskSaid {
            state: State::Warn,
            forms: vec![format!("full in {d}, filling {r}"), format!("full in {d}")],
            spoken: sentence(&speak(&format!("disk full in {d}, filling {r}"))),
            word: format!("full {d}"),
            label: format!("Disk full {d}"),
            prio: 0,
            mark: Some(Mark::Low),
        });
    }
    let Some(w) = facts.disk.as_ref().filter(|d| d.watch) else {
        return out;
    };
    if let Some((state, form, word)) = w.level.and_then(|l| level_words(l, disk_free(snap))) {
        let lead = out.is_empty();
        out.push(DiskSaid {
            state,
            spoken: sentence(&speak(&format!("disk {form}"))),
            forms: vec![form],
            label: format!("Disk {word}"),
            word,
            prio: if lead { 0 } else { 1 },
            mark: lead.then_some(Mark::Low),
        });
    }
    for (name, level, free) in &w.others {
        if let Some((_, form, word)) = level_words(*level, Some(*free)) {
            let lead = out.is_empty();
            let form = format!("{name} {form}");
            let word = format!("{name} {word}");
            out.push(DiskSaid {
                state: State::Warn,
                spoken: sentence(&speak(&form)),
                forms: vec![form],
                label: word.clone(),
                word,
                prio: if lead { 0 } else { 1 },
                mark: lead.then_some(Mark::Low),
            });
        }
    }
    if w.stuck > 0 {
        let n = u64::from(w.stuck);
        let long = format!(
            "{n} {} stuck on a full disk",
            plural(n, "session", "sessions")
        );
        out.push(DiskSaid {
            state: State::Critical,
            spoken: sentence(&long),
            forms: vec![long, format!("{n} stuck")],
            word: format!("{n} stuck"),
            label: format!("Disk {n} stuck"),
            prio: 1,
            mark: None,
        });
    }
    if let Some(since) = w.stalled_since {
        let t = clock(since, snap.at_unix_ms, loc.utc_offset_s);
        out.push(DiskSaid {
            state: State::Warn,
            forms: vec![
                format!("disk watch stalled since {t}"),
                "watch stalled".into(),
            ],
            spoken: format!("Disk watch stalled since {t}."),
            word: "watch stalled".into(),
            label: "Disk watch stalled".into(),
            prio: 2,
            mark: None,
        });
    }
    out
}

struct JobSaid {
    state: State,
    mark: Mark,
    ink: InkKind,
    words: String,
}

fn job_marks(snap: &Snapshot) -> Vec<JobSaid> {
    let Some(jobs) = snap.jobs.as_deref() else {
        return Vec::new();
    };
    let count = |v: Verdict| jobs.rows.iter().filter(|r| r.verdict == v).count() as u64;
    let zombies: u64 = jobs
        .findings
        .zombies
        .iter()
        .filter(|z| z.tab.is_some())
        .map(|z| u64::from(z.count))
        .sum();
    [
        (State::Stuck, count(Verdict::Stuck), "stuck"),
        (
            State::Zombies,
            zombies,
            plural(zombies, "zombie", "zombies"),
        ),
        (State::Quiet, count(Verdict::Quiet), "quiet"),
        (
            State::Waiting,
            count(Verdict::WaitingYou),
            "waiting for you",
        ),
        (State::Stopped, count(Verdict::Parked), "stopped"),
    ]
    .into_iter()
    .filter(|&(_, n, _)| n > 0)
    .map(|(state, n, what)| {
        let (mark, ink) = state_mark(Resource::Processes, state);
        JobSaid {
            state,
            mark,
            ink,
            words: format!("{n} {what}"),
        }
    })
    .collect()
}

fn times_words(n: u64) -> String {
    match n {
        1 => "once".into(),
        2 => "twice".into(),
        n => format!("{n} times"),
    }
}

/// State summary words for resource row `res` when abnormal.
#[must_use]
pub fn state(res: Resource, snap: &Snapshot, loc: &Locale) -> Option<StateWords> {
    let view = &snap.resources[res.index()];
    let worst = view.state?;
    let (state, clauses, word, label) = match res {
        Resource::Memory => {
            let w = if worst == State::Critical {
                "critical"
            } else {
                "warn"
            };
            (
                worst,
                vec![format!("pressure {w}")],
                w.to_string(),
                format!("Memory {w}"),
            )
        }
        Resource::Gpu => {
            let (n, since) = snap.facts.gpu_resets?;
            let t = clock(since, snap.at_unix_ms, loc.utc_offset_s);
            (
                worst,
                vec![
                    format!("GPU reset {n}\u{d7} since {t}"),
                    format!("reset {n}\u{d7}"),
                ],
                format!("reset {n}\u{d7}"),
                format!("GPU reset {n}\u{d7}"),
            )
        }
        Resource::Disk => {
            let top = disk_states(snap, loc)
                .into_iter()
                .fold(None::<DiskSaid>, |best, s| match best {
                    Some(b) if b.state >= s.state => Some(b),
                    _ => Some(s),
                })?;
            (top.state, top.forms, top.word, top.label)
        }
        Resource::Processes => {
            let marks = job_marks(snap);
            let first = marks.first()?;
            let mut clauses = vec![first.words.clone()];
            if marks.len() > 1 {
                let all: Vec<&str> = marks.iter().map(|m| m.words.as_str()).collect();
                clauses.insert(0, all.join(", "));
            }
            (
                first.state,
                clauses,
                first.words.clone(),
                first.words.clone(),
            )
        }
        Resource::Cpu | Resource::Network | Resource::Tokens => return None,
    };
    let (mark, ink) = state_mark(res, state);
    let amount_forms = match res {
        Resource::Disk => match disk_shapes(view) {
            Some((Some((_, slash)), alone)) => vec![slash, alone],
            Some((None, alone)) => vec![alone],
            None => Vec::new(),
        },
        _ => amount_forms(res, view, &snap.facts, Ladder::NoCols, loc),
    };
    Some(StateWords {
        state,
        clauses,
        word,
        label,
        amount_forms,
        mark,
        ink,
    })
}

/// Priority-ordered Details clauses for resource row `res`.
#[must_use]
pub fn details(res: Resource, snap: &Snapshot, loc: &Locale) -> Vec<Clause> {
    let mut out = match res {
        Resource::Cpu => cpu_details(snap, loc),
        Resource::Gpu => gpu_details(snap, loc),
        Resource::Memory => memory_details(snap),
        Resource::Disk => disk_details(snap, loc),
        Resource::Network => network_details(snap),
        Resource::Tokens => tokens_details(snap, loc),
        Resource::Processes => processes_details(snap),
    };
    out.sort_by_key(|c| (c.prio, !c.keep));
    out
}

fn cpu_details(snap: &Snapshot, loc: &Locale) -> Vec<Clause> {
    let view = &snap.resources[Resource::Cpu.index()];
    let mut out = Vec::new();
    if let (Some(load), Some(n)) = (snap.facts.load1, view.capacity) {
        let l = fixed(load, 1);
        let on = format!("load {l} on {n:.0} cores");
        let waiting = half_up(load - n);
        out.push(if loc.platform != Platform::Linux && waiting >= 1.0 {
            let w = waiting as u64;
            Clause::new(0, vec![format!("{w} waiting for a core (load {l})"), on])
                .said(format!("{w} waiting for a core, load {l} on {n:.0} cores."))
        } else {
            Clause::new(0, vec![on, format!("load {l}")])
        });
    }
    out.extend(cause(snap));
    out.extend(busiest(snap));
    out
}

fn cause(snap: &Snapshot) -> Option<Clause> {
    let c = deck_cause(snap)?;
    let long = cause_words(c.word, c.context);
    Some(
        Clause::new(1, vec![long.clone(), c.word.to_owned()])
            .marked(Some(Mark::Cause))
            .said(format!("Why: {long}.")),
    )
}

/// A cause's words `base`, with the context that times it (RD §4.1).
#[must_use]
pub fn cause_words(base: &str, context: Option<Context>) -> String {
    match context {
        Some(Context::Startup) => format!("{base}, after start-up"),
        Some(Context::Wake) => format!("{base}, since wake"),
        _ => base.to_owned(),
    }
}

fn busiest(snap: &Snapshot) -> Option<Clause> {
    let g = snap.facts.busiest.as_ref().filter(|g| g.cores >= 0.05)?;
    let place = match g.scope {
        Some(ProcScope::Tab(key)) => snap.tab(key).map(|t| format!(" in {}", t.label)),
        Some(ProcScope::Aterm) => Some(" in aterm".into()),
        Some(ProcScope::Other) | None => None,
    }
    .unwrap_or_default();
    let (times, many) = if g.n > 1 {
        (format!(" \u{d7}{}", g.n), format!(", {} processes", g.n))
    } else {
        (String::new(), String::new())
    };
    let c = fixed(f64::from(g.cores), 1);
    let name = &g.name;
    Some(
        Clause::new(
            1,
            vec![
                format!("busiest {name}{times}{place}, {c} cores"),
                format!("{name}{times}{place}"),
            ],
        )
        .said(format!("Busiest {name}{many}{place}, {c} cores.")),
    )
}

fn gpu_details(snap: &Snapshot, loc: &Locale) -> Vec<Clause> {
    // A GPU with no reading has no Details (v2 §6.2).
    if snap.resources[Resource::Gpu.index()].now.is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some((n, since)) = snap.facts.gpu_resets {
        let t = clock(since, snap.at_unix_ms, loc.utc_offset_s);
        out.push(
            Clause::new(0, vec![format!("GPU reset {n}\u{d7} since {t}")])
                .kept()
                .marked(Some(Mark::Dot))
                .inked(InkKind::Warn)
                .said(format!("GPU reset {} since {t}.", times_words(n))),
        );
    }
    let gpu = snap.facts.gpu.as_ref();
    if let Some(c) = gpu.and_then(|g| g.cores) {
        out.push(Clause::new(0, vec![format!("{c} GPU cores")]));
    }
    if let Some(b) = gpu.and_then(|g| g.in_use_bytes) {
        out.push(Clause::new(
            1,
            vec![
                format!("{} GPU memory in use", mem(b)),
                format!("{} in use", mem(b)),
            ],
        ));
    }
    out
}

fn memory_details(snap: &Snapshot) -> Vec<Clause> {
    let Some(m) = snap.mem.as_ref().map(|m| &m.sample) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match m.pressure {
        Pressure::Warn | Pressure::Critical => {
            let (state, said) = if m.pressure == Pressure::Warn {
                (State::Warn, "Pressure warning.")
            } else {
                (State::Critical, "Pressure critical.")
            };
            let (mark, ink) = state_mark(Resource::Memory, state);
            out.push(
                Clause::new(0, vec![format!("pressure {}", m.pressure.word())])
                    .kept()
                    .marked(Some(mark))
                    .inked(ink)
                    .said(said.into()),
            );
        }
        Pressure::Normal => {
            out.push(Clause::new(0, vec!["pressure normal".into()]).inked(InkKind::Words2));
        }
        Pressure::Unknown => {}
    }
    if let Some(r) = snap.facts.swap_rate.filter(|r| *r >= MIB) {
        out.push(Clause::new(
            1,
            vec![format!("swapping {} MiB/s", rate_lone(r / MIB))],
        ));
    }
    if m.cached > 0 {
        out.push(Clause::new(
            2,
            vec![format!(
                "file cache {} GiB",
                fixed(m.cached as f64 / GIB, 1)
            )],
        ));
    }
    let swap = if m.swap_used == 0 {
        "swap 0".into()
    } else {
        format!("swap {}", mem(m.swap_used))
    };
    out.push(Clause::new(3, vec![swap]));
    out
}

fn disk_details(snap: &Snapshot, loc: &Locale) -> Vec<Clause> {
    let facts = &snap.facts;
    let view = &snap.resources[Resource::Disk.index()];
    let mut out: Vec<Clause> = disk_states(snap, loc)
        .into_iter()
        .map(|s| {
            Clause::new(s.prio, s.forms)
                .kept()
                .marked(s.mark)
                .inked(state_mark(Resource::Disk, s.state).1)
                .said(s.spoken)
        })
        .collect();
    if let Some(f) = facts.filling.filter(|f| !f.outranks_level()) {
        let d = duration_approx(f.secs);
        out.push(Clause::new(
            2,
            vec![
                format!("filling {}, full in {d}", per_minute(f.rate)),
                format!("full in {d}"),
            ],
        ));
    }
    if facts.disk.as_ref().is_none_or(|d| !d.watch) {
        out.push(Clause::new(2, vec!["disk watch off".into()]).inked(InkKind::Words2));
    }
    if let Some(b) = freed_today(facts) {
        let b = bytes_lone(b);
        out.push(Clause::new(
            3,
            vec![format!("aterm freed {b} today"), format!("freed {b}")],
        ));
    }
    match facts.disk_rates.rate10 {
        Some(r) => out.push(growth(r * 600.0)),
        None if view.now.is_some() => out.push(
            Clause::new(
                4,
                vec![
                    "growth after 10 min of readings".into(),
                    "growth after 10 min".into(),
                ],
            )
            .inked(InkKind::Words2),
        ),
        None => {}
    }
    let level_says_free = facts
        .disk
        .as_ref()
        .filter(|d| d.watch)
        .and_then(|d| d.level)
        .is_some_and(|l| matches!(l, DiskLevel::Low | DiskLevel::Short | DiskLevel::Emergency));
    if let Some(free) = disk_free(snap).filter(|_| !level_says_free) {
        out.push(Clause::new(5, vec![free_words(free)]));
    }
    if let Some(h) = facts.disk.as_ref().and_then(|d| d.hogs.first()) {
        let size = format!(
            "{}{}",
            if h.partial { "≥ " } else { "" },
            bytes_lone(h.bytes)
        );
        out.push(Clause::new(
            6,
            vec![
                format!("most in {}, {size}", h.path),
                format!("{} {size}", h.path),
            ],
        ));
    }
    out
}

fn growth(delta: f64) -> Clause {
    if delta.abs() < 1e8 {
        return Clause::new(4, vec!["steady".into()])
            .said("Used held steady over the last 10 minutes.".into());
    }
    let fig = bytes_lone(delta.abs() as u64);
    let (sign, verb) = if delta > 0.0 {
        ('+', "grew")
    } else {
        ('\u{2212}', "shrank")
    };
    Clause::new(4, vec![format!("{sign}{fig} in 10 min")]).said(format!(
        "Used {verb} {} in the last 10 minutes.",
        speak(&fig)
    ))
}

fn network_learning(snap: &Snapshot) -> Option<Clause> {
    let view = &snap.resources[Resource::Network.index()];
    if !matches!(view.basis, CapBasis::Link | CapBasis::Learning)
        || snap.facts.net_read_s >= NET_LEARN_S
    {
        return None;
    }
    let (h, of) = (snap.facts.net_read_s / 3600, NET_LEARN_S / 3600);
    Some(
        Clause::new(
            0,
            vec![
                format!("learning its peak: {h} of {of} h read"),
                "learning peak".into(),
            ],
        )
        .inked(InkKind::Words2)
        .said(format!(
            "Learning its peak: {h} of {of} {} read.",
            plural(u64::from(of), "hour", "hours")
        )),
    )
}

fn network_details(snap: &Snapshot) -> Vec<Clause> {
    let view = &snap.resources[Resource::Network.index()];
    let mut out: Vec<Clause> = network_learning(snap).into_iter().collect();
    let (other, mark, word, verb) = if second_binds(view) {
        (Some(view.first()), Mark::Down, "down", "Receiving")
    } else {
        (view.second.clone(), Mark::Up, "up", "Sending")
    };
    if let Some(now) = other.as_ref().and_then(|s| s.now) {
        let n = rate_figure(now / 1e6);
        let short = format!("{word} {n} MB/s");
        let (forms, said) = match (basis_word(view.basis), other.and_then(|s| s.capacity)) {
            (Some(basis), Some(cap)) => {
                let c = rate_figure(cap / 1e6);
                (
                    vec![format!("{word} {n} of {c} MB/s {basis}"), short],
                    format!("{verb} {n} of {c} megabytes per second."),
                )
            }
            _ => (vec![short], format!("{verb} {n} megabytes per second.")),
        };
        out.push(Clause::new(0, forms).marked(Some(mark)).said(said));
    }
    if let Some(l) = snap.facts.link.as_ref() {
        let kind = match l.kind {
            LinkKind::Wifi => "Wi-Fi ",
            LinkKind::Ethernet => "Ethernet ",
            LinkKind::Cellular => "Cellular ",
            LinkKind::Vpn => "VPN ",
            LinkKind::Bridge => "Bridge ",
            LinkKind::Other => "",
        };
        let name = format!("{kind}{}", l.name);
        let forms = match l.rate_bps {
            Some(bps) => vec![format!("{name}, link {}", rate_mb(bps as f64 / 8.0)), name],
            None => vec![name],
        };
        out.push(Clause::new(1, forms));
    }
    if let Some(tcp) = snap.net.as_ref().and_then(|n| n.tcp) {
        let est = grouped(u64::from(tcp.established));
        let lis = grouped(u64::from(tcp.listen));
        let conn = plural(
            u64::from(tcp.established),
            "TCP connection established",
            "TCP connections established",
        );
        out.push(
            Clause::new(
                2,
                vec![
                    format!("{est} TCP established, {lis} listening"),
                    format!("{est} TCP"),
                ],
            )
            .said(format!("{est} {conn}, {lis} listening.")),
        );
    }
    out
}

fn tokens_details(snap: &Snapshot, loc: &Locale) -> Vec<Clause> {
    let facts = &snap.facts;
    let mut out = Vec::new();
    match facts.plan.as_ref() {
        Some(plan) => {
            let p = half_up(f64::from(plan.used_x10) / 10.0);
            let t = clock(plan.read_at, snap.at_unix_ms, loc.utc_offset_s);
            let w = &plan.window_word;
            out.push(
                Clause::new(
                    0,
                    vec![
                        format!("{w} {p:.0}% used (account), read {t}"),
                        format!("{w} {p:.0}% used, {t}"),
                    ],
                )
                .said(sentence(&format!(
                    "{w} limit {p:.0} percent used on the account, read at {t}"
                ))),
            );
        }
        None => out.push(Clause::new(2, vec!["plan limit not read".into()]).inked(InkKind::Words2)),
    }
    if let Some(input) = facts.tokens_in {
        let k = tokens_k(input);
        out.push(
            Clause::new(
                1,
                vec![
                    format!("input {k}/min, cache re-reads included"),
                    format!("input {k}/min"),
                ],
            )
            .said(format!(
                "Input {} tokens per minute, cache re-reads included.",
                speak_figure(&k)
            )),
        );
    }
    out
}

fn processes_details(snap: &Snapshot) -> Vec<Clause> {
    let view = &snap.resources[Resource::Processes.index()];
    let mut out: Vec<Clause> = job_marks(snap)
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let c = Clause::new(if i == 0 { 0 } else { 1 }, vec![m.words])
                .marked(Some(m.mark))
                .inked(m.ink);
            if i == 0 { c.kept() } else { c }
        })
        .collect();
    let first = view.first();
    if let Some(second) = view.second.as_ref() {
        let (who, s) = if second.binds {
            ("all", &first)
        } else {
            ("yours", second)
        };
        if let (Some(n), Some(cap)) = (s.now, s.capacity) {
            let n = grouped(half_up(n) as u64);
            out.push(Clause::new(
                2,
                vec![
                    format!("{who} {n} of {}", grouped(half_up(cap) as u64)),
                    format!("{who} {n}"),
                ],
            ));
        }
    }
    out
}

/// Format the Disk lens `Wrong?` diagnostic line.
#[must_use]
pub fn disk_wrong(snap: &Snapshot, loc: &Locale) -> Option<String> {
    let d = snap.facts.disk.as_ref().filter(|d| d.watch)?;
    let free_bytes = disk_free(snap);
    let free = free_bytes.map(bytes_lone);
    let line = |l: Option<u64>, what: &str| {
        let free = free
            .as_deref()
            .map_or_else(|| "The free space".to_string(), |f| format!("{f} free"));
        match l {
            Some(l) => format!("{free} is under the {} {what} line.", bytes_lone(l)),
            None => format!("{free} is under the {what} line."),
        }
    };
    let filling = snap.facts.filling.map(|f| {
        let rate = if f.burst {
            "the last 5 minutes'"
        } else {
            "the last hour's"
        };
        let about = duration_approx(f.secs).replacen('~', "about ", 1);
        format!("At {rate} rate it is full in {about}.")
    });
    let fact = match d.level {
        Some(DiskLevel::Low) => Some(line(d.warn, "warning")),
        Some(DiskLevel::Short) => Some(line(d.floor, "cleaning")),
        Some(DiskLevel::Emergency) => Some(line(d.emergency, "emergency")),
        Some(DiskLevel::EmergencyFiles) => Some("The disk is out of file slots.".into()),
        Some(DiskLevel::Ok) | None => None,
    }
    .or_else(|| {
        (d.stuck > 0).then(|| {
            let n = u64::from(d.stuck);
            format!(
                "{n} {} stuck on a full disk.",
                plural(n, "session is", "sessions are")
            )
        })
    })
    .or(filling)
    .or_else(|| {
        d.stalled_since.map(|t| {
            format!(
                "The disk watch has stalled since {}.",
                clock(t, snap.at_unix_ms, loc.utc_offset_s)
            )
        })
    })
    .or_else(|| {
        d.others
            .iter()
            .find(|(_, level, _)| *level != DiskLevel::Ok)
            .map(|(name, _, b)| format!("{name} is low, {} free.", bytes_lone(*b)))
    });
    let Some(fact) = fact else {
        let mut no = String::from("No.");
        if let (Some(f), Some(bytes)) = (free.as_deref(), free_bytes) {
            match snap.facts.disk_rates.rate60 {
                Some(r) if r > 0.0 => {
                    let about = duration_approx(bytes as f64 / r).replacen('~', "about ", 1);
                    no.push_str(&format!(
                        " At the last hour's rate, {f} free lasts {about}."
                    ));
                }
                Some(_) => no.push_str(&format!(
                    " {f} free, and used did not grow in the last hour."
                )),
                None => no.push_str(&format!(" {f} free.")),
            }
        }
        no.push_str(" No session is stuck on a full disk.");
        return Some(no);
    };
    let action = match (d.level, d.floor) {
        (Some(DiskLevel::Short | DiskLevel::Emergency | DiskLevel::EmergencyFiles), _) => {
            " aterm is cleaning its build caches.".to_string()
        }
        (_, Some(floor)) => format!(
            " aterm cleans its build caches below {} free.",
            bytes_lone(floor)
        ),
        _ => String::new(),
    };
    Some(format!("Yes. {fact}{action}"))
}

/// Format the Disk lens used/free/total volume space line.
#[must_use]
pub fn disk_space_line(v: &VolumeSample) -> String {
    let free = v.available.min(v.total);
    let p = parts_of(v.total, &[v.total - free, free]);
    let u = p.unit;
    format!(
        "{} {u} used · {} {u} free · {} {u}",
        p.parts[0], p.parts[1], p.whole
    )
}

const SPOKEN_UNITS: [(&str, &str, &str); 23] = [
    ("TB", "terabytes", "terabyte"),
    ("GB", "gigabytes", "gigabyte"),
    ("MB", "megabytes", "megabyte"),
    ("kB", "kilobytes", "kilobyte"),
    ("GiB", "gibibytes", "gibibyte"),
    ("MiB", "mebibytes", "mebibyte"),
    ("MB/s", "megabytes per second", "megabyte per second"),
    ("GB/s", "gigabytes per second", "gigabyte per second"),
    ("MiB/s", "mebibytes per second", "mebibyte per second"),
    ("GB/min", "gigabytes per minute", "gigabyte per minute"),
    ("MB/min", "megabytes per minute", "megabyte per minute"),
    ("TB/h", "terabytes per hour", "terabyte per hour"),
    ("GB/h", "gigabytes per hour", "gigabyte per hour"),
    ("MB/h", "megabytes per hour", "megabyte per hour"),
    ("kB/h", "kilobytes per hour", "kilobyte per hour"),
    ("B/h", "bytes per hour", "byte per hour"),
    ("Tb/s", "terabits per second", "terabit per second"),
    ("Gb/s", "gigabits per second", "gigabit per second"),
    ("Mb/s", "megabits per second", "megabit per second"),
    ("kb/s", "kilobits per second", "kilobit per second"),
    ("b/s", "bits per second", "bit per second"),
    ("min", "minutes", "minute"),
    ("h", "hours", "hour"),
];

/// Expand abbreviations, symbols, and units in `text` for screen readers.
#[must_use]
pub fn speak(text: &str) -> String {
    let text = text.replace(" · ", ", ");
    let words: Vec<&str> = text.split(' ').collect();
    let mut out = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        let core = w.trim_end_matches([',', '.', ':']);
        let tail = &w[core.len()..];
        let one = i > 0 && words[i - 1].trim_start_matches('~') == "1";
        let said = match SPOKEN_UNITS.iter().find(|u| u.0 == core) {
            Some(&(_, many, single)) => (if one { single } else { many }).to_string(),
            None => speak_figure(core),
        };
        out.push(format!("{said}{tail}"));
    }
    out.join(" ")
}

fn speak_figure(w: &str) -> String {
    let digit = |s: &str| s.starts_with(|c: char| c.is_ascii_digit());
    if w == "≥" {
        return "at least".into();
    }
    if let Some(rest) = w.strip_prefix('~').filter(|r| digit(r)) {
        return format!("about {}", speak_figure(rest));
    }
    if let Some(rest) = w.strip_suffix("/min").filter(|r| digit(r)) {
        return format!("{} per minute", speak_figure(rest));
    }
    if let Some(rest) = w.strip_suffix('%') {
        return if rest == "<1" {
            "less than 1 percent".into()
        } else {
            format!("{rest} percent")
        };
    }
    if let Some(rest) = w.strip_prefix('<').filter(|r| digit(r)) {
        return format!("less than {}", speak_figure(rest));
    }
    if !digit(w) {
        return w.to_string();
    }
    if let Some((a, b)) = w.split_once('/') {
        return format!("{} of {}", speak_figure(a), speak_figure(b));
    }
    for (suffix, word) in [('k', "thousand"), ('M', "million"), ('G', "billion")] {
        if let Some(n) = w.strip_suffix(suffix).filter(|n| n.parse::<f64>().is_ok()) {
            return format!("{n} {word}");
        }
    }
    w.to_string()
}

fn sentence(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 1);
    let mut chars = s.chars();
    if let Some(c) = chars.next() {
        if s.starts_with("aterm") {
            out.push(c);
        } else {
            out.extend(c.to_uppercase());
        }
        out.push_str(chars.as_str());
    }
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

fn capacity_words(res: Resource, view: &ResourceView) -> &'static str {
    match (res, view.basis) {
        (Resource::Network, CapBasis::Peak) => "its 7-day peak",
        (Resource::Network, _) => "its link rate",
        (Resource::Tokens, _) => "aterm's 7-day peak",
        (Resource::Processes, _) if second_binds(view) => "your limit",
        (Resource::Processes, _) => "the limit",
        _ => "capacity",
    }
}

fn spoken_amount(res: Resource, view: &ResourceView, facts: &RowFacts, loc: &Locale) -> String {
    match res {
        Resource::Network => {
            let b = view.binding();
            let dir = if second_binds(view) {
                "uploading"
            } else {
                "downloading"
            };
            let n = rate_figure(b.now.unwrap_or(0.0) / 1e6);
            match b.capacity {
                Some(c) => format!(
                    "{n} of {} megabytes per second, {dir}",
                    rate_figure(c / 1e6)
                ),
                None => format!("{n} megabytes per second, {dir}"),
            }
        }
        Resource::Tokens => {
            let k = |v: f64| speak_figure(&tokens_k(v));
            match (view.now, view.capacity) {
                (Some(n), Some(c)) => {
                    format!("aterm only, {} of {} output tokens per minute", k(n), k(c))
                }
                (None, Some(c)) => format!("aterm only, peak {} output tokens per minute", k(c)),
                (Some(n), None) => format!("aterm only, {} output tokens per minute", k(n)),
                (None, None) => "aterm only".into(),
            }
        }
        _ => amount_forms(res, view, facts, Ladder::Det, loc)
            .first()
            .map(|a| speak(a))
            .unwrap_or_default(),
    }
}

fn scope_sentences(
    res: Resource,
    view: &ResourceView,
    snap: &Snapshot,
    panes: &[u64],
    loc: &Locale,
) -> Vec<String> {
    let (aterm, tab_cell) = match scope_forms(res, view, loc) {
        ScopeForms::Span(c) => return vec![sentence(&c.forms[0])],
        ScopeForms::Each { aterm, tab } => (aterm, tab),
    };
    let label = (!panes.is_empty()).then(|| {
        panes
            .iter()
            .find_map(|&key| snap.tab(key))
            .map_or_else(|| "this tab".to_string(), |t| t.label.clone())
    });
    let mut figures = Vec::new();
    let mut none = Vec::new();
    let scopes = [
        (Some("aterm".to_string()), &view.aterm, aterm),
        (label, &view.tab, tab_cell),
    ];
    for (who, fig, cell) in scopes {
        let Some(who) = who else { continue };
        if fig.missing == Some(NoFigure::NoAgent) {
            none.push(format!("{who} has no agent."));
        } else if fig.now.is_some() {
            figures.push(format!("{who} {}", speak(&cell.forms[0])));
        }
    }
    let mut out = Vec::new();
    if !figures.is_empty() {
        out.push(format!("{}.", figures.join(", ")));
    }
    out.extend(none);
    out
}

fn trend_sentences(view: &ResourceView, now_ms: i64, loc: &Locale) -> Vec<String> {
    const COLS: usize = 60;
    let heat = view.heat_columns(10, COLS, now_ms);
    let mean = |cells: &[HeatCell]| {
        let read: Vec<f64> = cells.iter().filter_map(|c| c.heat.map(f64::from)).collect();
        (!read.is_empty()).then(|| read.iter().sum::<f64>() / read.len() as f64 * 100.0)
    };
    let p = |v: f64| format!("{:.0}", half_up(v.max(0.0)).min(100.0));
    let mut out = Vec::new();
    if let (Some(m0), Some(m1)) = (mean(&heat[..6]), mean(&heat[COLS - 6..])) {
        out.push(if (m1 - m0).abs() < 5.0 {
            format!("Steady near {} percent.", p(m1))
        } else if m1 > m0 {
            format!("Rose from about {} percent.", p(m0))
        } else {
            format!("Fell from about {} percent.", p(m0))
        });
    }
    let peak = heat
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.max.map(|m| (i, m)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    if let (Some((i, max)), Some(now)) = (peak, view.heat_now)
        && max - now >= 0.10
    {
        let now_k = now_ms.div_euclid(BUCKET_MS);
        let at = (now_k - (COLS - 1 - i) as i64) * BUCKET_MS;
        out.push(format!(
            "Peak {} percent at {}.",
            p(f64::from(max) * 100.0),
            clock(at, now_ms, loc.utc_offset_s)
        ));
    }
    let over = heat.iter().filter(|c| c.over).count();
    if over > 0 {
        out.push(format!(
            "Over capacity {} of the last 10 minutes.",
            of_ten_minutes(over)
        ));
    }
    let unread = view
        .columns(10, COLS, now_ms)
        .iter()
        .filter(|c| c.mean.is_none())
        .count();
    if unread > 0 {
        out.push(format!(
            "Not read for {} of the last 10 minutes.",
            of_ten_minutes(unread)
        ));
    }
    out
}

fn of_ten_minutes(n: usize) -> String {
    match n {
        0..6 => format!("{} seconds", n * 10),
        6..60 => ((n + 3) / 6).min(9).to_string(),
        _ => "10".into(),
    }
}

/// Full spoken and copied accessibility summary sentence for resource row `res`, "this tab"
/// being the tab whose panes are the sessions `panes` ([`Snapshot::resource`]).
#[must_use]
pub fn row_sentence(res: Resource, snap: &Snapshot, panes: &[u64], loc: &Locale) -> String {
    let name = resource_name(res);
    if snap.seq == 0 {
        return format!("{name}: reading.");
    }
    let view = snap.resource(res, panes);
    let mut out = Vec::new();
    let learning = network_learning(snap).filter(|_| res == Resource::Network);
    if res == Resource::Tokens && view.basis == CapBasis::Learning {
        out.push(format!(
            "Tokens: learning aterm's peak, {} of {} minutes.",
            snap.facts.tokens_read_s / 60,
            TOKENS_LEARN_S / 60
        ));
    } else if view.binding().now.is_none() {
        return format!("{name}: not read.");
    } else {
        let amount = spoken_amount(res, &view, &snap.facts, loc);
        out.push(match view.heat_now {
            Some(h) => format!(
                "{name} {} of {}: {amount}.",
                speak_figure(&pct(Some(h))),
                capacity_words(res, &view)
            ),
            None => format!("{name}: {amount}."),
        });
        out.extend(learning.as_ref().map(|c| c.spoken.clone()));
    }
    out.extend(scope_sentences(res, &view, snap, panes, loc));
    out.extend(trend_sentences(&view, snap.at_unix_ms, loc));
    out.extend(
        details(res, snap, loc)
            .into_iter()
            .filter(|c| learning.as_ref() != Some(c))
            .map(|c| c.spoken),
    );
    out.join(" ")
}

// The merged deck's words (`docs/DESIGN-top-deck-merged-2026-10-21.md` §3–§7). Minutes go
// through `duration` and `up_words`, never a formatter of their own.

/// The needs rail's head while any need is open (§6).
pub const NEEDS_RAIL_HEAD: &str = "Needs you";
/// The needs rail's words while none is, after a drawn check (§6).
pub const NEEDS_RAIL_CALM: &str = "Nothing needs you";
/// What the Processes block's gauge measures against, named over it (§5).
pub const PER_USER_LIMIT: &str = "per-user limit";
/// The front tab's row and place.
const THIS_TAB: &str = "this tab";
/// Cores-worth waiting under which nothing is drawn past CPU's wall, so nothing is said.
const WAITING_GATE: f64 = 0.05;

/// A row of the merged deck: the Mac and the scopes inside it (§7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeckScope {
    /// The whole machine.
    Mac,
    /// aterm and everything its tabs run.
    AllTabs,
    /// The front tab, all its panes.
    ThisTab,
}

impl DeckScope {
    /// Every row, top first.
    pub const ALL: [Self; 3] = [Self::Mac, Self::AllTabs, Self::ThisTab];

    /// Its name in the deck: `Mac` (off macOS, [`Platform::machine`]'s `computer`), `all tabs`,
    /// `this tab`.
    #[must_use]
    pub fn word(self, platform: Platform) -> &'static str {
        match self {
            Self::Mac => platform.machine().trim_start_matches("this "),
            Self::AllTabs => "all tabs",
            Self::ThisTab => THIS_TAB,
        }
    }
}

/// What the Mac row's count counts, said once after its name (`Mac procs 897`): `procs`, or
/// `threads` on Linux, whose machine count is its tasks.
#[must_use]
pub const fn count_word(platform: Platform) -> &'static str {
    if platform.counts_threads() {
        "threads"
    } else {
        "procs"
    }
}

/// CPU's capacity in row A: the core kinds the OS names, fastest first (`Super, Perf`), each
/// name over five letters cut before its second vowel group (`Perf`, `Eff`); `18 cores` where
/// the OS names none.
#[must_use]
pub fn cpu_capacity(cpu: &CpuView) -> String {
    let kinds: Vec<String> = cpu
        .clusters
        .iter()
        .filter(|c| c.name != "CPU")
        .map(|c| kind_word(&c.name))
        .collect();
    if kinds.is_empty() {
        let n = cpu.total() as u64;
        format!("{n} {}", plural(n, "core", "cores"))
    } else {
        kinds.join(", ")
    }
}

fn kind_word(name: &str) -> String {
    if name.chars().count() <= 5 {
        return name.to_owned();
    }
    let (mut vowel_seen, mut consonant_after) = (false, false);
    for (i, c) in name.char_indices() {
        if "aeiouAEIOU".contains(c) {
            if consonant_after {
                return name[..i].to_owned();
            }
            vowel_seen = true;
        } else if vowel_seen {
            consonant_after = true;
        }
    }
    name.to_owned()
}

/// Memory's capacity in row A, physical memory `total` in bytes: `128 GiB`.
#[must_use]
pub fn memory_capacity(total: u64) -> String {
    format!("{} GiB", gib_total(total))
}

/// GPU's capacity in row A: `40 cores`.
#[must_use]
pub fn gpu_capacity(cores: u16) -> String {
    format!("{cores} {}", plural(u64::from(cores), "core", "cores"))
}

/// Disk's capacity in row A, the volume's size `total` in bytes as [`bytes_lone`] says it but
/// for a `.0`: `2 TB`, `995 GB`.
#[must_use]
pub fn disk_capacity(total: u64) -> String {
    let (figure, unit) = lone(total);
    format!("{} {unit}", whole(&figure))
}

/// Network's capacity in row A, its basis `bytes_per_s` said in bits and as approximate,
/// whichever basis is in force (§9): `~1 Gb/s`, `~867 Mb/s`.
#[must_use]
pub fn link_capacity(bytes_per_s: f64) -> String {
    let (figure, unit) = lone(half_up(bytes_per_s.max(0.0) * 8.0) as u64);
    format!("~{} {}/s", whole(&figure), unit.replace('B', "b"))
}

fn whole(figure: &str) -> &str {
    figure.strip_suffix(".0").unwrap_or(figure)
}

/// CPU's orange words past its wall, `wait` being the cores-worth waiting (the 1-minute load
/// average less the cores, §5): `~8 waiting`, or `<1 waiting` for under half a core. `None`
/// under 0.05, where no slat is drawn, and on Linux, whose load average counts threads waiting
/// on disk too ([`details`]' rule).
#[must_use]
pub fn waiting_words(wait: f64, platform: Platform) -> Option<String> {
    if platform == Platform::Linux || wait.is_nan() || wait < WAITING_GATE {
        return None;
    }
    let n = half_up(wait);
    Some(if n < 1.0 {
        "<1 waiting".into()
    } else {
        format!("~{n:.0} waiting")
    })
}

/// Memory's orange words past its wall, swap in use `bytes` by the GiB rule ([`gib_words`]):
/// `6 GiB swap`. `None` under 0.05 GiB, where no swap tile shows.
#[must_use]
pub fn swap_words(bytes: u64) -> Option<String> {
    (half_up(bytes as f64 / GIB * 10.0) >= 1.0).then(|| format!("{} swap", gib_words(bytes)))
}

/// Memory in GiB as the merged deck says it, chips and swap alike: one decimal under 10 GiB
/// and none from there, a whole figure without its `.0` (`0.3 GiB`, `1.1 GiB`, `6 GiB`,
/// `16 GiB`), and `<0.1 GiB` for a little, never `0`.
#[must_use]
pub fn gib_words(bytes: u64) -> String {
    let g = bytes as f64 / GIB;
    let tenths = half_up(g * 10.0) as u64;
    if tenths >= 100 {
        format!("{} GiB", half_up(g) as u64)
    } else if tenths == 0 && bytes > 0 {
        "<0.1 GiB".into()
    } else if tenths.is_multiple_of(10) {
        format!("{} GiB", tenths / 10)
    } else {
        format!("{} GiB", point(tenths, 1))
    }
}

/// What a run covers, its ends named over it, oldest at its left and now at its right (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunSpan {
    /// The last 10 minutes: CPU, Memory, GPU, Network and Processes.
    TenMinutes,
    /// The last 24 hours: Disk.
    Day,
}

impl RunSpan {
    /// The words at its oldest and its newest end (§4's `10m … now`, `24h … now`).
    #[must_use]
    pub const fn ends(self) -> (&'static str, &'static str) {
        match self {
            Self::TenMinutes => ("10m", "now"),
            Self::Day => ("24h", "now"),
        }
    }

    /// The span as a screen reader says it.
    #[must_use]
    pub const fn spoken(self) -> &'static str {
        match self {
            Self::TenMinutes => "the last 10 minutes",
            Self::Day => "the last 24 hours",
        }
    }
}

/// Free space on the front tab's volume, `bytes` free: `40 GB free`, `1.2 TB free`.
#[must_use]
pub fn free_words(bytes: u64) -> String {
    format!("{} free", bytes_lone(bytes))
}

/// Disk's writer line after the program's name, `bytes_per_hour` it wrote
/// ([`crate::writes::ProgramHour::per_hour`]): `writes 68 GB/h`.
#[must_use]
pub fn writes_words(bytes_per_hour: f64) -> String {
    format!(
        "writes {}/h",
        bytes_lone(half_up(bytes_per_hour.max(0.0)) as u64)
    )
}

/// One phrase at each of the merged deck's widths (§3), none longer than a wider form's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FormWords {
    /// Full.
    pub full: String,
    /// Mid.
    pub mid: String,
    /// Narrow, and the rail and row A narrower still.
    pub narrow: String,
}

impl FormWords {
    fn every(words: String) -> Self {
        Self {
            full: words.clone(),
            mid: words.clone(),
            narrow: words,
        }
    }

    /// Full's, Mid's and Narrow's words.
    #[must_use]
    pub fn forms(&self) -> [&str; 3] {
        [&self.full, &self.mid, &self.narrow]
    }
}

/// The Drive pill (§6): the front tab's volume is full in `secs`, floored to the minute
/// ([`duration`]): `Disk full in 34 min` at every width, the place and the verb being all of
/// it.
#[must_use]
pub fn disk_full_pill(secs: f64) -> FormWords {
    let ms = (secs.max(0.0) * 1000.0) as u64;
    FormWords::every(format!("Disk full in {}", duration(ms)))
}

/// The Lock pill (§6): the tab labelled `tab` waits for the owner's `what`, the need's own word
/// (`password` for the jobs engine's waiting-you, else the needs system's: `approval`, `login`):
/// `Tab 1 password` at every width.
#[must_use]
pub fn waiting_pill(tab: &str, what: &str) -> FormWords {
    FormWords::every(format!("{tab} {what}"))
}

/// The Hourglass pill (§6): the front tab's `program` is stuck, `quiet_ms` its quiet age
/// ([`duration`]), `tab` the tab's label. Full says `node stuck 12 min · this tab`, Mid drops
/// the minutes, and Narrow names the place by the label where that is shorter (`node stuck ·
/// Tab 3`): the verb and the place stay at every width.
#[must_use]
pub fn stuck_pill(program: &str, quiet_ms: Option<u64>, tab: &str) -> FormWords {
    let minutes = quiet_ms.map(|q| format!(" {}", duration(q)));
    let short = if !tab.is_empty() && tab.chars().count() < THIS_TAB.len() {
        tab
    } else {
        THIS_TAB
    };
    FormWords {
        full: format!(
            "{program} stuck{} · {THIS_TAB}",
            minutes.unwrap_or_default()
        ),
        mid: format!("{program} stuck · {THIS_TAB}"),
        narrow: format!("{program} stuck · {short}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CpuView, GpuView, MemSample, MemView, ScopeView};
    use crate::trend::TrendView;

    fn scenario_a() -> Snapshot {
        Snapshot {
            cpu: Some(CpuView {
                cores: vec![crate::model::CoreView::default(); 10],
                busy: TrendView::flat(3.1),
                ..CpuView::default()
            }),
            gpu: Some(GpuView {
                load: TrendView::flat(0.04),
                ..GpuView::default()
            }),
            mem: Some(MemView {
                sample: MemSample {
                    total: 32 << 30,
                    app: 12 << 30,
                    wired: (4 << 30) + (700 << 20),
                    compressed: Some(1 << 30),
                    pressure: Pressure::Normal,
                    ..MemSample::default()
                },
                ..MemView::default()
            }),
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: 1_810_000_000_000,
                available: 1_810_000_000_000,
                mount: "/".into(),
            }),
            aterm: Some(ScopeView {
                cores: TrendView::flat(1.6),
                footprint: Some((2 << 30) + (300 << 20)),
                procs: 17,
                ..ScopeView::default()
            }),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_global_sentence_reads_like_the_design() {
        assert_eq!(
            global_sentence(&scenario_a()),
            "Global: 3.1 of 10 cores busy; GPU 4%; memory 17.7 of 32 GiB used, pressure normal; \
             swap 0; disk 1.81 TB free."
        );
    }

    #[test]
    fn a_rising_figure_says_so() {
        let mut s = scenario_a();
        s.cpu.as_mut().unwrap().busy = TrendView {
            raw: 4.0,
            level: 3.1,
            slow: 2.0,
            mad: 0.0,
        };
        assert!(global_sentence(&s).contains("3.1 of 10 cores busy, rising;"));
    }

    #[test]
    fn the_aterm_sentence_gives_its_share_of_busy() {
        assert_eq!(
            aterm_sentence(&scenario_a()),
            "aterm: 1.6 cores, 52% of busy; 2.3 GiB; 17 processes."
        );
        assert_eq!(aterm_sentence(&Snapshot::default()), "aterm: measuring.");
    }

    #[test]
    fn a_tab_sentence_names_its_program_and_busiest_helper() {
        let t = TabView {
            key: 1,
            label: "Tab 2".into(),
            program: "claude".into(),
            shell_pid: 0,
            scope: ScopeView {
                cores: TrendView::flat(0.9),
                footprint: Some(310 << 20),
                top: Some(("cargo".into(), 0.7)),
                ..ScopeView::default()
            },
        };
        assert_eq!(
            tab_sentence(&t),
            "Tab 2: claude; 0.9 cores; 310 MiB; busiest cargo 0.7c."
        );
    }

    #[test]
    fn an_unread_figure_is_left_out_never_zero() {
        assert_eq!(
            global_sentence(&Snapshot::default()),
            "Global: not read yet."
        );
        let mut s = scenario_a();
        s.gpu = None;
        s.volume = None;
        let w = global_sentence(&s);
        assert!(!w.contains("GPU") && !w.contains("disk"), "{w}");
    }

    #[test]
    fn units_follow_the_platform_conventions() {
        assert_eq!(mem(310 << 20), "310 MiB");
        assert_eq!(mem(2_469_606_195), "2.3 GiB");
        assert_eq!(disk(1_810_000_000_000), "1.81 TB");
        assert_eq!(disk(412_000_000_000), "412 GB");
        assert_eq!(disk(9_400_000_000), "9.4 GB");
        assert_eq!(gib_total(32 << 30), "32");
        assert_eq!(cores(-0.2), "0.0");
        assert_eq!(
            cores_figure(3.06, Some(10), Motion::Rising),
            "3.1c/10\u{25B4}"
        );
        assert_eq!(cores_figure(0.9, None, Motion::Steady), "0.9c");
    }

    use crate::explain::{ActivityId, CoalLane, DeckCause, WhyFacts};
    use crate::history::Rates;
    use crate::jobs::{Class, Consumer, Findings, JobKind, JobRow, JobsView, Tick, ZombieParent};
    use crate::model::{Busiest, DiskFacts, GpuSample, Hog, LinkSample, PlanFact, Series};
    use crate::ring::Cell;
    use std::sync::Arc;

    const NOW: i64 = 1_791_974_400_000;
    const MIN_MS: i64 = 60_000;
    const MAC: Locale = Locale {
        platform: Platform::MacOs,
        utc_offset_s: 0,
    };
    const LINUX: Locale = Locale {
        platform: Platform::Linux,
        utc_offset_s: 0,
    };

    fn row(now: f64, capacity: f64) -> ResourceView {
        ResourceView {
            now: Some(now),
            capacity: Some(capacity),
            heat_now: Some((now / capacity) as f32),
            ..ResourceView::default()
        }
    }

    fn steady(mut view: ResourceView, over: usize) -> ResourceView {
        let v = view.now;
        view.ring = (0..60)
            .map(|i| Cell {
                mean: v,
                max: v,
                over: i >= 60 - over,
            })
            .collect::<Vec<_>>()
            .into();
        view.ring_k = NOW.div_euclid(10_000);
        view
    }

    fn series(now: f64, capacity: f64, binds: bool) -> Series {
        Series {
            now: Some(now),
            capacity: Some(capacity),
            ring: Arc::from(Vec::new()),
            binds,
        }
    }

    fn fig(now: f64) -> ScopeFig {
        ScopeFig {
            now: Some(now),
            ..ScopeFig::default()
        }
    }

    fn missing(why: NoFigure) -> ScopeFig {
        ScopeFig {
            missing: Some(why),
            ..ScopeFig::default()
        }
    }

    fn snap() -> Snapshot {
        let tab = |key: u64| TabView {
            key,
            label: format!("Tab {key}"),
            ..TabView::default()
        };
        Snapshot {
            seq: 1,
            at_unix_ms: NOW,
            tabs: vec![tab(2), tab(3)],
            ..Snapshot::default()
        }
    }

    fn set(s: &mut Snapshot, res: Resource, view: ResourceView) {
        s.resources[res.index()] = view;
    }

    fn spotlight(context: Option<Context>) -> Option<Arc<WhyFacts>> {
        Some(Arc::new(WhyFacts {
            lane: CoalLane::On,
            clause: Some(DeckCause {
                activity: ActivityId::Spotlight,
                word: "Spotlight",
                context,
            }),
            ..WhyFacts::default()
        }))
    }

    fn watch(level: DiskLevel) -> DiskFacts {
        DiskFacts {
            level: Some(level),
            warn: Some(69_000_000_000),
            floor: Some(34_000_000_000),
            emergency: Some(8_600_000_000),
            watch: true,
            ..DiskFacts::default()
        }
    }

    fn disk_trouble() -> Snapshot {
        let mut s = snap();
        let total = 2_000_000_000_000u64;
        let available = 44_800_000_000u64;
        let mut view = steady(row((total - available) as f64, total as f64), 0);
        view.state = Some(State::Critical);
        view.aterm = fig(38.4e6);
        view.tabs = vec![(2, fig(0.2e6))];
        set(&mut s, Resource::Disk, view);
        s.volume = Some(VolumeSample {
            total,
            free: available,
            available,
            mount: "/".into(),
        });
        s.facts.filling = Some(Filling {
            secs: 34.0 * 60.0,
            rate: 20e6,
            burst: true,
        });
        s.facts.disk_rates = Rates {
            rate10: Some(2.6e9 / 600.0),
            ..Rates::default()
        };
        s.facts.disk = Some(DiskFacts {
            stuck: 2,
            freed_today: Some((41_200_000_000, NOW - 90 * MIN_MS, "build caches".into())),
            hogs: vec![Hog {
                path: "~/aterm/target".into(),
                bytes: 212_000_000_000,
                partial: false,
                remedy: String::new(),
            }],
            ..watch(DiskLevel::Low)
        });
        s
    }

    fn job(id: &str, verdict: Verdict) -> JobRow {
        JobRow {
            id: id.into(),
            tab: 2,
            kind: JobKind::Shell,
            consumer: Consumer::default(),
            promoted_ms: None,
            task: None,
            pgid: 300,
            leader: (300, None),
            exe: "make".into(),
            members: 1,
            class: Class::Running,
            wait: None,
            named: None,
            stop: None,
            input: None,
            cpu_mc: None,
            zombies: None,
            standing: None,
            quiet_ms: None,
            phi_milli: None,
            mu_ms: None,
            mu_from: None,
            verdict,
            no_progress: false,
            episode: 0,
            exit: None,
            tick: Tick::default(),
        }
    }

    fn processes_trouble() -> Snapshot {
        let mut s = snap();
        let mut view = row(897.0, 16_000.0);
        view.second = Some(series(578.0, 10_666.0, false));
        set(&mut s, Resource::Processes, view);
        s.set_jobs(Some(Arc::new(JobsView {
            rows: vec![
                job("j1", Verdict::Stuck),
                job("j2", Verdict::Quiet),
                job("j3", Verdict::Quiet),
            ],
            findings: Findings {
                zombies: vec![ZombieParent {
                    ppid: 9,
                    name: "node".into(),
                    count: 1,
                    per_min_deci: 0,
                    tab: Some(2),
                }],
                ..Findings::default()
            },
            ..JobsView::default()
        })));
        s
    }

    fn ladder_rows(loc: &Locale) -> Vec<(Resource, ResourceView)> {
        let mut net = row(2.1e6, 11.8e6);
        net.basis = CapBasis::Peak;
        net.second = Some(series(0.3e6, 4.2e6, false));
        let mut tokens = row(3_100.0, 9_000.0);
        tokens.basis = CapBasis::Peak;
        let procs = if loc.platform == Platform::Linux {
            row(3_412.0, 126_592.0)
        } else {
            let mut v = row(897.0, 16_000.0);
            v.second = Some(series(578.0, 10_666.0, false));
            v
        };
        vec![
            (Resource::Cpu, row(17.7, 18.0)),
            (Resource::Gpu, row(12.0, 100.0)),
            (Resource::Memory, row(42.1 * GIB, 128.0 * GIB)),
            (Resource::Disk, row(1.11e12, 2e12)),
            (Resource::Network, net),
            (Resource::Tokens, tokens),
            (Resource::Processes, procs),
        ]
    }

    fn freed_facts() -> RowFacts {
        RowFacts {
            disk: Some(DiskFacts {
                freed_today: Some((41_200_000_000, NOW, "build caches".into())),
                ..watch(DiskLevel::Ok)
            }),
            ..RowFacts::default()
        }
    }

    fn strings(forms: &[&str]) -> Vec<String> {
        forms.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn amount_ladders() {
        use Ladder::{Cols, Det, NoCols};
        let golden = |res: Resource, loc: &Locale| -> [Vec<&'static str>; 3] {
            match (res, loc.platform) {
                (Resource::Cpu, _) => [
                    vec!["17.7 of 18 cores"],
                    vec!["17.7 of 18 cores", "17.7/18 cores"],
                    vec!["17.7/18 cores"],
                ],
                (Resource::Gpu, _) => [
                    vec!["busy 12% of the time"],
                    vec!["busy 12% of the time", "busy 12% of time", "busy 12%"],
                    vec!["busy 12% of time", "busy 12%"],
                ],
                (Resource::Memory, _) => [
                    vec!["42.1 of 128 GiB in use"],
                    vec!["42.1 of 128 GiB in use", "42.1 of 128 GiB", "42.1/128 GiB"],
                    vec!["42.1/128 GiB"],
                ],
                (Resource::Disk, _) => [
                    vec!["1.11 of 2.00 TB used"],
                    vec![
                        "1.11 of 2.00 TB used · freed 41 GB",
                        "1.11/2.00 TB used · freed 41 GB",
                        "1.11 TB used · freed 41 GB",
                        "1.11 of 2.00 TB used",
                        "1.11/2.00 TB used",
                    ],
                    vec![
                        "1.11/2.00 TB used · freed 41 GB",
                        "1.11 TB used · freed 41 GB",
                        "1.11/2.00 TB used",
                    ],
                ],
                (Resource::Network, _) => [
                    vec!["2.1 of 11.8 MB/s peak"],
                    vec!["2.1 of 11.8 MB/s peak", "2.1/11.8 MB/s peak"],
                    vec!["2.1/11.8 MB/s peak"],
                ],
                (Resource::Tokens, _) => [
                    vec!["aterm only, peak 9.0k/min", "aterm only, peak 9.0k"],
                    vec![
                        "aterm only, peak 9.0k/min",
                        "aterm only, peak 9.0k",
                        "peak 9.0k/min",
                    ],
                    vec![
                        "aterm 3.1k/min, peak 9.0k",
                        "aterm 3.1k/9.0k peak",
                        "3.1k/9.0k peak",
                    ],
                ],
                (Resource::Processes, Platform::Linux) => [
                    vec!["3,412 of 126,592 threads"],
                    vec![
                        "3,412 of 126,592 threads",
                        "3,412/126,592 threads",
                        "3.4k/127k threads",
                    ],
                    vec!["3,412/126,592 threads", "3.4k/127k threads"],
                ],
                (Resource::Processes, _) => [
                    vec!["897 of 16,000 processes"],
                    vec!["897 of 16,000 processes", "897 of 16,000", "897/16,000"],
                    vec!["897/16,000"],
                ],
            }
        };
        let names_unit = |res: Resource, form: &str, loc: &Locale| match res {
            Resource::Cpu => form.contains("cores"),
            Resource::Gpu => form.contains("busy") && form.contains('%'),
            Resource::Memory => form.contains("GiB"),
            Resource::Disk => form.contains(" used"),
            Resource::Network => {
                form.contains("MB/s") && (form.contains("peak") || form.contains("link"))
            }
            Resource::Tokens => form.contains("peak") || form.contains("/min"),
            Resource::Processes => loc.platform != Platform::Linux || form.ends_with(" threads"),
        };
        for loc in [MAC, LINUX] {
            let facts = freed_facts();
            for (res, view) in ladder_rows(&loc) {
                let want = golden(res, &loc);
                for (ladder, want) in [Det, Cols, NoCols].into_iter().zip(want) {
                    let got = amount_forms(res, &view, &facts, ladder, &loc);
                    assert_eq!(got, strings(&want), "{res:?} {ladder:?} {loc:?}");
                    for form in &got {
                        assert!(names_unit(res, form, &loc), "{res:?}: {form}");
                    }
                }
                let blank = ResourceView::default();
                for ladder in [Det, Cols, NoCols] {
                    assert!(amount_forms(res, &blank, &facts, ladder, &loc).is_empty());
                }
            }
        }

        let disk = row(1.11e12, 2e12);
        let calm = RowFacts::default();
        assert_eq!(
            amount_forms(Resource::Disk, &disk, &calm, Cols, &MAC),
            strings(&["1.11 of 2.00 TB used", "1.11/2.00 TB used"])
        );
        let mut off = freed_facts();
        off.disk.as_mut().expect("facts").watch = false;
        assert_eq!(
            amount_forms(Resource::Disk, &disk, &off, NoCols, &MAC),
            strings(&["1.11/2.00 TB used"])
        );
        assert_eq!(
            amount_forms(Resource::Disk, &row(512e9, 994e9), &calm, Det, &MAC),
            strings(&["512 of 994 GB used"])
        );

        let mut link = row(2.1e6, 22.9e6);
        link.basis = CapBasis::Link;
        link.second = Some(series(0.3e6, 22.9e6, false));
        assert_eq!(
            amount_forms(Resource::Network, &link, &calm, Cols, &MAC),
            strings(&["2.1 of 22.9 MB/s link", "2.1/22.9 MB/s link"])
        );
        let mut up = row(0.1e6, 11.8e6);
        up.basis = CapBasis::Peak;
        up.second = Some(series(3.0e6, 4.2e6, true));
        assert_eq!(
            amount_forms(Resource::Network, &up, &calm, Det, &MAC),
            strings(&["3.0 of 4.2 MB/s peak"])
        );

        let mut learning = row(3_100.0, 1.0);
        learning.capacity = None;
        learning.basis = CapBasis::Learning;
        for ladder in [Det, Cols] {
            assert_eq!(
                amount_forms(Resource::Tokens, &learning, &calm, ladder, &MAC),
                strings(&[
                    "aterm only · learning its peak",
                    "aterm only, learning peak",
                    "learning peak",
                    "learning"
                ])
            );
        }
        assert_eq!(
            amount_forms(Resource::Tokens, &learning, &calm, NoCols, &MAC),
            strings(&["aterm 3.1k/min, learning", "3.1k/min, learning", "3.1k/min"])
        );
        learning.now = None;
        assert_eq!(
            amount_forms(Resource::Tokens, &learning, &calm, NoCols, &MAC),
            strings(&[
                "aterm only · learning its peak",
                "aterm only, learning peak",
                "learning peak",
                "learning"
            ])
        );

        let mut yours = row(897.0, 16_000.0);
        yours.second = Some(series(900.0, 10_666.0, true));
        assert_eq!(
            amount_forms(Resource::Processes, &yours, &calm, Cols, &MAC),
            strings(&["900 of 10,666 yours", "900/10,666 yours"])
        );
    }

    #[test]
    fn rounding_parts_sum() {
        assert_eq!(bytes_dec(1_995_200_000_000), "2.00 TB");
        assert_eq!(bytes_dec(885_300_000_000), "885 GB");
        assert_eq!(bytes_dec(41_240_000_000), "41.2 GB");
        assert_eq!(
            bytes_dec(999_600_000_000),
            "1.00 TB",
            "rounds into the next unit"
        );
        assert_eq!(bytes_dec(9_995_000_000), "10.0 GB");
        assert_eq!(bytes_dec(99), "99 B");
        assert_eq!(bytes_lone(41_240_000_000), "41 GB");
        assert_eq!(bytes_lone(69_000_000_000), "69 GB");
        assert_eq!(bytes_lone(885_300_000_000), "885 GB");
        assert_eq!(bytes_lone(8_600_000_000), "8.6 GB");
        assert_eq!(bytes_lone(9_960_000_000), "10 GB");
        assert_eq!(bytes_lone(1_234_000_000_000), "1.2 TB");
        assert_eq!(rate_mb_lone(38.0e6), "38 MB/s");
        assert_eq!(rate_mb_lone(5.1e6), "5.1 MB/s");
        assert_eq!(rate_mb_lone(9.96e6), "10 MB/s");
        assert_eq!(rate_mb_lone(410e6), "410 MB/s");
        let vol = |total: u64, available: u64| VolumeSample {
            total,
            free: available,
            available,
            mount: "/".into(),
        };
        assert_eq!(
            disk_space_line(&vol(2_000_000_000_000, 890_000_000_000)),
            "1.11 TB used · 0.89 TB free · 2.00 TB"
        );
        assert_eq!(
            disk_space_line(&vol(2_000_000_000_000, 44_800_000_000)),
            "1.96 TB used · 0.04 TB free · 2.00 TB"
        );
        assert_eq!(
            amount_forms(
                Resource::Disk,
                &row(1_955_200_000_000.0, 2e12),
                &RowFacts::default(),
                Ladder::NoCols,
                &MAC
            ),
            strings(&["1.96/2.00 TB used"])
        );
        let quanta = |s: &str| s.replace('.', "").parse::<u64>().expect("a figure");
        for whole in [
            99u64,
            12_345,
            128 << 30,
            512_000_000_000,
            994_400_000_000,
            1_995_200_000_000,
            2_000_000_000_000,
            7_777_777_777_777,
        ] {
            for split in [0.001, 0.1234, 0.5, 0.8765, 0.999] {
                let a = (whole as f64 * split) as u64;
                let b = (whole - a) / 3;
                let c = a.min(whole - a) / 2;
                for parts in [[a, b, whole - a - b], [c, c, whole - 2 * c]] {
                    let p = parts_of(whole, &parts);
                    let sum: u64 = p.parts.iter().map(|s| quanta(s)).sum();
                    assert_eq!(sum, quanta(&p.whole), "{whole} {parts:?}: {p:?}");
                    let decimals = |s: &str| s.split_once('.').map(|(_, f)| f.len());
                    assert!(
                        p.parts.iter().all(|s| decimals(s) == decimals(&p.whole)),
                        "every part in the whole's decimal places: {p:?}"
                    );
                }
            }
        }
        assert_eq!(
            parts_of(12_345, &[6_150, 6_150, 45]),
            Parts {
                whole: "12.3".into(),
                parts: strings(&["6.2", "6.1", "0.0"]),
                unit: "kB",
            }
        );
        assert_eq!(
            parts_of(1_020, &[255, 255, 255, 255]).parts,
            strings(&["0.26", "0.26", "0.25", "0.25"])
        );
    }

    #[test]
    fn figures_follow_the_number_rules() {
        assert_eq!(pct(None), "–");
        assert_eq!(pct(Some(f32::NAN)), "–");
        assert_eq!(pct(Some(0.0)), "0%");
        assert_eq!(pct(Some(0.004)), "<1%");
        assert_eq!(pct(Some(0.125)), "13%", "half rounds up");
        assert_eq!(pct(Some(17.7 / 18.0)), "98%");
        assert_eq!(pct(Some(1.3)), "100%", "the over mark carries the excess");
        assert_eq!(rate_mb(2.1e6), "2.1 MB/s");
        assert_eq!(rate_mb(99.96e6), "100 MB/s");
        assert_eq!(rate_mb(410e6), "410 MB/s");
        assert_eq!(rate_mb(9_999e6), "9,999 MB/s");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(16_000), "16,000");
        assert_eq!(grouped(1_000_000), "1,000,000");
        assert_eq!(tokens_k(897.0), "897");
        assert_eq!(tokens_k(3_100.0), "3.1k");
        assert_eq!(tokens_k(9_000.0), "9.0k");
        assert_eq!(tokens_k(99_960.0), "100k");
        assert_eq!(tokens_k(126_592.0), "127k");
        assert_eq!(tokens_k(999_600.0), "1.0M");
        assert_eq!(tokens_k(1_234_567.0), "1.2M");
        assert_eq!(duration_approx(20.0), "~1 min");
        assert_eq!(duration_approx(34.0 * 60.0), "~34 min");
        assert_eq!(duration_approx(3_590.0), "~1 h");
        assert_eq!(duration_approx(5.0 * 3600.0), "~5 h");
        assert_eq!(duration_approx(1.2 * 86_400.0), "~1 day");
        assert_eq!(duration_approx(2.0 * 86_400.0), "~2 days");
        assert_eq!(clock(NOW, NOW, 0), "10:40");
        assert_eq!(clock(NOW, NOW, 3_600), "11:40");
        assert_eq!(clock(NOW - 86_400_000 - 100 * MIN_MS, NOW, 0), "Tue 09:00");
    }

    /// The deck's duration never writes a bare `m` (it reads as megabytes), floors to the
    /// minute, and speaks as minutes and hours. NEGATIVE CONTROL: `sys jobs`' own `12m` stays.
    #[test]
    fn a_duration_says_min_and_h_never_a_bare_m() {
        const MIN: u64 = 60_000;
        for (ms, want) in [
            (0, "under 1 min"),
            (59_999, "under 1 min"),
            (MIN, "1 min"),
            (12 * MIN + 59_999, "12 min"),
            (59 * MIN, "59 min"),
            (60 * MIN, "1 h"),
            (185 * MIN, "3 h 5 min"),
            (23 * 60 * MIN + 59 * MIN, "23 h 59 min"),
            (24 * 60 * MIN + 30 * MIN, "1 day"),
            (52 * 60 * MIN + 7 * MIN, "2 days 4 h"),
        ] {
            let got = duration(ms);
            assert_eq!(got, want, "{ms} ms");
            let bare_m = |w: &str| {
                w.strip_suffix('m')
                    .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            };
            assert!(!got.split(' ').any(bare_m), "{got}");
        }
        assert_eq!(speak(&duration(185 * MIN)), "3 hours 5 minutes");
        assert_eq!(speak(&duration(61 * MIN)), "1 hour 1 minute");
        assert_eq!(speak(&duration(0)), "under 1 minute");
        assert_eq!(
            crate::jobs::wire::age_words(12 * MIN),
            "12m",
            "sys jobs keeps its own"
        );
    }

    /// A program's age shows once it has run long, as `up …`, and as a lower bound while a
    /// member's birth went unread. NEGATIVE CONTROLS: a young program and an unread age say
    /// nothing.
    #[test]
    fn a_long_runner_says_how_long_it_has_been_up() {
        const MIN: u64 = 60_000;
        assert_eq!(
            up_words(Some(47 * MIN), false).as_deref(),
            Some("up 47 min")
        );
        assert_eq!(
            up_words(Some(185 * MIN), true).as_deref(),
            Some("up ≥ 3 h 5 min")
        );
        assert_eq!(
            up_words(Some(LONG_RUN_MS), false).as_deref(),
            Some("up 10 min")
        );
        assert_eq!(up_words(Some(LONG_RUN_MS - 1), false), None, "young");
        assert_eq!(up_words(None, true), None, "unread");
        assert_eq!(
            speak(&up_words(Some(47 * MIN), true).expect("long")),
            "up at least 47 minutes"
        );
        let at = 1_790_000_000_000;
        let g = crate::model::ProgramGroup {
            name: "rustc".into(),
            scope: ProcScope::Tab(3),
            n: 9,
            cores: 5.9,
            footprint: None,
            born_ms: Some(at - 47 * 60_000),
            unborn: 0,
        };
        assert_eq!(program_age(&g, at).as_deref(), Some("up 47 min"));
        let partly = crate::model::ProgramGroup {
            unborn: 1,
            ..g.clone()
        };
        assert_eq!(program_age(&partly, at).as_deref(), Some("up ≥ 47 min"));
    }

    #[test]
    fn scope_cells_name_their_unit() {
        let each = |res: Resource, aterm: ScopeFig, tab: ScopeFig, loc: &Locale| {
            let view = ResourceView {
                aterm,
                tab,
                ..ResourceView::default()
            };
            match scope_forms(res, &view, loc) {
                ScopeForms::Each { aterm, tab } => (aterm, tab),
                ScopeForms::Span(c) => panic!("{res:?} spans: {c:?}"),
            }
        };
        let words = |forms: &[&str]| ScopeCell {
            forms: strings(forms),
            ink: InkKind::Words,
        };
        let words2 = |s: &str| ScopeCell {
            forms: strings(&[s]),
            ink: InkKind::Words2,
        };
        assert_eq!(
            each(Resource::Cpu, fig(6.8), fig(0.04), &MAC),
            (words(&["6.8 cores"]), words(&["0.0 cores"]))
        );
        assert_eq!(
            each(Resource::Memory, fig(5.0 * GIB), fig(0.18 * GIB), &MAC),
            (words(&["5.0 GiB"]), words(&["0.18 GiB"]))
        );
        assert_eq!(
            each(Resource::Disk, fig(38.4e6), fig(0.04e6), &MAC),
            (words(&["writes 38 MB/s"]), words(&["writes 0"]))
        );
        assert_eq!(
            each(Resource::Disk, fig(5.1e6), fig(0.2e6), &MAC),
            (
                words(&["writes 5.1 MB/s", "writes 5 MB/s"]),
                words(&["writes 0.2 MB/s"])
            )
        );
        assert_eq!(
            each(Resource::Disk, fig(410e6), fig(0.2e6), &MAC).0,
            words(&["writes 410 MB/s", "writes 0.41 GB/s", "writes 0.4 GB/s"])
        );
        assert_eq!(
            each(
                Resource::Tokens,
                fig(3_100.0),
                missing(NoFigure::NoAgent),
                &MAC
            ),
            (words(&["3.1k/min"]), words2("no agent"))
        );
        assert_eq!(
            each(Resource::Processes, fig(42.0), fig(1.0), &MAC),
            (words(&["42 processes", "42"]), words(&["1 process", "1"]))
        );
        assert_eq!(
            each(Resource::Processes, fig(212.0), fig(3.0), &LINUX),
            (
                words(&["212 processes", "212"]),
                words(&["3 processes", "3"])
            )
        );
        assert_eq!(
            each(
                Resource::Cpu,
                ScopeFig::default(),
                ScopeFig::default(),
                &MAC
            ),
            (words2("–"), words2("–"))
        );
        let span = |res: Resource, why: NoFigure, loc: &Locale| {
            let view = ResourceView {
                aterm: missing(why),
                tab: missing(why),
                ..ResourceView::default()
            };
            scope_forms(res, &view, loc)
        };
        let spanned = |forms: &[&str]| {
            ScopeForms::Span(ScopeCell {
                forms: strings(forms),
                ink: InkKind::Words2,
            })
        };
        for res in [Resource::Gpu, Resource::Network] {
            assert_eq!(
                span(res, NoFigure::NotPerApp, &MAC),
                spanned(&["no per-app figure", "not per app"])
            );
        }
        assert_eq!(
            span(Resource::Disk, NoFigure::NotReadHere, &LINUX),
            spanned(&["writes not read on Linux", "writes not read"])
        );
    }

    #[test]
    fn rule_s_says_each_state_in_words() {
        let words = |s: &Snapshot, res: Resource| state(res, s, &MAC).expect("a state");

        let mut s = snap();
        let mut mem = row(112.6 * GIB, 128.0 * GIB);
        mem.state = Some(State::Warn);
        set(&mut s, Resource::Memory, mem);
        assert_eq!(
            words(&s, Resource::Memory),
            StateWords {
                state: State::Warn,
                clauses: strings(&["pressure warn"]),
                word: "warn".into(),
                label: "Memory warn".into(),
                amount_forms: strings(&["112.6/128 GiB"]),
                mark: Mark::Dot,
                ink: InkKind::Warn,
            }
        );

        let mut s = disk_trouble();
        s.facts.disk.as_mut().expect("facts").stuck = 0;
        s.resources[Resource::Disk.index()].state = Some(State::Warn);
        assert_eq!(
            words(&s, Resource::Disk),
            StateWords {
                state: State::Warn,
                clauses: strings(&["full in ~34 min, filling 1.2 GB/min", "full in ~34 min"]),
                word: "full ~34 min".into(),
                label: "Disk full ~34 min".into(),
                amount_forms: strings(&["1.96/2.00 TB used", "1.96 TB used"]),
                mark: Mark::Low,
                ink: InkKind::Warn,
            }
        );
        let w = words(&disk_trouble(), Resource::Disk);
        assert_eq!((w.state, w.ink), (State::Critical, InkKind::Critical));
        assert_eq!(
            (w.clauses, w.word.as_str(), w.label.as_str()),
            (
                strings(&["2 sessions stuck on a full disk", "2 stuck"]),
                "2 stuck",
                "Disk 2 stuck"
            )
        );
        let mut s = disk_trouble();
        s.facts.filling = None;
        s.facts.disk = Some(watch(DiskLevel::Low));
        s.resources[Resource::Disk.index()].state = Some(State::Warn);
        let w = words(&s, Resource::Disk);
        assert_eq!(
            (w.clauses, w.word.as_str(), w.label.as_str()),
            (strings(&["low, 45 GB free"]), "low", "Disk low")
        );
        s.facts.disk = Some(watch(DiskLevel::EmergencyFiles));
        s.resources[Resource::Disk.index()].state = Some(State::Critical);
        let w = words(&s, Resource::Disk);
        assert_eq!(
            (w.clauses, w.word.as_str(), w.label.as_str(), w.ink),
            (
                strings(&["emergency, out of file slots"]),
                "emergency",
                "Disk emergency",
                InkKind::Critical
            )
        );

        assert_eq!(
            words(&processes_trouble(), Resource::Processes),
            StateWords {
                state: State::Stuck,
                clauses: strings(&["1 stuck, 1 zombie, 2 quiet", "1 stuck"]),
                word: "1 stuck".into(),
                label: "1 stuck".into(),
                amount_forms: strings(&["897/16,000"]),
                mark: Mark::Stuck,
                ink: InkKind::Stuck,
            }
        );

        let mut s = snap();
        let mut gpu = row(12.0, 100.0);
        gpu.state = Some(State::Warn);
        set(&mut s, Resource::Gpu, gpu);
        s.facts.gpu_resets = Some((1, NOW - 28 * MIN_MS));
        let w = words(&s, Resource::Gpu);
        assert_eq!(
            (w.clauses, w.word.as_str(), w.label.as_str(), w.mark),
            (
                strings(&["GPU reset 1\u{d7} since 10:12", "reset 1\u{d7}"]),
                "reset 1\u{d7}",
                "GPU reset 1\u{d7}",
                Mark::Dot
            )
        );

        let mut s = snap();
        for res in Resource::ALL {
            let mut v = row(1.0, 2.0);
            if matches!(res, Resource::Cpu | Resource::Network | Resource::Tokens) {
                v.state = Some(State::Warn);
            }
            set(&mut s, res, v);
            assert_eq!(state(res, &s, &MAC), None, "{res:?}");
        }
    }

    type Shape = (u8, bool, Option<Mark>, InkKind, Vec<String>);

    fn shape(clauses: &[Clause]) -> Vec<Shape> {
        clauses
            .iter()
            .map(|c| (c.prio, c.keep, c.mark, c.ink, c.forms.clone()))
            .collect()
    }

    #[test]
    fn details_lead_with_kept_state_clauses() {
        use InkKind::{Critical, Warn, Words, Words2};
        let f = |forms: &[&str]| strings(forms);

        assert_eq!(
            shape(&details(Resource::Disk, &disk_trouble(), &MAC)),
            vec![
                (
                    0,
                    true,
                    Some(Mark::Low),
                    Warn,
                    f(&["full in ~34 min, filling 1.2 GB/min", "full in ~34 min"])
                ),
                (1, true, None, Warn, f(&["low, 45 GB free"])),
                (
                    1,
                    true,
                    None,
                    Critical,
                    f(&["2 sessions stuck on a full disk", "2 stuck"])
                ),
                (
                    3,
                    false,
                    None,
                    Words,
                    f(&["aterm freed 41 GB today", "freed 41 GB"])
                ),
                (4, false, None, Words, f(&["+2.6 GB in 10 min"])),
                (
                    6,
                    false,
                    None,
                    Words,
                    f(&["most in ~/aterm/target, 212 GB", "~/aterm/target 212 GB"])
                ),
            ]
        );
        let mut s = disk_trouble();
        s.facts.filling = Some(Filling {
            secs: 5.0 * 3600.0,
            rate: 20e6,
            burst: false,
        });
        s.facts.disk_rates.rate10 = Some(-1.2e9 / 600.0);
        s.facts.disk = Some(DiskFacts {
            stalled_since: Some(NOW - 9 * MIN_MS),
            ..watch(DiskLevel::Ok)
        });
        assert_eq!(
            shape(&details(Resource::Disk, &s, &MAC)),
            vec![
                (
                    2,
                    true,
                    None,
                    Warn,
                    f(&["disk watch stalled since 10:31", "watch stalled"])
                ),
                (
                    2,
                    false,
                    None,
                    Words,
                    f(&["filling 1.2 GB/min, full in ~5 h", "full in ~5 h"])
                ),
                (4, false, None, Words, f(&["\u{2212}1.2 GB in 10 min"])),
                (5, false, None, Words, f(&["45 GB free"])),
            ]
        );
        let mut s = disk_trouble();
        s.facts.filling = None;
        s.facts.disk_rates = Rates::default();
        s.facts.disk.as_mut().expect("facts").watch = false;
        assert_eq!(
            shape(&details(Resource::Disk, &s, &MAC)),
            vec![
                (2, false, None, Words2, f(&["disk watch off"])),
                (
                    4,
                    false,
                    None,
                    Words2,
                    f(&["growth after 10 min of readings", "growth after 10 min"])
                ),
                (5, false, None, Words, f(&["45 GB free"])),
                (
                    6,
                    false,
                    None,
                    Words,
                    f(&["most in ~/aterm/target, 212 GB", "~/aterm/target 212 GB"])
                ),
            ]
        );
        assert_eq!(
            state(Resource::Disk, &s, &MAC),
            None,
            "no state the watch would judge"
        );

        let mut s = snap();
        s.mem = Some(MemView {
            sample: MemSample {
                total: 128 << 30,
                cached: (38.5 * GIB) as u64,
                swap_used: (6.4 * GIB) as u64,
                pressure: Pressure::Warn,
                ..MemSample::default()
            },
            ..MemView::default()
        });
        s.facts.swap_rate = Some(36.0 * MIB);
        assert_eq!(
            shape(&details(Resource::Memory, &s, &MAC)),
            vec![
                (0, true, Some(Mark::Dot), Warn, f(&["pressure warn"])),
                (1, false, None, Words, f(&["swapping 36 MiB/s"])),
                (2, false, None, Words, f(&["file cache 38.5 GiB"])),
                (3, false, None, Words, f(&["swap 6.4 GiB"])),
            ]
        );
        s.mem.as_mut().expect("mem").sample.pressure = Pressure::Normal;
        assert_eq!(
            shape(&details(Resource::Memory, &s, &MAC))[0],
            (0, false, None, Words2, f(&["pressure normal"]))
        );

        assert_eq!(
            shape(&details(Resource::Processes, &processes_trouble(), &MAC)),
            vec![
                (0, true, Some(Mark::Stuck), InkKind::Stuck, f(&["1 stuck"])),
                (1, false, Some(Mark::Zombie), Warn, f(&["1 zombie"])),
                (1, false, Some(Mark::Quiet), InkKind::Stuck, f(&["2 quiet"])),
                (
                    2,
                    false,
                    None,
                    Words,
                    f(&["yours 578 of 10,666", "yours 578"])
                ),
            ]
        );

        let mut s = snap();
        set(&mut s, Resource::Cpu, row(17.7, 18.0));
        s.facts.load1 = Some(25.7);
        s.facts.busiest = Some(Busiest {
            name: "rustc".into(),
            cores: 5.9,
            n: 9,
            scope: Some(ProcScope::Tab(3)),
            born_ms: None,
            unborn: 9,
        });
        let busiest = (
            1,
            false,
            None,
            Words,
            f(&[
                "busiest rustc \u{d7}9 in Tab 3, 5.9 cores",
                "rustc \u{d7}9 in Tab 3",
            ]),
        );
        assert_eq!(
            shape(&details(Resource::Cpu, &s, &MAC)),
            vec![
                (
                    0,
                    false,
                    None,
                    Words,
                    f(&["8 waiting for a core (load 25.7)", "load 25.7 on 18 cores"])
                ),
                busiest.clone(),
            ]
        );
        assert_eq!(
            shape(&details(Resource::Cpu, &s, &LINUX)),
            vec![
                (
                    0,
                    false,
                    None,
                    Words,
                    f(&["load 25.7 on 18 cores", "load 25.7"])
                ),
                busiest.clone()
            ]
        );
        s.why = spotlight(Some(Context::Startup));
        assert_eq!(
            shape(&details(Resource::Cpu, &s, &MAC)),
            vec![
                (
                    0,
                    false,
                    None,
                    Words,
                    f(&["8 waiting for a core (load 25.7)", "load 25.7 on 18 cores"])
                ),
                (
                    1,
                    false,
                    Some(Mark::Cause),
                    Words,
                    f(&["Spotlight, after start-up", "Spotlight"])
                ),
                busiest
            ]
        );

        let mut s = snap();
        let mut net = row(2.1e6, 22.9e6);
        net.basis = CapBasis::Link;
        net.second = Some(series(0.3e6, 22.9e6, false));
        set(&mut s, Resource::Network, net);
        s.facts.net_read_s = 3_600;
        s.facts.link = Some(LinkSample {
            name: "en0".into(),
            kind: LinkKind::Wifi,
            counts: true,
            rate_bps: Some(183_200_000),
            ..LinkSample::default()
        });
        assert_eq!(
            shape(&details(Resource::Network, &s, &MAC)),
            vec![
                (
                    0,
                    false,
                    None,
                    Words2,
                    f(&["learning its peak: 1 of 24 h read", "learning peak"])
                ),
                (
                    0,
                    false,
                    Some(Mark::Up),
                    Words,
                    f(&["up 0.3 of 22.9 MB/s link", "up 0.3 MB/s"])
                ),
                (
                    1,
                    false,
                    None,
                    Words,
                    f(&["Wi-Fi en0, link 22.9 MB/s", "Wi-Fi en0"])
                ),
            ]
        );
        s.facts.net_read_s = NET_LEARN_S;
        let clauses = details(Resource::Network, &s, &MAC);
        assert!(
            clauses.iter().all(|c| !c.forms[0].contains("learning")),
            "{clauses:?}"
        );
        assert!(!row_sentence(Resource::Network, &s, &[], &MAC).contains("Learning"));
        let mut lost = row(2.1e6, 1.0);
        lost.capacity = None;
        lost.heat_now = None;
        lost.basis = CapBasis::Peak;
        assert_eq!(
            amount_forms(
                Resource::Network,
                &lost,
                &RowFacts::default(),
                Ladder::Det,
                &MAC
            ),
            strings(&["2.1 MB/s, no peak in 7 days"])
        );

        let mut s = snap();
        s.facts.tokens_in = Some(410_000.0);
        s.facts.plan = Some(PlanFact {
            window_word: "weekly".into(),
            used_x10: 340,
            read_at: NOW - 2 * MIN_MS,
            ..PlanFact::default()
        });
        let input = (
            1,
            false,
            None,
            Words,
            f(&["input 410k/min, cache re-reads included", "input 410k/min"]),
        );
        assert_eq!(
            shape(&details(Resource::Tokens, &s, &MAC)),
            vec![
                (
                    0,
                    false,
                    None,
                    Words,
                    f(&[
                        "weekly 34% used (account), read 10:38",
                        "weekly 34% used, 10:38"
                    ])
                ),
                input.clone(),
            ]
        );
        s.facts.plan = None;
        assert_eq!(
            shape(&details(Resource::Tokens, &s, &MAC)),
            vec![input, (2, false, None, Words2, f(&["plan limit not read"]))]
        );

        let mut s = snap();
        set(&mut s, Resource::Gpu, row(12.0, 100.0));
        s.facts.gpu_resets = Some((1, NOW - 28 * MIN_MS));
        s.facts.gpu = Some(GpuSample {
            cores: Some(40),
            in_use_bytes: Some((1.7 * GIB) as u64),
            ..GpuSample::default()
        });
        assert_eq!(
            shape(&details(Resource::Gpu, &s, &MAC)),
            vec![
                (
                    0,
                    true,
                    Some(Mark::Dot),
                    Warn,
                    f(&["GPU reset 1\u{d7} since 10:12"])
                ),
                (0, false, None, Words, f(&["40 GPU cores"])),
                (
                    1,
                    false,
                    None,
                    Words,
                    f(&["1.7 GiB GPU memory in use", "1.7 GiB in use"])
                ),
            ]
        );
        set(&mut s, Resource::Gpu, ResourceView::default());
        assert!(details(Resource::Gpu, &s, &MAC).is_empty());
    }

    #[test]
    fn cpu_details_name_the_cause_after_load_and_before_busiest() {
        let mut s = snap();
        set(&mut s, Resource::Cpu, row(3.3, 18.0));
        s.facts.load1 = Some(5.6);
        s.facts.busiest = Some(Busiest {
            name: "siriactionsd".into(),
            cores: 0.7,
            n: 1,
            scope: None,
            born_ms: None,
            unborn: 1,
        });
        let read = |s: &Snapshot| -> Vec<(Vec<String>, Option<Mark>, String)> {
            details(Resource::Cpu, s, &MAC)
                .into_iter()
                .map(|c| (c.forms, c.mark, c.spoken))
                .collect()
        };
        let load = (
            strings(&["load 5.6 on 18 cores", "load 5.6"]),
            None,
            "Load 5.6 on 18 cores.".to_owned(),
        );
        let busiest = (
            strings(&["busiest siriactionsd, 0.7 cores", "siriactionsd"]),
            None,
            "Busiest siriactionsd, 0.7 cores.".to_owned(),
        );
        assert_eq!(read(&s), [load.clone(), busiest.clone()]);
        for (context, long) in [
            (Some(Context::Startup), "Spotlight, after start-up"),
            (Some(Context::Wake), "Spotlight, since wake"),
            (Some(Context::Throttled), "Spotlight"),
            (None, "Spotlight"),
        ] {
            s.why = spotlight(context);
            let cause = (
                strings(&[long, "Spotlight"]),
                Some(Mark::Cause),
                format!("Why: {long}."),
            );
            assert_eq!(
                read(&s),
                [load.clone(), cause, busiest.clone()],
                "{context:?}"
            );
            assert_eq!(state(Resource::Cpu, &s, &MAC), None, "a cause is no state");
        }

        s.why = spotlight(Some(Context::Startup));
        s.at_ns = 15_000_000_000;
        assert_eq!(read(&s).len(), 3, "15 s old");
        s.at_ns += 1_000_000;
        assert_eq!(read(&s), [load.clone(), busiest.clone()], "older");
        s.at_ns = 0;
        s.why = Some(Arc::new(WhyFacts::default()));
        assert_eq!(read(&s), [load, busiest], "no clause held");
    }

    /// Tab 2 split in panes 2 and 3 speaks as one tab: named once, its figure both panes'.
    #[test]
    fn a_split_tab_speaks_for_all_its_panes() {
        let mut s = snap();
        s.tabs[1].label = "Tab 2".into();
        let mut cpu = steady(row(17.7, 18.0), 48);
        cpu.aterm = fig(6.8);
        cpu.tabs = vec![(2, fig(0.1)), (3, fig(2.4))];
        set(&mut s, Resource::Cpu, cpu);
        let said = |panes: &[u64]| row_sentence(Resource::Cpu, &s, panes, &MAC);
        assert!(
            said(&[2, 3]).contains(". aterm 6.8 cores, Tab 2 2.5 cores. "),
            "{}",
            said(&[2, 3])
        );
        assert_eq!(said(&[3, 2]), said(&[2, 3]));
        assert!(
            said(&[2]).contains(". aterm 6.8 cores, Tab 2 0.1 cores. "),
            "one pane, as before: {}",
            said(&[2])
        );
    }

    #[test]
    fn row_sentences() {
        let mut s = snap();
        let mut cpu = steady(row(17.7, 18.0), 48);
        cpu.aterm = fig(6.8);
        cpu.tabs = vec![(2, fig(0.1))];
        set(&mut s, Resource::Cpu, cpu);
        s.facts.load1 = Some(25.7);
        assert_eq!(
            row_sentence(Resource::Cpu, &s, &[2], &MAC),
            "CPU 98 percent of capacity: 17.7 of 18 cores. aterm 6.8 cores, Tab 2 0.1 cores. \
             Steady near 98 percent. Over capacity 8 of the last 10 minutes. 8 waiting for a \
             core, load 25.7 on 18 cores."
        );
        s.why = spotlight(Some(Context::Startup));
        s.facts.busiest = Some(Busiest {
            name: "rustc".into(),
            cores: 5.9,
            n: 9,
            scope: Some(ProcScope::Tab(3)),
            born_ms: None,
            unborn: 9,
        });
        assert!(
            row_sentence(Resource::Cpu, &s, &[2], &MAC).ends_with(
                "8 waiting for a core, load 25.7 on 18 cores. Why: Spotlight, after start-up. \
                 Busiest rustc, 9 processes in Tab 3, 5.9 cores."
            ),
            "the meter says the cause where no Details column shows"
        );

        assert_eq!(
            row_sentence(Resource::Disk, &disk_trouble(), &[2], &MAC),
            "Disk 98 percent of capacity: 1.96 of 2.00 terabytes used. aterm writes 38 \
             megabytes per second, Tab 2 writes 0.2 megabytes per second. Steady near 98 \
             percent. Disk full in about 34 minutes, filling 1.2 gigabytes per minute. Disk low, \
             45 gigabytes free. 2 sessions stuck on a full disk. aterm freed 41 gigabytes \
             today. Used grew 2.6 gigabytes in the last 10 minutes. Most in ~/aterm/target, 212 \
             gigabytes."
        );

        let mut s = snap();
        let mut net = steady(row(2.1e6, 22.9e6), 0);
        net.basis = CapBasis::Link;
        net.second = Some(series(0.3e6, 22.9e6, false));
        net.aterm = missing(NoFigure::NotPerApp);
        net.tab = missing(NoFigure::NotPerApp);
        set(&mut s, Resource::Network, net);
        s.facts.net_read_s = 3_600;
        s.facts.link = Some(LinkSample {
            name: "en0".into(),
            kind: LinkKind::Wifi,
            counts: true,
            rate_bps: Some(183_200_000),
            ..LinkSample::default()
        });
        assert_eq!(
            row_sentence(Resource::Network, &s, &[2], &MAC),
            "Network 9 percent of its link rate: 2.1 of 22.9 megabytes per second, downloading. \
             Learning its peak: 1 of 24 hours read. No per-app figure. Steady near 9 percent. \
             Sending 0.3 of 22.9 megabytes per second. Wi-Fi en0, link 22.9 megabytes per \
             second."
        );
        s.net = Some(crate::NetView {
            tcp: Some(crate::TcpCounts {
                established: 14,
                listen: 6,
                time_wait: 3,
                ..crate::TcpCounts::default()
            }),
            ..crate::NetView::default()
        });
        assert!(
            row_sentence(Resource::Network, &s, &[2], &MAC)
                .ends_with("14 TCP connections established, 6 listening."),
            "network row sentence reports TCP connection counts when present"
        );

        let mut s = snap();
        let mut tokens = steady(row(3_100.0, 1.0), 0);
        tokens.capacity = None;
        tokens.heat_now = None;
        tokens.basis = CapBasis::Learning;
        tokens.aterm = fig(3_100.0);
        tokens.tabs = vec![(2, missing(NoFigure::NoAgent))];
        set(&mut s, Resource::Tokens, tokens);
        s.facts.tokens_read_s = 60;
        assert_eq!(
            row_sentence(Resource::Tokens, &s, &[2], &MAC),
            "Tokens: learning aterm's peak, 1 of 10 minutes. aterm 3.1 thousand per minute. \
             Tab 2 has no agent. Plan limit not read."
        );

        let unread_said = |unread: usize| {
            let mut s = snap();
            let mut cpu = steady(row(9.0, 18.0), 0);
            cpu.ring = (0..60)
                .map(|i| Cell {
                    mean: (i >= unread).then_some(9.0),
                    max: (i >= unread).then_some(9.0),
                    over: false,
                })
                .collect::<Vec<_>>()
                .into();
            set(&mut s, Resource::Cpu, cpu);
            row_sentence(Resource::Cpu, &s, &[], &MAC)
        };
        for (unread, said) in [
            (1, "Not read for 10 seconds of the last 10 minutes."),
            (12, "Not read for 2 of the last 10 minutes."),
            (59, "Not read for 9 of the last 10 minutes."),
        ] {
            let got = unread_said(unread);
            assert!(got.contains(said), "{unread}: {got}");
        }

        assert_eq!(
            row_sentence(Resource::Cpu, &Snapshot::default(), &[], &MAC),
            "CPU: reading."
        );
        assert_eq!(
            row_sentence(Resource::Gpu, &snap(), &[], &MAC),
            "GPU: not read."
        );
    }

    #[test]
    fn the_disk_lens_answers_wrong() {
        let mut s = disk_trouble();
        s.facts.filling = None;
        s.facts.disk = Some(watch(DiskLevel::Ok));
        s.volume.as_mut().expect("volume").available = 885_000_000_000;
        s.facts.disk_rates.rate60 = Some(885e9 / (2.0 * 86_400.0));
        assert_eq!(
            disk_wrong(&s, &MAC).as_deref(),
            Some(
                "No. At the last hour's rate, 885 GB free lasts about 2 days. No session is \
                 stuck on a full disk."
            )
        );
        s.volume.as_mut().expect("volume").available = 41_200_000_000;
        s.facts.disk = Some(watch(DiskLevel::Low));
        assert_eq!(
            disk_wrong(&s, &MAC).as_deref(),
            Some(
                "Yes. 41 GB free is under the 69 GB warning line. aterm cleans its build \
                 caches below 34 GB free."
            )
        );
        s.facts.disk = Some(watch(DiskLevel::Short));
        assert_eq!(
            disk_wrong(&s, &MAC).as_deref(),
            Some(
                "Yes. 41 GB free is under the 34 GB cleaning line. aterm is cleaning its \
                 build caches."
            )
        );
        s.facts.disk = Some(DiskFacts {
            stuck: 2,
            ..watch(DiskLevel::Ok)
        });
        assert!(
            disk_wrong(&s, &MAC)
                .is_some_and(|w| w.starts_with("Yes. 2 sessions are stuck on a full disk."))
        );
        s.facts.disk.as_mut().expect("facts").watch = false;
        assert_eq!(disk_wrong(&s, &MAC), None);
    }

    #[test]
    fn wells_and_strip_sentences() {
        let mut s = snap();
        assert_eq!(
            strip_words(Resource::Gpu, &s),
            Some(strings(&[
                "no GPU reading on this system",
                "no GPU reading"
            ]))
        );
        set(&mut s, Resource::Gpu, steady(row(12.0, 100.0), 0));
        assert_eq!(strip_words(Resource::Gpu, &s), None);
        let mut tokens = row(3_100.0, 1.0);
        tokens.basis = CapBasis::Learning;
        set(&mut s, Resource::Tokens, tokens);
        s.facts.tokens_read_s = 60;
        assert_eq!(
            strip_words(Resource::Tokens, &s),
            Some(strings(&[
                "learning aterm's token peak: 1 of 10 min",
                "learning token peak: 1 of 10 min",
                "learning peak: 1 of 10 min",
                "learning: 1/10 min",
                "learning"
            ]))
        );

        let mut net = row(2.1e6, 11.8e6);
        net.second = Some(series(0.3e6, 4.2e6, false));
        assert_eq!(
            well(Resource::Network, &net),
            Some((Mark::Down, InkKind::Words2))
        );
        net.second = Some(series(4.0e6, 4.2e6, true));
        assert_eq!(
            well(Resource::Network, &net),
            Some((Mark::Up, InkKind::Words2))
        );
        let with = |state: State| ResourceView {
            state: Some(state),
            ..ResourceView::default()
        };
        assert_eq!(
            well(Resource::Disk, &with(State::Warn)),
            Some((Mark::Low, InkKind::Warn))
        );
        assert_eq!(
            well(Resource::Memory, &with(State::Critical)),
            Some((Mark::Dot, InkKind::Critical))
        );
        assert_eq!(
            well(Resource::Processes, &with(State::Quiet)),
            Some((Mark::Quiet, InkKind::Stuck))
        );
        assert_eq!(
            well(Resource::Processes, &with(State::Zombies)),
            Some((Mark::Zombie, InkKind::Warn))
        );
        assert_eq!(well(Resource::Cpu, &row(1.0, 2.0)), None);
    }

    /// One merged word: what it is, ours, the mockup's (`docs/design-studies/top-deck-*-2026-10-23`,
    /// gen.py's strings), and where the two differ, the words meant instead and why.
    struct Said {
        what: String,
        ours: String,
        mockup: &'static str,
        differs: Option<(&'static str, &'static str)>,
    }

    fn said(what: impl Into<String>, ours: impl Into<String>, mockup: &'static str) -> Said {
        Said {
            what: what.into(),
            ours: ours.into(),
            mockup,
            differs: None,
        }
    }

    /// The merged deck's words for a scene, read from its fixture snapshot the way the deck
    /// will read them, beside the mockup's. An absence is the empty string on both sides.
    fn merged_table(scene: crate::fixtures::DeckScene) -> Vec<Said> {
        use crate::fixtures::{self, DeckScene, Owner};
        let s = fixtures::snapshot(scene);
        let busy = scene == DeckScene::Busy;
        let mac = Platform::MacOs;
        let cpu = s.cpu.as_ref().expect("the scene reads its CPU");
        let mem = &s.mem.as_ref().expect("the scene reads its memory").sample;
        let vol = s.volume.as_ref().expect("the scene reads its volume");
        let gib_of = |name: Option<&str>, owner: &dyn Fn(Owner) -> bool| {
            gib_words(
                fixtures::programs(scene)
                    .iter()
                    .filter(|p| name.is_none_or(|n| p.name == n) && owner(p.owner))
                    .map(|p| (p.gib * fixtures::GIB) as u64)
                    .sum(),
            )
        };
        let pane = |k: u64| move |o: Owner| o == Owner::Pane(k);
        let (span, day) = (RunSpan::TenMinutes.ends(), RunSpan::Day.ends());
        let writer = &s.facts.hour_writers[0];
        let per_hour = writer
            .per_hour(s.facts.writes_covered_s)
            .expect("a whole hour read");
        let wait = s.facts.load1.expect("load read") - cpu.total() as f64;
        let link = s.resources[Resource::Network.index()]
            .binding()
            .capacity
            .expect("the link's rate");
        let mut out = vec![
            said("Mac row", DeckScope::Mac.word(mac), "Mac"),
            said("all-tabs row", DeckScope::AllTabs.word(mac), "all tabs"),
            said("this-tab row", DeckScope::ThisTab.word(mac), "this tab"),
            said("CPU capacity", cpu_capacity(cpu), "Super, Perf"),
            said("Memory capacity", memory_capacity(mem.total), "128 GiB"),
            said(
                "GPU capacity",
                gpu_capacity(s.gpu.as_ref().and_then(|g| g.cores).expect("cores")),
                "40 cores",
            ),
            said("Disk capacity", disk_capacity(vol.total), "2 TB"),
            said("Network capacity", link_capacity(link), "~1 Gb/s"),
            said(
                "CPU past its wall",
                waiting_words(wait, mac).unwrap_or_default(),
                if busy { "~8 waiting" } else { "" },
            ),
            said(
                "Memory past its wall",
                swap_words(mem.swap_used).unwrap_or_default(),
                if busy { "6 GiB swap" } else { "" },
            ),
            said(
                "10-minute run",
                format!("{} … {}", span.0, span.1),
                "10m … now",
            ),
            said("Disk's run", format!("{} … {}", day.0, day.1), "24h … now"),
            said(
                "Disk free",
                free_words(vol.available),
                if busy { "40 GB free" } else { "880 GB free" },
            ),
            Said {
                differs: (!busy).then_some((
                    "Safari writes 200 MB/h",
                    "the byte ladder: under 1 GB an hour says MB",
                )),
                ..said(
                    "Disk's writer",
                    format!("{} {}", writer.name, writes_words(per_hour)),
                    if busy {
                        "rustc writes 68 GB/h"
                    } else {
                        "Safari writes 0.2 GB/h"
                    },
                )
            },
            said("Mac's count", count_word(mac), "procs"),
            said("Processes gauge", PER_USER_LIMIT, "per-user limit"),
            said(
                "needs rail",
                if busy {
                    NEEDS_RAIL_HEAD
                } else {
                    NEEDS_RAIL_CALM
                },
                if busy {
                    "Needs you"
                } else {
                    "Nothing needs you"
                },
            ),
        ];
        if busy {
            out.extend([
                said("Mac chip rustc", gib_of(Some("rustc"), &pane(3)), "16 GiB"),
                said("all-tabs chip Tab 1", gib_of(None, &pane(1)), "22 GiB"),
                said("all-tabs chip Tab 4", gib_of(None, &pane(4)), "20 GiB"),
                said(
                    "this-tab chip rustc",
                    gib_of(Some("rustc"), &pane(3)),
                    "16 GiB",
                ),
                said(
                    "this-tab chip node",
                    gib_of(Some("node"), &pane(5)),
                    "1.1 GiB",
                ),
                // The scene reads no birth: gen.py's 47 minutes.
                said(
                    "rustc's age",
                    up_words(Some(47 * 60_000), false).unwrap_or_default(),
                    "up 47 min",
                ),
            ]);
            let jobs = s.jobs.as_deref().expect("the jobs engine's rows");
            let label = |tab: u64| s.tab(tab).map_or("", |t| t.label.as_str());
            let row = |v: Verdict| jobs.rows.iter().find(|r| r.verdict == v).expect("a job");
            let waits = row(Verdict::WaitingYou);
            let stuck = row(Verdict::Stuck);
            let drive = disk_full_pill(s.facts.filling.expect("filling").secs);
            let lock = waiting_pill(label(waits.tab), "password");
            let hourglass = stuck_pill(&stuck.exe, stuck.quiet_ms, label(stuck.tab));
            let full_mid_narrow = ["Full", "Mid", "Narrow"];
            for (i, form) in full_mid_narrow.into_iter().enumerate() {
                let what = |kind: &str| format!("{kind} pill, {form}");
                out.push(said(what("Drive"), drive.forms()[i], "Disk full in 34 min"));
                out.push(said(what("Lock"), lock.forms()[i], "Tab 1 password"));
                out.push(Said {
                    differs: Some((
                        [
                            "node stuck 12 min · this tab",
                            "node stuck · this tab",
                            "node stuck · Tab 3",
                        ][i],
                        "§6: the jobs engine's verdict, stuck, not the mockup's hung",
                    )),
                    ..said(
                        what("Hourglass"),
                        hourglass.forms()[i],
                        [
                            "node hung 12 min · this tab",
                            "node hung · this tab",
                            "node hung · Tab 3",
                        ][i],
                    )
                });
            }
        } else {
            let outside = |o: Owner| matches!(o, Owner::Outside(_));
            out.extend([
                said(
                    "Mac chip Safari",
                    gib_of(Some("Safari"), &outside),
                    "6.1 GiB",
                ),
                said("all-tabs chip Tab 1", gib_of(None, &pane(1)), "3.6 GiB"),
                said(
                    "all-tabs chip aterm",
                    gib_of(None, &|o: Owner| o == Owner::Aterm),
                    "1.1 GiB",
                ),
                said(
                    "this-tab chip claude",
                    gib_of(Some("claude"), &pane(2)),
                    "1.4 GiB",
                ),
                said(
                    "this-tab chip node",
                    gib_of(Some("node"), &pane(2)),
                    "0.3 GiB",
                ),
                said(
                    "claude's age",
                    up_words(Some(3 * 3_600_000), false).unwrap_or_default(),
                    "up 3 h",
                ),
            ]);
        }
        out
    }

    /// THE SLICE'S PROOF: busy's and calm's merged words, read from their fixture snapshots,
    /// beside the mockups' (gen.py's strings), printed as a table (`--nocapture`). Each word
    /// matches the mockup's but where a reason is named, and there it is the words meant.
    #[test]
    fn merged_words_beside_the_mockups() {
        use crate::fixtures::DeckScene;
        for scene in [DeckScene::Busy, DeckScene::Calm] {
            println!("\n{} | ours | mockup", scene.name());
            for row in merged_table(scene) {
                let mark = if row.ours == row.mockup { "=" } else { "≠" };
                println!(
                    "  {:<24} {:<30} {mark} {:<30} {}",
                    row.what,
                    format!("{:?}", row.ours),
                    format!("{:?}", row.mockup),
                    row.differs.map_or("", |d| d.1)
                );
                let want = row.differs.map_or(row.mockup, |d| d.0);
                assert_eq!(row.ours, want, "{}: {}", scene.name(), row.what);
                assert_eq!(row.differs.is_some(), want != row.mockup, "{}", row.what);
            }
        }
    }

    #[test]
    fn merged_words_round_at_their_edges() {
        let gib = |g: f64| (g * GIB).round() as u64;
        assert_eq!(gib_words(gib(9.94)), "9.9 GiB");
        assert_eq!(gib_words(gib(9.95)), "10 GiB", "a decimal only under 10");
        assert_eq!(gib_words(gib(15.6)), "16 GiB");
        assert_eq!(gib_words(gib(1.1)), "1.1 GiB");
        assert_eq!(gib_words(gib(6.0)), "6 GiB", "a whole figure has no .0");
        assert_eq!(gib_words(gib(0.3)), "0.3 GiB");
        assert_eq!(gib_words(gib(0.04)), "<0.1 GiB", "a little is never 0");
        assert_eq!(gib_words(0), "0 GiB");
        assert_eq!(gib_words(gib(127.6)), "128 GiB");

        assert_eq!(free_words(999_400_000_000), "999 GB free");
        assert_eq!(
            free_words(999_500_000_000),
            "1.0 TB free",
            "999 GB rounds up to TB"
        );
        assert_eq!(free_words(40_000_000_000), "40 GB free");
        assert_eq!(free_words(1_240_000_000_000), "1.2 TB free");
        assert_eq!(free_words(400_000_000), "400 MB free");

        for (wait, want) in [
            (0.04, None),
            (WAITING_GATE, Some("<1 waiting")),
            (0.49, Some("<1 waiting")),
            (0.5, Some("~1 waiting")),
            (1.5, Some("~2 waiting")),
            (7.7, Some("~8 waiting")),
            (-3.0, None),
            (f64::NAN, None),
        ] {
            assert_eq!(
                waiting_words(wait, Platform::MacOs).as_deref(),
                want,
                "{wait}"
            );
        }
        assert_eq!(swap_words(gib(0.049)), None, "no swap tile shows");
        assert_eq!(swap_words(gib(0.051)).as_deref(), Some("0.1 GiB swap"));
        assert_eq!(swap_words(gib(6.0)).as_deref(), Some("6 GiB swap"));
        assert_eq!(swap_words(gib(9.95)).as_deref(), Some("10 GiB swap"));

        assert_eq!(writes_words(68e9), "writes 68 GB/h");
        assert_eq!(writes_words(0.2e9), "writes 200 MB/h");
        assert_eq!(writes_words(f64::NAN), "writes 0 B/h");

        assert_eq!(disk_capacity(2_000_000_000_000), "2 TB");
        assert_eq!(
            disk_capacity(1_995_218_165_760),
            "2 TB",
            "a 2 TB drive as APFS reports it"
        );
        assert_eq!(disk_capacity(994_662_584_320), "995 GB");
        assert_eq!(disk_capacity(500_107_862_016), "500 GB");
        assert_eq!(link_capacity(125e6), "~1 Gb/s");
        assert_eq!(link_capacity(312.5e6), "~2.5 Gb/s");
        assert_eq!(link_capacity(1.25e9), "~10 Gb/s");
        assert_eq!(link_capacity(866.7e6 / 8.0), "~867 Mb/s");
        assert_eq!(memory_capacity(128 << 30), "128 GiB");
        assert_eq!(memory_capacity(gib(7.7)), "7.7 GiB");
        assert_eq!(gpu_capacity(40), "40 cores");
        assert_eq!(gpu_capacity(1), "1 core");

        assert_eq!(speak(&link_capacity(125e6)), "about 1 gigabit per second");
        assert_eq!(speak(&writes_words(68e9)), "writes 68 gigabytes per hour");
        assert_eq!(speak(&writes_words(50.0)), "writes 50 bytes per hour");
        assert_eq!(speak(&link_capacity(10.0)), "about 80 bits per second");
        assert_eq!(speak("<1 waiting"), "less than 1 waiting");
        assert_eq!(speak("<1%"), "less than 1 percent");
    }

    #[test]
    fn cpu_capacity_names_the_core_kinds() {
        let cpu = |kinds: &[(&str, usize)]| {
            let mut clusters = Vec::new();
            let mut n = 0;
            for &(name, count) in kinds {
                clusters.push(crate::model::ClusterView {
                    name: name.into(),
                    short: name[..1].into(),
                    cores: (n..n + count).collect(),
                });
                n += count;
            }
            CpuView {
                cores: vec![crate::model::CoreView::default(); n],
                clusters,
                ..CpuView::default()
            }
        };
        assert_eq!(
            cpu_capacity(&cpu(&[("Super", 6), ("Performance", 12)])),
            "Super, Perf"
        );
        assert_eq!(
            cpu_capacity(&cpu(&[("Performance", 4), ("Efficiency", 6)])),
            "Perf, Eff"
        );
        assert_eq!(
            cpu_capacity(&cpu(&[("Performance", 8), ("Mid 1", 2), ("Efficiency", 2)])),
            "Perf, Mid 1, Eff"
        );
        assert_eq!(
            cpu_capacity(&cpu(&[("CPU", 8)])),
            "8 cores",
            "the OS names no kind"
        );
        assert_eq!(cpu_capacity(&cpu(&[("CPU", 1)])), "1 core");
    }

    /// The scopes' names follow the platform, and so does what the Mac's count counts.
    #[test]
    fn scope_names_follow_the_platform() {
        let names = |p: Platform| DeckScope::ALL.map(|s| s.word(p));
        assert_eq!(names(Platform::MacOs), ["Mac", "all tabs", "this tab"]);
        assert_eq!(names(Platform::Linux), ["computer", "all tabs", "this tab"]);
        assert_eq!(
            names(Platform::Windows),
            ["computer", "all tabs", "this tab"]
        );
        assert_eq!(count_word(Platform::MacOs), "procs");
        assert_eq!(count_word(Platform::Windows), "procs");
        assert_eq!(
            count_word(Platform::Linux),
            "threads",
            "Linux counts its tasks"
        );
        assert_eq!(RunSpan::TenMinutes.spoken(), "the last 10 minutes");
    }

    /// Linux says no CPU wait, as CPU's Details says none there (its load average counts
    /// threads waiting on disk too). NEGATIVE CONTROL: the same busy scene on macOS and Windows
    /// says it, in both places.
    #[test]
    fn linux_says_no_cpu_wait() {
        use crate::fixtures::{DeckScene, snapshot};
        let s = snapshot(DeckScene::Busy);
        let wait = s.facts.load1.expect("load") - s.cpu.as_ref().expect("cpu").total() as f64;
        for (platform, said) in [
            (Platform::MacOs, true),
            (Platform::Windows, true),
            (Platform::Linux, false),
        ] {
            let loc = Locale {
                platform,
                utc_offset_s: 0,
            };
            let first = &details(Resource::Cpu, &s, &loc)[0].forms[0];
            assert_eq!(first.contains("waiting"), said, "{platform:?}: {first}");
            assert_eq!(
                waiting_words(wait, platform).as_deref(),
                said.then_some("~8 waiting"),
                "{platform:?}"
            );
        }
        for wait in [0.3, 1.5, 40.0] {
            assert_eq!(waiting_words(wait, Platform::Linux), None, "{wait}");
        }
    }

    /// Every need pill keeps its verb and its place at every width, and no narrower form is
    /// longer than a wider one. NEGATIVE CONTROL: a pill that drops its place fails the check.
    #[test]
    fn need_pills_keep_the_verb_and_the_place_at_every_width() {
        const MIN: u64 = 60_000;
        let keeps = |p: &FormWords, verb: &str, places: &[&str]| {
            p.forms()
                .iter()
                .all(|f| f.contains(verb) && places.iter().any(|at| f.contains(at)))
        };
        let narrowing = |p: &FormWords| {
            p.forms()
                .windows(2)
                .all(|w| w[1].chars().count() <= w[0].chars().count())
        };
        for secs in [0.0, 59.0, 2_057.0, 3_599.0, 86_399.0, f64::NAN] {
            let p = disk_full_pill(secs);
            assert!(keeps(&p, "full in", &["Disk"]) && narrowing(&p), "{p:?}");
        }
        assert_eq!(
            disk_full_pill(2_057.0).narrow,
            "Disk full in 34 min",
            "floored"
        );
        assert_eq!(disk_full_pill(30.0).full, "Disk full in under 1 min");
        for (tab, what) in [
            ("Tab 1", "password"),
            ("Tab 12", "approval"),
            ("Tab 4", "login"),
        ] {
            let p = waiting_pill(tab, what);
            assert!(keeps(&p, what, &[tab]) && narrowing(&p), "{p:?}");
        }
        assert_eq!(waiting_pill("Tab 1", "password").mid, "Tab 1 password");
        let stuck = stuck_pill("node", Some(12 * MIN), "Tab 3");
        assert_eq!(
            stuck.forms(),
            [
                "node stuck 12 min · this tab",
                "node stuck · this tab",
                "node stuck · Tab 3"
            ]
        );
        for (quiet, tab) in [
            (Some(12 * MIN), "Tab 3"),
            (Some(185 * MIN), "Tab 12"),
            (None, "Tab 3"),
            (Some(0), ""),
            (Some(MIN), "Tab 1234"),
        ] {
            let p = stuck_pill("node", quiet, tab);
            let places = [THIS_TAB, tab];
            let places = if tab.is_empty() {
                &places[..1]
            } else {
                &places[..]
            };
            assert!(
                keeps(&p, "stuck", places) && keeps(&p, "node", places),
                "{p:?}"
            );
            assert!(narrowing(&p), "{p:?}");
        }
        assert_eq!(
            stuck_pill("node", None, "Tab 3").full,
            "node stuck · this tab"
        );
        assert_eq!(
            stuck_pill("node", Some(MIN), "").narrow,
            "node stuck · this tab"
        );
        assert_eq!(
            stuck_pill("node", Some(MIN), "Tab 1234").narrow,
            "node stuck · this tab",
            "a label no shorter keeps this tab"
        );
        let dropped = FormWords {
            narrow: "node stuck".into(),
            ..stuck.clone()
        };
        assert!(
            !keeps(&dropped, "stuck", &[THIS_TAB, "Tab 3"]),
            "the check sees a lost place"
        );
    }

    /// No merged word writes minutes as a bare `m` (§6: it reads as megabytes) but §4's `10m`
    /// over a run, which names a span, not an amount. NEGATIVE CONTROL: the check sees `sys
    /// jobs`' own `12m`.
    #[test]
    fn no_merged_word_writes_a_bare_m() {
        use crate::fixtures::DeckScene;
        let bare_m = |w: &str| {
            w.split(' ').any(|t| {
                t.strip_suffix('m')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        };
        let mut words: Vec<String> = [DeckScene::Busy, DeckScene::Calm]
            .into_iter()
            .flat_map(merged_table)
            .map(|row| row.ours)
            .collect();
        for ms in [
            0,
            59_999,
            60_000,
            12 * 60_000,
            47 * 60_000 + 5,
            185 * 60_000,
            86_400_000 * 3,
        ] {
            words.extend(
                disk_full_pill(ms as f64 / 1000.0)
                    .forms()
                    .map(str::to_owned),
            );
            words.extend(
                stuck_pill("node", Some(ms), "Tab 3")
                    .forms()
                    .map(str::to_owned),
            );
            words.extend(up_words(Some(ms), true));
        }
        let bare: Vec<&String> = words.iter().filter(|w| bare_m(w)).collect();
        let span = format!(
            "{} … {}",
            RunSpan::TenMinutes.ends().0,
            RunSpan::TenMinutes.ends().1
        );
        assert!(bare.iter().all(|w| **w == span), "{bare:?}");
        assert!(!bare.is_empty(), "the run's span is read");
        assert!(
            bare_m(&crate::jobs::wire::age_words(12 * 60_000)),
            "sys jobs' 12m is seen"
        );
    }
}
