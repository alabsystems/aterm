// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Human-readable formatting, amount ladders, Details clauses, and spoken sentences for system vitals.

use crate::explain::{Context, deck_cause};
use crate::history::Filling;
use crate::jobs::Verdict;
use crate::model::{
    CapBasis, DiskLevel, HeatCell, LinkKind, NoFigure, Pressure, ProcScope, Resource, ResourceView,
    RowFacts, ScopeFig, Snapshot, State, TabView, VolumeSample,
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

/// `rising` / `easing` after a figure, or nothing while steady.
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

/// A gibibyte, bytes.
const GIB: f64 = 1_073_741_824.0;
/// A mebibyte, bytes.
const MIB: f64 = 1_048_576.0;
/// Decimal units by power of 1000, from bytes up (v2 §6.0).
const DEC_UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];

/// Target OS platform for platform-specific resource wording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Platform {
    /// macOS.
    #[default]
    MacOs,
    /// Linux: the load average counts threads waiting on disk too, the
    /// process limits count threads, and no process's write counter is read.
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

    /// Whether its process limits, and so the Processes row, count threads.
    const fn counts_threads(self) -> bool {
        matches!(self, Self::Linux)
    }
}

/// What other users' processes are called on the OS family `family` (`macOS`,
/// [`Platform::name`]): the split's services part, in `sys why` and the
/// lens's Why now alike.
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
    /// The local time's offset from UTC, seconds east: times print as local
    /// `HH:MM`.
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
    /// Details shows (wide, mid with Details, fwide with Details), so the
    /// amount never repeats a Details figure.
    Det,
    /// The aterm and Tab columns show but Details does not (mid, narrow,
    /// fwide).
    Cols,
    /// No scope columns (slim, fmid, fnarrow).
    NoCols,
}

/// Single Details column clause with priority, folding forms, spoken text, and mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause {
    /// Its forms, longest first.
    pub forms: Vec<String>,
    /// The clause as a sentence a screen reader says and ⌘C copies: units
    /// spelled out and marks said as words (v2 §12).
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
    /// A clause of `forms` (longest first) at `prio`, in words ink, said as
    /// its longest form.
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
    /// The state in a word or two (tiers 2 and 4): `warn`, `low`, `full ~34
    /// min`, `1 stuck`.
    pub word: String,
    /// The fslim header's label, drawn after the mark: `Disk low`, `Memory
    /// warn`, `1 stuck` (Processes names itself through its mark and word).
    pub label: String,
    /// The amount forms drawn before the state, longest first: the compact
    /// ladder, without the freed figure the state outranks.
    pub amount_forms: Vec<String>,
    /// The mark.
    pub mark: Mark,
    /// The ink of the mark and its words.
    pub ink: InkKind,
}

/// Candidate text forms (longest first) and ink for an `aterm` or `Tab` scope cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeCell {
    /// The forms, longest first. A figure that does not fit shortens its
    /// number, never its label.
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

/// `v` rounded half up to a whole number (v2 §6.0).
fn half_up(v: f64) -> f64 {
    // The nudge rounds a half that binary floating point stores a hair low
    // (12.5 % of 0.125 as 12.499…) up, as the reader expects.
    (v + 0.5 + 1e-9).floor()
}

/// `v` (at least 0) with `dec` decimals, rounded half up: `2.25` is `2.3`.
fn fixed(v: f64, dec: usize) -> String {
    let scale = 10f64.powi(dec as i32);
    let r = half_up(v.max(0.0) * scale) / scale;
    format!("{r:.dec$}")
}

/// `n` units of `10^-dec` with `dec` decimals: `point(89, 2)` is `0.89`.
fn point(n: u64, dec: u32) -> String {
    if dec == 0 {
        return n.to_string();
    }
    let p = 10u64.pow(dec);
    format!("{}.{:0w$}", n / p, n % p, w = dec as usize)
}

/// `bytes` rounded half up to three significant digits: the significand
/// (100..=999, or `bytes` itself under 100) and the power of ten of its last
/// digit.
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

/// A significand from [`sig3`] in its decimal unit: the figure's decimals
/// and the unit.
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

/// A rate's number (v2 §6.0): one decimal under 100 (`2.1`), whole and
/// grouped from 100 (`410`, `9,999`).
fn rate_figure(v: f64) -> String {
    let one = half_up(v.max(0.0) * 10.0) / 10.0;
    if one < 100.0 {
        format!("{one:.1}")
    } else {
        grouped(half_up(v) as u64)
    }
}

