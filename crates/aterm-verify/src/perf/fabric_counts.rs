// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `fabric-counts` floor lane: the bus campaign's structural wins, judged as the exact counts
//! the fabric bench already reports, at the load the lane finds.
//!
//! The fabric lane ([`super::fabric`]) holds no floor: what it times is a disk's fsync and a
//! shared desk's scheduler before it is aterm's. Some of what its bench reports is not time but
//! structure, how many calls of a kind a world made and how many records a restart read back from
//! the log file, and those counts are what the campaign's wins removed. Unit tests pin each step
//! alone; only the bench sees aterm's handlers and the bridge compose. This lane builds the bench
//! as the fabric lane does ([`bench_build::prepare`], the `test` profile), runs three small worlds
//! on its default in-process broker ([`worlds`]) without waiting for a quiet machine, reports the
//! load each ran at, and judges every count against its law ([`Law`]) and the committed baseline
//! (`perf-baseline-fabric-counts.json`), whichever is lower:
//!
//! - `attach_<N>_lane_meta`: the bridge's `@<sid> meta` calls over every restart of the attach
//!   world at N sessions, from the wait tally: 0 since E05 (640 over ten restarts at 32 before);
//! - `pingpong_fleet_mail`, `pingpong_paced_fleet_mail`: aterm's fleet-mailbox transactions in
//!   the ping-pong world's back-to-back and paced round trips, from the handler tally: 0 since
//!   D01 (six a round trip before);
//! - `messaging_<window>_rate_limited`: the publishes the broker refused for the rate in the
//!   messaging world's latency, burst and paced windows, under aterm's default rate limit: 0
//!   (E06);
//! - `restart_<N>_cold_reads_first`, `restart_<N>_cold_reads_later`: the records a restart's
//!   attach read back from the file with every seeded note behind the broker's resident window:
//!   at most 2N − 1 in the first restart after the seeding and N − 1 in each later one (D10d:
//!   the shared refill's N − 1, and the outbox drain's N once).
//!
//! A count is exact or it is not judged here. The lane gives no ratio a ceiling and judges no
//! time: `broker_batches_per_note`, `broker_batches_per_message` and the latencies depend on how
//! the bridge's writes coalesce, which moves with the load, so each run prints them as measured.
//! So does E04's count of paced notes whose sender snapshot lands between the delivery and its
//! commit, 54-56 of 60 before E04: it moved, so it is no floor. Main's own ledger reads it at 0
//! to 3 of 60 from run to run (`docs/measured/fabric/2026-11-05-fabric-6061` and `-6065` read 1,
//! `-4106` 3), this lane's first three runs 3, 0 and 3 of 20, and a run with every `Fx::now` of
//! the bridge slowed by 6 ms 20 of 20, where each judged count held its value
//! (`docs/measured/fabric-counts-2026-11-05.md`).
//!
//! The judge reads a bench report ([`counts`]), so its unit tests run it over the committed
//! ledger entry of the arm before each win, which it must fail on that win's law.

use std::collections::BTreeMap;
use std::path::Path;

use aterm_json::Value;

use super::arena::{self, Halt};
use super::bench_build::bench_profile::Profile;
use super::bench_build::{self, Built, Entry, Targo, another_build, load_average};
use super::fabric::{self, bench_spec};
use super::{LaneRun, Status};

/// The lane's name.
const LANE: &str = "fabric-counts";

/// The committed baseline, in `crates/aterm-verify/perf/golden/`.
const BASELINE_FILE: &str = "perf-baseline-fabric-counts.json";

const COMMENT: &str = "aterm FABRIC-COUNTS baseline: exact structural counts of the fabric \
    bench's worlds on its default in-process broker, `test` profile: attach at 8 and 32 \
    sessions with both tallies on and 100,000 records behind every session's refill watermark, \
    2 restarts each; ping-pong, 200 round trips back to back and 50 paced; messaging, 200 \
    round trips, a 500-post burst and 20 paced notes, under aterm's default rate limit. \
    SMALLER-OR-EQUAL: a count above its value here fails, and so does one past its law \
    (lane_meta, fleet_mail and rate_limited 0; a restart's cold reads 2N - 1 at the first \
    restart and N - 1 after), which a recording refuses to write. No ratio and no time is \
    judged. The lane runs at the load it finds and reports it. Re-record with targo \
    --unverified verify perf --record fabric-counts.";

/// The attach world's populations.
const ATTACH: [u32; 2] = [8, 32];

/// Bridge restarts per population: the first after the seeding reads the drain's fetches cold
/// too, and the second shows the steady state.
const RESTARTS: u32 = 2;

/// Records filled behind every session's refill watermark: past what the broker's 16 MiB
/// resident window can hold of them (16 MiB / [`bench_spec::FILL_RECORD_BYTES`], 78,766), so
/// every seeded note is cold when the restarts read it. [`cold_path_reached`] checks this
/// against the budget the report names.
const BACKLOG: u32 = 100_000;

/// The messaging world's paced notes: enough to print a paced note's batches as measured; the
/// paced window's refusals are judged with the others.
const PACED_NOTES: u32 = 20;

