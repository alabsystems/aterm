// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance suite for `ClaudeFooterModel`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::footer::{self, FooterFacts, LaunchFacts, SessionEntry, TailCache};
use aterm_models_harness::claude_footer_model_model;
use aterm_spec::derive::Model;
use aterm_spec::{verify, xref};

type Vars = BTreeMap<&'static str, i64>;

const START: u64 = 1_790_607_848;
const PID: u32 = 4242;

fn session_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

const LATE: u32 = 999;

/// Which reader a replay drives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reader {
    /// The shipped chain.
    Real,
    /// No floor: the whole tail is read as this process's.
    NoFloor,
    /// Model results are not read: the result rows are hidden from it, so a
    /// `/model` command voids.
    NoResults,
    /// A fixed tail window: no carry and no look past a huge row.
    TailWindow,
    /// The real chain with a cache that forgets the process at a session switch.
    NoCarry,
    /// The real chain, the resumed conversations' rows before the floor
    /// hidden from it: every switch reads as a `/clear`.
    ResumeBlind,
    /// The real chain with a cache that, at a `/resume` only a restore's pin
    /// keeps, has forgotten that pin while keeping the model (a restore taken
    /// for no pin).
    RestoreNoPin,
    /// The real chain reading at the floor: it knows only that a switch came
    /// after the process started, so a conversation begun since reads as a
    /// `/clear`.
    FloorOnly,
}

struct World {
    _root: aterm_tempfile::TempDir,
    claude: PathBuf,
    project: PathBuf,
    cwd: PathBuf,
    transcript: PathBuf,
    /// The next session id a `Clear` takes (1 and 2 are A and B).
    next_session: u32,
    argv: Vec<String>,
    launch: LaunchFacts,
    /// The model the launch card names, when one is drawn.
    card: Option<i64>,
    cache: TailCache,
    /// Seconds after the floor the next row is stamped at.
    clock: u64,
    runs: i64,
    /// The process pinned its model by a `--model` or a choice…
    chose: bool,
    /// …or by a restore.
    restore_pinned: bool,
    /// The conversations taken up (A, then B).
    resumed: u32,
    reader: Reader,
    /// Every transcript written or appended to since the last reset.
    wrote: BTreeSet<PathBuf>,
    /// The session the registry names, once it is written.
    named: Option<u32>,
}

fn stamp(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let base_days = START / 86_400;
    assert_eq!(days, base_days, "a replay stays within the day");
    format!(
        "2026-09-28T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn model_id(m: i64) -> &'static str {
    match m {
        1 => "claude-opus-5-5",
        2 => "claude-fable-5-1",
        other => panic!("no model {other}"),
    }
}

fn model_name(m: i64) -> &'static str {
    match m {
        1 => "Opus 5.5",
        2 => "Fable 5.1",
        other => panic!("no model {other}"),
    }
}

fn project(model: Option<&str>) -> i64 {
    match model {
        None => 0,
        Some("Opus 5.5") => 1,
        Some("Fable 5.1") => 2,
        Some(other) => panic!("the footer named a model no state has: {other}"),
    }
}

fn screen(card: Option<i64>) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-phase/src/fixtures/claude-2.1.283-footer-bypass-idle.txt");
    let rows: Vec<String> = std::fs::read_to_string(path)
        .expect("the measured screen")
        .lines()
        .skip(1)
        .map(str::to_owned)
        .collect();
    if let Some(m) = card {
        return rows
            .into_iter()
            .map(|r| r.replace("Opus 5.5 with", &format!("{} with", model_name(m))))
            .collect();
    }
    let at = rows
        .iter()
        .position(|r| r.contains("Claude Code v"))
        .expect("the card");
    rows.into_iter()
        .enumerate()
        .map(|(i, r)| {
            if (at..at + 3).contains(&i) {
                String::new()
            } else {
                r
            }
        })
        .collect()
}

