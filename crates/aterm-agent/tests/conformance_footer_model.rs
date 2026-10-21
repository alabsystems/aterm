// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ClaudeFooterModel, Tier-1: which model the Claude Code footer names
//! (`harness::footer`), driven from the states of the derived model within
//! [`WALK_DEPTH`] actions of its start and projected back onto it.
//!
//! Driven: every `Read` is the real `footer::session_of_pid`, then
//! `footer::facts_for_entry_at` at the world's clock (the floor, the transcript
//! through a real `TailCache`, the launch flag's tier), then
//! `FooterFacts::filled_from` with the real `footer::launch_card` over a
//! recorded 2.1.283 screen: the host's whole chain minus painting. Transcripts
//! are real rows in Claude Code 2.1.283's shapes, appended as the model acts,
//! including readable and unreadable `/model` results and a `Flood` row 4 KiB
//! larger than the reader's tail window (`footer::TAIL_BYTES`). Conversations A (last answered by
//! `Pred`), B (by `Other`) and D (begun by another process at the floor) sit
//! on disk. `Flag` and `LaunchResume` are launch argv (`footer::launch_facts`);
//! `Clear` and `Resume`/`ResumeLate` rewrite `sessions/<pid>.json` as Claude
//! does.
//!
//! Transcribed, not driven: the host's card memo, the resolver's thread and cadence
//! (a switch waits for a read of the session it leaves, `known == 1`), and the
//! argv/environment read.
//!
//! Projection: model 1 is `claude-opus-5-5`, model 2 `claude-fable-5-1`; `shown` is
//! the footer's model, `0` for none.
//!
//! Negative controls, each caught by the same enumeration: a forged `shown`, and the
//! defective readers of [`Reader`]. The walk is a sample by depth; [`WALK_DEPTH`]
//! says why it bites.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::footer::{self, FooterFacts, LaunchFacts, SessionEntry, TailCache};
use aterm_models_harness::claude_footer_model_model;
use aterm_spec::derive::Model;
use aterm_spec::{verify, xref};

type Vars = BTreeMap<&'static str, i64>;

/// The process's start: the floor.
const START: u64 = 1_790_607_848;
const PID: u32 = 4242;

/// The session id the registry names for the `n`th session of the process.
fn session_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// D's session number: the conversation another process began at the floor.
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

/// A replay's world: a Claude directory, its registry file, the transcripts,
/// the screen and the reader's cache. A replay starts from [`World::reset`]'s,
/// so one world serves a replay thread's replays.
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
    // `lstart_utc` knows the civil date; the transcript wants RFC 3339.
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