/// The win whose law a count is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Law {
    /// E05: the attach reads each session's meta from the status batch and the watch ack, and
    /// calls `@<sid> meta` for none.
    AttachMeta,
    /// D01: `inbox` and `await inbox` read the fleet mailbox only when its file moved.
    FleetMail,
    /// E06: the default rate limit refuses nothing a world of this size publishes.
    RateLimited,
    /// D10d: the first restart after the seeding reads 2N − 1 records cold.
    ColdFirst,
    /// D10d: every later restart reads N − 1 records cold.
    ColdLater,
}

impl Law {
    const fn win(self) -> &'static str {
        match self {
            Law::AttachMeta => {
                "E05: the attach reads each session's meta from the status batch and the watch ack"
            }
            Law::FleetMail => "D01: the fleet mailbox is read only when its file moved",
            Law::RateLimited => {
                "E06: aterm's default rate limit refuses nothing these worlds publish"
            }
            Law::ColdFirst => {
                "D10d: the first restart after the seeding reads the shared refill's N - 1 \
                 records cold and the outbox drain's N"
            }
            Law::ColdLater => "D10d: a later restart reads the shared refill's N - 1 records cold",
        }
    }
}

/// One count a bench report holds: its key, its value, and the most its law allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Count {
    pub key: String,
    pub value: u64,
    pub law: Law,
    pub bound: u64,
}

/// One world the lane runs: its name and the bench run it is.
struct World {
    name: &'static str,
    args: fabric::Args,
}

/// The lane's worlds, each one run of the fabric bench on the shipped arm (ordered, its 100 ms
/// horizon, sealed at rest, the broker in the bench's process).
fn worlds() -> [World; 3] {
    [
        World {
            name: "attach",
            args: fabric::Args {
                scenario: bench_spec::Scenario::Attach,
                attach: Some(ATTACH.to_vec()),
                rounds: Some(RESTARTS),
                backlog: Some(BACKLOG),
                tally: true,
                ..fabric::Args::default()
            },
        },
        World {
            name: "pingpong",
            args: fabric::Args {
                scenario: bench_spec::Scenario::Pingpong,
                ..fabric::Args::default()
            },
        },
        World {
            name: "messaging",
            args: fabric::Args {
                scenario: bench_spec::Scenario::Messaging,
                paced_notes: Some(PACED_NOTES),
                ..fabric::Args::default()
            },
        },
    ]
}

/// Every key the lane's worlds must report: a key missing from a run fails it, so no law is
/// skipped by a report that stopped carrying its count.
fn expected() -> Vec<String> {
    let mut keys: Vec<String> = ATTACH
        .iter()
        .map(|n| format!("attach_{n}_lane_meta"))
        .collect();
    keys.extend(["pingpong_fleet_mail", "pingpong_paced_fleet_mail"].map(String::from));
    keys.extend(
        ["latency", "burst", "paced"].map(|window| format!("messaging_{window}_rate_limited")),
    );
    for n in ATTACH {
        keys.extend(["first", "later"].map(|when| format!("restart_{n}_cold_reads_{when}")));
    }
    keys
}

/// The value of `key=` among a report's `spec` words.
fn spec_word<'a>(report: &'a Value, key: &str) -> Option<&'a str> {
    report
        .get("spec")
        .and_then(Value::as_str)?
        .split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
}

/// The session count an attach tally round's group names (`attach, 8 session(s)`).
fn attach_sessions(group: &str) -> Option<u64> {
    group
        .strip_prefix("attach, ")?
        .strip_suffix(" session(s)")?
        .parse()
        .ok()
}

/// The wait-tally formats (an attach round's `wait.format`: `aterm_link::wait_tally::FORMAT`)
/// whose `lane_meta` row counts the bridge's `@<sid> meta` calls and whose
/// `lane_sessions_status` row its `sessions status` batches, under those names: v=4, the format
/// of E05's committed arms, through v=7, the bridge's when this lane was written, and v=8, which
/// split the subject budget's reads out of `request` (critic item G04; no bump between them
/// touched a `lane_` row). The report leaves out every row that counted nothing, so in a
/// format that renamed or split either row, a read by the old name would pass E05's law at 0. A
/// round in any other format is refused until those rows are checked and its format added here.
const WAIT_FORMATS: [&str; 5] = [
    "wait-tally v=4",
    "wait-tally v=5",
    "wait-tally v=6",
    "wait-tally v=7",
    "wait-tally v=8",
];

/// A tally row's count: a row the tally never counted is absent, which is 0.
fn row_count(rows: &aterm_json::Map, row: &str, at: &str) -> Result<u64, String> {
    match rows.get(row) {
        None => Ok(0),
        Some(r) => r
            .get("count")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{at}: the `{row}` row has no count")),
    }
}