impl World {
    fn new(tag: &str, reader: Reader) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-footer-model-{tag}-"))
            .tempdir()
            .unwrap();
        let cwd = root.path().join("work");
        let claude = root.path().join("claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(footer::project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let mut world = Self {
            _root: root,
            claude,
            transcript: project.clone(),
            project,
            cwd,
            next_session: 4,
            argv: Vec::new(),
            launch: LaunchFacts::default(),
            card: None,
            cache: TailCache::default(),
            clock: 1,
            runs: 1,
            chose: false,
            restore_pinned: false,
            resumed: 0,
            reader,
            wrote: BTreeSet::new(),
            named: None,
        };
        world.reset();
        world
    }

    fn reset(&mut self) {
        let first = self.named.is_none();
        let gone = std::mem::take(&mut self.wrote);
        for path in &gone {
            std::fs::remove_file(path).unwrap();
        }
        let project = &self.project;
        let unwritten = |n: u32| {
            let path = project.join(format!("{}.jsonl", session_id(n)));
            (first || gone.contains(&path)).then_some(path)
        };
        if self.reader != Reader::ResumeBlind {
            let before = |s: u64| stamp(START - s);
            let write = |n: u32, rows: &[String]| {
                if let Some(path) = unwritten(n) {
                    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
                    std::fs::write(path, body).unwrap();
                }
            };
            write(
                1,
                &[
                    prompt_row(&before(1801)),
                    answer_row(2, &before(1800)),
                    format!(
                        r#"{{"type":"assistant","isMeta":true,"timestamp":"{}","message":{{"model":"{}"}}}}"#,
                        before(1799),
                        model_id(1)
                    ),
                    format!(
                        r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"<synthetic>"}}}}"#,
                        before(1798)
                    ),
                ],
            );
            write(
                2,
                &[
                    prompt_row(&before(7300)),
                    answer_row(2, &before(7250)),
                    prompt_row(&before(7201)),
                    answer_row(1, &before(7200)),
                ],
            );
        }
        if let Some(path) = unwritten(LATE) {
            let at_floor = stamp(START);
            std::fs::write(
                path,
                format!("{}\n{}\n", prompt_row(&at_floor), answer_row(2, &at_floor)),
            )
            .unwrap();
        }
        if self.named != Some(3) {
            self.switch_to(3);
        }
        self.transcript = self.project.join(format!("{}.jsonl", session_id(3)));
        self.next_session = 4;
        self.argv = vec!["claude".to_owned()];
        self.launch = footer::launch_facts(&self.argv);
        self.card = None;
        self.cache = TailCache::default();
        self.clock = 1;
        self.runs = 1;
        self.chose = false;
        self.restore_pinned = false;
        self.resumed = 0;
    }

    fn append(&mut self, rows: &[String]) {
        self.wrote.insert(self.transcript.clone());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.transcript)
            .unwrap();
        for row in rows {
            file.write_all(row.as_bytes()).unwrap();
            file.write_all(b"\n").unwrap();
        }
    }

    fn next_stamp(&mut self) -> String {
        self.clock += 1;
        stamp(START + self.clock)
    }

    fn switch_to(&mut self, n: u32) {
        register(&self.claude, &self.cwd, n);
        self.named = Some(n);
        self.transcript = self.project.join(format!("{}.jsonl", session_id(n)));
    }

    fn act(&mut self, name: &str) -> Option<i64> {
        match name {
            "Flag" => {
                self.argv
                    .extend(["--model".to_owned(), model_id(1).to_owned()]);
                self.launch = footer::launch_facts(&self.argv);
                self.runs = 1;
                self.chose = true;
                self.restore_pinned = false;
            }
            "LaunchResume" => {
                self.argv.extend(["--resume".to_owned(), session_id(1)]);
                self.launch = footer::launch_facts(&self.argv);
                self.switch_to(1);
                self.resumed = 1;
                if !self.chose {
                    self.runs = 2;
                    self.restore_pinned = true;
                }
            }
            "DrawCard" => self.card = Some(self.runs),
            "Answer" => {
                let at = self.next_stamp();
                self.append(&[answer_row(self.runs, &at)]);
            }
            "ChooseA" | "ChooseB" => {
                let m = if name == "ChooseA" { 1 } else { 2 };
                self.runs = m;
                self.chose = true;
                let at = self.next_stamp();
                self.append(&[
                    command_row(&at),
                    stdout_row(
                        &at,
                        &format!(
                            "Set model to `{} (default)` and saved as your default for new sessions",
                            model_name(m)
                        ),
                    ),
                ]);
            }
            "ChooseUnread" => {
                self.runs = if self.runs == 1 { 2 } else { 1 };
                self.chose = true;
                let at = self.next_stamp();
                self.append(&[
                    command_row(&at),
                    stdout_row(&at, &format!("Set model to {}", model_name(self.runs))),
                ]);
            }
            "Clear" => {
                let n = self.next_session;
                self.next_session += 1;
                self.switch_to(n);
                if self.reader == Reader::NoCarry {
                    self.cache = TailCache::default();
                }
            }
            "Resume" => {
                // An in-REPL `/resume`: into A first, then B. Claude restores
                // the conversation's model onto a process nothing pinned —
                // only A can meet one — and the restore pins it.
                if self.reader == Reader::RestoreNoPin && self.restore_pinned && !self.chose {
                    self.forget_the_restore_pin();
                }
                self.resumed += 1;
                self.switch_to(self.resumed);
                if !self.chose && !self.restore_pinned {
                    self.runs = 2;
                    self.restore_pinned = true;
                }
                if self.reader == Reader::NoCarry {
                    self.cache = TailCache::default();
                }
            }
            "ResumeLate" => {
                self.resumed = 1;
                self.switch_to(LATE);
                if !self.chose && !self.restore_pinned {
                    self.runs = 2;
                    self.restore_pinned = true;
                }
                if self.reader == Reader::NoCarry {
                    self.cache = TailCache::default();
                }
            }
            "Flood" => {
                let at = self.next_stamp();
                self.append(&[format!(
                    r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":[{{"type":"image","source":"{}"}}]}}}}"#,
                    "A".repeat(usize::try_from(footer::TAIL_BYTES).unwrap() + 4096)
                )]);
            }
            "Read" => return Some(self.read()),
            other => panic!("the model has no action {other}"),
        }
        None
    }

    fn forget_the_restore_pin(&mut self) {
        self.cache = TailCache::default();
        let own = self.next_session;
        self.next_session += 1;
        self.switch_to(own);
        let at = self.next_stamp();
        self.append(&[answer_row(self.runs, &at)]);
        assert_eq!(self.read(), self.runs, "the process's model is kept");
    }

    fn read(&mut self) -> i64 {
        let entry: SessionEntry =
            footer::session_of_pid(&self.claude, PID, Some(START)).expect("the registry");
        let facts: FooterFacts = match self.reader {
            Reader::Real
            | Reader::NoCarry
            | Reader::ResumeBlind
            | Reader::RestoreNoPin
            | Reader::FloorOnly => {
                let at = if self.reader == Reader::FloorOnly {
                    START
                } else {
                    START + self.clock
                };
                footer::facts_for_entry_at(
                    &self.claude,
                    PID,
                    Some(START),
                    &entry,
                    &self.launch,
                    &mut self.cache,
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(at),
                )
            }
            Reader::TailWindow => {
                let tail =
                    footer::read_tail_facts(&self.transcript, Some(START)).unwrap_or_default();
                let (model, model_open) = match tail.model {
                    footer::Said::Is(m) => (Some(m), false),
                    footer::Said::Unread => (None, false),
                    footer::Said::Unsaid => {
                        (self.launch.model.clone(), self.launch.model.is_none())
                    }
                };
                FooterFacts {
                    model,
                    model_open,
                    version: entry.version.clone(),
                    ..FooterFacts::default()
                }
            }
            Reader::NoFloor | Reader::NoResults => {
                let body = std::fs::read(&self.transcript).unwrap_or_default();
                let body: Vec<u8> = if self.reader == Reader::NoResults {
                    String::from_utf8(body)
                        .unwrap()
                        .lines()
                        .filter(|l| !l.contains("<local-command-stdout>"))
                        .map(|l| format!("{l}\n"))
                        .collect::<String>()
                        .into_bytes()
                } else {
                    body
                };
                let since = (self.reader == Reader::NoResults).then_some(START);
                let tail = footer::tail_facts(&body, since);
                let (model, model_open) = match tail.model {
                    footer::Said::Is(m) => (Some(m), false),
                    footer::Said::Unread => (None, false),
                    footer::Said::Unsaid => {
                        (self.launch.model.clone(), self.launch.model.is_none())
                    }
                };
                FooterFacts {
                    model,
                    model_open,
                    version: entry.version.clone(),
                    ..FooterFacts::default()
                }
            }
        };
        let card = footer::launch_card(&screen(self.card));
        if self.card.is_some() {
            assert!(card.is_some(), "the measured card is read");
        }
        project(facts.filled_from(card.as_ref()).model.as_deref())
    }
}

