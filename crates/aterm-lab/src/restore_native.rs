// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native cold-restore evidence, with a separate controlled file-wait experiment.
//! The ordinary runs measure the real restore. Injected waits establish responsiveness only.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::band::Band;
use crate::first_key::fields;
use crate::instance::{self, Instance, Launch, World, take_flag, take_value};
use crate::title_drift::{self as common, Build, HANG, Raw, Session, Wire};
use crate::{Fail, Res, stop};

#[path = "restore_native_correctness.rs"]
mod correctness;
#[path = "restore_native_diagnostics.rs"]
mod diagnostics;
#[path = "restore_native_fixture.rs"]
mod fixture;

const HELP: &str = "restore-native --a APP --a-source SHA --a-sha256 SHA256 --a-build-record JSONL \
    --b APP --b-source SHA --b-sha256 SHA256 --b-build-record JSONL --out NEW_DIR \
    [--cases mixed,split,native,editor-first,editor-first-native-window] \
    [--mode natural|wait|both] [--tabs 8] [--leaves 16] \
    [--keys 100] [--key-interval-ms 20] [--wait-ms 1200] [--quads 1] [--keep]\n\
    Requires genuine debug app compiler records and the common restore measurement hooks.\n\
    For B-only native-first correctness, use --correctness-only --help.\n\
    Each take obtains real standalone descriptors with A, composes saved mixed tabs,\n\
    and requires a baseline restore/quit round-trip before restoring with A or B in ABBA order.\n\
    Natural rows measure ordinary restore; wait rows inject a file wait and only prove\n\
    responsiveness. Input percentiles cover the whole burst; overlap counts identify its\n\
    actual posts during restore and never turn whole-burst percentiles into cohort latency.\n\
    Admission is sampled, not continuously quiet. Source labels are declared;\n\
    trusted compiler records bind the supplied executable digests to debug builds.\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Mixed,
    Split,
    Native,
    EditorFirst,
    EditorFirstNativeWindow,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Split => "split",
            Self::Native => "native",
            Self::EditorFirst => "editor-first",
            Self::EditorFirstNativeWindow => "editor-first-native-window",
        }
    }

    fn editor_first(self) -> bool {
        matches!(self, Self::EditorFirst | Self::EditorFirstNativeWindow)
    }

    fn paired(self) -> bool {
        matches!(self, Self::Mixed | Self::Split)
    }

    fn extra_window(self) -> bool {
        self == Self::EditorFirstNativeWindow
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Natural,
    Wait,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Wait => "controlled-wait",
        }
    }
}

struct Options {
    window: RestoreWindow,
    builds: [Build; 2],
    out: PathBuf,
    cases: Vec<Case>,
    modes: Vec<Mode>,
    tabs: usize,
    leaves: usize,
    keys: u64,
    key_ms: u64,
    wait_ms: u64,
    quads: usize,
    keep: bool,
}

fn number(args: &mut Vec<String>, name: &str, default: u64) -> Result<u64, Fail> {
    take_value(args, name)?.map_or(Ok(default), |s| {
        s.parse()
            .map_err(|_| Fail::usage(format!("{name}: expected integer")))
    })
}