/// Every structural count `report` (a fabric bench report, `fabric.json`) holds, keyed as the
/// baseline keys them. A world the report did not run, or one that ran without what a count
/// needs (the tally off, an older handler tally, no rate limit, no backlog), yields none of
/// that world's counts; a report that has the world but not the shape the count is read from is
/// an error.
///
/// # Errors
/// A report whose attach tally, ping-pong handlers, messaging windows or backlog sizes do not
/// read as the bench writes them, or whose attach tally is in a wait-tally format outside
/// [`WAIT_FORMATS`].
pub(crate) fn counts(report: &Value) -> Result<Vec<Count>, String> {
    let mut out = Vec::new();
    attach_meta(report, &mut out)?;
    fleet_mail(report, &mut out)?;
    rate_limited(report, &mut out)?;
    cold_reads(report, &mut out)?;
    Ok(out)
}

/// `attach_<N>_lane_meta`: every restart's `lane_meta` row at N sessions, summed. Each round must
/// be in a wait-tally format of [`WAIT_FORMATS`], so the rows it reads are the ones it names, and
/// must have counted the attach's `sessions status` batch, or it is not the attach the law is of.
fn attach_meta(report: &Value, out: &mut Vec<Count>) -> Result<(), String> {
    let Some(rounds) = report
        .pointer("/attach/tallies/rounds")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let mut by_size: BTreeMap<u64, u64> = BTreeMap::new();
    for round in rounds {
        let group = round
            .get("group")
            .and_then(Value::as_str)
            .ok_or("an attach tally round names no group")?;
        let sessions = attach_sessions(group)
            .ok_or_else(|| format!("an attach tally round's group `{group}` names no sessions"))?;
        match round.pointer("/wait/format").and_then(Value::as_str) {
            Some(format) if WAIT_FORMATS.contains(&format) => {}
            Some(format) => {
                return Err(format!(
                    "`{group}`: a tally round in `{format}`, a wait-tally format this lane was \
                     not written against ({}): check its `lane_meta` and \
                     `lane_sessions_status` rows, then add it to `WAIT_FORMATS`",
                    WAIT_FORMATS.join(", ")
                ));
            }
            None => {
                return Err(format!(
                    "`{group}`: a tally round that names no wait-tally format"
                ));
            }
        }
        let rows = round
            .pointer("/wait/rows")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("`{group}`: a tally round with no wait rows"))?;
        if row_count(rows, "lane_sessions_status", group)? == 0 {
            return Err(format!(
                "`{group}`: a tally round that counted no `sessions status` batch, so no attach"
            ));
        }
        let total = by_size.entry(sessions).or_default();
        *total = total.saturating_add(row_count(rows, "lane_meta", group)?);
    }
    out.extend(by_size.into_iter().map(|(n, value)| Count {
        key: format!("attach_{n}_lane_meta"),
        value,
        law: Law::AttachMeta,
        bound: 0,
    }));
    Ok(())
}

/// `pingpong_fleet_mail` and `pingpong_paced_fleet_mail`: the handler tally's `fleet_mail` row in
/// each ping-pong window, which must have counted the `inbox` listings it spans. A handler tally
/// before v=2 has no such row and yields none.
fn fleet_mail(report: &Value, out: &mut Vec<Count>) -> Result<(), String> {
    for (key, at) in [
        ("pingpong_fleet_mail", "/pingpong/handlers/rows"),
        ("pingpong_paced_fleet_mail", "/pingpong/paced/handlers/rows"),
    ] {
        let Some(rows) = report.pointer(at).and_then(Value::as_object) else {
            continue;
        };
        if !rows.contains_key("fleet_mail") {
            continue;
        }
        if row_count(rows, "inbox", at)? == 0 {
            return Err(format!(
                "{at}: no `inbox` listing counted, so no round trip"
            ));
        }
        out.push(Count {
            key: key.to_string(),
            value: row_count(rows, "fleet_mail", at)?,
            law: Law::FleetMail,
            bound: 0,
        });
    }
    Ok(())
}

/// `messaging_<window>_rate_limited`: each messaging window's refusals for the rate, when the
/// report's broker ran in the bench's process under aterm's default rate limit (one of its own
/// keeps its `BrokerStats` there).
fn rate_limited(report: &Value, out: &mut Vec<Count>) -> Result<(), String> {
    let Some(messaging) = report.get("messaging").filter(|m| m.is_object()) else {
        return Ok(());
    };
    if spec_word(report, "rate_limit") != Some("on")
        || spec_word(report, "broker_process").is_some()
    {
        return Ok(());
    }
    for (window, key) in [
        ("latency", "messaging_latency_rate_limited"),
        ("burst", "messaging_burst_rate_limited"),
        ("paced_notes", "messaging_paced_rate_limited"),
    ] {
        let Some(w) = messaging.get(window).filter(|w| w.is_object()) else {
            continue;
        };
        let value = w
            .get("rate_limited")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                format!("the messaging {window} window reports no rate_limited count")
            })?;
        out.push(Count {
            key: key.to_string(),
            value,
            law: Law::RateLimited,
            bound: 0,
        });
    }
    Ok(())
}

