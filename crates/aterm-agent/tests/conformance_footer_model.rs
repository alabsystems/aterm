// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ClaudeFooterModel, Tier-1: WHICH MODEL the Claude Code footer names
//! (`harness::footer`), driven from every reachable state of the derived
//! model and projected back onto it.
//!
//! WHAT IS DRIVEN. Every `Read` is the real `footer::session_of_pid` over
//! the registry file, then the real `footer::facts_for_entry_at` at the
//! world's clock (the stamp of the newest row written) — the floor, the
//! transcript read through a real `TailCache` (its append carry, the
//! process's own facts, its pin included, and when it was last seen in each
//! session), the launch flag's tier —
//! then `FooterFacts::filled_from` with the real `footer::launch_card` over a
//! measured 2.1.283 screen: the whole chain the host's resolver and painter
//! run for one pane, minus the painting. The model's first `Read` is the
//! resolver's first sight of the process. The transcript is REAL rows in
//! Claude Code 2.1.283's shapes, appended as the model's actions happen:
//! answers, `/model` command and result rows (a readable `Set model to`
//! result; an unreadable one that names the switch unquoted, `Set model to
//! Fable 5.1`), and a 600 KiB image prompt for `Flood` — larger than the
//! reader's whole tail window. Two conversations sit on disk from before the
//! floor: A, which last answered with `claude-fable-5-1` (`Pred`) under a
//! meta and a `<synthetic>` answer Claude's restore skips, and B, which last
//! answered with `claude-opus-5-5` (`Other`) over an older Fable 5.1 answer.
//! A third, D, was begun and answered with `Pred` by another process in the
//! second this one started — every row of it at the floor, none before it —
//! before the reader's first sight of this one.
//! The launch argv is the real `footer::launch_facts` over `claude`, with
//! `--model claude-opus-5-5` for `Flag` and `--resume <A>` for
//! `LaunchResume` — whose first session is A, restored by Claude unless
//! flagged. `Clear` is `/clear` as Claude does it: `sessions/<pid>.json`
//! REWRITTEN with a new `sessionId` under the same pid, `procStart` and
//! `startedAt`, the new session's transcript not yet written. `Resume` is an
//! in-REPL `/resume`, the same registry rewrite into the next conversation:
//! A, then B (a process launched onto A goes on to B); `ResumeLate` the same
//! into D, and a later `Resume` goes on to B.
//!
//! WHAT IS TRANSCRIBED, NOT DRIVEN: the host's card MEMO (it keeps the
//! newest card it read for one owner; here the card is read on every `Read`,
//! naming the model the process started on), the resolver's thread and its
//! refresh cadence — the model's `Clear`, `Resume` and `ResumeLate` wait for
//! a read of the session they leave (`known == 1`), and a read's wall clock
//! is the stamp of the newest row written, the soonest it can come — and the
//! argv and environment read (`KERN_PROCARGS2`): an environment model pin is
//! not driven.
//!
//! PROJECTION. Model 1 is `claude-opus-5-5` (`Opus 5.5`: the fresh start, its
//! `--model`, and `Other`), model 2 `claude-fable-5-1` (`Fable 5.1`, `Pred`);
//! `shown` is the footer's model, `0` for none.
//!
//! NEGATIVE CONTROLS, each caught by the same enumeration that passes the
//! shipped reader: a forged `shown` after a real `Read`; the reader without
//! its floor (the resumed conversation's own answer is read); the reader
//! before 2026-09-28, which read no model result (a readable choice voids
//! until the next answer); the fixed tail window it read through (a huge row
//! flushes the answer above it); the reader that keeps nothing of the process
//! across a session switch (after `/clear`, the launch flag's model); the
//! reader that cannot tell a `/resume` from a `/clear` — the resumed
//! conversations' rows from before the floor hidden from it — which carries
//! the process's model over the one Claude restored; the reader that takes a
//! restore for no pin (the second round's), which restores B's model at the
//! second resume where Claude keeps the one it restored; and the reader that
//! tells a `/resume` from a `/clear` by the floor alone (the third round's:
//! the real chain with its clock stopped at the floor), which reads D as a
//! `/clear` — D's answer as a pinned process's own, and no pin from D's
//! restore.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_agent::harness::footer::{self, FooterFacts, LaunchFacts, SessionEntry, TailCache};
use aterm_spec::derive::{Model, claude_footer_model_model};
use aterm_spec::verify;

type Vars = BTreeMap<&'static str, i64>;

