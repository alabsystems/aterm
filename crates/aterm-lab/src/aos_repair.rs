// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-repair`: Aos proof runner for live momentum-glow fault detection, hot-swap repair and plan publication against a control.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::RelayClient;
use aterm_aos::Digest;
use aterm_effects::aos_momentum::bundle::{BUNDLE_DOMAIN, CandidateBundle, UncheckedBundle};
use aterm_effects::aos_momentum::m4::Evaluator;
use aterm_effects::aos_momentum::plan::{PLAN_LEN, PLAN_NAME, SchedulePlan};
use aterm_uds::CtlStream;

use crate::aos_phase::{self, ms};
use crate::instance::{self, Instance, Launch, World, take_flag, take_value};
use crate::lab_build::LAB_BUILD;
use crate::paths::{from_caller, paths};
use crate::pyjson::J;
use crate::stop::{self, Stopped};
use crate::{Fail, Res};

/// The verb's own help (`aos-repair --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-repair [--aterm PATH] [--control | --publish PLAN_HEX] [--keep] [--out DIR]

OPENS ONE WINDOW. Aos M2's live repair on a private windowed aterm (a debug build: `aos` is refused in a release one):
the front window's momentum glow attached under the demonstration fault demo-stranded-settle, 24 keys typed through
the normal key path, the stranded glow witnessed past its cold instant by reads that repaint nothing (aos status, trail
status, metrics frames), `aos repair code=shipped`, the clearing frame (a present with no key, the glow cold, `image` and
`image clean` with outside_caret_px 0, then a sealed `aos trace` with no key and no reset since the commit), 12 fresh
keys that heat, settle and clear by themselves with no further replay or publication, a sealed `aos trace` holding
exactly those 12 keys and no reset, `metrics percentiles`; the instance stopped by its pid. The control's trace must
hold exactly its 24 keys and no reset. A host whose `aos trace` lacks sealed=/keys=/resets=/ticks=/queries= fails.

  --aterm PATH  the aterm to run (default: the lab's pick, `targo --unverified lab paths`; with CARGO_TARGET_DIR set,
                the aterm beside this program in that target dir)
  --control     the negative control: attach the shipped code, skip the repair, and require the glow to clear by
                itself; then the 12-key phase of --publish with no publication (the caret moved once with no key in
                its settling tail, 3 s after the last key in both modes), measured from its exported trace (MEASURED
                lines, never gates; `aos-phase` re-derives them from the kept trace)
  --publish PLAN_HEX
                Aos M4: the control's first 24 keys, then `aos publish plan=PLAN_HEX` (a plan's 12 bytes as 24 hex
                digits), the plan live, `aos bundle` decoded and verified in the lab (only when the window's build
                names this lab's clean commit; else verify=skipped(foreign build), a FAIL), then the same 12-key
                phase measured: T, P and Z, presents, the slot rounding of the settle answers, and the moved caret's
                lag
  --keep        keep the scratch world (and the frames in it when --out is not given)
  --out DIR     where the frames, sidecars, contrast reports and the transcript go, OUTSIDE the repository

exit: 0 every gate passed; 1 a gate failed or a step never happened (a one-minute hang detector); 2 usage; 3 not run
";

/// `aterm.toml` lines enabling momentum glow alone on a quiet window.
pub const CONFIG: &[&str] = &[
    "cursor_trail_style = \"off\"",
    "cursor_momentum_glow = true",
    "cursor_blink = false",
    "motion = \"full\"",
    "trail_sounds = false",
    "serious_mode = false",
    "cursor_trail_bloom = false",
    "cursor_fire_shimmer = false",
    "hdr_glow = false",
    "cursor_glow_sdr_boost = 0.0",
];

/// The instance's aterm.toml: [`CONFIG`], [`instance::CONFIG_OFF`]'s tables, then the
/// stats Deck held off.
fn world_config() -> String {
    let top: Vec<String> = CONFIG.iter().map(|l| (*l).to_string()).collect();
    format!(
        "{}{}",
        instance::config_with(&top),
        crate::linux_proof::DECK_OFF
    )
}

/// The tab's program: one line, then nothing ever draws again but the line discipline's
/// echo of the typed keys.
const PROGRAM: &str =
    "printf 'aos-repair: the typed keys echo below; nothing else draws\\n'; exec /bin/sleep 3600";

/// The keys that strand the glow (24, the in-process test's count).
pub const STRAND_KEYS: &str = "thequickbrownfoxjumpsove";
/// The fresh keys after the repair (12, the in-process test's count).
pub const FRESH_KEYS: &str = "rthelazydogs";
/// A brisk typist.
const KEY_GAP: Duration = Duration::from_millis(60);
/// Between two status reads of a wait.
const POLL: Duration = Duration::from_millis(100);
/// A wait for something that must happen: a hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);
/// How far past the glow's cold instant the stranded readings start.
const PAST_COLD: Duration = Duration::from_millis(500);
/// Between the two stranded readings.
const HOLD: Duration = Duration::from_secs(1);
/// The momentum glow's hot floor (`aterm_effects::cursor_momentum::MOMENTUM_GLOW_HOT`).
pub const HOT_FLOOR: f64 = 0.02;
/// The grid: the trail gallery's.
const GRID: [&str; 6] = ["--columns", "72", "--lines", "14", "--font-px", "22"];

/// The `k=v` words of a reply line (the bare words are skipped).
#[must_use]
pub fn fields(line: &str) -> BTreeMap<&str, &str> {
    line.split_whitespace()
        .filter_map(|w| w.split_once('='))
        .collect()
}

/// An `OK …` line's body; an `ERR …` (or anything else) as the error.
fn body(line: &str) -> Result<&str, String> {
    let line = line.trim_end();
    line.strip_prefix("OK ")
        .ok_or_else(|| format!("not an OK reply: {line}"))
}

fn req<'a>(f: &BTreeMap<&str, &'a str>, key: &str) -> Result<&'a str, String> {
    f.get(key).copied().ok_or_else(|| format!("no {key}="))
}

fn num<T: std::str::FromStr>(f: &BTreeMap<&str, &str>, key: &str) -> Result<T, String> {
    let v = req(f, key)?;
    v.parse().map_err(|_| format!("{key}={v} is not a number"))
}

fn flag(f: &BTreeMap<&str, &str>, key: &str) -> Result<bool, String> {
    match req(f, key)? {
        "true" => Ok(true),
        "false" => Ok(false),
        v => Err(format!("{key}={v} is not a boolean")),
    }
}

/// A signed epoch offset, or `none`.
fn offset(f: &BTreeMap<&str, &str>, key: &str) -> Result<Option<i128>, String> {
    match req(f, key)? {
        "none" => Ok(None),
        v => v
            .parse()
            .map(Some)
            .map_err(|_| format!("{key}={v} is not an offset")),
    }
}

/// The last delivered tick, as `aos status` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// The caret fill (`rrggbb`), `None` when the tick filled nothing.
    pub fill: Option<String>,
    pub hot: bool,
    /// The fingerprint as printed (`0x…`).
    pub fp: String,
    /// Quads appended to the additive (dark page) and under-ink (light page) streams.
    pub out: u64,
    pub under: u64,
}

impl Frame {
    /// The inert frame: not hot, no fill, no fingerprint, no quad.
    #[must_use]
    pub fn inert(&self) -> bool {
        !self.hot && self.fill.is_none() && self.fp == "0x0" && self.out + self.under == 0
    }
}

/// An `aos status` line (the verb's `status_line`). Offsets are ns from the attach epoch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub window: u64,
    pub attached: bool,
    pub code: String,
    /// The build the window's code was compiled into (`build=`), when the line names one.
    pub build: Option<String>,
    /// An attached plan's presentation guard (`guard=moved:<b>,firings:<n>`) as printed.
    pub guard: Option<String>,
    pub hot: bool,
    pub active: bool,
    pub settling: bool,
    pub rev: u64,
    pub paused: bool,
    pub log: u64,
    pub broken: bool,
    pub latched: f64,
    /// `fp=<last>/<prev>` as printed.
    pub fp: String,
    pub clock: Option<i128>,
    pub cold_at: Option<i128>,
    pub query_at: Option<i128>,
    pub deadline: Option<i128>,
    pub frame: Option<Frame>,
    pub replays: u64,
    pub publications: u64,
    pub rejections: u64,
    pub delivered: u64,
    pub refused: u64,
    pub reanchors: u64,
    pub breaks: u64,
    pub restores: u64,
    pub clamped: u64,
}

/// Parse an `aos status` (or `aos attach`) reply.
///
/// # Errors
/// Not an OK reply, or a field missing or malformed.
pub fn parse_status(line: &str) -> Result<Status, String> {
    let b = body(line)?;
    let f = fields(b);
    let mut s = Status {
        window: num(&f, "window")?,
        attached: flag(&f, "attached")?,
        code: req(&f, "code")?.to_string(),
        hot: flag(&f, "hot")?,
        active: flag(&f, "active")?,
        settling: flag(&f, "settling")?,
        ..Status::default()
    };
    if !s.attached {
        return Ok(s);
    }
    s.build = f.get("build").map(|b| (*b).to_string());
    s.guard = f.get("guard").map(|g| (*g).to_string());
    s.rev = num(&f, "rev")?;
    s.paused = flag(&f, "paused")?;
    s.log = num(&f, "log")?;
    s.broken = flag(&f, "broken")?;
    s.latched = num(&f, "latched")?;
    s.fp = req(&f, "fp")?.to_string();
    s.clock = offset(&f, "clock")?;
    s.cold_at = offset(&f, "cold_at")?;
    s.query_at = offset(&f, "query_at")?;
    s.deadline = offset(&f, "deadline")?;
    if f.contains_key("frame_hot") {
        let (out, under) = req(&f, "quads")?
            .split_once('+')
            .ok_or("quads= is not <out>+<under>")?;
        s.frame = Some(Frame {
            fill: match req(&f, "frame_fill")? {
                "none" => None,
                c => Some(c.to_string()),
            },
            hot: flag(&f, "frame_hot")?,
            fp: req(&f, "frame_fp")?.to_string(),
            out: out.parse().map_err(|_| format!("quads out {out}"))?,
            under: under.parse().map_err(|_| format!("quads under {under}"))?,
        });
    }
    s.replays = num(&f, "replays")?;
    s.publications = num(&f, "publications")?;
    s.rejections = num(&f, "rejections")?;
    s.delivered = num(&f, "delivered")?;
    s.refused = num(&f, "refused")?;
    s.reanchors = num(&f, "reanchors")?;
    s.breaks = num(&f, "breaks")?;
    s.restores = num(&f, "restores")?;
    s.clamped = num(&f, "clamped")?;
    Ok(s)
}

/// An `aos repair` reply (the verb's `repair_line`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repair {
    pub window: u64,
    pub paused_first: bool,
    pub rev: u64,
    /// The published (candidate) code's artifact name.
    pub code: String,
    /// The live code's artifact name.
    pub live: String,
    pub live_verdict: String,
    pub candidate_verdict: String,
    pub same_code_reproduced: bool,
    pub grade: String,
    pub trace: String,
    pub ancestor: String,
    pub range: String,
    pub log_epoch: u64,
    pub delivered: u64,
}

/// Parse an `aos repair` reply.
///
/// # Errors
/// Not an OK `repaired` reply, or a field missing or malformed.
pub fn parse_repair(line: &str) -> Result<Repair, String> {
    let b = body(line)?;
    if !b.split_whitespace().any(|w| w == "repaired") {
        return Err(format!("not a repair reply: {b}"));
    }
    let f = fields(b);
    Ok(Repair {
        window: num(&f, "window")?,
        paused_first: flag(&f, "paused_first")?,
        rev: num(&f, "rev")?,
        code: req(&f, "code")?.to_string(),
        live: req(&f, "live")?.to_string(),
        live_verdict: req(&f, "live_verdict")?.to_string(),
        candidate_verdict: req(&f, "candidate_verdict")?.to_string(),
        same_code_reproduced: flag(&f, "same_code_reproduced")?,
        grade: req(&f, "grade")?.to_string(),
        trace: req(&f, "trace")?.to_string(),
        ancestor: req(&f, "ancestor")?.to_string(),
        range: req(&f, "range")?.to_string(),
        log_epoch: num(&f, "log_epoch")?,
        delivered: num(&f, "delivered")?,
    })
}

/// An `aos trace` reply (the verb's `trace_line`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceLine {
    pub log: u64,
    pub log_epoch: u64,
    pub first: u64,
    pub end: u64,
    /// The trace digest, `none` when the log is broken.
    pub trace: String,
    pub checkpoint: String,
    pub broken: bool,
    /// `<checkpoint>+<log>` bytes as printed.
    pub bytes: String,
    /// Whether the exported bytes decoded with their end seal.
    pub sealed: bool,
    /// Per-kind event counts in the exported log since the last anchor.
    pub keys: u64,
    pub resets: u64,
    pub ticks: u64,
    pub queries: u64,
}

/// Parse an `aos trace` reply.
///
/// # Errors
/// Not an OK reply, or a field missing or malformed.
pub fn parse_trace(line: &str) -> Result<TraceLine, String> {
    let f = fields(body(line)?);
    Ok(TraceLine {
        log: num(&f, "log")?,
        log_epoch: num(&f, "log_epoch")?,
        first: num(&f, "first")?,
        end: num(&f, "end")?,
        trace: req(&f, "trace")?.to_string(),
        checkpoint: req(&f, "checkpoint")?.to_string(),
        broken: flag(&f, "broken")?,
        bytes: req(&f, "bytes")?.to_string(),
        sealed: flag(&f, "sealed")?,
        keys: num(&f, "keys")?,
        resets: num(&f, "resets")?,
        ticks: num(&f, "ticks")?,
        queries: num(&f, "queries")?,
    })
}

/// The number of keys `word` types (one `key <c>` per char).
#[must_use]
pub fn typed(word: &str) -> u64 {
    u64::try_from(word.chars().count()).unwrap_or(u64::MAX)
}

/// The cursor a range `a..b` ends at.
fn range_end(range: &str) -> Option<u64> {
    range.split_once("..").and_then(|(_, end)| end.parse().ok())
}

/// What the drive reads of a `trail status` row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrailRow {
    /// The caret's fill owner (`momentum`, `none`, …).
    pub block_fill: String,
    /// The momentum integrator's value now (`momentum_glow=`).
    pub momentum_glow: f64,
    /// The window's focus as the cursor-effect seam resolves it (a typed wake counts).
    pub focused: bool,
}

/// Parse a `trail status` row.
///
/// # Errors
/// Not a `trail` row, or a field missing or malformed.
pub fn parse_trail(row: &str) -> Result<TrailRow, String> {
    let row = row.trim();
    if !row.starts_with("trail ") {
        return Err(format!("not a trail status row: {row}"));
    }
    let f = fields(row);
    Ok(TrailRow {
        block_fill: req(&f, "block_fill")?.to_string(),
        momentum_glow: num(&f, "momentum_glow")?,
        focused: flag(&f, "focused")?,
    })
}

/// An `image` reply: `OK <w> <h> <path>`, or with `--meta` `OK <w> <h> image-meta … path="<path>"`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub path: String,
    /// `frame-phase=` with `--meta` (`destination`, `presented`, `staged`).
    pub phase: Option<String>,
}