/// A rate's number printed alone, not against a capacity (v2 §6.0): one
/// decimal under 10 (`5.1`, `0.2`), whole and grouped from 10 (`38`,
/// `410`), as [`bytes_lone`] prints bytes.
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
    let (sig, e) = sig3(bytes);
    let (dec, unit) = sig_unit(sig, e);
    if dec == 0 {
        return format!("{} {unit}", point(sig, 0));
    }
    let q = 10u128.pow(e + 1);
    let mut n = u64::try_from((u128::from(bytes) + q / 2) / q).unwrap_or(u64::MAX);
    let mut dec = dec - 1;
    if n >= 100 && dec > 0 {
        n /= 10;
        dec -= 1;
    }
    format!("{} {unit}", point(n, dec))
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
        // 1970-01-01 was a Thursday.
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

/// `one` for a count of 1, else `many`.
fn plural(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// A growth rate per minute: `1.2 GB/min`, or `4.2 MB/min` under a tenth of
/// a gigabyte.
fn per_minute(bytes_per_s: f64) -> String {
    let per_min = bytes_per_s * 60.0;
    if per_min >= 0.95e8 {
        format!("{} GB/min", rate_figure(per_min / 1e9))
    } else {
        format!("{} MB/min", rate_figure(per_min / 1e6))
    }
}

/// The forms of a figure printed as `a of b` (Det), both (Cols) or `a/b`
/// (NoCols).
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

/// Today's aterm-freed bytes, while the watch runs and freed some.
fn freed_today(facts: &RowFacts) -> Option<u64> {
    facts
        .disk
        .as_ref()
        .filter(|d| d.watch)
        .and_then(|d| d.freed_today.as_ref())
        .map(|f| f.0)
        .filter(|&b| b > 0)
}

/// The Disk amount's shapes: `1.11 of 2.00 TB used` and `1.11/2.00 TB used`
/// where the volume's size is known, and `1.11 TB used` alone.
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

/// What a peak basis is called in an amount: `peak`, or `link` while the
/// network learns its peak on the link's rate.
fn basis_word(basis: CapBasis) -> Option<&'static str> {
    match basis {
        CapBasis::Peak => Some("peak"),
        CapBasis::Link => Some("link"),
        CapBasis::Fixed | CapBasis::Learning => None,
    }
}

/// Whether a two-series row's second series binds: Network's send
/// direction, Processes' own-user count.
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

/// A scope's writes (v2 §6.4), a rate printed alone ([`rate_lone`]):
/// `writes 38 MB/s`, `writes 5.1 MB/s`, `writes 0` under 0.05 MB/s; from
/// 100 MB/s the number shortens and the label stays (`writes 410 MB/s`,
/// `writes 0.41 GB/s`, `writes 0.4 GB/s`).
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

/// The mark and ink a row's state puts in its well (v2 §6.0 "Well", §9.3).
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

/// Strip-spanning status sentence forms when a resource has no live cells (e.g., idle GPU or no agents).
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

/// One of the Disk row's states as the words say it (v2 §6.4), in Details
/// order.
struct DiskSaid {
    state: State,
    forms: Vec<String>,
    spoken: String,
    word: String,
    label: String,
    prio: u8,
    mark: Option<Mark>,
}

/// The home volume's free bytes now: the latest reading, else the row's
/// capacity less its "now".
fn disk_free(snap: &Snapshot) -> Option<u64> {
    let view = &snap.resources[Resource::Disk.index()];
    snap.volume
        .as_ref()
        .filter(|v| v.total > 0)
        .map(|v| v.available)
        .or_else(|| Some((view.capacity? - view.now?).max(0.0) as u64))
}

/// A volume's level clause and word (v2 §6.4): `low, 41 GB free` / `low`.
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

/// The Disk row's states in Details order (v2 §6.4): filling within the
/// hour, then the home volume's level, other volumes, sessions stuck on a
/// full disk and a stalled watch.
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

/// The Processes row's job marks, worst first (v2 §6.7).
struct JobSaid {
    state: State,
    mark: Mark,
    ink: InkKind,
    words: String,
}

/// The jobs engine's marks in the well's order, each with its count: `1
/// stuck`, `3 zombies`, `2 quiet`, `1 waiting for you`, `1 stopped`.
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

