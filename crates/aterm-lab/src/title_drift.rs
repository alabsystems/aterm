// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unchanged-title PTY output beside native typing, on two explicitly identified builds.
//! All acceptance checks precede CSV publication; raw observations survive a refused take.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::first_key::fields;
use crate::instance::{self, Instance, Launch, World, take_flag, take_value};
use crate::{Fail, Res, stop};

const HELP: &str = "\
usage: lab title-drift --a PATH --a-source SHA40 --a-sha256 SHA64 --a-build-record JSONL \
       --b PATH --b-source SHA40 --b-sha256 SHA64 --b-build-record JSONL --out NEW_DIR [OPTIONS]

Opens fresh private native aterm windows, serially in A B B A order for each case.
Cases 1 and 30 are tab controls; 200 is tab stress; split is one window with two panes.
Real OS-window cases windows1,windows2,windows8 share a two-tab output/typing window;
each additional OS window has 30 unrelated raw-cat tabs. They use distinct sizing evidence.
Every logical window must bind to a distinct observed native key window before/after.
All terminal geometry, render and resize receipts are checked outside the CPU interval.
The numbered case has an output window and N unrelated tabs in a second window, whose
front tab receives native typing. The source PTY runs a gated Rust output producer; all
other tabs run raw, echoing cats. Two persistent control connections observe the source
and post native keys to the typing window. No control input feeds the measured output.
The source's title never changes. No screenshot or video runs during measurement.

  --profile agent|stress  agent: 256 bytes/20 ms; stress: 4096 bytes/10 ms (default agent)
  --duration-ms N         fixed workload interval (default 20000)
  --typing-ms N           native burst interval (default 80% of output interval: 16000)
  --block-bytes N         override the profile's block size (64..65536)
  --output-interval-ms N  override the profile's output gap
  --key-interval-ms N     native key gap (default 40)
  --cases LIST           1,30,200,split,windows1,windows2,windows8
                         default: 1,30,200,split (real-window cases are explicit)
  --quads N              ABBA repetitions per case (default 3)
  --comparison-memory-record PATH  Frozen component peak evidence (required)
  --memory-bindings      Print exact case/workload/build evidence keys; start nothing
  --keep                 retain successful scratch worlds too

The duration must divide into whole output and key intervals (1..1500 keys). For a quick
harness check use --cases 1,split --quads 1 --duration-ms 400. This is not a performance sample.
Source SHAs are declared provenance; this tool cannot prove a source-to-artifact build link.
The supplied trusted compiler JSONL must bind the executable digest to an aterm host build
with debug_assertions=true. Release and release+dev-seams artifacts are refused before launch.
The record is trusted build evidence, not a cryptographic proof of how it was produced.
It verifies each explicit executable's SHA-256 before launch, at entry and at completion,
and binds whole-process CPU counters to the owned app PID, birth/exec identity and path.

Each unchanged ABBA quad retains one original budgeted strict Timing claim. Setup and warmup
precede fresh CPU/load/GPU entry observations; completion takes a fresh actual CPU/load/GPU
observation under the same unexpired original custody. Neither observation proves continuous
quiet or renews admission. Each quad releases and requeues before the next. Genuine native focus, exact input booking
and echoed text, completed output, stable geometry and clean cast drift are required.
The entire native posting RPC must fall between the first and last actual output sends,
measured with the same host CLOCK_MONOTONIC in both processes; partial overlap is refused.
Output sends preserve their minimum gap, never bunch to catch up; actual send timestamps and
duration are retained. CPU is the app process's work during output, typing and completion
drain; it excludes the producer process's own CPU and all setup.

Only a fully accepted run emits accepted.csv and CSV on stdout: per-take whole-app CPU/byte,
n_input and input p50/p95/p99 (never an average p99), with exact workload/provenance fields.
Global n_present/present p50/p95/p99 are diagnostic and not source-specific; a hidden output
tab/window does not establish output-to-glass latency. Native typing retains input quantiles.
Raw per-take diagnostics are retained on refusal; no accepted CSV is then emitted. Binary
digests and timing evidence are printed to stderr. Timings have no pass/fail threshold.
The background output window may be occluded: this measures its output/title handling, not
its visible repaint rate. The source cast is exported after timing. An untruncated cast must
retain every corpus byte and output in all four time quarters; stress may retain only its tail.
Accepted rows state retained cast bytes, events, span and truncation. Global wake counters
remain diagnostics and are never treated as source-specific counts.
Exit 0 accepted; 2 usage; 3 NOT_RUN (including missing native focus or timing evidence).
";

pub(crate) const HANG: Duration = Duration::from_secs(60);
const MARKER: &str = "TITLE_DRIFT_OUTPUT_COMPLETE";
pub(crate) const PROGRAM: &str = "stty raw -echo && exec /bin/cat";
pub(crate) const LAUNCHER: &str = "IFS= read -r command; eval \"$command\"";
pub(crate) const CLEAR: &[u8] = b"\x1b[2J\x1b[3J\x1b[H";
pub(crate) type Wire = aterm_agent::RelayClient<aterm_uds::CtlStream>;

#[path = "title_drift_calibration.rs"]
mod calibration;
#[path = "title_drift_producer.rs"]
mod producer;
pub(crate) use calibration::cmd as memory_cmd;
pub(crate) use producer::main as producer_main;

#[derive(Clone, Debug)]
pub(crate) struct Build {
    pub(crate) path: PathBuf,
    pub(crate) source: String,
    pub(crate) digest: String,
    pub(crate) record: PathBuf,
    pub(crate) record_digest: String,
    pub(crate) artifact: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Tabs(usize),
    Split,
    Windows(WindowTopology),
}

/// Real OS-window scaling: a fixed two-tab workload plus unrelated 30-tab windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowTopology {
    windows: usize,
}

impl WindowTopology {
    const IDLE_TABS: usize = 30;

    fn sessions(self) -> usize {
        2 + (self.windows - 1) * Self::IDLE_TABS
    }
}

impl Case {
    fn label(self) -> String {
        match self {
            Self::Tabs(n) => n.to_string(),
            Self::Split => "split".into(),
            Self::Windows(topology) => format!("windows{}", topology.windows),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "1" => Ok(Self::Tabs(1)),
            "30" => Ok(Self::Tabs(30)),
            "200" => Ok(Self::Tabs(200)),
            "split" => Ok(Self::Split),
            "windows1" => Ok(Self::Windows(WindowTopology { windows: 1 })),
            "windows2" => Ok(Self::Windows(WindowTopology { windows: 2 })),
            "windows8" => Ok(Self::Windows(WindowTopology { windows: 8 })),
            _ => Err("--cases accepts 1,30,200,split,windows1,windows2,windows8".into()),
        }
    }

    fn receipt(self) -> aterm_json::Value {
        use aterm_json::json;
        match self {
            Self::Windows(topology) => json!({"schema":"title_window_topology_v1",
                "kind":"cohosted_output_typing", "os_windows":topology.windows,
                "workload_window_tabs":2, "unrelated_windows":topology.windows - 1,
                "tabs_per_unrelated_window":WindowTopology::IDLE_TABS,
                "terminal_sessions":topology.sessions()}),
            Self::Tabs(n) => json!({"schema":"title_window_topology_v1",
                "kind":"separate_output_typing", "os_windows":2,
                "output_window_tabs":1, "typing_window_tabs":n, "terminal_sessions":n + 1}),
            Self::Split => json!({"schema":"title_window_topology_v1",
                "kind":"split", "os_windows":1, "tabs":1, "terminal_sessions":2}),
        }
    }
}

#[derive(Clone, Debug)]
struct Work {
    profile: String,
    duration_ms: u64,
    typing_ms: u64,
    block_bytes: usize,
    output_ms: u64,
    key_ms: u64,
}