fn parse(args: &[String]) -> Result<Options, Fail> {
    let mut args = args.to_vec();
    let builds = common::builds(&mut args)?;
    let out = PathBuf::from(
        take_value(&mut args, "--out")?.ok_or_else(|| Fail::usage("--out is required"))?,
    );
    if !out.is_absolute() {
        return Err(Fail::usage("--out must be an absolute new directory"));
    }
    let cases = take_value(&mut args, "--cases")?
        .unwrap_or_else(|| "mixed,split,native,editor-first,editor-first-native-window".into())
        .split(',')
        .map(|s| match s {
            "mixed" => Ok(Case::Mixed),
            "split" => Ok(Case::Split),
            "native" => Ok(Case::Native),
            "editor-first" => Ok(Case::EditorFirst),
            "editor-first-native-window" => Ok(Case::EditorFirstNativeWindow),
            _ => Err(Fail::usage(
                "--cases accepts mixed,split,native,editor-first,editor-first-native-window",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if cases
        .iter()
        .enumerate()
        .any(|(i, c)| cases[..i].contains(c))
    {
        return Err(Fail::usage("duplicate case"));
    }
    let modes = match take_value(&mut args, "--mode")?
        .as_deref()
        .unwrap_or("both")
    {
        "natural" => vec![Mode::Natural],
        "wait" => vec![Mode::Wait],
        "both" => vec![Mode::Natural, Mode::Wait],
        _ => return Err(Fail::usage("--mode takes natural,wait,both")),
    };
    let tabs = number(&mut args, "--tabs", 8)?;
    let leaves = number(&mut args, "--leaves", 16)?;
    let keys = number(&mut args, "--keys", 100)?;
    let key_ms = number(&mut args, "--key-interval-ms", 20)?;
    let wait_ms = number(&mut args, "--wait-ms", 1200)?;
    let quads = number(&mut args, "--quads", 1)?;
    let keep = take_flag(&mut args, "--keep");
    if !(1..=64).contains(&tabs)
        || !(2..=64).contains(&leaves)
        || !(4..=1000).contains(&keys)
        || !(1..=100).contains(&key_ms)
        || !(100..=5000).contains(&wait_ms)
        || !(1..=10).contains(&quads)
        || keys.saturating_mul(key_ms) < wait_ms.saturating_add(400)
        || keys.saturating_mul(key_ms) > 30_000
    {
        return Err(Fail::usage(
            "invalid workload bounds, or burst shorter than wait + 400 ms",
        ));
    }
    if !args.is_empty() {
        return Err(Fail::usage(format!("unexpected arguments: {args:?}")));
    }
    Ok(Options {
        window: RestoreWindow::Timing,
        builds,
        out,
        cases,
        modes,
        tabs: usize::try_from(tabs).map_err(|_| Fail::usage("tab count exceeds address space"))?,
        leaves: usize::try_from(leaves)
            .map_err(|_| Fail::usage("leaf count exceeds address space"))?,
        keys,
        key_ms,
        wait_ms,
        quads: usize::try_from(quads)
            .map_err(|_| Fail::usage("quad count exceeds address space"))?,
        keep,
    })
}

fn field(text: &str, name: &str) -> Result<u64, String> {
    fields(text)
        .get(name)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("missing integer {name}: {text}"))
}

/// Ignore freshly minted IDs, while preserving every window/tab/leaf position and rectangle.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shape {
    rows: Vec<String>,
    terminal: BTreeMap<String, u64>,
    native_views: BTreeMap<String, u64>,
    window_ids: Vec<u64>,
    window_tabs: Vec<usize>,
    active_tabs: Vec<usize>,
    native: usize,
    tabs: usize,
    tab_counts: Vec<(usize, usize)>,
    tab_kinds: Vec<String>,
}

fn shape(text: &str) -> Result<Shape, String> {
    let mut result = Shape {
        rows: Vec::new(),
        terminal: BTreeMap::new(),
        native_views: BTreeMap::new(),
        window_ids: Vec::new(),
        window_tabs: Vec::new(),
        active_tabs: Vec::new(),
        native: 0,
        tabs: 0,
        tab_counts: Vec::new(),
        tab_kinds: Vec::new(),
    };
    let mut window = 0_usize;
    let mut tab = 0_usize;
    let mut declared = 0_usize;
    let mut active = 0;
    let mut active_id = 0;
    let mut window_ids = BTreeSet::new();
    let mut tab_ids = BTreeSet::new();
    let mut leaf_focus = BTreeMap::<(usize, usize), usize>::new();
    let mut views = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    let mut nodes = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if line.starts_with("OK ") || line.starts_with("app-inspection ") {
            continue;
        }
        let f = fields(line);
        let get = |key: &str| {
            f.get(key)
                .cloned()
                .ok_or_else(|| format!("missing {key}: {line}"))
        };
        if line.starts_with("window ") {
            if window != 0 && (active != 1 || declared != tab) {
                return Err("incomplete window/tab topology".into());
            }
            window = window.checked_add(1).ok_or("window count overflow")?;
            tab = 0;
            active = 0;
            let id = field(line, "id")?;
            if !window_ids.insert(id) {
                return Err("duplicate window identity".into());
            }
            result.window_ids.push(id);
            declared = usize::try_from(field(line, "tabs")?)
                .map_err(|_| "declared tab count exceeds address space")?;
            result.window_tabs.push(declared);
            result.active_tabs.push(0);
            active_id = field(line, "active")?;
            result.rows.push(format!(
                "window {window} {} {} {}",
                get("rows")?,
                get("cols")?,
                get("focused")?
            ));
        } else if line.starts_with("tab ") {
            if window == 0 {
                return Err("tab has no window".into());
            }
            tab = tab.checked_add(1).ok_or("tab count overflow")?;
            result.tabs = result
                .tabs
                .checked_add(1)
                .ok_or("total tab count overflow")?;
            result.tab_counts.push((0, 0));
            let id = field(line, "id")?;
            if !tab_ids.insert(id) {
                return Err("duplicate tab identity".into());
            }
            let index = usize::try_from(field(line, "index")?)
                .map_err(|_| "tab index exceeds address space")?;
            if index == 0 || index != tab || index > declared {
                return Err("nonsequential tab index".into());
            }
            let prefix = line.split_once(" title=").ok_or("missing title")?.0;
            result.tab_kinds.push(
                fields(prefix)
                    .get("kind")
                    .cloned()
                    .ok_or("missing tab kind")?,
            );
            let suffix = line.rsplit_once(" state=").ok_or("missing tab state")?.1;
            let s = fields(suffix);
            let is_active = s.get("active").map(String::as_str) == Some("true");
            if is_active != (id == active_id) {
                return Err("selected tab identity and flag disagree".into());
            }
            active += usize::from(is_active);
            if is_active {
                *result.active_tabs.last_mut().expect("tab has a window") = tab;
            }
            result.rows.push(format!(
                "tab {window}:{tab} {} active={is_active} parked={}",
                fields(prefix).get("kind").ok_or("missing tab kind")?,
                s.get("parked").ok_or("missing parked")?
            ));
        } else if line.starts_with("split ") {
            if tab == 0 || nodes.insert((window, tab, get("path")?), true).is_some() {
                return Err("duplicate split path".into());
            }
            result.rows.push(format!(
                "split {window}:{tab} {} {} {} {}",
                get("path")?,
                get("axis")?,
                get("ratio")?,
                get("rect_px")?
            ));
        } else if line.starts_with("leaf ") {
            if tab == 0 || !views.insert(field(line, "view")?) {
                return Err("unplaced or duplicate view".into());
            }
            let slot = result.tabs.checked_sub(1).ok_or("leaf has no tab")?;
            let counts = result
                .tab_counts
                .get_mut(slot)
                .ok_or("leaf names an absent tab")?;
            let rect = get("rect_px")?
                .split(',')
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "invalid leaf rectangle")?;
            if rect.len() != 4
                || rect.iter().any(|x| !x.is_finite())
                || rect[2] <= 0.0
                || rect[3] <= 0.0
            {
                return Err("empty or invalid visible leaf rectangle".into());
            }
            let key = format!("{window}:{tab}:{}", get("path")?);
            let focused = match get("focused")?.as_str() {
                "true" => 1,
                "false" => 0,
                _ => return Err("invalid leaf focus".into()),
            };
            *leaf_focus.entry((window, tab)).or_default() += focused;
            if nodes.insert((window, tab, get("path")?), false).is_some() {
                return Err("duplicate leaf path".into());
            }
            let kind = get("kind")?;
            let extra = match kind.as_str() {
                "terminal" => {
                    counts.1 = counts
                        .1
                        .checked_add(1)
                        .ok_or("terminal leaf count overflow")?;
                    let session = field(line, "session")?;
                    if !sessions.insert(session)
                        || result.terminal.insert(key.clone(), session).is_some()
                    {
                        return Err("duplicate terminal placement".into());
                    }
                    String::new()
                }
                "app" if get("app")? == "editor" => {
                    result
                        .native_views
                        .insert(key.clone(), field(line, "view")?);
                    result.native = result
                        .native
                        .checked_add(1)
                        .ok_or("native leaf count overflow")?;
                    counts.0 = counts
                        .0
                        .checked_add(1)
                        .ok_or("native leaf count overflow")?;
                    format!(
                        " {}",
                        line.split_once(" title=").ok_or("missing native title")?.1
                    )
                }
                _ => return Err(format!("unexpected restored leaf: {line}")),
            };
            result.rows.push(format!(
                "leaf {key} {kind} {} {}{extra}",
                get("rect_px")?,
                get("focused")?
            ));
        } else {
            return Err(format!("unknown topology: {line}"));
        }
    }
    if window == 0 || active != 1 || declared != tab {
        return Err("incomplete window/tab topology".into());
    }
    for (window, count) in result.window_tabs.iter().enumerate() {
        for tab in 1..=*count {
            if !nodes.contains_key(&(window + 1, tab, "/".into()))
                || leaf_focus.get(&(window + 1, tab)) != Some(&1)
            {
                return Err("tab has no root or unique focused leaf".into());
            }
        }
    }
    for ((window, tab, path), split) in &nodes {
        if path != "/" {
            let (parent, side) = path.rsplit_once('/').ok_or("invalid split path")?;
            let parent = if parent.is_empty() { "/" } else { parent };
            if !matches!(side, "0" | "1")
                || nodes.get(&(*window, *tab, parent.into())) != Some(&true)
            {
                return Err("leaf/split has no canonical parent".into());
            }
        }
        if *split {
            let prefix = if path == "/" { "" } else { path };
            if (0..2).any(|side| !nodes.contains_key(&(*window, *tab, format!("{prefix}/{side}"))))
            {
                return Err("split is missing a child".into());
            }
        }
    }
    Ok(result)
}