/// `n` times as a reader says it: `once`, `twice`, `3 times`.
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

/// The cause the cause engine holds for the Deck (RD §4.1, p1 ahead of
/// busiest, so it folds after it): `∵ Spotlight, after start-up`, short
/// `∵ Spotlight` ([`deck_cause`]). Said "Why: …".
fn cause(snap: &Snapshot) -> Option<Clause> {
    let c = deck_cause(snap)?;
    let long = cause_words(c.word, c.context);
    Some(
        Clause::new(1, vec![long.clone(), c.word.to_owned()])
            .marked(Some(Mark::Cause))
            .said(format!("Why: {long}.")),
    )
}

/// A cause's words `base`, with the context that times it (RD §4.1):
/// `Spotlight, after start-up`, `…, since wake`; any other context adds
/// none. The one source of these words for the Deck, `sys why` and Why now.
#[must_use]
pub fn cause_words(base: &str, context: Option<Context>) -> String {
    match context {
        Some(Context::Startup) => format!("{base}, after start-up"),
        Some(Context::Wake) => format!("{base}, since wake"),
        _ => base.to_owned(),
    }
}

/// The busiest program now (v2 §6.1 p1), its processes over the whole sweep
/// grouped by name ([`RowFacts::busiest`]): `busiest rustc ×9 in Tab 3, 5.9
/// cores`.
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
        out.push(Clause::new(5, vec![format!("{} free", bytes_lone(free))]));
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

/// The Disk row's growth over the last 10 minutes (v2 §6.4 p4): `+2.6 GB in
/// 10 min`, `−1.2 GB in 10 min`, or `steady` under a tenth of a gigabyte.
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

/// Network's learning clause (v2 §6.5 p0), while its capacity is the link's
/// because 24 hours are not yet read.
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