/// A recorded 2.1.283 bypass screen whose card names `Opus 5.5 with xhigh
/// effort`: the card naming the model `card` says, or the same screen with its
/// card scrolled away.
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

    /// The world at the process's launch: the project's transcripts only those
    /// from before the floor, the registry naming a fresh launch's session, a
    /// fresh cache, and nothing chosen, resumed or drawn. The directories are
    /// made once ([`World::new`]): made and removed per replay, they were over
    /// half the walk's time. Of the files, only what the last replay wrote is
    /// undone: each transcript it wrote or appended to is removed (and written
    /// afresh where it is one from before the floor), and the registry written
    /// again where it switched; every other file is as the last reset left it.
    fn reset(&mut self) {
        let first = self.named.is_none();
        let gone = std::mem::take(&mut self.wrote);
        for path in &gone {
            std::fs::remove_file(path).unwrap();
        }
        let project = &self.project;
        // The conversation `n`'s transcript, where it is to be written.
        let unwritten = |n: u32| {
            let path = project.join(format!("{}.jsonl", session_id(n)));
            (first || gone.contains(&path)).then_some(path)
        };
        // The conversations on disk from before the floor: A last answered
        // with `Pred` (model 2) — a meta and a `<synthetic>` answer after it,
        // which Claude's restore skips — and B with `Other` (model 1), over
        // an older `Pred` answer. The resume-blind reader sees none of it.
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
        // D: begun and answered by another process at the floor, after this one
        // started and before the reader first saw it.
        if let Some(path) = unwritten(LATE) {
            let at_floor = stamp(START);
            std::fs::write(
                path,
                format!("{}\n{}\n", prompt_row(&at_floor), answer_row(2, &at_floor)),
            )
            .unwrap();
        }
        // A fresh launch's session of its own, not written yet.
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
        // A cleared session's transcript is written by its first row.
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

    /// The registry names session `n` now, whose transcript the rows go to.
    fn switch_to(&mut self, n: u32) {
        register(&self.claude, &self.cwd, n);
        self.named = Some(n);
        self.transcript = self.project.join(format!("{}.jsonl", session_id(n)));
    }

    /// The model's action `name`, done to the real world.
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
                // `claude --resume <A>`: the first session is A, and Claude
                // restores its `Pred` unless the `--model` pinned the model.
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
                // A switch that moves the model, its result unreadable: the model
                // named without the quotes every build puts on it.
                self.runs = if self.runs == 1 { 2 } else { 1 };
                self.chose = true;
                let at = self.next_stamp();
                self.append(&[
                    command_row(&at),
                    stdout_row(&at, &format!("Set model to {}", model_name(self.runs))),
                ]);
            }
            "Clear" => {
                // `/clear`: the registry names a new session of the same
                // process; its transcript is not written yet.
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
                // An in-REPL `/resume` into D; a later one goes on to B.
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

    /// [`Reader::RestoreNoPin`]'s cache, just before a `/resume` only a
    /// restore's pin keeps: a fresh cache whose process answered with the
    /// model it runs in a session of its own — its model kept, the pin gone.
    fn forget_the_restore_pin(&mut self) {
        self.cache = TailCache::default();
        let own = self.next_session;
        self.next_session += 1;
        self.switch_to(own);
        let at = self.next_stamp();
        self.append(&[answer_row(self.runs, &at)]);
        assert_eq!(self.read(), self.runs, "the process's model is kept");
    }

    /// The footer's model now, through `reader`.
    fn read(&mut self) -> i64 {
        let entry: SessionEntry =
            footer::session_of_pid(&self.claude, PID, Some(START)).expect("the registry");
        let facts: FooterFacts = match self.reader {
            Reader::Real
            | Reader::NoCarry
            | Reader::ResumeBlind
            | Reader::RestoreNoPin
            | Reader::FloorOnly => {
                // The read happens after every row written so far.
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

/// Write the registry file of the process for its `n`th session: the same
/// pid, `procStart` and `startedAt` every time — only the session id moves.
/// The directory is a JSON string as Claude Code writes it (a Windows path's
/// backslashes escaped).
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

/// [`paths`]'s result.
type Space = Vec<(Vars, Vec<&'static str>)>;

/// Every reachable state of `model` with the shortest path to it.
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

/// The derived model and every state it reaches with its shortest path ([`paths`]),
/// built once per process for the walk and every control.
fn footer() -> &'static (Model, Space) {
    static FOOTER: std::sync::OnceLock<(Model, Space)> = std::sync::OnceLock::new();
    FOOTER.get_or_init(|| {
        let model = claude_footer_model_model();
        let paths = paths(&model);
        (model, paths)
    })
}

/// The last `Read` of a replay, for both tiers to judge: the real successor
/// must be admitted, and every forged one — any other model shown — refused.
struct Last {
    path: Vec<&'static str>,
    reader: Reader,
    before: Vars,
    after: Vars,
}

/// Judge `last` by both tiers ([`verify::validate_transition_tiered`]); the first
/// disagreement is the `Err`. A transition met many times (a `Flood` moves the world,
/// not the model) is judged at every meeting; only the `ty` process is not run again,
/// as `verify` keeps `ty`'s verdict on the exact spec, cfg and trace.
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
    // A forged successor (any other model shown) is refused.
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

/// Replay `path` on `world`, reset ([`World::reset`]), through its reader; every
/// `Read` is checked against the model, and the first disagreement returned. The
/// last `Read` is judged by both tiers via `last` (a defective reader's only when the
/// interpreter refuses it), earlier ones by the interpreter alone: paths are
/// breadth-first, so every earlier `Read` is some prefix's last and is judged by both
/// tiers there.
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

/// Every reachable state whose path `keep` takes, reached and read, and read again
/// after a flood. A path is replayed once: a state just read was reached by its
/// breadth-first parent's `Read`, and a flood out of a state just read is the plain
/// read of the state it reaches, so replaying such a path again repeats its checks.
///
/// The replays run on a pool ([`POOL`]), each thread on a world of its own reset per
/// replay, and each replay judging its own last `Read` ([`Judging`]), because both
/// the worlds (transcripts, a flood past the tail window each) and the judge (one `ty` process per
/// distinct transition) are slow. The calling thread takes the outcomes in walk
/// order ([`in_walk_order`]), so the first disagreement in walk order is the `Err`
/// and a panic is raised at its place, as if the replays ran in sequence; after a
/// catch the pool stops taking paths. When the calling thread has an evidence window
/// open ([`bind`]'s), each pool thread opens its own and hands it back
/// (`xref::take_entered_anchors`, `xref::merge_entered_anchors`), so the bind sees
/// every anchor its walk entered, and only those.
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
                        // A caller that stopped taking outcomes (a panic)
                        // dropped the receiver: nothing more is wanted.
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

/// The most replay threads one [`enumerate`] runs (half the cores, at most this).
/// Every replay is files written and removed, which does not scale: on 14 cores the
/// replays (each then a world's directories made and removed) took 1.3-1.6 s on 4
/// threads and 3.0 s on 7.
const POOL: usize = 4;

/// [`judge_last`] on many threads, one `before` state at a time: every judgement of a
/// `before` asks `ty` about the same three successors (the real one and the forged
/// ones), so a second waits for the first and takes `verify`'s memo instead of
/// running the same `ty` processes beside it. Every meeting is still judged.
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

/// One replay's outcome, a panic included, at its place in the walk.
type Outcome = (usize, std::thread::Result<Result<usize, String>>);

/// The `len` replays' outcomes in walk order, whichever finished first: their
/// reads summed, or the first disagreement in walk order. A replay's panic is
/// raised at its place.
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

/// Where the reader is anchored in the shipping code (`#[refines]` on
/// `Read`, expanded in every test and debug build of this crate):
/// every part of the one read the host's resolver runs — the facts for a
/// registry entry, the tail cache's read and its session switch, the scan,
/// and a command result's fold.
const READERS: [&str; 5] = [
    "facts_for_entry_from",
    "fold_result",
    "for_process",
    "read_since",
    "scan",
];

/// The walk: every state within this many actions of the model's start (its
/// breadth-first depth in [`paths`]), each read, and read again after a flood, by the
/// real chain and judged by both tiers.
///
/// Why 5: each of the model's `Buggy` behaviours put into the real reader turns this
/// walk red within 4 actions, and the deepest shortest counterexample the controls
/// below keep is 5 (a restore taken for no pin, in a process not launched onto a
/// conversation). That is 707 of the 3,260 reachable states (2,551 reads), about a
/// fifth of the whole space's cost; the rest of the space, to depth 10, is not walked.
const WALK_DEPTH: usize = 5;

/// The paths of at most `depth` actions: a prefix-closed part of the space, so
/// [`replay`]'s argument that every `Read` is judged by both tiers holds in it.
fn within(depth: usize) -> impl Fn(&[&'static str]) -> bool {
    move |path| path.len() <= depth
}

/// The real reader's bind over the states within `depth` actions: the walk, then the
/// anchors, linked, naming the one projection, and entered by it. The evidence window
/// is per thread; the walk's replay threads hand theirs back into this one's.
fn bind(depth: usize) {
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let model = &footer().0;
    let reads = enumerate(Reader::Real, within(depth)).unwrap_or_else(|e| panic!("{e}"));
    assert!(reads >= 60, "the bind must read the model's space: {reads}");

    // The anchors are linked, name the one projection, and were entered by the
    // walk; every other action (Claude Code's, a person's) is waived by name.
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

/// The real reader names the running model from every state within
/// [`WALK_DEPTH`] actions ([`bind`]).
#[test]
fn the_footer_names_the_running_model_from_every_state_within_five_actions() {
    bind(WALK_DEPTH);
}

/// A path of a process launched onto a resumed conversation.
fn launch_resumed(path: &[&'static str]) -> bool {
    path.contains(&"LaunchResume")
}

/// A path through D, the conversation begun since the process started.
fn late(path: &[&'static str]) -> bool {
    path.contains(&"ResumeLate")
}

/// Negative controls: each defect the model's `Buggy` branches describe, put
/// into the reader, is caught by the same enumeration in the part of the space
/// where that defect lives, so no control passes on another's catch. Over the
/// states within `depth` actions; each is caught within [`WALK_DEPTH`].
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
