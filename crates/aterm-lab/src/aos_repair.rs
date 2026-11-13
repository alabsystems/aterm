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
use crate::lab_build::lab_build;
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
                names this lab's clean commit, which only a build with ATERM_STAMP=commit stamps; else
                verify=skipped(foreign build), a FAIL), then the same 12-key phase measured: T, P and Z, presents,
                the slot rounding of the settle answers, and the moved caret's lag
  --keep        keep the scratch world (and the frames in it when --out is not given)
  --out DIR     where the frames, sidecars, contrast reports and the transcript go, OUTSIDE the repository

exit: 0 every gate passed; 1 a gate failed or a step never happened (a one-minute hang detector); 2 usage; 3 not run
";

/// `aterm.toml` lines enabling momentum glow alone on a quiet window.
pub const CONFIG: &[&str] = &[
    "cursor_trail_style = \"off\"",
    "cursor_pet = \"off\"",
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
    if v.starts_with('+') {
        return Err(format!("{key}={v} is not a number"));
    }
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
        v if !v.starts_with('+') => v
            .parse()
            .map(Some)
            .map_err(|_| format!("{key}={v} is not an offset")),
        v => Err(format!("{key}={v} is not an offset")),
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
            out: out
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| out.parse().ok())
                .flatten()
                .ok_or_else(|| format!("quads out {out}"))?,
            under: under
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| under.parse().ok())
                .flatten()
                .ok_or_else(|| format!("quads under {under}"))?,
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
    range.split_once("..").and_then(|(_, end)| {
        end.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| end.parse().ok())?
    })
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
        width: w
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| w.parse().ok())
            .flatten()
            .ok_or_else(|| format!("image width {w}"))?,
        height: h
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| h.parse().ok())
            .flatten()
            .ok_or_else(|| format!("image height {h}"))?,
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
        let _ = crate::util::write_fresh(&self.out.join(format!("{}.{suffix}", self.stem)), text);
    }

    fn keep_bytes(&self, suffix: &str, bytes: &[u8]) {
        let _ = crate::util::write_fresh(&self.out.join(format!("{}.{suffix}", self.stem)), bytes);
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
        crate::util::copy_regular_file(&shot.path, dest)
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
            let v = verify_in_lab(&bytes, d.build.as_deref().unwrap_or("none"), lab_build());
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
    // The first present booked after the move (the last reading when none came): the move's own
    // when the lag line's present_carries_move holds, after which no present has a key or the move
    // behind it; when it does not, it may be a tick composed before the echo, and the move's present
    // then among those counted after it.
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
    /// The first present booked past `frames_before` as `(lo, hi)` ns after the move's send (no later than `hi`, and later than `lo` when it showed the move); `None` when none came within [`MOVE_WATCH`]. It showed the moved caret when [`aos_phase::first_present_carries`] says so of [`Moved::barrier_lo`]; when it does not, it may be a glow tick composed before the echo reached the grid.
    pub present: Option<(u64, u64)>,
    /// The move's offset from the attach epoch: no earlier than `at_lo` (its send) and no later than `at_hi` (its reply).
    pub at_lo: i128,
    pub at_hi: i128,
    /// No later than when the drive asked for `before` (ns from the attach epoch): every present booked past `frames_before` was composed at a glow tick from here on.
    pub barrier_lo: i128,
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
    let cursor_before = parse_cursor(&d.ask("cursor")?);
    // The main loop answers `aos status` between passes, so every present composed before its reply
    // is booked in `frames_before`, read after it.
    let barrier_asked = Instant::now();
    let before = d.status()?;
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
        barrier_lo: d.offset_lower(barrier_asked),
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
         cursor={}->{} present_after_move_ms={} present_carries_move={} metrics_reads={} \
         tick_before_move={} \
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
        m.present.is_some()
            && aos_phase::first_present_carries(
                &replayed.steps,
                m.barrier_lo,
                m.cursor_after.unwrap_or((u16::MAX, u16::MAX)),
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
    let cleanup = inst.stop();
    let _ = crate::util::copy_regular_file(&inst.log, out.join(format!("{stem}.gui.log")));
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
        cleanup.is_ok() && !pids.is_empty() && survivors.is_empty(),
        format!("aterm pids {pids:?}, survivors {survivors:?}, cleanup {cleanup:?}"),
    ));
    drop(inst);
    if let Some(sig) = stopped {
        println!(
            "aos-repair: interrupted (signal {sig}); instance cleanup {}",
            if cleanup.is_ok() {
                "completed"
            } else {
                "failed"
            }
        );
        return crate::instance::after_stop(Err(Fail::from(Stopped(sig))), cleanup);
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
    let _ = crate::util::write_fresh(
        &out.join(format!("{stem}.transcript.txt")),
        transcript.join("\n") + "\n",
    );
    if !measured.is_empty() {
        let _ = crate::util::write_fresh(
            &out.join(format!("{stem}.measured.txt")),
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
#[path = "aos_repair_tests.rs"]
mod tests;
