// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Human-readable formatting, amount ladders, Details clauses, and spoken sentences for system vitals.

use crate::explain::{Context, deck_cause};
use crate::history::Filling;
use crate::jobs::Verdict;
use crate::model::{
    CapBasis, DiskLevel, HeatCell, LinkKind, MemSample, NetState, NoFigure, Pressure, ProcScope,
    Resource, ResourceView, RowFacts, ScopeFig, Snapshot, State, TaskRoom, VolumeSample,
};
use crate::peaks::{NET_LEARN_S, PROC_LEARN_S, TOKENS_LEARN_S};
use crate::ring::BUCKET_MS;

/// Noise gate for CPU core-count motion (`0.1` cores).
pub const CORES_GATE: f32 = 0.1;
/// Noise gate for memory footprint motion (`0.05` GiB).
pub const FOOTPRINT_GATE_GIB: f32 = 0.05;
/// Noise gate for single-core or GPU busy fraction (`0.04`).
pub const LOAD_GATE: f32 = 0.04;

/// Format a CPU core count to one decimal place.
#[must_use]
pub fn cores(c: f32) -> String {
    let c = if c.is_finite() { c.max(0.0) } else { 0.0 };
    format!("{c:.1}")
}

/// Format memory bytes in binary `MiB` or `GiB`; a nonzero amount that rounds to `0 KiB` is `<1 KiB`.
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
        not_zero(bytes, format!("{:.0}", b / 1024.0), "<1") + " KiB"
    }
}

/// Format bytes as `GiB` to one decimal place without a unit suffix; a nonzero amount that rounds to `0.0` is `<0.1`.
#[must_use]
pub fn gib_figure(bytes: u64) -> String {
    not_zero(
        bytes,
        format!("{:.1}", bytes as f64 / f64::from(1u32 << 30)),
        "<0.1",
    )
}