fn same_shape(a: &Shape, b: &Shape) -> Result<(), String> {
    if a.rows != b.rows
        || a.terminal.keys().ne(b.terminal.keys())
        || a.native_views.keys().ne(b.native_views.keys())
        || a.window_tabs != b.window_tabs
        || a.active_tabs != b.active_tabs
    {
        return Err("restored window/tab/split/focus topology differs from seeded fixture".into());
    }
    Ok(())
}

fn judge_seed(shape: &Shape, case: Case, tabs: usize, leaves: usize) -> Result<(), String> {
    if tabs == 0 || leaves < 2 {
        return Err("invalid fixture workload".into());
    }
    let mut expected = vec![(0, 1)];
    let mut kinds = vec!["terminal"];
    match case {
        Case::Mixed => {
            expected.extend(std::iter::repeat_n((1, 1), tabs));
            kinds.extend(std::iter::repeat_n("mixed", tabs));
        }
        Case::Native => {
            expected.extend(std::iter::repeat_n((1, 0), tabs));
            kinds.extend(std::iter::repeat_n("app", tabs));
        }
        Case::Split => {
            expected.push((1, leaves - 1));
            kinds.push("mixed");
        }
        Case::EditorFirst | Case::EditorFirstNativeWindow => {
            expected.insert(0, (1, 0));
            kinds.insert(0, "app");
            expected.extend(std::iter::repeat_n((1, 0), tabs - 1));
            kinds.extend(std::iter::repeat_n("app", tabs - 1));
            if case.extra_window() {
                expected.push((1, 0));
                kinds.push("app");
            }
        }
    }
    let windows = if case.extra_window() {
        vec![tabs + 1, 1]
    } else {
        vec![expected.len()]
    };
    if shape.window_tabs != windows
        || shape.tab_counts != expected
        || shape.tab_kinds.iter().map(String::as_str).ne(kinds)
    {
        return Err("seeded fixture has the wrong per-tab native/terminal distribution".into());
    }
    Ok(())
}

struct Seed {
    shape: Shape,
    grids: BTreeMap<String, Vec<String>>,
    wait_uri: String,
    layout_digest: String,
    bootstrap_key: String,
    native_uris: BTreeMap<String, String>,
    saved: fixture::Composition,
}

fn launch(
    world: &World,
    build: &Build,
    gated: bool,
    window: RestoreWindow,
) -> Result<Instance, String> {
    launch_with_program(world, build, gated, common::PROGRAM, window)
}

#[derive(Clone, Copy)]
enum RestoreWindow {
    Timing,
    Correctness { memory_bytes: u64 },
}

fn launch_with_program(
    world: &World,
    build: &Build,
    gated: bool,
    program: &str,
    work: RestoreWindow,
) -> Result<Instance, String> {
    let mut env = world.env();
    if gated {
        env.push((
            "ATERM_DEBUG_RESTORE_MEASUREMENT".into(),
            world
                .home
                .join("restore-measure/config.json")
                .to_string_lossy()
                .into_owned(),
        ));
    }
    let launch = Launch {
        aterm: build.path.clone(),
        headless: false,
        args: [
            "--columns",
            "110",
            "--lines",
            "48",
            "-e",
            "/bin/sh",
            "-c",
            program,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        env,
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    };
    match work {
        RestoreWindow::Timing => Instance::start_in(world, launch, Band::App),
        RestoreWindow::Correctness { memory_bytes } => {
            Instance::start_correctness_in(world, launch, memory_bytes, Band::App)
        }
    }
}

fn wait<T>(mut read: impl FnMut() -> Result<Option<T>, String>) -> Result<T, String> {
    let began = Instant::now();
    loop {
        if let Some(value) = read()? {
            return Ok(value);
        }
        if began.elapsed() >= HANG {
            return Err("native restore hang detector expired".into());
        }
        common::nap(Duration::from_millis(20))?;
    }
}

fn get_shape(wire: &mut Wire, raw: &mut Raw, label: &str) -> Result<Shape, String> {
    let text = common::counted(wire, "inspect app/v1 tabs")?;
    raw.note(label, &text);
    shape(&text)
}

fn terminal_rows(wire: &mut Wire, topology: &Shape) -> Result<BTreeMap<String, Session>, String> {
    let rows = common::roster(wire)?;
    if rows.len() != topology.terminal.len() {
        return Err("session roster and terminal leaf count differ".into());
    }
    topology
        .terminal
        .iter()
        .map(|(key, local)| {
            let row = rows
                .iter()
                .find(|s| s.local == *local)
                .ok_or("missing leaf's session")?;
            let window = key
                .split(':')
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| topology.window_ids.get(n))
                .ok_or("terminal has no containing window")?;
            if row.window != window.to_string() {
                return Err("terminal roster and topology window disagree".into());
            }
            Ok((key.clone(), row.clone()))
        })
        .collect()
}

/// Bind canonical file identities to actual native views, not their mutable display titles.
fn document_positions(topology: &Shape, text: &str) -> Result<BTreeMap<String, String>, String> {
    document_positions_with_dirty(topology, text, false)
}