impl Work {
    fn validate(&self) -> Result<(), String> {
        if !(64..=65536).contains(&self.block_bytes)
            || self.duration_ms == 0
            || self.duration_ms > 60000
            || self.output_ms == 0
            || self.key_ms == 0
            || self.key_ms > 1000
            || self.typing_ms == 0
            || self.typing_ms >= self.duration_ms
            || !self.duration_ms.is_multiple_of(self.output_ms)
            || !self.typing_ms.is_multiple_of(self.key_ms)
            || !(1..=1500).contains(&self.keys())
            || !(4..=6000).contains(&self.blocks())
        {
            return Err("invalid workload: use 64..65536 bytes, 1..60000 ms, whole nonzero intervals, 1..1500 keys and 4..6000 blocks".into());
        }
        Ok(())
    }

    fn keys(&self) -> u64 {
        self.typing_ms.checked_div(self.key_ms).unwrap_or(0)
    }

    fn blocks(&self) -> u64 {
        self.duration_ms.checked_div(self.output_ms).unwrap_or(0)
    }

    fn payloads(&self) -> (Vec<u8>, Vec<u8>) {
        let mut block = vec![b'x'; self.block_bytes];
        block[self.block_bytes - 2..].copy_from_slice(b"\r\n");
        let mut last = block.clone();
        let tail = format!("\r\n{MARKER}\r\n");
        last[self.block_bytes - tail.len()..].copy_from_slice(tail.as_bytes());
        (block, last)
    }
}

struct Options {
    builds: [Build; 2],
    out: PathBuf,
    work: Work,
    cases: Vec<Case>,
    quads: usize,
    keep: bool,
    memory_record: Option<PathBuf>,
    memory_bindings: bool,
}

pub(crate) fn number(args: &mut Vec<String>, name: &str, default: u64) -> Result<u64, Fail> {
    take_value(args, name)?
        .map(|s| {
            s.parse()
                .map_err(|_| Fail::usage(format!("{name}: expected an integer")))
        })
        .unwrap_or(Ok(default))
}

impl Build {
    /// Parse one explicitly identified debug artifact using the native measurement contract.
    pub(crate) fn parse(args: &mut Vec<String>, name: &str) -> Result<Self, Fail> {
        let path = take_value(args, &format!("--{name}"))?
            .ok_or_else(|| Fail::usage(format!("--{name} is required")))?;
        let source = take_value(args, &format!("--{name}-source"))?
            .ok_or_else(|| Fail::usage(format!("--{name}-source is required")))?;
        let digest = take_value(args, &format!("--{name}-sha256"))?
            .ok_or_else(|| Fail::usage(format!("--{name}-sha256 is required")))?;
        let record = take_value(args, &format!("--{name}-build-record"))?
            .ok_or_else(|| Fail::usage(format!("--{name}-build-record is required")))?;
        if !hex(&source, 40) || !hex(&digest, 64) {
            return Err(Fail::usage(
                "source must be 40 hex digits and SHA-256 64 hex digits",
            ));
        }
        let path = std::fs::canonicalize(path).map_err(|e| Fail::exit(3, e.to_string()))?;
        let digest = digest.to_ascii_lowercase();
        let record = std::fs::canonicalize(record).map_err(|e| Fail::exit(3, e.to_string()))?;
        let record_text =
            std::fs::read_to_string(&record).map_err(|e| Fail::exit(3, e.to_string()))?;
        let artifact = build_record(&record_text, &digest).map_err(|e| Fail::exit(3, e))?;
        let record_digest = crate::measure::sha256(&record_text);
        Ok(Self {
            path,
            source: source.to_ascii_lowercase(),
            digest,
            record,
            record_digest,
            artifact,
        })
    }
}

pub(crate) fn builds(args: &mut Vec<String>) -> Result<[Build; 2], Fail> {
    Ok([Build::parse(args, "a")?, Build::parse(args, "b")?])
}

pub(crate) fn bounded_wire(inst: &Instance) -> Result<Wire, String> {
    Ok(inst.connect()?.bounded(HANG))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Timing,
    MemorySizing,
}

fn parse(args: &[String], purpose: Purpose) -> Result<Options, Fail> {
    let mut args = args.to_vec();
    let builds = builds(&mut args)?;
    let memory_record = take_value(&mut args, "--comparison-memory-record")?.map(PathBuf::from);
    let memory_bindings = take_flag(&mut args, "--memory-bindings");
    if purpose == Purpose::Timing && memory_record.is_none() && !memory_bindings {
        return Err(Fail::usage(
            "--comparison-memory-record is required (evidenced whole-workload peak)",
        ));
    }
    if purpose == Purpose::MemorySizing && (memory_record.is_some() || memory_bindings) {
        return Err(Fail::usage(
            "memory sizing does not accept timing memory records or --memory-bindings",
        ));
    }
    let out = PathBuf::from(
        take_value(&mut args, "--out")?.ok_or_else(|| Fail::usage("--out is required"))?,
    );
    if !out.is_absolute() {
        return Err(Fail::usage("--out must be an absolute new directory"));
    }
    let profile = take_value(&mut args, "--profile")?.unwrap_or_else(|| "agent".into());
    let (bytes, gap) = match profile.as_str() {
        "agent" => (256, 20),
        "stress" => (4096, 10),
        _ => return Err(Fail::usage("--profile must be agent or stress")),
    };
    let duration_ms = number(&mut args, "--duration-ms", 20000)?;
    let work = Work {
        profile,
        duration_ms,
        typing_ms: number(&mut args, "--typing-ms", duration_ms.saturating_mul(4) / 5)?,
        block_bytes: usize::try_from(number(&mut args, "--block-bytes", bytes)?)
            .map_err(|_| Fail::usage("block size overflow"))?,
        output_ms: number(&mut args, "--output-interval-ms", gap)?,
        key_ms: number(&mut args, "--key-interval-ms", 40)?,
    };
    work.validate().map_err(Fail::usage)?;
    let cases = take_value(&mut args, "--cases")?
        .unwrap_or_else(|| "1,30,200,split".into())
        .split(',')
        .map(|s| Case::parse(s).map_err(Fail::usage))
        .collect::<Result<Vec<_>, _>>()?;
    if cases
        .iter()
        .enumerate()
        .any(|(i, c)| cases[..i].contains(c))
    {
        return Err(Fail::usage("duplicate case"));
    }
    let quads = usize::try_from(number(&mut args, "--quads", 3)?)
        .map_err(|_| Fail::usage("quad count overflow"))?;
    if !(1..=10).contains(&quads) {
        return Err(Fail::usage("--quads must be 1..10"));
    }
    let keep = take_flag(&mut args, "--keep");
    if !args.is_empty() {
        return Err(Fail::usage(format!("unexpected arguments: {args:?}")));
    }
    Ok(Options {
        builds,
        out,
        work,
        cases,
        quads,
        keep,
        memory_record,
        memory_bindings,
    })
}

pub(crate) fn hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn build_record(text: &str, digest: &str) -> Result<String, String> {
    let mut selected = None;
    for line in text.lines().filter(|s| !s.trim().is_empty()) {
        let row: aterm_json::Value =
            aterm_json::from_str(line).map_err(|e| format!("compiler JSONL: {e}"))?;
        if row.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-artifact")
            || row
                .get("target")
                .and_then(|t| t.get("name"))
                .and_then(aterm_json::Value::as_str)
                != Some("aterm")
            || row
                .get("trust_executable_sha256")
                .and_then(aterm_json::Value::as_str)
                != Some(digest)
        {
            continue;
        }
        let profile = row.get("profile").ok_or("artifact has no profile")?;
        let is_bin = row
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(aterm_json::Value::as_array)
            .is_some_and(|k| k.iter().any(|v| v.as_str() == Some("bin")));
        if profile
            .get("debug_assertions")
            .and_then(aterm_json::Value::as_bool)
            != Some(true)
            || profile.get("test").and_then(aterm_json::Value::as_bool) != Some(false)
            || row
                .get("trust_compile_mode")
                .and_then(aterm_json::Value::as_str)
                != Some("build")
            || row
                .get("trust_compile_kind")
                .and_then(aterm_json::Value::as_str)
                != Some("host")
            || !is_bin
        {
            return Err("matching compiler artifact is not a genuine debug host app build".into());
        }
        selected = Some(line.to_owned());
    }
    selected
        .ok_or_else(|| "compiler record does not bind this app digest to a debug host build".into())
}