/// Parse an `image` reply. The path is always the last word.
///
/// # Errors
/// Not an OK reply with a size and a path.
pub fn parse_image(line: &str) -> Result<Shot, String> {
    let b = body(line.lines().next().unwrap_or(""))?;
    let words: Vec<&str> = b.split_whitespace().collect();
    let (Some(w), Some(h), Some(last)) = (words.first(), words.get(1), words.last()) else {
        return Err(format!("not an image reply: {b}"));
    };
    if words.len() < 3 {
        return Err(format!("not an image reply: {b}"));
    }
    let path = last
        .strip_prefix("path=\"")
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(last);
    Ok(Shot {
        width: w.parse().map_err(|_| format!("image width {w}"))?,
        height: h.parse().map_err(|_| format!("image height {h}"))?,
        path: path.to_string(),
        phase: fields(b).get("frame-phase").map(|p| (*p).to_string()),
    })
}

/// A count of a `metrics` line.
///
/// # Errors
/// The field is missing or not a count.
pub fn count(line: &str, key: &str) -> Result<u64, String> {
    num(&fields(line), key)
}

/// One gate's verdict and what was observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    pub name: &'static str,
    pub pass: bool,
    pub observed: String,
}

const fn gate(name: &'static str, pass: bool, observed: String) -> Gate {
    Gate {
        name,
        pass,
        observed,
    }
}

fn frame_word(f: Option<&Frame>) -> String {
    f.map_or_else(
        || "no tick".to_string(),
        |f| {
            format!(
                "frame_hot={} fill={} fp={} quads={}+{}",
                f.hot,
                f.fill.as_deref().unwrap_or("none"),
                f.fp,
                f.out,
                f.under
            )
        },
    )
}

fn opt(v: Option<i128>) -> String {
    v.map_or_else(|| "none".to_string(), |t| t.to_string())
}

/// What the repair run read while the demonstration fault held the glow (step 3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stranded {
    /// The two readings past `cold_at`, [`HOLD`] apart, and `frames` taken with each.
    pub a: Status,
    pub b: Status,
    pub frames_a: u64,
    pub frames_b: u64,
    /// The hold between them, measured.
    pub hold_ms: u64,
    /// `trail status` at the first reading.
    pub trail: TrailRow,
    /// Whether the window held OS key focus before the first key.
    pub key_window: bool,
}

/// Evaluate the stranded-glow verification gates.
#[must_use]
pub fn stranded_gates(e: &Stranded) -> Vec<Gate> {
    let (a, b) = (&e.a, &e.b);
    let past = |s: &Status| matches!((s.query_at, s.cold_at), (Some(q), Some(c)) if q > c);
    let halo = b
        .frame
        .as_ref()
        .is_some_and(|f| f.hot && f.out > 0 && f.fill.is_some());
    vec![
        gate(
            "fault-attached",
            b.attached && b.code == "demo-stranded-settle" && b.rev == 0 && !b.paused,
            format!(
                "attached={} code={} rev={} paused={}",
                b.attached, b.code, b.rev, b.paused
            ),
        ),
        gate(
            "stranded-hot-past-cold",
            [a, b]
                .iter()
                .all(|s| s.hot && s.settling && !s.active && past(s)),
            format!(
                "hot={}/{} settling={}/{} active={}/{} cold_at={} query_at={}/{}",
                a.hot,
                b.hot,
                a.settling,
                b.settling,
                a.active,
                b.active,
                opt(b.cold_at),
                opt(a.query_at),
                opt(b.query_at)
            ),
        ),
        gate(
            "no-wake-named",
            a.deadline.is_none() && b.deadline.is_none(),
            format!("deadline={}/{}", opt(a.deadline), opt(b.deadline)),
        ),
        gate(
            "no-present-while-stranded",
            e.frames_b == e.frames_a && e.hold_ms >= 1000,
            format!(
                "frames {} -> {} over {} ms past cold_at",
                e.frames_a, e.frames_b, e.hold_ms
            ),
        ),
        gate(
            "last-tick-unchanged",
            a.frame == b.frame && a.fp == b.fp && a.latched.to_bits() == b.latched.to_bits(),
            format!(
                "latched={:.4}/{:.4} fp={}/{}",
                a.latched, b.latched, a.fp, b.fp
            ),
        ),
        gate(
            "halo-on-last-frame",
            halo && e.trail.block_fill == if e.key_window { "momentum" } else { "none" },
            format!(
                "{} block_fill={} key_window={}",
                frame_word(b.frame.as_ref()),
                e.trail.block_fill,
                e.key_window
            ),
        ),
        gate(
            "integrator-cold",
            e.trail.momentum_glow < HOT_FLOOR,
            format!("momentum_glow={:.2}", e.trail.momentum_glow),
        ),
        gate(
            "stranded-admitted",
            b.refused == 0 && !b.broken,
            format!("refused={} broken={}", b.refused, b.broken),
        ),
    ]
}

/// Evaluate the `aos repair` verification gates.
#[must_use]
pub fn repair_gates(r: &Repair, before: &Status) -> Vec<Gate> {
    vec![
        gate(
            "repair-paused-first",
            r.paused_first,
            format!("paused_first={}", r.paused_first),
        ),
        gate(
            "live-fails-hot-has-wake",
            r.live_verdict.starts_with("fail:HotHasWake@"),
            format!("live_verdict={}", r.live_verdict),
        ),
        gate(
            "candidate-passes",
            r.candidate_verdict == "pass",
            format!("candidate_verdict={}", r.candidate_verdict),
        ),
        gate(
            "same-code-reproduced",
            r.same_code_reproduced,
            format!("same_code_reproduced={}", r.same_code_reproduced),
        ),
        gate(
            "grade-tested",
            r.grade == "tested",
            format!("grade={}", r.grade),
        ),
        gate(
            "new-revision",
            r.rev > before.rev,
            format!("rev {} -> {}", before.rev, r.rev),
        ),
        gate(
            "shipped-over-the-fault",
            r.code == "momentum-glow.shipped" && r.live == "momentum-glow.demo-stranded-settle",
            format!("code={} live={}", r.code, r.live),
        ),
        gate(
            "nothing-queued",
            r.delivered == 0,
            format!("delivered={}", r.delivered),
        ),
    ]
}

/// What the repair run read after the commit (step 5).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cleared {
    /// `frames` at the hold's end, and at the first cold reading.
    pub frames_before: u64,
    pub frames_after: u64,
    pub status: Status,
    pub repaired_rev: u64,
    pub trail: TrailRow,
    /// `diff_vs_clean.outside_caret_px` of the frame taken after.
    pub outside_caret_px: Option<i64>,
}

/// Evaluate the post-commit clearing gates.
#[must_use]
pub fn cleared_gates(e: &Cleared) -> Vec<Gate> {
    let s = &e.status;
    vec![
        gate(
            "clearing-frame-presented",
            e.frames_after > e.frames_before,
            format!(
                "frames {} -> {} with no key",
                e.frames_before, e.frames_after
            ),
        ),
        gate(
            "cold-after-repair",
            !s.hot
                && !s.active
                && !s.settling
                && s.code == "shipped"
                && s.rev == e.repaired_rev
                && !s.paused
                && s.publications == 1,
            format!(
                "hot={} active={} settling={} code={} rev={} paused={} publications={}",
                s.hot, s.active, s.settling, s.code, s.rev, s.paused, s.publications
            ),
        ),
        gate(
            "clearing-tick-inert",
            s.frame.as_ref().is_some_and(Frame::inert) && e.trail.block_fill == "none",
            format!(
                "{} block_fill={}",
                frame_word(s.frame.as_ref()),
                e.trail.block_fill
            ),
        ),
        gate(
            "clearing-pixels",
            e.outside_caret_px == Some(0),
            format!("outside_caret_px={}", px_word(e.outside_caret_px)),
        ),
    ]
}

fn px_word(v: Option<i64>) -> String {
    v.map_or_else(|| "unmeasured".to_string(), |p| p.to_string())
}

/// Readings of a glow typed hot and left to cool under the shipped code.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cooled {
    /// A reading with the glow hot after the keys.
    pub hot: Status,
    /// A reading while it settled, of a query made after the last key.
    pub settling: Status,
    pub frames_settling: u64,
    /// The first cold reading, and `frames` with it.
    pub cold: Status,
    pub frames_cold: u64,
    pub trail: TrailRow,
    pub outside_caret_px: Option<i64>,
}

/// Evaluate the self-cooling glow verification gates.
#[must_use]
pub fn cooled_gates(e: &Cooled) -> Vec<Gate> {
    cooled_gates_in(Burst::First, "shipped", e)
}

/// Which typed burst a self-cooling glow's gates judge: the first after the attach (or the repair's fresh keys), or the 12 fresh keys after it, whose gates are named `fresh-…`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Burst {
    First,
    Fresh,
}

/// [`cooled_gates`] for `burst`, under the live code whose status name is `code` (`shipped`, or [`PLAN_NAME`] for a published plan).
#[must_use]
pub fn cooled_gates_in(burst: Burst, code: &str, e: &Cooled) -> Vec<Gate> {
    let c = &e.cold;
    let name = |first: &'static str, fresh: &'static str| match burst {
        Burst::First => first,
        Burst::Fresh => fresh,
    };
    vec![
        gate(
            name("keys-heat", "fresh-keys-heat"),
            e.hot.hot && e.hot.code == code,
            format!("hot={} code={}", e.hot.hot, e.hot.code),
        ),
        gate(
            name("settling-names-wake", "fresh-settling-names-wake"),
            e.settling.settling && e.settling.deadline.is_some(),
            format!(
                "settling={} query_at={} deadline={} cold_at={}",
                e.settling.settling,
                opt(e.settling.query_at),
                opt(e.settling.deadline),
                opt(e.settling.cold_at)
            ),
        ),
        gate(
            name("cleared-by-itself", "fresh-cleared-by-itself"),
            e.frames_cold > e.frames_settling && !c.hot && !c.active && !c.settling,
            format!(
                "frames {} -> {} with no key; hot={} active={} settling={}",
                e.frames_settling, e.frames_cold, c.hot, c.active, c.settling
            ),
        ),
        gate(
            name("last-tick-inert", "fresh-last-tick-inert"),
            c.frame.as_ref().is_some_and(Frame::inert) && e.trail.block_fill == "none",
            format!(
                "{} block_fill={}",
                frame_word(c.frame.as_ref()),
                e.trail.block_fill
            ),
        ),
        gate(
            name("cooled-pixels", "fresh-cooled-pixels"),
            e.outside_caret_px == Some(0),
            format!("outside_caret_px={}", px_word(e.outside_caret_px)),
        ),
        gate(
            name("admitted", "fresh-admitted"),
            c.refused == 0 && !c.broken,
            format!("refused={} broken={}", c.refused, c.broken),
        ),
    ]
}

/// Evaluate the post-repair continuation gates.
#[must_use]
pub fn continuation_gates(
    committed: &Status,
    after: &Status,
    beside: &Status,
    trace: &TraceLine,
) -> Vec<Gate> {
    continuation_gates_for("shipped", committed, after, beside, trace)
}

/// [`continuation_gates`] for the live code whose status name is `code`: [`PLAN_NAME`] after a publication, or `shipped` in the control (`committed` then the reading before its fresh keys).
#[must_use]
pub fn continuation_gates_for(
    code: &str,
    committed: &Status,
    after: &Status,
    beside: &Status,
    trace: &TraceLine,
) -> Vec<Gate> {
    let tally = |s: &Status| {
        (
            s.replays,
            s.publications,
            s.rejections,
            s.reanchors,
            s.breaks,
            s.restores,
        )
    };
    vec![
        gate(
            "same-published-pair",
            after.code == code && after.rev == committed.rev,
            format!("code={} rev {} -> {}", after.code, committed.rev, after.rev),
        ),
        gate(
            "no-further-replay-or-publication",
            tally(after) == tally(committed),
            format!(
                "replays/publications/rejections/reanchors/breaks/restores {:?} -> {:?}",
                tally(committed),
                tally(after)
            ),
        ),
        gate(
            "fresh-keys-delivered",
            after.delivered > committed.delivered,
            format!("delivered {} -> {}", committed.delivered, after.delivered),
        ),
        gate(
            "trace-agrees",
            trace.log == beside.log
                && trace.end.checked_sub(trace.first) == Some(trace.log)
                && !trace.broken
                && trace.trace != "none",
            format!(
                "trace log={} first={} end={} broken={} status log={}",
                trace.log, trace.first, trace.end, trace.broken, beside.log
            ),
        ),
    ]
}

/// Evaluate the exported trace log kind-counts gate.
#[must_use]
pub fn log_kinds_gate(name: &'static str, trace: &TraceLine, keys: u64) -> Gate {
    let kinds = [trace.keys, trace.resets, trace.ticks, trace.queries]
        .into_iter()
        .try_fold(0u64, u64::checked_add);
    gate(
        name,
        trace.sealed && trace.keys == keys && trace.resets == 0 && kinds == Some(trace.log),
        format!(
            "sealed={} keys={} typed={keys} resets={} ticks={} queries={} log={}",
            trace.sealed, trace.keys, trace.resets, trace.ticks, trace.queries, trace.log
        ),
    )
}

/// Evaluate the `aos trace` gates immediately after commit before fresh keys.
#[must_use]
pub fn post_commit_gates(r: &Repair, trace: &TraceLine) -> Vec<Gate> {
    post_commit_gates_at(r.log_epoch, &r.range, trace)
}

/// [`post_commit_gates`] for a commit whose witness read the log epoch `log_epoch` over the range `range` (`a..b`): a repair's, or a publication's.
#[must_use]
pub fn post_commit_gates_at(log_epoch: u64, range: &str, trace: &TraceLine) -> Vec<Gate> {
    vec![
        gate(
            "post-commit-log",
            trace.log_epoch > log_epoch && range_end(range) == Some(trace.first) && !trace.broken,
            format!(
                "log_epoch {log_epoch} -> {} range={range} first={} broken={}",
                trace.log_epoch, trace.first, trace.broken
            ),
        ),
        log_kinds_gate("post-commit-no-key-or-reset", trace, 0),
    ]
}

/// The control's own gates: the shipped code attached and never replaced.
#[must_use]
pub fn control_gates(attached: &Status, cold: &Status) -> Vec<Gate> {
    vec![
        gate(
            "shipped-attached",
            attached.attached && attached.code == "shipped" && attached.rev == 0,
            format!(
                "attached={} code={} rev={}",
                attached.attached, attached.code, attached.rev
            ),
        ),
        gate(
            "no-publication",
            cold.publications == 0 && cold.replays == 0 && cold.rejections == 0 && cold.rev == 0,
            format!(
                "publications={} replays={} rejections={} rev={}",
                cold.publications, cold.replays, cold.rejections, cold.rev
            ),
        ),
    ]
}

/// An `aos publish` reply (the verb's `publish_line`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishLine {
    pub window: u64,
    pub paused_first: bool,
    pub rev: u64,
    /// The plan's 12 bytes in lowercase hex, and its candidate id's first 12 digits.
    pub plan: String,
    pub plan_digest: String,
    pub useful: bool,
    pub gain_ppm: i64,
    pub s0_gain_ppm: i64,
    /// The published (candidate) code's artifact name, the live code's, and their verdicts.
    pub code: String,
    pub live: String,
    pub live_verdict: String,
    pub candidate_verdict: String,
    pub trace: String,
    pub ancestor: String,
    pub range: String,
    pub log_epoch: u64,
    pub grade: String,
    /// The bundle's digest, its first 12 digits.
    pub bundle: String,
    pub delivered: u64,
}