fn document_positions_with_dirty(
    topology: &Shape,
    text: &str,
    dirty: bool,
) -> Result<BTreeMap<String, String>, String> {
    let mut documents = BTreeSet::new();
    let mut uris = BTreeSet::new();
    let mut views = BTreeMap::new();
    let mut lines = text.lines();
    let count: usize = lines
        .next()
        .and_then(|line| line.strip_prefix("OK "))
        .and_then(|count| count.parse().ok())
        .ok_or("document roster count missing")?;
    let mut rows = 0;
    for line in lines {
        rows += 1;
        let f = fields(line);
        if f.len() != 9
            || line.split_whitespace().count() != 9
            || ![
                "doc", "uri", "seq", "dirty", "disk", "views", "ticket", "cursor", "sel",
            ]
            .iter()
            .all(|key| f.contains_key(*key))
        {
            return Err("malformed document roster".into());
        }
        let uri = &f["uri"];
        let doc = field(line, "doc")?;
        field(line, "seq")?;
        if doc == 0
            || !documents.insert(doc)
            || !uri.starts_with("file:///")
            || !uris.insert(uri.clone())
            || f["dirty"] != (if dirty { "1" } else { "0" })
            || f["disk"] != "same"
        {
            return Err("unexpected document identity or state".into());
        }
        for view in f["views"].split(',') {
            let view: u64 = view.parse().map_err(|_| "invalid document view")?;
            if view == 0 || views.insert(view, uri.clone()).is_some() {
                return Err("duplicate document view".into());
            }
        }
    }
    if count != rows || views.len() != topology.native_views.len() {
        return Err("document roster and native topology differ".into());
    }
    topology
        .native_views
        .iter()
        .map(|(key, view)| {
            Ok((
                key.clone(),
                views
                    .get(view)
                    .ok_or("native view has no document")?
                    .clone(),
            ))
        })
        .collect()
}

fn check_documents(
    wire: &mut Wire,
    raw: &mut Raw,
    label: &str,
    topology: &Shape,
    expected: &BTreeMap<String, String>,
) -> Result<(), String> {
    let text = common::counted(wire, "doc ls")?;
    raw.note(label, &text);
    if &document_positions(topology, &text)? != expected {
        return Err("native document URI is in the wrong window/tab/leaf".into());
    }
    Ok(())
}

fn same_bootstrap(typing: &Session, restored: &Session) -> Result<(), String> {
    if typing.sid != restored.sid
        || typing.local != restored.local
        || typing.window != restored.window
    {
        return Err("restore replaced or moved the live bootstrap session".into());
    }
    Ok(())
}

fn split_terminal_tree(wire: &mut Wire, initial: String, leaves: usize) -> Result<(), String> {
    let mut frontier = VecDeque::from([(initial, 0)]);
    for _ in 1..leaves {
        let (sid, depth) = frontier.pop_front().ok_or("split frontier empty")?;
        common::ask(wire, &format!("raise {sid}"))?;
        let before: BTreeSet<_> = common::roster(wire)?
            .into_iter()
            .map(|row| row.sid)
            .collect();
        common::ask(
            wire,
            if depth % 2 == 0 {
                "spawn split=h"
            } else {
                "spawn split=v"
            },
        )?;
        let after = common::await_roster(wire, before.len() + 1)?;
        let new = after
            .into_iter()
            .find(|row| !before.contains(&row.sid))
            .ok_or("split added no new session")?;
        frontier.push_back((sid, depth + 1));
        frontier.push_back((new.sid, depth + 1));
    }
    Ok(())
}

fn request_fixture_quit(request: impl FnMut(&str) -> Result<String, String>) -> Result<(), String> {
    request_fixture_action("Quit", "quit", request)
}

fn request_fixture_action(
    action: &str,
    kind: &str,
    mut request: impl FnMut(&str) -> Result<String, String>,
) -> Result<(), String> {
    let reply = request(&format!("invoke {action}"))?;
    if reply.trim() == format!("OK invoked {action}") {
        return Ok(());
    }
    let protocol = reply.split(';').next().unwrap_or_default().trim();
    let Some(pending) = protocol.strip_prefix("OK confirm pending ") else {
        return Err(format!("fixture {action} refused: {reply}"));
    };
    let fields = fields(pending);
    if pending.split_whitespace().count() != 2
        || fields.get("kind").map(String::as_str) != Some(kind)
        || fields.get("window").map(String::as_str) != Some("0")
    {
        return Err(format!("fixture {action} refused: {reply}"));
    }
    let reply = request("confirm yes")?;
    if reply.trim() == format!("OK answered=yes kind={kind}") {
        Ok(())
    } else {
        Err(format!("fixture {action} confirmation refused: {reply}"))
    }
}

fn fixture_gui_absent(
    presence: aterm_uds::process::ProcessPresence,
    identity_matches: Option<bool>,
) -> Result<bool, String> {
    use aterm_uds::process::ProcessPresence;
    match presence {
        ProcessPresence::Absent => Ok(true),
        ProcessPresence::Unknown => Err("fixture GUI native absence is unknown".into()),
        ProcessPresence::Exited => Ok(false),
        ProcessPresence::Present if identity_matches == Some(true) => Ok(false),
        ProcessPresence::Present => {
            Err("fixture GUI identity changed or became unreadable before observed absence".into())
        }
    }
}

fn quit_layout(
    world: &World,
    inst: &Instance,
    wire: &mut Wire,
    raw: &mut Raw,
    label: &str,
) -> Result<String, String> {
    let pid = inst.aterm_pid().ok_or("seed app PID missing")?;
    let identity = aterm_uds::process::read_identity(pid).ok_or("seed app identity missing")?;
    request_fixture_quit(|request| {
        let reply = wire.request_line(request);
        let suffix = if request == "invoke Quit" {
            "quit"
        } else {
            "quit_confirmation"
        };
        raw.note(&format!("{label}_{suffix}"), format!("{reply:?}"));
        reply.map_err(|error| format!("{request}: {error}"))
    })?;
    wait(|| {
        // Read identity first so an exit between reads is observed as Exited/Absent, rather
        // than misclassifying an unreadable identity after a prior Present observation.
        let current = aterm_uds::process::read_identity(pid);
        let presence = aterm_uds::process::process_presence(pid);
        fixture_gui_absent(presence, current.map(|current| current == identity))
            .map(|absent| absent.then_some(()))
    })?;
    raw.note(&format!("{label}_gui_absent"), format!("pid={pid}"));
    let layout = world.state().join("session.toml");
    let text = std::fs::read_to_string(&layout)
        .map_err(|e| format!("seed layout {}: {e}", layout.display()))?;
    raw.note(label, &text);
    Ok(text)
}