/// `restart_<N>_cold_reads_first` and `_later`: the first restart's cold reads at N sessions, and
/// the most any later restart read. A backlog before D10d reported none and yields none.
fn cold_reads(report: &Value, out: &mut Vec<Count>) -> Result<(), String> {
    let Some(sizes) = report
        .pointer("/attach/backlog/sizes")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for size in sizes {
        let Some(reads) = size.get("cold_reads_per_restart") else {
            continue;
        };
        let n = size
            .get("sessions")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or("a backlog size names no sessions")?;
        let reads: Vec<u64> = reads
            .as_array()
            .ok_or_else(|| format!("{n} sessions: cold_reads_per_restart is not a list"))?
            .iter()
            .map(|r| {
                r.as_u64()
                    .ok_or_else(|| format!("{n} sessions: a cold read count is not a count"))
            })
            .collect::<Result<_, _>>()?;
        if let Some(&first) = reads.first() {
            out.push(Count {
                key: format!("restart_{n}_cold_reads_first"),
                value: first,
                law: Law::ColdFirst,
                bound: n.saturating_mul(2) - 1,
            });
        }
        if let Some(&later) = reads.iter().skip(1).max() {
            out.push(Count {
                key: format!("restart_{n}_cold_reads_later"),
                value: later,
                law: Law::ColdLater,
                bound: n - 1,
            });
        }
    }
    Ok(())
}

/// Why the attach world's restarts did not read the seeded notes cold, if they did not: the
/// backlog it filled must outgrow the resident window the report names, or the cold-read counts
/// would hold their bounds at 0 for want of reading anything cold.
fn cold_path_reached(report: &Value) -> Result<(), String> {
    let backlog = report
        .pointer("/attach/backlog/records")
        .and_then(Value::as_u64)
        .ok_or("the attach world reports no backlog")?;
    let budget = report
        .pointer("/attach/backlog/resident_budget")
        .and_then(Value::as_u64)
        .ok_or("the attach world's broker names no resident budget, so it reads nothing cold")?;
    let filled = backlog.saturating_mul(bench_spec::FILL_RECORD_BYTES as u64);
    if filled <= budget {
        return Err(format!(
            "the attach world's backlog of {backlog} records ({filled} B) fits the broker's \
             {budget} B resident window, so no restart reads the seeded notes cold"
        ));
    }
    Ok(())
}

/// Judge `counts` against their laws and, when there is one, `baseline` (the committed file's
/// JSON): a count past its law or above its baseline fails; one below its baseline passes and
/// says a recording would ratchet it.
pub(crate) fn judge(counts: &[Count], baseline: Option<&Value>) -> LaneRun {
    let mut run = LaneRun::pass();
    for c in counts {
        if c.value > c.bound {
            eprintln!(
                "    {}: FAILED — {} > {}, its law's bound",
                c.key, c.value, c.bound
            );
            run.add_failure(format!(
                "{} {} > {}, past its law ({})",
                c.key,
                c.value,
                c.bound,
                c.law.win()
            ));
            continue;
        }
        match baseline.and_then(|b| b.get(&c.key)).and_then(Value::as_u64) {
            Some(floor) if c.value > floor => {
                eprintln!("    {}: FAILED — {} > baseline {floor}", c.key, c.value);
                run.add_failure(format!("{} {} > its baseline {floor}", c.key, c.value));
            }
            Some(floor) if c.value < floor => eprintln!(
                "    {}: GREEN — {} < baseline {floor} (--record {LANE} ratchets it)",
                c.key, c.value
            ),
            Some(floor) => eprintln!("    {}: GREEN — {} == baseline {floor}", c.key, c.value),
            None => eprintln!(
                "    {}: GREEN — {} <= {}, its law's bound (no baseline)",
                c.key, c.value, c.bound
            ),
        }
    }
    if baseline.is_none() {
        run.unjudged.push(format!(
            "{LANE} baseline (none recorded; its laws alone judged)"
        ));
    }
    run
}

/// The baseline file's text: every count by key.
fn baseline_json(counts: &[Count]) -> String {
    let by_key: BTreeMap<&str, u64> = counts.iter().map(|c| (c.key.as_str(), c.value)).collect();
    let mut s = format!(
        "{{\n  \"_comment\": {}",
        aterm_json::to_string(&Value::String(COMMENT.to_string()))
            .unwrap_or_else(|_| "\"\"".to_string())
    );
    for (key, value) in by_key {
        s.push_str(&format!(",\n  \"{key}\": {value}"));
    }
    s.push_str("\n}\n");
    s
}

/// The committed baseline at `path`: `Ok(None)` when there is none.
fn read_baseline(path: &Path) -> Result<Option<Value>, String> {
    let text = match crate::read_regular_text(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} is unreadable: {e}", path.display())),
    };
    aterm_json::from_str::<Value>(text.trim())
        .ok()
        .filter(Value::is_object)
        .map(Some)
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))
}

/// Record or judge `counts` against the baseline at `path`. A recording refuses a count past its
/// law, so no law is weakened by writing its regression down.
fn conclude(counts: &[Count], record: bool, path: &Path) -> LaneRun {
    conclude_using(counts, record, path, super::publication::publish)
}