/// Parse an `aos publish` reply, whose witness half (`code=` to `log_epoch=`) is required.
///
/// # Errors
/// Not an OK `published` reply (a refusal is an `ERR …` line), or a field missing or malformed.
pub fn parse_publish(line: &str) -> Result<PublishLine, String> {
    let b = body(line)?;
    if !b.split_whitespace().any(|w| w == "published") {
        return Err(format!("not a publish reply: {b}"));
    }
    let f = fields(b);
    Ok(PublishLine {
        window: num(&f, "window")?,
        paused_first: flag(&f, "paused_first")?,
        rev: num(&f, "rev")?,
        plan: req(&f, "plan")?.to_string(),
        plan_digest: req(&f, "plan_digest")?.to_string(),
        useful: flag(&f, "useful")?,
        gain_ppm: num(&f, "gain_ppm")?,
        s0_gain_ppm: num(&f, "s0_gain_ppm")?,
        code: req(&f, "code")?.to_string(),
        live: req(&f, "live")?.to_string(),
        live_verdict: req(&f, "live_verdict")?.to_string(),
        candidate_verdict: req(&f, "candidate_verdict")?.to_string(),
        trace: req(&f, "trace")?.to_string(),
        ancestor: req(&f, "ancestor")?.to_string(),
        range: req(&f, "range")?.to_string(),
        log_epoch: num(&f, "log_epoch")?,
        grade: req(&f, "grade")?.to_string(),
        bundle: req(&f, "bundle")?.to_string(),
        delivered: num(&f, "delivered")?,
    })
}

/// The plan's artifact name, as a publication names its candidate code.
pub const PLAN_CODE: &str = "momentum-glow.schedule-plan";

/// The publication gates: paused first with nothing queued, the plan the run named, useful with a gain, a new revision of the plan over the shipped code, both verdicts passing at grade `tested`, and the bundle's digest named.
#[must_use]
pub fn publish_gates(p: &PublishLine, plan_hex: &str, before: &Status) -> Vec<Gate> {
    vec![
        gate(
            "publish-paused-first",
            p.paused_first && p.delivered == 0,
            format!("paused_first={} delivered={}", p.paused_first, p.delivered),
        ),
        gate(
            "publish-plan",
            p.plan == plan_hex.to_ascii_lowercase(),
            format!("plan={} named={plan_hex}", p.plan),
        ),
        gate("publish-useful", p.useful, format!("useful={}", p.useful)),
        gate(
            "publish-gain",
            p.gain_ppm > 0,
            format!("gain_ppm={} s0_gain_ppm={}", p.gain_ppm, p.s0_gain_ppm),
        ),
        gate(
            "publish-new-revision",
            p.rev > before.rev,
            format!("rev {} -> {}", before.rev, p.rev),
        ),
        gate(
            "plan-over-shipped",
            p.code == PLAN_CODE && p.live == "momentum-glow.shipped",
            format!("code={} live={}", p.code, p.live),
        ),
        gate(
            "publish-witness",
            p.live_verdict == "pass" && p.candidate_verdict == "pass" && p.grade == "tested",
            format!(
                "live_verdict={} candidate_verdict={} grade={}",
                p.live_verdict, p.candidate_verdict, p.grade
            ),
        ),
        gate(
            "publish-bundle-digest",
            aterm_codec::hex::is_lower(&p.bundle, 12),
            format!("bundle={}", p.bundle),
        ),
    ]
}

/// The plan live after its publication: the status names the plan at the published revision, running, and published once.
#[must_use]
pub fn plan_live_gate(s: &Status, p: &PublishLine) -> Gate {
    gate(
        "plan-live",
        s.attached && s.code == PLAN_NAME && s.rev == p.rev && !s.paused && s.publications == 1,
        format!(
            "attached={} code={} rev={} published_rev={} paused={} publications={} guard={}",
            s.attached,
            s.code,
            s.rev,
            p.rev,
            s.paused,
            s.publications,
            s.guard.as_deref().unwrap_or("none")
        ),
    )
}

/// An `aos bundle` reply (the verb's `bundle_line`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BundleLine {
    pub window: u64,
    /// The bundle's full digest (64 hex digits).
    pub bundle: String,
    /// The revision it published.
    pub rev: u64,
    pub bytes: usize,
    /// Its bytes in lowercase hex.
    pub hex: String,
}

/// Parse an `aos bundle` reply.
///
/// # Errors
/// Not an OK reply, or a field missing or malformed.
pub fn parse_bundle_line(line: &str) -> Result<BundleLine, String> {
    let f = fields(body(line)?);
    Ok(BundleLine {
        window: num(&f, "window")?,
        bundle: req(&f, "bundle")?.to_string(),
        rev: num(&f, "rev")?,
        bytes: num(&f, "bytes")?,
        hex: req(&f, "hex")?.to_string(),
    })
}

/// A bundle reply's bytes: its hex decoded, exactly as long as `bytes=` says.
///
/// # Errors
/// Not hex, or another length.
pub fn bundle_bytes(b: &BundleLine) -> Result<Vec<u8>, String> {
    let bytes = aterm_codec::hex::decode_vec(b.hex.as_bytes())
        .ok_or_else(|| "hex= is not an even run of hex digits".to_string())?;
    if bytes.len() != b.bytes {
        return Err(format!(
            "hex= holds {} bytes, bytes={}",
            bytes.len(),
            b.bytes
        ));
    }
    Ok(bytes)
}

/// What the lab's own verification of a window's bundle came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verify {
    /// `CandidateBundle::verify` accepted it under an evaluator of the window's build.
    Verified,
    /// It refused the bundle, or no evaluator could be built, as a line.
    Refused(String),
    /// Not run, and never a pass: the window's build and this lab's do not name the same clean commit, so the lab's evaluator is not shown to be the window's interpreter.
    Foreign { instance: String, lab: String },
}

impl Verify {
    /// `verify=verified`, `verify=refused(<why>)` or `verify=skipped(foreign build) …`.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Verify::Verified => "verify=verified".to_string(),
            Verify::Refused(why) => format!("verify=refused({why})"),
            Verify::Foreign { instance, lab } => {
                format!("verify=skipped(foreign build) instance={instance} lab={lab}")
            }
        }
    }
}

/// The commit a build id names: the text after its last `+g`, when it is a clean commit of at least 12 lowercase hex digits.
fn clean_commit(build: &str) -> Option<&str> {
    let (_, commit) = build.rsplit_once("+g")?;
    (commit.len() >= 12
        && commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(commit)
}

/// The window's bundle `bytes` verified under an evaluator of the window's own build `instance`, but only when `instance` and the lab's build `lab` name the same clean commit (otherwise [`Verify::Foreign`], never run).
/// `aos-search --verify` takes only the lab's exact build name, which no window's `build_id()` is, so a window's publication is verified here or not at all.
#[must_use]
pub fn verify_in_lab(bytes: &[u8], instance: &str, lab: &str) -> Verify {
    match (clean_commit(instance), clean_commit(lab)) {
        (Some(a), Some(b)) if a == b => {}
        _ => {
            return Verify::Foreign {
                instance: instance.to_string(),
                lab: lab.to_string(),
            };
        }
    }
    match Evaluator::v1(instance, Instant::now()) {
        Err(e) => Verify::Refused(format!("no evaluator of {instance}: {e}")),
        Ok(ev) => match CandidateBundle::verify(bytes, &ev) {
            Ok(_) => Verify::Verified,
            Err(e) => Verify::Refused(e.to_string()),
        },
    }
}

/// The bundle gates: the bundle exported whole at the published revision, its plan the run's, its publication part naming that revision, and the lab's own verification of it (a skipped one fails).
#[must_use]
pub fn bundle_gates(
    b: &BundleLine,
    p: &PublishLine,
    plan: &[u8; PLAN_LEN],
    verify: &Verify,
) -> Vec<Gate> {
    let bytes = bundle_bytes(b);
    let digest = bytes
        .as_ref()
        .map_or_else(Clone::clone, |v| Digest::of(BUNDLE_DOMAIN, &[v]).hex());
    let decoded = bytes
        .and_then(|v| UncheckedBundle::decode(&v).map_err(|e| format!("does not decode: {e}")));
    let (plan_word, publication_word) = match &decoded {
        Ok(d) => (
            aterm_codec::hex::encode_lower(&d.plan),
            d.publication
                .as_ref()
                .map_or_else(|| "none".to_string(), |x| x.revision.to_string()),
        ),
        Err(e) => (e.clone(), e.clone()),
    };
    vec![
        gate(
            "bundle-exported",
            digest == b.bundle
                && aterm_codec::hex::is_lower(&p.bundle, 12)
                && b.bundle.starts_with(&p.bundle)
                && b.rev == p.rev,
            format!(
                "bytes={} digest={digest} named={} published={} rev={} published_rev={}",
                b.bytes, b.bundle, p.bundle, b.rev, p.rev
            ),
        ),
        gate(
            "bundle-plan",
            decoded.as_ref().is_ok_and(|d| d.plan == plan[..]),
            format!(
                "plan={plan_word} named={}",
                aterm_codec::hex::encode_lower(plan)
            ),
        ),
        gate(
            "bundle-publication",
            decoded
                .as_ref()
                .is_ok_and(|d| d.publication.as_ref().map(|x| x.revision) == Some(p.rev)),
            format!("publication_rev={publication_word} published_rev={}", p.rev),
        ),
        gate(
            "bundle-verifies",
            *verify == Verify::Verified,
            verify.word(),
        ),
    ]
}

/// The run's one machine-readable line.
#[must_use]
pub fn result_line(mode: &str, gates: &[Gate], extra: &[(&str, String)]) -> String {
    let failed: Vec<&str> = gates.iter().filter(|g| !g.pass).map(|g| g.name).collect();
    let passed = gates.len() - failed.len();
    let mut line = format!(
        "aos-repair result={} mode={mode} gates={passed}/{} failed={}",
        if failed.is_empty() && !gates.is_empty() {
            "pass"
        } else {
            "fail"
        },
        gates.len(),
        if failed.is_empty() {
            "none".to_string()
        } else {
            failed.join(",")
        }
    );
    for (k, v) in extra {
        let _ = write!(line, " {k}={}", v.replace(' ', "_"));
    }
    line
}

/// Why a step ended the run early.
enum Halt {
    /// An interrupt (its signal).
    Stopped(i32),
    /// A step that did not happen, or a reply the drive could not read.
    Broke(String),
}

impl From<Stopped> for Halt {
    fn from(s: Stopped) -> Self {
        Halt::Stopped(s.0)
    }
}

impl From<String> for Halt {
    fn from(msg: String) -> Self {
        Halt::Broke(msg)
    }
}

/// A run's connection, its output and its transcript.
struct Drive<'a> {
    inst: &'a Instance,
    wire: RelayClient<CtlStream>,
    out: PathBuf,
    stem: &'static str,
    /// The attach request's send and reply instants: the epoch lies between them.
    attach_sent: Instant,
    attach_answered: Instant,
    transcript: Vec<String>,
    /// Status reads made by waits (each one a main-loop turn the planner follows).
    polls: u32,
    /// OS key focus before the first key (`trail status` `focused=`, no typed wake yet).
    key_window: bool,
    /// The build the window's code names (the attach reply's `build=`).
    build: Option<String>,
    /// The `MEASURED …` lines, in order (never a gate).
    measured: Vec<String>,
}

impl Drive<'_> {
    fn note(&mut self, line: String) {
        println!("{line}");
        self.transcript.push(line);
    }

    /// One request on the held connection, its reply line returned.
    fn ask(&mut self, request: &str) -> Result<String, Halt> {
        let reply = self
            .wire
            .request_line(request)
            .map_err(|e| format!("`{request}`: the held control connection failed: {e}"))?;
        self.note(format!("> {request}\n< {reply}"));
        Ok(reply)
    }

    /// `aos status`, read, with its reply line.
    fn status_read(&mut self) -> Result<(Status, String), Halt> {
        let reply = self
            .wire
            .request_line("aos status")
            .map_err(|e| format!("`aos status`: the held control connection failed: {e}"))?;
        let s = parse_status(&reply).map_err(|e| format!("aos status: {e}: {reply}"))?;
        Ok((s, reply))
    }

    /// `aos status`, kept in the transcript.
    fn status(&mut self) -> Result<Status, Halt> {
        let (s, reply) = self.status_read()?;
        self.note(format!("> aos status\n< {reply}"));
        Ok(s)
    }

    /// `metrics`' `frames`: every successful present since the instance started.
    fn frames(&mut self) -> Result<u64, Halt> {
        let reply = self.ask("metrics")?;
        Ok(count(body(&reply)?, "frames").map_err(|e| format!("metrics: {e}"))?)
    }

    fn trail(&mut self) -> Result<TrailRow, Halt> {
        let (head, rows) = self
            .wire
            .request_counted("trail status")
            .map_err(|e| format!("`trail status`: the held control connection failed: {e}"))?;
        self.note(format!("> trail status\n< {head}\n{}", rows.trim_end()));
        parse_trail(rows.lines().next().unwrap_or(""))
            .map_err(|e| Halt::Broke(format!("trail status: {e}")))
    }

    /// Read `aos status` every [`POLL`] until `done` holds; a one-minute hang detector.
    fn wait_for(&mut self, what: &str, done: impl Fn(&Status) -> bool) -> Result<Status, Halt> {
        let start = Instant::now();
        let mut reads = 0u32;
        loop {
            let (s, reply) = self.status_read()?;
            reads += 1;
            if done(&s) || start.elapsed() >= HANG {
                self.polls += reads;
                self.note(format!(
                    "# {what}: {} after {reads} status reads, {} ms\n< {reply}",
                    if done(&s) { "met" } else { "NOT MET" },
                    start.elapsed().as_millis()
                ));
                if done(&s) {
                    return Ok(s);
                }
                return Err(Halt::Broke(format!(
                    "{what}: not within {} s (a hang detector); last status: {reply}",
                    HANG.as_secs()
                )));
            }
            stop::sleep(POLL)?;
        }
    }

    /// The keys of `word`, unguarded `key <c>` to the front tab [`KEY_GAP`] apart on the
    /// held connection.
    fn type_keys(&mut self, word: &str) -> Result<Instant, Halt> {
        let start = Instant::now();
        let mut last = start;
        for (i, c) in (0u32..).zip(word.chars()) {
            let due = start + KEY_GAP * i;
            let now = Instant::now();
            if due > now {
                stop::sleep(due - now)?;
            }
            last = Instant::now();
            let reply = self
                .wire
                .request_line(&format!("key {c}"))
                .map_err(|e| format!("`key {c}`: the held control connection failed: {e}"))?;
            if !reply.starts_with("OK") {
                return Err(Halt::Broke(format!("key {c} refused: {reply}")));
            }
        }
        self.note(format!(
            "# typed {} keys ({word}) in {} ms",
            word.chars().count(),
            last.duration_since(start).as_millis()
        ));
        Ok(last)
    }

    /// An upper bound of `t`'s offset from the attach epoch, in ns (the epoch is no
    /// earlier than the attach request).
    fn offset_upper(&self, t: Instant) -> i128 {
        i128::try_from(t.saturating_duration_since(self.attach_sent).as_nanos())
            .unwrap_or(i128::MAX)
    }

    /// The drive's instant no earlier than epoch offset `ns`.
    fn instant_after(&self, ns: i128) -> Instant {
        let ns = u64::try_from(ns.max(0)).unwrap_or(u64::MAX);
        self.attach_answered + Duration::from_nanos(ns)
    }

    /// A lower bound of `t`'s offset from the attach epoch, in ns (the epoch is no later than the attach reply).
    fn offset_lower(&self, t: Instant) -> i128 {
        i128::try_from(t.saturating_duration_since(self.attach_answered).as_nanos())
            .unwrap_or(i128::MAX)
    }

    fn keep(&self, suffix: &str, text: &str) {
        let _ = std::fs::write(self.out.join(format!("{}.{suffix}", self.stem)), text);
    }

    fn keep_bytes(&self, suffix: &str, bytes: &[u8]) {
        let _ = std::fs::write(self.out.join(format!("{}.{suffix}", self.stem)), bytes);
    }

    /// One request whose reply ends with a `hex=` run (`aos bundle`, `aos trace hex`), transcribed with the run's length in place of its digits.
    fn ask_hex(&mut self, request: &str) -> Result<String, Halt> {
        let reply = self
            .wire
            .request_line(request)
            .map_err(|e| format!("`{request}`: the held control connection failed: {e}"))?;
        let shown = match reply.split_once(" hex=") {
            Some((head, hex)) => format!("{head} hex=<{} digits>", hex.trim_end().len()),
            None => reply.clone(),
        };
        self.note(format!("> {request}\n< {shown}"));
        Ok(reply)
    }

    /// A figure the run measured: printed and kept as `MEASURED <line>`.
    fn measure(&mut self, line: String) {
        let line = format!("MEASURED {line}");
        self.note(line.clone());
        self.measured.push(line);
    }

    /// `ctl image …` copied to `dest`; its parsed reply.
    fn grab(&mut self, request: &[&str], dest: &Path) -> Result<Shot, Halt> {
        let r = self.inst.ctl(request);
        let line = r.stdout.lines().next().unwrap_or("").to_string();
        self.note(format!("> {}\n< {}", request.join(" "), line));
        let shot = parse_image(&line).map_err(|e| {
            format!(
                "{}: {e} ({})",
                request.join(" "),
                format!("{}{}", r.stdout, r.stderr).trim_end()
            )
        })?;
        std::fs::copy(&shot.path, dest)
            .map_err(|e| format!("cannot copy {} to {}: {e}", shot.path, dest.display()))?;
        Ok(shot)
    }

    /// A kept frame of the cold glass, its clean twin and sidecars, measured by
    /// `trail-contrast`: its `outside_caret_px`.
    fn capture(&mut self, frame: &str) -> Result<Option<i64>, Halt> {
        let png = self.out.join(format!("{}.{frame}.png", self.stem));
        let clean = self.out.join(format!("{}.{frame}.clean.png", self.stem));
        self.grab(&["image", "--meta", &format!("{frame}.png")], &png)?;
        self.grab(
            &["image", "clean", "--meta", &format!("{frame}.clean.png")],
            &clean,
        )?;
        let cursor = self.inst.ctl(&["cursor"]);
        self.keep(
            &format!("{frame}.cursor.txt"),
            &format!("{}{}", cursor.stdout, cursor.stderr),
        );
        let text = self.inst.ctl(&["text"]);
        self.keep(
            &format!("{frame}.text.txt"),
            &format!("{}{}", text.stdout, text.stderr),
        );
        let args = crate::trail_contrast::Args {
            png: png.to_string_lossy().into_owned(),
            newest: 3,
            ..crate::trail_contrast::Args::default()
        };
        let report = crate::trail_contrast::report(&args)
            .map_err(|e| format!("trail-contrast {}: {e}", png.display()))?;
        let rendered = crate::trail_contrast::render(&report, 0);
        self.keep(&format!("{frame}.contrast.json"), &format!("{rendered}\n"));
        let diff = report.get("diff_vs_clean");
        let outside = diff
            .and_then(|d| d.get("outside_caret_px"))
            .and_then(J::as_i64);
        let px = diff.and_then(|d| d.get("px")).and_then(J::as_i64);
        let caret = report
            .get("caret")
            .map_or_else(|| "none".to_string(), |c| c.dump(crate::pyjson::COMPACT));
        self.note(format!(
            "# trail-contrast {}: diff_vs_clean px={} outside_caret_px={} caret={caret}",
            png.display(),
            px_word(px),
            px_word(outside)
        ));
        Ok(outside)
    }
}