fn standalone_descriptors(
    world: &World,
    build: &Build,
    case: Case,
    opts: &Options,
    raw: &mut Raw,
) -> Result<(String, Vec<String>), String> {
    let mut inst = launch(world, build, false, opts.window)?;
    let result = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "seed boot failed".into()))?;
        common::cpu(&inst, build, None, raw, "descriptor_identity")?;
        let mut wire = common::bounded_wire(&inst)?;
        let bootstrap = common::await_roster(&mut wire, 1)?.remove(0);
        let native = if case == Case::Split { 1 } else { opts.tabs };
        let mut uris = Vec::new();
        for n in 0..native {
            let path = world.home.join(format!("restore-{n:02}.txt"));
            std::fs::write(&path, format!("Native restore fixture {n}\n"))
                .map_err(|e| e.to_string())?;
            let uri = format!("file://{}", path.display());
            common::ask(&mut wire, &format!("open app editor {uri}"))?;
            uris.push(uri);
            let expected_tabs = if case.paired() { n * 2 + 2 } else { n + 2 };
            wait(|| {
                let t = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
                Ok((t.tabs == expected_tabs && t.native == n + 1).then_some(()))
            })?;
            if case.paired() {
                let before: BTreeSet<_> = common::roster(&mut wire)?
                    .into_iter()
                    .map(|row| row.sid)
                    .collect();
                common::ask(&mut wire, "spawn")?;
                let initial = common::await_roster(&mut wire, before.len() + 1)?
                    .into_iter()
                    .find(|row| !before.contains(&row.sid))
                    .ok_or("new standalone terminal is missing")?;
                split_terminal_tree(
                    &mut wire,
                    initial.sid,
                    if case == Case::Split {
                        opts.leaves - 1
                    } else {
                        1
                    },
                )?;
            }
        }
        if case.editor_first() {
            common::ask(&mut wire, &format!("@{} tab move 2 1", bootstrap.sid))?;
        }
        if case.extra_window() {
            let path = world.home.join("restore-extra-window.txt");
            std::fs::write(&path, "Native-only additional window\n").map_err(|e| e.to_string())?;
            let uri = format!("file://{}", path.display());
            common::ask(&mut wire, &format!("open app editor {uri}"))?;
            wait(|| {
                let t = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
                Ok((t.tabs == native + 2 && t.native == native + 1).then_some(()))
            })?;
            common::ask(&mut wire, "invoke MoveTabToNewWindow")?;
            uris.push(uri);
            wait(|| {
                let t = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
                Ok(judge_seed(&t, case, opts.tabs, opts.leaves)
                    .is_ok()
                    .then_some(()))
            })?;
        }
        common::raise_for_setup(&mut wire, &bootstrap)?;
        let standalone = get_shape(&mut wire, raw, "ui_created_standalone_topology")?;
        if case.editor_first() {
            judge_seed(&standalone, case, opts.tabs, opts.leaves)?;
        }
        Ok((
            quit_layout(world, &inst, &mut wire, raw, "standalone_layout")?,
            uris,
        ))
    })();
    if result.is_err() {
        raw.note("descriptor_app_log", inst.log_tail(80));
    }
    common::finish(result, || inst.stop(), raw)
}

fn seed(
    world: &World,
    build: &Build,
    case: Case,
    opts: &Options,
    raw: &mut Raw,
) -> Result<Seed, String> {
    common::digest(build, raw, "seed_before")?;
    // Editor panes have no public split command. Compose their saved placement from real
    // descriptors, then require the baseline app to accept and re-save that exact topology.
    let (standalone, uris) = standalone_descriptors(world, build, case, opts, raw)?;
    let composed = fixture::compose(&standalone, case, &uris, opts.leaves)?;
    let bootstrap_key = composed
        .bootstrap_key
        .clone()
        .ok_or("ABBA fixture has no terminal bootstrap")?;
    raw.note("composed_saved_layout", &composed.text);
    crate::util::atomic_write_mode(
        &world.state().join("session.toml"),
        composed.text.as_bytes(),
        0o600,
    )
    .map_err(|e| format!("write composed layout: {e}"))?;
    let mut inst = launch(world, build, false, opts.window)?;
    let result = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "seed boot failed".into()))?;
        common::cpu(&inst, build, None, raw, "normalization_identity")?;
        let mut wire = common::bounded_wire(&inst)?;
        let expected_terminals = match case {
            Case::Mixed => opts.tabs + 1,
            Case::Native | Case::EditorFirst | Case::EditorFirstNativeWindow => 1,
            Case::Split => opts.leaves,
        };
        let normalized = wait(|| {
            let t = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok(judge_seed(&t, case, opts.tabs, opts.leaves)
                .is_ok()
                .then_some(t))
        })?;
        let bootstrap_local = *normalized
            .terminal
            .get(&bootstrap_key)
            .ok_or("normalized bootstrap missing")?;
        let bootstrap = common::await_roster(&mut wire, expected_terminals)?
            .into_iter()
            .find(|row| row.local == bootstrap_local)
            .ok_or("normalized bootstrap session missing")?;
        common::raise_for_setup(&mut wire, &bootstrap)?;
        // Warm-up and all direct input precede the measured relaunch.
        common::post(&mut wire, 1, 20)?;
        common::await_text(&mut wire, &bootstrap.sid, "a", true)?;
        common::feed(&mut wire, &bootstrap.sid, common::CLEAR)?;
        common::nap(Duration::from_millis(300))?;
        let shape = get_shape(&mut wire, raw, "seed_topology")?;
        judge_seed(&shape, case, opts.tabs, opts.leaves)?;
        let native =
            if case == Case::Split { 1 } else { opts.tabs } + usize::from(case.extra_window());
        if shape.native != native || shape.terminal.len() != expected_terminals {
            return Err("seeded fixture does not have requested native/terminal leaves".into());
        }
        let native_uris = composed.native_uris()?;
        check_documents(&mut wire, raw, "seed_documents", &shape, &native_uris)?;
        let mut grids = BTreeMap::new();
        for (key, session) in terminal_rows(&mut wire, &shape)? {
            let snap = common::snapshot(&mut wire, &session.sid, &format!("seed_{key}"), raw)?;
            grids.insert(key, common::geometry(&snap.dims)?);
        }
        let text = quit_layout(world, &inst, &mut wire, raw, "normalized_layout")?;
        composed.check_round_trip(&text)?;
        let layout_digest = crate::measure::sha256(&text);
        common::digest(build, raw, "seed_end")?;
        Ok(Seed {
            shape,
            grids,
            wait_uri: uris[if case.extra_window() {
                uris.len() - 1
            } else {
                0
            }]
            .clone(),
            layout_digest,
            bootstrap_key,
            native_uris,
            saved: composed,
        })
    })();
    if result.is_err() {
        raw.note("seed_app_log", inst.log_tail(80));
    }
    common::finish(result, || inst.stop(), raw)
}

fn private_write(home: &Path, name: &str, text: &str) -> Result<(), String> {
    aterm_uds::private_file::write_private(home, name, text.as_bytes()).map_err(|e| e.to_string())
}

#[derive(Clone, Debug)]
struct Observation {
    calls: u64,
    max_ns: u64,
    first: u64,
    last: u64,
    max_first: u64,
    max_last: u64,
    finished: bool,
    key_first: u64,
    key_last: u64,
    posted: u64,
    burst: u64,
    enabled: bool,
    refused: bool,
    gate_released: bool,
    key_active: bool,
    key_conflict: bool,
}