fn register(claude: &Path, cwd: &Path, n: u32) {
    std::fs::write(
        claude.join(format!("sessions/{PID}.json")),
        format!(
            r#"{{"pid":{PID},"sessionId":"{}","cwd":{},"procStart":"{}","startedAt":{},"version":"2.1.283"}}"#,
            session_id(n),
            aterm_json::to_string(&cwd.display().to_string()).unwrap(),
            footer::lstart_utc(START),
            START * 1000 + 684
        ),
    )
    .unwrap();
}

fn prompt_row(at: &str) -> String {
    format!(r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"go on"}}}}"#)
}

fn answer_row(m: i64, at: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{at}","effort":"xhigh","message":{{"model":"{}","role":"assistant","content":[{{"type":"text","text":"ok"}}]}}}}"#,
        model_id(m)
    )
}

fn command_row(at: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}}}"#
    )
}

fn stdout_row(at: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"<local-command-stdout>{text}</local-command-stdout>"}}}}"#
    )
}

type Space = Vec<(Vars, Vec<&'static str>)>;

fn paths(model: &Model) -> Space {
    let init = model.init_state();
    let mut seen: BTreeMap<Vars, Vec<&'static str>> = BTreeMap::new();
    seen.insert(init.clone(), Vec::new());
    let mut queue = VecDeque::from([init]);
    while let Some(state) = queue.pop_front() {
        let path = seen[&state].clone();
        for action in &model.actions {
            let mut next = state.clone();
            if model.fire(action.name, &mut next) && !seen.contains_key(&next) {
                let mut p = path.clone();
                p.push(action.name);
                seen.insert(next.clone(), p);
                queue.push_back(next);
            }
        }
    }
    seen.into_iter().collect()
}

fn footer() -> &'static (Model, Space) {
    static FOOTER: std::sync::OnceLock<(Model, Space)> = std::sync::OnceLock::new();
    FOOTER.get_or_init(|| {
        let model = claude_footer_model_model();
        let paths = paths(&model);
        (model, paths)
    })
}