#[derive(Default)]
pub(crate) struct Raw(pub(crate) String, pub(crate) Option<String>);

impl Raw {
    pub(crate) fn as_text(&self) -> &str {
        &self.0
    }

    pub(crate) fn note(&mut self, name: &str, value: impl std::fmt::Display) {
        let _ = writeln!(self.0, "\n[{name}]\n{value}");
    }
}

pub(crate) fn ask(wire: &mut Wire, request: &str) -> Result<String, String> {
    let reply = wire
        .request_line(request)
        .map_err(|e| format!("{request}: {e}"))?;
    if !reply.starts_with("OK") {
        return Err(format!("{request}: {reply}"));
    }
    Ok(reply)
}

pub(crate) fn counted(wire: &mut Wire, request: &str) -> Result<String, String> {
    let (head, body) = wire
        .request_counted(request)
        .map_err(|e| format!("{request}: {e}"))?;
    if !head.starts_with("OK") {
        return Err(format!("{request}: {head}"));
    }
    Ok(format!("{head}\n{body}"))
}

pub(crate) fn feed(wire: &mut Wire, sid: &str, payload: &[u8]) -> Result<(), String> {
    let reply = wire
        .request_frame(&format!("@{sid} feed-bin"), payload)
        .map_err(|e| e.to_string())?;
    if reply.trim() != format!("OK {} bytes", payload.len()) {
        return Err(format!("feed acknowledgement: {reply}"));
    }
    Ok(())
}

pub(crate) fn nap(d: Duration) -> Result<(), String> {
    stop::sleep(d).map_err(|s| format!("interrupted by signal {}", s.0))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) local: u64,
    pub(crate) sid: String,
    pub(crate) window: String,
    pub(crate) title: String,
    pub(crate) active: bool,
    pub(crate) program: String,
    pub(crate) focused: bool,
}

pub(crate) fn sessions(text: &str) -> Result<Vec<Session>, String> {
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let words: Vec<_> = line.split_whitespace().collect();
            let f = fields(line);
            if words.len() < 5
                || words[3] != "alive"
                || crate::stub::first_sid(words[1]) != Some(words[1])
            {
                return Err(format!("not a live terminal roster row: {line}"));
            }
            let window = f
                .get("window")
                .filter(|w| w.parse::<u64>().is_ok())
                .ok_or_else(|| format!("no numeric window: {line}"))?;
            Ok(Session {
                local: words[0]
                    .parse()
                    .map_err(|_| format!("invalid local session: {line}"))?,
                sid: words[1].into(),
                window: window.clone(),
                title: words[4].into(),
                active: f.get("active").map(String::as_str) == Some("1"),
                program: f.get("program").cloned().unwrap_or_default(),
                focused: f.get("wfocus").map(String::as_str) == Some("1"),
            })
        })
        .collect()
}

pub(crate) fn roster(wire: &mut Wire) -> Result<Vec<Session>, String> {
    sessions(&counted(wire, "sessions")?)
}

pub(crate) fn await_roster(wire: &mut Wire, n: usize) -> Result<Vec<Session>, String> {
    let start = Instant::now();
    loop {
        if let Ok(rows) = roster(wire)
            && rows.len() == n
        {
            return Ok(rows);
        }
        if start.elapsed() >= HANG {
            return Err(format!("raw-cat roster did not reach {n} sessions"));
        }
        nap(Duration::from_millis(100))?;
    }
}

struct Targets {
    source: Session,
    typing: Session,
    rows: Vec<Session>,
    native_window: String,
    native_windows: BTreeMap<String, String>,
    topology: Topology,
}

#[derive(Debug, PartialEq, Eq)]
struct Topology(Vec<WindowTree>);

#[derive(Debug, PartialEq, Eq)]
struct WindowTree {
    id: u64,
    focused: bool,
    declared_tabs: usize,
    active: u64,
    tabs: Vec<TabTree>,
}

#[derive(Debug, PartialEq, Eq)]
struct TabTree {
    id: u64,
    kind: String,
    index: usize,
    active: bool,
    parked: bool,
    splits: Vec<String>,
    leaves: Vec<LeafTree>,
}

#[derive(Debug, PartialEq, Eq)]
struct LeafTree {
    view: u64,
    session: u64,
    focused: bool,
    path: String,
}

fn tree_number<T: std::str::FromStr>(line: &str, key: &str) -> Result<T, String> {
    fields(line)
        .get(key)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("topology lacks numeric {key}: {line}"))
}

fn tree_flag(line: &str, key: &str) -> Result<bool, String> {
    match fields(line).get(key).map(String::as_str) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(format!("topology lacks boolean {key}: {line}")),
    }
}

fn topology(
    text: &str,
    case: Case,
    rows: &[Session],
    source: &Session,
    typing: &Session,
) -> Result<Topology, String> {
    let mut windows: Vec<WindowTree> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("OK ") || line.starts_with("app-inspection ") {
            continue;
        }
        if line.starts_with("window ") {
            windows.push(WindowTree {
                id: tree_number(line, "id")?,
                focused: tree_flag(line, "focused")?,
                declared_tabs: tree_number(line, "tabs")?,
                active: tree_number(line, "active")?,
                tabs: Vec::new(),
            });
        } else if line.starts_with("tab ") {
            // Titles are quoted free text; never parse apparent fields inside them.
            let prefix = line
                .split_once(" title=")
                .ok_or("topology tab lacks title boundary")?
                .0;
            let suffix = line
                .rsplit_once(" state=")
                .ok_or("topology tab lacks state boundary")?
                .1;
            let kind = fields(prefix)
                .get("kind")
                .cloned()
                .ok_or("topology tab kind missing")?;
            let tab = TabTree {
                id: tree_number(prefix, "id")?,
                index: tree_number(prefix, "index")?,
                kind,
                active: tree_flag(suffix, "active")?,
                parked: tree_flag(suffix, "parked")?,
                splits: Vec::new(),
                leaves: Vec::new(),
            };
            windows
                .last_mut()
                .ok_or("tab outside window")?
                .tabs
                .push(tab);
        } else if line.starts_with("split ") || line.starts_with("leaf ") {
            let tab = windows
                .last_mut()
                .and_then(|w| w.tabs.last_mut())
                .ok_or("tree outside tab")?;
            if line.starts_with("split ") {
                let f = fields(line);
                let path = f.get("path").ok_or("split path missing")?;
                let axis = f.get("axis").ok_or("split axis missing")?;
                tab.splits.push(format!("{path}:{axis}"));
            } else {
                let f = fields(line);
                if f.get("kind").map(String::as_str) != Some("terminal") {
                    return Err("topology contains a native or missing leaf".into());
                }
                tab.leaves.push(LeafTree {
                    view: tree_number(line, "view")?,
                    session: tree_number(line, "session")?,
                    focused: tree_flag(line, "focused")?,
                    path: f.get("path").cloned().ok_or("leaf path missing")?,
                });
            }
        } else {
            return Err(format!("unrecognized topology row: {line}"));
        }
    }
    let source_window = source
        .window
        .parse::<u64>()
        .map_err(|_| "source window invalid")?;
    let typing_window = typing
        .window
        .parse::<u64>()
        .map_err(|_| "typing window invalid")?;
    let expected_windows = match case {
        Case::Split => 1,
        Case::Tabs(_) => 2,
        Case::Windows(t) => t.windows,
    };
    if matches!(case, Case::Windows(_)) && source_window != typing_window {
        return Err("workload tabs must share the first OS window".into());
    }
    if windows.len() != expected_windows {
        return Err("unexpected number of windows".into());
    }
    let mut window_ids = BTreeSet::new();
    let mut tab_ids = BTreeSet::new();
    let mut views = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    let mut typing_placed = false;
    for window in &windows {
        if !window_ids.insert(window.id)
            || !rows
                .iter()
                .any(|r| r.window.parse::<u64>() == Ok(window.id))
            || window.focused != (window.id == typing_window)
        {
            return Err("wrong or duplicate focused window".into());
        }
        let expected_tabs = match case {
            Case::Split => 1,
            Case::Windows(_) => {
                if window.id == source_window {
                    2
                } else {
                    WindowTopology::IDLE_TABS
                }
            }
            Case::Tabs(n) => {
                if window.id == source_window {
                    1
                } else {
                    n
                }
            }
        };
        if window.tabs.len() != expected_tabs || window.declared_tabs != expected_tabs {
            return Err("window has wrong tab count".into());
        }
        if window.tabs.iter().filter(|t| t.active).count() != 1 {
            return Err("window has no unique active tab".into());
        }
        for (index, tab) in window.tabs.iter().enumerate() {
            if !tab_ids.insert(tab.id)
                || tab.id == 0
                || tab.index != index + 1
                || tab.parked
                || tab.active != (tab.id == window.active)
            {
                return Err("invalid tab identity, order or active selection".into());
            }
            let expected_leaves = if case == Case::Split { 2 } else { 1 };
            let expected_kind = if case == Case::Split {
                "mixed"
            } else {
                "terminal"
            };
            if tab.kind != expected_kind {
                return Err("unexpected topology tab kind".into());
            }
            if tab.leaves.len() != expected_leaves
                || tab.leaves.iter().filter(|l| l.focused).count() != 1
            {
                return Err("tab has wrong leaf count or focus".into());
            }
            if case == Case::Split {
                if tab.splits != ["/:horizontal"]
                    || tab
                        .leaves
                        .iter()
                        .map(|l| l.path.as_str())
                        .collect::<Vec<_>>()
                        != ["/0", "/1"]
                {
                    return Err("split control is not one horizontal two-leaf tab".into());
                }
            } else if !tab.splits.is_empty() || tab.leaves[0].path != "/" {
                return Err("unrelated tab contains a split".into());
            }
            for leaf in &tab.leaves {
                if leaf.view == 0
                    || !views.insert(leaf.view)
                    || !sessions.insert(leaf.session)
                    || !rows.iter().any(|r| {
                        r.local == leaf.session && r.window.parse::<u64>() == Ok(window.id)
                    })
                {
                    return Err("leaf identity/session does not match the roster".into());
                }
                if matches!(case, Case::Windows(_)) && leaf.session == source.local && tab.active {
                    return Err("source must stay behind the typing tab".into());
                }
                if leaf.session == source.local && window.id != source_window {
                    return Err("source leaf moved windows".into());
                }
                if leaf.session == typing.local {
                    typing_placed =
                        window.id == typing_window && window.focused && tab.active && leaf.focused;
                }
            }
        }
    }
    if sessions.len() != rows.len() || !sessions.contains(&source.local) || !typing_placed {
        return Err("source or focused typing leaf missing from topology".into());
    }
    Ok(Topology(windows))
}