fn observation(text: &str) -> Result<Observation, String> {
    let get = |s| field(text, s);
    Ok(Observation {
        calls: get("restore_measurement_callback_count")?,
        max_ns: get("restore_measurement_callback_max_ns")?,
        first: get("restore_measurement_callback_first_begin_ns")?,
        last: get("restore_measurement_callback_last_end_ns")?,
        max_first: get("restore_measurement_callback_max_begin_ns")?,
        max_last: get("restore_measurement_callback_max_end_ns")?,
        finished: get("restore_measurement_finished")? == 1,
        key_first: get("restore_measurement_key_first_post_ns")?,
        key_last: get("restore_measurement_key_last_post_ns")?,
        posted: get("restore_measurement_key_posted")?,
        burst: get("restore_measurement_key_burst_id")?,
        enabled: get("restore_measurement_enabled")? == 1,
        refused: get("restore_measurement_refused")? != 0,
        gate_released: get("restore_measurement_gate_released")? == 1,
        key_active: get("restore_measurement_key_active")? != 0,
        key_conflict: get("restore_measurement_key_conflict")? != 0,
    })
}

fn overlap(o: &Observation) -> Result<(), String> {
    if !o.enabled
        || o.refused
        || !o.gate_released
        || o.key_active
        || o.key_conflict
        || !o.finished
        || o.calls == 0
        || o.first == 0
        || o.last < o.first
        || o.key_first == 0
        || o.key_first > o.first
        || o.key_last < o.last
        || o.posted == 0
        || o.max_first < o.first
        || o.max_last > o.last
        || o.max_last < o.max_first
        || o.max_ns != o.max_last - o.max_first
    {
        return Err(
            "native burst did not enclose the complete observed restore callback interval".into(),
        );
    }
    Ok(())
}