struct Last {
    path: Vec<&'static str>,
    reader: Reader,
    before: Vars,
    after: Vars,
}

fn judge_last(model: &Model, last: &Last) -> Result<(), String> {
    let Last {
        path,
        reader,
        before,
        after,
    } = last;
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some("Read"),
        "claude footer model read",
    );
    if !accepted {
        return Err(format!(
            "{reader:?} after {path:?}: the real read is not the model's `Read`: \
             {before:?} -> {after:?}\n{diagnostics}"
        ));
    }
    for forged in [0, 1, 2].into_iter().filter(|v| *v != after["shown"]) {
        let mut lie = after.clone();
        lie.insert("shown", forged);
        let (accepted, _) = verify::validate_transition_tiered(
            model,
            &[("Buggy", 0)],
            before,
            &lie,
            Some("Read"),
            "forged footer read",
        );
        if accepted {
            return Err(format!("{path:?}: a forged shown={forged} passed"));
        }
    }
    Ok(())
}

fn replay(
    model: &Model,
    world: &mut World,
    path: &[&'static str],
    last: &mut dyn FnMut(Last) -> Result<(), String>,
) -> Result<usize, String> {
    world.reset();
    let reader = world.reader;
    let mut state = model.init_state();
    let mut reads = 0;
    for (at, &action) in path.iter().enumerate() {
        let before = state.clone();
        assert!(model.fire(action, &mut state), "{action} on {before:?}");
        if let Some(shown) = world.act(action) {
            reads += 1;
            let mut after = state.clone();
            after.insert("shown", shown);
            if at + 1 < path.len() {
                if after != state {
                    return Err(format!(
                        "{reader:?} after {:?}: the real read is not the model's `Read`: \
                         {before:?} -> {after:?}",
                        &path[..=at]
                    ));
                }
                continue;
            }
            if reader != Reader::Real && model.successors("Read", &before).contains(&after) {
                continue;
            }
            last(Last {
                path: path.to_vec(),
                reader,
                before,
                after,
            })?;
        }
    }
    Ok(reads)
}

fn enumerate(reader: Reader, keep: impl Fn(&[&'static str]) -> bool) -> Result<usize, String> {
    let (model, space) = footer();
    let mut replayed = BTreeSet::new();
    let mut walk: Vec<Vec<&'static str>> = Vec::new();
    for (state, path) in space.iter().filter(|(_, p)| keep(p)) {
        let mut read_now = path.clone();
        if state["synced"] == 0 {
            read_now.push("Read");
        }
        if replayed.insert(read_now.clone()) {
            walk.push(read_now);
        }
        if state["seen"] > 0 {
            let mut flooded = path.clone();
            flooded.extend(["Flood", "Read"]);
            if replayed.insert(flooded.clone()) {
                walk.push(flooded);
            }
        }
    }
    let evidence = xref::window_is_armed() == Some(true);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().div_ceil(2).min(POOL));
    let (next, stop, judging) = (
        AtomicUsize::new(0),
        AtomicBool::new(false),
        Judging::default(),
    );
    std::thread::scope(|scope| {
        let (done, outcomes) = std::sync::mpsc::channel();
        let pool: Vec<_> = (0..threads.min(walk.len()))
            .map(|_| {
                let (done, walk, next, stop, judging) =
                    (done.clone(), &walk, &next, &stop, &judging);
                scope.spawn(move || {
                    if evidence {
                        assert!(
                            xref::reset_entered_anchors(),
                            "a replay thread's evidence window opens"
                        );
                    }
                    let mut world = World::new("replay", reader);
                    while !stop.load(Ordering::Relaxed) {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = walk.get(at) else { break };
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                replay(model, &mut world, path, &mut |last| {
                                    judging.judge(model, &last)
                                })
                            }));
                        if done.send((at, outcome)).is_err() {
                            break;
                        }
                    }
                    evidence.then(xref::take_entered_anchors)
                })
            })
            .collect();
        drop(done);
        let walked = in_walk_order(walk.len(), &outcomes);
        stop.store(true, Ordering::Relaxed);
        for thread in pool {
            let entered = thread
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            if let Some(entered) = entered {
                let entered = entered.expect("a replay thread's evidence window is read");
                assert!(
                    xref::merge_entered_anchors(entered),
                    "the replays' anchors reach the caller's open evidence window"
                );
            }
        }
        walked
    })
}