/// Observe each real key window once outside measurement, then restore the typing tab.
/// Distinct logical roots must not alias one native window. No native APIs run in tests.
fn native_windows(
    wire: &mut Wire,
    rows: &[Session],
    typing: &Session,
    raw: &mut Raw,
    phase: &str,
) -> Result<BTreeMap<String, String>, String> {
    let mut observed = BTreeMap::new();
    for row in rows.iter().filter(|row| row.active) {
        if observed.contains_key(&row.window) {
            continue;
        }
        let reply = raise_for_setup(wire, row)?;
        raw.note(&format!("{phase}_{}", row.window), &reply);
        let native = fields(&reply)
            .get("window")
            .cloned()
            .ok_or("native window missing")?;
        observed.insert(row.window.clone(), native);
    }
    validate_native_windows(rows, &observed)?;
    raw.note(
        &format!("{phase}_restore_typing"),
        raise_for_setup(wire, typing)?,
    );
    Ok(observed)
}

fn validate_native_windows(
    rows: &[Session],
    observed: &BTreeMap<String, String>,
) -> Result<(), String> {
    let logical: BTreeSet<_> = rows.iter().map(|row| row.window.clone()).collect();
    let native = observed
        .values()
        .map(|id| {
            id.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or("invalid native window identity")
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if observed.keys().cloned().collect::<BTreeSet<_>>() != logical || native.len() != logical.len()
    {
        return Err("logical OS windows lack distinct positive native identities".into());
    }
    Ok(())
}

fn read_topology(
    wire: &mut Wire,
    case: Case,
    targets: (&[Session], &Session, &Session),
    raw: &mut Raw,
    phase: &str,
) -> Result<Topology, String> {
    let text = counted(wire, "inspect app/v1 tabs")?;
    raw.note(phase, &text);
    topology(&text, case, targets.0, targets.1, targets.2)
}

/// The launched producer and every cat must be observed before setup freezes metadata.
fn setup_roster_ready(
    expected: &[Session],
    current: &[Session],
    source: &Session,
    typing: &Session,
    producer_program: &str,
) -> bool {
    expected.len() == current.len()
        && expected.iter().all(|old| {
            current
                .iter()
                .any(|new| old.sid == new.sid && old.local == new.local && old.window == new.window)
        })
        && current
            .iter()
            .any(|row| row.sid == source.sid && row.program == producer_program)
        && current
            .iter()
            .all(|row| row.sid == source.sid || row.program == "cat")
        && current
            .iter()
            .any(|row| row.sid == typing.sid && row.active)
}

/// Create the canonical session/window topology before starting any workload process.
fn place_sessions(wire: &mut Wire, case: Case) -> Result<(Session, Session, Vec<Session>), String> {
    let source = await_roster(wire, 1)?.remove(0);
    match case {
        Case::Tabs(_) => {
            ask(wire, "invoke NewWindow")?;
        }
        Case::Split => {
            ask(wire, "spawn split=v")?;
        }
        Case::Windows(_) => {
            ask(wire, &format!("spawn window={} raise=false", source.window))?;
        }
    }
    let initial = await_roster(wire, 2)?;
    let typing = initial
        .iter()
        .find(|s| s.sid != source.sid)
        .ok_or("new typing session missing")?
        .clone();
    if let Case::Tabs(n) = case {
        if source.window == typing.window {
            return Err("NewWindow did not create a second window".into());
        }
        for _ in 1..n {
            ask(wire, &format!("spawn window={} raise=false", typing.window))?;
        }
    } else if let Case::Windows(t) = case {
        if source.window != typing.window {
            return Err("workload tabs did not share one window".into());
        }
        let mut total = 2;
        for _ in 1..t.windows {
            let before = await_roster(wire, total)?;
            ask(wire, "invoke NewWindow")?;
            let after = await_roster(wire, total + 1)?;
            let idle = after
                .iter()
                .find(|row| !before.iter().any(|old| old.sid == row.sid))
                .ok_or("new unrelated window has no session")?;
            if before.iter().any(|old| old.window == idle.window) {
                return Err("NewWindow reused an existing OS window".into());
            }
            for _ in 1..WindowTopology::IDLE_TABS {
                ask(wire, &format!("spawn window={} raise=false", idle.window))?;
            }
            total += WindowTopology::IDLE_TABS;
        }
    } else if source.window != typing.window {
        return Err("split moved to another window".into());
    }
    let n = match case {
        Case::Tabs(n) => n + 1,
        Case::Split => 2,
        Case::Windows(t) => t.sessions(),
    };
    let rows = await_roster(wire, n)?;
    let expected_source = if matches!(case, Case::Split | Case::Windows(_)) {
        2
    } else {
        1
    };
    let expected_typing = match case {
        Case::Tabs(n) => n,
        Case::Split | Case::Windows(_) => 2,
    };
    if rows.iter().filter(|s| s.window == source.window).count() != expected_source
        || rows.iter().filter(|s| s.window == typing.window).count() != expected_typing
    {
        return Err("session counts do not match the requested window topology".into());
    }
    Ok((source, typing, rows))
}

fn setup(
    wire: &mut Wire,
    case: Case,
    files: &producer::Files,
    raw: &mut Raw,
) -> Result<Targets, String> {
    let (source, typing, rows) = place_sessions(wire, case)?;
    let lab = std::fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let producer_program = lab
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or("producer executable has no UTF-8 basename")?;
    for row in &rows {
        let command = if row.sid == source.sid {
            format!(
                "stty raw -echo && exec {} __title-drift-producer {}\n",
                crate::util::shell_quote(&lab.to_string_lossy()),
                crate::util::shell_quote(&files.dir.to_string_lossy())
            )
        } else {
            format!("{PROGRAM}\n")
        };
        feed(wire, &row.sid, command.as_bytes())?;
    }
    raw.note("producer_ready", files.wait("ready")?);
    ask(wire, &format!("raise {}", typing.sid))?;
    let start = Instant::now();
    let mut stable_since = None;
    let current = loop {
        let current = roster(wire)?;
        let ready = setup_roster_ready(&rows, &current, &source, &typing, producer_program);
        if ready {
            if stable_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(3) {
                break current;
            }
        } else {
            stable_since = None;
        }
        if start.elapsed() >= HANG {
            return Err(
                "producer and typing/idle cats did not settle in the expected topology".into(),
            );
        }
        nap(Duration::from_millis(100))?;
    };
    let native_windows = native_windows(wire, &current, &typing, raw, "native_windows_setup")?;
    let topology = read_topology(
        wire,
        case,
        (&current, &source, &typing),
        raw,
        "topology_setup",
    )?;
    raw.note("roster", counted(wire, "sessions")?);
    let native = focus(wire, &typing, None)?;
    let native_window = fields(&native)
        .get("window")
        .cloned()
        .ok_or("native window missing")?;
    raw.note("native_focus_setup", &native);
    raw.note(
        "window_binding",
        format!(
            "logical={} native={native_window} typing={}",
            typing.window, typing.sid
        ),
    );
    Ok(Targets {
        source,
        typing,
        rows: current,
        native_window,
        native_windows,
        topology,
    })
}

pub(crate) fn validate_roster(
    before: &[Session],
    after: &[Session],
    typing: &Session,
) -> Result<(), String> {
    if before.len() != after.len()
        || before.iter().any(|old| {
            !after.iter().any(|new| {
                old.sid == new.sid
                    && old.local == new.local
                    && old.window == new.window
                    && old.title == new.title
                    && old.program == new.program
            })
        })
        || !after
            .iter()
            .any(|s| s.sid == typing.sid && s.active && s.focused)
    {
        return Err(format!(
            "session membership, title, placement or typing selection changed; \
             expected_before={before:?}; observed_after={after:?}; expected_typing={typing:?}"
        ));
    }
    Ok(())
}

pub(crate) fn native_focus_ok(reply: &str, window: &str) -> bool {
    let f = fields(reply);
    ["available", "app_active", "window_key", "key"]
        .iter()
        .all(|k| f.get(*k).map(String::as_str) == Some("1"))
        && f.get("window").map(String::as_str) == Some(window)
}

enum FocusState {
    Ready(String),
    Pending(String),
}

fn focus_state(
    wire: &mut Wire,
    typing: &Session,
    expected: Option<&str>,
) -> Result<FocusState, String> {
    let selected = |rows: Vec<Session>| {
        rows.iter()
            .any(|s| s.sid == typing.sid && s.window == typing.window && s.active && s.focused)
    };
    if !selected(roster(wire)?) {
        return Ok(FocusState::Pending(
            "NOT_RUN typing session is not in the focused window".into(),
        ));
    }
    let reply = ask(wire, "native-focus")?;
    let f = fields(&reply);
    let observed = f
        .get("window")
        .filter(|w| w.parse::<u64>().is_ok_and(|n| n > 0));
    let window = expected.or(observed.map(String::as_str));
    if window.is_some_and(|w| native_focus_ok(&reply, w)) && selected(roster(wire)?) {
        Ok(FocusState::Ready(reply))
    } else {
        Ok(FocusState::Pending(format!(
            "NOT_RUN native focus unavailable or wrong window: {reply}"
        )))
    }
}

pub(crate) fn focus(
    wire: &mut Wire,
    typing: &Session,
    expected: Option<&str>,
) -> Result<String, String> {
    match focus_state(wire, typing, expected)? {
        FocusState::Ready(reply) => Ok(reply),
        FocusState::Pending(why) => Err(why),
    }
}

/// Request focus once during fixture setup, then observe its asynchronous completion.
/// No input or timing starts before both the logical selection and native key window agree.
pub(crate) fn raise_for_setup(wire: &mut Wire, typing: &Session) -> Result<String, String> {
    ask(wire, &format!("raise {}", typing.sid))?;
    let began = Instant::now();
    loop {
        match focus_state(wire, typing, None)? {
            FocusState::Ready(reply) => return Ok(reply),
            FocusState::Pending(why) if began.elapsed() >= HANG => {
                return Err(format!("{why}; fixture focus hang detector expired"));
            }
            FocusState::Pending(_) => nap(Duration::from_millis(20))?,
        }
    }
}

pub(crate) fn screen(wire: &mut Wire, sid: &str) -> Result<String, String> {
    let (head, text) = wire
        .request_counted(&format!("@{sid} text"))
        .map_err(|e| e.to_string())?;
    if !head.starts_with("OK") {
        return Err(format!("text: {head}"));
    }
    Ok(text)
}

pub(crate) fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

pub(crate) fn await_text(
    wire: &mut Wire,
    sid: &str,
    wanted: &str,
    exact: bool,
) -> Result<String, String> {
    let start = Instant::now();
    loop {
        let text = screen(wire, sid)?;
        if if exact {
            compact(&text) == wanted
        } else {
            text.contains(wanted)
        } {
            return Ok(text);
        }
        if start.elapsed() >= HANG {
            return Err(format!("expected text never arrived for {sid}: {text:?}"));
        }
        nap(Duration::from_millis(10))?;
    }
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) status: String,
    pub(crate) dims: String,
    pub(crate) resizes: String,
    pub(crate) drift: String,
    pub(crate) title: String,
}

pub(crate) fn snapshot(
    wire: &mut Wire,
    sid: &str,
    label: &str,
    raw: &mut Raw,
) -> Result<Snapshot, String> {
    let s = Snapshot {
        status: ask(wire, &format!("@{sid} status"))?,
        dims: ask(wire, &format!("@{sid} dims"))?,
        resizes: counted(wire, &format!("@{sid} resizes"))?,
        drift: counted(wire, &format!("@{sid} cast drift"))?,
        title: ask(wire, &format!("@{sid} title"))?,
    };
    raw.note(&format!("{label}_status"), &s.status);
    raw.note(&format!("{label}_dims"), &s.dims);
    raw.note(&format!("{label}_resizes"), &s.resizes);
    raw.note(&format!("{label}_drift"), &s.drift);
    raw.note(&format!("{label}_title"), &s.title);
    clean_snapshot(&s)?;
    Ok(s)
}

pub(crate) fn clean_snapshot(s: &Snapshot) -> Result<(), String> {
    let r = fields(s.resizes.lines().next().unwrap_or_default());
    let d = fields(s.drift.lines().next().unwrap_or_default());
    if r.get("render").map(String::as_str) != Some("ok")
        || r.get("risks").map(String::as_str) != Some("0")
        || d.get("verdict").map(String::as_str) != Some("clean")
        || d.get("fidelity").map(String::as_str) != Some("pass")
    {
        return Err("render ledger or cast drift is not clean".into());
    }
    Ok(())
}

pub(crate) fn stable_snapshot(before: &Snapshot, after: &Snapshot) -> Result<(), String> {
    clean_snapshot(after)?;
    let a = fields(&before.status);
    let b = fields(&after.status);
    for key in ["render", "resizes", "flaps"] {
        if !a.contains_key(key) || a.get(key) != b.get(key) {
            return Err(format!("status {key} changed or missing"));
        }
    }
    if geometry(&before.dims)? != geometry(&after.dims)? {
        return Err("terminal or window geometry changed".into());
    }
    if before.resizes != after.resizes || before.title != after.title {
        return Err("resize history or OSC title changed".into());
    }
    Ok(())
}

pub(crate) fn geometry(dims: &str) -> Result<Vec<String>, String> {
    let mut out = dims
        .split_whitespace()
        .skip(1)
        .take(4)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if out.len() != 4 || out.iter().any(|n| !n.parse::<u32>().is_ok_and(|n| n > 0)) {
        return Err("terminal geometry missing".into());
    }
    let f = fields(dims);
    for key in ["window", "window_rows", "window_cols"] {
        out.push(
            f.get(key)
                .cloned()
                .ok_or_else(|| format!("dims missing {key}"))?,
        );
    }
    Ok(out)
}

pub(crate) fn digest(build: &Build, raw: &mut Raw, phase: &str) -> Result<(), String> {
    let got = aterm_update_core::sha256_file(&build.path)?;
    raw.note(&format!("digest_{phase}"), &got);
    eprintln!("title-drift: {phase} {} sha256={got}", build.path.display());
    if aterm_update_core::sha256_file(&build.record)? != build.record_digest {
        return Err("trusted compiler build record changed".into());
    }
    if got == build.digest {
        Ok(())
    } else {
        Err(format!(
            "binary digest changed: expected {}, got {got}",
            build.digest
        ))
    }
}

pub(crate) fn cpu(
    inst: &Instance,
    build: &Build,
    identity: Option<aterm_uds::process::Identity>,
    raw: &mut Raw,
    phase: &str,
) -> Result<(aterm_uds::process::Identity, u64), String> {
    let pid = inst.aterm_pid().ok_or("owned app PID unavailable")?;
    let before = aterm_uds::process::read_identity(pid).ok_or("app identity unavailable")?;
    let path = aterm_uds::process::read_process_path(pid).ok_or("app path unavailable")?;
    let value = crate::sys::process_cpu_ns(pid).ok_or("whole-app CPU counter unavailable")?;
    let after = aterm_uds::process::read_identity(pid).ok_or("app identity disappeared")?;
    raw.note(
        &format!("cpu_{phase}"),
        format!(
            "identity={} path={} cpu_ns={value}",
            before.token(),
            path.display()
        ),
    );
    if before != after
        || identity.is_some_and(|id| id != before)
        || path != build.path
        || inst.aterm_pid() != Some(pid)
    {
        return Err("owned app PID, birth/exec identity or executable path changed".into());
    }
    Ok((before, value))
}

pub(crate) fn post(wire: &mut Wire, keys: u64, key_ms: u64) -> Result<String, String> {
    // hwkey's intentional burst wait is not a named timeout on the wire.
    wire.bound_by(HANG + Duration::from_millis(keys * key_ms));
    let result = ask(
        wire,
        &format!("hwkey a count={keys} interval={key_ms} shape=press"),
    );
    wire.bound_by(HANG);
    let reply = result?;
    let f = fields(&reply);
    if f.get("posted").and_then(|s| s.parse::<u64>().ok()) != Some(keys)
        || f.get("key").map(String::as_str) != Some("1")
    {
        return Err(format!("native key posting refused: {reply}"));
    }
    Ok(reply)
}

#[derive(Debug)]
pub(crate) struct Metrics {
    pub(crate) n: u64,
    pub(crate) p50: f64,
    pub(crate) p95: f64,
    pub(crate) p99: f64,
}

pub(crate) fn metrics(reply: &str, keys: u64) -> Result<Metrics, String> {
    let f = fields(reply);
    let count = |key: &str| {
        f.get(key)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| format!("missing {key}"))
    };
    let n = count("n_input")?;
    let number = |k: &str| {
        f.get(k)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| format!("missing finite {k}"))
    };
    let m = Metrics {
        n,
        p50: number("input_p50_ms")?,
        p95: number("input_p95_ms")?,
        p99: number("input_p99_ms")?,
    };
    if n != keys || m.p50 > m.p95 || m.p95 > m.p99 {
        return Err("native input count or percentile ordering is invalid".into());
    }
    // A complete dispatch-to-present histogram can still omit the native queue wait.
    // These lanes report native input latency only when every key retained that clock
    // and reached a real presentation without a lost edge or a prediction closing it.
    for (counter, expected) in [
        ("n_key_queue", keys),
        ("key_queue_unavailable", 0),
        ("input_edges_dropped", 0),
        ("input_predicted", 0),
    ] {
        if count(counter)? != expected {
            return Err(format!(
                "native input clock evidence is incomplete: {counter}"
            ));
        }
    }
    Ok(m)
}