fn event(path: &Path, wanted: &str, pid: i32) -> Result<Option<u64>, String> {
    let pid = u64::try_from(pid).map_err(|_| "gate process ID is negative")?;
    let bytes = match aterm_uds::private_file::read_private(
        path.parent().ok_or("event parent missing")?,
        path.file_name()
            .and_then(|s| s.to_str())
            .ok_or("event filename missing")?,
        64 * 1024,
    ) {
        Ok(bytes) => bytes,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    for line in text
        .split_inclusive('\n')
        .rev()
        .filter(|line| line.ends_with('\n'))
    {
        let row: aterm_json::Value = aterm_json::from_str(line).map_err(|e| e.to_string())?;
        if row.get("pid").and_then(aterm_json::Value::as_u64) != Some(pid) {
            return Err("gate receipt belongs to another process".into());
        }
        if row
            .get("event")
            .and_then(aterm_json::Value::as_str)
            .is_some_and(|name| name.ends_with("_failed"))
        {
            return Err(format!("gate refused: {line}"));
        }
        if row.get("event").and_then(aterm_json::Value::as_str) == Some(wanted) {
            return row
                .get("monotonic_ns")
                .and_then(aterm_json::Value::as_u64)
                .map(Some)
                .ok_or_else(|| "gate event missing monotonic ns".into());
        }
    }
    Ok(None)
}

fn release_absent(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[derive(Debug)]
struct Take {
    observation: Observation,
    metrics: common::Metrics,
    cpu_ns: u64,
    layout: String,
    windows: usize,
    bootstrap_key: String,
    wait_uri: String,
    wait_first: Option<u64>,
    wait_last: Option<u64>,
    during_wait: Option<u64>,
    posts_during_restore: usize,
    posts_during_max: usize,
    posts_during_wait: usize,
    ambiguous_restore: usize,
    ambiguous_max: usize,
    ambiguous_wait: usize,
}

fn cohort(posts: &[(u64, u64)], first: u64, last: u64) -> (usize, usize) {
    let contained = |&(begin, end): &(u64, u64)| begin >= first && end <= last;
    let certain = posts.iter().filter(|span| contained(span)).count();
    let ambiguous = posts
        .iter()
        .filter(|span| !contained(span) && span.1 >= first && span.0 <= last)
        .count();
    (certain, ambiguous)
}

fn posted_times(
    path: &Path,
    pid: i32,
    observation: &Observation,
) -> Result<Vec<(u64, u64)>, String> {
    let pid = u64::try_from(pid).map_err(|_| "key-post process ID is negative")?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    for line in text.lines().rev() {
        let row: aterm_json::Value = aterm_json::from_str(line).map_err(|e| e.to_string())?;
        if row.get("event").and_then(aterm_json::Value::as_str) == Some("key_posts")
            && row.get("burst_id").and_then(aterm_json::Value::as_u64) == Some(observation.burst)
        {
            if row.get("pid").and_then(aterm_json::Value::as_u64) != Some(pid) {
                return Err("key posts came from another app".into());
            }
            let begins = row
                .get("post_ns")
                .and_then(aterm_json::Value::as_array)
                .ok_or("key post timestamps missing")?
                .iter()
                .map(|v| v.as_u64().ok_or("noninteger key timestamp"))
                .collect::<Result<Vec<_>, _>>()?;
            let ends = row
                .get("post_end_ns")
                .and_then(aterm_json::Value::as_array)
                .ok_or("key post end timestamps missing")?
                .iter()
                .map(|v| v.as_u64().ok_or("noninteger key end timestamp"))
                .collect::<Result<Vec<_>, _>>()?;
            if begins.len() != ends.len() {
                return Err("key post bounds have different lengths".into());
            }
            let posts: Vec<_> = begins.into_iter().zip(ends).collect();
            if u64::try_from(posts.len()).map_err(|_| "post count exceeds wire range")?
                != observation.posted
                || posts.iter().any(|&(begin, end)| begin > end)
                || posts.windows(2).any(|w| w[0].1 > w[1].0)
                || posts.first().map(|s| s.0) != Some(observation.key_first)
                || posts.last().map(|s| s.1) != Some(observation.key_last)
            {
                return Err("key timestamp trace disagrees with post counters".into());
            }
            return Ok(posts);
        }
    }
    Err("native key trace missing".into())
}

fn measured(
    world: &World,
    build: &Build,
    seed: &Seed,
    mode: Mode,
    require_progress: bool,
    opts: &Options,
    raw: &mut Raw,
) -> Result<Take, String> {
    let gates = world.home.join("restore-measure");
    crate::util::create_private_dir_all(&gates).map_err(|e| e.to_string())?;
    let config = aterm_json::json!({
        "start_file": "restore-start", "events_file": "restore-events.jsonl",
        "wait_uri": if mode == Mode::Wait { Some(&seed.wait_uri) } else { None },
        "release_file": if mode == Mode::Wait { Some("restore-release") } else { None },
    });
    private_write(&gates, "config.json", &config.to_string())?;
    common::digest(build, raw, "before_restore")?;
    let mut inst = launch(world, build, true, opts.window)?;
    let mut posting = None;
    let mut releaser = None;
    let mut typing_sid = None;
    let result = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "restore boot failed".into()))?;
        let mut wire = common::bounded_wire(&inst)?;
        let events = gates.join("restore-events.jsonl");
        let pid = inst.aterm_pid().ok_or("restore app PID unavailable")?;
        wait(|| event(&events, "start_ready", pid))?;
        let typing = common::await_roster(&mut wire, 1)?.remove(0);
        typing_sid = Some(typing.sid.clone());
        raw.note("bootstrap_position", &seed.bootstrap_key);
        raw.note("bootstrap_before", format!("{typing:?}"));
        let native = common::raise_for_setup(&mut wire, &typing)?;
        raw.note("native_focus_before", &native);
        common::post(&mut wire, 1, 20)?;
        common::await_text(&mut wire, &typing.sid, "a", true)?;
        common::feed(&mut wire, &typing.sid, common::CLEAR)?;
        common::nap(Duration::from_millis(100))?;
        common::ask(&mut wire, "metrics reset")?;
        if matches!(opts.window, RestoreWindow::Timing) {
            let entry = inst.measurement_entry()?;
            raw.note("measurement_entry_sampled", format!("{entry:?}"));
        } else {
            raw.note(
                "correctness_entry",
                "held functional admission; no timing qualification",
            );
        }
        common::digest(build, raw, "entry")?;
        let (identity, cpu_start) = common::cpu(&inst, build, None, raw, "start")?;
        let mut keys_wire = common::bounded_wire(&inst)?;
        let keys = opts.keys;
        let key_ms = opts.key_ms;
        posting = Some(std::thread::spawn(move || {
            common::post(&mut keys_wire, keys, key_ms)
        }));
        // The first native post proves target resolution completed while the UI could answer.
        wait(|| {
            let text = common::ask(&mut wire, "metrics")?;
            let o = observation(&text)?;
            Ok((o.key_first != 0 && o.posted > 0).then_some(()))
        })?;
        private_write(&gates, "restore-start", "start\n")?;
        let mut wait_first = None;
        let mut wait_last = None;
        let mut during_wait = None;
        if mode == Mode::Wait {
            let entered = wait(|| event(&events, "wait_entered", pid))?;
            wait_first = Some(entered);
            // Release is autonomous: a control reply delayed by A's blocked UI cannot hold it.
            let home = gates.clone();
            let wait_ms = opts.wait_ms;
            releaser = Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(wait_ms));
                private_write(&home, "restore-release", "release\n")
            }));
            let mut first = None;
            let release_path = gates.join("restore-release");
            while release_absent(&release_path)? && event(&events, "wait_released", pid)?.is_none()
            {
                let text = common::ask(&mut wire, "metrics percentiles")?;
                if !release_absent(&release_path)?
                    || event(&events, "wait_released", pid)?.is_some()
                {
                    break;
                }
                raw.note("during_wait_metrics", &text);
                let n = field(&text, "n_input")?;
                let before = *first.get_or_insert(n);
                during_wait = Some(n.saturating_sub(before));
                if n > before {
                    break;
                }
                common::nap(Duration::from_millis(20))?;
            }
            releaser
                .take()
                .expect("release started")
                .join()
                .map_err(|_| "release worker panicked")??;
            wait_last = Some(wait(|| event(&events, "wait_released", pid))?);
            if require_progress && during_wait.is_none_or(|n| n == 0) {
                return Err("optimized app booked no new native input while its file wait was observed held".into());
            }
        }
        let posted = posting
            .take()
            .expect("native poster started")
            .join()
            .map_err(|_| "native poster panicked")??;
        raw.note("native_post_receipt", &posted);
        let observed = wait(|| {
            let text = common::ask(&mut wire, "metrics")?;
            let observed = observation(&text)?;
            Ok(observed.finished.then_some(observed))
        })?;
        overlap(&observed)?;
        if wait_first.zip(wait_last).is_some_and(|(first, last)| {
            first < observed.key_first || last > observed.key_last || last <= first
        }) {
            return Err("native posting did not enclose the controlled file wait".into());
        }
        if observed.posted != opts.keys {
            return Err("post count differs from requested keys".into());
        }
        let posts = posted_times(&events, pid, &observed)?;
        let (posts_during_restore, ambiguous_restore) =
            cohort(&posts, observed.first, observed.last);
        let (posts_during_max, ambiguous_max) =
            cohort(&posts, observed.max_first, observed.max_last);
        let (posts_during_wait, ambiguous_wait) = wait_first
            .zip(wait_last)
            .map_or((0, 0), |(first, last)| cohort(&posts, first, last));
        if posts_during_restore == 0 || (mode == Mode::Wait && posts_during_wait == 0) {
            return Err("no actual native post overlapped the required restore work".into());
        }
        common::await_text(
            &mut wire,
            &typing.sid,
            &"a".repeat(usize::try_from(opts.keys).map_err(|_| "key count exceeds address space")?),
            true,
        )?;
        let metrics_text = common::ask(&mut wire, "metrics percentiles")?;
        raw.note("final_metrics", &metrics_text);
        let metrics = common::metrics(&metrics_text, opts.keys)?;
        let (_, cpu_end) = common::cpu(&inst, build, Some(identity), raw, "end")?;
        let cpu_ns = cpu_end
            .checked_sub(cpu_start)
            .ok_or("CPU counter regressed")?;
        let topology = get_shape(&mut wire, raw, "restored_topology")?;
        same_shape(&seed.shape, &topology)?;
        let restored = terminal_rows(&mut wire, &topology)?;
        let bootstrap = restored
            .get(&seed.bootstrap_key)
            .ok_or("restored bootstrap position missing")?;
        same_bootstrap(&typing, bootstrap)?;
        raw.note("bootstrap_after", format!("{bootstrap:?}"));
        check_documents(
            &mut wire,
            raw,
            "restored_documents",
            &topology,
            &seed.native_uris,
        )?;
        for (key, session) in restored {
            let snap = common::snapshot(&mut wire, &session.sid, &format!("restored_{key}"), raw)?;
            if seed.grids.get(&key) != Some(&common::geometry(&snap.dims)?) {
                return Err(format!("restored program grid differs at {key}"));
            }
        }
        common::focus(
            &mut wire,
            &typing,
            fields(&native).get("window").map(String::as_str),
        )?;
        common::digest(build, raw, "end")?;
        if matches!(opts.window, RestoreWindow::Timing) {
            let final_claim = inst.timing_admission()?;
            raw.note("final_timing_admission_sampled", format!("{final_claim:?}"));
        }
        raw.note(
            "gate_events",
            std::fs::read_to_string(&events).map_err(|e| e.to_string())?,
        );
        let layout = quit_layout(world, &inst, &mut wire, raw, "measured_final_layout")?;
        seed.saved.check_round_trip(&layout)?;
        Ok(Take {
            observation: observed,
            metrics,
            cpu_ns,
            layout: seed.layout_digest.clone(),
            windows: seed.shape.window_ids.len(),
            bootstrap_key: seed.bootstrap_key.clone(),
            wait_uri: if mode == Mode::Wait {
                seed.wait_uri.clone()
            } else {
                String::new()
            },
            wait_first,
            wait_last,
            during_wait,
            posts_during_restore,
            posts_during_max,
            posts_during_wait,
            ambiguous_restore,
            ambiguous_max,
            ambiguous_wait,
        })
    })();
    if let Err(error) = &result {
        diagnostics::capture(&inst, typing_sid.as_deref(), error, raw);
        raw.note("restore_app_log", inst.log_tail(80));
    }
    // Release every gate before cleanup, including a failed overlap or control request.
    let _ = private_write(&gates, "restore-start", "start\n");
    let _ = private_write(&gates, "restore-release", "release\n");
    common::finish(
        result,
        || {
            let stopped = inst.stop();
            if let Some(thread) = posting {
                let _ = thread.join();
            }
            if let Some(thread) = releaser {
                let _ = thread.join();
            }
            stopped
        },
        raw,
    )
}