/// The repair run's steps 3 to 6 (after the keys).
fn repair_story(d: &mut Drive<'_>, last_key: Instant) -> Result<Vec<Gate>, Halt> {
    let mut gates = Vec::new();
    let settle = d.wait_for("the faulty glow settles", |s| s.settling && !s.active)?;
    let cold_at = settle
        .cold_at
        .ok_or_else(|| format!("a settling glow with no cold_at: {settle:?}"))?;
    let due = d.instant_after(cold_at) + PAST_COLD;
    let wait = due.saturating_duration_since(Instant::now());
    if wait > HANG {
        return Err(Halt::Broke(format!(
            "cold_at={cold_at} lies {} s ahead (a hang detector)",
            wait.as_secs()
        )));
    }
    d.note(format!(
        "# past cold_at={cold_at}: waiting {} ms with no request",
        wait.as_millis()
    ));
    stop::sleep(wait)?;
    let _ = d.status()?;
    let frames_a = d.frames()?;
    let a = d.status()?;
    let trail = d.trail()?;
    let held = Instant::now();
    stop::sleep(HOLD)?;
    let frames_b = d.frames()?;
    let b = d.status()?;
    let hold_ms = u64::try_from(held.elapsed().as_millis()).unwrap_or(u64::MAX);
    gates.extend(stranded_gates(&Stranded {
        a,
        b: b.clone(),
        frames_a,
        frames_b,
        hold_ms,
        trail,
        key_window: d.key_window,
    }));
    d.note(format!(
        "# stranded readings at {} ms and {} ms after the last key",
        held.duration_since(last_key).as_millis(),
        Instant::now().duration_since(last_key).as_millis()
    ));

    let line = d.ask("aos repair code=shipped")?;
    let repair = parse_repair(&line).map_err(|e| format!("aos repair: {e}"))?;
    gates.extend(repair_gates(&repair, &b));

    let cold = d.wait_for("the repaired glow is cold", |s| !s.hot)?;
    let frames_after = d.frames()?;
    let trail = d.trail()?;
    let outside = d.capture("cleared")?;
    gates.extend(cleared_gates(&Cleared {
        frames_before: frames_b,
        frames_after,
        status: cold.clone(),
        repaired_rev: repair.rev,
        trail,
        outside_caret_px: outside,
    }));
    let _ = d.frames()?;
    let committed = read_trace(d)?;
    gates.extend(post_commit_gates(&repair, &committed));

    let last_key = d.type_keys(FRESH_KEYS)?;
    let after_keys = d.offset_upper(last_key);
    let cooled = cool(d, after_keys, "fresh-cleared")?;
    let beside = d.status()?;
    let trace = read_trace(d)?;
    gates.extend(cooled_gates(&cooled));
    gates.extend(continuation_gates(&cold, &cooled.cold, &beside, &trace));
    gates.push(log_kinds_gate(
        "fresh-keys-recorded-once",
        &trace,
        typed(FRESH_KEYS),
    ));
    Ok(gates)
}

/// `aos trace`, read.
fn read_trace(d: &mut Drive<'_>) -> Result<TraceLine, Halt> {
    Ok(parse_trace(&d.ask("aos trace")?).map_err(|e| format!("aos trace: {e}"))?)
}

/// A glow typed hot, left to cool by itself: hot, settling (a query made after the last
/// key, whose offset is at most `after_keys`), cold, then the cold glass photographed.
fn cool(d: &mut Drive<'_>, after_keys: i128, frame: &str) -> Result<Cooled, Halt> {
    let hot = d.wait_for("the keys heat the glow", |s| s.hot)?;
    let settling = d.wait_for("the glow settles, asked after the last key", |s| {
        s.settling && s.query_at.is_some_and(|q| q > after_keys)
    })?;
    let frames_settling = d.frames()?;
    let cold = d.wait_for("the glow cools by itself", |s| !s.hot)?;
    let frames_cold = d.frames()?;
    let trail = d.trail()?;
    let outside_caret_px = d.capture(frame)?;
    Ok(Cooled {
        hot,
        settling,
        frames_settling,
        cold,
        frames_cold,
        trail,
        outside_caret_px,
    })
}

/// The run after the instance is up: the attach and the first 24 keys, then the repair story, or the first burst cooled by itself and the 12-key phase (with the publication between them under `--publish`).
fn story(d: &mut Drive<'_>, mode: Mode) -> Result<Vec<Gate>, Halt> {
    let dims = d.inst.ctl(&["dims"]);
    d.keep("dims.txt", &format!("{}{}", dims.stdout, dims.stderr));
    d.note(format!("> dims\n< {}", dims.first()));
    d.key_window = d.trail()?.focused;
    let _ = d.frames()?;
    let before = d.status()?;
    if before.attached {
        return Err(Halt::Broke(format!(
            "the glow was attached before the drive: {before:?}"
        )));
    }
    let code = mode.attaches();
    d.attach_sent = Instant::now();
    let line = d.ask(&format!("aos attach code={code}"))?;
    d.attach_answered = Instant::now();
    let attached = parse_status(&line).map_err(|e| format!("aos attach: {e}"))?;
    if !attached.attached || attached.code != code {
        return Err(Halt::Broke(format!(
            "aos attach did not attach {code}: {line}"
        )));
    }
    d.build.clone_from(&attached.build);
    d.measure(format!(
        "attach_rtt_ms={} build={}",
        ms(
            i128::try_from(d.attach_answered.duration_since(d.attach_sent).as_nanos())
                .unwrap_or(i128::MAX)
        ),
        d.build.as_deref().unwrap_or("none")
    ));
    let last_key = d.type_keys(STRAND_KEYS)?;
    if mode == Mode::Repair {
        let hot = d.wait_for("the keys heat the glow", |s| s.hot)?;
        let mut gates = vec![gate(
            "keys-heat-the-fault",
            hot.hot && hot.code == "demo-stranded-settle",
            format!("hot={} code={}", hot.hot, hot.code),
        )];
        gates.extend(repair_story(d, last_key)?);
        return Ok(gates);
    }
    let cooled = cool(d, d.offset_upper(last_key), "cleared")?;
    let trace = read_trace(d)?;
    let mut gates = control_gates(&attached, &cooled.cold);
    gates.extend(cooled_gates(&cooled));
    gates.push(log_kinds_gate(
        "keys-recorded-once",
        &trace,
        typed(STRAND_KEYS),
    ));
    let fresh = if let Mode::Publish(plan) = mode {
        let Some(committed) = publish_story(d, &plan, &mut gates)? else {
            return Ok(gates);
        };
        Fresh {
            code: PLAN_NAME,
            committed,
            keys_before: 0,
            plan: SchedulePlan::decode(&plan).ok(),
        }
    } else {
        Fresh {
            code: "shipped",
            committed: d.status()?,
            keys_before: typed(STRAND_KEYS),
            plan: None,
        }
    };
    gates.extend(fresh_phase(d, &fresh)?);
    Ok(gates)
}

/// `--publish`'s publication over the cold glow the first burst left, gated from `aos publish` through the plan live and its bundle verified in the lab to the log it started; returns the status read last, or `None` when the window refused the plan.
fn publish_story(
    d: &mut Drive<'_>,
    plan: &[u8; PLAN_LEN],
    gates: &mut Vec<Gate>,
) -> Result<Option<Status>, Halt> {
    let before = d.status()?;
    let plan_hex = aterm_codec::hex::encode_lower(plan);
    let asked = Instant::now();
    let line = d.ask(&format!("aos publish plan={plan_hex}"))?;
    d.measure(format!(
        "publish_rtt_ms={}",
        ms(i128::try_from(asked.elapsed().as_nanos()).unwrap_or(i128::MAX))
    ));
    let p = match parse_publish(&line) {
        Ok(p) => p,
        Err(why) => {
            gates.push(gate("publish-admitted", false, why));
            return Ok(None);
        }
    };
    gates.push(gate(
        "publish-admitted",
        true,
        format!("rev={} plan={} bundle={}", p.rev, p.plan, p.bundle),
    ));
    gates.extend(publish_gates(&p, &plan_hex, &before));
    let live = d.status()?;
    gates.push(plan_live_gate(&live, &p));
    let reply = d.ask_hex("aos bundle")?;
    let b = parse_bundle_line(&reply).map_err(|e| format!("aos bundle: {e}"))?;
    let verify = match bundle_bytes(&b) {
        Ok(bytes) => {
            d.keep_bytes("bundle", &bytes);
            let started = Instant::now();
            let v = verify_in_lab(&bytes, d.build.as_deref().unwrap_or("none"), LAB_BUILD);
            d.note(format!(
                "# the lab's verification of the bundle: {} in {} ms",
                v.word(),
                started.elapsed().as_millis()
            ));
            v
        }
        Err(why) => Verify::Refused(why),
    };
    gates.extend(bundle_gates(&b, &p, plan, &verify));
    let committed = read_trace(d)?;
    gates.extend(post_commit_gates_at(p.log_epoch, &p.range, &committed));
    Ok(Some(d.status()?))
}

/// What the 12-key phase runs under.
struct Fresh {
    /// The live code's status name (`shipped`, or [`PLAN_NAME`] after a publication).
    code: &'static str,
    /// The status read just before its keys: the continuation's base.
    committed: Status,
    /// The keys the exported log holds before the phase's: none after a commit, the first burst's in the control.
    keys_before: u64,
    /// The plan the run published, so the lab can replay its trace.
    plan: Option<SchedulePlan>,
}

/// The trace file an `aos trace hex` reply carries: its `hex=` decoded, exactly as long as `file_bytes=` says.
///
/// # Errors
/// Not an OK reply, a field missing, not hex, or another length.
pub fn trace_file(line: &str) -> Result<Vec<u8>, String> {
    let f = fields(body(line)?);
    let n: usize = num(&f, "file_bytes")?;
    let bytes = aterm_codec::hex::decode_vec(req(&f, "hex")?.as_bytes())
        .ok_or_else(|| "hex= is not an even run of hex digits".to_string())?;
    if bytes.len() != n {
        return Err(format!("hex= holds {} bytes, file_bytes={n}", bytes.len()));
    }
    Ok(bytes)
}

/// The 12-key phase of the control and of `--publish`: the first burst's witnesses under `fresh-` names, the continuation and the log's keys, the caret moved once in the settling tail, and the phase measured from the exported trace.
fn fresh_phase(d: &mut Drive<'_>, f: &Fresh) -> Result<Vec<Gate>, Halt> {
    let frames_before = d.frames()?;
    let last_key = d.type_keys(FRESH_KEYS)?;
    let after_keys = d.offset_upper(last_key);
    let hot = d.wait_for("the fresh keys heat the glow", |s| s.hot)?;
    let settling = d.wait_for("the glow settles, asked after the last fresh key", |s| {
        s.settling && s.query_at.is_some_and(|q| q > after_keys)
    })?;
    let moved = move_caret(d, last_key, &settling)?;
    // From the move's present on, every present has no key and no move behind it.
    let frames_settling = moved.frames_after;
    let cold = d.wait_for("the glow cools by itself", |s| !s.hot)?;
    let frames_cold = d.frames()?;
    let trail = d.trail()?;
    let outside_caret_px = d.capture("fresh-cleared")?;
    let beside = d.status()?;
    let reply = d.ask_hex("aos trace hex")?;
    let trace = parse_trace(&reply).map_err(|e| format!("aos trace hex: {e}"))?;
    let file = trace_file(&reply).map_err(|e| format!("aos trace hex: {e}"))?;
    d.keep_bytes("fresh.trace", &file);
    let cooled = Cooled {
        hot,
        settling,
        frames_settling,
        cold,
        frames_cold,
        trail,
        outside_caret_px,
    };
    let mut gates = cooled_gates_in(Burst::Fresh, f.code, &cooled);
    gates.extend(continuation_gates_for(
        f.code,
        &f.committed,
        &cooled.cold,
        &beside,
        &trace,
    ));
    gates.push(log_kinds_gate(
        if f.keys_before == 0 {
            "fresh-keys-recorded-once"
        } else {
            "all-keys-recorded-once"
        },
        &trace,
        f.keys_before.saturating_add(typed(FRESH_KEYS)),
    ));
    gates.push(measure_phase(
        d,
        f,
        &file,
        &moved,
        frames_cold.saturating_sub(frames_before),
    ));
    Ok(gates)
}