const POOL: usize = 4;

/// [`judge_last`] on many threads, one `before` state at a time: every judgement of a
/// `before` asks `ty` about the same three successors (the real one and the forged
/// ones), so a second waits for the first and takes `verify`'s memo instead of
/// running the same `ty` processes beside it.
#[derive(Default)]
struct Judging(std::sync::Mutex<BTreeMap<Vars, std::sync::Arc<std::sync::Mutex<()>>>>);

impl Judging {
    fn judge(&self, model: &Model, last: &Last) -> Result<(), String> {
        let one = std::sync::Arc::clone(
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(last.before.clone())
                .or_default(),
        );
        let _alone = one
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        judge_last(model, last)
    }
}

type Outcome = (usize, std::thread::Result<Result<usize, String>>);

fn in_walk_order(
    len: usize,
    outcomes: &std::sync::mpsc::Receiver<Outcome>,
) -> Result<usize, String> {
    let mut ahead = BTreeMap::new();
    let mut reads = 0;
    for at in 0..len {
        let outcome = loop {
            if let Some(outcome) = ahead.remove(&at) {
                break outcome;
            }
            let (done, outcome) = outcomes.recv().expect("every path is replayed");
            ahead.insert(done, outcome);
        };
        reads += outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
    }
    Ok(reads)
}