pub(crate) fn run(args: &[String]) -> Res {
    if args.iter().any(|arg| arg == "--correctness-only") {
        return correctness::run(args);
    }
    if args.iter().any(|s| s == "--help") {
        println!("{HELP}");
        return Ok(0);
    }
    let opts = parse(args)?;
    stop::install();
    std::fs::create_dir(&opts.out).map_err(|e| Fail::exit(3, e.to_string()))?;
    let mut csv = String::from(
        "take,arm,case,mode,fixture,source,sha256,layout_sha256,windows,bootstrap_position,wait_uri,tabs,leaves,keys,key_interval_ms,wait_ms,callbacks,longest_callback_ns,restore_first_ns,restore_last_ns,key_first_ns,key_last_ns,whole_burst_n_input,whole_burst_input_p50_ms,whole_burst_input_p95_ms,whole_burst_input_p99_ms,cpu_ns,wait_first_ns,wait_last_ns,input_booked_during_observed_wait,posts_in_restore_span,posts_in_max_callback,posts_in_wait,ambiguous_restore_posts,ambiguous_max_posts,ambiguous_wait_posts\n",
    );
    let mut number = 0;
    for case in &opts.cases {
        for mode in &opts.modes {
            for _ in 0..opts.quads {
                for arm in [0, 1, 1, 0] {
                    number += 1;
                    let build = &opts.builds[arm];
                    let mut raw = Raw::default();
                    raw.note("declared_source", &build.source);
                    raw.note("trusted_artifact", &build.artifact);
                    raw.note("trusted_build_record", build.record.display());
                    raw.note("trusted_build_record_sha256", &build.record_digest);
                    raw.note("fixture_source", &opts.builds[0].source);
                    let config = format!(
                        "{}{}",
                        instance::config_with(&[]),
                        crate::linux_proof::DECK_OFF
                    );
                    let mut world = World::new(&config).map_err(|e| Fail::exit(3, e))?;
                    if opts.keep {
                        world.keep();
                    }
                    raw.note("world", world.dir.display());
                    let result =
                        seed(&world, &opts.builds[0], *case, &opts, &mut raw).and_then(|seed| {
                            measured(&world, build, &seed, *mode, arm == 1, &opts, &mut raw)
                        });
                    if result.is_err() {
                        world.keep();
                    }
                    raw.note("result", format!("{result:?}"));
                    let path = opts.out.join(format!(
                        "{number:03}-{}-{}-{}.txt",
                        if arm == 0 { "a" } else { "b" },
                        case.name(),
                        mode.name()
                    ));
                    std::fs::write(&path, raw.as_text())
                        .map_err(|e| Fail::exit(3, e.to_string()))?;
                    let taken = result.map_err(|e| {
                        Fail::exit(
                            3,
                            format!(
                                "restore-native refused; diagnostics {}: {e}",
                                path.display()
                            ),
                        )
                    })?;
                    let o = &taken.observation;
                    let q = common::csv_field;
                    let optional = |n: Option<u64>| n.map_or_else(String::new, |n| n.to_string());
                    let row = vec![
                        number.to_string(),
                        if arm == 0 { "a" } else { "b" }.into(),
                        case.name().into(),
                        mode.name().into(),
                        "saved-layout-baseline-roundtrip".into(),
                        q(&build.source),
                        q(&build.digest),
                        q(&taken.layout),
                        taken.windows.to_string(),
                        q(&taken.bootstrap_key),
                        q(&taken.wait_uri),
                        opts.tabs.to_string(),
                        opts.leaves.to_string(),
                        opts.keys.to_string(),
                        opts.key_ms.to_string(),
                        if *mode == Mode::Wait { opts.wait_ms } else { 0 }.to_string(),
                        o.calls.to_string(),
                        o.max_ns.to_string(),
                        o.first.to_string(),
                        o.last.to_string(),
                        o.key_first.to_string(),
                        o.key_last.to_string(),
                        taken.metrics.n.to_string(),
                        taken.metrics.p50.to_string(),
                        taken.metrics.p95.to_string(),
                        taken.metrics.p99.to_string(),
                        taken.cpu_ns.to_string(),
                        optional(taken.wait_first),
                        optional(taken.wait_last),
                        optional(taken.during_wait),
                        taken.posts_during_restore.to_string(),
                        taken.posts_during_max.to_string(),
                        taken.posts_during_wait.to_string(),
                        taken.ambiguous_restore.to_string(),
                        taken.ambiguous_max.to_string(),
                        taken.ambiguous_wait.to_string(),
                    ];
                    let _ = writeln!(csv, "{}", row.join(","));
                }
            }
        }
    }
    std::fs::write(opts.out.join("accepted.csv"), &csv)
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    print!("{csv}");
    Ok(0)
}

#[cfg(test)]
#[path = "restore_native_tests.rs"]
mod tests;