/// Global presentation observations are diagnostics, not source-specific visible output.
fn present_metrics(reply: &str) -> Result<Metrics, String> {
    let f = fields(reply);
    let n = f
        .get("n_present")
        .and_then(|n| n.parse().ok())
        .ok_or("missing n_present")?;
    let number = |key: &str| {
        f.get(key)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| format!("missing finite {key}"))
    };
    let m = Metrics {
        n,
        p50: number("present_p50_ms")?,
        p95: number("present_p95_ms")?,
        p99: number("present_p99_ms")?,
    };
    if m.p50 > m.p95 || m.p95 > m.p99 {
        return Err("present percentile ordering is invalid".into());
    }
    Ok(m)
}

struct Taken {
    cpu_ns: u64,
    elapsed_ns: u128,
    bytes: u64,
    metrics: Metrics,
    present: Metrics,
    key_start_ns: u64,
    key_end_ns: u64,
    first_output_ns: u64,
    last_output_ns: u64,
    cast: CastEvidence,
}

struct CastEvidence {
    bytes: usize,
    events: usize,
    span_ms: f64,
    truncated: bool,
}

fn cast_evidence(text: &str, work: &Work) -> Result<CastEvidence, String> {
    let head: aterm_json::Value = aterm_json::from_str(text.lines().next().ok_or("empty cast")?)
        .map_err(|e| e.to_string())?;
    let truncated = head
        .get("aterm_truncated")
        .and_then(|v| v.get("evicted_events"))
        .and_then(aterm_json::Value::as_u64)
        .is_some_and(|n| n > 0);
    let cast = crate::halfdraw::parse_cast(text)?;
    let all = cast
        .events
        .iter()
        .map(|e| e.data.as_str())
        .collect::<String>();
    let (block, last) = work.payloads();
    let mut expected = Vec::new();
    for i in 0..work.blocks() {
        expected.extend_from_slice(if i + 1 == work.blocks() {
            &last
        } else {
            &block
        });
    }
    let bytes = expected
        .iter()
        .rev()
        .zip(all.as_bytes().iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if bytes == 0 || (!truncated && bytes != expected.len()) {
        return Err("cast does not retain the complete untruncated output corpus".into());
    }
    let from = all.len() - bytes;
    let mut offset = 0;
    let mut times = Vec::new();
    for event in &cast.events {
        offset += event.data.len();
        if offset > from {
            times.push(event.at);
        }
    }
    let first = *times.first().ok_or("cast has no output events")?;
    let last = *times.last().ok_or("cast has no final output event")?;
    if times.windows(2).any(|p| p[1] < p[0]) || last <= first {
        return Err("cast has no chronological output spread".into());
    }
    if !truncated {
        let expected_span = (work.duration_ms - work.output_ms) as f64 / 1000.0;
        let mut quarters = [false; 4];
        for at in &times {
            let fraction = ((at - first) / expected_span).clamp(0.0, 1.0);
            quarters[((fraction * 4.0) as usize).min(3)] = true;
        }
        if quarters.iter().any(|seen| !seen) {
            return Err("complete cast lacks output across the workload's four quarters".into());
        }
    }
    Ok(CastEvidence {
        bytes,
        events: times.len(),
        span_ms: (last - first) * 1000.0,
        truncated,
    })
}

struct PreparedFixture {
    typing: Wire,
    source: Wire,
    t: Targets,
    before: Vec<Snapshot>,
}

fn prepare_fixture(
    inst: &Instance,
    build: &Build,
    case: Case,
    work: &Work,
    files: &producer::Files,
    raw: &mut Raw,
) -> Result<PreparedFixture, String> {
    let mut typing = bounded_wire(inst)?;
    let mut source = bounded_wire(inst)?;
    let t = setup(&mut typing, case, files, raw)?;
    feed(&mut typing, &t.typing.sid, CLEAR)?;
    await_text(&mut typing, &t.typing.sid, "", true)?;
    post(&mut typing, 8, work.key_ms)?;
    await_text(&mut typing, &t.typing.sid, "aaaaaaaa", true)?;
    await_text(&mut source, &t.source.sid, "warm source", false)?;
    files.gate("clear")?;
    raw.note("producer_armed", files.wait("armed")?);
    feed(&mut typing, &t.typing.sid, CLEAR)?;
    await_text(&mut source, &t.source.sid, "", true)?;
    await_text(&mut typing, &t.typing.sid, "", true)?;
    nap(Duration::from_millis(500))?;
    let before = t
        .rows
        .iter()
        .map(|row| {
            snapshot(
                &mut source,
                &row.sid,
                &format!("session_{}_before", row.local),
                raw,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_roster(&t.rows, &roster(&mut typing)?, &t.typing)?;
    if read_topology(
        &mut typing,
        case,
        (&t.rows, &t.source, &t.typing),
        raw,
        "topology_before",
    )? != t.topology
    {
        return Err("window/tab/view topology changed during setup".into());
    }
    raw.note(
        "native_focus_before",
        focus(&mut typing, &t.typing, Some(&t.native_window))?,
    );
    raw.note("metrics_reset", ask(&mut typing, "metrics reset")?);
    digest(build, raw, "entry")?;
    Ok(PreparedFixture {
        typing,
        source,
        t,
        before,
    })
}

fn exercise_fixture(
    inst: &Instance,
    build: &Build,
    case: Case,
    work: &Work,
    files: &producer::Files,
    prepared: PreparedFixture,
    raw: &mut Raw,
) -> Result<Taken, String> {
    let PreparedFixture {
        mut typing,
        mut source,
        t,
        before,
    } = prepared;
    let (identity, cpu_before) = cpu(inst, build, None, raw, "before")?;
    let start = Instant::now();
    files.gate("start")?;
    let started = files.wait("started.json")?;
    raw.note("producer_started", &started);
    let started: aterm_json::Value = aterm_json::from_str(&started).map_err(|e| e.to_string())?;
    let started_at = started
        .get("first_output_monotonic_ns")
        .and_then(aterm_json::Value::as_u64)
        .ok_or("producer first-output clock missing")?;
    let key_start_ns = crate::sys::monotonic_ns();
    let key_result = post(&mut typing, work.keys(), work.key_ms);
    let key_end_ns = crate::sys::monotonic_ns();
    raw.note(
        "key_rpc_clock_monotonic_ns",
        format!("start={key_start_ns} end={key_end_ns}"),
    );
    raw.note("native_post", key_result?);
    let producer_done = files.wait("done.json")?;
    raw.note("producer_done", &producer_done);
    let (first_output_ns, last_output_ns) = producer::validate(&producer_done, work)?;
    if first_output_ns != started_at {
        return Err("producer first-output receipts disagree".into());
    }
    enclosed(first_output_ns, last_output_ns, key_start_ns, key_end_ns)?;
    raw.note(
        "source_final_text",
        await_text(&mut source, &t.source.sid, MARKER, false)?,
    );
    raw.note(
        "typing_final_text",
        await_text(
            &mut typing,
            &t.typing.sid,
            &"a".repeat(usize::try_from(work.keys()).unwrap_or(0)),
            true,
        )?,
    );
    let metric_reply = ask(&mut typing, "metrics percentiles")?;
    raw.note("metrics", &metric_reply);
    let measured = metrics(&metric_reply, work.keys())?;
    let present = present_metrics(&metric_reply)?;
    let (_, cpu_after) = cpu(inst, build, Some(identity), raw, "after")?;
    let elapsed_ns = start.elapsed().as_nanos();
    let cpu_ns = cpu_after
        .checked_sub(cpu_before)
        .ok_or("app CPU counter reset")?;
    // These reads occur after the CPU interval and cannot create earlier cast timestamps.
    raw.note("metrics_summary", ask(&mut typing, "metrics")?);
    let (cast_head, cast_bytes) = source
        .request_bytes(&format!("@{} cast", t.source.sid))
        .map_err(|e| e.to_string())?;
    if !cast_head.starts_with("OK") {
        return Err(format!("source cast: {cast_head}"));
    }
    let cast_text = String::from_utf8(cast_bytes).map_err(|e| e.to_string())?;
    let cast_result = cast_evidence(&cast_text, work);
    raw.1 = Some(cast_text);
    let cast = cast_result?;
    raw.note(
        "source_cast_evidence",
        format!(
            "retained_corpus_bytes={} events={} span_ms={} truncated={}",
            cast.bytes, cast.events, cast.span_ms, cast.truncated
        ),
    );
    raw.note(
        "native_focus_after",
        focus(&mut typing, &t.typing, Some(&t.native_window))?,
    );
    validate_roster(&t.rows, &roster(&mut typing)?, &t.typing)?;
    if read_topology(
        &mut typing,
        case,
        (&t.rows, &t.source, &t.typing),
        raw,
        "topology_after",
    )? != t.topology
    {
        return Err("window/tab/view topology changed during measurement".into());
    }
    if native_windows(&mut typing, &t.rows, &t.typing, raw, "native_windows_after")?
        != t.native_windows
    {
        return Err("native OS-window identities changed".into());
    }
    for (row, before) in t.rows.iter().zip(&before) {
        let after = snapshot(
            &mut source,
            &row.sid,
            &format!("session_{}_after", row.local),
            raw,
        )?;
        stable_snapshot(before, &after)?;
    }
    digest(build, raw, "completion")?;
    Ok(Taken {
        cpu_ns,
        elapsed_ns,
        bytes: work.blocks() * u64::try_from(work.block_bytes).unwrap_or(0),
        metrics: measured,
        present,
        key_start_ns,
        key_end_ns,
        first_output_ns,
        last_output_ns,
        cast,
    })
}

fn drive(
    arm: crate::timing_comparison::Arm<'_>,
    build: &Build,
    case: Case,
    work: &Work,
    files: &producer::Files,
    raw: &mut Raw,
) -> Result<Taken, String> {
    arm.run(
        raw,
        |inst, raw| prepare_fixture(inst, build, case, work, files, raw),
        |inst, prepared, raw| exercise_fixture(inst, build, case, work, files, prepared, raw),
    )
}

fn enclosed(first: u64, last: u64, key_start: u64, key_end: u64) -> Result<(), String> {
    if first == 0 || first >= last || key_start < first || key_end <= key_start || key_end > last {
        return Err(
            "the complete native posting interval was not enclosed by actual output sends".into(),
        );
    }
    Ok(())
}

fn fixture_world(
    build: &Build,
    work: &Work,
    keep: bool,
    raw: &mut Raw,
) -> Result<(World, producer::Files, Launch), String> {
    digest(build, raw, "before_launch")?;
    raw.note("declared_source", &build.source);
    raw.note("binary_path", build.path.display());
    raw.note("trusted_compiler_record_path", build.record.display());
    raw.note("trusted_compiler_record_sha256", &build.record_digest);
    raw.note("trusted_compiler_artifact", &build.artifact);
    let top = Vec::new();
    let config = format!(
        "{}{}",
        instance::config_with(&top),
        crate::linux_proof::DECK_OFF
    );
    raw.note("config", &config);
    let mut world = World::new(&config)?;
    if keep {
        world.keep();
    }
    raw.note("world", world.dir.display());
    let files = producer::Files::create(&world.home, work)?;
    let args: Vec<_> = [
        "--columns",
        "110",
        "--lines",
        "32",
        "-e",
        "/bin/sh",
        "-c",
        LAUNCHER,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let launch = Launch {
        aterm: build.path.clone(),
        headless: false,
        args,
        env: world.env(),
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    };
    Ok((world, files, launch))
}

fn take(
    reservation: &mut crate::timing_comparison::Quad,
    arm: char,
    build: &Build,
    case: Case,
    work: &Work,
    keep: bool,
    raw: &mut Raw,
) -> Result<Taken, String> {
    let (mut world, files, launch) = fixture_world(build, work, keep, raw)?;
    let inst = match reservation.start(&world, launch, &case.label(), arm, raw) {
        Ok(inst) => inst,
        Err(why) => {
            world.keep();
            return Err(why);
        }
    };
    let result = drive(inst, build, case, work, &files, raw);
    if result.is_err() {
        world.keep();
    }
    result
}

pub(crate) fn finish<T>(
    result: Result<T, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
    raw: &mut Raw,
) -> Result<T, String> {
    let cleanup = cleanup();
    raw.note("cleanup", format!("{cleanup:?}"));
    match (result, cleanup) {
        (Ok(t), Ok(())) => Ok(t),
        (Err(why), Ok(())) => Err(why),
        (Ok(_), Err(why)) => Err(format!("instance cleanup failed: {why}")),
        (Err(why), Err(cleanup)) => Err(format!("{why}; cleanup failed: {cleanup}")),
    }
}

pub(crate) fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn csv_row(
    take: usize,
    quad: usize,
    arm: char,
    case: Case,
    build: &Build,
    work: &Work,
    t: &Taken,
) -> String {
    let row = vec![
        take.to_string(),
        quad.to_string(),
        arm.to_string(),
        case.label(),
        csv_field(&case.receipt().to_string()),
        csv_field(&build.path.to_string_lossy()),
        build.source.clone(),
        build.digest.clone(),
        build.record_digest.clone(),
        work.profile.clone(),
        work.duration_ms.to_string(),
        work.typing_ms.to_string(),
        work.block_bytes.to_string(),
        work.output_ms.to_string(),
        work.key_ms.to_string(),
        work.blocks().to_string(),
        t.bytes.to_string(),
        t.cpu_ns.to_string(),
        format!("{:.9}", t.cpu_ns as f64 / t.bytes as f64),
        t.elapsed_ns.to_string(),
        t.metrics.n.to_string(),
        t.metrics.p50.to_string(),
        t.metrics.p95.to_string(),
        t.metrics.p99.to_string(),
        t.present.n.to_string(),
        t.present.p50.to_string(),
        t.present.p95.to_string(),
        t.present.p99.to_string(),
        t.first_output_ns.to_string(),
        t.last_output_ns.to_string(),
        t.key_start_ns.to_string(),
        t.key_end_ns.to_string(),
        t.cast.bytes.to_string(),
        t.cast.events.to_string(),
        t.cast.span_ms.to_string(),
        t.cast.truncated.to_string(),
    ];
    format!("{}\n", row.join(","))
}

fn experiment_binding(
    o: &Options,
    case: Case,
) -> Result<crate::timing_comparison::Experiment, String> {
    crate::timing_comparison::Experiment::freeze(
        &o.builds,
        case.label(),
        format!(
            "topology={} work={:?} cases={:?} keep={}",
            case.receipt(),
            o.work,
            o.cases,
            o.keep
        ),
        o.quads,
    )
}

/// Run the native ABBA experiment, publishing only complete, qualified observations.
/// # Errors
/// Invalid arguments (2), or a refused/incomplete measurement (3).
pub fn cmd(args: &[String]) -> Res {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return Ok(0);
    }
    let o = parse(args, Purpose::Timing)?;
    let experiments = o
        .cases
        .iter()
        .map(|case| experiment_binding(&o, *case).map_err(|why| Fail::exit(3, why)))
        .collect::<Result<Vec<_>, _>>()?;
    if o.memory_bindings {
        for (case, experiment) in o.cases.iter().zip(&experiments) {
            println!(
                "case={} experiment_sha256={} topology={}",
                case.label(),
                experiment.key,
                case.receipt()
            );
        }
        return Ok(0);
    }
    let memory_path = o
        .memory_record
        .as_ref()
        .ok_or_else(|| Fail::usage("missing memory record"))?;
    let memories = experiments
        .iter()
        .map(|experiment| {
            crate::timing_comparison::Memory::read(memory_path.clone(), experiment)
                .map_err(|why| Fail::exit(3, why))
        })
        .collect::<Result<Vec<_>, _>>()?;
    stop::install();
    std::fs::create_dir(&o.out).map_err(|e| Fail::exit(3, format!("new output directory: {e}")))?;
    let mut csv = "take,quad,arm,case,topology_json,binary_path,declared_source_sha,binary_sha256,trusted_build_record_sha256,profile,duration_ms,typing_ms,block_bytes,output_interval_ms,key_interval_ms,blocks,output_bytes,app_cpu_ns,app_cpu_ns_per_byte,elapsed_ns,n_input,input_p50_ms,input_p95_ms,input_p99_ms,n_present,present_p50_ms,present_p95_ms,present_p99_ms,first_output_monotonic_ns,last_output_monotonic_ns,key_rpc_start_monotonic_ns,key_rpc_end_monotonic_ns,source_cast_retained_corpus_bytes,source_cast_events,source_cast_span_ms,source_cast_truncated\n".to_owned();
    let mut index = 0;
    for ((case, memory), experiment) in o.cases.iter().zip(&memories).zip(&experiments) {
        for quad in 1..=o.quads {
            // Admission belongs to exactly this unchanged quad; the next quad requeues.
            let mut reservation = crate::timing_comparison::Quad::reserve(experiment, memory)
                .map_err(|why| Fail::exit(3, why))?;
            for arm in crate::aos_cadence::ORDER {
                index += 1;
                let build = &o.builds[usize::from(arm == 'B')];
                let mut raw = Raw::default();
                raw.note(
                    "workload",
                    format!(
                        "take={index} quad={quad} arm={arm} case={} work={:?}",
                        case.label(),
                        o.work
                    ),
                );
                raw.note("topology_configuration", case.receipt());
                raw.note("memory_record_sha256", &memory.digest);
                raw.note("memory_record", &memory.record);
                for (component, evidence) in &memory.evidence {
                    raw.note(&format!("memory_machine_readings_{component}"), evidence);
                }
                let result = take(
                    &mut reservation,
                    arm,
                    build,
                    *case,
                    &o.work,
                    o.keep,
                    &mut raw,
                );
                raw.note(
                    "result",
                    result
                        .as_ref()
                        .map(|_| "accepted take; run not yet published")
                        .unwrap_or_else(|why| why),
                );
                let path = o.out.join(format!("{index:03}-{arm}-{}.txt", case.label()));
                if let Some(cast) = &raw.1 {
                    crate::util::write_fresh(&path.with_extension("cast"), cast)
                        .map_err(|e| Fail::exit(3, e.to_string()))?;
                }
                crate::util::write_fresh(&path, raw.0).map_err(|e| Fail::exit(3, e.to_string()))?;
                let taken = result.map_err(|why| {
                    Fail::exit(
                        3,
                        format!(
                            "NOT_RUN title-drift take {index}: {why}; raw diagnostics: {}",
                            path.display()
                        ),
                    )
                })?;
                csv.push_str(&csv_row(index, quad, arm, *case, build, &o.work, &taken));
            }
            reservation.complete().map_err(|why| Fail::exit(3, why))?;
        }
    }
    for memory in &memories {
        memory.recheck().map_err(|why| Fail::exit(3, why))?;
    }
    crate::util::write_fresh(&o.out.join("accepted.csv"), &csv)
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    print!("{csv}");
    Ok(0)
}

#[cfg(test)]
#[path = "title_drift_tests.rs"]
mod tests;