/// The process's start: the floor (the owner's session, 2026-09-28).
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
    /// Model results are not read (the pre-2026-09-28 reader): the result
    /// rows are hidden from it, so a `/model` command voids.
    NoResults,
    /// The fixed tail window (the pre-2026-09-28 read): no carry and no
    /// look past a huge row.
    TailWindow,
    /// The real chain with a cache that forgets the process at a session
    /// switch (the reader before the process's own facts).
    NoCarry,
    /// The real chain, the resumed conversations' rows before the floor
    /// hidden from it: every switch reads as a `/clear` (the reader of the
    /// first 2026-09-28 round).
    ResumeBlind,
    /// The real chain with a cache that, at a `/resume` only a restore's pin
    /// keeps, has forgotten that pin while keeping the model: before the
    /// switch it is a fresh cache that saw the process's model answered in a
    /// session of its own (the reader of the second 2026-09-28 round, which
    /// took a restore for no pin).
    RestoreNoPin,
    /// The real chain reading at the floor: it knows only that a switch came
    /// after the process started, so a conversation begun since reads as a
    /// `/clear` (the reader of the third 2026-09-28 round).
    FloorOnly,
}

/// One replay's world: a Claude directory, its registry file, the
/// transcripts, the screen and the reader's cache.
struct World {
    root: PathBuf,
    claude: PathBuf,
    project: PathBuf,
    cwd: PathBuf,
    transcript: PathBuf,
    /// The next session id a `Clear` takes (1 and 2 are A and B).
    next_session: u32,
    /// The process's argv.
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

/// A measured 2.1.283 screen: the owner's fresh bypass screen, whose card
/// names `Opus 5.5 with xhigh effort` — the card naming the model `card`
/// says, or the same screen with its card scrolled away.
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
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "aterm-footer-model-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        let claude = root.join("claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(footer::project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        // The conversations on disk from before the floor: A last answered
        // with `Pred` (model 2) — a meta and a `<synthetic>` answer after it,
        // which Claude's restore skips — and B with `Other` (model 1), over
        // an older `Pred` answer. The resume-blind reader sees none of it.
        if reader != Reader::ResumeBlind {
            let before = |s: u64| stamp(START - s);
            let write = |n: u32, rows: &[String]| {
                let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
                std::fs::write(project.join(format!("{}.jsonl", session_id(n))), body).unwrap();
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
        // D: begun and answered by ANOTHER process at the floor — after this
        // one started, before the reader first saw it.
        let at_floor = stamp(START);
        std::fs::write(
            project.join(format!("{}.jsonl", session_id(LATE))),
            format!("{}\n{}\n", prompt_row(&at_floor), answer_row(2, &at_floor)),
        )
        .unwrap();
        // A fresh launch's session of its own, not written yet.
        register(&claude, &cwd, 3);
        let transcript = project.join(format!("{}.jsonl", session_id(3)));
        let argv = vec!["claude".to_owned()];
        Self {
            root,
            claude,
            project,
            cwd,
            transcript,
            next_session: 4,
            launch: footer::launch_facts(&argv),
            argv,
            card: None,
            cache: TailCache::default(),
            clock: 1,
            runs: 1,
            chose: false,
            restore_pinned: false,
            resumed: 0,
            reader,
        }
    }

    fn append(&mut self, rows: &[String]) {
        // A cleared session's transcript is written by its first row.
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
                // A switch that MOVES the model, its result unreadable: the
                // model named without the quotes every build puts on it.
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
                    "A".repeat(600 * 1024)
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

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Write the registry file of the process for its `n`th session: the same
/// pid, `procStart` and `startedAt` every time — only the session id moves.
fn register(claude: &Path, cwd: &Path, n: u32) {
    std::fs::write(
        claude.join(format!("sessions/{PID}.json")),
        format!(
            r#"{{"pid":{PID},"sessionId":"{}","cwd":"{}","procStart":"{}","startedAt":{},"version":"2.1.283"}}"#,
            session_id(n),
            cwd.display(),
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

/// Every reachable state of `model` with the shortest path to it.
fn paths(model: &Model) -> Vec<(Vars, Vec<&'static str>)> {
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

/// Replay `path` on a fresh world through `reader`; every `Read` is checked
/// against the model. Returns the first disagreement.
///
/// The LAST `Read` is judged by both tiers (`validate_transition_tiered`:
/// the interpreter and `ty`, with every forged successor refused); an
/// earlier one by the interpreter's own step alone. Nothing goes unjudged by
/// `ty`: the paths are breadth-first, so every prefix of one is the path to
/// the state it ends in, and a prefix ending in that earlier `Read` is
/// replayed on its own, the `Read` its last — the world is deterministic, so
/// it reads the same there.
fn replay(model: &Model, path: &[&'static str], reader: Reader) -> Result<usize, String> {
    let mut world = World::new("replay", reader);
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
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                model,
                &[("Buggy", 0)],
                &before,
                &after,
                Some("Read"),
                "claude footer model read",
            );
            if !accepted {
                return Err(format!(
                    "{reader:?} after {path:?}: the real read is not the model's `Read`: \
                     {before:?} -> {after:?}\n{diagnostics}"
                ));
            }
            // A FORGED successor — any other model shown — is refused.
            for forged in [0, 1, 2].into_iter().filter(|v| *v != shown) {
                let mut lie = after.clone();
                lie.insert("shown", forged);
                let (accepted, _) = verify::validate_transition_tiered(
                    model,
                    &[("Buggy", 0)],
                    &before,
                    &lie,
                    Some("Read"),
                    "forged footer read",
                );
                if accepted && reader == Reader::Real {
                    return Err(format!("{path:?}: a forged shown={forged} passed"));
                }
            }
        }
    }
    Ok(reads)
}

/// Every reachable state whose path `keep` takes, reached and READ — and
/// read again after a flood.
fn enumerate(
    model: &Model,
    reader: Reader,
    keep: impl Fn(&[&'static str]) -> bool,
) -> Result<usize, String> {
    let mut reads = 0;
    for (state, path) in paths(model).into_iter().filter(|(_, p)| keep(p)) {
        let mut read_now = path.clone();
        if state["synced"] == 0 {
            read_now.push("Read");
        }
        reads += replay(model, &read_now, reader)?;
        if state["seen"] > 0 {
            let mut flooded = path;
            flooded.extend(["Flood", "Read"]);
            reads += replay(model, &flooded, reader)?;
        }
    }
    Ok(reads)
}

/// Where the reader is anchored in the shipping code (`#[refines]` on
/// `Read`, this crate's `spec-anchors`, on through its dev edge on itself):
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

#[test]
fn the_footer_names_the_running_model_from_every_reachable_state() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let model = claude_footer_model_model();
    let reads = enumerate(&model, Reader::Real, |_| true).unwrap_or_else(|e| panic!("{e}"));
    assert!(reads >= 60, "the bind must read the model's space: {reads}");

    // The anchors are linked, name the one projection, and were ENTERED by
    // the walk; every other action — Claude Code's, a person's — is waived
    // by name.
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
    let waived: std::collections::BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "ClaudeFooterModel")
        .map(|w| w.action)
        .collect();
    let mut actions: std::collections::BTreeSet<&str> =
        model.actions.iter().map(|a| a.name).collect();
    assert!(actions.remove("Read"), "the reader's action");
    assert_eq!(waived, actions, "every other action waived, `Read` not");
}

/// A path of a process launched onto a resumed conversation.
fn launch_resumed(path: &[&'static str]) -> bool {
    path.contains(&"LaunchResume")
}

/// A path through D, the conversation begun since the process started.
fn late(path: &[&'static str]) -> bool {
    path.contains(&"ResumeLate")
}

/// NEGATIVE CONTROLS: each defect the model's `Buggy` branches describe,
/// put into the reader, is caught by the same enumeration — in the part of
/// the space where that defect lives, so no control passes on another's
/// catch: the readers older than the launch resume in a process launched
/// on its own session that never took D up (they never named a restore at
/// launch, nor met D), the reader that took a restore for no pin both there
/// (a second `/resume`, the reviewer's R1) and in a process launched with
/// `--resume` (a first in-REPL `/resume`, R2), and the floor-only judge on
/// the paths through D (the reviewer's H5), where alone it errs.
#[test]
fn the_bind_catches_each_old_reader() {
    let model = claude_footer_model_model();
    let fresh = |p: &[&'static str]| !launch_resumed(p) && !late(p);
    for reader in [
        Reader::NoFloor,
        Reader::NoResults,
        Reader::TailWindow,
        Reader::NoCarry,
        Reader::ResumeBlind,
        Reader::RestoreNoPin,
    ] {
        let caught = enumerate(&model, reader, fresh);
        assert!(
            caught.is_err(),
            "{reader:?} must disagree with the model somewhere"
        );
    }
    assert!(
        enumerate(&model, Reader::RestoreNoPin, launch_resumed).is_err(),
        "the restore a launch `--resume` made is a pin"
    );
    assert!(
        enumerate(&model, Reader::FloorOnly, late).is_err(),
        "a conversation begun since the process started is a resume"
    );
}