/// When the drive moves the caret: this long after it sent the phase's last key, the same instant in both modes, so the move does not depend on when the code's settling reading came.
/// It falls after every settling reading measured (at most 2.632 s after the last key) and before the glow's cold instant (about 4.6 s after it).
const MOVE_AFTER_LAST_KEY: Duration = Duration::from_secs(3);

/// How long the drive waits, asked at `now`, before moving the caret: until `last_key` plus [`MOVE_AFTER_LAST_KEY`], or none once that instant has passed.
///
/// # Errors
/// A wait past [`HANG`] (a hang detector), or a move instant no earlier than the glow's cold instant `cold`.
fn move_wait(now: Instant, last_key: Instant, cold: Option<Instant>) -> Result<Duration, String> {
    let at = last_key + MOVE_AFTER_LAST_KEY;
    if cold.is_some_and(|cold| cold <= at) {
        return Err(format!(
            "the move, {} ms after the last key, is not before the glow's cold instant",
            MOVE_AFTER_LAST_KEY.as_millis()
        ));
    }
    let wait = at.saturating_duration_since(now);
    if wait > HANG {
        return Err(format!(
            "the move lies {} s ahead (a hang detector)",
            wait.as_secs()
        ));
    }
    Ok(wait)
}

/// How long the drive watches for the moved caret's present.
const MOVE_WATCH: Duration = Duration::from_secs(2);
/// Between two `metrics` reads of that watch (`metrics` is answered off the main loop).
const MOVE_POLL: Duration = Duration::from_millis(1);

/// What the drive saw of the caret moved with no key while the glow settled (design §10.8's lag case).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Moved {
    /// The status read just before the move, and the first after its present.
    pub before: Status,
    pub after: Status,
    /// `cursor`'s (row, col) before and after.
    pub cursor_before: Option<(u16, u16)>,
    pub cursor_after: Option<(u16, u16)>,
    /// `frames` just before the move, and the first reading past it (the last reading when none was).
    pub frames_before: u64,
    pub frames_after: u64,
    /// The first present after the move as `(lo, hi)` ns after the move's send (later than `lo`, no later than `hi`); `None` when none came within [`MOVE_WATCH`].
    pub present: Option<(u64, u64)>,
    /// The move's offset from the attach epoch: no earlier than `at_lo` (its send) and no later than `at_hi` (its reply).
    pub at_lo: i128,
    pub at_hi: i128,
    /// The move's send after the phase's last key's, on the drive's clock ([`MOVE_AFTER_LAST_KEY`] unless the settling reading came later).
    pub after_last_key: Duration,
    /// The glow's cold instant the settling reading named (ns from the attach epoch).
    pub cold_at: Option<i128>,
    /// The `metrics` reads made watching for the present.
    pub polls: u32,
}

/// A `cursor` reply's `(row, col)`.
fn parse_cursor(line: &str) -> Option<(u16, u16)> {
    let mut w = body(line).ok()?.split_whitespace();
    Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
}

/// ns of a duration, saturating.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The caret moved once with no key in the settling tail, [`MOVE_AFTER_LAST_KEY`] after `last_key`: `feed 0d` echoes a carriage return that moves the caret a line down, and `metrics` `frames` is read every [`MOVE_POLL`] until the next present.
fn move_caret(d: &mut Drive<'_>, last_key: Instant, settling: &Status) -> Result<Moved, Halt> {
    let cold = settling.cold_at.map(|ns| d.instant_after(ns));
    let wait = move_wait(Instant::now(), last_key, cold)
        .map_err(|why| Halt::Broke(format!("{why} (cold_at={})", opt(settling.cold_at))))?;
    d.note(format!(
        "# the settling tail: waiting {} ms, to {} ms after the last key (cold_at={})",
        wait.as_millis(),
        MOVE_AFTER_LAST_KEY.as_millis(),
        opt(settling.cold_at)
    ));
    stop::sleep(wait)?;
    let before = d.status()?;
    let cursor_before = parse_cursor(&d.ask("cursor")?);
    let frames_before = d.frames()?;
    let sent = Instant::now();
    let after_last_key = sent.saturating_duration_since(last_key);
    let reply = d
        .wire
        .request_line("feed 0d")
        .map_err(|e| format!("`feed 0d`: the held control connection failed: {e}"))?;
    let answered = Instant::now();
    d.note(format!("> feed 0d\n< {reply}"));
    if !reply.starts_with("OK") {
        return Err(Halt::Broke(format!("feed 0d refused: {reply}")));
    }
    let (mut polls, mut present) = (0u32, None);
    let mut unchanged_at: Option<Instant> = None;
    let frames_after = loop {
        let asked = Instant::now();
        let line = d
            .wire
            .request_line("metrics")
            .map_err(|e| format!("`metrics`: the held control connection failed: {e}"))?;
        let read = Instant::now();
        polls += 1;
        let frames = count(body(&line)?, "frames").map_err(|e| format!("metrics: {e}"))?;
        if frames > frames_before {
            let lo = unchanged_at.map_or(Duration::ZERO, |t| t.saturating_duration_since(sent));
            present = Some((nanos(lo), nanos(read.duration_since(sent))));
            break frames;
        }
        if read.duration_since(sent) >= MOVE_WATCH {
            break frames;
        }
        unchanged_at = Some(asked);
        stop::sleep(MOVE_POLL)?;
    };
    d.note(format!(
        "# watched the move with {polls} metrics reads: frames {frames_before} -> \
         {frames_after}, present {}",
        present.map_or_else(
            || format!("none within {} ms", MOVE_WATCH.as_millis()),
            |(lo, hi)| format!(
                "{}..{} ms after the move",
                ms(i128::from(lo)),
                ms(i128::from(hi))
            )
        )
    ));
    let after = d.status()?;
    let cursor_after = parse_cursor(&d.ask("cursor")?);
    Ok(Moved {
        before,
        after,
        cursor_before,
        cursor_after,
        frames_before,
        frames_after,
        present,
        at_lo: d.offset_lower(sent),
        at_hi: d.offset_upper(answered),
        after_last_key,
        cold_at: settling.cold_at,
        polls,
    })
}

/// A caret cell as `row,col`, or `none`.
fn cell_word(c: Option<(u16, u16)>) -> String {
    c.map_or_else(|| "none".to_string(), |(r, c)| format!("{r},{c}"))
}

/// The 12-key phase measured from the exported trace `file`, each figure a `MEASURED` line, gated only by `phase-trace-replays`: the lab's replay reached the recorded head and the phase ends on its clearing tick.
fn measure_phase(d: &mut Drive<'_>, f: &Fresh, file: &[u8], m: &Moved, presents: u64) -> Gate {
    let build = d.build.clone().unwrap_or_default();
    let replayed = match aos_phase::replay_trace(file, &build, f.plan) {
        Ok(r) => r,
        Err(why) => return gate("phase-trace-replays", false, why),
    };
    let keys_in_log = aos_phase::keys_in(&replayed.steps);
    let prior = keys_in_log.saturating_sub(aos_phase::phase_keys());
    let Some(p) = aos_phase::phase(&replayed.steps, prior) else {
        return gate(
            "phase-trace-replays",
            false,
            format!("no phase: the log holds {keys_in_log} keys"),
        );
    };
    d.measure(aos_phase::replay_line(&replayed, &p));
    for line in aos_phase::measured_lines(&p, Some(presents)) {
        d.measure(line);
    }
    let ticks = aos_phase::move_ticks(
        &replayed.steps,
        m.at_lo,
        m.cursor_after.unwrap_or((u16::MAX, u16::MAX)),
    );
    let since = |at: i128| format!("{}..{}", ms(at - m.at_hi), ms(at - m.at_lo));
    d.measure(format!(
        "lag settling_read_before_move={} deadline_read_before_move={} cold_at={} \
         move_at_ms={}..{} move_after_last_key_ms={} planned_ms={} move_before_cold_at_ms={} \
         cursor={}->{} present_after_move_ms={} metrics_reads={} tick_before_move={} \
         next_tick_after_move_ms={} next_tick_cell={} carries_new_cell={} \
         next_tick_inside_move_bracket={} first_new_cell_tick_after_move_ms={} guard={}->{}",
        m.before.settling,
        opt(m.before.deadline),
        opt(m.before.cold_at),
        ms(m.at_lo),
        ms(m.at_hi),
        ms(i128::try_from(m.after_last_key.as_nanos()).unwrap_or(i128::MAX)),
        ms(i128::try_from(MOVE_AFTER_LAST_KEY.as_nanos()).unwrap_or(i128::MAX)),
        m.cold_at.map_or_else(
            || "none".to_string(),
            |cold| format!("{}..{}", ms(cold - m.at_hi), ms(cold - m.at_lo))
        ),
        cell_word(m.cursor_before),
        cell_word(m.cursor_after),
        m.present.map_or_else(
            || "none".to_string(),
            |(lo, hi)| format!("{}..{}", ms(i128::from(lo)), ms(i128::from(hi)))
        ),
        m.polls,
        ticks.before.map_or_else(
            || "none".to_string(),
            |(at, cur)| format!("{}ms@{}", ms(at), cell_word(cur))
        ),
        ticks
            .next
            .map_or_else(|| "none".to_string(), |(at, _)| since(at)),
        cell_word(ticks.next.and_then(|(_, cur)| cur)),
        ticks
            .next
            .is_some_and(|(_, cur)| cur.is_some() && cur == m.cursor_after),
        ticks.next.is_some_and(|(at, _)| at <= m.at_hi),
        ticks.at_cell.map_or_else(|| "none".to_string(), since),
        m.before.guard.as_deref().unwrap_or("none"),
        m.after.guard.as_deref().unwrap_or("none"),
    ));
    gate(
        "phase-trace-replays",
        p.cleared,
        format!(
            "code={} T={} P={} cleared={} (the lab's replay reached the recorded head)",
            replayed.code, p.counts.ticks, p.counts.presented, p.cleared
        ),
    )
}

/// The aterm processes of a windowed instance: the keeper's descendants whose command
/// line names the instance's own socket.
fn aterm_pids(inst: &Instance) -> Vec<i32> {
    let mark = format!("--control-sock {}", inst.sock.display());
    crate::procs::descendants(inst.pid)
        .into_iter()
        .filter(|pid| crate::procs::command_line(*pid).contains(&mark))
        .collect()
}

/// What a run does after its attach and its first 24 keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The demonstration fault, stranded, then `aos repair code=shipped` (the default).
    Repair,
    /// `--control`: the shipped code, never replaced, through the same script as [`Mode::Publish`] with no publication.
    Control,
    /// `--publish PLAN_HEX`: the shipped code, then `aos publish plan=<these bytes>`.
    Publish([u8; PLAN_LEN]),
}

impl Mode {
    /// Its name in the result line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Repair => "repair",
            Mode::Control => "control",
            Mode::Publish(_) => "publish",
        }
    }

    /// The stem of its files under `--out`.
    #[must_use]
    pub const fn stem(self) -> &'static str {
        match self {
            Mode::Repair => "aos-repair",
            Mode::Control => "aos-control",
            Mode::Publish(_) => "aos-publish",
        }
    }

    /// The code it attaches.
    #[must_use]
    pub const fn attaches(self) -> &'static str {
        match self {
            Mode::Repair => "demo-stranded-settle",
            Mode::Control | Mode::Publish(_) => "shipped",
        }
    }
}

/// `aos-repair`'s arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub aterm: Option<String>,
    pub out: Option<String>,
    pub mode: Mode,
    pub keep: bool,
}

/// Parse `aos-repair`'s arguments (`--help` aside): `--publish` takes a plan's 12 bytes as 24 hex digits of either case, and `--control` and `--publish` are two modes, not one.
///
/// # Errors
/// Usage (2): a flag without its value, a plan that is not 24 hex digits, both modes, or an unexpected word.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let out = take_value(&mut a, "--out")?;
    let publish = take_value(&mut a, "--publish")?;
    let control = take_flag(&mut a, "--control");
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "aos-repair: unexpected '{extra}' (aos-repair --help)"
        )));
    }
    let mode = match (control, publish) {
        (true, Some(_)) => {
            return Err(Fail::usage(
                "aos-repair: --control and --publish are two modes; give one (aos-repair --help)",
            ));
        }
        (true, None) => Mode::Control,
        (false, None) => Mode::Repair,
        (false, Some(hex)) => {
            let mut plan = [0u8; PLAN_LEN];
            if !aterm_codec::hex::decode_into(hex.as_bytes(), &mut plan) {
                return Err(Fail::usage(format!(
                    "aos-repair: --publish takes a plan's {PLAN_LEN} bytes as {} hex digits, \
                     not '{hex}'",
                    PLAN_LEN * 2
                )));
            }
            Mode::Publish(plan)
        }
    };
    Ok(Args {
        aterm,
        out,
        mode,
        keep,
    })
}