/// Unit symbols a reader spells out: the symbol, its plural and its singular.
const SPOKEN_UNITS: [(&str, &str, &str); 13] = [
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

/// A figure as a reader says it: `~34` "about 34", `12%` "12 percent",
/// `<1%` "less than 1 percent", `3.1k` "3.1 thousand", `1.2M` "1.2 million",
/// `3.1k/min` "3.1 thousand per minute", `2.1/11.8` "2.1 of 11.8", `≥` "at
/// least".
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

/// `s` as a sentence: a capital first letter (but `aterm` is never
/// capitalised) and a closing full stop.
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

/// What a row's % is a share of, in words (v2 §12).
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

/// The amount as sentence 1 says it: the Details-showing form, spoken.
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

/// The scopes as sentence 2 says them: `aterm 6.8 cores, Tab 2 0.1 cores.`,
/// `No per-app figure.`, `Tab 2 has no agent.`
fn scope_sentences(
    res: Resource,
    view: &ResourceView,
    snap: &Snapshot,
    tab: Option<u64>,
    loc: &Locale,
) -> Vec<String> {
    let (aterm, tab_cell) = match scope_forms(res, view, loc) {
        ScopeForms::Span(c) => return vec![sentence(&c.forms[0])],
        ScopeForms::Each { aterm, tab } => (aterm, tab),
    };
    let label = tab.map(|key| {
        snap.tab(key)
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

/// The last 10 minutes as sentence 3 says them (v2 §12): the trend from the
/// first minute's mean to the last's, a peak well above now, the minutes
/// over capacity and the minutes not read.
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

/// `n` of the last 10 minutes' 60 columns of 10 s as sentence 3 counts
/// them: seconds under a minute (`30 seconds`), else whole minutes rounded
/// half up (`2`), and all 10 only when every column counts.
fn of_ten_minutes(n: usize) -> String {
    match n {
        0..6 => format!("{} seconds", n * 10),
        6..60 => ((n + 3) / 6).min(9).to_string(),
        _ => "10".into(),
    }
}

/// Full spoken and copied accessibility summary sentence for resource row `res`.
#[must_use]
pub fn row_sentence(res: Resource, snap: &Snapshot, tab: Option<u64>, loc: &Locale) -> String {
    let name = resource_name(res);
    if snap.seq == 0 {
        return format!("{name}: reading.");
    }
    let view = snap.resource(res, tab);
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
    out.extend(scope_sentences(res, &view, snap, tab, loc));
    out.extend(trend_sentences(&view, snap.at_unix_ms, loc));
    out.extend(
        details(res, snap, loc)
            .into_iter()
            .filter(|c| learning.as_ref() != Some(c))
            .map(|c| c.spoken),
    );
    out.join(" ")
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

    /// 10:40:00 UTC on Wednesday 2026-10-14, unix ms.
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

    /// A row reading `now` of `capacity`, its heat their ratio.
    fn row(now: f64, capacity: f64) -> ResourceView {
        ResourceView {
            now: Some(now),
            capacity: Some(capacity),
            heat_now: Some((now / capacity) as f32),
            ..ResourceView::default()
        }
    }

    /// `view` with ten minutes of steady readings of its "now", the last
    /// `over` of them over its maximum.
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
            missing: None,
        }
    }

    fn missing(why: NoFigure) -> ScopeFig {
        ScopeFig {
            now: None,
            missing: Some(why),
        }
    }

    /// A snapshot read once at [`NOW`] with tabs 2 and 3 (keys 2 and 3).
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

    /// Facts read with the snapshot whose Deck clause names Spotlight in
    /// `context`, as the cause engine holds it.
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

    /// The facts of a disk watch that judged the home volume at `level`.
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

    /// The trouble mockup's Disk row: 1.96 of 2.00 TB used, full in ~34 min
    /// at 1.2 GB/min, low, two sessions stuck, 41.2 GB freed today, used
    /// grew 2.6 GB in 10 min, the biggest folder ~/aterm/target.
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

    /// A job of `verdict` in tab 2.
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

    /// 897 of 16,000 processes, 578 of them this user's of 10,666; one job
    /// stuck, two quiet, and one zombie in tab 2.
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

    /// One row of every resource, as the T-V8 ladders read them: 17.7 of 18
    /// cores, GPU busy 12 %, 42.1 of 128 GiB, 1.11 of 2.00 TB with 41.2 GB
    /// freed, 2.1 of 11.8 MB/s down (0.3 of 4.2 up), aterm 3.1k of 9.0k
    /// tokens a minute, and 897 of 16,000 processes (3,412 of 126,592
    /// threads on Linux).
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

    /// A clause's priority, keep flag, mark, ink and forms.
    type Shape = (u8, bool, Option<Mark>, InkKind, Vec<String>);

    /// Each clause's [`Shape`].
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
        assert!(!row_sentence(Resource::Network, &s, None, &MAC).contains("Learning"));
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

    /// The Deck's cause clause (RD §4.1): p1 after the load and before
    /// busiest (both p1, stably sorted, so it folds after busiest), marked ∵,
    /// its long form naming the context that times it (start-up or wake, none
    /// for the machine's), said "Why: …"; none without facts, without a
    /// clause, or once the facts are older than 15 s, and never a state.
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

    #[test]
    fn row_sentences() {
        let mut s = snap();
        let mut cpu = steady(row(17.7, 18.0), 48);
        cpu.aterm = fig(6.8);
        cpu.tabs = vec![(2, fig(0.1))];
        set(&mut s, Resource::Cpu, cpu);
        s.facts.load1 = Some(25.7);
        assert_eq!(
            row_sentence(Resource::Cpu, &s, Some(2), &MAC),
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
        });
        assert!(
            row_sentence(Resource::Cpu, &s, Some(2), &MAC).ends_with(
                "8 waiting for a core, load 25.7 on 18 cores. Why: Spotlight, after start-up. \
                 Busiest rustc, 9 processes in Tab 3, 5.9 cores."
            ),
            "the meter says the cause where no Details column shows"
        );

        assert_eq!(
            row_sentence(Resource::Disk, &disk_trouble(), Some(2), &MAC),
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
            row_sentence(Resource::Network, &s, Some(2), &MAC),
            "Network 9 percent of its link rate: 2.1 of 22.9 megabytes per second, downloading. \
             Learning its peak: 1 of 24 hours read. No per-app figure. Steady near 9 percent. \
             Sending 0.3 of 22.9 megabytes per second. Wi-Fi en0, link 22.9 megabytes per \
             second."
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
            row_sentence(Resource::Tokens, &s, Some(2), &MAC),
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
            row_sentence(Resource::Cpu, &s, None, &MAC)
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
            row_sentence(Resource::Cpu, &Snapshot::default(), None, &MAC),
            "CPU: reading."
        );
        assert_eq!(
            row_sentence(Resource::Gpu, &snap(), None, &MAC),
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
}