/// `figure`, or `floor` when a nonzero `bytes` rounded to a figure of zeros.
fn not_zero(bytes: u64, figure: String, floor: &str) -> String {
    if bytes > 0 && figure.bytes().all(|b| matches!(b, b'0' | b'.')) {
        floor.to_owned()
    } else {
        figure
    }
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
#[derive(Clone, Copy, Debug, Default)]
pub struct Locale {
    /// Whose wording.
    pub platform: Platform,
    /// The local time's offset from UTC, seconds east: times print as local `HH:MM`.
    pub utc_offset_s: i64,
    /// Per-instant local UTC offset (`unix_s -> seconds east`), so an earlier
    /// or later timestamp across a daylight-saving change reads its own offset
    /// rather than `utc_offset_s`. `None` keeps `utc_offset_s` for every instant.
    pub utc_offset_at: Option<fn(i64) -> Option<i64>>,
}

impl PartialEq for Locale {
    fn eq(&self, other: &Self) -> bool {
        self.platform == other.platform
            && self.utc_offset_s == other.utc_offset_s
            && self.utc_offset_at.is_some() == other.utc_offset_at.is_some()
    }
}

impl Eq for Locale {}

impl Locale {
    /// The local UTC offset at `at_unix_ms`, falling back to `self.utc_offset_s`.
    #[must_use]
    pub fn offset_at_ms(&self, at_unix_ms: i64) -> i64 {
        self.utc_offset_at
            .and_then(|f| f(at_unix_ms.div_euclid(1000)))
            .unwrap_or(self.utc_offset_s)
    }

    /// Format `at_unix_ms` as local `HH:MM` (with weekday prefix when on a
    /// different local day from `now_unix_ms`), placing each instant on its own
    /// local UTC offset.
    #[must_use]
    pub fn clock(&self, at_unix_ms: i64, now_unix_ms: i64) -> String {
        clock_local(
            at_unix_ms,
            now_unix_ms,
            self.offset_at_ms(at_unix_ms),
            self.offset_at_ms(now_unix_ms),
        )
    }
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
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    let scale = 10f64.powi(dec as i32);
    let r = half_up(v * scale) / scale;
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
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    let one = half_up(v * 10.0) / 10.0;
    if one < 100.0 {
        format!("{one:.1}")
    } else {
        grouped(half_up(v) as u64)
    }
}

fn rate_lone(v: f64) -> String {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    let one = half_up(v * 10.0) / 10.0;
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
    let n = if n.is_finite() { n.max(0.0) } else { 0.0 };
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
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
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

/// How long a program must have run before [`up_words`] gives its age: 10 minutes.
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

/// Format a `[0.0, 1.0]` share for the `%` column (`0%`, `<1%`, `12%`).
#[must_use]
pub fn pct(share: Option<f32>) -> String {
    let Some(p) = share
        .filter(|s| s.is_finite())
        .map(|s| f64::from(s) * 100.0)
    else {
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
    clock_local(at_unix_ms, now_unix_ms, utc_offset_s, utc_offset_s)
}

/// Format `at_unix_ms` at `at_offset_s` as local `HH:MM` (with weekday prefix
/// when on a different local day from `now_unix_ms` at `now_offset_s`).
#[must_use]
pub fn clock_local(
    at_unix_ms: i64,
    now_unix_ms: i64,
    at_offset_s: i64,
    now_offset_s: i64,
) -> String {
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let at = at_unix_ms.div_euclid(1000).saturating_add(at_offset_s);
    let now = now_unix_ms.div_euclid(1000).saturating_add(now_offset_s);
    let day = at.div_euclid(86_400);
    let secs = at.rem_euclid(86_400);
    let hm = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    if day == now.div_euclid(86_400) {
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
        Resource::Processes => processes_amount(view, facts, ladder, loc.platform),
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

/// Whether a Processes row reads process starts a second against their 7-day peak (record
/// §8.1): on every basis but [`CapBasis::Fixed`], which the count against its limit keeps where
/// no start counter reads (Windows).
fn proc_starts(view: &ResourceView) -> bool {
    view.basis != CapBasis::Fixed
}

/// Starts a second as the deck says them: a tenth under 10 (`0.4`), whole above (`37`).
fn starts_figure(v: f64) -> String {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    let one = half_up(v * 10.0) / 10.0;
    if one < 10.0 {
        format!("{one:.1}")
    } else {
        grouped(half_up(v) as u64)
    }
}

/// The Processes run where it reads starts: `37 starts/s · 742 running`, then `37/s` (record
/// §8.1); without a count, `37 starts/s`. Never a limit: the limits are the lens's Room's.
fn starts_forms(now: f64, room: &TaskRoom) -> Vec<String> {
    let n = starts_figure(now);
    let mut forms = match room.procs {
        Some(p) => vec![format!("{n} starts/s \u{b7} {} running", grouped(p))],
        None => vec![format!("{n} starts/s")],
    };
    forms.push(format!("{n}/s"));
    forms
}

fn processes_amount(
    view: &ResourceView,
    facts: &RowFacts,
    ladder: Ladder,
    platform: Platform,
) -> Vec<String> {
    let b = view.binding();
    let Some(now) = b.now else {
        return Vec::new();
    };
    if proc_starts(view) {
        let forms = starts_forms(now, &facts.room);
        return match ladder {
            Ladder::Det => forms[..1].to_vec(),
            Ladder::Cols => forms,
            Ladder::NoCols => forms[forms.len() - 1..].to_vec(),
        };
    }
    let n = half_up(now) as u64;
    let threads = platform.counts_threads();
    let unit = if threads { "threads" } else { "processes" };
    let Some(cap) = b.capacity.map(|c| half_up(c) as u64) else {
        return vec![format!("{} {unit}", grouped(n))];
    };
    let (n_s, c_s) = (grouped(n), grouped(cap));
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
    let starts = res == Resource::Processes && proc_starts(view);
    ScopeForms::Each {
        aterm: scope_cell(res, &view.aterm, starts),
        tab: scope_cell(res, &view.tab, starts),
    }
}

fn scope_cell(res: Resource, fig: &ScopeFig, starts: bool) -> ScopeCell {
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
        Resource::Processes if starts => {
            let n = starts_figure(v);
            vec![format!("{n} starts/s"), format!("{n}/s")]
        }
        Resource::Processes => {
            let n = half_up(v) as u64;
            let unit = plural(n, "process", "processes");
            vec![format!("{} {unit}", grouped(n)), grouped(n)]
        }
        Resource::Network => {
            let n = half_up(v) as u64;
            let unit = plural(n, "socket", "sockets");
            vec![
                format!("{} {unit}", grouped(n)),
                format!("{} sock", grouped(n)),
            ]
        }
        Resource::Gpu => return words2("not per app"),
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
        Resource::Processes if view.basis == CapBasis::Learning => {
            let (n, of) = (snap.facts.proc_read_s / 60, PROC_LEARN_S / 60);
            Some(vec![
                format!("learning its start peak: {n} of {of} min"),
                format!("learning start peak: {n} of {of} min"),
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
        .map(|v| v.free)
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
        let t = loc.clock(since, snap.at_unix_ms);
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
    let condition = |v| crate::jobs::wire::verdict_summary(jobs.rows.iter(), v);
    let zombies: u64 = jobs
        .findings
        .zombies
        .iter()
        .filter(|z| z.tab.is_some())
        .map(|z| u64::from(z.count))
        .sum();
    [
        (State::Stuck, condition(Verdict::Stuck)),
        (
            State::Zombies,
            (zombies > 0).then(|| format!("{zombies} {}", plural(zombies, "zombie", "zombies"))),
        ),
        (State::Quiet, condition(Verdict::Quiet)),
        (State::Waiting, condition(Verdict::WaitingYou)),
        (State::Stopped, condition(Verdict::Parked)),
    ]
    .into_iter()
    .filter_map(|(state, words)| words.map(|words| (state, words)))
    .map(|(state, words)| {
        let (mark, ink) = state_mark(Resource::Processes, state);
        JobSaid {
            state,
            mark,
            ink,
            words,
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
            let t = loc.clock(since, snap.at_unix_ms);
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
        Resource::Cpu => {
            // The machine's thermal level, not the CPU's: its words say "thermal", never "CPU"
            // (A2 §3.2), and its clause is its word.
            let thermal = snap.thermal.filter(|t| t.state().is_some())?;
            let phrase = thermal.phrase();
            (
                worst,
                vec![phrase.clone()],
                thermal.word().to_owned(),
                phrase,
            )
        }
        Resource::Network | Resource::Tokens => return None,
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
    let mut out: Vec<Clause> = thermal_clause(snap).into_iter().collect();
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

/// The CPU row's state clause, while the OS reports serious or critical
/// ([`crate::model::Thermal::state`]): `thermal serious` with its ●, kept so it leads Details,
/// as Memory's pressure does.
fn thermal_clause(snap: &Snapshot) -> Option<Clause> {
    let thermal = snap.thermal?;
    let (mark, ink) = state_mark(Resource::Cpu, thermal.state()?);
    Some(
        Clause::new(0, vec![thermal.phrase()])
            .kept()
            .marked(Some(mark))
            .inked(ink),
    )
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
        let t = loc.clock(since, snap.at_unix_ms);
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
    if let Some(top) = crate::services::over_in(snap).first() {
        let name = crate::sys::exe_word(top.lead.exe.as_deref().unwrap_or("-"));
        let state = match top.size {
            crate::services::Size::Huge => State::Critical,
            _ => State::Warn,
        };
        let (mark, ink) = state_mark(Resource::Memory, state);
        let held = mem(top.footprint);
        out.push(
            Clause::new(
                0,
                vec![
                    format!("macOS's {name} holds {held}"),
                    format!("{name} holds {held}"),
                ],
            )
            .kept()
            .marked(Some(mark))
            .inked(ink),
        );
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
    if let Some(net) = snap.net.as_ref() {
        match net.state {
            NetState::Offline => out.push(
                Clause::new(0, vec!["offline, no active link".into(), "offline".into()])
                    .kept()
                    .marked(Some(Mark::Dot))
                    .inked(InkKind::Warn)
                    .said("Network offline, no active link.".into()),
            ),
            NetState::Degraded => out.push(
                Clause::new(0, vec!["link degraded".into(), "degraded".into()])
                    .kept()
                    .marked(Some(Mark::Dot))
                    .inked(InkKind::Warn)
                    .said("Network link degraded.".into()),
            ),
            NetState::Online | NetState::Idle => {}
        }
    }
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
    if let Some(ms) = snap.net.as_ref().and_then(|n| n.srtt_ms) {
        let rtt = rtt_word(ms);
        out.push(
            Clause::new(1, vec![format!("{rtt} RTT"), rtt.clone()])
                .said(format!("TCP latency {rtt}.")),
        );
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
        let conn = plural(
            u64::from(tcp.established),
            "TCP connection established",
            "TCP connections established",
        );
        let Some(listen) = tcp.other(tcp.listen) else {
            out.push(
                Clause::new(
                    2,
                    vec![
                        format!("{est} TCP established or close-wait"),
                        format!("{est} TCP"),
                    ],
                )
                .said(format!("{est} {conn} or waiting to close.")),
            );
            return out;
        };
        let lis = grouped(u64::from(listen));
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
            // What is left, as every surface says an account's share (aterm-agent's
            // `harness::left`): 100 − the whole percent used, half up, and `0% left` only for a
            // reading of 100.
            let used = (plan.used_x10.min(1000) + 5) / 10;
            let p = 100
                - if plan.used_x10 < 1000 {
                    used.min(99)
                } else {
                    used
                };
            let t = loc.clock(plan.read_at, snap.at_unix_ms);
            let w = &plan.window_word;
            out.push(
                Clause::new(
                    0,
                    vec![
                        format!("{w} {p}% left (account), read {t}"),
                        format!("{w} {p}% left, {t}"),
                    ],
                )
                .said(sentence(&format!(
                    "{w} limit {p} percent left on the account, read at {t}"
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

/// This user's processes and the per-user limit, where both read: `(897, 10_666)`.
fn yours_of(room: &TaskRoom) -> Option<(u64, u64)> {
    room.user_procs.zip(room.user_procs_max.filter(|&c| c > 0))
}

fn processes_details(snap: &Snapshot) -> Vec<Clause> {
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
    if let Some((n, cap)) = yours_of(&snap.facts.room) {
        let n = grouped(n);
        out.push(Clause::new(
            2,
            vec![
                format!("yours {n} of {}", grouped(cap)),
                format!("yours {n}"),
            ],
        ));
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
                loc.clock(t, snap.at_unix_ms)
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

/// A volume's space as the Disk lens's first line says it: used (its size less what this user
/// could still write, [`VolumeSample::used`]), free ([`VolumeSample::free`]) and total, each in a
/// readable decimal unit (`2.34 TB used · 1.66 TB free · 4.00 TB total`).
#[must_use]
pub fn disk_space_line(v: &VolumeSample) -> String {
    format!(
        "{} used · {} free · {} total",
        bytes_dec(v.used()),
        bytes_dec(v.free.min(v.total)),
        bytes_dec(v.total)
    )
}

/// The free blocks a volume keeps from this user's writes (an ext4 root reserve), where they are
/// 1 % of its size or more: `40 GB reserved`. Its used figure counts them
/// ([`disk_space_line`]); the lens and speech say them after it.
#[must_use]
pub fn disk_reserved(v: &VolumeSample) -> Option<String> {
    let reserved = v.reserved;
    (v.total > 0 && reserved > 0 && u128::from(reserved) * 100 >= u128::from(v.total))
        .then(|| format!("{} reserved", bytes_lone(reserved)))
}

/// Format used, available (what the kernel can hand out without paging,
/// [`MemSample::available`]) and total physical memory.
#[must_use]
pub fn memory_space_line(m: &MemSample) -> String {
    format!(
        "{} GiB used · {} GiB available · {} GiB total",
        gib_figure(m.used()),
        gib_figure(m.available),
        gib_total(m.total)
    )
}

/// The GPU as its lens's first line names it: the device, its cores and the memory it holds in
/// use (`Apple M5 · 10 cores · 623 MiB in use`), each where read; `None` where none is.
#[must_use]
pub fn gpu_source(snap: &Snapshot) -> Option<String> {
    let g = snap.gpu_sample()?;
    let name = g.name.as_deref().filter(|n| !n.is_empty());
    let parts: Vec<String> = [
        name.map(str::to_owned),
        g.cores.map(|n| format!("{n} cores")),
        g.in_use_bytes.map(|b| format!("{} in use", mem(b))),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return None;
    }
    let mut line = parts.join(" \u{b7} ");
    if name.is_none() {
        line.insert_str(0, "GPU \u{b7} ");
    }
    Some(line)
}

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
        if let Some(n) = w
            .strip_suffix(suffix)
            .filter(|n| !n.starts_with('+') && n.parse::<f64>().is_ok())
        {
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
        (Resource::Processes, CapBasis::Fixed) => "the limit",
        (Resource::Processes, _) => "its 7-day peak",
        _ => "capacity",
    }
}

/// Memory's and Disk's exact figures as their lens's first line says them, which speech and ⌘C
/// say too: used, available (memory) or free (disk) and total ([`memory_space_line`],
/// [`disk_space_line`]), with a volume's reserved blocks where it keeps 1 % or more
/// ([`disk_reserved`]); `None` for the other rows and an unread sample.
#[must_use]
pub fn space_line(res: Resource, snap: &Snapshot) -> Option<String> {
    match res {
        Resource::Memory => snap
            .mem
            .as_ref()
            .filter(|m| m.sample.total > 0)
            .map(|m| memory_space_line(&m.sample)),
        Resource::Disk => snap.volume.as_ref().filter(|v| v.total > 0).map(|v| {
            let line = disk_space_line(v);
            match disk_reserved(v) {
                Some(r) => format!("{line} \u{b7} {r}"),
                None => line,
            }
        }),
        _ => None,
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
        Resource::Processes if proc_starts(view) => {
            let n = speak_figure(&starts_figure(view.binding().now.unwrap_or(0.0)));
            let unit = if n == "1" { "start" } else { "starts" };
            match facts.room.procs {
                Some(p) => format!("{n} {unit} a second, {} running", grouped(p)),
                None => format!("{n} {unit} a second"),
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
    let label = (!panes.is_empty()).then(|| {
        panes
            .iter()
            .find_map(|&key| snap.tab(key))
            .map_or_else(|| "this tab".to_string(), |t| t.label.clone())
    });
    let mut out = match scope_forms(res, view, loc) {
        ScopeForms::Span(c) => vec![sentence(&c.forms[0])],
        ScopeForms::Each { aterm, tab } => {
            let mut figures = Vec::new();
            let mut none = Vec::new();
            let scopes = [
                (Some("aterm"), &view.aterm, aterm),
                (label.as_deref(), &view.tab, tab),
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
    };
    out.extend(
        footprints_over(res, view, label.as_deref(), loc)
            .iter()
            .map(|f| format!("{}.", f.said())),
    );
    out
}

/// A Memory footprint larger than the figure of the scope around it ([`footprints_over`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FootprintOver {
    /// Whose footprint it is: "aterm", or the tab's name.
    pub who: String,
    /// Whether it is the tab's footprint, not aterm's (a tab may be named "aterm").
    pub tab: bool,
    /// What it is larger than: the machine's used memory ("this Mac's used memory"), or "aterm's".
    pub than: String,
}

impl FootprintOver {
    /// As speech says it: "aterm's footprint is larger than this Mac's used memory".
    #[must_use]
    pub fn said(&self) -> String {
        format!("{}'s footprint is {}", self.who, self.clause())
    }

    /// As it follows the footprint's own figure: "larger than this Mac's used memory".
    #[must_use]
    pub fn clause(&self) -> String {
        format!("larger than {}", self.than)
    }
}

/// Each Memory footprint larger than the figure of the scope around it: aterm's than the
/// machine's used memory, and this tab's (named `tab`) than aterm's, or than the machine's where
/// aterm has no figure. The deck's bar draws no fill past the one around it
/// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §4.6), and a footprint counts pages differently
/// from the machine's used memory, so these words are where a larger footprint shows. It compares
/// the figures, not the fills' px, and an equal one is not larger; a figure that is not a finite
/// number is no figure, as the bar draws it. Empty on every other row, and wherever each
/// footprint fits inside the scope around it.
#[must_use]
pub fn footprints_over(
    res: Resource,
    view: &ResourceView,
    tab: Option<&str>,
    loc: &Locale,
) -> Vec<FootprintOver> {
    if res != Resource::Memory {
        return Vec::new();
    }
    let read = |v: Option<f64>| v.filter(|v| v.is_finite());
    let (used, aterm) = (read(view.now), read(view.aterm.now));
    let machine = format!("{}'s used memory", loc.platform.machine());
    let mut out = Vec::new();
    if let (Some(a), Some(u)) = (aterm, used)
        && a > u
    {
        out.push(FootprintOver {
            who: "aterm".into(),
            tab: false,
            than: machine.clone(),
        });
    }
    let around = aterm
        .map(|a| (a, "aterm's".to_owned()))
        .or_else(|| used.map(|u| (u, machine)));
    if let (Some(who), Some(t), Some((p, than))) = (tab, read(view.tab.now), around)
        && t > p
    {
        out.push(FootprintOver {
            who: who.to_owned(),
            tab: true,
            than,
        });
    }
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
            loc.clock(at, now_ms)
        ));
    }
    // Each 10 s column counts the share of it that was over, so a short spell is said in
    // proportion, as its cap draws it (A1 §3).
    let over = heat.iter().map(|c| f64::from(c.over)).sum::<f64>().round() as usize;
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
    if res == Resource::Processes && view.basis == CapBasis::Learning {
        out.push(format!(
            "Processes: learning its start peak, {} of {} minutes.",
            snap.facts.proc_read_s / 60,
            PROC_LEARN_S / 60
        ));
    }
    if res == Resource::Tokens && view.basis == CapBasis::Learning {
        out.push(format!(
            "Tokens: learning aterm's peak, {} of {} minutes.",
            snap.facts.tokens_read_s / 60,
            TOKENS_LEARN_S / 60
        ));
    } else if view.binding().now.is_none() {
        return format!("{name}: not read.");
    } else {
        let amount = space_line(res, snap).map_or_else(
            || spoken_amount(res, &view, &snap.facts, loc),
            |l| speak(&l),
        );
        out.push(match view.heat_now {
            Some(h) => format!(
                "{name} {} of {}: {amount}.",
                speak_figure(&pct(Some(h))),
                capacity_words(res, &view)
            ),
            None => format!("{name}: {amount}."),
        });
        if res == Resource::Gpu
            && let Some(source) = gpu_source(snap)
        {
            out.push(format!("{}.", speak(&source)));
        }
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

// The deck's glance (the refined Stats Deck, 2026-10-24): what the deck says where it shows
// Details. The owner, on 0.134's deck: "there is too much text". The glance keeps each row's
// figure in its unit and says a clause only while it is unusual; the lens, the spoken sentence
// and ⌘C keep every word above.

/// The busiest program is named in the deck only once it burns at least this many cores.
pub const GLANCE_BUSIEST_CORES: f32 = 1.0;

/// The share of the per-user process limit from which the deck says "yours": the bar is a share
/// of the system's limit and cannot show the user's.
pub const GLANCE_YOURS_NEAR: f64 = 0.8;

/// From this write rate, bytes a second, the deck's Disk Details says aterm's writes.
pub const GLANCE_WRITES_BPS: f64 = 1e6;

/// A rate against a capacity under 1 MB/s is said in whole KB/s (decimal, as Finder writes
/// it), so a share the % prints never sits beside a figure of `0.0`.
const GLANCE_KB_BELOW_BPS: f64 = 0.95e6;

/// A row's one figure run in the deck's glance, said once at the end of its scope bar (the
/// owner, 2026-10-26: "the stats in columns after the time series doesn't make sense to me"):
/// this Mac's reading in the row's unit, longest first, with the row's maximum (`11.0 of 18
/// cores`), then without it (`11.0 cores`). Memory and Disk say how much is used of their total
/// (`112.6 of 128 GiB used`, `112.6/128 GiB used`, then `112.6 GiB used`; a volume's used is its
/// size less what this user can still write, `docs/DESIGN-stats-deck-flow-2026-11-02.md` §3), so
/// the legend gives its totals over to the run where one stands. A share of
/// something other than the row's own capacity keeps its base: Network's peak (`8 of 200 KB/s
/// peak`). Processes say their starts a second and how many run (`37 starts/s · 742 running`,
/// then `37/s`; record §8.1), never a limit; where no start counter reads they say their count
/// with no maximum (the system's limit is no use as one), then the bare count.
/// Network says both directions' live rates where the link's are read (`↓4K ↑3K/s`). Empty where
/// the row says no figure: the GPU, whose % is its reading, and a row not read yet. aterm's and
/// this tab's amounts are the bar's segments and the lens's figures, never a run of their own.
#[must_use]
pub fn glance_run(
    res: Resource,
    view: &ResourceView,
    snap: &Snapshot,
    loc: &Locale,
) -> Vec<String> {
    let (mac, unit): (Option<String>, Vec<String>) = match res {
        Resource::Cpu => match view.now {
            Some(now) => (
                Some(fixed(now, 1)),
                match view.capacity {
                    Some(n) => vec![format!("of {n:.0} cores"), "cores".into()],
                    None => vec!["cores".into()],
                },
            ),
            None => (None, Vec::new()),
        },
        Resource::Gpu => (None, Vec::new()),
        Resource::Memory => {
            let Some(now) = view.now else {
                return Vec::new();
            };
            let n = fixed(now / GIB, 1);
            let alone = format!("{n} GiB used");
            return match view.capacity {
                Some(t) => {
                    let t = gib_total(t as u64);
                    vec![
                        format!("{n} of {t} GiB used"),
                        format!("{n}/{t} GiB used"),
                        alone,
                    ]
                }
                None => vec![alone],
            };
        }
        Resource::Disk => return glance_disk(view),
        Resource::Network if net_rates_read(snap) && view.binding().now.is_some() => {
            return net_amount_det_forms(snap);
        }
        Resource::Network => glance_network(view),
        Resource::Tokens => return tokens_amount(view, Ladder::Det),
        Resource::Processes if proc_starts(view) => {
            return view
                .binding()
                .now
                .map(|now| starts_forms(now, &snap.facts.room))
                .unwrap_or_default();
        }
        Resource::Processes => glance_processes(view, loc.platform),
    };
    let Some(mac) = mac else {
        return Vec::new();
    };
    let mut forms: Vec<String> = unit.iter().map(|u| format!("{mac} {u}")).collect();
    if res == Resource::Processes {
        forms.push(mac);
    }
    forms.dedup();
    forms
}

/// Whether the link's own rates are read while it is not offline: the network's run then says
/// both directions ([`net_amount_det_forms`]), and its Details no other direction.
fn net_rates_read(snap: &Snapshot) -> bool {
    snap.net
        .as_ref()
        .is_some_and(|n| n.state != NetState::Offline && n.rates.is_some())
}

/// The used part of the volume of its size, in their shared unit: `1.36 of 2.00 TB used`,
/// `1.36/2.00 TB used`, then `1.36 TB used`; none where the size is unread.
fn glance_disk(view: &ResourceView) -> Vec<String> {
    let used = view.now.map(|u| u.max(0.0) as u64);
    let total = view.capacity.map(|c| c as u64).filter(|&t| t > 0);
    let (Some(used), Some(total)) = (used, total) else {
        return Vec::new();
    };
    let p = parts_of(total, &[used, total.saturating_sub(used)]);
    let (u, w, unit) = (&p.parts[0], &p.whole, p.unit);
    vec![
        format!("{u} of {w} {unit} used"),
        format!("{u}/{w} {unit} used"),
        format!("{u} {unit} used"),
    ]
}

/// The rate against its peak or link: `8`, `of 200 KB/s peak`, then `of 200 KB/s` (so a run too
/// short for the word still says the maximum, as every other row's does); with no peak yet, the
/// rate and why (`2.1`, `MB/s, learning peak`).
fn glance_network(view: &ResourceView) -> (Option<String>, Vec<String>) {
    let b = view.binding();
    let Some(now) = b.now else {
        return (None, Vec::new());
    };
    let kb = |v: f64| grouped(half_up(v.max(0.0) / 1e3) as u64);
    let (Some(word), Some(cap)) = (basis_word(view.basis), b.capacity) else {
        let (n, unit) = if now < GLANCE_KB_BELOW_BPS {
            (kb(now), "KB/s")
        } else {
            (rate_figure(now / 1e6), "MB/s")
        };
        let why = if view.basis == CapBasis::Peak {
            "no peak in 7 days"
        } else {
            "learning peak"
        };
        return (Some(n), vec![format!("{unit}, {why}"), unit.to_owned()]);
    };
    let (n, c, unit) = if cap < GLANCE_KB_BELOW_BPS {
        (kb(now), kb(cap), "KB/s")
    } else {
        (rate_figure(now / 1e6), rate_figure(cap / 1e6), "MB/s")
    };
    (
        Some(n),
        vec![
            format!("of {c} {unit} {word}"),
            format!("of {c} {unit}"),
            unit.to_owned(),
        ],
    )
}

/// Where the row counts processes (no start counter reads): the count and what it counts,
/// `897`, `processes` (threads where the platform counts them). The system's limit
/// (`kern.maxproc`, 16,000) is no maximum worth saying beside a few hundred.
fn glance_processes(view: &ResourceView, platform: Platform) -> (Option<String>, Vec<String>) {
    let b = view.binding();
    let Some(now) = b.now else {
        return (None, Vec::new());
    };
    let threads = platform.counts_threads();
    let unit = vec![if threads { "threads" } else { "processes" }.to_owned()];
    (Some(grouped(half_up(now) as u64)), unit)
}

/// The Details clauses the deck draws: a row's state clauses ([`details`]' kept ones) and only
/// what is unusual now. CPU: the threads waiting for a core while some wait, the cause, and the
/// busiest program once it burns [`GLANCE_BUSIEST_CORES`]. Memory: pressure past normal, a
/// macOS service past the large bar ([`crate::services`]) and swapping. Disk: its states, when it
/// fills, and aterm's writes (and the tab's of `panes`) from [`GLANCE_WRITES_BPS`], which no row
/// figure says: the row's run is the volume's space. Network: the other direction while it moves,
/// where the run says only the busier one ([`glance_run`]). Processes: the jobs' marks, and
/// "yours" from [`GLANCE_YOURS_NEAR`] of the user's limit. The standing facts (GPU cores and
/// memory, file cache, freed, growth, free space, the link, TCP, the sockets) stay in the lens and
/// the spoken sentence.
#[must_use]
pub fn glance_details(res: Resource, snap: &Snapshot, panes: &[u64], loc: &Locale) -> Vec<Clause> {
    let mut out = match res {
        Resource::Cpu => glance_cpu(snap, loc),
        Resource::Gpu | Resource::Disk => details(res, snap, loc)
            .into_iter()
            .filter(|c| c.keep)
            .collect(),
        Resource::Memory => memory_details(snap)
            .into_iter()
            .filter(|c| c.keep || c.prio == 1)
            .collect(),
        Resource::Network => glance_network_details(snap),
        Resource::Tokens => tokens_details(snap, loc),
        Resource::Processes => glance_processes_details(snap),
    };
    if res == Resource::Disk
        && let Some(f) = snap.facts.filling.filter(|f| !f.outranks_level())
    {
        out.push(Clause::new(
            2,
            vec![format!("full in {}", duration_approx(f.secs))],
        ));
    }
    if res == Resource::Disk {
        out.extend(glance_writes(snap, panes));
    }
    out.sort_by_key(|c| (c.prio, !c.keep));
    out
}

/// aterm's writes, and the tab's of `panes` in the same unit, while aterm writes at least
/// [`GLANCE_WRITES_BPS`]: `aterm writes 210 MB/s, 190 in Tab 1`, then `aterm writes 210 MB/s`.
fn glance_writes(snap: &Snapshot, panes: &[u64]) -> Option<Clause> {
    let view = snap.resource(Resource::Disk, panes);
    let aterm = view
        .aterm
        .now
        .filter(|&v| v >= GLANCE_WRITES_BPS && view.aterm.missing.is_none())?;
    let a = rate_mb_lone(aterm);
    let mut forms = Vec::new();
    if let (Some(t), Some(tab)) = (view.tab.now, snap.tab_of(panes)) {
        let t = rate_mb_lone(t.max(0.0));
        let t = t.strip_suffix(" MB/s").unwrap_or(&t);
        forms.push(format!("aterm writes {a}, {t} in {}", tab.label));
    }
    forms.push(format!("aterm writes {a}"));
    Some(Clause::new(3, forms))
}

fn glance_cpu(snap: &Snapshot, loc: &Locale) -> Vec<Clause> {
    let view = &snap.resources[Resource::Cpu.index()];
    let mut out: Vec<Clause> = thermal_clause(snap).into_iter().collect();
    if let (Some(load), Some(n)) = (snap.facts.load1, view.capacity) {
        let l = fixed(load, 1);
        let waiting = half_up(load - n);
        if loc.platform != Platform::Linux && waiting >= 1.0 {
            let w = waiting as u64;
            out.push(
                Clause::new(0, vec![format!("{w} waiting for a core")])
                    .kept()
                    .said(format!("{w} waiting for a core, load {l} on {n:.0} cores.")),
            );
        } else if loc.platform == Platform::Linux && load > n {
            out.push(
                Clause::new(
                    0,
                    vec![format!("load {l} on {n:.0} cores"), format!("load {l}")],
                )
                .kept(),
            );
        }
    }
    out.extend(cause(snap));
    // The busiest by name, where it runs, and by name alone where Details is short.
    if let Some(g) = snap
        .facts
        .busiest
        .as_ref()
        .filter(|g| g.cores >= GLANCE_BUSIEST_CORES)
        && let Some(mut c) = busiest(snap)
    {
        c.forms.remove(0);
        let times = if g.n > 1 {
            format!(" \u{d7}{}", g.n)
        } else {
            String::new()
        };
        c.forms.push(format!("{}{times}", g.name));
        c.forms.dedup();
        out.push(c);
    }
    out
}

fn glance_network_details(snap: &Snapshot) -> Vec<Clause> {
    let view = &snap.resources[Resource::Network.index()];
    let mut out = Vec::new();
    let link_word = snap.facts.link.as_ref().and_then(|l| match l.kind {
        LinkKind::Wifi => Some("Wi-Fi"),
        LinkKind::Ethernet => Some("Ethernet"),
        LinkKind::Cellular => Some("Cellular"),
        LinkKind::Vpn => Some("VPN"),
        LinkKind::Bridge | LinkKind::Other => None,
    });
    if let Some(net) = snap.net.as_ref() {
        match net.state {
            NetState::Offline => out.push(
                Clause::new(0, vec!["offline".into()])
                    .kept()
                    .marked(Some(Mark::Dot))
                    .inked(InkKind::Warn)
                    .said("Network offline.".into()),
            ),
            NetState::Degraded => out.push(
                Clause::new(0, vec!["degraded".into()])
                    .kept()
                    .marked(Some(Mark::Dot))
                    .inked(InkKind::Warn)
                    .said("Network degraded.".into()),
            ),
            NetState::Online => {
                let forms = match link_word {
                    Some(lk) => vec![format!("Online \u{b7} {lk}"), "Online".into()],
                    None => vec!["Online".into()],
                };
                out.push(
                    Clause::new(0, forms)
                        .inked(InkKind::Words)
                        .said("Network online.".into()),
                );
            }
            NetState::Idle => {
                let forms = match link_word {
                    Some(lk) => vec![format!("Idle \u{b7} {lk}"), "Idle".into()],
                    None => vec!["Idle".into()],
                };
                out.push(
                    Clause::new(1, forms)
                        .inked(InkKind::Words2)
                        .said("Network idle.".into()),
                );
            }
        }
    }
    // Where the link's rates are read the row's run says both directions ([`glance_run`]);
    // elsewhere it says the busier one against its peak, and Details the other while it moves.
    if !net_rates_read(snap) {
        let (other, mark, verb) = if second_binds(view) {
            (Some(view.first()), Mark::Down, "Receiving")
        } else {
            (view.second.clone(), Mark::Up, "Sending")
        };
        if let Some(now) = other.and_then(|s| s.now)
            && half_up(now / 1e5) >= 1.0
        {
            let n = rate_figure(now / 1e6);
            out.push(
                Clause::new(0, vec![format!("{n} MB/s")])
                    .marked(Some(mark))
                    .said(format!("{verb} {n} megabytes per second.")),
            );
        }
    }
    if let Some(ms) = snap.net.as_ref().and_then(|n| n.srtt_ms) {
        let rtt = rtt_word(ms);
        out.push(
            Clause::new(1, vec![format!("{rtt} RTT"), rtt.clone()])
                .inked(InkKind::Words2)
                .said(format!("TCP latency {rtt}.")),
        );
    }
    out
}

/// Format a passive TCP smoothed round-trip latency in milliseconds (`<1 ms`, `14 ms`, `240 ms`).
#[must_use]
pub fn rtt_word(srtt_ms: f32) -> String {
    if !srtt_ms.is_finite() || srtt_ms <= 0.0 {
        return "0 ms".into();
    }
    if srtt_ms < 1.0 {
        "<1 ms".into()
    } else if srtt_ms < 10.0 {
        format!("{srtt_ms:.1} ms")
    } else {
        format!("{:.0} ms", half_up(f64::from(srtt_ms)))
    }
}

/// Condense an OS processor brand string into a crisp hardware badge (`"M5 Max"`, `"Ryzen 9 9950X"`).
#[must_use]
pub fn compact_cpu_brand(brand: &str) -> String {
    let cleaned = brand
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace("(tm)", "");
    let before_at = cleaned.split('@').next().unwrap_or(&cleaned);
    let mut kept = Vec::new();
    for raw in before_at.split_whitespace() {
        let is_ordinal = ["st", "nd", "rd", "th"].iter().any(|suf| {
            raw.strip_suffix(suf).is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
            })
        });
        if raw.eq_ignore_ascii_case("Apple")
            || raw.eq_ignore_ascii_case("AMD")
            || raw.eq_ignore_ascii_case("Intel")
            || raw.eq_ignore_ascii_case("Genuine")
            || raw.eq_ignore_ascii_case("CPU")
            || raw.eq_ignore_ascii_case("Processor")
            || raw.ends_with("-Core")
            || raw.ends_with("-core")
            || is_ordinal
            || raw.eq_ignore_ascii_case("Gen")
        {
            continue;
        }
        kept.push(raw);
    }
    if kept.is_empty() {
        before_at.trim().to_owned()
    } else {
        kept.join(" ")
    }
}

/// Network Amount-column forms when the Details column stands beside it (`Ladder::Det`): reports
/// dual download/upload throughput without repeating connectivity state or RTT already shown in
/// Details.
#[must_use]
pub fn net_amount_det_forms(snap: &Snapshot) -> Vec<String> {
    let view = &snap.resources[Resource::Network.index()];
    let net = snap.net.as_ref();
    let state = net.map_or(NetState::Idle, |n| n.state);
    if state == NetState::Offline {
        return vec!["0 MB/s".into()];
    }
    let rx = net.and_then(|n| n.rates.map(|r| r.rx_bps)).or(view.now);
    let tx = net
        .and_then(|n| n.rates.map(|r| r.tx_bps))
        .or_else(|| view.second.as_ref().and_then(|s| s.now));
    let fmt_rate = |bps: f64| -> String {
        if bps < 950_000.0 {
            format!("{}K", grouped(half_up(bps.max(0.0) / 1e3) as u64))
        } else {
            format!("{}M", rate_figure(bps / 1e6))
        }
    };
    match (rx, tx) {
        (Some(r), Some(t)) => vec![format!("\u{2193}{} \u{2191}{}/s", fmt_rate(r), fmt_rate(t))],
        (Some(r), None) => vec![format!("\u{2193}{}/s", fmt_rate(r))],
        (None, Some(t)) => vec![format!("\u{2191}{}/s", fmt_rate(t))],
        (None, None) => Vec::new(),
    }
}

/// A volume's size as the legend says it, long and short: `2 TB`, `2T`; `1.5 TB`, `1.5T`;
/// `512 GB`, `512G`.
fn disk_total_compact(bytes: u64) -> (String, String) {
    if bytes >= 1_000_000_000_000 {
        let tb10 = half_up(bytes as f64 / 100_000_000_000.0) as u64;
        let (whole, frac) = (tb10 / 10, tb10 % 10);
        if frac == 0 {
            (format!("{whole} TB"), format!("{whole}T"))
        } else {
            (format!("{whole}.{frac} TB"), format!("{whole}.{frac}T"))
        }
    } else if bytes >= 1_000_000_000 {
        let gb = half_up(bytes as f64 / 1_000_000_000.0).max(1.0) as u64;
        (format!("{gb} GB"), format!("{gb}G"))
    } else {
        let s = disk(bytes);
        (s.clone(), s)
    }
}

/// One form of the machine's totals the deck's legend says after its noun: the words, and the
/// template the legend measures them by, each used figure's digits `8`s and as many as its total
/// allows (`88.8 of 32 GiB`, `8.88 of 4 TB`), so a changing reading never moves the legend from
/// one form to another (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotalsForm {
    /// What the legend says: `10 cores · 14.4 of 32 GiB · 2.34 of 4 TB`.
    pub said: String,
    /// What the legend measures: `10 cores · 88.8 of 32 GiB · 8.88 of 4 TB`.
    pub widest: String,
    /// Whether it says how much is used, not the totals alone: the figures a long form's
    /// short one keeps, which the legend gives up no figure of for the over mark's key's room
    /// (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4).
    pub used: bool,
}

/// `figure` with every digit an `8`.
fn eights(figure: &str) -> String {
    figure
        .chars()
        .map(|c| if c.is_ascii_digit() { '8' } else { c })
        .collect()
}

/// A volume's used bytes in its total's unit ([`disk_total_compact`]), at a fixed precision:
/// 0.01 TB on a total of 1 TB or more, whole GB below it (whole MB below 1 GB); and the widest
/// that figure can be, the total's own at that precision.
fn disk_used_figure(used: u64, total: u64) -> (String, String) {
    let (div, places) = if total >= 1_000_000_000_000 {
        (1e12, 2)
    } else if total >= 1_000_000_000 {
        (1e9, 0)
    } else {
        (1e6, 0)
    };
    let at = |b: u64| format!("{:.*}", places, b as f64 / div);
    (at(used.min(total)), eights(&at(total)))
}

/// The machine's totals the deck's legend says after its noun where no row's run says its
/// maximum, longest first: its cores, and its memory and disk as used of total where read
/// (`10 cores · 14.4 of 32 GiB · 2.34 of 4 TB`, then `10c · 14.4/32G · 2.34/4T`), then the
/// totals alone (`10 cores · 32 GiB · 4 TB`, `10c · 32G · 4T`); each part only where read, and
/// none where none is. Used is memory's in use (to 0.1 GiB) and the volume's size less what this
/// user could still write (to 0.01 TB, or whole GB on a smaller volume), the figures the
/// Memory and Disk lenses' first lines say.
#[must_use]
pub fn machine_totals(snap: &Snapshot) -> Vec<TotalsForm> {
    let cap = |res: Resource| {
        snap.resources[res.index()]
            .capacity
            .filter(|c| c.is_finite() && *c > 0.0)
    };
    let cores = cap(Resource::Cpu).map(|c| half_up(c) as u64);
    let mem_total = cap(Resource::Memory).map(|b| b as u64);
    let disk_total = cap(Resource::Disk).map(|b| b as u64);
    let mem = mem_total.map(gib_total);
    let hd = disk_total.map(disk_total_compact);
    // Each used figure with its widest, where read.
    let mem_used = mem_total
        .zip(snap.mem.as_ref().filter(|m| m.sample.total > 0))
        .map(|(total, m)| (gib_figure(m.sample.used()), eights(&gib_figure(total))));
    let disk_used = disk_total
        .zip(snap.volume.as_ref().filter(|v| v.total > 0))
        .map(|(total, v)| disk_used_figure(v.used(), total));
    let join = |parts: Vec<Option<String>>| -> Option<String> {
        let parts: Vec<String> = parts.into_iter().flatten().collect();
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    };
    let long_core = cores.map(|n| format!("{n} cores"));
    let short_core = cores.map(|n| format!("{n}c"));
    let totals = |long: bool| -> Option<String> {
        if long {
            join(vec![
                long_core.clone(),
                mem.as_ref().map(|g| format!("{g} GiB")),
                hd.as_ref().map(|(l, _)| l.clone()),
            ])
        } else {
            join(vec![
                short_core.clone(),
                mem.as_ref().map(|g| format!("{g}G")),
                hd.as_ref().map(|(_, s)| s.clone()),
            ])
        }
    };
    // Used of total, the used figure taken by `pick` (said, or its widest).
    let used = |long: bool, pick: fn(&(String, String)) -> &String| -> Option<String> {
        let m = mem.as_ref().map(|g| match (&mem_used, long) {
            (Some(u), true) => format!("{} of {g} GiB", pick(u)),
            (Some(u), false) => format!("{}/{g}G", pick(u)),
            (None, true) => format!("{g} GiB"),
            (None, false) => format!("{g}G"),
        });
        let d = hd.as_ref().map(|(l, sh)| match (&disk_used, long) {
            (Some(u), true) => format!("{} of {l}", pick(u)),
            (Some(u), false) => format!("{}/{sh}", pick(u)),
            (None, true) => l.clone(),
            (None, false) => sh.clone(),
        });
        let core = if long {
            long_core.clone()
        } else {
            short_core.clone()
        };
        join(vec![core, m, d])
    };
    let mut forms = Vec::with_capacity(4);
    if mem_used.is_some() || disk_used.is_some() {
        for long in [true, false] {
            if let (Some(said), Some(widest)) = (used(long, |u| &u.0), used(long, |u| &u.1)) {
                forms.push(TotalsForm {
                    said,
                    widest,
                    used: true,
                });
            }
        }
    }
    for long in [true, false] {
        if let Some(said) = totals(long) {
            forms.push(TotalsForm {
                widest: said.clone(),
                said,
                used: false,
            });
        }
    }
    forms
}

fn glance_processes_details(snap: &Snapshot) -> Vec<Clause> {
    let mut out: Vec<Clause> = processes_details(snap)
        .into_iter()
        .filter(|c| c.mark.is_some())
        .collect();
    if let Some((n, cap)) = yours_of(&snap.facts.room)
        && n as f64 >= GLANCE_YOURS_NEAR * cap as f64
    {
        let n = grouped(n);
        out.push(
            Clause::new(
                2,
                vec![
                    format!("yours {n} of {}", grouped(cap)),
                    format!("yours {n}"),
                ],
            )
            .kept(),
        );
    }
    out
}

#[cfg(test)]
#[path = "words_tests.rs"]
mod tests;