fn conclude_using(
    counts: &[Count],
    record: bool,
    path: &Path,
    publish: super::publication::Publisher,
) -> LaneRun {
    if record {
        let laws = judge(counts, None);
        if let Status::Fail(findings) = laws.status {
            return LaneRun::fail(format!(
                "REFUSING to record counts past their laws: {}",
                findings.join("; ")
            ));
        }
        return match publish(path, &baseline_json(counts)) {
            Ok(()) => {
                eprintln!("  RECORDED baseline -> {}", path.display());
                LaneRun {
                    status: Status::Recorded(path.to_path_buf()),
                    ..LaneRun::pass()
                }
            }
            Err(e) => LaneRun::fail(format!("could not write {}: {e}", path.display())),
        };
    }
    match read_baseline(path) {
        Ok(baseline) => judge(counts, baseline.as_ref()),
        Err(why) => LaneRun::fail(why),
    }
}

/// The lane: build the bench, run its worlds, then judge (or record) their counts.
pub(crate) fn run(record: bool) -> LaneRun {
    match measure() {
        Ok(counts) => conclude(
            &counts,
            record,
            &super::floors::golden_dir().join(BASELINE_FILE),
        ),
        Err(halt) => arena::outcome(Err(halt)),
    }
}

fn measure() -> Result<Vec<Count>, Halt> {
    let driver = crate::perf::driver::cargo_driver()?;
    let worlds = worlds();
    let build_args = worlds[0].args.build_args();
    let (targo, built, load_before) = bench_build::prepare(driver, Profile::Dev, &build_args)?;
    eprintln!("  load (1, 5, 15 min) before the build: {load_before}");
    let mut found: Vec<Count> = Vec::new();
    for world in &worlds {
        let before = load_average();
        let report = run_world(&targo, &world.args, &built)?;
        eprintln!(
            "  {}: load {before} before the world, {} after",
            world.name,
            load_average()
        );
        if world.args.backlog.is_some() {
            cold_path_reached(&report).map_err(Halt::Fail)?;
        }
        for line in measured(&report) {
            eprintln!("    measured, never judged: {line}");
        }
        found.extend(counts(&report).map_err(|why| format!("the {} world: {why}", world.name))?);
    }
    let missing: Vec<String> = expected()
        .into_iter()
        .filter(|key| !found.iter().any(|c| &c.key == key))
        .collect();
    if !missing.is_empty() {
        return Err(Halt::Fail(format!(
            "the bench reported no {}: nothing judges those laws",
            missing.join(", ")
        )));
    }
    Ok(found)
}

/// Run `args`'s world once on the build `built`, answering its report.
fn run_world(targo: &Targo<'_>, args: &fabric::Args, built: &Built) -> Result<Value, Halt> {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-fabric-counts-")
        .tempdir()
        .map_err(|e| format!("could not make a scratch directory: {e}"))?;
    let arm = args.plan()[0];
    let words = args.bench_words(arm);
    bench_build::run_bench(
        targo,
        &args.build_args(),
        (bench_spec::ENV, &words),
        (bench_spec::OUT_ENV, scratch.path()),
        "fabric bench:",
        "the fabric bench",
        Entry::Counted,
    )?;
    let path = scratch.path().join("fabric.json");
    let text = crate::read_regular_text(&path)
        .map_err(|e| format!("the bench wrote no {}: {e}", path.display()))?;
    let report: Value = aterm_json::from_str(&text)
        .map_err(|e| format!("the bench's fabric.json does not parse: {e}"))?;
    if let Some(why) = another_build(&report, &built.path, &built.identity) {
        return Err(Halt::Fail(format!(
            "{why}: these counts are not this build's"
        )));
    }
    if report.get("scenario").and_then(Value::as_str) != Some(args.scenario.word()) {
        return Err(Halt::Fail(format!(
            "the fabric bench did not report the requested scenario `{}`",
            args.scenario.word()
        )));
    }
    Ok(report)
}