const READERS: [&str; 5] = [
    "facts_for_entry_from",
    "fold_result",
    "for_process",
    "read_since",
    "scan",
];

const WALK_DEPTH: usize = 5;

fn within(depth: usize) -> impl Fn(&[&'static str]) -> bool {
    move |path| path.len() <= depth
}

fn bind(depth: usize) {
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let model = &footer().0;
    let reads = enumerate(Reader::Real, within(depth)).unwrap_or_else(|e| panic!("{e}"));
    assert!(reads >= 60, "the bind must read the model's space: {reads}");

    let mut anchored: Vec<&str> = xref::refinements()
        .filter(|a| a.machine == "ClaudeFooterModel")
        .map(|a| {
            assert_eq!(
                (a.action, a.project),
                ("Read", "conformance_footer_model::project")
            );
            a.rust_method
        })
        .collect();
    anchored.sort_unstable();
    assert_eq!(anchored, READERS);
    let entered = xref::entered_anchor_ids();
    for method in READERS {
        let id = format!("ClaudeFooterModel::Read @ {method}");
        assert!(entered.contains(id.as_str()), "{id} entered: {entered:?}");
    }
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "ClaudeFooterModel")
        .map(|w| w.action)
        .collect();
    let mut actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert!(actions.remove("Read"), "the reader's action");
    assert_eq!(waived, actions, "every other action waived, `Read` not");
}

#[test]
fn the_footer_names_the_running_model_from_every_state_within_five_actions() {
    bind(WALK_DEPTH);
}

fn launch_resumed(path: &[&'static str]) -> bool {
    path.contains(&"LaunchResume")
}

fn late(path: &[&'static str]) -> bool {
    path.contains(&"ResumeLate")
}

fn old_readers_are_caught(depth: usize) {
    let inside = within(depth);
    let fresh = |p: &[&'static str]| !launch_resumed(p) && !late(p) && inside(p);
    for reader in [
        Reader::NoFloor,
        Reader::NoResults,
        Reader::TailWindow,
        Reader::NoCarry,
        Reader::ResumeBlind,
        Reader::RestoreNoPin,
    ] {
        let caught = enumerate(reader, fresh);
        assert!(
            caught.is_err(),
            "{reader:?} must disagree with the model somewhere"
        );
    }
    let launched = |p: &[&'static str]| launch_resumed(p) && inside(p);
    assert!(
        enumerate(Reader::RestoreNoPin, launched).is_err(),
        "the restore a launch `--resume` made is a pin"
    );
    let through_d = |p: &[&'static str]| late(p) && inside(p);
    assert!(
        enumerate(Reader::FloorOnly, through_d).is_err(),
        "a conversation begun since the process started is a resume"
    );
}

#[test]
fn the_bind_catches_each_old_reader() {
    old_readers_are_caught(WALK_DEPTH);
}