/// `aos-repair`.
///
/// # Errors
/// Usage (2), not run (3), the instance could not start (1), or interrupted.
pub fn cmd_aos_repair(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    stop::install();
    let Args {
        aterm,
        out,
        mode,
        keep,
    } = parse_args(args)?;
    let out = out
        .map(|o| {
            crate::trail_gallery::resolve_out(
                &from_caller(&o),
                &crate::trail_gallery::checkouts(&paths().repo),
            )
            .map_err(|e| Fail::usage(format!("aos-repair: --out {e}")))
        })
        .transpose()?;
    let aterm = instance::resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    let mut world = World::new(&world_config())?;
    if keep {
        world.keep();
    }
    let out = out.unwrap_or_else(|| world.dir.join("frames"));
    std::fs::create_dir_all(&out)
        .map_err(|e| Fail::die(format!("aos-repair: cannot make {}: {e}", out.display())))?;
    let stem = mode.stem();
    let mut args: Vec<String> = GRID.iter().map(|s| (*s).to_string()).collect();
    args.extend(["-e", "/bin/sh", "-c", PROGRAM].map(str::to_string));
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: false,
            args,
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    println!(
        "aos-repair: OPENED A WINDOW: {} (keeper pid {}, sock {}); mode {}; out {}",
        aterm.display(),
        inst.pid,
        inst.sock.display(),
        mode.name(),
        out.display()
    );
    let booted = inst.await_socket(instance::BOOT).and_then(|()| {
        let ready = inst.ctl(&["ready"]);
        if ready.code != 0 {
            return Err(Fail::die(format!("not ready: {}", ready.first())));
        }
        let _ = inst.ctl(&["await", "idle", "500", "timeout=60000"]);
        inst.connect().map_err(Fail::die)
    });
    let pids = aterm_pids(&inst);
    let mut gates = Vec::new();
    let mut extra: Vec<(&str, String)> = Vec::new();
    let mut transcript = Vec::new();
    let mut measured: Vec<String> = Vec::new();
    let mut stopped = None;
    match booted {
        Err(f) => {
            if let Some(sig) = f.stopped {
                stopped = Some(sig);
            }
            gates.push(gate(
                "instance-up",
                false,
                f.msg.unwrap_or_else(|| "interrupted".to_string()),
            ));
        }
        Ok(wire) => {
            let mut d = Drive {
                inst: &inst,
                wire,
                out: out.clone(),
                stem,
                attach_sent: Instant::now(),
                attach_answered: Instant::now(),
                transcript: Vec::new(),
                polls: 0,
                key_window: false,
                build: None,
                measured: Vec::new(),
            };
            match story(&mut d, mode) {
                Ok(g) => gates = g,
                Err(Halt::Stopped(sig)) => stopped = Some(sig),
                Err(Halt::Broke(why)) => {
                    eprintln!("aos-repair: {why}");
                    gates.push(gate("drive", false, why));
                }
            }
            if stopped.is_none() {
                if let Ok(line) = d.ask("metrics percentiles") {
                    let f = fields(&line);
                    for key in [
                        "n_present",
                        "present_p50_ms",
                        "present_p95_ms",
                        "present_p99_ms",
                        "n_input",
                        "input_p50_ms",
                        "input_p95_ms",
                        "input_p99_ms",
                        "n_render",
                        "render_p50_ms",
                        "render_p95_ms",
                        "render_p99_ms",
                    ] {
                        extra.push((key, f.get(key).copied().unwrap_or("?").to_string()));
                    }
                }
                if let Ok(line) = d.ask("metrics") {
                    let f = fields(&line);
                    for key in ["frames", "capture_presents", "present_tainted"] {
                        extra.push((key, f.get(key).copied().unwrap_or("?").to_string()));
                    }
                }
                extra.push(("status_polls", d.polls.to_string()));
                extra.push(("key_window", d.key_window.to_string()));
            }
            transcript = std::mem::take(&mut d.transcript);
            measured = std::mem::take(&mut d.measured);
        }
    }
    inst.stop();
    let _ = std::fs::copy(&inst.log, out.join(format!("{stem}.gui.log")));
    let survivors: Vec<i32> = pids
        .iter()
        .copied()
        .filter(|pid| {
            crate::procs::command_line(*pid)
                .contains(&format!("--control-sock {}", inst.sock.display()))
        })
        .collect();
    for pid in &survivors {
        eprintln!("aos-repair: aterm pid {pid} outlived its stop; killing it by pid");
        let _ = crate::sys::kill(*pid, libc::SIGKILL);
    }
    gates.push(gate(
        "instance-ended",
        !pids.is_empty() && survivors.is_empty(),
        format!("aterm pids {pids:?}, survivors {survivors:?}"),
    ));
    drop(inst);
    if let Some(sig) = stopped {
        println!("aos-repair: interrupted (signal {sig}); the instance is stopped");
        return Err(Fail::from(Stopped(sig)));
    }
    let mut report = String::new();
    for g in &gates {
        let _ = writeln!(
            report,
            "gate {} {} {}",
            g.name,
            if g.pass { "PASS" } else { "FAIL" },
            g.observed
        );
    }
    extra.push(("out", out.display().to_string()));
    let result = result_line(mode.name(), &gates, &extra);
    print!("{report}");
    println!("{result}");
    transcript.push(report);
    transcript.push(result);
    let _ = std::fs::write(
        out.join(format!("{stem}.transcript.txt")),
        transcript.join("\n") + "\n",
    );
    if !measured.is_empty() {
        let _ = std::fs::write(
            out.join(format!("{stem}.measured.txt")),
            measured.join("\n") + "\n",
        );
    }
    if !keep && out.starts_with(&world.dir) {
        println!("aos-repair: the frames go with the world (pass --keep or --out to keep them)");
    }
    drop(world);
    Ok(i32::from(gates.iter().any(|g| !g.pass)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An attached status line as a debug build's `aos status` answers it (taken from a
    /// headless instance with six keys typed under the demonstration fault).
    const HOT: &str = "OK window=0 attached=true code=demo-stranded-settle hot=true active=true \
        settling=false build=0.110.0+geee60c4e22d2 code_digest=f337aa19938f rev=0 branch=0 \
        paused=false queued=0/1024 log=8 log_epoch=1 cursor=8 broken=false \
        checkpoint=31ecd090f577 state=e879c5317564 chain=8e764cc23cbe latched=0.3619 \
        fp=0x600100736d4915/0x60010093940ef3 clock=655412167 cold_at=4130106419 \
        query_at=none deadline=none frame_fill=6df179 frame_hot=true frame_fp=0x600100736d4915 \
        quads=72+0 replays=0 publications=0 rejections=0 delivered=8 refused=0 skipped=0 \
        queued_max=0 reanchors=0 breaks=0 restores=0 clamped=0";

    fn failed(gates: &[Gate]) -> Vec<&'static str> {
        gates.iter().filter(|g| !g.pass).map(|g| g.name).collect()
    }

    fn hot_frame() -> Frame {
        Frame {
            fill: Some("6df179".into()),
            hot: true,
            fp: "0x600100736d4915".into(),
            out: 72,
            under: 0,
        }
    }

    fn inert_frame() -> Frame {
        Frame {
            fill: None,
            hot: false,
            fp: "0x0".into(),
            out: 0,
            under: 0,
        }
    }

    /// A stranded reading: hot and settling past `cold_at`, asked after it, no wake.
    fn stranded_status(query_at: i128) -> Status {
        Status {
            attached: true,
            code: "demo-stranded-settle".into(),
            hot: true,
            settling: true,
            latched: 0.4902,
            fp: "0x1/0x1".into(),
            cold_at: Some(4_000_000_000),
            query_at: Some(query_at),
            deadline: None,
            frame: Some(hot_frame()),
            log: 400,
            ..Status::default()
        }
    }

    fn stranded() -> Stranded {
        Stranded {
            a: stranded_status(4_600_000_000),
            b: stranded_status(5_600_000_000),
            frames_a: 90,
            frames_b: 90,
            hold_ms: 1001,
            trail: TrailRow {
                block_fill: "momentum".into(),
                momentum_glow: 0.0,
                focused: true,
            },
            key_window: true,
        }
    }

    fn cold_status(rev: u64, publications: u64, delivered: u64) -> Status {
        Status {
            attached: true,
            code: "shipped".into(),
            rev,
            frame: Some(inert_frame()),
            fp: "0x0/0x1".into(),
            replays: 2 * publications,
            publications,
            delivered,
            log: 500,
            ..Status::default()
        }
    }

    fn quiet_trail() -> TrailRow {
        TrailRow {
            block_fill: "none".into(),
            momentum_glow: 0.0,
            focused: true,
        }
    }

    #[test]
    fn fields_are_the_key_value_words_and_skip_the_bare_ones() {
        let f = fields("window=1 repaired paused_first=true range=0..9 code=a=b");
        assert_eq!(
            f.into_iter().collect::<Vec<_>>(),
            vec![
                ("code", "a=b"),
                ("paused_first", "true"),
                ("range", "0..9"),
                ("window", "1")
            ]
        );
    }

    #[test]
    fn an_unattached_status_line_is_its_three_predicates() {
        let s = parse_status(
            "OK window=0 attached=false code=shipped hot=false active=false settling=false\n",
        );
        assert_eq!(
            s,
            Ok(Status {
                code: "shipped".into(),
                ..Status::default()
            })
        );
    }

    #[test]
    fn an_attached_status_line_parses_every_field_the_drive_reads() {
        assert_eq!(
            parse_status(HOT),
            Ok(Status {
                window: 0,
                attached: true,
                code: "demo-stranded-settle".into(),
                build: Some("0.110.0+geee60c4e22d2".into()),
                guard: None,
                hot: true,
                active: true,
                settling: false,
                rev: 0,
                paused: false,
                log: 8,
                broken: false,
                latched: 0.3619,
                fp: "0x600100736d4915/0x60010093940ef3".into(),
                clock: Some(655_412_167),
                cold_at: Some(4_130_106_419),
                query_at: None,
                deadline: None,
                frame: Some(hot_frame()),
                replays: 0,
                publications: 0,
                rejections: 0,
                delivered: 8,
                refused: 0,
                reanchors: 0,
                breaks: 0,
                restores: 0,
                clamped: 0,
            })
        );
    }

    #[test]
    fn a_status_line_before_any_tick_has_no_frame_and_signed_offsets_parse() {
        let line = HOT
            .replace(
                " frame_fill=6df179 frame_hot=true frame_fp=0x600100736d4915 quads=72+0",
                "",
            )
            .replace("clock=655412167", "clock=-5");
        let s = parse_status(&line).expect("parses");
        assert_eq!(s.frame, None);
        assert_eq!(s.clock, Some(-5));
    }

    #[test]
    fn a_refusal_or_a_missing_field_is_an_error_naming_it() {
        assert_eq!(
            parse_status("ERR aos needs a debug build"),
            Err("not an OK reply: ERR aos needs a debug build".to_string())
        );
        assert_eq!(
            parse_status(&HOT.replace(" refused=0", "")),
            Err("no refused=".to_string())
        );
        assert_eq!(
            parse_status(&HOT.replace("hot=true active", "hot=yes active")),
            Err("hot=yes is not a boolean".to_string())
        );
        assert_eq!(
            parse_status(&HOT.replace("quads=72+0", "quads=72")),
            Err("quads= is not <out>+<under>".to_string())
        );
    }

    #[test]
    fn a_repair_line_parses_and_a_status_line_is_not_one() {
        let line = "OK window=0 repaired paused_first=true rev=1 code=momentum-glow.shipped \
            live=momentum-glow.demo-stranded-settle live_verdict=fail:HotHasWake@803 \
            candidate_verdict=pass same_code_reproduced=true grade=tested trace=0123456789ab \
            ancestor=31ecd090f577 range=0..812 log_epoch=1 delivered=0";
        assert_eq!(
            parse_repair(line),
            Ok(Repair {
                window: 0,
                paused_first: true,
                rev: 1,
                code: "momentum-glow.shipped".into(),
                live: "momentum-glow.demo-stranded-settle".into(),
                live_verdict: "fail:HotHasWake@803".into(),
                candidate_verdict: "pass".into(),
                same_code_reproduced: true,
                grade: "tested".into(),
                trace: "0123456789ab".into(),
                ancestor: "31ecd090f577".into(),
                range: "0..812".into(),
                log_epoch: 1,
                delivered: 0,
            })
        );
        assert!(
            parse_repair(HOT)
                .expect_err("a status line")
                .starts_with("not a repair reply: ")
        );
    }

    /// An `aos trace` line of the agreed shape: the summary, then the seal and the
    /// per-kind counts at its end.
    const TRACE: &str = "OK window=0 log=8 log_epoch=1 first=0 end=8 \
        trace=0356c26b403b57eb92e88d0323cc7c2d3e3de743dcb3e6a7f203383fd6a79c30 \
        checkpoint=31ecd090f577e87f65549fe2b74ac40c921a79cc8a5795079008b9194ddea798 \
        identity=10464a926549 broken=false bytes=536+959 sealed=true keys=6 resets=0 ticks=1 \
        queries=1";

    #[test]
    fn a_trace_line_parses_with_its_seal_and_per_kind_counts() {
        assert_eq!(
            parse_trace(TRACE),
            Ok(TraceLine {
                log: 8,
                log_epoch: 1,
                first: 0,
                end: 8,
                trace: "0356c26b403b57eb92e88d0323cc7c2d3e3de743dcb3e6a7f203383fd6a79c30".into(),
                checkpoint: "31ecd090f577e87f65549fe2b74ac40c921a79cc8a5795079008b9194ddea798"
                    .into(),
                broken: false,
                bytes: "536+959".into(),
                sealed: true,
                keys: 6,
                resets: 0,
                ticks: 1,
                queries: 1,
            })
        );
    }

    /// The trace line the host printed before the seal and the counts existed is not a
    /// trace the drive can judge: each missing word is named.
    #[test]
    fn a_trace_line_without_its_seal_or_a_count_is_an_error_naming_it() {
        let unsealed = TRACE.replace(" sealed=true keys=6 resets=0 ticks=1 queries=1", "");
        assert_eq!(parse_trace(&unsealed), Err("no sealed=".to_string()));
        assert_eq!(
            parse_trace(&TRACE.replace(" resets=0", "")),
            Err("no resets=".to_string())
        );
        assert_eq!(
            parse_trace(&TRACE.replace("sealed=true", "sealed=1")),
            Err("sealed=1 is not a boolean".to_string())
        );
        assert_eq!(
            parse_trace(&TRACE.replace("keys=6", "keys=six")),
            Err("keys=six is not a number".to_string())
        );
    }

    #[test]
    fn a_sealed_log_of_the_typed_keys_and_no_reset_passes_and_each_miscount_fails() {
        let t = parse_trace(TRACE).expect("parses");
        let pass = log_kinds_gate("k", &t, 6);
        assert!(pass.pass, "{}", pass.observed);
        assert_eq!(
            pass.observed,
            "sealed=true keys=6 typed=6 resets=0 ticks=1 queries=1 log=8"
        );

        let fails = |t: &TraceLine, typed: u64| !log_kinds_gate("k", t, typed).pass;
        assert!(fails(&t, 5));
        assert!(fails(&t, 7));
        let reset = TraceLine {
            resets: 1,
            queries: 0,
            ..t.clone()
        };
        assert!(fails(&reset, 6));
        let unsealed = TraceLine {
            sealed: false,
            ..t.clone()
        };
        assert!(fails(&unsealed, 6));
        let uncounted = TraceLine {
            log: 9,
            end: 9,
            ..t.clone()
        };
        assert!(fails(&uncounted, 6));
        let huge = TraceLine {
            ticks: u64::MAX,
            ..t
        };
        assert!(fails(&huge, 6));
    }

    #[test]
    fn the_post_commit_trace_is_the_new_log_with_no_key_or_reset() {
        let r = Repair {
            range: "0..169".into(),
            log_epoch: 1,
            ..repaired()
        };
        let t = TraceLine {
            log: 12,
            log_epoch: 2,
            first: 169,
            end: 181,
            trace: "ab".repeat(32),
            sealed: true,
            ticks: 9,
            queries: 3,
            ..TraceLine::default()
        };
        assert_eq!(failed(&post_commit_gates(&r, &t)), Vec::<&str>::new());

        let old = TraceLine {
            log_epoch: 1,
            first: 0,
            keys: 24,
            log: 36,
            ..t.clone()
        };
        assert_eq!(
            failed(&post_commit_gates(&r, &old)),
            vec!["post-commit-log", "post-commit-no-key-or-reset"]
        );
        let elsewhere = TraceLine {
            first: 170,
            ..t.clone()
        };
        assert_eq!(
            failed(&post_commit_gates(&r, &elsewhere)),
            vec!["post-commit-log"]
        );
        let reset = TraceLine {
            resets: 1,
            queries: 2,
            ..t.clone()
        };
        assert_eq!(
            failed(&post_commit_gates(&r, &reset)),
            vec!["post-commit-no-key-or-reset"]
        );
        let key = TraceLine {
            keys: 1,
            queries: 2,
            ..t
        };
        assert_eq!(
            failed(&post_commit_gates(&r, &key)),
            vec!["post-commit-no-key-or-reset"]
        );
    }

    #[test]
    fn a_trail_status_row_gives_the_fill_owner_the_integrator_and_focus() {
        let row = "trail style=\"off\" resolved=off config_enabled=false effective=false \
            focused=true motion=full momentum=0.00 momentum_display=0.00 momentum_glow=0.37 \
            block_fill=momentum block_fill_rgb=6df179 block_fill_base=ffffff";
        assert_eq!(
            parse_trail(row),
            Ok(TrailRow {
                block_fill: "momentum".into(),
                momentum_glow: 0.37,
                focused: true,
            })
        );
        assert_eq!(
            parse_trail("OK 1"),
            Err("not a trail status row: OK 1".to_string())
        );
    }

    #[test]
    fn an_image_reply_gives_its_path_last_with_or_without_meta() {
        assert_eq!(
            parse_image("OK 794 462 /private/tmp/at.x/run/images/a.png\n"),
            Ok(Shot {
                width: 794,
                height: 462,
                path: "/private/tmp/at.x/run/images/a.png".into(),
                phase: None,
            })
        );
        assert_eq!(
            parse_image(
                "OK 794 462 image-meta image-meta-version=3 frame-kind=terminal \
                 frame-phase=destination window=0 path=\"/private/tmp/at.x/run/images/b.png\""
            ),
            Ok(Shot {
                width: 794,
                height: 462,
                path: "/private/tmp/at.x/run/images/b.png".into(),
                phase: Some("destination".into()),
            })
        );
        assert_eq!(
            parse_image("OK 794 462"),
            Err("not an image reply: 794 462".to_string())
        );
        assert!(parse_image("ERR image: no window to capture").is_err());
    }

    #[test]
    fn metrics_frames_is_read_by_name() {
        let line = "backend=gpu rows=14 cols=72 frames=12 n_present=3";
        assert_eq!(count(line, "frames"), Ok(12));
        assert_eq!(
            count(line, "capture_presents"),
            Err("no capture_presents=".into())
        );
    }

    #[test]
    fn a_stranded_reading_passes_and_each_symptom_fails_its_own_gate() {
        let e = stranded();
        assert_eq!(failed(&stranded_gates(&e)), Vec::<&str>::new());

        let mut presented = e.clone();
        presented.frames_b = 91;
        assert_eq!(
            failed(&stranded_gates(&presented)),
            vec!["no-present-while-stranded"]
        );

        let mut short = e.clone();
        short.hold_ms = 999;
        assert_eq!(
            failed(&stranded_gates(&short)),
            vec!["no-present-while-stranded"]
        );

        let mut woke = e.clone();
        woke.b.deadline = Some(5_610_000_000);
        assert_eq!(failed(&stranded_gates(&woke)), vec!["no-wake-named"]);

        let mut early = e.clone();
        early.a.query_at = Some(3_900_000_000);
        assert_eq!(
            failed(&stranded_gates(&early)),
            vec!["stranded-hot-past-cold"]
        );

        let mut cooled = e.clone();
        cooled.b.hot = false;
        cooled.b.settling = false;
        assert_eq!(
            failed(&stranded_gates(&cooled)),
            vec!["stranded-hot-past-cold"]
        );

        let mut ticked = e.clone();
        ticked.b.latched = 0.4863;
        assert_eq!(
            failed(&stranded_gates(&ticked)),
            vec!["last-tick-unchanged"]
        );

        let mut untinted = e.clone();
        untinted.trail.block_fill = "none".into();
        assert_eq!(
            failed(&stranded_gates(&untinted)),
            vec!["halo-on-last-frame"]
        );

        let mut outline = untinted.clone();
        outline.key_window = false;
        assert_eq!(failed(&stranded_gates(&outline)), Vec::<&str>::new());
        let mut claimed = e.clone();
        claimed.key_window = false;
        assert_eq!(
            failed(&stranded_gates(&claimed)),
            vec!["halo-on-last-frame"]
        );
        let mut dark = outline;
        dark.b.frame = Some(inert_frame());
        dark.a.frame = Some(inert_frame());
        assert!(failed(&stranded_gates(&dark)).contains(&"halo-on-last-frame"));

        let mut warm = e.clone();
        warm.trail.momentum_glow = 0.05;
        assert_eq!(failed(&stranded_gates(&warm)), vec!["integrator-cold"]);

        let mut shipped = e;
        shipped.b.code = "shipped".into();
        assert_eq!(failed(&stranded_gates(&shipped)), vec!["fault-attached"]);
    }

    fn repaired() -> Repair {
        Repair {
            paused_first: true,
            rev: 1,
            code: "momentum-glow.shipped".into(),
            live: "momentum-glow.demo-stranded-settle".into(),
            live_verdict: "fail:HotHasWake@803".into(),
            candidate_verdict: "pass".into(),
            same_code_reproduced: true,
            grade: "tested".into(),
            ..Repair::default()
        }
    }

    #[test]
    fn a_repair_witness_passes_and_each_wrong_word_fails_its_own_gate() {
        let before = stranded_status(5_600_000_000);
        assert_eq!(
            failed(&repair_gates(&repaired(), &before)),
            Vec::<&str>::new()
        );

        let mut passed_live = repaired();
        passed_live.live_verdict = "pass".into();
        assert_eq!(
            failed(&repair_gates(&passed_live, &before)),
            vec!["live-fails-hot-has-wake"]
        );

        let mut other_law = repaired();
        other_law.live_verdict = "fail:ColdTickClears@12".into();
        assert_eq!(
            failed(&repair_gates(&other_law, &before)),
            vec!["live-fails-hot-has-wake"]
        );

        let mut same_rev = repaired();
        same_rev.rev = 0;
        assert_eq!(
            failed(&repair_gates(&same_rev, &before)),
            vec!["new-revision"]
        );

        let mut queued = repaired();
        queued.delivered = 2;
        assert_eq!(
            failed(&repair_gates(&queued, &before)),
            vec!["nothing-queued"]
        );
    }

    #[test]
    fn a_cleared_reading_passes_and_a_missing_present_or_halo_fails() {
        let e = Cleared {
            frames_before: 90,
            frames_after: 92,
            status: cold_status(1, 1, 400),
            repaired_rev: 1,
            trail: quiet_trail(),
            outside_caret_px: Some(0),
        };
        assert_eq!(failed(&cleared_gates(&e)), Vec::<&str>::new());

        let mut unpresented = e.clone();
        unpresented.frames_after = 90;
        assert_eq!(
            failed(&cleared_gates(&unpresented)),
            vec!["clearing-frame-presented"]
        );

        let mut halo = e.clone();
        halo.outside_caret_px = Some(311);
        assert_eq!(failed(&cleared_gates(&halo)), vec!["clearing-pixels"]);

        let mut unmeasured = e.clone();
        unmeasured.outside_caret_px = None;
        assert_eq!(failed(&cleared_gates(&unmeasured)), vec!["clearing-pixels"]);

        let mut lit = e.clone();
        lit.status.frame = Some(hot_frame());
        assert_eq!(failed(&cleared_gates(&lit)), vec!["clearing-tick-inert"]);

        let mut old_code = e;
        old_code.status.code = "demo-stranded-settle".into();
        assert_eq!(failed(&cleared_gates(&old_code)), vec!["cold-after-repair"]);
    }

    fn cooled() -> Cooled {
        let mut hot = cold_status(1, 1, 450);
        hot.hot = true;
        hot.active = true;
        let mut settling = cold_status(1, 1, 460);
        settling.hot = true;
        settling.settling = true;
        settling.query_at = Some(9_000_000_000);
        settling.deadline = Some(9_009_000_000);
        Cooled {
            hot,
            settling,
            frames_settling: 120,
            cold: cold_status(1, 1, 700),
            frames_cold: 240,
            trail: quiet_trail(),
            outside_caret_px: Some(0),
        }
    }

    #[test]
    fn a_self_cooling_glow_passes_and_a_silent_settle_fails() {
        let e = cooled();
        assert_eq!(failed(&cooled_gates(&e)), Vec::<&str>::new());

        let mut no_wake = e.clone();
        no_wake.settling.deadline = None;
        assert_eq!(failed(&cooled_gates(&no_wake)), vec!["settling-names-wake"]);

        let mut parked = e.clone();
        parked.frames_cold = parked.frames_settling;
        assert_eq!(failed(&cooled_gates(&parked)), vec!["cleared-by-itself"]);

        let mut refused = e;
        refused.cold.refused = 1;
        assert_eq!(failed(&cooled_gates(&refused)), vec!["admitted"]);
    }

    #[test]
    fn the_continuation_holds_the_published_pair_and_the_trace_to_the_status() {
        let committed = cold_status(1, 1, 400);
        let after = cold_status(1, 1, 700);
        let trace = TraceLine {
            log: 500,
            first: 0,
            end: 500,
            trace: "ab".repeat(32),
            ..TraceLine::default()
        };
        assert_eq!(
            failed(&continuation_gates(&committed, &after, &after, &trace)),
            Vec::<&str>::new()
        );

        let mut republished = after.clone();
        republished.publications = 2;
        assert_eq!(
            failed(&continuation_gates(
                &committed,
                &republished,
                &after,
                &trace
            )),
            vec!["no-further-replay-or-publication"]
        );

        let mut reanchored = after.clone();
        reanchored.reanchors = 1;
        assert_eq!(
            failed(&continuation_gates(&committed, &reanchored, &after, &trace)),
            vec!["no-further-replay-or-publication"]
        );

        let mut torn = trace.clone();
        torn.broken = true;
        torn.trace = "none".into();
        assert_eq!(
            failed(&continuation_gates(&committed, &after, &after, &torn)),
            vec!["trace-agrees"]
        );

        let mut longer = trace;
        longer.log = 501;
        longer.end = 501;
        assert_eq!(
            failed(&continuation_gates(&committed, &after, &after, &longer)),
            vec!["trace-agrees"]
        );

        let undelivered = cold_status(1, 1, 400);
        assert_eq!(
            failed(&continuation_gates(
                &committed,
                &undelivered,
                &after,
                &TraceLine {
                    log: 500,
                    end: 500,
                    trace: "ab".into(),
                    ..TraceLine::default()
                }
            )),
            vec!["fresh-keys-delivered"]
        );
    }

    #[test]
    fn the_control_needs_the_shipped_code_never_replaced() {
        let attached = Status {
            attached: true,
            code: "shipped".into(),
            ..Status::default()
        };
        let cold = cold_status(0, 0, 700);
        assert_eq!(failed(&control_gates(&attached, &cold)), Vec::<&str>::new());
        let fault = Status {
            code: "demo-stranded-settle".into(),
            ..attached
        };
        assert_eq!(
            failed(&control_gates(&fault, &cold_status(1, 1, 700))),
            vec!["shipped-attached", "no-publication"]
        );
    }

    #[test]
    fn the_result_line_names_the_failed_gates_and_quotes_the_extras() {
        let gates = [
            gate("a", true, String::new()),
            gate("b", false, String::new()),
            gate("c", false, String::new()),
        ];
        assert_eq!(
            result_line(
                "repair",
                &gates,
                &[
                    ("n_present", "41".to_string()),
                    ("out", "/tmp/x y".to_string())
                ]
            ),
            "aos-repair result=fail mode=repair gates=1/3 failed=b,c n_present=41 out=/tmp/x_y"
        );
        assert_eq!(
            result_line("control", &gates[..1], &[]),
            "aos-repair result=pass mode=control gates=1/1 failed=none"
        );
        assert_eq!(
            result_line("control", &[], &[]),
            "aos-repair result=fail mode=control gates=0/0 failed=none"
        );
    }

    #[test]
    fn the_instance_config_lights_the_glow_alone_and_holds_the_deck_off() {
        let doc: aterm_toml::Value = aterm_toml::from_str(&world_config()).expect("valid TOML");
        assert_eq!(doc["cursor_momentum_glow"].as_bool(), Some(true));
        assert_eq!(doc["cursor_trail_style"].as_str(), Some("off"));
        assert_eq!(doc["cursor_glow_sdr_boost"].as_float(), Some(0.0));
        assert_eq!(doc["sys"]["stats_bars"].as_str(), Some("off"));
        assert_eq!(doc["harness"]["enabled"].as_bool(), Some(false));
        assert_eq!(doc["update"]["enabled"].as_bool(), Some(false));
    }

    #[test]
    fn the_typed_words_are_the_in_process_tests_counts_of_printable_keys() {
        assert_eq!(typed(STRAND_KEYS), 24);
        assert_eq!(typed(FRESH_KEYS), 12);
        assert!(
            STRAND_KEYS
                .chars()
                .chain(FRESH_KEYS.chars())
                .all(|c| c.is_ascii_lowercase())
        );
    }

    /// The grid winner's 12 bytes, as `--publish` and `publish plan=` spell them.
    const WINNER_HEX: &str = "010100000000000000000000";

    fn args(words: &[&str]) -> Result<Args, Fail> {
        parse_args(&words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>())
    }

    fn usage(words: &[&str]) -> String {
        let f = args(words).expect_err("a usage error");
        assert_eq!(f.code, 2, "{words:?}: usage");
        f.msg.unwrap_or_default()
    }

    /// The arguments name one mode (no flag the repair, `--control`, or `--publish` with a plan's 24 hex digits of either case), and every malformed form is a usage error.
    #[test]
    fn the_arguments_name_one_mode_and_publish_takes_a_plans_24_hex_digits() {
        let plain = args(&[]).expect("no flag");
        assert_eq!(
            plain,
            Args {
                aterm: None,
                out: None,
                mode: Mode::Repair,
                keep: false
            }
        );
        assert_eq!(args(&["--control"]).expect("control").mode, Mode::Control);
        let winner = [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            args(&["--publish", WINNER_HEX]).expect("publish").mode,
            Mode::Publish(winner)
        );
        assert_eq!(
            args(&["--publish", "0101000000000000000000AB"])
                .expect("upper case")
                .mode,
            Mode::Publish([1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xab])
        );
        assert_eq!(
            args(&[
                "--keep",
                "--publish",
                WINNER_HEX,
                "--aterm",
                "/a/aterm",
                "--out",
                "/o"
            ])
            .expect("every flag"),
            Args {
                aterm: Some("/a/aterm".into()),
                out: Some("/o".into()),
                mode: Mode::Publish(winner),
                keep: true
            }
        );
        assert_eq!(usage(&["--publish"]), "--publish needs a value");
        for bad in [
            "0101",
            "0101000000000000000000000",
            "01010000000000000000000000",
            "zz0100000000000000000000",
            "",
        ] {
            assert_eq!(
                usage(&["--publish", bad]),
                format!(
                    "aos-repair: --publish takes a plan's 12 bytes as 24 hex digits, not '{bad}'"
                )
            );
        }
        assert!(
            usage(&["--control", "--publish", WINNER_HEX]).contains("two modes; give one"),
            "both modes"
        );
        assert!(usage(&["--bogus"]).starts_with("aos-repair: unexpected '--bogus'"));
        for (mode, name, stem, code) in [
            (Mode::Repair, "repair", "aos-repair", "demo-stranded-settle"),
            (Mode::Control, "control", "aos-control", "shipped"),
            (Mode::Publish(winner), "publish", "aos-publish", "shipped"),
        ] {
            assert_eq!(
                (mode.name(), mode.stem(), mode.attaches()),
                (name, stem, code)
            );
        }
    }

    /// A window's publish reply, in the verb's `publish_line` shape.
    fn publish_reply() -> String {
        format!(
            "OK window=0 published paused_first=true rev=1 plan={WINNER_HEX} \
             plan_digest=4e3c0f2a9b1d useful=true gain_ppm=234255 s0_gain_ppm=118000 \
             code=momentum-glow.schedule-plan live=momentum-glow.shipped live_verdict=pass \
             candidate_verdict=pass trace=9f00aa11bb22 ancestor=00ff00ff00ff range=0..1742 \
             log_epoch=1 grade=tested bundle=c0ffee0123ab delivered=0"
        )
    }

    /// A publish reply parses with its witness half, and a refusal, a repair reply and a reply without the witness are errors naming why.
    #[test]
    fn a_publish_reply_parses_with_its_witness_and_a_refusal_is_an_error() {
        let p = parse_publish(&publish_reply()).expect("a publish reply");
        assert_eq!(
            p,
            PublishLine {
                window: 0,
                paused_first: true,
                rev: 1,
                plan: WINNER_HEX.into(),
                plan_digest: "4e3c0f2a9b1d".into(),
                useful: true,
                gain_ppm: 234_255,
                s0_gain_ppm: 118_000,
                code: PLAN_CODE.into(),
                live: "momentum-glow.shipped".into(),
                live_verdict: "pass".into(),
                candidate_verdict: "pass".into(),
                trace: "9f00aa11bb22".into(),
                ancestor: "00ff00ff00ff".into(),
                range: "0..1742".into(),
                log_epoch: 1,
                grade: "tested".into(),
                bundle: "c0ffee0123ab".into(),
                delivered: 0,
            }
        );
        assert!(
            parse_publish("ERR aos: the plan is not useful over the shipped code")
                .expect_err("a refusal")
                .starts_with("not an OK reply: ERR aos:")
        );
        assert!(
            parse_publish("OK window=0 repaired paused_first=true rev=1")
                .expect_err("a repair reply")
                .starts_with("not a publish reply")
        );
        let unwitnessed = publish_reply().replace(" code=momentum-glow.schedule-plan", "");
        assert_eq!(parse_publish(&unwitnessed), Err("no code=".to_string()));
        let signed = publish_reply().replace("gain_ppm=234255", "gain_ppm=-5");
        assert_eq!(parse_publish(&signed).map(|p| p.gain_ppm), Ok(-5));
    }

    /// One word of a publish reply made wrong.
    type Spoil = fn(&mut PublishLine);

    /// A publication over the shipped code passes every gate, each wrong word fails its own, and the plan named in upper case is the same plan.
    #[test]
    fn a_publication_passes_its_gates_and_each_wrong_word_fails_its_own() {
        let p = parse_publish(&publish_reply()).expect("a publish reply");
        let before = cold_status(0, 0, 700);
        assert_eq!(
            failed(&publish_gates(&p, WINNER_HEX, &before)),
            Vec::<&str>::new()
        );
        assert_eq!(
            failed(&publish_gates(
                &p,
                &WINNER_HEX.to_ascii_uppercase(),
                &before
            )),
            Vec::<&str>::new()
        );
        let cases: [(&str, Spoil); 9] = [
            ("publish-paused-first", |p| p.paused_first = false),
            ("publish-paused-first", |p| p.delivered = 1),
            ("publish-plan", |p| {
                p.plan = "010100000000000000000001".into()
            }),
            ("publish-useful", |p| p.useful = false),
            ("publish-gain", |p| p.gain_ppm = 0),
            ("publish-new-revision", |p| p.rev = 0),
            ("plan-over-shipped", |p| {
                p.code = "momentum-glow.shipped".into()
            }),
            ("publish-witness", |p| {
                p.candidate_verdict = "fail:HotHasWake@3".into()
            }),
            ("publish-bundle-digest", |p| {
                p.bundle = "C0FFEE0123AB".into()
            }),
        ];
        for (name, wrong) in cases {
            let mut q = p.clone();
            wrong(&mut q);
            assert_eq!(failed(&publish_gates(&q, WINNER_HEX, &before)), vec![name]);
        }
        let mut graded = p.clone();
        graded.grade = "replayed".into();
        assert_eq!(
            failed(&publish_gates(&graded, WINNER_HEX, &before)),
            vec!["publish-witness"]
        );

        let live = Status {
            code: PLAN_NAME.into(),
            guard: Some("moved:false,firings:0".into()),
            ..cold_status(1, 1, 700)
        };
        assert!(plan_live_gate(&live, &p).pass);
        assert!(
            plan_live_gate(&live, &p)
                .observed
                .ends_with(" guard=moved:false,firings:0")
        );
        for wrong in [
            Status {
                code: "shipped".into(),
                ..live.clone()
            },
            Status {
                rev: 2,
                ..live.clone()
            },
            Status {
                paused: true,
                ..live.clone()
            },
            Status {
                publications: 2,
                ..live.clone()
            },
        ] {
            assert!(!plan_live_gate(&wrong, &p).pass, "{wrong:?}");
        }
    }

    /// A bundle reply parses; its bytes are its hex decoded, exactly as long as it says.
    #[test]
    fn a_bundle_reply_parses_and_its_bytes_are_as_long_as_it_says() {
        let digest = "ab".repeat(32);
        let line = format!("OK window=0 bundle={digest} rev=1 bytes=3 hex=00ff7a");
        let b = parse_bundle_line(&line).expect("a bundle reply");
        assert_eq!(
            b,
            BundleLine {
                window: 0,
                bundle: digest,
                rev: 1,
                bytes: 3,
                hex: "00ff7a".into()
            }
        );
        assert_eq!(bundle_bytes(&b), Ok(vec![0x00, 0xff, 0x7a]));
        let long = BundleLine {
            bytes: 4,
            ..b.clone()
        };
        assert_eq!(
            bundle_bytes(&long),
            Err("hex= holds 3 bytes, bytes=4".to_string())
        );
        let odd = BundleLine {
            hex: "00f".into(),
            ..b
        };
        assert!(bundle_bytes(&odd).is_err());
        assert!(parse_bundle_line("ERR aos: the live code was published by no plan").is_err());
        assert_eq!(
            parse_bundle_line("OK window=0 bundle=ab rev=1 bytes=0"),
            Err("no hex=".to_string())
        );
    }

    /// A trace hex reply carries the trace file: its hex decoded, as long as it says.
    #[test]
    fn a_trace_hex_reply_carries_the_trace_file_it_counts() {
        let line = "OK window=0 log=3 log_epoch=2 first=10 end=13 trace=ab checkpoint=cd \
                    identity=ef broken=false bytes=1+2 sealed=true keys=0 resets=0 ticks=2 \
                    queries=1 file_bytes=3 hex=414f53";
        assert_eq!(trace_file(line), Ok(b"AOS".to_vec()));
        assert_eq!(
            parse_trace(line).map(|t| (t.log, t.ticks, t.queries)),
            Ok((3, 2, 1)),
            "the summary half is an `aos trace` line"
        );
        assert_eq!(
            trace_file(&line.replace("file_bytes=3", "file_bytes=4")),
            Err("hex= holds 3 bytes, file_bytes=4".to_string())
        );
        assert_eq!(
            trace_file(&line.replace(" hex=414f53", "")),
            Err("no hex=".to_string())
        );
        assert_eq!(parse_cursor("OK 3 17 1 steady_block\n"), Some((3, 17)));
        assert_eq!(parse_cursor("ERR no tab"), None);
        assert_eq!(cell_word(Some((3, 17))), "3,17");
        assert_eq!(cell_word(None), "none");
    }

    /// The 12 fresh keys after a publication are judged by the first burst's gates under their own `fresh-` names and the plan's status name, and the continuation holds the plan's revision.
    #[test]
    fn the_fresh_burst_is_judged_by_the_first_bursts_gates_under_its_own_names() {
        let mut e = cooled();
        e.hot.code = PLAN_NAME.into();
        let gates = cooled_gates_in(Burst::Fresh, PLAN_NAME, &e);
        assert_eq!(failed(&gates), Vec::<&str>::new());
        assert_eq!(
            gates.iter().map(|g| g.name).collect::<Vec<_>>(),
            [
                "fresh-keys-heat",
                "fresh-settling-names-wake",
                "fresh-cleared-by-itself",
                "fresh-last-tick-inert",
                "fresh-cooled-pixels",
                "fresh-admitted"
            ]
        );
        assert_eq!(
            failed(&cooled_gates_in(Burst::Fresh, "shipped", &e)),
            vec!["fresh-keys-heat"]
        );
        assert_eq!(
            cooled_gates(&cooled()),
            cooled_gates_in(Burst::First, "shipped", &cooled())
        );
        let committed = Status {
            code: PLAN_NAME.into(),
            ..cold_status(1, 1, 400)
        };
        let after = Status {
            code: PLAN_NAME.into(),
            ..cold_status(1, 1, 700)
        };
        let trace = TraceLine {
            log: 500,
            end: 500,
            trace: "ab".repeat(32),
            ..TraceLine::default()
        };
        assert_eq!(
            failed(&continuation_gates_for(
                PLAN_NAME, &committed, &after, &after, &trace
            )),
            Vec::<&str>::new()
        );
        assert_eq!(
            failed(&continuation_gates(&committed, &after, &after, &trace)),
            vec!["same-published-pair"],
            "the repair's continuation is the shipped code's"
        );
        let r = Repair {
            range: "0..169".into(),
            log_epoch: 1,
            ..repaired()
        };
        let t = TraceLine {
            log: 12,
            log_epoch: 2,
            first: 169,
            end: 181,
            sealed: true,
            ticks: 9,
            queries: 3,
            ..TraceLine::default()
        };
        assert_eq!(
            post_commit_gates(&r, &t),
            post_commit_gates_at(1, "0..169", &t)
        );
    }

    /// The caret moves at one instant after the last key whenever the settling reading came, and a move at or past the cold instant or past the hang detector is refused.
    #[test]
    fn the_caret_moves_at_one_instant_after_the_last_key_whatever_the_code() {
        let last_key = Instant::now();
        let after = |ms: u64| last_key + Duration::from_millis(ms);
        let cold = Some(after(4_600));
        for read in [105, 630, 2_632] {
            let wait = move_wait(after(read), last_key, cold).expect("in the tail");
            assert_eq!(
                after(read) + wait,
                last_key + MOVE_AFTER_LAST_KEY,
                "a settling reading {read} ms after the last key"
            );
        }
        assert_eq!(
            move_wait(after(3_500), last_key, cold),
            Ok(Duration::ZERO),
            "a reading past the move's instant moves at once"
        );
        for early_cold in [after(3_000), after(2_000)] {
            let why = move_wait(after(105), last_key, Some(early_cold)).expect_err("past cold");
            assert!(why.contains("cold instant"), "{why}");
        }
        let why = move_wait(last_key, after(61_000), None).expect_err("a hang");
        assert!(why.contains("hang detector"), "{why}");
    }

    /// A real publication's bundle as a window of the build `build` exports it: the grid winner admitted by an evaluator of that build and published over a paused shipped glow.
    fn published_bundle(build: &str) -> (Vec<u8>, u64) {
        use aterm_effects::aos_momentum::{MomentumCode, MomentumHost};
        let origin = Instant::now();
        let ev = Evaluator::v1(build, origin).expect("the pinned policy");
        let winner = SchedulePlan::decode(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("a plan");
        let admitted = ev
            .admit_one(&winner.encode())
            .expect("the winner is admitted");
        let mut host = MomentumHost::default();
        host.aos_attach(origin, MomentumCode::Shipped, build, 1);
        host.aos_pause(origin, Duration::from_nanos(16_666_667))
            .expect("a running glow pauses");
        let published = host.aos_publish(admitted).expect("the winner publishes");
        (published.bundle().encode(), published.revision().0)
    }

    /// A window's bundle is verified in the lab only under the same clean commit and refused offline at any commit, and the bundle gates pass only on the exported, verified bundle the publication names.
    #[test]
    fn a_published_bundle_passes_its_gates_and_is_verified_only_under_the_same_clean_commit() {
        let build = "0.0.0+g0123456789ab";
        let lab = "aterm-lab 0.0.0+g0123456789ab";
        let (bytes, rev) = published_bundle(build);
        assert_eq!(verify_in_lab(&bytes, build, lab), Verify::Verified);
        for offline_lab in [lab, "aterm-lab 0.0.0+g0123456789ac"] {
            let (code, r) = crate::aos_search::verify_bundle(
                &bytes,
                "w.bundle",
                offline_lab,
                || -> Result<&Evaluator, aterm_effects::aos_momentum::policy::PolicyRefusal> {
                    panic!("an evaluator was built for a window's bundle")
                },
            );
            assert_eq!(code, 1, "{r}");
            assert!(
                r.ends_with("aos-search verify=fail reason=ForeignBuild\n"),
                "{offline_lab}: {r}"
            );
        }
        assert!(matches!(
            verify_in_lab(&bytes[..bytes.len() - 1], build, lab),
            Verify::Refused(_)
        ));
        for (instance, lab) in [
            (build, "aterm-lab 0.0.0+g0123456789ac"),
            (build, "aterm-lab 0.0.0+g0123456789ab-dirty"),
            (
                "0.0.0+g0123456789ab-dirty",
                "aterm-lab 0.0.0+g0123456789ab-dirty",
            ),
            ("0.0.0+gunknown", "aterm-lab 0.0.0+gunknown"),
            ("none", lab),
        ] {
            let v = verify_in_lab(&bytes, instance, lab);
            assert_eq!(
                v,
                Verify::Foreign {
                    instance: instance.into(),
                    lab: lab.into()
                }
            );
            assert!(
                v.word().starts_with("verify=skipped(foreign build) "),
                "{}",
                v.word()
            );
        }

        let digest = Digest::of(BUNDLE_DOMAIN, &[&bytes]).hex();
        let b = BundleLine {
            window: 0,
            bundle: digest.clone(),
            rev,
            bytes: bytes.len(),
            hex: aterm_codec::hex::encode_lower(&bytes),
        };
        let p = PublishLine {
            rev,
            bundle: digest[..12].to_string(),
            ..parse_publish(&publish_reply()).expect("a publish reply")
        };
        let plan = [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            failed(&bundle_gates(&b, &p, &plan, &Verify::Verified)),
            Vec::<&str>::new()
        );
        let skipped = Verify::Foreign {
            instance: build.into(),
            lab: "aterm-lab 0.0.0+g0123456789ac".into(),
        };
        assert_eq!(
            failed(&bundle_gates(&b, &p, &plan, &skipped)),
            vec!["bundle-verifies"]
        );
        let later = PublishLine {
            rev: rev + 1,
            ..p.clone()
        };
        assert_eq!(
            failed(&bundle_gates(&b, &later, &plan, &Verify::Verified)),
            vec!["bundle-exported", "bundle-publication"]
        );
        let other = PublishLine {
            bundle: "000000000000".into(),
            ..p.clone()
        };
        assert_eq!(
            failed(&bundle_gates(&b, &other, &plan, &Verify::Verified)),
            vec!["bundle-exported"]
        );
        let mut another = plan;
        another[11] = 1;
        assert_eq!(
            failed(&bundle_gates(&b, &p, &another, &Verify::Verified)),
            vec!["bundle-plan"]
        );
        let cut = BundleLine {
            hex: aterm_codec::hex::encode_lower(&bytes[..bytes.len() - 1]),
            bytes: bytes.len() - 1,
            ..b
        };
        assert_eq!(
            failed(&bundle_gates(&cut, &p, &plan, &Verify::Verified)),
            vec!["bundle-exported", "bundle-plan", "bundle-publication"]
        );
    }
}