/// What a report measured that the lane never judges: the ratios that depend on how the writes
/// coalesced, E04's paced-note count (it moved), and the latencies.
fn measured(report: &Value) -> Vec<String> {
    let num = |at: &str| report.pointer(at).and_then(Value::as_f64);
    let mut lines = Vec::new();
    let mut quantiles = |what: &str, at: &str| {
        if let (Some(n), Some(p50), Some(p95)) = (
            num(&format!("{at}/n")),
            num(&format!("{at}/p50")),
            num(&format!("{at}/p95")),
        ) {
            lines.push(format!("{what} n={n} p50={p50:.3} ms p95={p95:.3} ms"));
        }
    };
    quantiles("post -> visible", "/messaging/latency/deliver_ms");
    quantiles("ping-pong round trip", "/pingpong/round_trip_ms");
    if let Some(sizes) = report.pointer("/attach/sizes").and_then(Value::as_array) {
        for (i, size) in sizes.iter().enumerate() {
            let n = size.get("sessions").and_then(Value::as_u64).unwrap_or(0);
            quantiles(
                &format!("attach at {n}: pid -> every row acked"),
                &format!("/attach/sizes/{i}/attach_ms"),
            );
            quantiles(
                &format!("attach at {n}: kill -> every session's live mail visible"),
                &format!("/attach/sizes/{i}/kill_to_every_live_mail_ms"),
            );
        }
    }
    if let Some(v) = num("/messaging/burst/broker_batches_per_message") {
        lines.push(format!("burst broker_batches_per_message={v:.3}"));
    }
    if let Some(v) = num("/messaging/paced_notes/broker_batches_per_note") {
        lines.push(format!("paced broker_batches_per_note={v:.3}"));
    }
    if let (Some(with), Some(of)) = (
        num("/messaging/paced_notes/ev_between_delivery_and_commit/notes_with_any"),
        num("/messaging/paced_notes/notes"),
    ) {
        lines.push(format!(
            "paced notes with an ev between the delivery and its commit: {with} of {of} (E04's \
             count; it moves from run to run, so no floor)"
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A committed fabric ledger entry's report.
    fn entry(name: &str) -> Value {
        let path = super::super::workspace_root()
            .join("docs/measured/fabric")
            .join(format!("{name}.json"));
        let text =
            crate::read_regular_text(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        aterm_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn findings(run: &LaneRun) -> Vec<String> {
        match &run.status {
            Status::Fail(findings) => findings.clone(),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn value_of(counts: &[Count], key: &str) -> u64 {
        counts
            .iter()
            .find(|c| c.key == key)
            .unwrap_or_else(|| panic!("no {key} in {counts:?}"))
            .value
    }

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("fabric-counts-")
            .tempdir()
            .expect("a scratch directory")
    }

    /// Every world's words are a run the bench takes, as the fabric lane checks its own.
    #[test]
    fn every_world_is_a_run_the_bench_takes() {
        for world in worlds() {
            let words = world.args.bench_words(world.args.plan()[0]);
            let spec = bench_spec::parse(&words, Path::new("/scratch"))
                .unwrap_or_else(|why| panic!("{}: {why}", world.name));
            assert!(spec.rate_limit, "{}: {words}", world.name);
            assert!(!spec.broker_process.outside(), "{}: {words}", world.name);
            assert_eq!(world.args.profile, Profile::Dev);
        }
    }

    /// E05's A arm (`2026-11-05-fabric-5051`, main before the meta batch): 16 `@<sid> meta` calls
    /// a restart at 8 sessions and 64 at 32, ten restarts each. The judge fails it on E05's law;
    /// E05's B arm (`-5052`) holds it.
    #[test]
    fn the_judge_fails_e05s_a_arm_on_lane_meta() {
        let before = counts(&entry("2026-11-05-fabric-5051")).expect("reads");
        assert_eq!(value_of(&before, "attach_8_lane_meta"), 160);
        assert_eq!(value_of(&before, "attach_32_lane_meta"), 640);
        let failed = findings(&judge(&before, None));
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(
            failed
                .iter()
                .all(|f| f.contains("lane_meta") && f.contains("E05")),
            "{failed:?}"
        );
        let after = counts(&entry("2026-11-05-fabric-5052")).expect("reads");
        assert_eq!(value_of(&after, "attach_32_lane_meta"), 0);
        assert_eq!(judge(&after, None).status, Status::Pass);
    }

    /// D01's A arm (`2026-11-03-fabric-80`, main before the stamp): 1,201 fleet-mailbox
    /// transactions over 200 round trips and 301 over the 50 paced. The judge fails it on D01's
    /// law; D01's B arm (`-81`) holds it.
    #[test]
    fn the_judge_fails_d01s_a_arm_on_fleet_mail() {
        let before = counts(&entry("2026-11-03-fabric-80")).expect("reads");
        assert_eq!(value_of(&before, "pingpong_fleet_mail"), 1201);
        assert_eq!(value_of(&before, "pingpong_paced_fleet_mail"), 301);
        let failed = findings(&judge(&before, None));
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(
            failed
                .iter()
                .all(|f| f.contains("fleet_mail") && f.contains("D01")),
            "{failed:?}"
        );
        let after = counts(&entry("2026-11-03-fabric-81")).expect("reads");
        assert_eq!(judge(&after, None).status, Status::Pass);
    }

    /// No committed arm refused a publish for the rate (E06's off arm limits nothing), so the
    /// control is E06's on arm (`2026-11-05-fabric-6061`) with one refusal written into its burst
    /// window: the judge fails it on E06's law, and holds the arm as committed. Its `rate_limit=off`
    /// sibling (`-6062`) yields no rate count at all.
    #[test]
    fn the_judge_fails_a_rate_refusal_and_reads_none_without_the_limit() {
        let mut report = entry("2026-11-05-fabric-6061");
        let committed = counts(&report).expect("reads");
        assert_eq!(value_of(&committed, "messaging_burst_rate_limited"), 0);
        assert_eq!(judge(&committed, None).status, Status::Pass);
        report
            .as_object_mut()
            .and_then(|r| r.get_mut("messaging"))
            .and_then(Value::as_object_mut)
            .and_then(|m| m.get_mut("burst"))
            .and_then(Value::as_object_mut)
            .expect("a burst window")
            .insert("rate_limited".to_string(), Value::from(1u64));
        let refused = counts(&report).expect("reads");
        let failed = findings(&judge(&refused, None));
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(
            failed[0].contains("messaging_burst_rate_limited") && failed[0].contains("E06"),
            "{failed:?}"
        );
        let unlimited = counts(&entry("2026-11-05-fabric-6062")).expect("reads");
        assert!(
            unlimited.iter().all(|c| c.law != Law::RateLimited),
            "{unlimited:?}"
        );
    }

    /// No committed arm read more than D10d's bounds (its no-budget arm reads none), so the
    /// control is D10f's swap-set entry (`2026-11-05-fabric-7002`: 15 then 7 at 8 sessions, 63
    /// then 31 at 32, exactly the bounds) with one more cold read written into each regime: the
    /// judge fails both on D10d's laws.
    #[test]
    fn the_judge_holds_d10ds_cold_reads_to_their_bounds() {
        let mut report = entry("2026-11-05-fabric-7002");
        let committed = counts(&report).expect("reads");
        for (key, value) in [
            ("restart_8_cold_reads_first", 15),
            ("restart_8_cold_reads_later", 7),
            ("restart_32_cold_reads_first", 63),
            ("restart_32_cold_reads_later", 31),
        ] {
            assert_eq!(value_of(&committed, key), value, "{key}");
        }
        assert_eq!(judge(&committed, None).status, Status::Pass);
        let sizes = report
            .as_object_mut()
            .and_then(|r| r.get_mut("attach"))
            .and_then(Value::as_object_mut)
            .and_then(|a| a.get_mut("backlog"))
            .and_then(Value::as_object_mut)
            .and_then(|b| b.get_mut("sizes"))
            .and_then(Value::as_array_mut)
            .expect("backlog sizes");
        let reads = sizes[0]
            .as_object_mut()
            .and_then(|s| s.get_mut("cold_reads_per_restart"))
            .and_then(Value::as_array_mut)
            .expect("8 sessions' cold reads");
        reads[0] = Value::from(16u64);
        reads[2] = Value::from(8u64);
        let over = counts(&report).expect("reads");
        let failed = findings(&judge(&over, None));
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(
            failed[0].contains("restart_8_cold_reads_first 16 > 15") && failed[0].contains("D10d"),
            "{failed:?}"
        );
        assert!(
            failed[1].contains("restart_8_cold_reads_later 8 > 7"),
            "{failed:?}"
        );
    }

    /// A count inside its law but above its baseline fails, one below passes, and a missing
    /// baseline judges the laws alone and says so.
    #[test]
    fn the_baseline_ratchets_inside_the_law() {
        let count = |value| Count {
            key: "restart_8_cold_reads_first".to_string(),
            value,
            law: Law::ColdFirst,
            bound: 15,
        };
        let baseline: Value =
            aterm_json::from_str("{\"restart_8_cold_reads_first\": 8}").expect("parses");
        let failed = findings(&judge(&[count(9)], Some(&baseline)));
        assert!(failed[0].contains("> its baseline 8"), "{failed:?}");
        assert_eq!(judge(&[count(8)], Some(&baseline)).status, Status::Pass);
        assert_eq!(judge(&[count(7)], Some(&baseline)).status, Status::Pass);
        let alone = judge(&[count(15)], None);
        assert_eq!(alone.status, Status::Pass);
        assert_eq!(alone.unjudged.len(), 1, "{:?}", alone.unjudged);
    }

    /// A recording writes every count by key, refuses one past its law, and the file it wrote
    /// judges the same counts green.
    #[test]
    fn a_recording_refuses_a_broken_law_and_judges_what_it_wrote() {
        let dir = scratch();
        let path = dir.path().join(BASELINE_FILE);
        let held = counts(&entry("2026-11-05-fabric-5052")).expect("reads");
        let recorded = conclude_using(&held, true, &path, super::super::publication::publish);
        assert_eq!(recorded.status, Status::Recorded(path.clone()));
        let written = read_baseline(&path).expect("reads").expect("written");
        assert_eq!(
            written.get("attach_32_lane_meta").and_then(Value::as_u64),
            Some(0)
        );
        assert!(written.get("_comment").and_then(Value::as_str).is_some());
        assert_eq!(
            conclude_using(&held, false, &path, super::super::publication::publish).status,
            Status::Pass
        );
        let broken = counts(&entry("2026-11-05-fabric-5051")).expect("reads");
        let refused = conclude_using(&broken, true, &path, super::super::publication::publish);
        assert!(
            findings(&refused)[0].starts_with("REFUSING to record"),
            "{refused:?}"
        );
        let still = read_baseline(&path).expect("reads").expect("kept");
        assert_eq!(
            still, written,
            "a refused recording leaves the baseline as it was"
        );
    }

    /// A tally round that counted no `sessions status` batch is not an attach the law is of: the
    /// judge refuses to read it rather than pass it at 0.
    #[test]
    fn a_tally_round_with_no_status_batch_is_refused() {
        let mut report = entry("2026-11-05-fabric-5052");
        let rows = report
            .as_object_mut()
            .and_then(|r| r.get_mut("attach"))
            .and_then(Value::as_object_mut)
            .and_then(|a| a.get_mut("tallies"))
            .and_then(Value::as_object_mut)
            .and_then(|t| t.get_mut("rounds"))
            .and_then(Value::as_array_mut)
            .and_then(|r| r.get_mut(0))
            .and_then(Value::as_object_mut)
            .and_then(|r| r.get_mut("wait"))
            .and_then(Value::as_object_mut)
            .and_then(|w| w.get_mut("rows"))
            .and_then(Value::as_object_mut)
            .expect("the first round's rows");
        rows.remove("lane_sessions_status");
        let why = counts(&report).unwrap_err();
        assert!(why.contains("sessions status"), "{why}");
    }

    /// E05's B arm with its first tally round's `wait.format` set to `format` (`None` removes it).
    fn with_wait_format(format: Option<&str>) -> Value {
        let mut report = entry("2026-11-05-fabric-5052");
        let wait = report
            .as_object_mut()
            .and_then(|r| r.get_mut("attach"))
            .and_then(Value::as_object_mut)
            .and_then(|a| a.get_mut("tallies"))
            .and_then(Value::as_object_mut)
            .and_then(|t| t.get_mut("rounds"))
            .and_then(Value::as_array_mut)
            .and_then(|r| r.get_mut(0))
            .and_then(Value::as_object_mut)
            .and_then(|r| r.get_mut("wait"))
            .and_then(Value::as_object_mut)
            .expect("the first round's wait tally");
        match format {
            Some(f) => {
                wait.insert("format".to_string(), Value::String(f.to_string()));
            }
            None => {
                wait.remove("format");
            }
        }
        report
    }

    /// A tally round in a wait-tally format the lane was not written against is refused, and so
    /// is one that names none: the report leaves out every row that counted nothing, so a row
    /// renamed or split in a later format would read as absent and pass E05's law at 0.
    #[test]
    fn a_tally_round_in_an_unknown_wait_format_is_refused() {
        let why = counts(&with_wait_format(Some("wait-tally v=9"))).unwrap_err();
        assert!(why.contains("`wait-tally v=9`"), "{why}");
        let why = counts(&with_wait_format(None)).unwrap_err();
        assert!(why.contains("names no wait-tally format"), "{why}");
        for format in WAIT_FORMATS {
            assert!(counts(&with_wait_format(Some(format))).is_ok(), "{format}");
        }
    }

    /// The committed baseline holds every key the lane's worlds report and no other, each inside
    /// its law.
    #[test]
    fn the_committed_baseline_holds_every_expected_key_inside_its_law() {
        let path = super::super::floors::golden_dir().join(BASELINE_FILE);
        let baseline = read_baseline(&path).expect("reads").expect("committed");
        let mut keys: Vec<&String> = baseline
            .as_object()
            .expect("an object")
            .keys()
            .filter(|k| *k != "_comment")
            .collect();
        keys.sort();
        let mut want = expected();
        want.sort();
        assert_eq!(keys, want.iter().collect::<Vec<_>>());
        for n in ATTACH {
            let n = u64::from(n);
            let at = |key: String| baseline.get(&key).and_then(Value::as_u64).expect("a count");
            assert_eq!(at(format!("attach_{n}_lane_meta")), 0);
            assert!(at(format!("restart_{n}_cold_reads_first")) < 2 * n);
            assert!(at(format!("restart_{n}_cold_reads_later")) < n);
        }
    }

    /// The backlog outgrows a 16 MiB window, so the restarts read the seeded notes cold; a
    /// report whose backlog fits its window is refused.
    #[test]
    fn the_attach_world_reaches_the_cold_path() {
        assert!(u64::from(BACKLOG) * bench_spec::FILL_RECORD_BYTES as u64 > 16 * 1024 * 1024);
        assert_eq!(cold_path_reached(&entry("2026-11-05-fabric-7002")), Ok(()));
        let small: Value = aterm_json::from_str(
            "{\"attach\": {\"backlog\": {\"records\": 1, \"resident_budget\": 16777216}}}",
        )
        .expect("parses");
        assert!(cold_path_reached(&small).unwrap_err().contains("fits"));
    }

    #[test]
    fn spec_words_and_groups_read_as_the_bench_writes_them() {
        let report: Value =
            aterm_json::from_str("{\"spec\": \"samples=200 rate_limit=on tally=on\"}")
                .expect("parses");
        assert_eq!(spec_word(&report, "rate_limit"), Some("on"));
        assert_eq!(spec_word(&report, "rate"), None);
        assert_eq!(spec_word(&report, "broker_process"), None);
        assert_eq!(attach_sessions("attach, 32 session(s)"), Some(32));
        assert_eq!(attach_sessions("handoff, 2 session(s)"), None);
    }
}
